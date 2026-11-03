//! `innerHTML` / `outerHTML` and the fragment parsing algorithms
//! (`inner_outer_html_mixin.c`).

use std::ffi::c_int;
use std::ptr;

use crate::error::{DomError, DomExceptionCode as C, DomResult};
use crate::ffi;
use crate::html5_parser::convert_fragment;
use crate::html5_serializer::Html5Serializer;
use crate::lexbor::Fragment;
use crate::ns::{ns_is_fast, HTML_TOKEN};
use crate::object::{Node, QuirksMode};
use crate::raw::N;

/// `XML_PARSE_IGNORE_ENC`.
const XML_PARSE_IGNORE_ENC: c_int = 1 << 21;

/// <https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#fragment-serializing-algorithm-steps>.
fn fragment_serialize(this: &Node, node: N) -> DomResult<Vec<u8>> {
    let private = this.doc_ref().and_then(|d| d.private_data());
    let is_html = node.doc().is_some_and(|d| d.t() == ffi::XML_HTML_DOCUMENT_NODE);
    if is_html {
        let mut s = Html5Serializer::new(private.as_deref());
        s.serialize(node);
        Ok(s.finish())
    } else {
        crate::xml_serializer::fragment_serialize(node, private.as_ref())
    }
}

/// `$innerHTML`.
pub(crate) fn inner_html(n: &Node) -> DomResult<Vec<u8>> {
    let node = n.prop_node()?;
    fragment_serialize(n, node)
}

/// `$outerHTML`.
pub(crate) fn outer_html(n: &Node) -> DomResult<Vec<u8>> {
    let this = n.prop_node()?;
    // 1. Let element be a fictional node whose only child is this.
    // SAFETY: an all-zero xmlNode is a valid empty node record.
    let mut element: ffi::xmlNode = unsafe { std::mem::zeroed() };
    element.type_ = ffi::XML_DOCUMENT_FRAG_NODE;
    element.children = this.p();
    element.last = this.p();
    element.doc = this.doc_ptr();
    let fake = N::new(&raw mut element).expect("stack node");
    let old_parent = this.parent();
    let old_next = this.next();
    this.set_parent(Some(fake));
    this.set_next(None);
    let r = fragment_serialize(n, fake);
    this.set_parent(old_parent);
    this.set_next(old_next);
    r
}

fn quirks_id(q: QuirksMode) -> c_int {
    match q {
        QuirksMode::NoQuirks => 0,
        QuirksMode::LimitedQuirks => 1,
        QuirksMode::Quirks => 2,
    }
}

/// <https://html.spec.whatwg.org/#html-fragment-parsing-algorithm>.
fn html_fragment_parsing_algorithm(this: &Node, context: N, input: &[u8], is_template: bool) -> DomResult<N> {
    let doc_ref = this.doc_ref();
    let quirks = doc_ref.as_ref().map_or(QuirksMode::NoQuirks, |d| d.quirks_mode.get());
    let private = doc_ref.as_ref().and_then(|d| d.private_data()).ok_or_else(|| DomError::dom(C::InvalidState))?;
    let ns_uri: &[u8] = context.ns().map_or(b"", crate::raw::Ns::href);
    let fragment = Fragment::parse(quirks_id(quirks), context.name(), ns_uri, input);
    let Some(root) = fragment.root() else {
        return Err(DomError::dom(C::InvalidState));
    };
    convert_fragment(root.last_child(), context.doc_ptr(), true, true, is_template, &private)
        .map_err(|_| DomError::dom(C::InvalidState))
}

/// <https://html.spec.whatwg.org/#xml-fragment-parsing-algorithm>.
fn xml_fragment_parsing_algorithm(this: &Node, context: N, input: &[u8]) -> DomResult<N> {
    use php_libxml::ffi as lx;
    let not_well_formed = || DomError::dom_msg(C::Syntax, "XML fragment is not well-formed");
    let private = this.doc_ref().and_then(|d| d.private_data()).ok_or_else(|| DomError::dom(C::InvalidState))?;
    let _scope = crate::parse::ErrorScope::new();
    // SAFETY: a fresh push parser; its dictionary is replaced by the
    // document's (and detached again before the parser is freed).
    let parser = unsafe { lx::xmlCreatePushParserCtxt(ptr::null_mut(), ptr::null_mut(), ptr::null(), 0, ptr::null()) };
    if parser.is_null() {
        return Err(DomError::dom(C::InvalidState));
    }
    let docp = context.doc_ptr();
    // SAFETY: live parser and document.
    unsafe {
        ffi::xmlDictFree((*parser).dict.cast());
        if (*docp).dict.is_null() {
            (*docp).dict = ffi::xmlDictCreate();
            lx::xmlDictSetLimit((*docp).dict.cast(), 10_000_000);
        }
        (*parser).dict = (*docp).dict.cast();
        crate::parse::sanitize_ctxt_pub(parser.cast());
        ffi::xmlCtxtUseOptions(parser.cast(), XML_PARSE_IGNORE_ENC | ffi::XML_PARSE_NOERROR | ffi::XML_PARSE_NOWARNING);
        let enc = ffi::xmlFindCharEncodingHandler(c"UTF-8".as_ptr());
        ffi::xmlSwitchToEncoding(parser.cast(), enc);
    }

    let chunk = |data: &[u8], terminate: bool| {
        // SAFETY: live parser; valid buffer.
        unsafe {
            lx::xmlParseChunk(parser, data.as_ptr().cast(), c_int::try_from(data.len()).unwrap_or(c_int::MAX), c_int::from(terminate));
        }
    };
    let tag_name = |chunk: &dyn Fn(&[u8], bool)| {
        if let Some(p) = context.ns().and_then(crate::raw::Ns::prefix) {
            chunk(p, false);
            chunk(b":", false);
        }
        chunk(context.name(), false);
    };
    chunk(b"<", false);
    tag_name(&chunk);
    let in_scope = crate::ns::in_scope_ns(&mut private.mapper.borrow_mut(), context, true);
    for ns in in_scope {
        let Some(ns) = crate::raw::Ns::new(ns) else { continue };
        chunk(b" xmlns:", false);
        chunk(ns.prefix().unwrap_or_default(), false);
        chunk(b"=\"", false);
        chunk(ns.href(), false);
        chunk(b"\"", false);
    }
    if let Some(default_ns) = crate::node::locate_a_namespace(context, None) {
        chunk(b" xmlns=\"", false);
        chunk(&default_ns, false);
        chunk(b"\"", false);
    }
    chunk(b">", false);
    chunk(input, false);
    chunk(b"</", false);
    tag_name(&chunk);
    chunk(b">", true);

    // SAFETY: live parser; the resulting document is ours.
    unsafe {
        if (*parser).wellFormed == 0 || (*parser).nsWellFormed == 0 {
            (*parser).dict = ptr::null_mut();
            let my = (*parser).myDoc;
            if !my.is_null() {
                ffi::xmlFreeDoc(my.cast());
            }
            lx::xmlFreeParserCtxt(parser);
            return Err(not_well_formed());
        }
        let doc = (*parser).myDoc.cast::<ffi::xmlDoc>();
        lx::xmlFreeParserCtxt(parser);
        let Some(d) = N::from_doc(doc) else {
            return Err(DomError::dom(C::InvalidState));
        };
        (*doc).dict = ptr::null_mut();
        let document_element = d.children();
        let Some(de) = document_element.filter(|de| de.next().is_none()) else {
            ffi::xmlFreeDoc(doc);
            return Err(not_well_formed());
        };
        let fragment = N::new(ffi::xmlNewDocFragment(docp));
        if let Some(fragment) = fragment {
            let child = de.children();
            ffi::xmlSetTreeDoc(de.p(), docp);
            ffi::xmlAddChildList(fragment.p(), crate::raw::raw(child));
            crate::document::mark_namespaces_as_attributes_too(&mut private.mapper.borrow_mut(), d);
            de.set_children(None);
            de.set_last(None);
        }
        ffi::xmlFreeDoc(doc);
        fragment.ok_or_else(|| DomError::dom(C::InvalidState))
    }
}

/// `dom_parse_fragment_ex`.
fn parse_fragment_ex(this: &Node, context: N, input: &[u8], is_template: bool) -> DomResult<N> {
    if context.doc().is_some_and(|d| d.t() == ffi::XML_DOCUMENT_NODE) {
        xml_fragment_parsing_algorithm(this, context, input)
    } else {
        html_fragment_parsing_algorithm(this, context, input, is_template)
    }
}

/// `dom_parse_fragment`.
pub(crate) fn parse_fragment(this: &Node, context: N, input: &[u8]) -> DomResult<N> {
    parse_fragment_ex(this, context, input, false)
}

/// `$innerHTML = $value`.
pub(crate) fn set_inner_html(n: &Node, value: &[u8]) -> DomResult<()> {
    let mut context = n.prop_node()?;
    let is_template = ns_is_fast(context, &HTML_TOKEN) && context.name() == b"template";
    let fragment = parse_fragment_ex(n, context, value, is_template)?;
    let doc_ref = n.doc_ref();
    if is_template {
        let private = doc_ref.as_ref().and_then(|d| d.private_data());
        match private.and_then(|p| p.ensure_template(context)) {
            Some(c) => context = c,
            None => {
                crate::tree::free_node(fragment);
                return Err(DomError::dom(C::InvalidState));
            }
        }
    }
    if let Some(d) = &doc_ref {
        d.invalidate();
    }
    crate::object::remove_all_children(context);
    crate::tree::pre_insert(crate::tree::Ctx { doc: doc_ref.as_ref() }, fragment, context, None)
}

/// `$outerHTML = $value`.
pub(crate) fn set_outer_html(n: &Node, value: &[u8]) -> DomResult<()> {
    let this = n.prop_node()?;
    let Some(mut parent) = this.parent() else { return Ok(()) };
    if matches!(parent.t(), ffi::XML_DOCUMENT_NODE | ffi::XML_HTML_DOCUMENT_NODE) {
        return Err(DomError::dom(C::InvalidModification));
    }
    let doc_ref = n.doc_ref();
    let mut created = None;
    if parent.t() == ffi::XML_DOCUMENT_FRAG_NODE {
        let html_ns = doc_ref
            .as_ref()
            .and_then(|d| d.private_data())
            .map_or(ptr::null_mut(), |p| p.mapper.borrow_mut().ensure_html_ns());
        // SAFETY: live document.
        let body = N::new(unsafe { ffi::xmlNewDocNode(parent.doc_ptr(), html_ns, c"body".as_ptr().cast(), ptr::null()) })
            .ok_or_else(|| DomError::dom(C::InvalidState))?;
        created = Some(body);
        parent = body;
    }
    let free_created = |c: Option<N>| {
        if let Some(c) = c {
            // SAFETY: our own detached element.
            unsafe { ffi::xmlFreeNode(c.p()) };
        }
    };
    let fragment = match parse_fragment(n, parent, value) {
        Ok(f) => f,
        Err(e) => {
            free_created(created);
            return Err(e);
        }
    };
    if let Some(d) = &doc_ref {
        d.invalidate();
    }
    let real_parent = this.parent().expect("checked above");
    if let Err(e) = crate::tree::pre_insert(crate::tree::Ctx { doc: doc_ref.as_ref() }, fragment, real_parent, Some(this)) {
        // SAFETY: the fragment was not inserted.
        unsafe { ffi::xmlFreeNode(fragment.p()) };
        free_created(created);
        return Err(e);
    }
    // SAFETY: live node.
    unsafe { ffi::xmlUnlinkNode(this.p()) };
    free_created(created);
    Ok(())
}

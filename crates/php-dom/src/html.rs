//! `Dom\HTMLDocument` (`html_document.c`): HTML5 parsing through lexbor,
//! HTML5 serialization, and the `body`/`head`/`title` accessors.

use std::io::{Read, Write};
use std::rc::Rc;

use crate::error::{DomError, DomExceptionCode as C, DomResult};
use crate::ffi;
use crate::host::DomHost;
use crate::html5_parser::convert_document;
use crate::html5_serializer::Html5Serializer;
use crate::lexbor::{Encoding, HtmlParser, Observations};
use crate::ns::{ns_is_fast, PrivateData, HTML_TOKEN, SVG_NS, SVG_TOKEN};
use crate::object::{Node, QuirksMode};
use crate::raw::{cs, N};

/// `HTML_PARSE_NOERROR`.
const HTML_PARSE_NOERROR: i64 = 1 << 5;
/// `HTML_PARSE_NOIMPLIED`.
const HTML_PARSE_NOIMPLIED: i64 = 1 << 13;
/// `XML_PARSE_COMPACT`.
const XML_PARSE_COMPACT: i64 = 1 << 16;

/// `php_dom_create_html_doc`.
pub(crate) fn create_html_doc() -> Option<N> {
    // SAFETY: fresh document with its own dictionary.
    unsafe {
        let d = ffi::htmlNewDocNoDtD(std::ptr::null(), std::ptr::null());
        let n = N::from_doc(d)?;
        n.d().dict = ffi::xmlDictCreate();
        Some(n)
    }
}

fn set_doc_encoding(doc: N, name: &[u8]) {
    let d = doc.d();
    if !d.encoding.is_null() {
        crate::raw::xml_free(d.encoding.cast_mut().cast());
    }
    d.encoding = crate::raw::xml_strdup(name);
}

/// `dom_html_document_encoding_write` (`$charset` / `$characterSet` /
/// `$inputEncoding` of `Dom\Document`).
pub(crate) fn set_document_encoding(this: &Node, value: &[u8]) -> DomResult<()> {
    let doc = this.prop_node()?;
    match Encoding::by_name(value) {
        Some(e) => {
            set_doc_encoding(doc, e.name());
            Ok(())
        }
        None => Err(DomError::Value("Invalid document encoding".into())),
    }
}

fn instantiate(doc: N, private: Rc<PrivateData>, quirks: QuirksMode) -> Node {
    let (obj, d) = crate::document::instantiate_modern(doc.as_doc());
    d.quirks_mode.set(quirks);
    *d.private.borrow_mut() = Some(private);
    obj
}

/// `Dom\HTMLDocument::createEmpty($encoding)`.
pub(crate) fn create_empty(encoding: &[u8]) -> DomResult<Node> {
    php_libxml::raw::ensure_init();
    if Encoding::by_name(encoding).is_none() {
        return Err(DomError::arg_value(1, "must be a valid document encoding"));
    }
    let doc = create_html_doc().ok_or_else(|| DomError::dom(C::InvalidState))?;
    set_doc_encoding(doc, encoding);
    Ok(instantiate(doc, PrivateData::create(), QuirksMode::NoQuirks))
}

fn check_options(options: i64) -> DomResult<()> {
    let valid = HTML_PARSE_NOERROR | XML_PARSE_COMPACT | HTML_PARSE_NOIMPLIED | crate::constants::HTML_NO_DEFAULT_NS;
    if options & !valid != 0 {
        return Err(DomError::arg_value(
            2,
            "contains invalid flags (allowed flags: LIBXML_NOERROR, LIBXML_COMPACT, LIBXML_HTML_NOIMPLIED, Dom\\HTML_NO_DEFAULT_NS)",
        ));
    }
    Ok(())
}

/// The error reporter: `php_libxml_pretend_ctx_error_ex()` with lexbor's
/// error codes.
fn error_sink(input_name: Vec<u8>) -> crate::lexbor::ErrorSink<'static> {
    Box::new(move |is_tree, code, line, column, len| {
        let name = String::from_utf8_lossy(&input_name);
        let msg = if !is_tree {
            format!("tokenizer error {code} in {name}, line: {line}, column: {column}\n")
        } else if len <= 1 {
            format!("tree error {code} in {name}, line: {line}, column: {column}\n")
        } else {
            format!("tree error {code} in {name}, line: {line}, column: {column}-{}\n", column + len - 1)
        };
        php_libxml::error::pretend_ctx_error(
            &input_name,
            i32::try_from(line).unwrap_or(i32::MAX),
            i32::try_from(column).unwrap_or(i32::MAX),
            msg.as_bytes(),
        );
    })
}

fn search_child(parent: N, name: &[u8]) -> Option<N> {
    parent.child_iter().find(|n| n.is_element() && n.name() == name)
}

/// `dom_place_remove_element_and_hoist_children`.
fn remove_element_and_hoist_children(parent: N, name: &[u8]) {
    let Some(node) = search_child(parent, name) else { return };
    // SAFETY: nodes of a document only we hold so far.
    unsafe {
        ffi::xmlUnlinkNode(node.p());
        while let Some(child) = node.children() {
            ffi::xmlUnlinkNode(child.p());
            ffi::xmlAddChild(parent.p(), child.p());
        }
        ffi::xmlFreeNode(node.p());
    }
}

/// `dom_post_process_html5_loading`.
fn post_process(doc: N, options: i64, observations: &Observations) {
    if options & HTML_PARSE_NOIMPLIED != 0 {
        if let Some(html) = search_child(doc, b"html") {
            if !observations.has_explicit_head_tag {
                remove_element_and_hoist_children(html, b"head");
            }
            if !observations.has_explicit_body_tag {
                remove_element_and_hoist_children(html, b"body");
            }
        }
        if !observations.has_explicit_html_tag {
            remove_element_and_hoist_children(doc, b"html");
        }
    }
}

fn quirks_of(o: &Observations) -> QuirksMode {
    match o.quirks_mode {
        1 => QuirksMode::LimitedQuirks,
        2 => QuirksMode::Quirks,
        _ => QuirksMode::NoQuirks,
    }
}

/// Bridges, post-processes and instantiates a finished parse.
fn finish(parser: &HtmlParser<'_>, options: i64, input_name: &[u8]) -> DomResult<(N, Rc<PrivateData>, QuirksMode)> {
    let private = PrivateData::create();
    let bridged = convert_document(
        parser.document_last_child(),
        options & XML_PARSE_COMPACT != 0,
        options & crate::constants::HTML_NO_DEFAULT_NS == 0,
        &private,
    );
    let observations = parser.observations();
    let doc = match bridged {
        Ok(d) => d,
        Err(status) => {
            private.destroy();
            let msg = format!("{} in {}", status.message(), String::from_utf8_lossy(input_name));
            php_libxml::error::ctx_error(None, msg.as_bytes());
            return Err(DomError::Silent);
        }
    };
    post_process(doc, options, &observations);
    set_doc_encoding(doc, &parser.encoding_name());
    Ok((doc, private, quirks_of(&observations)))
}

/// `Dom\HTMLDocument::createFromString()` / `createFromFile()`.
pub(crate) fn create_from(source: &[u8], options: i64, override_encoding: Option<&[u8]>, from_file: bool, host: &dyn DomHost) -> DomResult<Node> {
    php_libxml::raw::ensure_init();
    if from_file {
        if source.is_empty() {
            return Err(DomError::Value("Path must not be empty".into()));
        }
        if source.contains(&0) {
            return Err(DomError::arg_value(1, "must not contain any null bytes"));
        }
        if source.windows(3).any(|w| w == b"%00") {
            return Err(DomError::arg_value(1, "must not contain percent-encoded NUL bytes"));
        }
    }
    check_options(options)?;
    let override_enc = match override_encoding {
        Some(e) => Some(Encoding::by_name(e).ok_or_else(|| DomError::arg_value(3, "must be a valid document encoding"))?),
        None => None,
    };
    let input_name: Vec<u8> = if from_file { source.to_vec() } else { b"Entity".to_vec() };
    let sink = (options & HTML_PARSE_NOERROR == 0).then(|| error_sink(input_name.clone()));

    if !from_file {
        let mut parser = HtmlParser::new(override_enc, options & HTML_PARSE_NOIMPLIED != 0, sink)
            .ok_or_else(|| DomError::dom(C::InvalidState))?;
        if !parser.parse_string(source) {
            return Err(DomError::dom(C::InvalidState));
        }
        let (doc, private, quirks) = finish(&parser, options, &input_name)?;
        return Ok(instantiate(doc, private, quirks));
    }

    let Some(mut stream) = host.open_read_stream(source)? else {
        return Err(DomError::Exception(format!("Cannot open file '{}'", String::from_utf8_lossy(source))));
    };
    let mut parser = HtmlParser::new(override_enc, options & HTML_PARSE_NOIMPLIED != 0, sink)
        .ok_or_else(|| DomError::dom(C::InvalidState))?;
    if override_enc.is_none() {
        if let Some(e) = stream.charset.as_deref().and_then(Encoding::by_name) {
            parser.set_encoding(e);
        }
    }
    let mut buf = [0u8; 4096];
    loop {
        let read = match stream.reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if !parser.feed_file_chunk(&buf[..read]) {
            return Err(DomError::dom(C::InvalidState));
        }
    }
    if !parser.finish_file() {
        return Err(DomError::dom(C::InvalidState));
    }
    let (doc, private, quirks) = finish(&parser, options, &input_name)?;
    drop(parser);
    let url = match &stream.plain_file_path {
        Some(path) => {
            let p = cs(path);
            // SAFETY: valid C string; result owned by us.
            let converted = crate::raw::take(unsafe { ffi::xmlPathToURI(p.as_ptr().cast()) }).unwrap_or_else(|| path.clone());
            if converted.starts_with(b"file:/") {
                converted
            } else {
                let mut v = b"file://".to_vec();
                v.extend_from_slice(&converted);
                v
            }
        }
        None => source.to_vec(),
    };
    doc.d().URL = crate::raw::xml_strdup(&url);
    Ok(instantiate(doc, private, quirks))
}

/// `saveXmlFile()` (same as `DOMDocument::save()`).
pub(crate) fn save_xml_file(n: &Node, filename: &[u8], options: i64) -> DomResult<Option<i64>> {
    n.save(filename, options)
}

/// `dom_common_save`: HTML5 serialization in the document's encoding.
fn common_save(this: &Node, doc: N, node: N) -> Vec<u8> {
    let private = this.doc_ref().and_then(|d| d.private_data());
    let mut s = Html5Serializer::new(private.as_deref());
    s.serialize_outer(node);
    let utf8 = s.finish();
    let enc = crate::raw::opt_cstr(doc.d().encoding).and_then(Encoding::by_name);
    match enc {
        Some(e) => e.encode_from_utf8(&utf8),
        None => utf8,
    }
}

/// `Dom\HTMLDocument::saveHtml($node)`.
pub(crate) fn save_html(this: &Node, node: Option<&Node>) -> DomResult<Vec<u8>> {
    let doc = this.node()?;
    let target = match node {
        Some(n) => {
            let nn = n.node()?;
            if nn.doc_ptr() != doc.as_doc() {
                return Err(DomError::dom(C::WrongDocument));
            }
            nn
        }
        None => doc,
    };
    Ok(common_save(this, doc, target))
}

/// `Dom\HTMLDocument::saveHtmlFile($filename)`.
pub(crate) fn save_html_file(this: &Node, filename: &[u8], host: &dyn DomHost) -> DomResult<Option<i64>> {
    if filename.is_empty() {
        return Err(DomError::arg_value(1, "must not be empty"));
    }
    let Some(mut stream) = host.open_write_stream(filename)? else {
        return Ok(None);
    };
    let doc = this.node()?;
    let data = common_save(this, doc, doc);
    if stream.write_all(&data).is_err() || stream.flush().is_err() {
        return Ok(None);
    }
    Ok(Some(i64::try_from(data.len()).unwrap_or(i64::MAX)))
}

/// `debugGetTemplateCount()`.
pub(crate) fn template_count(n: &Node) -> i64 {
    n.doc_ref()
        .and_then(|d| d.private_data())
        .map_or(0, |p| i64::try_from(p.template_count()).unwrap_or(i64::MAX))
}

fn document_element(doc: N) -> Option<N> {
    // SAFETY: live document.
    N::new(unsafe { ffi::xmlDocGetRootElement(doc.as_doc()) })
}

fn accept_body(name: &[u8]) -> bool {
    name == b"body" || name == b"frameset"
}

fn accept_head(name: &[u8]) -> bool {
    name == b"head"
}

/// `dom_html_document_element_read_raw`.
fn element_read_raw(doc: N, accept: fn(&[u8]) -> bool) -> Option<N> {
    let root = document_element(doc)?;
    if !(ns_is_fast(root, &HTML_TOKEN) && root.name() == b"html") {
        return None;
    }
    root.child_iter()
        .find(|c| c.is_element() && ns_is_fast(*c, &HTML_TOKEN) && accept(c.name()))
}

/// `$body`.
pub(crate) fn body(n: &Node) -> DomResult<Option<Node>> {
    Ok(element_read_raw(n.prop_node()?, accept_body).map(Node::wrap))
}

/// `$head`.
pub(crate) fn head(n: &Node) -> DomResult<Option<Node>> {
    Ok(element_read_raw(n.prop_node()?, accept_head).map(Node::wrap))
}

/// `$body = $value`.
pub(crate) fn set_body(n: &Node, value: Option<&Node>) -> DomResult<()> {
    let doc = n.prop_node()?;
    let fail = || DomError::dom_msg(C::HierarchyRequest, "The new body must either be a body or a frameset tag");
    let Some(v) = value else { return Err(fail()) };
    let Some(newval) = v.n() else { return Err(fail()) };
    if !accept_body(newval.name()) {
        return Err(fail());
    }
    let current = element_read_raw(doc, accept_body);
    if current == Some(newval) {
        return Ok(());
    }
    let doc_ref = n.doc_ref();
    if let Some(cur) = current {
        crate::document::adopt_node_raw(newval, doc_ref.as_ref(), doc.as_doc());
        // SAFETY: live nodes of the same document.
        let old = N::new(unsafe { ffi::xmlReplaceNode(cur.p(), newval.p()) });
        if let Some(old) = old {
            if old.private().is_null() {
                crate::object::free_resource(old);
            }
        }
        return Ok(());
    }
    let Some(root) = document_element(doc) else {
        return Err(DomError::dom_msg(C::HierarchyRequest, "A body can only be set if there is a document element"));
    };
    crate::document::adopt_node_raw(newval, doc_ref.as_ref(), doc.as_doc());
    // SAFETY: live nodes.
    unsafe { ffi::xmlAddChild(root.p(), newval.p()) };
    Ok(())
}

/// `dom_get_child_text_content`.
fn child_text_content(node: N) -> Vec<u8> {
    let mut out = Vec::new();
    for t in node.child_iter() {
        if matches!(t.t(), ffi::XML_TEXT_NODE | ffi::XML_CDATA_SECTION_NODE) && !t.content_ptr().is_null() {
            out.extend_from_slice(t.content());
        }
    }
    out
}

/// `dom_get_title_element`.
fn title_element(doc: N) -> Option<N> {
    let mut node = doc.children();
    while let Some(n) = node {
        if n.is_element() && ns_is_fast(n, &HTML_TOKEN) && n.name() == b"title" {
            return Some(n);
        }
        node = crate::tree::next_in_tree_order(n, None);
    }
    None
}

/// `dom_get_svg_title_element`.
fn svg_title_element(svg: N) -> Option<N> {
    svg.child_iter()
        .find(|c| c.is_element() && ns_is_fast(*c, &SVG_TOKEN) && c.name() == b"title")
}

/// `dom_strip_and_collapse_ascii_whitespace`.
pub(crate) fn strip_and_collapse_ascii_whitespace(input: &[u8]) -> Vec<u8> {
    let ws = |c: &u8| matches!(c, b'\t' | b'\n' | 0x0c | b'\r' | b' ');
    let mut out = Vec::with_capacity(input.len());
    for word in input.split(ws).filter(|w| !w.is_empty()) {
        if !out.is_empty() {
            out.push(b' ');
        }
        out.extend_from_slice(word);
    }
    out
}

/// `$title`.
pub(crate) fn title(n: &Node) -> DomResult<Vec<u8>> {
    let doc = n.prop_node()?;
    let Some(root) = document_element(doc) else { return Ok(Vec::new()) };
    let value = if ns_is_fast(root, &SVG_TOKEN) && root.name() == b"svg" {
        svg_title_element(root).map(child_text_content).unwrap_or_default()
    } else {
        title_element(doc).map(child_text_content).unwrap_or_default()
    };
    Ok(strip_and_collapse_ascii_whitespace(&value))
}

/// `dom_string_replace_all`.
fn string_replace_all(doc: N, element: N, value: &[u8]) {
    crate::object::remove_all_children(element);
    let v = cs(value);
    // SAFETY: live document and element.
    unsafe {
        let text = ffi::xmlNewDocText(doc.as_doc(), v.as_ptr().cast());
        ffi::xmlAddChild(element.p(), text);
    }
}

/// `$title = $value`.
pub(crate) fn set_title(n: &Node, value: &[u8]) -> DomResult<()> {
    let doc = n.prop_node()?;
    let Some(root) = document_element(doc) else { return Ok(()) };
    let mapper_ns = |prefix: Option<&[u8]>, uri: &[u8]| -> *mut ffi::xmlNs {
        n.doc_ref()
            .and_then(|d| d.private_data())
            .map_or(std::ptr::null_mut(), |p| p.mapper.borrow_mut().get_ns(prefix, Some(uri)))
    };
    if ns_is_fast(root, &SVG_TOKEN) && root.name() == b"svg" {
        let element = match svg_title_element(root) {
            Some(e) => e,
            None => {
                let mut ns = root.ns_ptr();
                if root.ns().and_then(crate::raw::Ns::prefix).is_some() {
                    ns = mapper_ns(Some(b""), SVG_NS);
                }
                // SAFETY: live document; the name is a static C string.
                let el = N::new(unsafe { ffi::xmlNewDocNode(doc.as_doc(), ns, c"title".as_ptr().cast(), std::ptr::null()) })
                    .ok_or_else(|| DomError::dom(C::InvalidState))?;
                match root.children() {
                    None => root.set_last(Some(el)),
                    Some(first) => {
                        el.set_next(Some(first));
                        first.set_prev(Some(el));
                    }
                }
                root.set_children(Some(el));
                el.set_parent(Some(root));
                el
            }
        };
        string_replace_all(doc, element, value);
    } else if ns_is_fast(root, &HTML_TOKEN) {
        let title = title_element(doc);
        let head = element_read_raw(doc, accept_head);
        let element = match (title, head) {
            (None, None) => return Ok(()),
            (Some(t), _) => t,
            (None, Some(head)) => {
                let html_ns = n
                    .doc_ref()
                    .and_then(|d| d.private_data())
                    .map_or(std::ptr::null_mut(), |p| p.mapper.borrow_mut().ensure_html_ns());
                // SAFETY: live document.
                let el = N::new(unsafe { ffi::xmlNewDocNode(doc.as_doc(), html_ns, c"title".as_ptr().cast(), std::ptr::null()) })
                    .ok_or_else(|| DomError::dom(C::InvalidState))?;
                // SAFETY: live nodes.
                unsafe { ffi::xmlAddChild(head.p(), el.p()) };
                el
            }
        };
        string_replace_all(doc, element, value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_collapse() {
        assert_eq!(strip_and_collapse_ascii_whitespace(b"  a \t\n b  c "), b"a b c");
        assert_eq!(strip_and_collapse_ascii_whitespace(b""), b"");
        assert_eq!(strip_and_collapse_ascii_whitespace(b" \n "), b"");
    }
}

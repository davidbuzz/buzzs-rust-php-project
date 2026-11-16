// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Tree helpers shared by all classes (`php_dom.c`, `parentnode/tree.c`).

use std::ptr;

use crate::error::{throw, DomError, DomExceptionCode as C, DomResult};
use crate::ffi;
use crate::ns;
use crate::object::{node_list_unlink, set_old_ns, DocRef, Node};
use crate::raw::{cs, str_eq, take, Ns, N};

/// `php_dom_next_in_tree_order`.
pub(crate) fn next_in_tree_order(node: N, base: Option<N>) -> Option<N> {
    if node.is_element() {
        if let Some(c) = node.children() {
            return Some(c);
        }
    }
    if let Some(n) = node.next() {
        return Some(n);
    }
    let mut cur = node;
    loop {
        cur = cur.parent()?;
        if Some(cur) == base {
            return None;
        }
        if let Some(n) = cur.next() {
            return Some(n);
        }
    }
}

/// `dom_node_is_read_only`.
pub(crate) fn is_read_only(n: N) -> bool {
    match n.t() {
        ffi::XML_ENTITY_REF_NODE
        | ffi::XML_ENTITY_NODE
        | ffi::XML_DOCUMENT_TYPE_NODE
        | ffi::XML_NOTATION_NODE
        | ffi::XML_DTD_NODE
        | ffi::XML_ELEMENT_DECL
        | ffi::XML_ATTRIBUTE_DECL
        | ffi::XML_ENTITY_DECL
        | ffi::XML_NAMESPACE_DECL => true,
        _ => n.doc_ptr().is_null(),
    }
}

/// `dom_node_children_valid`.
pub(crate) fn children_valid(n: N) -> bool {
    !matches!(
        n.t(),
        ffi::XML_DOCUMENT_TYPE_NODE
            | ffi::XML_DTD_NODE
            | ffi::XML_PI_NODE
            | ffi::XML_COMMENT_NODE
            | ffi::XML_TEXT_NODE
            | ffi::XML_CDATA_SECTION_NODE
            | ffi::XML_NOTATION_NODE
    )
}

/// `dom_hierarchy`: false when inserting `child` into `parent` would create
/// a cycle (or `child` is a document).
pub(crate) fn hierarchy_ok(parent: N, child: N) -> bool {
    if child.doc_ptr() != parent.doc_ptr() {
        return true;
    }
    if child.t() == ffi::XML_DOCUMENT_NODE {
        return false;
    }
    let mut cur = Some(parent);
    while let Some(c) = cur {
        if c == child {
            return false;
        }
        cur = c.parent();
    }
    true
}

/// `php_libxml_attr_value`.
pub(crate) fn attr_value(attr: N) -> Vec<u8> {
    let Some(child) = attr.children() else {
        return Vec::new();
    };
    if child.t() == ffi::XML_TEXT_NODE && child.next().is_none() {
        return child.content().to_vec();
    }
    // SAFETY: live attribute.
    take(unsafe { ffi::xmlNodeGetContent(attr.p()) }).unwrap_or_default()
}

/// `php_dom_get_content_into_zval`: `None` means "failure" (PHP returns
/// NULL or "" depending on the caller).
pub(crate) fn get_content(n: N) -> Option<Vec<u8>> {
    match n.t() {
        ffi::XML_TEXT_NODE
        | ffi::XML_CDATA_SECTION_NODE
        | ffi::XML_PI_NODE
        | ffi::XML_COMMENT_NODE => {
            if n.content_ptr().is_null() {
                None
            } else {
                Some(n.content().to_vec())
            }
        }
        ffi::XML_ATTRIBUTE_NODE => Some(attr_value(n)),
        // SAFETY: live node.
        _ => take(unsafe { ffi::xmlNodeGetContent(n.p()) }),
    }
}

/// `xmlNodeGetContent` (owned copy).
pub(crate) fn node_get_content(n: N) -> Option<Vec<u8>> {
    // SAFETY: live node.
    take(unsafe { ffi::xmlNodeGetContent(n.p()) })
}

/// `php_dom_is_node_connected`.
pub(crate) fn is_connected(n: N) -> bool {
    let mut cur = Some(n);
    while let Some(c) = cur {
        if c.is_doc() {
            return true;
        }
        cur = c.parent();
    }
    false
}

/// `dom_node_get_node_name_attribute_or_element`.
pub(crate) fn qualified_name(n: N, uppercase: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(prefix) = n.ns().and_then(Ns::prefix) {
        out.extend_from_slice(prefix);
        out.push(b':');
    }
    out.extend_from_slice(n.name());
    if uppercase {
        out.make_ascii_uppercase();
    }
    out
}

/// `php_dom_has_child_of_type`.
pub(crate) fn has_child_of_type(n: N, t: i32) -> bool {
    n.child_iter().any(|c| c.t() == t)
}

/// `php_dom_has_sibling_following_node`.
pub(crate) fn has_following_sibling_of_type(n: N, t: i32) -> bool {
    let mut cur = n.next();
    while let Some(c) = cur {
        if c.t() == t {
            return true;
        }
        cur = c.next();
    }
    false
}

/// `php_dom_has_sibling_preceding_node`.
pub(crate) fn has_preceding_sibling_of_type(n: N, t: i32) -> bool {
    let mut cur = n.prev();
    while let Some(c) = cur {
        if c.t() == t {
            return true;
        }
        cur = c.prev();
    }
    false
}

/// `php_dom_first_child_of_container_node`.
pub(crate) fn first_child_of_container(parent: N) -> Option<N> {
    if parent.is_doc() {
        // SAFETY: live document.
        N::new(unsafe { ffi::xmlDocGetRootElement(parent.as_doc()) })
    } else {
        parent.children()
    }
}

/// `dom_reconcile_ns_internal`.
fn reconcile_ns_internal(doc: *mut ffi::xmlDoc, node: N, search_parent: Option<N>) {
    let mut prevns: Option<Ns> = None;
    let mut cur = node.ns_def();
    while let Some(curns) = cur {
        let nsdftptr = curns.next();
        let mut kept = true;
        if !curns.href_ptr().is_null() {
            // SAFETY: live tree; search_parent may be NULL.
            let found = Ns::new(unsafe {
                ffi::xmlSearchNsByHref(doc, crate::raw::raw(search_parent), curns.href_ptr())
            });
            if let Some(nsptr) = found {
                if curns.prefix_ptr().is_null() || str_eq(nsptr.prefix(), curns.prefix()) {
                    curns.set_next(ptr::null_mut());
                    match prevns {
                        None => node.set_ns_def(nsdftptr.map_or(ptr::null_mut(), Ns::p)),
                        Some(p) => p.set_next(nsdftptr.map_or(ptr::null_mut(), Ns::p)),
                    }
                    set_old_ns(doc, curns.p());
                    kept = false;
                }
            }
        }
        if kept {
            prevns = Some(curns);
        }
        cur = nsdftptr;
    }
}

/// `dom_reconcile_ns`.
pub(crate) fn reconcile_ns(doc: *mut ffi::xmlDoc, node: N) {
    if node.is_element() {
        reconcile_ns_internal(doc, node, node.parent());
        // SAFETY: live element.
        unsafe { ffi::xmlReconciliateNs(doc, node.p()) };
    }
}

fn reconcile_ns_list_internal(doc: *mut ffi::xmlDoc, first: N, last: N, search_parent: Option<N>) {
    let mut node = first;
    loop {
        if node.is_element() {
            reconcile_ns_internal(doc, node, search_parent);
            if let (Some(c), Some(l)) = (node.children(), node.last()) {
                reconcile_ns_list_internal(doc, c, l, search_parent);
            }
        }
        if node == last {
            break;
        }
        match node.next() {
            Some(n) => node = n,
            None => break,
        }
    }
}

/// `dom_reconcile_ns_list`.
pub(crate) fn reconcile_ns_list(doc: *mut ffi::xmlDoc, first: N, last: N) {
    reconcile_ns_list_internal(doc, first, last, first.parent());
    let mut node = first;
    loop {
        // SAFETY: live node (libxml2 checks the node type).
        unsafe { ffi::xmlReconciliateNs(doc, node.p()) };
        if node == last {
            break;
        }
        match node.next() {
            Some(n) => node = n,
            None => break,
        }
    }
}

/// `xmlSplitQName2`: `(local, prefix)` or `None` when there is no prefix.
pub(crate) fn split_qname2(qname: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let c = cs(qname);
    let mut prefix: *mut u8 = ptr::null_mut();
    // SAFETY: valid C string; outputs are freshly allocated.
    let local = unsafe { ffi::xmlSplitQName2(c.as_ptr().cast(), &raw mut prefix) };
    let local = take(local)?;
    let prefix = take(prefix).unwrap_or_default();
    Some((local, prefix))
}

/// `xmlValidateQName(name, 0) == 0`.
pub(crate) fn valid_qname(q: &[u8]) -> bool {
    let c = cs(q);
    // SAFETY: valid C string.
    unsafe { ffi::xmlValidateQName(c.as_ptr().cast(), 0) == 0 }
}

/// `xmlValidateName(name, 0) == 0`.
pub(crate) fn valid_name(q: &[u8]) -> bool {
    let c = cs(q);
    // SAFETY: valid C string.
    unsafe { ffi::xmlValidateName(c.as_ptr().cast(), 0) == 0 }
}

/// `xmlValidateNCName(name, 0) == 0`.
pub(crate) fn valid_ncname(q: &[u8]) -> bool {
    let c = cs(q);
    // SAFETY: valid C string.
    unsafe { ffi::xmlValidateNCName(c.as_ptr().cast(), 0) == 0 }
}

/// `dom_validate_and_extract`: `(local, prefix)` or an error code.
pub(crate) fn validate_and_extract(
    namespace: Option<&[u8]>,
    qname: &[u8],
) -> Result<(Vec<u8>, Option<Vec<u8>>), C> {
    let namespace = namespace.unwrap_or_default();
    if !valid_qname(qname) {
        return Err(C::InvalidCharacter);
    }
    let (local, prefix) = match split_qname2(qname) {
        Some((l, p)) => (Some(l), Some(p)),
        None => (None, None),
    };
    if prefix.is_some() && namespace.is_empty() {
        return Err(C::Namespace);
    }
    if namespace != ns::XML_NS && prefix.as_deref() == Some(b"xml") {
        return Err(C::Namespace);
    }
    if (qname == b"xmlns" || prefix.as_deref() == Some(b"xmlns")) && namespace != ns::XMLNS_NS {
        return Err(C::Namespace);
    }
    if namespace == ns::XMLNS_NS && qname != b"xmlns" && prefix.as_deref() != Some(b"xmlns") {
        return Err(C::Namespace);
    }
    Ok((local.unwrap_or_else(|| qname.to_vec()), prefix))
}

/// `dom_check_qname`: `(local, prefix)` or `NAMESPACE_ERR`.
pub(crate) fn check_qname(qname: &[u8], uri_len: usize) -> Result<(Vec<u8>, Option<Vec<u8>>), C> {
    if qname.is_empty() {
        return Err(C::Namespace);
    }
    let (local, prefix) = if let Some((l, p)) = split_qname2(qname) {
        (l, Some(p))
    } else {
        if uri_len == 0 {
            return Ok((qname.to_vec(), None));
        }
        (qname.to_vec(), None)
    };
    if !valid_qname(qname) {
        return Err(C::Namespace);
    }
    if prefix.is_some() && uri_len == 0 {
        return Err(C::Namespace);
    }
    Ok((local, prefix))
}

/// `dom_get_ns_resolve_prefix_conflict`.
pub(crate) fn get_ns_resolve_prefix_conflict(tree: N, uri: &[u8]) -> *mut ffi::xmlNs {
    let doc = tree.doc_ptr();
    if doc.is_null() {
        return ptr::null_mut();
    }
    let mut counter = 1;
    let mut prefix = b"default".to_vec();
    loop {
        let c = cs(&prefix);
        // SAFETY: live tree.
        let found = unsafe { ffi::xmlSearchNs(doc, tree.p(), c.as_ptr().cast()) };
        if found.is_null() {
            break;
        }
        if counter > 1000 {
            return ptr::null_mut();
        }
        prefix = format!("default{counter}").into_bytes();
        counter += 1;
    }
    let u = cs(uri);
    let p = cs(&prefix);
    // SAFETY: live element.
    unsafe { ffi::xmlNewNs(tree.p(), u.as_ptr().cast(), p.as_ptr().cast()) }
}

/// `dom_get_ns_unchecked`.
pub(crate) fn get_ns_unchecked(node: N, uri: &[u8], prefix: Option<&[u8]>) -> *mut ffi::xmlNs {
    let u = cs(uri);
    let p = prefix.map(cs);
    // SAFETY: live element; strings valid.
    let ns = unsafe {
        ffi::xmlNewNs(
            node.p(),
            u.as_ptr().cast(),
            p.as_ref().map_or(ptr::null(), |p| p.as_ptr().cast()),
        )
    };
    if ns.is_null() {
        return get_ns_resolve_prefix_conflict(node, uri);
    }
    ns
}

/// `dom_get_ns`: `Err(())` means `NAMESPACE_ERR`.
pub(crate) fn get_ns(node: N, uri: &[u8], prefix: Option<&[u8]>) -> Result<*mut ffi::xmlNs, ()> {
    let bad = match prefix {
        Some(p) => {
            (p == b"xml" && uri != ffi::XML_XML_NAMESPACE)
                || (p == b"xmlns" && uri != ns::XMLNS_NS)
                || (uri == ns::XMLNS_NS && p != b"xmlns")
        }
        None => false,
    };
    if bad {
        return Err(());
    }
    let ns = get_ns_unchecked(node, uri, prefix);
    if ns.is_null() {
        Err(())
    } else {
        Ok(ns)
    }
}

/// `dom_get_nsdecl`.
pub(crate) fn get_nsdecl(node: N, local: Option<&[u8]>) -> Option<Ns> {
    let mut cur = node.ns_def();
    let want_default = local.is_none_or(<[u8]>::is_empty);
    while let Some(c) = cur {
        if want_default {
            if c.prefix_ptr().is_null() && !c.href_ptr().is_null() {
                return Some(c);
            }
        } else if c.prefix().is_some_and(|p| Some(p) == local) {
            return Some(c);
        }
        cur = c.next();
    }
    None
}

/// `dom_match_qualified_name_according_to_spec`.
pub(crate) fn match_qualified_name(qname: &[u8], node: N) -> bool {
    match node.ns().and_then(Ns::prefix) {
        Some(prefix) => {
            qname.len() > prefix.len()
                && qname.starts_with(prefix)
                && qname[prefix.len()] == b':'
                && &qname[prefix.len() + 1..] == node.name()
        }
        None => node.name() == qname,
    }
}

/// `php_dom_get_attribute_node`: spec-compliant qualified-name attribute
/// lookup (lowercasing for HTML elements in HTML documents).
pub(crate) fn get_attribute_node(elem: N, name: &[u8]) -> Option<N> {
    let lowered;
    let name = if ns::is_html_and_document_is_html(elem) {
        lowered = name.to_ascii_lowercase();
        &lowered[..]
    } else {
        name
    };
    elem.attr_iter().find(|a| match_qualified_name(name, *a))
}

/// `dom_set_attribute_value`.
pub(crate) fn set_attribute_value(
    element: N,
    ns: *mut ffi::xmlNs,
    name: &[u8],
    value: &[u8],
) -> Option<N> {
    let n = cs(name);
    let v = cs(value);
    // SAFETY: live element, valid strings.
    unsafe {
        let href = if ns.is_null() {
            ptr::null()
        } else {
            (*ns).href
        };
        let existing = ffi::xmlHasNsProp(element.p(), n.as_ptr().cast(), href);
        if let Some(e) = N::from_attr(existing) {
            if e.t() != ffi::XML_ATTRIBUTE_DECL {
                node_list_unlink(e.children());
            }
        }
        N::from_attr(ffi::xmlSetNsProp(
            element.p(),
            ns,
            n.as_ptr().cast(),
            v.as_ptr().cast(),
        ))
    }
}

/// Appends without text merging (`dom_add_child_without_merging`).
pub(crate) fn add_child_without_merging(parent: N, child: N) {
    match parent.last() {
        None => parent.set_children(Some(child)),
        Some(last) => {
            last.set_next(Some(child));
            child.set_prev(Some(last));
        }
    }
    parent.set_last(Some(child));
    child.set_parent(Some(parent));
}

/// `xmlUnlinkNode`.
pub(crate) fn unlink(n: N) {
    // SAFETY: live node.
    unsafe { ffi::xmlUnlinkNode(n.p()) };
}

/// `xmlFreeNode` of a handle-less temporary node.
pub(crate) fn free_node(n: N) {
    // SAFETY: n is a live, unlinked node without handles.
    unsafe { ffi::xmlFreeNode(n.p()) };
}

/// Releases a node struct without freeing its children (`xmlFree(node)`).
pub(crate) fn free_shell(n: N) {
    crate::raw::xml_free(n.p().cast());
}

/// `php_dom_pre_insert_is_parent_invalid`.
pub(crate) fn pre_insert_parent_invalid(parent: N) -> bool {
    !matches!(
        parent.t(),
        ffi::XML_DOCUMENT_NODE
            | ffi::XML_HTML_DOCUMENT_NODE
            | ffi::XML_ELEMENT_NODE
            | ffi::XML_DOCUMENT_FRAG_NODE
    )
}

fn hierarchy_err(msg: &'static str) -> DomError {
    DomError::dom_msg(C::HierarchyRequest, msg)
}

fn fragment_common_check(node: N) -> DomResult<bool> {
    let mut seen_element = false;
    for c in node.child_iter() {
        if c.is_element() {
            if seen_element {
                return Err(hierarchy_err(
                    "Cannot have more than one element child in a document",
                ));
            }
            seen_element = true;
        } else if c.is_text_like() {
            return Err(hierarchy_err("Cannot insert text as a child of a document"));
        }
    }
    Ok(seen_element)
}

/// `php_dom_fragment_insertion_hierarchy_check_pre_insertion`.
pub(crate) fn fragment_check_pre_insertion(parent: N, node: N, child: Option<N>) -> DomResult<()> {
    if fragment_common_check(node)? {
        if has_child_of_type(parent, ffi::XML_ELEMENT_NODE) {
            return Err(hierarchy_err(
                "Cannot have more than one element child in a document",
            ));
        }
        if let Some(child) = child {
            if child.t() == ffi::XML_DTD_NODE
                || has_following_sibling_of_type(child, ffi::XML_DTD_NODE)
            {
                return Err(hierarchy_err(
                    "Document types must be the first child in a document",
                ));
            }
        }
    }
    Ok(())
}

/// `php_dom_fragment_insertion_hierarchy_check_replace`.
pub(crate) fn fragment_check_replace(parent: N, node: N, child: N) -> DomResult<()> {
    if fragment_common_check(node)? {
        if parent.child_iter().any(|c| c.is_element() && c != child) {
            return Err(hierarchy_err(
                "Cannot have more than one element child in a document",
            ));
        }
        if has_following_sibling_of_type(child, ffi::XML_DTD_NODE) {
            return Err(hierarchy_err(
                "Document types must be the first child in a document",
            ));
        }
    }
    Ok(())
}

/// Document-level context for the insertion algorithms.
#[derive(Clone, Copy)]
pub(crate) struct Ctx<'a> {
    pub doc: Option<&'a DocRef>,
}

impl Ctx<'_> {
    pub fn strict(self) -> bool {
        self.doc.is_none_or(|d| d.strict())
    }
    pub fn modern(self) -> bool {
        self.doc.is_some_and(|d| d.is_modern())
    }
    pub fn invalidate(self) {
        if let Some(d) = self.doc {
            d.invalidate();
        }
    }
}

/// `dom_is_pre_insert_valid_without_step_1`.
pub(crate) fn pre_insert_valid_without_step_1(
    ctx: Ctx<'_>,
    parent: N,
    node: N,
    child: Option<N>,
    document: *mut ffi::xmlDoc,
) -> DomResult<()> {
    if pre_insert_parent_invalid(parent) {
        return Err(throw(C::HierarchyRequest, ctx.strict()));
    }
    if node.doc_ptr() != document {
        return Err(throw(C::WrongDocument, ctx.strict()));
    }
    if let Some(child) = child {
        if child.parent() != Some(parent) {
            return Err(throw(C::NotFound, ctx.strict()));
        }
    }
    let parent_is_document = parent.is_doc();
    let t = node.t();
    if !hierarchy_ok(parent, node)
        || t == ffi::XML_ATTRIBUTE_NODE
        || (ctx.modern()
            && (t == ffi::XML_ENTITY_REF_NODE
                || t == ffi::XML_ENTITY_NODE
                || t == ffi::XML_NOTATION_NODE
                || t == ffi::XML_DOCUMENT_NODE
                || t == ffi::XML_HTML_DOCUMENT_NODE
                || t >= ffi::XML_ELEMENT_DECL))
    {
        return Err(throw(C::HierarchyRequest, ctx.strict()));
    }
    if ctx.modern() {
        if parent_is_document && node.is_text_like() {
            return Err(hierarchy_err("Cannot insert text as a child of a document"));
        }
        if !parent_is_document && t == ffi::XML_DTD_NODE {
            return Err(hierarchy_err(
                "Cannot insert a document type into anything other than a document",
            ));
        }
        if parent_is_document {
            if t == ffi::XML_DOCUMENT_FRAG_NODE {
                fragment_check_pre_insertion(parent, node, child)?;
            } else if t == ffi::XML_ELEMENT_NODE {
                if has_child_of_type(parent, ffi::XML_ELEMENT_NODE) {
                    return Err(hierarchy_err(
                        "Cannot have more than one element child in a document",
                    ));
                }
                if let Some(child) = child {
                    if child.t() == ffi::XML_DTD_NODE
                        || has_following_sibling_of_type(child, ffi::XML_DTD_NODE)
                    {
                        return Err(hierarchy_err(
                            "Document types must be the first child in a document",
                        ));
                    }
                }
            } else if t == ffi::XML_DTD_NODE {
                if has_child_of_type(parent, ffi::XML_DTD_NODE) {
                    return Err(hierarchy_err("Cannot have more than one document type"));
                }
                let bad = match child {
                    Some(c) => has_preceding_sibling_of_type(c, ffi::XML_ELEMENT_NODE),
                    None => has_child_of_type(parent, ffi::XML_ELEMENT_NODE),
                };
                if bad {
                    return Err(hierarchy_err(
                        "Document types must be the first child in a document",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn free_after_single_node_creation(node: N) {
    let mut cur = node.children();
    while let Some(c) = cur {
        let next = c.next();
        unlink(c);
        if c.private().is_null() {
            free_node(c);
        }
        cur = next;
    }
}

/// An argument of the variadic `append()`/`before()`/… methods.
#[derive(Debug, Clone)]
pub enum NodeOrString {
    /// A node object.
    Node(Node),
    /// A string (becomes a text node).
    Str(Vec<u8>),
    /// Any other value; the payload is PHP's type name for the error
    /// message (`zend_zval_type_name`, e.g. `"int"` or a class name).
    Invalid(String),
}

fn doc_from_context_node(n: N) -> *mut ffi::xmlDoc {
    if n.is_doc() {
        n.as_doc()
    } else {
        n.doc_ptr()
    }
}

fn cannot_create_temp_nodes() -> DomError {
    DomError::dom_msg(C::InvalidModification, "Unable to allocate temporary nodes")
}

fn new_text(doc: *mut ffi::xmlDoc, s: &[u8]) -> DomResult<N> {
    // SAFETY: doc may be NULL; s is valid for its length.
    N::new(unsafe { ffi::xmlNewDocTextLen(doc, s.as_ptr(), crate::raw::c_len(s.len())) })
        .ok_or_else(cannot_create_temp_nodes)
}

/// `dom_zvals_to_single_node`.
pub(crate) fn zvals_to_single_node(
    ctx: Ctx<'_>,
    context: N,
    nodes: &[NodeOrString],
) -> DomResult<N> {
    let document = doc_from_context_node(context);
    if nodes.len() == 1 {
        return match &nodes[0] {
            NodeOrString::Node(n) => n.n().ok_or(DomError::dom(C::InvalidState)),
            NodeOrString::Str(s) => new_text(document, s),
            NodeOrString::Invalid(_) => Err(DomError::dom(C::InvalidState)),
        };
    }
    // SAFETY: document may be NULL.
    let frag =
        N::new(unsafe { ffi::xmlNewDocFragment(document) }).ok_or_else(cannot_create_temp_nodes)?;
    let result = (|| {
        for item in nodes {
            match item {
                NodeOrString::Node(obj) => {
                    let Some(new_node) = obj.n() else {
                        return Err(DomError::dom(C::InvalidState));
                    };
                    pre_insert_valid_without_step_1(ctx, frag, new_node, None, document)?;
                    if new_node.parent().is_some() {
                        unlink(new_node);
                    }
                    if new_node.t() == ffi::XML_DOCUMENT_FRAG_NODE {
                        let mut c = new_node.children();
                        while let Some(child) = c {
                            let next = child.next();
                            unlink(child);
                            add_child_without_merging(frag, child);
                            c = next;
                        }
                    } else {
                        add_child_without_merging(frag, new_node);
                    }
                }
                NodeOrString::Str(s) => {
                    let t = new_text(document, s)?;
                    add_child_without_merging(frag, t);
                }
                NodeOrString::Invalid(_) => return Err(DomError::dom(C::InvalidState)),
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(frag),
        Err(e) => {
            free_after_single_node_creation(frag);
            free_shell(frag);
            Err(e)
        }
    }
}

/// `dom_sanity_check_node_list_types`.
pub(crate) fn sanity_check_node_list(nodes: &[NodeOrString], modern: bool) -> DomResult<()> {
    let ce = if modern { "Dom\\Node" } else { "DOMNode" };
    for (i, item) in nodes.iter().enumerate() {
        let arg = u32::try_from(i + 1).unwrap_or(u32::MAX);
        match item {
            NodeOrString::Node(n) => {
                if n.family_is_modern() != modern {
                    return Err(DomError::arg_type(
                        arg,
                        format!("must be of type {ce}|string, {} given", n.class_name()),
                    ));
                }
            }
            NodeOrString::Str(s) => {
                if s.len() > i32::MAX as usize {
                    return Err(DomError::arg_value(
                        arg,
                        format!("must be less than or equal to {} bytes long", i32::MAX),
                    ));
                }
            }
            NodeOrString::Invalid(ty) => {
                return Err(DomError::arg_type(
                    arg,
                    format!("must be of type {ce}|string, {ty} given"),
                ));
            }
        }
    }
    Ok(())
}

fn pre_insert_helper(insertion_point: Option<N>, parent: N, newchild: N, last: N) {
    match insertion_point {
        None => {
            if let Some(plast) = parent.last().filter(|_| parent.children().is_some()) {
                newchild.set_prev(Some(plast));
                plast.set_next(Some(newchild));
            } else {
                parent.set_children(Some(newchild));
            }
            parent.set_last(Some(last));
        }
        Some(ip) => {
            last.set_next(Some(ip));
            if let Some(prev) = ip.prev() {
                prev.set_next(Some(newchild));
                newchild.set_prev(Some(prev));
            }
            ip.set_prev(Some(last));
            if parent.children() == Some(ip) {
                parent.set_children(Some(newchild));
            }
        }
    }
}

fn fragment_assign_parent(parent: N, fragment: N) {
    let last = fragment.last();
    let mut cur = fragment.children();
    while let Some(c) = cur {
        c.set_parent(Some(parent));
        if Some(c) == last {
            break;
        }
        cur = c.next();
    }
}

/// `dom_insert_node_list_cleanup`.
pub(crate) fn insert_node_list_cleanup(node: N) {
    if !node.private().is_null() {
        return;
    }
    if node.t() == ffi::XML_DOCUMENT_FRAG_NODE {
        free_after_single_node_creation(node);
        free_shell(node);
    } else if node.t() == ffi::XML_TEXT_NODE {
        free_node(node);
    }
}

/// `dom_insert_node_list_unchecked`.
pub(crate) fn insert_node_list_unchecked(
    ctx: Ctx<'_>,
    node: N,
    parent: N,
    mut insertion_point: Option<N>,
) {
    if node.t() == ffi::XML_DOCUMENT_FRAG_NODE {
        if let (Some(newchild), Some(last)) = (node.children(), node.last()) {
            pre_insert_helper(insertion_point, parent, newchild, last);
            fragment_assign_parent(parent, node);
            if !ctx.modern() {
                reconcile_ns_list(parent.doc_ptr(), newchild, last);
            }
            if let Some(doc) = parent.doc() {
                if newchild.t() == ffi::XML_DTD_NODE {
                    doc.d().intSubset = newchild.as_dtd();
                    newchild.set_parent(Some(doc));
                }
            }
        }
        if node.private().is_null() {
            free_shell(node);
        } else {
            node.set_children(None);
            node.set_last(None);
        }
    } else {
        if insertion_point == Some(node) {
            insertion_point = node.next();
        }
        unlink(node);
        pre_insert_helper(insertion_point, parent, node, node);
        node.set_parent(Some(parent));
        match parent.doc() {
            Some(doc) if node.t() == ffi::XML_DTD_NODE => {
                doc.d().intSubset = node.as_dtd();
                node.set_parent(Some(doc));
            }
            _ => {
                if !ctx.modern() {
                    reconcile_ns(parent.doc_ptr(), node);
                }
            }
        }
    }
}

/// `php_dom_pre_insert`.
pub(crate) fn pre_insert(
    ctx: Ctx<'_>,
    node: N,
    parent: N,
    insertion_point: Option<N>,
) -> DomResult<()> {
    match pre_insert_valid_without_step_1(ctx, parent, node, insertion_point, parent.doc_ptr()) {
        Ok(()) => {
            insert_node_list_unchecked(ctx, node, parent, insertion_point);
            Ok(())
        }
        Err(e) => {
            insert_node_list_cleanup(node);
            Err(e)
        }
    }
}

/// `php_dom_node_append`.
pub(crate) fn node_append(ctx: Ctx<'_>, node: N, parent: N) -> DomResult<()> {
    pre_insert(ctx, node, parent, None)
}

fn is_node_in_list(nodes: &[NodeOrString], n: N) -> bool {
    nodes
        .iter()
        .any(|x| matches!(x, NodeOrString::Node(o) if o.n() == Some(n)))
}

/// `dom_parent_node_append`.
pub(crate) fn parent_node_append(this: &Node, nodes: &[NodeOrString]) -> DomResult<()> {
    let doc = this.doc_ref();
    let ctx = Ctx { doc: doc.as_ref() };
    sanity_check_node_list(nodes, this.family_is_modern())?;
    let parent = this.node()?;
    ctx.invalidate();
    let node = zvals_to_single_node(ctx, parent, nodes)?;
    node_append(ctx, node, parent)
}

/// `dom_parent_node_prepend`.
pub(crate) fn parent_node_prepend(this: &Node, nodes: &[NodeOrString]) -> DomResult<()> {
    let parent = this.node()?;
    if parent.children().is_none() {
        return parent_node_append(this, nodes);
    }
    let doc = this.doc_ref();
    let ctx = Ctx { doc: doc.as_ref() };
    sanity_check_node_list(nodes, this.family_is_modern())?;
    ctx.invalidate();
    let node = zvals_to_single_node(ctx, parent, nodes)?;
    pre_insert(ctx, node, parent, parent.children())
}

/// `dom_parent_node_after`.
pub(crate) fn child_node_after(this: &Node, nodes: &[NodeOrString]) -> DomResult<()> {
    let doc = this.doc_ref();
    let ctx = Ctx { doc: doc.as_ref() };
    sanity_check_node_list(nodes, this.family_is_modern())?;
    let thisp = this.node()?;
    let Some(parent) = thisp.parent() else {
        return Ok(());
    };
    let mut viable = thisp.next();
    while let Some(v) = viable {
        if !is_node_in_list(nodes, v) {
            break;
        }
        viable = v.next();
    }
    ctx.invalidate();
    let frag = zvals_to_single_node(ctx, parent, nodes)?;
    pre_insert(ctx, frag, parent, viable)
}

/// `dom_parent_node_before`.
pub(crate) fn child_node_before(this: &Node, nodes: &[NodeOrString]) -> DomResult<()> {
    let doc = this.doc_ref();
    let ctx = Ctx { doc: doc.as_ref() };
    sanity_check_node_list(nodes, this.family_is_modern())?;
    let thisp = this.node()?;
    let Some(parent) = thisp.parent() else {
        return Ok(());
    };
    let mut viable = thisp.prev();
    while let Some(v) = viable {
        if !is_node_in_list(nodes, v) {
            break;
        }
        viable = v.prev();
    }
    ctx.invalidate();
    let frag = zvals_to_single_node(ctx, parent, nodes)?;
    let viable = match viable {
        None => parent.children(),
        Some(v) => v.next(),
    };
    pre_insert(ctx, frag, parent, viable)
}

fn child_removal_preconditions(ctx: Ctx<'_>, child: N) -> DomResult<()> {
    if is_read_only(child) || child.parent().is_some_and(is_read_only) {
        return Err(throw(C::NoModificationAllowed, ctx.strict()));
    }
    if child.parent().is_none() {
        return Err(throw(C::NotFound, ctx.strict()));
    }
    Ok(())
}

/// `dom_child_node_remove`.
pub(crate) fn child_node_remove(this: &Node) -> DomResult<()> {
    let doc = this.doc_ref();
    let ctx = Ctx { doc: doc.as_ref() };
    let child = this.node()?;
    child_removal_preconditions(ctx, child)?;
    ctx.invalidate();
    unlink(child);
    Ok(())
}

/// `dom_child_replace_with`.
pub(crate) fn child_replace_with(this: &Node, nodes: &[NodeOrString]) -> DomResult<()> {
    let doc = this.doc_ref();
    let ctx = Ctx { doc: doc.as_ref() };
    sanity_check_node_list(nodes, this.family_is_modern())?;
    let child = this.node()?;
    let Some(parent) = child.parent() else {
        return Ok(());
    };
    let mut viable = child.next();
    while let Some(v) = viable {
        if !is_node_in_list(nodes, v) {
            break;
        }
        viable = v.next();
    }
    child_removal_preconditions(ctx, child)?;
    ctx.invalidate();
    let node = zvals_to_single_node(ctx, parent, nodes)?;
    match pre_insert_valid_without_step_1(ctx, parent, node, viable, parent.doc_ptr()) {
        Ok(()) => {
            if child.parent() != Some(node) {
                unlink(child);
            }
            insert_node_list_unchecked(ctx, node, parent, viable);
            Ok(())
        }
        Err(e) => {
            insert_node_list_cleanup(node);
            Err(e)
        }
    }
}

/// `dom_parent_node_replace_children`.
pub(crate) fn parent_node_replace_children(this: &Node, nodes: &[NodeOrString]) -> DomResult<()> {
    let doc = this.doc_ref();
    let ctx = Ctx { doc: doc.as_ref() };
    sanity_check_node_list(nodes, this.family_is_modern())?;
    let thisp = this.node()?;
    ctx.invalidate();
    let node = zvals_to_single_node(ctx, thisp, nodes)?;
    match pre_insert_valid_without_step_1(ctx, thisp, node, None, thisp.doc_ptr()) {
        Ok(()) => {
            crate::object::remove_all_children(thisp);
            pre_insert(ctx, node, thisp, None)
        }
        Err(e) => {
            insert_node_list_cleanup(node);
            Err(e)
        }
    }
}

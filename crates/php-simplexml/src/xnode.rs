// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! [`XNode`]: a node reference that `SimpleXML` can walk the way the C code
//! walks `xmlNode` pointers (children, siblings, properties, entity
//! declarations behind entity references), while keeping the node alive
//! through a `php-libxml` handle.
//!
//! Every `XNode` holds its [`Document`] (so the tree outlives it) and,
//! whenever the node can be reached through `php-libxml`'s navigation, a
//! [`Node`] handle ("pin") that registers it as referenced: libxml2
//! operations performed through `php-libxml` (and `SimpleXML`'s own
//! unlink-and-free) then keep it alive, exactly like `node->_private` in
//! PHP. All raw reads below rely on that invariant.

use std::ffi::{c_char, CStr};
use std::ptr::NonNull;

use php_libxml::ffi as lx;
use php_libxml::tree::{Document, Node};

use crate::ffi_extra as fx;

/// A C string from PHP bytes, truncated at the first NUL byte.
pub(crate) fn c_string(bytes: &[u8]) -> std::ffi::CString {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    std::ffi::CString::new(&bytes[..end]).unwrap_or_default()
}

/// Bytes up to the first NUL (what `xmlStrEqual` compares).
pub(crate) fn cstr(bytes: &[u8]) -> &[u8] {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    &bytes[..end]
}

/// Copies a borrowed C string.
///
/// # Safety
/// `p` must be NULL or a NUL-terminated string valid for reads.
pub(crate) unsafe fn opt_bytes(p: *const u8) -> Option<Vec<u8>> {
    if p.is_null() {
        None
    } else {
        // SAFETY: guaranteed by the caller.
        Some(
            unsafe { CStr::from_ptr(p.cast::<c_char>()) }
                .to_bytes()
                .to_vec(),
        )
    }
}

/// Copies and frees a libxml2-allocated string.
///
/// # Safety
/// `p` must be NULL or a string from libxml2's allocator not used later.
pub(crate) unsafe fn take_xml_string(p: *mut u8) -> Option<Vec<u8>> {
    // SAFETY: forwarded contract.
    let out = unsafe { opt_bytes(p) };
    if !p.is_null() {
        // SAFETY: allocated by libxml2, released once.
        unsafe { xml_free(p.cast()) };
    }
    out
}

/// Releases libxml2-allocated memory.
///
/// # Safety
/// `p` must be NULL or a pointer from libxml2's allocator not freed elsewhere.
pub(crate) unsafe fn xml_free(p: *mut std::ffi::c_void) {
    if p.is_null() {
        return;
    }
    // SAFETY: xmlFree is libxml2's initialised deallocator.
    unsafe {
        if let Some(free) = lx::xmlFree {
            free(p);
        }
    }
}

/// A namespace as seen from a node (`node->ns`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NsInfo {
    pub(crate) ptr: *mut lx::xmlNs,
    pub(crate) prefix: Option<Vec<u8>>,
    pub(crate) href: Vec<u8>,
}

/// A reference to a node of a `SimpleXML` document. See the module docs.
#[derive(Clone)]
pub struct XNode {
    doc: Document,
    ptr: NonNull<lx::xmlNode>,
    pin: Option<Node>,
}

impl std::fmt::Debug for XNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XNode")
            .field("type", &self.kind())
            .field(
                "name",
                &self
                    .name()
                    .map(|n| String::from_utf8_lossy(&n).into_owned()),
            )
            .finish_non_exhaustive()
    }
}

impl PartialEq for XNode {
    fn eq(&self, other: &Self) -> bool {
        self.ptr == other.ptr
    }
}

impl XNode {
    /// Wraps a `php-libxml` node handle.
    pub fn from_node(node: Node) -> Self {
        Self {
            doc: node.document(),
            ptr: NonNull::new(node.as_ptr()).unwrap_or(NonNull::dangling()),
            pin: Some(node),
        }
    }

    /// The `php-libxml` handle of this node (for DOM interop:
    /// `dom_import_simplexml`), when it has one.
    pub fn node(&self) -> Option<&Node> {
        self.pin.as_ref()
    }

    /// The owning document.
    pub fn document(&self) -> &Document {
        &self.doc
    }

    pub(crate) fn ptr(&self) -> *mut lx::xmlNode {
        self.ptr.as_ptr()
    }

    fn raw(&self) -> &lx::xmlNode {
        // SAFETY: the node is kept alive by its pin or, for nodes owned by
        // the DTD (entity declarations and their content), by the document
        // (see module docs). Only the common node prefix is read unless
        // the type was checked.
        unsafe { self.ptr.as_ref() }
    }

    fn wrap_raw(&self, ptr: *mut lx::xmlNode) -> Option<XNode> {
        NonNull::new(ptr).map(|ptr| XNode {
            doc: self.doc.clone(),
            ptr,
            pin: None,
        })
    }

    /// `node->type`.
    pub(crate) fn kind(&self) -> i32 {
        self.raw().type_
    }

    pub(crate) fn is_element(&self) -> bool {
        self.kind() == lx::XML_ELEMENT_NODE
    }

    pub(crate) fn is_attribute(&self) -> bool {
        self.kind() == lx::XML_ATTRIBUTE_NODE
    }

    pub(crate) fn is_text(&self) -> bool {
        self.kind() == lx::XML_TEXT_NODE
    }

    /// `node->name` (`None` when NULL).
    pub(crate) fn name(&self) -> Option<Vec<u8>> {
        // SAFETY: name is NULL or a C string owned by the node/dictionary.
        unsafe { opt_bytes(self.raw().name) }
    }

    /// `node->name` compared like `xmlStrEqual(node->name, name)`.
    pub(crate) fn name_is(&self, name: &[u8]) -> bool {
        self.name().as_deref() == Some(cstr(name))
    }

    /// `node->ns` of an element or attribute.
    pub(crate) fn ns(&self) -> Option<NsInfo> {
        if !(self.is_element() || self.is_attribute()) {
            return None;
        }
        let ns = self.raw().ns;
        // SAFETY: elements and attributes share the `ns` offset; the
        // namespace is kept alive by the tree (reconciled on moves).
        unsafe {
            ns.as_ref().map(|n| NsInfo {
                ptr: ns,
                prefix: opt_bytes(n.prefix),
                href: opt_bytes(n.href).unwrap_or_default(),
            })
        }
    }

    pub(crate) fn ns_ptr(&self) -> *mut lx::xmlNs {
        if self.is_element() || self.is_attribute() {
            self.raw().ns
        } else {
            std::ptr::null_mut()
        }
    }

    /// `node->content` of a text-like node (text, CDATA, comment, PI).
    pub(crate) fn raw_content(&self) -> Option<Vec<u8>> {
        if !matches!(
            self.kind(),
            lx::XML_TEXT_NODE | lx::XML_CDATA_SECTION_NODE | lx::XML_COMMENT_NODE | lx::XML_PI_NODE
        ) {
            return None;
        }
        // SAFETY: content is NULL or a C string owned by the node.
        unsafe { opt_bytes(self.raw().content) }
    }

    /// `xmlIsBlankNode`.
    pub(crate) fn is_blank(&self) -> bool {
        if !matches!(self.kind(), lx::XML_TEXT_NODE | lx::XML_CDATA_SECTION_NODE) {
            return false;
        }
        self.raw_content()
            .is_none_or(|c| c.iter().all(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r')))
    }

    /// Whether `node->children` is non-NULL.
    pub(crate) fn has_children(&self) -> bool {
        !self.raw().children.is_null() && !self.is_text_like()
    }

    fn is_text_like(&self) -> bool {
        // These node types store content, not children (`children` is
        // never set for them).
        matches!(
            self.kind(),
            lx::XML_TEXT_NODE | lx::XML_CDATA_SECTION_NODE | lx::XML_COMMENT_NODE | lx::XML_PI_NODE
        )
    }

    pub(crate) fn has_next(&self) -> bool {
        !self.raw().next.is_null()
    }

    pub(crate) fn has_prev(&self) -> bool {
        !self.raw().prev.is_null()
    }

    pub(crate) fn has_parent(&self) -> bool {
        !self.raw().parent.is_null()
    }

    /// `node->children`.
    pub(crate) fn children(&self) -> Option<XNode> {
        if self.is_text_like() {
            return None;
        }
        let raw = self.raw().children;
        if raw.is_null() {
            return None;
        }
        if self.kind() == lx::XML_ENTITY_REF_NODE {
            // The entity declaration (owned by the DTD).
            return self.locate(raw).or_else(|| self.wrap_raw(raw));
        }
        match &self.pin {
            Some(pin) => pin.first_child().map(XNode::from_node),
            None => self.wrap_raw(raw),
        }
    }

    /// `node->next` (the next attribute for attributes).
    pub(crate) fn next(&self) -> Option<XNode> {
        match &self.pin {
            Some(pin) => pin.next_sibling().map(XNode::from_node),
            None => self.wrap_raw(self.raw().next),
        }
    }

    /// `node->parent` (the document node for the root element).
    pub(crate) fn parent(&self) -> Option<XNode> {
        match &self.pin {
            Some(pin) => pin.parent().map(XNode::from_node),
            None => self.wrap_raw(self.raw().parent),
        }
    }

    /// `node->properties` (first attribute of an element).
    pub(crate) fn properties(&self) -> Option<XNode> {
        if !self.is_element() {
            return None;
        }
        match &self.pin {
            Some(pin) => pin.attributes().into_iter().next().map(XNode::from_node),
            None => self.wrap_raw(self.raw().properties.cast()),
        }
    }

    /// The parent is a document node.
    pub(crate) fn parent_is_document(&self) -> bool {
        let p = self.raw().parent;
        // SAFETY: the parent of a live node is live.
        !p.is_null()
            && matches!(
                unsafe { (*p).type_ },
                lx::XML_DOCUMENT_NODE | lx::XML_HTML_DOCUMENT_NODE
            )
    }

    /// `parent->children == parent->last` for this node's parent.
    pub(crate) fn parent_has_single_child(&self) -> bool {
        let p = self.raw().parent;
        // SAFETY: the parent of a live node is live.
        !p.is_null() && unsafe { (*p).children == (*p).last }
    }

    /// `xmlNodeListGetString(doc, node, 1)` for the sibling list starting
    /// at this node.
    pub(crate) fn list_get_string(&self) -> Option<Vec<u8>> {
        // SAFETY: node and document are live; the result is a fresh string.
        unsafe {
            take_xml_string(fx::xmlNodeListGetString(
                self.doc.as_ptr(),
                self.ptr.as_ptr(),
                1,
            ))
        }
    }

    /// A pinned reference to `target`, a node reachable from this node or
    /// from the document node through parent links (descendants, newly
    /// created children, `XPath` results, entity declarations).
    pub(crate) fn locate(&self, target: *mut lx::xmlNode) -> Option<XNode> {
        if target.is_null() {
            return None;
        }
        if target == self.ptr.as_ptr() {
            return Some(self.clone());
        }
        // Handles for this node and all its ancestors (up to the document
        // node or a detached root): the target is reached from the nearest
        // of them.
        let mut anchors: Vec<Node> = Vec::new();
        let mut up = self.pin.clone().or_else(|| Some(self.doc.as_node()));
        while let Some(n) = up {
            up = n.parent();
            anchors.push(n);
        }
        if anchors
            .iter()
            .all(|a| a.node_type() != php_libxml::tree::NodeType::Document)
        {
            anchors.push(self.doc.as_node());
        }
        let mut cur = target;
        loop {
            if cur.is_null() {
                return self.wrap_raw(target);
            }
            if anchors.iter().any(|a| a.as_ptr() == cur) {
                break;
            }
            // SAFETY: walking parent links of live nodes.
            cur = unsafe { (*cur).parent };
        }
        // SAFETY: `target` is reachable through parent links from a node
        // this document keeps alive (a pinned ancestor or the document
        // node), so it is a live node of this document's tree.
        unsafe { self.doc.node_handle(target) }.map(XNode::from_node)
    }

    /// Whether the node belongs to the document tree proper (not to the
    /// DTD: entity declarations and their content are shared with every
    /// entity reference and are never modified by `SimpleXML` here).
    pub(crate) fn is_mutable(&self) -> bool {
        let mut cur = self.ptr.as_ptr();
        while !cur.is_null() {
            // SAFETY: walking parent links of live nodes.
            let t = unsafe { (*cur).type_ };
            if matches!(
                t,
                lx::XML_DTD_NODE | lx::XML_ENTITY_DECL | lx::XML_ENTITY_NODE
            ) {
                return false;
            }
            // SAFETY: as above.
            cur = unsafe { (*cur).parent };
        }
        true
    }

    /// `sxe_unlink_node`: unlinks the node; it is freed once nothing
    /// references it any more.
    pub(crate) fn unlink(self) {
        if !self.is_mutable() {
            return;
        }
        if let Some(pin) = &self.pin {
            pin.unlink();
        }
    }

    /// `change_node_zval`: replaces the content with `value`, escaped with
    /// `xmlEncodeEntitiesReentrant` and re-parsed by `xmlNodeSetContent`.
    pub(crate) fn set_value(&self, value: &[u8]) {
        let Some(pin) = &self.pin else { return };
        // An entity reference's "children" are its declaration, which
        // xmlNodeSetContent would free.
        if !self.is_mutable() || self.kind() == lx::XML_ENTITY_REF_NODE {
            return;
        }
        let c = c_string(value);
        // SAFETY: the document is live; the result is a fresh string.
        let encoded = unsafe {
            take_xml_string(fx::xmlEncodeEntitiesReentrant(
                self.doc.as_ptr(),
                c.as_ptr().cast(),
            ))
        };
        if let Some(e) = encoded {
            pin.set_content(&e);
        }
    }

    /// `xmlNewTextChild(self, ns, name, content)` (content is raw text).
    pub(crate) fn new_text_child(
        &self,
        ns: *mut lx::xmlNs,
        name: &[u8],
        content: Option<&[u8]>,
    ) -> Option<XNode> {
        if !self.is_mutable() {
            return None;
        }
        let n = c_string(name);
        let c = content.map(c_string);
        // SAFETY: self is a live node; the new child is attached to it.
        let p = unsafe {
            fx::xmlNewTextChild(
                self.ptr.as_ptr(),
                ns,
                n.as_ptr().cast(),
                c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr().cast()),
            )
        };
        self.locate(p)
    }

    /// `xmlNewChild(self, ns, name, content)` (content may contain entity
    /// references).
    pub(crate) fn new_child(
        &self,
        ns: *mut lx::xmlNs,
        name: &[u8],
        content: Option<&[u8]>,
    ) -> Option<XNode> {
        if !self.is_mutable() {
            return None;
        }
        let n = c_string(name);
        let c = content.map(c_string);
        // SAFETY: self is a live node; the new child is attached to it.
        let p = unsafe {
            fx::xmlNewChild(
                self.ptr.as_ptr(),
                ns,
                n.as_ptr().cast(),
                c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr().cast()),
            )
        };
        self.locate(p)
    }

    /// `xmlNewNsProp(self, ns, name, value)` (`xmlNewProp` when `ns` is NULL).
    pub(crate) fn new_prop(
        &self,
        ns: *mut lx::xmlNs,
        name: &[u8],
        value: Option<&[u8]>,
    ) -> Option<XNode> {
        if !self.is_element() || !self.is_mutable() {
            return None;
        }
        let n = c_string(name);
        let v = value.map(c_string);
        // SAFETY: self is a live element; the attribute is attached to it.
        let p = unsafe {
            fx::xmlNewNsProp(
                self.ptr.as_ptr(),
                ns,
                n.as_ptr().cast(),
                v.as_ref().map_or(std::ptr::null(), |v| v.as_ptr().cast()),
            )
        };
        self.locate(p.cast())
    }

    /// `xmlSearchNsByHref(doc, self, href)`.
    pub(crate) fn search_ns_by_href(&self, href: &[u8]) -> *mut lx::xmlNs {
        let h = c_string(href);
        // SAFETY: live node and document.
        unsafe { lx::xmlSearchNsByHref(self.doc.as_ptr(), self.ptr.as_ptr(), h.as_ptr().cast()) }
    }

    /// `xmlNewNs(self, href, prefix)` (declared on this element).
    pub(crate) fn new_ns(&self, href: &[u8], prefix: Option<&[u8]>) -> *mut lx::xmlNs {
        if !self.is_element() || !self.is_mutable() {
            return std::ptr::null_mut();
        }
        let h = c_string(href);
        let p = prefix.map(c_string);
        // SAFETY: live element; the namespace is owned by its nsDef list.
        unsafe {
            lx::xmlNewNs(
                self.ptr.as_ptr(),
                h.as_ptr().cast(),
                p.as_ref().map_or(std::ptr::null(), |p| p.as_ptr().cast()),
            )
        }
    }

    /// `node->ns = ns` for an element.
    pub(crate) fn set_ns(&self, ns: *mut lx::xmlNs) {
        if self.is_element() {
            // SAFETY: live element; `ns` is NULL or a namespace declared in
            // scope (or on the element itself).
            unsafe { (*self.ptr.as_ptr()).ns = ns };
        }
    }

    /// `xmlHasNsProp(self, name, uri)` is an attribute (not a DTD default).
    pub(crate) fn has_ns_prop(&self, name: &[u8], uri: Option<&[u8]>) -> bool {
        let n = c_string(name);
        let u = uri.map(c_string);
        // SAFETY: live node; the result is only inspected.
        unsafe {
            let a = lx::xmlHasNsProp(
                self.ptr.as_ptr(),
                n.as_ptr().cast(),
                u.as_ref().map_or(std::ptr::null(), |u| u.as_ptr().cast()),
            );
            !a.is_null() && (*a).type_ != lx::XML_ATTRIBUTE_DECL
        }
    }

    /// The namespace declarations of this element (`nsDef`).
    pub(crate) fn ns_defs(&self) -> Vec<(Option<Vec<u8>>, Vec<u8>)> {
        let mut out = Vec::new();
        if !self.is_element() {
            return out;
        }
        let mut cur = self.raw().nsDef;
        // SAFETY: nsDef is a NULL-terminated list owned by the element.
        unsafe {
            while let Some(ns) = cur.as_ref() {
                out.push((opt_bytes(ns.prefix), opt_bytes(ns.href).unwrap_or_default()));
                cur = ns.next;
            }
        }
        out
    }
}

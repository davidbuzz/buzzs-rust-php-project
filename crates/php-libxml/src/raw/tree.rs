// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Reference-counted documents and node handles over libxml2 trees.
//!
//! # Ownership model
//!
//! This mirrors `php_libxml_ref_obj` / `php_libxml_node_ptr` from
//! `ext/libxml` while making every operation memory safe:
//!
//! * A [`Document`] is a cheap, cloneable reference to a shared
//!   `xmlDoc` (the `php_libxml_ref_obj` refcount). The `xmlDoc` and
//!   everything still attached to it is freed when the last `Document`
//!   *and* the last [`Node`] of that document are dropped (every `Node`
//!   keeps its document alive).
//! * A [`Node`] is a handle to one node of a document. The document keeps
//!   a registry `node address -> number of live handles` (the role of
//!   `node->_private` in PHP). Clones share the count.
//! * Attached nodes are only ever freed together with their document.
//! * A *detached* node (unlinked, or created and never inserted) is owned
//!   by its handles: when the last handle goes away the subtree is freed —
//!   except for descendants that still have handles of their own, which are
//!   first unlinked and become detached roots themselves
//!   (`php_libxml_node_free_list`).
//! * libxml2 operations that free nodes internally (setting content,
//!   replacing an attribute value, replacing the root) first rescue
//!   descendants that have handles, the same way.
//! * Insertion never uses libxml2's text-merging `xmlAddChild` family (it
//!   frees the inserted node); links are spliced by hand.
//! * Moving or detaching an element reconciles its namespace references
//!   (`xmlReconciliateNs`) so it never points at a namespace declaration
//!   owned by a node that may be freed; detached attributes get a private
//!   copy of their namespace stored in the document's `oldNs` list (as
//!   `php_libxml_set_old_ns` does).
//! * Nodes never move between documents: [`Document::import`] copies.
//!
//! `Document` and `Node` are `!Send`/`!Sync` (they contain `Rc`), which
//! also matches libxml2's per-thread error/dictionary state.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_int;
use std::io::Write;
use std::ptr::NonNull;
use std::rc::Rc;

use super::errors::{CtxtRegistration, ParserChannels};
use super::io::output_buffer_from_writer;
use super::{c_string, ensure_init, guard, opt_slice, take_xml_string};
use crate::ffi;

/// Node types (`xmlElementType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeType {
    /// `XML_ELEMENT_NODE`.
    Element,
    /// `XML_ATTRIBUTE_NODE`.
    Attribute,
    /// `XML_TEXT_NODE`.
    Text,
    /// `XML_CDATA_SECTION_NODE`.
    CData,
    /// `XML_ENTITY_REF_NODE`.
    EntityRef,
    /// `XML_ENTITY_NODE`.
    Entity,
    /// `XML_PI_NODE`.
    ProcessingInstruction,
    /// `XML_COMMENT_NODE`.
    Comment,
    /// `XML_DOCUMENT_NODE`.
    Document,
    /// `XML_DOCUMENT_TYPE_NODE`.
    DocumentType,
    /// `XML_DOCUMENT_FRAG_NODE`.
    DocumentFragment,
    /// `XML_NOTATION_NODE`.
    Notation,
    /// `XML_HTML_DOCUMENT_NODE`.
    HtmlDocument,
    /// `XML_DTD_NODE`.
    Dtd,
    /// `XML_ELEMENT_DECL`.
    ElementDecl,
    /// `XML_ATTRIBUTE_DECL`.
    AttributeDecl,
    /// `XML_ENTITY_DECL`.
    EntityDecl,
    /// `XML_NAMESPACE_DECL`.
    NamespaceDecl,
    /// `XML_XINCLUDE_START`.
    XIncludeStart,
    /// `XML_XINCLUDE_END`.
    XIncludeEnd,
    /// Any other value.
    Other(i32),
}

impl NodeType {
    /// Maps libxml2's numeric type.
    pub fn from_raw(t: c_int) -> Self {
        match t {
            ffi::XML_ELEMENT_NODE => Self::Element,
            ffi::XML_ATTRIBUTE_NODE => Self::Attribute,
            ffi::XML_TEXT_NODE => Self::Text,
            ffi::XML_CDATA_SECTION_NODE => Self::CData,
            ffi::XML_ENTITY_REF_NODE => Self::EntityRef,
            ffi::XML_ENTITY_NODE => Self::Entity,
            ffi::XML_PI_NODE => Self::ProcessingInstruction,
            ffi::XML_COMMENT_NODE => Self::Comment,
            ffi::XML_DOCUMENT_NODE => Self::Document,
            ffi::XML_DOCUMENT_TYPE_NODE => Self::DocumentType,
            ffi::XML_DOCUMENT_FRAG_NODE => Self::DocumentFragment,
            ffi::XML_NOTATION_NODE => Self::Notation,
            ffi::XML_HTML_DOCUMENT_NODE => Self::HtmlDocument,
            ffi::XML_DTD_NODE => Self::Dtd,
            ffi::XML_ELEMENT_DECL => Self::ElementDecl,
            ffi::XML_ATTRIBUTE_DECL => Self::AttributeDecl,
            ffi::XML_ENTITY_DECL => Self::EntityDecl,
            ffi::XML_NAMESPACE_DECL => Self::NamespaceDecl,
            ffi::XML_XINCLUDE_START => Self::XIncludeStart,
            ffi::XML_XINCLUDE_END => Self::XIncludeEnd,
            other => Self::Other(other),
        }
    }

    /// libxml2's numeric type (`XML_*_NODE`, the value of `nodeType`).
    pub fn to_raw(self) -> i32 {
        match self {
            Self::Element => ffi::XML_ELEMENT_NODE,
            Self::Attribute => ffi::XML_ATTRIBUTE_NODE,
            Self::Text => ffi::XML_TEXT_NODE,
            Self::CData => ffi::XML_CDATA_SECTION_NODE,
            Self::EntityRef => ffi::XML_ENTITY_REF_NODE,
            Self::Entity => ffi::XML_ENTITY_NODE,
            Self::ProcessingInstruction => ffi::XML_PI_NODE,
            Self::Comment => ffi::XML_COMMENT_NODE,
            Self::Document => ffi::XML_DOCUMENT_NODE,
            Self::DocumentType => ffi::XML_DOCUMENT_TYPE_NODE,
            Self::DocumentFragment => ffi::XML_DOCUMENT_FRAG_NODE,
            Self::Notation => ffi::XML_NOTATION_NODE,
            Self::HtmlDocument => ffi::XML_HTML_DOCUMENT_NODE,
            Self::Dtd => ffi::XML_DTD_NODE,
            Self::ElementDecl => ffi::XML_ELEMENT_DECL,
            Self::AttributeDecl => ffi::XML_ATTRIBUTE_DECL,
            Self::EntityDecl => ffi::XML_ENTITY_DECL,
            Self::NamespaceDecl => ffi::XML_NAMESPACE_DECL,
            Self::XIncludeStart => ffi::XML_XINCLUDE_START,
            Self::XIncludeEnd => ffi::XML_XINCLUDE_END,
            Self::Other(n) => n,
        }
    }

    /// Whether nodes of this type may be inserted as children / freed as
    /// detached roots by this layer.
    fn is_tree_content(self) -> bool {
        matches!(
            self,
            Self::Element
                | Self::Text
                | Self::CData
                | Self::EntityRef
                | Self::ProcessingInstruction
                | Self::Comment
                | Self::DocumentFragment
        )
    }
}

/// A namespace binding (`xmlNs`), copied out of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Namespace {
    /// Prefix (`None` for the default namespace).
    pub prefix: Option<Vec<u8>>,
    /// Namespace URI.
    pub href: Vec<u8>,
}

/// Why a tree operation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeError {
    /// The nodes belong to different documents.
    WrongDocument,
    /// The node type cannot be used here (e.g. inserting an attribute as a
    /// child, or a parent that cannot have children).
    InvalidNodeType,
    /// Inserting a node into itself or its own descendant.
    Hierarchy,
    /// The reference node is not a child of the parent.
    NotFound,
    /// libxml2 failed (out of memory or rejected input).
    Failed,
}

impl std::fmt::Display for TreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::WrongDocument => "node belongs to a different document",
            Self::InvalidNodeType => "invalid node type for this operation",
            Self::Hierarchy => "cannot insert a node into itself or its descendant",
            Self::NotFound => "reference node is not a child of this node",
            Self::Failed => "libxml2 operation failed",
        })
    }
}

impl std::error::Error for TreeError {}

/// Failure of [`Document::parse_memory`] / [`Document::parse_file`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// libxml2 produced no document (I/O failure, empty input, fatal error).
    NoDocument,
    /// The document is not well-formed and recovery was not requested.
    NotWellFormed,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoDocument => "libxml2 did not produce a document",
            Self::NotWellFormed => "document is not well-formed",
        })
    }
}

impl std::error::Error for ParseError {}

/// Options for parsing a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseOptions {
    /// `LIBXML_*` / `XML_PARSE_*` option bits.
    pub options: i32,
    /// Base URL of the document (`ctxt->input->filename` for messages).
    pub url: Option<Vec<u8>>,
    /// Encoding override.
    pub encoding: Option<Vec<u8>>,
    /// How errors are reported when internal errors are off.
    pub channels: ParserChannels,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            options: 0,
            url: None,
            encoding: None,
            channels: ParserChannels::CTX,
        }
    }
}

struct DocInner {
    ptr: NonNull<ffi::xmlDoc>,
    /// Live handle count per node address.
    live: RefCell<HashMap<usize, usize>>,
    /// Shared with another object model ([`SharedDocument`]): a node whose
    /// `_private` is set is referenced by that model (PHP's convention:
    /// `_private` points at the owning PHP object) and counts as live.
    shared: std::cell::Cell<bool>,
    /// Bumped by every structural mutation made through this layer (the
    /// cache tag other models compare against).
    generation: std::cell::Cell<u64>,
    /// `php_libxml_ref_obj.class_type` for a document imported into DOM:
    /// `None` until then, then whether it was imported as `Dom\` nodes
    /// (`true`) or legacy `DOMNode`s. It lives and dies with the document.
    family: std::cell::Cell<Option<bool>>,
}

impl DocInner {
    fn doc(&self) -> *mut ffi::xmlDoc {
        self.ptr.as_ptr()
    }

    fn is_live(&self, node: *mut ffi::xmlNode) -> bool {
        self.is_pinned(node) || self.foreign_ref(node)
    }

    /// Whether this layer holds handles to `node`.
    fn is_pinned(&self, node: *mut ffi::xmlNode) -> bool {
        self.live
            .borrow()
            .get(&(node as usize))
            .is_some_and(|&n| n > 0)
    }

    /// Whether another object model references `node` (shared mode only).
    #[allow(clippy::used_underscore_binding)]
    fn foreign_ref(&self, node: *mut ffi::xmlNode) -> bool {
        // SAFETY: node is a live node of this document; `_private` is the
        // first field of every node struct.
        self.shared.get() && !unsafe { (*node)._private }.is_null()
    }

    fn touch(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
    }

    fn acquire(&self, node: *mut ffi::xmlNode) {
        *self.live.borrow_mut().entry(node as usize).or_insert(0) += 1;
    }

    /// Returns true when this was the last handle.
    fn release(&self, node: *mut ffi::xmlNode) -> bool {
        let mut live = self.live.borrow_mut();
        let key = node as usize;
        match live.get_mut(&key) {
            Some(n) if *n > 1 => {
                *n -= 1;
                false
            }
            Some(_) => {
                live.remove(&key);
                true
            }
            None => false,
        }
    }

    /// Ensures `doc->oldNs` starts with the `xml` namespace declaration.
    fn ensure_xml_decl(&self) {
        let doc = self.doc();
        // SAFETY: doc is live; xmlSearchNs with prefix "xml" creates the
        // reserved oldNs entry (xmlTreeEnsureXMLDecl) when missing.
        unsafe {
            ffi::xmlSearchNs(doc, doc.cast(), c"xml".as_ptr().cast());
        }
    }

    /// Gives a detached attribute a private copy of its namespace kept in
    /// `doc->oldNs` (freed with the document).
    fn pin_attr_ns(&self, attr: *mut ffi::xmlNode) {
        // SAFETY: attr is a live attribute of this document.
        unsafe {
            let a = attr.cast::<ffi::xmlAttr>();
            let ns = (*a).ns;
            if ns.is_null() {
                return;
            }
            self.ensure_xml_decl();
            let doc = self.doc();
            if !(*doc).oldNs.is_null() && ns == (*doc).oldNs {
                return;
            }
            let copy = ffi::xmlNewNs(std::ptr::null_mut(), (*ns).href, (*ns).prefix);
            if copy.is_null() || (*doc).oldNs.is_null() {
                return;
            }
            (*copy).next = (*(*doc).oldNs).next;
            (*(*doc).oldNs).next = copy;
            (*a).ns = copy;
        }
    }

    /// Unlinks `node` and makes it self-contained regarding namespaces.
    fn detach(&self, node: *mut ffi::xmlNode) {
        self.touch();
        // SAFETY: node is a live node of this document.
        unsafe {
            ffi::xmlUnlinkNode(node);
            match (*node).type_ {
                ffi::XML_ELEMENT_NODE => {
                    ffi::xmlReconciliateNs(self.doc(), node);
                }
                ffi::XML_ATTRIBUTE_NODE => self.pin_attr_ns(node),
                _ => {}
            }
        }
    }

    /// Detaches every descendant of `node` (children and attributes) that
    /// still has handles, so that libxml2 may free the rest.
    fn rescue_descendants(&self, node: *mut ffi::xmlNode) {
        self.rescue(node, true);
    }

    /// Like [`Self::rescue_descendants`] but leaves the node's own
    /// attributes alone (used before replacing an element's content).
    fn rescue_children(&self, node: *mut ffi::xmlNode) {
        self.rescue(node, false);
    }

    fn rescue(&self, node: *mut ffi::xmlNode, with_attributes: bool) {
        // SAFETY: node is a live node of this document; we only follow
        // child/property links of tree-content nodes.
        unsafe {
            let t = (*node).type_;
            if t == ffi::XML_ENTITY_REF_NODE {
                // Children of entity references belong to the declaration.
                return;
            }
            let mut child = (*node).children;
            while !child.is_null() {
                let next = (*child).next;
                if self.is_live(child) {
                    self.detach(child);
                } else {
                    self.rescue_descendants(child);
                }
                child = next;
            }
            if with_attributes && t == ffi::XML_ELEMENT_NODE {
                let mut attr = (*node).properties.cast::<ffi::xmlNode>();
                while !attr.is_null() {
                    let next = (*attr).next;
                    if self.is_live(attr) {
                        self.detach(attr);
                    } else {
                        self.rescue_descendants(attr);
                    }
                    attr = next;
                }
            }
        }
    }

    /// Frees a detached, handle-less node (rescuing referenced descendants).
    fn free_detached(&self, node: *mut ffi::xmlNode) {
        // SAFETY: node is live, belongs to this document, has no parent and
        // no handles; referenced descendants are detached first.
        unsafe {
            if !(*node).parent.is_null() {
                return;
            }
            let t = (*node).type_;
            if !(NodeType::from_raw(t).is_tree_content() || t == ffi::XML_ATTRIBUTE_NODE) {
                // Declarations and documents are owned elsewhere.
                return;
            }
            if self.doc().cast::<ffi::xmlNode>() == node || self.foreign_ref(node) {
                return;
            }
            self.rescue_descendants(node);
            if t == ffi::XML_ATTRIBUTE_NODE {
                ffi::xmlFreeProp(node.cast());
            } else {
                ffi::xmlFreeNode(node);
            }
        }
    }
}

impl Drop for DocInner {
    fn drop(&mut self) {
        // SAFETY: no handle references the document any more (they all hold
        // an Rc to this inner value), so the tree is exclusively ours.
        unsafe { ffi::xmlFreeDoc(self.ptr.as_ptr()) };
    }
}

/// A shared reference to a libxml2 document. See the module docs.
#[derive(Clone)]
pub struct Document {
    inner: Rc<DocInner>,
}

impl std::fmt::Debug for Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Document")
            .field("ptr", &self.inner.ptr)
            .finish_non_exhaustive()
    }
}

impl PartialEq for Document {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for Document {}

/// A `Write` sink appending to a shared buffer.
struct SharedVec(Rc<RefCell<Vec<u8>>>);

impl Write for SharedVec {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Document {
    fn wrap(ptr: NonNull<ffi::xmlDoc>) -> Self {
        Self {
            inner: Rc::new(DocInner {
                ptr,
                live: RefCell::new(HashMap::new()),
                shared: std::cell::Cell::new(false),
                generation: std::cell::Cell::new(0),
                family: std::cell::Cell::new(None),
            }),
        }
    }

    /// `xmlNewDoc(version)`: an empty document.
    pub fn new(version: &[u8]) -> Result<Self, TreeError> {
        ensure_init();
        let v = c_string(version);
        // SAFETY: v is a valid C string; the returned document is owned.
        let ptr = unsafe { ffi::xmlNewDoc(v.as_ptr().cast()) };
        NonNull::new(ptr).map(Self::wrap).ok_or(TreeError::Failed)
    }

    /// Takes ownership of a document produced by libxml2 elsewhere (e.g. by
    /// an XSLT transformation or a reader expanding its tree).
    ///
    /// # Safety
    /// `ptr` must be a valid `xmlDoc` that nothing else frees or uses after
    /// this call, and whose nodes are not referenced elsewhere.
    pub unsafe fn from_raw(ptr: NonNull<ffi::xmlDoc>) -> Self {
        Self::wrap(ptr)
    }

    /// The underlying `xmlDoc` pointer (valid while `self` lives).
    pub fn as_ptr(&self) -> *mut ffi::xmlDoc {
        self.inner.doc()
    }

    /// Parses an in-memory document (`xmlCtxtReadMemory`).
    pub fn parse_memory(data: &[u8], options: &ParseOptions) -> Result<Self, ParseError> {
        Self::parse_with(options, |ctxt, url, enc| {
            let len = c_int::try_from(data.len()).unwrap_or(c_int::MAX);
            // SAFETY: data is valid for len bytes; ctxt is live.
            unsafe {
                ffi::xmlCtxtReadMemory(ctxt, data.as_ptr().cast(), len, url, enc, options.options)
            }
        })
    }

    /// Parses a document from a path or URL (`xmlCtxtReadFile`), going
    /// through the installed stream hooks.
    pub fn parse_file(path: &[u8], options: &ParseOptions) -> Result<Self, ParseError> {
        let p = c_string(path);
        Self::parse_with(options, |ctxt, _url, enc| {
            // SAFETY: p is a valid C string; ctxt is live.
            unsafe { ffi::xmlCtxtReadFile(ctxt, p.as_ptr(), enc, options.options) }
        })
    }

    fn parse_with(
        options: &ParseOptions,
        read: impl FnOnce(
            *mut ffi::xmlParserCtxt,
            *const std::ffi::c_char,
            *const std::ffi::c_char,
        ) -> *mut ffi::xmlDoc,
    ) -> Result<Self, ParseError> {
        ensure_init();
        // SAFETY: a fresh parser context, freed below.
        let ctxt = unsafe { ffi::xmlNewParserCtxt() };
        if ctxt.is_null() {
            return Err(ParseError::NoDocument);
        }
        let url = options.url.as_deref().map(c_string);
        let enc = options.encoding.as_deref().map(c_string);
        let registration = CtxtRegistration::new(ctxt, options.channels);
        let doc = read(
            ctxt,
            url.as_ref().map_or(std::ptr::null(), |u| u.as_ptr()),
            enc.as_ref().map_or(std::ptr::null(), |e| e.as_ptr()),
        );
        // SAFETY: ctxt is still live; reading a plain field.
        let well_formed = unsafe { (*ctxt).wellFormed } != 0;
        drop(registration);
        // SAFETY: the document (if any) is detached from the context by
        // xmlCtxtRead*; the context is freed exactly once.
        unsafe { ffi::xmlFreeParserCtxt(ctxt) };
        guard::resume();
        let doc = NonNull::new(doc).ok_or(if well_formed {
            ParseError::NoDocument
        } else {
            ParseError::NotWellFormed
        })?;
        if !well_formed && options.options & ffi::XML_PARSE_RECOVER == 0 {
            // SAFETY: we own the fresh document.
            unsafe { ffi::xmlFreeDoc(doc.as_ptr()) };
            return Err(ParseError::NotWellFormed);
        }
        Ok(Self::wrap(doc))
    }

    fn handle(&self, ptr: *mut ffi::xmlNode) -> Option<Node> {
        let ptr = NonNull::new(ptr)?;
        self.inner.acquire(ptr.as_ptr());
        Some(Node {
            doc: self.inner.clone(),
            ptr,
        })
    }

    fn new_detached(&self, ptr: *mut ffi::xmlNode) -> Result<Node, TreeError> {
        self.handle(ptr).ok_or(TreeError::Failed)
    }

    /// A handle for a node of this document obtained from libxml2 directly
    /// (e.g. an `XPath` result, an `xmlTextReaderExpand` node or an XSLT
    /// output node). Returns `None` for NULL.
    ///
    /// # Safety
    /// `ptr` must point to a live node (element, attribute, text, …) whose
    /// `doc` is this document and that is either attached to this
    /// document's tree or not referenced/freed by anything else; the usual
    /// ownership rules of this module then apply to it.
    pub unsafe fn node_handle(&self, ptr: *mut ffi::xmlNode) -> Option<Node> {
        self.handle(ptr)
    }

    /// The document node itself.
    pub fn as_node(&self) -> Node {
        let ptr = self.inner.ptr.cast::<ffi::xmlNode>();
        self.inner.acquire(ptr.as_ptr());
        Node {
            doc: self.inner.clone(),
            ptr,
        }
    }

    /// The document element.
    pub fn root(&self) -> Option<Node> {
        // SAFETY: the document is live.
        self.handle(unsafe { ffi::xmlDocGetRootElement(self.inner.doc()) })
    }

    /// Replaces (or sets) the document element. The previous root becomes
    /// a detached node (freed unless it has handles).
    pub fn set_root(&self, node: &Node) -> Result<(), TreeError> {
        if !Rc::ptr_eq(&self.inner, &node.doc) {
            return Err(TreeError::WrongDocument);
        }
        if node.node_type() != NodeType::Element {
            return Err(TreeError::InvalidNodeType);
        }
        self.inner.detach(node.ptr.as_ptr());
        // SAFETY: node is a detached element of this document.
        let old = unsafe { ffi::xmlDocSetRootElement(self.inner.doc(), node.ptr.as_ptr()) };
        if !old.is_null() && old != node.ptr.as_ptr() && !self.inner.is_live(old) {
            self.inner.free_detached(old);
        }
        Ok(())
    }

    /// `xmlNewDocNode`: a detached element (`name` may be `prefix:local`
    /// only when used together with [`Node::set_namespace`]).
    pub fn create_element(&self, name: &[u8]) -> Result<Node, TreeError> {
        let n = c_string(name);
        // SAFETY: the document is live; n is a valid C string.
        self.new_detached(unsafe {
            ffi::xmlNewDocNode(
                self.inner.doc(),
                std::ptr::null_mut(),
                n.as_ptr().cast(),
                std::ptr::null(),
            )
        })
    }

    /// A detached element in namespace `uri` with qualified name `qname`
    /// (the namespace is declared on the element).
    pub fn create_element_ns(&self, uri: &[u8], qname: &[u8]) -> Result<Node, TreeError> {
        let (prefix, local) = match qname.iter().position(|&b| b == b':') {
            Some(i) => (Some(&qname[..i]), &qname[i + 1..]),
            None => (None, qname),
        };
        let el = self.create_element(local)?;
        el.declare_namespace(prefix, uri, true)?;
        Ok(el)
    }

    /// A detached text node (content may contain NUL bytes).
    pub fn create_text(&self, content: &[u8]) -> Result<Node, TreeError> {
        let len = c_int::try_from(content.len()).map_err(|_| TreeError::Failed)?;
        // SAFETY: content is valid for len bytes.
        self.new_detached(unsafe { ffi::xmlNewDocTextLen(self.inner.doc(), content.as_ptr(), len) })
    }

    /// A detached comment node.
    pub fn create_comment(&self, content: &[u8]) -> Result<Node, TreeError> {
        let c = c_string(content);
        // SAFETY: c is a valid C string.
        self.new_detached(unsafe { ffi::xmlNewDocComment(self.inner.doc(), c.as_ptr().cast()) })
    }

    /// A detached CDATA section.
    pub fn create_cdata(&self, content: &[u8]) -> Result<Node, TreeError> {
        let len = c_int::try_from(content.len()).map_err(|_| TreeError::Failed)?;
        // SAFETY: content is valid for len bytes.
        self.new_detached(unsafe { ffi::xmlNewCDataBlock(self.inner.doc(), content.as_ptr(), len) })
    }

    /// A detached processing instruction.
    pub fn create_processing_instruction(
        &self,
        target: &[u8],
        data: &[u8],
    ) -> Result<Node, TreeError> {
        let t = c_string(target);
        let d = c_string(data);
        // SAFETY: valid C strings.
        self.new_detached(unsafe {
            ffi::xmlNewDocPI(self.inner.doc(), t.as_ptr().cast(), d.as_ptr().cast())
        })
    }

    /// Deep (`deep = true`) or shallow copy of a node of any document into
    /// this one, detached (`xmlDocCopyNode`).
    pub fn import(&self, node: &Node, deep: bool) -> Result<Node, TreeError> {
        // SAFETY: node is live (its document is kept alive by the handle);
        // the copy belongs to this document.
        self.new_detached(unsafe {
            ffi::xmlDocCopyNode(
                node.ptr.as_ptr(),
                self.inner.doc(),
                if deep { 1 } else { 2 },
            )
        })
    }

    fn doc_str(&self, f: impl FnOnce(&ffi::xmlDoc) -> *const u8) -> Option<Vec<u8>> {
        // SAFETY: the document is live; the string fields are C strings.
        unsafe { opt_slice(f(&*self.inner.doc())).map(<[u8]>::to_vec) }
    }

    /// `doc->version`.
    pub fn version(&self) -> Option<Vec<u8>> {
        self.doc_str(|d| d.version)
    }

    /// `doc->encoding`.
    pub fn encoding(&self) -> Option<Vec<u8>> {
        self.doc_str(|d| d.encoding)
    }

    /// `doc->URL`.
    pub fn url(&self) -> Option<Vec<u8>> {
        self.doc_str(|d| d.URL)
    }

    /// `doc->standalone` (1 yes, 0 no, -1 no declaration, -2 unspecified).
    pub fn standalone(&self) -> i32 {
        // SAFETY: the document is live.
        unsafe { (*self.inner.doc()).standalone }
    }

    /// Serialises the whole document with `xmlSaveDoc` and `XML_SAVE_*`
    /// options (`php_libxml_default_dump_doc_to_str`).
    pub fn save(&self, options: i32, encoding: Option<&[u8]>) -> Option<Vec<u8>> {
        let out = Rc::new(RefCell::new(Vec::new()));
        let enc = encoding.map(c_string);
        let ctx = Box::into_raw(Box::new(Box::new(SharedVec(out.clone())) as Box<dyn Write>));
        // SAFETY: the context matches the callbacks of super::io; ownership
        // passes to the save context, which closes it in xmlSaveClose.
        let status = unsafe {
            let save = ffi::xmlSaveToIO(
                Some(super::io::write_callback),
                Some(super::io::write_close_callback),
                ctx.cast(),
                enc.as_ref().map_or(std::ptr::null(), |e| e.as_ptr()),
                options,
            );
            if save.is_null() {
                drop(Box::from_raw(ctx));
                return None;
            }
            let mut status = ffi::xmlSaveDoc(save, self.inner.doc());
            status |= std::ffi::c_long::from(ffi::xmlSaveClose(save));
            status
        };
        guard::resume();
        if status < 0 {
            return None;
        }
        Some(out.take())
    }

    /// `xmlDocDumpFormatMemoryEnc`: the classic `saveXML()` output.
    pub fn dump(&self, format: bool, encoding: Option<&[u8]>) -> Vec<u8> {
        let enc = encoding.map(c_string);
        let mut mem: *mut u8 = std::ptr::null_mut();
        let mut size: c_int = 0;
        // SAFETY: out-pointers are valid; the buffer is released by
        // take_xml_string.
        unsafe {
            ffi::xmlDocDumpFormatMemoryEnc(
                self.inner.doc(),
                &raw mut mem,
                &raw mut size,
                enc.as_ref().map_or(std::ptr::null(), |e| e.as_ptr()),
                c_int::from(format),
            );
            if mem.is_null() {
                return Vec::new();
            }
            let bytes =
                std::slice::from_raw_parts(mem, usize::try_from(size).unwrap_or(0)).to_vec();
            super::xml_free(mem.cast());
            bytes
        }
    }

    /// Switches this document to shared ownership (see [`SharedDocument`]);
    /// the `Document` (and its clones and nodes) keep working unchanged.
    pub fn into_shared(self) -> SharedDocument {
        self.inner.shared.set(true);
        SharedDocument { inner: self.inner }
    }

    /// Like [`Document::into_shared`] without consuming `self`.
    pub fn to_shared(&self) -> SharedDocument {
        self.clone().into_shared()
    }

    /// Whether the tree is shared with another object model.
    pub fn is_shared(&self) -> bool {
        self.inner.shared.get()
    }

    /// The mutation counter (see [`SharedDocument::generation`]).
    pub fn generation(&self) -> u64 {
        self.inner.generation.get()
    }

    /// Records a mutation made outside this layer.
    pub fn mark_modified(&self) {
        self.inner.touch();
    }

    /// Number of distinct nodes that currently have handles.
    pub fn live_handles(&self) -> usize {
        self.inner.live.borrow().len()
    }
}

/// A [`Document`] whose tree is co-owned by another object model (DOM and
/// `SimpleXML` / XSL results sharing one `xmlDoc`, PHP's shared
/// `php_libxml_ref_obj`).
///
/// * The `xmlDoc` is freed when the last `SharedDocument`, [`Document`] and
///   [`Node`] of it are gone; the other model holds a `SharedDocument` for
///   as long as it has objects of the tree.
/// * Following PHP's convention, the other model marks every node it has
///   an object for by a non-NULL `node->_private` (php-dom does). Such
///   nodes count as referenced here: a detached subtree is never freed by
///   this layer while one of its nodes is referenced by either model, and
///   referenced descendants are rescued (unlinked) before a subtree is
///   freed (`php_libxml_node_free_list`).
/// * In the other direction, the other model asks [`SharedDocument::is_pinned`]
///   before freeing a detached node and rescues pinned descendants with
///   [`SharedDocument::detach`]; whichever model releases the last
///   reference frees the node.
/// * [`SharedDocument::generation`] changes on every structural mutation
///   made through this layer, so the other model can invalidate its caches
///   (node lists); call [`SharedDocument::mark_modified`] after mutating the
///   tree directly through FFI.
#[derive(Clone)]
pub struct SharedDocument {
    inner: Rc<DocInner>,
}

impl std::fmt::Debug for SharedDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedDocument")
            .field("ptr", &self.inner.ptr)
            .finish_non_exhaustive()
    }
}

impl PartialEq for SharedDocument {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for SharedDocument {}

impl SharedDocument {
    /// Takes ownership of an `xmlDoc` in shared mode (e.g. a DOM document
    /// handed to `SimpleXML`, or an XSLT result tree).
    ///
    /// # Safety
    /// `ptr` must be a valid `xmlDoc` that from now on is only freed through
    /// this handle (the other model drops its `SharedDocument` instead of
    /// calling `xmlFreeDoc`).
    pub unsafe fn from_raw(ptr: NonNull<ffi::xmlDoc>) -> Self {
        let doc = Document::wrap(ptr);
        doc.into_shared()
    }

    /// The underlying `xmlDoc` pointer (valid while `self` lives).
    pub fn as_ptr(&self) -> *mut ffi::xmlDoc {
        self.inner.doc()
    }

    /// A [`Document`] view of the same tree (same ownership).
    pub fn document(&self) -> Document {
        Document {
            inner: self.inner.clone(),
        }
    }

    /// A [`Node`] handle pinning `ptr` (see [`Document::node_handle`]).
    ///
    /// # Safety
    /// As for [`Document::node_handle`].
    pub unsafe fn pin(&self, ptr: *mut ffi::xmlNode) -> Option<Node> {
        self.document().handle(ptr)
    }

    /// Whether this layer holds [`Node`] handles to `node` (references of
    /// the other model, i.e. `_private`, are not counted).
    pub fn is_pinned(&self, node: *const ffi::xmlNode) -> bool {
        self.inner.is_pinned(node.cast_mut())
    }

    /// Unlinks a node and makes it self-contained regarding namespaces
    /// (`xmlReconciliateNs` for elements, a private namespace copy in
    /// `doc->oldNs` for attributes): how a model rescues a node pinned by
    /// the other one before freeing the subtree around it.
    ///
    /// # Safety
    /// `node` must be a live node of this document.
    pub unsafe fn detach(&self, node: *mut ffi::xmlNode) {
        self.inner.detach(node);
    }

    /// Frees a detached node that neither model references any more
    /// (rescuing referenced descendants first); a no-op when the node is
    /// attached, pinned or referenced through `_private`.
    ///
    /// # Safety
    /// `node` must be a live node of this document.
    pub unsafe fn free_if_unreferenced(&self, node: *mut ffi::xmlNode) {
        if !self.inner.is_live(node) {
            self.inner.free_detached(node);
        }
    }

    /// The mutation counter (see the type docs).
    pub fn generation(&self) -> u64 {
        self.inner.generation.get()
    }

    /// Records a mutation made outside this layer.
    pub fn mark_modified(&self) {
        self.inner.touch();
    }

    /// The node family the document was imported into DOM as
    /// (`php_libxml_ref_obj.class_type`): `Some(true)` for `Dom\`,
    /// `Some(false)` for the legacy classes, `None` before an import.
    pub fn family(&self) -> Option<bool> {
        self.inner.family.get()
    }

    /// Locks the document to a node family (see [`SharedDocument::family`]).
    pub fn set_family(&self, modern: bool) {
        self.inner.family.set(Some(modern));
    }
}

/// A handle to a node of a [`Document`]. See the module docs.
pub struct Node {
    doc: Rc<DocInner>,
    ptr: NonNull<ffi::xmlNode>,
}

impl Clone for Node {
    fn clone(&self) -> Self {
        self.doc.acquire(self.ptr.as_ptr());
        Self {
            doc: self.doc.clone(),
            ptr: self.ptr,
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let p = self.ptr.as_ptr();
        if self.doc.release(p) {
            self.doc.free_detached(p);
        }
    }
}

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.ptr == other.ptr
    }
}

impl Eq for Node {}

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node")
            .field("type", &self.node_type())
            .field("name", &String::from_utf8_lossy(&self.name()))
            .finish()
    }
}

impl Node {
    fn raw(&self) -> &ffi::xmlNode {
        // SAFETY: the node is live while a handle exists (see module docs);
        // only the common node prefix is read through this reference unless
        // the type has been checked.
        unsafe { self.ptr.as_ref() }
    }

    fn handle(&self, ptr: *mut ffi::xmlNode) -> Option<Node> {
        let ptr = NonNull::new(ptr)?;
        self.doc.acquire(ptr.as_ptr());
        Some(Node {
            doc: self.doc.clone(),
            ptr,
        })
    }

    /// The underlying `xmlNode` pointer (valid while the handle lives).
    pub fn as_ptr(&self) -> *mut ffi::xmlNode {
        self.ptr.as_ptr()
    }

    /// A stable identity for the node while it has handles.
    pub fn id(&self) -> usize {
        self.ptr.as_ptr() as usize
    }

    /// The owning document.
    pub fn document(&self) -> Document {
        Document {
            inner: self.doc.clone(),
        }
    }

    /// The node type.
    pub fn node_type(&self) -> NodeType {
        NodeType::from_raw(self.raw().type_)
    }

    fn is_element(&self) -> bool {
        self.raw().type_ == ffi::XML_ELEMENT_NODE
    }

    fn is_attribute(&self) -> bool {
        self.raw().type_ == ffi::XML_ATTRIBUTE_NODE
    }

    /// Whether the node is attached to a parent.
    pub fn is_attached(&self) -> bool {
        !self.raw().parent.is_null()
    }

    /// The local name (`node->name`); empty when NULL.
    pub fn name(&self) -> Vec<u8> {
        // SAFETY: name is NULL or a C string owned by the node/dictionary.
        unsafe { opt_slice(self.raw().name) }
            .unwrap_or_default()
            .to_vec()
    }

    fn ns(&self) -> Option<&ffi::xmlNs> {
        if !(self.is_element() || self.is_attribute()) {
            return None;
        }
        // SAFETY: elements and attributes share the `ns` field offset; it is
        // NULL or a namespace kept alive by the reconciliation rules.
        unsafe { self.raw().ns.as_ref() }
    }

    /// The namespace of an element or attribute.
    pub fn namespace(&self) -> Option<Namespace> {
        self.ns().map(|ns| Namespace {
            // SAFETY: prefix/href are NULL or C strings owned by the xmlNs.
            prefix: unsafe { opt_slice(ns.prefix) }.map(<[u8]>::to_vec),
            // SAFETY: as for `prefix`.
            href: unsafe { opt_slice(ns.href) }.unwrap_or_default().to_vec(),
        })
    }

    /// `prefix:name` (or `name`) of an element or attribute; the plain
    /// name for other nodes.
    pub fn qualified_name(&self) -> Vec<u8> {
        let name = self.name();
        match self.namespace().and_then(|ns| ns.prefix) {
            Some(mut p) => {
                p.push(b':');
                p.extend_from_slice(&name);
                p
            }
            None => name,
        }
    }

    /// Parent node (the document node for the root element).
    pub fn parent(&self) -> Option<Node> {
        self.handle(self.raw().parent)
    }

    /// First child (for elements: first non-attribute child).
    pub fn first_child(&self) -> Option<Node> {
        if self.is_tree_leaf() {
            return None;
        }
        self.handle(self.raw().children)
    }

    /// Last child.
    pub fn last_child(&self) -> Option<Node> {
        if self.is_tree_leaf() {
            return None;
        }
        self.handle(self.raw().last)
    }

    /// Next sibling.
    pub fn next_sibling(&self) -> Option<Node> {
        self.handle(self.raw().next)
    }

    /// Previous sibling.
    pub fn prev_sibling(&self) -> Option<Node> {
        self.handle(self.raw().prev)
    }

    /// Text-like nodes have content, not children; entity references point
    /// at declarations that are not part of the tree.
    fn is_tree_leaf(&self) -> bool {
        matches!(
            self.node_type(),
            NodeType::Text
                | NodeType::CData
                | NodeType::Comment
                | NodeType::ProcessingInstruction
                | NodeType::EntityRef
                | NodeType::NamespaceDecl
        )
    }

    /// All children, in order.
    pub fn children(&self) -> Vec<Node> {
        let mut out = Vec::new();
        let mut cur = self.first_child();
        while let Some(n) = cur {
            cur = n.next_sibling();
            out.push(n);
        }
        out
    }

    /// The attribute nodes of an element.
    pub fn attributes(&self) -> Vec<Node> {
        let mut out = Vec::new();
        if !self.is_element() {
            return out;
        }
        let mut cur = self.raw().properties.cast::<ffi::xmlNode>();
        while let Some(n) = self.handle(cur) {
            cur = n.raw().next;
            out.push(n);
        }
        out
    }

    /// Namespace declarations made on an element (`nsDef`).
    pub fn namespace_declarations(&self) -> Vec<Namespace> {
        let mut out = Vec::new();
        if !self.is_element() {
            return out;
        }
        let mut cur = self.raw().nsDef;
        // SAFETY: nsDef is a NULL-terminated list owned by the element.
        unsafe {
            while let Some(ns) = cur.as_ref() {
                out.push(Namespace {
                    prefix: opt_slice(ns.prefix).map(<[u8]>::to_vec),
                    href: opt_slice(ns.href).unwrap_or_default().to_vec(),
                });
                cur = ns.next;
            }
        }
        out
    }

    /// `xmlNodeGetContent`: text content (concatenated for elements).
    pub fn content(&self) -> Vec<u8> {
        // SAFETY: the node is live; the result is a fresh allocation.
        unsafe { take_xml_string(ffi::xmlNodeGetContent(self.ptr.as_ptr())) }.unwrap_or_default()
    }

    /// Replaces the content (children of elements/attributes are replaced
    /// by a text node; referenced children survive detached).
    pub fn set_content(&self, content: &[u8]) {
        self.doc.touch();
        self.doc.rescue_children(self.ptr.as_ptr());
        let c = c_string(content);
        // SAFETY: referenced descendants were detached; libxml2 frees the
        // remaining ones. The node itself stays alive.
        unsafe { ffi::xmlNodeSetContent(self.ptr.as_ptr(), c.as_ptr().cast()) };
    }

    /// Attribute value by name (`xmlGetProp`, no namespace check).
    pub fn attribute(&self, name: &[u8]) -> Option<Vec<u8>> {
        if !self.is_element() {
            return None;
        }
        let n = c_string(name);
        // SAFETY: element is live; result is a fresh allocation.
        unsafe { take_xml_string(ffi::xmlGetProp(self.ptr.as_ptr(), n.as_ptr().cast())) }
    }

    /// Attribute value by local name and namespace URI.
    pub fn attribute_ns(&self, name: &[u8], uri: Option<&[u8]>) -> Option<Vec<u8>> {
        if !self.is_element() {
            return None;
        }
        let n = c_string(name);
        let u = uri.map(c_string);
        // SAFETY: element is live; result is a fresh allocation.
        unsafe {
            take_xml_string(ffi::xmlGetNsProp(
                self.ptr.as_ptr(),
                n.as_ptr().cast(),
                u.as_ref().map_or(std::ptr::null(), |u| u.as_ptr().cast()),
            ))
        }
    }

    /// The attribute node `name` of an element.
    pub fn attribute_node(&self, name: &[u8]) -> Option<Node> {
        if !self.is_element() {
            return None;
        }
        let n = c_string(name);
        // SAFETY: element is live; the attribute belongs to it.
        self.handle(unsafe { ffi::xmlHasProp(self.ptr.as_ptr(), n.as_ptr().cast()) }.cast())
    }

    /// Sets (or replaces the value of) an attribute without namespace.
    pub fn set_attribute(&self, name: &[u8], value: &[u8]) -> Result<Node, TreeError> {
        self.doc.touch();
        if !self.is_element() {
            return Err(TreeError::InvalidNodeType);
        }
        let n = c_string(name);
        // SAFETY: element is live; an existing attribute's children are
        // about to be freed by xmlSetProp, so referenced ones are rescued.
        unsafe {
            let existing = ffi::xmlHasProp(self.ptr.as_ptr(), n.as_ptr().cast());
            if !existing.is_null() {
                self.doc.rescue_descendants(existing.cast());
            }
        }
        let v = c_string(value);
        // SAFETY: valid C strings, live element.
        let attr =
            unsafe { ffi::xmlSetProp(self.ptr.as_ptr(), n.as_ptr().cast(), v.as_ptr().cast()) };
        self.handle(attr.cast()).ok_or(TreeError::Failed)
    }

    /// Removes an attribute by name; returns whether one was removed. A
    /// handle to the attribute keeps it alive (detached).
    pub fn remove_attribute(&self, name: &[u8]) -> bool {
        self.doc.touch();
        match self.attribute_node(name) {
            Some(attr) => {
                attr.unlink();
                true
            }
            None => false,
        }
    }

    /// Declares a namespace on this element (`xmlNewNs`); with `use_it`
    /// the element is put into it.
    pub fn declare_namespace(
        &self,
        prefix: Option<&[u8]>,
        uri: &[u8],
        use_it: bool,
    ) -> Result<(), TreeError> {
        self.doc.touch();
        if !self.is_element() {
            return Err(TreeError::InvalidNodeType);
        }
        let p = prefix.map(c_string);
        let u = c_string(uri);
        // SAFETY: element is live; the namespace is owned by its nsDef list.
        unsafe {
            let ns = ffi::xmlNewNs(
                self.ptr.as_ptr(),
                u.as_ptr().cast(),
                p.as_ref().map_or(std::ptr::null(), |p| p.as_ptr().cast()),
            );
            if ns.is_null() {
                return Err(TreeError::Failed);
            }
            if use_it {
                ffi::xmlSetNs(self.ptr.as_ptr(), ns);
            }
        }
        Ok(())
    }

    /// Looks up the namespace URI bound to `prefix` in scope.
    pub fn lookup_namespace_uri(&self, prefix: Option<&[u8]>) -> Option<Vec<u8>> {
        let p = prefix.map(c_string);
        // SAFETY: node and document are live; the result is owned by the tree.
        unsafe {
            let ns = ffi::xmlSearchNs(
                self.doc.doc(),
                self.ptr.as_ptr(),
                p.as_ref().map_or(std::ptr::null(), |p| p.as_ptr().cast()),
            );
            ns.as_ref()
                .and_then(|ns| opt_slice(ns.href))
                .map(<[u8]>::to_vec)
        }
    }

    fn check_insert(&self, child: &Node) -> Result<(), TreeError> {
        if !Rc::ptr_eq(&self.doc, &child.doc) {
            return Err(TreeError::WrongDocument);
        }
        let parent_ok = matches!(
            self.node_type(),
            NodeType::Element
                | NodeType::Document
                | NodeType::DocumentFragment
                | NodeType::HtmlDocument
        );
        let child_type = child.node_type();
        if !parent_ok || !child_type.is_tree_content() || child_type == NodeType::DocumentFragment {
            return Err(TreeError::InvalidNodeType);
        }
        // The child must not be self or an ancestor of self.
        let mut cur = self.ptr.as_ptr();
        while !cur.is_null() {
            if cur == child.ptr.as_ptr() {
                return Err(TreeError::Hierarchy);
            }
            // SAFETY: walking parent links of live nodes.
            cur = unsafe { (*cur).parent };
        }
        Ok(())
    }

    /// Appends `child` (moving it if attached). Adjacent text nodes are not
    /// merged.
    pub fn append_child(&self, child: &Node) -> Result<(), TreeError> {
        self.insert_before(child, None)
    }

    /// Inserts `child` before `reference` (or at the end when `None`).
    pub fn insert_before(&self, child: &Node, reference: Option<&Node>) -> Result<(), TreeError> {
        self.check_insert(child)?;
        if let Some(r) = reference {
            if r.raw().parent != self.ptr.as_ptr() || r.is_attribute() {
                return Err(TreeError::NotFound);
            }
            if r == child {
                return Ok(());
            }
        }
        let c = child.ptr.as_ptr();
        self.doc.detach(c);
        let parent = self.ptr.as_ptr();
        // SAFETY: `c` is detached (no parent/siblings); parent and reference
        // are live nodes of the same document; we splice the doubly linked
        // child list by hand.
        unsafe {
            (*c).parent = parent;
            match reference {
                None => {
                    let last = (*parent).last;
                    (*c).prev = last;
                    (*c).next = std::ptr::null_mut();
                    if last.is_null() {
                        (*parent).children = c;
                    } else {
                        (*last).next = c;
                    }
                    (*parent).last = c;
                }
                Some(r) => {
                    let r = r.ptr.as_ptr();
                    let prev = (*r).prev;
                    (*c).next = r;
                    (*c).prev = prev;
                    (*r).prev = c;
                    if prev.is_null() {
                        (*parent).children = c;
                    } else {
                        (*prev).next = c;
                    }
                }
            }
            if (*c).type_ == ffi::XML_ELEMENT_NODE {
                ffi::xmlReconciliateNs(self.doc.doc(), c);
            }
        }
        Ok(())
    }

    /// Detaches the node from its parent; it stays alive while handles
    /// exist (the document node cannot be unlinked).
    pub fn unlink(&self) {
        if self.node_type() == NodeType::Document {
            return;
        }
        self.doc.detach(self.ptr.as_ptr());
    }

    /// Copies the node into the same document, detached.
    pub fn copy(&self, deep: bool) -> Result<Node, TreeError> {
        self.document().import(self, deep)
    }

    /// `xmlGetLineNo`.
    pub fn line(&self) -> i64 {
        // SAFETY: the node is live.
        unsafe { ffi::xmlGetLineNo(self.ptr.as_ptr()) }
    }

    /// `xmlGetNodePath` (e.g. `/root/child[2]`).
    pub fn path(&self) -> Option<Vec<u8>> {
        // SAFETY: the node is live; result is a fresh allocation.
        unsafe { take_xml_string(ffi::xmlGetNodePath(self.ptr.as_ptr())) }
    }

    /// `xmlNodeGetBase`.
    pub fn base_uri(&self) -> Option<Vec<u8>> {
        // SAFETY: the node and document are live.
        unsafe { take_xml_string(ffi::xmlNodeGetBase(self.doc.doc(), self.ptr.as_ptr())) }
    }

    /// Serialises the node (`xmlNodeDumpOutput`,
    /// `php_libxml_default_dump_node_to_str`).
    pub fn dump(&self, format: bool, encoding: Option<&[u8]>) -> Vec<u8> {
        let out = Rc::new(RefCell::new(Vec::new()));
        let buf = output_buffer_from_writer(Box::new(SharedVec(out.clone())), std::ptr::null_mut());
        if buf.is_null() {
            return Vec::new();
        }
        let enc = encoding.map(c_string);
        // SAFETY: buf is a fresh output buffer, closed exactly once; node
        // and document are live.
        unsafe {
            ffi::xmlNodeDumpOutput(
                buf,
                self.doc.doc(),
                self.ptr.as_ptr(),
                0,
                c_int::from(format),
                enc.as_ref().map_or(std::ptr::null(), |e| e.as_ptr()),
            );
            ffi::xmlOutputBufferFlush(buf);
            ffi::xmlOutputBufferClose(buf);
        }
        guard::resume();
        out.take()
    }
}

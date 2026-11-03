//! The object model: documents (`php_libxml_ref_obj`) and node handles
//! (`php_libxml_node_object`), plus the node-freeing routines of
//! `ext/libxml` (`php_libxml_node_free_list` and friends).
//!
//! * [`DocRef`] is the shared document reference. It owns the `xmlDoc` and
//!   frees it when the last reference goes away; it carries the document
//!   properties (`formatOutput`, …), the `registerNodeClass` map, the
//!   live-list modification counter and, for `Dom\*` documents, the
//!   namespace mapper / template-content private data.
//! * [`Node`] is the payload of one PHP node object. There is at most one
//!   `Node` per libxml2 node at a time: its address is stored in the
//!   node's `_private` field (exactly like PHP), so navigating to the same
//!   node twice yields the same handle (`===` identity). A `Node` keeps its
//!   document alive. When the last clone of a `Node` is dropped and the
//!   node is not attached to a parent, the node (and its handle-less
//!   descendants) are freed, matching PHP's destruction semantics.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr;
use std::rc::{Rc, Weak};

use crate::ffi;
use crate::raw::{xml_free, N};

/// `php_libxml_class_type`: which API family created the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassType {
    /// Not decided yet (`PHP_LIBXML_CLASS_UNSET`).
    Unset,
    /// The `DOM*` classes.
    Legacy,
    /// The `Dom\*` classes.
    Modern,
}

/// `php_libxml_quirks_mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuirksMode {
    /// No quirks.
    NoQuirks,
    /// Quirks mode.
    Quirks,
    /// Limited quirks mode.
    LimitedQuirks,
}

/// Serialization handlers (`php_libxml_document_handlers`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handlers {
    /// libxml2's serializer.
    Default,
    /// The spec-compliant XML serializer of `Dom\XMLDocument`.
    ModernXml,
}

/// `libxml_doc_props`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct DocProps {
    /// `formatOutput`.
    pub format_output: bool,
    /// `validateOnParse`.
    pub validate_on_parse: bool,
    /// `resolveExternals`.
    pub resolve_externals: bool,
    /// `preserveWhiteSpace`.
    pub preserve_white_space: bool,
    /// `substituteEntities`.
    pub substitute_entities: bool,
    /// `strictErrorChecking`.
    pub strict_error_checking: bool,
    /// `recover`.
    pub recover: bool,
    /// `registerNodeClass` map: base class name (as registered) → user
    /// class name.
    pub classmap: Option<Vec<(String, String)>>,
}

impl Default for DocProps {
    fn default() -> Self {
        Self {
            format_output: false,
            validate_on_parse: false,
            resolve_externals: false,
            preserve_white_space: true,
            substitute_entities: false,
            strict_error_checking: true,
            recover: false,
            classmap: None,
        }
    }
}

/// Shared document state (`php_libxml_ref_obj`).
pub struct DocInner {
    ptr: *mut ffi::xmlDoc,
    pub(crate) props: RefCell<Option<DocProps>>,
    /// `cache_tag.modification_nr`.
    pub(crate) modification_nr: Cell<u64>,
    pub(crate) private: RefCell<Option<Rc<crate::ns::PrivateData>>>,
    pub(crate) class_type: Cell<ClassType>,
    pub(crate) quirks_mode: Cell<QuirksMode>,
    pub(crate) handlers: Cell<Handlers>,
    /// Set once the tree is shared with another object model (`SimpleXML`,
    /// XSL): the `xmlDoc` is then owned through this handle and freed when
    /// the last holder of either model is gone.
    pub(crate) shared: RefCell<Option<php_libxml::tree::SharedDocument>>,
}

impl std::fmt::Debug for DocInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocInner")
            .field("ptr", &self.ptr)
            .field("class_type", &self.class_type.get())
            .finish_non_exhaustive()
    }
}

/// A shared reference to a document.
pub type DocRef = Rc<DocInner>;

thread_local! {
    /// xmlDoc address → its reference object.
    static DOCS: RefCell<HashMap<usize, Weak<DocInner>>> = RefCell::new(HashMap::new());
    /// Number of live documents shared with another object model (fast
    /// path for [`foreign_pinned`]).
    static SHARED_COUNT: Cell<usize> = const { Cell::new(0) };
}

/// Whether another object model (php-libxml handles, e.g. `SimpleXML`)
/// holds `n`: such nodes are rescued, not freed.
pub(crate) fn foreign_pinned(n: N) -> bool {
    if SHARED_COUNT.with(Cell::get) == 0 {
        return false;
    }
    shared_of(n).is_some_and(|s| s.is_pinned(n.p()))
}

fn shared_of(n: N) -> Option<php_libxml::tree::SharedDocument> {
    DocInner::lookup(n.doc_ptr()).and_then(|d| d.shared.borrow().clone())
}

/// Rescues a node pinned by another model out of a subtree being freed.
fn rescue_foreign(n: N) {
    if let Some(s) = shared_of(n) {
        // SAFETY: n is a live node of the shared document.
        unsafe { s.detach(n.p()) };
    } else {
        // SAFETY: live node.
        unsafe { ffi::xmlUnlinkNode(n.p()) };
    }
}

impl DocInner {
    /// Takes ownership of `doc` (`php_libxml_increment_doc_ref` with a new
    /// reference object).
    pub(crate) fn new(doc: *mut ffi::xmlDoc) -> DocRef {
        let inner = Rc::new(DocInner {
            ptr: doc,
            props: RefCell::new(None),
            modification_nr: Cell::new(1),
            private: RefCell::new(None),
            class_type: Cell::new(ClassType::Unset),
            quirks_mode: Cell::new(QuirksMode::NoQuirks),
            handlers: Cell::new(Handlers::Default),
            shared: RefCell::new(None),
        });
        DOCS.with(|d| d.borrow_mut().insert(doc as usize, Rc::downgrade(&inner)));
        inner
    }

    /// Finds the reference object owning `doc`.
    pub(crate) fn lookup(doc: *mut ffi::xmlDoc) -> Option<DocRef> {
        if doc.is_null() {
            return None;
        }
        DOCS.with(|d| d.borrow().get(&(doc as usize)).and_then(Weak::upgrade))
    }

    /// The document as a node.
    pub(crate) fn n(&self) -> N {
        N::from_doc(self.ptr).expect("document pointer is never NULL")
    }

    /// `php_dom_follow_spec_doc_ref`.
    pub(crate) fn is_modern(&self) -> bool {
        self.class_type.get() == ClassType::Modern
    }

    /// Read-only view of the properties (defaults when never written).
    pub(crate) fn props(&self) -> DocProps {
        self.props.borrow().clone().unwrap_or_default()
    }

    /// Mutates the properties (`dom_get_doc_props`).
    pub(crate) fn with_props<R>(&self, f: impl FnOnce(&mut DocProps) -> R) -> R {
        let mut p = self.props.borrow_mut();
        f(p.get_or_insert_with(DocProps::default))
    }

    /// `dom_get_strict_error`.
    pub(crate) fn strict(&self) -> bool {
        self.props
            .borrow()
            .as_ref()
            .is_none_or(|p| p.strict_error_checking)
    }

    /// `php_libxml_invalidate_node_list_cache`.
    pub(crate) fn invalidate(&self) {
        self.modification_nr
            .set(self.modification_nr.get().wrapping_add(1));
        if let Some(s) = self.shared.borrow().as_ref() {
            s.mark_modified();
        }
    }

    /// The tag node-list caches compare against: this document's
    /// modification counter combined with the mutations made by another
    /// object model sharing the tree.
    pub(crate) fn cache_nr(&self) -> u64 {
        let shared = self
            .shared
            .borrow()
            .as_ref()
            .map_or(0, php_libxml::tree::SharedDocument::generation);
        self.modification_nr
            .get()
            .wrapping_add(shared.wrapping_mul(0x9e37_79b9))
    }

    /// The shared-ownership handle of this document, creating it (and
    /// handing the `xmlDoc` over to it) on first use.
    pub(crate) fn shared_document(&self) -> Option<php_libxml::tree::SharedDocument> {
        if let Some(s) = self.shared.borrow().as_ref() {
            return Some(s.clone());
        }
        let ptr = std::ptr::NonNull::new(self.ptr)?;
        // SAFETY: we own the document; from now on it is only freed
        // through the shared handle (see Drop).
        let s = unsafe { php_libxml::tree::SharedDocument::from_raw(ptr) };
        *self.shared.borrow_mut() = Some(s.clone());
        SHARED_COUNT.with(|c| c.set(c.get() + 1));
        Some(s)
    }

    /// A reference object for a tree another model shares with us.
    pub(crate) fn new_shared(shared: php_libxml::tree::SharedDocument) -> DocRef {
        if let Some(existing) = DocInner::lookup(shared.as_ptr()) {
            if existing.shared.borrow().is_none() {
                // Cannot happen: a document is shared through exactly one
                // ownership path; keep ours.
            }
            return existing;
        }
        let d = DocInner::new(shared.as_ptr());
        *d.shared.borrow_mut() = Some(shared);
        SHARED_COUNT.with(|c| c.set(c.get() + 1));
        d
    }

    /// `dom_minimum_modification_nr_since_parsing`.
    fn min_modification_nr(&self) -> u64 {
        if self.class_type.get() == ClassType::Modern {
            1
        } else {
            2
        }
    }

    /// `dom_mark_document_cache_as_modified_since_parsing`.
    pub(crate) fn mark_modified_since_parsing(&self) {
        let v = (self.min_modification_nr() + 1).max(self.modification_nr.get());
        self.modification_nr.set(v);
    }

    /// `dom_is_document_cache_modified_since_parsing`.
    pub(crate) fn modified_since_parsing(&self) -> bool {
        self.modification_nr.get() > self.min_modification_nr()
    }

    /// The private data of a `Dom\*` document.
    pub(crate) fn private_data(&self) -> Option<Rc<crate::ns::PrivateData>> {
        self.private.borrow().clone()
    }
}

impl Drop for DocInner {
    fn drop(&mut self) {
        DOCS.with(|d| {
            let mut d = d.borrow_mut();
            if d.get(&(self.ptr as usize))
                .is_some_and(|w| w.strong_count() == 0)
            {
                d.remove(&(self.ptr as usize));
            }
        });
        // Private data first (template fragments), as PHP does.
        if let Some(private) = self.private.borrow_mut().take() {
            private.destroy();
        }
        if !self.ptr.is_null() {
            // The document node's handle (if any) is gone: every Node holds
            // a DocRef, so none can remain.
            self.n().set_private(ptr::null_mut());
            if let Some(shared) = self.shared.borrow_mut().take() {
                // Co-owned: the tree goes with the last holder of either
                // object model.
                SHARED_COUNT.with(|c| c.set(c.get().saturating_sub(1)));
                drop(shared);
            } else {
                // SAFETY: no handle references this document any more (every
                // handle holds a strong reference), so we own the tree.
                unsafe { ffi::xmlFreeDoc(self.ptr) };
            }
        }
    }
}

/// Extra per-object state for `Dom\*` objects with `[SameObject]`
/// properties.
#[derive(Default)]
pub(crate) struct NodeExtra {
    pub class_list: Option<crate::tokenlist::TokenList>,
    pub children: Option<crate::lists::NodeList>,
    pub implementation: Option<crate::implementation::Implementation>,
    /// Objects kept alive for as long as the handle exists (see
    /// [`Node::attach_keepalive`]); dropped after the node is released.
    pub keepalive: Vec<Rc<dyn Any>>,
}

/// The payload of a PHP DOM node object.
pub struct NodeInner {
    ptr: Cell<*mut ffi::xmlNode>,
    /// The PHP class of the object (fixed at creation).
    class: RefCell<std::borrow::Cow<'static, str>>,
    /// Whether the object belongs to the `Dom\*` family.
    modern: Cell<bool>,
    /// The built-in DOM class the object's class derives from.
    builtin: Cell<&'static str>,
    doc: RefCell<Option<DocRef>>,
    /// For `DOMNameSpaceNode`: the element the declaration belongs to.
    pub(crate) ns_parent: RefCell<Option<Node>>,
    pub(crate) extra: RefCell<NodeExtra>,
    host: RefCell<Option<Box<dyn Any>>>,
}

impl std::fmt::Debug for NodeInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeInner")
            .field("ptr", &self.ptr.get())
            .field("class", &self.class.borrow())
            .field("modern", &self.modern.get())
            .finish_non_exhaustive()
    }
}

/// A handle to a DOM node object (cheap to clone; clones are the same
/// PHP object).
#[derive(Clone)]
pub struct Node(pub(crate) Rc<NodeInner>);

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.n() {
            Some(n) => write!(f, "Node({:?} {:?})", n, String::from_utf8_lossy(n.name())),
            None => f.write_str("Node(<detached>)"),
        }
    }
}

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for Node {}

impl Node {
    /// `php_dom_object_get_data`: the existing handle of a node, if any.
    pub(crate) fn existing(n: N) -> Option<Node> {
        let p = n.private().cast::<NodeInner>().cast_const();
        if p.is_null() {
            return None;
        }
        // SAFETY: `_private` is only ever set to `Rc::as_ptr` of a live
        // NodeInner (see `wrap`), and cleared in NodeInner::drop before the
        // allocation is released; the strong count is therefore >= 1.
        unsafe {
            Rc::increment_strong_count(p);
            Some(Node(Rc::from_raw(p)))
        }
    }

    /// `php_dom_create_object`: returns the handle of `n`, creating one when
    /// needed. The new handle references the document that owns the node.
    pub(crate) fn wrap(n: N) -> Node {
        if let Some(existing) = Node::existing(n) {
            return existing;
        }
        let doc = DocInner::lookup(n.doc_ptr());
        Self::wrap_with_doc(n, doc)
    }

    /// Creates a handle with an explicit document reference
    /// (`php_dom_instantiate_object_helper`).
    pub(crate) fn wrap_with_doc(n: N, doc: Option<DocRef>) -> Node {
        if let Some(existing) = Node::existing(n) {
            return existing;
        }
        let modern = doc.as_ref().is_some_and(|d| d.is_modern());
        let class = crate::classes::class_for_node(n, modern, doc.as_ref());
        let builtin = crate::classes::builtin_class(n, modern).unwrap_or("DOMNode");
        let inner = Rc::new(NodeInner {
            ptr: Cell::new(n.p()),
            class: RefCell::new(class),
            modern: Cell::new(modern),
            builtin: Cell::new(builtin),
            doc: RefCell::new(doc),
            ns_parent: RefCell::new(None),
            extra: RefCell::new(NodeExtra::default()),
            host: RefCell::new(None),
        });
        n.set_private(Rc::as_ptr(&inner).cast_mut().cast::<c_void>());
        Node(inner)
    }

    /// Creates a handle for a fake namespace-declaration node
    /// (`DOMNameSpaceNode`).
    pub(crate) fn wrap_namespace(n: N, parent: Option<Node>) -> Node {
        let doc = parent
            .as_ref()
            .and_then(Node::doc_ref)
            .or_else(|| DocInner::lookup(n.doc_ptr()));
        let node = Self::wrap_with_doc(n, doc);
        *node.0.ns_parent.borrow_mut() = parent;
        node
    }

    /// A handle that is not bound to any node (an object constructed
    /// without its constructor having run, e.g. via reflection).
    ///
    /// `builtin` is the built-in DOM class the object's class derives from
    /// (e.g. `"DOMElement"` for `class Foo extends DOMElement`).
    pub fn new_unbound(
        class: impl Into<std::borrow::Cow<'static, str>>,
        builtin: &'static str,
    ) -> Node {
        let modern = builtin.starts_with("Dom\\");
        Node(Rc::new(NodeInner {
            ptr: Cell::new(ptr::null_mut()),
            class: RefCell::new(class.into()),
            modern: Cell::new(modern),
            builtin: Cell::new(builtin),
            doc: RefCell::new(None),
            ns_parent: RefCell::new(None),
            extra: RefCell::new(NodeExtra::default()),
            host: RefCell::new(None),
        }))
    }

    /// The node pointer, if bound.
    pub(crate) fn n(&self) -> Option<N> {
        N::new(self.0.ptr.get())
    }

    /// The document reference object.
    pub(crate) fn doc_ref(&self) -> Option<DocRef> {
        self.0.doc.borrow().clone()
    }

    pub(crate) fn set_doc_ref(&self, doc: Option<DocRef>) {
        *self.0.doc.borrow_mut() = doc;
    }

    /// Rebinds this handle to another node (used by `DOMDocument` when a
    /// new document is loaded into an existing object).
    pub(crate) fn rebind(&self, n: N, doc: DocRef) {
        if let Some(old) = self.n() {
            if old.private() == Rc::as_ptr(&self.0).cast_mut().cast::<c_void>() {
                old.set_private(ptr::null_mut());
            }
        }
        self.0.ptr.set(n.p());
        n.set_private(Rc::as_ptr(&self.0).cast_mut().cast::<c_void>());
        self.set_doc_ref(Some(doc));
    }

    /// Binds a freshly constructed node to this (constructor) object,
    /// releasing a node bound by an earlier constructor call
    /// (`php_libxml_node_decrement_resource` + `php_libxml_increment_node_ptr`).
    pub(crate) fn bind_constructed(&self, n: N) {
        self.release_node();
        self.0.ptr.set(n.p());
        n.set_private(Rc::as_ptr(&self.0).cast_mut().cast::<c_void>());
    }

    /// Releases the bound node like object destruction does.
    pub(crate) fn release_node(&self) {
        if let Some(old) = self.n() {
            if old.private() == Rc::as_ptr(&self.0).cast_mut().cast::<c_void>() {
                old.set_private(ptr::null_mut());
            }
            match old.t() {
                ffi::XML_NOTATION_NODE => free_notation(old),
                ffi::XML_DOCUMENT_NODE | ffi::XML_HTML_DOCUMENT_NODE => {}
                _ => free_resource(old),
            }
            self.0.ptr.set(ptr::null_mut());
        }
        self.set_doc_ref(None);
    }

    /// Unbinds the handle (`php_libxml_unregister_node` with an object).
    pub(crate) fn unbind(&self) {
        if let Some(n) = self.n() {
            n.set_private(ptr::null_mut());
        }
        self.0.ptr.set(ptr::null_mut());
        self.set_doc_ref(None);
    }

    /// `php_dom_follow_spec_intern`.
    pub(crate) fn is_modern(&self) -> bool {
        self.0.doc.borrow().as_ref().is_some_and(|d| d.is_modern())
    }

    /// Whether two handles refer to the same object.
    pub fn same(&self, other: &Node) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    /// A stable identifier of the object while it is alive.
    pub fn object_id(&self) -> usize {
        Rc::as_ptr(&self.0) as usize
    }

    /// Embedder data attached to this object (e.g. the VM's object handle).
    pub fn host_data(&self) -> std::cell::RefMut<'_, Option<Box<dyn Any>>> {
        self.0.host.borrow_mut()
    }

    /// Whether the handle is bound to a node.
    pub fn is_bound(&self) -> bool {
        !self.0.ptr.get().is_null()
    }

    /// The PHP class name of this object.
    pub fn class_name(&self) -> String {
        self.0.class.borrow().to_string()
    }

    /// Overrides the PHP class name (the embedder instantiated a subclass).
    pub fn set_class_name(&self, class: impl Into<std::borrow::Cow<'static, str>>) {
        *self.0.class.borrow_mut() = class.into();
    }

    /// Whether the object is a `Dom\Node` (as opposed to a `DOMNode`).
    pub fn family_is_modern(&self) -> bool {
        self.0.modern.get()
    }

    pub(crate) fn set_family_modern(&self, modern: bool) {
        self.0.modern.set(modern);
    }

    /// The built-in DOM class this object's class derives from.
    pub fn builtin_class(&self) -> &'static str {
        self.0.builtin.get()
    }

    pub(crate) fn set_builtin_class(&self, c: &'static str) {
        self.0.builtin.set(c);
    }

    /// `DOM_GET_OBJ`: the node, or "Couldn't fetch".
    pub(crate) fn node(&self) -> crate::error::DomResult<N> {
        self.n().ok_or(crate::error::DomError::CouldntFetch)
    }

    /// `DOM_PROP_NODE`: the node, or an `InvalidStateError`.
    pub(crate) fn prop_node(&self) -> crate::error::DomResult<N> {
        self.n().ok_or_else(|| {
            crate::error::DomError::dom(crate::error::DomExceptionCode::InvalidState)
        })
    }
}

impl Drop for NodeInner {
    fn drop(&mut self) {
        let Some(n) = N::new(self.ptr.get()) else {
            return;
        };
        // php_libxml_decrement_node_ptr: the node no longer has an object.
        if n.private() == ptr::from_mut(self).cast::<c_void>() {
            n.set_private(ptr::null_mut());
        }
        match n.t() {
            ffi::XML_NOTATION_NODE => free_notation(n),
            ffi::XML_DOCUMENT_NODE | ffi::XML_HTML_DOCUMENT_NODE => {}
            _ => free_resource(n),
        }
        self.ptr.set(ptr::null_mut());
        // The document reference (if any) is released after the node.
        let doc = self.doc.borrow_mut().take();
        drop(doc);
        let parent = self.ns_parent.borrow_mut().take();
        drop(parent);
    }
}

// ---------------------------------------------------------------------------
// Node freeing (ext/libxml/libxml.c)
// ---------------------------------------------------------------------------

/// `LIBXML_NS_TAG_HOOK`.
pub(crate) const NS_TAG_HOOK: usize = 1;

/// `php_libxml_set_old_ns_list`.
pub(crate) fn set_old_ns_list(
    doc: *mut ffi::xmlDoc,
    first: *mut ffi::xmlNs,
    last: *mut ffi::xmlNs,
) {
    if doc.is_null() {
        return;
    }
    // SAFETY: doc is a live document; first..last is a detached ns chain
    // whose ownership moves to the document.
    unsafe {
        if (*doc).oldNs.is_null() {
            let ns = alloc_ns();
            if ns.is_null() {
                return;
            }
            (*ns).type_ = ffi::XML_LOCAL_NAMESPACE;
            (*ns).href = crate::raw::xml_strdup(ffi::XML_XML_NAMESPACE);
            (*ns).prefix = crate::raw::xml_strdup(b"xml");
            (*doc).oldNs = ns;
        } else {
            (*last).next = (*(*doc).oldNs).next;
        }
        (*(*doc).oldNs).next = first;
    }
}

/// `php_libxml_set_old_ns`.
pub(crate) fn set_old_ns(doc: *mut ffi::xmlDoc, ns: *mut ffi::xmlNs) {
    set_old_ns_list(doc, ns, ns);
}

/// Allocates a zeroed `xmlNs` with libxml2's allocator.
pub(crate) fn alloc_ns() -> *mut ffi::xmlNs {
    // SAFETY: xmlMalloc is libxml2's allocator; the memory is zeroed before
    // use and released by libxml2 (xmlFreeNs) later.
    unsafe {
        let Some(malloc) = ffi::xmlMalloc else {
            return ptr::null_mut();
        };
        let p = malloc(std::mem::size_of::<ffi::xmlNs>()).cast::<ffi::xmlNs>();
        if !p.is_null() {
            ptr::write_bytes(p, 0, 1);
        }
        p
    }
}

/// `php_libxml_unregister_node`.
pub(crate) fn unregister_node(n: N) {
    if let Some(obj) = Node::existing(n) {
        obj.unbind();
    }
}

/// `php_libxml_unlink_entity_decl`.
fn unlink_entity_decl(n: N) {
    let e = n.ent();
    let dtd = e.parent;
    if dtd.is_null() {
        return;
    }
    // SAFETY: dtd is the live parent DTD of the entity.
    unsafe {
        let ents = (*dtd).entities;
        if !ents.is_null() && ffi::xmlHashLookup(ents, e.name) == n.p().cast() {
            ffi::xmlHashRemoveEntry(ents, e.name, None);
        }
        let pents = (*dtd).pentities;
        if !pents.is_null() && ffi::xmlHashLookup(pents, e.name) == n.p().cast() {
            ffi::xmlHashRemoveEntry(pents, e.name, None);
        }
    }
}

unsafe extern "C" fn unlink_entity_scanner(
    payload: *mut c_void,
    table: *mut c_void,
    name: *const u8,
) {
    // SAFETY: payload is an xmlEntity of the scanned table.
    unsafe {
        let entity = payload.cast::<ffi::xmlEntity>();
        if !(*entity)._private.is_null() {
            ffi::xmlHashRemoveEntry(table, name, None);
        }
    }
}

/// `dom_free_notation`.
pub(crate) fn free_notation(n: N) {
    let e = n.ent();
    xml_free(e.name.cast_mut().cast());
    xml_free(e.ExternalID.cast_mut().cast());
    xml_free(e.SystemID.cast_mut().cast());
    xml_free(n.p().cast());
}

/// `php_libxml_node_free`.
pub(crate) fn node_free(n: N) {
    if !n.private().is_null() {
        // ((php_libxml_node_ptr *) node->_private)->node = NULL
        unregister_node(n);
    }
    // SAFETY: n is a live node without handles that is being destroyed;
    // every branch mirrors php_libxml_node_free for libxml2 < 2.12.
    unsafe {
        match n.t() {
            ffi::XML_ATTRIBUTE_NODE => ffi::xmlFreeProp(n.attr()),
            ffi::XML_ENTITY_DECL => {
                let e = n.ent();
                if e.etype != ffi::XML_INTERNAL_PREDEFINED_ENTITY {
                    unlink_entity_decl(n);
                    if !e.children.is_null() && e.owner != 0 && n.p() == (*e.children).parent {
                        ffi::xmlFreeNodeList(e.children);
                    }
                    let dict = if e.doc.is_null() {
                        ptr::null_mut()
                    } else {
                        (*e.doc).dict
                    };
                    let free_s = |s: *const u8| {
                        if !s.is_null() && (dict.is_null() || ffi::xmlDictOwns(dict, s) == 0) {
                            xml_free(s.cast_mut().cast());
                        }
                    };
                    free_s(e.name);
                    free_s(e.ExternalID);
                    free_s(e.SystemID);
                    free_s(e.URI);
                    free_s(e.content);
                    free_s(e.orig);
                    xml_free(n.p().cast());
                }
            }
            ffi::XML_NOTATION_NODE => {
                let e = n.ent();
                xml_free(e.name.cast_mut().cast());
                xml_free(e.ExternalID.cast_mut().cast());
                xml_free(e.SystemID.cast_mut().cast());
                xml_free(n.p().cast());
            }
            ffi::XML_ELEMENT_DECL | ffi::XML_ATTRIBUTE_DECL => {}
            ffi::XML_NAMESPACE_DECL => {
                let ns = n.ns_ptr();
                if !ns.is_null() {
                    ffi::xmlFreeNs(ns);
                    n.set_ns(ptr::null_mut());
                }
                n.set_type(ffi::XML_ELEMENT_NODE);
                ffi::xmlFreeNode(n.p());
            }
            ffi::XML_DTD_NODE => {
                let dtd = n.dtd();
                if dtd._private.is_null() {
                    if !dtd.entities.is_null() {
                        ffi::xmlHashScan(dtd.entities, Some(unlink_entity_scanner), dtd.entities);
                    }
                    if !dtd.pentities.is_null() {
                        ffi::xmlHashScan(dtd.pentities, Some(unlink_entity_scanner), dtd.pentities);
                    }
                }
                ffi::xmlFreeDtd(n.as_dtd());
            }
            ffi::XML_ELEMENT_NODE => {
                if let Some(ns) = n.ns() {
                    let tagged = ns.private() as usize;
                    if tagged & 1 == NS_TAG_HOOK {
                        crate::ns::run_ns_hook(tagged & !1, n);
                    }
                }
                let def = n.ns_def();
                if let (Some(first), false) = (def, n.doc_ptr().is_null()) {
                    let mut last = first;
                    while let Some(nx) = last.next() {
                        last = nx;
                    }
                    set_old_ns_list(n.doc_ptr(), first.p(), last.p());
                    n.set_ns_def(ptr::null_mut());
                }
                ffi::xmlFreeNode(n.p());
            }
            _ => ffi::xmlFreeNode(n.p()),
        }
    }
}

/// `php_libxml_node_free_list`.
pub(crate) fn free_list(first: Option<N>) {
    let mut cur = first;
    while let Some(node) = cur {
        if node.private().is_null() && foreign_pinned(node) {
            let next = node.next();
            rescue_foreign(node);
            cur = next;
            continue;
        }
        if !node.private().is_null() {
            let next = node.next();
            // SAFETY: node is live; unlinking keeps it alive as a detached
            // root owned by its handle.
            unsafe { ffi::xmlUnlinkNode(node.p()) };
            if node.is_element() {
                if let Some(obj) = Node::existing(node) {
                    let legacy = obj.doc_ref().is_none_or(|d| !d.is_modern());
                    if legacy {
                        reconciliate_detached(node);
                    }
                }
            }
            cur = next;
            continue;
        }
        match node.t() {
            ffi::XML_ENTITY_REF_NODE | ffi::XML_NOTATION_NODE => {}
            ffi::XML_ENTITY_DECL => unlink_entity_decl(node),
            ffi::XML_ATTRIBUTE_NODE => {
                if !node.doc_ptr().is_null() && node.atype() == ffi::XML_ATTRIBUTE_ID {
                    // SAFETY: live attribute of a live document.
                    unsafe { ffi::xmlRemoveID(node.doc_ptr(), node.attr()) };
                }
                free_list(node.children());
            }
            ffi::XML_ATTRIBUTE_DECL
            | ffi::XML_DTD_NODE
            | ffi::XML_DOCUMENT_TYPE_NODE
            | ffi::XML_NAMESPACE_DECL
            | ffi::XML_TEXT_NODE => free_list(node.children()),
            _ => {
                free_list(node.children());
                free_list(node.properties());
            }
        }
        cur = node.next();
        // SAFETY: node is live and handle-less.
        unsafe { ffi::xmlUnlinkNode(node.p()) };
        unregister_node(node);
        node_free(node);
    }
}

/// `xmlReconciliateNs` for a subtree that may have no document (the
/// libxml2 < 2.13 workaround of `php_libxml_node_free_list`).
fn reconciliate_detached(node: N) {
    // SAFETY: node is a live detached element.
    unsafe {
        if node.doc_ptr().is_null() {
            let mut dummy: ffi::xmlDoc = std::mem::zeroed();
            dummy.type_ = ffi::XML_DOCUMENT_NODE;
            node.set_doc_raw(&raw mut dummy);
            ffi::xmlReconciliateNs(&raw mut dummy, node.p());
            node.set_doc_raw(ptr::null_mut());
            if !dummy.oldNs.is_null() {
                let old = (*node.p()).nsDef;
                (*node.p()).nsDef = dummy.oldNs;
                (*dummy.oldNs).next = old;
            }
        } else {
            ffi::xmlReconciliateNs(node.doc_ptr(), node.p());
        }
    }
}

/// `php_libxml_node_free_resource`.
pub(crate) fn free_resource(n: N) {
    match n.t() {
        ffi::XML_DOCUMENT_NODE | ffi::XML_HTML_DOCUMENT_NODE => {}
        ffi::XML_ENTITY_REF_NODE => {
            unregister_node(n);
            if n.parent().is_none() {
                node_free(n);
            }
        }
        t => {
            if n.parent().is_none() && t != ffi::XML_NAMESPACE_DECL && foreign_pinned(n) {
                // Another model still holds the detached root.
                unregister_node(n);
                return;
            }
            if n.parent().is_none() || t == ffi::XML_NAMESPACE_DECL {
                free_list(n.children());
                if t == ffi::XML_ELEMENT_NODE {
                    free_list(n.properties());
                }
                unregister_node(n);
                node_free(n);
            } else {
                unregister_node(n);
            }
        }
    }
}

/// `node_list_unlink`: detaches descendants that have handles so that the
/// list can be freed.
pub(crate) fn node_list_unlink(first: Option<N>) {
    let mut cur = first;
    while let Some(node) = cur {
        let next = node.next();
        if node.private().is_null() && foreign_pinned(node) {
            rescue_foreign(node);
        } else if !node.private().is_null() {
            // SAFETY: live node.
            unsafe { ffi::xmlUnlinkNode(node.p()) };
        } else if node.t() != ffi::XML_ENTITY_REF_NODE {
            node_list_unlink(node.children());
            match node.t() {
                ffi::XML_ATTRIBUTE_DECL
                | ffi::XML_DTD_NODE
                | ffi::XML_DOCUMENT_TYPE_NODE
                | ffi::XML_ENTITY_DECL
                | ffi::XML_ATTRIBUTE_NODE
                | ffi::XML_TEXT_NODE => {}
                _ => node_list_unlink(node.properties()),
            }
        }
        cur = next;
    }
}

/// `dom_remove_all_children`.
pub(crate) fn remove_all_children(n: N) {
    if n.children().is_some() {
        node_list_unlink(n.children());
        free_list(n.children());
        n.set_children(None);
        n.set_last(None);
    }
}

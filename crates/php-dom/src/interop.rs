//! Raw interoperability with other libxml2-based extensions
//! (`php_libxml_import_node` / `php_dom_create_object` equivalents), used
//! by the XSL binding's `DomAccess` provider and similar consumers.
//!
//! Pointers are exchanged as `NonNull<c_void>` (`xmlDoc*` / `xmlNode*`)
//! so that crates with their own libxml2 FFI declarations can cast them.

use std::any::Any;
use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::rc::Rc;

use crate::ffi;
use crate::object::{DocInner, Node};
use crate::raw::{cs, Ns, N};

impl Node {
    /// The `xmlNode*` behind this handle (`None` when unbound).
    pub fn raw_node(&self) -> Option<NonNull<c_void>> {
        self.n().and_then(|n| NonNull::new(n.p().cast()))
    }

    /// Whether the handle wraps a document node.
    pub fn is_document_node(&self) -> bool {
        self.n().is_some_and(N::is_doc)
    }

    /// The owner `xmlDoc*` plus an object that keeps the document alive
    /// while held (`None` for unbound or detached document-less nodes).
    pub fn raw_document(&self) -> Option<(NonNull<c_void>, Rc<dyn Any>)> {
        let n = self.n()?;
        let doc = NonNull::new(n.doc_ptr().cast::<c_void>())?;
        let keep: Rc<dyn Any> = match self.doc_ref() {
            Some(d) => d,
            None => Rc::new(self.clone()),
        };
        Some((doc, keep))
    }

    /// Whether the document of this (freshly cloned) document object is
    /// referenced by any other object (`document->refcount > 1`).
    pub fn document_shared(&self) -> bool {
        self.doc_ref().is_some_and(|d| Rc::strong_count(&d) > 2)
    }

    /// Keeps `value` alive for as long as this handle exists; it is
    /// released after the node itself (use for nodes of documents owned
    /// by another crate, e.g. an XSLT input copy).
    pub fn attach_keepalive(&self, value: Rc<dyn Any>) {
        self.0.extra.borrow_mut().keepalive.push(value);
    }
}

/// Takes ownership of a libxml2 document (e.g. an XSLT result tree) and
/// returns its document object: `DOMDocument` (or `Dom\XMLDocument` when
/// `modern`), with the PHP class `class` when given (a user subclass; no
/// constructor is run).
///
/// # Safety
/// `doc` must be a valid `xmlDoc*` that nothing else frees.
pub unsafe fn adopt_document(doc: NonNull<c_void>, modern: bool, class: Option<&str>) -> Node {
    let docp = doc.as_ptr().cast::<ffi::xmlDoc>();
    let obj = if modern {
        let (obj, d) = crate::document::instantiate_modern(docp);
        crate::document::convert_to_modern(&d);
        obj
    } else {
        let d = DocInner::new(docp);
        let n = N::from_doc(docp).expect("non-null document");
        Node::wrap_with_doc(n, Some(d))
    };
    if let Some(c) = class {
        obj.set_class_name(c.to_string());
    }
    obj
}

/// The object for a node of a live document (`php_dom_create_object`):
/// the existing handle if there is one.
///
/// # Safety
/// `node` must be a valid `xmlNode*` (or `xmlAttr*`) that stays alive while
/// the handle exists; for documents not owned by this crate, attach a
/// keep-alive with [`Node::attach_keepalive`].
pub unsafe fn wrap_raw_node(node: NonNull<c_void>) -> Node {
    let n = N::new(node.as_ptr().cast()).expect("non-null node");
    Node::wrap(n)
}

/// A `DOMNameSpaceNode` for the declaration `prefix`/`href` on `parent`
/// (an `XPath` namespace node handed to a callback).
pub fn namespace_node(parent: &Node, prefix: Option<&[u8]>, href: &[u8]) -> Option<Node> {
    let parent_n = parent.n()?;
    let h = cs(href);
    let p = prefix.map(cs);
    // SAFETY: a temporary unlinked namespace record, copied by
    // fake_ns_node_ptr and freed right after.
    unsafe {
        let tmp = ffi::xmlNewNs(
            ptr::null_mut(),
            h.as_ptr().cast(),
            p.as_ref().map_or(ptr::null(), |c| c.as_ptr().cast()),
        );
        let ns = Ns::new(tmp)?;
        let fake = crate::xpath::fake_ns_node_ptr(Some(parent_n), ns, ptr::null_mut());
        ffi::xmlFreeNs(tmp);
        Some(Node::wrap_namespace(fake, Some(parent.clone())))
    }
}

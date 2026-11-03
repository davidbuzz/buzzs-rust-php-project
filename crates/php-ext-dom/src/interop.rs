//! The public API other libxml-based bindings use to exchange nodes with
//! the DOM extension (`php_libxml_import_node` and
//! `php_dom_create_object` in php-src).
//!
//! * [`import_node`] — the libxml2 node (and document) behind a DOM
//!   object, plus a keep-alive handle.
//! * [`share_node`] — the same, switching the document to shared
//!   ownership ([`SharedDocument`]) so another object model can co-own
//!   the tree (`simplexml_import_dom()`): pin nodes with
//!   [`SharedDocument::pin`]; DOM's live node lists follow the other
//!   model's mutations through the shared generation counter.
//! * [`shared_node_object`] — the PHP DOM object of a node of a shared
//!   tree (`dom_import_simplexml()`, `XMLReader::expand()`), legacy or
//!   modern (`Dom\*`) family; the existing object when there is one.
//! * [`serialize_shared_node`] — DOM's modern XML serializer for a shared
//!   `Dom\XMLDocument` tree (`SimpleXMLElement::asXML()` on it).
//! * [`node_object`] — the PHP DOM object of a node of a document DOM
//!   already knows (or a detached node handed over), with an optional
//!   keep-alive.
//!
//! Pointers are `NonNull<c_void>` (`xmlNode*` / `xmlDoc*`) so crates with
//! their own libxml2 declarations can cast them.

use std::any::Any;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;

use php_dom::dispatch::DomObject;
use php_libxml::tree::SharedDocument;
use php_runtime::{Obj, Value};
use php_vm::ext::VmResult;
use php_vm::Vm;

use crate::state::dom_object;

/// A node behind a DOM object.
#[derive(Clone)]
pub struct ImportedNode {
    /// The `xmlNode*` (for a document object: the `xmlDoc*` itself).
    pub node: NonNull<c_void>,
    /// The owner `xmlDoc*` (`None` for a node that belongs to no document).
    pub document: Option<NonNull<c_void>>,
    /// Keeps the node and its document alive while held.
    pub keepalive: Rc<dyn Any>,
    /// Whether the object is a `Dom\Node` (the document follows the
    /// spec-compliant API) rather than a `DOMNode`.
    pub modern: bool,
    /// Whether the object wraps the document node itself.
    pub is_document: bool,
}

impl std::fmt::Debug for ImportedNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportedNode")
            .field("node", &self.node)
            .field("document", &self.document)
            .field("modern", &self.modern)
            .field("is_document", &self.is_document)
            .finish_non_exhaustive()
    }
}

/// `php_libxml_import_node` for DOM objects: `None` when `obj` is not a
/// DOM node object or is not bound to a node (constructor not run).
pub fn import_node(obj: &Obj) -> Option<ImportedNode> {
    let DomObject::Node(n) = dom_object(obj)? else {
        return None;
    };
    let node = n.raw_node()?;
    let document = n.raw_document().map(|(d, _)| d);
    Some(ImportedNode {
        node,
        document,
        modern: n.family_is_modern(),
        is_document: n.is_document_node(),
        keepalive: Rc::new(n),
    })
}

/// `php_dom_create_object`: the PHP object of the libxml2 node `node`.
///
/// When the node's document is owned by the DOM extension the object
/// belongs to that document's family (legacy `DOM*` or modern `Dom\*`)
/// and honours `registerNodeClass()`. Nodes of documents owned by another
/// binding get legacy objects; `keepalive` (e.g. the other binding's
/// document handle) is then kept for as long as the object's node handle
/// lives. Passing an existing node returns its existing object.
///
/// # Safety
/// `node` must be a live `xmlNode*` (or `xmlAttr*`/`xmlDoc*`) that stays
/// alive while the returned object exists: it is attached to a document
/// that `keepalive` (or the DOM extension) keeps alive, or it is a
/// detached node whose ownership passes to the DOM extension.
pub unsafe fn node_object(
    vm: &mut Vm,
    node: NonNull<c_void>,
    keepalive: Option<Rc<dyn Any>>,
) -> VmResult<Value> {
    // SAFETY: forwarded to the caller.
    let n = unsafe { php_dom::interop::wrap_raw_node(node) };
    if let Some(k) = keepalive {
        n.attach_keepalive(k);
    }
    Ok(Value::Object(crate::state::node_object(vm, &n)?))
}

/// `simplexml_import_dom()` from the DOM side: the node behind `obj` and
/// the shared-ownership handle of its document (switching the document to
/// shared ownership on first use). `None` for non-DOM objects, unbound
/// objects and nodes without a document.
pub fn share_node(obj: &Obj) -> Option<(SharedDocument, NonNull<c_void>)> {
    let DomObject::Node(n) = dom_object(obj)? else {
        return None;
    };
    Some((n.shared_document()?, n.raw_node()?))
}

/// The PHP DOM object of `node`, a node of the shared tree `shared`
/// (`dom_import_simplexml()`, `Dom\import_simplexml()`, `XMLReader::expand()`).
///
/// A document DOM does not know yet becomes a `DOMDocument` tree (or a
/// `Dom\XMLDocument` one when `modern`); for a document DOM already has,
/// its family is kept and `modern` is ignored (check
/// [`shared_family`] first to report a family conflict).
///
/// # Safety
/// `node` must be a live node (element, attribute, …) of `shared`'s
/// document.
pub unsafe fn shared_node_object(
    vm: &mut Vm,
    shared: &SharedDocument,
    node: NonNull<c_void>,
    modern: bool,
) -> VmResult<Value> {
    // Creates (or finds) the document's reference object with the right
    // family; it must stay alive until the node holds it.
    let doc = php_dom::interop::adopt_shared_document(shared.clone(), modern, None);
    // SAFETY: forwarded to the caller.
    let n = unsafe { php_dom::interop::wrap_shared_node(shared, node.as_ptr()) };
    drop(doc);
    match n {
        Some(n) => Ok(Value::Object(crate::state::node_object(vm, &n)?)),
        None => Ok(Value::Null),
    }
}

/// The API family (`Some(true)` for `Dom\`) of the DOM objects of a shared
/// tree, when DOM already has objects for it.
pub fn shared_family(shared: &SharedDocument) -> Option<bool> {
    crate::xsl::family_of(shared)
}

/// Serializes a node of a shared tree whose DOM family is modern
/// (`Dom\XMLDocument`): PHP serializes such documents through DOM's
/// spec-compliant XML serializer even from `SimpleXMLElement::asXML()`
/// (the document's handlers). Passing the document node (`xmlDoc*`)
/// yields the whole document with its XML declaration (`saveXML()`),
/// any other node yields just that node (`saveXML($node)`).
///
/// `Ok(None)` when DOM has no modern objects for the tree (serialize with
/// libxml2 as usual) or the node cannot be serialized.
///
/// # Safety
/// `node` must be a live node (or the document node) of `shared`'s
/// document.
pub unsafe fn serialize_shared_node(
    vm: &mut Vm,
    shared: &SharedDocument,
    node: NonNull<c_void>,
) -> VmResult<Option<Vec<u8>>> {
    if crate::xsl::family_of(shared) != Some(true) {
        // Not imported as modern by this binding: modern only if DOM
        // already has a modern reference object for the document.
        let doc = php_dom::interop::adopt_shared_document(shared.clone(), false, None);
        if !doc.family_is_modern() {
            return Ok(None);
        }
    }
    let doc = php_dom::interop::adopt_shared_document(shared.clone(), true, None);
    let is_doc = node.as_ptr() == shared.as_ptr().cast::<c_void>();
    let args = if is_doc {
        Vec::new()
    } else {
        // SAFETY: forwarded to the caller.
        match unsafe { php_dom::interop::wrap_shared_node(shared, node.as_ptr()) } {
            Some(n) => vec![php_dom::DomValue::Node(n)],
            None => return Ok(None),
        }
    };
    let d = DomObject::Node(doc);
    let r = crate::run::direct(vm, || {
        php_dom::dispatch::call(&d, "saveXML", &args, &php_dom::NoHost)
    })?;
    Ok(match crate::errors::check(vm, r, None)? {
        Some(php_dom::DomValue::Str(s)) => Some(s),
        _ => None,
    })
}

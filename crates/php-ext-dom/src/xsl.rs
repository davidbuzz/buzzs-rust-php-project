// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The DOM provider of XML documents for `XSLTProcessor`
//! (`php_ext_xsl::access::DomAccess`, i.e. `php_libxml_import_node` and
//! `php_dom_create_object` for ext/xsl) and `dom_import_simplexml()`.

use std::any::Any;
use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;

use php_dom::dispatch::{self, DomObject, PropRead};
use php_dom::interop::{adopt_document, namespace_node, wrap_raw_node};
use php_dom::{DomValue, Node};
use php_ext_xsl::access::{DomAccess, ImportedNode, XmlSource};
use php_libxml::tree::SharedDocument;
use php_runtime::{Obj, Value};
use php_vm::class::Class;
use php_vm::ext::{Args, VmResult};
use php_vm::object::display_name;
use php_vm::Vm;
use php_xsl::{CallbackContext, NodeArg, ResultDocument, ReturnedNode};

use crate::state::{dom_object, node_object};

/// The provider (registered at module startup).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DomXslAccess;

/// The provider instance.
pub(crate) static ACCESS: DomXslAccess = DomXslAccess;

thread_local! {
    /// Objects returned from callbacks, kept alive until the
    /// transformation is over.
    static RETURNED: RefCell<Vec<Obj>> = const { RefCell::new(Vec::new()) };
}

fn node_of(obj: &Obj) -> Option<Node> {
    match dom_object(obj)? {
        DomObject::Node(n) => Some(n),
        _ => None,
    }
}

/// Builds the node handle for a callback argument.
fn arg_node(arg: NodeArg<'_>, ctx: &CallbackContext<'_>) -> Option<Node> {
    let keep = |n: &Node| {
        if let Some(d) = ctx.document_copy {
            n.attach_keepalive(Rc::new(d.clone()) as Rc<dyn Any>);
        }
    };
    match arg {
        NodeArg::Node(r) => {
            let p = NonNull::new(r.as_ptr().cast::<c_void>())?;
            // SAFETY: the node belongs to the transformation's main
            // document, which stays alive (input object or the copy kept
            // alive below).
            let n = unsafe { wrap_raw_node(p) };
            keep(&n);
            Some(n)
        }
        NodeArg::Copy(owned) => {
            let p = owned.into_raw().cast::<c_void>();
            // SAFETY: a detached copy we now own; the handle frees it.
            let n = unsafe { wrap_raw_node(p) };
            keep(&n);
            Some(n)
        }
        NodeArg::Namespace {
            parent,
            prefix,
            href,
        } => {
            let p = arg_node(*parent, ctx)?;
            namespace_node(&p, prefix.as_deref(), &href)
        }
    }
}

impl DomAccess for DomXslAccess {
    fn name(&self) -> &'static str {
        "dom"
    }

    fn root_classes(&self) -> &'static [&'static str] {
        &["DOMNode", "Dom\\Node"]
    }

    fn import(&self, _vm: &mut Vm, obj: &Obj) -> VmResult<Option<ImportedNode>> {
        let Some(n) = node_of(obj) else {
            return Ok(None);
        };
        let Some((doc, keep)) = n.raw_document() else {
            return Ok(Some(ImportedNode::Detached));
        };
        // SAFETY: `keep` keeps the document alive while the source exists.
        let src = unsafe { XmlSource::raw(doc.cast(), n.family_is_modern(), keep) };
        Ok(Some(if n.is_document_node() {
            ImportedNode::Document(src)
        } else {
            ImportedNode::Node(src)
        }))
    }

    fn owner_document(&self, vm: &mut Vm, obj: &Obj) -> VmResult<Option<Obj>> {
        let Some(n) = node_of(obj) else {
            return Ok(None);
        };
        let d = DomObject::Node(n);
        let r = crate::run::direct(vm, || dispatch::read_property(&d, "ownerDocument"))?;
        match r {
            Ok(PropRead::Value(DomValue::Node(doc))) => Ok(Some(node_object(vm, &doc)?)),
            _ => Ok(None),
        }
    }

    fn document_shared(&self, clone: &Obj) -> bool {
        node_of(clone).is_some_and(|n| n.document_shared())
    }

    fn wrap_document(
        &self,
        vm: &mut Vm,
        doc: ResultDocument,
        class: Option<&Rc<Class>>,
    ) -> VmResult<Value> {
        let ptr = doc.into_raw().cast::<c_void>();
        let modern = class.is_some_and(|c| c.instance_of_name(b"dom\\node"));
        let name = class.map(|c| display_name(&c.name).to_string_lossy());
        // SAFETY: the result tree is exclusively ours.
        let n = unsafe { adopt_document(ptr, modern, name.as_deref()) };
        Ok(Value::Object(node_object(vm, &n)?))
    }

    fn node_value(
        &self,
        vm: &mut Vm,
        _input: &XmlSource,
        node: NodeArg<'_>,
        ctx: &CallbackContext<'_>,
    ) -> VmResult<Option<Value>> {
        match arg_node(node, ctx) {
            Some(n) => Ok(Some(Value::Object(node_object(vm, &n)?))),
            None => Ok(None),
        }
    }

    fn returned_node(&self, _vm: &mut Vm, value: &Obj, modern: bool) -> Option<ReturnedNode> {
        let n = node_of(value)?;
        if n.family_is_modern() != modern {
            return None;
        }
        let p = n.raw_node()?;
        RETURNED.with(|r| r.borrow_mut().push(value.clone()));
        // SAFETY: the object (and so the node) is kept alive in RETURNED
        // until `transformation_finished`.
        Some(unsafe { ReturnedNode::from_raw(p.cast()) })
    }

    fn transformation_finished(&self, _vm: &mut Vm) {
        let old = RETURNED.with(|r| std::mem::take(&mut *r.borrow_mut()));
        drop(old);
    }
}

/// The family a shared document was imported with, if known
/// (`nodeobj->document->class_type`: kept on the document itself, so a
/// later document at the same address starts unlocked).
pub(crate) fn family_of(shared: &SharedDocument) -> Option<bool> {
    shared.family()
}

/// `dom_import_simplexml()` / `Dom\import_simplexml()`: the element or
/// attribute behind a `SimpleXMLElement`, sharing its tree (the document
/// is locked to the family it was first imported as).
pub(crate) fn import_simplexml(vm: &mut Vm, a: &Args) -> VmResult<Value> {
    let bad = |vm: &mut Vm, a: &Args| a.type_error(vm, 1, "is not a valid node type");
    let Value::Object(o) = a.at(0) else {
        return Err(bad(vm, a));
    };
    let modern = a.func_name().to_ascii_lowercase().starts_with("dom\\");
    if let Some(n) = node_of(&o) {
        return Ok(Value::Object(node_object(vm, &n)?));
    }
    let found = php_ext_xsl::access::import(vm, &o)?;
    let Some((_, ImportedNode::Node(XmlSource::Node(sx)))) = found else {
        return Err(bad(vm, a));
    };
    if !matches!(
        sx.node_type(),
        php_libxml::tree::NodeType::Element | php_libxml::tree::NodeType::Attribute
    ) {
        return Err(bad(vm, a));
    }
    let Some(p) = NonNull::new(sx.as_ptr().cast::<c_void>()) else {
        return Err(bad(vm, a));
    };
    let document = sx.document();
    let came_from_dom = document.is_shared();
    let shared = document.to_shared();
    // Lock the document to one node family (`document->class_type`).
    let known = shared.family();
    let previous = match known {
        Some(f) => Some(f),
        // A tree DOM handed to SimpleXML: its family is the DOM document's.
        None if came_from_dom => {
            // SAFETY: `p` is a live node of the shared document.
            let n = unsafe { php_dom::interop::wrap_shared_node(&shared, p.as_ptr()) };
            n.map(|n| n.family_is_modern())
        }
        None => None,
    };
    if let Some(prev) = previous {
        if prev != modern {
            let msg = if modern {
                "must not be already imported as a DOMNode"
            } else {
                "must not be already imported as a Dom\\Node"
            };
            return Err(a.type_error(vm, 1, msg));
        }
    }
    shared.set_family(modern);
    // SAFETY: `p` is a live node of the shared document.
    unsafe { crate::interop::shared_node_object(vm, &shared, p, modern) }
}

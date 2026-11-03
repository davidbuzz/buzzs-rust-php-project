//! `SimpleXMLElement` methods and the `simplexml_*()` functions.

use std::ptr::NonNull;
use std::rc::Rc;

use php_runtime::{RtResult, Value, ZStr};
use php_simplexml::{Sxe, SxeRef};
use php_vm::class::Class;
use php_vm::ext::{Args, VmResult};
use php_vm::Vm;

use php_ext_xsl::access::{ImportedNode, XmlSource};
use php_libxml::tree::Node;
use php_xsl::XmlDocument;

use crate::foreign;
use crate::hooks::direct;
use crate::host::{rt_throw, sxe_of, LentHost};

/// Runs a library call inside [`php_ext_xml::libxml::run`] (stream hooks,
/// entity loader, libxml2 diagnostics) with the lent VM as host.
fn lent<R>(vm: &mut Vm, a: &Args, f: impl FnOnce(&mut LentHost) -> RtResult<R>) -> VmResult<R> {
    let r = php_ext_xml::libxml::run(vm, a, || f(&mut LentHost))?;
    r.map_err(|e| rt_throw(vm, e))
}

fn this_sxe(vm: &mut Vm, a: &Args) -> VmResult<SxeRef> {
    match a.this.as_ref().and_then(sxe_of) {
        Some(s) => Ok(s),
        None => Err(vm.throw("Error", "SimpleXMLElement is not properly initialized")),
    }
}

fn opt_bytes(a: &Args, n: usize) -> Option<Vec<u8>> {
    a.opt(n).map(|v| match v {
        Value::Str(s) => s.as_bytes().to_vec(),
        _ => Vec::new(),
    })
}

fn bytes(a: &Args, n: usize) -> Vec<u8> {
    a.str(n).as_bytes().to_vec()
}

fn str_value(b: Vec<u8>) -> Value {
    Value::Str(ZStr::from(b))
}

/// `SimpleXMLElement::__construct()`.
pub fn construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    let data = bytes(a, 0);
    let options = a.int(1);
    let is_url = a.bool(2);
    let ns = bytes(a, 3);
    let is_prefix = a.bool(4);
    lent(vm, a, |_| {
        Sxe::construct(&st, &data, options, is_url, &ns, is_prefix)
    })?;
    Ok(Value::Null)
}

/// `SimpleXMLElement::xpath()`.
pub fn xpath(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    let expr = bytes(a, 0);
    lent(vm, a, |h| Sxe::xpath(&st, h, &expr))
}

/// `SimpleXMLElement::registerXPathNamespace()`.
pub fn register_xpath_namespace(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    let (prefix, ns) = (bytes(a, 0), bytes(a, 1));
    lent(vm, a, |_| Sxe::register_xpath_namespace(&st, &prefix, &ns)).map(Value::Bool)
}

/// `SimpleXMLElement::asXML()` / `saveXML()`.
pub fn as_xml(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    let filename = opt_bytes(a, 0);
    lent(vm, a, |_| Sxe::as_xml(&st, filename.as_deref()))
}

/// `SimpleXMLElement::getNamespaces()`.
pub fn get_namespaces(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    let recursive = a.bool(0);
    direct(vm, |_| Sxe::get_namespaces(&st, recursive)).map(Value::Array)
}

/// `SimpleXMLElement::getDocNamespaces()`.
pub fn get_doc_namespaces(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    let (recursive, from_root) = (a.bool(0), a.bool(1));
    direct(vm, |_| Sxe::get_doc_namespaces(&st, recursive, from_root))
}

/// `SimpleXMLElement::children()`.
pub fn children(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    let ns = opt_bytes(a, 0);
    let is_prefix = a.bool(1);
    direct(vm, |h| Sxe::children(&st, h, ns.as_deref(), is_prefix))
}

/// `SimpleXMLElement::attributes()`.
pub fn attributes(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    let ns = opt_bytes(a, 0);
    let is_prefix = a.bool(1);
    direct(vm, |h| Sxe::attributes(&st, h, ns.as_deref(), is_prefix))
}

/// `SimpleXMLElement::addChild()`.
pub fn add_child(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    let qname = bytes(a, 0);
    let value = opt_bytes(a, 1);
    let ns = opt_bytes(a, 2);
    lent(vm, a, |h| {
        Sxe::add_child(&st, h, &qname, value.as_deref(), ns.as_deref())
    })
}

/// `SimpleXMLElement::addAttribute()`.
pub fn add_attribute(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    let qname = bytes(a, 0);
    let value = bytes(a, 1);
    let ns = opt_bytes(a, 2);
    lent(vm, a, |h| {
        Sxe::add_attribute(&st, h, &qname, &value, ns.as_deref())
    })?;
    Ok(Value::Null)
}

/// `SimpleXMLElement::getName()`.
pub fn get_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    direct(vm, |_| Sxe::get_name(&st)).map(str_value)
}

/// `SimpleXMLElement::__toString()`.
pub fn to_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    direct(vm, |h| Sxe::to_string(&st, h)).map(Value::Str)
}

/// `SimpleXMLElement::__debugInfo()`.
pub fn debug_info(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    direct(vm, |h| Sxe::properties(&st, h, true)).map(Value::Array)
}

/// `SimpleXMLElement::count()`.
pub fn count(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    direct(vm, |_| Sxe::count(&st)).map(Value::Int)
}

/// `SimpleXMLElement::rewind()`.
pub fn rewind(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    direct(vm, |h| Sxe::rewind(&st, h))?;
    Ok(Value::Null)
}

/// `SimpleXMLElement::valid()`.
pub fn valid(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    Ok(Value::Bool(Sxe::valid(&st)))
}

/// `SimpleXMLElement::current()`.
pub fn current(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    direct(vm, |_| Sxe::current(&st))
}

/// `SimpleXMLElement::key()`.
pub fn key(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    direct(vm, |h| Sxe::key(&st, h)).map(str_value)
}

/// `SimpleXMLElement::next()`.
pub fn next(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    direct(vm, |h| Sxe::next(&st, h))?;
    Ok(Value::Null)
}

/// `SimpleXMLElement::hasChildren()`.
pub fn has_children(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    direct(vm, |h| Sxe::has_children(&st, h)).map(Value::Bool)
}

/// `SimpleXMLElement::getChildren()`.
pub fn get_children(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = this_sxe(vm, a)?;
    Ok(Sxe::get_children(&st))
}

/// `Z_PARAM_CLASS_OR_NULL` against `SimpleXMLElement`: argument `n`
/// (0-based) as the class to instantiate.
fn class_arg(vm: &mut Vm, a: &Args, n: usize) -> VmResult<Option<Rc<Class>>> {
    let Some(arg) = a.opt(n) else {
        return Ok(None);
    };
    let name = vm.value_to_string(&arg)?;
    let base = vm.find_class(b"SimpleXMLElement");
    let found = vm.lookup_class(&name)?;
    match (found, base) {
        (Some(cls), Some(root)) if cls.is_subclass_of(&root) => Ok(Some(cls)),
        _ => Err(a.type_error(
            vm,
            n + 1,
            &format!(
                "must be a class name derived from SimpleXMLElement, \"{}\" given",
                name.to_string_lossy()
            ),
        )),
    }
}

/// `simplexml_load_string()`.
pub fn load_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let data = bytes(a, 0);
    let class = class_arg(vm, a, 1)?.map(|c| c.name.clone());
    let options = a.int(2);
    let ns = bytes(a, 3);
    let is_prefix = a.bool(4);
    lent(vm, a, |h| {
        Sxe::load_string(h, &data, class.as_ref(), options, &ns, is_prefix)
    })
}

/// `simplexml_load_file()`.
pub fn load_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let filename = bytes(a, 0);
    let class = class_arg(vm, a, 1)?.map(|c| c.name.clone());
    let options = a.int(2);
    let ns = bytes(a, 3);
    let is_prefix = a.bool(4);
    lent(vm, a, |h| {
        Sxe::load_file(h, &filename, class.as_ref(), options, &ns, is_prefix)
    })
}

/// `simplexml_import_dom()`: the node comes from the object model's
/// `php_ext_xsl::access::DomAccess` provider (`php_libxml_import_node`)
/// and the tree is shared, not copied; anything else is "not a valid XML
/// node".
pub fn import_dom(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let class = class_arg(vm, a, 1)?.map(|c| c.name.clone());
    let node = match a.at(0) {
        Value::Object(o) => imported_node(vm, &o)?,
        _ => None,
    };
    lent(vm, a, |h| Sxe::import_dom(h, node, class.as_ref()))
}

/// The libxml node behind a DOM (or other provider's) object.
fn imported_node(vm: &mut Vm, o: &php_runtime::Obj) -> VmResult<Option<Node>> {
    let Some((_, found)) = php_ext_xsl::access::import(vm, o)? else {
        return Ok(None);
    };
    let (src, is_doc) = match found {
        ImportedNode::Document(s) => (s, true),
        ImportedNode::Node(s) => (s, false),
        ImportedNode::Detached => return Ok(None),
    };
    let doc = match src {
        XmlSource::Document(d) => return Ok(if is_doc { Some(d.as_node()) } else { None }),
        XmlSource::Node(n) => return Ok(Some(n)),
        XmlSource::Raw { doc, keepalive } => {
            let Some(ptr) = NonNull::new(doc.xml_doc().as_ptr().cast()) else {
                return Ok(None);
            };
            // SAFETY: the provider guarantees the document lives while
            // `keepalive` does; every SimpleXML object on it holds it.
            unsafe { foreign::document(ptr, &keepalive) }
        }
    };
    if is_doc {
        return Ok(Some(doc.as_node()));
    }
    // The provider exposes only the document: locate the node by its
    // `getNodePath()`.
    let path = vm.call_method_by_name(o, "getNodePath", Vec::new())?;
    Ok(match path {
        Value::Str(p) => foreign::node_by_path(&doc, p.as_bytes()),
        _ => None,
    })
}

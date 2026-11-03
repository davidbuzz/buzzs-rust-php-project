//! `SimpleXMLElement` methods and the `simplexml_*()` functions.

use std::rc::Rc;

use php_runtime::{RtResult, Value, ZStr};
use php_simplexml::{Sxe, SxeRef};
use php_vm::class::Class;
use php_vm::ext::{Args, VmResult};
use php_vm::Vm;

use php_libxml::tree::Node;

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
    let plain_path = filename.as_ref().is_none_or(|f| !f.contains(&0));
    if st.is_modern_document() && plain_path {
        if let Some(xml) = modern_xml(vm, &st)? {
            return Ok(match filename {
                None => str_value(xml),
                Some(f) => {
                    use std::os::unix::ffi::OsStrExt;
                    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(&f));
                    Value::Bool(std::fs::write(path, xml).is_ok())
                }
            });
        }
    }
    lent(vm, a, |_| Sxe::as_xml(&st, filename.as_deref()))
}

/// `asXML()` of a tree imported from the modern DOM: PHP serializes it
/// through the document's handlers, i.e. DOM's spec-compliant serializer
/// (which reconciles namespace declarations). The root element stands
/// for the whole document. `None` falls back to libxml2's serializer.
fn modern_xml(vm: &mut Vm, st: &SxeRef) -> VmResult<Option<Vec<u8>>> {
    let Some(node) = direct(vm, |_| Sxe::export_node(st))? else {
        return Ok(None);
    };
    let doc = node.document();
    let is_root = node
        .parent()
        .is_some_and(|p| p.node_type() == php_libxml::tree::NodeType::Document);
    let ptr = if is_root {
        doc.as_ptr().cast::<std::ffi::c_void>()
    } else {
        node.as_ptr().cast::<std::ffi::c_void>()
    };
    let Some(ptr) = std::ptr::NonNull::new(ptr) else {
        return Ok(None);
    };
    let shared = doc.to_shared();
    // SAFETY: `ptr` is the live node (or document node) of `shared`'s
    // document, kept alive by `node`.
    unsafe { php_ext_dom::interop::serialize_shared_node(vm, &shared, ptr) }
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
    let (node, modern) = match a.at(0) {
        Value::Object(o) => imported_node(&o).map_or((None, false), |(n, m)| (Some(n), m)),
        _ => (None, false),
    };
    let v = lent(vm, a, |h| Sxe::import_dom(h, node, class.as_ref()))?;
    if let Value::Object(o) = &v {
        if let Some(st) = sxe_of(o) {
            st.set_modern_document(modern);
        }
    }
    Ok(v)
}

/// The libxml node behind a DOM object (`php_libxml_import_node`): the
/// DOM document switches to shared ownership and the node is pinned for
/// `SimpleXML`, so both object models reference (and may edit) the same
/// tree. Returns the node and whether it belongs to the modern DOM.
fn imported_node(o: &php_runtime::Obj) -> Option<(Node, bool)> {
    let info = php_ext_dom::interop::import_node(o)?;
    let (shared, ptr) = php_ext_dom::interop::share_node(o)?;
    let node = if info.is_document {
        shared.document().as_node()
    } else {
        // SAFETY: `ptr` is the DOM object's node, a node of `shared`'s
        // document; pinning gives SimpleXML its own reference.
        unsafe { shared.pin(ptr.as_ptr().cast()) }?
    };
    Some((node, info.modern))
}

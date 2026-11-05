//! The native state of DOM objects and the conversion between VM values
//! and [`DomValue`]s.
//!
//! Every object of a DOM class carries a [`DomState`] (set by the `create`
//! hook). Node objects wrap a [`Node`] handle; php-dom guarantees one
//! handle per libxml2 node while the handle is alive, and the handle stores
//! a weak reference to its PHP object in [`Node::host_data`]. Navigating to
//! a node whose PHP object is still referenced therefore yields the same
//! object (`$doc->documentElement === $doc->documentElement`), exactly like
//! PHP's `_private` back-pointer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use php_dom::dispatch::{DomObject, IterHandle};
use php_dom::{ArrayKey as DomKey, DomValue, NamespaceInfo, Node};
use php_runtime::{ArrayKey, HashTable, Obj, PhpObject, Value, ZArray, ZStr};
use php_vm::ext::{NativeIterator, VmResult};
use php_vm::object::{class_of, internal, set_internal};
use php_vm::Vm;

/// The native payload of a DOM object.
#[derive(Debug, Default)]
pub(crate) struct DomState {
    /// The php-dom object (`None` for an object whose constructor has not
    /// run yet, e.g. a `DOMXPath`, or a class without native state).
    pub obj: Option<DomObject>,
    /// PHP objects kept alive by this object (the node objects of a fixed
    /// node list, which PHP stores in the list's `baseobj` array).
    pub keep: Vec<Value>,
}

thread_local! {
    /// Set while this module instantiates an object whose native state it
    /// supplies itself (the `create` hook then does nothing).
    static WRAPPING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Identity cache for collection objects PHP returns the same object
    /// for (`$el->children`, `$doc->implementation`).
    static IDENTITY: RefCell<HashMap<usize, Weak<dyn PhpObject>>> = RefCell::new(HashMap::new());
}

/// The php-dom object behind a PHP object, if any.
pub(crate) fn dom_object(o: &Obj) -> Option<DomObject> {
    internal::<DomState, _>(o, |s| s.obj.clone()).flatten()
}

/// Replaces the php-dom object of `o`.
pub(crate) fn set_dom_object(o: &Obj, d: DomObject) {
    if internal::<DomState, _>(o, |s| s.obj = Some(d.clone())).is_none() {
        set_internal(
            o,
            DomState {
                obj: Some(d),
                keep: Vec::new(),
            },
        );
    }
}

/// Records `o` as the PHP object of node `n`.
pub(crate) fn bind_node_object(n: &Node, o: &Obj) {
    *n.host_data() = Some(Box::new(Rc::downgrade(o.rc())));
}

/// The live PHP object of node `n`, if any.
pub(crate) fn existing_node_object(n: &Node) -> Option<Obj> {
    let slot = n.host_data();
    let w = slot.as_ref()?.downcast_ref::<Weak<dyn PhpObject>>()?;
    w.upgrade().map(Obj::from_rc)
}

/// The class to instantiate for `name`, falling back to `fallback`.
fn class_named(vm: &Vm, name: &str, fallback: &str) -> Option<Rc<php_vm::class::Class>> {
    vm.find_class(name.as_bytes())
        .or_else(|| vm.find_class(fallback.as_bytes()))
}

/// Creates (without constructor) an object of `class` with `d` as state.
fn new_dom_object(vm: &mut Vm, class: &str, fallback: &str, d: DomObject) -> VmResult<Obj> {
    let Some(cls) = class_named(vm, class, fallback) else {
        return Err(vm.throw("Error", &format!("Class \"{class}\" not found")));
    };
    WRAPPING.with(|w| w.set(true));
    let o = vm.new_object(&cls);
    WRAPPING.with(|w| w.set(false));
    let o = o?;
    set_dom_object(&o, d);
    Ok(o)
}

/// Whether the object being created gets its state from this module.
pub(crate) fn wrapping() -> bool {
    WRAPPING.with(std::cell::Cell::get)
}

/// `php_dom_create_object`: the PHP object of a node.
pub(crate) fn node_object(vm: &mut Vm, n: &Node) -> VmResult<Obj> {
    if let Some(o) = existing_node_object(n) {
        return Ok(o);
    }
    // A `DOMNameSpaceNode` holds a reference to its element's object
    // (`parent_intern`), which php-src creates first ("Make sure parent dom
    // object exists", ext/dom/xpath.c) — this fixes handle numbering.
    let parent = match n.namespace_parent() {
        Some(p) => Some(node_object(vm, &p)?),
        None => None,
    };
    let class = n.class_name();
    let o = new_dom_object(vm, &class, n.builtin_class(), DomObject::Node(n.clone()))?;
    bind_node_object(n, &o);
    if let Some(p) = parent {
        internal::<DomState, _>(&o, |s| s.keep.push(Value::Object(p)));
    }
    Ok(o)
}

/// Looks up an identity-cached object.
fn cached(id: usize) -> Option<Obj> {
    IDENTITY.with(|m| {
        m.borrow()
            .get(&id)
            .and_then(Weak::upgrade)
            .map(Obj::from_rc)
    })
}

/// Stores an identity-cached object.
fn cache(id: usize, o: &Obj) {
    IDENTITY.with(|m| {
        let mut m = m.borrow_mut();
        if m.len() > 4096 {
            m.retain(|_, w| w.strong_count() > 0);
        }
        m.insert(id, Rc::downgrade(o.rc()));
    });
}

/// A string value.
pub(crate) fn bytes(s: impl Into<Vec<u8>>) -> Value {
    Value::Str(ZStr::from(s.into()))
}

/// Converts a php-dom value to a VM value (creating objects as needed).
pub(crate) fn to_value(vm: &mut Vm, v: DomValue) -> VmResult<Value> {
    Ok(match v {
        DomValue::Null | DomValue::Callable(_) | DomValue::Other(_) => Value::Null,
        DomValue::Bool(b) => Value::Bool(b),
        DomValue::Int(i) => Value::Int(i),
        DomValue::Float(f) => Value::Float(f),
        DomValue::Str(s) => bytes(s),
        DomValue::Node(n) => Value::Object(node_object(vm, &n)?),
        DomValue::NodeList(l) => {
            let id = l.identity();
            if let Some(o) = cached(id) {
                return Ok(Value::Object(o));
            }
            // PHP materializes the node objects of a fixed list first.
            let mut keep = Vec::new();
            if let Some(nodes) = l.fixed_nodes() {
                for n in nodes {
                    keep.push(Value::Object(node_object(vm, &n)?));
                    // `if (ZEND_HAS_OOM()) goto oom_nodeset;` (ext/dom/xpath.c):
                    // the VM raises the pending memory-limit fatal after
                    // the call instead of running into the hard limit.
                    if php_memlimit::is_pending() {
                        return Ok(Value::Null);
                    }
                }
            }
            // `mapptr->baseobj` holds a reference to the base node's object.
            if let Some(b) = l.base_node().as_ref().and_then(existing_node_object) {
                keep.push(Value::Object(b));
            }
            let class = l.class_name();
            let o = new_dom_object(vm, class, class, DomObject::NodeList(l))?;
            internal::<DomState, _>(&o, |s| s.keep = keep);
            cache(id, &o);
            Value::Object(o)
        }
        DomValue::NamedNodeMap(m) => {
            let base = m.base_node().as_ref().and_then(existing_node_object);
            let class = m.class_name();
            let o = new_dom_object(vm, class, class, DomObject::NamedNodeMap(m))?;
            if let Some(b) = base {
                internal::<DomState, _>(&o, |s| s.keep.push(Value::Object(b)));
            }
            Value::Object(o)
        }
        DomValue::Implementation(i) => {
            let id = i.identity();
            if let Some(o) = cached(id) {
                return Ok(Value::Object(o));
            }
            let class = i.class_name();
            let o = new_dom_object(vm, class, class, DomObject::Implementation(i))?;
            cache(id, &o);
            Value::Object(o)
        }
        DomValue::TokenList(t) => {
            // `classList` is cached per element: key by the element handle.
            let id = t.element().object_id() | 1;
            if let Some(o) = cached(id) {
                return Ok(Value::Object(o));
            }
            let o = new_dom_object(
                vm,
                "Dom\\TokenList",
                "Dom\\TokenList",
                DomObject::TokenList(t),
            )?;
            cache(id, &o);
            Value::Object(o)
        }
        DomValue::XPath(x) => {
            let class = x.class_name();
            Value::Object(new_dom_object(vm, class, class, DomObject::XPath(x))?)
        }
        DomValue::Iterator(h) => {
            let it = DomIter {
                source: None,
                handle: Some(h),
                started: false,
                rewindable: false,
            };
            Value::Object(vm.new_internal_iterator(Box::new(it))?)
        }
        DomValue::NamespaceInfo(ni) => namespace_info(vm, ni)?,
        DomValue::Array(items) => {
            let mut t = HashTable::new();
            for (k, v) in items {
                let key = match k {
                    DomKey::Int(i) => ArrayKey::Int(i),
                    DomKey::Str(s) => ArrayKey::Str(ZStr::from(s)),
                };
                let v = to_value(vm, v)?;
                t.insert(key, v);
            }
            Value::Array(ZArray::from_table(t))
        }
        DomValue::Enum(class, case) => {
            let name = format!("{class}::{case}");
            vm.call_value(&bytes("constant"), vec![bytes(name)])?
        }
    })
}

/// Builds a `Dom\NamespaceInfo` object.
fn namespace_info(vm: &mut Vm, ni: NamespaceInfo) -> VmResult<Value> {
    let Some(cls) = vm.find_class(b"Dom\\NamespaceInfo") else {
        return Ok(Value::Null);
    };
    let o = vm.new_object(&cls)?;
    let prefix = ni.prefix.map_or(Value::Null, bytes);
    let uri = ni.namespace_uri.map_or(Value::Null, bytes);
    let el = Value::Object(node_object(vm, &ni.element)?);
    vm.set_property_raw(&o, "prefix", prefix);
    vm.set_property_raw(&o, "namespaceURI", uri);
    vm.set_property_raw(&o, "element", el);
    Ok(Value::Object(o))
}

/// Converts a VM value to a php-dom value.
pub(crate) fn to_dom(vm: &mut Vm, v: &Value) -> DomValue {
    match v {
        Value::Null => DomValue::Null,
        Value::Bool(b) => DomValue::Bool(*b),
        Value::Int(i) => DomValue::Int(*i),
        Value::Float(f) => DomValue::Float(*f),
        Value::Str(s) => DomValue::Str(s.as_bytes().to_vec()),
        Value::Reference(r) => to_dom(vm, &r.get()),
        Value::Resource(_) => DomValue::Other("resource".into()),
        Value::Array(a) => DomValue::Array(
            a.iter()
                .map(|(k, v)| {
                    let key = match k {
                        ArrayKey::Int(i) => DomKey::Int(*i),
                        ArrayKey::Str(s) => DomKey::Str(s.as_bytes().to_vec()),
                    };
                    (key, to_dom(vm, v))
                })
                .collect(),
        ),
        Value::Object(o) => {
            if let Some(d) = dom_object(o) {
                return match d {
                    DomObject::Node(n) => DomValue::Node(n),
                    DomObject::NodeList(l) => DomValue::NodeList(l),
                    DomObject::NamedNodeMap(m) => DomValue::NamedNodeMap(m),
                    DomObject::Implementation(i) => DomValue::Implementation(i),
                    DomObject::XPath(x) => DomValue::XPath(x),
                    DomObject::TokenList(t) => DomValue::TokenList(t),
                };
            }
            let cls = class_of(o).clone();
            if cls.lc_name.as_bytes() == b"closure" {
                return DomValue::Callable(Rc::new(v.clone()));
            }
            if cls.lc_name.as_bytes() == b"dom\\adjacentposition" {
                if let Ok(Value::Str(name)) = vm.get_property(o, "name") {
                    if let Some(c) = php_dom::classes::find_class("Dom\\AdjacentPosition") {
                        if let Some((case, _)) = c
                            .enum_cases
                            .iter()
                            .find(|(n, _)| n.as_bytes() == name.as_bytes())
                        {
                            return DomValue::Enum(c.name, case);
                        }
                    }
                }
            }
            DomValue::Other(php_vm::object::display_name(&cls.name).to_string_lossy())
        }
    }
}

/// A `foreach` / `getIterator()` iterator over a DOM collection.
pub(crate) struct DomIter {
    /// The collection (to restart on rewind), when known.
    pub source: Option<DomObject>,
    /// The library iterator.
    pub handle: Option<IterHandle>,
    /// Whether `rewind()` was called once already.
    pub started: bool,
    /// Whether a second `rewind()` restarts (token lists) or throws
    /// "Iterator does not support rewinding" (node collections).
    pub rewindable: bool,
}

impl DomIter {
    /// Creates the library iterator if needed.
    fn ensure(&mut self, vm: &mut Vm) -> VmResult<()> {
        if self.handle.is_none() {
            if let Some(src) = self.source.clone() {
                let h = crate::run::direct(vm, || php_dom::dispatch::iterate(&src))?;
                self.handle = crate::errors::check(vm, h, None)?;
            }
        }
        Ok(())
    }
}

impl NativeIterator for DomIter {
    fn rewind(&mut self, vm: &mut Vm) -> VmResult<()> {
        if self.started {
            if !self.rewindable {
                return Err(vm.throw("Error", "Iterator does not support rewinding"));
            }
            if self.source.is_some() {
                self.handle = None;
            }
        }
        self.started = true;
        self.ensure(vm)
    }

    fn valid(&mut self, vm: &mut Vm) -> VmResult<bool> {
        self.ensure(vm)?;
        Ok(self
            .handle
            .as_ref()
            .is_some_and(|h| h.0.borrow_mut().valid()))
    }

    fn current(&mut self, vm: &mut Vm) -> VmResult<Value> {
        self.ensure(vm)?;
        let v = match &self.handle {
            Some(h) => h.0.borrow().current(),
            None => DomValue::Null,
        };
        to_value(vm, v)
    }

    fn key(&mut self, vm: &mut Vm) -> VmResult<Value> {
        self.ensure(vm)?;
        let v = match &self.handle {
            Some(h) => h.0.borrow_mut().key(),
            None => DomValue::Null,
        };
        to_value(vm, v)
    }

    fn next(&mut self, vm: &mut Vm) -> VmResult<()> {
        self.ensure(vm)?;
        let Some(h) = self.handle.clone() else {
            return Ok(());
        };
        let r = crate::run::direct(vm, || h.0.borrow_mut().next())?;
        crate::errors::check(vm, r, None).map(|_| ())
    }
}

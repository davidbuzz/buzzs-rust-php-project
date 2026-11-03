//! Construction of internal (builtin) classes.

use super::sig::{parse_sig, parse_type};
use crate::class::{cflags, Class, ClassConst, ConstExpr, InternalKind, PropInfo, StaticProp};
use crate::classlink::build_magic;
use crate::func::{flags, Builtin, BuiltinFn, Func, Method};
use crate::vm::Vm;
use php_runtime::{Value, ZStr};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

/// A builder for internal (native) classes.
pub struct ClassBuilder {
    name: String,
    parent: Option<Rc<Class>>,
    interfaces: Vec<Rc<Class>>,
    flags: u32,
    consts: Vec<(String, Value, u32)>,
    props: Vec<(String, u32, Option<&'static str>, Option<Value>)>,
    statics: Vec<(String, u32, Value)>,
    methods: Vec<(&'static str, u32, BuiltinFn)>,
    internal: InternalKind,
    hooks: Option<Rc<crate::ext::ClassHooks>>,
}

fn abstract_stub(vm: &mut Vm, _a: &mut crate::func::Args) -> crate::vm::VmResult<php_runtime::Value> {
    Err(vm.make_throw("Error", "Cannot call abstract method"))
}

impl ClassBuilder {
    /// Starts a class.
    pub fn new(name: &str) -> Self {
        ClassBuilder {
            name: name.to_string(),
            parent: None,
            interfaces: Vec::new(),
            flags: cflags::INTERNAL,
            consts: Vec::new(),
            props: Vec::new(),
            statics: Vec::new(),
            methods: Vec::new(),
            internal: InternalKind::None,
            hooks: None,
        }
    }

    /// Sets the parent class (looked up in the VM).
    pub fn extends(mut self, vm: &Vm, parent: &str) -> Self {
        self.parent = vm.find_class(parent.as_bytes());
        self
    }

    /// Adds implemented interfaces.
    pub fn implements(mut self, vm: &Vm, names: &[&str]) -> Self {
        for n in names {
            if let Some(c) = vm.find_class(n.as_bytes()) {
                self.interfaces.push(c);
            }
        }
        self
    }

    /// Adds class flags.
    pub fn flags(mut self, f: u32) -> Self {
        self.flags |= f;
        self
    }

    /// Sets native object handlers.
    pub fn hooks(mut self, h: crate::ext::ClassHooks) -> Self {
        self.hooks = Some(Rc::new(h));
        self
    }

    /// Sets the internal kind.
    pub(crate) fn internal(mut self, k: InternalKind) -> Self {
        self.internal = k;
        self
    }

    /// Adds a constant.
    pub fn constant(mut self, name: &str, v: Value) -> Self {
        self.consts.push((name.to_string(), v, flags::PUBLIC));
        self
    }

    /// Adds a property: `ty` is a stub type or `None`; `default` `None`
    /// leaves typed properties uninitialized.
    pub fn prop(mut self, name: &str, fl: u32, ty: Option<&'static str>, default: Option<Value>) -> Self {
        self.props.push((name.to_string(), fl, ty, default));
        self
    }

    /// Adds a static property.
    pub fn static_prop(mut self, name: &str, fl: u32, v: Value) -> Self {
        self.statics.push((name.to_string(), fl | flags::STATIC, v));
        self
    }

    /// Adds a method with a stub signature.
    pub fn method(mut self, sig: &'static str, fl: u32, f: BuiltinFn) -> Self {
        self.methods.push((sig, fl, f));
        self
    }

    /// Adds an abstract (interface) method.
    pub fn abstract_method(mut self, sig: &'static str, fl: u32) -> Self {
        self.methods.push((sig, fl | flags::ABSTRACT, abstract_stub));
        self
    }

    /// Builds and registers the class.
    #[allow(clippy::too_many_lines)]
    pub fn register(self, vm: &mut Vm) -> Rc<Class> {
        let parent = self.parent.clone();
        let mut interfaces: Vec<Rc<Class>> = parent.as_ref().map(|p| p.interfaces.clone()).unwrap_or_default();
        for i in &self.interfaces {
            for sub in &i.interfaces {
                if !interfaces.iter().any(|x| Rc::ptr_eq(x, sub)) {
                    interfaces.push(sub.clone());
                }
            }
            if !interfaces.iter().any(|x| Rc::ptr_eq(x, i)) {
                interfaces.push(i.clone());
            }
        }
        let name = ZStr::from(self.name.as_str());
        let internal = if self.internal == InternalKind::None {
            parent.as_ref().map_or(InternalKind::None, |p| p.internal)
        } else {
            self.internal
        };
        let mut cflag = self.flags;
        if let Some(p) = &parent {
            cflag |= p.flags & cflags::ALLOW_DYNAMIC;
        }
        let cls = Rc::new_cyclic(|weak: &Weak<Class>| {
            // Constants.
            let mut consts: Vec<Rc<ClassConst>> = Vec::new();
            let mut const_map = HashMap::new();
            let mut push_const = |c: Rc<ClassConst>, consts: &mut Vec<Rc<ClassConst>>, map: &mut HashMap<ZStr, usize>| {
                if let Some(&i) = map.get(&c.name) {
                    consts[i] = c;
                } else {
                    map.insert(c.name.clone(), consts.len());
                    consts.push(c);
                }
            };
            if let Some(p) = &parent {
                for c in &p.consts {
                    push_const(c.clone(), &mut consts, &mut const_map);
                }
            }
            for i in &interfaces {
                for c in &i.consts {
                    if !const_map.contains_key(&c.name) {
                        push_const(c.clone(), &mut consts, &mut const_map);
                    }
                }
            }
            for (n, v, f) in &self.consts {
                push_const(
                    Rc::new(ClassConst {
                        name: ZStr::from(n.as_str()),
                        value: RefCell::new(Some(v.clone())),
                        expr: ConstExpr::Value(v.clone()),
                        flags: *f,
                        class: name.clone(),
                        ty: None,
                        is_case: false,
                        evaluating: Cell::new(false),
                        doc_comment: None,
                        attributes: Vec::new(),
                    }),
                    &mut consts,
                    &mut const_map,
                );
            }
            // Properties.
            let mut props: Vec<Rc<PropInfo>> = parent.as_ref().map(|p| p.props.clone()).unwrap_or_default();
            let mut prop_map: HashMap<ZStr, Rc<PropInfo>> = HashMap::new();
            if let Some(p) = &parent {
                for (k, v) in &p.prop_map {
                    if v.visibility() != flags::PRIVATE {
                        prop_map.insert(k.clone(), v.clone());
                    }
                }
            }
            for (n, f, ty, def) in &self.props {
                let pn = ZStr::from(n.as_str());
                let slot = match prop_map.get(&pn) {
                    Some(e) => e.slot,
                    None => u32::try_from(props.len()).unwrap_or(0),
                };
                let pi = Rc::new(PropInfo {
                    name: pn.clone(),
                    slot,
                    flags: *f,
                    ty: ty.map(parse_type),
                    class: name.clone(),
                    default: def.clone().map(ConstExpr::Value),
                    doc_comment: None,
                    attributes: Vec::new(),
                    hooks: None,
                });
                if (slot as usize) < props.len() {
                    props[slot as usize] = pi.clone();
                } else {
                    props.push(pi.clone());
                }
                prop_map.insert(pn, pi);
            }
            let mut static_props: Vec<StaticProp> = parent.as_ref().map(|p| p.static_props.clone()).unwrap_or_default();
            let mut static_map: HashMap<ZStr, usize> = parent.as_ref().map(|p| p.static_map.clone()).unwrap_or_default();
            for (n, f, v) in &self.statics {
                let sp = StaticProp {
                    name: ZStr::from(n.as_str()),
                    flags: *f,
                    ty: None,
                    class: name.clone(),
                    cell: Rc::new(RefCell::new(v.clone())),
                    default: None,
                    initialized: Rc::new(Cell::new(true)),
                    doc_comment: None,
                };
                static_map.insert(sp.name.clone(), static_props.len());
                static_props.push(sp);
            }
            // Methods.
            let mut methods: HashMap<ZStr, Rc<Method>> = HashMap::new();
            let mut order: Vec<ZStr> = Vec::new();
            for (sig, f, h) in &self.methods {
                let s = parse_sig(sig);
                let mut bfl = *f;
                if s.variadic {
                    bfl |= flags::VARIADIC;
                }
                if s.by_ref_return {
                    bfl |= flags::RETURN_REF;
                }
                if bfl & flags::VISIBILITY == 0 {
                    bfl |= flags::PUBLIC;
                }
                let b = Builtin {
                    name: ZStr::from(s.name.as_str()),
                    params: s.params,
                    defaults: s.defaults,
                    required: s.required,
                    return_type: s.ret,
                    flags: bfl,
                    handler: *h,
                };
                let lc = ZStr::from(s.name.to_ascii_lowercase());
                let proto = parent
                    .as_ref()
                    .and_then(|p| p.methods.get(&lc))
                    .map_or_else(|| weak.clone(), |m| m.proto_scope.clone());
                order.push(lc.clone());
                methods.insert(
                    lc,
                    Rc::new(Method {
                        name: ZStr::from(s.name.as_str()),
                        func: Func::Builtin(Rc::new(b)),
                        flags: bfl,
                        scope: weak.clone(),
                        proto_scope: proto,
                    }),
                );
            }
            if let Some(p) = &parent {
                for lc in &p.method_order {
                    if !methods.contains_key(lc) {
                        if let Some(m) = p.methods.get(lc) {
                            order.push(lc.clone());
                            methods.insert(lc.clone(), m.clone());
                        }
                    }
                }
            }
            for i in &interfaces {
                for lc in &i.method_order {
                    if !methods.contains_key(lc) {
                        if let Some(m) = i.methods.get(lc) {
                            order.push(lc.clone());
                            methods.insert(lc.clone(), m.clone());
                        }
                    }
                }
            }
            let magic = build_magic(&methods);
            Class {
                lc_name: ZStr::from(self.name.to_ascii_lowercase()),
                name: name.clone(),
                flags: cflag,
                parent: parent.clone(),
                interfaces: interfaces.clone(),
                traits: Vec::new(),
                consts,
                const_map,
                props,
                prop_map,
                static_props,
                static_map,
                methods,
                method_order: order,
                magic,
                defaults: RefCell::new(None),
                backing: None,
                enum_cases: RefCell::new(HashMap::new()),
                decl: None,
                internal,
                attributes: Vec::new(),
                filename: None,
                line_start: 0,
                line_end: 0,
                doc_comment: None,
                linked: Cell::new(true),
                hooks: self.hooks.clone().or_else(|| parent.as_ref().and_then(|p| p.hooks.clone())),
            }
        });
        vm.register_class(cls.clone());
        cls
    }
}

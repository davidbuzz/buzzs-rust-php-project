// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Member tables in php-src order.
//!
//! php-src keeps a class's properties, methods and constants in ordered
//! hash tables: the class's own declarations first (in declaration order),
//! then the inherited entries (in the parent's order), then entries bound
//! from traits and finally those from interfaces. The VM's linked
//! [`Class`] stores members in slot / lookup order, so reflection rebuilds
//! the php-src order here.

use crate::class::{AttributeInfo, Class, ClassConst, ConstExpr, PropHooks, PropInfo};
use crate::func::{flags, Method};
use crate::types::TypeDecl;
use crate::vm::Vm;
use php_runtime::ZStr;
use std::collections::HashSet;
use std::rc::Rc;

/// A declared property (instance or static) as reflection sees it
/// (`zend_property_info`).
#[derive(Clone, Debug)]
pub(crate) struct PropMeta {
    /// Name.
    pub name: ZStr,
    /// VM flags (`crate::func::flags`).
    pub flags: u32,
    /// Declared type.
    pub ty: Option<TypeDecl>,
    /// Declaring class name.
    pub class: ZStr,
    /// Default value expression.
    pub default: Option<ConstExpr>,
    /// Doc comment.
    pub doc: Option<ZStr>,
    /// Attributes.
    pub attributes: Vec<AttributeInfo>,
    /// Hooks.
    pub hooks: Option<Rc<PropHooks>>,
    /// Whether the property is static.
    pub is_static: bool,
    /// The instance property info.
    pub info: Option<Rc<PropInfo>>,
    /// Declared by an internal class (no compile-time flag normalization).
    pub internal: bool,
}

impl PropMeta {
    fn from_info(pi: &Rc<PropInfo>) -> Self {
        PropMeta {
            name: pi.name.clone(),
            flags: pi.flags,
            ty: pi.ty.clone(),
            class: pi.class.clone(),
            default: pi.default.clone(),
            doc: pi.doc_comment.clone(),
            attributes: pi.attributes.clone(),
            hooks: pi.hooks.clone(),
            is_static: false,
            info: Some(pi.clone()),
            internal: false,
        }
    }
}

/// The hooks of a property including those inherited from parent classes
/// (`zend_property_info.hooks` after inheritance).
fn merged_hooks(c: &Rc<Class>, pi: &PropInfo) -> Option<Rc<PropHooks>> {
    let own = pi.hooks.clone()?;
    if own.get.is_some() && own.set.is_some() {
        return Some(own);
    }
    let mut h = (*own).clone();
    for pc in std::iter::successors(c.parent.clone(), |x| x.parent.clone()) {
        if let Some(ph) = pc.prop_map.get(&pi.name).and_then(|x| x.hooks.clone()) {
            if h.get.is_none() && ph.get.is_some() {
                h.get.clone_from(&ph.get);
                h.final_get = ph.final_get;
            }
            if h.set.is_none() && ph.set.is_some() {
                h.set.clone_from(&ph.set);
                h.final_set = ph.final_set;
            }
        }
    }
    Some(Rc::new(h))
}

/// Finds the declaration of a static property (for its attributes).
fn static_decl_attrs(vm: &Vm, class: &ZStr, name: &ZStr) -> Vec<AttributeInfo> {
    let Some(c) = vm.find_class(class.as_bytes()) else {
        return Vec::new();
    };
    let Some(decl) = &c.decl else {
        return Vec::new();
    };
    if let Some(p) = decl.props.iter().find(|p| p.name == *name) {
        return p.attributes.clone();
    }
    for t in &c.traits {
        if let Some(td) = &t.decl {
            if let Some(p) = td.props.iter().find(|p| p.name == *name) {
                return p.attributes.clone();
            }
        }
    }
    Vec::new()
}

/// Looks up property `name` of `c` like `zend_hash_find(&ce->properties_info)`
/// restricted to entries visible in `c` (no inherited private shadows).
pub(crate) fn find_prop(vm: &Vm, c: &Rc<Class>, name: &[u8]) -> Option<PropMeta> {
    let key = ZStr::from(name);
    let internal = |pi: &PropInfo| {
        vm.find_class(pi.class.as_bytes())
            .is_some_and(|d| d.flags & crate::class::cflags::INTERNAL != 0 || d.is_enum())
    };
    if let Some(pi) = c.prop_map.get(&key) {
        if pi.visibility() != flags::PRIVATE || pi.class.eq_ignore_ascii_case(c.name.as_bytes()) {
            return Some(PropMeta {
                internal: internal(pi),
                hooks: merged_hooks(c, pi),
                ..PropMeta::from_info(pi)
            });
        }
    }
    if let Some(sp) = c.find_static(name) {
        if sp.flags & flags::PRIVATE == 0 || sp.class.eq_ignore_ascii_case(c.name.as_bytes()) {
            return Some(PropMeta {
                name: sp.name.clone(),
                flags: sp.flags | flags::STATIC,
                ty: sp.ty.clone(),
                class: sp.class.clone(),
                default: sp.default.clone(),
                doc: sp.doc_comment.clone(),
                attributes: static_decl_attrs(vm, &sp.class, &sp.name),
                hooks: None,
                is_static: true,
                info: None,
                internal: c.flags & crate::class::cflags::INTERNAL != 0,
            });
        }
    }
    // Own private properties not reachable through `prop_map`.
    c.props
        .iter()
        .find(|p| p.name == key && p.class.eq_ignore_ascii_case(c.name.as_bytes()))
        .map(PropMeta::from_info)
}

/// Names of `properties_info` in php-src order (inherited private
/// properties included).
fn prop_names(c: &Rc<Class>, out: &mut Vec<ZStr>, seen: &mut HashSet<ZStr>) {
    let mut push = |n: &ZStr, out: &mut Vec<ZStr>| {
        if seen.insert(n.clone()) {
            out.push(n.clone());
        }
    };
    let parent_names = |c: &Rc<Class>| {
        let mut pn = Vec::new();
        if let Some(p) = &c.parent {
            prop_names(p, &mut pn, &mut HashSet::new());
        }
        pn
    };
    // An internal class inherits before declaring its own properties
    // (`zend_register_internal_class_ex()`), so the parent's come first; a
    // user class declares its own at compile time and inherits after.
    if c.decl.is_none() {
        for n in &parent_names(c) {
            push(n, out);
        }
    }
    if let Some(decl) = &c.decl {
        if c.is_enum() {
            push(&ZStr::from("name"), out);
            if c.backing.is_some() {
                push(&ZStr::from("value"), out);
            }
        }
        for p in &decl.props {
            push(&p.name, out);
        }
    } else {
        for p in &c.props {
            if p.class.eq_ignore_ascii_case(c.name.as_bytes()) {
                push(&p.name, out);
            }
        }
        for s in &c.static_props {
            if s.class.eq_ignore_ascii_case(c.name.as_bytes()) {
                push(&s.name, out);
            }
        }
    }
    for n in &parent_names(c) {
        push(n, out);
    }
    for t in &c.traits {
        if let Some(td) = &t.decl {
            for p in &td.props {
                push(&p.name, out);
            }
        }
    }
    for p in &c.props {
        push(&p.name, out);
    }
    for s in &c.static_props {
        push(&s.name, out);
    }
}

/// The properties of `c` visible from `c` (`properties_info` without the
/// inherited private ones), in php-src order.
pub(crate) fn props(vm: &Vm, c: &Rc<Class>) -> Vec<PropMeta> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    prop_names(c, &mut names, &mut seen);
    names
        .iter()
        .filter_map(|n| find_prop(vm, c, n.as_bytes()))
        .collect()
}

/// The methods of `c` (`function_table`) in php-src order, worked out once
/// per class ([`Class::reflection_methods`]).
pub(crate) fn methods(c: &Rc<Class>) -> Rc<[Rc<Method>]> {
    Rc::clone(c.reflection_methods.get_or_init(|| method_order(c).into()))
}

/// [`methods`] worked out.
fn method_order(c: &Rc<Class>) -> Vec<Rc<Method>> {
    let mut order: Vec<ZStr> = Vec::new();
    let mut seen: HashSet<ZStr> = HashSet::new();
    let mut push = |lc: ZStr, order: &mut Vec<ZStr>| {
        if c.methods.contains_key(&lc) && seen.insert(lc.clone()) {
            order.push(lc);
        }
    };
    if let Some(decl) = &c.decl {
        for m in &decl.methods {
            push(m.name.to_ascii_lowercase(), &mut order);
        }
        if c.is_enum() {
            push(ZStr::from("cases"), &mut order);
            if c.backing.is_some() {
                push(ZStr::from("from"), &mut order);
                push(ZStr::from("tryfrom"), &mut order);
            }
        }
        if let Some(p) = &c.parent {
            for m in methods(p).iter() {
                push(m.name.to_ascii_lowercase(), &mut order);
            }
        }
    }
    for lc in &c.method_order {
        push(lc.clone(), &mut order);
    }
    order
        .iter()
        .filter_map(|lc| c.methods.get(lc).cloned())
        .collect()
}

/// The constants of `c` (`constants_table`) in php-src order.
pub(crate) fn consts(c: &Rc<Class>) -> Vec<Rc<ClassConst>> {
    let mut order: Vec<ZStr> = Vec::new();
    let mut seen: HashSet<ZStr> = HashSet::new();
    let mut push = |n: &ZStr, order: &mut Vec<ZStr>| {
        if c.const_map.contains_key(n) && seen.insert(n.clone()) {
            order.push(n.clone());
        }
    };
    if let Some(decl) = &c.decl {
        for member in decl.own_constants() {
            let n = match member {
                crate::class::OwnConstant::Const(i) => &decl.consts[i].name,
                crate::class::OwnConstant::Case(i) => &decl.cases[i].0,
            };
            push(n, &mut order);
        }
        if let Some(p) = &c.parent {
            for cc in consts(p) {
                push(&cc.name, &mut order);
            }
        }
        for t in &c.traits {
            for cc in consts(t) {
                push(&cc.name, &mut order);
            }
        }
    }
    // The table order: an internal class inherits (`zend_do_inheritance` in
    // `zend_register_internal_class_with_flags`) and implements its
    // interfaces before declaring its own constants, which is the order
    // `ClassBuilder` builds the table in.
    for cc in &c.consts {
        push(&cc.name, &mut order);
    }
    order
        .iter()
        .filter_map(|n| c.find_const(n.as_bytes()).cloned())
        .collect()
}

/// `common.prototype` of method `m` as seen in class `class`: an inherited
/// method gets the interface method as prototype when a subclass implements
/// the interface (php-src duplicates the function there).
pub(crate) fn prototype_in(m: &Rc<Method>, class: &Rc<Class>) -> Option<Rc<Method>> {
    let scope = m.scope();
    let lc = m.name.to_ascii_lowercase();
    let mut k = Some(class.clone());
    while let Some(kc) = k.take() {
        if Rc::ptr_eq(&kc, &scope) {
            break;
        }
        let inherited: Vec<Rc<Class>> = kc
            .parent
            .as_ref()
            .map(|p| p.interfaces.clone())
            .unwrap_or_default();
        for i in &kc.interfaces {
            if inherited.iter().any(|x| Rc::ptr_eq(x, i)) {
                continue;
            }
            if let Some(im) = i.methods.get(&lc) {
                if !Rc::ptr_eq(im, m) {
                    return Some(prototype(im).unwrap_or_else(|| im.clone()));
                }
            }
        }
        k.clone_from(&kc.parent);
    }
    prototype(m)
}

/// `common.prototype` of method `m`.
pub(crate) fn prototype(m: &Rc<Method>) -> Option<Rc<Method>> {
    let scope = m.scope();
    let lc = m.name.to_ascii_lowercase();
    let mut proto: Option<Rc<Method>> = None;
    if let Some(p) = &scope.parent {
        if let Some(pm) = p.methods.get(&lc) {
            if pm.visibility() != flags::PRIVATE {
                let pp = prototype(pm).unwrap_or_else(|| pm.clone());
                let is_ctor = lc.as_bytes() == b"__construct";
                if !is_ctor || pp.is_abstract() {
                    proto = Some(pp);
                }
            }
        }
    }
    let inherited: Vec<Rc<Class>> = scope
        .parent
        .as_ref()
        .map(|p| p.interfaces.clone())
        .unwrap_or_default();
    for i in &scope.interfaces {
        if Rc::ptr_eq(i, &scope) || inherited.iter().any(|x| Rc::ptr_eq(x, i)) {
            continue;
        }
        if let Some(im) = i.methods.get(&lc) {
            if Rc::ptr_eq(&im.scope(), &scope) {
                continue;
            }
            proto = Some(prototype(im).unwrap_or_else(|| im.clone()));
        }
    }
    if scope.is_interface() && proto.is_none() {
        for i in &scope.interfaces {
            if let Some(im) = i.methods.get(&lc) {
                if !Rc::ptr_eq(&im.scope(), &scope) {
                    proto = Some(prototype(im).unwrap_or_else(|| im.clone()));
                    break;
                }
            }
        }
    }
    proto.filter(|p| !Rc::ptr_eq(p, m))
}

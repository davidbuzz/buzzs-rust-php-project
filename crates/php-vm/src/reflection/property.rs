// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ReflectionProperty`.

use super::class::{has_dynamic, update_class_constants};
use super::order::{self, PropMeta};
use super::{
    class_value, rexc, set_name_props, str_arg, this, this_refl, with_scope, PropRef, Refl, PRIV,
    PUB, RO,
};
use crate::builtins::classbuilder::ClassBuilder;
use crate::class::{cflags, Class, PropInfo, StaticProp};
use crate::func::{flags, Args, Func, Method};
use crate::object;
use crate::opcode::FetchMode;
use crate::types::{mask, TypeDecl};
use crate::vm::{Vm, VmResult};
use php_runtime::{ArrayKey, Obj, Value, ZStr};
use std::rc::Rc;

/// php-src property flags (`zend_property_info.flags`) of a declaration.
pub(crate) fn php_flags(p: &PropMeta) -> i64 {
    let f = p.flags;
    let mut r = i64::from(f & 0xf7);
    if p.is_static {
        r |= super::acc::STATIC;
    }
    if f & flags::VIRTUAL != 0 {
        r |= super::acc::VIRTUAL;
    }
    let vis = f & flags::VISIBILITY;
    let set = f & flags::SET_MASK;
    let set_vis = match set {
        flags::SET_PUBLIC => flags::PUBLIC,
        flags::SET_PROTECTED => flags::PROTECTED,
        flags::SET_PRIVATE => flags::PRIVATE,
        _ => 0,
    };
    if set == 0 {
        if vis == flags::PUBLIC && f & flags::READONLY != 0 && p.internal {
            r |= super::acc::PROTECTED_SET;
        }
    } else if set_vis != vis {
        match set {
            flags::SET_PROTECTED => r |= super::acc::PROTECTED_SET,
            flags::SET_PRIVATE => r |= super::acc::PRIVATE_SET | super::acc::FINAL,
            _ => {}
        }
    }
    if let Some(h) = &p.hooks {
        if h.abstract_get || h.abstract_set {
            r |= super::acc::ABSTRACT;
        }
    }
    r
}

fn this_prop(vm: &mut Vm, a: &Args) -> VmResult<PropRef> {
    match this_refl(vm, a)? {
        Refl::Prop(p) => Ok(p),
        _ => Err(vm.make_throw(
            "Error",
            "Internal error: Failed to retrieve the reflection object",
        )),
    }
}

/// Coerces a value for a typed static property.
pub(crate) fn coerce_static(
    vm: &mut Vm,
    sp: &StaticProp,
    ty: &TypeDecl,
    v: Value,
) -> VmResult<Value> {
    let pi = PropInfo {
        name: sp.name.clone(),
        slot: 0,
        flags: sp.flags,
        ty: Some(ty.clone()),
        class: sp.class.clone(),
        default: None,
        doc_comment: None,
        attributes: Vec::new(),
        hooks: None,
    };
    vm.coerce_prop(None, &pi, v)
}

fn rp_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = this(a);
    let arg = a.at(0);
    let (c, obj) = match &arg {
        Value::Object(x) => (object::class_of(x).clone(), Some(x.clone())),
        v => {
            let s = vm.to_zstr(v)?;
            match vm.lookup_class(&s)? {
                Some(c) => (c, None),
                None => {
                    return Err(rexc(
                        vm,
                        &format!("Class \"{}\" does not exist", s.to_string_lossy()),
                    ))
                }
            }
        }
    };
    let n = str_arg(a, 1);
    let meta = order::find_prop(vm, &c, n.as_bytes());
    if meta.is_none() {
        let shadow = c.props.iter().any(|p| p.name == n) || c.prop_map.contains_key(&n);
        let dynamic = !shadow && obj.as_ref().is_some_and(|x| has_dynamic(x, &n));
        if !dynamic {
            return Err(rexc(
                vm,
                &format!(
                    "Property {}::${} does not exist",
                    c.name,
                    n.to_string_lossy()
                ),
            ));
        }
    }
    let decl = match &meta {
        Some(p) => vm
            .find_class(p.class.as_bytes())
            .map_or_else(|| p.class.clone(), |x| x.name.clone()),
        None => c.name.clone(),
    };
    set_name_props(&o, Some(&n), Some(&decl));
    object::set_internal(
        &o,
        Refl::Prop(PropRef {
            class: c,
            name: n,
            meta: meta.map(Rc::new),
        }),
    );
    Ok(Value::Null)
}

fn rp_to_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    let s = super::export::property_string(vm, p.meta.as_deref(), &p.name, &p.class, "");
    Ok(Value::Str(ZStr::from(s)))
}

fn rp_get_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Str(this_prop(vm, a)?.name))
}

fn rp_get_mangled_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    let Some(m) = &p.meta else {
        return Ok(Value::Str(p.name));
    };
    let mut v = Vec::new();
    match m.flags & flags::VISIBILITY {
        flags::PRIVATE => {
            v.push(0);
            let decl = vm
                .find_class(m.class.as_bytes())
                .map_or_else(|| m.class.clone(), |c| c.name.clone());
            v.extend_from_slice(decl.as_bytes());
            v.push(0);
        }
        flags::PROTECTED => v.extend_from_slice(b"\0*\0"),
        _ => {}
    }
    v.extend_from_slice(p.name.as_bytes());
    Ok(Value::Str(ZStr::from(v)))
}

fn flags_of(p: &PropRef) -> i64 {
    p.meta.as_ref().map_or(super::acc::PUBLIC, |m| php_flags(m))
}

macro_rules! rp_flag {
    ($name:ident, $mask:expr) => {
        fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let p = this_prop(vm, a)?;
            Ok(Value::Bool(flags_of(&p) & $mask != 0))
        }
    };
}

rp_flag!(rp_is_public, super::acc::PUBLIC);
rp_flag!(rp_is_private, super::acc::PRIVATE);
rp_flag!(rp_is_protected, super::acc::PROTECTED);
rp_flag!(rp_is_private_set, super::acc::PRIVATE_SET);
rp_flag!(rp_is_protected_set, super::acc::PROTECTED_SET);
rp_flag!(rp_is_static, super::acc::STATIC);
rp_flag!(rp_is_readonly, super::acc::READONLY);
rp_flag!(rp_is_abstract, super::acc::ABSTRACT);
rp_flag!(rp_is_virtual, super::acc::VIRTUAL);
rp_flag!(rp_is_final, super::acc::FINAL);

fn rp_get_modifiers(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    Ok(Value::Int(
        flags_of(&p) & (0x7 | 0x1800 | 0x10 | 0x80 | 0x40 | 0x200 | 0x20),
    ))
}

fn rp_is_default(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_prop(vm, a)?.meta.is_some()))
}

fn rp_is_dynamic(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_prop(vm, a)?.meta.is_none()))
}

/// Whether property `name` of class `decl` is a promoted constructor
/// parameter.
fn is_promoted(vm: &Vm, m: &PropMeta) -> bool {
    let Some(decl) = vm.find_class(m.class.as_bytes()) else {
        return false;
    };
    let ctor = decl.methods.get(b"__construct".as_slice()).cloned();
    ctor.is_some_and(|c| {
        Rc::ptr_eq(&c.scope(), &decl)
            && match &c.func {
                Func::User(u) => u.params.iter().any(|p| p.name == m.name && p.promote != 0),
                Func::Builtin(_) => false,
            }
    })
}

fn rp_is_promoted(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    Ok(Value::Bool(
        p.meta
            .as_ref()
            .is_some_and(|m| !m.is_static && is_promoted(vm, m)),
    ))
}

fn object_arg(a: &Args, n: usize) -> Option<Obj> {
    match a.get(n) {
        Some(Value::Object(o)) => Some(o),
        _ => None,
    }
}

fn decl_class(vm: &Vm, p: &PropRef) -> Rc<Class> {
    p.meta
        .as_ref()
        .and_then(|m| vm.find_class(m.class.as_bytes()))
        .unwrap_or_else(|| p.class.clone())
}

fn static_cell(vm: &mut Vm, p: &PropRef) -> VmResult<Rc<std::cell::RefCell<Value>>> {
    let c = p.class.clone();
    update_class_constants(vm, &c)?;
    with_scope(vm, Some(c.clone()), |vm| vm.static_prop_cell(&c, &p.name))
}

fn rp_get_value(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    if p.meta.as_ref().is_some_and(|m| m.is_static) {
        let cell = static_cell(vm, &p)?;
        let v = cell.borrow().deref();
        if super::class::static_uninit(&p.class, p.name.as_bytes(), &v) {
            let dc = decl_class(vm, &p);
            return Err(vm.make_throw(
                "Error",
                &format!(
                    "Typed static property {}::${} must not be accessed before initialization",
                    super::dn(&dc.name),
                    p.name
                ),
            ));
        }
        return Ok(v);
    }
    let Some(o) = object_arg(a, 0) else {
        return Err(vm.make_throw(
            "TypeError",
            "ReflectionProperty::getValue(): Argument #1 ($object) must be provided for instance properties",
        ));
    };
    let dc = decl_class(vm, &p);
    if !object::class_of(&o).is_subclass_of(&dc) {
        return Err(rexc(
            vm,
            "Given object is not an instance of the class this property was declared in",
        ));
    }
    let c = p.class.clone();
    with_scope(vm, Some(c), |vm| {
        vm.read_property(&o, &p.name, FetchMode::Read)
    })
}

fn rp_set_value(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    if p.meta.as_ref().is_some_and(|m| m.is_static) {
        let v = if a.has(1) {
            let first = a.at(0);
            if !first.is_null() && !matches!(first, Value::Object(_)) {
                vm.deprecated("Calling ReflectionProperty::setValue() with a 1st argument which is not null or an object is deprecated")?;
            }
            a.at(1)
        } else {
            vm.deprecated(
                "Calling ReflectionProperty::setValue() with a single argument is deprecated",
            )?;
            a.at(0)
        };
        let cell = static_cell(vm, &p)?;
        let c = p.class.clone();
        let sp = c.find_static(p.name.as_bytes()).cloned();
        let v = match (&sp, sp.as_ref().and_then(|s| s.ty.clone())) {
            (Some(sp), Some(ty)) => coerce_static(vm, sp, &ty, v)?,
            _ => v,
        };
        let cur = cell.borrow().clone();
        match cur {
            // `zend_update_static_property_ex()` assigns through a typed
            // reference with its type checks (non-strict).
            Value::Reference(r) => {
                vm.assign_ref_ex(&r, v, Some(false))?;
            }
            _ => *cell.borrow_mut() = v,
        }
        return Ok(Value::Null);
    }
    let Value::Object(o) = a.at(0) else {
        return Err(vm.make_throw(
            "TypeError",
            &format!(
                "ReflectionProperty::setValue(): Argument #1 ($objectOrValue) must be of type object, {} given",
                crate::typecheck::given_type(&a.at(0))
            ),
        ));
    };
    if !a.has(1) {
        return Err(vm.make_throw(
            "ArgumentCountError",
            "ReflectionProperty::setValue() expects exactly 2 arguments, 1 given",
        ));
    }
    if !object::class_of(&o).is_subclass_of(&p.class) {
        vm.deprecated("Calling ReflectionProperty::setValue() with a given object that is not an instance of the class this property was declared in is deprecated")?;
    }
    let v = a.at(1);
    let c = p.class.clone();
    with_scope(vm, Some(c), |vm| vm.write_property(&o, &p.name, v))?;
    Ok(Value::Null)
}

/// `reflection_property_get_effective_prop`.
fn effective(vm: &Vm, p: &PropRef, o: &Obj) -> Option<PropMeta> {
    let oc = object::class_of(o).clone();
    match &p.meta {
        Some(m) if m.flags & flags::PRIVATE != 0 => Some((**m).clone()),
        _ if Rc::ptr_eq(&oc, &p.class) => p.meta.as_deref().cloned(),
        _ => order::find_prop(vm, &oc, p.name.as_bytes()),
    }
}

fn rp_get_raw_value(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    let Some(o) = object_arg(a, 0) else {
        return Ok(Value::Null);
    };
    if !object::class_of(&o).is_subclass_of(&p.class) {
        return Err(rexc(
            vm,
            "Given object is not an instance of the class this property was declared in",
        ));
    }
    let m = effective(vm, &p, &o);
    if m.as_ref().is_some_and(|m| m.is_static) {
        return Err(rexc(vm, "May not use getRawValue on static properties"));
    }
    match m
        .as_ref()
        .filter(|m| m.hooks.as_ref().is_some_and(|h| h.get.is_some()))
    {
        Some(m) => {
            if m.flags & flags::VIRTUAL != 0 {
                return Err(vm.make_throw(
                    "Error",
                    &format!(
                        "Must not read from virtual property {}::${}",
                        super::dn(&object::class_of(&o).name),
                        p.name
                    ),
                ));
            }
            let Some(pi) = &m.info else {
                return Ok(Value::Null);
            };
            let v = object::data(&o)
                .slots
                .borrow()
                .get(pi.slot as usize)
                .cloned()
                .flatten();
            match v {
                Some(v) => Ok(v.deref()),
                None => Err(vm.make_throw(
                    "Error",
                    &format!(
                        "Typed property {}::${} must not be accessed before initialization",
                        super::dn(&pi.class),
                        pi.name
                    ),
                )),
            }
        }
        None => {
            let c = p.class.clone();
            with_scope(vm, Some(c), |vm| {
                vm.read_property(&o, &p.name, FetchMode::Read)
            })
        }
    }
}

/// `zend_reflection_property_set_raw_value`.
pub(crate) fn set_raw(vm: &mut Vm, p: &PropRef, o: &Obj, v: Value) -> VmResult<()> {
    let m = effective(vm, p, o);
    if m.as_ref().is_some_and(|m| m.is_static) {
        return Err(rexc(vm, "May not use setRawValue on static properties"));
    }
    match m
        .as_ref()
        .filter(|m| m.hooks.as_ref().is_some_and(|h| h.set.is_some()))
    {
        Some(m) => {
            if m.flags & flags::VIRTUAL != 0 {
                return Err(vm.make_throw(
                    "Error",
                    &format!(
                        "Must not write to virtual property {}::${}",
                        super::dn(&object::class_of(o).name),
                        p.name
                    ),
                ));
            }
            let Some(pi) = m.info.clone() else {
                return Ok(());
            };
            let v = vm.coerce_prop(Some(o), &pi, v)?;
            let d = object::data(o);
            let mut slots = d.slots.borrow_mut();
            if let Some(slot) = slots.get_mut(pi.slot as usize) {
                match slot {
                    Some(Value::Reference(r)) => {
                        r.set(v);
                    }
                    s => *s = Some(v),
                }
            }
            Ok(())
        }
        None => {
            let c = p.class.clone();
            with_scope(vm, Some(c), |vm| vm.write_property(o, &p.name, v)).map(|_| ())
        }
    }
}

fn rp_set_raw_value(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    let Some(o) = object_arg(a, 0) else {
        return Ok(Value::Null);
    };
    if !object::class_of(&o).is_subclass_of(&p.class) {
        vm.deprecated("Calling ReflectionProperty::setRawValue() with a given object that is not an instance of the class this property was declared in is deprecated")?;
    }
    set_raw(vm, &p, &o, a.at(1))?;
    Ok(Value::Null)
}

fn rp_is_initialized(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    if p.meta.as_ref().is_some_and(|m| m.is_static) {
        let cell = static_cell(vm, &p)?;
        let v = cell.borrow().deref();
        return Ok(Value::Bool(!super::class::static_uninit(
            &p.class,
            p.name.as_bytes(),
            &v,
        )));
    }
    let Some(o) = object_arg(a, 0) else {
        return Err(vm.make_throw(
            "TypeError",
            "ReflectionProperty::isInitialized(): Argument #1 ($object) must be provided for instance properties",
        ));
    };
    let dc = decl_class(vm, &p);
    if !object::class_of(&o).is_subclass_of(&dc) {
        return Err(rexc(
            vm,
            "Given object is not an instance of the class this property was declared in",
        ));
    }
    let target = vm.lazy_object_for(&o, Some(&p.name))?;
    let m = effective(vm, &p, &target);
    Ok(Value::Bool(match m.as_ref().and_then(|m| m.info.clone()) {
        Some(pi) => {
            if pi.is_virtual() {
                true
            } else {
                object::data(&target)
                    .slots
                    .borrow()
                    .get(pi.slot as usize)
                    .is_some_and(Option::is_some)
            }
        }
        None => has_dynamic(&target, &p.name),
    }))
}

fn rp_get_declaring_class(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    let c = decl_class(vm, &p);
    class_value(vm, &c)
}

fn rp_get_doc_comment(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    Ok(p.meta
        .and_then(|m| m.doc.clone())
        .map_or(Value::Bool(false), Value::Str))
}

fn rp_get_attributes(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    let Some(m) = &p.meta else {
        return Ok(Value::Array(php_runtime::ZArray::new()));
    };
    let dc = decl_class(vm, &p);
    super::attribute::attributes_array_in(
        vm,
        &m.attributes,
        super::target::PROPERTY,
        Some(&dc),
        dc.filename.clone(),
        a,
    )
}

fn rp_set_accessible(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    vm.deprecated("Method ReflectionProperty::setAccessible() is deprecated since 8.5, as it has no effect since PHP 8.1")?;
    Ok(Value::Null)
}

fn rp_get_type(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    let dc = decl_class(vm, &p);
    match p.meta.as_ref().and_then(|m| m.ty.clone()) {
        Some(t) => super::types::type_value(vm, &t, Some(&dc), true),
        None => Ok(Value::Null),
    }
}

fn rp_get_settable_type(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    let Some(m) = &p.meta else {
        return Ok(Value::Null);
    };
    let dc = decl_class(vm, &p);
    if let Some(h) = &m.hooks {
        if m.flags & flags::VIRTUAL != 0 && h.set.is_none() {
            return super::types::type_value(vm, &TypeDecl::simple(mask::NEVER), Some(&dc), true);
        }
        if let Some(s) = &h.set {
            return match s.params.first().and_then(|x| x.ty.clone()) {
                Some(t) => super::types::type_value(vm, &t, Some(&dc), true),
                None => Ok(Value::Null),
            };
        }
    }
    match &m.ty {
        Some(t) => super::types::type_value(vm, t, Some(&dc), true),
        None => Ok(Value::Null),
    }
}

fn rp_has_type(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    Ok(Value::Bool(p.meta.as_ref().is_some_and(|m| m.ty.is_some())))
}

/// The default value, `None` when the property has none (`UNDEF`).
fn default_value(vm: &mut Vm, p: &PropRef) -> VmResult<Option<Value>> {
    let Some(m) = &p.meta else { return Ok(None) };
    if m.flags & flags::VIRTUAL != 0 || (m.hooks.is_some() && is_promoted(vm, m)) {
        return Ok(None);
    }
    let dc = decl_class(vm, p);
    if m.is_static {
        return match &m.default {
            Some(e) => Ok(Some(vm.eval_const_expr(e, Some(&dc))?)),
            None if m.ty.is_none() => Ok(Some(Value::Null)),
            None => Ok(None),
        };
    }
    let Some(e) = &m.default else { return Ok(None) };
    Ok(Some(vm.eval_const_expr(e, Some(&dc))?))
}

fn rp_has_default(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    let Some(m) = &p.meta else {
        return Ok(Value::Bool(false));
    };
    if m.flags & flags::VIRTUAL != 0 || (m.hooks.is_some() && is_promoted(vm, m)) {
        return Ok(Value::Bool(false));
    }
    Ok(Value::Bool(
        m.default.is_some() || (m.is_static && m.ty.is_none()),
    ))
}

fn rp_get_default(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    match default_value(vm, &p)? {
        Some(v) => Ok(v),
        None => {
            vm.deprecated("ReflectionProperty::getDefaultValue() for a property without a default value is deprecated, use ReflectionProperty::hasDefaultValue() to check if the default value exists")?;
            Ok(Value::Null)
        }
    }
}

/// A hook function as a method of its declaring class.
fn hook_method(
    vm: &Vm,
    m: &PropMeta,
    f: &Rc<crate::func::UserFunction>,
    is_final: bool,
) -> Option<(Rc<Method>, Rc<Class>)> {
    let cn = f
        .class_name
        .clone()
        .or_else(|| m.hooks.as_ref().map(|h| h.class.clone()))
        .unwrap_or_else(|| m.class.clone());
    let c = vm.find_class(cn.as_bytes())?;
    let mut fl = flags::PUBLIC;
    if is_final {
        fl |= flags::FINAL;
    }
    let meth = Rc::new(Method {
        name: f.name.clone(),
        func: Func::User(f.clone()),
        flags: fl | (f.flags & flags::ABSTRACT),
        scope: Rc::downgrade(&c),
        proto_scope: Rc::downgrade(&c),
    });
    Some((meth, c))
}

fn hook_kind(vm: &mut Vm, v: &Value) -> VmResult<bool> {
    match v {
        Value::Object(o) => Ok(vm.enum_case_name(o).is_some_and(|n| n.as_bytes() == b"Get")),
        _ => Ok(true),
    }
}

fn rp_has_hooks(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    Ok(Value::Bool(p.meta.as_ref().is_some_and(|m| {
        m.hooks
            .as_ref()
            .is_some_and(|h| h.get.is_some() || h.set.is_some())
    })))
}

fn rp_get_hooks(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    let mut t = php_runtime::HashTable::new();
    if let Some(m) = &p.meta {
        if let Some(h) = m.hooks.clone() {
            if let Some(g) = &h.get {
                if let Some((meth, c)) = hook_method(vm, m, g, h.final_get) {
                    let v = super::method_value(vm, &meth, &c)?;
                    t.insert(ArrayKey::from("get"), v);
                }
            }
            if let Some(s) = &h.set {
                if let Some((meth, c)) = hook_method(vm, m, s, h.final_set) {
                    let v = super::method_value(vm, &meth, &c)?;
                    t.insert(ArrayKey::from("set"), v);
                }
            }
        }
    }
    Ok(Value::Array(php_runtime::ZArray::from_table(t)))
}

fn rp_has_hook(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    let get = hook_kind(vm, &a.at(0))?;
    Ok(Value::Bool(
        p.meta
            .as_ref()
            .and_then(|m| m.hooks.clone())
            .is_some_and(|h| {
                if get {
                    h.get.is_some()
                } else {
                    h.set.is_some()
                }
            }),
    ))
}

fn rp_get_hook(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    let get = hook_kind(vm, &a.at(0))?;
    let Some(m) = &p.meta else {
        return Ok(Value::Null);
    };
    let Some(h) = m.hooks.clone() else {
        return Ok(Value::Null);
    };
    let (f, fin) = if get {
        (h.get.clone(), h.final_get)
    } else {
        (h.set.clone(), h.final_set)
    };
    match f.and_then(|f| hook_method(vm, m, &f, fin)) {
        Some((meth, c)) => super::method_value(vm, &meth, &c),
        None => Ok(Value::Null),
    }
}

fn scope_arg(vm: &mut Vm, a: &Args) -> VmResult<Option<Rc<Class>>> {
    match a.at(0) {
        Value::Null => Ok(None),
        v => {
            let s = vm.to_zstr(&v)?;
            match vm.lookup_class(&s)? {
                Some(c) => Ok(Some(c)),
                None => Err(vm.make_throw(
                    "Error",
                    &format!("Class \"{}\" not found", s.to_string_lossy()),
                )),
            }
        }
    }
}

fn check_visibility(vis: u32, ce: &Rc<Class>, scope: Option<&Rc<Class>>) -> bool {
    if vis & flags::PUBLIC != 0 {
        return true;
    }
    let Some(s) = scope else { return false };
    if Rc::ptr_eq(s, ce) {
        return true;
    }
    if vis & flags::PRIVATE != 0 {
        return false;
    }
    s.is_subclass_of(ce) || ce.is_subclass_of(s)
}

fn rw_prelude(vm: &mut Vm, a: &Args, p: &PropRef) -> VmResult<(Option<Obj>, Option<PropMeta>)> {
    let obj = object_arg(a, 1);
    let mut prop = p.meta.as_deref().cloned();
    if let (Some(m), Some(o)) = (&p.meta, &obj) {
        if m.is_static {
            return Err(rexc(
                vm,
                "null is expected as object argument for static properties",
            ));
        }
        let dc = decl_class(vm, p);
        if !object::class_of(o).is_subclass_of(&dc) {
            return Err(rexc(
                vm,
                "Given object is not an instance of the class this property was declared in",
            ));
        }
        prop = effective(vm, p, o);
    }
    Ok((obj, prop))
}

fn magic_get_readable(
    vm: &mut Vm,
    ce: &Rc<Class>,
    obj: Option<&Obj>,
    name: &ZStr,
) -> VmResult<bool> {
    if ce.magic.get.is_some() {
        if let (Some(o), Some(isset)) = (obj, ce.magic.isset.clone()) {
            let r = vm.call_method_n(o, &isset, [Value::Str(name.clone())])?;
            return vm.to_bool(&r);
        }
        return Ok(true);
    }
    if let Some(o) = obj {
        if object::data(o)
            .lazy
            .borrow()
            .as_ref()
            .is_some_and(|l| !l.initialized)
        {
            let t = vm.lazy_object_for(o, None)?;
            super::lazy::force_init(vm, o)?;
            return Ok(has_dynamic(&t, name) || has_dynamic(o, name));
        }
    }
    Ok(false)
}

fn rp_is_readable(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    let (obj, prop) = rw_prelude(vm, a, &p)?;
    let ce = obj
        .as_ref()
        .map_or_else(|| p.class.clone(), |o| object::class_of(o).clone());
    let Some(prop) = prop else {
        if let Some(o) = &obj {
            if has_dynamic(o, &p.name) {
                return Ok(Value::Bool(true));
            }
        }
        return Ok(Value::Bool(magic_get_readable(
            vm,
            &ce,
            obj.as_ref(),
            &p.name,
        )?));
    };
    let scope = scope_arg(vm, a)?;
    let pce = vm
        .find_class(prop.class.as_bytes())
        .unwrap_or_else(|| ce.clone());
    if !check_visibility(prop.flags & flags::VISIBILITY, &pce, scope.as_ref()) {
        if !prop.is_static {
            return Ok(Value::Bool(magic_get_readable(
                vm,
                &ce,
                obj.as_ref(),
                &p.name,
            )?));
        }
        return Ok(Value::Bool(false));
    }
    let has_get = prop.hooks.as_ref().is_some_and(|h| h.get.is_some());
    // A virtual property is readable unless it has hooks but no `get`
    // (an internal one, such as `DOMNode::$nodeName`, has none: GH-25266).
    if prop.flags & flags::VIRTUAL != 0 {
        return Ok(Value::Bool(prop.hooks.is_none() || has_get));
    }
    if prop.is_static {
        let cell = static_cell(vm, &p)?;
        let v = cell.borrow().deref();
        return Ok(Value::Bool(!super::class::static_uninit(
            &p.class,
            p.name.as_bytes(),
            &v,
        )));
    }
    if let (Some(o), false) = (&obj, has_get) {
        if let Some(pi) = &prop.info {
            let mut o = o.clone();
            if object::data(&o)
                .slots
                .borrow()
                .get(pi.slot as usize)
                .is_some_and(Option::is_none)
            {
                if object::data(&o)
                    .lazy
                    .borrow()
                    .as_ref()
                    .is_some_and(|l| !l.initialized)
                {
                    o = super::lazy::force_init(vm, &o)?;
                    if object::data(&o)
                        .slots
                        .borrow()
                        .get(pi.slot as usize)
                        .is_some_and(Option::is_some)
                    {
                        return Ok(Value::Bool(true));
                    }
                }
                if pi.ty.is_none() || object::data(&o).slot_unset(pi.slot as usize) {
                    return Ok(Value::Bool(magic_get_readable(vm, &ce, Some(&o), &p.name)?));
                }
                return Ok(Value::Bool(false));
            }
        }
    }
    Ok(Value::Bool(true))
}

fn rp_is_writable(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    let (obj, prop) = rw_prelude(vm, a, &p)?;
    let ce = obj
        .as_ref()
        .map_or_else(|| p.class.clone(), |o| object::class_of(o).clone());
    let Some(prop) = prop else {
        if ce.flags & cflags::READONLY == 0 && !ce.is_enum() {
            return Ok(Value::Bool(true));
        }
        return Ok(Value::Bool(ce.magic.set.is_some()));
    };
    let scope = scope_arg(vm, a)?;
    let pce = vm
        .find_class(prop.class.as_bytes())
        .unwrap_or_else(|| ce.clone());
    if !check_visibility(prop.flags & flags::VISIBILITY, &pce, scope.as_ref()) {
        if !prop.is_static {
            return Ok(Value::Bool(ce.magic.set.is_some()));
        }
        return Ok(Value::Bool(false));
    }
    let pf = php_flags(&prop);
    let set_vis = if pf & super::acc::PRIVATE_SET != 0 {
        flags::PRIVATE
    } else if pf & super::acc::PROTECTED_SET != 0 {
        flags::PROTECTED
    } else {
        prop.flags & flags::VISIBILITY
    };
    if !check_visibility(set_vis, &pce, scope.as_ref()) {
        return Ok(Value::Bool(false));
    }
    if prop.flags & flags::VIRTUAL != 0 && prop.hooks.is_some() {
        return Ok(Value::Bool(
            prop.hooks.as_ref().is_some_and(|h| h.set.is_some()),
        ));
    }
    if prop.flags & flags::VIRTUAL != 0 {
        return Ok(Value::Bool(prop.flags & flags::READONLY == 0));
    }
    if prop.flags & flags::READONLY != 0 {
        if let (Some(o), Some(pi)) = (&obj, &prop.info) {
            let mut o = o.clone();
            if object::data(&o)
                .slots
                .borrow()
                .get(pi.slot as usize)
                .is_some_and(Option::is_none)
                && object::data(&o)
                    .lazy
                    .borrow()
                    .as_ref()
                    .is_some_and(|l| !l.initialized)
            {
                o = super::lazy::force_init(vm, &o)?;
            }
            if object::data(&o)
                .slots
                .borrow()
                .get(pi.slot as usize)
                .is_some_and(Option::is_some)
                // An initialized readonly property is writable again, once,
                // while the object is being cloned (`IS_PROP_REINITABLE`).
                && vm.readonly_locked(&o, pi)
            {
                return Ok(Value::Bool(false));
            }
        }
    }
    Ok(Value::Bool(true))
}

fn clone_error(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(rexc(vm, "Cannot clone object using __clone()"))
}

/// Registers `ReflectionProperty`.
pub(crate) fn register(vm: &mut Vm) {
    use super::lazy as lz;
    ClassBuilder::new("ReflectionProperty")
        .implements(vm, &["Reflector"])
        .hooks(super::constant::hooks())
        .flags(cflags::NOT_SERIALIZABLE | cflags::NO_CLONE)
        .prop("name", RO, Some("string"), None)
        .prop("class", RO, Some("string"), None)
        .constant("IS_STATIC", Value::Int(16))
        .constant("IS_READONLY", Value::Int(128))
        .constant("IS_PUBLIC", Value::Int(1))
        .constant("IS_PROTECTED", Value::Int(2))
        .constant("IS_PRIVATE", Value::Int(4))
        .constant("IS_ABSTRACT", Value::Int(64))
        .constant("IS_PROTECTED_SET", Value::Int(2048))
        .constant("IS_PRIVATE_SET", Value::Int(4096))
        .constant("IS_VIRTUAL", Value::Int(512))
        .constant("IS_FINAL", Value::Int(32))
        .method("__clone(): void", PRIV, clone_error)
        .method(
            "__construct(object|string $class, string $property)",
            PUB,
            rp_construct,
        )
        .method("__toString(): string", PUB, rp_to_string)
        .method("getName(): string", PUB | flags::PURE, rp_get_name)
        .method("getMangledName(): string", PUB, rp_get_mangled_name)
        .method("getValue(?object $object = null): mixed", PUB, rp_get_value)
        .method(
            "setValue(mixed $objectOrValue, mixed $value = UNKNOWN): void",
            PUB,
            rp_set_value,
        )
        .method("getRawValue(object $object): mixed", PUB, rp_get_raw_value)
        .method(
            "setRawValue(object $object, mixed $value): void",
            PUB,
            rp_set_raw_value,
        )
        .method(
            "setRawValueWithoutLazyInitialization(object $object, mixed $value): void",
            PUB,
            lz::rp_set_raw_value_without_lazy_initialization,
        )
        .method(
            "skipLazyInitialization(object $object): void",
            PUB,
            lz::rp_skip_lazy_initialization,
        )
        .method("isLazy(object $object): bool", PUB, lz::rp_is_lazy)
        .method(
            "isInitialized(?object $object = null): bool",
            PUB,
            rp_is_initialized,
        )
        .method("isPublic(): bool", PUB, rp_is_public)
        .method("isPrivate(): bool", PUB, rp_is_private)
        .method("isProtected(): bool", PUB, rp_is_protected)
        .method("isPrivateSet(): bool", PUB, rp_is_private_set)
        .method("isProtectedSet(): bool", PUB, rp_is_protected_set)
        .method("isStatic(): bool", PUB | flags::PURE, rp_is_static)
        .method("isReadOnly(): bool", PUB, rp_is_readonly)
        .method("isDefault(): bool", PUB, rp_is_default)
        .method("isDynamic(): bool", PUB, rp_is_dynamic)
        .method("isAbstract(): bool", PUB, rp_is_abstract)
        .method("isVirtual(): bool", PUB, rp_is_virtual)
        .method("isPromoted(): bool", PUB, rp_is_promoted)
        .method("getModifiers(): int", PUB, rp_get_modifiers)
        .method(
            "getDeclaringClass(): ReflectionClass",
            PUB | flags::PURE,
            rp_get_declaring_class,
        )
        .method(
            "getDocComment(): string|false",
            PUB | flags::PURE,
            rp_get_doc_comment,
        )
        .method(
            "setAccessible(bool $accessible): void",
            PUB,
            rp_set_accessible,
        )
        .method("getType(): ?ReflectionType", PUB, rp_get_type)
        .method(
            "getSettableType(): ?ReflectionType",
            PUB,
            rp_get_settable_type,
        )
        .method("hasType(): bool", PUB, rp_has_type)
        .method("hasDefaultValue(): bool", PUB, rp_has_default)
        .method("getDefaultValue(): mixed", PUB, rp_get_default)
        .method(
            "getAttributes(?string $name = null, int $flags = 0): array",
            PUB,
            rp_get_attributes,
        )
        .method("hasHooks(): bool", PUB, rp_has_hooks)
        .method("getHooks(): array", PUB, rp_get_hooks)
        .method("hasHook(PropertyHookType $type): bool", PUB, rp_has_hook)
        .method(
            "getHook(PropertyHookType $type): ?ReflectionMethod",
            PUB,
            rp_get_hook,
        )
        .method("isFinal(): bool", PUB, rp_is_final)
        .method(
            "isReadable(?string $scope, ?object $object = null): bool",
            PUB,
            rp_is_readable,
        )
        .method(
            "isWritable(?string $scope, ?object $object = null): bool",
            PUB,
            rp_is_writable,
        )
        .register(vm);
}

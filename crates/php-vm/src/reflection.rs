//! The Reflection API (`ext/reflection/php_reflection.c`), core subset.

use crate::builtins::classbuilder::ClassBuilder;
use crate::class::{cflags, AttributeInfo, Class, ClassConst, PropInfo};
use crate::func::{flags, Args, Func, Method};
use crate::object;
use crate::opcode::ParamInfo;
use crate::types::{mask, TypeDecl};
use crate::vm::{PendingCall, Vm, VmResult};
use php_runtime::{ArrayKey, HashTable, Obj, Value, ZArray, ZStr};
use std::rc::Rc;

const PUB: u32 = flags::PUBLIC;
const STAT: u32 = flags::PUBLIC | flags::STATIC;

/// Attribute targets.
pub(crate) mod target {
    pub const CLASS: i64 = 1;
    pub const FUNCTION: i64 = 2;
    pub const METHOD: i64 = 4;
    pub const PROPERTY: i64 = 8;
    pub const CLASS_CONSTANT: i64 = 16;
    pub const PARAMETER: i64 = 32;
    pub const CONSTANT: i64 = 64;
}

/// Internal state of reflection objects.
#[derive(Clone)]
pub(crate) enum Refl {
    Class(Rc<Class>),
    Function { func: Func, closure: Option<Obj>, scope: Option<Rc<Class>> },
    Method { m: Rc<Method>, class: Rc<Class> },
    Param { func: Func, scope: Option<Rc<Class>>, idx: usize, closure: Option<Obj> },
    Prop { class: Rc<Class>, name: ZStr, info: Option<Rc<PropInfo>>, is_static: bool },
    Const { class: Rc<Class>, c: Rc<ClassConst> },
    Type { ty: TypeDecl, scope: Option<Rc<Class>> },
    Attr { info: AttributeInfo, target: i64, repeated: bool, scope: Option<Rc<Class>> },
}

fn refl(o: &Obj) -> Option<Refl> {
    object::internal::<Refl, _>(o, |r| r.clone())
}

impl Vm {
    fn refl_obj(&mut self, class: &str, r: Refl, props: &[(&str, Value)]) -> VmResult<Value> {
        let cls = self.find_class(class.as_bytes()).expect("reflection class");
        let o = self.instantiate_unchecked(&cls)?;
        for (k, v) in props {
            if let Some(pi) = cls.prop_map.get(&ZStr::from(*k)) {
                object::data(&o).slots.borrow_mut()[pi.slot as usize] = Some(v.clone());
            }
        }
        object::set_internal(&o, r);
        Ok(Value::Object(o))
    }

    fn refl_exc(&mut self, msg: &str) -> crate::vm::Throw {
        self.make_throw("ReflectionException", msg)
    }

    fn refl_class_value(&mut self, c: &Rc<Class>) -> VmResult<Value> {
        let cn = if c.is_enum() { "ReflectionEnum" } else { "ReflectionClass" };
        self.refl_obj(cn, Refl::Class(c.clone()), &[("name", Value::Str(c.name.clone()))])
    }

    fn refl_method_value(&mut self, m: &Rc<Method>, class: &Rc<Class>) -> VmResult<Value> {
        let decl = m.scope();
        self.refl_obj(
            "ReflectionMethod",
            Refl::Method { m: m.clone(), class: class.clone() },
            &[("name", Value::Str(m.name.clone())), ("class", Value::Str(decl.name.clone()))],
        )
    }

    fn refl_type_value(&mut self, ty: &TypeDecl, scope: Option<&Rc<Class>>) -> VmResult<Value> {
        let n_named = ty.classes.len()
            + [mask::NULL, mask::BOOL, mask::LONG, mask::DOUBLE, mask::STRING, mask::ARRAY, mask::OBJECT, mask::CALLABLE, mask::VOID, mask::NEVER, mask::STATIC, mask::ITERABLE]
                .iter()
                .filter(|m| ty.mask & **m == **m && **m != mask::NULL)
                .count()
            + usize::from(ty.mask & mask::BOOL != mask::BOOL && ty.mask & (mask::TRUE | mask::FALSE) != 0);
        let cls = if ty.pure_intersection {
            "ReflectionIntersectionType"
        } else if !ty.intersections.is_empty() || (n_named > 1 && !ty.is_mixed()) {
            "ReflectionUnionType"
        } else {
            "ReflectionNamedType"
        };
        self.refl_obj(cls, Refl::Type { ty: ty.clone(), scope: scope.cloned() }, &[])
    }

    fn attributes_array(
        &mut self,
        attrs: &[AttributeInfo],
        target: i64,
        scope: Option<&Rc<Class>>,
        a: &Args,
    ) -> VmResult<Value> {
        let filter = a.get(0).filter(|v| !v.is_null()).map(|v| v.as_str().cloned().unwrap_or_default());
        let fl = if a.has(1) { a.at(1).as_int().unwrap_or(0) } else { 0 };
        if fl & !2 != 0 {
            return Err(self.make_throw("ValueError", &format!("{}(): Argument #2 ($flags) must be a valid attribute filter flag", a.func_name())));
        }
        let filter_cls = match &filter {
            Some(f) if fl & 2 != 0 => match self.lookup_class(f)? {
                Some(c) => Some(c),
                None => {
                    return Err(self.make_throw("Error", &format!("Class \"{}\" not found", f.to_string_lossy())));
                }
            },
            _ => None,
        };
        let mut out = Vec::new();
        for at in attrs {
            if let Some(f) = &filter {
                if fl & 2 != 0 {
                    let ac = self.find_class(at.name.as_bytes());
                    match (ac, &filter_cls) {
                        (Some(ac), Some(fc)) if ac.is_subclass_of(fc) => {}
                        _ => continue,
                    }
                } else if !at.name.eq_ignore_ascii_case(f.as_bytes().strip_prefix(b"\\").unwrap_or(f.as_bytes())) {
                    continue;
                }
            }
            let repeated = attrs.iter().filter(|x| x.name.eq_ignore_ascii_case(at.name.as_bytes())).count() > 1;
            out.push(self.refl_obj(
                "ReflectionAttribute",
                Refl::Attr { info: at.clone(), target, repeated, scope: scope.cloned() },
                &[],
            )?);
        }
        Ok(Value::Array(ZArray::from_values(out)))
    }

    fn func_of(r: &Refl) -> Option<(Func, Option<Rc<Class>>)> {
        match r {
            Refl::Function { func, scope, .. } => Some((func.clone(), scope.clone())),
            Refl::Method { m, .. } => Some((m.func.clone(), Some(m.scope()))),
            _ => None,
        }
    }
}

fn this_refl(vm: &mut Vm, a: &Args) -> VmResult<Refl> {
    match refl(&a.this_obj()) {
        Some(r) => Ok(r),
        None => Err(vm.make_throw("Error", "Internal error: Failed to retrieve the reflection object")),
    }
}

fn this_class(vm: &mut Vm, a: &Args) -> VmResult<Rc<Class>> {
    match this_refl(vm, a)? {
        Refl::Class(c) => Ok(c),
        Refl::Method { class, .. } => Ok(class),
        Refl::Prop { class, .. } | Refl::Const { class, .. } => Ok(class),
        _ => Err(vm.make_throw("Error", "Not a class")),
    }
}

fn zs(s: &str) -> Value {
    Value::Str(ZStr::from(s))
}

// ---------------------------------------------------------------------
// ReflectionClass
// ---------------------------------------------------------------------

fn rc_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let arg = a.at(0);
    let cls = match &arg {
        Value::Object(o) => object::class_of(o).clone(),
        Value::Str(s) => match vm.lookup_class(s)? {
            Some(c) => c,
            None => {
                return Err(vm.refl_exc(&format!("Class \"{}\" does not exist", s.to_string_lossy())));
            }
        },
        _ => return Err(vm.refl_exc("Class \"\" does not exist")),
    };
    let o = a.this_obj();
    let c2 = object::class_of(&o).clone();
    if let Some(pi) = c2.prop_map.get(&ZStr::from("name")) {
        object::data(&o).slots.borrow_mut()[pi.slot as usize] = Some(Value::Str(cls.name.clone()));
    }
    object::set_internal(&o, Refl::Class(cls));
    Ok(Value::Null)
}

fn rc_get_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Str(this_class(vm, a)?.name.clone()))
}

fn short_name(n: &ZStr) -> ZStr {
    match n.as_bytes().iter().rposition(|&c| c == b'\\') {
        Some(p) => ZStr::from(&n.as_bytes()[p + 1..]),
        None => n.clone(),
    }
}

fn ns_name(n: &ZStr) -> ZStr {
    match n.as_bytes().iter().rposition(|&c| c == b'\\') {
        Some(p) => ZStr::from(&n.as_bytes()[..p]),
        None => ZStr::empty(),
    }
}

fn rc_get_short_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Str(short_name(&this_class(vm, a)?.name)))
}
fn rc_get_ns(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Str(ns_name(&this_class(vm, a)?.name)))
}
fn rc_in_ns(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_class(vm, a)?.name.contains(&b'\\')))
}

macro_rules! rc_bool {
    ($name:ident, |$c:ident| $e:expr) => {
        fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let $c = this_class(vm, a)?;
            Ok(Value::Bool($e))
        }
    };
}

rc_bool!(rc_is_interface, |c| c.is_interface());
rc_bool!(rc_is_trait, |c| c.is_trait());
rc_bool!(rc_is_enum, |c| c.is_enum());
rc_bool!(rc_is_abstract, |c| c.flags & cflags::ABSTRACT != 0 && !c.is_interface()
    || c.methods.values().any(|m| m.is_abstract()) && !c.is_interface() && !c.is_trait());
rc_bool!(rc_is_final, |c| c.flags & cflags::FINAL != 0);
rc_bool!(rc_is_readonly, |c| c.flags & cflags::READONLY != 0);
rc_bool!(rc_is_anon, |c| c.flags & cflags::ANON != 0);
rc_bool!(rc_is_internal, |c| c.flags & cflags::INTERNAL != 0);
rc_bool!(rc_is_user, |c| c.flags & cflags::INTERNAL == 0);
rc_bool!(rc_is_instantiable, |c| !c.is_interface()
    && !c.is_trait()
    && !c.is_enum()
    && c.flags & cflags::ABSTRACT == 0
    && c.magic.construct.as_ref().is_none_or(|m| m.visibility() == flags::PUBLIC));
rc_bool!(rc_is_cloneable, |c| !c.is_interface()
    && !c.is_trait()
    && !c.is_enum()
    && c.flags & (cflags::ABSTRACT | cflags::NO_CLONE) == 0
    && c.magic.clone.as_ref().is_none_or(|m| m.visibility() == flags::PUBLIC));
rc_bool!(rc_is_iterable, |c| c.instance_of_name(b"traversable") && !c.is_interface() && c.flags & cflags::ABSTRACT == 0);

fn rc_is_instance(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    Ok(Value::Bool(match a.at(0) {
        Value::Object(o) => object::class_of(&o).is_subclass_of(&c),
        _ => false,
    }))
}

fn class_from_arg(vm: &mut Vm, v: &Value) -> VmResult<Rc<Class>> {
    match v {
        Value::Object(o) => match refl(o) {
            Some(Refl::Class(c)) => Ok(c),
            _ => Ok(object::class_of(o).clone()),
        },
        Value::Str(s) => match vm.lookup_class(s)? {
            Some(c) => Ok(c),
            None => Err(vm.refl_exc(&format!("Class \"{}\" does not exist", s.to_string_lossy()))),
        },
        _ => Err(vm.refl_exc("Class \"\" does not exist")),
    }
}

fn rc_is_subclass_of(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let o = class_from_arg(vm, &a.at(0))?;
    Ok(Value::Bool(!Rc::ptr_eq(&c, &o) && c.is_subclass_of(&o)))
}

fn rc_implements(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let i = match a.at(0) {
        Value::Str(s) => match vm.lookup_class(&s)? {
            Some(c) => c,
            None => return Err(vm.refl_exc(&format!("Interface \"{}\" does not exist", s.to_string_lossy()))),
        },
        other => class_from_arg(vm, &other)?,
    };
    if !i.is_interface() {
        return Err(vm.refl_exc(&format!("{} is not an interface", i.name)));
    }
    Ok(Value::Bool(c.is_subclass_of(&i)))
}

fn rc_get_parent(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    match &c.parent {
        Some(p) => vm.refl_class_value(&p.clone()),
        None => Ok(Value::Bool(false)),
    }
}

fn rc_get_interface_names(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    Ok(Value::Array(ZArray::from_values(c.interfaces.iter().map(|i| Value::Str(i.name.clone())))))
}

fn rc_get_interfaces(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let mut t = HashTable::new();
    for i in &c.interfaces {
        let v = vm.refl_class_value(i)?;
        t.insert(ArrayKey::Str(i.name.clone()), v);
    }
    Ok(Value::Array(ZArray::from_table(t)))
}

fn rc_get_trait_names(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    Ok(Value::Array(ZArray::from_values(c.traits.iter().map(|i| Value::Str(i.name.clone())))))
}

fn rc_get_traits(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let mut t = HashTable::new();
    for i in &c.traits {
        let v = vm.refl_class_value(i)?;
        t.insert(ArrayKey::Str(i.name.clone()), v);
    }
    Ok(Value::Array(ZArray::from_table(t)))
}

fn method_filter_ok(m: &Method, filter: Option<i64>) -> bool {
    match filter {
        None => true,
        Some(f) => {
            let mut mf = 0;
            if m.flags & flags::PUBLIC != 0 {
                mf |= 1;
            }
            if m.flags & flags::PROTECTED != 0 {
                mf |= 2;
            }
            if m.flags & flags::PRIVATE != 0 {
                mf |= 4;
            }
            if m.flags & flags::STATIC != 0 {
                mf |= 16;
            }
            if m.flags & flags::FINAL != 0 {
                mf |= 32;
            }
            if m.flags & flags::ABSTRACT != 0 {
                mf |= 64;
            }
            mf & f != 0
        }
    }
}

fn rc_get_methods(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let filter = a.get(0).filter(|v| !v.is_null()).and_then(|v| v.as_int());
    let mut out = Vec::new();
    for lc in &c.method_order {
        let m = c.methods[lc].clone();
        if method_filter_ok(&m, filter) {
            out.push(vm.refl_method_value(&m, &c)?);
        }
    }
    Ok(Value::Array(ZArray::from_values(out)))
}

fn rc_get_method(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let n = a.at(0).as_str().cloned().unwrap_or_default();
    match c.find_method(n.as_bytes()).cloned() {
        Some(m) => vm.refl_method_value(&m, &c),
        None => Err(vm.refl_exc(&format!("Method {}::{}() does not exist", c.name, n.to_string_lossy()))),
    }
}

fn rc_has_method(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let n = a.at(0).as_str().cloned().unwrap_or_default();
    Ok(Value::Bool(c.find_method(n.as_bytes()).is_some()))
}

fn rc_get_constructor(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    match c.magic.construct.clone() {
        Some(m) => vm.refl_method_value(&m, &c),
        None => Ok(Value::Null),
    }
}

fn prop_value(vm: &mut Vm, class: &Rc<Class>, pi: Option<Rc<PropInfo>>, name: &ZStr, is_static: bool) -> VmResult<Value> {
    let decl = pi.as_ref().map_or_else(|| class.name.clone(), |p| p.class.clone());
    vm.refl_obj(
        "ReflectionProperty",
        Refl::Prop { class: class.clone(), name: name.clone(), info: pi, is_static },
        &[("name", Value::Str(name.clone())), ("class", Value::Str(decl))],
    )
}

fn prop_filter_ok(fl: u32, filter: Option<i64>) -> bool {
    match filter {
        None => true,
        Some(f) => {
            let mut mf = 0;
            if fl & flags::PUBLIC != 0 {
                mf |= 1;
            }
            if fl & flags::PROTECTED != 0 {
                mf |= 2;
            }
            if fl & flags::PRIVATE != 0 {
                mf |= 4;
            }
            if fl & flags::STATIC != 0 {
                mf |= 16;
            }
            if fl & flags::READONLY != 0 {
                mf |= 128;
            }
            mf & f != 0
        }
    }
}

fn rc_get_properties(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let filter = a.get(0).filter(|v| !v.is_null()).and_then(|v| v.as_int());
    let mut out = Vec::new();
    // Own/visible declared properties: those in prop_map plus own privates.
    let mut seen: Vec<ZStr> = Vec::new();
    for pi in &c.props {
        let visible = c.prop_map.get(&pi.name).is_some_and(|x| Rc::ptr_eq(x, pi));
        if !visible || seen.contains(&pi.name) {
            continue;
        }
        if prop_filter_ok(pi.flags, filter) {
            seen.push(pi.name.clone());
            out.push(prop_value(vm, &c, Some(pi.clone()), &pi.name.clone(), false)?);
        }
    }
    for sp in c.static_props.clone() {
        if prop_filter_ok(sp.flags, filter) {
            out.push(prop_value(vm, &c, None, &sp.name, true)?);
        }
    }
    if let Value::Object(o) = a.this.clone().map(Value::Object).unwrap_or_default() {
        let _ = o;
    }
    Ok(Value::Array(ZArray::from_values(out)))
}

fn rc_get_property(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let n = a.at(0).as_str().cloned().unwrap_or_default();
    if let Some(pi) = c.prop_map.get(&n).cloned() {
        return prop_value(vm, &c, Some(pi), &n, false);
    }
    if c.find_static(n.as_bytes()).is_some() {
        return prop_value(vm, &c, None, &n, true);
    }
    if let Some(pi) = c.props.iter().find(|p| p.name == n && p.class.eq_ignore_ascii_case(c.name.as_bytes())).cloned() {
        return prop_value(vm, &c, Some(pi), &n, false);
    }
    Err(vm.refl_exc(&format!("Property {}::${} does not exist", c.name, n.to_string_lossy())))
}

fn rc_has_property(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let n = a.at(0).as_str().cloned().unwrap_or_default();
    Ok(Value::Bool(c.prop_map.contains_key(&n) || c.find_static(n.as_bytes()).is_some()))
}

fn rc_get_constants(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let mut t = HashTable::new();
    for cc in c.consts.clone() {
        let v = vm.class_constant(&c, &cc.name)?;
        t.insert(ArrayKey::Str(cc.name.clone()), v);
    }
    Ok(Value::Array(ZArray::from_table(t)))
}

fn rc_get_constant(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let n = a.at(0).as_str().cloned().unwrap_or_default();
    if c.find_const(n.as_bytes()).is_none() {
        return Ok(Value::Bool(false));
    }
    vm.class_constant(&c, &n)
}

fn rc_has_constant(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let n = a.at(0).as_str().cloned().unwrap_or_default();
    Ok(Value::Bool(c.find_const(n.as_bytes()).is_some()))
}

fn const_value(vm: &mut Vm, c: &Rc<Class>, cc: &Rc<ClassConst>) -> VmResult<Value> {
    let cls = if cc.is_case {
        if c.backing.is_some() { "ReflectionEnumBackedCase" } else { "ReflectionEnumUnitCase" }
    } else {
        "ReflectionClassConstant"
    };
    vm.refl_obj(
        cls,
        Refl::Const { class: c.clone(), c: cc.clone() },
        &[("name", Value::Str(cc.name.clone())), ("class", Value::Str(cc.class.clone()))],
    )
}

fn rc_get_reflection_constants(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let mut out = Vec::new();
    for cc in c.consts.clone() {
        out.push(const_value(vm, &c, &cc)?);
    }
    Ok(Value::Array(ZArray::from_values(out)))
}

fn rc_get_reflection_constant(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let n = a.at(0).as_str().cloned().unwrap_or_default();
    match c.find_const(n.as_bytes()).cloned() {
        Some(cc) => const_value(vm, &c, &cc),
        None => Ok(Value::Bool(false)),
    }
}

fn rc_new_instance(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let args = a.values.clone();
    let named = std::mem::take(&mut a.named);
    new_instance(vm, &c, args, named)
}

fn new_instance(vm: &mut Vm, c: &Rc<Class>, args: Vec<Value>, named: Vec<(ZStr, Value)>) -> VmResult<Value> {
    let o = vm.instantiate(c)?;
    match c.magic.construct.clone() {
        Some(m) => {
            if m.visibility() != flags::PUBLIC {
                return Err(vm.refl_exc(&format!("Access to non-public constructor of class {}", c.name)));
            }
            let mut call = PendingCall {
                this: Some(o.clone()),
                scope: Some(m.scope()),
                called: Some(c.clone()),
                ..PendingCall::new(m.func.clone())
            };
            call.args = args.into_iter().map(Some).collect();
            for (k, v) in named {
                match m.func.params().iter().position(|p| p.name == k) {
                    Some(p) => {
                        if call.args.len() <= p {
                            call.args.resize(p + 1, None);
                        }
                        call.args[p] = Some(v);
                    }
                    None => {
                        return Err(vm.make_throw("Error", &format!("Unknown named parameter ${}", k.to_string_lossy())));
                    }
                }
                call.has_named = true;
            }
            vm.call_pending(call)?;
        }
        None => {
            if !args.is_empty() || !named.is_empty() {
                return Err(vm.refl_exc(&format!(
                    "Class {} does not have a constructor, so you cannot pass any constructor arguments",
                    c.name
                )));
            }
        }
    }
    Ok(Value::Object(o))
}

fn rc_new_instance_args(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let mut args = Vec::new();
    let mut named = Vec::new();
    if let Value::Array(arr) = a.at(0) {
        for (k, v) in arr.iter() {
            match k {
                ArrayKey::Int(_) => args.push(v.clone()),
                ArrayKey::Str(s) => named.push((s.clone(), v.clone())),
            }
        }
    }
    new_instance(vm, &c, args, named)
}

fn rc_new_instance_without_ctor(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    if c.flags & cflags::INTERNAL != 0 && c.flags & cflags::FINAL != 0 {
        return Err(vm.refl_exc(&format!(
            "Class {} is an internal class marked as final that cannot be instantiated without invoking its constructor",
            c.name
        )));
    }
    let o = vm.instantiate(&c)?;
    Ok(Value::Object(o))
}

fn rc_get_attributes(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let attrs = c.attributes.clone();
    vm.attributes_array(&attrs, target::CLASS, Some(&c), a)
}

fn rc_get_doc_comment(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    Ok(c.doc_comment.clone().map_or(Value::Bool(false), Value::Str))
}

fn rc_get_file_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    Ok(c.filename.clone().map_or(Value::Bool(false), Value::Str))
}

fn rc_get_start_line(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    Ok(if c.filename.is_some() { Value::Int(i64::from(c.line_start)) } else { Value::Bool(false) })
}

fn rc_get_end_line(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    Ok(if c.filename.is_some() { Value::Int(i64::from(c.line_end)) } else { Value::Bool(false) })
}

fn rc_get_static_property_value(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let n = a.at(0).as_str().cloned().unwrap_or_default();
    if c.find_static(n.as_bytes()).is_none() {
        if a.has(1) {
            return Ok(a.at(1));
        }
        return Err(vm.refl_exc(&format!("Property {}::${} does not exist", c.name, n.to_string_lossy())));
    }
    let cell = vm.static_prop_cell(&c, &n)?;
    let v = cell.borrow().deref();
    Ok(v)
}

fn rc_set_static_property_value(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let n = a.at(0).as_str().cloned().unwrap_or_default();
    if c.find_static(n.as_bytes()).is_none() {
        return Err(vm.refl_exc(&format!("Class {} does not have a property named {}", c.name, n.to_string_lossy())));
    }
    let cell = vm.static_prop_cell(&c, &n)?;
    let v = a.at(1);
    let mut b = cell.borrow_mut();
    match &mut *b {
        Value::Reference(r) => {
            r.set(v);
        }
        slot => *slot = v,
    }
    Ok(Value::Null)
}

fn rc_get_static_properties(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let mut t = HashTable::new();
    for sp in c.static_props.clone() {
        let cell = vm.static_prop_cell(&c, &sp.name);
        if let Ok(cell) = cell {
            t.insert(ArrayKey::Str(sp.name.clone()), cell.borrow().deref());
        }
    }
    Ok(Value::Array(ZArray::from_table(t)))
}

fn rc_get_default_properties(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let mut t = HashTable::new();
    for sp in c.static_props.clone() {
        if let Ok(cell) = vm.static_prop_cell(&c, &sp.name) {
            t.insert(ArrayKey::Str(sp.name.clone()), cell.borrow().deref());
        }
    }
    let defs = vm.class_defaults(&c)?;
    for pi in &c.props {
        if c.prop_map.get(&pi.name).is_some_and(|x| Rc::ptr_eq(x, pi)) {
            if let Some(Some(v)) = defs.get(pi.slot as usize) {
                t.insert(ArrayKey::Str(pi.name.clone()), v.clone());
            }
        }
    }
    Ok(Value::Array(ZArray::from_table(t)))
}

fn rc_get_modifiers(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let mut m = 0;
    if c.flags & cflags::EXPLICIT_ABSTRACT != 0 {
        m |= 64;
    }
    if c.flags & cflags::FINAL != 0 {
        m |= 32;
    }
    if c.flags & cflags::READONLY != 0 {
        m |= 65536;
    }
    Ok(Value::Int(m))
}

fn rc_get_extension_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    Ok(if c.flags & cflags::INTERNAL != 0 { zs("Core") } else { Value::Bool(false) })
}

fn rc_to_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    Ok(Value::Str(ZStr::from(format!("Class [ <user> class {} ] {{}}\n", c.name))))
}

// ReflectionEnum
fn re_get_cases(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let mut out = Vec::new();
    for cc in c.consts.clone() {
        if cc.is_case {
            out.push(const_value(vm, &c, &cc)?);
        }
    }
    Ok(Value::Array(ZArray::from_values(out)))
}

fn re_get_case(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let n = a.at(0).as_str().cloned().unwrap_or_default();
    match c.find_const(n.as_bytes()).cloned() {
        Some(cc) if cc.is_case => const_value(vm, &c, &cc),
        Some(_) => Err(vm.refl_exc(&format!("{}::{} is not a case", c.name, n.to_string_lossy()))),
        None => Err(vm.refl_exc(&format!("Case {}::{} does not exist", c.name, n.to_string_lossy()))),
    }
}

fn re_has_case(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    let n = a.at(0).as_str().cloned().unwrap_or_default();
    Ok(Value::Bool(c.find_const(n.as_bytes()).is_some_and(|cc| cc.is_case)))
}

fn re_is_backed(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_class(vm, a)?.backing.is_some()))
}

fn re_get_backing_type(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = this_class(vm, a)?;
    match c.backing {
        Some(m) => vm.refl_type_value(&TypeDecl::simple(m), None),
        None => Ok(Value::Null),
    }
}

fn re_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    rc_construct(vm, a)?;
    let c = this_class(vm, a)?;
    if !c.is_enum() {
        return Err(vm.refl_exc(&format!("Class \"{}\" is not an enum", c.name)));
    }
    Ok(Value::Null)
}

// ---------------------------------------------------------------------
// Functions and methods
// ---------------------------------------------------------------------

fn rf_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let (func, closure, scope, name) = match a.at(0) {
        Value::Object(c) => match vm.closure_data(&c) {
            Some(d) => {
                let n = d.func.name().clone();
                (d.func.clone(), Some(c.clone()), d.scope.clone(), n)
            }
            None => return Err(vm.refl_exc("Function does not exist")),
        },
        Value::Str(s) => {
            let n = s.as_bytes().strip_prefix(b"\\").unwrap_or(s.as_bytes());
            match vm.functions.get(n.to_ascii_lowercase().as_slice()).cloned() {
                Some(f) => {
                    let name = f.name().clone();
                    (f, None, None, name)
                }
                None => {
                    return Err(vm.refl_exc(&format!("Function {}() does not exist", s.to_string_lossy())));
                }
            }
        }
        _ => return Err(vm.refl_exc("Function does not exist")),
    };
    let c2 = object::class_of(&o).clone();
    if let Some(pi) = c2.prop_map.get(&ZStr::from("name")) {
        object::data(&o).slots.borrow_mut()[pi.slot as usize] = Some(Value::Str(name));
    }
    object::set_internal(&o, Refl::Function { func, closure, scope });
    Ok(Value::Null)
}

fn rm_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let (cls, mname) = if a.has(1) && !a.at(1).is_null() {
        let c = class_from_arg(vm, &a.at(0))?;
        (c, a.at(1).as_str().cloned().unwrap_or_default())
    } else {
        let s = vm.to_zstr(&a.at(0))?;
        match crate::call::find_double_colon(s.as_bytes()) {
            Some(p) => {
                let cn = ZStr::from(&s.as_bytes()[..p]);
                let c = class_from_arg(vm, &Value::Str(cn))?;
                (c, ZStr::from(&s.as_bytes()[p + 2..]))
            }
            None => {
                return Err(vm.refl_exc(&format!(
                    "ReflectionMethod::__construct(): Argument #1 ($objectOrMethod) must be a valid method name"
                )))
            }
        }
    };
    let Some(m) = cls.find_method(mname.as_bytes()).cloned() else {
        return Err(vm.refl_exc(&format!("Method {}::{}() does not exist", cls.name, mname.to_string_lossy())));
    };
    let c2 = object::class_of(&o).clone();
    if let Some(pi) = c2.prop_map.get(&ZStr::from("name")) {
        object::data(&o).slots.borrow_mut()[pi.slot as usize] = Some(Value::Str(m.name.clone()));
    }
    if let Some(pi) = c2.prop_map.get(&ZStr::from("class")) {
        object::data(&o).slots.borrow_mut()[pi.slot as usize] = Some(Value::Str(m.scope().name.clone()));
    }
    object::set_internal(&o, Refl::Method { m, class: cls });
    Ok(Value::Null)
}

fn this_func(vm: &mut Vm, a: &Args) -> VmResult<(Func, Option<Rc<Class>>)> {
    let r = this_refl(vm, a)?;
    match Vm::func_of(&r) {
        Some(x) => Ok(x),
        None => Err(vm.make_throw("Error", "Not a function")),
    }
}

fn rf_get_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, _) = this_func(vm, a)?;
    Ok(Value::Str(f.name().clone()))
}

fn rf_get_short_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, _) = this_func(vm, a)?;
    Ok(Value::Str(short_name(f.name())))
}

fn rf_get_ns(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, _) = this_func(vm, a)?;
    Ok(Value::Str(ns_name(f.name())))
}

fn rf_in_ns(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, _) = this_func(vm, a)?;
    Ok(Value::Bool(f.name().contains(&b'\\')))
}

fn rf_get_parameters(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = this_refl(vm, a)?;
    let (f, scope) = Vm::func_of(&r).expect("function reflection");
    let closure = match &r {
        Refl::Function { closure, .. } => closure.clone(),
        _ => None,
    };
    let mut out = Vec::new();
    for (i, p) in f.params().iter().enumerate() {
        out.push(vm.refl_obj(
            "ReflectionParameter",
            Refl::Param { func: f.clone(), scope: scope.clone(), idx: i, closure: closure.clone() },
            &[("name", Value::Str(p.name.clone()))],
        )?);
    }
    Ok(Value::Array(ZArray::from_values(out)))
}

fn rf_num_params(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, _) = this_func(vm, a)?;
    Ok(Value::Int(i64::try_from(f.params().len()).unwrap_or(0)))
}

fn rf_num_required(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, _) = this_func(vm, a)?;
    Ok(Value::Int(i64::from(f.required())))
}

fn rf_get_return_type(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, scope) = this_func(vm, a)?;
    match f.return_type().cloned() {
        Some(t) => vm.refl_type_value(&t, scope.as_ref()),
        None => Ok(Value::Null),
    }
}

fn rf_has_return_type(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, _) = this_func(vm, a)?;
    Ok(Value::Bool(f.return_type().is_some()))
}

macro_rules! rf_bool {
    ($name:ident, |$f:ident| $e:expr) => {
        fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let ($f, _) = this_func(vm, a)?;
            Ok(Value::Bool($e))
        }
    };
}

rf_bool!(rf_is_variadic, |f| f.is_variadic());
rf_bool!(rf_returns_ref, |f| f.flags() & flags::RETURN_REF != 0);
rf_bool!(rf_is_generator, |f| f.flags() & flags::GENERATOR != 0);
rf_bool!(rf_is_closure, |f| f.flags() & flags::CLOSURE != 0);
rf_bool!(rf_is_internal, |f| !f.is_user());
rf_bool!(rf_is_user, |f| f.is_user());
rf_bool!(rf_is_deprecated, |f| f.flags() & flags::DEPRECATED != 0);
rf_bool!(rf_is_static, |f| f.flags() & flags::STATIC != 0);

fn rf_get_doc_comment(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, _) = this_func(vm, a)?;
    Ok(match f {
        Func::User(u) => u.doc_comment.clone().map_or(Value::Bool(false), Value::Str),
        Func::Builtin(_) => Value::Bool(false),
    })
}

fn rf_get_start_line(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, _) = this_func(vm, a)?;
    Ok(match f {
        Func::User(u) => Value::Int(i64::from(u.line_start)),
        Func::Builtin(_) => Value::Bool(false),
    })
}

fn rf_get_end_line(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, _) = this_func(vm, a)?;
    Ok(match f {
        Func::User(u) => Value::Int(i64::from(u.line_end)),
        Func::Builtin(_) => Value::Bool(false),
    })
}

fn rf_get_file_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, _) = this_func(vm, a)?;
    Ok(match f {
        Func::User(u) => Value::Str(u.code.filename.clone()),
        Func::Builtin(_) => Value::Bool(false),
    })
}

fn rf_get_attributes(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = this_refl(vm, a)?;
    let (f, scope) = Vm::func_of(&r).expect("function reflection");
    let t = if matches!(r, Refl::Method { .. }) { target::METHOD } else { target::FUNCTION };
    let attrs = match &f {
        Func::User(u) => u.attributes.clone(),
        Func::Builtin(_) => Vec::new(),
    };
    vm.attributes_array(&attrs, t, scope.as_ref(), a)
}

fn rf_get_static_variables(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = this_refl(vm, a)?;
    let mut t = HashTable::new();
    if let Refl::Function { func: Func::User(u), closure: Some(c), .. } = &r {
        if let Some(d) = vm.closure_data(c) {
            for (cv, v) in d.vars {
                let n = u.code.cv_names.get(cv as usize).cloned().unwrap_or_default();
                t.insert(ArrayKey::Str(n), v.deref());
            }
        }
    }
    Ok(Value::Array(ZArray::from_table(t)))
}

fn rf_get_closure_this(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = this_refl(vm, a)?;
    if let Refl::Function { closure: Some(c), .. } = &r {
        if let Some(d) = vm.closure_data(c) {
            return Ok(d.this.map_or(Value::Null, Value::Object));
        }
    }
    Ok(Value::Null)
}

fn rf_get_closure_scope_class(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = this_refl(vm, a)?;
    if let Refl::Function { scope: Some(s), .. } = &r {
        return vm.refl_class_value(s);
    }
    Ok(Value::Null)
}

fn rf_get_closure_used_variables(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    rf_get_static_variables(vm, a)
}

fn rf_invoke(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = this_refl(vm, a)?;
    let args = a.values.clone();
    let named = std::mem::take(&mut a.named);
    match r {
        Refl::Function { closure: Some(c), .. } => vm.call_value_named(&Value::Object(c), args, named),
        Refl::Function { func, .. } => {
            let mut call = PendingCall::new(func);
            call.args = args.into_iter().map(Some).collect();
            vm.call_pending(call)
        }
        _ => Ok(Value::Null),
    }
}

fn rf_invoke_args(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let arr = match a.at(0) {
        Value::Array(x) => x,
        _ => ZArray::new(),
    };
    a.values = arr.iter().filter(|(k, _)| matches!(k, ArrayKey::Int(_))).map(|(_, v)| v.clone()).collect();
    a.named = arr
        .iter()
        .filter_map(|(k, v)| match k {
            ArrayKey::Str(s) => Some((s.clone(), v.clone())),
            ArrayKey::Int(_) => None,
        })
        .collect();
    rf_invoke(vm, a)
}

fn rf_get_closure(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = this_refl(vm, a)?;
    match r {
        Refl::Function { closure: Some(c), .. } => Ok(Value::Object(c)),
        Refl::Function { func, scope, .. } => {
            let mut call = PendingCall::new(func);
            call.scope = scope;
            vm.make_closure_from_call(call)
        }
        Refl::Method { m, class } => {
            let this = if m.is_static() {
                None
            } else {
                match a.at(0) {
                    Value::Object(o) => Some(o),
                    _ => {
                        return Err(vm.make_throw(
                            "TypeError",
                            "ReflectionMethod::getClosure(): Argument #1 ($object) must be of type object, null given",
                        ))
                    }
                }
            };
            let called = this.as_ref().map_or(class, |t| object::class_of(t).clone());
            let call = PendingCall {
                this,
                scope: Some(m.scope()),
                called: Some(called),
                ..PendingCall::new(m.func.clone())
            };
            vm.make_closure_from_call(call)
        }
        _ => Ok(Value::Null),
    }
}

fn rf_is_anonymous(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, _) = this_func(vm, a)?;
    Ok(Value::Bool(match f {
        Func::User(u) => u.is_closure() && u.name.starts_with(b"{closure"),
        Func::Builtin(_) => false,
    }))
}

fn this_method(vm: &mut Vm, a: &Args) -> VmResult<(Rc<Method>, Rc<Class>)> {
    match this_refl(vm, a)? {
        Refl::Method { m, class } => Ok((m, class)),
        _ => Err(vm.make_throw("Error", "Not a method")),
    }
}

macro_rules! rm_bool {
    ($name:ident, |$m:ident| $e:expr) => {
        fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let ($m, _) = this_method(vm, a)?;
            Ok(Value::Bool($e))
        }
    };
}

rm_bool!(rm_is_public, |m| m.visibility() == flags::PUBLIC);
rm_bool!(rm_is_private, |m| m.visibility() == flags::PRIVATE);
rm_bool!(rm_is_protected, |m| m.visibility() == flags::PROTECTED);
rm_bool!(rm_is_abstract, |m| m.is_abstract());
rm_bool!(rm_is_final, |m| m.flags & flags::FINAL != 0);
rm_bool!(rm_is_static, |m| m.is_static());
rm_bool!(rm_is_ctor, |m| m.name.eq_ignore_ascii_case(b"__construct"));
rm_bool!(rm_is_dtor, |m| m.name.eq_ignore_ascii_case(b"__destruct"));

fn rm_get_declaring_class(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (m, _) = this_method(vm, a)?;
    vm.refl_class_value(&m.scope())
}

fn modifiers_of(fl: u32) -> i64 {
    let mut m = 0;
    if fl & flags::PUBLIC != 0 {
        m |= 1;
    }
    if fl & flags::PROTECTED != 0 {
        m |= 2;
    }
    if fl & flags::PRIVATE != 0 {
        m |= 4;
    }
    if fl & flags::STATIC != 0 {
        m |= 16;
    }
    if fl & flags::FINAL != 0 {
        m |= 32;
    }
    if fl & flags::ABSTRACT != 0 {
        m |= 64;
    }
    if fl & flags::READONLY != 0 {
        m |= 128;
    }
    m
}

fn rm_get_modifiers(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (m, _) = this_method(vm, a)?;
    Ok(Value::Int(modifiers_of(m.flags)))
}

fn rm_invoke(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (m, class) = this_method(vm, a)?;
    let obj = a.at(0);
    let args: Vec<Value> = a.values.iter().skip(1).cloned().collect();
    invoke_method(vm, &m, &class, obj, args)
}

fn invoke_method(vm: &mut Vm, m: &Rc<Method>, class: &Rc<Class>, obj: Value, args: Vec<Value>) -> VmResult<Value> {
    if m.is_abstract() {
        return Err(vm.refl_exc(&format!("Trying to invoke abstract method {}::{}()", m.scope().name, m.name)));
    }
    let this = if m.is_static() {
        None
    } else {
        match obj {
            Value::Object(o) => {
                if !object::class_of(&o).is_subclass_of(&m.scope()) {
                    return Err(vm.refl_exc("Given object is not an instance of the class this method was declared in"));
                }
                Some(o)
            }
            _ => {
                return Err(vm.refl_exc(&format!(
                    "Trying to invoke non static method {}::{}() without an object",
                    m.scope().name, m.name
                )))
            }
        }
    };
    let called = this.as_ref().map_or_else(|| class.clone(), |t| object::class_of(t).clone());
    let mut call = PendingCall {
        this,
        scope: Some(m.scope()),
        called: Some(called),
        ..PendingCall::new(m.func.clone())
    };
    call.args = args.into_iter().map(Some).collect();
    vm.call_pending(call)
}

fn rm_invoke_args(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (m, class) = this_method(vm, a)?;
    let obj = a.at(0);
    let args: Vec<Value> = match a.at(1) {
        Value::Array(x) => x.iter().map(|(_, v)| v.clone()).collect(),
        _ => Vec::new(),
    };
    invoke_method(vm, &m, &class, obj, args)
}

fn ret_null(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Null)
}

fn rm_get_prototype(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (m, class) = this_method(vm, a)?;
    let lc = ZStr::from(m.name.to_ascii_lowercase());
    let mut p = m.scope().parent.clone();
    while let Some(pc) = p {
        if let Some(pm) = pc.methods.get(&lc).cloned() {
            if pm.visibility() != flags::PRIVATE {
                return vm.refl_method_value(&pm, &pc);
            }
        }
        p = pc.parent.clone();
    }
    for i in &class.interfaces {
        if let Some(pm) = i.methods.get(&lc).cloned() {
            return vm.refl_method_value(&pm, i);
        }
    }
    Err(vm.refl_exc(&format!("Method {}::{} does not have a prototype", class.name, m.name)))
}

// ---------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------

fn this_param(vm: &mut Vm, a: &Args) -> VmResult<(ParamInfo, Func, Option<Rc<Class>>, usize)> {
    match this_refl(vm, a)? {
        Refl::Param { func, scope, idx, .. } => {
            let p = func.params()[idx].clone();
            Ok((p, func, scope, idx))
        }
        _ => Err(vm.make_throw("Error", "Not a parameter")),
    }
}

fn rp_get_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Str(this_param(vm, a)?.0.name))
}
fn rp_get_position(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(i64::try_from(this_param(vm, a)?.3).unwrap_or(0)))
}
fn rp_is_optional(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (p, f, _, idx) = this_param(vm, a)?;
    Ok(Value::Bool(p.variadic || idx >= f.required() as usize))
}
fn rp_has_default(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (p, _, _, _) = this_param(vm, a)?;
    Ok(Value::Bool(p.has_default))
}
fn rp_get_default(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (p, f, scope, _) = this_param(vm, a)?;
    if !p.has_default {
        return Err(vm.refl_exc("Internal error: Failed to retrieve the default value"));
    }
    if let Some(e) = &p.default_expr {
        return vm.eval_const_expr(e, scope.as_ref());
    }
    if let Func::Builtin(b) = &f {
        let i = b.params.iter().position(|x| x.name == p.name).unwrap_or(0);
        return Ok(b.defaults.get(i).cloned().flatten().unwrap_or_default());
    }
    Ok(Value::Null)
}
fn rp_has_type(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_param(vm, a)?.0.ty.is_some()))
}
fn rp_get_type(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (p, _, scope, _) = this_param(vm, a)?;
    match p.ty {
        Some(t) => vm.refl_type_value(&t, scope.as_ref()),
        None => Ok(Value::Null),
    }
}
fn rp_allows_null(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_param(vm, a)?.0;
    Ok(Value::Bool(p.ty.as_ref().is_none_or(TypeDecl::allows_null)))
}
fn rp_by_ref(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_param(vm, a)?.0.by_ref))
}
fn rp_by_val(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(!this_param(vm, a)?.0.by_ref))
}
fn rp_is_variadic(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_param(vm, a)?.0.variadic))
}
fn rp_is_promoted(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_param(vm, a)?.0.promote != 0))
}
fn rp_get_attributes(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (p, _, scope, _) = this_param(vm, a)?;
    vm.attributes_array(&p.attributes, target::PARAMETER, scope.as_ref(), a)
}
fn rp_get_declaring_function(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, f, scope, _) = this_param(vm, a)?;
    if let Some(s) = &scope {
        if let Some(m) = s.methods.values().find(|m| Func::ptr_eq(&m.func, &f)).cloned() {
            return vm.refl_method_value(&m, &s.clone());
        }
    }
    let name = f.name().clone();
    vm.refl_obj("ReflectionFunction", Refl::Function { func: f, closure: None, scope }, &[("name", Value::Str(name))])
}
fn rp_get_declaring_class(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, _, scope, _) = this_param(vm, a)?;
    match scope {
        Some(s) => vm.refl_class_value(&s),
        None => Ok(Value::Null),
    }
}
fn rp_is_default_constant(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_param(vm, a)?.0;
    Ok(Value::Bool(p.default_text.as_ref().is_some_and(|t| {
        let t = t.trim();
        !t.is_empty() && (t.chars().next().is_some_and(|c| c.is_ascii_uppercase() || c == '\\') && !t.eq_ignore_ascii_case("null") && !t.eq_ignore_ascii_case("true") && !t.eq_ignore_ascii_case("false") || t.contains("::")) && !t.ends_with(')')
    })))
}
fn rp_get_default_constant_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_param(vm, a)?.0;
    Ok(p.default_text.map_or(Value::Null, |t| Value::Str(ZStr::from(t.trim().trim_start_matches('\\')))))
}

// ---------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------

fn this_type(vm: &mut Vm, a: &Args) -> VmResult<(TypeDecl, Option<Rc<Class>>)> {
    match this_refl(vm, a)? {
        Refl::Type { ty, scope } => Ok((ty, scope)),
        _ => Err(vm.make_throw("Error", "Not a type")),
    }
}

fn rt_to_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (t, scope) = this_type(vm, a)?;
    let s = t.display(scope.as_ref().map(|c| &c.name), scope.as_ref().and_then(|c| c.parent_name()), None);
    Ok(Value::Str(ZStr::from(s)))
}

fn rt_get_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (mut t, scope) = this_type(vm, a)?;
    if !t.is_mixed() && t.mask & mask::NULL != 0 && (t.mask != mask::NULL || t.has_classes()) {
        t.mask &= !mask::NULL;
    }
    let s = t.display(scope.as_ref().map(|c| &c.name), scope.as_ref().and_then(|c| c.parent_name()), None);
    Ok(Value::Str(ZStr::from(s)))
}

fn rt_allows_null(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_type(vm, a)?.0.allows_null()))
}

fn rt_is_builtin(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (t, _) = this_type(vm, a)?;
    Ok(Value::Bool(!t.has_classes() && t.mask & mask::STATIC == 0))
}

fn rt_get_types(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (t, scope) = this_type(vm, a)?;
    let mut out = Vec::new();
    if t.pure_intersection {
        for c in &t.intersections[0] {
            let td = TypeDecl { classes: vec![c.clone()], ..Default::default() };
            out.push(vm.refl_type_value(&td, scope.as_ref())?);
        }
        return Ok(Value::Array(ZArray::from_values(out)));
    }
    for g in &t.intersections {
        let td = TypeDecl { intersections: vec![g.clone()], pure_intersection: true, ..Default::default() };
        out.push(vm.refl_type_value(&td, scope.as_ref())?);
    }
    for c in &t.classes {
        let td = TypeDecl { classes: vec![c.clone()], ..Default::default() };
        out.push(vm.refl_type_value(&td, scope.as_ref())?);
    }
    let order = [
        mask::STATIC,
        mask::CALLABLE,
        mask::OBJECT,
        mask::ARRAY,
        mask::STRING,
        mask::LONG,
        mask::DOUBLE,
        mask::BOOL,
        mask::FALSE,
        mask::TRUE,
        mask::VOID,
        mask::NEVER,
        mask::NULL,
    ];
    for m in order {
        if m == mask::FALSE || m == mask::TRUE {
            if t.mask & mask::BOOL == mask::BOOL || t.mask & m == 0 {
                continue;
            }
        } else if t.mask & m != m {
            continue;
        }
        out.push(vm.refl_type_value(&TypeDecl::simple(m), scope.as_ref())?);
    }
    Ok(Value::Array(ZArray::from_values(out)))
}

// ---------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------

fn rprop_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let c = class_from_arg(vm, &a.at(0))?;
    let n = a.at(1).as_str().cloned().unwrap_or_default();
    let (info, is_static) = if let Some(pi) = c.prop_map.get(&n).cloned() {
        (Some(pi), false)
    } else if c.find_static(n.as_bytes()).is_some() {
        (None, true)
    } else if let Some(pi) = c.props.iter().find(|p| p.name == n && p.class.eq_ignore_ascii_case(c.name.as_bytes())).cloned() {
        (Some(pi), false)
    } else {
        let dynamic = match a.at(0) {
            Value::Object(obj) => object::data(&obj)
                .dynamic
                .borrow()
                .as_ref()
                .is_some_and(|t| t.contains_key(&ArrayKey::Str(n.clone()))),
            _ => false,
        };
        if !dynamic {
            return Err(vm.refl_exc(&format!("Property {}::${} does not exist", c.name, n.to_string_lossy())));
        }
        (None, false)
    };
    let decl = info.as_ref().map_or_else(|| c.name.clone(), |p| p.class.clone());
    let c2 = object::class_of(&o).clone();
    if let Some(pi) = c2.prop_map.get(&ZStr::from("name")) {
        object::data(&o).slots.borrow_mut()[pi.slot as usize] = Some(Value::Str(n.clone()));
    }
    if let Some(pi) = c2.prop_map.get(&ZStr::from("class")) {
        object::data(&o).slots.borrow_mut()[pi.slot as usize] = Some(Value::Str(decl));
    }
    object::set_internal(&o, Refl::Prop { class: c, name: n, info, is_static });
    Ok(Value::Null)
}

fn this_prop(vm: &mut Vm, a: &Args) -> VmResult<(Rc<Class>, ZStr, Option<Rc<PropInfo>>, bool)> {
    match this_refl(vm, a)? {
        Refl::Prop { class, name, info, is_static } => Ok((class, name, info, is_static)),
        _ => Err(vm.make_throw("Error", "Not a property")),
    }
}

fn prop_flags(c: &Class, name: &ZStr, info: Option<&Rc<PropInfo>>, is_static: bool) -> u32 {
    if is_static {
        return c.find_static(name.as_bytes()).map_or(flags::PUBLIC | flags::STATIC, |s| s.flags);
    }
    info.map_or(flags::PUBLIC, |p| p.flags)
}

fn rprop_get_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Str(this_prop(vm, a)?.1))
}

fn rprop_get_value(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, n, info, is_static) = this_prop(vm, a)?;
    if is_static {
        let cell = vm.static_prop_cell(&c, &n)?;
        let v = cell.borrow().deref();
        return Ok(v);
    }
    let Value::Object(o) = a.at(0) else {
        return Err(vm.make_throw(
            "TypeError",
            "ReflectionProperty::getValue(): Argument #1 ($object) must be provided for instance properties",
        ));
    };
    match info {
        Some(pi) => {
            let v = object::data(&o).slots.borrow().get(pi.slot as usize).cloned().flatten();
            match v {
                Some(v) => Ok(v.deref()),
                None => {
                    if pi.ty.is_some() {
                        return Err(vm.make_throw(
                            "Error",
                            &format!("Typed property {}::${} must not be accessed before initialization", pi.class, pi.name),
                        ));
                    }
                    vm.read_property(&o, &n, crate::opcode::FetchMode::Read)
                }
            }
        }
        None => vm.read_property(&o, &n, crate::opcode::FetchMode::Read),
    }
}

fn rprop_set_value(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, n, info, is_static) = this_prop(vm, a)?;
    if is_static {
        let v = if a.has(1) { a.at(1) } else { a.at(0) };
        let cell = vm.static_prop_cell(&c, &n)?;
        let mut b = cell.borrow_mut();
        match &mut *b {
            Value::Reference(r) => {
                r.set(v);
            }
            slot => *slot = v,
        }
        return Ok(Value::Null);
    }
    let Value::Object(o) = a.at(0) else {
        return Ok(Value::Null);
    };
    let v = a.at(1);
    match info {
        Some(pi) => {
            if pi.is_readonly() && object::data(&o).slots.borrow()[pi.slot as usize].is_some() {
                return Err(vm.make_throw(
                    "Error",
                    &format!("Cannot modify readonly property {}::${}", object::class_of(&o).name, pi.name),
                ));
            }
            let v = vm.coerce_prop(Some(&o), &pi, v)?;
            let d = object::data(&o);
            let mut slots = d.slots.borrow_mut();
            match &mut slots[pi.slot as usize] {
                Some(Value::Reference(r)) => {
                    r.set(v);
                }
                slot => *slot = Some(v),
            }
            Ok(Value::Null)
        }
        None => {
            vm.write_dynamic_prop(&o, n, v);
            Ok(Value::Null)
        }
    }
}

fn rprop_is_initialized(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, n, info, is_static) = this_prop(vm, a)?;
    if is_static {
        return Ok(Value::Bool(true));
    }
    let Value::Object(o) = a.at(0) else { return Ok(Value::Bool(false)) };
    Ok(Value::Bool(match info {
        Some(pi) => object::data(&o).slots.borrow()[pi.slot as usize].is_some(),
        None => object::data(&o)
            .dynamic
            .borrow()
            .as_ref()
            .is_some_and(|t| t.contains_key(&ArrayKey::Str(n))),
    }))
}

macro_rules! rprop_bool {
    ($name:ident, |$f:ident| $e:expr) => {
        fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let (c, n, info, is_static) = this_prop(vm, a)?;
            let $f = prop_flags(&c, &n, info.as_ref(), is_static);
            Ok(Value::Bool($e))
        }
    };
}

rprop_bool!(rprop_is_public, |f| f & flags::PUBLIC != 0);
rprop_bool!(rprop_is_private, |f| f & flags::PRIVATE != 0);
rprop_bool!(rprop_is_protected, |f| f & flags::PROTECTED != 0);
rprop_bool!(rprop_is_static, |f| f & flags::STATIC != 0);
rprop_bool!(rprop_is_readonly, |f| f & flags::READONLY != 0);

fn rprop_get_modifiers(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, n, info, is_static) = this_prop(vm, a)?;
    Ok(Value::Int(modifiers_of(prop_flags(&c, &n, info.as_ref(), is_static))))
}

fn rprop_is_default(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, _, info, is_static) = this_prop(vm, a)?;
    Ok(Value::Bool(info.is_some() || is_static))
}

fn rprop_is_promoted(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, n, info, _) = this_prop(vm, a)?;
    let Some(pi) = info else { return Ok(Value::Bool(false)) };
    let decl = vm.find_class(pi.class.as_bytes()).unwrap_or(c);
    Ok(Value::Bool(decl.magic.construct.as_ref().is_some_and(|m| m.func.params().iter().any(|p| p.name == n && p.promote != 0))))
}

fn rprop_get_type(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, n, info, is_static) = this_prop(vm, a)?;
    let ty = if is_static { c.find_static(n.as_bytes()).and_then(|s| s.ty.clone()) } else { info.and_then(|p| p.ty.clone()) };
    match ty {
        Some(t) => vm.refl_type_value(&t, Some(&c)),
        None => Ok(Value::Null),
    }
}

fn rprop_has_type(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, n, info, is_static) = this_prop(vm, a)?;
    Ok(Value::Bool(if is_static { c.find_static(n.as_bytes()).is_some_and(|s| s.ty.is_some()) } else { info.is_some_and(|p| p.ty.is_some()) }))
}

fn rprop_has_default(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, _, info, is_static) = this_prop(vm, a)?;
    Ok(Value::Bool(is_static || info.is_some_and(|p| p.default.is_some())))
}

fn rprop_get_default(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, _, info, _) = this_prop(vm, a)?;
    match info.and_then(|p| p.default.clone()) {
        Some(e) => vm.eval_const_expr(&e, Some(&c)),
        None => Ok(Value::Null),
    }
}

fn rprop_get_doc_comment(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, _, info, _) = this_prop(vm, a)?;
    Ok(info.and_then(|p| p.doc_comment.clone()).map_or(Value::Bool(false), Value::Str))
}

fn rprop_get_declaring_class(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, _, info, _) = this_prop(vm, a)?;
    let d = info.and_then(|p| vm.find_class(p.class.as_bytes())).unwrap_or(c);
    vm.refl_class_value(&d)
}

fn rprop_get_attributes(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, _, info, _) = this_prop(vm, a)?;
    let attrs = info.map(|p| p.attributes.clone()).unwrap_or_default();
    vm.attributes_array(&attrs, target::PROPERTY, Some(&c), a)
}

// ---------------------------------------------------------------------
// Class constants and enum cases
// ---------------------------------------------------------------------

fn this_const(vm: &mut Vm, a: &Args) -> VmResult<(Rc<Class>, Rc<ClassConst>)> {
    match this_refl(vm, a)? {
        Refl::Const { class, c } => Ok((class, c)),
        _ => Err(vm.make_throw("Error", "Not a constant")),
    }
}

fn rcc_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let c = class_from_arg(vm, &a.at(0))?;
    let n = a.at(1).as_str().cloned().unwrap_or_default();
    let Some(cc) = c.find_const(n.as_bytes()).cloned() else {
        return Err(vm.refl_exc(&format!("Constant {}::{} does not exist", c.name, n.to_string_lossy())));
    };
    let c2 = object::class_of(&o).clone();
    if let Some(pi) = c2.prop_map.get(&ZStr::from("name")) {
        object::data(&o).slots.borrow_mut()[pi.slot as usize] = Some(Value::Str(n.clone()));
    }
    if let Some(pi) = c2.prop_map.get(&ZStr::from("class")) {
        object::data(&o).slots.borrow_mut()[pi.slot as usize] = Some(Value::Str(cc.class.clone()));
    }
    object::set_internal(&o, Refl::Const { class: c, c: cc });
    Ok(Value::Null)
}

fn rcc_get_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Str(this_const(vm, a)?.1.name.clone()))
}
fn rcc_get_value(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, cc) = this_const(vm, a)?;
    vm.class_constant(&c, &cc.name)
}
fn rcc_get_declaring_class(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, cc) = this_const(vm, a)?;
    let d = vm.find_class(cc.class.as_bytes()).unwrap_or(c);
    vm.refl_class_value(&d)
}
fn rcc_get_modifiers(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(modifiers_of(this_const(vm, a)?.1.flags)))
}
fn rcc_is_public(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_const(vm, a)?.1.flags & flags::PUBLIC != 0))
}
fn rcc_is_private(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_const(vm, a)?.1.flags & flags::PRIVATE != 0))
}
fn rcc_is_protected(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_const(vm, a)?.1.flags & flags::PROTECTED != 0))
}
fn rcc_is_final(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_const(vm, a)?.1.flags & flags::FINAL != 0))
}
fn rcc_is_enum_case(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_const(vm, a)?.1.is_case))
}
fn rcc_get_attributes(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, cc) = this_const(vm, a)?;
    let attrs = cc.attributes.clone();
    vm.attributes_array(&attrs, target::CLASS_CONSTANT, Some(&c), a)
}
fn rcc_get_doc_comment(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(this_const(vm, a)?.1.doc_comment.clone().map_or(Value::Bool(false), Value::Str))
}
fn rcc_get_type(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, cc) = this_const(vm, a)?;
    match cc.ty.clone() {
        Some(t) => vm.refl_type_value(&t, Some(&c)),
        None => Ok(Value::Null),
    }
}
fn rcc_has_type(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_const(vm, a)?.1.ty.is_some()))
}
fn rcase_get_enum(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, _) = this_const(vm, a)?;
    vm.refl_class_value(&c)
}
fn rcase_get_backing_value(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, cc) = this_const(vm, a)?;
    let v = vm.class_constant(&c, &cc.name)?;
    if let Value::Object(o) = v {
        let cls = object::class_of(&o).clone();
        if let Some(pi) = cls.prop_map.get(&ZStr::from("value")) {
            return Ok(object::data(&o).slots.borrow()[pi.slot as usize].clone().unwrap_or_default());
        }
    }
    Ok(Value::Null)
}

// ---------------------------------------------------------------------
// Attributes
// ---------------------------------------------------------------------

fn this_attr(vm: &mut Vm, a: &Args) -> VmResult<(AttributeInfo, i64, bool, Option<Rc<Class>>)> {
    match this_refl(vm, a)? {
        Refl::Attr { info, target, repeated, scope } => Ok((info, target, repeated, scope)),
        _ => Err(vm.make_throw("Error", "Not an attribute")),
    }
}

fn ra_get_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Str(this_attr(vm, a)?.0.name))
}
fn ra_get_target(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(this_attr(vm, a)?.1))
}
fn ra_is_repeated(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_attr(vm, a)?.2))
}

fn attr_args(vm: &mut Vm, info: &AttributeInfo, scope: Option<&Rc<Class>>) -> VmResult<(Vec<Value>, Vec<(ZStr, Value)>)> {
    let mut pos = Vec::new();
    let mut named = Vec::new();
    for (n, e) in &info.args {
        let v = vm.eval_const_expr(e, scope)?;
        match n {
            Some(n) => named.push((n.clone(), v)),
            None => pos.push(v),
        }
    }
    Ok((pos, named))
}

fn ra_get_arguments(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (info, _, _, scope) = this_attr(vm, a)?;
    let (pos, named) = attr_args(vm, &info, scope.as_ref())?;
    let mut t = HashTable::new();
    for v in pos {
        let _ = t.push(v);
    }
    for (k, v) in named {
        t.insert(ArrayKey::Str(k), v);
    }
    Ok(Value::Array(ZArray::from_table(t)))
}

fn target_name(t: i64) -> &'static str {
    match t {
        target::CLASS => "class",
        target::FUNCTION => "function",
        target::METHOD => "method",
        target::PROPERTY => "property",
        target::CLASS_CONSTANT => "class constant",
        target::PARAMETER => "parameter",
        _ => "constant",
    }
}

fn allowed_targets(f: i64) -> String {
    let mut v = Vec::new();
    for t in [target::CLASS, target::FUNCTION, target::METHOD, target::PROPERTY, target::CLASS_CONSTANT, target::PARAMETER, target::CONSTANT] {
        if f & t != 0 {
            v.push(target_name(t));
        }
    }
    v.join(", ")
}

fn ra_new_instance(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (info, target, repeated, scope) = this_attr(vm, a)?;
    let Some(c) = vm.lookup_class(&info.name)? else {
        return Err(vm.make_throw("Error", &format!("Attribute class \"{}\" not found", info.name)));
    };
    // The attribute class must itself carry #[Attribute].
    let flags_v = if c.lc_name.as_bytes() == b"attribute" {
        Some(1)
    } else if c.flags & cflags::INTERNAL != 0 {
        match c.lc_name.as_bytes() {
            b"returntypewillchange" | b"override" => Some(target::METHOD),
            b"allowdynamicproperties" => Some(target::CLASS),
            b"sensitiveparameter" => Some(target::PARAMETER),
            b"deprecated" | b"nodiscard" => Some(target::FUNCTION | target::METHOD | target::CLASS_CONSTANT | target::CONSTANT | 127),
            _ => None,
        }
    } else {
        match c.attributes.iter().find(|x| x.name.eq_ignore_ascii_case(b"Attribute")) {
            Some(at) => {
                let (pos, _) = attr_args(vm, at, Some(&c))?;
                Some(pos.first().and_then(Value::as_int).unwrap_or(127))
            }
            None => None,
        }
    };
    let Some(fl) = flags_v else {
        return Err(vm.make_throw(
            "Error",
            &format!("Attempting to use non-attribute class \"{}\" as attribute", c.name),
        ));
    };
    if fl & target == 0 {
        return Err(vm.make_throw(
            "Error",
            &format!(
                "Attribute \"{}\" cannot target {} (allowed targets: {})",
                info.name,
                target_name(target),
                allowed_targets(fl)
            ),
        ));
    }
    if repeated && fl & 128 == 0 {
        return Err(vm.make_throw("Error", &format!("Attribute \"{}\" must not be repeated", info.name)));
    }
    let (pos, named) = attr_args(vm, &info, scope.as_ref())?;
    new_instance(vm, &c, pos, named)
}

fn reflection_get_modifier_names(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let m = a.at(0).as_int().unwrap_or(0);
    let mut out = Vec::new();
    if m & 64 != 0 {
        out.push(zs("abstract"));
    }
    if m & 32 != 0 {
        out.push(zs("final"));
    }
    if m & 1 != 0 {
        out.push(zs("public"));
    } else if m & 2 != 0 {
        out.push(zs("protected"));
    } else if m & 4 != 0 {
        out.push(zs("private"));
    }
    if m & 16 != 0 {
        out.push(zs("static"));
    }
    if m & 128 != 0 || m & 65536 != 0 {
        out.push(zs("readonly"));
    }
    Ok(Value::Array(ZArray::from_values(out)))
}

fn ro_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    rc_construct(vm, a)
}

/// Registers the Reflection classes.
#[allow(clippy::too_many_lines)]
pub(crate) fn register(vm: &mut Vm) {
    ClassBuilder::new("ReflectionException").extends(vm, "Exception").register(vm);
    ClassBuilder::new("Reflection")
        .method("getModifierNames(int $modifiers): array", STAT, reflection_get_modifier_names)
        .register(vm);
    ClassBuilder::new("Reflector").flags(cflags::INTERFACE).implements(vm, &["Stringable"]).register(vm);
    ClassBuilder::new("ReflectionClass")
        .implements(vm, &["Reflector"])
        .prop("name", PUB, Some("string"), None)
        .constant("IS_IMPLICIT_ABSTRACT", Value::Int(16))
        .constant("IS_EXPLICIT_ABSTRACT", Value::Int(64))
        .constant("IS_FINAL", Value::Int(32))
        .constant("IS_READONLY", Value::Int(65536))
        .constant("SKIP_INITIALIZATION_ON_SERIALIZE", Value::Int(8))
        .constant("SKIP_DESTRUCTOR", Value::Int(16))
        .method("__construct(object|string $objectOrClass)", PUB, rc_construct)
        .method("getName(): string", PUB, rc_get_name)
        .method("getShortName(): string", PUB, rc_get_short_name)
        .method("getNamespaceName(): string", PUB, rc_get_ns)
        .method("inNamespace(): bool", PUB, rc_in_ns)
        .method("isInterface(): bool", PUB, rc_is_interface)
        .method("isTrait(): bool", PUB, rc_is_trait)
        .method("isEnum(): bool", PUB, rc_is_enum)
        .method("isAbstract(): bool", PUB, rc_is_abstract)
        .method("isFinal(): bool", PUB, rc_is_final)
        .method("isReadOnly(): bool", PUB, rc_is_readonly)
        .method("isAnonymous(): bool", PUB, rc_is_anon)
        .method("isInternal(): bool", PUB, rc_is_internal)
        .method("isUserDefined(): bool", PUB, rc_is_user)
        .method("isInstantiable(): bool", PUB, rc_is_instantiable)
        .method("isCloneable(): bool", PUB, rc_is_cloneable)
        .method("isIterable(): bool", PUB, rc_is_iterable)
        .method("isIterateable(): bool", PUB, rc_is_iterable)
        .method("isInstance(object $object): bool", PUB, rc_is_instance)
        .method("isSubclassOf(ReflectionClass|string $class): bool", PUB, rc_is_subclass_of)
        .method("implementsInterface(ReflectionClass|string $interface): bool", PUB, rc_implements)
        .method("getParentClass(): ReflectionClass|false", PUB, rc_get_parent)
        .method("getInterfaceNames(): array", PUB, rc_get_interface_names)
        .method("getInterfaces(): array", PUB, rc_get_interfaces)
        .method("getTraitNames(): array", PUB, rc_get_trait_names)
        .method("getTraits(): array", PUB, rc_get_traits)
        .method("getMethods(?int $filter = null): array", PUB, rc_get_methods)
        .method("getMethod(string $name): ReflectionMethod", PUB, rc_get_method)
        .method("hasMethod(string $name): bool", PUB, rc_has_method)
        .method("getConstructor(): ?ReflectionMethod", PUB, rc_get_constructor)
        .method("getProperties(?int $filter = null): array", PUB, rc_get_properties)
        .method("getProperty(string $name): ReflectionProperty", PUB, rc_get_property)
        .method("hasProperty(string $name): bool", PUB, rc_has_property)
        .method("getConstants(?int $filter = null): array", PUB, rc_get_constants)
        .method("getConstant(string $name): mixed", PUB, rc_get_constant)
        .method("hasConstant(string $name): bool", PUB, rc_has_constant)
        .method("getReflectionConstants(?int $filter = null): array", PUB, rc_get_reflection_constants)
        .method("getReflectionConstant(string $name): ReflectionClassConstant|false", PUB, rc_get_reflection_constant)
        .method("newInstance(mixed ...$args): object", PUB, rc_new_instance)
        .method("newInstanceArgs(array $args = []): ?object", PUB, rc_new_instance_args)
        .method("newInstanceWithoutConstructor(): object", PUB, rc_new_instance_without_ctor)
        .method("getAttributes(?string $name = null, int $flags = 0): array", PUB, rc_get_attributes)
        .method("getDocComment(): string|false", PUB, rc_get_doc_comment)
        .method("getFileName(): string|false", PUB, rc_get_file_name)
        .method("getStartLine(): int|false", PUB, rc_get_start_line)
        .method("getEndLine(): int|false", PUB, rc_get_end_line)
        .method("getStaticPropertyValue(string $name, mixed $default = UNKNOWN): mixed", PUB, rc_get_static_property_value)
        .method("setStaticPropertyValue(string $name, mixed $value): void", PUB, rc_set_static_property_value)
        .method("getStaticProperties(): array", PUB, rc_get_static_properties)
        .method("getDefaultProperties(): array", PUB, rc_get_default_properties)
        .method("getModifiers(): int", PUB, rc_get_modifiers)
        .method("getExtensionName(): string|false", PUB, rc_get_extension_name)
        .method("__toString(): string", PUB, rc_to_string)
        .register(vm);
    ClassBuilder::new("ReflectionObject")
        .extends(vm, "ReflectionClass")
        .method("__construct(object $object)", PUB, ro_construct)
        .register(vm);
    ClassBuilder::new("ReflectionEnum")
        .extends(vm, "ReflectionClass")
        .method("__construct(object|string $objectOrClass)", PUB, re_construct)
        .method("getCases(): array", PUB, re_get_cases)
        .method("getCase(string $name): ReflectionEnumUnitCase", PUB, re_get_case)
        .method("hasCase(string $name): bool", PUB, re_has_case)
        .method("isBacked(): bool", PUB, re_is_backed)
        .method("getBackingType(): ?ReflectionNamedType", PUB, re_get_backing_type)
        .register(vm);
    ClassBuilder::new("ReflectionFunctionAbstract")
        .flags(cflags::ABSTRACT)
        .implements(vm, &["Reflector"])
        .prop("name", PUB, Some("string"), None)
        .method("getName(): string", PUB, rf_get_name)
        .method("getShortName(): string", PUB, rf_get_short_name)
        .method("getNamespaceName(): string", PUB, rf_get_ns)
        .method("inNamespace(): bool", PUB, rf_in_ns)
        .method("getParameters(): array", PUB, rf_get_parameters)
        .method("getNumberOfParameters(): int", PUB, rf_num_params)
        .method("getNumberOfRequiredParameters(): int", PUB, rf_num_required)
        .method("getReturnType(): ?ReflectionType", PUB, rf_get_return_type)
        .method("hasReturnType(): bool", PUB, rf_has_return_type)
        .method("isVariadic(): bool", PUB, rf_is_variadic)
        .method("returnsReference(): bool", PUB, rf_returns_ref)
        .method("isGenerator(): bool", PUB, rf_is_generator)
        .method("isClosure(): bool", PUB, rf_is_closure)
        .method("isInternal(): bool", PUB, rf_is_internal)
        .method("isUserDefined(): bool", PUB, rf_is_user)
        .method("isDeprecated(): bool", PUB, rf_is_deprecated)
        .method("isStatic(): bool", PUB, rf_is_static)
        .method("getDocComment(): string|false", PUB, rf_get_doc_comment)
        .method("getStartLine(): int|false", PUB, rf_get_start_line)
        .method("getEndLine(): int|false", PUB, rf_get_end_line)
        .method("getFileName(): string|false", PUB, rf_get_file_name)
        .method("getAttributes(?string $name = null, int $flags = 0): array", PUB, rf_get_attributes)
        .method("getStaticVariables(): array", PUB, rf_get_static_variables)
        .method("getClosureThis(): ?object", PUB, rf_get_closure_this)
        .method("getClosureScopeClass(): ?ReflectionClass", PUB, rf_get_closure_scope_class)
        .method("getClosureCalledClass(): ?ReflectionClass", PUB, rf_get_closure_scope_class)
        .method("getClosureUsedVariables(): array", PUB, rf_get_closure_used_variables)
        .register(vm);
    ClassBuilder::new("ReflectionFunction")
        .extends(vm, "ReflectionFunctionAbstract")
        .method("__construct(Closure|string $function)", PUB, rf_construct)
        .method("invoke(mixed ...$args): mixed", PUB, rf_invoke)
        .method("invokeArgs(array $args = []): mixed", PUB, rf_invoke_args)
        .method("getClosure(): Closure", PUB, rf_get_closure)
        .method("isAnonymous(): bool", PUB, rf_is_anonymous)
        .method("isDisabled(): bool", PUB, |_, _| Ok(Value::Bool(false)))
        .register(vm);
    ClassBuilder::new("ReflectionMethod")
        .extends(vm, "ReflectionFunctionAbstract")
        .prop("class", PUB, Some("string"), None)
        .constant("IS_STATIC", Value::Int(16))
        .constant("IS_PUBLIC", Value::Int(1))
        .constant("IS_PROTECTED", Value::Int(2))
        .constant("IS_PRIVATE", Value::Int(4))
        .constant("IS_ABSTRACT", Value::Int(64))
        .constant("IS_FINAL", Value::Int(32))
        .method("__construct(object|string $objectOrMethod, ?string $method = null)", PUB, rm_construct)
        .method("isPublic(): bool", PUB, rm_is_public)
        .method("isPrivate(): bool", PUB, rm_is_private)
        .method("isProtected(): bool", PUB, rm_is_protected)
        .method("isAbstract(): bool", PUB, rm_is_abstract)
        .method("isFinal(): bool", PUB, rm_is_final)
        .method("isStatic(): bool", PUB, rm_is_static)
        .method("isConstructor(): bool", PUB, rm_is_ctor)
        .method("isDestructor(): bool", PUB, rm_is_dtor)
        .method("getDeclaringClass(): ReflectionClass", PUB, rm_get_declaring_class)
        .method("getModifiers(): int", PUB, rm_get_modifiers)
        .method("invoke(?object $object, mixed ...$args): mixed", PUB, rm_invoke)
        .method("invokeArgs(?object $object, array $args = []): mixed", PUB, rm_invoke_args)
        .method("setAccessible(bool $accessible): void", PUB, ret_null)
        .method("getClosure(?object $object = null): Closure", PUB, rf_get_closure)
        .method("getPrototype(): ReflectionMethod", PUB, rm_get_prototype)
        .register(vm);
    ClassBuilder::new("ReflectionParameter")
        .implements(vm, &["Reflector"])
        .prop("name", PUB, Some("string"), None)
        .method("getName(): string", PUB, rp_get_name)
        .method("getPosition(): int", PUB, rp_get_position)
        .method("isOptional(): bool", PUB, rp_is_optional)
        .method("isDefaultValueAvailable(): bool", PUB, rp_has_default)
        .method("getDefaultValue(): mixed", PUB, rp_get_default)
        .method("isDefaultValueConstant(): bool", PUB, rp_is_default_constant)
        .method("getDefaultValueConstantName(): ?string", PUB, rp_get_default_constant_name)
        .method("hasType(): bool", PUB, rp_has_type)
        .method("getType(): ?ReflectionType", PUB, rp_get_type)
        .method("allowsNull(): bool", PUB, rp_allows_null)
        .method("isPassedByReference(): bool", PUB, rp_by_ref)
        .method("canBePassedByValue(): bool", PUB, rp_by_val)
        .method("isVariadic(): bool", PUB, rp_is_variadic)
        .method("isPromoted(): bool", PUB, rp_is_promoted)
        .method("getAttributes(?string $name = null, int $flags = 0): array", PUB, rp_get_attributes)
        .method("getDeclaringFunction(): ReflectionFunctionAbstract", PUB, rp_get_declaring_function)
        .method("getDeclaringClass(): ?ReflectionClass", PUB, rp_get_declaring_class)
        .register(vm);
    ClassBuilder::new("ReflectionType")
        .flags(cflags::ABSTRACT)
        .implements(vm, &["Stringable"])
        .method("allowsNull(): bool", PUB, rt_allows_null)
        .method("__toString(): string", PUB, rt_to_string)
        .register(vm);
    ClassBuilder::new("ReflectionNamedType")
        .extends(vm, "ReflectionType")
        .method("getName(): string", PUB, rt_get_name)
        .method("isBuiltin(): bool", PUB, rt_is_builtin)
        .register(vm);
    ClassBuilder::new("ReflectionUnionType")
        .extends(vm, "ReflectionType")
        .method("getTypes(): array", PUB, rt_get_types)
        .register(vm);
    ClassBuilder::new("ReflectionIntersectionType")
        .extends(vm, "ReflectionType")
        .method("getTypes(): array", PUB, rt_get_types)
        .register(vm);
    ClassBuilder::new("ReflectionProperty")
        .implements(vm, &["Reflector"])
        .prop("name", PUB, Some("string"), None)
        .prop("class", PUB, Some("string"), None)
        .constant("IS_STATIC", Value::Int(16))
        .constant("IS_READONLY", Value::Int(128))
        .constant("IS_PUBLIC", Value::Int(1))
        .constant("IS_PROTECTED", Value::Int(2))
        .constant("IS_PRIVATE", Value::Int(4))
        .constant("IS_ABSTRACT", Value::Int(64))
        .constant("IS_FINAL", Value::Int(32))
        .method("__construct(object|string $class, string $property)", PUB, rprop_construct)
        .method("getName(): string", PUB, rprop_get_name)
        .method("getValue(?object $object = null): mixed", PUB, rprop_get_value)
        .method("setValue(mixed $objectOrValue, mixed $value = UNKNOWN): void", PUB, rprop_set_value)
        .method("isInitialized(?object $object = null): bool", PUB, rprop_is_initialized)
        .method("isPublic(): bool", PUB, rprop_is_public)
        .method("isPrivate(): bool", PUB, rprop_is_private)
        .method("isProtected(): bool", PUB, rprop_is_protected)
        .method("isStatic(): bool", PUB, rprop_is_static)
        .method("isReadOnly(): bool", PUB, rprop_is_readonly)
        .method("isDefault(): bool", PUB, rprop_is_default)
        .method("isPromoted(): bool", PUB, rprop_is_promoted)
        .method("getModifiers(): int", PUB, rprop_get_modifiers)
        .method("getDeclaringClass(): ReflectionClass", PUB, rprop_get_declaring_class)
        .method("getDocComment(): string|false", PUB, rprop_get_doc_comment)
        .method("setAccessible(bool $accessible): void", PUB, ret_null)
        .method("getType(): ?ReflectionType", PUB, rprop_get_type)
        .method("hasType(): bool", PUB, rprop_has_type)
        .method("hasDefaultValue(): bool", PUB, rprop_has_default)
        .method("getDefaultValue(): mixed", PUB, rprop_get_default)
        .method("getAttributes(?string $name = null, int $flags = 0): array", PUB, rprop_get_attributes)
        .register(vm);
    ClassBuilder::new("ReflectionClassConstant")
        .implements(vm, &["Reflector"])
        .prop("name", PUB, Some("string"), None)
        .prop("class", PUB, Some("string"), None)
        .constant("IS_PUBLIC", Value::Int(1))
        .constant("IS_PROTECTED", Value::Int(2))
        .constant("IS_PRIVATE", Value::Int(4))
        .constant("IS_FINAL", Value::Int(32))
        .method("__construct(object|string $class, string $constant)", PUB, rcc_construct)
        .method("getName(): string", PUB, rcc_get_name)
        .method("getValue(): mixed", PUB, rcc_get_value)
        .method("getDeclaringClass(): ReflectionClass", PUB, rcc_get_declaring_class)
        .method("getModifiers(): int", PUB, rcc_get_modifiers)
        .method("isPublic(): bool", PUB, rcc_is_public)
        .method("isPrivate(): bool", PUB, rcc_is_private)
        .method("isProtected(): bool", PUB, rcc_is_protected)
        .method("isFinal(): bool", PUB, rcc_is_final)
        .method("isEnumCase(): bool", PUB, rcc_is_enum_case)
        .method("getAttributes(?string $name = null, int $flags = 0): array", PUB, rcc_get_attributes)
        .method("getDocComment(): string|false", PUB, rcc_get_doc_comment)
        .method("getType(): ?ReflectionType", PUB, rcc_get_type)
        .method("hasType(): bool", PUB, rcc_has_type)
        .register(vm);
    ClassBuilder::new("ReflectionEnumUnitCase")
        .extends(vm, "ReflectionClassConstant")
        .method("getEnum(): ReflectionEnum", PUB, rcase_get_enum)
        .register(vm);
    ClassBuilder::new("ReflectionEnumBackedCase")
        .extends(vm, "ReflectionEnumUnitCase")
        .method("getBackingValue(): int|string", PUB, rcase_get_backing_value)
        .register(vm);
    ClassBuilder::new("ReflectionAttribute")
        .implements(vm, &["Reflector"])
        .constant("IS_INSTANCEOF", Value::Int(2))
        .method("getName(): string", PUB, ra_get_name)
        .method("getTarget(): int", PUB, ra_get_target)
        .method("isRepeated(): bool", PUB, ra_is_repeated)
        .method("getArguments(): array", PUB, ra_get_arguments)
        .method("newInstance(): object", PUB, ra_new_instance)
        .register(vm);
}

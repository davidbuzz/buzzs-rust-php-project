// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The Reflection API (`ext/reflection/php_reflection.c`).
//!
//! Every reflection object keeps its target in a [`Refl`] value stored as
//! the object's internal state (`intern->ptr` in php-src). The classes are
//! split over submodules:
//!
//! * [`class`] — `ReflectionClass`, `ReflectionObject`, `ReflectionEnum`
//! * [`function`] — `ReflectionFunctionAbstract`, `ReflectionFunction`,
//!   `ReflectionMethod`
//! * [`param`] — `ReflectionParameter`
//! * [`types`] — `ReflectionType` and its subclasses
//! * [`property`] — `ReflectionProperty`
//! * [`constant`] — `ReflectionClassConstant`, enum cases,
//!   `ReflectionConstant`
//! * [`attribute`] — `ReflectionAttribute`
//! * [`misc`] — `Reflection`, `ReflectionGenerator`, `ReflectionFiber`,
//!   `ReflectionReference`, `ReflectionExtension`,
//!   `ReflectionZendExtension`, `PropertyHookType`
//! * [`lazy`] — lazy objects (`newLazyGhost()` & co)
//! * [`export`] — the `__toString()` renderers
//! * [`order`] — php-src hash table orders of class members

// Builtin handlers all share the `BuiltinFn` signature (`vm`, `a`), and
// some never fail; the class tables use `ZStr` keys like the rest of the VM.
#![allow(
    clippy::many_single_char_names,
    clippy::unnecessary_wraps,
    clippy::mutable_key_type,
    clippy::single_match_else,
    clippy::type_complexity,
    clippy::too_many_lines,
    clippy::option_option,
    clippy::needless_pass_by_value,
    clippy::match_same_arms,
    clippy::collapsible_match
)]

mod attribute;
pub(crate) mod class;
mod constant;
mod export;
mod function;
pub(crate) mod lazy;
pub(crate) mod misc;
mod order;
mod param;
mod property;
mod source;
mod types;

use crate::class::{AttributeInfo, Class, ClassConst};
use crate::func::{flags, Args, Builtin, Func, Method};
use crate::object;
use crate::opcode::ParamInfo;
use crate::types::TypeDecl;
use crate::vm::{Throw, Vm, VmResult};
use php_runtime::{Obj, PhpRef, Value, ZStr};
use std::rc::Rc;

pub(crate) use misc::record_module;

const PUB: u32 = flags::PUBLIC;
const STAT: u32 = flags::PUBLIC | flags::STATIC;
const PRIV: u32 = flags::PRIVATE;
const RO: u32 = flags::PUBLIC | flags::READONLY;

/// `Attribute::TARGET_*`.
pub(crate) mod target {
    pub const CLASS: i64 = 1;
    pub const FUNCTION: i64 = 2;
    pub const METHOD: i64 = 4;
    pub const PROPERTY: i64 = 8;
    pub const CLASS_CONSTANT: i64 = 16;
    pub const PARAMETER: i64 = 32;
    pub const CONSTANT: i64 = 64;
    pub const ALL: i64 = 127;
    pub const IS_REPEATABLE: i64 = 128;
}

/// php-src `ZEND_ACC_*` values as exposed by `getModifiers()`.
pub(crate) mod acc {
    pub const PUBLIC: i64 = 1;
    pub const PROTECTED: i64 = 2;
    pub const PRIVATE: i64 = 4;
    pub const STATIC: i64 = 16;
    pub const FINAL: i64 = 32;
    pub const ABSTRACT: i64 = 64;
    pub const READONLY: i64 = 128;
    pub const VIRTUAL: i64 = 512;
    pub const PROTECTED_SET: i64 = 2048;
    pub const PRIVATE_SET: i64 = 4096;
    pub const DEPRECATED: i64 = 2048;
    pub const EXPLICIT_ABSTRACT_CLASS: i64 = 64;
    pub const READONLY_CLASS: i64 = 65536;
}

/// A reflected function or method (`zend_function` + `intern->ce` +
/// `intern->obj`).
#[derive(Clone)]
pub(crate) struct FnRef {
    /// The function body.
    pub func: Func,
    /// The method entry, for methods (its name may be a trait alias).
    pub method: Option<Rc<Method>>,
    /// The reflected class (`intern->ce`) for methods.
    pub class: Option<Rc<Class>>,
    /// The closure object (`intern->obj`).
    pub closure: Option<Obj>,
}

impl FnRef {
    /// A plain function.
    pub fn function(func: Func) -> Self {
        FnRef {
            func,
            method: None,
            class: None,
            closure: None,
        }
    }

    /// A method of `class`.
    pub fn method(m: &Rc<Method>, class: &Rc<Class>) -> Self {
        FnRef {
            func: m.func.clone(),
            method: Some(m.clone()),
            class: Some(class.clone()),
            closure: None,
        }
    }

    /// The function of closure object `c` (with data `d`). A fake closure
    /// of a `__call`/`__callStatic` trampoline reflects as the internal
    /// function of `zend_closure_from_frame`: named after the called
    /// method, with `zend_call_trampoline_arginfo` (`mixed ...$arguments`).
    pub fn of_closure(c: &Obj, d: &crate::closure::ClosureData) -> Self {
        let func = match &d.magic_name {
            Some(name) => Func::Builtin(Rc::new(Builtin::new(
                name.clone(),
                crate::func::Signature {
                    params: vec![ParamInfo {
                        name: ZStr::from("arguments"),
                        by_ref: false,
                        variadic: true,
                        ty: Some(TypeDecl::mixed()),
                        has_default: false,
                        default_text: None,
                        promote: 0,
                        sensitive: false,
                        default_expr: None,
                        attributes: Vec::new(),
                    }],
                    defaults: Vec::new(),
                    required: 0,
                    return_type: None,
                },
                flags::PUBLIC
                    | flags::VARIADIC
                    | (d.func.flags() & (flags::STATIC | flags::RETURN_REF | flags::DEPRECATED)),
                reflected_trampoline,
            ))),
            None => d.func.clone(),
        };
        FnRef {
            func,
            method: None,
            class: None,
            closure: Some(c.clone()),
        }
    }

    /// `Closure::__invoke` of a closure object: an internal trampoline
    /// with the closure's (userland) arginfo (`ZEND_ACC_USER_ARG_INFO`).
    pub fn is_trampoline(&self) -> bool {
        self.func.is_user()
            && self.method.as_ref().is_some_and(|m| {
                m.name.as_bytes() == b"__invoke"
                    && m.scope
                        .upgrade()
                        .is_some_and(|c| c.lc_name.as_bytes() == b"closure")
            })
    }

    /// Whether this is user code (`ZEND_USER_FUNCTION`).
    pub fn is_user(&self) -> bool {
        self.func.is_user() && !self.is_trampoline()
    }

    /// The function name (`common.function_name`).
    pub fn name(&self) -> ZStr {
        match &self.method {
            Some(m) => m.name.clone(),
            None => self.func.name().clone(),
        }
    }

    /// The function flags, with method modifiers.
    pub fn flags(&self) -> u32 {
        match &self.method {
            Some(m) => {
                m.flags
                    | (self.func.flags()
                        & !(flags::VISIBILITY | flags::STATIC | flags::FINAL | flags::ABSTRACT))
            }
            None => self.func.flags(),
        }
    }

    /// The declaring class (`common.scope`).
    pub fn scope(&self, vm: &Vm) -> Option<Rc<Class>> {
        if let Some(m) = &self.method {
            return Some(m.scope());
        }
        if let Some(c) = &self.closure {
            if let Some(d) = vm.closure_data(c) {
                return d.scope;
            }
        }
        if let Func::User(u) = &self.func {
            if let Some(cn) = &u.class_name {
                return vm.find_class(cn.as_bytes());
            }
        }
        None
    }
}

/// The handler of a reflected `__call` trampoline: calls go through the
/// closure object (`FnRef::closure`), never here.
fn reflected_trampoline(_: &mut Vm, _: &mut Args) -> VmResult<Value> {
    Ok(Value::Null)
}

/// A reflected property (`property_reference`).
#[derive(Clone)]
pub(crate) struct PropRef {
    /// The reflected class (`intern->ce`).
    pub class: Rc<Class>,
    /// The (unmangled) property name.
    pub name: ZStr,
    /// The declaration; `None` for dynamic properties.
    pub meta: Option<Rc<order::PropMeta>>,
}

/// A reflected attribute (`attribute_reference`).
#[derive(Clone)]
pub(crate) struct AttrRef {
    /// The attribute.
    pub info: AttributeInfo,
    /// `Attribute::TARGET_*` of the declaration.
    pub target: i64,
    /// Whether the attribute is repeated on the declaration.
    pub repeated: bool,
    /// Scope for evaluating the arguments.
    pub scope: Option<Rc<Class>>,
    /// The file declaring the attribute.
    pub file: Option<ZStr>,
    /// Whether the declaration is a property hook
    /// (`CG(context).active_property_info_name` for validators).
    pub hook: bool,
}

/// A reflected global constant (`ReflectionConstant`).
#[derive(Clone)]
pub(crate) struct ConstRef {
    /// The constant table's key (namespace part lowercased for a
    /// `define()`d constant), which the side tables are keyed by.
    pub name: ZStr,
    /// The name as given to `define()` or `const` (`zend_constant.name`),
    /// which `getName()` and the other name accessors report.
    pub shown: ZStr,
    /// Its value.
    pub value: Value,
}

/// Internal state of reflection objects.
#[derive(Clone)]
pub(crate) enum Refl {
    /// `ReflectionClass` (with the object for `ReflectionObject`).
    Class { cls: Rc<Class>, obj: Option<Obj> },
    /// `ReflectionFunction` / `ReflectionMethod`.
    Function(FnRef),
    /// `ReflectionParameter`.
    Param { f: FnRef, idx: usize },
    /// `ReflectionProperty`.
    Prop(PropRef),
    /// `ReflectionClassConstant` / enum cases.
    Const { class: Rc<Class>, c: Rc<ClassConst> },
    /// `ReflectionType` subclasses.
    Type {
        ty: TypeDecl,
        scope: Option<Rc<Class>>,
        legacy: bool,
    },
    /// `ReflectionAttribute`.
    Attr(Rc<AttrRef>),
    /// `ReflectionExtension` / `ReflectionZendExtension`.
    Extension(ZStr),
    /// `ReflectionGenerator` (with the generator's function, captured at
    /// construction so that it survives termination).
    Generator(Obj, Option<FnRef>),
    /// `ReflectionFiber`.
    Fiber(Obj),
    /// `ReflectionReference`.
    Reference(PhpRef),
    /// `ReflectionConstant`.
    Constant(ConstRef),
}

/// The reflection state of an object, if initialized.
pub(crate) fn refl(o: &Obj) -> Option<Refl> {
    object::internal::<Refl, _>(o, |r| r.clone())
}

/// `GET_REFLECTION_OBJECT()`.
pub(crate) fn this_refl(vm: &mut Vm, a: &Args) -> VmResult<Refl> {
    let Some(o) = a.this.as_ref() else {
        return Err(vm.make_throw(
            "Error",
            "Internal error: Failed to retrieve the reflection object",
        ));
    };
    match refl(o) {
        Some(r) => Ok(r),
        None => Err(vm.make_throw(
            "Error",
            "Internal error: Failed to retrieve the reflection object",
        )),
    }
}

/// [`this_refl`] of a function's or method's reflection, read in place:
/// `f` gets the object's state borrowed, where [`refl`] copies its handles
/// (four reference counts up and down again for each getter's call). `f`
/// must not reach the VM (the state stays borrowed while it runs).
pub(crate) fn with_this_fn<R>(vm: &mut Vm, a: &Args, f: impl FnOnce(&FnRef) -> R) -> VmResult<R> {
    let got = a.this.as_ref().and_then(|o| {
        object::internal::<Refl, _>(o, |r| match r {
            Refl::Function(func) => Some(f(func)),
            _ => None,
        })
        .flatten()
    });
    got.ok_or_else(|| {
        vm.make_throw(
            "Error",
            "Internal error: Failed to retrieve the reflection object",
        )
    })
}

/// [`with_this_fn`] of any reflection object: `f` gives `None` for one
/// of another kind than it reads (the error of [`this_refl`]).
pub(crate) fn with_this_refl<R>(
    vm: &mut Vm,
    a: &Args,
    f: impl FnOnce(&Refl) -> Option<R>,
) -> VmResult<R> {
    let got = a
        .this
        .as_ref()
        .and_then(|o| object::internal::<Refl, _>(o, |r| f(r)).flatten());
    got.ok_or_else(|| {
        vm.make_throw(
            "Error",
            "Internal error: Failed to retrieve the reflection object",
        )
    })
}

/// A string value.
pub(crate) fn zs(s: &str) -> Value {
    Value::Str(ZStr::from(s))
}

/// The part of a name after the last backslash.
pub(crate) fn short_name(n: &ZStr) -> ZStr {
    match n.as_bytes().iter().rposition(|&c| c == b'\\') {
        Some(p) => ZStr::from(&n.as_bytes()[p + 1..]),
        None => n.clone(),
    }
}

/// The namespace part of a name.
pub(crate) fn ns_name(n: &ZStr) -> ZStr {
    match n.as_bytes().iter().rposition(|&c| c == b'\\') {
        Some(p) => ZStr::from(&n.as_bytes()[..p]),
        None => ZStr::empty(),
    }
}

/// Whether a name is namespaced.
pub(crate) fn in_ns(n: &ZStr) -> bool {
    n.as_bytes().contains(&b'\\')
}

/// The class name as PHP prints it (anonymous class names are cut at NUL).
pub(crate) fn dn(n: &ZStr) -> String {
    object::display_name(n).to_string_lossy()
}

/// Writes the `$name` / `$class` properties of a reflection object.
pub(crate) fn set_name_props(o: &Obj, name: Option<&ZStr>, class: Option<&ZStr>) {
    let cls = object::class_of(o);
    let (name_slot, class_slot) = name_slots(cls);
    let d = object::data(o);
    let Ok(mut slots) = d.slots.try_borrow_mut() else {
        return;
    };
    if let (Some(n), Some(i)) = (name, name_slot) {
        if let Some(s) = slots.get_mut(i as usize) {
            *s = Some(Value::Str(n.clone()));
        }
    }
    if let (Some(c), Some(i)) = (class, class_slot) {
        if let Some(s) = slots.get_mut(i as usize) {
            *s = Some(Value::Str(c.clone()));
        }
    }
}

thread_local! {
    /// The slots of `$name` and `$class` of the reflection classes
    /// [`set_name_props`] filled last (each reflection object made looked
    /// both up by name): the class held weakly, which pins the address
    /// compared.
    static NAME_SLOTS: std::cell::RefCell<[(std::rc::Weak<Class>, Option<u32>, Option<u32>); 4]> =
        std::cell::RefCell::new(std::array::from_fn(|_| (std::rc::Weak::new(), None, None)));
    static NAME_SLOTS_NEXT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The slots of `cls`'s `$name` and `$class` properties ([`NAME_SLOTS`]).
fn name_slots(cls: &Rc<Class>) -> (Option<u32>, Option<u32>) {
    let addr = Rc::as_ptr(cls);
    let hit = NAME_SLOTS.with(|c| {
        c.borrow()
            .iter()
            .find(|(w, _, _)| std::ptr::eq(w.as_ptr(), addr))
            .map(|(_, n, k)| (*n, *k))
    });
    if let Some(h) = hit {
        return h;
    }
    let n = cls.prop_map.get(b"name".as_slice()).map(|pi| pi.slot);
    let k = cls.prop_map.get(b"class".as_slice()).map(|pi| pi.slot);
    NAME_SLOTS.with(|c| {
        let i = NAME_SLOTS_NEXT.with(|x| {
            let i = x.get();
            x.set((i + 1) % 4);
            i
        });
        if let Some(e) = c.borrow_mut().get_mut(i) {
            *e = (Rc::downgrade(cls), n, k);
        }
    });
    (n, k)
}

/// The object a builtin was called on.
pub(crate) fn this(a: &Args) -> Obj {
    a.this_obj()
}

/// A `ReflectionException`.
pub(crate) fn rexc(vm: &mut Vm, msg: &str) -> Throw {
    vm.make_throw("ReflectionException", msg)
}

/// Creates a reflection object of class `class` with state `r`.
pub(crate) fn new_refl(
    vm: &mut Vm,
    class: &str,
    r: Refl,
    name: Option<&ZStr>,
    cname: Option<&ZStr>,
) -> VmResult<Value> {
    let cls = refl_class(vm, class)?;
    new_refl_of(vm, &cls, r, name, cname)
}

/// Gives the native methods `(lowercase name, getter)` of `cls` their
/// [`GetterFn`](crate::func::GetterFn)s (see [`crate::func::Builtin::getter`]).
pub(crate) fn set_getters(cls: &Rc<Class>, list: &[(&str, crate::func::GetterFn)]) {
    for (lc, g) in list {
        if let Some(m) = cls.methods.get(&ZStr::from(*lc)) {
            if let Func::Builtin(b) = &m.func {
                debug_assert!(b.flags & crate::func::flags::PURE != 0);
                b.set_getter(*g);
            }
        }
    }
}

/// The reflection class named `class`, for [`new_refl_of`].
pub(crate) fn refl_class(vm: &mut Vm, class: &str) -> VmResult<Rc<Class>> {
    // The internal reflection classes, looked up once per VM
    // (`Vm::refl_classes`: every reflection object a loop made looked its
    // class up by name, ~750,000 lookups in a framework application's unit
    // tests). A user class (a subclass of one) is looked up every time.
    if let Some((_, c)) = vm.refl_classes.iter().find(|(n, _)| &**n == class) {
        return Ok(Rc::clone(c));
    }
    let c = vm
        .find_class(class.as_bytes())
        .ok_or_else(|| vm.make_throw("Error", &format!("Class \"{class}\" not found")))?;
    if c.decl.is_none() && vm.refl_classes.len() < 16 {
        vm.refl_classes.push((Box::from(class), Rc::clone(&c)));
    }
    Ok(c)
}

/// [`new_refl`] of a class already looked up (a loop creating many).
pub(crate) fn new_refl_of(
    vm: &mut Vm,
    cls: &Rc<Class>,
    r: Refl,
    name: Option<&ZStr>,
    cname: Option<&ZStr>,
) -> VmResult<Value> {
    let o = vm.instantiate_unchecked(cls)?;
    set_name_props(&o, name, cname);
    object::set_internal(&o, r);
    Ok(Value::Object(o))
}

/// `zend_reflection_class_factory`.
pub(crate) fn class_value(vm: &mut Vm, c: &Rc<Class>) -> VmResult<Value> {
    let cn = if c.is_enum() {
        "ReflectionEnum"
    } else {
        "ReflectionClass"
    };
    new_refl(
        vm,
        cn,
        Refl::Class {
            cls: c.clone(),
            obj: None,
        },
        Some(&c.name),
        None,
    )
}

/// `reflection_method_factory`.
pub(crate) fn method_value(vm: &mut Vm, m: &Rc<Method>, class: &Rc<Class>) -> VmResult<Value> {
    let refl = refl_class(vm, "ReflectionMethod")?;
    method_value_of(vm, &refl, m, class)
}

/// [`method_value`] with the `ReflectionMethod` class looked up.
pub(crate) fn method_value_of(
    vm: &mut Vm,
    refl: &Rc<Class>,
    m: &Rc<Method>,
    class: &Rc<Class>,
) -> VmResult<Value> {
    let scope = m.scope();
    new_refl_of(
        vm,
        refl,
        Refl::Function(FnRef::method(m, class)),
        Some(&m.name),
        Some(&scope.name),
    )
}

/// `reflection_function_factory`.
pub(crate) fn function_value(vm: &mut Vm, f: FnRef) -> VmResult<Value> {
    let n = f.name();
    new_refl(vm, "ReflectionFunction", Refl::Function(f), Some(&n), None)
}

/// Extracts a string argument (already coerced by the stub signature).
pub(crate) fn str_arg(a: &Args, n: usize) -> ZStr {
    match a.at(n) {
        Value::Str(s) => s,
        Value::Int(i) => ZStr::from(i.to_string()),
        _ => ZStr::empty(),
    }
}

/// Runs `f` with the class scope of the calling user frame replaced by
/// `scope` (`EG(fake_scope)`).
pub(crate) fn with_scope<T>(
    vm: &mut Vm,
    scope: Option<Rc<Class>>,
    f: impl FnOnce(&mut Vm) -> VmResult<T>,
) -> VmResult<T> {
    let idx = vm
        .frames
        .iter()
        .rposition(|fr| fr.kind != crate::vm::FrameKind::Builtin);
    let Some(idx) = idx else {
        return f(vm);
    };
    let saved = std::mem::replace(&mut vm.frames[idx].scope, scope);
    let r = f(vm);
    if let Some(fr) = vm.frames.get_mut(idx) {
        fr.scope = saved;
    }
    r
}

/// Registers the Reflection classes (in php-src `MINIT` order).
pub(crate) fn register(vm: &mut Vm) {
    misc::register_base(vm);
    function::register_abstract(vm);
    function::register_function(vm);
    misc::register_generator(vm);
    param::register(vm);
    types::register(vm);
    function::register_method(vm);
    class::register(vm);
    property::register(vm);
    constant::register_class_constant(vm);
    misc::register_extension(vm);
    misc::register_reference(vm);
    attribute::register(vm);
    class::register_enum(vm);
    constant::register_cases(vm);
    misc::register_fiber(vm);
    constant::register_constant(vm);
    misc::register_hook_type(vm);
}

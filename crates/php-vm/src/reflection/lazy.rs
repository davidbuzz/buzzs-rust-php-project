// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Lazy objects (`Zend/zend_lazy_objects.c`): ghosts and proxies created by
//! `ReflectionClass::newLazyGhost()` / `newLazyProxy()` and the reflection
//! methods operating on them.
//!
//! The state lives in `ObjectData::lazy` ([`LazyInfo`]); `lazy_slots` marks
//! the declared property slots that are still lazy (`IS_PROP_LAZY`). The VM's
//! property handlers call [`object_for`] (through `Vm::lazy_object_for`)
//! before every property operation.

use super::class::{this_class_obj, update_class_constants};
use super::{rexc, this_refl, PropRef, Refl};
use crate::class::{cflags, Class};
use crate::func::{flags, Args};
use crate::lazy::LazyInfo;
use crate::object::{self, oflags};
use crate::vm::{Throw, Vm, VmResult};
use php_runtime::{HashTable, Obj, Value, ZStr};
use std::rc::Rc;

/// `ZEND_LAZY_OBJECT_SKIP_INITIALIZATION_ON_SERIALIZE`.
pub(crate) const SKIP_INITIALIZATION_ON_SERIALIZE: i64 = 8;
/// `ZEND_LAZY_OBJECT_SKIP_DESTRUCTOR`.
pub(crate) const SKIP_DESTRUCTOR: i64 = 16;

fn info(o: &Obj) -> Option<LazyInfo> {
    object::data(o).lazy.borrow().as_deref().cloned()
}

/// `zend_object_is_lazy() && !zend_lazy_object_initialized()`.
pub(crate) fn is_uninit(o: &Obj) -> bool {
    info(o).is_some_and(|i| !i.initialized)
}

/// Whether slot `slot` of `o` is lazy.
fn slot_is_lazy(o: &Obj, slot: usize) -> bool {
    object::data(o)
        .lazy
        .borrow()
        .as_ref()
        .is_some_and(|i| !i.initialized && i.lazy_slots.get(slot).copied().unwrap_or(false))
}

/// The slot of a declared, non-virtual property `name` of `o`'s class.
fn slot_of(o: &Obj, name: &ZStr) -> Option<usize> {
    let c = object::class_of(o).clone();
    let pi = c
        .prop_map
        .get(name)
        .cloned()
        .or_else(|| c.props.iter().find(|p| p.name == *name).cloned())?;
    if pi.is_virtual() {
        return None;
    }
    Some(pi.slot as usize)
}

/// The object a property operation on `o` acts on (`Vm::lazy_object_for`).
pub(crate) fn object_for(vm: &mut Vm, o: &Obj, name: Option<&ZStr>) -> VmResult<Obj> {
    let Some(i) = info(o) else {
        return Ok(o.clone());
    };
    if i.initialized {
        return match i.instance {
            Some(inst) => object_for(vm, &inst, name),
            None => Ok(o.clone()),
        };
    }
    if i.initializing {
        return Ok(o.clone());
    }
    if let Some(n) = name {
        let c = object::class_of(o).clone();
        if c.prop_map.get(n).is_some_and(|p| p.hooks.is_some()) {
            return Ok(o.clone());
        }
        if let Some(slot) = slot_of(o, n) {
            if !slot_is_lazy(o, slot) {
                return Ok(o.clone());
            }
        } else {
            let c = object::class_of(o).clone();
            if !i.proxy && c.prop_map.get(n).is_some_and(|p| p.is_virtual()) {
                return Ok(o.clone());
            }
            // Undeclared property handled by a magic method: the magic
            // method runs first and initializes only if it observes state.
            let m = &c.magic;
            let guarded = vm
                .prop_guards
                .iter()
                .any(|(h, gn, _)| *h == o.handle() && gn == n);
            if !guarded
                && (m.get.is_some() || m.set.is_some() || m.isset.is_some() || m.unset.is_some())
            {
                return Ok(o.clone());
            }
        }
    }
    init(vm, o)
}

/// Forces initialization (if lazy) and returns the real instance.
pub(crate) fn force_init(vm: &mut Vm, o: &Obj) -> VmResult<Obj> {
    object_for(vm, o, None)
}

fn set_info(o: &Obj, i: Option<LazyInfo>) {
    *object::data(o).lazy.borrow_mut() = i.map(Box::new);
}

fn with_info(o: &Obj, f: impl FnOnce(&mut LazyInfo)) {
    if let Some(i) = object::data(o).lazy.borrow_mut().as_deref_mut() {
        f(i);
    }
}

fn clear_destructed(o: &Obj) {
    let d = object::data(o);
    d.flags.set(d.flags.get() & !oflags::DESTRUCTED);
}

/// Snapshot of an object's properties for reverting a failed init.
struct Snapshot {
    slots: Vec<Option<Value>>,
    dynamic: Option<Box<HashTable>>,
}

fn snapshot(o: &Obj) -> Snapshot {
    let d = object::data(o);
    let s = Snapshot {
        slots: d.slots.borrow().clone(),
        dynamic: d.dynamic.borrow().clone(),
    };
    s
}

fn revert(o: &Obj, s: Snapshot) {
    let d = object::data(o);
    // The slots are the object's own (their number is fixed): the snapshot
    // goes back in place, releasing the values it replaces.
    for (slot, v) in d.slots.borrow_mut().iter_mut().zip(s.slots) {
        *slot = v;
    }
    *d.dynamic.borrow_mut() = s.dynamic;
    let mut separated = false;
    with_info(o, |i| {
        i.initializing = false;
        separated = std::mem::take(&mut i.props_separated);
        i.props_shared = false;
    });
    // `zend_lazy_object_revert_init` puts the snapshot table back: the
    // frozen copies of a table the initializer separated go away.
    if separated {
        let frozen = crate::lazy::set_stale_slots(d, None);
        drop(frozen);
    }
}

/// `zend_lazy_object_init`.
pub(crate) fn init(vm: &mut Vm, o: &Obj) -> VmResult<Obj> {
    let Some(i) = info(o) else {
        return Ok(o.clone());
    };
    if i.initialized {
        return match i.instance {
            Some(inst) => force_init(vm, &inst),
            None => Ok(o.clone()),
        };
    }
    let initial_refs = o.refcount();
    let r = init_uninit(vm, o, &i);
    // `GC_DELREF(obj) == 0`: the initializer dropped every reference but
    // the handle the caller borrowed `o` through (the counterpart of
    // php-src's `GC_ADDREF` around the initializer).
    let returned = usize::from(matches!(&r, Ok(x) if Obj::ptr_eq(x, o)));
    if initial_refs == 1
        || o.refcount() - returned > 1
        || matches!(&r, Err(e) if !matches!(e, Throw::Exception(_)))
    {
        return r;
    }
    let e = vm.make_throw("Error", "Lazy object was released during initialization");
    if let (Throw::Exception(new), Err(Throw::Exception(prev))) = (&e, &r) {
        crate::exceptions::chain_previous(new, prev);
    }
    Err(e)
}

/// `zend_lazy_object_init` / `zend_lazy_object_init_proxy` on an
/// uninitialized lazy object.
fn init_uninit(vm: &mut Vm, o: &Obj, i: &LazyInfo) -> VmResult<Obj> {
    let cls = object::class_of(o).clone();
    with_info(o, |i| i.initializing = true);
    let snap = snapshot(o);
    if i.proxy {
        let r = vm.call_value(&i.initializer, vec![Value::Object(o.clone())]);
        let retval = match r {
            Ok(v) => v.deref(),
            Err(e) => {
                revert(o, snap);
                return Err(e);
            }
        };
        let inst = match &retval {
            Value::Object(x) => x.clone(),
            other => {
                revert(o, snap);
                return Err(vm.make_throw(
                    "TypeError",
                    &format!(
                        "Lazy proxy factory must return an instance of a class compatible with {}, {} returned",
                        super::dn(&cls.name),
                        value_name(other)
                    ),
                ));
            }
        };
        if !compatible(&inst, o) {
            revert(o, snap);
            return Err(vm.make_throw(
                "TypeError",
                &format!(
                    "The real instance class {} is not compatible with the proxy class {}. The proxy must be a instance of the same class as the real instance, or a sub-class with no additional properties, and no overrides of the __destructor or __clone methods.",
                    super::dn(&object::class_of(&inst).name),
                    super::dn(&cls.name)
                ),
            ));
        }
        if Obj::ptr_eq(&inst, o)
            || info(&inst).is_some_and(|x| !x.initialized || x.proxy) && info(&inst).is_some()
        {
            revert(o, snap);
            return Err(vm.make_throw("Error", "Lazy proxy factory must return a non-lazy object"));
        }
        let d = object::data(o);
        *d.dynamic.borrow_mut() = None;
        for s in d.slots.borrow_mut().iter_mut() {
            *s = None;
        }
        // `zend_object_dtor_dynamic_properties(obj); obj->properties = NULL`.
        d.flags
            .set(d.flags.get() & !(oflags::PROPS_TABLE | oflags::EMPTY_IND));
        let frozen = crate::lazy::set_stale_slots(d, None);
        drop(frozen);
        with_info(o, |x| {
            x.initialized = true;
            x.initializing = false;
            x.instance = Some(inst.clone());
        });
        return Ok(inst);
    }
    // Ghost: initialize lazy properties to their default value.
    let defaults = match vm.class_defaults(&cls) {
        Ok(d) => d,
        Err(e) => {
            revert(o, snap);
            return Err(e);
        }
    };
    {
        let d = object::data(o);
        let mut slots = d.slots.borrow_mut();
        for (k, s) in slots.iter_mut().enumerate() {
            if i.lazy_slots.get(k).copied().unwrap_or(false) {
                *s = defaults.get(k).cloned().flatten();
            }
        }
    }
    // Snapshot of the properties table (`GC_TRY_ADDREF(properties_snapshot)`):
    // while the initializer runs, a write to an existing table separates it.
    let shared = crate::lazy::props_built(object::data(o));
    with_info(o, |x| {
        x.props_shared = shared;
        x.props_separated = false;
    });
    let r = vm.call_value(&i.initializer, vec![Value::Object(o.clone())]);
    match r {
        Err(e) => {
            revert(o, snap);
            Err(e)
        }
        Ok(v) if !v.deref().is_null() => {
            revert(o, snap);
            Err(vm.make_throw(
                "TypeError",
                "Lazy object initializer must return NULL or no value",
            ))
        }
        Ok(_) => {
            set_info(o, None);
            clear_destructed(o);
            Ok(o.clone())
        }
    }
}

fn value_name(v: &Value) -> String {
    match v {
        Value::Object(o) => super::dn(&object::class_of(o).name),
        other => crate::typecheck::given_type(other),
    }
}

/// `zend_lazy_object_compatible`.
fn compatible(real: &Obj, lazy: &Obj) -> bool {
    let rc = object::class_of(real);
    let lc = object::class_of(lazy);
    if Rc::ptr_eq(rc, lc) {
        return true;
    }
    if !lc.is_subclass_of(rc) {
        return false;
    }
    let count = |c: &Rc<Class>| c.props.len() + c.static_props.len();
    if count(lc) != count(rc) {
        return false;
    }
    let same =
        |a: &Option<Rc<crate::func::Method>>, b: &Option<Rc<crate::func::Method>>| match (a, b) {
            (None, None) => true,
            (Some(x), Some(y)) => Rc::ptr_eq(x, y),
            _ => false,
        };
    same(&lc.magic.destruct, &rc.magic.destruct) && same(&lc.magic.clone, &rc.magic.clone)
}

/// Lazy slots of a fresh object of `c`.
fn all_slots(c: &Class) -> Vec<bool> {
    let mut v = vec![false; c.props.len()];
    for p in &c.props {
        if !p.is_virtual() {
            if let Some(s) = v.get_mut(p.slot as usize) {
                *s = true;
            }
        }
    }
    v
}

/// `zend_object_make_lazy`.
fn make_lazy(
    vm: &mut Vm,
    obj: Option<Obj>,
    cls: &Rc<Class>,
    initializer: Value,
    opts: i64,
    proxy: bool,
) -> VmResult<Obj> {
    let is_std = cls.lc_name.as_bytes() == b"stdclass";
    if cls.flags & cflags::INTERNAL != 0 && !is_std {
        return Err(vm.make_throw(
            "Error",
            &format!(
                "Cannot make instance of internal class lazy: {} is internal",
                cls.name
            ),
        ));
    }
    for pc in std::iter::successors(cls.parent.clone(), |x| x.parent.clone()) {
        if pc.flags & cflags::INTERNAL != 0 && pc.lc_name.as_bytes() != b"stdclass" {
            return Err(vm.make_throw(
                "Error",
                &format!(
                    "Cannot make instance of internal class lazy: {} inherits internal class {}",
                    cls.name, pc.name
                ),
            ));
        }
    }
    let (o, lazy_slots) = match obj {
        None => {
            if cls.is_interface()
                || cls.is_trait()
                || cls.is_enum()
                || super::class::is_abstract(cls)
            {
                vm.instantiate(cls)?;
            }
            update_class_constants(vm, cls)?;
            let o = object::new_object(cls.clone(), vec![None; cls.props.len()]);
            let slots = all_slots(cls);
            (o, slots)
        }
        Some(o) => {
            if let Some(i) = info(&o) {
                if i.initializing {
                    return Err(vm.make_throw(
                        "Error",
                        "Can not reset an object while it is being initialized",
                    ));
                }
                set_info(&o, None);
                drop(i);
                vm.run_pending_destructors()?;
            } else {
                let d = object::data(&o);
                let destructed = d.flags.get() & oflags::DESTRUCTED != 0;
                if opts & SKIP_DESTRUCTOR == 0 && !destructed && cls.magic.destruct.is_some() {
                    d.flags.set(d.flags.get() | oflags::DESTRUCTED);
                    vm.call_destructor(&o)?;
                }
            }
            clear_destructed(&o);
            let oc = object::class_of(&o).clone();
            let d = object::data(&o);
            // php-src keeps `obj->properties` and removes its trailing
            // plain entries (`zlo_hash_remove_dyn_props_func`): the dynamic
            // properties, and in a table holding frozen copies of the
            // declared properties, those copies too.
            if d.dynamic.borrow().is_some() {
                crate::lazy::mark_props_built(d);
            }
            if let Some(frozen) = crate::lazy::stale_slots(d) {
                let prev = crate::lazy::set_stale_slots(d, Some(vec![None; frozen.len()]));
                drop((frozen, prev));
            }
            *d.dynamic.borrow_mut() = None;
            object::set_prop_order(d, None);
            let mut lazy = vec![false; oc.props.len()];
            {
                let mut slots = d.slots.borrow_mut();
                for p in &oc.props {
                    if !cls.props.iter().any(|declared| {
                        declared.name == p.name
                            && (declared.visibility() != flags::PRIVATE
                                || declared.class == p.class)
                    }) {
                        continue;
                    }
                    if p.is_virtual() {
                        continue;
                    }
                    let k = p.slot as usize;
                    if slots.get(k).is_some_and(Option::is_some)
                        && p.flags & flags::READONLY != 0
                        && (oc.flags & cflags::FINAL != 0 || p.flags & flags::FINAL != 0)
                    {
                        continue;
                    }
                    if let Some(s) = slots.get_mut(k) {
                        *s = None;
                    }
                    if let Some(l) = lazy.get_mut(k) {
                        *l = true;
                    }
                }
            }
            (o, lazy)
        }
    };
    if !lazy_slots.iter().any(|x| *x) {
        return Ok(o);
    }
    let d = object::data(&o);
    d.flags.set(d.flags.get() | oflags::DESTRUCTED);
    set_info(
        &o,
        Some(LazyInfo {
            proxy,
            initializer,
            options: opts,
            initialized: false,
            instance: None,
            initializing: false,
            lazy_slots,
            props_shared: false,
            props_separated: false,
        }),
    );
    Ok(o)
}

/// Decrements the lazy property count; realizes the object when none is
/// left (`zend_lazy_object_decr_lazy_props` + `zend_lazy_object_realize`).
fn after_slot_unmarked(o: &Obj) {
    let done = object::data(o)
        .lazy
        .borrow()
        .as_ref()
        .is_some_and(|i| !i.initialized && !i.lazy_slots.iter().any(|x| *x));
    if done {
        set_info(o, None);
        clear_destructed(o);
    }
}

fn unmark(o: &Obj, slot: usize) {
    with_info(o, |i| {
        if let Some(s) = i.lazy_slots.get_mut(slot) {
            *s = false;
        }
    });
}

// ---------------------------------------------------------------------
// ReflectionClass methods
// ---------------------------------------------------------------------

fn check_options(vm: &mut Vm, a: &Args, idx: usize, method: &str, reset: bool) -> VmResult<i64> {
    let opts = match a.get(idx) {
        Some(Value::Int(i)) => i,
        _ => 0,
    };
    let argn = idx + 1;
    if opts & !(SKIP_INITIALIZATION_ON_SERIALIZE | SKIP_DESTRUCTOR) != 0 {
        return Err(rexc(
            vm,
            &format!(
                "ReflectionClass::{method}(): Argument #{argn} ($options) contains invalid flags"
            ),
        ));
    }
    if !reset && opts & SKIP_DESTRUCTOR != 0 {
        return Err(rexc(
            vm,
            &format!("ReflectionClass::{method}(): Argument #{argn} ($options) does not accept ReflectionClass::SKIP_DESTRUCTOR"),
        ));
    }
    Ok(opts)
}

/// `Z_PARAM_OBJ_OF_CLASS(object, ce)`.
fn obj_of_class(vm: &mut Vm, a: &Args, idx: usize, c: &Rc<Class>, method: &str) -> VmResult<Obj> {
    match a.at(idx) {
        Value::Object(o) if object::class_of(&o).is_subclass_of(c) => Ok(o),
        v => Err(vm.make_throw(
            "TypeError",
            &format!(
                "ReflectionClass::{method}(): Argument #{} ($object) must be of type {}, {} given",
                idx + 1,
                super::dn(&c.name),
                value_name(&v)
            ),
        )),
    }
}

pub(crate) fn rc_new_lazy_ghost(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, _) = this_class_obj(vm, a)?;
    let opts = check_options(vm, a, 1, "newLazyGhost", false)?;
    let o = make_lazy(vm, None, &c, a.at(0), opts, false)?;
    Ok(Value::Object(o))
}

pub(crate) fn rc_new_lazy_proxy(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, _) = this_class_obj(vm, a)?;
    let opts = check_options(vm, a, 1, "newLazyProxy", false)?;
    let o = make_lazy(vm, None, &c, a.at(0), opts, true)?;
    Ok(Value::Object(o))
}

fn reset(vm: &mut Vm, a: &Args, proxy: bool, method: &str) -> VmResult<Value> {
    let (c, _) = this_class_obj(vm, a)?;
    let o = obj_of_class(vm, a, 0, &c, method)?;
    let opts = check_options(vm, a, 2, method, true)?;
    if info(&o).is_some_and(|i| i.initializing) {
        return Err(vm.make_throw(
            "Error",
            "Can not reset an object while it is being initialized",
        ));
    }
    if is_uninit(&o) {
        return Err(rexc(vm, "Object is already lazy"));
    }
    make_lazy(vm, Some(o), &c, a.at(1), opts, proxy)?;
    Ok(Value::Null)
}

pub(crate) fn rc_reset_as_lazy_ghost(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    reset(vm, a, false, "resetAsLazyGhost")
}

pub(crate) fn rc_reset_as_lazy_proxy(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    reset(vm, a, true, "resetAsLazyProxy")
}

pub(crate) fn rc_initialize_lazy_object(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, _) = this_class_obj(vm, a)?;
    let o = obj_of_class(vm, a, 0, &c, "initializeLazyObject")?;
    Ok(Value::Object(force_init(vm, &o)?))
}

pub(crate) fn rc_is_uninitialized_lazy_object(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, _) = this_class_obj(vm, a)?;
    let o = obj_of_class(vm, a, 0, &c, "isUninitializedLazyObject")?;
    Ok(Value::Bool(is_uninit(&o)))
}

pub(crate) fn rc_mark_lazy_object_as_initialized(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, _) = this_class_obj(vm, a)?;
    let o = obj_of_class(vm, a, 0, &c, "markLazyObjectAsInitialized")?;
    if let Some(i) = info(&o) {
        if !i.initialized {
            let cls = object::class_of(&o).clone();
            let defaults = vm.class_defaults(&cls)?;
            let d = object::data(&o);
            for (k, s) in d.slots.borrow_mut().iter_mut().enumerate() {
                if i.lazy_slots.get(k).copied().unwrap_or(false) {
                    *s = defaults.get(k).cloned().flatten();
                }
            }
            set_info(&o, None);
            clear_destructed(&o);
        }
    }
    match info(&o) {
        Some(i) => Ok(i.instance.map_or(Value::Object(o), Value::Object)),
        None => Ok(Value::Object(o)),
    }
}

pub(crate) fn rc_get_lazy_initializer(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (c, _) = this_class_obj(vm, a)?;
    let o = obj_of_class(vm, a, 0, &c, "getLazyInitializer")?;
    Ok(match info(&o) {
        Some(i) if !i.initialized => i.initializer,
        _ => Value::Null,
    })
}

// ---------------------------------------------------------------------
// ReflectionProperty methods
// ---------------------------------------------------------------------

fn this_prop(vm: &mut Vm, a: &Args) -> VmResult<PropRef> {
    match this_refl(vm, a)? {
        Refl::Prop(p) => Ok(p),
        _ => Err(vm.make_throw(
            "Error",
            "Internal error: Failed to retrieve the reflection object",
        )),
    }
}

/// Follows initialized proxies to their instance.
fn real(o: &Obj) -> Obj {
    let mut o = o.clone();
    while let Some(i) = info(&o) {
        match (i.initialized, i.instance) {
            (true, Some(inst)) => o = inst,
            _ => break,
        }
    }
    o
}

fn prop_obj(vm: &mut Vm, a: &Args, p: &PropRef, method: &str) -> VmResult<Obj> {
    match a.at(0) {
        Value::Object(o) if object::class_of(&o).is_subclass_of(&p.class) => Ok(o),
        v => Err(vm.make_throw(
            "TypeError",
            &format!(
                "ReflectionProperty::{method}(): Argument #1 ($object) must be of type {}, {} given",
                super::dn(&p.class.name),
                value_name(&v)
            ),
        )),
    }
}

/// `reflection_property_check_lazy_compatible`; returns the slot.
fn lazy_compatible(vm: &mut Vm, p: &PropRef, o: &Obj, method: &str) -> VmResult<usize> {
    let oc = object::class_of(o).clone();
    let meta = match &p.meta {
        Some(m) if m.flags & flags::PRIVATE != 0 => Some((**m).clone()),
        _ => super::order::find_prop(vm, &oc, p.name.as_bytes()),
    };
    let Some(m) = meta else {
        return Err(rexc(
            vm,
            &format!(
                "Cannot use {method}() on dynamic property {}::${}",
                p.class.name, p.name
            ),
        ));
    };
    let decl = super::dn(&m.class);
    if m.is_static {
        return Err(rexc(
            vm,
            &format!(
                "Cannot use {method}() on static property {decl}::${}",
                p.name
            ),
        ));
    }
    if m.flags & flags::VIRTUAL != 0 {
        return Err(rexc(
            vm,
            &format!(
                "Cannot use {method}() on virtual property {decl}::${}",
                p.name
            ),
        ));
    }
    if oc.flags & cflags::INTERNAL != 0
        && oc.lc_name.as_bytes() != b"stdclass"
        && oc
            .hooks
            .as_ref()
            .is_some_and(|h| h.write_property.is_some())
    {
        return Err(rexc(
            vm,
            &format!("Cannot use {method}() on internal class {}", oc.name),
        ));
    }
    Ok(m.info.map_or(0, |pi| pi.slot as usize))
}

/// Writes a declared property slot directly (no hooks, no `__set`).
fn set_slot_raw(vm: &mut Vm, p: &PropRef, o: &Obj, slot: usize, v: Value) -> VmResult<()> {
    let oc = object::class_of(o).clone();
    let Some(pi) = oc
        .props
        .iter()
        .find(|x| x.slot as usize == slot && x.name == p.name)
        .cloned()
    else {
        return super::property::set_raw(vm, p, o, v);
    };
    if pi.hooks.as_ref().is_some_and(|h| h.set.is_some()) {
        return super::property::set_raw(vm, p, o, v);
    }
    if pi.is_readonly()
        && object::data(o)
            .slots
            .borrow()
            .get(slot)
            .is_some_and(Option::is_some)
    {
        return Err(vm.make_throw(
            "Error",
            &format!(
                "Cannot modify readonly property {}::${}",
                super::dn(&pi.class),
                pi.name
            ),
        ));
    }
    let v = vm.coerce_prop(Some(o), &pi, v)?;
    if let Some(s) = object::data(o).slots.borrow_mut().get_mut(slot) {
        *s = Some(v);
    }
    Ok(())
}

pub(crate) fn rp_set_raw_value_without_lazy_initialization(
    vm: &mut Vm,
    a: &mut Args,
) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    let o = prop_obj(vm, a, &p, "setRawValueWithoutLazyInitialization")?;
    let o = real(&o);
    let slot = lazy_compatible(vm, &p, &o, "setRawValueWithoutLazyInitialization")?;
    let was_lazy = slot_is_lazy(&o, slot);
    unmark(&o, slot);
    let r = set_slot_raw(vm, &p, &o, slot, a.at(1));
    if r.is_err()
        && was_lazy
        && object::data(&o)
            .slots
            .borrow()
            .get(slot)
            .is_some_and(Option::is_none)
        && is_uninit(&o)
    {
        with_info(&o, |i| {
            if let Some(s) = i.lazy_slots.get_mut(slot) {
                *s = true;
            }
        });
    }
    r?;
    if was_lazy {
        after_slot_unmarked(&o);
    }
    Ok(Value::Null)
}

pub(crate) fn rp_skip_lazy_initialization(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    let o = prop_obj(vm, a, &p, "skipLazyInitialization")?;
    let o = real(&o);
    let slot = lazy_compatible(vm, &p, &o, "skipLazyInitialization")?;
    if !slot_is_lazy(&o, slot) {
        return Ok(Value::Null);
    }
    let cls = object::class_of(&o).clone();
    let defaults = vm.class_defaults(&cls)?;
    if let Some(s) = object::data(&o).slots.borrow_mut().get_mut(slot) {
        *s = defaults.get(slot).cloned().flatten();
    }
    unmark(&o, slot);
    after_slot_unmarked(&o);
    Ok(Value::Null)
}

pub(crate) fn rp_is_lazy(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = this_prop(vm, a)?;
    let o = prop_obj(vm, a, &p, "isLazy")?;
    let o = real(&o);
    let oc = object::class_of(&o).clone();
    let meta = match &p.meta {
        Some(m) if m.flags & flags::PRIVATE != 0 => Some((**m).clone()),
        _ => super::order::find_prop(vm, &oc, p.name.as_bytes()),
    };
    let Some(m) = meta else {
        return Ok(Value::Bool(false));
    };
    if m.is_static || m.flags & flags::VIRTUAL != 0 {
        return Ok(Value::Bool(false));
    }
    let slot = m.info.map_or(0, |pi| pi.slot as usize);
    Ok(Value::Bool(slot_is_lazy(&o, slot)))
}

/// The real instance of an initialized lazy proxy (its `var_dump()` shows
/// `["instance" => ...]`, `zend_lazy_object_debug_info`).
#[allow(dead_code)] // VM hook (object dump), see the module docs
pub(crate) fn proxy_instance(o: &Obj) -> Option<Obj> {
    match info(o) {
        Some(i) if i.proxy && i.initialized => i.instance,
        _ => None,
    }
}

/// `zend_lazy_object_clone`: initializes `o` and returns the object to clone
/// (`None`: not lazy). For an initialized proxy, returns a new proxy whose
/// instance is a clone of the real instance.
#[allow(dead_code)] // VM hook (`clone`), see the module docs
pub(crate) fn clone_lazy(vm: &mut Vm, o: &Obj) -> VmResult<Option<Value>> {
    let Some(i) = info(o) else { return Ok(None) };
    if !i.initialized {
        init(vm, o)?;
    }
    let Some(i) = info(o) else { return Ok(None) };
    let Some(inst) = i.instance.clone() else {
        return Ok(None);
    };
    let cls = object::class_of(o).clone();
    let new_inst = vm.clone_value(Value::Object(inst))?;
    let Value::Object(new_inst) = new_inst else {
        return Ok(None);
    };
    let np = object::new_object(cls.clone(), vec![None; cls.props.len()]);
    let d = object::data(&np);
    d.flags.set(d.flags.get() | oflags::DESTRUCTED);
    set_info(
        &np,
        Some(LazyInfo {
            instance: Some(new_inst),
            ..i
        }),
    );
    Ok(Some(Value::Object(np)))
}

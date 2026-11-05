//! Native state of the date classes and the object plumbing shared by them
//! (`php_date_obj`, `php_timezone_obj`, `php_interval_obj`,
//! `php_period_obj`, uninitialised-object errors, raw clones, the standard
//! property table).

use std::rc::Rc;

use php_date::api::Interval;
use php_date::parse::TimezoneSpec;
use php_date::{RelTime, Time};
use php_runtime::{ArrayKey, Obj, Value, ZArray, ZStr};
use php_vm::class::{cflags, Class};
use php_vm::ext::{Throw, VmResult};
use php_vm::func::flags;
use php_vm::object;
use php_vm::Vm;

/// `php_date_obj`: `None` until a constructor/factory initialised it.
#[derive(Debug, Clone, Default)]
pub(crate) struct DateObj {
    pub(crate) time: Option<Time>,
}

/// `php_timezone_obj`.
#[derive(Debug, Clone, Default)]
pub(crate) struct TzObj {
    pub(crate) tz: Option<TimezoneSpec>,
}

/// `php_interval_obj`.
#[derive(Debug, Clone, Default)]
pub(crate) struct IntervalObj {
    pub(crate) iv: Option<Interval>,
}

/// `php_period_obj`.
#[derive(Debug, Clone, Default)]
pub(crate) struct PeriodObj {
    pub(crate) start: Option<Time>,
    pub(crate) start_ce: Option<Rc<Class>>,
    pub(crate) current: Option<Time>,
    pub(crate) end: Option<Time>,
    pub(crate) interval: Option<RelTime>,
    pub(crate) recurrences: i64,
    pub(crate) initialized: bool,
    pub(crate) include_start_date: bool,
    pub(crate) include_end_date: bool,
}

/// The time of a date object (cloned), `None` if uninitialised or not a
/// date object.
pub(crate) fn time_of(o: &Obj) -> Option<Time> {
    object::internal::<DateObj, _>(o, |s| s.time.clone()).flatten()
}

/// Replaces the time of a date object.
pub(crate) fn set_time(o: &Obj, t: Option<Time>) {
    if object::internal::<DateObj, _>(o, |s| s.time.clone_from(&t)).is_none() {
        object::set_internal(o, DateObj { time: t });
    }
}

/// Runs `f` on the initialised time of `o`, or throws the uninitialised
/// error.
pub(crate) fn with_time<R>(vm: &mut Vm, o: &Obj, f: impl FnOnce(&mut Time) -> R) -> VmResult<R> {
    let r = object::internal::<DateObj, _>(o, |s| s.time.as_mut().map(f)).flatten();
    match r {
        Some(r) => Ok(r),
        None => Err(uninit_error(vm, object::class_of(o))),
    }
}

/// The time of `o` or the uninitialised error.
pub(crate) fn require_time(vm: &mut Vm, o: &Obj) -> VmResult<Time> {
    match time_of(o) {
        Some(t) => Ok(t),
        None => Err(uninit_error(vm, object::class_of(o))),
    }
}

/// The zone of a `DateTimeZone` object.
pub(crate) fn tz_of(o: &Obj) -> Option<TimezoneSpec> {
    object::internal::<TzObj, _>(o, |s| s.tz.clone()).flatten()
}

/// Sets the zone of a `DateTimeZone` object.
pub(crate) fn set_tz(o: &Obj, tz: Option<TimezoneSpec>) {
    if object::internal::<TzObj, _>(o, |s| s.tz.clone_from(&tz)).is_none() {
        object::set_internal(o, TzObj { tz });
    }
}

/// The zone of `o` or the uninitialised error.
pub(crate) fn require_tz(vm: &mut Vm, o: &Obj) -> VmResult<TimezoneSpec> {
    match tz_of(o) {
        Some(t) => Ok(t),
        None => Err(uninit_error(vm, object::class_of(o))),
    }
}

/// The interval of a `DateInterval` object.
pub(crate) fn interval_of(o: &Obj) -> Option<Interval> {
    object::internal::<IntervalObj, _>(o, |s| s.iv.clone()).flatten()
}

/// Sets the interval of a `DateInterval` object and mirrors its fields into
/// the property table, like `date_object_get_properties_interval()` does
/// (so `(array)`, `get_object_vars()` and `foreach` see them).
pub(crate) fn set_interval(o: &Obj, iv: Option<Interval>) {
    if let Some(iv) = &iv {
        if iv.from_string {
            // Only `from_string`/`date_string` describe such an interval;
            // drop mirrored numeric fields (PHP fills the table lazily).
            if let Some(t) = object::data(o).dynamic.borrow_mut().as_mut() {
                for k in ["y", "m", "d", "h", "i", "s", "f", "invert", "days"] {
                    t.remove(&ArrayKey::Str(ZStr::from(k)));
                }
            }
        }
        if iv.from_string {
            write_table_prop(o, "from_string", Value::Bool(true));
            let ds = iv.date_string.as_deref().unwrap_or_default();
            write_table_prop(o, "date_string", Value::Str(ZStr::from(ds)));
        } else {
            // `date_interval_object_to_hash()`, without the intermediate tree.
            let r = &iv.rel;
            for (k, v) in [
                ("y", r.y),
                ("m", r.m),
                ("d", r.d),
                ("h", r.h),
                ("i", r.i),
                ("s", r.s),
            ] {
                write_table_prop(o, k, Value::Int(v));
            }
            #[allow(clippy::cast_precision_loss)]
            write_table_prop(o, "f", Value::Float(r.us as f64 / 1_000_000.0));
            write_table_prop(o, "invert", Value::Int(i64::from(r.invert)));
            let days = if r.days == php_date::UNSET {
                Value::Bool(false)
            } else {
                Value::Int(r.days)
            };
            write_table_prop(o, "days", days);
            write_table_prop(o, "from_string", Value::Bool(false));
        }
    }
    if object::internal::<IntervalObj, _>(o, |s| s.iv.clone_from(&iv)).is_none() {
        object::set_internal(o, IntervalObj { iv });
    }
}

/// `zend_hash_str_update()` on the object's property table: the declared
/// slot when there is one, the dynamic table otherwise.
pub(crate) fn write_table_prop(o: &Obj, name: &str, v: Value) {
    let d = object::data(o);
    let key = ZStr::from(name);
    if let Some(pi) = d.class.prop_map.get(&key) {
        if pi.flags & flags::STATIC == 0 && !pi.is_virtual() {
            if let Some(s) = d.slots.borrow_mut().get_mut(pi.slot as usize) {
                *s = Some(v);
                return;
            }
        }
    }
    d.dynamic
        .borrow_mut()
        .get_or_insert_with(Box::default)
        .insert(ArrayKey::Str(key), v);
}

/// The interval of `o` or the uninitialised error.
pub(crate) fn require_interval(vm: &mut Vm, o: &Obj) -> VmResult<Interval> {
    match interval_of(o) {
        Some(t) => Ok(t),
        None => Err(uninit_error(vm, object::class_of(o))),
    }
}

/// The period state (cloned).
pub(crate) fn period_of(o: &Obj) -> PeriodObj {
    object::internal::<PeriodObj, _>(o, |s| s.clone()).unwrap_or_default()
}

/// Runs `f` on the period state.
pub(crate) fn with_period<R>(o: &Obj, f: impl FnOnce(&mut PeriodObj) -> R) -> Option<R> {
    object::internal::<PeriodObj, _>(o, f)
}

/// Whether `o` is an instance of class `lc` (lower-case name).
pub(crate) fn instance_of(o: &Obj, lc: &[u8]) -> bool {
    object::class_of(o).instance_of_name(lc)
}

/// Whether `v` is an object instance of `lc`.
pub(crate) fn value_instance_of(v: &Value, lc: &[u8]) -> Option<Obj> {
    match v {
        Value::Object(o) if instance_of(o, lc) => Some(o.clone()),
        _ => None,
    }
}

fn is_internal(c: &Class) -> bool {
    c.flags & cflags::INTERNAL != 0
}

/// `date_throw_uninitialized_error()`.
pub(crate) fn uninit_error(vm: &mut Vm, ce: &Rc<Class>) -> Throw {
    let name = object::display_name(&ce.name).to_string_lossy();
    if is_internal(ce) {
        return vm.throw(
            "DateObjectError",
            &format!("Object of type {name} has not been correctly initialized by calling parent::__construct() in its constructor"),
        );
    }
    let mut p = ce.clone();
    while !is_internal(&p) {
        match p.parent.clone() {
            Some(pp) => p = pp,
            None => break,
        }
    }
    if !is_internal(&p) {
        return vm.throw(
            "DateObjectError",
            &format!("Object of type {name} not been correctly initialized by calling parent::__construct() in its constructor"),
        );
    }
    vm.throw(
        "DateObjectError",
        &format!(
            "Object of type {name} (inheriting {}) has not been correctly initialized by calling parent::__construct() in its constructor",
            p.name
        ),
    )
}

/// Looks up a registered class by name.
pub(crate) fn class(vm: &Vm, name: &str) -> Rc<Class> {
    vm.find_class(name.as_bytes())
        .unwrap_or_else(|| panic!("class {name} is registered at startup"))
}

/// `object_init_ex()` (runs the `create` hook, no constructor).
pub(crate) fn instantiate(vm: &mut Vm, cls: &Rc<Class>) -> VmResult<Obj> {
    if cls.flags & (cflags::ABSTRACT | cflags::INTERFACE) != 0 {
        let what = if cls.flags & cflags::INTERFACE != 0 {
            "interface"
        } else {
            "abstract class"
        };
        return Err(vm.throw("Error", &format!("Cannot instantiate {what} {}", cls.name)));
    }
    vm.new_object(cls)
}

/// Creates a date object of class `cls` holding `t`.
pub(crate) fn new_date(vm: &mut Vm, cls: &Rc<Class>, t: Time) -> VmResult<Obj> {
    let o = instantiate(vm, cls)?;
    set_time(&o, Some(t));
    Ok(o)
}

/// Creates a `DateTimeZone` holding `tz`.
pub(crate) fn new_tz(vm: &mut Vm, tz: TimezoneSpec) -> VmResult<Obj> {
    let cls = class(vm, "DateTimeZone");
    let o = instantiate(vm, &cls)?;
    set_tz(&o, Some(tz));
    Ok(o)
}

/// Creates a `DateInterval` holding `iv`.
pub(crate) fn new_interval(vm: &mut Vm, iv: Interval) -> VmResult<Obj> {
    let cls = class(vm, "DateInterval");
    let o = instantiate(vm, &cls)?;
    set_interval(&o, Some(iv));
    Ok(o)
}

/// `date_object_clone_*`: a copy of `o` (properties and native state)
/// without calling `__clone`.
pub(crate) fn clone_raw(o: &Obj) -> Obj {
    let d = object::data(o);
    let slots = d.slots.borrow().clone();
    let n = object::new_object(d.class.clone(), slots);
    let nd = object::data(&n);
    nd.dynamic.borrow_mut().clone_from(&d.dynamic.borrow());
    nd.copy_unset_flags(d);
    copy_state(o, &n);
    n
}

/// Copies whichever native state `from` has to `to`.
pub(crate) fn copy_state(from: &Obj, to: &Obj) {
    if let Some(s) = object::internal::<DateObj, _>(from, |s| s.clone()) {
        object::set_internal(to, s);
    } else if let Some(s) = object::internal::<TzObj, _>(from, |s| s.clone()) {
        object::set_internal(to, s);
    } else if let Some(s) = object::internal::<IntervalObj, _>(from, |s| s.clone()) {
        object::set_internal(to, s);
    } else if let Some(s) = object::internal::<PeriodObj, _>(from, |s| s.clone()) {
        object::set_internal(to, s);
    }
}

/// The mangled property-table key of a declared property.
fn mangle(pi: &php_vm::class::PropInfo) -> ZStr {
    match pi.visibility() {
        flags::PRIVATE => {
            let mut v = vec![0u8];
            v.extend_from_slice(pi.class.as_bytes());
            v.push(0);
            v.extend_from_slice(pi.name.as_bytes());
            ZStr::from(v)
        }
        flags::PROTECTED => {
            let mut v = b"\0*\0".to_vec();
            v.extend_from_slice(pi.name.as_bytes());
            ZStr::from(v)
        }
        _ => pi.name.clone(),
    }
}

/// `zend_std_get_properties()` as an array: initialised declared
/// properties (mangled names) followed by dynamic ones.
pub(crate) fn std_props(o: &Obj) -> ZArray {
    let d = object::data(o);
    let mut a = ZArray::new();
    {
        let t = a.make_mut();
        let slots = d.slots.borrow();
        for pi in &d.class.props {
            if pi.is_virtual() || pi.flags & flags::STATIC != 0 {
                continue;
            }
            if let Some(Some(v)) = slots.get(pi.slot as usize) {
                t.insert(ArrayKey::Str(mangle(pi)), v.deref());
            }
        }
        if let Some(dynp) = d.dynamic.borrow().as_deref() {
            for (k, v) in dynp {
                t.insert(k.clone(), v.deref());
            }
        }
    }
    a
}

/// `update_property()` used by `__unserialize()` to restore custom
/// properties: mangled private/protected names go to the declared slot.
pub(crate) fn restore_property(vm: &mut Vm, o: &Obj, key: &ZStr, v: &Value) -> VmResult<()> {
    let b = key.as_bytes();
    if b.first() == Some(&0) {
        let rest = &b[1..];
        let Some(p) = rest.iter().position(|&c| c == 0) else {
            return Ok(());
        };
        let cls_name = &rest[..p];
        let prop = &rest[p + 1..];
        if cls_name != b"*" && vm.find_class(cls_name).is_none() {
            return Ok(());
        }
        vm.set_property_raw(o, &String::from_utf8_lossy(prop), v.clone());
        return Ok(());
    }
    vm.set_property(o, &key.to_string_lossy(), v.clone())
}

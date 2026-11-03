//! `DatePeriod`: construction overloads, ISO 8601 strings, iteration,
//! the read-only property handlers and (un)serialisation.

use std::rc::Rc;

use php_date::api::{self, Interval, IntervalKind};
use php_date::parse::parse_iso_interval;
use php_date::{RelTime, Time};
use php_runtime::{Obj, PropertyPurpose, Value, ZArray, ZStr};
use php_vm::class::Class;
use php_vm::ext::{
    acc, Args, ClassBuilder, ClassHooks, ModuleBuilder, NativeIterator, PropCtx, Throw, VmResult,
};
use php_vm::object;
use php_vm::Vm;

use crate::common::{get_str, lossy, set_str, E_DEPRECATED};
use crate::datetime::{add_common_properties, clone_hook, restore_custom, wakeup_deprecation};
use crate::objects::{
    class, instantiate, interval_of, new_date, new_interval, period_of, set_time, std_props,
    time_of, uninit_error, value_instance_of, with_period, PeriodObj,
};

/// `INT_MAX - 8`.
const MAX_RECURRENCES: i64 = (i32::MAX as i64) - 8;

/// The properties served by the period handlers.
const INTERNAL_PROPS: [&str; 7] = [
    "start",
    "current",
    "end",
    "interval",
    "recurrences",
    "include_start_date",
    "include_end_date",
];

fn is_internal(name: &[u8]) -> bool {
    INTERNAL_PROPS.iter().any(|p| p.as_bytes() == name)
}

/// `date_period_advance()`.
fn advance(t: &mut Time, interval: &RelTime) {
    t.have_relative = true;
    t.relative = interval.clone();
    t.sse_uptodate = false;
    t.update_ts(None);
    t.update_from_sse();
}

/// `create_date_period_datetime()`.
fn date_value(vm: &mut Vm, t: Option<&Time>, ce: Option<&Rc<Class>>) -> VmResult<Value> {
    match (t, ce) {
        (Some(t), Some(ce)) => Ok(Value::Object(new_date(vm, ce, t.clone())?)),
        (Some(t), None) => {
            let ce = class(vm, "DateTime");
            Ok(Value::Object(new_date(vm, &ce, t.clone())?))
        }
        _ => Ok(Value::Null),
    }
}

/// `create_date_period_interval()`.
fn interval_value(vm: &mut Vm, r: Option<&RelTime>) -> VmResult<Value> {
    match r {
        Some(r) => Ok(Value::Object(new_interval(
            vm,
            Interval {
                rel: r.clone(),
                kind: IntervalKind::Civil,
                from_string: false,
                date_string: None,
            },
        )?)),
        None => Ok(Value::Null),
    }
}

/// `get_base_date_class()`.
fn base_date_class(ce: &Rc<Class>) -> Rc<Class> {
    let mut c = ce.clone();
    loop {
        let lc = c.lc_name.as_bytes();
        if lc == b"datetime" || lc == b"datetimeimmutable" {
            return c;
        }
        match c.parent.clone() {
            Some(p) => c = p,
            None => return c,
        }
    }
}

fn func_error(vm: &mut Vm, class_name: &str, func: &str, msg: &str) -> Throw {
    vm.throw(class_name, &format!("{func}(): {msg}"))
}

/// `date_period_init_finish()`.
fn init_finish(
    vm: &mut Vm,
    p: &mut PeriodObj,
    options: i64,
    recurrences: i64,
    func: &str,
) -> VmResult<()> {
    if p.end.is_none() && !(1..=MAX_RECURRENCES).contains(&recurrences) {
        return Err(func_error(
            vm,
            "DateMalformedPeriodStringException",
            func,
            &format!(
                "Recurrence count must be greater or equal to 1 and lower than {}",
                MAX_RECURRENCES + 1
            ),
        ));
    }
    p.include_start_date = options & api::EXCLUDE_START_DATE == 0;
    p.include_end_date = options & api::INCLUDE_END_DATE != 0;
    let rec = recurrences + i64::from(p.include_start_date) + i64::from(p.include_end_date);
    if rec > MAX_RECURRENCES {
        return Err(func_error(
            vm,
            "DateMalformedStringException",
            func,
            &format!(
                "Recurrence count must be greater or equal to 1 and lower than {} (including options)",
                MAX_RECURRENCES + 1
            ),
        ));
    }
    p.recurrences = rec;
    p.initialized = true;
    Ok(())
}

/// `date_period_init_iso8601_string()`; returns the recurrence count.
fn init_iso(vm: &mut Vm, p: &mut PeriodObj, base: &str, iso: &[u8], func: &str) -> VmResult<i64> {
    let r = parse_iso_interval(iso);
    let s = lossy(iso);
    if !r.errors.errors.is_empty() {
        return Err(vm.throw(
            "DateMalformedPeriodStringException",
            &format!("Unknown or bad format ({s})"),
        ));
    }
    p.start = r.begin;
    p.end = r.end;
    p.interval = r.period;
    let recurrences = r.recurrences.unwrap_or(0);
    if p.start.is_none() {
        return Err(func_error(
            vm,
            "DateMalformedPeriodStringException",
            func,
            &format!("ISO interval must contain a start date, \"{s}\" given"),
        ));
    }
    if p.interval.is_none() {
        return Err(func_error(
            vm,
            "DateMalformedPeriodStringException",
            func,
            &format!("ISO interval must contain an interval, \"{s}\" given"),
        ));
    }
    if p.end.is_none() && recurrences == 0 {
        return Err(func_error(
            vm,
            "DateMalformedPeriodStringException",
            func,
            &format!("ISO interval must contain an end date or a recurrence count, \"{s}\" given"),
        ));
    }
    if let Some(t) = p.start.as_mut() {
        t.update_ts(None);
    }
    if let Some(t) = p.end.as_mut() {
        t.update_ts(None);
    }
    p.start_ce = Some(class(vm, base));
    Ok(recurrences)
}

fn create_from_iso(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    let options = if a.has(1) { a.int(1) } else { 0 };
    let cls = a.called.clone().unwrap_or_else(|| class(vm, "DatePeriod"));
    let o = instantiate(vm, &cls)?;
    let mut p = PeriodObj::default();
    let func = "DatePeriod::createFromISO8601String";
    let rec = init_iso(vm, &mut p, "DateTimeImmutable", s.as_bytes(), func)?;
    init_finish(vm, &mut p, options, rec, func)?;
    object::set_internal(&o, p);
    Ok(Value::Object(o))
}

/// Weak `l` coercion for the overload detection (no diagnostics).
fn quiet_long(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => Some(*i),
        Value::Bool(b) => Some(i64::from(*b)),
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        Value::Float(f) if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.2e18 => Some(*f as i64),
        Value::Str(s) => std::str::from_utf8(s.as_bytes())
            .ok()
            .and_then(|s| s.trim_start().parse::<i64>().ok()),
        Value::Null => Some(0),
        _ => None,
    }
}

enum Overload {
    Recurrences(Obj, Obj, i64, i64),
    End(Obj, Obj, Obj, i64),
    Iso(Vec<u8>, i64),
}

fn parse_overload(a: &Args) -> Option<Overload> {
    let n = a.len();
    let opt = |i: usize| {
        if a.has(i) {
            quiet_long(&a.at(i))
        } else {
            Some(0)
        }
    };
    if (3..=4).contains(&n) {
        let start = value_instance_of(&a.at(0), b"datetimeinterface");
        let iv = value_instance_of(&a.at(1), b"dateinterval");
        if let (Some(s), Some(i)) = (start, iv) {
            if !matches!(a.at(2), Value::Object(_)) {
                if let (Some(r), Some(o)) = (quiet_long(&a.at(2)), opt(3)) {
                    return Some(Overload::Recurrences(s, i, r, o));
                }
            }
            if let (Some(e), Some(o)) = (value_instance_of(&a.at(2), b"datetimeinterface"), opt(3))
            {
                return Some(Overload::End(s, i, e, o));
            }
        }
    }
    if (1..=2).contains(&n) {
        let s = match a.at(0) {
            Value::Str(s) => Some(s.as_bytes().to_vec()),
            Value::Int(i) => Some(i.to_string().into_bytes()),
            Value::Null => Some(Vec::new()),
            _ => None,
        };
        if let (Some(s), Some(o)) = (s, opt(1)) {
            return Some(Overload::Iso(s, o));
        }
    }
    None
}

fn construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let Some(ov) = parse_overload(a) else {
        return Err(vm.throw("TypeError", api::messages::PERIOD_CTOR_SIGNATURES));
    };
    let func = "DatePeriod::__construct";
    let mut p = PeriodObj::default();
    let (options, rec) = match ov {
        Overload::Iso(s, options) => {
            if matches!(a.at(0), Value::Null) {
                vm.php_deprecated(
                    "Passing null to parameter #1 ($start) of type string is deprecated",
                )?;
            }
            vm.raise(E_DEPRECATED, api::messages::PERIOD_STRING_CTOR_DEPRECATED)?;
            let rec = init_iso(vm, &mut p, "DateTime", &s, func)?;
            (options, rec)
        }
        Overload::Recurrences(start, iv, rec, options) => {
            init_from_objects(vm, &mut p, &start, &iv, None)?;
            (options, rec)
        }
        Overload::End(start, iv, end, options) => {
            init_from_objects(vm, &mut p, &start, &iv, Some(&end))?;
            (options, 0)
        }
    };
    init_finish(vm, &mut p, options, rec, func)?;
    object::set_internal(&this, p);
    Ok(Value::Null)
}

fn init_from_objects(
    vm: &mut Vm,
    p: &mut PeriodObj,
    start: &Obj,
    iv: &Obj,
    end: Option<&Obj>,
) -> VmResult<()> {
    let iface = class(vm, "DateTimeInterface");
    let Some(st) = time_of(start) else {
        return Err(uninit_error(vm, &iface));
    };
    let et = match end {
        Some(e) => match time_of(e) {
            Some(t) => Some(t),
            None => return Err(uninit_error(vm, &iface)),
        },
        None => None,
    };
    let Some(i) = interval_of(iv) else {
        return Err(uninit_error(vm, object::class_of(iv)));
    };
    p.start = Some(st);
    p.start_ce = Some(object::class_of(start).clone());
    p.interval = Some(i.rel);
    p.end = et;
    Ok(())
}

fn get_start_date(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let p = period_of(&this);
    let Some(start) = p.start.as_ref() else {
        return Err(uninit_error(vm, object::class_of(&this)));
    };
    date_value(vm, Some(start), p.start_ce.as_ref())
}

fn get_end_date(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = period_of(&a.this_obj());
    date_value(vm, p.end.as_ref(), p.start_ce.as_ref())
}

fn get_date_interval(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let p = period_of(&this);
    if p.interval.is_none() {
        return Err(uninit_error(vm, object::class_of(&this)));
    }
    interval_value(vm, p.interval.as_ref())
}

#[allow(clippy::unnecessary_wraps)]
fn get_recurrences(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = period_of(&a.this_obj());
    let r = p.recurrences - i64::from(p.include_start_date) - i64::from(p.include_end_date);
    Ok(if r == 0 { Value::Null } else { Value::Int(r) })
}

/// `zend_create_internal_iterator_zval()` over the native iterator.
fn get_iterator_m(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let it = PeriodIter {
        obj: a.this_obj(),
        index: 0,
    };
    Ok(Value::Object(vm.new_internal_iterator(Box::new(it))?))
}

// ---------------------------------------------------------------------------
// Serialisation
// ---------------------------------------------------------------------------

/// `date_period_object_to_hash()`.
fn add_period_props(vm: &mut Vm, props: &mut ZArray, p: &PeriodObj) -> VmResult<()> {
    let start = date_value(vm, p.start.as_ref(), p.start_ce.as_ref())?;
    set_str(props, "start", start);
    let current = date_value(vm, p.current.as_ref(), p.start_ce.as_ref())?;
    set_str(props, "current", current);
    let end = date_value(vm, p.end.as_ref(), p.start_ce.as_ref())?;
    set_str(props, "end", end);
    let iv = interval_value(vm, p.interval.as_ref())?;
    set_str(props, "interval", iv);
    set_str(props, "recurrences", Value::Int(p.recurrences));
    set_str(
        props,
        "include_start_date",
        Value::Bool(p.include_start_date),
    );
    set_str(props, "include_end_date", Value::Bool(p.include_end_date));
    Ok(())
}

/// A date entry of the restore hash: `Ok(None)` for null, `Err(())` for
/// invalid data.
fn hash_date(h: &ZArray, key: &str) -> Result<Option<(Time, Rc<Class>)>, ()> {
    match get_str(h, key).map(Value::deref) {
        Some(Value::Null) => Ok(None),
        Some(v) => match value_instance_of(&v, b"datetimeinterface") {
            Some(o) => match time_of(&o) {
                Some(t) => Ok(Some((t, object::class_of(&o).clone()))),
                None => Err(()),
            },
            None => Err(()),
        },
        None => Err(()),
    }
}

/// `php_date_period_initialize_from_hash()` (no rollback on error, like
/// PHP).
fn from_hash(p: &mut PeriodObj, h: &ZArray) -> bool {
    let Ok(start) = hash_date(h, "start") else {
        return false;
    };
    if let Some((t, ce)) = start {
        p.start = Some(t);
        p.start_ce = Some(ce);
    }
    let Ok(end) = hash_date(h, "end") else {
        return false;
    };
    if let Some((t, _)) = end {
        if p.start_ce.is_none() {
            return false;
        }
        p.end = Some(t);
    }
    let Ok(current) = hash_date(h, "current") else {
        return false;
    };
    if let Some((t, _)) = current {
        if p.start_ce.is_none() {
            return false;
        }
        p.current = Some(t);
    }
    match get_str(h, "interval").map(Value::deref) {
        Some(Value::Object(o)) if object::class_of(&o).lc_name.as_bytes() == b"dateinterval" => {
            match interval_of(&o) {
                Some(iv) => p.interval = Some(iv.rel),
                None => return false,
            }
        }
        _ => return false,
    }
    match get_str(h, "recurrences").map(Value::deref) {
        Some(Value::Int(r)) if (0..=i64::from(i32::MAX)).contains(&r) => p.recurrences = r,
        _ => return false,
    }
    match get_str(h, "include_start_date").map(Value::deref) {
        Some(Value::Bool(b)) => p.include_start_date = b,
        _ => return false,
    }
    match get_str(h, "include_end_date").map(Value::deref) {
        Some(Value::Bool(b)) => p.include_end_date = b,
        _ => return false,
    }
    p.initialized = true;
    true
}

fn invalid(vm: &mut Vm) -> Throw {
    vm.throw("Error", "Invalid serialization data for DatePeriod object")
}

fn restore(vm: &mut Vm, o: &Obj, h: &ZArray) -> VmResult<()> {
    let mut p = period_of(o);
    let ok = from_hash(&mut p, h);
    object::set_internal(o, p);
    if ok {
        Ok(())
    } else {
        Err(invalid(vm))
    }
}

fn set_state(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = a.array(0);
    let cls = class(vm, "DatePeriod");
    let o = instantiate(vm, &cls)?;
    restore(vm, &o, &h)?;
    Ok(Value::Object(o))
}

fn serialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let p = period_of(&o);
    if p.start.is_none() {
        return Err(uninit_error(vm, object::class_of(&o)));
    }
    let mut out = ZArray::new();
    add_period_props(vm, &mut out, &p)?;
    add_common_properties(&mut out, &o);
    Ok(Value::Array(out))
}

fn unserialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let h = a.array(0);
    restore(vm, &o, &h)?;
    restore_custom(vm, &o, &h, &INTERNAL_PROPS)?;
    Ok(Value::Null)
}

fn wakeup(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    wakeup_deprecation(vm, "DatePeriod")?;
    let o = a.this_obj();
    let h = std_props(&o);
    restore(vm, &o, &h)?;
    restore_custom(vm, &o, &h, &INTERNAL_PROPS)?;
    Ok(Value::Null)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

fn create_hook(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    object::set_internal(o, PeriodObj::default());
    Ok(())
}

fn properties_for(vm: &mut Vm, o: &Obj, purpose: PropertyPurpose) -> VmResult<Option<ZArray>> {
    if purpose == PropertyPurpose::Debug && object::class_of(o).magic.debug_info.is_some() {
        return Ok(None);
    }
    let mut props = std_props(o);
    let p = period_of(o);
    if p.initialized {
        add_period_props(vm, &mut props, &p)?;
    }
    Ok(Some(props))
}

fn read_one(vm: &mut Vm, p: &PeriodObj, name: &[u8]) -> VmResult<Value> {
    Ok(match name {
        b"start" => date_value(vm, p.start.as_ref(), p.start_ce.as_ref())?,
        b"current" => date_value(vm, p.current.as_ref(), p.start_ce.as_ref())?,
        b"end" => date_value(vm, p.end.as_ref(), p.start_ce.as_ref())?,
        b"interval" => interval_value(vm, p.interval.as_ref())?,
        b"recurrences" => Value::Int(p.recurrences),
        b"include_start_date" => Value::Bool(p.include_start_date),
        b"include_end_date" => Value::Bool(p.include_end_date),
        _ => Value::Null,
    })
}

fn read_property(vm: &mut Vm, o: &Obj, name: &ZStr) -> VmResult<Option<Value>> {
    if !is_internal(name.as_bytes()) {
        return Ok(None);
    }
    if !matches!(vm.property_context(), PropCtx::Read | PropCtx::Isset) {
        return Err(readonly_error(vm, name));
    }
    let p = period_of(o);
    read_one(vm, &p, name.as_bytes()).map(Some)
}

fn readonly_error(vm: &mut Vm, name: &ZStr) -> Throw {
    vm.throw(
        "Error",
        &format!(
            "Cannot modify readonly property DatePeriod::${}",
            name.to_string_lossy()
        ),
    )
}

fn write_property(vm: &mut Vm, _o: &Obj, name: &ZStr, _v: &Value) -> VmResult<bool> {
    if is_internal(name.as_bytes()) {
        return Err(readonly_error(vm, name));
    }
    Ok(false)
}

fn has_property(vm: &mut Vm, o: &Obj, name: &ZStr, check_empty: bool) -> VmResult<Option<bool>> {
    if !is_internal(name.as_bytes()) {
        return Ok(None);
    }
    if vm.property_context() == PropCtx::Exists {
        return Ok(Some(true));
    }
    let p = period_of(o);
    if !p.initialized {
        return Ok(Some(false));
    }
    let v = read_one(vm, &p, name.as_bytes())?;
    if check_empty {
        Ok(Some(vm.value_to_bool(&v)?))
    } else {
        Ok(Some(!matches!(v, Value::Null)))
    }
}

fn unset_property(vm: &mut Vm, o: &Obj, name: &ZStr) -> VmResult<bool> {
    if is_internal(name.as_bytes()) {
        let cls = object::display_name(&object::class_of(o).name).to_string_lossy();
        return Err(vm.throw(
            "Error",
            &format!("Cannot unset {cls}::${}", name.to_string_lossy()),
        ));
    }
    Ok(false)
}

/// The `foreach` iterator (`date_period_it`).
struct PeriodIter {
    obj: Obj,
    index: i64,
}

impl NativeIterator for PeriodIter {
    fn rewind(&mut self, vm: &mut Vm) -> VmResult<()> {
        self.index = 0;
        let ok = with_period(&self.obj, |p| {
            let Some(start) = p.start.clone() else {
                p.current = None;
                return false;
            };
            let mut cur = start;
            if !p.include_start_date {
                if let Some(iv) = p.interval.clone() {
                    advance(&mut cur, &iv);
                }
            }
            p.current = Some(cur);
            true
        })
        .unwrap_or(false);
        if !ok {
            let cls = class(vm, "DatePeriod");
            return Err(uninit_error(vm, &cls));
        }
        Ok(())
    }

    fn valid(&mut self, _vm: &mut Vm) -> VmResult<bool> {
        let index = self.index;
        Ok(with_period(&self.obj, |p| {
            let Some(cur) = &p.current else {
                return false;
            };
            if let Some(end) = &p.end {
                if cur.sse == end.sse {
                    return if p.include_end_date {
                        cur.us <= end.us
                    } else {
                        cur.us < end.us
                    };
                }
                cur.sse < end.sse
            } else {
                index < p.recurrences
            }
        })
        .unwrap_or(false))
    }

    fn current(&mut self, vm: &mut Vm) -> VmResult<Value> {
        let p = period_of(&self.obj);
        let Some(cur) = p.current else {
            return Ok(Value::Null);
        };
        let ce = match &p.start_ce {
            Some(c) => base_date_class(c),
            None => class(vm, "DateTime"),
        };
        let o = instantiate(vm, &ce)?;
        set_time(&o, Some(cur));
        Ok(Value::Object(o))
    }

    fn key(&mut self, _vm: &mut Vm) -> VmResult<Value> {
        Ok(Value::Int(self.index))
    }

    fn next(&mut self, _vm: &mut Vm) -> VmResult<()> {
        with_period(&self.obj, |p| {
            if let (Some(cur), Some(iv)) = (p.current.as_mut(), p.interval.as_ref()) {
                advance(cur, iv);
            }
        });
        self.index += 1;
        Ok(())
    }
}

/// `zend_user_it_get_new_iterator()`: drives the `Iterator` returned by a
/// user override of `getIterator()`.
struct UserIter {
    it: Obj,
}

impl NativeIterator for UserIter {
    fn rewind(&mut self, vm: &mut Vm) -> VmResult<()> {
        vm.call_method_by_name(&self.it, "rewind", Vec::new())
            .map(|_| ())
    }

    fn valid(&mut self, vm: &mut Vm) -> VmResult<bool> {
        let v = vm.call_method_by_name(&self.it, "valid", Vec::new())?;
        vm.value_to_bool(&v)
    }

    fn current(&mut self, vm: &mut Vm) -> VmResult<Value> {
        vm.call_method_by_name(&self.it, "current", Vec::new())
    }

    fn key(&mut self, vm: &mut Vm) -> VmResult<Value> {
        vm.call_method_by_name(&self.it, "key", Vec::new())
    }

    fn next(&mut self, vm: &mut Vm) -> VmResult<()> {
        vm.call_method_by_name(&self.it, "next", Vec::new())
            .map(|_| ())
    }
}

/// Whether a subclass overrides `getIterator()`.
fn user_get_iterator(o: &Obj) -> bool {
    object::class_of(o)
        .find_method(b"getiterator")
        .and_then(|m| m.scope.upgrade())
        .is_some_and(|c| c.lc_name.as_bytes() != b"dateperiod")
}

/// Resolves the `Iterator` a user `getIterator()` returns (following nested
/// `IteratorAggregate`s).
fn user_iterator(vm: &mut Vm, o: &Obj) -> VmResult<Obj> {
    let mut cur = o.clone();
    loop {
        let r = vm.call_method_by_name(&cur, "getIterator", Vec::new())?;
        let Value::Object(it) = r else {
            let cls = object::display_name(&object::class_of(o).name).to_string_lossy();
            return Err(vm.throw(
                "Exception",
                &format!("Objects returned by {cls}::getIterator() must be traversable or implement interface Iterator"),
            ));
        };
        if object::class_of(&it).instance_of_name(b"iterator") {
            return Ok(it);
        }
        if !object::class_of(&it).instance_of_name(b"iteratoraggregate") {
            let cls = object::display_name(&object::class_of(o).name).to_string_lossy();
            return Err(vm.throw(
                "Exception",
                &format!("Objects returned by {cls}::getIterator() must be traversable or implement interface Iterator"),
            ));
        }
        cur = it;
    }
}

fn get_iterator(vm: &mut Vm, o: &Obj, by_ref: bool) -> VmResult<Box<dyn NativeIterator>> {
    if user_get_iterator(o) {
        let it = user_iterator(vm, o)?;
        return Ok(Box::new(UserIter { it }));
    }
    if by_ref {
        return Err(vm.throw("Error", api::messages::PERIOD_BY_REFERENCE));
    }
    Ok(Box::new(PeriodIter {
        obj: o.clone(),
        index: 0,
    }))
}

/// Registers `DatePeriod`.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    let vm = m.vm();
    let p = acc::PUBLIC;
    let ps = acc::PUBLIC | acc::STATIC;
    ClassBuilder::new("DatePeriod")
        .implements(vm, &["IteratorAggregate"])
        .constant("EXCLUDE_START_DATE", Value::Int(api::EXCLUDE_START_DATE))
        .constant("INCLUDE_END_DATE", Value::Int(api::INCLUDE_END_DATE))
        .prop("start", p, Some("?DateTimeInterface"), None)
        .prop("current", p, Some("?DateTimeInterface"), None)
        .prop("end", p, Some("?DateTimeInterface"), None)
        .prop("interval", p, Some("?DateInterval"), None)
        .prop("recurrences", p, Some("int"), None)
        .prop("include_start_date", p, Some("bool"), None)
        .prop("include_end_date", p, Some("bool"), None)
        .method(
            "createFromISO8601String(string $specification, int $options = 0): static",
            ps,
            create_from_iso,
        )
        .method("__construct($start = UNKNOWN, $interval = UNKNOWN, $end = UNKNOWN, $options = UNKNOWN)", p, construct)
        .method("getStartDate(): DateTimeInterface", p, get_start_date)
        .method("getEndDate(): ?DateTimeInterface", p, get_end_date)
        .method("getDateInterval(): DateInterval", p, get_date_interval)
        .method("getRecurrences(): ?int", p, get_recurrences)
        .method("__serialize(): array", p, serialize)
        .method("__unserialize(array $data): void", p, unserialize)
        .method("__wakeup(): void", p, wakeup)
        .method("__set_state(array $array): static", ps, set_state)
        .method("getIterator(): Iterator", p, get_iterator_m)
        .hooks(ClassHooks {
            create: Some(create_hook),
            clone: Some(clone_hook),
            properties_for: Some(properties_for),
            read_property: Some(read_property),
            write_property: Some(write_property),
            has_property: Some(has_property),
            unset_property: Some(unset_property),
            get_iterator: Some(get_iterator),
            ..ClassHooks::default()
        })
        .register(vm);
}

//! `DateInterval` and `date_interval_*()`.

use php_date::api::{self, DateStringIntervalError, Interval, IntervalKind};
use php_date::{RelTime, UNSET};
use php_runtime::{Obj, PropertyPurpose, Value, ZArray, ZStr};
use php_vm::ext::{acc, Args, ClassBuilder, ClassHooks, ModuleBuilder, VmResult};
use php_vm::object;
use php_vm::Vm;

use crate::common::{get_str, lossy, to_php};
use crate::datetime::{
    add_common_properties, clone_hook, obj_arg, restore_custom, wakeup_deprecation,
};
use crate::objects::{
    class, instantiate, interval_of, new_interval, require_interval, set_interval, std_props,
    IntervalObj,
};

/// The properties backed by the native interval.
const INTERNAL_PROPS: [&str; 11] = [
    "date_string",
    "from_string",
    "y",
    "m",
    "d",
    "h",
    "i",
    "s",
    "f",
    "invert",
    "days",
];

fn construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    match api::interval_from_iso(s.as_bytes()) {
        Ok(iv) => {
            set_interval(&a.this_obj(), Some(iv));
            Ok(Value::Null)
        }
        Err(m) => Err(vm.throw("DateMalformedIntervalStringException", &lossy(&m))),
    }
}

fn create_from_date_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    match api::interval_from_date_string(s.as_bytes()) {
        Ok(iv) => Ok(Value::Object(new_interval(vm, iv)?)),
        Err(DateStringIntervalError::BadFormat(m) | DateStringIntervalError::NonRelative(m)) => {
            Err(vm.throw("DateMalformedIntervalStringException", &lossy(&m)))
        }
    }
}

fn date_interval_create_from_date_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    match api::interval_from_date_string(s.as_bytes()) {
        Ok(iv) => Ok(Value::Object(new_interval(vm, iv)?)),
        Err(DateStringIntervalError::BadFormat(m) | DateStringIntervalError::NonRelative(m)) => {
            vm.php_warning(&lossy(&m))?;
            Ok(Value::Bool(false))
        }
    }
}

fn format(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, n) = match &a.this {
        Some(o) => (o.clone(), 0),
        None => (obj_arg(a, 0).unwrap_or_else(|| a.this_obj()), 1),
    };
    let f = a.str(n);
    let iv = require_interval(vm, &o)?;
    Ok(Value::Str(ZStr::from(iv.format(f.as_bytes()))))
}

/// `date_interval_object_to_hash()`.
fn add_interval_props(props: &mut ZArray, iv: &Interval) {
    if let Value::Array(d) = to_php(&iv.props()) {
        for (k, v) in d.iter() {
            props.make_mut().insert(k.clone(), v.clone());
        }
    }
}

/// `zval_get_long()` for the hash restore (silent).
fn get_long(vm: &mut Vm, v: &Value) -> VmResult<i64> {
    vm.value_to_int(v)
}

/// Whether `v`'s type is at most `IS_STRING` (scalar or null).
fn scalar(v: &Value) -> bool {
    matches!(
        v,
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_)
    )
}

/// `strtoll()` of the string form of `v` (`DATE_A64I`).
fn a64i(vm: &mut Vm, v: &Value) -> VmResult<i64> {
    let s = vm.value_to_string(v)?;
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    let neg = i < b.len() && b[i] == b'-';
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    let mut n: i64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        n = n.saturating_mul(10).saturating_add(i64::from(b[i] - b'0'));
        i += 1;
    }
    Ok(if neg { -n } else { n })
}

/// `php_date_interval_initialize_from_hash()`.
#[allow(clippy::cast_possible_truncation)]
fn from_hash(vm: &mut Vm, h: &ZArray) -> VmResult<Interval> {
    if let Some(Value::Str(s)) = get_str(h, "date_string").map(Value::deref) {
        return match Interval::from_date_string_property(s.as_bytes()) {
            Ok(iv) => Ok(iv),
            Err(m) => Err(vm.throw("Error", &lossy(&m))),
        };
    }
    let mut r = RelTime::default();
    let long = |vm: &mut Vm, key: &str, def: i64| -> VmResult<i64> {
        match get_str(h, key).map(Value::deref) {
            Some(v) if scalar(&v) => get_long(vm, &v),
            _ => Ok(def),
        }
    };
    r.y = long(vm, "y", -1)?;
    r.m = long(vm, "m", -1)?;
    r.d = long(vm, "d", -1)?;
    r.h = long(vm, "h", -1)?;
    r.i = long(vm, "i", -1)?;
    r.s = long(vm, "s", -1)?;
    if let Some(v) = get_str(h, "f").map(Value::deref) {
        let f = vm.value_to_float(&v)? * 1_000_000.0;
        r.us = if f.is_finite() && f.abs() < 9.2e18 {
            f as i64
        } else {
            0
        };
    }
    r.weekday = long(vm, "weekday", -1)? as i32;
    r.weekday_behavior = long(vm, "weekday_behavior", -1)? as i32;
    r.first_last_day_of = long(vm, "first_last_day_of", -1)? as i32;
    r.invert = long(vm, "invert", 0)? as i32;
    r.days = match get_str(h, "days").map(Value::deref) {
        Some(Value::Bool(false)) => UNSET,
        Some(v) if scalar(&v) => a64i(vm, &v)?,
        _ => -1,
    };
    r.special_type = long(vm, "special_type", 0)? as u32;
    r.special_amount = match get_str(h, "special_amount").map(Value::deref) {
        Some(v) if scalar(&v) => a64i(vm, &v)?,
        _ => -1,
    };
    r.have_weekday_relative = long(vm, "have_weekday_relative", 0)? != 0;
    r.have_special_relative = long(vm, "have_special_relative", 0)? != 0;
    let kind = match get_str(h, "civil_or_wall").map(Value::deref) {
        Some(v) if get_long(vm, &v)? == 2 => IntervalKind::Wall,
        _ => IntervalKind::Civil,
    };
    Ok(Interval {
        rel: r,
        kind,
        from_string: false,
        date_string: None,
    })
}

fn set_state(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = a.array(0);
    let cls = class(vm, "DateInterval");
    let o = instantiate(vm, &cls)?;
    let iv = from_hash(vm, &h)?;
    set_interval(&o, Some(iv));
    Ok(Value::Object(o))
}

fn serialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let iv = require_interval(vm, &o)?;
    let mut out = ZArray::new();
    add_interval_props(&mut out, &iv);
    add_common_properties(&mut out, &o);
    Ok(Value::Array(out))
}

fn unserialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let h = a.array(0);
    let iv = from_hash(vm, &h)?;
    set_interval(&o, Some(iv));
    restore_custom(vm, &o, &h, &INTERNAL_PROPS)?;
    Ok(Value::Null)
}

fn wakeup(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    wakeup_deprecation(vm, "DateInterval")?;
    let o = a.this_obj();
    let h = std_props(&o);
    let iv = from_hash(vm, &h)?;
    set_interval(&o, Some(iv));
    Ok(Value::Null)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

fn create_hook(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    object::set_internal(o, IntervalObj::default());
    Ok(())
}

/// `date_object_get_properties_interval()`.
fn properties_for(_vm: &mut Vm, o: &Obj, purpose: PropertyPurpose) -> VmResult<Option<ZArray>> {
    if purpose == PropertyPurpose::Debug && object::class_of(o).magic.debug_info.is_some() {
        return Ok(None);
    }
    let mut props = std_props(o);
    if let Some(iv) = interval_of(o) {
        add_interval_props(&mut props, &iv);
    }
    Ok(Some(props))
}

/// `date_interval_read_property()`.
fn read_property(_vm: &mut Vm, o: &Obj, name: &ZStr) -> VmResult<Option<Value>> {
    let Some(iv) = interval_of(o) else {
        return Ok(None);
    };
    Ok(std::str::from_utf8(name.as_bytes())
        .ok()
        .and_then(|n| iv.read_prop(n))
        .map(|v| to_php(&v)))
}

/// `date_interval_write_property()`.
fn write_property(vm: &mut Vm, o: &Obj, name: &ZStr, v: &Value) -> VmResult<bool> {
    let Some(mut iv) = interval_of(o) else {
        return Ok(false);
    };
    let n = name.as_bytes();
    if n == b"f" {
        let f = vm.value_to_float(v)?;
        iv.write_f(f);
    } else if matches!(n, b"y" | b"m" | b"d" | b"h" | b"i" | b"s" | b"invert") {
        let i = vm.value_to_int(v)?;
        iv.write_prop(&lossy(n), i);
    } else {
        return Ok(false);
    }
    set_interval(o, Some(iv));
    Ok(true)
}

/// `date_interval_has_property()`.
fn has_property(vm: &mut Vm, o: &Obj, name: &ZStr, check_empty: bool) -> VmResult<Option<bool>> {
    let Some(v) = read_property(vm, o, name)? else {
        return Ok(None);
    };
    if check_empty {
        Ok(Some(vm.value_to_bool(&v)?))
    } else {
        Ok(Some(!matches!(v, Value::Null)))
    }
}

/// `date_interval_compare_objects()`.
fn compare(vm: &mut Vm, a: &Value, b: &Value) -> VmResult<Option<i32>> {
    let (Value::Object(x), Value::Object(y)) = (a, b) else {
        return Ok(None);
    };
    if !object::has_internal::<IntervalObj>(x) || !object::has_internal::<IntervalObj>(y) {
        return Ok(None);
    }
    crate::timezone::warn(vm, api::messages::CANNOT_COMPARE_INTERVALS)?;
    Ok(Some(1))
}

/// Registers `DateInterval` and its functions.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    let vm = m.vm();
    let p = acc::PUBLIC;
    let ps = acc::PUBLIC | acc::STATIC;
    ClassBuilder::new("DateInterval")
        .method("__construct(string $duration)", p, construct)
        .method(
            "createFromDateString(string $datetime): DateInterval",
            ps,
            create_from_date_string,
        )
        .method("format(string $format): string", p, format)
        .method("__serialize(): array", p, serialize)
        .method("__unserialize(array $data): void", p, unserialize)
        .method("__wakeup(): void", p, wakeup)
        .method("__set_state(array $array): static", ps, set_state)
        .hooks(ClassHooks {
            create: Some(create_hook),
            clone: Some(clone_hook),
            properties_for: Some(properties_for),
            read_property: Some(read_property),
            write_property: Some(write_property),
            has_property: Some(has_property),
            compare: Some(compare),
            ..ClassHooks::default()
        })
        .register(vm);

    m.function(
        "date_interval_create_from_date_string(string $datetime): DateInterval|false",
        date_interval_create_from_date_string,
    )
    .function(
        "date_interval_format(DateInterval $object, string $format): string",
        format,
    );
}

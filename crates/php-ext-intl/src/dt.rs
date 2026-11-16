// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Glue with ext/date: reading `DateTimeZone`/`DateTimeInterface` objects
//! and building them, through their public PHP API.

use php_intl::calendar::PhpDateTime;
use php_intl::timezone::{DateTimeZoneSpec, PhpZone};
use php_runtime::{Obj, Value, ZStr};
use php_vm::ext::{Throw, VmResult};
use php_vm::Vm;

/// Whether `o` is an instance of the class `name` (or a subclass).
pub(crate) fn instance_of(o: &Obj, name: &str) -> bool {
    php_vm::object::class_of(o).instance_of_name(name.to_ascii_lowercase().as_bytes())
}

/// Parses `"+05:30"`-style names into seconds.
fn parse_offset(name: &str) -> Option<i64> {
    let (sign, rest) = match name.as_bytes().first()? {
        b'+' => (1, &name[1..]),
        b'-' => (-1, &name[1..]),
        _ => return None,
    };
    let mut parts = rest.split(':');
    let h: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    let s: i64 = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    Some(sign * (h * 3600 + m * 60 + s))
}

/// The zone of a `DateTimeZone` object.
pub(crate) fn php_zone(vm: &mut Vm, tz: &Obj) -> VmResult<PhpZone> {
    let name = vm.call_method_by_name(tz, "getName", Vec::new())?;
    let name = vm.value_to_string(&name)?.to_string_lossy();
    if let Some(secs) = parse_offset(&name) {
        return Ok(PhpZone::Offset(secs));
    }
    Ok(PhpZone::Id(name))
}

/// Decomposes a `DateTimeInterface`.
pub(crate) fn php_datetime(vm: &mut Vm, dt: &Obj) -> VmResult<PhpDateTime> {
    let ts = vm.call_method_by_name(dt, "getTimestamp", Vec::new())?;
    let timestamp = vm.value_to_int(&ts)?;
    let us = vm.call_method_by_name(dt, "format", vec![Value::Str(ZStr::from("u"))])?;
    let microseconds = vm.value_to_int(&us)?;
    let tz = vm.call_method_by_name(dt, "getTimezone", Vec::new())?;
    let zone = match tz {
        Value::Object(o) => Some(php_zone(vm, &o)?),
        _ => None,
    };
    Ok(PhpDateTime {
        timestamp,
        microseconds,
        zone,
    })
}

/// `date_default_timezone_get()`.
pub(crate) fn default_tz_name(vm: &mut Vm) -> VmResult<String> {
    let v = vm.call_function("date_default_timezone_get", Vec::new())?;
    Ok(vm.value_to_string(&v)?.to_string_lossy())
}

/// Builds a `DateTimeZone` for `spec`; `Ok(None)` when its constructor
/// threw (the exception is discarded and `IntlException` raised by the
/// caller).
pub(crate) fn make_timezone(vm: &mut Vm, spec: &DateTimeZoneSpec) -> VmResult<Value> {
    let name = match spec {
        DateTimeZoneSpec::Id(id) => id.clone(),
        DateTimeZoneSpec::Offset(secs) => {
            let sign = if *secs < 0 { '-' } else { '+' };
            let a = secs.abs();
            format!("{sign}{:02}:{:02}", a / 3600, (a % 3600) / 60)
        }
    };
    let Some(cls) = vm.find_class(b"DateTimeZone") else {
        return Err(vm.throw("Error", "Class \"DateTimeZone\" not found"));
    };
    match vm.new_object_with_args(&cls, vec![Value::Str(ZStr::from(name))]) {
        Ok(o) => Ok(Value::Object(o)),
        Err(Throw::Exception(prev)) if matches!(spec, DateTimeZoneSpec::Id(_)) => {
            Err(throw_with_previous(
                vm,
                "IntlException",
                "DateTimeZone constructor threw exception",
                prev,
            ))
        }
        Err(e) => Err(e),
    }
}

/// `new DateTime("@$ts")` moved to `tz`.
pub(crate) fn make_datetime(vm: &mut Vm, ts: i64, tz: &Value) -> VmResult<Value> {
    let Some(cls) = vm.find_class(b"DateTime") else {
        return Err(vm.throw("Error", "Class \"DateTime\" not found"));
    };
    let o = vm.new_object_with_args(&cls, vec![Value::Str(ZStr::from(format!("@{ts}")))])?;
    vm.call_method_by_name(&o, "setTimezone", vec![tz.clone()])?;
    Ok(Value::Object(o))
}

/// Throws `class` with `message`, chaining `prev` as its previous exception.
pub(crate) fn throw_with_previous(vm: &mut Vm, class: &str, message: &str, prev: Obj) -> Throw {
    let t = vm.throw(class, message);
    if let Throw::Exception(e) = &t {
        vm.set_property_raw(e, "previous", Value::Object(prev));
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets() {
        assert_eq!(parse_offset("+05:30"), Some(19800));
        assert_eq!(parse_offset("-01:00"), Some(-3600));
        assert_eq!(parse_offset("+00:00:30"), Some(30));
        assert_eq!(parse_offset("Europe/Lisbon"), None);
        assert_eq!(parse_offset("+x"), None);
    }
}

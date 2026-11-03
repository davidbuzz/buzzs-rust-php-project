//! `DateTimeZone` and the `timezone_*()` functions over it.

use php_date::api;
use php_date::parse::{parse_timezone_spec, TimezoneSpec};
use php_date::tz;
use php_runtime::{Obj, PropertyPurpose, Value, ZArray, ZStr};
use php_vm::ext::{acc, Args, ClassBuilder, ClassHooks, ModuleBuilder, VmResult};
use php_vm::object;
use php_vm::Vm;

use crate::common::{get_str, lossy, set_str, to_php, E_WARNING};
use crate::datetime::{
    add_common_properties, clone_hook, invalid_serialization, null_byte_check, obj_arg,
    restore_custom, wakeup_deprecation,
};
use crate::functions::{timezone_abbreviations_list, timezone_identifiers_list};
use crate::objects::{
    class, instantiate, require_time, require_tz, set_tz, std_props, tz_of, TzObj,
};

/// `timezone_initialize()`.
fn initialize(s: &[u8]) -> Result<TimezoneSpec, String> {
    parse_timezone_spec(&lossy(s))
}

/// The `timezone_type` / `timezone` pair of a zone.
fn add_tz_props(props: &mut ZArray, z: &TimezoneSpec) {
    set_str(props, "timezone_type", Value::Int(z.zone_type()));
    set_str(props, "timezone", Value::Str(ZStr::from(z.name())));
}

fn this_or_arg(a: &Args) -> (Obj, usize) {
    match &a.this {
        Some(o) => (o.clone(), 0),
        None => (obj_arg(a, 0).unwrap_or_else(|| a.this_obj()), 1),
    }
}

fn timezone_open(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    null_byte_check(vm, a, 1, &s)?;
    match initialize(s.as_bytes()) {
        Ok(z) => {
            let cls = class(vm, "DateTimeZone");
            let o = instantiate(vm, &cls)?;
            set_tz(&o, Some(z));
            Ok(Value::Object(o))
        }
        Err(m) => {
            vm.php_warning(&m)?;
            Ok(Value::Bool(false))
        }
    }
}

fn construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    null_byte_check(vm, a, 1, &s)?;
    match initialize(s.as_bytes()) {
        Ok(z) => {
            set_tz(&a.this_obj(), Some(z));
            Ok(Value::Null)
        }
        Err(m) => Err(vm.throw(
            "DateInvalidTimeZoneException",
            &format!("DateTimeZone::__construct(): {m}"),
        )),
    }
}

/// `php_date_timezone_initialize_from_hash()`.
fn from_hash(h: &ZArray) -> Option<TimezoneSpec> {
    let Some(Value::Int(tt)) = get_str(h, "timezone_type").map(Value::deref) else {
        return None;
    };
    let Some(Value::Str(name)) = get_str(h, "timezone").map(Value::deref) else {
        return None;
    };
    api::timezone_from_hash(tt, name.as_bytes())
}

fn set_state(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = a.array(0);
    let cls = class(vm, "DateTimeZone");
    let o = instantiate(vm, &cls)?;
    let Some(z) = from_hash(&h) else {
        return Err(invalid_serialization(vm, "DateTimeZone"));
    };
    set_tz(&o, Some(z));
    Ok(Value::Object(o))
}

fn wakeup(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    wakeup_deprecation(vm, "DateTimeZone")?;
    let o = a.this_obj();
    let Some(z) = from_hash(&std_props(&o)) else {
        return Err(invalid_serialization(vm, "DateTimeZone"));
    };
    set_tz(&o, Some(z));
    Ok(Value::Null)
}

fn serialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let z = require_tz(vm, &o)?;
    let mut out = ZArray::new();
    add_tz_props(&mut out, &z);
    add_common_properties(&mut out, &o);
    Ok(Value::Array(out))
}

fn unserialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let h = a.array(0);
    let Some(z) = from_hash(&h) else {
        return Err(invalid_serialization(vm, "DateTimeZone"));
    };
    set_tz(&o, Some(z));
    restore_custom(vm, &o, &h, &["timezone_type", "timezone"])?;
    Ok(Value::Null)
}

fn get_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, _) = this_or_arg(a);
    let z = require_tz(vm, &o)?;
    Ok(Value::Str(ZStr::from(z.name())))
}

fn get_offset(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, n) = this_or_arg(a);
    let Some(d) = obj_arg(a, n) else {
        return Ok(Value::Null);
    };
    let z = require_tz(vm, &o)?;
    let t = require_time(vm, &d)?;
    Ok(Value::Int(z.offset_at(&t)))
}

fn get_transitions(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, n) = this_or_arg(a);
    let begin = if a.has(n) { a.int(n) } else { i64::MIN };
    let end = if a.has(n + 1) {
        a.int(n + 1)
    } else {
        i64::from(i32::MAX)
    };
    let z = require_tz(vm, &o)?;
    Ok(z.transitions(begin, end)
        .map_or(Value::Bool(false), |v| to_php(&v)))
}

fn get_location(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, _) = this_or_arg(a);
    let z = require_tz(vm, &o)?;
    Ok(z.location().map_or(Value::Bool(false), |v| to_php(&v)))
}

fn create_hook(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    object::set_internal(o, TzObj::default());
    Ok(())
}

/// `date_object_get_properties_for_timezone()` / `get_debug_info`.
fn properties_for(_vm: &mut Vm, o: &Obj, purpose: PropertyPurpose) -> VmResult<Option<ZArray>> {
    if purpose == PropertyPurpose::Debug && object::class_of(o).magic.debug_info.is_some() {
        return Ok(None);
    }
    let mut props = std_props(o);
    if let Some(z) = tz_of(o) {
        add_tz_props(&mut props, &z);
    }
    Ok(Some(props))
}

/// `date_object_compare_timezone()`.
fn compare(vm: &mut Vm, a: &Value, b: &Value) -> VmResult<Option<i32>> {
    let (Value::Object(x), Value::Object(y)) = (a, b) else {
        return Ok(None);
    };
    if !object::has_internal::<TzObj>(x) || !object::has_internal::<TzObj>(y) {
        return Ok(None);
    }
    let (Some(z1), Some(z2)) = (tz_of(x), tz_of(y)) else {
        return Err(vm.throw("DateObjectError", api::messages::UNINITIALIZED_ZONE_COMPARE));
    };
    match z1.equals(&z2) {
        Some(eq) => Ok(Some(i32::from(!eq))),
        None => Err(vm.throw("DateException", api::messages::CANNOT_COMPARE_ZONE_KINDS)),
    }
}

/// Emits the interval comparison warning (used by `DateInterval`).
pub(crate) fn warn(vm: &mut Vm, msg: &str) -> VmResult<()> {
    vm.raise(E_WARNING, msg)
}

/// Registers `DateTimeZone` and its functions.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    let vm = m.vm();
    let p = acc::PUBLIC;
    let ps = acc::PUBLIC | acc::STATIC;
    let g = |n: i64| Value::Int(n);
    ClassBuilder::new("DateTimeZone")
        .constant("AFRICA", g(tz::group::AFRICA))
        .constant("AMERICA", g(tz::group::AMERICA))
        .constant("ANTARCTICA", g(tz::group::ANTARCTICA))
        .constant("ARCTIC", g(tz::group::ARCTIC))
        .constant("ASIA", g(tz::group::ASIA))
        .constant("ATLANTIC", g(tz::group::ATLANTIC))
        .constant("AUSTRALIA", g(tz::group::AUSTRALIA))
        .constant("EUROPE", g(tz::group::EUROPE))
        .constant("INDIAN", g(tz::group::INDIAN))
        .constant("PACIFIC", g(tz::group::PACIFIC))
        .constant("UTC", g(tz::group::UTC))
        .constant("ALL", g(tz::group::ALL))
        .constant("ALL_WITH_BC", g(tz::group::ALL_WITH_BC))
        .constant("PER_COUNTRY", g(tz::group::PER_COUNTRY))
        .method("__construct(string $timezone)", p, construct)
        .method("getName(): string", p, get_name)
        .method("getOffset(DateTimeInterface $datetime): int", p, get_offset)
        .method(
            "getTransitions(int $timestampBegin = PHP_INT_MIN, int $timestampEnd = 2147483647): array|false",
            p,
            get_transitions,
        )
        .method("getLocation(): array|false", p, get_location)
        .method("listAbbreviations(): array", ps, timezone_abbreviations_list)
        .method(
            "listIdentifiers(int $timezoneGroup = 2047, ?string $countryCode = null): array",
            ps,
            timezone_identifiers_list,
        )
        .method("__serialize(): array", p, serialize)
        .method("__unserialize(array $data): void", p, unserialize)
        .method("__wakeup(): void", p, wakeup)
        .method("__set_state(array $array): static", ps, set_state)
        .hooks(ClassHooks {
            create: Some(create_hook),
            clone: Some(clone_hook),
            properties_for: Some(properties_for),
            compare: Some(compare),
            ..ClassHooks::default()
        })
        .register(vm);

    m.function("timezone_open(string $timezone): DateTimeZone|false", timezone_open)
        .function("timezone_name_get(DateTimeZone $object): string", get_name)
        .function(
            "timezone_offset_get(DateTimeZone $object, DateTimeInterface $datetime): int",
            get_offset,
        )
        .function(
            "timezone_transitions_get(DateTimeZone $object, int $timestampBegin = PHP_INT_MIN, int $timestampEnd = 2147483647): array|false",
            get_transitions,
        )
        .function("timezone_location_get(DateTimeZone $object): array|false", get_location);
}

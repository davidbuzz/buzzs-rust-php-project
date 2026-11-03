//! `IntlTimeZone` and the `intltz_*()` functions, plus the time zone
//! argument processing shared with `IntlCalendar`/`IntlDateFormatter`.

use php_intl::timezone::{self as lib, EnumerationFilter, TimeZone, TimeZoneArg};
use php_runtime::{Obj, PropertyPurpose, Value, ZArray, ZStr};
use php_vm::ext::{Args, NativeFn, VmResult};
use php_vm::Vm;

use crate::core::{self, s};
use crate::dt;
use crate::locale::text;

/// Wraps a zone in a new `IntlTimeZone` object.
pub(crate) fn object(vm: &mut Vm, tz: TimeZone) -> VmResult<Value> {
    let o = core::instantiate(vm, "IntlTimeZone")?;
    core::set_data(&o, tz);
    Ok(Value::Object(o))
}

/// The zone of an `IntlTimeZone` object (`None` when unconstructed).
pub(crate) fn zone_of(o: &Obj) -> Option<TimeZone> {
    core::with::<TimeZone, _>(o, |t| t.clone())
}

/// `timezone_process_timezone_argument()` for argument `n` (0-based).
/// Intl errors are recorded on `err_obj` (and globally) and yield
/// `Ok(None)`.
pub(crate) fn process_arg(vm: &mut Vm, a: &Args, n: usize, err_obj: Option<&Obj>) -> VmResult<Option<TimeZone>> {
    let v = a.opt(n);
    process_value(vm, a, v, n, err_obj)
}

/// [`process_arg`] on an explicit value (`None` = `null`).
pub(crate) fn process_value(vm: &mut Vm, a: &Args, v: Option<Value>, n: usize, err_obj: Option<&Obj>) -> VmResult<Option<TimeZone>> {
    let mut zone = None;
    let mut name: Option<ZStr> = None;
    let mut intl: Option<TimeZone> = None;
    match v {
        None => name = Some(ZStr::from(dt::default_tz_name(vm)?)),
        Some(Value::Object(o)) => {
            if dt::instance_of(&o, "IntlTimeZone") {
                match zone_of(&o) {
                    Some(t) => intl = Some(t),
                    None => {
                        return Err(core::throw_engine(
                            vm,
                            a,
                            php_intl::Error::IntlExceptionInFunction("passed IntlTimeZone is not properly constructed".into()),
                            0,
                        ))
                    }
                }
            } else if dt::instance_of(&o, "DateTimeZone") {
                zone = Some(dt::php_zone(vm, &o)?);
            } else if crate::collator::has_to_string(&o) {
                name = Some(vm.value_to_string(&Value::Object(o))?);
            } else {
                let cls = php_vm::object::class_of(&o).name.to_string_lossy();
                return Err(a.type_error(vm, n + 1, &format!("Object of class {cls} could not be converted to string")));
            }
        }
        Some(other) => name = Some(vm.value_to_string(&other)?),
    }
    let arg = if let Some(t) = &intl {
        TimeZoneArg::Intl(t)
    } else if let Some(z) = &zone {
        TimeZoneArg::DateTimeZone(z)
    } else {
        TimeZoneArg::Str(name.as_ref().map_or(&[][..], ZStr::as_bytes))
    };
    match lib::process_timezone_argument(arg) {
        Ok(t) => Ok(Some(t)),
        Err(e) => {
            core::fail(vm, a, err_obj, e, 0)?;
            Ok(None)
        }
    }
}

fn construct(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.throw("Exception", "An object of this type cannot be created with the new operator"))
}

/// Static-call tail: string result or intl error + `fail`.
fn str_result(vm: &mut Vm, a: &Args, r: php_intl::Result<String>, fail: Value) -> VmResult<Value> {
    match r {
        Ok(v) => Ok(s(v)),
        Err(e) => core::fail_with(vm, a, None, e, 0, fail),
    }
}

fn count_equivalent_ids(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    match lib::count_equivalent_ids(a.str(0).as_bytes()) {
        Ok(n) => Ok(Value::Int(n)),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Bool(false)),
    }
}

fn create_default(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    object(vm, TimeZone::create_default())
}

fn create_enumeration(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let arg = a.opt(0);
    let country;
    let filter = match &arg {
        None => EnumerationFilter::All,
        Some(Value::Int(i)) => EnumerationFilter::RawOffset(*i),
        Some(Value::Str(z)) => {
            country = text(z);
            EnumerationFilter::Country(&country)
        }
        Some(_) => EnumerationFilter::All,
    };
    match lib::create_enumeration(filter) {
        Ok(ids) => crate::iter::from_strings(vm, ids),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Bool(false)),
    }
}

fn create_time_zone(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    match TimeZone::create_time_zone(a.str(0).as_bytes()) {
        Ok(t) => object(vm, t),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Null),
    }
}

fn create_time_zone_id_enumeration(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let region = a.opt(1).map(|v| match v {
        Value::Str(z) => text(&z),
        _ => String::new(),
    });
    let off = a.opt(2).map(|v| match v {
        Value::Int(i) => i,
        _ => 0,
    });
    match lib::create_time_zone_id_enumeration(a.int(0), region.as_deref(), off) {
        Ok(ids) => crate::iter::from_strings(vm, ids),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Bool(false)),
    }
}

fn from_date_time_zone(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let Value::Object(o) = a.at(0) else {
        return Ok(Value::Null);
    };
    let z = dt::php_zone(vm, &o)?;
    match lib::from_date_time_zone(&z) {
        Ok(t) => object(vm, t),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Null),
    }
}

fn get_canonical_id(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    match lib::canonical_id(a.str(0).as_bytes()) {
        Ok((id, sys)) => {
            if a.has(1) {
                a.set_ref(1, Value::Bool(sys));
            }
            Ok(s(id))
        }
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Bool(false)),
    }
}

fn get_equivalent_id(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let r = lib::equivalent_id(a.str(0).as_bytes(), a.int(1));
    str_result(vm, a, r, Value::Bool(false))
}

fn get_gmt(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    object(vm, TimeZone::gmt())
}

fn get_unknown(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    object(vm, TimeZone::unknown())
}

fn get_iana_id(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let r = lib::iana_id(a.str(0).as_bytes());
    str_result(vm, a, r, Value::Bool(false))
}

fn get_region(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let r = lib::region(a.str(0).as_bytes());
    str_result(vm, a, r, Value::Bool(false))
}

fn get_tz_data_version(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let r = lib::tz_data_version();
    str_result(vm, a, r, Value::Bool(false))
}

fn get_windows_id(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let r = lib::windows_id(a.str(0).as_bytes());
    str_result(vm, a, r, Value::Bool(false))
}

fn get_id_for_windows_id(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let region = a.opt(1).map(|v| match v {
        Value::Str(z) => text(&z),
        _ => String::new(),
    });
    let r = lib::id_for_windows_id(a.str(0).as_bytes(), region.as_deref());
    str_result(vm, a, r, Value::Bool(false))
}

/// Fetches `$this`/the first argument, resetting the errors.
fn fetch(vm: &mut Vm, a: &Args) -> VmResult<(Obj, usize, TimeZone)> {
    let (o, off) = core::target(a);
    core::reset_both(&o);
    match zone_of(&o) {
        Some(t) => Ok((o, off, t)),
        None => Err(core::unconstructed(vm, "IntlTimeZone")),
    }
}

fn get_id(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off, t) = fetch(vm, a)?;
    match t.id() {
        Ok(id) => Ok(s(id)),
        Err(e) => core::fail_with(vm, a, Some(&o), e, off, Value::Bool(false)),
    }
}

fn use_daylight_time(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, _, t) = fetch(vm, a)?;
    Ok(Value::Bool(t.use_daylight_time()))
}

fn get_offset(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off, t) = fetch(vm, a)?;
    match t.offset(a.float(off), a.bool(off + 1)) {
        Ok((raw, dst)) => {
            a.set_ref(off + 2, Value::Int(i64::from(raw)));
            a.set_ref(off + 3, Value::Int(i64::from(dst)));
            Ok(Value::Bool(true))
        }
        Err(e) => core::fail_with(vm, a, Some(&o), e, off, Value::Bool(false)),
    }
}

fn get_raw_offset(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, _, t) = fetch(vm, a)?;
    Ok(Value::Int(i64::from(t.raw_offset())))
}

fn has_same_rules(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off, t) = fetch(vm, a)?;
    let Value::Object(other) = a.at(off) else {
        return Ok(Value::Bool(false));
    };
    match zone_of(&other) {
        Some(ot) => Ok(Value::Bool(t.has_same_rules(&ot))),
        None => {
            core::errors_set(vm, &o, php_intl::UErrorCode::ILLEGAL_ARGUMENT_ERROR, "The second IntlTimeZone is unconstructed")?;
            Ok(Value::Bool(false))
        }
    }
}

fn get_display_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off, t) = fetch(vm, a)?;
    let loc = a.opt(off + 2).map(|v| match v {
        Value::Str(z) => text(&z),
        _ => String::new(),
    });
    match t.display_name(a.bool(off), a.int(off + 1), loc.as_deref()) {
        Ok(n) => Ok(s(n)),
        Err(e) => core::fail_with(vm, a, Some(&o), e, off, Value::Bool(false)),
    }
}

fn get_dst_savings(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, _, t) = fetch(vm, a)?;
    Ok(Value::Int(i64::from(t.dst_savings())))
}

fn to_date_time_zone(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off, t) = fetch(vm, a)?;
    match t.to_date_time_zone() {
        Ok(spec) => dt::make_timezone(vm, &spec),
        Err(e) => core::fail_with(vm, a, Some(&o), e, off, Value::Bool(false)),
    }
}

/// `get_debug_info`.
pub(crate) fn debug_info(_vm: &mut Vm, o: &Obj, p: PropertyPurpose) -> VmResult<Option<ZArray>> {
    if p != PropertyPurpose::Debug {
        return Ok(None);
    }
    Ok(Some(debug_array(o)))
}

/// The debug view of an `IntlTimeZone` object.
pub(crate) fn debug_array(o: &Obj) -> ZArray {
    let mut arr = ZArray::new();
    let h = arr.make_mut();
    match zone_of(o) {
        None => {
            h.insert("valid", Value::Bool(false));
        }
        Some(t) => {
            h.insert("valid", Value::Bool(true));
            let (id, off) = t.debug_info();
            h.insert("id", s(id));
            if let Some((raw, cur)) = off {
                h.insert("rawOffset", Value::Int(raw));
                h.insert("currentOffset", Value::Int(cur));
            }
        }
    }
    arr
}

/// `clone_obj`.
pub(crate) fn clone(vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    core::create(vm, new)?;
    match zone_of(old) {
        Some(t) => core::set_data(new, t),
        None => return Err(vm.throw("Error", "Cannot clone uninitialized IntlTimeZone")),
    }
    Ok(())
}

/// `compare` (only `==`).
pub(crate) fn compare(vm: &mut Vm, x: &Value, y: &Value) -> VmResult<Option<i32>> {
    let (Value::Object(a), Value::Object(b)) = (x, y) else {
        return Ok(None);
    };
    if !php_vm::object::class_of(a).instance_of_name(b"intltimezone") || !php_vm::object::class_of(b).instance_of_name(b"intltimezone") {
        return Ok(None);
    }
    if a == b {
        return Ok(Some(0));
    }
    match (zone_of(a), zone_of(b)) {
        (Some(ta), Some(tb)) => Ok(Some(if ta.equals(&tb) { 0 } else { 1 })),
        _ => Err(vm.throw("Exception", "Comparison with at least one unconstructed IntlTimeZone operand")),
    }
}

/// Handlers by stub key.
pub(crate) fn handlers() -> Vec<(&'static str, NativeFn)> {
    vec![
        ("IntlTimeZone::__construct", construct),
        ("IntlTimeZone::countEquivalentIDs", count_equivalent_ids),
        ("IntlTimeZone::createDefault", create_default),
        ("IntlTimeZone::createEnumeration", create_enumeration),
        ("IntlTimeZone::createTimeZone", create_time_zone),
        ("IntlTimeZone::createTimeZoneIDEnumeration", create_time_zone_id_enumeration),
        ("IntlTimeZone::fromDateTimeZone", from_date_time_zone),
        ("IntlTimeZone::getCanonicalID", get_canonical_id),
        ("IntlTimeZone::getDisplayName", get_display_name),
        ("IntlTimeZone::getDSTSavings", get_dst_savings),
        ("IntlTimeZone::getEquivalentID", get_equivalent_id),
        ("IntlTimeZone::getErrorCode", core::get_error_code),
        ("IntlTimeZone::getErrorMessage", core::get_error_message),
        ("IntlTimeZone::getGMT", get_gmt),
        ("IntlTimeZone::getIanaID", get_iana_id),
        ("IntlTimeZone::getID", get_id),
        ("IntlTimeZone::getOffset", get_offset),
        ("IntlTimeZone::getRawOffset", get_raw_offset),
        ("IntlTimeZone::getRegion", get_region),
        ("IntlTimeZone::getTZDataVersion", get_tz_data_version),
        ("IntlTimeZone::getUnknown", get_unknown),
        ("IntlTimeZone::getWindowsID", get_windows_id),
        ("IntlTimeZone::getIDForWindowsID", get_id_for_windows_id),
        ("IntlTimeZone::hasSameRules", has_same_rules),
        ("IntlTimeZone::toDateTimeZone", to_date_time_zone),
        ("IntlTimeZone::useDaylightTime", use_daylight_time),
    ]
}

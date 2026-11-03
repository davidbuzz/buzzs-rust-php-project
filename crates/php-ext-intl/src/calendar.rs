//! `IntlCalendar`, `IntlGregorianCalendar` and the `intlcal_*()` /
//! `intlgregcal_*()` functions.

use php_intl::calendar::{self as lib, IntlCalendar, Limit};
use php_intl::{Error, Result, UErrorCode};
use php_runtime::{Obj, PropertyPurpose, Value, ZArray, ZStr};
use php_vm::ext::{Args, NativeFn, VmResult};
use php_vm::Vm;

use crate::core::{self, s};
use crate::dt;
use crate::locale::text;

/// Wraps a calendar in a new `IntlCalendar`/`IntlGregorianCalendar`.
pub(crate) fn object(vm: &mut Vm, cal: IntlCalendar) -> VmResult<Value> {
    let class = if cal.is_gregorian() { "IntlGregorianCalendar" } else { "IntlCalendar" };
    let o = core::instantiate(vm, class)?;
    core::set_data(&o, cal);
    Ok(Value::Object(o))
}

/// A copy of the calendar of `o` (`None` when unconstructed).
pub(crate) fn calendar_of(o: &Obj) -> Option<Result<IntlCalendar>> {
    core::with::<IntlCalendar, _>(o, |c| c.try_clone())
}

fn opt_string(a: &Args, n: usize) -> Option<String> {
    a.opt(n).map(|v| match v {
        Value::Str(z) => text(&z),
        _ => String::new(),
    })
}

fn construct(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.throw("Exception", "An object of this type cannot be created with the new operator"))
}

fn create_instance(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let Some(tz) = crate::timezone::process_arg(vm, a, 0, None)? else {
        return Ok(Value::Null);
    };
    let loc = opt_string(a, 1);
    match IntlCalendar::create_instance(&tz, loc.as_deref()) {
        Ok(c) => object(vm, c),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Null),
    }
}

fn from_date_time(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let dto = match a.at(0) {
        Value::Object(o) => o,
        Value::Str(z) => {
            let Some(cls) = vm.find_class(b"DateTime") else {
                return Ok(Value::Null);
            };
            match vm.new_object_with_args(&cls, vec![Value::Str(z)]) {
                Ok(o) => o,
                Err(e) => return Err(e),
            }
        }
        _ => return Ok(Value::Null),
    };
    let pdt = dt::php_datetime(vm, &dto)?;
    let loc = opt_string(a, 1);
    match IntlCalendar::from_date_time(&pdt, loc.as_deref()) {
        Ok(c) => object(vm, c),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Null),
    }
}

fn get_available_locales(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    Ok(Value::Array(ZArray::from_values(lib::available_locales().into_iter().map(s))))
}

fn get_keyword_values_for_locale(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    match lib::keyword_values_for_locale(&text(&a.str(0)), &text(&a.str(1)), a.bool(2)) {
        Ok(v) => crate::iter::from_strings(vm, v),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Bool(false)),
    }
}

fn get_now(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    Ok(Value::Float(lib::now()))
}

/// Fetches the calendar object (`CALENDAR_METHOD_FETCH_OBJECT`).
fn fetch(vm: &mut Vm, a: &Args) -> VmResult<(Obj, usize)> {
    let (o, off) = core::target(a);
    core::reset_both(&o);
    if !core::is_constructed::<IntlCalendar>(&o) {
        return Err(core::unconstructed(vm, "IntlCalendar"));
    }
    Ok((o, off))
}

/// Runs `f` on the calendar; errors are reported on the object and
/// yield `false`.
fn run<T>(
    vm: &mut Vm,
    a: &Args,
    o: &Obj,
    off: usize,
    f: impl FnOnce(&mut IntlCalendar) -> Result<T>,
    ok: impl FnOnce(T) -> Value,
) -> VmResult<Value> {
    match core::with::<IntlCalendar, _>(o, f) {
        Some(Ok(v)) => Ok(ok(v)),
        Some(Err(e)) => core::fail_with(vm, a, Some(o), e, off, Value::Bool(false)),
        None => Ok(Value::Bool(false)),
    }
}

macro_rules! simple {
    ($name:ident, |$c:ident, $a:ident, $off:ident| $body:expr, $ok:expr) => {
        fn $name(vm: &mut Vm, $a: &mut Args) -> VmResult<Value> {
            let (o, $off) = fetch(vm, $a)?;
            let args: &Args = $a;
            let _ = args;
            run(vm, $a, &o, $off, |$c| $body, $ok)
        }
    };
}

simple!(get, |c, a, off| c.get(a.int(off)), Value::Int);
simple!(get_time, |c, a, off| c.get_time(), Value::Float);
simple!(set_time, |c, a, off| c.set_time(a.float(off)), |()| Value::Bool(true));
simple!(add, |c, a, off| c.add(a.int(off), a.int(off + 1)), |()| Value::Bool(true));
simple!(clear, |c, a, off| c.clear(a.opt(off).map(|v| if let Value::Int(i) = v { i } else { 0 })), |()| Value::Bool(true));
simple!(field_difference, |c, a, off| c.field_difference(a.float(off), a.int(off + 1)), Value::Int);
simple!(get_actual_maximum, |c, a, off| c.limit(a.int(off), Limit::ActualMaximum), Value::Int);
simple!(get_actual_minimum, |c, a, off| c.limit(a.int(off), Limit::ActualMinimum), Value::Int);
simple!(get_maximum, |c, a, off| c.limit(a.int(off), Limit::Maximum), Value::Int);
simple!(get_minimum, |c, a, off| c.limit(a.int(off), Limit::Minimum), Value::Int);
simple!(get_greatest_minimum, |c, a, off| c.limit(a.int(off), Limit::GreatestMinimum), Value::Int);
simple!(get_least_maximum, |c, a, off| c.limit(a.int(off), Limit::LeastMaximum), Value::Int);
simple!(get_day_of_week_type, |c, a, off| c.day_of_week_type(a.int(off)), Value::Int);
simple!(get_weekend_transition, |c, a, off| c.weekend_transition(a.int(off)), Value::Int);
simple!(get_first_day_of_week, |c, a, off| Ok(c.first_day_of_week()), Value::Int);
simple!(set_first_day_of_week, |c, a, off| c.set_first_day_of_week(a.int(off)), |()| Value::Bool(true));
simple!(get_minimal_days, |c, a, off| Ok(c.minimal_days_in_first_week()), Value::Int);
simple!(set_minimal_days, |c, a, off| c.set_minimal_days_in_first_week(a.int(off)), |()| Value::Bool(true));
simple!(get_locale, |c, a, off| c.locale(a.int(off)), s);
simple!(get_type, |c, a, off| Ok(c.cal_type()), s);
simple!(in_daylight_time, |c, a, off| c.in_daylight_time(), Value::Bool);
simple!(is_lenient, |c, a, off| Ok(c.is_lenient()), Value::Bool);
simple!(set_lenient, |c, a, off| {
    c.set_lenient(a.bool(off));
    Ok(())
}, |()| Value::Bool(true));
simple!(is_set, |c, a, off| c.is_set(a.int(off)), Value::Bool);
simple!(is_weekend, |c, a, off| c.is_weekend(a.opt(off).map(|v| if let Value::Float(f) = v { f } else { 0.0 })), Value::Bool);
simple!(get_repeated_wall_time_option, |c, a, off| Ok(c.repeated_wall_time_option()), Value::Int);
simple!(get_skipped_wall_time_option, |c, a, off| Ok(c.skipped_wall_time_option()), Value::Int);
simple!(set_repeated_wall_time_option, |c, a, off| c.set_repeated_wall_time_option(a.int(off)), |()| Value::Bool(true));
simple!(set_skipped_wall_time_option, |c, a, off| c.set_skipped_wall_time_option(a.int(off)), |()| Value::Bool(true));
simple!(set_date, |c, a, off| c.set_date(a.int(off), a.int(off + 1), a.int(off + 2)), |()| Value::Null);
simple!(set_date_time, |c, a, off| c.set_date_time(
    a.int(off),
    a.int(off + 1),
    a.int(off + 2),
    a.int(off + 3),
    a.int(off + 4),
    a.opt(off + 5).map(|v| if let Value::Int(i) = v { i } else { 0 })
), |()| Value::Null);
simple!(set_gregorian_change, |c, a, off| c.set_gregorian_change(a.float(off)), |()| Value::Bool(true));
simple!(get_gregorian_change, |c, a, off| Ok(c.gregorian_change()), Value::Float);
simple!(is_leap_year, |c, a, off| c.is_leap_year(a.int(off)), Value::Bool);

/// `before`/`after`/`equals`/`isEquivalentTo`.
fn compare_with(vm: &mut Vm, a: &Args, check_first: bool, f: fn(&IntlCalendar, &IntlCalendar) -> Result<bool>) -> VmResult<Value> {
    let (o0, off0) = core::target(a);
    let other = match a.at(off0) {
        Value::Object(x) => x,
        _ => return Ok(Value::Bool(false)),
    };
    let other_cal = calendar_of(&other);
    if check_first && other_cal.is_none() {
        let _ = o0;
        return Err(vm.throw("Error", &format!("{}(): Argument #{} ($other) is uninitialized", core::active_function(vm).unwrap_or_default(), off0 + 1)));
    }
    let (o, off) = fetch(vm, a)?;
    let Some(other_cal) = other_cal else {
        return Err(vm.throw("Error", &format!("{}(): Argument #{} ($other) is uninitialized", core::active_function(vm).unwrap_or_default(), off + 1)));
    };
    let other_cal = match other_cal {
        Ok(c) => c,
        Err(e) => return core::fail_with(vm, a, Some(&o), e, off, Value::Bool(false)),
    };
    run(vm, a, &o, off, |c| f(c, &other_cal), Value::Bool)
}

fn after(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    compare_with(vm, a, false, IntlCalendar::after)
}

fn before(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    compare_with(vm, a, false, IntlCalendar::before)
}

fn equals(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    compare_with(vm, a, false, IntlCalendar::equals)
}

fn is_equivalent_to(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    compare_with(vm, a, true, |x, y| Ok(x.is_equivalent_to(y)))
}

fn set(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let off = usize::from(a.this.is_none());
    let n = core::num_args(a).saturating_sub(off);
    if a.this.is_some() && n > 2 {
        vm.raise(
            8192,
            "Calling IntlCalendar::set() with more than 2 arguments is deprecated, use either IntlCalendar::setDate() or IntlCalendar::setDateTime() instead",
        )?;
    }
    let vals: Vec<i64> = (0..n).map(|i| a.int(off + i)).collect();
    // Range checks happen before the object is fetched (shifted to the
    // procedural numbering by `fail`).
    for (i, v) in vals.iter().enumerate() {
        if i32::try_from(*v).is_err() {
            return Err(core::throw_engine(vm, a, Error::arg_value(u32::try_from(i + 1).unwrap_or(1), format!("must be between {} and {}", i32::MIN, i32::MAX)), off));
        }
    }
    let (o, off) = fetch(vm, a)?;
    match core::with::<IntlCalendar, _>(&o, |c| c.set(&vals)) {
        Some(Err(e)) => Err(core::throw_engine(vm, a, e, off)),
        _ => Ok(Value::Bool(true)),
    }
}

fn roll(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = fetch(vm, a)?;
    let field = a.int(off);
    if !(0..lib::FIELD_COUNT).contains(&field) {
        return Err(a.value_error(vm, off + 1, "must be a valid field"));
    }
    let amount = match a.at(off + 1) {
        Value::Bool(b) => {
            vm.php_deprecated("Passing bool is deprecated, use 1 or -1 instead")?;
            if b {
                1
            } else {
                -1
            }
        }
        v => vm.value_to_int(&v)?,
    };
    run(vm, a, &o, off, |c| c.roll(field, amount), |()| Value::Bool(true))
}

fn get_time_zone(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = fetch(vm, a)?;
    match core::with::<IntlCalendar, _>(&o, |c| c.time_zone()) {
        Some(Ok(tz)) => crate::timezone::object(vm, tz),
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, off, Value::Bool(false)),
        None => Ok(Value::Bool(false)),
    }
}

fn set_time_zone(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = fetch(vm, a)?;
    if a.opt(off).is_none() {
        return Ok(Value::Bool(true));
    }
    let Some(tz) = crate::timezone::process_value(vm, a, a.opt(off), off, Some(&o))? else {
        return Ok(Value::Bool(false));
    };
    run(vm, a, &o, off, |c| c.set_time_zone(Some(&tz)), |()| Value::Bool(true))
}

fn to_date_time(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = fetch(vm, a)?;
    let r = core::with::<IntlCalendar, _>(&o, |c| c.to_date_time());
    match r {
        Some(Ok((ts, spec))) => {
            let tz = dt::make_timezone(vm, &spec)?;
            dt::make_datetime(vm, ts, &tz)
        }
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, off, Value::Bool(false)),
        None => Ok(Value::Bool(false)),
    }
}

// ------------------------------------------------------------------ Gregorian

/// The shared body of `new IntlGregorianCalendar(...)` and
/// `intlgregcal_create_instance(...)`; returns the calendar or `None`
/// (error recorded).
fn gregorian_body(vm: &mut Vm, a: &Args, is_ctor: bool, target: Option<&Obj>) -> VmResult<Option<IntlCalendar>> {
    core::reset_global();
    let n = core::num_args(a);
    if is_ctor && n > 2 {
        vm.raise(
            8192,
            "Calling IntlGregorianCalendar::__construct() with more than 2 arguments is deprecated, use either IntlGregorianCalendar::createFromDate() or IntlGregorianCalendar::createFromDateTime() instead",
        )?;
    }
    if n > 6 {
        return Err(vm.throw("ArgumentCountError", "Too many arguments"));
    }
    let mut variant = n;
    while variant > 0 && matches!(a.at(variant - 1), Value::Null) {
        variant -= 1;
    }
    if variant == 4 {
        return Err(vm.throw("ArgumentCountError", "No variant with 4 arguments (excluding trailing NULLs)"));
    }
    if variant <= 2 {
        if variant >= 2 && !matches!(a.at(1), Value::Str(_) | Value::Null) {
            let given = crate::core::type_name(&a.at(1));
            return Err(a.type_error(vm, 2, &format!("must be of type ?string, {given} given")));
        }
        if let Some(t) = target {
            if core::is_constructed::<IntlCalendar>(t) {
                return Err(vm.throw("Error", "IntlGregorianCalendar object is already constructed"));
            }
        }
        let Some(tz) = crate::timezone::process_arg(vm, a, 0, None)? else {
            return Ok(None);
        };
        let loc = opt_string(a, 1);
        return match IntlCalendar::gregorian(&tz, loc.as_deref()) {
            Ok(c) => Ok(Some(c)),
            Err(e) => {
                core::fail(vm, a, None, e, 0)?;
                Ok(None)
            }
        };
    }
    let mut vals = Vec::with_capacity(variant);
    for i in 0..variant {
        let v = a.at(i);
        let iv = match &v {
            Value::Int(x) => *x,
            Value::Float(f) if f.fract() == 0.0 && f.is_finite() => {
                #[allow(clippy::cast_possible_truncation)]
                let x = *f as i64;
                x
            }
            Value::Bool(b) => i64::from(*b),
            Value::Str(z) if php_intl::common::numeric_string(z.as_bytes()).is_some() => vm.value_to_int(&v)?,
            Value::Null if i >= 3 => 0,
            other => {
                let given = crate::core::type_name(other);
                return Err(a.type_error(vm, i + 1, &format!("must be of type int, {given} given")));
            }
        };
        vals.push(iv);
    }
    if let Some(t) = target {
        if core::is_constructed::<IntlCalendar>(t) {
            return Err(vm.throw("Error", "IntlGregorianCalendar object is already constructed"));
        }
    }
    for (i, v) in vals.iter().enumerate() {
        if i32::try_from(*v).is_err() {
            return Err(a.value_error(vm, i + 1, &format!("must be between {} and {}", i32::MIN, i32::MAX)));
        }
    }
    let tzname = dt::default_tz_name(vm)?;
    match IntlCalendar::gregorian_from_fields(&vals, &tzname) {
        Ok(c) => Ok(Some(c)),
        Err(e) => {
            core::fail(vm, a, None, e, 0)?;
            Ok(None)
        }
    }
}

fn greg_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let r = core::in_ctor(|| gregorian_body(vm, a, true, Some(&o)))?;
    if let Some(c) = r {
        core::set_data(&o, c);
    }
    Ok(Value::Null)
}

fn greg_create_instance(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    match gregorian_body(vm, a, false, None)? {
        Some(c) => {
            let o = core::instantiate(vm, "IntlGregorianCalendar")?;
            core::set_data(&o, c);
            Ok(Value::Object(o))
        }
        None => Ok(Value::Null),
    }
}

fn greg_from_fields(vm: &mut Vm, a: &mut Args, count: usize) -> VmResult<Value> {
    core::reset_global();
    let mut vals: Vec<i64> = (0..count).map(|i| a.int(i)).collect();
    if count == 6 {
        match a.opt(5) {
            Some(Value::Int(x)) => vals[5] = x,
            _ => {
                vals.pop();
            }
        }
    }
    for (i, v) in vals.iter().enumerate() {
        if i32::try_from(*v).is_err() {
            return Err(a.value_error(vm, i + 1, &format!("must be between {} and {}", i32::MIN, i32::MAX)));
        }
    }
    let tzname = dt::default_tz_name(vm)?;
    let r = core::in_ctor(|| IntlCalendar::gregorian_from_fields(&vals, &tzname));
    match r {
        Ok(c) => {
            let o = core::instantiate(vm, "IntlGregorianCalendar")?;
            core::set_data(&o, c);
            Ok(Value::Object(o))
        }
        Err(e) => {
            let _g = core::CtorGuard::new();
            core::fail(vm, a, None, e, 0)?;
            Ok(Value::Null)
        }
    }
}

fn greg_create_from_date(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    greg_from_fields(vm, a, 3)
}

fn greg_create_from_date_time(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    greg_from_fields(vm, a, 6)
}

/// `get_debug_info`.
pub(crate) fn debug_info(_vm: &mut Vm, o: &Obj, p: PropertyPurpose) -> VmResult<Option<ZArray>> {
    if p != PropertyPurpose::Debug {
        return Ok(None);
    }
    let mut arr = ZArray::new();
    let h = arr.make_mut();
    let info = core::with::<IntlCalendar, _>(o, |c| c.debug_info());
    let Some(info) = info else {
        h.insert("valid", Value::Bool(false));
        return Ok(Some(arr));
    };
    h.insert("valid", Value::Bool(true));
    h.insert("type", s(info.cal_type));
    let mut tz = ZArray::new();
    {
        let t = tz.make_mut();
        t.insert("valid", Value::Bool(true));
        t.insert("id", s(info.time_zone.0));
        if let Some((raw, cur)) = info.time_zone.1 {
            t.insert("rawOffset", Value::Int(raw));
            t.insert("currentOffset", Value::Int(cur));
        }
    }
    h.insert("timeZone", Value::Array(tz));
    h.insert("locale", s(info.locale));
    let mut fields = ZArray::new();
    for (name, v) in info.fields {
        fields.make_mut().insert(ZStr::from(name), match v {
            Ok(i) => Value::Int(i),
            Err(e) => s(e),
        });
    }
    h.insert("fields", Value::Array(fields));
    Ok(Some(arr))
}

/// `clone_obj`.
pub(crate) fn clone(vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    core::create(vm, new)?;
    match calendar_of(old) {
        Some(Ok(c)) => core::set_data(new, c),
        Some(Err(_)) => return Err(vm.throw("Error", "Failed to clone IntlCalendar")),
        None => return Err(vm.throw("Error", "Cannot clone uninitialized IntlCalendar")),
    }
    Ok(())
}

/// An `IntlCalendar` argument for other classes: a copy of its calendar.
pub(crate) fn require(o: &Obj) -> Option<IntlCalendar> {
    calendar_of(o).and_then(std::result::Result::ok)
}

/// Unused marker to keep the import list honest.
#[allow(dead_code)]
const _CODE: UErrorCode = UErrorCode::ZERO;

/// Handlers by stub key.
pub(crate) fn handlers() -> Vec<(&'static str, NativeFn)> {
    vec![
        ("IntlCalendar::__construct", construct),
        ("IntlCalendar::createInstance", create_instance),
        ("IntlCalendar::equals", equals),
        ("IntlCalendar::fieldDifference", field_difference),
        ("IntlCalendar::add", add),
        ("IntlCalendar::after", after),
        ("IntlCalendar::before", before),
        ("IntlCalendar::clear", clear),
        ("IntlCalendar::fromDateTime", from_date_time),
        ("IntlCalendar::get", get),
        ("IntlCalendar::getActualMaximum", get_actual_maximum),
        ("IntlCalendar::getActualMinimum", get_actual_minimum),
        ("IntlCalendar::getAvailableLocales", get_available_locales),
        ("IntlCalendar::getDayOfWeekType", get_day_of_week_type),
        ("IntlCalendar::getErrorCode", core::get_error_code),
        ("IntlCalendar::getErrorMessage", core::get_error_message),
        ("IntlCalendar::getFirstDayOfWeek", get_first_day_of_week),
        ("IntlCalendar::getGreatestMinimum", get_greatest_minimum),
        ("IntlCalendar::getKeywordValuesForLocale", get_keyword_values_for_locale),
        ("IntlCalendar::getLeastMaximum", get_least_maximum),
        ("IntlCalendar::getLocale", get_locale),
        ("IntlCalendar::getMaximum", get_maximum),
        ("IntlCalendar::getMinimalDaysInFirstWeek", get_minimal_days),
        ("IntlCalendar::setMinimalDaysInFirstWeek", set_minimal_days),
        ("IntlCalendar::getMinimum", get_minimum),
        ("IntlCalendar::getNow", get_now),
        ("IntlCalendar::getRepeatedWallTimeOption", get_repeated_wall_time_option),
        ("IntlCalendar::getSkippedWallTimeOption", get_skipped_wall_time_option),
        ("IntlCalendar::getTime", get_time),
        ("IntlCalendar::getTimeZone", get_time_zone),
        ("IntlCalendar::getType", get_type),
        ("IntlCalendar::getWeekendTransition", get_weekend_transition),
        ("IntlCalendar::inDaylightTime", in_daylight_time),
        ("IntlCalendar::isEquivalentTo", is_equivalent_to),
        ("IntlCalendar::isLenient", is_lenient),
        ("IntlCalendar::isWeekend", is_weekend),
        ("IntlCalendar::roll", roll),
        ("IntlCalendar::isSet", is_set),
        ("intlcal_set", set),
        ("IntlCalendar::setDate", set_date),
        ("IntlCalendar::setDateTime", set_date_time),
        ("IntlCalendar::setFirstDayOfWeek", set_first_day_of_week),
        ("IntlCalendar::setLenient", set_lenient),
        ("IntlCalendar::setRepeatedWallTimeOption", set_repeated_wall_time_option),
        ("IntlCalendar::setSkippedWallTimeOption", set_skipped_wall_time_option),
        ("IntlCalendar::setTime", set_time),
        ("IntlCalendar::setTimeZone", set_time_zone),
        ("IntlCalendar::toDateTime", to_date_time),
        ("IntlGregorianCalendar::createFromDate", greg_create_from_date),
        ("IntlGregorianCalendar::createFromDateTime", greg_create_from_date_time),
        ("IntlGregorianCalendar::__construct", greg_construct),
        ("intlgregcal_create_instance", greg_create_instance),
        ("IntlGregorianCalendar::setGregorianChange", set_gregorian_change),
        ("IntlGregorianCalendar::getGregorianChange", get_gregorian_change),
        ("IntlGregorianCalendar::isLeapYear", is_leap_year),
    ]
}

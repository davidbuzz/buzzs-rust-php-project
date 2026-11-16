// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `IntlDateFormatter` and the `datefmt_*()` functions.

use php_intl::calendar::{IntlCalendar, PhpDateTime};
use php_intl::dateformat::{
    self as lib, CalendarArg, DateArg, IntlDateFormatter, ObjectArg, ObjectFormat, Timestamp,
    TmArray, TmElem,
};
use php_intl::timezone::{PhpZone, TimeZone, TimeZoneArg};
use php_intl::{Error, UErrorCode};
use php_runtime::{Obj, Value, ZArray, ZStr};
use php_vm::ext::{Args, NativeFn, VmResult};
use php_vm::Vm;

use crate::core::{self, s};
use crate::dt;
use crate::locale::text;

/// An owned time zone argument (see [`TimeZoneArg`]).
pub(crate) enum OwnedTz {
    Intl(TimeZone),
    Php(PhpZone),
    Str(ZStr),
}

impl OwnedTz {
    pub(crate) fn view(&self) -> TimeZoneArg<'_> {
        match self {
            Self::Intl(t) => TimeZoneArg::Intl(t),
            Self::Php(z) => TimeZoneArg::DateTimeZone(z),
            Self::Str(z) => TimeZoneArg::Str(z.as_bytes()),
        }
    }
}

/// Converts a time zone argument value (not `null`) without processing it.
pub(crate) fn owned_tz(vm: &mut Vm, a: &Args, v: Value, n: usize) -> VmResult<OwnedTz> {
    match v {
        Value::Object(o) => {
            if dt::instance_of(&o, "IntlTimeZone") {
                match crate::timezone::zone_of(&o) {
                    Some(t) => Ok(OwnedTz::Intl(t)),
                    None => Err(core::throw_engine(
                        vm,
                        a,
                        Error::IntlExceptionInFunction(
                            "passed IntlTimeZone is not properly constructed".into(),
                        ),
                        0,
                    )),
                }
            } else if dt::instance_of(&o, "DateTimeZone") {
                Ok(OwnedTz::Php(dt::php_zone(vm, &o)?))
            } else if crate::collator::has_to_string(&o) {
                Ok(OwnedTz::Str(vm.value_to_string(&Value::Object(o))?))
            } else {
                let cls = php_vm::object::class_of(&o).name.to_string_lossy();
                Err(core::type_error(
                    vm,
                    a,
                    n + 1,
                    &format!("Object of class {cls} could not be converted to string"),
                ))
            }
        }
        other => Ok(OwnedTz::Str(vm.value_to_string(&other)?)),
    }
}

/// An owned calendar argument.
enum OwnedCal {
    Null,
    Type(i64),
    Object(IntlCalendar),
}

impl OwnedCal {
    fn view(&self) -> CalendarArg<'_> {
        match self {
            Self::Null => CalendarArg::Null,
            Self::Type(t) => CalendarArg::Type(*t),
            Self::Object(c) => CalendarArg::Object(c),
        }
    }
}

fn owned_cal(vm: &mut Vm, a: &Args, v: Option<Value>, n: usize) -> VmResult<OwnedCal> {
    Ok(match v {
        None | Some(Value::Null) => OwnedCal::Null,
        Some(Value::Int(i)) => OwnedCal::Type(i),
        Some(Value::Object(o)) if dt::instance_of(&o, "IntlCalendar") => {
            match crate::calendar::require(&o) {
                Some(c) => OwnedCal::Object(c),
                None => {
                    return Err(core::throw_engine(
                        vm,
                        a,
                        Error::IntlExceptionInFunction(
                            "Found unconstructed IntlCalendar object".into(),
                        ),
                        0,
                    ))
                }
            }
        }
        Some(Value::Float(f)) if f.fract() == 0.0 => {
            #[allow(clippy::cast_possible_truncation)]
            let i = f as i64;
            OwnedCal::Type(i)
        }
        Some(Value::Bool(b)) => OwnedCal::Type(i64::from(b)),
        Some(Value::Str(z)) if php_intl::common::numeric_string(z.as_bytes()).is_some() => {
            OwnedCal::Type(vm.value_to_int(&Value::Str(z))?)
        }
        Some(other) => {
            let given = core::type_name(&other);
            return Err(core::type_error(
                vm,
                a,
                n + 1,
                &format!("must be of type IntlCalendar|int|null, {given} given"),
            ));
        }
    })
}

/// `datefmt_ctor`: returns whether construction succeeded.
fn ctor(vm: &mut Vm, a: &Args, o: &Obj) -> VmResult<bool> {
    core::reset_global();
    let tz = match a.opt(3) {
        Some(v) => Some(owned_tz(vm, a, v, 3)?),
        None => None,
    };
    let cal = owned_cal(vm, a, a.opt(4), 4)?;
    if core::is_constructed::<IntlDateFormatter>(o) {
        core::errors_set(
            vm,
            o,
            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
            "cannot call constructor twice",
        )?;
        return Ok(false);
    }
    let locale = a.opt(0).map(|v| match v {
        Value::Str(z) => text(&z),
        _ => String::new(),
    });
    let pattern = match a.opt(5) {
        Some(Value::Str(z)) => Some(z),
        _ => None,
    };
    let default_tz = dt::default_tz_name(vm)?;
    let r = IntlDateFormatter::new(
        locale.as_deref(),
        a.int(1),
        a.int(2),
        tz.as_ref().map(OwnedTz::view),
        cal.view(),
        pattern.as_ref().map(ZStr::as_bytes),
        &default_tz,
    );
    match r {
        Ok(f) => {
            core::set_data(o, f);
            Ok(true)
        }
        Err(e) => {
            core::fail(vm, a, Some(o), e, 0)?;
            Ok(false)
        }
    }
}

fn construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    core::in_ctor(|| ctor(vm, a, &o))?;
    Ok(Value::Null)
}

fn create(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = core::instantiate(vm, "IntlDateFormatter")?;
    if ctor(vm, a, &o)? {
        Ok(Value::Object(o))
    } else {
        Ok(Value::Null)
    }
}

fn fetch(vm: &mut Vm, a: &Args) -> VmResult<(Obj, usize)> {
    let (o, off) = core::target(a);
    core::reset_both(&o);
    if !core::is_constructed::<IntlDateFormatter>(&o) {
        return Err(core::unconstructed(vm, "IntlDateFormatter"));
    }
    Ok((o, off))
}

fn run<T>(
    vm: &mut Vm,
    a: &Args,
    o: &Obj,
    off: usize,
    f: impl FnOnce(&mut IntlDateFormatter) -> php_intl::Result<T>,
    ok: impl FnOnce(T) -> Value,
) -> VmResult<Value> {
    match core::with::<IntlDateFormatter, _>(o, f) {
        Some(Ok(v)) => Ok(ok(v)),
        Some(Err(e)) => core::fail_with(vm, a, Some(o), e, off, Value::Bool(false)),
        None => Ok(Value::Bool(false)),
    }
}

macro_rules! simple {
    ($name:ident, |$f:ident, $a:ident, $off:ident| $body:expr, $ok:expr) => {
        fn $name(vm: &mut Vm, $a: &mut Args) -> VmResult<Value> {
            let (o, $off) = fetch(vm, $a)?;
            let _ = $off;
            run(vm, $a, &o, $off, |$f| $body, $ok)
        }
    };
}

simple!(get_date_type, |f, a, off| Ok(f.date_type()), Value::Int);
simple!(get_time_type, |f, a, off| Ok(f.time_type()), Value::Int);
simple!(get_calendar, |f, a, off| Ok(f.calendar_type()), |t| t
    .map_or(Value::Bool(false), Value::Int));
simple!(get_time_zone_id, |f, a, off| f.time_zone_id(), s);
simple!(
    set_pattern,
    |f, a, off| f.set_pattern(a.str(off).as_bytes()),
    |()| Value::Bool(true)
);
simple!(get_pattern, |f, a, off| f.pattern(), s);
simple!(get_locale, |f, a, off| f.locale(a.int(off)), s);
simple!(
    set_lenient,
    |f, a, off| {
        f.set_lenient(a.bool(off));
        Ok(())
    },
    |()| Value::Null
);
simple!(is_lenient, |f, a, off| Ok(f.is_lenient()), Value::Bool);

fn get_time_zone(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = fetch(vm, a)?;
    match core::with::<IntlDateFormatter, _>(&o, |f| f.time_zone()) {
        Some(Ok(t)) => crate::timezone::object(vm, t),
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, off, Value::Bool(false)),
        None => Ok(Value::Bool(false)),
    }
}

fn get_calendar_object(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = fetch(vm, a)?;
    match core::with::<IntlDateFormatter, _>(&o, |f| f.calendar_object()) {
        Some(Ok(c)) => crate::calendar::object(vm, c),
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, off, Value::Bool(false)),
        None => Ok(Value::Bool(false)),
    }
}

fn set_time_zone(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = fetch(vm, a)?;
    let tz = match a.opt(off) {
        Some(v) => owned_tz(vm, a, v, off)?,
        None => OwnedTz::Str(ZStr::from(dt::default_tz_name(vm)?)),
    };
    run(
        vm,
        a,
        &o,
        off,
        |f| f.set_time_zone(tz.view()),
        |()| Value::Bool(true),
    )
}

fn set_calendar(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = fetch(vm, a)?;
    let cal = owned_cal(vm, a, a.opt(off), off)?;
    run(
        vm,
        a,
        &o,
        off,
        |f| f.set_calendar(cal.view()),
        |()| Value::Bool(true),
    )
}

/// An owned `format()` argument.
enum OwnedDate {
    Int(i64),
    Float(f64),
    Str(ZStr),
    DateTime(PhpDateTime),
    Calendar(IntlCalendar),
    Array(TmArray),
    OtherObject,
    Other,
}

impl OwnedDate {
    fn view(&self) -> DateArg<'_> {
        match self {
            Self::Int(i) => DateArg::Int(*i),
            Self::Float(f) => DateArg::Float(*f),
            Self::Str(z) => DateArg::Str(z.as_bytes()),
            Self::DateTime(d) => DateArg::DateTime(d),
            Self::Calendar(c) => DateArg::Calendar(c),
            Self::Array(t) => DateArg::Array(t),
            Self::OtherObject => DateArg::OtherObject,
            Self::Other => DateArg::Other,
        }
    }
}

fn tm_elem(arr: &ZArray, key: &str) -> TmElem {
    match arr.get_str(key.as_bytes()).map(Value::deref) {
        None => TmElem::Missing,
        Some(Value::Int(i)) => TmElem::Int(i),
        Some(_) => TmElem::NonInt,
    }
}

fn owned_date(vm: &mut Vm, v: Value) -> VmResult<OwnedDate> {
    Ok(match v {
        Value::Int(i) => OwnedDate::Int(i),
        Value::Float(f) => OwnedDate::Float(f),
        Value::Str(z) => OwnedDate::Str(z),
        Value::Array(arr) => OwnedDate::Array(TmArray {
            year: tm_elem(&arr, "tm_year"),
            mon: tm_elem(&arr, "tm_mon"),
            hour: tm_elem(&arr, "tm_hour"),
            min: tm_elem(&arr, "tm_min"),
            sec: tm_elem(&arr, "tm_sec"),
            mday: tm_elem(&arr, "tm_mday"),
            empty: arr.is_empty(),
        }),
        Value::Object(o) => {
            if dt::instance_of(&o, "DateTimeInterface") {
                OwnedDate::DateTime(dt::php_datetime(vm, &o)?)
            } else if dt::instance_of(&o, "IntlCalendar") {
                match crate::calendar::require(&o) {
                    Some(c) => OwnedDate::Calendar(c),
                    None => OwnedDate::OtherObject,
                }
            } else {
                OwnedDate::OtherObject
            }
        }
        _ => OwnedDate::Other,
    })
}

fn format(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = fetch(vm, a)?;
    let d = owned_date(vm, a.at(off))?;
    run(vm, a, &o, off, |f| f.format(d.view()), s)
}

fn format_object(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let obj = match a.at(0) {
        Value::Object(o) => o,
        other => {
            let given = core::type_name(&other);
            return Err(core::type_error(
                vm,
                a,
                1,
                &format!("must be of type object, {given} given"),
            ));
        }
    };
    let owned = if dt::instance_of(&obj, "IntlCalendar") {
        let Some(c) = crate::calendar::require(&obj) else {
            core::global_error_set(
                vm,
                UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                "bad IntlCalendar instance: not initialized properly",
            )?;
            return Ok(Value::Bool(false));
        };
        Some(OwnedDate::Calendar(c))
    } else if dt::instance_of(&obj, "DateTimeInterface") {
        Some(OwnedDate::DateTime(dt::php_datetime(vm, &obj)?))
    } else {
        None
    };
    let fmt_val = a.opt(1);
    let pattern_holder;
    let format = match &fmt_val {
        None => ObjectFormat::Default,
        Some(Value::Array(arr)) => {
            let get = |i: i64| match arr.get_int(i).map(Value::deref) {
                Some(Value::Int(x)) => Some(x),
                _ => None,
            };
            ObjectFormat::Styles {
                count: arr.len(),
                date: get(0),
                time: get(1),
            }
        }
        Some(Value::Int(i)) => ObjectFormat::Style(*i),
        Some(v) => {
            pattern_holder = vm.value_to_string(v)?;
            ObjectFormat::Pattern(pattern_holder.as_bytes())
        }
    };
    let locale = a.opt(2).map(|v| match v {
        Value::Str(z) => text(&z),
        _ => String::new(),
    });
    let arg = match &owned {
        Some(OwnedDate::Calendar(c)) => ObjectArg::Calendar(c),
        Some(OwnedDate::DateTime(d)) => ObjectArg::DateTime(d),
        _ => ObjectArg::Other,
    };
    match lib::format_object(arg, format, locale.as_deref()) {
        Ok(v) => Ok(s(v)),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Bool(false)),
    }
}

/// `zval_try_get_long()` (`None` = failed).
pub(crate) fn try_get_long(vm: &mut Vm, v: &Value) -> VmResult<Option<i64>> {
    Ok(match v {
        Value::Int(i) => Some(*i),
        Value::Null => Some(0),
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Float(f) => Some(float_to_long(vm, *f)?),
        Value::Str(z) => match php_intl::common::numeric_string(z.as_bytes()) {
            Some(php_intl::common::Numeric::Long(l)) => Some(l),
            Some(php_intl::common::Numeric::Double(d)) => Some(float_to_long(vm, d)?),
            None => None,
        },
        _ => None,
    })
}

/// A float to int conversion with PHP's diagnostics.
fn float_to_long(vm: &mut Vm, f: f64) -> VmResult<i64> {
    #[allow(clippy::cast_precision_loss)]
    let in_range = f.is_finite() && f >= i64::MIN as f64 && f < i64::MAX as f64;
    if !in_range {
        let repr = php_intl::common::php_gcvt(f, 17);
        vm.raise(
            2,
            &format!("The float {repr} is not representable as an int, cast occurred"),
        )?;
        return Ok(php_intl::common::dval_to_lval_cap(f));
    }
    if f.fract() != 0.0 {
        let repr = php_intl::common::php_gcvt(f, 17);
        vm.raise(
            8192,
            &format!("Implicit conversion from float {repr} to int loses precision"),
        )?;
    }
    Ok(php_intl::common::dval_to_lval_cap(f))
}

/// Reads the `&$offset` argument: `Ok(Err(throw))` for type errors.
fn read_offset(vm: &mut Vm, a: &Args, n: usize) -> VmResult<Option<i64>> {
    if !a.has(n) {
        return Ok(None);
    }
    let v = a.at(n);
    if let Some(l) = try_get_long(vm, &v)? {
        return Ok(Some(l));
    }
    let given = core::type_name(&v);
    Err(core::type_error(
        vm,
        a,
        n + 1,
        &format!("must be of type int, {given} given"),
    ))
}

fn timestamp(t: Timestamp) -> Value {
    match t {
        Timestamp::Int(i) => Value::Int(i),
        Timestamp::Float(f) => Value::Float(f),
    }
}

fn finish_parse<T>(
    vm: &mut Vm,
    a: &Args,
    o: &Obj,
    off: usize,
    r: Option<(php_intl::Result<T>, Option<i64>)>,
    ok: impl FnOnce(T) -> Value,
) -> VmResult<Value> {
    let Some((r, new_off)) = r else {
        return Ok(Value::Bool(false));
    };
    if let Some(p) = new_off {
        a.set_ref(off + 1, Value::Int(p));
    }
    match r {
        Ok(v) => Ok(ok(v)),
        Err(e) => core::fail_with(vm, a, Some(o), e, off, Value::Bool(false)),
    }
}

fn parse(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = fetch(vm, a)?;
    let pos = read_offset(vm, a, off + 1)?;
    let text = a.str(off);
    let r = core::with::<IntlDateFormatter, _>(&o, |f| f.parse(text.as_bytes(), pos));
    finish_parse(vm, a, &o, off, r, timestamp)
}

fn parse_to_calendar(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = fetch(vm, a)?;
    let pos = read_offset(vm, a, off + 1)?;
    let text = a.str(off);
    let r = core::with::<IntlDateFormatter, _>(&o, |f| f.parse_to_calendar(text.as_bytes(), pos));
    finish_parse(vm, a, &o, off, r, timestamp)
}

fn localtime(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = fetch(vm, a)?;
    let pos = read_offset(vm, a, off + 1)?;
    let text = a.str(off);
    let r = core::with::<IntlDateFormatter, _>(&o, |f| f.localtime(text.as_bytes(), pos));
    finish_parse(vm, a, &o, off, r, |fields| {
        let mut arr = ZArray::new();
        for (k, v) in fields {
            arr.make_mut().insert(ZStr::from(k), Value::Int(v));
        }
        Value::Array(arr)
    })
}

/// `clone_obj`.
pub(crate) fn clone(vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    core::create(vm, new)?;
    match core::with::<IntlDateFormatter, _>(old, |f| f.try_clone()) {
        Some(Ok(f)) => core::set_data(new, f),
        Some(Err(_)) => return Err(vm.throw("Error", "Failed to clone IntlDateFormatter")),
        None => return Err(vm.throw("Error", "Cannot clone uninitialized IntlDateFormatter")),
    }
    Ok(())
}

/// Handlers by stub key.
pub(crate) fn handlers() -> Vec<(&'static str, NativeFn)> {
    vec![
        ("IntlDateFormatter::__construct", construct),
        ("IntlDateFormatter::create", create),
        ("IntlDateFormatter::getDateType", get_date_type),
        ("IntlDateFormatter::getTimeType", get_time_type),
        ("IntlDateFormatter::getCalendar", get_calendar),
        ("IntlDateFormatter::setCalendar", set_calendar),
        ("IntlDateFormatter::getTimeZoneId", get_time_zone_id),
        ("IntlDateFormatter::getCalendarObject", get_calendar_object),
        ("IntlDateFormatter::getTimeZone", get_time_zone),
        ("IntlDateFormatter::setTimeZone", set_time_zone),
        ("datefmt_set_timezone", set_time_zone),
        ("IntlDateFormatter::setPattern", set_pattern),
        ("IntlDateFormatter::getPattern", get_pattern),
        ("IntlDateFormatter::getLocale", get_locale),
        ("IntlDateFormatter::setLenient", set_lenient),
        ("IntlDateFormatter::isLenient", is_lenient),
        ("IntlDateFormatter::format", format),
        ("IntlDateFormatter::formatObject", format_object),
        ("IntlDateFormatter::parse", parse),
        ("IntlDateFormatter::parseToCalendar", parse_to_calendar),
        ("IntlDateFormatter::localtime", localtime),
        ("IntlDateFormatter::getErrorCode", core::get_error_code),
        (
            "IntlDateFormatter::getErrorMessage",
            core::get_error_message,
        ),
    ]
}

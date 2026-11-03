//! `ext/calendar` for the phprust VM: binds the `php-calendar` library
//! through the `php_vm::ext` extension API.
//!
//! The library does all conversions; this crate maps PHP arguments to it,
//! turns [`CalError`] into `ValueError`s and supplies the process-local
//! time zone (C's `localtime_r`/`mktime`, which follow the `TZ`
//! environment variable rather than `date.timezone`).

// Native handlers share one signature even when they cannot fail.
#![allow(clippy::unnecessary_wraps)]

use std::sync::Arc;

use php_calendar::{self as cal, CalError, DayOfWeek};
use php_date::tz::TzInfo;
use php_runtime::{Value, ZArray, ZStr};
use php_vm::ext::{Args, Extension, ModuleBuilder, VmResult};
use php_vm::Vm;

/// The `calendar` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Calendar;

/// Throws `err` as a `ValueError` raised by `func`.
fn throw(vm: &mut Vm, func: &str, err: &CalError) -> php_vm::ext::Throw {
    vm.throw("ValueError", &err.message(func))
}

/// Maps a library result to a PHP value, throwing on error.
fn ok_or_throw<T>(vm: &mut Vm, func: &str, r: Result<T, CalError>) -> VmResult<T> {
    r.map_err(|e| throw(vm, func, &e))
}

/// The C library's local time zone: `$TZ`, else `/etc/timezone` or the
/// `/etc/localtime` link target, else UTC (`None`: no zone database).
fn system_tz() -> Option<Arc<TzInfo>> {
    let name = std::env::var("TZ")
        .ok()
        .map(|s| s.trim_start_matches(':').to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/timezone")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            std::fs::read_link("/etc/localtime").ok().and_then(|p| {
                let p = p.to_string_lossy().into_owned();
                p.split_once("zoneinfo/").map(|(_, z)| z.to_string())
            })
        });
    name.and_then(|n| php_date::tz::get(&n))
        .or_else(|| php_date::tz::get("UTC"))
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// `localtime_r(ts)` as `(year, month, day)`.
fn local_ymd(ts: i64) -> (i64, i64, i64) {
    let t = match system_tz() {
        Some(tz) => php_date::time::Time::from_timestamp_in(ts, &tz),
        None => php_date::time::Time::from_gmt(ts),
    };
    (t.y, t.m, t.d)
}

fn cal_days_in_month(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = cal::cal_days_in_month(a.int(0), a.int(1), a.int(2));
    ok_or_throw(vm, "cal_days_in_month", r).map(Value::Int)
}

fn cal_from_jd(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = ok_or_throw(vm, "cal_from_jd", cal::cal_from_jd(a.int(0), a.int(1)))?;
    let mut arr = ZArray::new();
    let t = arr.make_mut();
    t.insert("date", Value::from(r.date));
    t.insert("month", Value::Int(r.month));
    t.insert("day", Value::Int(r.day));
    t.insert("year", Value::Int(r.year));
    t.insert("dow", r.dow.map_or(Value::Null, Value::Int));
    t.insert("abbrevdayname", Value::from(r.abbrevdayname));
    t.insert("dayname", Value::from(r.dayname));
    t.insert("abbrevmonth", Value::from(r.abbrevmonth));
    t.insert("monthname", Value::from(r.monthname));
    Ok(Value::Array(arr))
}

fn info_array(info: &cal::CalInfo) -> Value {
    let names = |list: &[(i64, &str)]| {
        Value::Array(ZArray::from_pairs(
            list.iter().map(|(i, n)| (*i, Value::from(*n))),
        ))
    };
    let mut arr = ZArray::new();
    let t = arr.make_mut();
    t.insert("months", names(&info.months));
    t.insert("abbrevmonths", names(&info.abbrevmonths));
    t.insert("maxdaysinmonth", Value::Int(info.maxdaysinmonth));
    t.insert("calname", Value::from(info.calname));
    t.insert("calsymbol", Value::from(info.calsymbol));
    Value::Array(arr)
}

fn cal_info(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let which = if a.has(0) { a.int(0) } else { -1 };
    let infos = ok_or_throw(vm, "cal_info", cal::cal_info(which))?;
    if which == -1 {
        Ok(Value::Array(ZArray::from_values(
            infos.iter().map(info_array),
        )))
    } else {
        Ok(infos.first().map_or(Value::Null, info_array))
    }
}

fn cal_to_jd(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = cal::cal_to_jd(a.int(0), a.int(1), a.int(2), a.int(3));
    ok_or_throw(vm, "cal_to_jd", r).map(Value::Int)
}

/// The `?int $year = null` argument of the easter functions.
fn easter_year(a: &Args) -> i64 {
    match a.opt(0) {
        Some(Value::Int(y)) => y,
        _ => local_ymd(now()).0,
    }
}

fn easter_mode(a: &Args) -> i64 {
    if a.has(1) {
        a.int(1)
    } else {
        cal::CAL_EASTER_DEFAULT
    }
}

fn easter_date(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (year, mode) = (easter_year(a), easter_mode(a));
    let r = cal::easter_date(year, mode, |y, m, d| {
        let tz = system_tz();
        php_date::api::mktime(
            0,
            Some(0),
            Some(0),
            Some(m),
            Some(d),
            Some(y),
            now(),
            tz.as_ref(),
        )
    });
    ok_or_throw(vm, "easter_date", r).map(Value::Int)
}

fn easter_days(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = cal::easter_days(easter_year(a), easter_mode(a));
    ok_or_throw(vm, "easter_days", r).map(Value::Int)
}

fn frenchtojd(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(cal::frenchtojd(a.int(0), a.int(1), a.int(2))))
}

fn gregoriantojd(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(cal::gregoriantojd(a.int(0), a.int(1), a.int(2))))
}

fn juliantojd(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(cal::juliantojd(a.int(0), a.int(1), a.int(2))))
}

fn jewishtojd(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = cal::jewishtojd(a.int(0), a.int(1), a.int(2));
    ok_or_throw(vm, "jewishtojd", r).map(Value::Int)
}

fn jddayofweek(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mode = if a.has(1) {
        a.int(1)
    } else {
        cal::CAL_DOW_DAYNO
    };
    Ok(match cal::jddayofweek(a.int(0), mode) {
        DayOfWeek::Number(n) => Value::Int(n),
        DayOfWeek::Name(s) => Value::from(s),
    })
}

fn jdmonthname(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::from(cal::jdmonthname(a.int(0), a.int(1))))
}

fn jdtofrench(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::from(cal::jdtofrench(a.int(0))))
}

fn jdtogregorian(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::from(cal::jdtogregorian(a.int(0))))
}

fn jdtojulian(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::from(cal::jdtojulian(a.int(0))))
}

fn jdtojewish(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let hebrew = a.has(1) && a.bool(1);
    let flags = if a.has(2) { a.int(2) } else { 0 };
    let r = ok_or_throw(vm, "jdtojewish", cal::jdtojewish(a.int(0), hebrew, flags))?;
    Ok(Value::Str(ZStr::from(r.as_slice())))
}

fn jdtounix(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = cal::jdtounix(a.int(0));
    ok_or_throw(vm, "jdtounix", r).map(Value::Int)
}

fn unixtojd(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let ts = match a.opt(0) {
        Some(Value::Int(t)) => t,
        _ => now(),
    };
    let r = cal::unixtojd(ts, |t| Some(local_ymd(t)));
    Ok(ok_or_throw(vm, "unixtojd", r)?.map_or(Value::Bool(false), Value::Int))
}

impl Extension for Calendar {
    fn name(&self) -> &'static str {
        "calendar"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        for (name, value) in cal::CONSTANTS {
            m.constant(name, Value::Int(value));
        }
        m.function(
            "cal_days_in_month(int $calendar, int $month, int $year): int",
            cal_days_in_month,
        )
        .function(
            "cal_from_jd(int $julian_day, int $calendar): array",
            cal_from_jd,
        )
        .function("cal_info(int $calendar = -1): array", cal_info)
        .function(
            "cal_to_jd(int $calendar, int $month, int $day, int $year): int",
            cal_to_jd,
        )
        .function(
            "easter_date(?int $year = null, int $mode = CAL_EASTER_DEFAULT): int",
            easter_date,
        )
        .function(
            "easter_days(?int $year = null, int $mode = CAL_EASTER_DEFAULT): int",
            easter_days,
        )
        .function(
            "frenchtojd(int $month, int $day, int $year): int",
            frenchtojd,
        )
        .function(
            "gregoriantojd(int $month, int $day, int $year): int",
            gregoriantojd,
        )
        .function(
            "jddayofweek(int $julian_day, int $mode = CAL_DOW_DAYNO): int|string",
            jddayofweek,
        )
        .function(
            "jdmonthname(int $julian_day, int $mode): string",
            jdmonthname,
        )
        .function("jdtofrench(int $julian_day): string", jdtofrench)
        .function("jdtogregorian(int $julian_day): string", jdtogregorian)
        .function(
            "jdtojewish(int $julian_day, bool $hebrew = false, int $flags = 0): string",
            jdtojewish,
        )
        .function("jdtojulian(int $julian_day): string", jdtojulian)
        .function("jdtounix(int $julian_day): int", jdtounix)
        .function(
            "jewishtojd(int $month, int $day, int $year): int",
            jewishtojd,
        )
        .function(
            "juliantojd(int $month, int $day, int $year): int",
            juliantojd,
        )
        .function("unixtojd(?int $timestamp = null): int|false", unixtojd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_ymd_of_epoch_is_1970_or_1969() {
        let (y, m, d) = local_ymd(0);
        assert!((y, m, d) == (1970, 1, 1) || (y, m, d) == (1969, 12, 31));
    }

    #[test]
    fn errors_map_to_php_messages() {
        let e = cal::cal_to_jd(9, 1, 1, 1).unwrap_err();
        assert_eq!(
            e.message("cal_to_jd"),
            "cal_to_jd(): Argument #1 ($calendar) must be a valid calendar ID"
        );
    }

    #[test]
    fn info_array_has_php_keys() {
        let info = cal::CalInfo::for_calendar(cal::Calendar::Gregorian);
        let Value::Array(a) = info_array(&info) else {
            panic!("not an array");
        };
        let keys: Vec<String> = a.keys().map(|k| format!("{k:?}")).collect();
        assert_eq!(keys.len(), 5);
        assert!(a.get_str(b"calsymbol").is_some());
    }
}

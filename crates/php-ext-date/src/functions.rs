//! The procedural functions of ext/date that do not operate on objects:
//! `date()`, `strtotime()`, `mktime()`, `date_parse()`, the timezone
//! listing functions, the sun functions, ...

use php_date::api;
use php_date::tz;
use php_runtime::{Value, ZArray, ZStr};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::common::{
    default_tz, default_tz_name, ini_double, last_errors, lossy, php_time, set_tz_override,
    to_php, E_DEPRECATED,
};

/// The optional `?int $timestamp` argument `n`, defaulting to now.
fn ts_arg(a: &Args, n: usize) -> i64 {
    match a.opt(n) {
        Some(Value::Int(i)) => i,
        _ => php_time(),
    }
}

fn date(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let tz = default_tz(vm)?;
    let ts = ts_arg(a, 1);
    Ok(Value::Str(ZStr::from(api::date(a.str(0).as_bytes(), ts, &tz))))
}

#[allow(clippy::unnecessary_wraps)]
fn gmdate(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let ts = ts_arg(a, 1);
    Ok(Value::Str(ZStr::from(api::gmdate(a.str(0).as_bytes(), ts))))
}

fn idate(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let format = a.str(0);
    if format.len() != 1 {
        vm.php_warning(api::IdateError::FormatNotOneChar.message())?;
        return Ok(Value::Bool(false));
    }
    let tz = default_tz(vm)?;
    let ts = ts_arg(a, 1);
    match api::idate(format.as_bytes(), ts, &tz) {
        Ok(v) => Ok(Value::Int(v)),
        Err(e) => {
            vm.php_warning(e.message())?;
            Ok(Value::Bool(false))
        }
    }
}

fn strtotime(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    if s.is_empty() {
        return Ok(Value::Bool(false));
    }
    let tz = default_tz(vm)?;
    let base = ts_arg(a, 1);
    Ok(api::strtotime(s.as_bytes(), base, &tz).map_or(Value::Bool(false), Value::Int))
}

fn mktime_impl(vm: &mut Vm, a: &Args, gmt: bool) -> VmResult<Value> {
    let opt = |n: usize| match a.opt(n) {
        Some(Value::Int(i)) => Some(i),
        _ => None,
    };
    let tz = if gmt { None } else { Some(default_tz(vm)?) };
    let ts = api::mktime(
        a.int(0),
        opt(1),
        opt(2),
        opt(3),
        opt(4),
        opt(5),
        php_time(),
        tz.as_ref(),
    );
    Ok(Value::Int(ts))
}

fn mktime(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    mktime_impl(vm, a, false)
}

fn gmmktime(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    mktime_impl(vm, a, true)
}

#[allow(clippy::unnecessary_wraps)]
fn checkdate(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (m, d, y) = (a.int(0), a.int(1), a.int(2));
    Ok(Value::Bool(
        (1..=32767).contains(&y) && api::checkdate(m, d, y),
    ))
}

fn strftime_impl(vm: &mut Vm, a: &Args, gmt: bool) -> VmResult<Value> {
    let name = if gmt { "gmstrftime" } else { "strftime" };
    vm.raise(
        E_DEPRECATED,
        &format!("Function {name}() is deprecated since 8.1, use IntlDateFormatter::format() instead"),
    )?;
    let format = a.str(0);
    if format.is_empty() {
        return Ok(Value::Bool(false));
    }
    let ts = ts_arg(a, 1);
    let tz = if gmt { None } else { Some(default_tz(vm)?) };
    Ok(
        php_date::format::strftime(format.as_bytes(), ts, tz.as_ref())
            .map_or(Value::Bool(false), |s| Value::Str(ZStr::from(s))),
    )
}

fn strftime(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    strftime_impl(vm, a, false)
}

fn gmstrftime(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    strftime_impl(vm, a, true)
}

#[allow(clippy::unnecessary_wraps)]
fn time(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(php_time()))
}

fn localtime(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let ts = ts_arg(a, 0);
    let assoc = a.has(1) && a.bool(1);
    let tz = default_tz(vm)?;
    Ok(to_php(&api::localtime(ts, &tz, assoc)))
}

fn getdate(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let ts = ts_arg(a, 0);
    let tz = default_tz(vm)?;
    Ok(to_php(&api::getdate(ts, &tz)))
}

#[allow(clippy::unnecessary_wraps)]
fn date_parse(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(to_php(&api::date_parse(a.str(0).as_bytes())))
}

#[allow(clippy::unnecessary_wraps)]
fn date_parse_from_format(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(to_php(&api::date_parse_from_format(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
    )))
}

/// `date_get_last_errors()` / `DateTime::getLastErrors()`.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn date_get_last_errors(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(last_errors()
        .and_then(|e| api::last_errors(&e))
        .map_or(Value::Bool(false), |v| to_php(&v)))
}

#[allow(clippy::unnecessary_wraps)]
fn timezone_name_from_abbr(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let abbr = lossy(a.str(0).as_bytes());
    let off = if a.has(1) { a.int(1) } else { -1 };
    let dst = if a.has(2) { a.int(2) } else { -1 };
    Ok(tz::timezone_id_from_abbr(&abbr, off, dst)
        .map_or(Value::Bool(false), |s| Value::Str(ZStr::from(s))))
}

/// `timezone_identifiers_list()` / `DateTimeZone::listIdentifiers()`.
pub(crate) fn timezone_identifiers_list(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let what = if a.has(0) { a.int(0) } else { tz::group::ALL };
    let country = a.opt(1).map(|_| lossy(a.str(1).as_bytes()));
    if what == tz::group::PER_COUNTRY && country.as_deref().map_or(0, str::len) != 2 {
        return Err(a.value_error(vm, 2, tz::PER_COUNTRY_ERROR));
    }
    match tz::identifiers_list(what, country.as_deref()) {
        Ok(ids) => Ok(Value::Array(ZArray::from_values(
            ids.into_iter().map(|s| Value::Str(ZStr::from(s))),
        ))),
        Err(m) => Err(a.value_error(vm, 2, m)),
    }
}

/// `timezone_abbreviations_list()` / `DateTimeZone::listAbbreviations()`.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn timezone_abbreviations_list(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(to_php(&api::timezone_abbreviations_list()))
}

#[allow(clippy::unnecessary_wraps)]
fn timezone_version_get(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Str(ZStr::from(tz::version())))
}

fn date_default_timezone_set(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let zone = a.str(0);
    let name = lossy(zone.as_bytes());
    if zone.as_bytes().contains(&0) || !tz::is_valid_id(&name) {
        vm.php_notice(&format!("Timezone ID '{name}' is invalid"))?;
        return Ok(Value::Bool(false));
    }
    set_tz_override(&name);
    Ok(Value::Bool(true))
}

fn date_default_timezone_get(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let tz = default_tz(vm)?;
    // The zone keeps the spelling it was requested with.
    let _ = default_tz_name(vm);
    Ok(Value::Str(ZStr::from(tz.name.as_str())))
}

fn sunrise_sunset(vm: &mut Vm, a: &Args, sunset: bool) -> VmResult<Value> {
    let name = if sunset { "date_sunset" } else { "date_sunrise" };
    vm.raise(
        E_DEPRECATED,
        &format!("Function {name}() is deprecated since 8.1, use date_sun_info() instead"),
    )?;
    let time = a.int(0);
    let retformat = if a.has(1) {
        a.int(1)
    } else {
        api::SUNFUNCS_RET_STRING
    };
    let float_or = |vm: &Vm, n: usize, ini: &str| match a.opt(n) {
        Some(Value::Float(f)) => f,
        #[allow(clippy::cast_precision_loss)]
        Some(Value::Int(i)) => i as f64,
        _ => ini_double(vm, ini),
    };
    let latitude = float_or(vm, 2, "date.default_latitude");
    let longitude = float_or(vm, 3, "date.default_longitude");
    let zenith = float_or(
        vm,
        4,
        if sunset {
            "date.sunset_zenith"
        } else {
            "date.sunrise_zenith"
        },
    );
    if !matches!(
        retformat,
        api::SUNFUNCS_RET_TIMESTAMP | api::SUNFUNCS_RET_STRING | api::SUNFUNCS_RET_DOUBLE
    ) {
        return Err(a.value_error(
            vm,
            2,
            "must be one of SUNFUNCS_RET_TIMESTAMP, SUNFUNCS_RET_STRING, or SUNFUNCS_RET_DOUBLE",
        ));
    }
    let gmt_offset = match a.opt(5) {
        Some(Value::Float(f)) => Some(f),
        #[allow(clippy::cast_precision_loss)]
        Some(Value::Int(i)) => Some(i as f64),
        _ => None,
    };
    let tz = default_tz(vm)?;
    Ok(to_php(&api::date_sunrise_sunset(
        time, retformat, latitude, longitude, zenith, gmt_offset, &tz, sunset,
    )))
}

fn date_sunrise(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    sunrise_sunset(vm, a, false)
}

fn date_sunset(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    sunrise_sunset(vm, a, true)
}

fn date_sun_info(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let time = a.int(0);
    let lat = a.float(1);
    let lon = a.float(2);
    if !lat.is_finite() {
        return Err(a.value_error(vm, 2, "must be finite"));
    }
    if !lon.is_finite() {
        return Err(a.value_error(vm, 3, "must be finite"));
    }
    let tz = default_tz(vm)?;
    Ok(to_php(&api::date_sun_info(time, lat, lon, &tz)))
}

/// Registers the functions of this module.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function("strtotime(string $datetime, ?int $baseTimestamp = null): int|false", strtotime)
        .function("date(string $format, ?int $timestamp = null): string", date)
        .function("idate(string $format, ?int $timestamp = null): int|false", idate)
        .function("gmdate(string $format, ?int $timestamp = null): string", gmdate)
        .function(
            "mktime(int $hour, ?int $minute = null, ?int $second = null, ?int $month = null, ?int $day = null, ?int $year = null): int|false",
            mktime,
        )
        .function(
            "gmmktime(int $hour, ?int $minute = null, ?int $second = null, ?int $month = null, ?int $day = null, ?int $year = null): int|false",
            gmmktime,
        )
        .function("checkdate(int $month, int $day, int $year): bool", checkdate)
        .function("strftime(string $format, ?int $timestamp = null): string|false", strftime)
        .function("gmstrftime(string $format, ?int $timestamp = null): string|false", gmstrftime)
        .function("time(): int", time)
        .function("localtime(?int $timestamp = null, bool $associative = false): array", localtime)
        .function("getdate(?int $timestamp = null): array", getdate)
        .function("date_parse(string $datetime): array", date_parse)
        .function(
            "date_parse_from_format(string $format, string $datetime): array",
            date_parse_from_format,
        )
        .function("date_get_last_errors(): array|false", date_get_last_errors)
        .function(
            "timezone_name_from_abbr(string $abbr, int $utcOffset = -1, int $isDST = -1): string|false",
            timezone_name_from_abbr,
        )
        .function(
            "timezone_identifiers_list(int $timezoneGroup = 2047, ?string $countryCode = null): array",
            timezone_identifiers_list,
        )
        .function("timezone_abbreviations_list(): array", timezone_abbreviations_list)
        .function("timezone_version_get(): string", timezone_version_get)
        .function("date_default_timezone_set(string $timezoneId): bool", date_default_timezone_set)
        .function("date_default_timezone_get(): string", date_default_timezone_get)
        .function(
            "date_sunrise(int $timestamp, int $returnFormat = 1, ?float $latitude = null, ?float $longitude = null, ?float $zenith = null, ?float $utcOffset = null): string|int|float|false",
            date_sunrise,
        )
        .function(
            "date_sunset(int $timestamp, int $returnFormat = 1, ?float $latitude = null, ?float $longitude = null, ?float $zenith = null, ?float $utcOffset = null): string|int|float|false",
            date_sunset,
        )
        .function(
            "date_sun_info(int $timestamp, float $latitude, float $longitude): array",
            date_sun_info,
        );
}

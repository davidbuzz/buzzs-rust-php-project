//! PHP-function-shaped entry points built on the timelib port.
//!
//! These mirror the bodies of `php_date.c`'s functions and methods so the
//! VM only has to marshal arguments and return values. Array-valued results
//! are returned as [`Value`] trees (ordered, PHP-array-like) and object
//! state is plain [`Time`] / [`RelTime`] / [`TimezoneSpec`] data.

use std::sync::Arc;

use crate::astro::rise_set_altitude;
use crate::dow::{
    day_of_week, day_of_year, daynr_from_weeknr, days_in_month, is_leap, iso_day_of_week,
    isoweek_from_date,
};
use crate::format::{
    format_date, offset_string, swatch_beat, time_offset, DAY_FULL_NAMES, MON_FULL_NAMES,
};
use crate::interval;
use crate::parse::{
    parse_from_format, parse_iso_interval, strtotime as parse_strtotime, Errors, TimezoneSpec,
};
use crate::time::{RelTime, Time, ZoneType, FIRST_DAY_OF_MONTH, SPECIAL_WEEKDAY};
use crate::tz::{self, TzInfo};
use crate::UNSET;

/// An array key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    /// Integer key.
    Int(i64),
    /// String key.
    Str(String),
}

/// A PHP-like value used for array-shaped results.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `null`
    Null,
    /// `true` / `false`
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A float.
    Float(f64),
    /// A string.
    Str(String),
    /// An ordered array.
    Array(Vec<(Key, Value)>),
}

impl Value {
    fn assoc() -> Self {
        Self::Array(Vec::new())
    }

    /// Sets `key` (string key), replacing an existing entry in place like
    /// `zend_hash_str_update`.
    pub fn set(&mut self, key: &str, v: Value) {
        if let Self::Array(items) = self {
            if let Some(slot) = items
                .iter_mut()
                .find(|(k, _)| matches!(k, Key::Str(s) if s == key))
            {
                slot.1 = v;
            } else {
                items.push((Key::Str(key.to_string()), v));
            }
        }
    }

    /// Sets an integer key.
    pub fn set_index(&mut self, key: i64, v: Value) {
        if let Self::Array(items) = self {
            if let Some(slot) = items.iter_mut().find(|(k, _)| *k == Key::Int(key)) {
                slot.1 = v;
            } else {
                items.push((Key::Int(key), v));
            }
        }
    }

    /// Appends with the next integer key.
    pub fn push(&mut self, v: Value) {
        if let Self::Array(items) = self {
            let next = items
                .iter()
                .filter_map(|(k, _)| {
                    if let Key::Int(i) = k {
                        Some(*i + 1)
                    } else {
                        None
                    }
                })
                .max()
                .unwrap_or(0)
                .max(0);
            items.push((Key::Int(next), v));
        }
    }

    /// Looks up a string key.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Self::Array(items) => items
                .iter()
                .find(|(k, _)| matches!(k, Key::Str(s) if s == key))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    fn long_or_false(v: i64) -> Self {
        if v == UNSET {
            Self::Bool(false)
        } else {
            Self::Int(v)
        }
    }
}

// ---------------------------------------------------------------------------
// date(), gmdate(), idate()
// ---------------------------------------------------------------------------

/// `date($format, $ts)` with the default timezone `tz`.
pub fn date(format: &[u8], ts: i64, tz: &Arc<TzInfo>) -> Vec<u8> {
    let t = Time::from_timestamp_in(ts, tz);
    format_date(format, &t, true)
}

/// `gmdate($format, $ts)`.
pub fn gmdate(format: &[u8], ts: i64) -> Vec<u8> {
    format_date(format, &Time::from_gmt(ts), false)
}

/// Errors from `idate()` (emitted as `E_WARNING`, returning `false`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdateError {
    /// `idate format is one char`
    FormatNotOneChar,
    /// `Unrecognized date format token`
    UnrecognizedToken,
}

impl IdateError {
    /// PHP's warning text.
    pub fn message(self) -> &'static str {
        match self {
            Self::FormatNotOneChar => "idate format is one char",
            Self::UnrecognizedToken => "Unrecognized date format token",
        }
    }
}

/// `idate($format, $ts)` with the default timezone `tz`.
#[allow(clippy::cast_possible_truncation)]
pub fn idate(format: &[u8], ts: i64, tz: &Arc<TzInfo>) -> Result<i64, IdateError> {
    if format.len() != 1 {
        return Err(IdateError::FormatNotOneChar);
    }
    let t = Time::from_timestamp_in(ts, tz);
    let offset = tz.time_zone_info(t.sse);
    let (isoweek, isoyear) = isoweek_from_date(t.y, t.m, t.d);
    let int = |v: i64| i64::from(v as i32);
    let r = match format[0] {
        b'd' | b'j' => int(t.d),
        b'N' => iso_day_of_week(t.y, t.m, t.d),
        b'w' => day_of_week(t.y, t.m, t.d),
        b'z' => int(day_of_year(t.y, t.m, t.d)),
        b'W' => int(isoweek),
        b'm' | b'n' => int(t.m),
        b't' => days_in_month(t.y, t.m),
        b'L' => i64::from(is_leap(int(t.y))),
        b'y' => int(t.y % 100),
        b'Y' => int(t.y),
        b'o' => int(isoyear),
        b'B' => swatch_beat(t.sse),
        b'g' | b'h' => {
            if t.h % 12 != 0 {
                int(t.h % 12)
            } else {
                12
            }
        }
        b'H' | b'G' => int(t.h),
        b'i' => int(t.i),
        b's' => int(t.s),
        b'I' => i64::from(offset.is_dst),
        b'Z' => i64::from(offset.offset),
        b'U' => int(t.sse),
        _ => return Err(IdateError::UnrecognizedToken),
    };
    Ok(r)
}

// ---------------------------------------------------------------------------
// strtotime(), mktime(), checkdate()
// ---------------------------------------------------------------------------

/// `strtotime($datetime, $baseTimestamp)`; `None` means `false`.
pub fn strtotime(s: &[u8], now_ts: i64, tz: &Arc<TzInfo>) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    let now = Time::from_timestamp_in(now_ts, tz);
    let (mut t, errors) = parse_strtotime(s);
    if !errors.errors.is_empty() {
        return None;
    }
    t.fill_holes(&now, false);
    t.update_ts(Some(tz));
    Some(t.sse)
}

/// `mktime()` (`tz` = default timezone) / `gmmktime()` (`tz` = `None`).
///
/// `now_ts` supplies the omitted fields. Arguments are hour, then optional
/// minute, second, month, day, year.
pub fn mktime(
    hour: i64,
    min: Option<i64>,
    sec: Option<i64>,
    mon: Option<i64>,
    day: Option<i64>,
    year: Option<i64>,
    now_ts: i64,
    tz: Option<&Arc<TzInfo>>,
) -> i64 {
    let mut now = match tz {
        Some(tz) => Time::from_timestamp_in(now_ts, tz),
        None => Time::from_gmt(now_ts),
    };
    now.h = hour;
    if let Some(v) = min {
        now.i = v;
    }
    if let Some(v) = sec {
        now.s = v;
    }
    if let Some(v) = mon {
        now.m = v;
    }
    if let Some(v) = day {
        now.d = v;
    }
    if let Some(mut y) = year {
        if (0..70).contains(&y) {
            y += 2000;
        } else if (70..=100).contains(&y) {
            y += 1900;
        }
        now.y = y;
    }
    now.update_ts(tz);
    now.sse
}

/// `checkdate($month, $day, $year)`.
pub fn checkdate(month: i64, day: i64, year: i64) -> bool {
    crate::dow::checkdate(month, day, year)
}

// ---------------------------------------------------------------------------
// localtime(), getdate()
// ---------------------------------------------------------------------------

/// `localtime($ts, $associative)`.
pub fn localtime(ts: i64, tz: &Arc<TzInfo>, associative: bool) -> Value {
    let t = Time::from_timestamp_in(ts, tz);
    let fields = [
        ("tm_sec", t.s),
        ("tm_min", t.i),
        ("tm_hour", t.h),
        ("tm_mday", t.d),
        ("tm_mon", t.m - 1),
        ("tm_year", t.y - 1900),
        ("tm_wday", day_of_week(t.y, t.m, t.d)),
        ("tm_yday", day_of_year(t.y, t.m, t.d)),
        ("tm_isdst", t.dst),
    ];
    let mut v = Value::assoc();
    for (k, x) in fields {
        if associative {
            v.set(k, Value::Int(x));
        } else {
            v.push(Value::Int(x));
        }
    }
    v
}

/// `getdate($ts)`.
pub fn getdate(ts: i64, tz: &Arc<TzInfo>) -> Value {
    let t = Time::from_timestamp_in(ts, tz);
    let mut v = Value::assoc();
    v.set("seconds", Value::Int(t.s));
    v.set("minutes", Value::Int(t.i));
    v.set("hours", Value::Int(t.h));
    v.set("mday", Value::Int(t.d));
    v.set("wday", Value::Int(day_of_week(t.y, t.m, t.d)));
    v.set("mon", Value::Int(t.m));
    v.set("year", Value::Int(t.y));
    v.set("yday", Value::Int(day_of_year(t.y, t.m, t.d)));
    let dow = usize::try_from(day_of_week(t.y, t.m, t.d)).unwrap_or(0) % 7;
    v.set("weekday", Value::Str(DAY_FULL_NAMES[dow].to_string()));
    let mon = usize::try_from(t.m - 1).unwrap_or(0) % 12;
    v.set("month", Value::Str(MON_FULL_NAMES[mon].to_string()));
    v.set_index(0, Value::Int(ts));
    v
}

// ---------------------------------------------------------------------------
// date_parse(), date_parse_from_format(), date_get_last_errors()
// ---------------------------------------------------------------------------

/// Appends `warning_count`, `warnings`, `error_count`, `errors` to `v`
/// (`zval_from_error_container`).
pub fn add_errors(v: &mut Value, e: &Errors) {
    let list = |msgs: &[crate::parse::Message]| {
        let mut a = Value::assoc();
        for m in msgs {
            a.set_index(
                i64::try_from(m.position).unwrap_or(0),
                Value::Str(m.message.to_string()),
            );
        }
        a
    };
    v.set(
        "warning_count",
        Value::Int(i64::try_from(e.warnings.len()).unwrap_or(0)),
    );
    v.set("warnings", list(&e.warnings));
    v.set(
        "error_count",
        Value::Int(i64::try_from(e.errors.len()).unwrap_or(0)),
    );
    v.set("errors", list(&e.errors));
}

/// `date_get_last_errors()`: `None` (false) when there were no
/// errors/warnings.
pub fn last_errors(e: &Errors) -> Option<Value> {
    if e.is_empty() {
        return None;
    }
    let mut v = Value::assoc();
    add_errors(&mut v, e);
    Some(v)
}

/// The array returned by `date_parse()` / `date_parse_from_format()` for a
/// parsed time (`php_date_do_return_parsed_time`).
#[allow(clippy::cast_precision_loss)]
pub fn parsed_time_value(t: &Time, e: &Errors) -> Value {
    let mut v = Value::assoc();
    v.set("year", Value::long_or_false(t.y));
    v.set("month", Value::long_or_false(t.m));
    v.set("day", Value::long_or_false(t.d));
    v.set("hour", Value::long_or_false(t.h));
    v.set("minute", Value::long_or_false(t.i));
    v.set("second", Value::long_or_false(t.s));
    if t.us == UNSET {
        v.set("fraction", Value::Bool(false));
    } else {
        v.set("fraction", Value::Float(t.us as f64 / 1_000_000.0));
    }
    add_errors(&mut v, e);
    v.set("is_localtime", Value::Bool(t.is_localtime));
    if t.is_localtime {
        v.set("zone_type", Value::long_or_false(t.zone_type.as_i64()));
        match t.zone_type {
            ZoneType::Offset => {
                v.set("zone", Value::long_or_false(t.z));
                v.set("is_dst", Value::Bool(t.dst != 0));
            }
            ZoneType::Id => {
                if let Some(a) = &t.tz_abbr {
                    v.set("tz_abbr", Value::Str(a.clone()));
                }
                if let Some(tz) = &t.tz_info {
                    v.set("tz_id", Value::Str(tz.name.clone()));
                }
            }
            ZoneType::Abbr => {
                v.set("zone", Value::long_or_false(t.z));
                v.set("is_dst", Value::Bool(t.dst != 0));
                v.set("tz_abbr", Value::Str(t.tz_abbr.clone().unwrap_or_default()));
            }
            ZoneType::None => {}
        }
    }
    if t.have_relative {
        let r = &t.relative;
        let mut rel = Value::assoc();
        rel.set("year", Value::Int(r.y));
        rel.set("month", Value::Int(r.m));
        rel.set("day", Value::Int(r.d));
        rel.set("hour", Value::Int(r.h));
        rel.set("minute", Value::Int(r.i));
        rel.set("second", Value::Int(r.s));
        if r.have_weekday_relative {
            rel.set("weekday", Value::Int(i64::from(r.weekday)));
        }
        if r.have_special_relative && r.special_type == SPECIAL_WEEKDAY {
            rel.set("weekdays", Value::Int(r.special_amount));
        }
        if r.first_last_day_of != 0 {
            let key = if r.first_last_day_of == FIRST_DAY_OF_MONTH {
                "first_day_of_month"
            } else {
                "last_day_of_month"
            };
            rel.set(key, Value::Bool(true));
        }
        v.set("relative", rel);
    }
    v
}

/// `date_parse($datetime)`.
pub fn date_parse(s: &[u8]) -> Value {
    let (t, e) = parse_strtotime(s);
    parsed_time_value(&t, &e)
}

/// `date_parse_from_format($format, $datetime)`.
pub fn date_parse_from_format(format: &[u8], s: &[u8]) -> Value {
    let (t, e) = parse_from_format(format, s);
    parsed_time_value(&t, &e)
}

// ---------------------------------------------------------------------------
// Sun functions
// ---------------------------------------------------------------------------

/// `date_sun_info($ts, $lat, $lon)` (arguments must be finite).
pub fn date_sun_info(ts: i64, latitude: f64, longitude: f64, tz: &Arc<TzInfo>) -> Value {
    let mut t = Time::from_timestamp_in(ts, tz);
    let mut v = Value::assoc();
    let r = rise_set_altitude(&mut t, longitude, latitude, -35.0 / 60.0, true);
    match r.rc {
        -1 => {
            v.set("sunrise", Value::Bool(false));
            v.set("sunset", Value::Bool(false));
        }
        1 => {
            v.set("sunrise", Value::Bool(true));
            v.set("sunset", Value::Bool(true));
        }
        _ => {
            v.set("sunrise", Value::Int(r.ts_rise));
            v.set("sunset", Value::Int(r.ts_set));
        }
    }
    v.set("transit", Value::Int(r.ts_transit));
    for (alt, begin, end) in [
        (-6.0, "civil_twilight_begin", "civil_twilight_end"),
        (-12.0, "nautical_twilight_begin", "nautical_twilight_end"),
        (
            -18.0,
            "astronomical_twilight_begin",
            "astronomical_twilight_end",
        ),
    ] {
        let r = rise_set_altitude(&mut t, longitude, latitude, alt, false);
        match r.rc {
            -1 => {
                v.set(begin, Value::Bool(false));
                v.set(end, Value::Bool(false));
            }
            1 => {
                v.set(begin, Value::Bool(true));
                v.set(end, Value::Bool(true));
            }
            _ => {
                v.set(begin, Value::Int(r.ts_rise));
                v.set(end, Value::Int(r.ts_set));
            }
        }
    }
    v
}

/// `SUNFUNCS_RET_TIMESTAMP`
pub const SUNFUNCS_RET_TIMESTAMP: i64 = 0;
/// `SUNFUNCS_RET_STRING`
pub const SUNFUNCS_RET_STRING: i64 = 1;
/// `SUNFUNCS_RET_DOUBLE`
pub const SUNFUNCS_RET_DOUBLE: i64 = 2;

/// `date_sunrise()` / `date_sunset()` (deprecated in PHP but still present).
///
/// `retformat` must already be validated. `gmt_offset` is in hours; when
/// `None`, PHP uses the default zone's offset at the Unix epoch (sic).
/// Returns `Value::Bool(false)`, `Value::Int`, `Value::Str` or `Value::Float`.
#[allow(clippy::too_many_arguments, clippy::cast_possible_truncation)]
pub fn date_sunrise_sunset(
    ts: i64,
    retformat: i64,
    latitude: f64,
    longitude: f64,
    zenith: f64,
    gmt_offset: Option<f64>,
    tz: &Arc<TzInfo>,
    sunset: bool,
) -> Value {
    let altitude = 90.0 - zenith;
    if !latitude.is_finite() || !longitude.is_finite() {
        return Value::Bool(false);
    }
    #[allow(clippy::cast_precision_loss)]
    let gmt_offset =
        gmt_offset.unwrap_or_else(|| tz.offset_info(0).map_or(0.0, |o| f64::from(o.0)) / 3600.0);
    let mut t = Time::from_timestamp_in(ts, tz);
    let r = rise_set_altitude(&mut t, longitude, latitude, altitude, true);
    if r.rc != 0 {
        return Value::Bool(false);
    }
    if retformat == SUNFUNCS_RET_TIMESTAMP {
        return Value::Int(if sunset { r.ts_set } else { r.ts_rise });
    }
    let mut n = (if sunset { r.h_set } else { r.h_rise }) + gmt_offset;
    if !(0.0..=24.0).contains(&n) {
        n -= (n / 24.0).floor() * 24.0;
    }
    if !(0.0..=24.0).contains(&n) {
        return Value::Bool(false);
    }
    if retformat == SUNFUNCS_RET_STRING {
        let h = n as i32;
        let m = (60.0 * (n - f64::from(h))) as i32;
        Value::Str(format!("{h:02}:{m:02}"))
    } else {
        Value::Float(n)
    }
}

// ---------------------------------------------------------------------------
// Timezones
// ---------------------------------------------------------------------------

/// `timezone_abbreviations_list()`.
pub fn timezone_abbreviations_list() -> Value {
    let mut v = Value::assoc();
    for e in tz::abbreviations_list() {
        let mut el = Value::assoc();
        el.set("dst", Value::Bool(e.dst));
        el.set("offset", Value::Int(e.offset));
        el.set(
            "timezone_id",
            e.tz_id.map_or(Value::Null, |s| Value::Str(s.to_string())),
        );
        if let Value::Array(items) = &mut v {
            if let Some((_, arr)) = items
                .iter_mut()
                .find(|(k, _)| matches!(k, Key::Str(s) if s == e.name))
            {
                arr.push(el);
                continue;
            }
        }
        let mut arr = Value::assoc();
        arr.push(el);
        v.set(e.name, arr);
    }
    v
}

impl TimezoneSpec {
    /// `DateTimeZone::getName()`.
    pub fn name(&self) -> String {
        match self {
            Self::Id(tz) => tz.name.clone(),
            Self::Offset(o) => offset_string(*o),
            Self::Abbr { abbr, .. } => abbr.clone(),
        }
    }

    /// The numeric `timezone_type`.
    pub fn zone_type(&self) -> i64 {
        match self {
            Self::Offset(_) => 1,
            Self::Abbr { .. } => 2,
            Self::Id(_) => 3,
        }
    }

    /// `DateTimeZone::getOffset($datetime)`.
    pub fn offset_at(&self, t: &Time) -> i64 {
        match self {
            Self::Id(tz) => i64::from(tz.time_zone_info(t.sse).offset),
            Self::Offset(o) => *o,
            Self::Abbr {
                utc_offset, dst, ..
            } => utc_offset + i64::from(*dst) * 3600,
        }
    }

    /// `timezone_type` / `timezone` properties (`var_dump`, serialisation).
    pub fn props(&self) -> Value {
        let mut v = Value::assoc();
        v.set("timezone_type", Value::Int(self.zone_type()));
        v.set("timezone", Value::Str(self.name()));
        v
    }

    /// `DateTimeZone::getTransitions()`; `None` (false) for non-ID zones.
    pub fn transitions(&self, begin: i64, end: i64) -> Option<Value> {
        let Self::Id(tz) = self else {
            return None;
        };
        let mut v = Value::assoc();
        for tr in tz.transitions(begin, end) {
            let mut el = Value::assoc();
            el.set("ts", Value::Int(tr.ts));
            el.set("time", Value::Str(tr.time));
            el.set("offset", Value::Int(i64::from(tr.offset)));
            el.set("isdst", Value::Bool(tr.isdst));
            el.set("abbr", Value::Str(tr.abbr));
            v.push(el);
        }
        Some(v)
    }

    /// `DateTimeZone::getLocation()`; `None` (false) for non-ID zones.
    pub fn location(&self) -> Option<Value> {
        let Self::Id(tz) = self else {
            return None;
        };
        let mut v = Value::assoc();
        v.set("country_code", Value::Str(tz.location.country_code.clone()));
        v.set("latitude", Value::Float(tz.location.latitude));
        v.set("longitude", Value::Float(tz.location.longitude));
        v.set("comments", Value::Str(tz.location.comments.clone()));
        Some(v)
    }

    /// Whether two zones compare equal (`DateTimeZone ==`); `None` when PHP
    /// would throw "Cannot compare two different kinds of `DateTimeZone` objects".
    pub fn equals(&self, other: &Self) -> Option<bool> {
        match (self, other) {
            (Self::Offset(a), Self::Offset(b)) => Some(a == b),
            (Self::Abbr { abbr: a, .. }, Self::Abbr { abbr: b, .. }) => Some(a == b),
            (Self::Id(a), Self::Id(b)) => Some(a.name == b.name),
            _ => None,
        }
    }
}

/// The zone of a date object (`DateTime::getTimezone()`); `None` (false)
/// when the time has no zone.
pub fn timezone_of(t: &Time) -> Option<TimezoneSpec> {
    if !t.is_localtime {
        return None;
    }
    match t.zone_type {
        ZoneType::Id => t.tz_info.clone().map(TimezoneSpec::Id),
        ZoneType::Offset => Some(TimezoneSpec::Offset(t.z)),
        ZoneType::Abbr => Some(TimezoneSpec::Abbr {
            abbr: t.tz_abbr.clone().unwrap_or_default(),
            utc_offset: t.z,
            dst: t.dst != 0,
        }),
        ZoneType::None => None,
    }
}

// ---------------------------------------------------------------------------
// DateTime / DateTimeImmutable
// ---------------------------------------------------------------------------

/// `php_date_initialize()`: build the time for `new DateTime($str, $tz)`,
/// `date_create()` or `DateTime::createFromFormat($format, $str, $tz)`.
///
/// `now_sec`/`now_usec` is the current time; `default_tz` the default
/// timezone. Returns the time (or `None` on parse errors) and the parse
/// errors/warnings (for `getLastErrors()` and exception messages such as
/// `Failed to parse time string (%s) at position %d (%c): %s`).
pub fn date_initialize(
    time_str: &[u8],
    format: Option<&[u8]>,
    zone: Option<&TimezoneSpec>,
    default_tz: &Arc<TzInfo>,
    now_sec: i64,
    now_usec: i64,
) -> (Option<Time>, Errors) {
    let (mut t, errors) = if let Some(f) = format {
        parse_from_format(f, time_str)
    } else {
        let s: &[u8] = if time_str.is_empty() {
            b"now"
        } else {
            time_str
        };
        parse_strtotime(s)
    };
    if !errors.errors.is_empty() {
        return (None, errors);
    }

    let mut now = Time::default();
    let mut tzi = None;
    match zone {
        Some(TimezoneSpec::Id(tz)) => {
            now.zone_type = ZoneType::Id;
            now.tz_info = Some(Arc::clone(tz));
            tzi = Some(Arc::clone(tz));
        }
        Some(TimezoneSpec::Offset(o)) => {
            now.zone_type = ZoneType::Offset;
            now.z = *o;
        }
        Some(TimezoneSpec::Abbr {
            abbr,
            utc_offset,
            dst,
        }) => {
            now.zone_type = ZoneType::Abbr;
            now.z = *utc_offset;
            now.dst = i64::from(*dst);
            now.tz_abbr = Some(abbr.clone());
        }
        None => {
            let tz = t.tz_info.clone().unwrap_or_else(|| Arc::clone(default_tz));
            now.zone_type = ZoneType::Id;
            now.tz_info = Some(Arc::clone(&tz));
            tzi = Some(tz);
        }
    }
    now.unixtime2local(now_sec);
    now.us = now_usec;

    if format.is_none() && (time_str.is_empty() || time_str == b"now") {
        return (Some(now), errors);
    }

    t.fill_holes(&now, format.is_some());
    t.update_ts(tzi.as_ref());
    t.update_from_sse();
    t.have_relative = false;
    (Some(t), errors)
}

/// `DateTime::createFromTimestamp(int)` / `php_date_initialize_from_ts_long`.
pub fn from_timestamp(sec: i64, usec: i64) -> Time {
    let mut t = Time {
        zone_type: ZoneType::Offset,
        ..Time::default()
    };
    t.unixtime2gmt(sec);
    t.update_ts(None);
    t.us = usec;
    t
}

/// `DateTime::createFromTimestamp(float)`; `Err` carries PHP's
/// `DateRangeError` message for argument #1.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub fn from_timestamp_float(ts: f64) -> Result<Time, String> {
    let err = || {
        format!(
            "must be a finite number between {} and {}.999999, {} given",
            i64::MIN,
            i64::MAX,
            php_g(ts)
        )
    };
    let sec_dval = ts.trunc();
    if sec_dval.is_nan()
        || !(-9.223_372_036_854_776e18..9.223_372_036_854_776e18).contains(&sec_dval)
    {
        return Err(err());
    }
    let mut sec = sec_dval as i64;
    let mut usec = ((ts % 1.0) * 1_000_000.0).round() as i64;
    if usec.abs() == 1_000_000 {
        sec += if usec > 0 { 1 } else { -1 };
        usec = 0;
    }
    if usec < 0 {
        if sec == i64::MIN {
            return Err(err());
        }
        sec -= 1;
        usec += 1_000_000;
    }
    Ok(from_timestamp(sec, usec))
}

/// C's `%g` formatting (6 significant digits), used in a few messages.
#[allow(clippy::cast_possible_truncation)]
pub fn php_g(v: f64) -> String {
    if v.is_nan() {
        return "nan".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if v == 0.0 {
        return if v.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    let exp = v.abs().log10().floor() as i32;
    let s = format!("{v:.5e}");
    let (mantissa, e) = s.split_once('e').unwrap_or((&s, "0"));
    let e: i32 = e.parse().unwrap_or(exp);
    if (-4..6).contains(&e) {
        let decimals = usize::try_from(5 - e).unwrap_or(0);
        let f = format!("{v:.decimals$}");
        if f.contains('.') {
            f.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            f
        }
    } else {
        let m = if mantissa.contains('.') {
            mantissa.trim_end_matches('0').trim_end_matches('.')
        } else {
            mantissa
        };
        format!("{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs())
    }
}

/// Result of [`modify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModifyError {
    /// Parse failed; contains the errors. PHP reports
    /// `Failed to parse time string (%s) at position %d (%c): %s`.
    Parse(Errors),
}

/// `DateTime::modify($modifier)` (`php_date_modify`). On success returns
/// the parse warnings (for `getLastErrors()`).
pub fn modify(t: &mut Time, modifier: &[u8]) -> Result<Errors, Errors> {
    let (tmp, errors) = parse_strtotime(modifier);
    if !errors.errors.is_empty() {
        return Err(errors);
    }
    t.relative = tmp.relative.clone();
    t.have_relative = tmp.have_relative;
    t.sse_uptodate = false;
    if tmp.y != UNSET {
        t.y = tmp.y;
    }
    if tmp.m != UNSET {
        t.m = tmp.m;
    }
    if tmp.d != UNSET {
        t.d = tmp.d;
    }
    if tmp.h != UNSET {
        t.h = tmp.h;
        if tmp.i == UNSET {
            t.i = 0;
            t.s = 0;
        } else {
            t.i = tmp.i;
            t.s = if tmp.s == UNSET { 0 } else { tmp.s };
        }
    }
    if tmp.us != UNSET {
        t.us = tmp.us;
    }
    if tmp.y == 1970
        && tmp.m == 1
        && tmp.d == 1
        && tmp.h == 0
        && tmp.i == 0
        && tmp.s == 0
        && tmp.us == 0
        && tmp.have_zone != 0
        && tmp.zone_type == ZoneType::Offset
        && tmp.z == 0
        && tmp.dst == 0
    {
        t.set_timezone_from_offset(0);
    }
    t.update_ts(None);
    t.update_from_sse();
    t.have_relative = false;
    t.relative = RelTime::default();
    Ok(errors)
}

/// How an interval is applied (`civil_or_wall`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntervalKind {
    /// `PHP_DATE_CIVIL`: from `diff()` / `createFromDateString()`.
    Civil,
    /// `PHP_DATE_WALL`: from `new DateInterval($iso)`.
    Wall,
}

/// A `DateInterval`'s state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interval {
    /// The relative time.
    pub rel: RelTime,
    /// Civil or wall-clock semantics for add/sub.
    pub kind: IntervalKind,
    /// Created by `createFromDateString()`.
    pub from_string: bool,
    /// The original string for `from_string` intervals.
    pub date_string: Option<Vec<u8>>,
}

impl Interval {
    /// The properties PHP exposes (`var_dump`, `(array)`, serialisation).
    #[allow(clippy::cast_precision_loss)]
    pub fn props(&self) -> Value {
        let mut v = Value::assoc();
        if self.from_string {
            v.set("from_string", Value::Bool(true));
            v.set(
                "date_string",
                Value::Str(
                    String::from_utf8_lossy(self.date_string.as_deref().unwrap_or(b""))
                        .into_owned(),
                ),
            );
            return v;
        }
        let r = &self.rel;
        v.set("y", Value::Int(r.y));
        v.set("m", Value::Int(r.m));
        v.set("d", Value::Int(r.d));
        v.set("h", Value::Int(r.h));
        v.set("i", Value::Int(r.i));
        v.set("s", Value::Int(r.s));
        v.set("f", Value::Float(r.us as f64 / 1_000_000.0));
        v.set("invert", Value::Int(i64::from(r.invert)));
        v.set("days", Value::long_or_false(r.days));
        v.set("from_string", Value::Bool(false));
        v
    }

    /// `DateInterval::format()`.
    pub fn format(&self, format: &[u8]) -> Vec<u8> {
        crate::format::format_interval(format, &self.rel)
    }
}

/// `new DateInterval($duration)`. On failure returns the
/// `DateMalformedIntervalStringException` message.
pub fn interval_from_iso(spec: &[u8]) -> Result<Interval, Vec<u8>> {
    let r = parse_iso_interval(spec);
    let msg = |prefix: &str| {
        let mut m = prefix.as_bytes().to_vec();
        m.extend_from_slice(spec);
        m.push(b')');
        m
    };
    if !r.errors.errors.is_empty() {
        return Err(msg("Unknown or bad format ("));
    }
    let rel = if let Some(p) = r.period {
        p
    } else if let (Some(mut b), Some(mut e)) = (r.begin, r.end) {
        b.update_ts(None);
        e.update_ts(None);
        interval::diff(&b, &e)
    } else {
        return Err(msg("Failed to parse interval ("));
    };
    Ok(Interval {
        rel,
        kind: IntervalKind::Wall,
        from_string: false,
        date_string: None,
    })
}

/// Why `DateInterval::createFromDateString()` failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DateStringIntervalError {
    /// `Unknown or bad format (%s) at position %d (%c): %s`
    BadFormat(Vec<u8>),
    /// `String '%s' contains non-relative elements`
    NonRelative(Vec<u8>),
}

/// `DateInterval::createFromDateString($datetime)`.
pub fn interval_from_date_string(s: &[u8]) -> Result<Interval, DateStringIntervalError> {
    let (t, e) = parse_strtotime(s);
    if let Some(desc) = e.first_error_description() {
        let mut m = b"Unknown or bad format (".to_vec();
        m.extend_from_slice(s);
        m.extend_from_slice(b") ");
        m.extend_from_slice(&desc);
        return Err(DateStringIntervalError::BadFormat(m));
    }
    if t.have_date != 0 || t.have_time != 0 || t.have_zone != 0 {
        let mut m = b"String '".to_vec();
        m.extend_from_slice(s);
        m.extend_from_slice(b"' contains non-relative elements");
        return Err(DateStringIntervalError::NonRelative(m));
    }
    Ok(Interval {
        rel: t.relative,
        kind: IntervalKind::Civil,
        from_string: true,
        date_string: Some(s.to_vec()),
    })
}

/// `DateTime::diff($other, $absolute)`.
pub fn diff(a: &Time, b: &Time, absolute: bool) -> Interval {
    let mut rel = interval::diff(a, b);
    if absolute {
        rel.invert = false;
    }
    Interval {
        rel,
        kind: IntervalKind::Civil,
        from_string: false,
        date_string: None,
    }
}

/// `DateTime::add($interval)`.
pub fn add(t: &Time, iv: &Interval) -> Time {
    match iv.kind {
        IntervalKind::Wall => interval::add_wall(t, &iv.rel),
        IntervalKind::Civil => interval::add(t, &iv.rel),
    }
}

/// The message for subtracting special relative intervals.
pub const SUB_SPECIAL_ERROR: &str =
    "Only non-special relative time specifications are supported for subtraction";

/// `DateTime::sub($interval)`; `Err` carries [`SUB_SPECIAL_ERROR`].
pub fn sub(t: &Time, iv: &Interval) -> Result<Time, &'static str> {
    if iv.rel.have_weekday_relative || iv.rel.have_special_relative {
        return Err(SUB_SPECIAL_ERROR);
    }
    Ok(match iv.kind {
        IntervalKind::Wall => interval::sub_wall(t, &iv.rel),
        IntervalKind::Civil => interval::sub(t, &iv.rel),
    })
}

/// `DateTime::setTimezone($tz)`.
pub fn set_timezone(t: &mut Time, zone: &TimezoneSpec) {
    match zone {
        TimezoneSpec::Offset(o) => t.set_timezone_from_offset(*o),
        TimezoneSpec::Abbr {
            abbr,
            utc_offset,
            dst,
        } => t.set_timezone_from_abbr(abbr, *utc_offset, i64::from(*dst)),
        TimezoneSpec::Id(tz) => t.set_timezone(tz),
    }
    let sse = t.sse;
    t.unixtime2local(sse);
}

/// `DateTime::getOffset()`.
pub fn offset(t: &Time) -> i64 {
    if !t.is_localtime {
        return 0;
    }
    match t.zone_type {
        ZoneType::Id => t
            .tz_info
            .as_ref()
            .map_or(0, |tz| i64::from(tz.time_zone_info(t.sse).offset)),
        ZoneType::Offset => t.z,
        ZoneType::Abbr => t.z + 3600 * t.dst,
        ZoneType::None => 0,
    }
}

/// `DateTime::setTime($h, $i, $s, $us)`.
pub fn set_time(t: &mut Time, h: i64, i: i64, s: i64, us: i64) {
    t.h = h;
    t.i = i;
    t.s = s;
    t.us = us;
    t.update_ts(None);
    t.update_from_sse();
}

/// `DateTime::setDate($y, $m, $d)`.
pub fn set_date(t: &mut Time, y: i64, m: i64, d: i64) {
    t.y = y;
    t.m = m;
    t.d = d;
    t.update_ts(None);
}

/// `DateTime::setISODate($y, $w, $d)`.
pub fn set_isodate(t: &mut Time, y: i64, w: i64, d: i64) {
    t.y = y;
    t.m = 1;
    t.d = 1;
    t.relative = RelTime {
        d: daynr_from_weeknr(y, w, d),
        ..RelTime::default()
    };
    t.have_relative = true;
    t.update_ts(None);
}

/// `DateTime::setTimestamp($ts)`.
pub fn set_timestamp(t: &mut Time, ts: i64) {
    t.unixtime2local(ts);
    t.update_ts(None);
    t.us = 0;
}

/// The message for an out-of-range `setMicrosecond()` argument (#1).
pub fn microsecond_range_error(us: i64) -> Option<String> {
    if (0..=999_999).contains(&us) {
        None
    } else {
        Some(format!("must be between 0 and 999999, {us} given"))
    }
}

/// `DateTime::getTimestamp()`.
pub fn timestamp(t: &mut Time) -> i64 {
    if !t.sse_uptodate {
        t.update_ts(None);
    }
    t.sse
}

/// `DateTime::format($format)`.
pub fn format(t: &Time, f: &[u8]) -> Vec<u8> {
    format_date(f, t, t.is_localtime)
}

/// The `date`, `timezone_type`, `timezone` properties of a date object.
pub fn date_props(t: &Time) -> Value {
    let mut v = Value::assoc();
    v.set(
        "date",
        Value::Str(String::from_utf8_lossy(&format_date(b"x-m-d H:i:s.u", t, true)).into_owned()),
    );
    if t.is_localtime {
        v.set("timezone_type", Value::Int(t.zone_type.as_i64()));
        let tz = match t.zone_type {
            ZoneType::Id => t
                .tz_info
                .as_ref()
                .map(|z| z.name.clone())
                .unwrap_or_default(),
            ZoneType::Offset => offset_string(t.z),
            ZoneType::Abbr => t.tz_abbr.clone().unwrap_or_default(),
            ZoneType::None => String::new(),
        };
        v.set("timezone", Value::Str(tz));
    }
    v
}

/// Compare two date objects (`<=>`), by timestamp then microseconds.
pub fn compare(a: &Time, b: &Time) -> std::cmp::Ordering {
    a.compare(b)
}

/// The UTC offset view `date()` uses (abbreviation, offset, DST flag).
pub fn current_offset_info(t: &Time) -> crate::tz::TimeOffset {
    time_offset(t)
}

// ---------------------------------------------------------------------------
// DatePeriod
// ---------------------------------------------------------------------------

/// `DatePeriod::EXCLUDE_START_DATE`
pub const EXCLUDE_START_DATE: i64 = 1;
/// `DatePeriod::INCLUDE_END_DATE`
pub const INCLUDE_END_DATE: i64 = 2;

/// A `DatePeriod`'s state.
#[derive(Debug, Clone, PartialEq)]
pub struct DatePeriod {
    /// Start date.
    pub start: Time,
    /// Current iteration position (`None` before iteration).
    pub current: Option<Time>,
    /// End date (exclusive unless `include_end_date`).
    pub end: Option<Time>,
    /// The step.
    pub interval: RelTime,
    /// Internal recurrence count (includes start/end inclusion).
    pub recurrences: i64,
    /// Whether the start date is produced.
    pub include_start_date: bool,
    /// Whether the end date is produced.
    pub include_end_date: bool,
}

const MAX_RECURRENCES: i64 = (i32::MAX as i64) - 8;

impl DatePeriod {
    /// `new DatePeriod($start, $interval, $end_or_recurrences, $options)`.
    /// Pass `end` or `recurrences` (0 when an end date is given).
    /// `Err` carries the message without the `DatePeriod::__construct(): ` prefix.
    pub fn new(
        start: &Time,
        interval: &RelTime,
        end: Option<&Time>,
        recurrences: i64,
        options: i64,
    ) -> Result<Self, String> {
        let mut p = Self {
            start: start.clone(),
            current: None,
            end: end.cloned(),
            interval: interval.clone(),
            recurrences: 0,
            include_start_date: false,
            include_end_date: false,
        };
        p.finish(options, recurrences)?;
        Ok(p)
    }

    fn finish(&mut self, options: i64, recurrences: i64) -> Result<(), String> {
        if self.end.is_none() && !(1..=MAX_RECURRENCES).contains(&recurrences) {
            return Err(format!(
                "Recurrence count must be greater or equal to 1 and lower than {}",
                MAX_RECURRENCES + 1
            ));
        }
        self.include_start_date = options & EXCLUDE_START_DATE == 0;
        self.include_end_date = options & INCLUDE_END_DATE != 0;
        let recurrences =
            recurrences + i64::from(self.include_start_date) + i64::from(self.include_end_date);
        if recurrences > MAX_RECURRENCES {
            return Err(format!(
                "Recurrence count must be greater or equal to 1 and lower than {} (including options)",
                MAX_RECURRENCES + 1
            ));
        }
        self.recurrences = recurrences;
        Ok(())
    }

    /// `DatePeriod::createFromISO8601String($spec, $options)` (and the
    /// deprecated string constructor). `Err` carries the message without
    /// the function-name prefix where PHP adds one.
    pub fn from_iso(isostr: &[u8], options: i64) -> Result<Self, String> {
        let r = parse_iso_interval(isostr);
        let s = String::from_utf8_lossy(isostr);
        if !r.errors.errors.is_empty() {
            return Err(format!("Unknown or bad format ({s})"));
        }
        let recurrences = r.recurrences.unwrap_or(0);
        let Some(mut start) = r.begin else {
            return Err(format!(
                "ISO interval must contain a start date, \"{s}\" given"
            ));
        };
        let Some(interval) = r.period else {
            return Err(format!(
                "ISO interval must contain an interval, \"{s}\" given"
            ));
        };
        if r.end.is_none() && recurrences == 0 {
            return Err(format!(
                "ISO interval must contain an end date or a recurrence count, \"{s}\" given"
            ));
        }
        start.update_ts(None);
        let end = r.end.map(|mut e| {
            e.update_ts(None);
            e
        });
        let mut p = Self {
            start,
            current: None,
            end,
            interval,
            recurrences: 0,
            include_start_date: false,
            include_end_date: false,
        };
        p.finish(options, recurrences)?;
        Ok(p)
    }

    /// `DatePeriod::getRecurrences()`; `None` means `null`.
    pub fn get_recurrences(&self) -> Option<i64> {
        let r = self.recurrences
            - i64::from(self.include_start_date)
            - i64::from(self.include_end_date);
        if r == 0 {
            None
        } else {
            Some(r)
        }
    }

    fn advance(t: &mut Time, interval: &RelTime) {
        t.have_relative = true;
        t.relative = interval.clone();
        t.sse_uptodate = false;
        t.update_ts(None);
        t.update_from_sse();
    }

    /// Iterator `rewind()`: resets `current`.
    pub fn rewind(&mut self) {
        let mut cur = self.start.clone();
        if !self.include_start_date {
            Self::advance(&mut cur, &self.interval);
        }
        self.current = Some(cur);
    }

    /// Iterator `valid()` for iteration index `index`.
    pub fn valid(&self, index: i64) -> bool {
        let Some(cur) = &self.current else {
            return false;
        };
        if let Some(end) = &self.end {
            if cur.sse == end.sse {
                return if self.include_end_date {
                    cur.us <= end.us
                } else {
                    cur.us < end.us
                };
            }
            cur.sse < end.sse
        } else {
            index < self.recurrences
        }
    }

    /// Iterator `next()`.
    pub fn move_forward(&mut self) {
        if let Some(cur) = self.current.as_mut() {
            Self::advance(cur, &self.interval);
        }
    }

    /// All dates the period produces (rewinding first). `limit` guards
    /// against unbounded periods.
    pub fn dates(&mut self, limit: usize) -> Vec<Time> {
        let mut out = Vec::new();
        self.rewind();
        let mut i = 0;
        while self.valid(i) && out.len() < limit {
            if let Some(c) = &self.current {
                out.push(c.clone());
            }
            self.move_forward();
            i += 1;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc() -> Arc<TzInfo> {
        tz::get("UTC").unwrap()
    }

    #[test]
    fn strtotime_basics() {
        let tz = utc();
        let now = 1_136_073_600; // 2006-01-01 00:00:00 UTC (a Sunday)
        assert_eq!(strtotime(b"now", now, &tz), Some(now));
        assert_eq!(strtotime(b"tomorrow", now, &tz), Some(now + 86400));
        assert_eq!(strtotime(b"+1 week", now, &tz), Some(now + 7 * 86400));
        assert_eq!(strtotime(b"next monday", now, &tz), Some(now + 86400));
        assert_eq!(
            strtotime(b"2005-07-14 22:30:41", now, &tz),
            Some(1_121_380_241)
        );
        assert_eq!(strtotime(b"garbage", now, &tz), None);
        assert_eq!(strtotime(b"", now, &tz), None);
        let ams = tz::get("Europe/Amsterdam").unwrap();
        assert_eq!(
            strtotime(b"2005-07-14 22:30:41", now, &ams),
            Some(1_121_373_041)
        );
    }

    #[test]
    fn mktime_basics() {
        let tz = utc();
        assert_eq!(
            mktime(
                0,
                Some(0),
                Some(0),
                Some(1),
                Some(1),
                Some(2000),
                0,
                Some(&tz)
            ),
            946_684_800
        );
        assert_eq!(
            mktime(0, Some(0), Some(0), Some(1), Some(1), Some(70), 0, None),
            0
        );
        assert_eq!(
            mktime(0, Some(0), Some(0), Some(13), Some(1), Some(1999), 0, None),
            946_684_800
        );
    }

    #[test]
    fn idate_values() {
        let tz = utc();
        assert_eq!(idate(b"Y", 0, &tz), Ok(1970));
        assert_eq!(idate(b"YY", 0, &tz), Err(IdateError::FormatNotOneChar));
        assert_eq!(idate(b"x", 0, &tz), Err(IdateError::UnrecognizedToken));
    }

    #[test]
    fn date_parse_shape() {
        let v = date_parse(b"2006-12-12 10:00:00.5");
        assert_eq!(v.get("year"), Some(&Value::Int(2006)));
        assert_eq!(v.get("fraction"), Some(&Value::Float(0.5)));
        assert_eq!(v.get("is_localtime"), Some(&Value::Bool(false)));
        let v = date_parse(b"next monday");
        assert_eq!(v.get("year"), Some(&Value::Bool(false)));
        assert!(v.get("relative").is_some());
    }

    #[test]
    fn date_objects() {
        let tz = utc();
        let (t, e) = date_initialize(b"2010-01-31", None, None, &tz, 0, 0);
        assert!(e.is_empty());
        let mut t = t.unwrap();
        assert_eq!(format(&t, b"Y-m-d H:i:s"), b"2010-01-31 00:00:00");
        modify(&mut t, b"+1 month").unwrap();
        assert_eq!(format(&t, b"Y-m-d"), b"2010-03-03");
        let iv = interval_from_iso(b"P1D").unwrap();
        let t2 = add(&t, &iv);
        assert_eq!(format(&t2, b"Y-m-d"), b"2010-03-04");
        let d = diff(&t, &t2, false);
        assert_eq!(d.format(b"%R%a"), b"+1");
        assert!(interval_from_iso(b"XYZ").is_err());
        let (t, e) = date_initialize(b"foo", None, None, &tz, 0, 0);
        assert!(t.is_none());
        assert_eq!(
            e.first_error_description().unwrap(),
            b"at position 0 (f): The timezone could not be found in the database".to_vec()
        );
    }

    #[test]
    fn period_iteration() {
        let tz = utc();
        let (start, _) = date_initialize(b"2012-07-01", None, None, &tz, 0, 0);
        let iv = interval_from_iso(b"P7D").unwrap();
        let mut p = DatePeriod::new(&start.unwrap(), &iv.rel, None, 4, 0).unwrap();
        let dates: Vec<Vec<u8>> = p.dates(100).iter().map(|t| format(t, b"Y-m-d")).collect();
        assert_eq!(dates.len(), 5);
        assert_eq!(dates[4], b"2012-07-29");
        let mut p =
            DatePeriod::from_iso(b"R4/2012-07-01T00:00:00Z/P7D", EXCLUDE_START_DATE).unwrap();
        assert_eq!(p.dates(100).len(), 4);
        assert_eq!(p.get_recurrences(), Some(4));
    }

    #[test]
    fn g_format() {
        assert_eq!(php_g(1.5), "1.5");
        assert_eq!(php_g(1e20), "1e+20");
        assert_eq!(php_g(f64::INFINITY), "inf");
        assert_eq!(php_g(123_456_789.0), "1.23457e+08");
    }
}

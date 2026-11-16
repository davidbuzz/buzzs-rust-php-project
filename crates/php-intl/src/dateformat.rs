// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `IntlDateFormatter` / `datefmt_*` (php-src `ext/intl/dateformat`).

use crate::calendar::{IntlCalendar, PhpDateTime};
use crate::common;
use crate::error::{Error, IntlError, Result, StatusExt, UErrorCode};
use crate::sys::ucal::{caltype, Calendar};
use crate::sys::udat::DateFormat;
use crate::timezone::{self, TimeZone, TimeZoneArg};
use crate::ustr;

/// `IntlDateFormatter` style constants (`UDateFormatStyle`).
pub mod style {
    /// `FULL`
    pub const FULL: i64 = 0;
    /// `LONG`
    pub const LONG: i64 = 1;
    /// `MEDIUM`
    pub const MEDIUM: i64 = 2;
    /// `SHORT`
    pub const SHORT: i64 = 3;
    /// `NONE`
    pub const NONE: i64 = -1;
    /// `RELATIVE_FULL`
    pub const RELATIVE_FULL: i64 = 128;
    /// `RELATIVE_LONG`
    pub const RELATIVE_LONG: i64 = 129;
    /// `RELATIVE_MEDIUM`
    pub const RELATIVE_MEDIUM: i64 = 130;
    /// `RELATIVE_SHORT`
    pub const RELATIVE_SHORT: i64 = 131;
    /// `PATTERN`
    pub const PATTERN: i64 = -2;
    /// `UDAT_RELATIVE` flag.
    pub const RELATIVE: i64 = 128;
}

/// `IntlDateFormatter::GREGORIAN` etc.
pub mod caltypes {
    /// `TRADITIONAL`
    pub const TRADITIONAL: i64 = 0;
    /// `GREGORIAN`
    pub const GREGORIAN: i64 = 1;
    /// `PROLEPTIC_GREGORIAN`
    pub const PROLEPTIC_GREGORIAN: i64 = -16;
}

fn style_ok(s: i64) -> bool {
    matches!(s, -2..=3 | 128..=131)
}

/// The `$calendar` argument (`IntlCalendar|int|null`).
#[derive(Debug, Clone, Copy)]
pub enum CalendarArg<'a> {
    /// `null` — a Gregorian calendar.
    Null,
    /// A calendar type constant.
    Type(i64),
    /// An `IntlCalendar` object (copied).
    Object(&'a IntlCalendar),
}

/// One `localtime()`-style array element as the VM found it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TmElem {
    /// Key absent (counts as 0).
    #[default]
    Missing,
    /// An `int`.
    Int(i64),
    /// Any other type.
    NonInt,
}

/// The `tm_*` keys `format()` reads from an array argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TmArray {
    /// `tm_year` (years since 1900)
    pub year: TmElem,
    /// `tm_mon`
    pub mon: TmElem,
    /// `tm_hour`
    pub hour: TmElem,
    /// `tm_min`
    pub min: TmElem,
    /// `tm_sec`
    pub sec: TmElem,
    /// `tm_mday`
    pub mday: TmElem,
    /// Whether the array was empty (`format()` then returns `false`
    /// without setting an error).
    pub empty: bool,
}

/// The `$datetime` argument of `format()`.
#[derive(Debug, Clone, Copy)]
pub enum DateArg<'a> {
    /// An `int` timestamp (seconds).
    Int(i64),
    /// A `float` timestamp (seconds).
    Float(f64),
    /// A string (must be numeric).
    Str(&'a [u8]),
    /// A `DateTimeInterface`.
    DateTime(&'a PhpDateTime),
    /// An `IntlCalendar`.
    Calendar(&'a IntlCalendar),
    /// An array of `localtime()` fields.
    Array(&'a TmArray),
    /// Any other object.
    OtherObject,
    /// Any other type (`null`, `bool`, …).
    Other,
}

/// `intl_zval_to_millis()`.
pub fn zval_to_millis(arg: DateArg<'_>) -> Result<f64> {
    #[allow(clippy::cast_precision_loss)]
    match arg {
        DateArg::Int(i) => Ok(1000.0 * i as f64),
        DateArg::Float(f) => Ok(1000.0 * f),
        DateArg::Str(s) => match common::numeric_string(s) {
            Some(common::Numeric::Long(l)) => Ok(1000.0 * l as f64),
            Some(common::Numeric::Double(d)) => Ok(d * 1000.0),
            None => Err(Error::Intl(IntlError::from_bytes(
                UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                [
                    &b"string '"[..],
                    s,
                    b"' is not numeric, which would be required for it to be a valid date",
                ]
                .concat(),
            ))),
        },
        DateArg::DateTime(dt) => Ok(dt.millis()),
        DateArg::Calendar(c) => c.get_time().map_err(|e| {
            Error::intl(
                e.intl_code(),
                "call to internal Calendar::getTime() has failed",
            )
        }),
        DateArg::OtherObject => Err(Error::intl(
            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
            "invalid object type for date/time (only IntlCalendar and DateTimeInterface permitted)",
        )),
        DateArg::Array(_) | DateArg::Other => Err(Error::intl(
            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
            "invalid PHP type for date",
        )),
    }
}

/// The result of `parse()`: an `int` or, out of range, a `float`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Timestamp {
    /// `int`
    Int(i64),
    /// `float`
    Float(f64),
}

/// Converts parsed milliseconds to PHP's return value.
#[allow(clippy::cast_precision_loss)]
fn millis_to_timestamp(ms: f64) -> Timestamp {
    let r = ms / 1000.0;
    if r > i64::MAX as f64 || r < i64::MIN as f64 {
        Timestamp::Float(if r < 0.0 { r.ceil() } else { r.floor() })
    } else {
        Timestamp::Int(crate::numfmt::c_double_to_i64(r))
    }
}

/// A PHP `IntlDateFormatter` object.
#[derive(Debug)]
pub struct IntlDateFormatter {
    fmt: DateFormat,
    date_type: i64,
    time_type: i64,
    /// Calendar type constant, or -1 when an `IntlCalendar` was given.
    calendar: i64,
    requested_locale: String,
}

/// What `parse()`-style methods return: the value (or error) and the new
/// offset to assign to the by-reference argument (`None` = leave it).
pub type ParseOutcome<T> = (Result<T>, Option<i64>);

impl IntlDateFormatter {
    /// `new IntlDateFormatter(...)` / `datefmt_create()`.  `tz` is `None`
    /// when no time zone argument (or `null`) was given; `php_default_tz`
    /// is `date_default_timezone_get()`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        locale: Option<&str>,
        date_type: i64,
        time_type: i64,
        tz: Option<TimeZoneArg<'_>>,
        calendar: CalendarArg<'_>,
        pattern: Option<&[u8]>,
        php_default_tz: &str,
    ) -> Result<Self> {
        if !style_ok(date_type) {
            return Err(Error::global(
                UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                "invalid date format style",
            ));
        }
        if !style_ok(time_type) {
            return Err(Error::global(
                UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                "invalid time format style",
            ));
        }
        if date_type == style::PATTERN && time_type != style::PATTERN {
            return Err(Error::global(
                UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                "datefmt_create: time format must be IntlDateFormatter::PATTERN if date format is IntlDateFormatter::PATTERN",
            ));
        }
        let given = locale.unwrap_or("");
        common::check_locale_len(given)
            .map_err(|e| crate::timezone::rescope(e, crate::Scope::Global))?;
        let locale_str = if given.is_empty() {
            common::default_locale()
        } else {
            given.to_owned()
        };
        let canonical = common::canonicalize_locale_string(&locale_str);
        let final_locale = canonical.clone().unwrap_or_else(|| locale_str.clone());
        let bogus = crate::sys::loc::name(&final_locale).is_err();
        if bogus
            || (given.len() == 1 && given != "C")
            || (given.len() > 1 && crate::sys::loc::iso3_language(&final_locale).is_empty())
        {
            return Err(Error::arg_value(1, format!("\"{locale_str}\" is invalid")));
        }

        let (mut cal, cal_type, owned) = process_calendar_arg(calendar, &final_locale)?;
        if tz.is_some() || owned {
            let arg = tz.unwrap_or(TimeZoneArg::Str(php_default_tz.as_bytes()));
            let zone = timezone::process_timezone_argument(arg)?;
            cal.set_time_zone(zone.id_u16())
                .or_intl("Error setting time zone")?;
        }

        let upattern = match pattern {
            Some(p) if !p.is_empty() => Some(
                ustr::to_u16(p)
                    .map_err(|c| Error::global(c, "error converting pattern to UTF-16"))?,
            ),
            _ => None,
        };
        #[allow(clippy::cast_possible_truncation)]
        let mut fmt = DateFormat::open(
            time_type as i32,
            date_type as i32,
            Some(&final_locale),
            None,
            upattern.as_deref(),
        )
        .map_err(|c| Error::global(c, "date formatter creation failed"))?;
        if let Some(p) = &upattern {
            fmt.apply_pattern(true, p);
        }
        fmt.set_calendar(&cal);
        Ok(Self {
            fmt,
            date_type,
            time_type,
            calendar: cal_type,
            requested_locale: canonical.unwrap_or(locale_str),
        })
    }

    /// Clone (`clone $fmt`).
    pub fn try_clone(&self) -> Result<Self> {
        let fmt = self
            .fmt
            .try_clone()
            .map_err(|_| Error::Thrown("Failed to clone IntlDateFormatter".into()))?;
        Ok(Self {
            fmt,
            date_type: self.date_type,
            time_type: self.time_type,
            calendar: self.calendar,
            requested_locale: self.requested_locale.clone(),
        })
    }

    /// `getDateType()`.
    pub fn date_type(&self) -> i64 {
        self.date_type
    }

    /// `getTimeType()`.
    pub fn time_type(&self) -> i64 {
        self.time_type
    }

    /// `getCalendar()` (`None` → `false`, an `IntlCalendar` was given).
    pub fn calendar_type(&self) -> Option<i64> {
        (self.calendar != -1).then_some(self.calendar)
    }

    /// `getPattern()`.
    pub fn pattern(&self) -> Result<String> {
        let p = self
            .fmt
            .to_pattern(false)
            .or_intl("Error getting formatter pattern")?;
        ustr::from_u16(&p).or_intl("Error converting value to UTF-8")
    }

    /// `setPattern($pattern)`.
    pub fn set_pattern(&mut self, pattern: &[u8]) -> Result<()> {
        let p = ustr::to_u16(pattern).or_intl("Error converting pattern to UTF-16")?;
        self.fmt.apply_pattern(false, &p);
        Ok(())
    }

    /// `getLocale($type)`.
    pub fn locale(&self, ty: i64) -> Result<String> {
        #[allow(clippy::cast_possible_truncation)]
        self.fmt
            .locale_by_type(ty as i32)
            .or_intl("Error getting locale")
    }

    /// `isLenient()`.
    pub fn is_lenient(&self) -> bool {
        self.fmt.is_lenient()
    }

    /// `setLenient($lenient)`.
    pub fn set_lenient(&mut self, v: bool) {
        self.fmt.set_lenient(v);
    }

    /// `getTimeZoneId()`.
    pub fn time_zone_id(&self) -> Result<String> {
        let id = self
            .fmt
            .own_calendar()
            .time_zone_id()
            .or_intl("Could not convert time zone id to UTF-8")?;
        ustr::from_u16(&id).or_intl("Could not convert time zone id to UTF-8")
    }

    /// `getTimeZone()`.
    pub fn time_zone(&self) -> Result<TimeZone> {
        let id = self.fmt.own_calendar().time_zone_id().map_err(|_| {
            Error::intl(
                UErrorCode::MEMORY_ALLOCATION_ERROR,
                "Out of memory when cloning time zone",
            )
        })?;
        Ok(TimeZone::create_u16(&id))
    }

    /// `setTimeZone($tz)`; the VM passes `null` as PHP's default zone name.
    pub fn set_time_zone(&mut self, tz: TimeZoneArg<'_>) -> Result<()> {
        let zone = timezone::process_timezone_argument(tz)?;
        let mut cal = self.fmt.calendar().or_intl("Error setting time zone")?;
        cal.set_time_zone(zone.id_u16())
            .or_intl("Error setting time zone")?;
        self.fmt.set_calendar(&cal);
        Ok(())
    }

    /// `getCalendarObject()`.
    pub fn calendar_object(&self) -> Result<IntlCalendar> {
        let cal = self.fmt.calendar().map_err(|_| {
            Error::intl(
                UErrorCode::MEMORY_ALLOCATION_ERROR,
                "Out of memory when cloning calendar",
            )
        })?;
        Ok(IntlCalendar::from_calendar(cal))
    }

    /// `setCalendar($calendar)`.
    pub fn set_calendar(&mut self, calendar: CalendarArg<'_>) -> Result<()> {
        let (mut cal, cal_type, owned) = process_calendar_arg(calendar, &self.requested_locale)?;
        if owned {
            let zone = self.fmt.own_calendar().time_zone_id().map_err(|_| {
                Error::intl(
                    UErrorCode::MEMORY_ALLOCATION_ERROR,
                    "Out of memory when cloning calendar",
                )
            })?;
            cal.set_time_zone(&zone)
                .or_intl("Out of memory when cloning calendar")?;
        }
        self.fmt.set_calendar(&cal);
        self.calendar = cal_type;
        Ok(())
    }

    /// `format($datetime)`.
    pub fn format(&self, arg: DateArg<'_>) -> Result<String> {
        let ts = match arg {
            DateArg::Array(tm) => {
                if tm.empty {
                    return Err(Error::code(UErrorCode::ZERO));
                }
                self.tm_to_millis(tm)
                    .map_err(|e| Error::intl(e.intl_code(), "date formatting failed"))?
            }
            other => zval_to_millis(other)?,
        };
        let out = self.fmt.format(ts).or_intl("Date formatting failed")?;
        ustr::from_u16(&out).or_intl("Error converting value to UTF-8")
    }

    fn tm_to_millis(&self, tm: &TmArray) -> Result<f64> {
        let get = |e: TmElem, key: &str| -> Result<i32> {
            match e {
                TmElem::Missing => Ok(0),
                TmElem::NonInt => Err(Error::intl(
                    UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                    format!("parameter array contains a non-integer element for key '{key}'"),
                )),
                TmElem::Int(v) => i32::try_from(v).map_err(|_| {
                    Error::intl(
                        UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                        format!("value {v} is out of bounds for a 32-bit integer in key '{key}'"),
                    )
                }),
            }
        };
        let year = get(tm.year, "tm_year")?.wrapping_add(1900);
        let month = get(tm.mon, "tm_mon")?;
        let hour = get(tm.hour, "tm_hour")?;
        let minute = get(tm.min, "tm_min")?;
        let second = get(tm.sec, "tm_sec")?;
        let mday = get(tm.mday, "tm_mday")?;
        let mut cal = self
            .fmt
            .calendar()
            .map_err(|c| Error::intl(c, "error cloning calendar"))?;
        cal.set(1, year);
        cal.set(2, month);
        cal.set(5, mday);
        cal.set(11, hour);
        cal.set(12, minute);
        cal.set(13, second);
        cal.millis().or_intl("date formatting failed")
    }

    fn check_offset(
        offset: Option<i64>,
        len: usize,
        allow_minus_one: bool,
    ) -> std::result::Result<Option<i32>, Option<Error>> {
        let Some(o) = offset else { return Ok(None) };
        let Ok(p) = i32::try_from(o) else {
            return Err(Some(Error::global(
                UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                "String index is out of valid range.",
            )));
        };
        if allow_minus_one && p == -1 {
            return Ok(Some(p));
        }
        if usize::try_from(p).map_or(true, |u| u > len) {
            return Err(None);
        }
        Ok(Some(p))
    }

    fn prepare(text: &[u8], pos: Option<i32>) -> Result<(Vec<u16>, Option<i32>)> {
        let u = ustr::to_u16(text).or_intl("Error converting timezone to UTF-16")?;
        let pos = match pos {
            Some(p) => Some(
                ustr::utf8_offset_to_utf16(text, p)
                    .map_err(|c| Error::intl(c, "Invalid UTF-8 offset"))?,
            ),
            None => None,
        };
        Ok((u, pos))
    }

    /// `parse($string, &$offset)`.  A missing/`null` offset is `None`; the
    /// VM converts other offset values with `zval_try_get_long` (throwing
    /// the `TypeError` itself).
    pub fn parse(&self, text: &[u8], offset: Option<i64>) -> ParseOutcome<Timestamp> {
        let pos = match Self::check_offset(offset, text.len(), false) {
            Ok(p) => p,
            Err(e) => {
                return (
                    Err(e.unwrap_or_else(|| Error::code(UErrorCode::ZERO))),
                    None,
                )
            }
        };
        let (u, mut pos) = match Self::prepare(text, pos) {
            Ok(v) => v,
            Err(e) => return (Err(e), None),
        };
        let r = self.fmt.parse_opt(&u, pos.as_mut());
        let new_off = pos.map(|p| i64::from(ustr::utf16_offset_to_utf8(&u, p)));
        (
            r.map(millis_to_timestamp).or_intl("Date parsing failed"),
            new_off,
        )
    }

    /// `parseToCalendar($string, &$offset)`: parses into the formatter's
    /// own calendar (updating it) and returns its timestamp.
    pub fn parse_to_calendar(
        &mut self,
        text: &[u8],
        offset: Option<i64>,
    ) -> ParseOutcome<Timestamp> {
        let pos = match Self::check_offset(offset, text.len(), true) {
            Ok(p) => p,
            Err(e) => {
                return (
                    Err(e.unwrap_or_else(|| Error::code(UErrorCode::ZERO))),
                    None,
                )
            }
        };
        let (u, mut pos) = match Self::prepare(text, pos) {
            Ok(v) => v,
            Err(e) => return (Err(e), None),
        };
        let r = self.fmt.parse_own_calendar(&u, pos.as_mut());
        let new_off = pos.map(|p| i64::from(ustr::utf16_offset_to_utf8(&u, p)));
        if let Err(c) = r {
            return (Err(Error::intl(c, "Calendar parsing failed")), new_off);
        }
        let ms = self
            .fmt
            .own_calendar()
            .millis()
            .or_intl("Date parsing failed");
        (ms.map(millis_to_timestamp), new_off)
    }

    /// `localtime($string, &$offset)` → `[tm_sec, tm_min, tm_hour, tm_year,
    /// tm_mday, tm_wday, tm_yday, tm_mon, tm_isdst]`.
    pub fn localtime(
        &mut self,
        text: &[u8],
        offset: Option<i64>,
    ) -> ParseOutcome<Vec<(&'static str, i64)>> {
        let pos = match Self::check_offset(offset, text.len(), false) {
            Ok(p) => p,
            Err(e) => {
                return (
                    Err(e.unwrap_or_else(|| Error::code(UErrorCode::ZERO))),
                    None,
                )
            }
        };
        let (u, mut pos) = match Self::prepare(text, pos) {
            Ok(v) => v,
            Err(e) => return (Err(e), None),
        };
        let r = self.fmt.parse_own_calendar(&u, pos.as_mut());
        let new_off = pos.map(|p| i64::from(ustr::utf16_offset_to_utf8(&u, p)));
        if let Err(c) = r {
            return (Err(Error::intl(c, "Date parsing failed")), new_off);
        }
        let cal = self.fmt.own_calendar();
        let fields: [(&'static str, i32); 8] = [
            ("tm_sec", 13),
            ("tm_min", 12),
            ("tm_hour", 11),
            ("tm_year", 1),
            ("tm_mday", 5),
            ("tm_wday", 7),
            ("tm_yday", 6),
            ("tm_mon", 2),
        ];
        let mut out = Vec::with_capacity(9);
        for (k, f) in fields {
            let v =
                match cal.get(f) {
                    Ok(v) => i64::from(v),
                    Err(c) => return (
                        Err(Error::intl(
                            c,
                            "Date parsing - localtime failed : could not get a field from calendar",
                        )),
                        new_off,
                    ),
                };
            let v = match k {
                "tm_year" => v - 1900,
                "tm_wday" => v - 1,
                _ => v,
            };
            out.push((k, v));
        }
        match cal.in_daylight_time() {
            Ok(d) => out.push(("tm_isdst", i64::from(d))),
            Err(c) => {
                return (
                    Err(Error::intl(
                        c,
                        "Date parsing - localtime failed : while checking if currently in DST.",
                    )),
                    new_off,
                )
            }
        }
        (Ok(out), new_off)
    }
}

/// `datefmt_process_calendar_arg()` → `(calendar, type, owned)`.
fn process_calendar_arg(arg: CalendarArg<'_>, locale: &str) -> Result<(Calendar, i64, bool)> {
    let fail = |_| {
        Error::intl(
            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
            "Failure instantiating calendar",
        )
    };
    match arg {
        CalendarArg::Null => {
            let c = Calendar::open(None, Some(locale), caltype::GREGORIAN).map_err(fail)?;
            Ok((c, caltypes::GREGORIAN, true))
        }
        CalendarArg::Type(v) => {
            let c = match v {
                caltypes::TRADITIONAL => Calendar::open(None, Some(locale), caltype::TRADITIONAL).map_err(fail)?,
                caltypes::GREGORIAN => Calendar::open(None, Some(locale), caltype::GREGORIAN).map_err(fail)?,
                caltypes::PROLEPTIC_GREGORIAN => {
                    let mut c = Calendar::open(None, Some(locale), caltype::GREGORIAN).map_err(fail)?;
                    c.set_gregorian_change(f64::NEG_INFINITY).map_err(fail)?;
                    c
                }
                _ => {
                    return Err(Error::intl(
                        UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                        "Invalid value for calendar type; it must be one of IntlDateFormatter::TRADITIONAL (locale's default calendar) or IntlDateFormatter::GREGORIAN or IntlDateFormatter::PROLEPTIC_GREGORIAN. Alternatively, it can be an IntlCalendar object",
                    ))
                }
            };
            Ok((c, v, true))
        }
        CalendarArg::Object(c) => {
            let c = c.raw().try_clone().map_err(fail)?;
            Ok((c, -1, false))
        }
    }
}

/// The `$format` argument of `IntlDateFormatter::formatObject()`.
#[derive(Debug, Clone, Copy)]
pub enum ObjectFormat<'a> {
    /// `null`/absent — medium date and time.
    Default,
    /// A single style for both.
    Style(i64),
    /// `[dateStyle, timeStyle]`; `None` elements are non-integers.  `count`
    /// is the array's element count.
    Styles {
        /// Number of elements in the array.
        count: usize,
        /// First element.
        date: Option<i64>,
        /// Second element.
        time: Option<i64>,
    },
    /// A pattern string.
    Pattern(&'a [u8]),
}

/// The `$datetime` object of `formatObject()`.
#[derive(Debug, Clone, Copy)]
pub enum ObjectArg<'a> {
    /// An `IntlCalendar`.
    Calendar(&'a IntlCalendar),
    /// A `DateTimeInterface`.
    DateTime(&'a PhpDateTime),
    /// Anything else.
    Other,
}

fn valid_object_style(v: i64) -> bool {
    matches!(v, -1..=3 | 128..=131)
}

/// `IntlDateFormatter::formatObject($datetime, $format, $locale)`; all
/// errors are global.
pub fn format_object(
    obj: ObjectArg<'_>,
    format: ObjectFormat<'_>,
    locale: Option<&str>,
) -> Result<String> {
    let ill = |m: &'static str| Error::global(UErrorCode::ILLEGAL_ARGUMENT_ERROR, m);
    let locale = locale.map_or_else(common::default_locale, str::to_owned);
    let (mut date_style, mut time_style) = (style::MEDIUM, style::MEDIUM);
    let mut pattern = None;
    match format {
        ObjectFormat::Default => {}
        ObjectFormat::Styles { count, date, time } => {
            if count != 2 {
                return Err(ill("bad format; if array, it must have two elements"));
            }
            match date {
                Some(d) if valid_object_style(d) => date_style = d,
                _ => {
                    return Err(ill(
                        "bad format; the date format (first element of the array) is not valid",
                    ))
                }
            }
            match time {
                Some(t) if valid_object_style(t) => time_style = t,
                _ => {
                    return Err(ill(
                        "bad format; the time format (second element of the array) is not valid",
                    ))
                }
            }
        }
        ObjectFormat::Style(s) => {
            if !valid_object_style(s) {
                return Err(ill("the date/time format type is invalid"));
            }
            date_style = s;
            time_style = s;
        }
        ObjectFormat::Pattern(p) => {
            if p.is_empty() {
                return Err(ill("the format is empty"));
            }
            pattern = Some(
                String::from_utf8_lossy(p)
                    .encode_utf16()
                    .collect::<Vec<u16>>(),
            );
        }
    }
    if time_style != style::NONE {
        time_style &= !style::RELATIVE;
    }
    let (date, zone, cal) =
        match obj {
            ObjectArg::Calendar(c) => {
                let zone = c
                    .raw()
                    .time_zone_id()
                    .map_err(|e| Error::global(e, "error obtaining instant from IntlCalendar"))?;
                let date = c
                    .raw()
                    .millis()
                    .map_err(|e| Error::global(e, "error obtaining instant from IntlCalendar"))?;
                let cal = c
                    .raw()
                    .try_clone()
                    .map_err(|e| Error::global(e, "error obtaining instant from IntlCalendar"))?;
                (date, zone, cal)
            }
            ObjectArg::DateTime(dt) => {
                let tz = dt
                    .time_zone()
                    .map_err(|e| crate::timezone::rescope(e, crate::Scope::Global))?;
                let cal = Calendar::open(None, Some(&locale), caltype::GREGORIAN)
                    .map_err(|c| Error::global(c, "could not create GregorianCalendar"))?;
                (dt.millis(), tz.id_u16().to_vec(), cal)
            }
            ObjectArg::Other => return Err(Error::global(
                UErrorCode::ZERO,
                "the passed object must be an instance of either IntlCalendar or DateTimeInterface",
            )),
        };
    let mut fmt = if let Some(p) = &pattern {
        DateFormat::open(-2, -2, Some(&locale), None, Some(p))
            .map_err(|c| Error::global(c, "could not create SimpleDateFormat"))?
    } else {
        #[allow(clippy::cast_possible_truncation)]
        DateFormat::open(
            time_style as i32,
            date_style as i32,
            Some(&locale),
            None,
            None,
        )
        .map_err(|c| Error::global(c, "could not create DateFormat"))?
    };
    let mut cal = cal;
    cal.set_time_zone(&zone)
        .map_err(|c| Error::global(c, "could not create DateFormat"))?;
    fmt.set_calendar(&cal);
    let out = fmt
        .format(date)
        .map_err(|c| Error::global(c, "error converting result to UTF-8"))?;
    ustr::from_u16(&out).map_err(|c| Error::global(c, "error converting result to UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(loc: &str, d: i64, t: i64, tz: &str, pattern: Option<&str>) -> IntlDateFormatter {
        IntlDateFormatter::new(
            Some(loc),
            d,
            t,
            Some(TimeZoneArg::Str(tz.as_bytes())),
            CalendarArg::Null,
            pattern.map(str::as_bytes),
            "UTC",
        )
        .unwrap()
    }

    #[test]
    fn basic() {
        let f = fmt("en_US", style::FULL, style::FULL, "America/New_York", None);
        assert_eq!(
            f.format(DateArg::Int(0)).unwrap(),
            "Wednesday, December 31, 1969 at 7:00:00\u{202f}PM Eastern Standard Time"
        );
        let f = fmt(
            "en_US",
            style::PATTERN,
            style::PATTERN,
            "UTC",
            Some("yyyy-MM-dd HH:mm:ss"),
        );
        assert_eq!(
            f.format(DateArg::Str(b"86400")).unwrap(),
            "1970-01-02 00:00:00"
        );
        let (r, off) = f.parse(b"1970-01-02 00:00:00", None);
        assert_eq!(r.unwrap(), Timestamp::Int(86400));
        assert_eq!(off, None);
        assert_eq!(f.time_zone_id().unwrap(), "UTC");
    }

    #[test]
    fn errors() {
        let e = IntlDateFormatter::new(Some("en_US"), 99, 0, None, CalendarArg::Null, None, "UTC")
            .unwrap_err();
        assert_eq!(e.intl_message(), Some("invalid date format style"));
        let e = IntlDateFormatter::new(Some("xx"), 0, 0, None, CalendarArg::Null, None, "UTC")
            .unwrap_err();
        assert_eq!(e, Error::arg_value(1, "\"xx\" is invalid"));
        let e = IntlDateFormatter::new(
            Some("en"),
            0,
            0,
            Some(TimeZoneArg::Str(b"bad")),
            CalendarArg::Null,
            None,
            "UTC",
        )
        .unwrap_err();
        assert_eq!(
            e,
            Error::IntlExceptionInFunction("No such time zone: \"bad\"".into())
        );
    }
}

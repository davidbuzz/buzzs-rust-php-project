//! `IntlTimeZone` / `intltz_*` (php-src `ext/intl/timezone`) plus the
//! time-zone argument coercions shared by `IntlCalendar` and
//! `IntlDateFormatter` (`timezone_process_timezone_argument`,
//! `timezone_convert_datetimezone`, `timezone_convert_to_datetimezone`).
//!
//! ICU's C API has no time-zone object, so a zone is represented by its
//! ICU ID and every query goes through a `UCalendar` set to that zone.

use crate::common;
use crate::error::{Error, Result, Scope, StatusExt, UErrorCode};
use crate::sys::ucal::{self, caltype, Calendar};
use crate::sys::udat::DateFormat;
use crate::sys::ures::{self, Bundle};
use crate::ustr;

/// `UCalendarDateFields` used here.
const ZONE_OFFSET: i32 = 15;
const DST_OFFSET: i32 = 16;
/// `UCAL_TZ_TRANSITION_NEXT` / `_INCLUSIVE`.
const TRANSITION_NEXT: i32 = 0;
const TRANSITION_NEXT_INCLUSIVE: i32 = 1;
/// `UCAL_TZ_LOCAL_FORMER` / `UCAL_TZ_LOCAL_LATTER`.
const LOCAL_FORMER: i32 = 0x04;
const LOCAL_LATTER: i32 = 0x0C;

/// `IntlTimeZone::DISPLAY_*` (`TimeZone::EDisplayType`).
pub mod display {
    /// `DISPLAY_SHORT`
    pub const SHORT: i64 = 1;
    /// `DISPLAY_LONG`
    pub const LONG: i64 = 2;
    /// `DISPLAY_SHORT_GENERIC`
    pub const SHORT_GENERIC: i64 = 3;
    /// `DISPLAY_LONG_GENERIC`
    pub const LONG_GENERIC: i64 = 4;
    /// `DISPLAY_SHORT_GMT`
    pub const SHORT_GMT: i64 = 5;
    /// `DISPLAY_LONG_GMT`
    pub const LONG_GMT: i64 = 6;
    /// `DISPLAY_SHORT_COMMONLY_USED`
    pub const SHORT_COMMONLY_USED: i64 = 7;
    /// `DISPLAY_GENERIC_LOCATION`
    pub const GENERIC_LOCATION: i64 = 8;
}

/// `IntlTimeZone::TYPE_*` (`USystemTimeZoneType`).
pub mod zone_type {
    /// `TYPE_ANY`
    pub const ANY: i64 = 0;
    /// `TYPE_CANONICAL`
    pub const CANONICAL: i64 = 1;
    /// `TYPE_CANONICAL_LOCATION`
    pub const CANONICAL_LOCATION: i64 = 2;
}

/// ICU's unknown zone ID.
pub const UNKNOWN_ZONE_ID: &str = "Etc/Unknown";

/// A PHP `DateTimeZone` (or the zone of a `DateTimeInterface`) as ext/date
/// stores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhpZone {
    /// `TIMELIB_ZONETYPE_ID` — an Olson identifier.
    Id(String),
    /// `TIMELIB_ZONETYPE_OFFSET` — UTC offset in seconds.
    Offset(i64),
    /// `TIMELIB_ZONETYPE_ABBR` — an abbreviation such as `"EST"`.
    Abbr(String),
}

/// What `IntlTimeZone::toDateTimeZone()` (and `IntlCalendar::toDateTime()`)
/// asks the VM to construct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DateTimeZoneSpec {
    /// `new DateTimeZone($id)`; if that throws, the VM throws
    /// `IntlException("DateTimeZone constructor threw exception")`.
    Id(String),
    /// An offset zone of the given number of seconds (built directly,
    /// bypassing the constructor).
    Offset(i64),
}

/// A time-zone argument (`IntlTimeZone|DateTimeZone|string|null`).  A
/// `null` argument is passed by the VM as `Str` with PHP's default time
/// zone name (`date_default_timezone_get()`); objects with `__toString`
/// are passed as `Str` too.
#[derive(Debug, Clone, Copy)]
pub enum TimeZoneArg<'a> {
    /// An `IntlTimeZone` object.
    Intl(&'a TimeZone),
    /// A `DateTimeZone` object.
    DateTimeZone(&'a PhpZone),
    /// A string identifier.
    Str(&'a [u8]),
}

/// A PHP `IntlTimeZone` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeZone {
    id: Vec<u16>,
    /// ICU's `TimeZone::getUnknown()` `SimpleTimeZone` (what invalid IDs
    /// resolve to), as opposed to the `Etc/Unknown` Olson zone.
    unknown: bool,
}

fn zone_calendar(id: &[u16]) -> std::result::Result<Calendar, UErrorCode> {
    Calendar::open(Some(id), Some("en_US"), caltype::GREGORIAN)
}

impl TimeZone {
    /// `TimeZone::createTimeZone(id)`: never fails; unknown IDs yield
    /// `Etc/Unknown`, custom IDs are normalised (`GMT+5` → `GMT+05:00`).
    pub fn create_u16(id: &[u16]) -> Self {
        let unknown = ustr::str_u16(UNKNOWN_ZONE_ID);
        match zone_calendar(id).and_then(|c| c.time_zone_id()) {
            Ok(r) if r == unknown && id != unknown.as_slice() => Self::unknown(),
            Ok(r) => Self {
                id: r,
                unknown: false,
            },
            Err(_) => Self::unknown(),
        }
    }

    /// Wraps an ID ICU has already resolved (e.g. from a calendar).
    pub(crate) fn from_resolved_id(id: Vec<u16>) -> Self {
        Self { id, unknown: false }
    }

    /// `IntlTimeZone::createTimeZone($id)`.
    pub fn create_time_zone(id: &[u8]) -> Result<Self> {
        let u = ustr::to_u16(id)
            .map_err(|c| Error::global(c, "could not convert time zone id to UTF-16"))?;
        Ok(Self::create_u16(&u))
    }

    /// `IntlTimeZone::createDefault()` (ICU's default, i.e. host, zone).
    pub fn create_default() -> Self {
        ucal::default_time_zone().map_or_else(|_| Self::unknown(), |id| Self::create_u16(&id))
    }

    /// `IntlTimeZone::getGMT()`.
    pub fn gmt() -> Self {
        Self {
            id: ustr::str_u16("GMT"),
            unknown: false,
        }
    }

    /// `IntlTimeZone::getUnknown()`.
    pub fn unknown() -> Self {
        Self {
            id: ustr::str_u16(UNKNOWN_ZONE_ID),
            unknown: true,
        }
    }

    /// Whether this is ICU's unknown zone (`TimeZone::getUnknown()`).
    pub fn is_unknown(&self) -> bool {
        self.unknown
    }

    /// The UTF-16 ID.
    pub fn id_u16(&self) -> &[u16] {
        &self.id
    }

    /// `IntlTimeZone::getID()`.
    pub fn id(&self) -> Result<String> {
        ustr::from_u16(&self.id).or_intl("could not convert id to UTF-8")
    }

    /// A Gregorian calendar in this zone (for queries).
    pub(crate) fn calendar(&self) -> std::result::Result<Calendar, UErrorCode> {
        zone_calendar(&self.id)
    }

    /// `IntlTimeZone::getOffset($date, $local, &$raw, &$dst)` → `(raw, dst)`.
    pub fn offset(&self, date: f64, local: bool) -> Result<(i32, i32)> {
        let r = (|| {
            let mut c = self.calendar()?;
            c.set_millis(date)?;
            if local {
                c.offset_from_local(LOCAL_FORMER, LOCAL_LATTER)
            } else {
                Ok((c.get(ZONE_OFFSET)?, c.get(DST_OFFSET)?))
            }
        })();
        r.or_intl("error obtaining offset")
    }

    /// `IntlTimeZone::getRawOffset()` (raw offset now).
    pub fn raw_offset(&self) -> i32 {
        self.offset(ucal::now(), false).map_or(0, |(r, _)| r)
    }

    /// `IntlTimeZone::getDSTSavings()`.
    pub fn dst_savings(&self) -> i32 {
        if self.unknown {
            // SimpleTimeZone's default DST savings.
            return 3_600_000;
        }
        ucal::dst_savings(&self.id).unwrap_or(0)
    }

    /// `IntlTimeZone::useDaylightTime()`: whether DST is observed at any
    /// time during the current year.
    pub fn use_daylight_time(&self) -> bool {
        let Ok(mut c) = self.calendar() else {
            return false;
        };
        let now = ucal::now();
        if c.set_millis(now).is_err() {
            return false;
        }
        // Start and end of the current (UTC) year.
        let Ok(mut g) = Calendar::open(
            Some(&ustr::str_u16("UTC")),
            Some("en_US"),
            caltype::GREGORIAN,
        ) else {
            return false;
        };
        let _ = g.set_millis(now);
        let Ok(year) = g.get(1) else { return false };
        g.clear();
        g.set(1, year);
        let start = g.millis().unwrap_or(now);
        g.set(1, year + 1);
        let limit = g.millis().unwrap_or(now);
        if c.set_millis(start).is_err() {
            return false;
        }
        if c.get(DST_OFFSET).is_ok_and(|d| d != 0) {
            return true;
        }
        let mut t = start;
        for _ in 0..64 {
            let _ = c.set_millis(t);
            match c.transition(TRANSITION_NEXT) {
                Ok(Some(next)) if next < limit => {
                    let _ = c.set_millis(next);
                    if c.get(DST_OFFSET).is_ok_and(|d| d != 0) {
                        return true;
                    }
                    t = next;
                }
                _ => break,
            }
        }
        false
    }

    /// The transition history used to compare rules.
    fn rule_fingerprint(&self) -> Vec<(i64, i32, i32)> {
        let mut out = Vec::new();
        let Ok(mut c) = self.calendar() else {
            return out;
        };
        let mut t = -8.64e15 / 2.0;
        let end = 4_102_444_800_000.0 * 1.5; // ~2100+
        if c.set_millis(t).is_err() {
            return out;
        }
        out.push((
            i64::MIN,
            c.get(ZONE_OFFSET).unwrap_or(0),
            c.get(DST_OFFSET).unwrap_or(0),
        ));
        for _ in 0..2000 {
            let _ = c.set_millis(t);
            let Ok(Some(next)) = c.transition(if out.len() == 1 {
                TRANSITION_NEXT_INCLUSIVE
            } else {
                TRANSITION_NEXT
            }) else {
                break;
            };
            if next > end {
                break;
            }
            let _ = c.set_millis(next);
            #[allow(clippy::cast_possible_truncation)]
            out.push((
                next as i64,
                c.get(ZONE_OFFSET).unwrap_or(0),
                c.get(DST_OFFSET).unwrap_or(0),
            ));
            t = next;
        }
        out
    }

    /// Whether ICU implements this zone as an `OlsonTimeZone` (system
    /// zone) rather than a `SimpleTimeZone` (custom `GMT±hh:mm` IDs and
    /// `Etc/Unknown`).
    fn is_olson(&self) -> bool {
        let custom = self.id.starts_with(&ustr::str_u16("GMT+"))
            || self.id.starts_with(&ustr::str_u16("GMT-"));
        !self.unknown && !custom
    }

    /// `IntlTimeZone::hasSameRules($other)`.
    pub fn has_same_rules(&self, other: &TimeZone) -> bool {
        if self.id == other.id {
            return true;
        }
        match (self.is_olson(), other.is_olson()) {
            (false, false) => {
                // SimpleTimeZone: same raw offset, neither observes DST.
                return self.raw_offset() == other.raw_offset();
            }
            (true, true) => {}
            _ => return false,
        }
        let a = self.rule_fingerprint();
        let b = other.rule_fingerprint();
        // Zones without any transition share ICU's null type map and
        // compare equal regardless of their offsets.
        if a.len() == 1 && b.len() == 1 {
            return true;
        }
        // Compare the offset histories ignoring zero-change "transitions".
        let norm = |v: Vec<(i64, i32, i32)>| {
            let mut out: Vec<(i64, i32, i32)> = Vec::new();
            for e in v {
                if out.last().is_some_and(|l| l.1 == e.1 && l.2 == e.2) {
                    continue;
                }
                out.push(e);
            }
            out
        };
        norm(a) == norm(b)
    }

    /// `IntlTimeZone::getDisplayName($isDaylight, $style, $locale)`;
    /// `locale = None` uses the default locale.
    pub fn display_name(&self, daylight: bool, style: i64, locale: Option<&str>) -> Result<String> {
        if !(display::SHORT..=display::GENERIC_LOCATION).contains(&style) {
            return Err(Error::intl(
                UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                "wrong display type",
            ));
        }
        let locale = locale.map_or_else(common::default_locale, str::to_owned);
        let name = self.display_name_u16(daylight, style, &locale);
        ustr::from_u16(&name).or_intl("could not convert resulting time zone id to UTF-16")
    }

    fn effective_offset(&self, daylight: bool) -> i32 {
        let raw = self.raw_offset();
        if daylight && self.use_daylight_time() {
            raw + self.dst_savings()
        } else {
            raw
        }
    }

    fn display_name_u16(&self, daylight: bool, style: i64, locale: &str) -> Vec<u16> {
        match style {
            display::LONG | display::SHORT | display::SHORT_COMMONLY_USED => {
                // UCAL_STANDARD=0, UCAL_SHORT_STANDARD=1, UCAL_DST=2, UCAL_SHORT_DST=3
                let ty = match (style == display::LONG, daylight) {
                    (true, false) => 0,
                    (false, false) => 1,
                    (true, true) => 2,
                    (false, true) => 3,
                };
                self.calendar()
                    .and_then(|c| c.time_zone_display_name(ty, locale))
                    .unwrap_or_default()
            }
            display::LONG_GMT => localized_gmt(self.effective_offset(daylight), locale, false),
            display::SHORT_GMT => ustr::str_u16(&iso8601_basic(self.effective_offset(daylight))),
            _ => {
                let pattern = match style {
                    display::SHORT_GENERIC => "v",
                    display::LONG_GENERIC => "vvvv",
                    _ => "VVVV",
                };
                let r = format_with_pattern(&self.id, locale, pattern, ucal::now());
                // Generic names may fall back to localized GMT; then the
                // time type of "now" decides whether it matches $isDaylight.
                let now_dst = self.offset(ucal::now(), false).is_ok_and(|(_, d)| d != 0);
                let short = style == display::SHORT_GENERIC;
                let gmt_now = format_with_pattern(
                    &self.id,
                    locale,
                    if short { "O" } else { "OOOO" },
                    ucal::now(),
                );
                if r == gmt_now && daylight != now_dst {
                    let raw = self.raw_offset();
                    let off = if daylight {
                        raw + self.dst_savings()
                    } else {
                        raw
                    };
                    return localized_gmt(off, locale, short);
                }
                r
            }
        }
    }

    /// `IntlTimeZone::toDateTimeZone()`.
    pub fn to_date_time_zone(&self) -> Result<DateTimeZoneSpec> {
        if self.id.starts_with(&ustr::str_u16("GMT")) {
            return Ok(DateTimeZoneSpec::Offset(i64::from(
                self.raw_offset() / 1000,
            )));
        }
        let id = ustr::from_u16(&self.id).or_intl("could not convert id to UTF-8")?;
        Ok(DateTimeZoneSpec::Id(id))
    }

    /// The `var_dump()` view: `(id, rawOffset, currentOffset)`.
    pub fn debug_info(&self) -> (String, Option<(i64, i64)>) {
        let id = ustr::from_u16_lossy(&self.id);
        let off = self
            .offset(ucal::now(), false)
            .ok()
            .map(|(r, d)| (i64::from(r), i64::from(r) + i64::from(d)));
        (id, off)
    }

    /// The `==` comparison handler (same ID and rules).
    pub fn equals(&self, other: &TimeZone) -> bool {
        self.id == other.id && self.unknown == other.unknown
    }
}

/// Formats `date` in zone `id` with a raw pattern.
fn format_with_pattern(id: &[u16], locale: &str, pattern: &str, date: f64) -> Vec<u16> {
    let p = ustr::str_u16(pattern);
    // UDAT_PATTERN = -2
    DateFormat::open(-2, -2, Some(locale), Some(id), Some(&p))
        .and_then(|f| f.format(date))
        .unwrap_or_default()
}

/// `TimeZoneFormat::formatOffsetLocalizedGMT` (long or short form).
fn localized_gmt(offset_ms: i32, locale: &str, short: bool) -> Vec<u16> {
    let id = ustr::str_u16(&custom_id(offset_ms));
    format_with_pattern(&id, locale, if short { "O" } else { "OOOO" }, 0.0)
}

/// A custom `GMT±hh:mm[:ss]` zone ID for an offset in milliseconds.
fn custom_id(offset_ms: i32) -> String {
    let sign = if offset_ms < 0 { '-' } else { '+' };
    let total = offset_ms.unsigned_abs() / 1000;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if s == 0 {
        format!("GMT{sign}{h:02}:{m:02}")
    } else {
        format!("GMT{sign}{h:02}:{m:02}:{s:02}")
    }
}

/// `formatOffsetISO8601Basic(offset, false, false, false)`.
fn iso8601_basic(offset_ms: i32) -> String {
    let sign = if offset_ms < 0 { '-' } else { '+' };
    let total = offset_ms.unsigned_abs() / 1000;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if s == 0 {
        format!("{sign}{h:02}{m:02}")
    } else {
        format!("{sign}{h:02}{m:02}{s:02}")
    }
}

// ---------------------------------------------------------------------------
// Static helpers
// ---------------------------------------------------------------------------

fn conv_id(id: &[u8]) -> Result<Vec<u16>> {
    ustr::to_u16(id).map_err(|c| Error::global(c, "could not convert time zone id to UTF-16"))
}

/// What `IntlTimeZone::createEnumeration()` takes.
#[derive(Debug, Clone, Copy)]
pub enum EnumerationFilter<'a> {
    /// No argument / `null`.
    All,
    /// A country code.
    Country(&'a str),
    /// A raw offset in milliseconds.
    RawOffset(i64),
}

/// `IntlTimeZone::createEnumeration()` (the VM wraps the IDs in an
/// `IntlIterator`).
pub fn create_enumeration(filter: EnumerationFilter<'_>) -> Result<Vec<String>> {
    let r = match filter {
        EnumerationFilter::All => ucal::all_time_zones(),
        EnumerationFilter::Country(c) => ucal::country_time_zones(c),
        EnumerationFilter::RawOffset(o) => {
            let o = i32::try_from(o).map_err(|_| {
                Error::arg_value(1, format!("must be between {} and {}", i32::MIN, i32::MAX))
            })?;
            ucal::time_zone_id_enumeration(0, None, Some(o))
        }
    };
    r.map_err(|_| {
        Error::global(
            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
            "error obtaining enumeration",
        )
    })
}

/// `IntlTimeZone::createTimeZoneIDEnumeration($type, $region, $rawOffset)`.
pub fn create_time_zone_id_enumeration(
    ty: i64,
    region: Option<&str>,
    raw_offset: Option<i64>,
) -> Result<Vec<String>> {
    if !(zone_type::ANY..=zone_type::CANONICAL_LOCATION).contains(&ty) {
        return Err(Error::arg_value(
            1,
            "must be one of IntlTimeZone::TYPE_ANY, IntlTimeZone::TYPE_CANONICAL, or IntlTimeZone::TYPE_CANONICAL_LOCATION",
        ));
    }
    let off = match raw_offset {
        Some(o) => Some(i32::try_from(o).map_err(|_| {
            Error::arg_value(3, format!("must be between {} and {}", i32::MIN, i32::MAX))
        })?),
        None => None,
    };
    #[allow(clippy::cast_possible_truncation)]
    ucal::time_zone_id_enumeration(ty as i32, region, off)
        .map_err(|c| Error::global(c, "error obtaining time zone id enumeration"))
}

/// `IntlTimeZone::getCanonicalID($id, &$isSystemID)` → `(id, isSystemID)`.
pub fn canonical_id(id: &[u8]) -> Result<(String, bool)> {
    let u = conv_id(id)?;
    let (r, sys) =
        ucal::canonical_id(&u).map_err(|c| Error::global(c, "error obtaining canonical ID"))?;
    let s = ustr::from_u16(&r)
        .map_err(|c| Error::global(c, "could not convert time zone id to UTF-16"))?;
    Ok((s, sys))
}

/// `IntlTimeZone::getIanaID($id)`.
pub fn iana_id(id: &[u8]) -> Result<String> {
    let u = conv_id(id)?;
    let r = ucal::iana_id(&u).map_err(|c| Error::global(c, "error obtaining IANA ID"))?;
    ustr::from_u16(&r).map_err(|c| Error::global(c, "could not convert time zone id to UTF-16"))
}

/// `IntlTimeZone::getTZDataVersion()`.
pub fn tz_data_version() -> Result<String> {
    ucal::tz_data_version().map_err(|c| Error::global(c, "error obtaining time zone data version"))
}

/// `IntlTimeZone::getWindowsID($timezoneId)`.
pub fn windows_id(id: &[u8]) -> Result<String> {
    let u = conv_id(id)?;
    let r = ucal::windows_id(&u)
        .map_err(|c| Error::global(c, "Unable to get timezone from windows ID"))?;
    if r.is_empty() {
        return Err(Error::global(
            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
            "unknown system timezone",
        ));
    }
    ustr::from_u16(&r).map_err(|c| Error::global(c, "could not convert time zone id to UTF-8"))
}

/// `IntlTimeZone::getIDForWindowsID($timezoneId, $region)`.
pub fn id_for_windows_id(win: &[u8], region: Option<&str>) -> Result<String> {
    let u = conv_id(win)?;
    let r = ucal::id_for_windows_id(&u, region)
        .map_err(|c| Error::global(c, "unable to get windows ID for timezone"))?;
    if r.is_empty() {
        return Err(Error::global(
            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
            "unknown windows timezone",
        ));
    }
    ustr::from_u16(&r).map_err(|c| Error::global(c, "could not convert time zone id to UTF-8"))
}

/// Locates a zone in `zoneinfo64` → `(top bundle, index in Names)`.
fn olson_index(id: &[u16]) -> Option<(Bundle, i32)> {
    let (top, _) = Bundle::open_direct(None, "zoneinfo64").ok()?;
    let (names, _) = top.by_key("Names").ok()?;
    let n = names.size();
    // binary search (the Names array is sorted)
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let s = names.string_by_index(mid).ok()?;
        match s.as_slice().cmp(id) {
            std::cmp::Ordering::Equal => return Some((top, mid)),
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
        }
    }
    None
}

/// The `zoneinfo64` `Zones` entry for `id`, dereferencing aliases.
fn olson_zone(id: &[u16]) -> Option<(Bundle, Bundle)> {
    let (top, idx) = olson_index(id)?;
    let (zones, _) = top.by_key("Zones").ok()?;
    let (mut res, _) = zones.by_index(idx).ok()?;
    if res.res_type() == ures::restype::INT {
        let deref = res.int().ok()?;
        res = zones.by_index(deref).ok()?.0;
    }
    Some((top, res))
}

/// `IntlTimeZone::countEquivalentIDs($id)`.
pub fn count_equivalent_ids(id: &[u8]) -> Result<i64> {
    let u = conv_id(id)?;
    let n = olson_zone(&u)
        .and_then(|(_, z)| z.by_key("links").ok())
        .and_then(|(l, _)| l.int_vector().ok())
        .map_or(0, |v| v.len());
    Ok(i64::try_from(n).unwrap_or(0))
}

/// `IntlTimeZone::getEquivalentID($id, $index)`.
pub fn equivalent_id(id: &[u8], index: i64) -> Result<String> {
    let index = i32::try_from(index).map_err(|_| {
        Error::arg_value(2, format!("must be between {} and {}", i32::MIN, i32::MAX))
    })?;
    let u = conv_id(id)?;
    let result = (|| {
        let (top, zone) = olson_zone(&u)?;
        let links = zone.by_key("links").ok()?.0.int_vector().ok()?;
        let z = *links.get(usize::try_from(index).ok()?)?;
        let (names, _) = top.by_key("Names").ok()?;
        names.string_by_index(z).ok()
    })()
    .unwrap_or_default();
    ustr::from_u16(&result)
        .map_err(|c| Error::global(c, "could not convert resulting time zone id to UTF-16"))
}

/// `IntlTimeZone::getRegion($id)`.
pub fn region(id: &[u8]) -> Result<String> {
    let u = conv_id(id)?;
    let fail = || Error::global(UErrorCode::ILLEGAL_ARGUMENT_ERROR, "error obtaining region");
    if u == ustr::str_u16(UNKNOWN_ZONE_ID) {
        return Err(fail());
    }
    // TimeZone::getRegion: canonical IDs and links are in Names/Regions.
    let (top, idx) = olson_index(&u).ok_or_else(fail)?;
    let (regions, _) = top.by_key("Regions").map_err(|_| fail())?;
    let r = regions.string_by_index(idx).map_err(|_| fail())?;
    Ok(ustr::from_u16_lossy(&r))
}

// ---------------------------------------------------------------------------
// Argument coercions
// ---------------------------------------------------------------------------

/// `timezone_convert_datetimezone()`: a `DateTimeZone`'s zone as an ICU zone.
/// Errors are intl errors with `Scope::Both` (callers re-scope them).
pub fn from_php_zone(zone: &PhpZone) -> Result<TimeZone> {
    let id = match zone {
        PhpZone::Id(s) | PhpZone::Abbr(s) => s.clone(),
        PhpZone::Offset(secs) => {
            let offset_mins = secs / 60;
            let hours = offset_mins / 60;
            let mut minutes = offset_mins - hours * 60;
            if minutes < 0 {
                minutes = -minutes;
            }
            if offset_mins <= -24 * 60 || offset_mins >= 24 * 60 {
                return Err(Error::intl(
                    UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                    "object has a time zone offset that is too large",
                ));
            }
            let sign = if hours < 0 { '-' } else { '+' };
            format!("GMT{sign}{:02}:{minutes:02}", hours.abs())
        }
    };
    let tz = TimeZone::create_u16(&ustr::str_u16(&id));
    if tz.is_unknown() {
        return Err(Error::intl(
            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
            format!("time zone id '{id}' extracted from ext/date DateTimeZone not recognized"),
        ));
    }
    Ok(tz)
}

/// `IntlTimeZone::fromDateTimeZone($tz)`; errors are global-only.
pub fn from_date_time_zone(zone: &PhpZone) -> Result<TimeZone> {
    from_php_zone(zone).map_err(|e| rescope(e, Scope::Global))
}

/// Re-targets an intl error.
pub(crate) fn rescope(e: Error, scope: Scope) -> Error {
    match e {
        Error::Intl(i) => Error::Intl(i.with_scope(scope)),
        other => other,
    }
}

/// `timezone_process_timezone_argument()`.  `DateTimeZone` conversion
/// failures are intl errors (`Scope::Both`; re-scope for static callers),
/// everything else throws `IntlException` prefixed with the function name.
pub fn process_timezone_argument(arg: TimeZoneArg<'_>) -> Result<TimeZone> {
    match arg {
        TimeZoneArg::Intl(tz) => Ok(tz.clone()),
        TimeZoneArg::DateTimeZone(z) => from_php_zone(z),
        TimeZoneArg::Str(s) => {
            let Ok(u) = ustr::to_u16(s) else {
                return Err(Error::IntlExceptionInFunction(
                    "Time zone identifier given is not a valid UTF-8 string".into(),
                ));
            };
            let tz = TimeZone::create_u16(&u);
            if tz.is_unknown() {
                return Err(Error::IntlExceptionInFunction(format!(
                    "No such time zone: \"{}\"",
                    String::from_utf8_lossy(s)
                )));
            }
            Ok(tz)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        let tz = TimeZone::create_time_zone(b"Europe/Lisbon").unwrap();
        assert_eq!(tz.id().unwrap(), "Europe/Lisbon");
        assert!(tz.use_daylight_time());
        assert_eq!(tz.dst_savings(), 3_600_000);
        assert_eq!(tz.raw_offset(), 0);
        let tz = TimeZone::create_time_zone(b"GMT+5").unwrap();
        assert_eq!(tz.id().unwrap(), "GMT+05:00");
        assert!(!tz.use_daylight_time());
        assert!(TimeZone::create_time_zone(b"nowhere").unwrap().is_unknown());
        assert_eq!(
            tz.to_date_time_zone().unwrap(),
            DateTimeZoneSpec::Offset(18000)
        );
    }

    #[test]
    fn offsets() {
        let tz = TimeZone::create_time_zone(b"Europe/Lisbon").unwrap();
        // 2012-07-01T00:00Z
        assert_eq!(
            tz.offset(1_341_100_800_000.0, false).unwrap(),
            (0, 3_600_000)
        );
        assert_eq!(tz.offset(1_325_376_000_000.0, false).unwrap(), (0, 0));
    }

    #[test]
    fn equivalents_and_region() {
        assert!(count_equivalent_ids(b"Europe/Lisbon").unwrap() >= 2);
        assert_eq!(region(b"Europe/Lisbon").unwrap(), "PT");
        assert!(region(b"foo").is_err());
        let e = equivalent_id(b"Europe/Lisbon", 0).unwrap();
        assert!(!e.is_empty());
    }

    #[test]
    fn display_names() {
        let tz = TimeZone::create_time_zone(b"Europe/Lisbon").unwrap();
        assert_eq!(
            tz.display_name(false, display::LONG, Some("en_US"))
                .unwrap(),
            "Western European Standard Time"
        );
        assert_eq!(
            tz.display_name(true, display::LONG, Some("en_US")).unwrap(),
            "Western European Summer Time"
        );
        assert_eq!(
            tz.display_name(false, display::SHORT_GMT, Some("en_US"))
                .unwrap(),
            "+0000"
        );
        assert_eq!(
            tz.display_name(true, display::LONG_GMT, Some("en_US"))
                .unwrap(),
            "GMT+01:00"
        );
        assert_eq!(
            tz.display_name(false, display::GENERIC_LOCATION, Some("en_US"))
                .unwrap(),
            "Portugal Time"
        );
    }
}

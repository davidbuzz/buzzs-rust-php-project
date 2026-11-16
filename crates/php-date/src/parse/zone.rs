// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Timezone specifications inside date strings (`timelib_parse_zone` and
//! friends from `parse_date.re`).

use crate::time::{Time, ZoneType};
use crate::tz;

/// glibc's `_POSIX_TZNAME_MAX`: only words shorter than this are tried as
/// abbreviations.
const MAX_ABBR_LEN: usize = 6;

/// A byte cursor that reads NUL past the end (like a C string).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Cur<'a> {
    pub(crate) s: &'a [u8],
    pub(crate) p: usize,
}

impl<'a> Cur<'a> {
    pub(crate) fn new(s: &'a [u8], p: usize) -> Self {
        Self { s, p }
    }

    /// Current byte, NUL at/after the end.
    pub(crate) fn c(&self) -> u8 {
        self.s.get(self.p).copied().unwrap_or(0)
    }

    /// Byte at offset `k` from the current position.
    pub(crate) fn at(&self, k: usize) -> u8 {
        self.s.get(self.p + k).copied().unwrap_or(0)
    }

    pub(crate) fn adv(&mut self) {
        self.p += 1;
    }
}

/// `strtol(s, NULL, 10)` on a byte string (leading digits only; our inputs
/// never carry whitespace or signs here).
pub(crate) fn strtol(s: &[u8]) -> i64 {
    let mut v: i64 = 0;
    for &b in s {
        if !b.is_ascii_digit() {
            break;
        }
        v = v.saturating_mul(10).saturating_add(i64::from(b - b'0'));
    }
    v
}

#[allow(clippy::cast_possible_truncation)]
fn s_hour(a: i64) -> i64 {
    i64::from(a.wrapping_mul(3600) as i32)
}

#[allow(clippy::cast_possible_truncation)]
fn s_min(a: i64) -> i64 {
    i64::from(a.wrapping_mul(60) as i32)
}

/// `timelib_parse_tz_cor`: `[+-]` already consumed; returns seconds and
/// whether the offset was recognised.
fn parse_tz_cor(c: &mut Cur<'_>) -> (i64, bool) {
    let begin = c.p;
    while c.c().is_ascii_digit() || c.c() == b':' {
        c.adv();
    }
    let b = &c.s[begin..c.p];
    let at = |k: usize| b.get(k).copied().unwrap_or(0);
    let from = |k: usize| b.get(k..).unwrap_or(&[]);
    match b.len() {
        1 | 2 => (s_hour(strtol(b)), true),
        3 | 4 => {
            if at(1) == b':' {
                (s_hour(strtol(b)) + s_min(strtol(from(2))), true)
            } else if at(2) == b':' {
                (s_hour(strtol(b)) + s_min(strtol(from(3))), true)
            } else {
                let tmp = strtol(b);
                (s_hour(tmp / 100) + s_min(tmp % 100), true)
            }
        }
        5 if at(2) == b':' => (s_hour(strtol(b)) + s_min(strtol(from(3))), true),
        6 => {
            let tmp = strtol(b);
            (
                s_hour(tmp / 10000) + s_min((tmp / 100) % 100) + (tmp % 100),
                true,
            )
        }
        8 if at(2) == b':' && at(5) == b':' => (
            s_hour(strtol(b)) + s_min(strtol(from(3))) + strtol(from(6)),
            true,
        ),
        _ => (0, false),
    }
}

/// `timelib_lookup_abbr`: returns (offset, dst, word, found).
fn lookup_abbr(c: &mut Cur<'_>) -> (i64, i64, String, bool) {
    let begin = c.p;
    while c.c().is_ascii_alphanumeric() || matches!(c.c(), b'/' | b'_' | b'-' | b'+') {
        c.adv();
    }
    let word = String::from_utf8_lossy(&c.s[begin..c.p]).into_owned();
    if word.len() < MAX_ABBR_LEN {
        if let Some(tp) = tz::abbr_search(&word, -1, 0) {
            let dst = i64::from(tp.dst);
            return (tp.offset - dst * 3600, dst, word, true);
        }
    }
    (0, 0, word, false)
}

/// `timelib_parse_zone`: parse a zone at the cursor, updating the zone
/// fields of `t`. Returns the UTC offset (excluding DST for
/// abbreviations) and `true` if the zone was *not* found.
pub(crate) fn parse_zone(c: &mut Cur<'_>, t: &mut Time) -> (i64, bool) {
    let mut paren_count = 0;
    while matches!(c.c(), b' ' | b'\t' | b'(') {
        if c.c() == b'(' {
            paren_count += 1;
        }
        c.adv();
    }
    if c.c() == b'G' && c.at(1) == b'M' && c.at(2) == b'T' && matches!(c.at(3), b'+' | b'-') {
        c.p += 3;
    }
    let (retval, not_found);
    if c.c() == b'+' || c.c() == b'-' {
        let neg = c.c() == b'-';
        c.adv();
        t.is_localtime = true;
        t.zone_type = ZoneType::Offset;
        t.dst = 0;
        let (v, ok) = parse_tz_cor(c);
        retval = if neg { -v } else { v };
        not_found = !ok;
    } else {
        t.is_localtime = true;
        let (offset, dst, word, abbr_found) = lookup_abbr(c);
        let mut found = 0;
        if abbr_found {
            found = 1;
            t.zone_type = ZoneType::Abbr;
            t.dst = dst;
            t.tz_abbr = Some(word.to_ascii_uppercase());
        }
        if !abbr_found || word == "UTC" {
            if let Some(res) = tz::get(&word) {
                t.tz_info = Some(res);
                t.zone_type = ZoneType::Id;
                found += 1;
            }
        }
        not_found = found == 0;
        retval = offset;
    }
    while paren_count > 0 && c.c() == b')' {
        c.adv();
        paren_count -= 1;
    }
    (retval, not_found)
}

/// Result of parsing a `DateTimeZone` constructor argument.
#[derive(Debug, Clone, PartialEq)]
pub enum TimezoneSpec {
    /// A UTC offset in seconds (`timezone_type` 1).
    Offset(i64),
    /// An abbreviation (`timezone_type` 2): upper-cased name, offset
    /// excluding DST, DST flag.
    Abbr {
        /// Upper-case abbreviation.
        abbr: String,
        /// UTC offset excluding the DST hour.
        utc_offset: i64,
        /// DST flag.
        dst: bool,
    },
    /// A zone identifier (`timezone_type` 3).
    Id(std::sync::Arc<tz::TzInfo>),
}

/// `timezone_initialize()`: parse a `DateTimeZone` constructor string.
///
/// On failure returns PHP's warning/exception message, e.g.
/// `"Unknown or bad timezone (Foo)"` or
/// `"Timezone offset is out of range (+101:00)"`.
pub fn parse_timezone_spec(spec: &str) -> Result<TimezoneSpec, String> {
    if spec.contains('\0') {
        return Err(format!("Unknown or bad timezone ({spec})"));
    }
    let mut t = Time::default();
    let mut c = Cur::new(spec.as_bytes(), 0);
    let (z, not_found) = parse_zone(&mut c, &mut t);
    if z >= 100 * 3600 || z <= -100 * 3600 {
        return Err(format!("Timezone offset is out of range ({spec})"));
    }
    if not_found || c.c() != 0 {
        return Err(format!("Unknown or bad timezone ({spec})"));
    }
    match t.zone_type {
        ZoneType::Offset => Ok(TimezoneSpec::Offset(z)),
        ZoneType::Abbr => Ok(TimezoneSpec::Abbr {
            abbr: t.tz_abbr.unwrap_or_default(),
            utc_offset: z,
            dst: t.dst != 0,
        }),
        ZoneType::Id => t
            .tz_info
            .map(TimezoneSpec::Id)
            .ok_or_else(|| format!("Unknown or bad timezone ({spec})")),
        ZoneType::None => Err(format!("Unknown or bad timezone ({spec})")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone(s: &str) -> (i64, bool, ZoneType, i64, Option<String>) {
        let mut t = Time::default();
        let mut c = Cur::new(s.as_bytes(), 0);
        let (z, nf) = parse_zone(&mut c, &mut t);
        (z, nf, t.zone_type, t.dst, t.tz_abbr)
    }

    #[test]
    fn offsets() {
        assert_eq!(zone("+0200").0, 7200);
        assert_eq!(zone("-05:30").0, -19800);
        assert_eq!(zone("GMT+1").0, 3600);
        assert_eq!(zone("+1").2, ZoneType::Offset);
        assert!(zone("+12345").1);
        assert_eq!(zone("+123456").0, 12 * 3600 + 34 * 60 + 56);
        assert_eq!(zone("+12:34:56").0, 12 * 3600 + 34 * 60 + 56);
    }

    #[test]
    fn abbreviations_and_ids() {
        let (z, nf, ty, dst, abbr) = zone("CEST");
        assert_eq!(
            (z, nf, ty, dst, abbr.as_deref()),
            (3600, false, ZoneType::Abbr, 1, Some("CEST"))
        );
        let (_, nf, ty, ..) = zone("Europe/Amsterdam");
        assert_eq!((nf, ty), (false, ZoneType::Id));
        let (_, nf, ty, ..) = zone("UTC");
        assert_eq!((nf, ty), (false, ZoneType::Id));
        let (_, nf, ty, ..) = zone("utc");
        assert_eq!((nf, ty), (false, ZoneType::Abbr));
        assert!(zone("Foo/Bar").1);
    }

    #[test]
    fn timezone_specs() {
        assert_eq!(
            parse_timezone_spec("+01:00"),
            Ok(TimezoneSpec::Offset(3600))
        );
        assert!(matches!(
            parse_timezone_spec("Europe/London"),
            Ok(TimezoneSpec::Id(_))
        ));
        assert_eq!(
            parse_timezone_spec("EDT"),
            Ok(TimezoneSpec::Abbr {
                abbr: "EDT".into(),
                utc_offset: -18000,
                dst: true
            })
        );
        assert_eq!(
            parse_timezone_spec("Foo").unwrap_err(),
            "Unknown or bad timezone (Foo)"
        );
        assert_eq!(parse_timezone_spec("+100:00"), Ok(TimezoneSpec::Offset(60)));
        assert_eq!(
            parse_timezone_spec("+999999").unwrap_err(),
            "Timezone offset is out of range (+999999)"
        );
        assert_eq!(
            parse_timezone_spec("Europe/London ").unwrap_err(),
            "Unknown or bad timezone (Europe/London )"
        );
    }
}

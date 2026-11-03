//! `date()` / `DateTime::format()` and `DateInterval::format()` (ports of
//! `date_format()` and `date_interval_format()` from `php_date.c`).

use std::fmt::Write as _;
use std::sync::Arc;

use crate::dow::{day_of_week, day_of_year, days_in_month, is_leap, iso_day_of_week, isoweek_from_date};
use crate::time::{RelTime, Time, ZoneType};
use crate::tz::{TimeOffset, TzInfo};
use crate::UNSET;

/// Full month names.
pub const MON_FULL_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
/// Abbreviated month names.
pub const MON_SHORT_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
/// Full day names, Sunday first.
pub const DAY_FULL_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
/// Abbreviated day names, Sunday first.
pub const DAY_SHORT_NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Predefined `DateTimeInterface` format constants.
pub mod formats {
    /// `DATE_ATOM`
    pub const ATOM: &str = "Y-m-d\\TH:i:sP";
    /// `DATE_COOKIE`
    pub const COOKIE: &str = "l, d-M-Y H:i:s T";
    /// `DATE_ISO8601`
    pub const ISO8601: &str = "Y-m-d\\TH:i:sO";
    /// `DATE_ISO8601_EXPANDED`
    pub const ISO8601_EXPANDED: &str = "X-m-d\\TH:i:sP";
    /// `DATE_RFC822`
    pub const RFC822: &str = "D, d M y H:i:s O";
    /// `DATE_RFC850`
    pub const RFC850: &str = "l, d-M-y H:i:s T";
    /// `DATE_RFC1036`
    pub const RFC1036: &str = "D, d M y H:i:s O";
    /// `DATE_RFC1123`
    pub const RFC1123: &str = "D, d M Y H:i:s O";
    /// `DATE_RFC7231`
    pub const RFC7231: &str = "D, d M Y H:i:s \\G\\M\\T";
    /// `DATE_RFC2822`
    pub const RFC2822: &str = "D, d M Y H:i:s O";
    /// `DATE_RFC3339`
    pub const RFC3339: &str = "Y-m-d\\TH:i:sP";
    /// `DATE_RFC3339_EXTENDED`
    pub const RFC3339_EXTENDED: &str = "Y-m-d\\TH:i:s.vP";
    /// `DATE_RSS`
    pub const RSS: &str = "D, d M Y H:i:s O";
    /// `DATE_W3C`
    pub const W3C: &str = "Y-m-d\\TH:i:sP";
}

/// English ordinal suffix for a day number (`S`).
pub fn english_suffix(number: i64) -> &'static str {
    if (10..=19).contains(&number) {
        return "th";
    }
    match number % 10 {
        1 => "st",
        2 => "nd",
        3 => "rd",
        _ => "th",
    }
}

fn month_name(m: i64, full: bool) -> &'static str {
    let idx = usize::try_from(m - 1).ok().filter(|&i| i < 12);
    match idx {
        Some(i) if full => MON_FULL_NAMES[i],
        Some(i) => MON_SHORT_NAMES[i],
        None => "",
    }
}

fn day_name(t: &Time, full: bool) -> &'static str {
    let dow = usize::try_from(day_of_week(t.y, t.m, t.d)).unwrap_or(0) % 7;
    if full {
        DAY_FULL_NAMES[dow]
    } else {
        DAY_SHORT_NAMES[dow]
    }
}

/// C's `(int)` truncation of a 64-bit value.
#[allow(clippy::cast_possible_truncation)]
fn int(v: i64) -> i32 {
    v as i32
}

/// The "offset" view of a time used by `date_format` for the `localtime`
/// case.
pub fn time_offset(t: &Time) -> TimeOffset {
    match t.zone_type {
        ZoneType::Abbr => TimeOffset {
            offset: int(t.z + t.dst * 3600),
            leap_secs: 0,
            is_dst: t.dst != 0,
            abbr: t.tz_abbr.clone().unwrap_or_default(),
            transition_time: 0,
        },
        ZoneType::Offset => {
            let offset = int(t.z);
            TimeOffset {
                offset,
                leap_secs: 0,
                is_dst: false,
                abbr: format!(
                    "GMT{}{:02}{:02}",
                    if offset < 0 { '-' } else { '+' },
                    (offset / 3600).abs(),
                    ((offset % 3600) / 60).abs()
                ),
                transition_time: 0,
            }
        }
        ZoneType::Id => match &t.tz_info {
            Some(tz) => tz.time_zone_info(t.sse),
            None => TimeOffset {
                offset: 0,
                leap_secs: 0,
                is_dst: false,
                abbr: String::new(),
                transition_time: 0,
            },
        },
        ZoneType::None => TimeOffset {
            offset: 0,
            leap_secs: 0,
            is_dst: false,
            abbr: String::new(),
            transition_time: 0,
        },
    }
}

fn abs_year(y: i64) -> u64 {
    y.unsigned_abs()
}

/// Swatch internet time (`B`) for a timestamp.
pub fn swatch_beat(sse: i64) -> i64 {
    let x = sse.wrapping_rem(86400).wrapping_add(3600);
    let mut retval = i64::from(int(sse.wrapping_sub(sse.wrapping_sub(x)).wrapping_mul(10)));
    if retval < 0 {
        retval += 864_000;
    }
    (retval / 864) % 1000
}

/// `date_format()`: format `t` according to PHP's `date()` format string.
///
/// `localtime` is `false` for `gmdate()` (and for times without a zone).
pub fn format_date(format: &[u8], t: &Time, localtime: bool) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(format.len() * 2);
    if format.is_empty() {
        return out;
    }
    let offset = if localtime { Some(time_offset(t)) } else { None };
    let off = offset.as_ref().map_or(0, |o| o.offset);
    let mut iso: Option<(i64, i64)> = None;
    let mut buf = String::new();
    let mut i = 0;
    while i < format.len() {
        buf.clear();
        let c = format[i];
        match c {
            b'd' => {
                let _ = write!(buf, "{:02}", int(t.d));
            }
            b'D' => buf.push_str(day_name(t, false)),
            b'j' => {
                let _ = write!(buf, "{}", int(t.d));
            }
            b'l' => buf.push_str(day_name(t, true)),
            b'S' => buf.push_str(english_suffix(t.d)),
            b'w' => {
                let _ = write!(buf, "{}", day_of_week(t.y, t.m, t.d));
            }
            b'N' => {
                let _ = write!(buf, "{}", iso_day_of_week(t.y, t.m, t.d));
            }
            b'z' => {
                let _ = write!(buf, "{}", int(day_of_year(t.y, t.m, t.d)));
            }
            b'W' | b'o' => {
                let (w, y) = *iso.get_or_insert_with(|| isoweek_from_date(t.y, t.m, t.d));
                if c == b'W' {
                    let _ = write!(buf, "{:02}", int(w));
                } else {
                    let _ = write!(buf, "{y}");
                }
            }
            b'F' => buf.push_str(month_name(t.m, true)),
            b'm' => {
                let _ = write!(buf, "{:02}", int(t.m));
            }
            b'M' => buf.push_str(month_name(t.m, false)),
            b'n' => {
                let _ = write!(buf, "{}", int(t.m));
            }
            b't' => {
                let _ = write!(buf, "{}", days_in_month(t.y, t.m));
            }
            b'L' => {
                let _ = write!(buf, "{}", i32::from(is_leap(i64::from(int(t.y)))));
            }
            b'y' => {
                let _ = write!(buf, "{:02}", int(t.y % 100));
            }
            b'Y' => {
                let _ = write!(buf, "{}{:04}", if t.y < 0 { "-" } else { "" }, abs_year(t.y));
            }
            b'x' => {
                let sign = if t.y < 0 {
                    "-"
                } else if t.y >= 10000 {
                    "+"
                } else {
                    ""
                };
                let _ = write!(buf, "{sign}{:04}", abs_year(t.y));
            }
            b'X' => {
                let _ = write!(buf, "{}{:04}", if t.y < 0 { "-" } else { "+" }, abs_year(t.y));
            }
            b'a' => buf.push_str(if t.h >= 12 { "pm" } else { "am" }),
            b'A' => buf.push_str(if t.h >= 12 { "PM" } else { "AM" }),
            b'B' => {
                let _ = write!(buf, "{:03}", swatch_beat(t.sse));
            }
            b'g' => {
                let _ = write!(buf, "{}", if t.h % 12 != 0 { int(t.h % 12) } else { 12 });
            }
            b'G' => {
                let _ = write!(buf, "{}", int(t.h));
            }
            b'h' => {
                let _ = write!(buf, "{:02}", if t.h % 12 != 0 { int(t.h % 12) } else { 12 });
            }
            b'H' => {
                let _ = write!(buf, "{:02}", int(t.h));
            }
            b'i' => {
                let _ = write!(buf, "{:02}", int(t.i));
            }
            b's' => {
                let _ = write!(buf, "{:02}", int(t.s));
            }
            b'u' => {
                let _ = write!(buf, "{:06}", int(t.us));
            }
            b'v' => {
                let _ = write!(buf, "{:03}", int(t.us / 1000));
            }
            b'I' => {
                let _ = write!(buf, "{}", i32::from(offset.as_ref().is_some_and(|o| o.is_dst)));
            }
            b'p' | b'P' | b'O' => {
                let is_z = c == b'p'
                    && offset
                        .as_ref()
                        .is_none_or(|o| o.abbr == "UTC" || o.abbr == "Z" || o.abbr == "GMT+0000");
                if is_z {
                    buf.push('Z');
                } else {
                    let colon = c != b'O';
                    let sign = if localtime && off < 0 { '-' } else { '+' };
                    let _ = write!(
                        buf,
                        "{sign}{:02}{}{:02}",
                        (off / 3600).abs(),
                        if colon { ":" } else { "" },
                        ((off % 3600) / 60).abs()
                    );
                }
            }
            b'T' => match &offset {
                Some(o) => buf.push_str(&o.abbr),
                None => buf.push_str("GMT"),
            },
            b'e' => {
                if localtime {
                    match t.zone_type {
                        ZoneType::Id => {
                            if let Some(tz) = &t.tz_info {
                                buf.push_str(&tz.name);
                            }
                        }
                        ZoneType::Abbr => {
                            if let Some(o) = &offset {
                                buf.push_str(&o.abbr);
                            }
                        }
                        ZoneType::Offset => buf.push_str(&offset_string(i64::from(off))),
                        ZoneType::None => {}
                    }
                } else {
                    buf.push_str("UTC");
                }
            }
            b'Z' => {
                let _ = write!(buf, "{off}");
            }
            b'c' => {
                let sign = if localtime && off < 0 { '-' } else { '+' };
                let _ = write!(
                    buf,
                    "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}",
                    t.y,
                    int(t.m),
                    int(t.d),
                    int(t.h),
                    int(t.i),
                    int(t.s),
                    (off / 3600).abs(),
                    ((off % 3600) / 60).abs()
                );
            }
            b'r' => {
                let sign = if localtime && off < 0 { '-' } else { '+' };
                let _ = write!(
                    buf,
                    "{:>3}, {:02} {:>3} {:04} {:02}:{:02}:{:02} {sign}{:02}{:02}",
                    day_name(t, false),
                    int(t.d),
                    month_name(t.m, false),
                    t.y,
                    int(t.h),
                    int(t.i),
                    int(t.s),
                    (off / 3600).abs(),
                    ((off % 3600) / 60).abs()
                );
            }
            b'U' => {
                let _ = write!(buf, "{}", t.sse);
            }
            b'\\' => {
                i += 1;
                out.push(format.get(i).copied().unwrap_or(0));
                i += 1;
                continue;
            }
            other => {
                out.push(other);
                i += 1;
                continue;
            }
        }
        out.extend_from_slice(buf.as_bytes());
        i += 1;
    }
    out
}

/// [`format_date`] for `&str` formats, returning a `String` (lossy only if
/// the format itself contains invalid UTF-8 escapes).
pub fn format(format: &str, t: &Time, localtime: bool) -> String {
    String::from_utf8_lossy(&format_date(format.as_bytes(), t, localtime)).into_owned()
}

/// `gmdate($format, $ts)`.
pub fn date_gmt(format: &str, ts: i64) -> String {
    let t = Time::from_gmt(ts);
    self::format(format, &t, false)
}

/// `date($format, $ts)` in zone `tz`.
pub fn date_local(format: &str, ts: i64, tz: &Arc<TzInfo>) -> String {
    let t = Time::from_timestamp_in(ts, tz);
    self::format(format, &t, true)
}

/// `+HH:MM` (or `+HH:MM:SS` when there are seconds) for a UTC offset, as
/// used for offset-type timezone names (`date_create_tz_offset_str`).
pub fn offset_string(offset: i64) -> String {
    let seconds = offset % 60;
    let sign = if offset < 0 { '-' } else { '+' };
    let h = (offset / 3600).abs();
    let m = ((offset % 3600) / 60).abs();
    if seconds == 0 {
        format!("{sign}{h:02}:{m:02}")
    } else {
        format!("{sign}{h:02}:{m:02}:{:02}", seconds.abs())
    }
}

/// `DateInterval::format()`.
pub fn format_interval(format: &[u8], t: &RelTime) -> Vec<u8> {
    let mut out = Vec::with_capacity(format.len() * 2);
    let mut have_spec = false;
    for &c in format {
        if have_spec {
            let s = match c {
                b'Y' => format!("{:02}", int(t.y)),
                b'y' => format!("{}", int(t.y)),
                b'M' => format!("{:02}", int(t.m)),
                b'm' => format!("{}", int(t.m)),
                b'D' => format!("{:02}", int(t.d)),
                b'd' => format!("{}", int(t.d)),
                b'H' => format!("{:02}", int(t.h)),
                b'h' => format!("{}", int(t.h)),
                b'I' => format!("{:02}", int(t.i)),
                b'i' => format!("{}", int(t.i)),
                b'S' => format!("{:02}", t.s),
                b's' => format!("{}", t.s),
                b'F' => format!("{:06}", t.us),
                b'f' => format!("{}", t.us),
                b'a' => {
                    if i64::from(int(t.days)) == UNSET {
                        "(unknown)".to_string()
                    } else {
                        format!("{}", int(t.days))
                    }
                }
                b'r' => (if t.invert { "-" } else { "" }).to_string(),
                b'R' => (if t.invert { "-" } else { "+" }).to_string(),
                b'%' => "%".to_string(),
                other => {
                    out.push(b'%');
                    out.push(other);
                    have_spec = false;
                    continue;
                }
            };
            out.extend_from_slice(s.as_bytes());
            have_spec = false;
        } else if c == b'%' {
            have_spec = true;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tz;

    #[test]
    fn gmdate_basics() {
        assert_eq!(date_gmt("Y-m-d H:i:s", 0), "1970-01-01 00:00:00");
        assert_eq!(
            date_gmt("D, d M Y H:i:s O", 1_234_567_890),
            "Fri, 13 Feb 2009 23:31:30 +0000"
        );
        assert_eq!(date_gmt("c", 1_234_567_890), "2009-02-13T23:31:30+00:00");
        assert_eq!(date_gmt("r", 1_234_567_890), "Fri, 13 Feb 2009 23:31:30 +0000");
        assert_eq!(date_gmt("T e P p Z I", 0), "GMT UTC +00:00 Z 0 0");
        assert_eq!(date_gmt("\\Y\\m\\d", 0), "Ymd");
        assert_eq!(date_gmt("jS F, l z t L N w W o", 1_234_567_890), "13th February, Friday 43 28 0 5 5 07 2009");
        assert_eq!(date_gmt("B", 0), "041");
        assert_eq!(date_gmt("g G h H a A", 13 * 3600), "1 13 01 13 pm PM");
        assert_eq!(date_gmt("x X", 0), "1970 +1970");
        assert_eq!(format_date(b"\\", &Time::from_gmt(0), false), b"\0");
    }

    #[test]
    fn local_date() {
        let tz = tz::get("Europe/Amsterdam").unwrap();
        assert_eq!(
            date_local("Y-m-d H:i:s T e P O Z I U", 1_121_373_041, &tz),
            "2005-07-14 22:30:41 CEST Europe/Amsterdam +02:00 +0200 7200 1 1121373041"
        );
    }

    #[test]
    fn offsets_and_suffixes() {
        assert_eq!(offset_string(3600), "+01:00");
        assert_eq!(offset_string(-19800), "-05:30");
        assert_eq!(offset_string(3601), "+01:00:01");
        assert_eq!(english_suffix(1), "st");
        assert_eq!(english_suffix(11), "th");
        assert_eq!(english_suffix(22), "nd");
        assert_eq!(english_suffix(23), "rd");
    }

    #[test]
    fn interval_format() {
        let r = RelTime {
            y: 1,
            m: 2,
            d: 3,
            h: 4,
            i: 5,
            s: 6,
            us: 7,
            days: UNSET,
            invert: true,
            ..RelTime::default()
        };
        assert_eq!(
            format_interval(b"%y %Y %m %M %d %D %h %H %i %I %s %S %f %F %a %r %R %% %z", &r),
            b"1 01 2 02 3 03 4 04 5 05 6 06 7 000007 (unknown) - - % %z".to_vec()
        );
    }
}

//! POSIX TZ footer strings (`EST5EDT,M3.2.0,M11.1.0`), port of timelib's
//! `parse_posix.c`.
//!
//! Slim `TZif` files only carry transitions up to the point where the footer
//! rule takes over; later offsets are computed from this rule.

use crate::dow::is_leap;
use crate::UNSET;

/// How a POSIX transition day is specified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransDay {
    /// `Jn`: Julian day 1..365, February 29th never counted.
    JulianNoFeb29(i64),
    /// `n`: zero-based day of year 0..365, February 29th counted.
    JulianFeb29(i64),
    /// `Mm.w.d`: day `d` of week `w` of month `m`.
    Mwd {
        /// Month 1..12.
        month: i64,
        /// Week 1..5 (5 = last).
        week: i64,
        /// Day of week 0 (Sunday)..6.
        dow: i64,
    },
}

/// One transition specification (`start[/time]` or `end[/time]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransInfo {
    /// The day rule.
    pub day: TransDay,
    /// Local time of the transition, in seconds after midnight.
    pub hour: i64,
}

/// A parsed POSIX TZ string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PosixStr {
    /// Standard time abbreviation.
    pub std: String,
    /// Standard UTC offset in seconds (east positive).
    pub std_offset: i64,
    /// DST abbreviation, if any.
    pub dst: Option<String>,
    /// DST UTC offset in seconds.
    pub dst_offset: i64,
    /// DST start rule.
    pub dst_begin: Option<TransInfo>,
    /// DST end rule.
    pub dst_end: Option<TransInfo>,
    /// Index into the zone's type table for standard time.
    pub type_index_std: usize,
    /// Index into the zone's type table for DST.
    pub type_index_dst: usize,
}

struct Reader<'a> {
    s: &'a [u8],
    p: usize,
}

impl Reader<'_> {
    fn peek(&self) -> u8 {
        self.s.get(self.p).copied().unwrap_or(0)
    }

    fn description(&mut self) -> Option<String> {
        if self.peek() == b'<' {
            self.p += 1;
            let begin = self.p;
            while self.peek() != 0 && self.peek() != b'>' {
                self.p += 1;
            }
            if self.peek() == 0 {
                return None;
            }
            let end = self.p;
            self.p += 1;
            if end == begin {
                return None;
            }
            Some(String::from_utf8_lossy(&self.s[begin..end]).into_owned())
        } else {
            let begin = self.p;
            while self.peek().is_ascii_alphabetic() {
                self.p += 1;
            }
            if self.p == begin {
                return None;
            }
            Some(String::from_utf8_lossy(&self.s[begin..self.p]).into_owned())
        }
    }

    fn sign(&mut self) -> i64 {
        match self.peek() {
            b'+' => {
                self.p += 1;
                1
            }
            b'-' => {
                self.p += 1;
                -1
            }
            _ => 1,
        }
    }

    fn number(&mut self) -> i64 {
        let begin = self.p;
        while self.peek() == b'0' {
            self.p += 1;
        }
        let mut acc: i32 = 0;
        while self.peek().is_ascii_digit() {
            acc = acc
                .wrapping_mul(10)
                .wrapping_add(i32::from(self.peek() - b'0'));
            self.p += 1;
        }
        if begin == self.p {
            UNSET
        } else {
            i64::from(acc)
        }
    }

    fn offset(&mut self) -> i64 {
        let bias = self.sign();
        let begin = self.p;
        let hours = self.number();
        if hours == UNSET {
            return UNSET;
        }
        let mut minutes = 0;
        let mut seconds = 0;
        if self.peek() == b':' {
            self.p += 1;
            minutes = self.number();
            if minutes == UNSET {
                return UNSET;
            }
        }
        if self.peek() == b':' {
            self.p += 1;
            seconds = self.number();
            if seconds == UNSET {
                return UNSET;
            }
        }
        if begin == self.p {
            return UNSET;
        }
        -bias * (hours * 3600 + minutes * 60 + seconds)
    }

    fn trans_spec(&mut self) -> Option<TransInfo> {
        let day = if self.peek() == b'M' {
            self.p += 1;
            let month = self.number();
            if month == UNSET || self.peek() != b'.' {
                return None;
            }
            self.p += 1;
            let week = self.number();
            if week == UNSET || self.peek() != b'.' {
                return None;
            }
            self.p += 1;
            let dow = self.number();
            if dow == UNSET {
                return None;
            }
            TransDay::Mwd { month, week, dow }
        } else {
            let julian = self.peek() == b'J';
            if julian {
                self.p += 1;
            }
            let days = self.number();
            if days == UNSET {
                return None;
            }
            if julian {
                TransDay::JulianNoFeb29(days)
            } else {
                TransDay::JulianFeb29(days)
            }
        };
        let mut hour = 2 * 3600;
        if self.peek() == b'/' {
            self.p += 1;
            hour = self.offset();
            if hour == UNSET {
                return None;
            }
            hour = -hour;
        }
        Some(TransInfo { day, hour })
    }
}

/// Parses a POSIX TZ string; `None` if it is malformed (timelib returns NULL).
pub fn parse_posix_str(posix: &str) -> Option<PosixStr> {
    let mut r = Reader {
        s: posix.as_bytes(),
        p: 0,
    };
    let std = r.description()?;
    let std_offset = r.offset();
    if std_offset == UNSET {
        return None;
    }
    let mut ps = PosixStr {
        std,
        std_offset,
        dst: None,
        dst_offset: 0,
        dst_begin: None,
        dst_end: None,
        type_index_std: 0,
        type_index_dst: 0,
    };
    if r.peek() == 0 {
        return Some(ps);
    }
    ps.dst_offset = std_offset + 3600;
    ps.dst = Some(r.description()?);
    if r.peek() != b',' && r.peek() != 0 {
        ps.dst_offset = r.offset();
        if ps.dst_offset == UNSET {
            return None;
        }
    }
    if r.peek() != b',' {
        return None;
    }
    r.p += 1;
    ps.dst_begin = Some(r.trans_spec()?);
    if r.peek() != b',' {
        return None;
    }
    r.p += 1;
    ps.dst_end = Some(r.trans_spec()?);
    if r.peek() != 0 {
        return None;
    }
    Some(ps)
}

const MONTH_LENGTHS: [[i64; 12]; 2] = [
    [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31],
    [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31],
];

/// Seconds from the start of `year` to the transition day described by `psi`.
fn calc_transition(psi: &TransInfo, year: i64) -> i64 {
    let leap = usize::from(is_leap(year));
    match psi.day {
        TransDay::JulianNoFeb29(days) => {
            let mut value = days - 1;
            if leap == 1 && days >= 60 {
                value += 1;
            }
            value * 86400
        }
        TransDay::JulianFeb29(days) => days * 86400,
        TransDay::Mwd { month, week, dow } => {
            // Zeller's congruence, with C's truncating arithmetic on `int`.
            let m1 = (month + 9) % 12 + 1;
            let yy0 = if month <= 2 { year - 1 } else { year };
            let yy1 = yy0 / 100;
            let yy2 = yy0 % 100;
            let mut wd = ((26 * m1 - 2) / 10 + 1 + yy2 + yy2 / 4 + yy1 / 4 - 2 * yy1) % 7;
            if wd < 0 {
                wd += 7;
            }
            let mut d = dow - wd;
            if d < 0 {
                d += 7;
            }
            let mi = usize::try_from((month - 1).clamp(0, 11)).unwrap_or(0);
            let mut i = 1;
            while i < week {
                if d + 7 >= MONTH_LENGTHS[leap][mi] {
                    break;
                }
                d += 7;
                i += 1;
            }
            let mut value = d * 86400;
            for len in MONTH_LENGTHS[leap].iter().take(mi) {
                value += len * 86400;
            }
            value
        }
    }
}

fn count_leap_years(y: i64) -> i64 {
    let y = y - 1;
    y / 4 - y / 100 + y / 400
}

/// Unix timestamp of January 1st, 00:00 UTC of `year`.
pub fn ts_at_start_of_year(year: i64) -> i64 {
    let epoch_leap_years = count_leap_years(1970);
    let current_leap_years = count_leap_years(year);
    86400_i64.wrapping_mul(
        (year - 1970)
            .wrapping_mul(365)
            .wrapping_add(current_leap_years)
            .wrapping_sub(epoch_leap_years),
    )
}

/// Transition times (UTC) and type indices for one year, in time order.
pub fn transitions_for_year(ps: &PosixStr, year: i64) -> [(i64, usize); 2] {
    let (Some(begin), Some(end)) = (ps.dst_begin.as_ref(), ps.dst_end.as_ref()) else {
        return [(i64::MIN, ps.type_index_std), (i64::MIN, ps.type_index_std)];
    };
    let year_begin_ts = ts_at_start_of_year(year);
    let trans_begin = year_begin_ts + calc_transition(begin, year) + begin.hour - ps.std_offset;
    let trans_end = year_begin_ts + calc_transition(end, year) + end.hour - ps.dst_offset;
    if trans_begin < trans_end {
        [
            (trans_begin, ps.type_index_dst),
            (trans_end, ps.type_index_std),
        ]
    } else {
        [
            (trans_end, ps.type_index_std),
            (trans_begin, ps.type_index_dst),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_us_eastern() {
        let p = parse_posix_str("EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(p.std, "EST");
        assert_eq!(p.std_offset, -18000);
        assert_eq!(p.dst.as_deref(), Some("EDT"));
        assert_eq!(p.dst_offset, -14400);
        assert_eq!(
            p.dst_begin,
            Some(TransInfo {
                day: TransDay::Mwd {
                    month: 3,
                    week: 2,
                    dow: 0
                },
                hour: 7200
            })
        );
    }

    #[test]
    fn parses_numeric_abbreviations() {
        let p = parse_posix_str("<-03>3").unwrap();
        assert_eq!(p.std, "-03");
        assert_eq!(p.std_offset, -10800);
        assert!(p.dst.is_none());
        let p = parse_posix_str("<+0330>-3:30").unwrap();
        assert_eq!(p.std_offset, 12600);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_posix_str("").is_none());
        assert!(parse_posix_str("EST5EDT").is_none());
        assert!(parse_posix_str("EST5EDT,M3.2.0").is_none());
        assert!(parse_posix_str("EST5EDT,M3.2.0,M11.1.0x").is_none());
    }

    #[test]
    fn computes_us_transitions_2021() {
        let mut p = parse_posix_str("EST5EDT,M3.2.0,M11.1.0").unwrap();
        p.type_index_dst = 1;
        p.type_index_std = 2;
        let t = transitions_for_year(&p, 2021);
        // 2021-03-14 07:00 UTC and 2021-11-07 06:00 UTC
        assert_eq!(t, [(1_615_705_200, 1), (1_636_264_800, 2)]);
    }

    #[test]
    fn start_of_year() {
        assert_eq!(ts_at_start_of_year(1970), 0);
        assert_eq!(ts_at_start_of_year(2000), 946_684_800);
        assert_eq!(ts_at_start_of_year(1969), -31_536_000);
    }
}

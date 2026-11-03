//! `DateTime::createFromFormat()` / `date_parse_from_format()`: a port of
//! timelib's `timelib_parse_from_format`.

use super::code;
use super::strtotime::{
    eat_spaces, get_nr, get_nr_ex, lookup_month, lookup_relunit, meridian_with_check, process_year,
    skip_day_suffix, State,
};
use super::zone::{parse_zone, Cur};
use super::Errors;
use crate::dow::{valid_date, valid_time};
use crate::time::{Time, ZoneType};
use crate::UNSET;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spec {
    AllowExtra,
    AnySeparator,
    Day,
    DayOfYear,
    DaySuffix,
    EpochSeconds,
    Escape,
    Hour12,
    Hour24,
    Literal,
    Meridian,
    Microsecond,
    Millisecond,
    Minute,
    Month,
    RandomChar,
    ResetAll,
    ResetAllWhenNotSet,
    Second,
    Separator,
    SkipToSeparator,
    TextualDay,
    TextualMonth,
    TimezoneOffset,
    Whitespace,
    Year2,
    Year4,
    YearExpanded,
}

fn lookup_format(c: u8) -> Spec {
    match c {
        b'+' => Spec::AllowExtra,
        b'#' => Spec::AnySeparator,
        b'j' | b'd' => Spec::Day,
        b'z' => Spec::DayOfYear,
        b'S' => Spec::DaySuffix,
        b'U' => Spec::EpochSeconds,
        b'\\' => Spec::Escape,
        b'h' | b'g' => Spec::Hour12,
        b'H' | b'G' => Spec::Hour24,
        b'a' | b'A' => Spec::Meridian,
        b'u' => Spec::Microsecond,
        b'v' => Spec::Millisecond,
        b'i' => Spec::Minute,
        b'n' | b'm' => Spec::Month,
        b'?' => Spec::RandomChar,
        b'!' => Spec::ResetAll,
        b'|' => Spec::ResetAllWhenNotSet,
        b's' => Spec::Second,
        b';' | b':' | b'/' | b'.' | b',' | b'-' | b'(' | b')' => Spec::Separator,
        b'*' => Spec::SkipToSeparator,
        b'D' | b'l' => Spec::TextualDay,
        b'M' | b'F' => Spec::TextualMonth,
        b'e' | b'P' | b'p' | b'T' | b'O' => Spec::TimezoneOffset,
        b' ' => Spec::Whitespace,
        b'y' => Spec::Year2,
        b'Y' => Spec::Year4,
        b'x' | b'X' => Spec::YearExpanded,
        _ => Spec::Literal,
    }
}

fn reset_fields(t: &mut Time) {
    t.y = 1970;
    t.m = 1;
    t.d = 1;
    t.h = 0;
    t.i = 0;
    t.s = 0;
    t.us = 0;
    t.tz_info = None;
}

fn reset_unset_fields(t: &mut Time) {
    for (f, v) in [
        (&mut t.y, 1970),
        (&mut t.m, 1),
        (&mut t.d, 1),
        (&mut t.h, 0),
        (&mut t.i, 0),
        (&mut t.s, 0),
        (&mut t.us, 0),
    ] {
        if *f == UNSET {
            *f = v;
        }
    }
}

struct Pbf<'a> {
    st: State<'a>,
    string: &'a [u8],
}

impl Pbf<'_> {
    fn err(&mut self, code: u32, msg: &'static str, pos: usize) {
        let ch = self.string.get(pos).copied().unwrap_or(0);
        self.st.errors.add_error(code, pos, ch, msg);
    }

    fn warn(&mut self, code: u32, msg: &'static str, pos: usize) {
        let ch = self.string.get(pos).copied().unwrap_or(0);
        self.st.errors.add_warning(code, pos, ch, msg);
    }

    fn check_number(&mut self, c: &Cur<'_>, begin: usize) {
        if !c.c().is_ascii_digit() {
            self.err(code::ERR_UNEXPECTED_DATA, "Unexpected data found.", begin);
        }
    }

    fn check_signed_number(&mut self, c: &Cur<'_>, begin: usize) {
        if !(c.c().is_ascii_digit() || c.c() == b'+' || c.c() == b'-') {
            self.err(code::ERR_UNEXPECTED_DATA, "Unexpected data found.", begin);
        }
    }
}

/// `timelib_parse_from_format`: parse `string` according to `format`.
///
/// Returns the (unresolved) time — fields not given by the format are
/// [`UNSET`] — and the errors/warnings, as `date_parse_from_format()` and
/// `DateTime::getLastErrors()` report them.
#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss
)]
pub fn parse_from_format(format: &[u8], string: &[u8]) -> (Time, Errors) {
    let mut p = Pbf {
        st: State {
            input: string,
            tok: None,
            time: Time::new_unset(),
            errors: Errors::default(),
        },
        string,
    };
    let mut allow_extra = false;
    let mut f = Cur::new(format, 0);
    let mut c = Cur::new(string, 0);

    while f.c() != 0 && c.c() != 0 {
        let begin = c.p;
        let fc = f.c();
        match lookup_format(fc) {
            Spec::TextualDay => {
                if let Some(rel) = lookup_relunit(&mut c) {
                    let t = &mut p.st.time;
                    t.have_relative = true;
                    t.relative.have_weekday_relative = true;
                    t.relative.weekday = i32::try_from(rel.multiplier).unwrap_or(0);
                    t.relative.weekday_behavior = 1;
                } else {
                    p.err(
                        code::ERR_NO_TEXTUAL_DAY,
                        "A textual day could not be found",
                        begin,
                    );
                }
            }
            Spec::Day => {
                p.check_number(&c, begin);
                let d = get_nr(&mut c, 2);
                p.st.time.d = d;
                if d == UNSET {
                    p.err(
                        code::ERR_NO_TWO_DIGIT_DAY,
                        "A two digit day could not be found",
                        begin,
                    );
                } else {
                    p.st.time.have_date = 1;
                }
            }
            Spec::DaySuffix => skip_day_suffix(&mut c),
            Spec::DayOfYear => {
                p.check_number(&c, begin);
                if p.st.time.y == UNSET {
                    p.err(
                        code::ERR_MERIDIAN_BEFORE_HOUR,
                        "A 'day of year' can only come after a year has been found",
                        begin,
                    );
                }
                let tmp = get_nr(&mut c, 3);
                if tmp == UNSET {
                    p.err(
                        code::ERR_NO_THREE_DIGIT_DAY_OF_YEAR,
                        "A three digit day-of-year could not be found",
                        begin,
                    );
                } else if p.st.time.y != UNSET {
                    let t = &mut p.st.time;
                    t.have_date = 1;
                    t.m = 1;
                    t.d = tmp + 1;
                    t.do_normalize();
                }
            }
            Spec::Month => {
                p.check_number(&c, begin);
                let m = get_nr(&mut c, 2);
                p.st.time.m = m;
                if m == UNSET {
                    p.err(
                        code::ERR_NO_TWO_DIGIT_MONTH,
                        "A two digit month could not be found",
                        begin,
                    );
                } else {
                    p.st.time.have_date = 1;
                }
            }
            Spec::TextualMonth => {
                let tmp = lookup_month(&mut c);
                if tmp == 0 {
                    p.err(
                        code::ERR_NO_TEXTUAL_MONTH,
                        "A textual month could not be found",
                        begin,
                    );
                } else {
                    p.st.time.have_date = 1;
                    p.st.time.m = tmp;
                }
            }
            Spec::Year2 => {
                p.check_number(&c, begin);
                let (mut y, len) = get_nr_ex(&mut c, 2);
                if y == UNSET {
                    p.st.time.y = y;
                    p.err(
                        code::ERR_NO_TWO_DIGIT_YEAR,
                        "A two digit year could not be found",
                        begin,
                    );
                } else {
                    p.st.time.have_date = 1;
                    process_year(&mut y, len);
                    p.st.time.y = y;
                }
            }
            Spec::Year4 => {
                p.check_number(&c, begin);
                let y = get_nr(&mut c, 4);
                p.st.time.y = y;
                if y == UNSET {
                    p.err(
                        code::ERR_NO_FOUR_DIGIT_YEAR,
                        "A four digit year could not be found",
                        begin,
                    );
                } else {
                    p.st.time.have_date = 1;
                }
            }
            Spec::YearExpanded => {
                p.check_signed_number(&c, begin);
                let y = p.st.get_signed_nr(&mut c, 19);
                p.st.time.y = y;
                if y == UNSET {
                    p.err(
                        code::ERR_NO_FOUR_DIGIT_YEAR,
                        "An expanded digit year could not be found",
                        begin,
                    );
                } else {
                    p.st.time.have_date = 1;
                }
            }
            Spec::Hour12 => {
                p.check_number(&c, begin);
                let h = get_nr(&mut c, 2);
                p.st.time.h = h;
                if h == UNSET {
                    p.err(
                        code::ERR_NO_TWO_DIGIT_HOUR,
                        "A two digit hour could not be found",
                        begin,
                    );
                } else if h > 12 {
                    p.err(
                        code::ERR_HOUR_LARGER_THAN_12,
                        "Hour cannot be higher than 12",
                        begin,
                    );
                } else {
                    p.st.time.have_time = 1;
                }
            }
            Spec::Hour24 => {
                p.check_number(&c, begin);
                let h = get_nr(&mut c, 2);
                p.st.time.h = h;
                if h == UNSET {
                    p.err(
                        code::ERR_NO_TWO_DIGIT_HOUR,
                        "A two digit hour could not be found",
                        begin,
                    );
                } else {
                    p.st.time.have_time = 1;
                }
            }
            Spec::Meridian => {
                if p.st.time.h == UNSET {
                    p.err(
                        code::ERR_MERIDIAN_BEFORE_HOUR,
                        "Meridian can only come after an hour has been found",
                        begin,
                    );
                }
                let tmp = meridian_with_check(&mut c, p.st.time.h);
                if tmp == UNSET {
                    p.err(
                        code::ERR_NO_MERIDIAN,
                        "A meridian could not be found",
                        begin,
                    );
                } else {
                    p.st.time.have_time = 1;
                    if p.st.time.h != UNSET {
                        p.st.time.h += tmp;
                    }
                }
            }
            Spec::Minute | Spec::Second => {
                p.check_number(&c, begin);
                let (v, len) = get_nr_ex(&mut c, 2);
                if v == UNSET || len != 2 {
                    if fc == b'i' {
                        p.err(
                            code::ERR_NO_TWO_DIGIT_MINUTE,
                            "A two digit minute could not be found",
                            begin,
                        );
                    } else {
                        p.err(
                            code::ERR_NO_TWO_DIGIT_SECOND,
                            "A two digit second could not be found",
                            begin,
                        );
                    }
                } else {
                    p.st.time.have_time = 1;
                    if fc == b'i' {
                        p.st.time.i = v;
                    } else {
                        p.st.time.s = v;
                    }
                }
            }
            Spec::Microsecond | Spec::Millisecond => {
                p.check_number(&c, begin);
                let tptr = c.p;
                let (digits, mult) = if fc == b'u' { (6, 1.0) } else { (3, 1000.0) };
                let f = get_nr(&mut c, digits);
                if f == UNSET || c.p - tptr < 1 {
                    if fc == b'u' {
                        p.err(
                            code::ERR_NO_SIX_DIGIT_MICROSECOND,
                            "A six digit microsecond could not be found",
                            begin,
                        );
                    } else {
                        p.err(
                            code::ERR_NO_THREE_DIGIT_MILLISECOND,
                            "A three digit millisecond could not be found",
                            begin,
                        );
                    }
                } else {
                    let exp = digits as f64 - (c.p - tptr) as f64;
                    p.st.time.us = (f as f64 * 10f64.powf(exp) * mult) as i64;
                }
            }
            Spec::Whitespace => eat_spaces(&mut c),
            Spec::EpochSeconds => {
                p.check_signed_number(&c, begin);
                let tmp = p.st.get_signed_nr(&mut c, 24);
                let t = &mut p.st.time;
                t.have_zone = 1;
                t.sse = tmp;
                t.is_localtime = true;
                t.zone_type = ZoneType::Offset;
                t.z = 0;
                t.dst = 0;
                t.update_from_sse();
            }
            Spec::AnySeparator => {
                if lookup_format(c.c()) == Spec::Separator {
                    c.adv();
                } else {
                    p.err(
                        code::ERR_NO_SEP_SYMBOL,
                        "The separation symbol ([;:/.,-]) could not be found",
                        begin,
                    );
                }
            }
            Spec::Separator => {
                if c.c() == fc {
                    c.adv();
                } else {
                    p.err(
                        code::ERR_NO_SEP_SYMBOL,
                        "The separation symbol could not be found",
                        begin,
                    );
                }
            }
            Spec::ResetAll => reset_fields(&mut p.st.time),
            Spec::ResetAllWhenNotSet => reset_unset_fields(&mut p.st.time),
            Spec::RandomChar => c.adv(),
            Spec::Escape => {
                if f.at(1) == 0 {
                    p.err(
                        code::ERR_EXPECTED_ESCAPE_CHAR,
                        "Escaped character expected",
                        begin,
                    );
                } else {
                    f.adv();
                    if c.c() == f.c() {
                        c.adv();
                    } else {
                        p.err(
                            code::ERR_NO_ESCAPED_CHAR,
                            "The escaped character could not be found",
                            begin,
                        );
                    }
                }
            }
            Spec::SkipToSeparator => {
                c.adv();
                while !matches!(
                    c.c(),
                    0 | b' ' | b'\t' | b'.' | b',' | b':' | b';' | b'/' | b'-' | b'0'..=b'9'
                ) {
                    c.adv();
                }
            }
            Spec::AllowExtra => allow_extra = true,
            Spec::TimezoneOffset => {
                let (z, not_found) = parse_zone(&mut c, &mut p.st.time);
                p.st.time.z = z;
                if not_found {
                    p.err(
                        code::ERR_TZID_NOT_FOUND,
                        "The timezone could not be found in the database",
                        begin,
                    );
                } else {
                    p.st.time.have_zone = 1;
                }
            }
            Spec::Literal => {
                if fc != c.c() {
                    p.err(
                        code::ERR_WRONG_FORMAT_SEP,
                        "The format separator does not match",
                        begin,
                    );
                }
                c.adv();
            }
        }
        f.adv();
    }

    let end = c.p;
    if c.c() != 0 {
        if allow_extra {
            p.warn(code::WARN_TRAILING_DATA, "Trailing data", end);
        } else {
            p.err(code::ERR_TRAILING_DATA, "Trailing data", end);
        }
    }

    while f.c() != 0 {
        match lookup_format(f.c()) {
            Spec::ResetAll => reset_fields(&mut p.st.time),
            Spec::ResetAllWhenNotSet => reset_unset_fields(&mut p.st.time),
            Spec::AllowExtra => {}
            _ => {
                p.err(
                    code::ERR_DATA_MISSING,
                    "Not enough data available to satisfy format",
                    end,
                );
                break;
            }
        }
        f.adv();
    }

    let t = &mut p.st.time;
    if t.h != UNSET || t.i != UNSET || t.s != UNSET || t.us != UNSET {
        for v in [&mut t.h, &mut t.i, &mut t.s, &mut t.us] {
            if *v == UNSET {
                *v = 0;
            }
        }
    }

    let t = &p.st.time;
    let bad_time = t.h != UNSET && t.i != UNSET && t.s != UNSET && !valid_time(t.h, t.i, t.s);
    let bad_date = t.y != UNSET && t.m != UNSET && t.d != UNSET && !valid_date(t.y, t.m, t.d);
    if bad_time {
        p.warn(code::WARN_INVALID_TIME, "The parsed time was invalid", end);
    }
    if bad_date {
        p.warn(code::WARN_INVALID_DATE, "The parsed date was invalid", end);
    }
    (p.st.time, p.st.errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pf(f: &str, s: &str) -> (Time, Errors) {
        parse_from_format(f.as_bytes(), s.as_bytes())
    }

    #[test]
    fn basic_formats() {
        let (t, e) = pf("Y-m-d H:i:s", "2009-02-15 15:16:17");
        assert!(e.is_empty());
        assert_eq!((t.y, t.m, t.d, t.h, t.i, t.s), (2009, 2, 15, 15, 16, 17));
        let (t, _) = pf("!d/m/Y", "15/08/2010");
        assert_eq!((t.y, t.m, t.d, t.h), (2010, 8, 15, 0));
        let (t, _) = pf("D, d M Y", "Mon, 15 Aug 2005");
        assert_eq!(t.relative.weekday, 1);
        let (t, _) = pf("U.u", "1234567890.123456");
        assert_eq!((t.y, t.us), (2009, 123_456));
        let (t, _) = pf("g:i a", "3:05 pm");
        assert_eq!((t.h, t.i), (15, 5));
    }

    #[test]
    fn errors() {
        let (_, e) = pf("Y-m-d", "2009-02-15 extra");
        assert_eq!(e.errors[0].message, "Trailing data");
        assert_eq!(e.errors[0].position, 10);
        let (_, e) = pf("Y-m-d H", "2009-02-15");
        assert_eq!(
            e.errors[0].message,
            "Not enough data available to satisfy format"
        );
        let (_, e) = pf("m", "x");
        assert_eq!(e.errors[0].message, "Unexpected data found.");
        assert_eq!(e.errors[1].message, "A two digit month could not be found");
        let (_, e) = pf("h", "13");
        assert_eq!(e.errors[0].message, "Hour cannot be higher than 12");
        let (_, e) = pf("Y-m-d+", "2009-02-15 extra");
        assert!(e.errors.is_empty());
        assert_eq!(e.warnings[0].message, "Trailing data");
        let (_, e) = pf("Y-m-d", "2009-02-30");
        assert_eq!(e.warnings[0].message, "The parsed date was invalid");
    }
}

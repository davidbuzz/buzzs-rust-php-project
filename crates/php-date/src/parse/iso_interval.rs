//! ISO 8601 durations, recurrences and start/end dates (port of timelib's
//! `parse_iso_intervals.re`), used by `new DateInterval()` and
//! `new DatePeriod($isostr)`.

use std::sync::OnceLock;

use super::regex::{parse_defs, parse_expr, Node, Scanner};
use super::strtotime::get_nr;
use super::zone::Cur;
use super::Errors;
use crate::time::{RelTime, Time, ZoneType};
use crate::UNSET;

const DEFS: &str = r##"
any = [\000-\377];
number = [0-9]+;

hour24lz = [01][0-9] | "2"[0-4];
minutelz = [0-5][0-9];
monthlz = "0" [1-9] | "1" [0-2];
monthlzz = "0" [0-9] | "1" [0-2];
daylz   = "0" [1-9] | [1-2][0-9] | "3" [01];
daylzz  = "0" [0-9] | [1-2][0-9] | "3" [01];
secondlz = minutelz;
year4 = [0-9]{4};
weekofyear = "0"[1-9] | [1-4][0-9] | "5"[0-3];

space = [ \t]+;
datetimebasic  = year4 monthlz daylz "T" hour24lz minutelz secondlz "Z";
datetimeextended  = year4 "-" monthlz "-" daylz "T" hour24lz ':' minutelz ':' secondlz "Z";
period   = "P" (number "Y")? (number "M")? (number "W")? (number "D")? ("T" (number "H")? (number "M")? (number "S")?)?;
combinedrep = "P" year4 "-" monthlzz "-" daylzz "T" hour24lz ':' minutelz ':' secondlz;

recurrences = "R" number;
"##;

#[derive(Debug, Clone, Copy)]
enum Rule {
    Recurrences,
    DateTime,
    Period,
    CombinedRep,
    Separator,
    Nul,
    Any,
}

const RULES: &[(&str, Rule)] = &[
    ("recurrences", Rule::Recurrences),
    ("datetimebasic | datetimeextended", Rule::DateTime),
    ("period", Rule::Period),
    ("combinedrep", Rule::CombinedRep),
    ("[ .,\\t/]", Rule::Separator),
    ("\"\\000\"|\"\\n\"", Rule::Nul),
    ("any", Rule::Any),
];

fn scanner() -> &'static Scanner {
    static SCANNER: OnceLock<Scanner> = OnceLock::new();
    SCANNER.get_or_init(|| {
        let defs = parse_defs(DEFS);
        let nodes: Vec<Node> = RULES.iter().map(|(e, _)| parse_expr(e, &defs)).collect();
        Scanner::new(&nodes)
    })
}

/// The result of parsing an ISO 8601 interval string.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IsoInterval {
    /// Start date (`2008-03-01T13:00:00Z`), if given.
    pub begin: Option<Time>,
    /// End date, if given.
    pub end: Option<Time>,
    /// The duration (`P1Y2M`), if given.
    pub period: Option<RelTime>,
    /// The recurrence count (`R5`), if given.
    pub recurrences: Option<i64>,
    /// Parse errors (codes are always 0 in this parser).
    pub errors: Errors,
}

/// `timelib_get_unsigned_nr`.
fn get_unsigned_nr(c: &mut Cur<'_>, max_length: usize) -> i64 {
    while !c.c().is_ascii_digit() && c.c() != b'+' && c.c() != b'-' {
        if c.c() == 0 {
            return UNSET;
        }
        c.adv();
    }
    let mut dir: i64 = 1;
    while c.c() == b'+' || c.c() == b'-' {
        if c.c() == b'-' {
            dir = dir.wrapping_neg();
        }
        c.adv();
    }
    dir.wrapping_mul(get_nr(c, max_length))
}

fn new_time() -> Time {
    Time {
        y: UNSET,
        m: UNSET,
        d: UNSET,
        h: UNSET,
        i: UNSET,
        s: UNSET,
        us: 0,
        z: 0,
        dst: 0,
        is_localtime: false,
        zone_type: ZoneType::Offset,
        ..Time::default()
    }
}

fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `timelib_strtointerval`: parse an ISO 8601 interval specification such
/// as `P1Y2M3DT4H5M6S`, `R4/2012-07-01T00:00:00Z/P7D` or
/// `2008-03-01T13:00:00Z/2008-05-11T15:30:00Z`.
#[allow(clippy::too_many_lines)]
pub fn parse_iso_interval(s: &[u8]) -> IsoInterval {
    let mut out = IsoInterval::default();
    if s.is_empty() {
        out.errors.add_error(0, 0, 0, "Empty string");
        return out;
    }
    // Mirrors the C trimming loops exactly (note `s < e`, not `s <= e`).
    let mut start = 0;
    let mut e = s.len() - 1;
    while is_c_space(s[start]) && start < e {
        start += 1;
    }
    while is_c_space(s[e]) && e > start {
        e -= 1;
    }
    let input = &s[start..=e];

    let mut begin = new_time();
    let mut end = new_time();
    let mut period = RelTime {
        days: UNSET,
        ..RelTime::default()
    };
    let mut recurrences = 1;
    let (mut have_period, mut have_recurrences, mut have_date) = (false, false, false);
    let (mut have_begin_date, mut have_end_date) = (false, false);

    let sc = scanner();
    let mut cursor = 0;
    while cursor <= input.len() {
        let tok_pos = cursor;
        let Some((len, rule)) = sc.longest_match(input, cursor) else {
            break;
        };
        let len = len.max(1);
        let tok = input.get(cursor..(cursor + len).min(input.len())).unwrap_or(&[]);
        cursor += len;
        let mut c = Cur::new(tok, 0);
        let tok_char = input.get(tok_pos).copied().unwrap_or(0);
        match RULES[rule].1 {
            Rule::Recurrences => {
                c.adv();
                recurrences = get_unsigned_nr(&mut c, 9);
                have_recurrences = true;
            }
            Rule::DateTime => {
                let current = if have_date || have_period {
                    have_end_date = true;
                    &mut end
                } else {
                    have_begin_date = true;
                    &mut begin
                };
                current.y = get_nr(&mut c, 4);
                current.m = get_nr(&mut c, 2);
                current.d = get_nr(&mut c, 2);
                current.h = get_nr(&mut c, 2);
                current.i = get_nr(&mut c, 2);
                current.s = get_nr(&mut c, 2);
                have_date = true;
            }
            Rule::Period => {
                c.adv();
                let mut in_time = false;
                loop {
                    if c.c() == b'T' {
                        in_time = true;
                        c.adv();
                    }
                    if c.c() == 0 {
                        out.errors.add_error(0, tok_pos, tok_char, "Missing expected time part");
                        break;
                    }
                    let nr = get_unsigned_nr(&mut c, 12);
                    match c.c() {
                        b'Y' => period.y = nr,
                        b'W' => period.d = period.d.wrapping_add(nr.wrapping_mul(7)),
                        b'D' => period.d = period.d.wrapping_add(nr),
                        b'H' => period.h = nr,
                        b'S' => period.s = nr,
                        b'M' => {
                            if in_time {
                                period.i = nr;
                            } else {
                                period.m = nr;
                            }
                        }
                        _ => out.errors.add_error(0, tok_pos, tok_char, "Undefined period specifier"),
                    }
                    c.adv();
                    if !out.errors.errors.is_empty() || c.c() == 0 {
                        break;
                    }
                }
                have_period = true;
            }
            Rule::CombinedRep => {
                period.y = get_unsigned_nr(&mut c, 4);
                c.adv();
                period.m = get_unsigned_nr(&mut c, 2);
                c.adv();
                period.d = get_unsigned_nr(&mut c, 2);
                c.adv();
                period.h = get_unsigned_nr(&mut c, 2);
                c.adv();
                period.i = get_unsigned_nr(&mut c, 2);
                c.adv();
                period.s = get_unsigned_nr(&mut c, 2);
                have_period = true;
            }
            Rule::Separator | Rule::Nul => {}
            Rule::Any => out.errors.add_error(0, tok_pos, tok_char, "Unexpected character"),
        }
    }

    if have_begin_date {
        out.begin = Some(begin);
    }
    if have_end_date {
        out.end = Some(end);
    }
    if have_period {
        out.period = Some(period);
    }
    if have_recurrences {
        out.recurrences = Some(recurrences);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        let r = parse_iso_interval(b"P1Y2M3DT4H5M6S");
        assert!(r.errors.errors.is_empty());
        let p = r.period.unwrap();
        assert_eq!((p.y, p.m, p.d, p.h, p.i, p.s, p.days), (1, 2, 3, 4, 5, 6, UNSET));
        let p = parse_iso_interval(b"P2W").period.unwrap();
        assert_eq!(p.d, 14);
        let p = parse_iso_interval(b"P0001-02-03T04:05:06").period.unwrap();
        assert_eq!((p.y, p.m, p.d, p.h, p.i, p.s), (1, 2, 3, 4, 5, 6));
    }

    #[test]
    fn periods() {
        let r = parse_iso_interval(b"R4/2012-07-01T00:00:00Z/P7D");
        assert_eq!(r.recurrences, Some(4));
        let b = r.begin.unwrap();
        assert_eq!((b.y, b.m, b.d), (2012, 7, 1));
        assert_eq!(r.period.unwrap().d, 7);
        let r = parse_iso_interval(b"2008-03-01T13:00:00Z/2008-05-11T15:30:00Z");
        assert!(r.begin.is_some() && r.end.is_some() && r.period.is_none());
    }

    #[test]
    fn errors() {
        assert_eq!(parse_iso_interval(b"").errors.errors[0].message, "Empty string");
        assert_eq!(parse_iso_interval(b"PT").errors.errors[0].message, "Missing expected time part");
        assert_eq!(parse_iso_interval(b"X").errors.errors[0].message, "Unexpected character");
    }
}

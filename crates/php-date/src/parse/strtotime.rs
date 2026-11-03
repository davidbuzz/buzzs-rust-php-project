//! The `strtotime()` grammar: a faithful port of timelib's `parse_date.re`
//! scanner and its actions.

use std::sync::OnceLock;

use super::code;
use super::regex::{parse_defs, parse_expr, Node, Scanner};
use super::zone::{parse_zone, Cur};
use super::Errors;
use crate::dow::{daynr_from_weeknr, valid_date, valid_time};
use crate::time::{
    Time, ZoneType, FIRST_DAY_OF_MONTH, LAST_DAY_OF_MONTH, SPECIAL_DAY_OF_WEEK_IN_MONTH,
    SPECIAL_LAST_DAY_OF_WEEK_IN_MONTH, SPECIAL_WEEKDAY,
};
use crate::UNSET;

/// The re2c definitions from `parse_date.re`, verbatim.
const DEFS: &str = r#"
any = [\000-\377];

nbsp = [\302][\240];
nnbsp = [\342][\200][\257];
space = [ \t]+ | nbsp+ | nnbsp+;
frac = "."[0-9]+;

ago = 'ago';

hour24 = [01]?[0-9] | "2"[0-4];
hour24lz = [01][0-9] | "2"[0-4];
hour12 = "0"?[1-9] | "1"[0-2];
minute = [0-5]?[0-9];
minutelz = [0-5][0-9];
second = minute | "60";
secondlz = minutelz | "60";
meridian = ([AaPp] "."? [Mm] "."?) [\000\t ];
tz = "("? [A-Za-z]{1,6} ")"? | [A-Z][a-z]+([_/-][A-Za-z]+)+;
tzcorrection = "GMT"? [+-] ((hour24 (":"? minute)?) | (hour24lz minutelz secondlz) | (hour24lz ":" minutelz ":" secondlz));

daysuf = "st" | "nd" | "rd" | "th";

month = "0"? [0-9] | "1"[0-2];
day   = (([0-2]?[0-9]) | ("3"[01])) daysuf?;
year  = [0-9]{1,4};
year2 = [0-9]{2};
year4 = [0-9]{4};
year4withsign = [+-]? [0-9]{4};
yearx = [+-] [0-9]{5,19};

dayofyear = "00"[1-9] | "0"[1-9][0-9] | [1-2][0-9][0-9] | "3"[0-5][0-9] | "36"[0-6];
weekofyear = "0"[1-9] | [1-4][0-9] | "5"[0-3];

monthlz = "0" [0-9] | "1" [0-2];
daylz   = "0" [0-9] | [1-2][0-9] | "3" [01];

dayfulls = 'sundays' | 'mondays' | 'tuesdays' | 'wednesdays' | 'thursdays' | 'fridays' | 'saturdays';
dayfull = 'sunday' | 'monday' | 'tuesday' | 'wednesday' | 'thursday' | 'friday' | 'saturday';
dayabbr = 'sun' | 'mon' | 'tue' | 'wed' | 'thu' | 'fri' | 'sat' | 'sun';
dayspecial = 'weekday' | 'weekdays';
daytext = dayfulls | dayfull | dayabbr | dayspecial;

monthfull = 'january' | 'february' | 'march' | 'april' | 'may' | 'june' | 'july' | 'august' | 'september' | 'october' | 'november' | 'december';
monthabbr = 'jan' | 'feb' | 'mar' | 'apr' | 'may' | 'jun' | 'jul' | 'aug' | 'sep' | 'sept' | 'oct' | 'nov' | 'dec';
monthroman = "I" | "II" | "III" | "IV" | "V" | "VI" | "VII" | "VIII" | "IX" | "X" | "XI" | "XII";
monthtext = monthfull | monthabbr | monthroman;

timetiny12 = hour12 space? meridian;
timeshort12 = hour12[:.]minutelz space? meridian;
timelong12 = hour12[:.]minute[:.]secondlz space? meridian;

timetiny24 = 't' hour24;
timeshort24 = 't'? hour24[:.]minute;
timelong24 =  't'? hour24[:.]minute[:.]second;
iso8601long =  't'? hour24 [:.] minute [:.] second frac;

iso8601normtz =  't'? hour24 [:.] minute [:.] secondlz space? (tzcorrection | tz);

gnunocolon       = 't'? hour24lz minutelz;
iso8601nocolon   = 't'? hour24lz minutelz secondlz;

americanshort    = month "/" day;
american         = month "/" day "/" year;
iso8601dateslash = year4 "/" monthlz "/" daylz "/"?;
dateslash        = year4 "/" month "/" day;
iso8601date4     = year4withsign "-" monthlz "-" daylz;
iso8601date2     = year2 "-" monthlz "-" daylz;
iso8601datex     = yearx "-" monthlz "-" daylz;
gnudateshorter   = year4 "-" month;
gnudateshort     = year "-" month "-" day;
pointeddate4     = day [.\t-] month [.-] year4;
pointeddate2     = day [.\t] month "." year2;
datefull         = day ([ \t.-])* monthtext ([ \t.-])* year;
datenoday        = monthtext ([ .\t-])* year4;
datenodayrev     = year4 ([ .\t-])* monthtext;
datetextual      = monthtext ([ .\t-])* day [,.stndrh\t ]+ year;
datenoyear       = monthtext ([ .\t-])* day ([,.stndrh\t ]+|[\000]);
datenoyearrev    = day ([ .\t-])* monthtext;
datenocolon      = year4 monthlz daylz;

soap             = year4 "-" monthlz "-" daylz "T" hour24lz ":" minutelz ":" secondlz frac tzcorrection?;
xmlrpc           = year4 monthlz daylz "T" hour24 ":" minutelz ":" secondlz;
xmlrpcnocolon    = year4 monthlz daylz 't' hour24 minutelz secondlz;
wddx             = year4 "-" month "-" day "T" hour24 ":" minute ":" second;
pgydotd          = year4 [.-]? dayofyear;
pgtextshort      = monthabbr "-" daylz "-" year;
pgtextreverse    = year "-" monthabbr "-" daylz;
mssqltime        = hour12 ":" minutelz ":" secondlz [:.] [0-9]+ meridian;
isoweekday       = year4 "-"? "W" weekofyear "-"? [0-7];
isoweek          = year4 "-"? "W" weekofyear;
exif             = year4 ":" monthlz ":" daylz " " hour24lz ":" minutelz ":" secondlz;
firstdayof       = 'first day of';
lastdayof        = 'last day of';
backof           = 'back of ' hour24 (space? meridian)?;
frontof          = 'front of ' hour24 (space? meridian)?;

clf              = day "/" monthabbr "/" year4 ":" hour24lz ":" minutelz ":" secondlz space tzcorrection;

timestamp        = "@" "-"? [0-9]+;
timestampms      = "@" "-"? [0-9]+ "." [0-9]{0,6};

dateshortwithtimeshort12  = datenoyear timeshort12;
dateshortwithtimelong12   = datenoyear timelong12;
dateshortwithtimeshort  = datenoyear timeshort24;
dateshortwithtimelong   = datenoyear timelong24;
dateshortwithtimelongtz = datenoyear iso8601normtz;

reltextnumber = 'first'|'second'|'third'|'fourth'|'fifth'|'sixth'|'seventh'|'eight'|'eighth'|'ninth'|'tenth'|'eleventh'|'twelfth';
reltexttext = 'next'|'last'|'previous'|'this';
reltextunit = 'ms' | 'µs' | (('msec'|'millisecond'|'µsec'|'microsecond'|'usec'|'sec'|'second'|'min'|'minute'|'hour'|'day'|'fortnight'|'forthnight'|'month'|'year') 's'?) | 'weeks' | daytext;

relnumber = ([+-]*[ \t]*[0-9]{1,13});
relative = relnumber space? (reltextunit | 'week' );
relativetext = (reltextnumber|reltexttext) space reltextunit;
relativetextweek = reltexttext space 'week';

weekdayof        = (reltextnumber|reltexttext) space (dayfulls|dayfull|dayabbr) space 'of';
"#;

/// The scanner actions, in re2c rule order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    Yesterday,
    Now,
    Noon,
    MidnightToday,
    Tomorrow,
    Timestamp,
    TimestampMs,
    FirstLastDayOf,
    BackFrontOf,
    WeekdayOf,
    Time12,
    MssqlTime,
    Time24,
    GnuNoColon,
    Iso8601NoColon,
    American,
    IsoDate4,
    IsoDate2,
    IsoDateX,
    GnuDateShorter,
    GnuDateShort,
    DateFull,
    PointedDate4,
    PointedDate2,
    DateNoDay,
    DateNoDayRev,
    DateTextual,
    DateNoYearRev,
    DateNoColon,
    XmlRpc,
    PgYdotD,
    IsoWeekDay,
    IsoWeek,
    PgTextShort,
    PgTextReverse,
    Clf,
    Year4,
    Ago,
    DayText,
    RelativeTextWeek,
    RelativeText,
    MonthText,
    Timezone,
    DateShortWithTime12,
    DateShortWithTime24,
    Relative,
    Separator,
    Space,
    Nul,
    Any,
}

const RULES: &[(&str, Rule)] = &[
    ("'yesterday'", Rule::Yesterday),
    ("'now'", Rule::Now),
    ("'noon'", Rule::Noon),
    ("'midnight' | 'today'", Rule::MidnightToday),
    ("'tomorrow'", Rule::Tomorrow),
    ("timestamp", Rule::Timestamp),
    ("timestampms", Rule::TimestampMs),
    ("firstdayof | lastdayof", Rule::FirstLastDayOf),
    ("backof | frontof", Rule::BackFrontOf),
    ("weekdayof", Rule::WeekdayOf),
    ("timetiny12 | timeshort12 | timelong12", Rule::Time12),
    ("mssqltime", Rule::MssqlTime),
    (
        "timetiny24 | timeshort24 | timelong24 | iso8601long",
        Rule::Time24,
    ),
    ("gnunocolon", Rule::GnuNoColon),
    ("iso8601nocolon", Rule::Iso8601NoColon),
    ("americanshort | american", Rule::American),
    (
        "iso8601date4 | iso8601dateslash | dateslash",
        Rule::IsoDate4,
    ),
    ("iso8601date2", Rule::IsoDate2),
    ("iso8601datex", Rule::IsoDateX),
    ("gnudateshorter", Rule::GnuDateShorter),
    ("gnudateshort", Rule::GnuDateShort),
    ("datefull", Rule::DateFull),
    ("pointeddate4", Rule::PointedDate4),
    ("pointeddate2", Rule::PointedDate2),
    ("datenoday", Rule::DateNoDay),
    ("datenodayrev", Rule::DateNoDayRev),
    ("datetextual | datenoyear", Rule::DateTextual),
    ("datenoyearrev", Rule::DateNoYearRev),
    ("datenocolon", Rule::DateNoColon),
    ("xmlrpc | xmlrpcnocolon | soap | wddx | exif", Rule::XmlRpc),
    ("pgydotd", Rule::PgYdotD),
    ("isoweekday", Rule::IsoWeekDay),
    ("isoweek", Rule::IsoWeek),
    ("pgtextshort", Rule::PgTextShort),
    ("pgtextreverse", Rule::PgTextReverse),
    ("clf", Rule::Clf),
    ("year4", Rule::Year4),
    ("ago", Rule::Ago),
    ("daytext", Rule::DayText),
    ("relativetextweek", Rule::RelativeTextWeek),
    ("relativetext", Rule::RelativeText),
    ("monthfull | monthabbr", Rule::MonthText),
    ("tzcorrection | tz", Rule::Timezone),
    (
        "dateshortwithtimeshort12 | dateshortwithtimelong12",
        Rule::DateShortWithTime12,
    ),
    (
        "dateshortwithtimeshort | dateshortwithtimelong | dateshortwithtimelongtz",
        Rule::DateShortWithTime24,
    ),
    ("relative", Rule::Relative),
    ("[.,]", Rule::Separator),
    ("space", Rule::Space),
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

// ---------------------------------------------------------------------------
// Lookup tables
// ---------------------------------------------------------------------------

/// Relative unit kinds (`TIMELIB_SECOND` ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unit {
    Microsec,
    Second,
    Minute,
    Hour,
    Day,
    Month,
    Year,
    Weekday,
    Special,
}

/// An entry of `timelib_relunit_lookup`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RelUnit {
    pub(crate) name: &'static str,
    pub(crate) unit: Unit,
    pub(crate) multiplier: i64,
}

macro_rules! ru {
    ($n:expr, $u:ident, $m:expr) => {
        RelUnit {
            name: $n,
            unit: Unit::$u,
            multiplier: $m,
        }
    };
}

pub(crate) const RELUNITS: &[RelUnit] = &[
    ru!("ms", Microsec, 1000),
    ru!("msec", Microsec, 1000),
    ru!("msecs", Microsec, 1000),
    ru!("millisecond", Microsec, 1000),
    ru!("milliseconds", Microsec, 1000),
    ru!("µs", Microsec, 1),
    ru!("usec", Microsec, 1),
    ru!("usecs", Microsec, 1),
    ru!("µsec", Microsec, 1),
    ru!("µsecs", Microsec, 1),
    ru!("microsecond", Microsec, 1),
    ru!("microseconds", Microsec, 1),
    ru!("sec", Second, 1),
    ru!("secs", Second, 1),
    ru!("second", Second, 1),
    ru!("seconds", Second, 1),
    ru!("min", Minute, 1),
    ru!("mins", Minute, 1),
    ru!("minute", Minute, 1),
    ru!("minutes", Minute, 1),
    ru!("hour", Hour, 1),
    ru!("hours", Hour, 1),
    ru!("day", Day, 1),
    ru!("days", Day, 1),
    ru!("week", Day, 7),
    ru!("weeks", Day, 7),
    ru!("fortnight", Day, 14),
    ru!("fortnights", Day, 14),
    ru!("forthnight", Day, 14),
    ru!("forthnights", Day, 14),
    ru!("month", Month, 1),
    ru!("months", Month, 1),
    ru!("year", Year, 1),
    ru!("years", Year, 1),
    ru!("mondays", Weekday, 1),
    ru!("monday", Weekday, 1),
    ru!("mon", Weekday, 1),
    ru!("tuesdays", Weekday, 2),
    ru!("tuesday", Weekday, 2),
    ru!("tue", Weekday, 2),
    ru!("wednesdays", Weekday, 3),
    ru!("wednesday", Weekday, 3),
    ru!("wed", Weekday, 3),
    ru!("thursdays", Weekday, 4),
    ru!("thursday", Weekday, 4),
    ru!("thu", Weekday, 4),
    ru!("fridays", Weekday, 5),
    ru!("friday", Weekday, 5),
    ru!("fri", Weekday, 5),
    ru!("saturdays", Weekday, 6),
    ru!("saturday", Weekday, 6),
    ru!("sat", Weekday, 6),
    ru!("sundays", Weekday, 0),
    ru!("sunday", Weekday, 0),
    ru!("sun", Weekday, 0),
    ru!("weekday", Special, 1),
    ru!("weekdays", Special, 1),
];

const RELTEXT: &[(&str, i32, i64)] = &[
    ("first", 0, 1),
    ("next", 0, 1),
    ("second", 0, 2),
    ("third", 0, 3),
    ("fourth", 0, 4),
    ("fifth", 0, 5),
    ("sixth", 0, 6),
    ("seventh", 0, 7),
    ("eight", 0, 8),
    ("eighth", 0, 8),
    ("ninth", 0, 9),
    ("tenth", 0, 10),
    ("eleventh", 0, 11),
    ("twelfth", 0, 12),
    ("last", 0, -1),
    ("previous", 0, -1),
    ("this", 1, 0),
];

const MONTHS: &[(&str, i64)] = &[
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("may", 5),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("sept", 9),
    ("oct", 10),
    ("nov", 11),
    ("dec", 12),
    ("i", 1),
    ("ii", 2),
    ("iii", 3),
    ("iv", 4),
    ("v", 5),
    ("vi", 6),
    ("vii", 7),
    ("viii", 8),
    ("ix", 9),
    ("x", 10),
    ("xi", 11),
    ("xii", 12),
    ("january", 1),
    ("february", 2),
    ("march", 3),
    ("april", 4),
    ("may", 5),
    ("june", 6),
    ("july", 7),
    ("august", 8),
    ("september", 9),
    ("october", 10),
    ("november", 11),
    ("december", 12),
];

fn eq_ci(a: &[u8], b: &str) -> bool {
    a.eq_ignore_ascii_case(b.as_bytes())
}

// ---------------------------------------------------------------------------
// Token helpers (shared with the format parser)
// ---------------------------------------------------------------------------

/// `timelib_meridian`: returns the hour adjustment.
pub(crate) fn meridian(c: &mut Cur<'_>, h: i64) -> i64 {
    while c.c() != 0 && !matches!(c.c(), b'A' | b'a' | b'P' | b'p') {
        c.adv();
    }
    let mut retval = 0;
    if matches!(c.c(), b'a' | b'A') {
        if h == 12 {
            retval = -12;
        }
    } else if h != 12 {
        retval = 12;
    }
    c.adv();
    if c.c() == b'.' {
        c.adv();
    }
    if matches!(c.c(), b'M' | b'm') {
        c.adv();
    }
    if c.c() == b'.' {
        c.adv();
    }
    retval
}

/// `timelib_meridian_with_check`: [`UNSET`] when no valid meridian follows.
pub(crate) fn meridian_with_check(c: &mut Cur<'_>, h: i64) -> i64 {
    while c.c() != 0 && !matches!(c.c(), b'A' | b'a' | b'P' | b'p') {
        c.adv();
    }
    if c.c() == 0 {
        return UNSET;
    }
    let mut retval = 0;
    if matches!(c.c(), b'a' | b'A') {
        if h == 12 {
            retval = -12;
        }
    } else if h != 12 {
        retval = 12;
    }
    c.adv();
    if c.c() == b'.' {
        c.adv();
        if !matches!(c.c(), b'm' | b'M') {
            return UNSET;
        }
        c.adv();
        if c.c() != b'.' {
            return UNSET;
        }
        c.adv();
    } else if matches!(c.c(), b'm' | b'M') {
        c.adv();
    } else {
        return UNSET;
    }
    retval
}

/// `timelib_get_nr_ex`: skip to the first digit and read at most
/// `max_length` digits. Returns (value or [`UNSET`], digits read).
pub(crate) fn get_nr_ex(c: &mut Cur<'_>, max_length: usize) -> (i64, usize) {
    while !c.c().is_ascii_digit() {
        if c.c() == 0 {
            return (UNSET, 0);
        }
        c.adv();
    }
    let begin = c.p;
    let mut len = 0;
    while c.c().is_ascii_digit() && len < max_length {
        c.adv();
        len += 1;
    }
    (strtoll_digits(&c.s[begin..c.p]), c.p - begin)
}

/// `strtoll` of a digit run, saturating like glibc.
fn strtoll_digits(d: &[u8]) -> i64 {
    let mut v: i64 = 0;
    for &b in d {
        v = v.saturating_mul(10).saturating_add(i64::from(b - b'0'));
    }
    v
}

/// `timelib_get_nr`.
pub(crate) fn get_nr(c: &mut Cur<'_>, max_length: usize) -> i64 {
    get_nr_ex(c, max_length).0
}

/// `timelib_skip_day_suffix`.
pub(crate) fn skip_day_suffix(c: &mut Cur<'_>) {
    if matches!(c.c(), b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        return;
    }
    let two = [c.c(), c.at(1)];
    if ["nd", "rd", "st", "th"].iter().any(|s| eq_ci(&two, s)) {
        c.p += 2;
    }
}

/// `strtod` on a byte string: the longest valid decimal prefix.
fn strtod(s: &[u8]) -> f64 {
    let mut end = 0;
    while end < s.len() && s[end].is_ascii_digit() {
        end += 1;
    }
    if end < s.len() && s[end] == b'.' {
        let mut e2 = end + 1;
        while e2 < s.len() && s[e2].is_ascii_digit() {
            e2 += 1;
        }
        if e2 > end + 1 || end > 0 {
            end = e2;
        }
    }
    std::str::from_utf8(&s[..end])
        .ok()
        .and_then(|x| {
            if x.is_empty() || x == "." {
                None
            } else {
                x.parse().ok()
            }
        })
        .unwrap_or(0.0)
}

/// `timelib_get_frac_nr`: microseconds from a `.123` style fraction.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub(crate) fn get_frac_nr(c: &mut Cur<'_>) -> i64 {
    while c.c() != b'.' && c.c() != b':' && !c.c().is_ascii_digit() {
        if c.c() == 0 {
            return UNSET;
        }
        c.adv();
    }
    let begin = c.p;
    while c.c() == b'.' || c.c() == b':' || c.c().is_ascii_digit() {
        c.adv();
    }
    let end = c.p;
    let s = &c.s[begin + 1..end];
    let len = end - begin;
    let v = strtod(s) * 10f64.powf(7.0 - len as f64);
    v as i64
}

/// `timelib_lookup_relunit`.
pub(crate) fn lookup_relunit(c: &mut Cur<'_>) -> Option<&'static RelUnit> {
    let begin = c.p;
    while !matches!(
        c.c(),
        0 | b' ' | b',' | b'\t' | b';' | b':' | b'/' | b'.' | b'-' | b'(' | b')'
    ) {
        c.adv();
    }
    let word = &c.s[begin..c.p];
    RELUNITS.iter().find(|r| eq_ci(word, r.name))
}

/// `timelib_lookup_month` (0 if not found).
pub(crate) fn lookup_month(c: &mut Cur<'_>) -> i64 {
    let begin = c.p;
    while c.c().is_ascii_alphabetic() {
        c.adv();
    }
    let word = &c.s[begin..c.p];
    let mut value = 0;
    for (name, v) in MONTHS {
        if eq_ci(word, name) {
            value = *v;
        }
    }
    value
}

/// `timelib_get_month`.
pub(crate) fn get_month(c: &mut Cur<'_>) -> i64 {
    while matches!(c.c(), b' ' | b'\t' | b'-' | b'.' | b'/') {
        c.adv();
    }
    lookup_month(c)
}

/// `timelib_eat_spaces`: spaces, tabs, NBSP and NNBSP.
pub(crate) fn eat_spaces(c: &mut Cur<'_>) {
    loop {
        if matches!(c.c(), b' ' | b'\t') {
            c.adv();
        } else if c.c() == 0xe2 && c.at(1) == 0x80 && c.at(2) == 0xaf {
            c.p += 3;
        } else if c.c() == 0xc2 && c.at(1) == 0xa0 {
            c.p += 2;
        } else {
            break;
        }
    }
}

fn lookup_relative_text(c: &mut Cur<'_>, behavior: &mut i32) -> i64 {
    let begin = c.p;
    while c.c().is_ascii_alphabetic() {
        c.adv();
    }
    let word = &c.s[begin..c.p];
    let mut value = 0;
    for (name, ty, v) in RELTEXT {
        if eq_ci(word, name) {
            value = *v;
            *behavior = *ty;
        }
    }
    value
}

fn get_relative_text(c: &mut Cur<'_>, behavior: &mut i32) -> i64 {
    while matches!(c.c(), b' ' | b'\t' | b'-' | b'/') {
        c.adv();
    }
    lookup_relative_text(c, behavior)
}

/// `TIMELIB_PROCESS_YEAR`.
pub(crate) fn process_year(y: &mut i64, len: usize) {
    if *y == UNSET || len >= 4 {
        return;
    }
    if *y < 100 {
        if *y < 70 {
            *y += 2000;
        } else {
            *y += 1900;
        }
    }
}

// ---------------------------------------------------------------------------
// The scanner state
// ---------------------------------------------------------------------------

pub(crate) struct State<'a> {
    pub(crate) input: &'a [u8],
    /// Start of the current token; `None` models timelib's NULL `tok`
    /// (errors then report position 0 and character NUL).
    pub(crate) tok: Option<usize>,
    pub(crate) time: Time,
    pub(crate) errors: Errors,
}

impl State<'_> {
    fn tok_pos_char(&self) -> (usize, u8) {
        match self.tok {
            Some(t) => (t, self.input.get(t).copied().unwrap_or(0)),
            None => (0, 0),
        }
    }

    pub(crate) fn add_error(&mut self, code: u32, msg: &'static str) {
        let (pos, ch) = self.tok_pos_char();
        self.errors.add_error(code, pos, ch, msg);
    }

    fn add_warning(&mut self, code: u32, msg: &'static str) {
        let (pos, ch) = self.tok_pos_char();
        self.errors.add_warning(code, pos, ch, msg);
    }

    /// `timelib_get_signed_nr`.
    pub(crate) fn get_signed_nr(&mut self, c: &mut Cur<'_>, max_length: usize) -> i64 {
        while !c.c().is_ascii_digit() && c.c() != b'+' && c.c() != b'-' {
            if c.c() == 0 {
                self.add_error(code::ERR_UNEXPECTED_DATA, "Found unexpected data");
                return 0;
            }
            c.adv();
        }
        let mut negative = false;
        while c.c() == b'+' || c.c() == b'-' {
            if c.c() == b'-' {
                negative = !negative;
            }
            c.adv();
        }
        while !c.c().is_ascii_digit() {
            if c.c() == 0 {
                self.add_error(code::ERR_UNEXPECTED_DATA, "Found unexpected data");
                return 0;
            }
            c.adv();
        }
        let begin = c.p;
        let mut len = 0;
        while c.c().is_ascii_digit() && len < max_length {
            c.adv();
            len += 1;
        }
        let digits = &c.s[begin..c.p];
        // strtoll with range checking.
        let mut v: i128 = 0;
        for &b in digits {
            v = v * 10 + i128::from(b - b'0');
            if v > i128::from(i64::MAX) + 1 {
                break;
            }
        }
        if negative {
            v = -v;
        }
        if let Ok(v) = i64::try_from(v) {
            v
        } else {
            self.add_error(code::ERR_NUMBER_OUT_OF_RANGE, "Number out of range");
            0
        }
    }

    fn add_with_overflow(&mut self, field: Field, amount: i64, multiplier: i64) {
        let r = &mut self.time.relative;
        let e = match field {
            Field::Us => &mut r.us,
            Field::S => &mut r.s,
            Field::I => &mut r.i,
            Field::H => &mut r.h,
            Field::D => &mut r.d,
            Field::M => &mut r.m,
            Field::Y => &mut r.y,
        };
        let (v, overflow) = e.overflowing_add(amount.wrapping_mul(multiplier));
        *e = v;
        if overflow {
            self.add_error(code::ERR_NUMBER_OUT_OF_RANGE, "Number out of range");
        }
    }

    fn unhave_time(&mut self) {
        let t = &mut self.time;
        t.have_time = 0;
        t.h = 0;
        t.i = 0;
        t.s = 0;
        t.us = 0;
    }

    /// `TIMELIB_HAVE_TIME`; `false` means "error added, abort the action".
    fn have_time(&mut self) -> bool {
        if self.time.have_time != 0 {
            self.add_error(code::ERR_DOUBLE_TIME, "Double time specification");
            return false;
        }
        self.time.have_time = 1;
        self.time.h = 0;
        self.time.i = 0;
        self.time.s = 0;
        self.time.us = 0;
        true
    }

    fn have_date(&mut self) -> bool {
        if self.time.have_date != 0 {
            self.add_error(code::ERR_DOUBLE_DATE, "Double date specification");
            return false;
        }
        self.time.have_date = 1;
        true
    }

    fn unhave_date(&mut self) {
        let t = &mut self.time;
        t.have_date = 0;
        t.d = 0;
        t.m = 0;
        t.y = 0;
    }

    fn have_weekday_relative(&mut self) {
        self.time.have_relative = true;
        self.time.relative.have_weekday_relative = true;
    }

    fn have_special_relative(&mut self) {
        self.time.have_relative = true;
        self.time.relative.have_special_relative = true;
    }

    fn have_tz(&mut self) -> bool {
        if self.time.have_zone != 0 {
            if self.time.have_zone > 1 {
                self.add_error(code::ERR_DOUBLE_TZ, "Double timezone specification");
            } else {
                self.add_warning(code::WARN_DOUBLE_TZ, "Double timezone specification");
            }
            self.time.have_zone += 1;
            return false;
        }
        self.time.have_zone += 1;
        true
    }

    /// `timelib_set_relative`.
    fn set_relative(&mut self, c: &mut Cur<'_>, amount: i64, behavior: i32, keep_time: bool) {
        let Some(relunit) = lookup_relunit(c) else {
            return;
        };
        match relunit.unit {
            Unit::Microsec => self.add_with_overflow(Field::Us, amount, relunit.multiplier),
            Unit::Second => self.add_with_overflow(Field::S, amount, relunit.multiplier),
            Unit::Minute => self.add_with_overflow(Field::I, amount, relunit.multiplier),
            Unit::Hour => self.add_with_overflow(Field::H, amount, relunit.multiplier),
            Unit::Day => self.add_with_overflow(Field::D, amount, relunit.multiplier),
            Unit::Month => self.add_with_overflow(Field::M, amount, relunit.multiplier),
            Unit::Year => self.add_with_overflow(Field::Y, amount, relunit.multiplier),
            Unit::Weekday => {
                self.have_weekday_relative();
                if !keep_time {
                    self.unhave_time();
                }
                let r = &mut self.time.relative;
                let add = if amount > 0 { amount - 1 } else { amount }.wrapping_mul(7);
                r.d = r.d.wrapping_add(add);
                r.weekday = i32::try_from(relunit.multiplier).unwrap_or(0);
                r.weekday_behavior = behavior;
            }
            Unit::Special => {
                self.have_special_relative();
                if !keep_time {
                    self.unhave_time();
                }
                self.time.relative.special_type = u32::try_from(relunit.multiplier).unwrap_or(0);
                self.time.relative.special_amount = amount;
            }
        }
    }

    fn zone(&mut self, c: &mut Cur<'_>) {
        let (z, not_found) = parse_zone(c, &mut self.time);
        self.time.z = z;
        if not_found {
            self.add_error(
                code::ERR_TZID_NOT_FOUND,
                "The timezone could not be found in the database",
            );
        }
    }

    #[allow(
        clippy::too_many_lines,
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss
    )]
    fn action(&mut self, rule: Rule, tok: &[u8]) {
        let mut c = Cur::new(tok, 0);
        let c = &mut c;
        match rule {
            Rule::Yesterday => {
                self.time.have_relative = true;
                self.unhave_time();
                self.time.relative.d = -1;
            }
            Rule::Now | Rule::Separator | Rule::Space | Rule::Nul => {}
            Rule::Noon => {
                self.unhave_time();
                self.have_time();
                self.time.h = 12;
            }
            Rule::MidnightToday => self.unhave_time(),
            Rule::Tomorrow => {
                self.time.have_relative = true;
                self.unhave_time();
                self.time.relative.d = 1;
            }
            Rule::Timestamp | Rule::TimestampMs => {
                self.time.have_relative = true;
                self.unhave_date();
                self.unhave_time();
                if !self.have_tz() {
                    return;
                }
                let is_negative = c.at(1) == b'-';
                let i = self.get_signed_nr(c, 24);
                let t = &mut self.time;
                t.y = 1970;
                t.m = 1;
                t.d = 1;
                t.h = 0;
                t.i = 0;
                t.s = 0;
                t.us = 0;
                t.relative.s = t.relative.s.wrapping_add(i);
                if rule == Rule::TimestampMs {
                    let before = c.p;
                    let us = self.get_signed_nr(c, 6);
                    let scaled = (us as f64 * 10f64.powf(7.0 - (c.p - before) as f64)) as i64;
                    self.time.relative.us = if is_negative {
                        scaled.wrapping_neg()
                    } else {
                        scaled
                    };
                }
                let t = &mut self.time;
                t.is_localtime = true;
                t.zone_type = ZoneType::Offset;
                t.z = 0;
                t.dst = 0;
            }
            Rule::FirstLastDayOf => {
                self.time.have_relative = true;
                self.time.relative.first_last_day_of = if matches!(c.c(), b'l' | b'L') {
                    LAST_DAY_OF_MONTH
                } else {
                    FIRST_DAY_OF_MONTH
                };
            }
            Rule::BackFrontOf => {
                self.unhave_time();
                self.have_time();
                if c.c() == b'b' {
                    self.time.h = get_nr(c, 2);
                    self.time.i = 15;
                } else {
                    self.time.h = get_nr(c, 2) - 1;
                    self.time.i = 45;
                }
                if c.c() != 0 {
                    eat_spaces(c);
                    let h = self.time.h;
                    self.time.h += meridian(c, h);
                }
            }
            Rule::WeekdayOf => {
                self.have_special_relative();
                let mut behavior = 0;
                let i = get_relative_text(c, &mut behavior);
                eat_spaces(c);
                if i > 0 {
                    self.time.relative.special_type = SPECIAL_DAY_OF_WEEK_IN_MONTH;
                    self.set_relative(c, i, 1, false);
                } else {
                    self.time.relative.special_type = SPECIAL_LAST_DAY_OF_WEEK_IN_MONTH;
                    self.set_relative(c, i, behavior, false);
                }
            }
            Rule::Time12 => {
                if !self.have_time() {
                    return;
                }
                self.time.h = get_nr(c, 2);
                if matches!(c.c(), b':' | b'.') {
                    self.time.i = get_nr(c, 2);
                    if matches!(c.c(), b':' | b'.') {
                        self.time.s = get_nr(c, 2);
                    }
                }
                eat_spaces(c);
                let h = self.time.h;
                self.time.h += meridian(c, h);
            }
            Rule::MssqlTime => {
                if !self.have_time() {
                    return;
                }
                self.time.h = get_nr(c, 2);
                self.time.i = get_nr(c, 2);
                if matches!(c.c(), b':' | b'.') {
                    self.time.s = get_nr(c, 2);
                    if matches!(c.c(), b':' | b'.') {
                        self.time.us = get_frac_nr(c);
                    }
                }
                eat_spaces(c);
                let h = self.time.h;
                self.time.h += meridian(c, h);
            }
            Rule::Time24 => {
                if !self.have_time() {
                    return;
                }
                self.time.h = get_nr(c, 2);
                if matches!(c.c(), b':' | b'.') {
                    self.time.i = get_nr(c, 2);
                    if matches!(c.c(), b':' | b'.') {
                        self.time.s = get_nr(c, 2);
                        if c.c() == b'.' {
                            self.time.us = get_frac_nr(c);
                        }
                    }
                }
                if c.c() != 0 {
                    self.zone(c);
                }
            }
            Rule::GnuNoColon => match self.time.have_time {
                0 => {
                    self.time.h = get_nr(c, 2);
                    self.time.i = get_nr(c, 2);
                    self.time.s = 0;
                    self.time.have_time += 1;
                }
                1 => {
                    self.time.y = get_nr(c, 4);
                    self.time.have_time += 1;
                }
                _ => self.add_error(code::ERR_DOUBLE_TIME, "Double time specification"),
            },
            Rule::Iso8601NoColon => {
                if !self.have_time() {
                    return;
                }
                self.time.h = get_nr(c, 2);
                self.time.i = get_nr(c, 2);
                self.time.s = get_nr(c, 2);
                if c.c() != 0 {
                    self.zone(c);
                }
            }
            Rule::American => {
                if !self.have_date() {
                    return;
                }
                self.time.m = get_nr(c, 2);
                self.time.d = get_nr(c, 2);
                if c.c() == b'/' {
                    let (mut y, len) = get_nr_ex(c, 4);
                    process_year(&mut y, len);
                    self.time.y = y;
                }
            }
            Rule::IsoDate4 => {
                if !self.have_date() {
                    return;
                }
                self.time.y = self.get_signed_nr(c, 4);
                self.time.m = get_nr(c, 2);
                self.time.d = get_nr(c, 2);
            }
            Rule::IsoDate2 | Rule::GnuDateShort => {
                if !self.have_date() {
                    return;
                }
                let (mut y, len) = get_nr_ex(c, 4);
                self.time.m = get_nr(c, 2);
                self.time.d = get_nr(c, 2);
                process_year(&mut y, len);
                self.time.y = y;
            }
            Rule::IsoDateX => {
                if !self.have_date() {
                    return;
                }
                self.time.y = self.get_signed_nr(c, 19);
                self.time.m = get_nr(c, 2);
                self.time.d = get_nr(c, 2);
            }
            Rule::GnuDateShorter => {
                if !self.have_date() {
                    return;
                }
                let (mut y, len) = get_nr_ex(c, 4);
                self.time.m = get_nr(c, 2);
                self.time.d = 1;
                process_year(&mut y, len);
                self.time.y = y;
            }
            Rule::DateFull => {
                if !self.have_date() {
                    return;
                }
                self.time.d = get_nr(c, 2);
                skip_day_suffix(c);
                self.time.m = get_month(c);
                let (mut y, len) = get_nr_ex(c, 4);
                process_year(&mut y, len);
                self.time.y = y;
            }
            Rule::PointedDate4 => {
                if !self.have_date() {
                    return;
                }
                self.time.d = get_nr(c, 2);
                self.time.m = get_nr(c, 2);
                self.time.y = get_nr(c, 4);
            }
            Rule::PointedDate2 => {
                if !self.have_date() {
                    return;
                }
                self.time.d = get_nr(c, 2);
                self.time.m = get_nr(c, 2);
                let (mut y, len) = get_nr_ex(c, 2);
                process_year(&mut y, len);
                self.time.y = y;
            }
            Rule::DateNoDay => {
                if !self.have_date() {
                    return;
                }
                self.time.m = get_month(c);
                let (mut y, len) = get_nr_ex(c, 4);
                self.time.d = 1;
                process_year(&mut y, len);
                self.time.y = y;
            }
            Rule::DateNoDayRev => {
                if !self.have_date() {
                    return;
                }
                let (mut y, len) = get_nr_ex(c, 4);
                self.time.m = get_month(c);
                self.time.d = 1;
                process_year(&mut y, len);
                self.time.y = y;
            }
            Rule::DateTextual | Rule::PgTextShort => {
                if !self.have_date() {
                    return;
                }
                self.time.m = get_month(c);
                self.time.d = get_nr(c, 2);
                let (mut y, len) = get_nr_ex(c, 4);
                process_year(&mut y, len);
                self.time.y = y;
            }
            Rule::DateNoYearRev => {
                if !self.have_date() {
                    return;
                }
                self.time.d = get_nr(c, 2);
                skip_day_suffix(c);
                self.time.m = get_month(c);
            }
            Rule::DateNoColon => {
                if !self.have_date() {
                    return;
                }
                self.time.y = get_nr(c, 4);
                self.time.m = get_nr(c, 2);
                self.time.d = get_nr(c, 2);
            }
            Rule::XmlRpc => {
                if !self.have_time() || !self.have_date() {
                    return;
                }
                self.time.y = get_nr(c, 4);
                self.time.m = get_nr(c, 2);
                self.time.d = get_nr(c, 2);
                self.time.h = get_nr(c, 2);
                self.time.i = get_nr(c, 2);
                self.time.s = get_nr(c, 2);
                if c.c() == b'.' {
                    self.time.us = get_frac_nr(c);
                    if c.c() != 0 {
                        self.zone(c);
                    }
                }
            }
            Rule::PgYdotD => {
                if !self.have_date() {
                    return;
                }
                let (mut y, len) = get_nr_ex(c, 4);
                self.time.d = get_nr(c, 3);
                self.time.m = 1;
                process_year(&mut y, len);
                self.time.y = y;
            }
            Rule::IsoWeekDay | Rule::IsoWeek => {
                if !self.have_date() {
                    return;
                }
                self.time.have_relative = true;
                self.time.y = get_nr(c, 4);
                let w = get_nr(c, 2);
                let d = if rule == Rule::IsoWeekDay {
                    get_nr(c, 1)
                } else {
                    1
                };
                self.time.m = 1;
                self.time.d = 1;
                self.time.relative.d = daynr_from_weeknr(self.time.y, w, d);
            }
            Rule::PgTextReverse => {
                if !self.have_date() {
                    return;
                }
                let (mut y, len) = get_nr_ex(c, 4);
                self.time.m = get_month(c);
                self.time.d = get_nr(c, 2);
                process_year(&mut y, len);
                self.time.y = y;
            }
            Rule::Clf => {
                if !self.have_time() || !self.have_date() {
                    return;
                }
                self.time.d = get_nr(c, 2);
                self.time.m = get_month(c);
                self.time.y = get_nr(c, 4);
                self.time.h = get_nr(c, 2);
                self.time.i = get_nr(c, 2);
                self.time.s = get_nr(c, 2);
                eat_spaces(c);
                self.zone(c);
            }
            Rule::Year4 => self.time.y = get_nr(c, 4),
            Rule::Ago => {
                let r = &mut self.time.relative;
                r.y = r.y.wrapping_neg();
                r.m = r.m.wrapping_neg();
                r.d = r.d.wrapping_neg();
                r.h = r.h.wrapping_neg();
                r.i = r.i.wrapping_neg();
                r.s = r.s.wrapping_neg();
                r.weekday = -r.weekday;
                if r.weekday == 0 {
                    r.weekday = -7;
                }
                if r.have_special_relative && r.special_type == SPECIAL_WEEKDAY {
                    r.special_amount = r.special_amount.wrapping_neg();
                }
            }
            Rule::DayText => {
                self.have_weekday_relative();
                self.unhave_time();
                if let Some(ru) = lookup_relunit(c) {
                    self.time.relative.weekday = i32::try_from(ru.multiplier).unwrap_or(0);
                }
                if self.time.relative.weekday_behavior != 2 {
                    self.time.relative.weekday_behavior = 1;
                }
            }
            Rule::RelativeTextWeek => {
                self.time.have_relative = true;
                while c.c() != 0 {
                    let start = c.p;
                    let mut behavior = 0;
                    let i = get_relative_text(c, &mut behavior);
                    eat_spaces(c);
                    self.set_relative(c, i, behavior, false);
                    self.time.relative.weekday_behavior = 2;
                    if !self.time.relative.have_weekday_relative {
                        self.have_weekday_relative();
                        self.time.relative.weekday = 1;
                    }
                    if c.p == start {
                        break;
                    }
                }
            }
            Rule::RelativeText => {
                self.time.have_relative = true;
                while c.c() != 0 {
                    let start = c.p;
                    let mut behavior = 0;
                    let i = get_relative_text(c, &mut behavior);
                    eat_spaces(c);
                    self.set_relative(c, i, behavior, false);
                    if c.p == start {
                        break;
                    }
                }
            }
            Rule::MonthText => {
                if !self.have_date() {
                    return;
                }
                self.time.m = lookup_month(c);
            }
            Rule::Timezone => {
                if !self.have_tz() {
                    return;
                }
                eat_spaces(c);
                self.zone(c);
            }
            Rule::DateShortWithTime12 => {
                if !self.have_date() {
                    return;
                }
                self.time.m = get_month(c);
                self.time.d = get_nr(c, 2);
                if !self.have_time() {
                    return;
                }
                self.time.h = get_nr(c, 2);
                self.time.i = get_nr(c, 2);
                if matches!(c.c(), b':' | b'.') {
                    self.time.s = get_nr(c, 2);
                    if c.c() == b'.' {
                        self.time.us = get_frac_nr(c);
                    }
                }
                let h = self.time.h;
                self.time.h += meridian(c, h);
            }
            Rule::DateShortWithTime24 => {
                if !self.have_date() {
                    return;
                }
                self.time.m = get_month(c);
                self.time.d = get_nr(c, 2);
                if !self.have_time() {
                    return;
                }
                self.time.h = get_nr(c, 2);
                self.time.i = get_nr(c, 2);
                if c.c() == b':' {
                    self.time.s = get_nr(c, 2);
                    if c.c() == b'.' {
                        self.time.us = get_frac_nr(c);
                    }
                }
                if c.c() != 0 {
                    self.zone(c);
                }
            }
            Rule::Relative => {
                self.time.have_relative = true;
                while c.c() != 0 {
                    let start = c.p;
                    let i = self.get_signed_nr(c, 24);
                    eat_spaces(c);
                    self.set_relative(c, i, 1, true);
                    if c.p == start {
                        break;
                    }
                }
            }
            Rule::Any => self.add_error(code::ERR_UNEXPECTED_CHARACTER, "Unexpected character"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Field {
    Us,
    S,
    I,
    H,
    D,
    M,
    Y,
}

fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `timelib_strtotime`: parse a free-form date/time string.
///
/// Returns the parsed (unresolved) time with [`UNSET`] for missing fields,
/// and the errors/warnings — exactly what `date_parse()` reports.
pub fn strtotime(s: &[u8]) -> (Time, Errors) {
    let mut start = 0;
    let mut end = s.len();
    while start < end && is_c_space(s[start]) {
        start += 1;
    }
    while end > start && is_c_space(s[end - 1]) {
        end -= 1;
    }
    let mut errors = Errors::default();
    if start == end {
        errors.add_error(code::ERR_EMPTY_STRING, 0, 0, "Empty string");
        let mut t = Time::new_unset();
        t.is_localtime = false;
        t.zone_type = ZoneType::None;
        return (t, errors);
    }
    let input = &s[start..end];
    let mut time = Time::new_unset();
    time.relative.days = UNSET;
    let mut st = State {
        input,
        tok: Some(0),
        time,
        errors,
    };
    let sc = scanner();
    let mut cursor = 0;
    loop {
        st.tok = Some(cursor);
        if cursor > input.len() {
            break;
        }
        let Some((len, rule_idx)) = sc.longest_match(input, cursor) else {
            break;
        };
        let len = len.max(1);
        let tok_end = cursor + len;
        let tok = if cursor < input.len() {
            &input[cursor..tok_end.min(input.len())]
        } else {
            &[][..]
        };
        st.action(RULES[rule_idx].1, tok);
        cursor = tok_end;
    }

    if st.time.have_time != 0 && !valid_time(st.time.h, st.time.i, st.time.s) {
        st.add_warning(code::WARN_INVALID_TIME, "The parsed time was invalid");
    }
    if st.time.have_date != 0 && !valid_date(st.time.y, st.time.m, st.time.d) {
        st.add_warning(code::WARN_INVALID_DATE, "The parsed date was invalid");
    }
    (st.time, st.errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Time {
        strtotime(s.as_bytes()).0
    }

    #[test]
    fn iso_date_time() {
        let t = p("2005-07-14 22:30:41");
        assert_eq!((t.y, t.m, t.d, t.h, t.i, t.s), (2005, 7, 14, 22, 30, 41));
        assert_eq!(t.zone_type, ZoneType::None);
    }

    #[test]
    fn invalid_date_warning_position() {
        let (_, e) = strtotime(b"2008-02-30");
        assert_eq!(e.warnings.len(), 1);
        assert_eq!(e.warnings[0].position, 11);
        assert_eq!(e.warnings[0].message, "The parsed date was invalid");
    }

    #[test]
    fn relative() {
        let t = p("+1 week 2 days");
        assert_eq!(t.relative.d, 9);
        let t = p("next monday");
        assert!(t.relative.have_weekday_relative);
        assert_eq!(t.relative.weekday, 1);
        let t = p("last day of next month");
        assert_eq!(t.relative.first_last_day_of, LAST_DAY_OF_MONTH);
        assert_eq!(t.relative.m, 1);
        let t = p("2 days ago");
        assert_eq!(t.relative.d, -2);
    }

    #[test]
    fn timestamps() {
        let t = p("@1234567890");
        assert_eq!(t.relative.s, 1_234_567_890);
        assert_eq!(t.zone_type, ZoneType::Offset);
        let t = p("@-1.5");
        assert_eq!((t.relative.s, t.relative.us), (-1, -500_000));
    }

    #[test]
    fn errors() {
        let (_, e) = strtotime(b"");
        assert_eq!(e.errors[0].message, "Empty string");
        let (_, e) = strtotime(b"foo");
        assert_eq!(
            e.errors[0].message,
            "The timezone could not be found in the database"
        );
        let (_, e) = strtotime(b"10:00 11:00");
        assert_eq!(e.errors[0].message, "Double time specification");
        assert_eq!(e.errors[0].position, 6);
    }

    #[test]
    fn zones() {
        let t = p("2005-07-14 22:30:41 CEST");
        assert_eq!((t.zone_type, t.z, t.dst), (ZoneType::Abbr, 3600, 1));
        let t = p("2005-07-14T22:30:41+02:00");
        assert_eq!((t.zone_type, t.z), (ZoneType::Offset, 7200));
        let t = p("2005-07-14 Europe/Amsterdam");
        assert_eq!(t.zone_type, ZoneType::Id);
    }
}

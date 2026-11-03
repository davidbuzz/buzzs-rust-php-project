//! Expectations taken from `ext/calendar/tests/*.phpt` (php-src master).

use super::*;

fn err(r: Result<impl std::fmt::Debug, CalError>, func: &str) -> String {
    r.expect_err("expected error").message(func)
}

#[test]
fn gh19371_argument_ranges() {
    assert_eq!(
        err(
            cal_days_in_month(CAL_GREGORIAN, 12, i64::MAX),
            "cal_days_in_month"
        ),
        "cal_days_in_month(): Argument #3 ($year) must be less than 2147483646"
    );
    assert_eq!(
        err(
            cal_days_in_month(CAL_GREGORIAN, i64::MIN, 1),
            "cal_days_in_month"
        ),
        "cal_days_in_month(): Argument #2 ($month) must be between 1 and 2147483646"
    );
    assert_eq!(
        err(cal_to_jd(CAL_GREGORIAN, 1, i64::MIN, 1), "cal_to_jd"),
        "cal_to_jd(): Argument #3 ($day) must be between -2147483648 and 2147483647"
    );
    assert_eq!(
        err(cal_to_jd(CAL_GREGORIAN, 1, 1, i64::MAX), "cal_to_jd"),
        "cal_to_jd(): Argument #4 ($year) must be less than 2147483646"
    );
}

#[test]
fn out_of_int_range() {
    let bias = 1_i64 << 32;
    assert_eq!(gregoriantojd(1, 1, 1), 1_721_426);
    assert_eq!(gregoriantojd(1 + bias, 1, 1), 0);
    assert_eq!(gregoriantojd(1, 1 + bias, 1), 0);
    assert_eq!(gregoriantojd(1, 1, 1 + bias), 0);
    assert_eq!(juliantojd(1, 1, 1), 1_721_424);
    assert_eq!(juliantojd(1, 1, 1 + bias), 0);
    assert_eq!(jewishtojd(1, 1, 1), Ok(347_998));
    assert_eq!(jewishtojd(1 + bias, 1, 1), Ok(0));
    assert_eq!(jewishtojd(1, 1 + bias, 1), Ok(0));
    assert_eq!(
        err(jewishtojd(1, 1, 1 + bias), "jewishtojd"),
        "jewishtojd(): Argument #3 ($year) must be between -2147483648 and 2147483647"
    );
    assert_eq!(frenchtojd(1, 1, 1), 2_375_840);
    assert_eq!(frenchtojd(1, 1, 1 + bias), 0);
    assert_eq!(cal_to_jd(CAL_GREGORIAN, 1, 1, 1 - bias), Ok(0));
    assert_eq!(
        err(cal_days_in_month(CAL_GREGORIAN, 1, 1 - bias), "x"),
        "Invalid date"
    );
}

#[test]
fn gh22602_and_overflow() {
    assert_eq!(gregoriantojd(5, 5, 2_147_483_647), 0);
    assert_eq!(gregoriantojd(5, 5, -2_147_483_648), 0);
    assert_eq!(juliantojd(5, 5, 2_147_483_647), 0);
    assert_eq!(juliantojd(5, 5, -2_147_483_647), 0);
    assert_eq!(gregoriantojd(5, 5, 6_000_000), 2_193_176_185);
    assert_eq!(juliantojd(5, 5, 2_147_478_847), 784_368_370_049);
    // gh16834 / bug55797
    assert_eq!(
        cal_from_jd(0o76_545_676_543_223, CAL_GREGORIAN)
            .unwrap()
            .date,
        "0/0/0"
    );
    let r = cal_from_jd(9_223_372_036_854_743_639, CAL_GREGORIAN).unwrap();
    assert_eq!(
        (r.date.as_str(), r.monthname, r.abbrevmonth),
        ("0/0/0", "", "")
    );
    // gh16258, gh16234: must not panic.
    jddayofweek(i64::MAX, 1);
    jddayofweek(i64::MIN, 1);
    let _ = jewishtojd(1_218_182_888, 1, 1_218_182_888);
    let _ = jewishtojd(10, 6, 2_147_483_647);
    assert!(jewishtojd(10, 6, i64::MIN).is_err());
    for jd in [i64::MIN, i64::MAX, -1, 0] {
        for c in 0..4 {
            let _ = cal_from_jd(jd, c);
        }
        for m in 0..6 {
            jdmonthname(jd, m);
        }
    }
}

#[test]
fn jdtojewish64() {
    assert_eq!(jdtojewish(38_245_310, false, 0).unwrap(), b"2/22/103759");
    assert_eq!(jdtojewish(324_542_846, false, 0).unwrap(), b"12/13/887605");
    assert_eq!(jdtojewish(324_542_847, false, 0).unwrap(), b"0/0/0");
    assert_eq!(
        jdtojewish(9_223_372_036_854_743_639, false, 0).unwrap(),
        b"0/0/0"
    );
    assert_eq!(
        err(jdtojewish(1, true, 0), "jdtojewish"),
        "Year out of range (0-9999)"
    );
}

#[test]
fn bug71894() {
    let r = cal_from_jd(347_997, CAL_JEWISH).unwrap();
    assert_eq!(r.dow, None);
    assert_eq!((r.dayname, r.abbrevdayname, r.monthname), ("", "", ""));
    assert_eq!(jdmonthname(347_997, CAL_MONTH_JEWISH), "");
}

#[test]
fn bug54254_and_67976() {
    let expected = [
        30, 30, 30, 29, 30, 30, 29, 30, 29, 30, 29, 30, 29, 30, 29, 30, 29, 30, 0, 29, 30, 29, 30,
        29, 30, 29,
    ];
    for (i, e) in expected.iter().enumerate() {
        let (year, month) = (
            5771 + i64::try_from(i / 13).unwrap(),
            i64::try_from(i % 13).unwrap() + 1,
        );
        assert_eq!(
            cal_days_in_month(CAL_JEWISH, month, year),
            Ok(*e),
            "{month}/{year}"
        );
    }
    assert_eq!(cal_days_in_month(CAL_FRENCH, 13, 14), Ok(5));
}

#[test]
fn hebrew_month_names() {
    let mut lines = Vec::new();
    for year in 5000..=5001 {
        for month in 1..=13 {
            let jd = jewishtojd(month, 1, year).unwrap();
            let h = jdtojewish(jd, true, 0).unwrap();
            lines.push(h.iter().fold(String::new(), |mut acc, b| {
                let _ = std::fmt::Write::write_fmt(&mut acc, format_args!("{b:02x}"));
                acc
            }));
        }
    }
    assert_eq!(lines[0], "e020faf9f8e920e4");
    assert_eq!(lines[5], "e020e0e3f820e02720e4");
    assert_eq!(lines[6], "e020e0e3f820e12720e4");
    assert_eq!(lines[13], "e020faf9f8e920e4e0");
    assert_eq!(lines[18], "e020e0e3f820e4e0");
    assert_eq!(lines[25], "e020e0ece5ec20e4e0");
}

#[test]
fn hebrew_flags() {
    assert_eq!(
        heb_number_to_chars(5771, 0).unwrap(),
        b"\xE4\xFA\xF9\xF2\xE0"
    );
    assert_eq!(
        heb_number_to_chars(5771, CAL_JEWISH_ADD_GERESHAYIM).unwrap(),
        b"\xE4\xFA\xF9\xF2\"\xE0"
    );
    assert_eq!(
        heb_number_to_chars(5771, CAL_JEWISH_ADD_ALAFIM_GERESH | CAL_JEWISH_ADD_ALAFIM).unwrap(),
        b"\xE4' \xE0\xEC\xF4\xE9\xED \xFA\xF9\xF2\xE0"
    );
    assert_eq!(heb_number_to_chars(15, 0).unwrap(), b"\xE8\xE5");
    assert_eq!(heb_number_to_chars(16, 0).unwrap(), b"\xE8\xE6");
    assert_eq!(
        heb_number_to_chars(1, CAL_JEWISH_ADD_GERESHAYIM).unwrap(),
        b"\xE0'"
    );
    assert_eq!(
        heb_number_to_chars(5000, CAL_JEWISH_ADD_GERESHAYIM).unwrap(),
        b"\xE4"
    );
    assert_eq!(heb_number_to_chars(0, 0), None);
    assert_eq!(heb_number_to_chars(10_000, 0), None);
}

#[test]
fn easter() {
    assert_eq!(easter_days(1999, 0), Ok(14));
    assert_eq!(easter_days(1492, 0), Ok(32));
    assert_eq!(easter_days(1913, 0), Ok(2));
    let ymd = |y| {
        let (m, d) = easter::easter_month_day(y, 0);
        format!("{y}-{m:02}-{d:02}")
    };
    assert_eq!(ymd(2000), "2000-04-23");
    assert_eq!(ymd(2001), "2001-04-15");
    assert_eq!(ymd(2002), "2002-03-31");
    assert_eq!(ymd(2045), "2045-04-09");
    assert_eq!(ymd(2046), "2046-03-25");
    assert_eq!(ymd(2047), "2047-04-14");
    assert_eq!(
        err(easter_date(1492, 0, |_, _, _| 0), "easter_date"),
        "easter_date(): Argument #1 ($year) must be a year after 1970 (inclusive)"
    );
    assert_eq!(
        err(easter_date(293_274_701_009, 0, |_, _, _| 0), "easter_date"),
        "easter_date(): Argument #1 ($year) must be a year before 2.000.000.000 (inclusive)"
    );
    let max = "must be between 1 and 7378697629483820644";
    assert_eq!(
        err(easter_days(i64::MAX, 0), "easter_days"),
        format!("easter_days(): Argument #1 ($year) {max}")
    );
    assert_eq!(
        err(easter_days(-1, 0), "easter_days"),
        format!("easter_days(): Argument #1 ($year) {max}")
    );
    assert_eq!(
        err(easter_date(i64::MAX, 0, |_, _, _| 0), "easter_date"),
        format!("easter_date(): Argument #1 ($year) {max}")
    );
    // Must not panic at the upper bound.
    let _ = easter_days(easter::MAX_YEAR, 3);
    let _ = easter_days(easter::MAX_YEAR, 0);
    assert_eq!(
        easter_date(2000, 0, |y, m, d| y * 10_000 + m * 100 + d),
        Ok(20_000_423)
    );
}

#[test]
fn unix() {
    assert_eq!(jdtounix(2_465_712), Ok(2_170_713_600));
    assert_eq!(
        jdtounix(i64::MAX / 86400 + 2_440_588),
        Ok(9_223_372_036_854_720_000)
    );
    let msg =
        "jdtounix(): Argument #1 ($julian_day) jday must be between 2440588 and 106751993607888";
    assert_eq!(err(jdtounix(i64::MAX / 86400 + 2_440_589), "jdtounix"), msg);
    assert_eq!(err(jdtounix(i64::MIN), "jdtounix"), msg);
    assert_eq!(err(jdtounix(240_587), "jdtounix"), msg);
    assert_eq!(
        err(unixtojd(-1, |_| None), "unixtojd"),
        "unixtojd(): Argument #1 ($timestamp) must be greater than or equal to 0"
    );
    assert_eq!(unixtojd(40000, |_| Some((1970, 1, 1))), Ok(Some(2_440_588)));
    assert_eq!(unixtojd(40000, |_| None), Ok(None));
}

#[test]
fn info() {
    let all = cal_info(-1).unwrap();
    assert_eq!(all.len(), 4);
    assert_eq!(all[2].months[5], (6, "Adar I"));
    assert_eq!(all[3].abbrevmonths.len(), 13);
    assert_eq!(
        err(cal_info(99999), "cal_info"),
        "cal_info(): Argument #1 ($calendar) must be a valid calendar ID"
    );
    assert_eq!(
        err(cal_from_jd(1, 9), "cal_from_jd"),
        "cal_from_jd(): Argument #2 ($calendar) must be a valid calendar ID"
    );
}

#[test]
fn day_names() {
    assert_eq!(jddayofweek(2_440_588, CAL_DOW_DAYNO), DayOfWeek::Number(4));
    assert_eq!(
        jddayofweek(2_440_588, CAL_DOW_LONG),
        DayOfWeek::Name("Thursday")
    );
    assert_eq!(
        jddayofweek(2_440_588, CAL_DOW_SHORT),
        DayOfWeek::Name("Thu")
    );
    assert_eq!(jddayofweek(2_440_588, 99), DayOfWeek::Number(4));
    assert_eq!(jdmonthname(2_440_588, CAL_MONTH_GREGORIAN_LONG), "January");
    assert_eq!(jdmonthname(2_440_588, CAL_MONTH_JULIAN_SHORT), "Dec");
    assert_eq!(jdmonthname(2_375_840, CAL_MONTH_FRENCH), "Vendemiaire");
}

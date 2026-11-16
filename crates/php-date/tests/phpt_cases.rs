// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Hand-ported expectations from php-src `ext/date/tests/*.phpt`.

#![allow(
    clippy::many_single_char_names,
    clippy::too_many_lines,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use std::fmt::Write as _;

use php_date::api::{self, Value};
use php_date::parse::TimezoneSpec;
use php_date::tz;

fn show_transitions(tzid: &str, from: i64) -> String {
    // PHP: $from + ((2.5 * 366) * 24 * 60 * 60)
    let to = from + 79_056_000;
    let z = TimezoneSpec::Id(tz::get(tzid).unwrap());
    let Some(Value::Array(items)) = z.transitions(from, to) else {
        panic!("no transitions");
    };
    let mut out = String::new();
    for (_, t) in items {
        let ts = match t.get("ts") {
            Some(Value::Int(v)) => *v,
            _ => 0,
        };
        let time = match t.get("time") {
            Some(Value::Str(s)) => s.clone(),
            _ => String::new(),
        };
        let off = match t.get("offset") {
            Some(Value::Int(v)) => *v,
            _ => 0,
        };
        let dst = matches!(t.get("isdst"), Some(Value::Bool(true)));
        let abbr = match t.get("abbr") {
            Some(Value::Str(s)) => s.clone(),
            _ => String::new(),
        };
        let _ = writeln!(
            out,
            "{ts:12} {time} {off:6} {} {abbr}",
            if dst { "DST" } else { " x " }
        );
    }
    out
}

#[test]
fn gh7752_get_transitions() {
    assert_eq!(
        show_transitions("Europe/London", 1_648_342_200),
        "  1648342200 2022-03-27T00:50:00+00:00      0  x  GMT
  1648342800 2022-03-27T01:00:00+00:00   3600 DST BST
  1667091600 2022-10-30T01:00:00+00:00      0  x  GMT
  1679792400 2023-03-26T01:00:00+00:00   3600 DST BST
  1698541200 2023-10-29T01:00:00+00:00      0  x  GMT
  1711846800 2024-03-31T01:00:00+00:00   3600 DST BST
"
    );
    assert_eq!(
        show_transitions("America/Los_Angeles", 1_648_557_596),
        "  1648557596 2022-03-29T12:39:56+00:00 -25200 DST PDT
  1667725200 2022-11-06T09:00:00+00:00 -28800  x  PST
  1678615200 2023-03-12T10:00:00+00:00 -25200 DST PDT
  1699174800 2023-11-05T09:00:00+00:00 -28800  x  PST
  1710064800 2024-03-10T10:00:00+00:00 -25200 DST PDT
"
    );
    assert_eq!(
        show_transitions("America/Chicago", 1_293_861_600),
        "  1293861600 2011-01-01T06:00:00+00:00 -21600  x  CST
  1300003200 2011-03-13T08:00:00+00:00 -18000 DST CDT
  1320562800 2011-11-06T07:00:00+00:00 -21600  x  CST
  1331452800 2012-03-11T08:00:00+00:00 -18000 DST CDT
  1352012400 2012-11-04T07:00:00+00:00 -21600  x  CST
  1362902400 2013-03-10T08:00:00+00:00 -18000 DST CDT
"
    );
    assert_eq!(
        show_transitions("Europe/Paris", 1_645_095_600),
        "  1645095600 2022-02-17T11:00:00+00:00   3600  x  CET
  1648342800 2022-03-27T01:00:00+00:00   7200 DST CEST
  1667091600 2022-10-30T01:00:00+00:00   3600  x  CET
  1679792400 2023-03-26T01:00:00+00:00   7200 DST CEST
  1698541200 2023-10-29T01:00:00+00:00   3600  x  CET
  1711846800 2024-03-31T01:00:00+00:00   7200 DST CEST
"
    );
}

#[test]
fn get_transitions_basic1() {
    let z = TimezoneSpec::Id(tz::get("Europe/London").unwrap());
    let Some(Value::Array(items)) = z.transitions(-306_972_000, -37_241_999) else {
        panic!()
    };
    assert_eq!(items.len(), 18);
    let t = &items[6].1;
    assert_eq!(t.get("ts"), Some(&Value::Int(-213_228_000)));
    assert_eq!(
        t.get("time"),
        Some(&Value::Str("1963-03-31T02:00:00+00:00".into()))
    );
    assert_eq!(t.get("offset"), Some(&Value::Int(3600)));
    assert_eq!(t.get("isdst"), Some(&Value::Bool(true)));
    assert_eq!(t.get("abbr"), Some(&Value::Str("BST".into())));
}

#[test]
fn date_sun_info_001_002() {
    let utc = tz::get("UTC").unwrap();
    let v = api::date_sun_info(1_165_881_600, 31.7667, 35.2333, &utc);
    let want = [
        ("sunrise", 1_165_897_761),
        ("sunset", 1_165_934_160),
        ("transit", 1_165_915_961),
        ("civil_twilight_begin", 1_165_896_156),
        ("civil_twilight_end", 1_165_935_765),
        ("nautical_twilight_begin", 1_165_894_334),
        ("nautical_twilight_end", 1_165_937_588),
        ("astronomical_twilight_begin", 1_165_892_551),
        ("astronomical_twilight_end", 1_165_939_371),
    ];
    for (k, ts) in want {
        assert_eq!(v.get(k), Some(&Value::Int(ts)), "{k}");
    }
    let oslo = tz::get("Europe/Oslo").unwrap();
    let ts = api::strtotime(b"2007-04-13 08:31:15 UTC", 0, &oslo).unwrap();
    let v = api::date_sun_info(ts, 59.21, 9.61, &oslo);
    let Value::Array(items) = v else { panic!() };
    let got: Vec<String> = items
        .iter()
        .map(|(k, v)| {
            let Value::Int(ts) = v else { panic!() };
            let k = match k {
                api::Key::Str(s) => s.clone(),
                api::Key::Int(i) => i.to_string(),
            };
            format!(
                "{} {k}",
                String::from_utf8_lossy(&api::date(b"Y-m-d H:i:s T", *ts, &oslo))
            )
        })
        .collect();
    assert_eq!(
        got.join("\n"),
        "2007-04-13 06:13:31 CEST sunrise
2007-04-13 20:30:51 CEST sunset
2007-04-13 13:22:11 CEST transit
2007-04-13 05:29:22 CEST civil_twilight_begin
2007-04-13 21:15:00 CEST civil_twilight_end
2007-04-13 04:31:43 CEST nautical_twilight_begin
2007-04-13 22:12:39 CEST nautical_twilight_end
2007-04-13 03:17:01 CEST astronomical_twilight_begin
2007-04-13 23:27:21 CEST astronomical_twilight_end"
    );
}

#[test]
fn date_sunrise_variation7() {
    let inputs: [(&str, f64, f64, f64); 9] = [
        ("Pacific/Samoa", -14.24, -170.72, -11.0),
        ("US/Alaska", 61.0, -150.0, -9.0),
        ("America/Chicago", 41.85, -87.65, -5.0),
        ("America/Montevideo", -34.88, -56.18, -3.0),
        ("Africa/Casablanca", 33.65, -7.58, 0.0),
        ("Europe/Moscow", 55.75, 37.58, 4.0),
        ("Asia/Hong_Kong", 22.28, 114.15, 8.0),
        ("Australia/Brisbane", -27.46, 153.2, 10.0),
        ("Pacific/Wallis", -13.3, -176.16, 12.0),
    ];
    let expected = [
        "06:42", "06:41", "05:07", "05:09", "05:58", "05:59", "07:31", "07:30", "05:52", "05:53",
        "05:59", "06:01", "06:01", "06:02", "06:23", "06:22", "06:03", "06:02",
    ];
    let mut got = Vec::new();
    for (zone, lat, lon, gmt) in inputs {
        let z = tz::get(zone).unwrap();
        for day in [11, 12] {
            let t = api::mktime(
                8,
                Some(8),
                Some(8),
                Some(8),
                Some(day),
                Some(2008),
                0,
                Some(&z),
            );
            let v = api::date_sunrise_sunset(
                t,
                api::SUNFUNCS_RET_STRING,
                lat,
                lon,
                90.0,
                Some(gmt),
                &z,
                false,
            );
            let Value::Str(s) = v else { panic!("{v:?}") };
            got.push(s);
        }
    }
    assert_eq!(got, expected);
}

#[test]
fn date_sunset_variation8_poles() {
    let z = tz::get("Africa/Casablanca").unwrap();
    let t = api::mktime(
        8,
        Some(8),
        Some(8),
        Some(8),
        Some(12),
        Some(2008),
        0,
        Some(&z),
    );
    for fmt in [
        api::SUNFUNCS_RET_STRING,
        api::SUNFUNCS_RET_DOUBLE,
        api::SUNFUNCS_RET_TIMESTAMP,
    ] {
        for lat in [90.0, -90.0] {
            let v = api::date_sunrise_sunset(t, fmt, lat, 0.0, 90.833_333, None, &z, true);
            assert_eq!(v, Value::Bool(false));
        }
    }
}

#[test]
fn timezone_location_get() {
    let z = TimezoneSpec::Id(tz::get("Europe/Oslo").unwrap());
    let v = z.location().unwrap();
    assert_eq!(v.get("country_code"), Some(&Value::Str("NO".into())));
    let Some(Value::Float(lat)) = v.get("latitude") else {
        panic!()
    };
    assert!((59.9166..59.9167).contains(lat), "{lat}");
    assert_eq!(v.get("longitude"), Some(&Value::Float(10.75)));
    assert_eq!(v.get("comments"), Some(&Value::Str(String::new())));
    assert!(TimezoneSpec::Offset(3600).location().is_none());
}

#[test]
fn timezone_abbreviations_list_basic1() {
    let Value::Array(abbr) = api::timezone_abbreviations_list() else {
        panic!()
    };
    assert_eq!(abbr.len(), 144);
    let acst = api::timezone_abbreviations_list();
    let Some(Value::Array(entries)) = acst.get("acst") else {
        panic!()
    };
    assert_eq!(entries.len(), 6);
    let first = &entries[0].1;
    assert_eq!(first.get("dst"), Some(&Value::Bool(false)));
    assert_eq!(first.get("offset"), Some(&Value::Int(34200)));
    assert_eq!(
        first.get("timezone_id"),
        Some(&Value::Str("Australia/Adelaide".into()))
    );
}

#[test]
fn timezones_list() {
    let a = tz::identifiers_list(tz::group::ALL, None).unwrap();
    let b = tz::identifiers_list(tz::group::AMERICA, None).unwrap();
    let c = tz::identifiers_list(tz::group::ALL_WITH_BC, None).unwrap();
    let d = tz::identifiers_list(tz::group::EUROPE | tz::group::UTC, None).unwrap();
    let found = |l: &[&str], z: &str| l.contains(&z);
    let got: Vec<[bool; 4]> = ["Europe/Oslo", "America/New_York", "UTC", "US/Eastern"]
        .iter()
        .map(|z| [found(&a, z), found(&b, z), found(&c, z), found(&d, z)])
        .collect();
    assert_eq!(
        got,
        vec![
            [true, false, true, true],
            [true, true, true, false],
            [true, false, true, true],
            [false, false, true, false],
        ]
    );
    assert!(!tz::is_valid_id("AAA"));
    assert!(tz::is_valid_id("Africa/Abidjan"));
    assert!(tz::is_valid_id("Zulu"));
}

#[test]
fn interval_exception_messages() {
    let iso =
        |s: &str| String::from_utf8(api::interval_from_iso(s.as_bytes()).unwrap_err()).unwrap();
    assert_eq!(
        iso("2007-05-11T15:30:00Z"),
        "Failed to parse interval (2007-05-11T15:30:00Z)"
    );
    assert_eq!(
        iso("2007-05-11T15:30:00Z/"),
        "Failed to parse interval (2007-05-11T15:30:00Z/)"
    );
    assert_eq!(iso(""), "Unknown or bad format ()");
    assert_eq!(
        iso("2007-05-11T15:30:00Z/:00Z"),
        "Unknown or bad format (2007-05-11T15:30:00Z/:00Z)"
    );
    assert_eq!(iso("blah"), "Unknown or bad format (blah)");
    assert_eq!(iso("P3\"D"), "Unknown or bad format (P3\"D)");
    let ds = |s: &str| match api::interval_from_date_string(s.as_bytes()).unwrap_err() {
        api::DateStringIntervalError::BadFormat(m)
        | api::DateStringIntervalError::NonRelative(m) => String::from_utf8(m).unwrap(),
    };
    assert_eq!(
        ds("+5 hours noon"),
        "String '+5 hours noon' contains non-relative elements"
    );
    assert_eq!(
        ds("+72 seconds UTC"),
        "String '+72 seconds UTC' contains non-relative elements"
    );
    assert_eq!(
        ds("-8 days March 23"),
        "String '-8 days March 23' contains non-relative elements"
    );
    assert_eq!(
        ds("next weekday 15:30"),
        "String 'next weekday 15:30' contains non-relative elements"
    );
    assert_eq!(
        ds("foobar"),
        "Unknown or bad format (foobar) at position 0 (f): The timezone could not be found in the database"
    );
    assert_eq!(
        String::from_utf8(api::Interval::from_date_string_property(b"2023-01-16-foobar$*").unwrap_err()).unwrap(),
        "Unknown or bad format (2023-01-16-foobar$*) at position 10 (-) while unserializing: Unexpected character"
    );
    for (s, msg) in [
        ("", "Unknown or bad format ()"),
        (
            "2008-03-01T12:00:00Z1",
            "Unknown or bad format (2008-03-01T12:00:00Z1)",
        ),
        ("2D", "Unknown or bad format (2D)"),
        (
            "R4/2012-07-01T00:/P7D",
            "Unknown or bad format (R4/2012-07-01T00:/P7D)",
        ),
    ] {
        assert_eq!(api::DatePeriod::from_iso(s.as_bytes(), 0).unwrap_err(), msg);
    }
}

#[test]
fn gh18076_sun_info() {
    let utc = tz::get("UTC").unwrap();
    let ts = api::strtotime(b"2025-03-21", 0, &utc).unwrap();
    let v = api::date_sun_info(ts, 51.48, 0.0, &utc);
    let Some(Value::Int(rise)) = v.get("sunrise") else {
        panic!()
    };
    let Some(Value::Int(set)) = v.get("sunset") else {
        panic!()
    };
    assert_eq!(api::date(b"H:i:s", *rise, &utc), b"05:59:21");
    assert_eq!(api::date(b"H:i:s", *set, &utc), b"18:14:48");
}

#[test]
fn gh18422_extreme_isodate() {
    let utc = tz::get("UTC").unwrap();
    let (t, _) = api::date_initialize(b"2006-12-12", None, None, &utc, 0, 0);
    let mut t = t.unwrap();
    api::set_isodate(&mut t, i64::MIN, 1, 1);
    for f in [b"Y", b"x", b"X"] {
        let s = api::format(&t, f);
        assert_eq!(s[0], b'-');
        assert!(s[1..].iter().all(u8::is_ascii_digit));
    }
    let (t, _) = api::date_initialize(b"-0042-01-01", None, None, &utc, 0, 0);
    assert_eq!(api::format(&t.unwrap(), b"Y"), b"-0042");
}

#[test]
fn gh18481_non_finite_offsets() {
    let utc = tz::get("UTC").unwrap();
    for off in [f64::NAN, -f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let v = api::date_sunrise_sunset(
            0,
            api::SUNFUNCS_RET_STRING,
            38.4,
            -9.0,
            90.0,
            Some(off),
            &utc,
            false,
        );
        assert_eq!(v, Value::Bool(false));
    }
}

#[test]
fn gh20764_offsets_with_seconds() {
    let utc = tz::get("UTC").unwrap();
    for (spec, name) in [("+03:00:30", "+03:00:30"), ("-03:00:30", "-03:00:30")] {
        let z = php_date::parse::parse_timezone_spec(spec).unwrap();
        assert_eq!(z.name(), name);
        let (t, _) = api::date_initialize(b"2025-04-01", None, Some(&z), &utc, 0, 0);
        let t = t.unwrap();
        assert_eq!(api::format(&t, b"e"), name.as_bytes());
        let props = api::date_props(&t);
        assert_eq!(
            props.get("date"),
            Some(&Value::Str("2025-04-01 00:00:00.000000".into()))
        );
        assert_eq!(props.get("timezone_type"), Some(&Value::Int(1)));
        assert_eq!(props.get("timezone"), Some(&Value::Str(name.into())));
    }
}

/// Extreme inputs must not panic (debug builds check overflow) and must
/// behave like C's wrapping arithmetic.
#[test]
fn extreme_values_do_not_panic() {
    let utc = tz::get("UTC").unwrap();
    let ams = tz::get("Europe/Amsterdam").unwrap();
    for s in [
        "@9223372036854775807",
        "@-9223372036854775808",
        "+9223372036854775807 seconds",
        "-9223372036854775808 days",
        "9999999999999 years ago",
        "+1000000000000 weeks",
        "292277026596-12-04 15:30:07",
        "-292277022657-01-27 08:29:52",
        "+99999999999999999-01-01",
        "@1.999999",
        "last day of +999999999999 months",
        "+999999999999 weekdays",
    ] {
        let _ = api::date_parse(s.as_bytes());
        for tz in [&utc, &ams] {
            if let Some(ts) = api::strtotime(s.as_bytes(), 0, tz) {
                let _ = api::date(b"c r U e T I L W o t z N B u v", ts, tz);
            }
        }
    }
    for ts in [i64::MIN, i64::MAX, i64::MIN + 1, i64::MAX - 86400] {
        for tz in [&utc, &ams] {
            let _ = api::date(b"c r U e T I L W o t z N B u v x X", ts, tz);
            let _ = api::getdate(ts, tz);
            let _ = php_date::format::strftime(b"%c %s %G %V %z", ts, Some(tz));
            let _ = api::date_sun_info(ts, 50.0, 5.0, tz);
            let _ = api::idate(b"U", ts, tz);
        }
    }
    for v in [i64::MIN, i64::MAX, 0, -1] {
        let _ = api::mktime(
            v,
            Some(v),
            Some(v),
            Some(v),
            Some(v),
            Some(v),
            0,
            Some(&ams),
        );
        let (t, _) = api::date_initialize(b"2010-01-01", None, None, &ams, 0, 0);
        let mut t = t.unwrap();
        api::set_date(&mut t, v, v, v);
        api::set_time(&mut t, v, v, v, 0);
        let _ = api::format(&t, b"c u");
        let mut t2 = t.clone();
        api::set_timestamp(&mut t2, v);
        let d = api::diff(&t, &t2, false);
        let _ = d.format(b"%a %y %d %h");
        let _ = api::add(&t, &d);
        let _ = api::sub(&t, &d);
    }
}

/// Deterministic fuzzing of the arithmetic paths with extreme values.
#[test]
fn fuzz_extremes_do_not_panic() {
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let zones: Vec<_> = [
        "UTC",
        "Europe/Amsterdam",
        "America/St_Johns",
        "Pacific/Chatham",
        "Asia/Kathmandu",
    ]
    .iter()
    .map(|z| tz::get(z).unwrap())
    .collect();
    let interesting = [
        i64::MIN,
        i64::MIN + 1,
        -1,
        0,
        1,
        i64::MAX - 1,
        i64::MAX,
        1 << 40,
        -(1 << 50),
    ];
    let pick = |n: &mut dyn FnMut() -> u64| -> i64 {
        let r = n();
        if r % 3 == 0 {
            interesting[(r >> 8) as usize % interesting.len()]
        } else {
            (r >> 2) as i64 >> ((r >> 60) as u32 * 4)
        }
    };
    let words = [
        "+",
        "-",
        "day",
        "week",
        "month",
        "year",
        "hour",
        "min",
        "sec",
        "usec",
        "monday",
        "weekday",
        "ago",
        "first day of",
        "last day of",
        "next",
        "last",
        "@",
        " ",
        "2010-01-01",
        "10:00",
        "UTC",
        "CEST",
        "+0200",
    ];
    for i in 0..3000 {
        let tz = &zones[i % zones.len()];
        let a = pick(&mut next);
        let b = pick(&mut next);
        let c = pick(&mut next);
        let (t, _) = api::date_initialize(b"2010-03-28 02:30:00", None, None, tz, 0, 0);
        let mut t = t.unwrap();
        match i % 6 {
            0 => api::set_date(&mut t, a, b, c),
            1 => api::set_time(&mut t, a, b, c, pick(&mut next)),
            2 => api::set_isodate(&mut t, a, b, c),
            3 => api::set_timestamp(&mut t, a),
            4 => {
                let s = format!(
                    "{a} {} {b} {}",
                    words[(c as u64 % 24) as usize],
                    words[(b as u64 % 24) as usize]
                );
                let _ = api::modify(&mut t, s.as_bytes());
                let _ = api::strtotime(s.as_bytes(), a, tz);
                let _ = api::date_parse(s.as_bytes());
            }
            _ => {
                let _ = api::mktime(a, Some(b), Some(c), Some(a), Some(b), Some(c), a, Some(tz));
            }
        }
        let _ = api::format(&t, b"c r U e T I L W o t z N B u v x X");
        let mut other = t.clone();
        api::set_timestamp(&mut other, pick(&mut next));
        let d = api::diff(&t, &other, i % 2 == 0);
        let _ = d.format(b"%a %y %m %d %h %i %s %f %R");
        let _ = api::add(&t, &d);
        let _ = api::sub(&t, &d);
        let mut wall = d.clone();
        wall.kind = api::IntervalKind::Wall;
        let _ = api::add(&other, &wall);
        let _ = api::sub(&other, &wall);
        let _ = php_date::format::strftime(b"%c %s %G %V %z %C %y", a, Some(tz));
        let _ = api::idate(b"B", a, tz);
    }
}

#[test]
fn bug52480_diff_in_every_zone() {
    let utc = tz::get("UTC").unwrap();
    let mut wrong = Vec::new();
    for id in tz::identifiers_list(tz::group::ALL, None).unwrap() {
        let zone = TimezoneSpec::Id(tz::get(id).unwrap());
        let start = api::date_initialize(b"2017-03-01", None, Some(&zone), &utc, 0, 0)
            .0
            .unwrap();
        let end = api::date_initialize(b"2017-03-31", None, Some(&zone), &utc, 0, 0)
            .0
            .unwrap();
        let d = api::diff(&start, &end, false).rel;
        if (d.y, d.m, d.d, d.h, d.i, d.s, d.us, d.invert, d.days) != (0, 0, 30, 0, 0, 0, 0, 0, 30) {
            wrong.push(id);
        }
    }
    assert!(wrong.is_empty(), "wrong result for {wrong:?}");
}

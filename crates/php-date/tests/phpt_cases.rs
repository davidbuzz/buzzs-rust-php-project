//! Hand-ported expectations from php-src `ext/date/tests/*.phpt`.

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
    let iso = |s: &str| String::from_utf8(api::interval_from_iso(s.as_bytes()).unwrap_err()).unwrap();
    assert_eq!(iso("2007-05-11T15:30:00Z"), "Failed to parse interval (2007-05-11T15:30:00Z)");
    assert_eq!(iso("2007-05-11T15:30:00Z/"), "Failed to parse interval (2007-05-11T15:30:00Z/)");
    assert_eq!(iso(""), "Unknown or bad format ()");
    assert_eq!(iso("2007-05-11T15:30:00Z/:00Z"), "Unknown or bad format (2007-05-11T15:30:00Z/:00Z)");
    assert_eq!(iso("blah"), "Unknown or bad format (blah)");
    assert_eq!(iso("P3\"D"), "Unknown or bad format (P3\"D)");
    let ds = |s: &str| match api::interval_from_date_string(s.as_bytes()).unwrap_err() {
        api::DateStringIntervalError::BadFormat(m) | api::DateStringIntervalError::NonRelative(m) => {
            String::from_utf8(m).unwrap()
        }
    };
    assert_eq!(ds("+5 hours noon"), "String '+5 hours noon' contains non-relative elements");
    assert_eq!(ds("+72 seconds UTC"), "String '+72 seconds UTC' contains non-relative elements");
    assert_eq!(ds("-8 days March 23"), "String '-8 days March 23' contains non-relative elements");
    assert_eq!(ds("next weekday 15:30"), "String 'next weekday 15:30' contains non-relative elements");
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
        ("2008-03-01T12:00:00Z1", "Unknown or bad format (2008-03-01T12:00:00Z1)"),
        ("2D", "Unknown or bad format (2D)"),
        ("R4/2012-07-01T00:/P7D", "Unknown or bad format (R4/2012-07-01T00:/P7D)"),
    ] {
        assert_eq!(api::DatePeriod::from_iso(s.as_bytes(), 0).unwrap_err(), msg);
    }
}

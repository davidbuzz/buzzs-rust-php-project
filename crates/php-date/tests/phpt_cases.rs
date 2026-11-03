//! Hand-ported expectations from php-src `ext/date/tests/*.phpt`.

use php_date::api::{self, Value};
use php_date::parse::TimezoneSpec;
use php_date::tz;

fn show_transitions(tzid: &str, from: i64) -> String {
    let to = from + (2.5 * 366.0 * 24.0 * 60.0 * 60.0) as i64;
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
        out.push_str(&format!(
            "{ts:12} {time} {off:6} {} {abbr}\n",
            if dst { "DST" } else { " x " }
        ));
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
    assert_eq!(t.get("time"), Some(&Value::Str("1963-03-31T02:00:00+00:00".into())));
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
            format!("{} {k}", String::from_utf8_lossy(&api::date(b"Y-m-d H:i:s T", *ts, &oslo)))
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

//! Oracle probe: reads tab-separated commands on stdin and prints results
//! in the same canonical format as `tools/probe.php`, so the two can be
//! diffed against `/usr/bin/php`.
//!
//! Commands (fields backslash-escaped: `\t`, `\n`, `\\`):
//! * `parse <str>` — `date_parse()`
//! * `pff <format> <str>` — `date_parse_from_format()`
//! * `strtotime <tz> <now> <str>`
//! * `date <tz> <ts> <format>`
//! * `create <tz> <str> <format>` — `new DateTime($str)` then `format()`
//! * `modify <tz> <str> <modifier> <format>`
//! * `diff <tz> <a> <b> <format>` — `$a->diff($b)->format($format)`
//! * `cff <tz> <cff-format> <str> <format>` — `createFromFormat` then `format()`

use std::io::{self, BufRead, Write};

use php_date::api::{self, Key, Value};
use php_date::tz;

fn unescape(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            match b[i + 1] {
                b't' => out.push(b'\t'),
                b'n' => out.push(b'\n'),
                b'0' => out.push(0),
                other => out.push(other),
            }
            i += 2;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

fn dump(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push('N'),
        Value::Bool(b) => out.push(if *b { 'T' } else { 'F' }),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Float(f) => out.push_str(&format!("{f:.6}")),
        Value::Str(s) => {
            out.push('"');
            out.push_str(s);
            out.push('"');
        }
        Value::Array(items) => {
            out.push('{');
            for (k, v) in items {
                match k {
                    Key::Int(i) => out.push_str(&i.to_string()),
                    Key::Str(s) => out.push_str(s),
                }
                out.push(':');
                dump(v, out);
                out.push(',');
            }
            out.push('}');
        }
    }
}

fn s(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn run(fields: &[Vec<u8>]) -> String {
    let cmd = s(&fields[0]);
    let f = |i: usize| fields.get(i).cloned().unwrap_or_default();
    let zone = |i: usize| tz::get(&s(&f(i))).unwrap_or_else(|| tz::get("UTC").expect("UTC"));
    match cmd.as_str() {
        "parse" => {
            let mut o = String::new();
            dump(&api::date_parse(&f(1)), &mut o);
            o
        }
        "pff" => {
            let mut o = String::new();
            dump(&api::date_parse_from_format(&f(1), &f(2)), &mut o);
            o
        }
        "strtotime" => {
            let now: i64 = s(&f(2)).parse().unwrap_or(0);
            match api::strtotime(&f(3), now, &zone(1)) {
                Some(v) => v.to_string(),
                None => "F".into(),
            }
        }
        "date" => {
            let ts: i64 = s(&f(2)).parse().unwrap_or(0);
            s(&api::date(&f(3), ts, &zone(1)))
        }
        "create" => match api::date_initialize(&f(2), None, None, &zone(1), 1_136_073_600, 0).0 {
            Some(t) => s(&api::format(&t, &f(3))),
            None => "ERR".into(),
        },
        "cff" => match api::date_initialize(&f(3), Some(&f(2)), None, &zone(1), 1_136_073_600, 0).0 {
            Some(t) => s(&api::format(&t, &f(4))),
            None => "ERR".into(),
        },
        "modify" => match api::date_initialize(&f(2), None, None, &zone(1), 1_136_073_600, 0).0 {
            Some(mut t) => match api::modify(&mut t, &f(3)) {
                Ok(_) => s(&api::format(&t, &f(4))),
                Err(_) => "ERR".into(),
            },
            None => "ERR".into(),
        },
        "diff" => {
            let a = api::date_initialize(&f(2), None, None, &zone(1), 1_136_073_600, 0).0;
            let b = api::date_initialize(&f(3), None, None, &zone(1), 1_136_073_600, 0).0;
            match (a, b) {
                (Some(a), Some(b)) => s(&api::diff(&a, &b, false).format(&f(4))),
                _ => "ERR".into(),
            }
        }
        "interval" => match api::interval_from_iso(&f(1)) {
            Ok(iv) => s(&iv.format(&f(2))),
            Err(e) => format!("ERR {}", s(&e)),
        },
        "datestr" => match api::interval_from_date_string(&f(1)) {
            Ok(iv) => s(&iv.format(&f(2))),
            Err(_) => "ERR".into(),
        },
        "add" | "sub" => {
            let Some(t) = api::date_initialize(&f(2), None, None, &zone(1), 1_136_073_600, 0).0 else {
                return "ERR".into();
            };
            let iv = if f(3).first() == Some(&b'P') {
                api::interval_from_iso(&f(3)).ok()
            } else {
                api::interval_from_date_string(&f(3)).ok()
            };
            let Some(iv) = iv else { return "ERR".into() };
            let r = if cmd == "add" { Ok(api::add(&t, &iv)) } else { api::sub(&t, &iv) };
            match r {
                Ok(t) => s(&api::format(&t, &f(4))),
                Err(_) => "ERR".into(),
            }
        }
        "period" => {
            let opts: i64 = s(&f(2)).parse().unwrap_or(0);
            match api::DatePeriod::from_iso(&f(1), opts) {
                Ok(mut p) => p.dates(50).iter().map(|t| s(&api::format(t, &f(3)))).collect::<Vec<_>>().join("|"),
                Err(e) => format!("ERR {e}"),
            }
        }
        "sun" => {
            let ts: i64 = s(&f(2)).parse().unwrap_or(0);
            let lat: f64 = s(&f(3)).parse().unwrap_or(0.0);
            let lon: f64 = s(&f(4)).parse().unwrap_or(0.0);
            let mut o = String::new();
            dump(&api::date_sun_info(ts, lat, lon, &zone(1)), &mut o);
            o
        }
        "abbr" => {
            let off: i64 = s(&f(2)).parse().unwrap_or(-1);
            let dst: i64 = s(&f(3)).parse().unwrap_or(-1);
            tz::timezone_id_from_abbr(&s(&f(1)), off, dst).map_or_else(|| "F".to_string(), str::to_string)
        }
        "tzinit" => match php_date::parse::parse_timezone_spec(&s(&f(1))) {
            Ok(z) => format!("{} {}", z.zone_type(), z.name()),
            Err(e) => format!("ERR {e}"),
        },
        _ => "?".into(),
    }
}

fn main() {
    let stdin = io::stdin();
    let mut out = io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let fields: Vec<Vec<u8>> = line.split('\t').map(unescape).collect();
        let r = run(&fields);
        let _ = writeln!(out, "{r}");
    }
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Shared command interpreter for the oracle probe (`examples/probe.rs`)
//! and the oracle table tests (`tests/oracle.rs`).
//!
//! Each command mirrors a snippet of `tools/probe.php`; outputs use a
//! canonical text form so they can be compared with `/usr/bin/php8.5`.

#![allow(dead_code, clippy::too_many_lines)]

use std::fmt::Write as _;

use php_date::api::{self, Key, Value};
use php_date::tz;

pub fn unescape(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
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
        Value::Float(f) => {
            let _ = write!(out, "{f:.6}");
        }
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

/// Escapes like the generator does (`\\`, `\t`, `\n`, `\0`).
pub fn escape(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    for &c in b {
        match c {
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            0 => out.extend_from_slice(b"\\0"),
            _ => out.push(c),
        }
    }
    out
}

/// Runs one probe command (already split and unescaped).
pub fn run(fields: &[Vec<u8>]) -> String {
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
        "cff" => match api::date_initialize(&f(3), Some(&f(2)), None, &zone(1), 1_136_073_600, 0).0
        {
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
            let Some(t) = api::date_initialize(&f(2), None, None, &zone(1), 1_136_073_600, 0).0
            else {
                return "ERR".into();
            };
            let iv = if f(3).first() == Some(&b'P') {
                api::interval_from_iso(&f(3)).ok()
            } else {
                api::interval_from_date_string(&f(3)).ok()
            };
            let Some(iv) = iv else { return "ERR".into() };
            let r = if cmd == "add" {
                Ok(api::add(&t, &iv))
            } else {
                api::sub(&t, &iv)
            };
            match r {
                Ok(t) => s(&api::format(&t, &f(4))),
                Err(_) => "ERR".into(),
            }
        }
        "period" => {
            let opts: i64 = s(&f(2)).parse().unwrap_or(0);
            match api::DatePeriod::from_iso(&f(1), opts) {
                Ok(mut p) => p
                    .dates(50)
                    .iter()
                    .map(|t| s(&api::format(t, &f(3))))
                    .collect::<Vec<_>>()
                    .join("|"),
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
            tz::timezone_id_from_abbr(&s(&f(1)), off, dst)
                .map_or_else(|| "F".to_string(), str::to_string)
        }
        "tzinit" => match php_date::parse::parse_timezone_spec(&s(&f(1))) {
            Ok(z) => format!("{} {}", z.zone_type(), z.name()),
            Err(e) => format!("ERR {e}"),
        },
        "strftime" | "gmstrftime" => {
            let ts: i64 = s(&f(2)).parse().unwrap_or(0);
            let z = zone(1);
            let r = php_date::format::strftime(
                &f(3),
                ts,
                if cmd == "strftime" { Some(&z) } else { None },
            );
            r.map_or_else(|| "F".to_string(), |b| s(&b))
        }
        "idate" => {
            let ts: i64 = s(&f(2)).parse().unwrap_or(0);
            match api::idate(&f(3), ts, &zone(1)) {
                Ok(v) => v.to_string(),
                Err(_) => "F".into(),
            }
        }
        "getdate" => {
            let ts: i64 = s(&f(2)).parse().unwrap_or(0);
            let mut o = String::new();
            dump(&api::getdate(ts, &zone(1)), &mut o);
            o.push('|');
            dump(&api::localtime(ts, &zone(1), true), &mut o);
            o
        }
        "mktime" | "gmmktime" => {
            let n: Vec<Option<i64>> = (2..8)
                .map(|i| {
                    let v = s(&f(i));
                    if v.is_empty() {
                        None
                    } else {
                        v.parse().ok()
                    }
                })
                .collect();
            let z = zone(1);
            let now = 1_136_073_600;
            api::mktime(
                n[0].unwrap_or(0),
                n[1],
                n[2],
                n[3],
                n[4],
                n[5],
                now,
                if cmd == "mktime" { Some(&z) } else { None },
            )
            .to_string()
        }
        _ => "?".into(),
    }
}

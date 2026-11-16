// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Bundled stream filters checked against `/usr/bin/php` output recorded in
//! `tests/data/filters_oracle.txt` (generator: `tests/data/gen_filters.php`).

use php_streams::diag;
use php_streams::filter::standard::register_standard_filters;
use php_streams::filter::{
    Brigade, Bucket, ChainKind, FilterEnv, FilterFlags, FilterRegistry, FilterStatus,
};
use php_streams::value::{Key, Value};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn hex(b: &[u8]) -> String {
    use std::fmt::Write;
    b.iter().fold(String::new(), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

fn parse_params(s: &str) -> Option<Value> {
    if s == "-" {
        return None;
    }
    if let Some(h) = s.strip_prefix('!') {
        return Some(Value::Str(unhex(h)));
    }
    if s.is_empty() {
        return Some(Value::Array(Vec::new()));
    }
    let items = s
        .split(',')
        .map(|item| {
            let mut parts = item.splitn(3, ':');
            let key = parts.next().expect("key");
            let ty = parts.next().expect("type");
            let val = unhex(parts.next().expect("value"));
            let v = match ty {
                "i" => Value::Int(String::from_utf8(val).unwrap().parse().unwrap()),
                "b" => Value::Bool(val == b"1"),
                _ => Value::Str(val),
            };
            (Key::str(key), v)
        })
        .collect();
    Some(Value::Array(items))
}

/// Mirrors the generator: one write-chain run per non-empty piece, then the
/// closing flush done by `stream_filter_remove()`.
fn run(reg: &FilterRegistry, name: &str, params: Option<&Value>, pieces: &[Vec<u8>]) -> String {
    let Some(mut f) = reg.create(name, params, false) else {
        return "FAIL".into();
    };
    let mut env = FilterEnv::detached(ChainKind::Write);
    let mut out = Vec::new();
    for piece in pieces.iter().filter(|p| !p.is_empty()) {
        let mut input = Brigade::from(vec![Bucket::new(piece.clone())]);
        let mut o = Brigade::new();
        let mut consumed = 0;
        let st = f.filter(
            &mut env,
            &mut input,
            &mut o,
            Some(&mut consumed),
            FilterFlags::Normal,
        );
        if st == FilterStatus::PassOn {
            out.extend(o.into_iter().flat_map(|b| b.data));
        }
    }
    let mut o = Brigade::new();
    let st = f.filter(
        &mut env,
        &mut Brigade::new(),
        &mut o,
        None,
        FilterFlags::FlushClose,
    );
    if st == FilterStatus::PassOn {
        out.extend(o.into_iter().flat_map(|b| b.data));
    }
    hex(&out)
}

#[test]
fn filters_match_php() {
    let data = include_str!("data/filters_oracle.txt");
    let mut reg = FilterRegistry::new();
    register_standard_filters(&mut reg);
    let mut checked = 0;
    let mut failures = Vec::new();
    for line in data.lines() {
        let cols: Vec<&str> = line.split('\t').collect();
        assert_eq!(cols.len(), 6, "bad line {line}");
        let (name, params, pieces, expected, warnings) =
            (cols[0], cols[1], cols[2], cols[4], cols[5]);
        if params.contains("write_seek_mode:s:626f677573") {
            // PHP 8.3 has no write_seek_mode; master rejects bogus values
            // (checked separately below).
            continue;
        }
        let params = parse_params(params);
        let pieces: Vec<Vec<u8>> = pieces.split(',').map(unhex).collect();
        if name == "dechunk" && pieces.concat().starts_with(b"fffffffffffffffff") {
            // master rejects chunk sizes overflowing size_t; 8.3 wraps
            // (checked separately below).
            continue;
        }
        let _ = diag::take();
        let got = run(&reg, name, params.as_ref(), &pieces);
        let got_warn: Vec<String> = diag::take().iter().map(diag::Diagnostic::text).collect();
        let got_warn = got_warn.join("|");
        if got != expected || got_warn != warnings {
            failures.push(format!(
                "{name} {:?} {pieces:?}\n  want {expected} [{warnings}]\n  got  {got} [{got_warn}]",
                cols[1]
            ));
        }
        checked += 1;
    }
    assert!(checked > 1500, "only {checked} cases");
    assert!(
        failures.is_empty(),
        "{} of {checked} cases differ:\n{}",
        failures.len(),
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn dechunk_size_overflow_follows_master() {
    let mut reg = FilterRegistry::new();
    register_standard_filters(&mut reg);
    let piece = b"ffffffffffffffffff\r\nx".to_vec();
    // The 17th hex digit would overflow: the decoder enters the error state
    // and passes the remaining input through unchanged.
    assert_eq!(run(&reg, "dechunk", None, &[piece]), hex(b"ff\r\nx"));
}

#[test]
fn bogus_write_seek_mode_follows_master() {
    let mut reg = FilterRegistry::new();
    register_standard_filters(&mut reg);
    let _ = diag::take();
    let p = Value::Array(vec![(Key::str("write_seek_mode"), Value::str("bogus"))]);
    assert!(reg
        .create("convert.base64-encode", Some(&p), false)
        .is_none());
    let w: Vec<String> = diag::take().iter().map(diag::Diagnostic::text).collect();
    assert_eq!(
        w,
        [
            "\"write_seek_mode\" filter parameter must be one of \"preserve\", \"reset\", or \"strict\"",
            "Unable to create or locate filter \"convert.base64-encode\""
        ]
    );
}

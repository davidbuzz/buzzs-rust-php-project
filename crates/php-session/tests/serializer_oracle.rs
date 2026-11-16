// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Byte-exact comparison of the three serializers against fixtures produced
//! by `/usr/bin/php` (see `fixtures/gen_serializers.php`).

use php_runtime::serialize::{unserialize, UnserializeOptions};
use php_runtime::{CollectingContext, Value, ZArray};
use php_session::serializer::{decode, encode, Decoded, Serializer};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

struct Row {
    name: String,
    whole: Vec<u8>,
    php: Option<Vec<u8>>,
    php_warnings: Vec<String>,
    binary: Vec<u8>,
    binary_warnings: Vec<String>,
}

fn warnings(s: &str) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split(';')
        .map(|w| w.trim_start_matches("session_encode(): ").to_string())
        .collect()
}

fn rows() -> Vec<Row> {
    include_str!("fixtures/serializers.tsv")
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            // PHP 8.3 returns false for an empty encoding; master
            // (smart_str_extract) returns "". Only a '|' key fails there.
            let pipe = f[0].starts_with("pipe_key");
            let field = |x: &str| if x == "FALSE" { Vec::new() } else { unhex(x) };
            Row {
                name: f[0].to_string(),
                whole: unhex(f[1]),
                php: (!pipe).then(|| field(f[2])),
                php_warnings: warnings(f[3]),
                binary: field(f[4]),
                binary_warnings: warnings(f.get(5).copied().unwrap_or("")),
            }
        })
        .collect()
}

fn shape(row: &Row, ctx: &mut CollectingContext) -> ZArray {
    match unserialize(&row.whole, &UnserializeOptions::default(), ctx).unwrap() {
        Some(Value::Array(a)) => a,
        other => panic!("{}: {other:?}", row.name),
    }
}

fn merge(entries: Vec<(php_runtime::ZStr, Value)>) -> ZArray {
    let mut a = ZArray::new();
    for (k, v) in entries {
        a.make_mut().insert_str_raw(k, v);
    }
    a
}

#[test]
fn encode_matches_php() {
    let rows = rows();
    assert!(rows.len() > 200);
    for row in &rows {
        let mut ctx = CollectingContext::new();
        let vars = shape(row, &mut ctx);

        let whole = encode(Serializer::PhpSerialize, &vars, &mut ctx).unwrap();
        assert_eq!(
            whole.data.as_deref(),
            Some(row.whole.as_slice()),
            "{}",
            row.name
        );

        let php = encode(Serializer::Php, &vars, &mut ctx).unwrap();
        assert_eq!(php.data, row.php, "php encode of {}", row.name);
        let mut expected = row.php_warnings.clone();
        if row.php.is_none() {
            // PHP master (unlike 8.3) reports the rejected key.
            let bad = php.warnings.last().cloned().unwrap_or_default();
            assert!(bad.starts_with("Failed to write session data. Data contains invalid key"));
            expected.push(bad);
        }
        assert_eq!(php.warnings, expected, "php warnings of {}", row.name);

        let bin = encode(Serializer::PhpBinary, &vars, &mut ctx).unwrap();
        assert_eq!(
            bin.data.as_deref(),
            Some(row.binary.as_slice()),
            "binary encode of {}",
            row.name
        );
        assert_eq!(
            bin.warnings, row.binary_warnings,
            "binary warnings of {}",
            row.name
        );
        assert!(
            ctx.diagnostics.is_empty(),
            "{}: {:?}",
            row.name,
            ctx.diagnostics
        );
    }
}

#[test]
fn decode_round_trips() {
    for row in rows() {
        let mut ctx = CollectingContext::new();
        for (ser, data) in [
            (Serializer::Php, row.php.clone()),
            (Serializer::PhpBinary, Some(row.binary.clone())),
        ] {
            let Some(data) = data else { continue };
            let Decoded::Merge { entries, ok } = decode(ser, &data, &mut ctx).unwrap() else {
                panic!()
            };
            assert!(ok, "{} {}", ser.name(), row.name);
            let again = encode(ser, &merge(entries), &mut ctx).unwrap();
            assert_eq!(
                again.data.as_deref(),
                Some(data.as_slice()),
                "{} {}",
                ser.name(),
                row.name
            );
        }
        let Decoded::Replace { value, ok } =
            decode(Serializer::PhpSerialize, &row.whole, &mut ctx).unwrap()
        else {
            panic!()
        };
        assert!(ok);
        let Value::Array(a) = value else { panic!() };
        let again = encode(Serializer::PhpSerialize, &a, &mut ctx).unwrap();
        assert_eq!(
            again.data.as_deref(),
            Some(row.whole.as_slice()),
            "{}",
            row.name
        );
        assert!(
            ctx.diagnostics.is_empty(),
            "{}: {:?}",
            row.name,
            ctx.diagnostics
        );
    }
}

#[test]
fn truncated_data_fails_quietly() {
    for row in rows() {
        let Some(php) = row.php else { continue };
        if php.is_empty() {
            continue;
        }
        let mut ctx = CollectingContext::new();
        let cut = &php[..php.len() - 1];
        let Decoded::Merge { ok, .. } = decode(Serializer::Php, cut, &mut ctx).unwrap() else {
            panic!()
        };
        assert!(!ok, "{}", row.name);
        assert!(
            ctx.diagnostics.is_empty(),
            "{}: {:?}",
            row.name,
            ctx.diagnostics
        );
    }
}

#[test]
fn http_dates_match_gmdate() {
    let mut n = 0;
    for line in include_str!("fixtures/misc.tsv").lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f[0] == "date" {
            let t: i64 = f[1].parse().unwrap();
            assert_eq!(php_session::http::http_date(t), f[2], "{t}");
            n += 1;
        }
    }
    assert!(n > 200);
}

#[test]
fn sid_alphabets_match_php() {
    let all = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ,-";
    for line in include_str!("fixtures/misc.tsv").lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f[0] != "sidchars" {
            continue;
        }
        let bits: u32 = f[1].parse().unwrap();
        let mut ours: Vec<u8> = all[..1 << bits].to_vec();
        ours.sort_unstable();
        assert_eq!(String::from_utf8(ours).unwrap(), f[2], "bits {bits}");
        // Every encoded byte stream maps into exactly that alphabet.
        let bytes: Vec<u8> = (0..=255).collect();
        let id =
            php_session::sid::bin_to_readable(&bytes, 200, u8::try_from(bits).unwrap()).unwrap();
        assert!(id.iter().all(|c| f[2].as_bytes().contains(c)));
    }
}

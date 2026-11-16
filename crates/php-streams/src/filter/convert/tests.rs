// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
use super::*;
use crate::filter::ChainKind;
use crate::value::Key;

fn params(items: &[(&str, Value)]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|(k, v)| (Key::str(*k), v.clone()))
            .collect(),
    )
}

/// Runs `data` split into `split`-byte buckets (0 = one bucket) through a
/// fresh filter, then the closing flush. Returns output and status.
fn run(name: &str, p: Option<&Value>, data: &[u8], split: usize) -> (Vec<u8>, FilterStatus) {
    let mut f = ConvertFilterFactory
        .create(name, p, false)
        .expect("filter created");
    let mut env = FilterEnv::detached(ChainKind::Write);
    let mut out = Vec::new();
    let mut status = FilterStatus::PassOn;
    let pieces: Vec<&[u8]> = if split == 0 || data.is_empty() {
        vec![data]
    } else {
        data.chunks(split).collect()
    };
    for piece in pieces {
        let mut input = Brigade::from(vec![Bucket::new(piece)]);
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
            assert_eq!(consumed, piece.len());
            out.extend(o.into_iter().flat_map(|b| b.data));
        } else {
            status = st;
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
    } else {
        status = st;
    }
    (out, status)
}

#[test]
fn base64_roundtrip_all_splits() {
    let data: Vec<u8> = (0..=255u8).collect();
    for split in [0, 1, 2, 3, 4, 7] {
        let (enc, st) = run("convert.base64-encode", None, &data, split);
        assert_eq!(st, FilterStatus::PassOn);
        let (dec, st) = run("convert.base64-decode", None, &enc, split);
        assert_eq!(st, FilterStatus::PassOn);
        assert_eq!(dec, data, "split {split}");
    }
    assert_eq!(
        run("convert.base64-encode", None, b"abcd", 1).0,
        b"YWJjZA=="
    );
}

#[test]
fn base64_line_breaks() {
    let p = params(&[
        ("line-length", Value::Int(8)),
        ("line-break-chars", Value::str("\n")),
    ]);
    assert_eq!(
        run("convert.base64-encode", Some(&p), b"Hello, World!", 0).0,
        b"SGVsbG8s\nIFdvcmxk\nIQ=="
    );
    let p = params(&[("line-length", Value::Int(8))]);
    assert_eq!(
        run("convert.base64-encode", Some(&p), b"Hello, World!", 1).0,
        b"SGVsbG8s\r\nIFdvcmxk\r\nIQ=="
    );
}

#[test]
fn base64_decode_invalid() {
    let _ = diag::take();
    let (_, st) = run("convert.base64-decode", None, b"Q===", 0);
    assert_eq!(st, FilterStatus::ErrFatal);
    let d = diag::take();
    assert_eq!(
        d[0].text(),
        "Stream filter (convert.base64-decode): invalid byte sequence"
    );
}

#[test]
fn qprint_roundtrip() {
    let data = b"caf\xc3\xa9 = ok \r\nnext\tline  \r\n".to_vec();
    for split in [0, 1, 3] {
        let (enc, _) = run("convert.quoted-printable-encode", None, &data, split);
        // Without options there are no line break chars: CRLF is encoded.
        assert_eq!(
            enc, b"caf=C3=A9 =3D ok =0D=0Anext\tline  =0D=0A",
            "split {split}"
        );
        let (dec, _) = run("convert.quoted-printable-decode", None, &enc, split);
        assert_eq!(dec, data);
    }
}

#[test]
fn factory_errors() {
    let _ = diag::take();
    assert!(ConvertFilterFactory
        .create("convert.base64-encode", Some(&Value::str("x")), false)
        .is_none());
    assert!(ConvertFilterFactory
        .create("convert.nope", None, false)
        .is_none());
    assert!(ConvertFilterFactory
        .create("CONVERT.Base64-Encode", None, false)
        .is_some());
    let d = diag::take();
    assert_eq!(d.len(), 1);
    assert_eq!(
        d[0].text(),
        "Stream filter (convert.base64-encode): invalid filter parameter"
    );
}

#[test]
fn seek_resets_state() {
    let mut f = ConvertFilterFactory
        .create("convert.base64-encode", None, false)
        .unwrap();
    let mut env = FilterEnv::detached(ChainKind::Write);
    let mut input = Brigade::from(vec![Bucket::new("ab")]);
    let mut o = Brigade::new();
    f.filter(&mut env, &mut input, &mut o, None, FilterFlags::Normal);
    assert!(o.is_empty());
    assert!(f.seek(&mut env, 0, 0));
    let mut o = Brigade::new();
    f.filter(
        &mut env,
        &mut Brigade::new(),
        &mut o,
        None,
        FilterFlags::FlushClose,
    );
    assert!(o.is_empty());
    assert_eq!(f.read_seekable(), Seekable::Start);
    assert_eq!(f.write_seekable(), Seekable::Always);
    let p = params(&[("write_seek_mode", Value::str("strict"))]);
    let f = ConvertFilterFactory
        .create("convert.base64-encode", Some(&p), false)
        .unwrap();
    assert_eq!(f.write_seekable(), Seekable::Never);
}

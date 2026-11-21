// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Expectations hand-ported from ext/intl/tests/uconverter_*.phpt.

use std::sync::{Arc, Mutex};

use php_intl::converter::{self, CallbackValue, PhpCallbacks, UConverter};

fn conv(dest: &str, src: &str) -> UConverter {
    let mut c = UConverter::new_empty(None);
    c.construct(Some(dest), Some(src)).unwrap();
    c
}

#[test]
fn uconverter_oop_subst() {
    let mut c = conv("ascii", "utf-8");
    assert!(c.set_subst_chars(b"?"));
    assert_eq!(
        c.convert(b"This is an ascii string", false).unwrap(),
        b"This is an ascii string"
    );
    assert_eq!(
        c.convert("Snowman: (\u{2603})".as_bytes(), false).unwrap(),
        b"Snowman: (?)"
    );
    assert!(!c.set_subst_chars(b""));
    assert!(!c.set_subst_chars(b"<unknown>"));
}

#[test]
fn uconverter_func_subst() {
    let (v, _) = converter::transcode(
        "Snowman: (\u{2603})".as_bytes(),
        "ascii",
        "utf-8",
        None,
        Some(b"?"),
    )
    .unwrap();
    assert_eq!(v, b"Snowman: (?)");
    for subst in [&b""[..], b"??"] {
        let e = converter::transcode(b"x", "ascii", "utf-8", None, Some(subst)).unwrap_err();
        assert_eq!(
            e.intl_message(),
            Some("returned error 1: U_ILLEGAL_ARGUMENT_ERROR")
        );
    }
}

#[test]
fn uconverter_oop_algo() {
    let c = conv("utf-8", "latin1");
    assert_eq!(c.source_type(), Some(3)); // LATIN_1
    assert_eq!(c.destination_type(), Some(4)); // UTF8
    let c = conv("koi8-r", "utf-32be");
    assert_eq!(c.source_type(), Some(7)); // UTF32_BigEndian
    assert_eq!(c.destination_type(), Some(0)); // SBCS
}

struct Ret;

impl PhpCallbacks for Ret {
    fn to_u_callback(&mut self, _r: i64, _s: &[u8], units: &[u8], e: &mut i64) -> CallbackValue {
        *e = 0;
        match units {
            b"\x81" => CallbackValue::Str(b"a".to_vec()),
            b"\x82" => CallbackValue::Int(i64::from(b'b')),
            b"\x83" => CallbackValue::Array(vec![CallbackValue::Str(b"c".to_vec())]),
            _ => CallbackValue::Null,
        }
    }

    fn from_u_callback(&mut self, _r: i64, _s: &[i64], cp: i64, e: &mut i64) -> CallbackValue {
        *e = 0;
        match cp {
            0xF1 => CallbackValue::Str(b"A".to_vec()),
            0xF2 => CallbackValue::Int(i64::from(b'B')),
            0xF3 => CallbackValue::Array(vec![CallbackValue::Str(b"C".to_vec())]),
            _ => CallbackValue::Null,
        }
    }
}

#[test]
fn uconverter_oop_callback_return() {
    let mut c = UConverter::new_empty(Some(Box::new(Ret)));
    c.construct(Some("ascii"), Some("utf-8")).unwrap();
    assert_eq!(c.convert(b"\x80\x81\x82\x83", false).unwrap(), b"abc");
    assert_eq!(c.convert("ñòóô".as_bytes(), false).unwrap(), b"ABC");
}

struct Trace(Arc<Mutex<Vec<String>>>);

impl PhpCallbacks for Trace {
    // Like `parent::toUCallback()`: substitute and clear the error.
    fn to_u_callback(&mut self, r: i64, _s: &[u8], _u: &[u8], e: &mut i64) -> CallbackValue {
        self.0.lock().unwrap().push(format!(
            "toUCallback({}, ...)",
            converter::reason_text(r).unwrap()
        ));
        if r <= 2 {
            *e = 0;
            return CallbackValue::Str("\u{fffd}".as_bytes().to_vec());
        }
        CallbackValue::Null
    }

    fn from_u_callback(&mut self, r: i64, _s: &[i64], _c: i64, e: &mut i64) -> CallbackValue {
        self.0.lock().unwrap().push(format!(
            "fromUCallback({}, ...)",
            converter::reason_text(r).unwrap()
        ));
        if r <= 2 {
            *e = 0;
            return CallbackValue::Str("\u{fffd}".as_bytes().to_vec());
        }
        CallbackValue::Null
    }
}

#[test]
fn uconverter_oop_callback_sequence() {
    let log = Arc::new(Mutex::new(Vec::new()));
    {
        let mut c = UConverter::new_empty(Some(Box::new(Trace(log.clone()))));
        c.construct(Some("ascii"), Some("utf-8")).unwrap();
        for w in [
            &b"regular"[..],
            b"irregul\xC1\xA1r",
            b"\xC2\xA1unsupported!",
        ] {
            let _ = c.convert(w, false);
        }
    }
    // ICU 74 reports "\xC1" and "\xA1" as two illegal sequences (as
    // /usr/bin/php8.5 does with the same ICU), each pass of the
    // preflight-then-convert sequence calling back.
    let t = |r: &str| format!("toUCallback({r}, ...)");
    let f = |r: &str| format!("fromUCallback({r}, ...)");
    let mut want = vec![
        t("REASON_RESET"),
        t("REASON_RESET"),
        f("REASON_RESET"),
        f("REASON_RESET"),
    ];
    for _ in 0..2 {
        want.extend([t("REASON_RESET"), t("REASON_ILLEGAL"), t("REASON_ILLEGAL")]);
    }
    for _ in 0..2 {
        want.extend([
            f("REASON_RESET"),
            f("REASON_UNASSIGNED"),
            f("REASON_UNASSIGNED"),
        ]);
    }
    want.extend([t("REASON_RESET"), t("REASON_RESET")]);
    for _ in 0..2 {
        want.extend([f("REASON_RESET"), f("REASON_UNASSIGNED")]);
    }
    want.extend([
        t("REASON_CLOSE"),
        f("REASON_CLOSE"),
        t("REASON_CLOSE"),
        f("REASON_CLOSE"),
    ]);
    assert_eq!(*log.lock().unwrap(), want);
}

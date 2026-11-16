// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Ports of `ext/intl/tests/spoofchecker_*.phpt` (php-src master).

use php_intl::error::UErrorCode;
use php_intl::spoofchecker::{checks, level, pattern_option, Spoofchecker, LTR, RTL};
use php_intl::Error;

fn new() -> Spoofchecker {
    Spoofchecker::new().expect("open")
}

fn value_error(e: Error) -> (u32, String) {
    match e {
        Error::ArgumentValue { arg, message } => (arg, message),
        other => panic!("expected ValueError, got {other:?}"),
    }
}

#[test]
fn spoofchecker_002_confusable() {
    let mut x = new();
    assert!(!x.are_confusable(b"hello, world", b"goodbye, world").value);
    assert!(x.are_confusable(b"hello, world", b"hello, world").value);
    assert!(x.are_confusable(b"hello, world", b"he11o, wor1d").value);
}

#[test]
fn spoofchecker_003_locales() {
    let korean = "한국말".as_bytes();
    let mut x = new();
    assert!(x.set_allowed_locales(b"en_US").warnings.is_empty());
    assert!(x.is_suspicious(korean).value);
    x.set_allowed_locales(b"en_US, ko_KR");
    assert!(!x.is_suspicious(korean).value);
}

#[test]
fn spoofchecker_005_settings() {
    let mut x = new();
    assert!(x.are_confusable(b"HELLO", "H\u{0415}LLO".as_bytes()).value);
    assert!(x.are_confusable(b"hello", "h\u{0435}llo".as_bytes()).value);
    x.set_checks(
        checks::MIXED_SCRIPT_CONFUSABLE
            | checks::WHOLE_SCRIPT_CONFUSABLE
            | checks::SINGLE_SCRIPT_CONFUSABLE,
    );
    assert!(x.are_confusable(b"HELLO", "H\u{0415}LLO".as_bytes()).value);
    assert!(x.are_confusable(b"hello", "h\u{0435}llo".as_bytes()).value);
}

#[test]
fn spoofchecker_006_suspicious() {
    let mut x = new();
    assert!(
        x.is_suspicious("http://www.payp\u{0430}l.com".as_bytes())
            .value
    );
    assert!(x.is_suspicious("\u{041F}aypal.com".as_bytes()).value);
    let mut x = new();
    x.set_allowed_locales(b"gr_GR");
    assert!(x.is_suspicious(b"NAPKIN PEZ").value);
    assert!(x.is_suspicious(b"napkin pez").value);
}

#[test]
fn spoofchecker_007_restriction_level() {
    let mut x = new();
    x.set_restriction_level(level::HIGHLY_RESTRICTIVE)
        .expect("lvl");
    assert!(!x.is_suspicious("\u{91CE}\u{7403}\u{30FC}".as_bytes()).value);
    x.set_restriction_level(level::SINGLE_SCRIPT_RESTRICTIVE)
        .expect("lvl");
    assert!(
        x.is_suspicious("\u{91CE}\u{7403}\u{30FC} abc".as_bytes())
            .value
    );
}

#[test]
fn spoofchecker_008_allowed_chars() {
    let mut s = new();
    s.set_allowed_chars(b"[a-z]", 0).expect("set");
    assert!(s.is_suspicious(b"123").value);
    s.set_allowed_chars(b"[1-3]", 0).expect("set");
    assert!(!s.is_suspicious(b"123").value);
    s.set_allowed_chars(
        b"[a-z]",
        pattern_option::IGNORE_SPACE | pattern_option::CASE_INSENSITIVE,
    )
    .expect("set");
    assert!(!s.is_suspicious(b"ABC").value);
    let (arg, msg) = value_error(s.set_allowed_chars(b"[a-z]", 1024).unwrap_err());
    assert_eq!(arg, 2);
    assert!(msg.starts_with(
        "must be a valid pattern option, 0 or (SpoofChecker::IGNORE_SPACE|(<none> or SpoofChecker::CASE_INSENSITIVE"
    ));
    for bad in [&b"A-Z]"[..], b"[A-Z", b""] {
        let (arg, msg) = value_error(s.set_allowed_chars(bad, 0).unwrap_err());
        assert_eq!(arg, 1);
        assert_eq!(
            msg,
            "must be a valid regular expression character set pattern"
        );
    }
    let (_, msg) = value_error(s.set_allowed_chars(b"[[:bogus:]]", 0).unwrap_err());
    assert!(msg.starts_with("must be a valid regular expression character set pattern ("));
}

#[test]
fn spoofchecker_setchecks_error_code() {
    let mut s = new();
    let r = s.set_checks(1 << 20);
    assert_eq!(r.error, Some(UErrorCode::ILLEGAL_ARGUMENT_ERROR));
    assert_eq!(r.warnings, ["(1) U_ILLEGAL_ARGUMENT_ERROR"]);
}

#[test]
fn spoofchecker_unknown_restriction_level() {
    let mut x = new();
    let (arg, msg) = value_error(x.set_restriction_level(checks::SINGLE_SCRIPT).unwrap_err());
    assert_eq!(arg, 1);
    assert_eq!(
        msg,
        "must be one of Spoofchecker::ASCII, Spoofchecker::SINGLE_SCRIPT_RESTRICTIVE, \
         Spoofchecker::HIGHLY_RESTRICTIVE, Spoofchecker::MODERATELY_RESTRICTIVE, \
         Spoofchecker::MINIMALLY_RESTRICTIVE, or Spoofchecker::UNRESTRICTIVE"
    );
}

#[test]
fn spoofchecker_skeleton() {
    let mut checker = new();
    assert_eq!(checker.get_skeleton(b"").unwrap(), b"");
    assert_eq!(checker.get_skeleton(b"abc").unwrap(), b"abc");
    let cyr = "\u{0441}".as_bytes();
    assert_eq!(
        checker.get_skeleton(b"c").unwrap(),
        checker.get_skeleton(cyr).unwrap()
    );
    assert!(checker.are_confusable(b"c", cyr).value);
    assert_ne!(
        checker.get_skeleton(b"abc").unwrap(),
        checker.get_skeleton(b"xyz").unwrap()
    );
    let sk = checker.get_skeleton(cyr).unwrap();
    assert_eq!(checker.get_skeleton(&sk).unwrap(), sk);
    assert_eq!(checker.get_skeleton("\u{FB01}".as_bytes()).unwrap(), b"fi");
    assert_eq!(checker.get_skeleton(b"a\0b").unwrap(), b"a\0b");
    let mut configured = new();
    configured.set_checks(checks::SINGLE_SCRIPT);
    configured.set_restriction_level(level::ASCII).expect("lvl");
    configured.set_allowed_chars(b"[a-z]", 0).expect("chars");
    assert_eq!(configured.get_skeleton(cyr).unwrap(), sk);
    for bad in [
        &b"\x80"[..],
        b"\xC3",
        b"\xE2\x82",
        b"\xED\xA0\x80",
        b"abc\xFF",
    ] {
        let e = checker.get_skeleton(bad).unwrap_err();
        assert_eq!(e.intl_code(), UErrorCode::INVALID_CHAR_FOUND);
        assert_eq!(
            e.intl_message(),
            Some("Failed to convert input string to UTF-16")
        );
    }
    assert_eq!(checker.get_skeleton(b"abc").unwrap(), b"abc");
    assert_eq!(checker.status(), UErrorCode::ZERO);
}

#[test]
fn spoofchecker_bidi_skeleton() {
    let mut c = new();
    assert_eq!(c.get_bidi_skeleton(LTR, b"").unwrap(), b"");
    let (arg, msg) = value_error(c.get_bidi_skeleton(RTL + 1, b"a").unwrap_err());
    assert_eq!(arg, 1);
    assert_eq!(msg, "must be either Spoofchecker::LTR or Spoofchecker::RTL");
    let mut eq = |d, a: &str, b: &str| {
        c.get_bidi_skeleton(d, a.as_bytes()).unwrap()
            == c.get_bidi_skeleton(d, b.as_bytes()).unwrap()
    };
    assert!(eq(LTR, "A1\u{05D0}", "A\u{05D0}1"));
    assert!(!eq(RTL, "A1\u{05D0}", "A\u{05D0}1"));
    assert!(!eq(LTR, "\u{05D0}A_1", "\u{05D0}1_A"));
    assert!(eq(RTL, "\u{05D0}A_1", "\u{05D0}1_A"));
    let e = c.get_bidi_skeleton(LTR, b"\x80").unwrap_err();
    assert_eq!(e.intl_code(), UErrorCode::INVALID_CHAR_FOUND);
}

#[test]
fn spoofchecker_ubidi() {
    let mut s = new();
    let (arg, _) = value_error(s.are_bidi_confusable(RTL + 1, b"a", b"a").unwrap_err());
    assert_eq!(arg, 1);
    let mut bc = |d, a: &str, b: &str| {
        s.are_bidi_confusable(d, a.as_bytes(), b.as_bytes())
            .unwrap()
    };
    assert!(bc(LTR, "A1\u{05D0}", "A\u{05D0}1").value);
    assert!(!bc(RTL, "A1\u{05D0}", "A\u{05D0}1").value);
    assert!(!bc(LTR, "\u{05D0}A_1", "\u{05D0}1_A").value);
    assert!(bc(RTL, "\u{05D0}A_1", "\u{05D0}1_A").value);
    assert!(!bc(LTR, "Mark_", "_Mark").value);
    assert!(!bc(RTL, "Mark_", "_Mark").value);
    let r = bc(LTR, "A1\u{05D0}", "A\u{05D0}1");
    assert_eq!(r.error_code, Some(checks::MIXED_SCRIPT_CONFUSABLE));
    assert_eq!(bc(LTR, "Mark_", "_Mark").error_code, Some(0));
    assert!(
        !s.are_confusable("A1\u{05D0}".as_bytes(), "A\u{05D0}1".as_bytes())
            .value
    );
    assert!(
        !s.are_confusable("\u{05D0}A_1".as_bytes(), "\u{05D0}1_A".as_bytes())
            .value
    );
}

#[test]
fn spoofchecker_clone_is_independent() {
    let mut a = new();
    a.set_allowed_chars(b"[a-z]", 0).expect("chars");
    let mut b = a.try_clone().expect("clone");
    assert!(b.is_suspicious(b"123").value);
    b.set_allowed_chars(b"[0-9]", 0).expect("chars");
    assert!(!b.is_suspicious(b"123").value);
    assert!(a.is_suspicious(b"123").value);
}

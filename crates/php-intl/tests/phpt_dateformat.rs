// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Expectations hand-ported from ext/intl/tests (dateformat/ and
//! datepatterngenerator_*.phpt).

use php_intl::datepatterngenerator::{base_skeleton, skeleton, IntlDatePatternGenerator};

#[test]
fn datepatterngenerator_get_skeleton() {
    assert_eq!(skeleton(b"dd/MMM").unwrap(), "MMMdd");
    assert_eq!(skeleton(b"MMM-dd").unwrap(), "MMMdd");
    assert_eq!(base_skeleton(b"dd/MMM").unwrap(), "MMMd");
    assert_eq!(base_skeleton(b"MMM-dd").unwrap(), "MMMd");
    assert_eq!(skeleton(b"").unwrap(), "");
    assert_eq!(base_skeleton(b"").unwrap(), "");
    for (p, s, b) in [
        ("'at' HH:mm", "HHmm", "Hm"),
        ("y年M月d日", "yMd", "yMd"),
        ("yyyy-MM-dd", "yyyyMMdd", "yMd"),
        ("MMMMM", "MMMMM", "MMMMM"),
        ("MMM", "MMM", "MMM"),
        ("HH:mm:ss zzz dd/MM/y", "yMMddHHmmsszzz", "yMdHmsz"),
        ("'at HH:mm", "", ""),
    ] {
        assert_eq!(skeleton(p.as_bytes()).unwrap(), s, "{p}");
        assert_eq!(base_skeleton(p.as_bytes()).unwrap(), b, "{p}");
    }
}

#[test]
fn datepatterngenerator_skeleton_error() {
    let e = skeleton(b"dd/MMM\x80").unwrap_err();
    assert_eq!(
        e.intl_message(),
        Some("Pattern is not a valid UTF-8 string")
    );
    assert_eq!(e.intl_code().name(), "U_INVALID_CHAR_FOUND");
    let e = base_skeleton(b"dd/MMM\x80").unwrap_err();
    assert_eq!(
        e.intl_message(),
        Some("Pattern is not a valid UTF-8 string")
    );
}

#[test]
fn datepatterngenerator_get_best_pattern() {
    let mut g = IntlDatePatternGenerator::new(Some("en_US")).unwrap();
    assert_eq!(g.best_pattern(b"jjmm").unwrap(), "h:mm\u{202f}a");
    let mut g = IntlDatePatternGenerator::new(Some("de_DE")).unwrap();
    assert_eq!(g.best_pattern(b"jjmm").unwrap(), "HH:mm");
}

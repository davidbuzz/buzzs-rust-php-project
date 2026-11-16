// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Ports of `ext/intl/tests/rangeformatter/*.phpt` (php-src master).
#![allow(clippy::too_many_lines)]

use php_intl::error::{Scope, UErrorCode};
use php_intl::rangeformatter::{
    IntlNumberRangeFormatter, RangeValue, COLLAPSE_ALL, COLLAPSE_AUTO, COLLAPSE_NONE,
    COLLAPSE_UNIT, IDENTITY_FALLBACK_APPROXIMATELY,
    IDENTITY_FALLBACK_APPROXIMATELY_OR_SINGLE_VALUE, IDENTITY_FALLBACK_RANGE,
    IDENTITY_FALLBACK_SINGLE_VALUE,
};
use php_intl::Error;

use RangeValue::{Float as F, Int as I};

fn create(sk: &str, loc: &str, c: i64, i: i64) -> IntlNumberRangeFormatter {
    IntlNumberRangeFormatter::create_from_skeleton(sk.as_bytes(), loc, c, i).expect("create")
}

fn fmt(f: &IntlNumberRangeFormatter, a: RangeValue, b: RangeValue) -> String {
    String::from_utf8(f.format(a, b).expect("format")).expect("utf8")
}

const BASIC: &[(&str, &str, &str, [&str; 7])] = &[
    (
        "en_US",
        "COLLAPSE_AUTO",
        "IDENTITY_FALLBACK_SINGLE_VALUE",
        [
            "1.1–2.2 m",
            "100–200 m",
            "-5 – 5 m",
            "5 m",
            "5.0001 m",
            "5–5.1 m",
            "5.1–6 m",
        ],
    ),
    (
        "en_US",
        "COLLAPSE_NONE",
        "IDENTITY_FALLBACK_SINGLE_VALUE",
        [
            "1.1 m – 2.2 m",
            "100 m – 200 m",
            "-5 m – 5 m",
            "5 m",
            "5.0001 m",
            "5 m – 5.1 m",
            "5.1 m – 6 m",
        ],
    ),
    (
        "en_US",
        "COLLAPSE_UNIT",
        "IDENTITY_FALLBACK_SINGLE_VALUE",
        [
            "1.1–2.2 m",
            "100–200 m",
            "-5 – 5 m",
            "5 m",
            "5.0001 m",
            "5–5.1 m",
            "5.1–6 m",
        ],
    ),
    (
        "en_US",
        "COLLAPSE_ALL",
        "IDENTITY_FALLBACK_SINGLE_VALUE",
        [
            "1.1–2.2 m",
            "100–200 m",
            "-5 – 5 m",
            "5 m",
            "5.0001 m",
            "5–5.1 m",
            "5.1–6 m",
        ],
    ),
    (
        "en_US",
        "COLLAPSE_AUTO",
        "IDENTITY_FALLBACK_APPROXIMATELY_OR_SINGLE_VALUE",
        [
            "1.1–2.2 m",
            "100–200 m",
            "-5 – 5 m",
            "5 m",
            "5.0001 m",
            "5–5.1 m",
            "5.1–6 m",
        ],
    ),
    (
        "en_US",
        "COLLAPSE_NONE",
        "IDENTITY_FALLBACK_APPROXIMATELY_OR_SINGLE_VALUE",
        [
            "1.1 m – 2.2 m",
            "100 m – 200 m",
            "-5 m – 5 m",
            "5 m",
            "5.0001 m",
            "5 m – 5.1 m",
            "5.1 m – 6 m",
        ],
    ),
    (
        "en_US",
        "COLLAPSE_UNIT",
        "IDENTITY_FALLBACK_APPROXIMATELY_OR_SINGLE_VALUE",
        [
            "1.1–2.2 m",
            "100–200 m",
            "-5 – 5 m",
            "5 m",
            "5.0001 m",
            "5–5.1 m",
            "5.1–6 m",
        ],
    ),
    (
        "en_US",
        "COLLAPSE_ALL",
        "IDENTITY_FALLBACK_APPROXIMATELY_OR_SINGLE_VALUE",
        [
            "1.1–2.2 m",
            "100–200 m",
            "-5 – 5 m",
            "5 m",
            "5.0001 m",
            "5–5.1 m",
            "5.1–6 m",
        ],
    ),
    (
        "en_US",
        "COLLAPSE_AUTO",
        "IDENTITY_FALLBACK_APPROXIMATELY",
        [
            "1.1–2.2 m",
            "100–200 m",
            "-5 – 5 m",
            "~5 m",
            "~5.0001 m",
            "5–5.1 m",
            "5.1–6 m",
        ],
    ),
    (
        "en_US",
        "COLLAPSE_NONE",
        "IDENTITY_FALLBACK_APPROXIMATELY",
        [
            "1.1 m – 2.2 m",
            "100 m – 200 m",
            "-5 m – 5 m",
            "~5 m",
            "~5.0001 m",
            "5 m – 5.1 m",
            "5.1 m – 6 m",
        ],
    ),
    (
        "en_US",
        "COLLAPSE_UNIT",
        "IDENTITY_FALLBACK_APPROXIMATELY",
        [
            "1.1–2.2 m",
            "100–200 m",
            "-5 – 5 m",
            "~5 m",
            "~5.0001 m",
            "5–5.1 m",
            "5.1–6 m",
        ],
    ),
    (
        "en_US",
        "COLLAPSE_ALL",
        "IDENTITY_FALLBACK_APPROXIMATELY",
        [
            "1.1–2.2 m",
            "100–200 m",
            "-5 – 5 m",
            "~5 m",
            "~5.0001 m",
            "5–5.1 m",
            "5.1–6 m",
        ],
    ),
    (
        "en_US",
        "COLLAPSE_AUTO",
        "IDENTITY_FALLBACK_RANGE",
        [
            "1.1–2.2 m",
            "100–200 m",
            "-5 – 5 m",
            "5–5 m",
            "5.0001–5.0001 m",
            "5–5.1 m",
            "5.1–6 m",
        ],
    ),
    (
        "en_US",
        "COLLAPSE_NONE",
        "IDENTITY_FALLBACK_RANGE",
        [
            "1.1 m – 2.2 m",
            "100 m – 200 m",
            "-5 m – 5 m",
            "5 m – 5 m",
            "5.0001 m – 5.0001 m",
            "5 m – 5.1 m",
            "5.1 m – 6 m",
        ],
    ),
    (
        "en_US",
        "COLLAPSE_UNIT",
        "IDENTITY_FALLBACK_RANGE",
        [
            "1.1–2.2 m",
            "100–200 m",
            "-5 – 5 m",
            "5–5 m",
            "5.0001–5.0001 m",
            "5–5.1 m",
            "5.1–6 m",
        ],
    ),
    (
        "en_US",
        "COLLAPSE_ALL",
        "IDENTITY_FALLBACK_RANGE",
        [
            "1.1–2.2 m",
            "100–200 m",
            "-5 – 5 m",
            "5–5 m",
            "5.0001–5.0001 m",
            "5–5.1 m",
            "5.1–6 m",
        ],
    ),
    (
        "RO",
        "COLLAPSE_AUTO",
        "IDENTITY_FALLBACK_SINGLE_VALUE",
        [
            "1,1 - 2,2 m",
            "100 - 200 m",
            "-5 - 5 m",
            "5 m",
            "5,0001 m",
            "5 - 5,1 m",
            "5,1 - 6 m",
        ],
    ),
    (
        "RO",
        "COLLAPSE_NONE",
        "IDENTITY_FALLBACK_SINGLE_VALUE",
        [
            "1,1 m - 2,2 m",
            "100 m - 200 m",
            "-5 m - 5 m",
            "5 m",
            "5,0001 m",
            "5 m - 5,1 m",
            "5,1 m - 6 m",
        ],
    ),
    (
        "RO",
        "COLLAPSE_UNIT",
        "IDENTITY_FALLBACK_SINGLE_VALUE",
        [
            "1,1 - 2,2 m",
            "100 - 200 m",
            "-5 - 5 m",
            "5 m",
            "5,0001 m",
            "5 - 5,1 m",
            "5,1 - 6 m",
        ],
    ),
    (
        "RO",
        "COLLAPSE_ALL",
        "IDENTITY_FALLBACK_SINGLE_VALUE",
        [
            "1,1 - 2,2 m",
            "100 - 200 m",
            "-5 - 5 m",
            "5 m",
            "5,0001 m",
            "5 - 5,1 m",
            "5,1 - 6 m",
        ],
    ),
    (
        "RO",
        "COLLAPSE_AUTO",
        "IDENTITY_FALLBACK_APPROXIMATELY_OR_SINGLE_VALUE",
        [
            "1,1 - 2,2 m",
            "100 - 200 m",
            "-5 - 5 m",
            "5 m",
            "5,0001 m",
            "5 - 5,1 m",
            "5,1 - 6 m",
        ],
    ),
    (
        "RO",
        "COLLAPSE_NONE",
        "IDENTITY_FALLBACK_APPROXIMATELY_OR_SINGLE_VALUE",
        [
            "1,1 m - 2,2 m",
            "100 m - 200 m",
            "-5 m - 5 m",
            "5 m",
            "5,0001 m",
            "5 m - 5,1 m",
            "5,1 m - 6 m",
        ],
    ),
    (
        "RO",
        "COLLAPSE_UNIT",
        "IDENTITY_FALLBACK_APPROXIMATELY_OR_SINGLE_VALUE",
        [
            "1,1 - 2,2 m",
            "100 - 200 m",
            "-5 - 5 m",
            "5 m",
            "5,0001 m",
            "5 - 5,1 m",
            "5,1 - 6 m",
        ],
    ),
    (
        "RO",
        "COLLAPSE_ALL",
        "IDENTITY_FALLBACK_APPROXIMATELY_OR_SINGLE_VALUE",
        [
            "1,1 - 2,2 m",
            "100 - 200 m",
            "-5 - 5 m",
            "5 m",
            "5,0001 m",
            "5 - 5,1 m",
            "5,1 - 6 m",
        ],
    ),
    (
        "RO",
        "COLLAPSE_AUTO",
        "IDENTITY_FALLBACK_APPROXIMATELY",
        [
            "1,1 - 2,2 m",
            "100 - 200 m",
            "-5 - 5 m",
            "~5 m",
            "~5,0001 m",
            "5 - 5,1 m",
            "5,1 - 6 m",
        ],
    ),
    (
        "RO",
        "COLLAPSE_NONE",
        "IDENTITY_FALLBACK_APPROXIMATELY",
        [
            "1,1 m - 2,2 m",
            "100 m - 200 m",
            "-5 m - 5 m",
            "~5 m",
            "~5,0001 m",
            "5 m - 5,1 m",
            "5,1 m - 6 m",
        ],
    ),
    (
        "RO",
        "COLLAPSE_UNIT",
        "IDENTITY_FALLBACK_APPROXIMATELY",
        [
            "1,1 - 2,2 m",
            "100 - 200 m",
            "-5 - 5 m",
            "~5 m",
            "~5,0001 m",
            "5 - 5,1 m",
            "5,1 - 6 m",
        ],
    ),
    (
        "RO",
        "COLLAPSE_ALL",
        "IDENTITY_FALLBACK_APPROXIMATELY",
        [
            "1,1 - 2,2 m",
            "100 - 200 m",
            "-5 - 5 m",
            "~5 m",
            "~5,0001 m",
            "5 - 5,1 m",
            "5,1 - 6 m",
        ],
    ),
    (
        "RO",
        "COLLAPSE_AUTO",
        "IDENTITY_FALLBACK_RANGE",
        [
            "1,1 - 2,2 m",
            "100 - 200 m",
            "-5 - 5 m",
            "5 - 5 m",
            "5,0001 - 5,0001 m",
            "5 - 5,1 m",
            "5,1 - 6 m",
        ],
    ),
    (
        "RO",
        "COLLAPSE_NONE",
        "IDENTITY_FALLBACK_RANGE",
        [
            "1,1 m - 2,2 m",
            "100 m - 200 m",
            "-5 m - 5 m",
            "5 m - 5 m",
            "5,0001 m - 5,0001 m",
            "5 m - 5,1 m",
            "5,1 m - 6 m",
        ],
    ),
    (
        "RO",
        "COLLAPSE_UNIT",
        "IDENTITY_FALLBACK_RANGE",
        [
            "1,1 - 2,2 m",
            "100 - 200 m",
            "-5 - 5 m",
            "5 - 5 m",
            "5,0001 - 5,0001 m",
            "5 - 5,1 m",
            "5,1 - 6 m",
        ],
    ),
    (
        "RO",
        "COLLAPSE_ALL",
        "IDENTITY_FALLBACK_RANGE",
        [
            "1,1 - 2,2 m",
            "100 - 200 m",
            "-5 - 5 m",
            "5 - 5 m",
            "5,0001 - 5,0001 m",
            "5 - 5,1 m",
            "5,1 - 6 m",
        ],
    ),
    (
        "JA",
        "COLLAPSE_AUTO",
        "IDENTITY_FALLBACK_SINGLE_VALUE",
        [
            "1.1～2.2 m",
            "100～200 m",
            "-5 ～ 5 m",
            "5 m",
            "5.0001 m",
            "5～5.1 m",
            "5.1～6 m",
        ],
    ),
    (
        "JA",
        "COLLAPSE_NONE",
        "IDENTITY_FALLBACK_SINGLE_VALUE",
        [
            "1.1 m ～ 2.2 m",
            "100 m ～ 200 m",
            "-5 m ～ 5 m",
            "5 m",
            "5.0001 m",
            "5 m ～ 5.1 m",
            "5.1 m ～ 6 m",
        ],
    ),
    (
        "JA",
        "COLLAPSE_UNIT",
        "IDENTITY_FALLBACK_SINGLE_VALUE",
        [
            "1.1～2.2 m",
            "100～200 m",
            "-5 ～ 5 m",
            "5 m",
            "5.0001 m",
            "5～5.1 m",
            "5.1～6 m",
        ],
    ),
    (
        "JA",
        "COLLAPSE_ALL",
        "IDENTITY_FALLBACK_SINGLE_VALUE",
        [
            "1.1～2.2 m",
            "100～200 m",
            "-5 ～ 5 m",
            "5 m",
            "5.0001 m",
            "5～5.1 m",
            "5.1～6 m",
        ],
    ),
    (
        "JA",
        "COLLAPSE_AUTO",
        "IDENTITY_FALLBACK_APPROXIMATELY_OR_SINGLE_VALUE",
        [
            "1.1～2.2 m",
            "100～200 m",
            "-5 ～ 5 m",
            "5 m",
            "5.0001 m",
            "5～5.1 m",
            "5.1～6 m",
        ],
    ),
    (
        "JA",
        "COLLAPSE_NONE",
        "IDENTITY_FALLBACK_APPROXIMATELY_OR_SINGLE_VALUE",
        [
            "1.1 m ～ 2.2 m",
            "100 m ～ 200 m",
            "-5 m ～ 5 m",
            "5 m",
            "5.0001 m",
            "5 m ～ 5.1 m",
            "5.1 m ～ 6 m",
        ],
    ),
    (
        "JA",
        "COLLAPSE_UNIT",
        "IDENTITY_FALLBACK_APPROXIMATELY_OR_SINGLE_VALUE",
        [
            "1.1～2.2 m",
            "100～200 m",
            "-5 ～ 5 m",
            "5 m",
            "5.0001 m",
            "5～5.1 m",
            "5.1～6 m",
        ],
    ),
    (
        "JA",
        "COLLAPSE_ALL",
        "IDENTITY_FALLBACK_APPROXIMATELY_OR_SINGLE_VALUE",
        [
            "1.1～2.2 m",
            "100～200 m",
            "-5 ～ 5 m",
            "5 m",
            "5.0001 m",
            "5～5.1 m",
            "5.1～6 m",
        ],
    ),
    (
        "JA",
        "COLLAPSE_AUTO",
        "IDENTITY_FALLBACK_APPROXIMATELY",
        [
            "1.1～2.2 m",
            "100～200 m",
            "-5 ～ 5 m",
            "約5 m",
            "約5.0001 m",
            "5～5.1 m",
            "5.1～6 m",
        ],
    ),
    (
        "JA",
        "COLLAPSE_NONE",
        "IDENTITY_FALLBACK_APPROXIMATELY",
        [
            "1.1 m ～ 2.2 m",
            "100 m ～ 200 m",
            "-5 m ～ 5 m",
            "約5 m",
            "約5.0001 m",
            "5 m ～ 5.1 m",
            "5.1 m ～ 6 m",
        ],
    ),
    (
        "JA",
        "COLLAPSE_UNIT",
        "IDENTITY_FALLBACK_APPROXIMATELY",
        [
            "1.1～2.2 m",
            "100～200 m",
            "-5 ～ 5 m",
            "約5 m",
            "約5.0001 m",
            "5～5.1 m",
            "5.1～6 m",
        ],
    ),
    (
        "JA",
        "COLLAPSE_ALL",
        "IDENTITY_FALLBACK_APPROXIMATELY",
        [
            "1.1～2.2 m",
            "100～200 m",
            "-5 ～ 5 m",
            "約5 m",
            "約5.0001 m",
            "5～5.1 m",
            "5.1～6 m",
        ],
    ),
    (
        "JA",
        "COLLAPSE_AUTO",
        "IDENTITY_FALLBACK_RANGE",
        [
            "1.1～2.2 m",
            "100～200 m",
            "-5 ～ 5 m",
            "5～5 m",
            "5.0001～5.0001 m",
            "5～5.1 m",
            "5.1～6 m",
        ],
    ),
    (
        "JA",
        "COLLAPSE_NONE",
        "IDENTITY_FALLBACK_RANGE",
        [
            "1.1 m ～ 2.2 m",
            "100 m ～ 200 m",
            "-5 m ～ 5 m",
            "5 m ～ 5 m",
            "5.0001 m ～ 5.0001 m",
            "5 m ～ 5.1 m",
            "5.1 m ～ 6 m",
        ],
    ),
    (
        "JA",
        "COLLAPSE_UNIT",
        "IDENTITY_FALLBACK_RANGE",
        [
            "1.1～2.2 m",
            "100～200 m",
            "-5 ～ 5 m",
            "5～5 m",
            "5.0001～5.0001 m",
            "5～5.1 m",
            "5.1～6 m",
        ],
    ),
    (
        "JA",
        "COLLAPSE_ALL",
        "IDENTITY_FALLBACK_RANGE",
        [
            "1.1～2.2 m",
            "100～200 m",
            "-5 ～ 5 m",
            "5～5 m",
            "5.0001～5.0001 m",
            "5～5.1 m",
            "5.1～6 m",
        ],
    ),
];

fn collapse(name: &str) -> i64 {
    match name {
        "COLLAPSE_AUTO" => COLLAPSE_AUTO,
        "COLLAPSE_NONE" => COLLAPSE_NONE,
        "COLLAPSE_UNIT" => COLLAPSE_UNIT,
        _ => COLLAPSE_ALL,
    }
}

fn identity(name: &str) -> i64 {
    match name {
        "IDENTITY_FALLBACK_SINGLE_VALUE" => IDENTITY_FALLBACK_SINGLE_VALUE,
        "IDENTITY_FALLBACK_APPROXIMATELY_OR_SINGLE_VALUE" => {
            IDENTITY_FALLBACK_APPROXIMATELY_OR_SINGLE_VALUE
        }
        "IDENTITY_FALLBACK_APPROXIMATELY" => IDENTITY_FALLBACK_APPROXIMATELY,
        _ => IDENTITY_FALLBACK_RANGE,
    }
}

#[test]
fn basic() {
    let inputs = [
        (F(1.1), F(2.2)),
        (I(100), I(200)),
        (I(-5), I(5)),
        (I(5), I(5)),
        (F(5.0001), F(5.0001)),
        (I(5), F(5.1)),
        (F(5.1), I(6)),
    ];
    for (lang, c, i, want) in BASIC {
        let f = create("measure-unit/length-meter", lang, collapse(c), identity(i));
        for (k, (a, b)) in inputs.iter().enumerate() {
            assert_eq!(fmt(&f, *a, *b), want[k], "{lang} {c} {i} #{k}");
        }
    }
}

#[test]
fn errors() {
    let e = IntlNumberRangeFormatter::create_from_skeleton(
        b"invalid skeleton here",
        "ro",
        COLLAPSE_AUTO,
        IDENTITY_FALLBACK_SINGLE_VALUE,
    )
    .unwrap_err();
    match e {
        Error::Intl(ie) => {
            assert_eq!(ie.code, UErrorCode(65811));
            assert_eq!(
                ie.message.as_deref(),
                Some("Failed to create the number skeleton")
            );
            assert_eq!(ie.scope, Scope::Global);
            assert!(ie.always_throw);
        }
        other => panic!("{other:?}"),
    }
    let value_err =
        |sk: &[u8], loc: &str, c, i| match IntlNumberRangeFormatter::create_from_skeleton(
            sk, loc, c, i,
        )
        .unwrap_err()
        {
            Error::ArgumentValue { arg, message } => (arg, message),
            other => panic!("{other:?}"),
        };
    let (arg, msg) = value_err(b"x", "ro", 34, IDENTITY_FALLBACK_SINGLE_VALUE);
    assert_eq!(arg, 3);
    assert_eq!(msg, "must be one of IntlNumberRangeFormatter::COLLAPSE_AUTO, IntlNumberRangeFormatter::COLLAPSE_NONE, IntlNumberRangeFormatter::COLLAPSE_UNIT, or IntlNumberRangeFormatter::COLLAPSE_ALL");
    let (arg, msg) = value_err(b"x", "ro", COLLAPSE_AUTO, 343);
    assert_eq!(arg, 4);
    assert_eq!(msg, "must be one of IntlNumberRangeFormatter::IDENTITY_FALLBACK_SINGLE_VALUE, IntlNumberRangeFormatter::IDENTITY_FALLBACK_APPROXIMATELY_OR_SINGLE_VALUE, IntlNumberRangeFormatter::IDENTITY_FALLBACK_APPROXIMATELY, or IntlNumberRangeFormatter::IDENTITY_FALLBACK_RANGE");
    let (arg, msg) = value_err(
        b"x",
        "invalid-language",
        COLLAPSE_AUTO,
        IDENTITY_FALLBACK_SINGLE_VALUE,
    );
    assert_eq!((arg, msg.as_str()), (2, "\"invalid-language\" is invalid"));
    let long = "ro_thisiswaytooooooooooooooooooooooooooooooooooooooooooooolongtobevaliditneedstobeatleast157characterstofailthevalidationinthelistformattercodeimplementation";
    let (arg, msg) = value_err(b"x", long, COLLAPSE_AUTO, IDENTITY_FALLBACK_SINGLE_VALUE);
    assert_eq!(
        (arg, msg.as_str()),
        (2, "must be no longer than 156 characters")
    );
}

#[test]
fn format_failure() {
    let f = create(
        "",
        "en_US@numbers=foobar",
        COLLAPSE_AUTO,
        IDENTITY_FALLBACK_RANGE,
    );
    match f.format(I(1), I(2)).unwrap_err() {
        Error::Intl(ie) => {
            assert!(ie.code.is_failure());
            assert!(ie.always_throw);
            assert_eq!(ie.scope, Scope::Both);
            assert_eq!(ie.message.as_deref(), Some("Failed to format number range"));
        }
        other => panic!("{other:?}"),
    }
    let f = create("", "en_US", COLLAPSE_AUTO, IDENTITY_FALLBACK_RANGE);
    assert!(!fmt(&f, I(1), I(2)).is_empty());
}

#[test]
fn format_int64() {
    let f = create("", "en_US", COLLAPSE_AUTO, IDENTITY_FALLBACK_SINGLE_VALUE);
    let s = fmt(&f, I(9_007_199_254_740_993), I(9_007_199_254_740_993));
    let digits: String = s.chars().filter(char::is_ascii_digit).collect();
    assert_eq!(digits, "9007199254740993");
}

#[test]
fn fraction_skeleton() {
    let want = [
        (".##", ["5.1–5.2", "5.01–5.02", "~5"]),
        (".#", ["5.1–5.2", "~5", "~5"]),
    ];
    for (sk, w) in want {
        let f = create(sk, "en_GB", COLLAPSE_AUTO, IDENTITY_FALLBACK_APPROXIMATELY);
        assert_eq!(fmt(&f, F(5.1), F(5.2)), w[0]);
        assert_eq!(fmt(&f, F(5.01), F(5.02)), w[1]);
        assert_eq!(fmt(&f, F(5.001), F(5.002)), w[2]);
    }
}

#[test]
fn with_empty_skeleton() {
    let f = create("", "en_GB", COLLAPSE_AUTO, IDENTITY_FALLBACK_APPROXIMATELY);
    assert_eq!(fmt(&f, F(5.1), F(5.2)), "5.1–5.2");
}

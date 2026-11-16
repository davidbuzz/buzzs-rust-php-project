// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Ports of `ext/intl/tests/collator_sort_conversion_error*.phpt`
//! (php-src master).

use php_intl::collator::{sort_flags, Collator, SortValue};
use php_intl::error::UErrorCode;

fn coll() -> Collator {
    Collator::new("en_US").expect("collator").0
}

const BAD: SortValue<'static> = SortValue::Object {
    as_string: Some(b"\xFF"),
};

#[test]
fn sort_string_conversion_error() {
    let c = coll();
    let e = c
        .sort_order(
            &[SortValue::Str(b"b"), BAD, SortValue::Str(b"a")],
            sort_flags::STRING,
        )
        .unwrap_err();
    assert_eq!(e.intl_code(), UErrorCode::INVALID_CHAR_FOUND);
    assert_eq!(
        e.intl_message(),
        Some("Error converting string from UTF-8 to UTF-16")
    );
}

#[test]
fn sort_regular_conversion_error() {
    let c = coll();
    for values in [
        vec![BAD, SortValue::Int(1)],
        vec![SortValue::Str(b"a"), BAD],
    ] {
        let e = c.sort_order(&values, sort_flags::REGULAR).unwrap_err();
        assert_eq!(e.intl_code(), UErrorCode::INVALID_CHAR_FOUND);
        assert_eq!(
            e.intl_message(),
            Some("Error converting object string from UTF-8 to UTF-16")
        );
    }
    let order = c
        .sort_order(
            &[SortValue::Str(b"10"), SortValue::Str(b"9")],
            sort_flags::REGULAR,
        )
        .expect("sort");
    assert_eq!(order, [1, 0]);
    let order = c
        .sort_order(
            &[SortValue::Str(b"b"), SortValue::Str(b"9")],
            sort_flags::REGULAR,
        )
        .expect("sort");
    assert_eq!(order, [1, 0]);
}

#[test]
fn good_objects_sort_as_strings() {
    let c = coll();
    let order = c
        .sort_order(
            &[
                SortValue::Str(b"b"),
                SortValue::Object {
                    as_string: Some(b"a"),
                },
            ],
            sort_flags::STRING,
        )
        .expect("sort");
    assert_eq!(order, [1, 0]);
}

#[test]
fn gh18566_regular_sort_numeric_strings_with_spaces() {
    let c = Collator::new("en").expect("collator").0;
    let arr: [&[u8]; 9] = [
        b"2023-02-04 14:00:00",
        b"2023-01-08 12:00:00",
        b"2023-01-03 12:00:00",
        b"2023-01-03 12:00:00",
        b"2021-01-03 12:00:00",
        b"2023-01-05 14:00:00",
        b"2024-01-03 12:00:00",
        b"2023-01-03 12:00:00",
        b"   ",
    ];
    let v: Vec<_> = arr.iter().map(|s| SortValue::Str(s)).collect();
    assert_eq!(
        c.sort_order(&v, sort_flags::REGULAR).expect("sort"),
        [8, 4, 2, 3, 7, 5, 1, 0, 6]
    );
    let arr: [&[u8]; 3] = [b"   100000", b"  10", b"  -100  "];
    let v: Vec<_> = arr.iter().map(|s| SortValue::Str(s)).collect();
    assert_eq!(
        c.sort_order(&v, sort_flags::REGULAR).expect("sort"),
        [2, 1, 0]
    );
}

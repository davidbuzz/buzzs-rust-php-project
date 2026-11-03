//! Ports of `ext/intl/tests/listformatter/*.phpt` (php-src master).

use php_intl::error::UErrorCode;
use php_intl::listformatter::{
    IntlListFormatter, TYPE_AND, TYPE_OR, TYPE_UNITS, WIDTH_NARROW, WIDTH_SHORT, WIDTH_WIDE,
};
use php_intl::Error;

fn fmt(locale: &str, t: i64, w: i64, items: &[&str]) -> String {
    let f = IntlListFormatter::new(locale, t, w).expect("ctor");
    String::from_utf8(f.format(items).expect("format")).expect("utf8")
}

#[test]
fn listformatter_basic() {
    let one_two_three = ["1", "2", "3"];
    assert_eq!(
        fmt("EN_US", TYPE_AND, WIDTH_WIDE, &one_two_three),
        "1, 2, and 3"
    );
    assert_eq!(
        fmt("EN_US", TYPE_AND, WIDTH_WIDE, &["1.2", "2.3", "3.4"]),
        "1.2, 2.3, and 3.4"
    );
    assert_eq!(
        fmt("EN_US", TYPE_AND, WIDTH_WIDE, &["test", "test2", "test"]),
        "test, test2, and test"
    );
    // [null, true, false] stringify to "", "1", "".
    assert_eq!(
        fmt("EN_US", TYPE_AND, WIDTH_WIDE, &["", "1", ""]),
        ", 1, and "
    );
    assert_eq!(
        fmt("EN_US", TYPE_AND, WIDTH_WIDE, &["1", "foo"]),
        "1 and foo"
    );
    assert_eq!(fmt("FR", TYPE_AND, WIDTH_WIDE, &one_two_three), "1, 2 et 3");
    assert_eq!(
        fmt("FR", TYPE_AND, WIDTH_WIDE, &["1.2", "2.3", "3.4"]),
        "1.2, 2.3 et 3.4"
    );
}

#[test]
fn listformatter_with_parameters() {
    let cases: &[(&str, [&str; 9])] = &[
        (
            "EN_US",
            [
                "1, 2, and 3",
                "1, 2, & 3",
                "1, 2, 3",
                "1, 2, or 3",
                "1, 2, or 3",
                "1, 2, or 3",
                "1, 2, 3",
                "1, 2, 3",
                "1 2 3",
            ],
        ),
        (
            "en_GB",
            [
                "1, 2 and 3",
                "1, 2 and 3",
                "1, 2, 3",
                "1, 2 or 3",
                "1, 2 or 3",
                "1, 2 or 3",
                "1, 2, 3",
                "1, 2, 3",
                "1 2 3",
            ],
        ),
        (
            "FR",
            [
                "1, 2 et 3",
                "1, 2 et 3",
                "1, 2, 3",
                "1, 2 ou 3",
                "1, 2 ou 3",
                "1, 2 ou 3",
                "1, 2 et 3",
                "1, 2 et 3",
                "1 2 3",
            ],
        ),
    ];
    for (loc, want) in cases {
        let mut k = 0;
        for t in [TYPE_AND, TYPE_OR, TYPE_UNITS] {
            for w in [WIDTH_WIDE, WIDTH_SHORT, WIDTH_NARROW] {
                assert_eq!(fmt(loc, t, w, &["1", "2", "3"]), want[k], "{loc} {t} {w}");
                k += 1;
            }
        }
    }
}

fn value_error(e: Error) -> (u32, String) {
    match e {
        Error::ArgumentValue { arg, message } => (arg, message),
        other => panic!("expected ValueError, got {other:?}"),
    }
}

#[test]
fn listformatter_error() {
    let e = IntlListFormatter::new("f", TYPE_AND, WIDTH_WIDE).unwrap_err();
    assert_eq!(value_error(e), (1, "\"f\" is invalid".to_owned()));
    let long = "ro_thisiswaytooooooooooooooooooooooooooooooooooooooooooooolongtobevaliditneedstobeatleast157characterstofailthevalidationinthelistformattercodeimplementation";
    let e = IntlListFormatter::new(long, TYPE_AND, WIDTH_WIDE).unwrap_err();
    assert_eq!(
        value_error(e),
        (1, "must be less than or equal to 156 characters".to_owned())
    );
}

#[test]
fn listformatter_with_parameters_error() {
    let e = IntlListFormatter::new("ro", 23_232_323, WIDTH_WIDE).unwrap_err();
    assert_eq!(
        value_error(e),
        (
            2,
            "must be one of IntlListFormatter::TYPE_AND, IntlListFormatter::TYPE_OR, or IntlListFormatter::TYPE_UNITS".to_owned()
        )
    );
    let e = IntlListFormatter::new("ro", TYPE_AND, 2_323_232).unwrap_err();
    assert_eq!(
        value_error(e),
        (
            3,
            "must be one of IntlListFormatter::WIDTH_WIDE, IntlListFormatter::WIDTH_SHORT, or IntlListFormatter::WIDTH_NARROW".to_owned()
        )
    );
}

#[test]
fn listformatter_get_error() {
    let f = IntlListFormatter::new("en_US", TYPE_AND, WIDTH_WIDE).expect("ctor");
    let e = f.format(&[b"\x80".as_slice()]).unwrap_err();
    assert_eq!(e.intl_code(), UErrorCode::INVALID_CHAR_FOUND);
    assert_eq!(e.intl_message(), Some("Failed to convert string to UTF-16"));
    assert_eq!(f.format(&["a", "b"]).expect("ok"), b"a and b");
}

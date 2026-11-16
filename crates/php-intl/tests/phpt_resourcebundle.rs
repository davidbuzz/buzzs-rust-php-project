// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Expectations hand-ported from ext/intl/tests/resourcebundle_*.phpt.

use php_intl::resourcebundle::{Offset, ResourceBundle, ResourceValue};
use php_intl::UErrorCode;

fn bundle() -> String {
    format!(
        "{}/../../modules/php-src/ext/intl/tests/_files/resourcebundle_le",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn s(v: ResourceValue) -> String {
    match v {
        ResourceValue::Str(s) => s,
        other => panic!("{other:?}"),
    }
}

#[test]
fn resourcebundle_individual() {
    let b = bundle();
    let (r, _) = ResourceBundle::new(Some("en_US"), Some(&b), true).unwrap();
    assert_eq!(r.count(), 6);
    assert_eq!(
        s(r.get(Offset::Key(b"teststring"), true, Some(1)).unwrap().0),
        "Hello World!"
    );
    assert!(matches!(
        r.get(Offset::Key(b"testint"), true, Some(1)).unwrap().0,
        ResourceValue::Int(2)
    ));
    match r.get(Offset::Key(b"testvector"), true, Some(1)).unwrap().0 {
        ResourceValue::IntVector(v) => assert_eq!(v, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 0]),
        o => panic!("{o:?}"),
    }
    match r.get(Offset::Key(b"testbin"), true, Some(1)).unwrap().0 {
        ResourceValue::Binary(v) => assert_eq!(v, [0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x78, 0x90]),
        o => panic!("{o:?}"),
    }
    let ResourceValue::Bundle(t) = r.get(Offset::Key(b"testtable"), true, Some(1)).unwrap().0
    else {
        panic!()
    };
    assert!(matches!(
        t.get(Offset::Key(b"major"), true, Some(1)).unwrap().0,
        ResourceValue::Int(3)
    ));
    let ResourceValue::Bundle(a) = r.get(Offset::Key(b"testarray"), true, Some(1)).unwrap().0
    else {
        panic!()
    };
    assert_eq!(
        s(a.get(Offset::Index(2), true, Some(1)).unwrap().0),
        "string 3"
    );
}

#[test]
fn resourcebundle_create() {
    let b = bundle();
    let (r, st) = ResourceBundle::new(Some("root"), Some(&b), true).unwrap();
    assert_eq!(st, UErrorCode::ZERO);
    assert_eq!(
        s(r.get(Offset::Key(b"teststring"), true, None).unwrap().0),
        "Hello World!"
    );
    let (r, st) = ResourceBundle::new(Some("es"), Some(&b), true).unwrap();
    assert_eq!(st, UErrorCode::ZERO);
    assert_eq!(
        s(r.get(Offset::Key(b"teststring"), true, None).unwrap().0),
        "Hola Mundo!"
    );
    let (_, st) = ResourceBundle::new(Some("en_US"), Some(&b), true).unwrap();
    assert_eq!(st, UErrorCode::USING_DEFAULT_WARNING);
    let e = ResourceBundle::new(Some("en_US"), Some(&b), false).unwrap_err();
    assert_eq!(e.intl_code(), UErrorCode::MISSING_RESOURCE_ERROR);
    assert_eq!(e.intl_message(), Some("Cannot load libICU resource bundle"));
}

#[test]
fn resourcebundle_iterator() {
    let b = bundle();
    let (r, _) = ResourceBundle::new(Some("en_US"), Some(&b), true).unwrap();
    assert!(r.is_table());
    let keys: Vec<String> = (0..r.count())
        .map(|i| r.entry(i).unwrap().0.unwrap())
        .collect();
    assert_eq!(
        keys,
        [
            "testarray",
            "testbin",
            "testint",
            "teststring",
            "testtable",
            "testvector"
        ]
    );
    let ResourceValue::Bundle(a) = r.get(Offset::Key(b"testarray"), true, Some(1)).unwrap().0
    else {
        panic!()
    };
    assert!(!a.is_table());
    let vals: Vec<String> = (0..a.count()).map(|i| s(a.entry(i).unwrap().1)).collect();
    assert_eq!(vals, ["string 1", "string 2", "string 3"]);
}

#[test]
fn resourcebundle_offsets() {
    let b = bundle();
    let (r, _) = ResourceBundle::new(Some("root"), Some(&b), true).unwrap();
    assert_eq!(
        r.get(Offset::Key(b""), true, None).unwrap_err(),
        php_intl::Error::Value("Offset must not be empty".into())
    );
    assert_eq!(
        r.get(Offset::Key(b""), true, Some(1)).unwrap_err(),
        php_intl::Error::arg_value(1, "must not be empty")
    );
    let e = r.get(Offset::Index(99), true, Some(1)).unwrap_err();
    assert_eq!(e.intl_message(), Some("Cannot load resource element 99"));
}

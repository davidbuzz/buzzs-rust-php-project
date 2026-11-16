// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Object (`O:`/`C:`/`r:`) unserialize → serialize round trips through
//! `CollectingContext`'s plain-object hooks. Expected strings and error
//! offsets come from the PHP oracle.

use php_runtime::serialize::{serialize, unserialize, UnserializeOptions};
use php_runtime::{CollectingContext, Obj, PlainObject, Value};

fn roundtrip(input: &[u8]) -> (Value, Vec<u8>) {
    let mut ctx = CollectingContext::new();
    let v = unserialize(input, &UnserializeOptions::default(), &mut ctx)
        .unwrap()
        .unwrap_or_else(|| panic!("failed: {:?}", ctx.messages()));
    assert!(ctx.messages().is_empty(), "{:?}", ctx.messages());
    let out = serialize(&v, &mut ctx).unwrap();
    (v, out)
}

fn errors(input: &[u8]) -> Vec<String> {
    let mut ctx = CollectingContext::new();
    let v = unserialize(input, &UnserializeOptions::default(), &mut ctx).unwrap();
    if v.is_some() {
        assert!(!ctx.messages().is_empty(), "unexpected success");
    }
    ctx.messages()
}

#[test]
fn object_roundtrips() {
    for s in [
        &b"O:8:\"stdClass\":0:{}"[..],
        b"O:8:\"stdClass\":2:{s:1:\"a\";i:1;s:1:\"b\";a:0:{}}",
        b"O:8:\"stdClass\":2:{s:4:\"self\";r:1;s:3:\"arr\";a:1:{i:0;r:1;}}",
        b"O:1:\"B\":3:{s:1:\"x\";i:1;s:4:\"\0*\0y\";i:2;s:4:\"\0B\0z\";i:3;}",
        b"a:2:{i:0;O:8:\"stdClass\":0:{}i:1;r:2;}",
        b"C:3:\"Foo\":5:{hello}",
        b"a:2:{i:0;C:3:\"Foo\":0:{}i:1;r:2;}",
    ] {
        let (_, out) = roundtrip(s);
        assert_eq!(String::from_utf8_lossy(&out), String::from_utf8_lossy(s));
    }
    // Integer property keys become string keys (PHP prints s:1:"0").
    let (_, out) = roundtrip(b"O:8:\"stdClass\":1:{i:0;i:1;}");
    assert_eq!(out, b"O:8:\"stdClass\":1:{s:1:\"0\";i:1;}");
}

#[test]
fn back_references_share_the_object() {
    let (v, _) = roundtrip(b"O:8:\"stdClass\":2:{s:4:\"self\";r:1;s:3:\"arr\";a:1:{i:0;r:1;}}");
    let o = v.as_object().unwrap();
    let props = o.downcast_ref::<PlainObject>().unwrap().props();
    let Some(Value::Object(inner)) = props.get_str(b"self") else {
        panic!("self property missing")
    };
    assert!(Obj::ptr_eq(o, inner));
    let (v, _) = roundtrip(b"C:3:\"Foo\":5:{hello}");
    let p = v
        .as_object()
        .unwrap()
        .downcast_ref::<PlainObject>()
        .unwrap();
    assert_eq!(p.custom_data().as_deref(), Some(&b"hello"[..]));
    // Break the Rc cycle of the self-referencing object.
    o.downcast_ref::<PlainObject>()
        .unwrap()
        .set_props(php_runtime::ZArray::new());
}

#[test]
fn object_error_offsets_match_php() {
    let cases: [(&[u8], &str); 7] = [
        (
            b"O:8:\"stdClass\":1:{s:1:\"a\";}",
            "Error at offset 26 of 27 bytes",
        ),
        (b"O:8:\"stdClass\":0:{", "Error at offset 18 of 18 bytes"),
        (b"O:9:\"stdClass\":0:{}", "Error at offset 14 of 19 bytes"),
        (b"O:8:\"stdClass\"0:{}", "Error at offset 14 of 18 bytes"),
        (
            b"C:8:\"stdClass\":10:{hello}",
            "Error at offset 19 of 25 bytes",
        ),
        (
            b"C:8:\"stdClass\":5:{hello",
            "Error at offset 18 of 23 bytes",
        ),
        (
            b"O:8:\"stdClass\":1:{s:1:\"a\";i:1;}x",
            "Extra data starting at offset 31 of 32 bytes",
        ),
    ];
    for (input, msg) in cases {
        let msgs = errors(input);
        assert_eq!(
            msgs.last().map(String::as_str),
            Some(format!("Warning: unserialize(): {msg}").as_str()),
            "{:?}",
            String::from_utf8_lossy(input)
        );
    }
    assert_eq!(
        errors(b"C:8:\"stdClass\":10:{hello}")[0],
        "Warning: Insufficient data for unserializing - 10 required, 6 present"
    );
}

#[test]
fn property_write_and_object_cast() {
    let mut ctx = CollectingContext::new();
    let arr = php_runtime::ZArray::from_values([Value::Int(5)]);
    let o = Value::Array(arr).to_object(&mut ctx).unwrap();
    let obj = o.as_object().unwrap();
    php_runtime::RuntimeContext::property_write(
        &mut ctx,
        obj,
        &php_runtime::ZStr::from("x"),
        Value::Int(7),
    )
    .unwrap();
    let mut out = Vec::new();
    php_runtime::dump::var_dump(&mut out, &o, &mut ctx).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.starts_with("object(stdClass)#"), "{text}");
    assert!(
        text.ends_with(" (2) {\n  [\"0\"]=>\n  int(5)\n  [\"x\"]=>\n  int(7)\n}\n"),
        "{text}"
    );
}

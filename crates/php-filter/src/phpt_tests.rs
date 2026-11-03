//! Unit tests mirroring the `ext/filter/tests` cases the PHP 8.3 oracle
//! cannot record: request input (`--GET--`/`--POST--`/`--COOKIE--`), the
//! master-only `FILTER_THROW_ON_FAILURE`, references and ini handling.

#![allow(clippy::many_single_char_names, clippy::too_many_lines)]

use crate::constants::*;
use crate::engine::{FilterArgs, FilterInputs};
use crate::test_host::{object, TestHost, FAILED_PREFIX};
use crate::{
    apply_default_filter, filter_has_var, filter_input, filter_input_array, filter_var,
    filter_var_array, parse_default_filter_ini, parse_default_flags_ini,
};
use php_runtime::dump::var_export;
use php_runtime::{CollectingContext, RuntimeError, Value, ZArray};

fn arr(pairs: &[(&str, Value)]) -> ZArray {
    ZArray::from_pairs(pairs.iter().map(|(k, v)| (*k, v.clone())))
}

fn s(v: &str) -> Value {
    Value::from(v)
}

fn export(v: &Value) -> String {
    let mut ctx = CollectingContext::new();
    String::from_utf8(var_export(v, &mut ctx).expect("var_export")).expect("utf8")
}

fn opts(pairs: &[(&str, Value)]) -> FilterArgs {
    FilterArgs::Array(arr(pairs))
}

fn failed(e: &RuntimeError) -> String {
    match e {
        RuntimeError::Error(m) => m.strip_prefix(FAILED_PREFIX).map_or_else(
            || format!("Error: {m}"),
            |r| format!("Filter\\FilterFailedException: {r}"),
        ),
        other => format!(
            "{}: {}",
            other.class_name().unwrap_or("?"),
            other.message().unwrap_or("")
        ),
    }
}

fn get_inputs() -> FilterInputs {
    // a=1&b=&c=3 style GET plus a POST array.
    FilterInputs {
        get: Some(arr(&[
            ("a", s("1")),
            ("b", s("")),
            ("c", s("3")),
            ("abc", s("<a>href</a>")),
            (
                "ar",
                Value::Array(arr(&[("elm1", s("1234")), ("elm2", s("0660"))])),
            ),
            ("o", s("0234")),
            ("email", s("daniel.e.scherzer@gmail.com")),
        ])),
        post: Some(arr(&[
            ("ap", Value::Array(ZArray::from_values([s("1")]))),
            ("bp", s("test")),
            ("cp", s("")),
            ("d", s("12345.7")),
            ("p", s("<p>string</p>")),
        ])),
        ..FilterInputs::default()
    }
}

#[test]
fn has_var_007_040() {
    let i = get_inputs();
    assert!(filter_has_var(&i, INPUT_GET, b"a").unwrap());
    assert!(filter_has_var(&i, INPUT_GET, b"abc").unwrap());
    assert!(!filter_has_var(&i, INPUT_GET, b"nonex").unwrap());
    assert!(!filter_has_var(&i, INPUT_GET, b" ").unwrap());
    assert!(!filter_has_var(&i, INPUT_GET, b"").unwrap());
    assert!(filter_has_var(&i, INPUT_POST, b"ap").unwrap());
    assert!(filter_has_var(&i, INPUT_POST, b"cp").unwrap());
    assert!(!filter_has_var(&i, INPUT_COOKIE, b"a").unwrap());
    let e = filter_has_var(&i, -1, b"cc").unwrap_err();
    assert_eq!(
        e.message(),
        Some("filter_has_var(): Argument #1 ($input_type) must be an INPUT_* constant")
    );
}

#[test]
fn filter_input_011_035_037() {
    let i = get_inputs();
    let mut h = TestHost::default();
    let v = filter_input(
        &mut h,
        &i,
        INPUT_GET,
        b"abc",
        FILTER_SANITIZE_SPECIAL_CHARS,
        &FilterArgs::Long(0),
    )
    .unwrap();
    assert_eq!(export(&v), "'&#60;a&#62;href&#60;/a&#62;'");
    let v = filter_input(
        &mut h,
        &i,
        INPUT_POST,
        b"d",
        FILTER_VALIDATE_FLOAT,
        &FilterArgs::Long(0),
    )
    .unwrap();
    assert_eq!(export(&v), "12345.7");
    let v = filter_input(
        &mut h,
        &i,
        INPUT_POST,
        b"d",
        FILTER_VALIDATE_INT,
        &FilterArgs::Long(0),
    )
    .unwrap();
    assert_eq!(export(&v), "false");
    // 035.phpt
    let v = filter_input(
        &mut h,
        &i,
        INPUT_GET,
        b"o",
        FILTER_VALIDATE_INT,
        &FilterArgs::Long(0),
    )
    .unwrap();
    assert_eq!(export(&v), "false");
    let o = opts(&[("flags", Value::Int(FILTER_FLAG_ALLOW_OCTAL))]);
    let v = filter_input(&mut h, &i, INPUT_GET, b"o", FILTER_VALIDATE_INT, &o).unwrap();
    assert_eq!(export(&v), "156");
    let o = opts(&[("flags", Value::Int(FILTER_REQUIRE_ARRAY))]);
    let v = filter_input(&mut h, &i, INPUT_GET, b"ar", FILTER_VALIDATE_INT, &o).unwrap();
    assert_eq!(
        export(&v),
        "array (\n  'elm1' => 1234,\n  'elm2' => false,\n)"
    );
    let o = opts(&[(
        "flags",
        Value::Int(FILTER_FLAG_ALLOW_OCTAL | FILTER_REQUIRE_ARRAY),
    )]);
    let v = filter_input(&mut h, &i, INPUT_GET, b"ar", FILTER_VALIDATE_INT, &o).unwrap();
    assert_eq!(
        export(&v),
        "array (\n  'elm1' => 1234,\n  'elm2' => 432,\n)"
    );
    // 037.phpt: callbacks through filter_input / filter_input_array.
    let cb = opts(&[("options", s("strtoupper"))]);
    let v = filter_input(&mut h, &i, INPUT_GET, b"abc", FILTER_CALLBACK, &cb).unwrap();
    assert_eq!(export(&v), "'<A>HREF</A>'");
    let def = opts(&[
        (
            "abc",
            Value::Array(arr(&[
                ("filter", Value::Int(FILTER_CALLBACK)),
                ("options", s("strtoupper")),
            ])),
        ),
        ("c", Value::Int(FILTER_VALIDATE_INT)),
        ("zz", Value::Int(FILTER_VALIDATE_INT)),
    ]);
    let v = filter_input_array(&mut h, &i, INPUT_GET, &def, true).unwrap();
    assert_eq!(
        export(&v),
        "array (\n  'abc' => '<A>HREF</A>',\n  'c' => 3,\n  'zz' => NULL,\n)"
    );
    // Uninitialized storage.
    let v = filter_input_array(
        &mut h,
        &i,
        INPUT_COOKIE,
        &FilterArgs::Long(FILTER_DEFAULT),
        true,
    )
    .unwrap();
    assert!(v.is_null());
    assert!(h.diagnostics.is_empty());
}

#[test]
fn filter_input_missing_values() {
    let i = get_inputs();
    let mut h = TestHost::default();
    let v = filter_input(
        &mut h,
        &i,
        INPUT_GET,
        b"nope",
        FILTER_VALIDATE_INT,
        &FilterArgs::Long(0),
    )
    .unwrap();
    assert!(v.is_null());
    let v = filter_input(
        &mut h,
        &i,
        INPUT_GET,
        b"nope",
        FILTER_VALIDATE_INT,
        &FilterArgs::Long(FILTER_NULL_ON_FAILURE),
    )
    .unwrap();
    assert_eq!(export(&v), "false");
    let o = opts(&[("options", Value::Array(arr(&[("default", Value::Int(5))])))]);
    let v = filter_input(&mut h, &i, INPUT_GET, b"nope", FILTER_VALIDATE_INT, &o).unwrap();
    assert_eq!(export(&v), "5");
    let v = filter_input(&mut h, &i, INPUT_GET, b"a", 9999, &FilterArgs::Long(0)).unwrap();
    assert_eq!(export(&v), "false");
    assert_eq!(
        h.messages(),
        ["Warning: filter_input(): Unknown filter with ID 9999"]
    );
    let e = filter_input(&mut h, &i, 3, b"a", FILTER_DEFAULT, &FilterArgs::Long(0)).unwrap_err();
    assert_eq!(
        e.message(),
        Some("filter_input(): Argument #1 ($type) must be an INPUT_* constant")
    );
}

#[test]
fn throw_on_failure() {
    let i = get_inputs();
    let mut h = TestHost::default();
    let t = FILTER_THROW_ON_FAILURE;
    // filter_input_failure.phpt
    let e = filter_input(
        &mut h,
        &i,
        INPUT_GET,
        b"b2",
        FILTER_DEFAULT,
        &FilterArgs::Long(t),
    )
    .unwrap_err();
    assert_eq!(
        failed(&e),
        "Filter\\FilterFailedException: input value 'b2' not found"
    );
    let e = filter_input(
        &mut h,
        &i,
        INPUT_GET,
        b"a",
        FILTER_DEFAULT,
        &FilterArgs::Long(FILTER_REQUIRE_ARRAY | t),
    )
    .unwrap_err();
    assert_eq!(
        failed(&e),
        "Filter\\FilterFailedException: filter validation failed: not an array (got string)"
    );
    let e = filter_input(
        &mut h,
        &i,
        INPUT_GET,
        b"a",
        FILTER_VALIDATE_EMAIL,
        &FilterArgs::Long(t),
    )
    .unwrap_err();
    assert_eq!(failed(&e), "Filter\\FilterFailedException: filter validation failed: filter validate_email not satisfied by '1'");
    // filter_input_array_failure.phpt
    let def = opts(&[(
        "a",
        Value::Array(arr(&[("flags", Value::Int(FILTER_REQUIRE_ARRAY | t))])),
    )]);
    let e = filter_input_array(&mut h, &i, INPUT_GET, &def, true).unwrap_err();
    assert_eq!(
        failed(&e),
        "Filter\\FilterFailedException: filter validation failed: not an array (got string)"
    );
    // filter_var_failure.phpt / filter_var_array_failure.phpt
    let e = filter_var(
        &mut h,
        &s("a"),
        FILTER_DEFAULT,
        &FilterArgs::Long(FILTER_REQUIRE_ARRAY | t),
    )
    .unwrap_err();
    assert_eq!(
        failed(&e),
        "Filter\\FilterFailedException: filter validation failed: not an array (got string)"
    );
    let e = filter_var(
        &mut h,
        &Value::Bool(true),
        FILTER_DEFAULT,
        &FilterArgs::Long(FILTER_REQUIRE_ARRAY | t),
    )
    .unwrap_err();
    assert_eq!(
        failed(&e),
        "Filter\\FilterFailedException: filter validation failed: not an array (got true)"
    );
    let e = filter_var(
        &mut h,
        &object("stdClass", None),
        FILTER_VALIDATE_EMAIL,
        &FilterArgs::Long(t),
    )
    .unwrap_err();
    assert_eq!(failed(&e), "Filter\\FilterFailedException: filter validation failed: object of type stdClass has no __toString() method");
    let e = filter_var(&mut h, &s("a"), FILTER_VALIDATE_EMAIL, &FilterArgs::Long(t)).unwrap_err();
    assert_eq!(failed(&e), "Filter\\FilterFailedException: filter validation failed: filter validate_email not satisfied by 'a'");
    let e = filter_var(
        &mut h,
        &Value::Array(ZArray::new()),
        FILTER_DEFAULT,
        &FilterArgs::Long(t),
    )
    .unwrap_err();
    assert_eq!(failed(&e), "Filter\\FilterFailedException: filter validation failed: not a scalar value (got an array)");
    let e = filter_var(
        &mut h,
        &s("a\0b"),
        FILTER_VALIDATE_INT,
        &FilterArgs::Long(t),
    )
    .unwrap_err();
    assert_eq!(
        failed(&e),
        "Filter\\FilterFailedException: filter validation failed: filter int not satisfied by 'a'"
    );
    let def = opts(&[(
        "a",
        Value::Array(arr(&[
            ("filter", Value::Int(FILTER_VALIDATE_EMAIL)),
            ("flags", Value::Int(t)),
        ])),
    )]);
    let e = filter_var_array(&mut h, &arr(&[("a", Value::Bool(true))]), &def, true).unwrap_err();
    assert_eq!(failed(&e), "Filter\\FilterFailedException: filter validation failed: filter validate_email not satisfied by '1'");
    // filter_success.phpt
    let v = filter_input(
        &mut h,
        &i,
        INPUT_GET,
        b"email",
        FILTER_VALIDATE_EMAIL,
        &FilterArgs::Long(t),
    )
    .unwrap();
    assert_eq!(export(&v), "'daniel.e.scherzer@gmail.com'");
    let v = filter_var(
        &mut h,
        &object("MyString", Some(b"daniel.e.scherzer@gmail.com")),
        FILTER_VALIDATE_EMAIL,
        &FilterArgs::Long(t),
    )
    .unwrap();
    assert_eq!(export(&v), "'daniel.e.scherzer@gmail.com'");
    let def = opts(&[("a", Value::Array(arr(&[("flags", Value::Int(t))])))]);
    let v = filter_var_array(
        &mut h,
        &arr(&[("a", object("MyString", Some(b"bar")))]),
        &def,
        true,
    )
    .unwrap();
    assert_eq!(export(&v), "array (\n  'a' => 'bar',\n)");
    assert!(h.diagnostics.is_empty());
}

#[test]
fn throw_and_null_error() {
    let i = get_inputs();
    let mut h = TestHost::default();
    let both = FILTER_THROW_ON_FAILURE | FILTER_NULL_ON_FAILURE;
    let msg = |f: &str, n: u32| {
        format!("{f}(): Argument #{n} ($options) cannot use both FILTER_NULL_ON_FAILURE and FILTER_THROW_ON_FAILURE")
    };
    for name in [&b"b2"[..], b"a"] {
        let e = filter_input(
            &mut h,
            &i,
            INPUT_GET,
            name,
            FILTER_DEFAULT,
            &FilterArgs::Long(both),
        )
        .unwrap_err();
        assert_eq!(e.message(), Some(msg("filter_input", 4).as_str()));
        let e = filter_input(
            &mut h,
            &i,
            INPUT_GET,
            name,
            FILTER_DEFAULT,
            &opts(&[("flags", Value::Int(both))]),
        )
        .unwrap_err();
        assert_eq!(e.message(), Some(msg("filter_input", 4).as_str()));
    }
    let o = opts(&[
        ("flags", Value::Int(both)),
        ("options", Value::Array(arr(&[("default", s("a"))]))),
    ]);
    let e = filter_input(&mut h, &i, INPUT_GET, b"b2", FILTER_DEFAULT, &o).unwrap_err();
    assert_eq!(e.message(), Some(msg("filter_input", 4).as_str()));
    let e = filter_var(
        &mut h,
        &Value::Bool(true),
        FILTER_DEFAULT,
        &FilterArgs::Long(both),
    )
    .unwrap_err();
    assert_eq!(e.message(), Some(msg("filter_var", 3).as_str()));
    let def = opts(&[("a", Value::Array(arr(&[("flags", Value::Int(both))])))]);
    let e = filter_input_array(&mut h, &i, INPUT_GET, &def, true).unwrap_err();
    assert_eq!(e.message(), Some(msg("filter_input_array", 2).as_str()));
    let e = filter_var_array(&mut h, &arr(&[("a", Value::Bool(true))]), &def, true).unwrap_err();
    assert_eq!(e.message(), Some(msg("filter_var_array", 2).as_str()));
}

#[test]
fn references_filter_var_array_with_ref() {
    // $array = ["123foo"]; $array2 = [&$array]; filter_var_array($array2, FILTER_VALIDATE_INT)
    let mut inner = Value::Array(ZArray::from_values([s("123foo")]));
    let r = inner.make_ref();
    let outer = ZArray::from_values([Value::Reference(r.clone())]);
    let mut h = TestHost::default();
    let v = filter_var_array(&mut h, &outer, &FilterArgs::Long(FILTER_VALIDATE_INT), true).unwrap();
    let Value::Array(res) = v else {
        panic!("array")
    };
    assert!(matches!(res.get_int(0), Some(Value::Reference(_))));
    assert_eq!(export(&r.get()), "array (\n  0 => false,\n)");
}

#[test]
fn recursive_references_terminate() {
    // $a = [1]; $a[] = &$a;
    let mut a = Value::Array(ZArray::from_values([s("1")]));
    let r = a.make_ref();
    let Value::Array(mut table) = r.get() else {
        unreachable!()
    };
    table.make_mut().push(Value::Reference(r.clone())).unwrap();
    r.set(Value::Array(table));
    let mut h = TestHost::default();
    let v = filter_var(
        &mut h,
        &Value::Reference(r.clone()),
        FILTER_VALIDATE_INT,
        &FilterArgs::Long(FILTER_REQUIRE_ARRAY),
    )
    .unwrap();
    let Value::Array(res) = v else {
        panic!("array")
    };
    assert_eq!(export(res.get_int(0).unwrap()), "1");
    // Break the cycle so the test does not leak.
    r.set(Value::Null);
}

#[test]
fn callbacks() {
    let mut h = TestHost::default();
    let cb = opts(&[("options", s("strtoupper"))]);
    let nested = Value::Array(ZArray::from_values([
        s("a"),
        Value::Array(ZArray::from_values([s("b")])),
    ]));
    let v = filter_var(&mut h, &nested, FILTER_CALLBACK, &cb).unwrap();
    assert_eq!(
        export(&v),
        "array (\n  0 => 'A',\n  1 => \n  array (\n    0 => 'B',\n  ),\n)"
    );
    let e = filter_var(&mut h, &s("x"), FILTER_CALLBACK, &FilterArgs::Long(0)).unwrap_err();
    assert_eq!(
        e.message(),
        Some("filter_var(): Option must be a valid callback")
    );
    let e = filter_var(
        &mut h,
        &s("x"),
        FILTER_CALLBACK,
        &opts(&[("options", s("thrower"))]),
    )
    .unwrap_err();
    assert_eq!(e.message(), Some("thrown from callback"));
    let v = filter_var(
        &mut h,
        &s("x"),
        FILTER_CALLBACK,
        &opts(&[("options", s("to_null"))]),
    )
    .unwrap();
    assert!(v.is_null());
}

#[test]
fn regexp_through_host() {
    let mut h = TestHost {
        regexes: vec![("/^a+$/", |s: &[u8]| {
            !s.is_empty() && s.iter().all(|&c| c == b'a')
        })],
        ..TestHost::default()
    };
    let o = opts(&[("options", Value::Array(arr(&[("regexp", s("/^a+$/"))])))]);
    assert_eq!(
        export(&filter_var(&mut h, &s("aaa"), FILTER_VALIDATE_REGEXP, &o).unwrap()),
        "'aaa'"
    );
    assert_eq!(
        export(&filter_var(&mut h, &s("ab"), FILTER_VALIDATE_REGEXP, &o).unwrap()),
        "false"
    );
    let o = opts(&[("options", Value::Array(arr(&[("regexp", s("/unknown/"))])))]);
    assert_eq!(
        export(&filter_var(&mut h, &s("x"), FILTER_VALIDATE_REGEXP, &o).unwrap()),
        "false"
    );
}

#[test]
fn url_parser_option() {
    let mut h = TestHost::default();
    let o = opts(&[(
        "options",
        Value::Array(arr(&[("uri_parser_class", s("Uri\\Rfc3986\\Uri"))])),
    )]);
    let e = filter_var(&mut h, &s("http://x.y"), FILTER_VALIDATE_URL, &o).unwrap_err();
    assert_eq!(
        e.message(),
        Some("filter_var(): \"uri_parser_class\" option has invalid value")
    );
    let o = opts(&[(
        "options",
        Value::Array(arr(&[("uri_parser_class", s("parse_url"))])),
    )]);
    assert_eq!(
        export(&filter_var(&mut h, &s("http://x.y"), FILTER_VALIDATE_URL, &o).unwrap()),
        "'http://x.y'"
    );
}

#[test]
fn ini_settings() {
    // filter_default_deprecation.phpt / 004.phpt / 005.phpt
    assert_eq!(
        parse_default_filter_ini(b"special_chars"),
        (FILTER_SANITIZE_SPECIAL_CHARS, true)
    );
    assert_eq!(
        parse_default_filter_ini(b"STRIPPED"),
        (FILTER_SANITIZE_STRING, true)
    );
    assert_eq!(
        parse_default_filter_ini(b"unsafe_raw"),
        (FILTER_UNSAFE_RAW, false)
    );
    assert_eq!(parse_default_filter_ini(b"bogus"), (FILTER_DEFAULT, false));
    assert_eq!(parse_default_flags_ini(None), FILTER_FLAG_NO_ENCODE_QUOTES);
    assert_eq!(parse_default_flags_ini(Some(b"4")), 4);
    assert_eq!(parse_default_flags_ini(Some(b" 12abc")), 12);
    assert_eq!(parse_default_flags_ini(Some(b"x")), 0);

    let mut h = TestHost::default();
    let flags = parse_default_flags_ini(None);
    // 004.phpt: filter.default=special_chars
    let v = apply_default_filter(&mut h, b"O'Henry", FILTER_SANITIZE_SPECIAL_CHARS, flags).unwrap();
    assert_eq!(export(&v), "'O&#39;Henry'");
    let v =
        apply_default_filter(&mut h, b"<b>Bold</b>", FILTER_SANITIZE_SPECIAL_CHARS, flags).unwrap();
    assert_eq!(export(&v), "'&#60;b&#62;Bold&#60;/b&#62;'");
    // 005.phpt: filter.default=stripped keeps URLs intact.
    let v = apply_default_filter(
        &mut h,
        b"http://fifaworldcup.yahoo.com/03/en/photozone/index.html",
        FILTER_SANITIZE_STRING,
        flags,
    )
    .unwrap();
    assert_eq!(
        export(&v),
        "'http://fifaworldcup.yahoo.com/03/en/photozone/index.html'"
    );
    // 041.phpt: stripped with default_flags=0 encodes quotes.
    let v = apply_default_filter(&mut h, b"\"foo bar\"", FILTER_SANITIZE_STRING, 0).unwrap();
    assert_eq!(export(&v), "'&#34;foo bar&#34;'");
    // filter_default_parse_str.phpt: filter.default=int
    let v = apply_default_filter(&mut h, b"notint", FILTER_VALIDATE_INT, flags).unwrap();
    assert_eq!(export(&v), "false");
    // bug42718.phpt (XFAIL): unsafe_raw is never applied as default filter.
    let v = apply_default_filter(&mut h, b"1\0", FILTER_UNSAFE_RAW, FILTER_FLAG_STRIP_LOW).unwrap();
    assert_eq!(export(&v), "'1' . \"\\0\" . ''");
    assert!(h.diagnostics.is_empty());
}

#[test]
fn objects_and_defaults() {
    let mut h = TestHost::default();
    // bug49274 / bug67167: objects without __toString fail (no exception).
    let v = filter_var(
        &mut h,
        &object("stdClass", None),
        FILTER_VALIDATE_INT,
        &FilterArgs::Long(0),
    )
    .unwrap();
    assert_eq!(export(&v), "false");
    let v = filter_var(
        &mut h,
        &object("stdClass", None),
        FILTER_VALIDATE_BOOL,
        &FilterArgs::Long(FILTER_NULL_ON_FAILURE),
    )
    .unwrap();
    assert!(v.is_null());
    let o = opts(&[("options", Value::Array(arr(&[("default", Value::Int(7))])))]);
    let v = filter_var(&mut h, &object("stdClass", None), FILTER_VALIDATE_INT, &o).unwrap();
    assert_eq!(export(&v), "7");
    // A successful FILTER_VALIDATE_BOOL false is replaced by the default.
    let o = opts(&[("options", Value::Array(arr(&[("default", s("D"))])))]);
    let v = filter_var(&mut h, &s("no"), FILTER_VALIDATE_BOOL, &o).unwrap();
    assert_eq!(export(&v), "'D'");
    let v = filter_var(
        &mut h,
        &s("x"),
        FILTER_DEFAULT,
        &FilterArgs::Long(FILTER_FORCE_ARRAY),
    )
    .unwrap();
    assert_eq!(export(&v), "array (\n  0 => 'x',\n)");
}

//! Ports of selected `ext/intl/tests/{msgfmt,bug,gh}*.phpt` `MessageFormatter`
//! tests (php-src master).

use php_intl::msgformat::{format_message, MessageFormatter, MsgKey, MsgValue};

#[test]
fn bug74484_many_named_args() {
    let names: Vec<String> = ('a'..='l').map(String::from).collect();
    let args: Vec<_> = names
        .iter()
        .zip(1..)
        .map(|(n, i)| (MsgKey::Name(n.as_bytes()), MsgValue::Int(i)))
        .collect();
    let out = format_message(
        "en_US",
        b"{a} {b} {c} {d} {e} {f} {g} {h} {i} {j} {k} {l}",
        &args,
        "UTC",
    )
    .expect("format");
    assert_eq!(out, "1 2 3 4 5 6 7 8 9 10 11 12");
}

#[test]
fn msgfmt_bug70484_selectordinal() {
    let data = [
        MsgValue::Int(42),
        MsgValue::Float(42.42),
        MsgValue::Int(2_147_483_643),
        MsgValue::Float(2_147_483_643.123_45),
        MsgValue::Int(5),
    ];
    let want: &[(&str, [&str; 5])] = &[
        (
            "de",
            [
                "42-other",
                "42,42-other",
                "2.147.483.643-other",
                "2.147.483.643,123-other",
                "five",
            ],
        ),
        (
            "en",
            [
                "42-two",
                "42.42-other",
                "2,147,483,643-few",
                "2,147,483,643.123-other",
                "five",
            ],
        ),
        (
            "ru",
            [
                "42-other",
                "42,42-other",
                "2\u{a0}147\u{a0}483\u{a0}643-other",
                "2\u{a0}147\u{a0}483\u{a0}643,123-other",
                "five",
            ],
        ),
    ];
    for (lc, exp) in want {
        for pat in [
            &b"{n, selectordinal, =5 {five} zero {#-zero} one {#-one} two {#-two} few {#-few} many {#-many} other {#-other}}"[..],
            b"{0, selectordinal, =5 {five} zero {#-zero} one {#-one} two {#-two} few {#-few} many {#-many} other {#-other}}",
        ] {
            let mut m = MessageFormatter::new(lc, pat).expect("ctor");
            let named = pat[1] == b'n';
            for (v, e) in data.iter().zip(exp) {
                let key = if named { MsgKey::Name(b"n") } else { MsgKey::Index(0) };
                let got = m.format(&[(key, *v)], "UTC").expect("format");
                assert_eq!(got, *e, "{lc} {v:?}");
            }
        }
    }
}

#[test]
fn gh12020_gh11658_pattern_syntax_errors() {
    let e = format_message("en", b"some message with {invalid format}", &[], "UTC").unwrap_err();
    assert_eq!(
        e.intl_message(),
        Some("pattern syntax error (parse error at offset 19, after \" message with {\", before or at \"invalid format}\")")
    );
    let e = format_message("en", b"some {wrong.format}", &[], "UTC").unwrap_err();
    assert_eq!(
        e.intl_message(),
        Some("pattern syntax error (parse error at offset 6, after \"some {\", before or at \"wrong.format}\")")
    );
    assert_eq!(e.intl_code().name(), "U_PATTERN_SYNTAX_ERROR");
}

#[test]
fn msgfmt_fail2_creation_errors() {
    let cases: &[(&str, &[u8], &str, &str)] = &[
        ("", b"", "U_ILLEGAL_ARGUMENT_ERROR", "message formatter creation failed"),
        ("whatever", b"{0,whatever}", "U_ILLEGAL_ARGUMENT_ERROR", "message formatter creation failed"),
        ("root", b"{0,whatever} would not work!", "U_ILLEGAL_ARGUMENT_ERROR", "message formatter creation failed"),
        (
            "en",
            b"{0,choice}",
            "U_PATTERN_SYNTAX_ERROR",
            "pattern syntax error (parse error at offset 1, after \"{\", before or at \"0,choice}\")",
        ),
        ("fr", b"{0,", "U_UNMATCHED_BRACES", "message formatter creation failed"),
        ("en_US", b"\xD0", "U_INVALID_CHAR_FOUND", "error converting pattern to UTF-16"),
    ];
    for (loc, pat, code, msg) in cases {
        let e = MessageFormatter::new(loc, pat).unwrap_err();
        assert_eq!(e.intl_code().name(), *code, "{loc} {pat:?}");
        assert_eq!(e.intl_message(), Some(*msg), "{loc} {pat:?}");
    }
}

fn fmt(locale: &str, pat: &[u8], args: &[(MsgKey<'_>, MsgValue<'_>)]) -> php_intl::Result<String> {
    MessageFormatter::new(locale, pat)
        .expect("ctor")
        .format(args, "UTC")
}

#[test]
fn msgfmt_format_errors() {
    assert_eq!(
        fmt("en_US", b"{0} {1}", &[(MsgKey::Index(0), MsgValue::Int(7))]).expect("ok"),
        "7 {1}"
    );
    let e = fmt(
        "en_US",
        b"{foo,number} {foo}",
        &[(MsgKey::Index(0), MsgValue::Int(7))],
    )
    .unwrap_err();
    assert_eq!(
        e.intl_message(),
        Some("Inconsistent types declared for an argument")
    );
    let e = fmt(
        "en_US",
        b"{foo,number,percent}",
        &[
            (MsgKey::Name(b"foo"), MsgValue::Int(7)),
            (MsgKey::Index(-1), MsgValue::Str(b"bar")),
        ],
    )
    .unwrap_err();
    assert_eq!(
        e.intl_message(),
        Some("Found negative or too large array key")
    );
    let e = fmt(
        "en_US",
        b"{foo}",
        &[
            (MsgKey::Name(b"foo"), MsgValue::Int(7)),
            (MsgKey::Name(b"\x80"), MsgValue::Str(b"bar")),
        ],
    )
    .unwrap_err();
    assert_eq!(
        e.intl_message_bytes(),
        Some(&b"Invalid UTF-8 data in argument key: '\x80'"[..])
    );
    let e = fmt(
        "en_US",
        b"{foo}",
        &[(MsgKey::Name(b"foo"), MsgValue::Str(b"\x80"))],
    )
    .unwrap_err();
    assert_eq!(
        e.intl_message_bytes(),
        Some(&b"Invalid UTF-8 data in string argument: '\x80'"[..])
    );
    let e = fmt(
        "en_US",
        b"{foo,date}",
        &[(
            MsgKey::Name(b"foo"),
            MsgValue::Object {
                class: "stdClass",
                as_string: None,
            },
        )],
    )
    .unwrap_err();
    assert_eq!(
        e.intl_message(),
        Some("The argument for key 'foo' cannot be used as a date or time")
    );
}

#[test]
fn msgfmt_set_pattern_cache_and_errors() {
    let mut mf = MessageFormatter::new("en_US", b"{0,number} -- {1,ordinal}").expect("ctor");
    let args = [
        (MsgKey::Index(0), MsgValue::Float(1.3)),
        (MsgKey::Index(1), MsgValue::Float(1.3)),
    ];
    assert_eq!(mf.format(&args, "UTC").expect("ok"), "1.3 -- 1st");
    assert_eq!(mf.format(&args, "UTC").expect("ok"), "1.3 -- 1st");
    mf.set_pattern(b"{0,ordinal} -- {1,number}").expect("set");
    assert_eq!(mf.format(&args, "UTC").expect("ok"), "1st -- 1.3");

    let mut mf =
        MessageFormatter::new("en_US", b"{0,number} monkeys on {1,number} trees").expect("ctor");
    let e = mf
        .set_pattern(b"{0,number} trees hosting {1,number monkeys")
        .unwrap_err();
    assert_eq!(e.intl_code().name(), "U_PATTERN_SYNTAX_ERROR");
    assert_eq!(
        e.intl_message(),
        Some("Error setting symbol value at line 0, offset 26")
    );
}

#[test]
fn msgfmt_static_creation_errors() {
    let v = [(MsgKey::Index(0), MsgValue::Int(4560))];
    let e = format_message("root", b"{0,whatever} would not work!", &v, "UTC").unwrap_err();
    assert_eq!(e.intl_message(), Some("Creating message formatter failed"));
    assert_eq!(e.intl_code().name(), "U_ZERO_ERROR");
    let e = php_intl::msgformat::parse_message("root", b"{0,whatever} would not work!", b"4,560")
        .unwrap_err();
    assert_eq!(e.intl_message(), Some("Creating message formatter failed"));
    assert_eq!(e.intl_code().name(), "U_ILLEGAL_ARGUMENT_ERROR");
}

#[test]
fn msgfmt_parse_int64_min() {
    let mut mf = MessageFormatter::new("en_US", b"{0,number,integer}").expect("ctor");
    let r = mf.parse(b"-9,223,372,036,854,775,808").expect("parse");
    assert_eq!(
        format!("{r:?}"),
        format!(
            "{:?}",
            php_intl::msgformat::parse_message(
                "en_US",
                b"{0,number,integer}",
                b"-9,223,372,036,854,775,808"
            )
            .expect("parse")
        )
    );
    assert!(format!("{r:?}").contains("-9223372036854775808"), "{r:?}");
}

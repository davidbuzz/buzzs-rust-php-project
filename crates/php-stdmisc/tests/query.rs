//! Oracle tests for `http_build_query()` and `parse_str()`, plus hand
//! ports of the `max_input_vars`/nesting phpt expectations.

mod common;

use common::{fields, fixture_lines, show, unhex};
use php_runtime::serialize::{serialize, unserialize, UnserializeOptions};
use php_runtime::{CollectingContext, Value, ZArray};
use php_stdmisc::query::{self, InputSettings, ParseKind};

fn unser(data: &[u8], ctx: &mut CollectingContext) -> Value {
    unserialize(data, &UnserializeOptions::default(), ctx)
        .unwrap()
        .unwrap()
}

#[test]
fn http_build_query_matches_oracle() {
    let mut ctx = CollectingContext::new();
    let mut n = 0;
    for line in fixture_lines("query.txt") {
        let f = fields(&line);
        if f[0] != "hbq" {
            continue;
        }
        let data = unser(&unhex(f[1]), &mut ctx);
        let prefix = unhex(f[2]);
        let sep = f[3].strip_prefix('s').map(unhex);
        let enc: i64 = f[4].parse().unwrap();
        let got =
            query::http_build_query(&mut ctx, &data, &prefix, sep.as_deref(), enc, b"&").unwrap();
        assert_eq!(show(got.as_bytes()), show(&unhex(f[5])), "{line}");
        n += 1;
    }
    assert!(n > 1000);
}

#[test]
fn parse_str_matches_oracle() {
    let mut ctx = CollectingContext::new();
    let settings = InputSettings::default();
    for line in fixture_lines("query.txt") {
        let f = fields(&line);
        if f[0] != "ps" {
            continue;
        }
        let input = unhex(f[1]);
        let got = query::parse_str(&input, &settings, &mut ctx).unwrap();
        let got = serialize(&Value::Array(got), &mut ctx).unwrap();
        assert_eq!(
            show(&got),
            show(&unhex(f[2])),
            "parse_str({:?})",
            show(&input)
        );
    }
    assert!(ctx.messages().is_empty(), "{:?}", ctx.messages());
}

#[test]
fn input_limits() {
    let mut ctx = CollectingContext::new();
    // max_input_vars (ext/standard/tests/strings/parse_str_* and tests/basic/bug53180-ish).
    let settings = InputSettings {
        max_input_vars: 3,
        ..InputSettings::default()
    };
    let r = query::parse_str(b"a=1&b=2&c=3&d=4&e=5", &settings, &mut ctx).unwrap();
    assert_eq!(r.len(), 3);
    assert_eq!(
        ctx.messages(),
        ["Warning: parse_str(): Input variables exceeded 3. To increase the limit change max_input_vars in php.ini."]
    );
    ctx.take();
    // Nesting level: the variable is dropped; the warning only shows with display_errors=0.
    let settings = InputSettings {
        max_input_nesting_level: 2,
        display_errors: false,
        ..InputSettings::default()
    };
    let r = query::parse_str(b"a[b][c][d]=1&x[y]=2", &settings, &mut ctx).unwrap();
    assert_eq!(
        show(&serialize(&Value::Array(r), &mut ctx).unwrap()),
        r#"a:1:{s:1:"x";a:1:{s:1:"y";s:1:"2";}}"#
    );
    assert_eq!(
        ctx.messages(),
        ["Warning: parse_str(): Input variable nesting level exceeded 2. To increase the limit change max_input_nesting_level in php.ini."]
    );
    ctx.take();
    // arg_separator.input is a set of characters.
    let settings = InputSettings {
        arg_separator_input: b";&".to_vec(),
        ..InputSettings::default()
    };
    let r = query::parse_str(b"a=1;b=2&c=3", &settings, &mut ctx).unwrap();
    assert_eq!(r.len(), 3);
    let e = query::parse_str(b"a\0b", &settings, &mut ctx).unwrap_err();
    assert_eq!(
        e.message(),
        Some("parse_str(): Argument #1 ($string) must not contain any null bytes")
    );
}

#[test]
fn cookies_and_post() {
    let mut ctx = CollectingContext::new();
    let settings = InputSettings::default();
    let mut track = ZArray::new();
    query::treat_data(
        ParseKind::Cookie,
        b" a=1; a=2;  b=x%20y+z; ;=v; c.d=3",
        &mut track,
        &settings,
        "",
        None,
        &mut ctx,
    )
    .unwrap();
    // First cookie wins, values are raw-url-decoded, names are not decoded.
    assert_eq!(
        show(&serialize(&Value::Array(track), &mut ctx).unwrap()),
        r#"a:3:{s:1:"a";s:1:"1";s:1:"b";s:5:"x y+z";s:3:"c_d";s:1:"3";}"#
    );
    let mut track = ZArray::new();
    let mut filter = |_: ParseKind, name: &[u8], v: Vec<u8>| (name != b"drop").then_some(v);
    query::treat_data(
        ParseKind::Post,
        b"a=1&&drop=2&b[]=x+y&",
        &mut track,
        &settings,
        "",
        Some(&mut filter),
        &mut ctx,
    )
    .unwrap();
    assert_eq!(
        show(&serialize(&Value::Array(track), &mut ctx).unwrap()),
        r#"a:2:{s:1:"a";s:1:"1";s:1:"b";a:1:{i:0;s:3:"x y";}}"#
    );
    let settings = InputSettings {
        max_input_vars: 1,
        ..InputSettings::default()
    };
    let mut track = ZArray::new();
    query::treat_data(
        ParseKind::Post,
        b"a=1&b=2&c=3",
        &mut track,
        &settings,
        "",
        None,
        &mut ctx,
    )
    .unwrap();
    // POST registers one variable past the limit before warning.
    assert_eq!(track.len(), 2);
    assert_eq!(
        ctx.messages(),
        ["Warning: PHP Request Startup: Input variables exceeded 1. To increase the limit change max_input_vars in php.ini."]
    );
}

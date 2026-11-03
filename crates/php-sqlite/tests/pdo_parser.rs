//! `pdo_sql_parser.re` port: scanners and `pdo_parse_params()` rewriting,
//! following the cases of ext/pdo/tests (`debug_emulated_prepares`,
//! `bug_43130`, `bug_71885`), ext/pdo_mysql/tests/pdo_mysql_parser.phpt and
//! ext/pdo_sqlite/tests/pdo_sqlite_parser.phpt.

#![allow(clippy::doc_markdown, clippy::unnecessary_wraps)]

mod common;

use common::TestHost;
use php_runtime::{Resource, Value};
use php_sqlite::pdo::parser::{
    parse_params, tokenize, DefaultScanner, ParamMapKey, ParseContext, ParseError, Parsed,
    ParserParam, Scanner, SqliteScanner, Token, PARAM_BOOL, PARAM_INT, PARAM_LOB, PARAM_NULL,
    PARAM_STR, PLACEHOLDER_NAMED, PLACEHOLDER_NONE, PLACEHOLDER_POSITIONAL,
};
use php_sqlite::Cx;

fn binds(scanner: &dyn Scanner, q: &str) -> Vec<String> {
    tokenize(scanner, q.as_bytes())
        .into_iter()
        .filter(|(t, _)| matches!(t, Token::Bind | Token::BindPos))
        .map(|(_, b)| String::from_utf8(b).unwrap())
        .collect()
}

#[test]
fn default_scanner_tokens() {
    let d = DefaultScanner;
    assert_eq!(binds(&d, "SELECT * FROM t WHERE 1 = ?"), ["?"]);
    assert!(binds(&d, "SELECT * FROM t -- ?").is_empty());
    assert!(binds(&d, "SELECT * FROM t /* ? */").is_empty());
    assert_eq!(binds(&d, "SELECT * FROM t WHERE \"?\" IN (?, '?')"), ["?"]);
    assert_eq!(binds(&d, "SELECT * FROM t WHERE 1 = --?\n ?"), ["?"]);
    assert_eq!(binds(&d, "SELECT :a, :b_1, ?"), [":a", ":b_1", "?"]);
    assert_eq!(binds(&d, "SELECT 'it''s ?', ?"), ["?"]);
    // multi-char runs are text
    assert!(binds(&d, "SELECT a ?? b, x::int").is_empty());
    // an unterminated quote falls back to a single special char
    assert_eq!(binds(&d, "SELECT 'abc ?"), ["?"]);
    // an unterminated comment ends the scan
    assert!(binds(&d, "SELECT ? /* ?").len() == 1);
    // tokens cover the input
    let toks = tokenize(&d, b"SELECT :a-1");
    let joined: Vec<u8> = toks.iter().flat_map(|(_, b)| b.clone()).collect();
    assert_eq!(joined, b"SELECT :a-1");
    assert_eq!(toks[1], (Token::Bind, b":a".to_vec()));
}

#[test]
fn sqlite_scanner_tokens() {
    let s = SqliteScanner;
    assert_eq!(binds(&s, "SELECT * FROM t WHERE '1' = ?"), ["?"]);
    assert_eq!(binds(&s, "SELECT * FROM t WHERE `a``?` = ?"), ["?"]);
    assert_eq!(binds(&s, "SELECT * FROM t WHERE [a`?] = ?"), ["?"]);
    assert_eq!(binds(&s, "SELECT * FROM t WHERE \"a`?\" = ?"), ["?"]);
    assert!(binds(&s, "SELECT * FROM t -- ?").is_empty());
    assert!(binds(&s, "SELECT * FROM t /* ? */").is_empty());
    // the quoted-string rules are greedy (longest match up to the last quote)
    assert!(binds(&s, "SELECT * FROM t WHERE '?' IN (?, '?')").is_empty());
    assert_eq!(
        binds(&DefaultScanner, "SELECT * FROM t WHERE '?' IN (?, '?')"),
        ["?"]
    );
}

fn param(key: ParamMapKey, value: Value, param_type: i64) -> ParserParam {
    ParserParam {
        key,
        value,
        param_type,
    }
}

fn name(n: &str) -> ParamMapKey {
    ParamMapKey::Name(n.as_bytes().to_vec())
}

fn quote(_: &mut Cx<'_>, s: &[u8], _: i64) -> Option<Vec<u8>> {
    let mut out = vec![b'\''];
    for &b in s {
        if b == b'\'' {
            out.push(b'\'');
        }
        out.push(b);
    }
    out.push(b'\'');
    Some(out)
}

fn run(
    h: &mut TestHost,
    supports: u8,
    tmpl: Option<&str>,
    params: Option<&[ParserParam]>,
    q: &str,
    map: &mut Vec<(ParamMapKey, Vec<u8>)>,
) -> Result<Parsed, ParseError> {
    let mut cx = Cx::new(h);
    let mut ctx = ParseContext {
        supports_placeholders: supports,
        named_rewrite_template: tmpl,
        bound_params: params,
        bound_param_map: map,
    };
    let mut quoter = quote;
    let r = parse_params(
        &mut cx,
        &DefaultScanner,
        q.as_bytes(),
        &mut ctx,
        Some(&mut quoter),
    );
    let _ = cx.finish(());
    r
}

fn rewritten(r: Result<Parsed, ParseError>) -> String {
    match r {
        Ok(Parsed::Rewritten(q)) => String::from_utf8(q).unwrap(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn emulated_prepares_quote_values() {
    let mut h = TestHost::new();
    let mut map = Vec::new();
    // debug_emulated_prepares.phpt
    let params = [
        param(name(":bool"), Value::Bool(true), PARAM_BOOL),
        param(name(":int"), Value::Int(123), PARAM_INT),
        param(name(":string"), Value::from("foo"), PARAM_STR),
        param(name(":null"), Value::Null, PARAM_NULL),
    ];
    let q = "SELECT :bool, :int, :string, :null";
    assert_eq!(
        rewritten(run(
            &mut h,
            PLACEHOLDER_NONE,
            None,
            Some(&params),
            q,
            &mut map
        )),
        "SELECT 1, 123, 'foo', NULL"
    );
    // positional; nulls are always NULL (bug_73234)
    let params = [
        param(ParamMapKey::Index(0), Value::from("a'b"), PARAM_STR),
        param(ParamMapKey::Index(1), Value::Null, PARAM_STR),
        param(ParamMapKey::Index(2), Value::Int(0), PARAM_NULL),
    ];
    assert_eq!(
        rewritten(run(
            &mut h,
            PLACEHOLDER_NONE,
            None,
            Some(&params),
            "VALUES (?, ?, ?)",
            &mut map
        )),
        "VALUES ('a''b', NULL, NULL)"
    );
    // the same named parameter used twice
    let params = [param(name(":a"), Value::from("x"), PARAM_STR)];
    assert_eq!(
        rewritten(run(
            &mut h,
            PLACEHOLDER_NONE,
            None,
            Some(&params),
            "SELECT :a, :a",
            &mut map
        )),
        "SELECT 'x', 'x'"
    );
    // no placeholders
    assert_eq!(
        run(&mut h, PLACEHOLDER_NONE, None, None, "SELECT 1", &mut map),
        Ok(Parsed::Unchanged)
    );
    // a LOB stream is read and quoted
    let res = Resource::new("stream", None);
    h.streams.insert(res.handle(), b"blob'data".to_vec());
    let params = [param(
        ParamMapKey::Index(0),
        Value::Resource(res),
        PARAM_LOB,
    )];
    assert_eq!(
        rewritten(run(
            &mut h,
            PLACEHOLDER_NONE,
            None,
            Some(&params),
            "SELECT ?",
            &mut map
        )),
        "SELECT 'blob''data'"
    );
    let params = [param(
        ParamMapKey::Index(0),
        Value::Resource(Resource::new("gd", None)),
        PARAM_LOB,
    )];
    assert_eq!(
        run(
            &mut h,
            PLACEHOLDER_NONE,
            None,
            Some(&params),
            "SELECT ?",
            &mut map
        ),
        Err(ParseError::Impl {
            sqlstate: "HY105",
            message: "Expected a stream resource"
        })
    );
}

#[test]
fn parse_errors() {
    let mut h = TestHost::new();
    let mut map = Vec::new();
    assert_eq!(
        run(
            &mut h,
            PLACEHOLDER_NONE,
            None,
            None,
            "SELECT :a, ?",
            &mut map
        ),
        Err(ParseError::Impl {
            sqlstate: "HY093",
            message: "mixed named and positional parameters"
        })
    );
    let params = [param(ParamMapKey::Index(0), Value::Int(1), PARAM_INT)];
    assert_eq!(
        run(
            &mut h,
            PLACEHOLDER_NONE,
            None,
            Some(&params),
            "SELECT ?, ?",
            &mut map
        ),
        Err(ParseError::Impl {
            sqlstate: "HY093",
            message: "number of bound variables does not match number of tokens"
        })
    );
    // bug_43130: ":id-value" scans as ":id" followed by text
    let params = [param(name(":id-value"), Value::from("1"), PARAM_STR)];
    assert_eq!(
        run(
            &mut h,
            PLACEHOLDER_NONE,
            None,
            Some(&params),
            "SELECT a FROM t WHERE b=:id-value",
            &mut map
        ),
        Err(ParseError::Impl {
            sqlstate: "HY093",
            message: "parameter was not defined"
        })
    );
    // a name preceded by an alphanumeric char is not a placeholder
    assert_eq!(
        run(&mut h, PLACEHOLDER_NONE, None, None, "SELECT a:b", &mut map),
        Ok(Parsed::Unchanged)
    );
}

#[test]
fn native_rewrites() {
    let mut h = TestHost::new();
    // pgsql-style: positional native with a named rewrite template
    let mut map = Vec::new();
    assert_eq!(
        rewritten(run(
            &mut h,
            PLACEHOLDER_POSITIONAL,
            Some("$%d"),
            None,
            "SELECT :a, :b, :a",
            &mut map
        )),
        "SELECT $1, $2, $1"
    );
    assert!(map.contains(&(name(":a"), b"$1".to_vec())));
    assert!(map.contains(&(ParamMapKey::Index(2), b"$1".to_vec())));
    let mut map = Vec::new();
    assert_eq!(
        rewritten(run(
            &mut h,
            PLACEHOLDER_POSITIONAL,
            Some("$%d"),
            None,
            "SELECT ?, ?",
            &mut map
        )),
        "SELECT $1, $2"
    );
    // named native: ? becomes :pdoN
    let mut map = Vec::new();
    assert_eq!(
        rewritten(run(
            &mut h,
            PLACEHOLDER_NAMED,
            None,
            None,
            "SELECT ?, ?",
            &mut map
        )),
        "SELECT :pdo1, :pdo2"
    );
    assert_eq!(
        map,
        [
            (ParamMapKey::Index(0), b":pdo1".to_vec()),
            (ParamMapKey::Index(1), b":pdo2".to_vec())
        ]
    );
    // positional native: :name becomes ?
    let mut map = Vec::new();
    assert_eq!(
        rewritten(run(
            &mut h,
            PLACEHOLDER_POSITIONAL,
            None,
            None,
            "SELECT :x, :y",
            &mut map
        )),
        "SELECT ?, ?"
    );
    assert_eq!(
        map,
        [
            (ParamMapKey::Index(0), b":x".to_vec()),
            (ParamMapKey::Index(1), b":y".to_vec())
        ]
    );
    // matching native syntax is left alone
    let mut map = Vec::new();
    assert_eq!(
        run(
            &mut h,
            PLACEHOLDER_POSITIONAL,
            None,
            None,
            "SELECT ?",
            &mut map
        ),
        Ok(Parsed::Unchanged)
    );
    assert_eq!(
        run(&mut h, PLACEHOLDER_NAMED, None, None, "SELECT :a", &mut map),
        Ok(Parsed::Unchanged)
    );
}

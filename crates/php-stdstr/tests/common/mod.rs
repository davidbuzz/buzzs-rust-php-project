//! Table-driven oracle harness shared by the `oracle_*` integration tests.
//!
//! Each test file builds a list of [`Case`]s: a single-line PHP expression
//! plus the outcome our Rust port produced for it, rendered in the same
//! textual form the PHP side prints:
//!
//! ```text
//! Warning: trim(): Invalid '..'-range
//! => 'abc'
//! ```
//!
//! or `!! ValueError: message` for a thrown exception.  The expected PHP
//! outcomes are cached in `tests/fixtures/<name>.txt`; the `#[ignore]`d
//! `regenerate` test of each file re-runs `/usr/bin/php` to rebuild it.
//!
//! When the installed PHP (8.3) differs from php-src master, a case can be
//! given an explicit expectation with [`Case::expect`]; such cases are not
//! sent to PHP.

#![allow(dead_code)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;

use php_stdstr::{Notice, StdError};

/// A PHP value as rendered by `var_export()`.
#[derive(Debug, Clone, PartialEq)]
pub enum V {
    /// `NULL`
    Null,
    /// `true`/`false`
    Bool(bool),
    /// integer
    Int(i64),
    /// float
    Float(f64),
    /// string
    Str(Vec<u8>),
    /// array (ordered key/value pairs)
    Arr(Vec<(K, V)>),
}

/// An array key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum K {
    /// integer key
    Int(i64),
    /// string key
    Str(Vec<u8>),
}

impl From<i64> for K {
    fn from(v: i64) -> Self {
        K::Int(v)
    }
}
impl From<&str> for K {
    fn from(v: &str) -> Self {
        K::Str(v.as_bytes().to_vec())
    }
}
impl From<Vec<u8>> for K {
    fn from(v: Vec<u8>) -> Self {
        K::Str(v)
    }
}

impl From<bool> for V {
    fn from(v: bool) -> Self {
        V::Bool(v)
    }
}
impl From<i64> for V {
    fn from(v: i64) -> Self {
        V::Int(v)
    }
}
impl From<i32> for V {
    fn from(v: i32) -> Self {
        V::Int(i64::from(v))
    }
}
impl From<usize> for V {
    fn from(v: usize) -> Self {
        V::Int(i64::try_from(v).expect("usize fits i64"))
    }
}
impl From<f64> for V {
    fn from(v: f64) -> Self {
        V::Float(v)
    }
}
impl From<Vec<u8>> for V {
    fn from(v: Vec<u8>) -> Self {
        V::Str(v)
    }
}
impl From<&[u8]> for V {
    fn from(v: &[u8]) -> Self {
        V::Str(v.to_vec())
    }
}
impl From<&str> for V {
    fn from(v: &str) -> Self {
        V::Str(v.as_bytes().to_vec())
    }
}
impl From<String> for V {
    fn from(v: String) -> Self {
        V::Str(v.into_bytes())
    }
}
impl From<()> for V {
    fn from((): ()) -> Self {
        V::Null
    }
}
impl<T: Into<V>> From<Vec<T>> for V {
    fn from(v: Vec<T>) -> Self {
        V::Arr(
            v.into_iter()
                .enumerate()
                .map(|(i, x)| (K::Int(i64::try_from(i).expect("index")), x.into()))
                .collect(),
        )
    }
}
/// `Option<T>`: `None` renders as `false` (PHP's usual failure value).
impl<T: Into<V>> From<Option<T>> for V {
    fn from(v: Option<T>) -> Self {
        v.map_or(V::Bool(false), Into::into)
    }
}

/// Builds an associative array.
pub fn assoc<I, KK, VV>(items: I) -> V
where
    I: IntoIterator<Item = (KK, VV)>,
    KK: Into<K>,
    VV: Into<V>,
{
    V::Arr(
        items
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
    )
}

fn export_str(s: &[u8], out: &mut String) {
    // var_export: '...' with \ and ' escaped; NUL bytes as ' . "\0" . '
    // Raw bytes are decoded lossily, exactly like the fixture file is read.
    let mut raw = Vec::with_capacity(s.len() + 2);
    raw.push(b'\'');
    for &b in s {
        match b {
            b'\\' => raw.extend_from_slice(b"\\\\"),
            b'\'' => raw.extend_from_slice(b"\\'"),
            0 => raw.extend_from_slice(b"' . \"\\0\" . '"),
            _ => raw.push(b),
        }
    }
    raw.push(b'\'');
    out.push_str(&String::from_utf8_lossy(&raw));
}

/// `smart_str_append_double(buf, d, serialize_precision = -1, zero_frac)`.
pub fn export_float(d: f64) -> String {
    if d.is_nan() {
        return "NAN".into();
    }
    if d.is_infinite() {
        return if d > 0.0 { "INF".into() } else { "-INF".into() };
    }
    if d == 0.0 {
        return if d.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    // Shortest round-trip digits via Rust's `{:e}` formatting.
    let e = format!("{:e}", d.abs());
    let (mant, exp) = e.split_once('e').expect("exp form");
    let exp: i32 = exp.parse().expect("exp");
    let digits: String = mant.chars().filter(char::is_ascii_digit).collect();
    let decpt = exp + 1;
    let mut s = String::new();
    if d < 0.0 {
        s.push('-');
    }
    if !(-3..=17).contains(&decpt) {
        s.push_str(&digits[..1]);
        s.push('.');
        if digits.len() > 1 {
            s.push_str(&digits[1..]);
        } else {
            s.push('0');
        }
        let _ = write!(
            s,
            "E{}{}",
            if decpt - 1 < 0 { '-' } else { '+' },
            (decpt - 1).abs()
        );
    } else if decpt <= 0 {
        s.push_str("0.");
        for _ in 0..-decpt {
            s.push('0');
        }
        s.push_str(&digits);
    } else {
        let dp = usize::try_from(decpt).expect("decpt");
        if digits.len() <= dp {
            s.push_str(&digits);
            for _ in digits.len()..dp {
                s.push('0');
            }
            s.push_str(".0");
        } else {
            s.push_str(&digits[..dp]);
            s.push('.');
            s.push_str(&digits[dp..]);
        }
    }
    s
}

impl V {
    /// `var_export($v, true)`.
    pub fn export(&self) -> String {
        let mut out = String::new();
        self.export_level(1, &mut out);
        out
    }

    fn export_level(&self, level: usize, out: &mut String) {
        match self {
            V::Null => out.push_str("NULL"),
            V::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            V::Int(i) => {
                if *i == i64::MIN {
                    out.push_str("-9223372036854775807-1");
                } else {
                    let _ = write!(out, "{i}");
                }
            }
            V::Float(f) => out.push_str(&export_float(*f)),
            V::Str(s) => export_str(s, out),
            V::Arr(items) => {
                if level > 1 {
                    out.push('\n');
                    out.push_str(&" ".repeat(level - 1));
                }
                out.push_str("array (\n");
                for (k, v) in items {
                    out.push_str(&" ".repeat(level + 1));
                    match k {
                        K::Int(i) => {
                            let _ = write!(out, "{i}");
                        }
                        K::Str(s) => export_str(s, out),
                    }
                    out.push_str(" => ");
                    v.export_level(level + 2, out);
                    out.push_str(",\n");
                }
                if level > 1 {
                    out.push_str(&" ".repeat(level - 1));
                }
                out.push(')');
            }
        }
    }
}

/// One oracle case.
#[derive(Debug, Clone)]
pub struct Case {
    /// Single-line PHP expression.
    pub php: String,
    /// Our rendered outcome.
    pub rust: String,
    /// Explicit expectation overriding the oracle (php-src master differs
    /// from the installed PHP).
    pub expected: Option<String>,
}

impl Case {
    /// Overrides the expected outcome instead of asking PHP.
    #[must_use]
    pub fn expect(mut self, expected: &str) -> Self {
        self.expected = Some(expected.to_owned());
        self
    }
}

fn render_notices(notices: &[Notice], out: &mut String) {
    for n in notices {
        let _ = writeln!(out, "{}: {}", n.level.label(), n);
    }
}

fn render_error(e: &StdError, out: &mut String) {
    let _ = write!(out, "!! {}: {}", e.kind.class_name(), e);
}

/// Builds a case from a fallible computation that may push notices.
pub fn case<T, F>(php: &str, f: F) -> Case
where
    T: Into<V>,
    F: FnOnce(&mut Vec<Notice>) -> Result<T, StdError>,
{
    assert!(
        !php.contains('\n'),
        "PHP expression must be one line: {php}"
    );
    let mut notices = Vec::new();
    let r = f(&mut notices);
    let mut out = String::new();
    render_notices(&notices, &mut out);
    match r {
        Ok(v) => {
            out.push_str("=> ");
            out.push_str(&v.into().export());
        }
        Err(e) => render_error(&e, &mut out),
    }
    Case {
        php: php.to_owned(),
        rust: out,
        expected: None,
    }
}

/// Builds a case from an infallible, notice-free computation.
pub fn pure<T: Into<V>>(php: &str, v: T) -> Case {
    case(php, |_| Ok::<T, StdError>(v))
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.txt"))
}

const DRIVER: &str = r#"<?php
set_error_handler(function ($no, $str) {
    $lvl = match ($no) {
        E_WARNING, E_USER_WARNING => 'Warning',
        E_NOTICE, E_USER_NOTICE => 'Notice',
        E_DEPRECATED, E_USER_DEPRECATED => 'Deprecated',
        default => "E$no",
    };
    echo "$lvl: $str\n";
    return true;
});
foreach (file($argv[1], FILE_IGNORE_NEW_LINES) as $expr) {
    echo "=== $expr\n";
    try {
        $r = eval("return $expr;");
        echo "=> ", var_export($r, true), "\n";
    } catch (Throwable $e) {
        echo "!! ", get_class($e), ": ", $e->getMessage(), "\n";
    }
}
"#;

fn parse_fixture(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut cur: Option<(String, String)> = None;
    for line in text.split('\n') {
        if let Some(expr) = line.strip_prefix("=== ") {
            if let Some((k, v)) = cur.take() {
                map.insert(k, v.trim_end_matches('\n').to_owned());
            }
            cur = Some((expr.to_owned(), String::new()));
        } else if let Some((_, v)) = cur.as_mut() {
            v.push_str(line);
            v.push('\n');
        }
    }
    if let Some((k, v)) = cur {
        map.insert(k, v.trim_end_matches('\n').to_owned());
    }
    map
}

/// Compares every case against the cached fixture.
pub fn check(name: &str, cases: &[Case]) {
    let path = fixture_path(name);
    let text = std::fs::read(&path).unwrap_or_default();
    let fixture = parse_fixture(&String::from_utf8_lossy(&text));
    let mut failures = Vec::new();
    for c in cases {
        let expected = match &c.expected {
            Some(e) => e.clone(),
            None => {
                if let Some(e) = fixture.get(&c.php) {
                    e.clone()
                } else {
                    failures.push(format!(
                        "{}\n  missing from fixture {name}.txt (run `cargo test -p php-stdstr --test oracle_{name} -- --ignored`)",
                        c.php
                    ));
                    continue;
                }
            }
        };
        if expected != c.rust {
            failures.push(format!(
                "{}\n  php:  {}\n  rust: {}",
                c.php,
                expected.replace('\n', "\n        "),
                c.rust.replace('\n', "\n        ")
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} oracle cases failed:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

/// Re-runs `/usr/bin/php` over all cases and rewrites the fixture.
pub fn regenerate(name: &str, cases: &[Case]) {
    let dir = std::env::temp_dir().join(format!("php-stdstr-oracle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let driver = dir.join("driver.php");
    let exprs = dir.join(format!("{name}.exprs"));
    std::fs::write(&driver, DRIVER).expect("write driver");
    let mut list = String::new();
    let mut seen = std::collections::HashSet::new();
    for c in cases.iter().filter(|c| c.expected.is_none()) {
        if seen.insert(c.php.clone()) {
            list.push_str(&c.php);
            list.push('\n');
        }
    }
    std::fs::write(&exprs, list).expect("write exprs");
    let out = Command::new("/usr/bin/php")
        .args(["-n", "-d", "error_reporting=-1", "-d", "display_errors=1"])
        .arg(&driver)
        .arg(&exprs)
        .output()
        .expect("run php");
    assert!(
        out.status.success(),
        "php failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::write(fixture_path(name), &out.stdout).expect("write fixture");
    let _ = std::fs::remove_dir_all(&dir);
}

/// PHP single-quoted string literal for a byte string; bytes that cannot
/// appear literally are emitted via a double-quoted escape concatenation.
pub fn lit(s: &[u8]) -> String {
    let printable = s.iter().all(|&b| (0x20..0x7f).contains(&b));
    if printable {
        let mut out = String::from("'");
        for &b in s {
            if b == b'\'' || b == b'\\' {
                out.push('\\');
            }
            out.push(char::from(b));
        }
        out.push('\'');
        out
    } else {
        let mut out = String::from("\"");
        for &b in s {
            match b {
                b'"' | b'\\' | b'$' => {
                    out.push('\\');
                    out.push(char::from(b));
                }
                0x20..=0x7e => out.push(char::from(b)),
                _ => {
                    let _ = write!(out, "\\x{b:02x}");
                }
            }
        }
        out.push('"');
        out
    }
}

#[test]
fn export_float_forms() {
    assert_eq!(export_float(1e15), "1000000000000000.0");
    assert_eq!(export_float(1e16), "10000000000000000.0");
    assert_eq!(export_float(1e17), "1.0E+17");
    assert_eq!(export_float(0.0001), "0.0001");
    assert_eq!(export_float(0.00001), "1.0E-5");
    assert_eq!(export_float(1.5e-7), "1.5E-7");
    assert_eq!(
        export_float(123_456_789_012_345_678.0),
        "1.2345678901234568E+17"
    );
    assert_eq!(export_float(-2.5), "-2.5");
    assert_eq!(
        V::from(vec![V::Int(1), V::from(vec![2i64])]).export(),
        "array (\n  0 => 1,\n  1 => \n  array (\n    0 => 2,\n  ),\n)"
    );
    assert_eq!(
        V::from(&b"a\0b'\\"[..]).export(),
        "'a' . \"\\0\" . 'b\\'\\\\'"
    );
}

//! Shared fixture reader for the oracle tests.  Fixtures are produced by
//! `tests/oracle/*.php` run under `/usr/bin/php` (ICU 74.2) — see
//! `regenerate_fixtures` in `tests/oracle_regen.rs`.
#![allow(dead_code, clippy::cast_precision_loss)]

use std::path::PathBuf;

/// A decoded fixture field.
#[derive(Debug, Clone, PartialEq)]
pub enum V {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    /// JSON text of an array.
    Arr(String),
}

impl V {
    pub fn s(&self) -> &str {
        match self {
            V::Str(s) => s,
            other => panic!("expected string, got {other:?}"),
        }
    }

    pub fn opt_s(&self) -> Option<&str> {
        match self {
            V::Null => None,
            V::Str(s) => Some(s),
            other => panic!("expected ?string, got {other:?}"),
        }
    }

    pub fn i(&self) -> i64 {
        match self {
            V::Int(i) => *i,
            other => panic!("expected int, got {other:?}"),
        }
    }

    pub fn f(&self) -> f64 {
        match self {
            V::Float(f) => *f,
            V::Int(i) => *i as f64,
            other => panic!("expected float, got {other:?}"),
        }
    }

    /// `Some(code name)` when the field is an `!ERROR` marker.
    pub fn err(&self) -> Option<&str> {
        match self {
            V::Str(s) => s.strip_prefix('!'),
            _ => None,
        }
    }

    /// Renders like the fixture would.
    pub fn show(&self) -> String {
        match self {
            V::Null => "~N".into(),
            V::Bool(b) => if *b { "~T" } else { "~F" }.into(),
            V::Int(i) => format!("i:{i}"),
            V::Float(f) => format!("d:{}", php_float(*f)),
            V::Str(s) => format!("s:{s}"),
            V::Arr(a) => format!("a:{a}"),
        }
    }
}

/// PHP's `var_export()` rendering of a float (shortest round-trip repr).
pub fn php_float(f: f64) -> String {
    if f.is_nan() {
        return "NAN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "INF".into() } else { "-INF".into() };
    }
    let s = format!("{f:?}");
    // Rust: 1e-7 → "1e-7"; PHP: "1.0E-7"
    if let Some((m, e)) = s.split_once('e') {
        let m = if m.contains('.') {
            m.to_owned()
        } else {
            format!("{m}.0")
        };
        let e = if e.starts_with('-') {
            e.to_owned()
        } else {
            format!("+{e}")
        };
        return format!("{m}E{e}");
    }
    s
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some(o) => out.push(o),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_field(f: &str) -> V {
    match f {
        "~N" => return V::Null,
        "~T" => return V::Bool(true),
        "~F" => return V::Bool(false),
        _ => {}
    }
    let (tag, rest) = f.split_at(2);
    match tag {
        "i:" => V::Int(rest.parse().expect("int field")),
        "d:" => V::Float(match rest {
            "INF" => f64::INFINITY,
            "-INF" => f64::NEG_INFINITY,
            "NAN" => f64::NAN,
            r => r.parse().expect("float field"),
        }),
        "s:" => V::Str(unescape(rest)),
        "a:" => V::Arr(rest.to_owned()),
        _ => panic!("bad field {f:?}"),
    }
}

/// Loads `tests/fixtures/<name>.tsv` as rows of fields.
pub fn load(name: &str) -> Vec<Vec<V>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.tsv"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.split('\t').map(parse_field).collect())
        .collect()
}

/// Collects mismatches and fails with a summary at the end.
#[derive(Default)]
pub struct Checker {
    pub total: usize,
    pub failures: Vec<String>,
}

impl Checker {
    pub fn check(&mut self, ctx: impl FnOnce() -> String, got: &str, want: &str) {
        self.total += 1;
        if got != want {
            self.failures
                .push(format!("{}\n    got:  {got:?}\n    want: {want:?}", ctx()));
        }
    }

    pub fn finish(self, name: &str) {
        if !self.failures.is_empty() {
            let n = self.failures.len();
            let shown: Vec<_> = self.failures.into_iter().take(40).collect();
            panic!(
                "{name}: {n}/{} mismatches\n{}",
                self.total,
                shown.join("\n")
            );
        }
        eprintln!("{name}: {} checks passed", self.total);
    }
}

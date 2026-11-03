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

/// Renders an error like the fixtures' `attempt()` helper does.
pub fn err_name(e: &php_intl::Error) -> String {
    use php_intl::Error;
    match e {
        Error::Intl(i) => format!("s:!{}", i.code.name()),
        Error::ArgumentValue { .. } | Error::Value(_) => "s:!ValueError".into(),
        Error::ArgumentType { .. } => "s:!TypeError".into(),
        Error::Thrown(_) => "s:!Error".into(),
        Error::ArgumentCount(_) => "s:!ArgumentCountError".into(),
        Error::IntlException(_) | Error::IntlExceptionInFunction(_) => "s:!IntlException".into(),
    }
}

/// `s:<string>` or the error marker.
pub fn show_s(r: php_intl::Result<String>) -> String {
    match r {
        Ok(s) => format!("s:{s}"),
        Err(e) => err_name(&e),
    }
}

/// JSON rendering of a string list (as `json_encode` with unescaped
/// unicode/slashes would produce).
pub fn json_strs(v: &[String]) -> String {
    let mut out = String::from("[");
    for (i, s) in v.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_str(s));
    }
    out.push(']');
    out
}

/// `json_encode` of one string (unescaped unicode and slashes).
pub fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = std::fmt::Write::write_fmt(&mut out, format_args!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A minimal JSON value (fixtures encode PHP arrays as JSON).
#[derive(Debug, Clone, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

/// Parses JSON produced by `json_encode` (with `PRESERVE_ZERO_FRACTION`).
pub fn json(s: &str) -> J {
    let b = s.as_bytes();
    let mut i = 0;

    json_value(b, &mut i)
}

fn ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && b[*i].is_ascii_whitespace() {
        *i += 1;
    }
}

fn json_value(b: &[u8], i: &mut usize) -> J {
    ws(b, i);
    match b[*i] {
        b'n' => {
            *i += 4;
            J::Null
        }
        b't' => {
            *i += 4;
            J::Bool(true)
        }
        b'f' => {
            *i += 5;
            J::Bool(false)
        }
        b'"' => J::Str(json_string(b, i)),
        b'[' => {
            *i += 1;
            let mut v = Vec::new();
            ws(b, i);
            if b[*i] == b']' {
                *i += 1;
                return J::Arr(v);
            }
            loop {
                v.push(json_value(b, i));
                ws(b, i);
                if b[*i] == b',' {
                    *i += 1;
                } else {
                    *i += 1;
                    return J::Arr(v);
                }
            }
        }
        b'{' => {
            *i += 1;
            let mut v = Vec::new();
            ws(b, i);
            if b[*i] == b'}' {
                *i += 1;
                return J::Obj(v);
            }
            loop {
                ws(b, i);
                let k = json_string(b, i);
                ws(b, i);
                *i += 1; // ':'
                v.push((k, json_value(b, i)));
                ws(b, i);
                if b[*i] == b',' {
                    *i += 1;
                } else {
                    *i += 1;
                    return J::Obj(v);
                }
            }
        }
        _ => {
            let start = *i;
            while *i < b.len()
                && (b[*i].is_ascii_digit() || matches!(b[*i], b'-' | b'+' | b'.' | b'e' | b'E'))
            {
                *i += 1;
            }
            let t = std::str::from_utf8(&b[start..*i]).expect("num");
            if t.contains(['.', 'e', 'E']) {
                J::Float(t.parse().expect("float"))
            } else {
                t.parse()
                    .map_or_else(|_| J::Float(t.parse().expect("float")), J::Int)
            }
        }
    }
}

fn json_string(b: &[u8], i: &mut usize) -> String {
    *i += 1;
    let mut out: Vec<u16> = Vec::new();
    let mut raw = Vec::new();
    loop {
        let c = b[*i];
        *i += 1;
        match c {
            b'"' => break,
            b'\\' => {
                let e = b[*i];
                *i += 1;
                let ch = match e {
                    b'n' => '\n',
                    b't' => '\t',
                    b'r' => '\r',
                    b'b' => '\u{8}',
                    b'f' => '\u{c}',
                    b'u' => {
                        let h = std::str::from_utf8(&b[*i..*i + 4]).expect("hex");
                        *i += 4;
                        out.extend(String::from_utf8_lossy(&raw).encode_utf16());
                        raw.clear();
                        out.push(u16::from_str_radix(h, 16).expect("hex"));
                        continue;
                    }
                    o => o as char,
                };
                raw.extend(ch.to_string().as_bytes());
            }
            o => raw.push(o),
        }
    }
    out.extend(String::from_utf8_lossy(&raw).encode_utf16());
    String::from_utf16_lossy(&out)
}

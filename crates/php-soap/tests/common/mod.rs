//! Shared helpers for the integration tests: a test engine, a `.phpt`
//! reader, an `EXPECTF` matcher and a tiny evaluator for the PHP literals
//! used as test parameters.

#![allow(
    dead_code,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::many_single_char_names,
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::similar_names,
    clippy::match_same_arms,
    clippy::type_complexity,
    clippy::items_after_statements,
    clippy::needless_pass_by_value,
    clippy::unused_self,
    clippy::format_collect,
    clippy::assigning_clones
)]

pub mod host;
pub mod php;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use php_runtime::array::ArrayKey;
use php_runtime::{RtResult, RuntimeError, Value, ZArray, ZStr};

pub use host::TestHost;

/// The ext/soap tests of the reference tree, when present.
pub fn soap_tests_dir() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modules/php-src/ext/soap/tests");
    p.is_dir().then_some(p)
}

/// The sections of a `.phpt` file.
pub fn phpt_sections(src: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut cur: Option<String> = None;
    let mut buf = String::new();
    for line in src.split_inclusive('\n') {
        let t = line.trim_end_matches(['\n', '\r']);
        if t.starts_with("--")
            && t.ends_with("--")
            && t.len() > 4
            && t[2..t.len() - 2]
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == '_')
        {
            if let Some(c) = cur.take() {
                out.insert(c, std::mem::take(&mut buf));
            }
            cur = Some(t[2..t.len() - 2].to_owned());
        } else if cur.is_some() {
            buf.push_str(line);
        }
    }
    if let Some(c) = cur {
        out.insert(c, buf);
    }
    out
}

fn match_from(p: &[u8], t: &[u8]) -> bool {
    if p.is_empty() {
        return t.is_empty();
    }
    if p[0] == b'%' && p.len() > 1 {
        let rest = &p[2..];
        let class = p[1];
        let ok_char = |c: u8| match class {
            b's' | b'S' => c != b'\n' && c != b'\r',
            b'a' | b'A' => true,
            b'w' => c.is_ascii_whitespace(),
            b'd' => c.is_ascii_digit(),
            b'i' => c.is_ascii_digit() || c == b'+' || c == b'-',
            b'x' => c.is_ascii_hexdigit(),
            b'f' => c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-'),
            _ => false,
        };
        match class {
            b'%' => return t.first() == Some(&b'%') && match_from(rest, &t[1..]),
            b'e' => return t.first() == Some(&b'/') && match_from(rest, &t[1..]),
            b'c' => return !t.is_empty() && match_from(rest, &t[1..]),
            b's' | b'S' | b'a' | b'A' | b'w' | b'd' | b'i' | b'x' | b'f' => {
                let min = usize::from(matches!(class, b's' | b'a' | b'd' | b'i' | b'x' | b'f'));
                let mut n = 0;
                while n < t.len() && ok_char(t[n]) {
                    n += 1;
                }
                let mut k = n;
                loop {
                    if k >= min && match_from(rest, &t[k..]) {
                        return true;
                    }
                    if k == 0 {
                        return false;
                    }
                    k -= 1;
                }
            }
            _ => {}
        }
    }
    !t.is_empty() && t[0] == p[0] && match_from(&p[1..], &t[1..])
}

/// `run-tests.php` comparison: trimmed, `EXPECTF` patterns when `fmt`.
pub fn output_matches(expected: &str, actual: &[u8], fmt: bool) -> bool {
    let e = expected.replace("\r\n", "\n");
    let e = e.trim().as_bytes().to_vec();
    let a = String::from_utf8_lossy(actual).replace("\r\n", "\n");
    let a = a.trim().as_bytes().to_vec();
    if fmt {
        match_from(&e, &a)
    } else {
        e == a
    }
}

// ---- PHP literal evaluator ----------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Str(Vec<u8>),
    Int(i64),
    Float(f64),
    Ident(String),
    Punct(&'static str),
}

fn lex(src: &str) -> Vec<Tok> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'\'' {
            let mut s = Vec::new();
            i += 1;
            while i < b.len() && b[i] != b'\'' {
                if b[i] == b'\\' && i + 1 < b.len() && (b[i + 1] == b'\'' || b[i + 1] == b'\\') {
                    i += 1;
                }
                s.push(b[i]);
                i += 1;
            }
            i += 1;
            out.push(Tok::Str(s));
        } else if c == b'"' {
            let mut s = Vec::new();
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' && i + 1 < b.len() {
                    i += 1;
                    match b[i] {
                        b'n' => s.push(b'\n'),
                        b't' => s.push(b'\t'),
                        b'r' => s.push(b'\r'),
                        b'x' => {
                            let h =
                                std::str::from_utf8(&b[i + 1..(i + 3).min(b.len())]).unwrap_or("0");
                            s.push(u8::from_str_radix(h, 16).unwrap_or(0));
                            i += 2;
                        }
                        other => s.push(other),
                    }
                } else {
                    s.push(b[i]);
                }
                i += 1;
            }
            i += 1;
            out.push(Tok::Str(s));
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit))
        {
            let start = i;
            if c == b'0' && matches!(b.get(i + 1), Some(b'x' | b'X')) {
                i += 2;
                while i < b.len() && b[i].is_ascii_hexdigit() {
                    i += 1;
                }
                out.push(Tok::Int(
                    i64::from_str_radix(&src[start + 2..i], 16).unwrap_or(0),
                ));
                continue;
            }
            while i < b.len() && (b[i].is_ascii_digit() || matches!(b[i], b'.' | b'e' | b'E')) {
                i += 1;
            }
            let t = &src[start..i];
            if t.contains(['.', 'e', 'E']) {
                out.push(Tok::Float(t.parse().unwrap_or(0.0)));
            } else {
                out.push(Tok::Int(t.parse().unwrap_or(0)));
            }
        } else if c.is_ascii_alphabetic() || c == b'_' || c == b'\\' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'\\') {
                i += 1;
            }
            out.push(Tok::Ident(src[start..i].to_owned()));
        } else if src[i..].starts_with("=>") {
            out.push(Tok::Punct("=>"));
            i += 2;
        } else if src[i..].starts_with("::") {
            out.push(Tok::Punct("::"));
            i += 2;
        } else {
            let p = match c {
                b'(' => "(",
                b')' => ")",
                b'[' => "[",
                b']' => "]",
                b',' => ",",
                b'-' => "-",
                b'$' => "$",
                _ => "?",
            };
            out.push(Tok::Punct(p));
            i += 1;
        }
    }
    out
}

/// Evaluates simple PHP expressions (literals, arrays, casts, `new` of the
/// SOAP value classes, constants).
pub struct Eval<'a> {
    toks: Vec<Tok>,
    pos: usize,
    host: &'a mut TestHost,
    pub vars: HashMap<String, Value>,
}

impl<'a> Eval<'a> {
    pub fn new(src: &str, host: &'a mut TestHost) -> Self {
        Self {
            toks: lex(src),
            pos: 0,
            host,
            vars: HashMap::new(),
        }
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn expect(&mut self, p: &'static str) -> RtResult<()> {
        match self.next() {
            Some(Tok::Punct(x)) if x == p => Ok(()),
            other => Err(RuntimeError::Error(format!("expected {p}, got {other:?}"))),
        }
    }

    fn is_punct(&self, p: &str) -> bool {
        matches!(self.peek(), Some(Tok::Punct(x)) if *x == p)
    }

    /// All comma-separated top-level expressions.
    pub fn args(&mut self) -> RtResult<Vec<Value>> {
        let mut out = Vec::new();
        while self.peek().is_some() && !self.is_punct(")") {
            out.push(self.expr()?);
            if self.is_punct(",") {
                self.pos += 1;
            }
        }
        Ok(out)
    }

    fn items(&mut self, close: &'static str) -> RtResult<ZArray> {
        let mut arr = ZArray::new();
        while !self.is_punct(close) {
            let v = self.expr()?;
            if self.is_punct("=>") {
                self.pos += 1;
                let val = self.expr()?;
                let key = v.to_array_key(self.host)?.unwrap_or(ArrayKey::Int(0));
                arr.make_mut().insert(key, val);
            } else {
                let _ = arr.make_mut().push(v);
            }
            if self.is_punct(",") {
                self.pos += 1;
            }
        }
        self.expect(close)?;
        Ok(arr)
    }

    #[allow(clippy::too_many_lines)]
    pub fn expr(&mut self) -> RtResult<Value> {
        match self.next() {
            Some(Tok::Str(s)) => Ok(Value::from(s.as_slice())),
            Some(Tok::Int(i)) => Ok(Value::Int(i)),
            Some(Tok::Float(f)) => Ok(Value::Float(f)),
            Some(Tok::Punct("-")) => match self.expr()? {
                Value::Int(i) => Ok(Value::Int(-i)),
                Value::Float(f) => Ok(Value::Float(-f)),
                v => Ok(v),
            },
            Some(Tok::Punct("[")) => Ok(Value::Array(self.items("]")?)),
            Some(Tok::Punct("$")) => {
                let Some(Tok::Ident(n)) = self.next() else {
                    return Err(RuntimeError::Error("bad variable".into()));
                };
                Ok(self.vars.get(&n).cloned().unwrap_or_default())
            }
            Some(Tok::Punct("(")) => {
                if let Some(Tok::Ident(c)) = self.peek().cloned() {
                    if c.eq_ignore_ascii_case("object")
                        && matches!(self.toks.get(self.pos + 1), Some(Tok::Punct(")")))
                    {
                        self.pos += 2;
                        let v = self.expr()?;
                        let o = self.host.new_object("stdClass");
                        if let Value::Array(a) = v {
                            for (k, v) in a.iter() {
                                let key = match k {
                                    ArrayKey::Int(i) => ZStr::from(i.to_string().as_str()),
                                    ArrayKey::Str(s) => s.clone(),
                                };
                                TestHost::props_of(&o)
                                    .borrow_mut()
                                    .make_mut()
                                    .insert(ArrayKey::Str(key), v.clone());
                            }
                        }
                        return Ok(Value::Object(o));
                    }
                }
                let v = self.expr()?;
                self.expect(")")?;
                Ok(v)
            }
            Some(Tok::Ident(id)) => {
                let lc = id.to_ascii_lowercase();
                match lc.as_str() {
                    "true" => return Ok(Value::Bool(true)),
                    "false" => return Ok(Value::Bool(false)),
                    "null" => return Ok(Value::Null),
                    "array" => {
                        self.expect("(")?;
                        return Ok(Value::Array(self.items(")")?));
                    }
                    "new" => {
                        let Some(Tok::Ident(class)) = self.next() else {
                            return Err(RuntimeError::Error("bad new".into()));
                        };
                        let args = if self.is_punct("(") {
                            self.pos += 1;
                            let a = self.args()?;
                            self.expect(")")?;
                            a
                        } else {
                            Vec::new()
                        };
                        return self.construct(&class, &args);
                    }
                    _ => {}
                }
                if self.is_punct("(") {
                    self.pos += 1;
                    let args = self.args()?;
                    self.expect(")")?;
                    return self.call(&lc, &args);
                }
                if let Some((_, v)) = php_soap::consts::php_constants()
                    .into_iter()
                    .find(|(n, _)| *n == id)
                {
                    return Ok(v);
                }
                Err(RuntimeError::Error(format!("unknown constant {id}")))
            }
            other => Err(RuntimeError::Error(format!("unexpected token {other:?}"))),
        }
    }

    fn construct(&mut self, class: &str, args: &[Value]) -> RtResult<Value> {
        let o = self.host.new_object(class);
        let bytes = |v: Option<&Value>| -> Option<Vec<u8>> {
            match v {
                Some(Value::Str(s)) => Some(s.as_bytes().to_vec()),
                _ => None,
            }
        };
        match class.to_ascii_lowercase().as_str() {
            "soapvar" => {
                let enc = match args.get(1) {
                    Some(Value::Int(i)) => Some(*i),
                    _ => None,
                };
                php_soap::objects::soap_var_construct(
                    self.host,
                    &o,
                    args.first().unwrap_or(&Value::Null),
                    enc,
                    bytes(args.get(2)).as_deref(),
                    bytes(args.get(3)).as_deref(),
                    bytes(args.get(4)).as_deref(),
                    bytes(args.get(5)).as_deref(),
                )?;
            }
            "soapparam" => {
                php_soap::objects::soap_param_construct(
                    self.host,
                    &o,
                    args.first().unwrap_or(&Value::Null),
                    &bytes(args.get(1)).unwrap_or_default(),
                )?;
            }
            "soapheader" => {
                let actor = match args.get(4) {
                    Some(Value::Str(s)) => {
                        Some(php_soap::objects::Actor::Uri(s.as_bytes().to_vec()))
                    }
                    Some(Value::Int(i)) => Some(php_soap::objects::Actor::Code(*i)),
                    _ => None,
                };
                php_soap::objects::soap_header_construct(
                    self.host,
                    &o,
                    &bytes(args.first()).unwrap_or_default(),
                    &bytes(args.get(1)).unwrap_or_default(),
                    args.get(2),
                    matches!(args.get(3), Some(Value::Bool(true))),
                    actor,
                )?;
            }
            _ => {}
        }
        Ok(Value::Object(o))
    }

    fn call(&mut self, name: &str, args: &[Value]) -> RtResult<Value> {
        let int = |i: usize| -> i64 {
            match args.get(i) {
                Some(Value::Int(v)) => *v,
                _ => 0,
            }
        };
        match name {
            "gmmktime" => {
                let (h, mi, s, mo, d, y) = (int(0), int(1), int(2), int(3), int(4), int(5));
                Ok(Value::Int(
                    days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + s,
                ))
            }
            _ => Err(RuntimeError::Error(format!("unknown function {name}"))),
        }
    }
}

/// Days since 1970-01-01 for a civil date.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Runs `f` on a thread with a large stack (deep SOAP payloads recurse).
pub fn with_big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(f)
        .expect("spawn")
        .join()
        .expect("join")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expectf() {
        assert!(output_matches("a %s b %d", b"a xyz b 12", true));
        assert!(!output_matches("a %s b", b"a  b", true));
        assert!(output_matches("x%Ay", b"x\n\ny", true));
    }
}

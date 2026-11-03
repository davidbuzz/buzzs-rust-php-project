//! Loading `.phpt` files and comparing output like run-tests.php.

use std::collections::HashMap;
use std::path::PathBuf;

pub struct Phpt {
    pub sections: HashMap<String, String>,
}

impl Phpt {
    pub fn load(rel: &str) -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/phpt")
            .join(rel);
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let mut sections = HashMap::new();
        let mut cur: Option<String> = None;
        let mut buf = String::new();
        for line in text.split_inclusive('\n') {
            let t = line.trim_end_matches(['\n', '\r']);
            if t.len() > 4
                && t.starts_with("--")
                && t.ends_with("--")
                && t[2..t.len() - 2]
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c == '_')
            {
                if let Some(name) = cur.take() {
                    sections.insert(name, std::mem::take(&mut buf));
                }
                cur = Some(t[2..t.len() - 2].to_string());
                continue;
            }
            buf.push_str(line);
        }
        if let Some(name) = cur {
            sections.insert(name, buf);
        }
        Self { sections }
    }

    pub fn file(&self) -> &str {
        self.sections.get("FILE").map_or("", String::as_str)
    }

    /// `bcmath.scale` from the INI section, if any.
    pub fn ini_scale(&self) -> Option<i64> {
        let ini = self.sections.get("INI")?;
        ini.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == "bcmath.scale")
                .then(|| v.trim().parse().ok())
                .flatten()
        })
    }

    /// Checks `out` against EXPECT/EXPECTF; returns a diff description on mismatch.
    pub fn check(&self, out: &str) -> Result<(), String> {
        let out = normalize(out);
        if let Some(exp) = self.sections.get("EXPECT") {
            let exp = normalize(&exp.replace("\r\n", "\n"));
            if exp == out {
                return Ok(());
            }
            return Err(diff(&exp, &out));
        }
        if let Some(exp) = self.sections.get("EXPECTF") {
            let exp = normalize(&exp.replace("\r\n", "\n"));
            if expectf_match(exp.as_bytes(), out.as_bytes()) {
                return Ok(());
            }
            return Err(diff(&exp, &out));
        }
        Err("no EXPECT section".into())
    }
}

fn normalize(s: &str) -> String {
    // Object handles depend on allocation order; the harness prints #1.
    const KEY: &str = "object(BcMath\\Number)#";
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find(KEY) {
        out.push_str(&rest[..p + KEY.len()]);
        rest = &rest[p + KEY.len()..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            out.push('1');
            rest = &rest[digits..];
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

fn diff(exp: &str, got: &str) -> String {
    let e: Vec<&str> = exp.lines().collect();
    let g: Vec<&str> = got.lines().collect();
    for k in 0..e.len().max(g.len()) {
        let (a, b) = (e.get(k).copied(), g.get(k).copied());
        if a != b {
            return format!(
                "first difference at line {}:\n  expected: {a:?}\n  got:      {b:?}",
                k + 1
            );
        }
    }
    "outputs differ".into()
}

/// A wildcard's minimum length and accepted bytes.
type Class = Option<(usize, fn(u8) -> bool)>;

/// run-tests.php `%` wildcards (the subset used by ext/bcmath tests).
fn expectf_match(p: &[u8], s: &[u8]) -> bool {
    if p.is_empty() {
        return s.is_empty();
    }
    if p[0] == b'%' && p.len() >= 2 {
        let rest = &p[2..];
        let class: Class = match p[1] {
            b's' => Some((1, |c| c != b'\n')),
            b'S' => Some((0, |c| c != b'\n')),
            b'a' => Some((1, |_| true)),
            b'A' => Some((0, |_| true)),
            b'd' => Some((1, |c| c.is_ascii_digit())),
            b'i' => Some((1, |c| c.is_ascii_digit() || c == b'-' || c == b'+')),
            b'x' => Some((1, |c| c.is_ascii_hexdigit())),
            b'f' => Some((1, |c| c.is_ascii_digit() || b".-+eE".contains(&c))),
            _ => None,
        };
        if let Some((min, ok)) = class {
            let max = s.iter().take_while(|&&c| ok(c)).count();
            return (min..=max).rev().any(|n| expectf_match(rest, &s[n..]));
        }
        if p[1] == b'%' {
            return s.first() == Some(&b'%') && expectf_match(rest, &s[1..]);
        }
        if p[1] == b'c' {
            return !s.is_empty() && expectf_match(rest, &s[1..]);
        }
        if p[1] == b'e' {
            return s.first() == Some(&b'/') && expectf_match(rest, &s[1..]);
        }
    }
    !s.is_empty() && s[0] == p[0] && expectf_match(&p[1..], &s[1..])
}

// ---------------------------------------------------------------------------
// Tiny parser for the string-array literals in the scripts.
// ---------------------------------------------------------------------------

/// Parses the PHP string literals of an array body (`"a", 'b', ...`).
fn parse_items(body: &str) -> Vec<String> {
    let mut items = Vec::new();
    let b = body.as_bytes();
    let mut k = 0;
    while k < b.len() {
        let q = b[k];
        if q == b'"' || q == b'\'' {
            let start = k + 1;
            let mut end = start;
            while b[end] != q {
                end += 1;
            }
            items.push(body[start..end].to_string());
            k = end + 1;
        } else {
            k += 1;
        }
    }
    items
}

/// Collects `$var = [...]` and `$var = array_merge($other, [...])`.
pub fn string_arrays(file: &str) -> HashMap<String, Vec<String>> {
    let mut vars: HashMap<String, Vec<String>> = HashMap::new();
    let mut rest = file;
    while let Some(p) = rest.find('$') {
        rest = &rest[p + 1..];
        let name_len = rest
            .bytes()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == b'_')
            .count();
        let name = rest[..name_len].to_string();
        let after = rest[name_len..].trim_start();
        let Some(after) = after.strip_prefix('=') else {
            continue;
        };
        let after = after.trim_start();
        if let Some(body) = after.strip_prefix('[') {
            let end = body.find("];").expect("array end");
            vars.insert(name, parse_items(&body[..end]));
        } else if let Some(args) = after.strip_prefix("array_merge(") {
            let other_len = args[1..]
                .bytes()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == b'_')
                .count();
            let other = &args[1..=other_len];
            let body_start = args.find('[').expect("merge array");
            let end = args.find("]);").expect("merge end");
            let mut items = vars.get(other).cloned().expect("merged var");
            items.extend(parse_items(&args[body_start + 1..end]));
            vars.insert(name, items);
        }
    }
    vars
}

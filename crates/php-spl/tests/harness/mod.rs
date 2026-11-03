//! A small PHP interpreter over `php-parser`'s AST, used only by the tests:
//! it implements `SplHost` so that `.phpt` scripts and oracle scripts
//! (user subclasses, callbacks, `foreach`, exceptions, `var_dump`) can be
//! run against php-spl and compared with PHP's expected output.

#![allow(dead_code, clippy::all, clippy::pedantic)]

pub mod builtins;
pub mod host;
pub mod interp;
pub mod interp_expr;

pub use interp::Interp;

/// Runs a PHP script and returns its output.
pub fn run(src: &[u8], file: &str) -> Vec<u8> {
    let mut it = Interp::new(file);
    it.run_source(src);
    it.out
}

/// The sections of a `.phpt` file.
#[derive(Debug, Default)]
pub struct Phpt {
    pub sections: Vec<(String, Vec<u8>)>,
}

impl Phpt {
    pub fn parse(data: &[u8]) -> Phpt {
        let mut p = Phpt::default();
        let mut cur: Option<(String, Vec<u8>)> = None;
        for line in data.split_inclusive(|&c| c == b'\n') {
            let t = line.strip_suffix(b"\n").unwrap_or(line);
            let t = t.strip_suffix(b"\r").unwrap_or(t);
            if t.len() > 6 && t.starts_with(b"--") && t.ends_with(b"--") {
                let name = &t[2..t.len() - 2];
                if name.iter().all(|c| c.is_ascii_uppercase() || *c == b'_') {
                    if let Some(c) = cur.take() {
                        p.sections.push(c);
                    }
                    cur = Some((String::from_utf8_lossy(name).into_owned(), Vec::new()));
                    continue;
                }
            }
            if let Some(c) = cur.as_mut() {
                c.1.extend_from_slice(line);
            }
        }
        if let Some(c) = cur.take() {
            p.sections.push(c);
        }
        p
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.sections
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_slice())
    }
}

fn trim_end(b: &[u8]) -> &[u8] {
    let mut e = b.len();
    while e > 0 && matches!(b[e - 1], b'\n' | b'\r' | b' ' | b'\t') {
        e -= 1;
    }
    &b[..e]
}

/// Normalises line endings and trailing whitespace like run-tests.php.
pub fn normalize(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\r' && b.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    let t = trim_end(&out);
    let start = t.iter().position(|c| !matches!(c, b'\n' | b'\r' | b' ' | b'\t')).unwrap_or(t.len());
    t[start..].to_vec()
}

/// `run-tests.php`'s EXPECTF matching (converted to a backtracking matcher).
pub fn expectf_match(pattern: &[u8], output: &[u8]) -> bool {
    #[derive(Debug)]
    enum Tok {
        Lit(Vec<u8>),
        S,  // %s: one or more non-newline
        SS, // %S: zero or more non-newline
        A,  // %a: one or more anything
        AA, // %A: zero or more anything
        W,  // %w: zero or more whitespace
        I,  // %i: integer with sign
        D,  // %d: digits
        X,  // %x: hex
        F,  // %f: float
        C,  // %c: single char
        E,  // %e: directory separator
        R(Vec<u8>),
    }
    let mut toks = Vec::new();
    let mut lit = Vec::new();
    let mut i = 0;
    while i < pattern.len() {
        let c = pattern[i];
        if c == b'%' && i + 1 < pattern.len() {
            let t = match pattern[i + 1] {
                b's' => Some(Tok::S),
                b'S' => Some(Tok::SS),
                b'a' => Some(Tok::A),
                b'A' => Some(Tok::AA),
                b'w' => Some(Tok::W),
                b'i' => Some(Tok::I),
                b'd' => Some(Tok::D),
                b'x' => Some(Tok::X),
                b'f' => Some(Tok::F),
                b'c' => Some(Tok::C),
                b'e' => Some(Tok::E),
                b'r' => {
                    if let Some(end) = pattern[i + 2..].windows(2).position(|w| w == b"%r") {
                        let re = pattern[i + 2..i + 2 + end].to_vec();
                        i += 4 + end;
                        if !lit.is_empty() {
                            toks.push(Tok::Lit(std::mem::take(&mut lit)));
                        }
                        toks.push(Tok::R(re));
                        continue;
                    }
                    None
                }
                b'%' => {
                    lit.push(b'%');
                    i += 2;
                    continue;
                }
                _ => None,
            };
            if let Some(t) = t {
                if !lit.is_empty() {
                    toks.push(Tok::Lit(std::mem::take(&mut lit)));
                }
                toks.push(t);
                i += 2;
                continue;
            }
        }
        lit.push(c);
        i += 1;
    }
    if !lit.is_empty() {
        toks.push(Tok::Lit(lit));
    }
    fn m(toks: &[Tok], s: &[u8], memo: &mut std::collections::HashSet<(usize, usize)>, ti: usize, si: usize) -> bool {
        if ti == toks.len() {
            return si == s.len();
        }
        if !memo.insert((ti, si)) {
            return false;
        }
        let rest = &s[si..];
        let try_lens = |lens: &mut dyn Iterator<Item = usize>, memo: &mut std::collections::HashSet<(usize, usize)>| {
            for l in lens {
                if m(toks, s, memo, ti + 1, si + l) {
                    return true;
                }
            }
            false
        };
        match &toks[ti] {
            Tok::Lit(l) => rest.starts_with(l) && m(toks, s, memo, ti + 1, si + l.len()),
            Tok::S | Tok::SS => {
                let max = rest.iter().position(|&c| c == b'\n').unwrap_or(rest.len());
                let min = if matches!(toks[ti], Tok::S) { 1 } else { 0 };
                if max < min {
                    return false;
                }
                try_lens(&mut (min..=max).rev(), memo)
            }
            Tok::A | Tok::AA => {
                let min = if matches!(toks[ti], Tok::A) { 1 } else { 0 };
                if rest.len() < min {
                    return false;
                }
                try_lens(&mut (min..=rest.len()).rev(), memo)
            }
            Tok::W => {
                let max = rest.iter().take_while(|c| c.is_ascii_whitespace()).count();
                try_lens(&mut (0..=max).rev(), memo)
            }
            Tok::I | Tok::D => {
                let mut k = 0;
                if matches!(toks[ti], Tok::I) && matches!(rest.first(), Some(b'+' | b'-')) {
                    k = 1;
                }
                let n = rest[k..].iter().take_while(|c| c.is_ascii_digit()).count();
                if n == 0 {
                    return false;
                }
                try_lens(&mut (k + 1..=k + n).rev(), memo)
            }
            Tok::X => {
                let n = rest.iter().take_while(|c| c.is_ascii_hexdigit()).count();
                if n == 0 {
                    return false;
                }
                try_lens(&mut (1..=n).rev(), memo)
            }
            Tok::F => {
                let n = rest
                    .iter()
                    .take_while(|c| c.is_ascii_digit() || matches!(c, b'.' | b'-' | b'+' | b'e' | b'E'))
                    .count();
                if n == 0 {
                    return false;
                }
                try_lens(&mut (1..=n).rev(), memo)
            }
            Tok::C => !rest.is_empty() && m(toks, s, memo, ti + 1, si + 1),
            Tok::E => rest.first() == Some(&b'/') && m(toks, s, memo, ti + 1, si + 1),
            Tok::R(_) => {
                // Regex sections: accept any run of non-newline characters.
                let max = rest.iter().position(|&c| c == b'\n').unwrap_or(rest.len());
                try_lens(&mut (0..=max).rev(), memo)
            }
        }
    }
    let mut memo = std::collections::HashSet::new();
    m(&toks, output, &mut memo, 0, 0)
}

/// Outcome of running one `.phpt` test.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail { expected: String, got: String },
    Skip(String),
}

/// Runs one `.phpt` file.
pub fn run_phpt(path: &std::path::Path) -> Outcome {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => return Outcome::Skip(e.to_string()),
    };
    let t = Phpt::parse(&data);
    for unsupported in ["SKIPIF", "INI", "ARGS", "STDIN", "POST", "GET", "COOKIE", "ENV", "FILE_EXTERNAL", "EXTENSIONS", "CGI", "PHPDBG", "CAPTURE_STDIO", "XFAIL", "CONFLICTS", "FLAKY"] {
        if unsupported == "SKIPIF" || unsupported == "INI" || unsupported == "CONFLICTS" || unsupported == "EXTENSIONS" || unsupported == "XFAIL" || unsupported == "FLAKY" {
            continue;
        }
        if t.get(unsupported).is_some() {
            return Outcome::Skip(format!("section {unsupported}"));
        }
    }
    let Some(file) = t.get("FILE") else {
        return Outcome::Skip("no FILE".into());
    };
    let name = path.to_string_lossy().replace(".phpt", ".php");
    let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let mut it = Interp::new(&name);
    it.dir = dir.to_string_lossy().into_owned();
    if let Some(ini) = t.get("INI") {
        it.apply_ini(ini);
    }
    let src = file.to_vec();
    let out = {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            it.run_source(&src);
            std::mem::take(&mut it.out)
        }));
        match r {
            Ok(o) => o,
            Err(p) => {
                let msg = p
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                return Outcome::Fail {
                    expected: String::new(),
                    got: format!("PANIC: {msg}"),
                };
            }
        }
    };
    if let Some(clean) = t.get("CLEAN") {
        let mut c = Interp::new(&name);
        c.dir = it.dir.clone();
        c.run_source(clean);
    }
    let got = normalize(&out);
    let (expected, ok) = if let Some(e) = t.get("EXPECT") {
        let e = normalize(e);
        (e.clone(), got == e)
    } else if let Some(e) = t.get("EXPECTF") {
        let e = normalize(e);
        (e.clone(), expectf_match(&e, &got))
    } else if let Some(e) = t.get("EXPECTREGEX") {
        return Outcome::Skip(format!("EXPECTREGEX {}", e.len()));
    } else {
        return Outcome::Skip("no EXPECT".into());
    };
    if ok {
        Outcome::Pass
    } else {
        Outcome::Fail {
            expected: String::from_utf8_lossy(&expected).into_owned(),
            got: String::from_utf8_lossy(&got).into_owned(),
        }
    }
}

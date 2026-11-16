// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A miniature `.phpt` runner for `ext/uri/tests`: it evaluates the small
//! PHP subset those tests use directly against `php_uri::api`.

pub mod ast;
pub mod interp;
pub mod lexer;

use std::path::Path;

/// The outcome of one test file.
#[derive(Debug)]
pub enum Outcome {
    Pass,
    Fail { expected: String, actual: String },
    Unsupported(String),
}

fn sections(text: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    for line in text.split_inclusive(|&c| c == b'\n') {
        let trimmed = line.strip_suffix(b"\n").unwrap_or(line);
        let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);
        if trimmed.len() > 4
            && trimmed.starts_with(b"--")
            && trimmed.ends_with(b"--")
            && trimmed[2..trimmed.len() - 2]
                .iter()
                .all(|c| c.is_ascii_uppercase() || *c == b'_')
        {
            out.push((
                String::from_utf8_lossy(&trimmed[2..trimmed.len() - 2]).into_owned(),
                Vec::new(),
            ));
            continue;
        }
        if let Some(last) = out.last_mut() {
            last.1.extend_from_slice(line);
        }
    }
    out
}

fn trim(s: &[u8]) -> &[u8] {
    let start = s
        .iter()
        .position(|c| !c.is_ascii_whitespace() && *c != 0)
        .unwrap_or(s.len());
    let end = s
        .iter()
        .rposition(|c| !c.is_ascii_whitespace() && *c != 0)
        .map_or(start, |e| e + 1);
    &s[start..end.max(start)]
}

/// Replaces `%r...%r` sections made of literal characters and `\xHH`
/// escapes by their literal bytes (the only regex form used by the suite).
fn expand_regex_sections(pat: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < pat.len() {
        if pat[i..].starts_with(b"%r") {
            let end = lexer::find(&pat[i + 2..], b"%r")? + i + 2;
            let mut j = i + 2;
            while j < end {
                if pat[j] == b'\\' && pat.get(j + 1) == Some(&b'x') && j + 3 < end {
                    let hex = std::str::from_utf8(&pat[j + 2..j + 4]).ok()?;
                    out.push(u8::from_str_radix(hex, 16).ok()?);
                    j += 4;
                } else if pat[j].is_ascii_alphanumeric() {
                    out.push(pat[j]);
                    j += 1;
                } else {
                    return None;
                }
            }
            i = end + 2;
        } else {
            out.push(pat[i]);
            i += 1;
        }
    }
    Some(out)
}

/// Matches `actual` against an EXPECTF pattern.
pub fn expectf_match(pat: &[u8], s: &[u8]) -> bool {
    let Some(pat) = expand_regex_sections(pat) else {
        return false;
    };
    let pat = pat.as_slice();
    fn m(p: &[u8], s: &[u8]) -> bool {
        if p.is_empty() {
            return s.is_empty();
        }
        if p[0] == b'%' && p.len() >= 2 {
            let rest = &p[2..];
            let class: Option<fn(u8) -> bool> = match p[1] {
                b'd' => Some(|c: u8| c.is_ascii_digit()),
                b'i' => Some(|c: u8| c.is_ascii_digit() || c == b'-' || c == b'+'),
                b'x' => Some(|c: u8| c.is_ascii_hexdigit()),
                b's' => Some(|c: u8| c != b'\n' && c != b'\r'),
                b'a' => Some(|_| true),
                b'S' => Some(|c: u8| c != b'\n' && c != b'\r'),
                b'A' => Some(|_| true),
                b'w' => Some(|c: u8| c.is_ascii_whitespace()),
                b'f' => Some(|c: u8| {
                    c.is_ascii_digit() || matches!(c, b'.' | b'-' | b'+' | b'e' | b'E')
                }),
                b'c' => {
                    return !s.is_empty() && m(rest, &s[1..]);
                }
                b'e' => {
                    return !s.is_empty() && s[0] == b'/' && m(rest, &s[1..]);
                }
                b'0' => {
                    return !s.is_empty() && s[0] == 0 && m(rest, &s[1..]);
                }
                _ => None,
            };
            if let Some(class) = class {
                let min = usize::from(!matches!(p[1], b'S' | b'A' | b'w'));
                let mut n = 0;
                while n < s.len() && class(s[n]) {
                    n += 1;
                }
                let mut k = n;
                loop {
                    if k >= min && m(rest, &s[k..]) {
                        return true;
                    }
                    if k == 0 {
                        return false;
                    }
                    k -= 1;
                }
            }
        }
        !s.is_empty() && p[0] == s[0] && m(&p[1..], &s[1..])
    }
    m(pat, s)
}

/// Runs one `.phpt` file.
pub fn run_file(path: &Path) -> Outcome {
    let Ok(text) = std::fs::read(path) else {
        return Outcome::Unsupported("unreadable".into());
    };
    let secs = sections(&text);
    let get = |n: &str| secs.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
    for unsupported in [
        "SKIPIF",
        "INI",
        "EXTENSIONS",
        "EXPECTREGEX",
        "POST",
        "GET",
        "ENV",
        "ARGS",
    ] {
        if get(unsupported).is_some() {
            return Outcome::Unsupported(format!("section {unsupported}"));
        }
    }
    let Some(code) = get("FILE") else {
        return Outcome::Unsupported("no FILE".into());
    };
    let (expected, is_f) = match (get("EXPECT"), get("EXPECTF")) {
        (Some(e), _) => (e, false),
        (None, Some(e)) => (e, true),
        _ => return Outcome::Unsupported("no EXPECT".into()),
    };
    let toks = match lexer::lex(&code) {
        Ok(t) => t,
        Err(e) => return Outcome::Unsupported(format!("lex: {e}")),
    };
    let prog = match ast::Parser::new(toks).program() {
        Ok(p) => p,
        Err(e) => return Outcome::Unsupported(format!("parse: {e}")),
    };
    let mut it = interp::Interp::new();
    match it.run(&prog) {
        Ok(()) => {}
        Err(interp::Flow::Unsupported(m)) => return Outcome::Unsupported(m),
        Err(interp::Flow::Throw(ex)) => {
            let msg = interp::uncaught(&it, &ex);
            it.out.extend(msg);
        }
    }
    let expected_norm: Vec<u8> = expected.iter().copied().filter(|&c| c != b'\r').collect();
    let want = trim(&expected_norm);
    let got = trim(&it.out);
    let ok = if is_f {
        expectf_match(want, got)
    } else {
        want == got
    };
    if ok {
        Outcome::Pass
    } else {
        Outcome::Fail {
            expected: String::from_utf8_lossy(want).into_owned(),
            actual: String::from_utf8_lossy(got).into_owned(),
        }
    }
}

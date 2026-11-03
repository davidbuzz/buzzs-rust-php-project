//! Tokenizer for the small PHP subset used by `ext/uri/tests`.

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Var(String),
    Ident(String),
    Str(Vec<u8>),
    /// A double-quoted string with `$var` interpolation parts.
    Interp(Vec<Part>),
    Int(i64),
    Punct(&'static str),
    End,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Lit(Vec<u8>),
    Var(String),
}

const PUNCTS: [&str; 34] = [
    "...", "===", "!==", "?->", "<=>", "->", "::", "=>", "==", "!=", "&&", "||", "++", "--", "<=",
    ">=", "??", "(", ")", "[", "]", "{", "}", ",", ";", "=", ".", "!", "&", "?", ":", "<", ">",
    "|",
];
const PUNCTS2: [&str; 4] = ["+", "-", "*", "/"];

pub fn lex(src: &[u8]) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    // Skip to "<?php".
    if let Some(pos) = find(src, b"<?php") {
        i = pos + 5;
    }
    while i < src.len() {
        let c = src[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if src[i..].starts_with(b"?>") {
            break;
        }
        if src[i..].starts_with(b"//") || c == b'#' && !src[i..].starts_with(b"#[") {
            while i < src.len() && src[i] != b'\n' {
                if src[i..].starts_with(b"?>") {
                    break;
                }
                i += 1;
            }
            continue;
        }
        if src[i..].starts_with(b"#[") {
            // Attribute: skip to matching ']'.
            let mut depth = 0;
            while i < src.len() {
                if src[i] == b'[' {
                    depth += 1;
                } else if src[i] == b']' {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                i += 1;
            }
            continue;
        }
        if src[i..].starts_with(b"/*") {
            let end = find(&src[i + 2..], b"*/").ok_or("unterminated comment")?;
            i += end + 4;
            continue;
        }
        if c == b'$' && i + 1 < src.len() && is_ident_start(src[i + 1]) {
            let s = i + 1;
            i += 1;
            while i < src.len() && is_ident(src[i]) {
                i += 1;
            }
            out.push(Tok::Var(String::from_utf8_lossy(&src[s..i]).into_owned()));
            continue;
        }
        if is_ident_start(c) || c == b'\\' {
            let s = i;
            while i < src.len() && (is_ident(src[i]) || src[i] == b'\\') {
                i += 1;
            }
            out.push(Tok::Ident(String::from_utf8_lossy(&src[s..i]).into_owned()));
            continue;
        }
        if c.is_ascii_digit() {
            let s = i;
            while i < src.len() && (src[i].is_ascii_digit() || src[i] == b'_') {
                i += 1;
            }
            let text: String = String::from_utf8_lossy(&src[s..i]).replace('_', "");
            out.push(Tok::Int(text.parse().map_err(|_| "bad int")?));
            continue;
        }
        if c == b'\'' {
            i += 1;
            let mut v = Vec::new();
            while i < src.len() && src[i] != b'\'' {
                if src[i] == b'\\'
                    && i + 1 < src.len()
                    && (src[i + 1] == b'\'' || src[i + 1] == b'\\')
                {
                    v.push(src[i + 1]);
                    i += 2;
                } else {
                    v.push(src[i]);
                    i += 1;
                }
            }
            i += 1;
            out.push(Tok::Str(v));
            continue;
        }
        if c == b'"' {
            i += 1;
            let mut parts = Vec::new();
            let mut v = Vec::new();
            while i < src.len() && src[i] != b'"' {
                if src[i] == b'\\' && i + 1 < src.len() {
                    let e = src[i + 1];
                    i += 2;
                    match e {
                        b'n' => v.push(b'\n'),
                        b't' => v.push(b'\t'),
                        b'r' => v.push(b'\r'),
                        b'v' => v.push(0x0B),
                        b'e' => v.push(0x1B),
                        b'f' => v.push(0x0C),
                        b'\\' => v.push(b'\\'),
                        b'$' => v.push(b'$'),
                        b'"' => v.push(b'"'),
                        b'0'..=b'7' => {
                            let mut n = u32::from(e - b'0');
                            let mut k = 0;
                            while k < 2 && i < src.len() && (b'0'..=b'7').contains(&src[i]) {
                                n = n * 8 + u32::from(src[i] - b'0');
                                i += 1;
                                k += 1;
                            }
                            v.push((n & 0xFF) as u8);
                        }
                        b'x' if i < src.len() && src[i].is_ascii_hexdigit() => {
                            let mut n = 0u32;
                            let mut k = 0;
                            while k < 2 && i < src.len() && src[i].is_ascii_hexdigit() {
                                n = n * 16 + (src[i] as char).to_digit(16).unwrap_or(0);
                                i += 1;
                                k += 1;
                            }
                            v.push(n as u8);
                        }
                        b'u' if i < src.len() && src[i] == b'{' => {
                            let close = find(&src[i..], b"}").ok_or("bad \\u")?;
                            let hex = std::str::from_utf8(&src[i + 1..i + close])
                                .map_err(|_| "bad \\u")?;
                            let cp = u32::from_str_radix(hex, 16).map_err(|_| "bad \\u")?;
                            let ch = char::from_u32(cp).ok_or("bad cp")?;
                            let mut buf = [0u8; 4];
                            v.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                            i += close + 1;
                        }
                        _ => {
                            v.push(b'\\');
                            v.push(e);
                        }
                    }
                    continue;
                }
                if src[i] == b'$' && i + 1 < src.len() && is_ident_start(src[i + 1]) {
                    if !v.is_empty() {
                        parts.push(Part::Lit(std::mem::take(&mut v)));
                    }
                    let s = i + 1;
                    i += 1;
                    while i < src.len() && is_ident(src[i]) {
                        i += 1;
                    }
                    parts.push(Part::Var(String::from_utf8_lossy(&src[s..i]).into_owned()));
                    continue;
                }
                if src[i] == b'{' && i + 1 < src.len() && src[i + 1] == b'$' {
                    return Err("complex interpolation".into());
                }
                v.push(src[i]);
                i += 1;
            }
            i += 1;
            if parts.is_empty() {
                out.push(Tok::Str(v));
            } else {
                if !v.is_empty() {
                    parts.push(Part::Lit(v));
                }
                out.push(Tok::Interp(parts));
            }
            continue;
        }
        if src[i..].starts_with(b"<<<") {
            return Err("heredoc".into());
        }
        let mut matched = false;
        for p in PUNCTS.iter().chain(PUNCTS2.iter()) {
            if src[i..].starts_with(p.as_bytes()) {
                out.push(Tok::Punct(p));
                i += p.len();
                matched = true;
                break;
            }
        }
        if !matched {
            return Err(format!("unexpected char {:?}", c as char));
        }
    }
    out.push(Tok::End);
    Ok(out)
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

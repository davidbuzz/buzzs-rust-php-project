// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `get_meta_tags()` (`php_next_meta_token` in `ext/standard/file.c`).

use php_streams::stream::Stream;

/// `META_DEF_BUFSIZE`.
const BUFSIZE: usize = 8192;
/// Characters of a `name` attribute replaced by `_`.
const UNSAFE: &[u8] = b".\\+*?[^]$() ";
/// `PHP_META_HTML401_CHARS`.
const HTML401: &[u8] = b"-_.:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    Eof,
    OpenTag,
    CloseTag,
    Slash,
    Equal,
    Space,
    Id,
    Str,
    Other,
}

/// `php_meta_tags_data`.
struct Tokenizer<'a> {
    stream: &'a mut Stream,
    ulc: bool,
    lc: i32,
    in_meta: bool,
    token: Option<Vec<u8>>,
}

/// `php_stream_getc` as a C `int` (`-1` at EOF).
fn getc(s: &mut Stream) -> i32 {
    s.getc().map_or(-1, i32::from)
}

fn byte(ch: i32) -> u8 {
    u8::try_from(ch & 0xff).unwrap_or(0)
}

impl Tokenizer<'_> {
    fn next(&mut self) -> Tok {
        let mut ch: i32 = 0;
        loop {
            if !self.ulc {
                if self.stream.eof() {
                    break;
                }
                ch = getc(self.stream);
                if ch == 0 {
                    break;
                }
            }
            if self.stream.eof() {
                break;
            }
            if self.ulc {
                ch = self.lc;
                self.ulc = false;
            }
            match byte(ch) {
                b'<' if ch >= 0 => return Tok::OpenTag,
                b'>' if ch >= 0 => return Tok::CloseTag,
                b'=' if ch >= 0 => return Tok::Equal,
                b'/' if ch >= 0 => return Tok::Slash,
                b'\'' | b'"' if ch >= 0 => {
                    let compliment = ch;
                    let mut buf = Vec::new();
                    loop {
                        if self.stream.eof() {
                            break;
                        }
                        ch = getc(self.stream);
                        if ch == 0
                            || ch == compliment
                            || ch == i32::from(b'<')
                            || ch == i32::from(b'>')
                        {
                            break;
                        }
                        buf.push(byte(ch));
                        if buf.len() == BUFSIZE {
                            break;
                        }
                    }
                    if ch == i32::from(b'<') || ch == i32::from(b'>') {
                        self.ulc = true;
                        self.lc = ch;
                    }
                    if self.in_meta {
                        self.token = Some(buf);
                    }
                    return Tok::Str;
                }
                b'\n' | b'\r' | b'\t' if ch >= 0 => {}
                b' ' if ch >= 0 => return Tok::Space,
                c if ch >= 0 && c.is_ascii_alphanumeric() => {
                    let mut buf = vec![c];
                    loop {
                        if self.stream.eof() {
                            break;
                        }
                        ch = getc(self.stream);
                        if ch <= 0 {
                            break;
                        }
                        let b = byte(ch);
                        if !(b.is_ascii_alphanumeric() || HTML401.contains(&b)) {
                            break;
                        }
                        buf.push(b);
                        if buf.len() == BUFSIZE {
                            break;
                        }
                    }
                    if !(ch >= 0 && byte(ch).is_ascii_alphabetic()) && ch != i32::from(b'-') {
                        self.ulc = true;
                        self.lc = ch;
                    }
                    self.token = Some(buf);
                    return Tok::Id;
                }
                _ => return Tok::Other,
            }
        }
        Tok::Eof
    }
}

fn sanitize(mut name: Vec<u8>) -> Vec<u8> {
    if let Some(p) = name.iter().position(|&c| c == 0) {
        name.truncate(p);
    }
    for c in &mut name {
        if UNSAFE.contains(c) {
            *c = b'_';
        }
    }
    name
}

fn c_str(mut v: Vec<u8>) -> Vec<u8> {
    if let Some(p) = v.iter().position(|&c| c == 0) {
        v.truncate(p);
    }
    v
}

/// Parses the `<meta>` tags of `stream` (the body of `get_meta_tags()`).
#[allow(clippy::too_many_lines)]
pub fn meta_tags(stream: &mut Stream) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut md = Tokenizer {
        stream,
        ulc: false,
        lc: 0,
        in_meta: false,
        token: None,
    };
    let mut out: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let (mut in_tag, mut done, mut looking_for_val) = (false, false, false);
    let (mut have_name, mut have_content, mut saw_name, mut saw_content) =
        (false, false, false, false);
    let mut name: Option<Vec<u8>> = None;
    let mut value: Option<Vec<u8>> = None;
    let mut tok_last = Tok::Eof;
    while !done {
        let tok = md.next();
        if tok == Tok::Eof {
            break;
        }
        let data = md.token.take();
        match tok {
            Tok::Id => {
                let d = data.unwrap_or_default();
                if tok_last == Tok::OpenTag {
                    md.in_meta = d.eq_ignore_ascii_case(b"meta");
                } else if tok_last == Tok::Slash && in_tag {
                    if d.eq_ignore_ascii_case(b"head") {
                        done = true;
                    }
                } else if tok_last == Tok::Equal && looking_for_val {
                    if saw_name {
                        name = Some(sanitize(d));
                        have_name = true;
                    } else if saw_content {
                        value = Some(c_str(d));
                        have_content = true;
                    }
                    looking_for_val = false;
                } else if md.in_meta {
                    if d.eq_ignore_ascii_case(b"name") {
                        saw_name = true;
                        saw_content = false;
                        looking_for_val = true;
                    } else if d.eq_ignore_ascii_case(b"content") {
                        saw_name = false;
                        saw_content = true;
                        looking_for_val = true;
                    }
                }
            }
            Tok::Str if tok_last == Tok::Equal && looking_for_val => {
                let d = data.unwrap_or_default();
                if saw_name {
                    name = Some(sanitize(d));
                    have_name = true;
                } else if saw_content {
                    value = Some(c_str(d));
                    have_content = true;
                }
                looking_for_val = false;
            }
            Tok::OpenTag => {
                if looking_for_val {
                    looking_for_val = false;
                    have_name = false;
                    saw_name = false;
                    have_content = false;
                    saw_content = false;
                }
                in_tag = true;
            }
            Tok::CloseTag => {
                if have_name {
                    let n = name.take().unwrap_or_default().to_ascii_lowercase();
                    let v = if have_content {
                        value.take().unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    if let Some(slot) = out.iter_mut().find(|(k, _)| *k == n) {
                        slot.1 = v;
                    } else {
                        out.push((n, v));
                    }
                }
                name = None;
                value = None;
                in_tag = false;
                looking_for_val = false;
                have_name = false;
                saw_name = false;
                have_content = false;
                saw_content = false;
                md.in_meta = false;
            }
            _ => {}
        }
        tok_last = tok;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use php_streams::memory::memory_open;
    use php_streams::mode::TempMode;

    #[test]
    fn parses_meta() {
        let html = b"<html><head><meta name=\"author\" content=\"name\">\n<meta name=\"geo.position\" content=\"49.33;-86.59\">\n</head>";
        let mut s = memory_open(TempMode::ReadOnly, html.to_vec());
        let tags = meta_tags(&mut s);
        assert_eq!(tags[0], (b"author".to_vec(), b"name".to_vec()));
        assert_eq!(tags[1].0, b"geo_position".to_vec());
    }
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `htmlentities()` subset used by `FILTER_SANITIZE_FULL_SPECIAL_CHARS`:
//! `php_escape_html_entities_ex(s, all = 1, ENT_QUOTES | ENT_NOQUOTES,
//! charset = default (UTF-8), double_encode = 0, quiet = 0)` with the
//! HTML 4.01 entity table (`ext/standard/html.c`).

use crate::html_table::{BY_CODEPOINT, NAMES};

fn entity_for(cp: u32) -> Option<&'static str> {
    BY_CODEPOINT
        .binary_search_by_key(&cp, |&(c, _)| c)
        .ok()
        .map(|i| BY_CODEPOINT[i].1)
}

/// `process_numeric_entity`: `s` starts after `&#`; returns the length of
/// the entity body (up to, not including, the `;`) when valid.
fn numeric_entity_len(s: &[u8]) -> Option<usize> {
    let at = |i: usize| s.get(i).copied().unwrap_or(0);
    let hex = matches!(at(0), b'x' | b'X');
    let start = usize::from(hex);
    let first = at(start);
    if (hex && !first.is_ascii_hexdigit()) || (!hex && !first.is_ascii_digit()) {
        return None;
    }
    let radix: u32 = if hex { 16 } else { 10 };
    let mut i = start;
    let mut value: u64 = 0;
    while let Some(d) = char::from(at(i)).to_digit(radix) {
        // strtol saturates at LONG_MAX, which is > 0x10FFFF anyway.
        value = value
            .saturating_mul(u64::from(radix))
            .saturating_add(u64::from(d));
        i += 1;
    }
    (at(i) == b';' && value <= 0x10_FFFF).then_some(i)
}

/// `process_named_entity_html` + `resolve_named_entity_html` against the
/// HTML 4.01 inverse map: `s` starts after `&`.
fn named_entity_len(s: &[u8]) -> Option<usize> {
    let len = s.iter().take_while(|b| b.is_ascii_alphanumeric()).count();
    if len == 0 || s.get(len) != Some(&b';') {
        return None;
    }
    NAMES
        .binary_search_by(|n| n.as_bytes().cmp(&s[..len]))
        .ok()
        .map(|_| len)
}

/// `htmlentities($s, ($quotes ? ENT_QUOTES : ENT_NOQUOTES) | ENT_HTML401,
/// 'UTF-8', double_encode: false)`. Invalid UTF-8 gives the empty string.
#[must_use]
pub fn htmlentities_utf8_no_double_encode(s: &[u8], quotes: bool) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(s) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(s.len() + s.len() / 2);
    let mut chars = text.char_indices();
    while let Some((pos, ch)) = chars.next() {
        let cp = u32::from(ch);
        if ch == '&' {
            let rest = &s[pos + 1..];
            let body = if rest.first() == Some(&b'#') {
                numeric_entity_len(&rest[1..]).map(|n| n + 1)
            } else {
                named_entity_len(rest)
            };
            if let Some(n) = body {
                out.push(b'&');
                out.extend_from_slice(&rest[..=n]);
                // Skip the entity body and its ';' (all ASCII).
                for _ in 0..=n {
                    chars.next();
                }
            } else {
                out.extend_from_slice(b"&amp;");
            }
            continue;
        }
        if !quotes && (ch == '\'' || ch == '"') {
            out.push(ch as u8);
            continue;
        }
        if let Some(ent) = entity_for(cp) {
            out.push(b'&');
            out.extend_from_slice(ent.as_bytes());
            out.push(b';');
        } else {
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: &[u8]) -> String {
        String::from_utf8(htmlentities_utf8_no_double_encode(s, true)).unwrap()
    }

    #[test]
    fn basics() {
        assert_eq!(
            h(b"<a href='x'>\"&"),
            "&lt;a href=&#039;x&#039;&gt;&quot;&amp;"
        );
        assert_eq!(h("é ü €".as_bytes()), "&eacute; &uuml; &euro;");
        assert_eq!(
            h(b"&amp; &foo; &#65; &#x41; &#xZ; &apos; &"),
            "&amp; &amp;foo; &#65; &#x41; &amp;#xZ; &amp;apos; &amp;"
        );
        assert_eq!(h(b"&#1114112;"), "&amp;#1114112;");
        assert_eq!(h(b"\xff"), "");
        assert_eq!(
            String::from_utf8(htmlentities_utf8_no_double_encode(b"'\"<", false)).unwrap(),
            "'\"&lt;"
        );
        assert_eq!(h("ɑ".as_bytes()), "ɑ");
    }
}

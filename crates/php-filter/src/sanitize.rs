// Derived from php-src ext/filter/sanitizing_filters.c. Similarity metric to original: 33%
//   Copyright © The PHP Group and Contributors. Authors: Derick Rethans <derick@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! The sanitizing filters of `sanitizing_filters.c`.

use crate::constants::{
    FILTER_FLAG_ALLOW_FRACTION, FILTER_FLAG_ALLOW_SCIENTIFIC, FILTER_FLAG_ALLOW_THOUSAND,
    FILTER_FLAG_EMPTY_STRING_NULL, FILTER_FLAG_ENCODE_AMP, FILTER_FLAG_ENCODE_HIGH,
    FILTER_FLAG_ENCODE_LOW, FILTER_FLAG_NO_ENCODE_QUOTES, FILTER_FLAG_STRIP_BACKTICK,
    FILTER_FLAG_STRIP_HIGH, FILTER_FLAG_STRIP_LOW,
};
use php_runtime::{Value, ZStr};

const LOWALPHA: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
const HIALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const DIGIT: &[u8] = b"0123456789";

const fn table(lists: &[&[u8]]) -> [bool; 256] {
    let mut t = [false; 256];
    let mut i = 0;
    while i < lists.len() {
        let mut j = 0;
        while j < lists[i].len() {
            t[lists[i][j] as usize] = true;
            j += 1;
        }
        i += 1;
    }
    t
}

/// The bytes `FILTER_SANITIZE_URL` keeps (RFC 1738 section 5).
pub(crate) const URL_ALLOWED: [bool; 256] = table(&[
    LOWALPHA,
    HIALPHA,
    DIGIT,
    b"$-_.+",      // SAFE
    b"!*'(),",     // EXTRA
    b"{}|\\^~[]`", // NATIONAL
    b"<>#%\"",     // PUNCTUATION
    b";/?:@&=",    // RESERVED
]);

/// The bytes `FILTER_SANITIZE_EMAIL` keeps (RFC 822 section 6).
const EMAIL_ALLOWED: [bool; 256] = table(&[LOWALPHA, HIALPHA, DIGIT, b"!#$%&'*+-=?^_`{|}~@.[]"]);

/// The bytes `FILTER_SANITIZE_ENCODED` leaves unencoded.
const URL_UNRESERVED: [bool; 256] = table(&[LOWALPHA, HIALPHA, DIGIT, b"-._"]);

/// `php_filter_strip`: removes bytes per `FILTER_FLAG_STRIP_LOW` (`< 32`),
/// `STRIP_HIGH` (`>= 127`) and `STRIP_BACKTICK`.
#[must_use]
pub fn strip(s: &[u8], flags: i64) -> Vec<u8> {
    let low = flags & FILTER_FLAG_STRIP_LOW != 0;
    let high = flags & FILTER_FLAG_STRIP_HIGH != 0;
    let backtick = flags & FILTER_FLAG_STRIP_BACKTICK != 0;
    s.iter()
        .copied()
        .filter(|&c| !((high && c >= 127) || (low && c < 32) || (backtick && c == b'`')))
        .collect()
}

/// `php_filter_encode_html`: replaces every byte marked in `chars` by its
/// decimal numeric entity `&#N;`.
#[must_use]
pub fn encode_html(s: &[u8], chars: &[bool; 256]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for &c in s {
        if chars[usize::from(c)] {
            out.extend_from_slice(format!("&#{c};").as_bytes());
        } else {
            out.push(c);
        }
    }
    out
}

fn encode_table(flags: i64, quotes: bool, amp: bool) -> [bool; 256] {
    let mut enc = [false; 256];
    if quotes {
        enc[usize::from(b'\'')] = true;
        enc[usize::from(b'"')] = true;
    }
    if amp {
        enc[usize::from(b'&')] = true;
    }
    if flags & FILTER_FLAG_ENCODE_LOW != 0 {
        enc[..32].fill(true);
    }
    if flags & FILTER_FLAG_ENCODE_HIGH != 0 {
        enc[127..].fill(true);
    }
    enc
}

/// `php_filter_string`: `FILTER_SANITIZE_STRING` (deprecated): strips,
/// encodes quotes (unless `NO_ENCODE_QUOTES`) and then strips tags.
#[must_use]
pub fn filter_string(s: &[u8], flags: i64) -> Value {
    let stripped = strip(s, flags);
    let enc = encode_table(
        flags,
        flags & FILTER_FLAG_NO_ENCODE_QUOTES == 0,
        flags & FILTER_FLAG_ENCODE_AMP != 0,
    );
    let mut encoded = if stripped.is_empty() {
        stripped
    } else {
        encode_html(&stripped, &enc)
    };
    let new_len = strip_tags(&mut encoded);
    encoded.truncate(new_len);
    if encoded.is_empty() && flags & FILTER_FLAG_EMPTY_STRING_NULL != 0 {
        Value::Null
    } else {
        Value::Str(ZStr::new(encoded))
    }
}

/// `php_filter_encoded`: `FILTER_SANITIZE_ENCODED` (strip, then
/// percent-encode everything but `[A-Za-z0-9._-]`).
#[must_use]
pub fn filter_encoded(s: &[u8], flags: i64) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let stripped = strip(s, flags);
    let mut out = Vec::with_capacity(stripped.len() * 3);
    for c in stripped {
        if URL_UNRESERVED[usize::from(c)] {
            out.push(c);
        } else {
            out.extend_from_slice(&[b'%', HEX[usize::from(c >> 4)], HEX[usize::from(c & 15)]]);
        }
    }
    out
}

/// `php_filter_special_chars`: `FILTER_SANITIZE_SPECIAL_CHARS` (strip, then
/// encode `'"<>&`, all bytes below 32 and, with `ENCODE_HIGH`, bytes >= 127).
#[must_use]
pub fn filter_special_chars(s: &[u8], flags: i64) -> Vec<u8> {
    let stripped = strip(s, flags);
    let mut enc = [false; 256];
    for c in [b'\'', b'"', b'<', b'>', b'&'] {
        enc[usize::from(c)] = true;
    }
    enc[..32].fill(true);
    if flags & FILTER_FLAG_ENCODE_HIGH != 0 {
        enc[127..].fill(true);
    }
    encode_html(&stripped, &enc)
}

/// `php_filter_unsafe_raw`: `FILTER_UNSAFE_RAW` / `FILTER_DEFAULT`.
#[must_use]
pub fn filter_unsafe_raw(s: &[u8], flags: i64) -> Value {
    if flags != 0 && !s.is_empty() {
        let stripped = strip(s, flags);
        let enc = encode_table(flags, false, flags & FILTER_FLAG_ENCODE_AMP != 0);
        Value::Str(ZStr::new(encode_html(&stripped, &enc)))
    } else if flags & FILTER_FLAG_EMPTY_STRING_NULL != 0 && s.is_empty() {
        Value::Null
    } else {
        Value::from(s)
    }
}

fn keep(s: &[u8], allowed: &[bool; 256]) -> Vec<u8> {
    s.iter()
        .copied()
        .filter(|&c| allowed[usize::from(c)])
        .collect()
}

/// `php_filter_email`: `FILTER_SANITIZE_EMAIL`.
#[must_use]
pub fn filter_email(s: &[u8]) -> Vec<u8> {
    keep(s, &EMAIL_ALLOWED)
}

/// `php_filter_url`: `FILTER_SANITIZE_URL`.
#[must_use]
pub fn filter_url(s: &[u8]) -> Vec<u8> {
    keep(s, &URL_ALLOWED)
}

/// `php_filter_number_int`: `FILTER_SANITIZE_NUMBER_INT` (keeps `[0-9+-]`).
#[must_use]
pub fn filter_number_int(s: &[u8]) -> Vec<u8> {
    s.iter()
        .copied()
        .filter(|&c| c.is_ascii_digit() || c == b'+' || c == b'-')
        .collect()
}

/// `php_filter_number_float`: `FILTER_SANITIZE_NUMBER_FLOAT` (keeps
/// `[0-9+-]` plus `.`, `,` and `eE` per `ALLOW_FRACTION`/`THOUSAND`/
/// `SCIENTIFIC`).
#[must_use]
pub fn filter_number_float(s: &[u8], flags: i64) -> Vec<u8> {
    let fraction = flags & FILTER_FLAG_ALLOW_FRACTION != 0;
    let thousand = flags & FILTER_FLAG_ALLOW_THOUSAND != 0;
    let scientific = flags & FILTER_FLAG_ALLOW_SCIENTIFIC != 0;
    s.iter()
        .copied()
        .filter(|&c| {
            c.is_ascii_digit()
                || c == b'+'
                || c == b'-'
                || (fraction && c == b'.')
                || (thousand && c == b',')
                || (scientific && (c == b'e' || c == b'E'))
        })
        .collect()
}

/// `php_addslashes`: `FILTER_SANITIZE_ADD_SLASHES`.
#[must_use]
pub fn add_slashes(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for &c in s {
        match c {
            0 => out.extend_from_slice(b"\\0"),
            b'\'' | b'"' | b'\\' => {
                out.push(b'\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// `php_strip_tags_ex(buf, len, NULL, 0, allow_tag_spaces = 1)`: strips
/// HTML and PHP tags (and NUL bytes) in place, returning the new length.
#[allow(clippy::too_many_lines)]
pub fn strip_tags(rbuf: &mut [u8]) -> usize {
    let buf = rbuf.to_vec();
    let end = buf.len();
    let prev = |p: usize, k: usize| if p >= k { buf[p - k] } else { 0 };
    let mut rp = 0usize;
    let mut p = 0usize;
    let mut lc: u8 = 0;
    let mut br: i64 = 0;
    let mut depth: i64 = 0;
    let mut in_q: u8 = 0;
    let mut state = 0u8;
    let mut is_xml = false;

    while p < end {
        let c = buf[p];
        match state {
            0 => match c {
                0 => {}
                b'<' => {
                    if in_q == 0 {
                        lc = b'<';
                        state = 1;
                    }
                }
                b'>' => {
                    if depth > 0 {
                        depth -= 1;
                    } else if in_q == 0 {
                        rbuf[rp] = c;
                        rp += 1;
                    }
                }
                _ => {
                    rbuf[rp] = c;
                    rp += 1;
                }
            },
            1 => match c {
                b'<'
                    if in_q == 0 => {
                        depth += 1;
                    }
                b'>' => {
                    if depth > 0 {
                        depth -= 1;
                    } else if in_q == 0 {
                        lc = b'>';
                        if !(is_xml && prev(p, 1) == b'-') {
                            in_q = 0;
                            state = 0;
                            is_xml = false;
                        }
                    }
                }
                b'"' | b'\''
                    if p != 0 && (in_q == 0 || c == in_q) => {
                        in_q = if in_q != 0 { 0 } else { c };
                    }
                b'!'
                    // JavaScript & other HTML scripting languages.
                    if p >= 1 && prev(p, 1) == b'<' => {
                        state = 3;
                        lc = c;
                    }
                b'?'
                    if p >= 1 && prev(p, 1) == b'<' => {
                        br = 0;
                        state = 2;
                    }
                _ => {}
            },
            2 => match c {
                b'('
                    if lc != b'"' && lc != b'\'' => {
                        lc = b'(';
                        br += 1;
                    }
                b')'
                    if lc != b'"' && lc != b'\'' => {
                        lc = b')';
                        br -= 1;
                    }
                b'>' => {
                    if depth > 0 {
                        depth -= 1;
                    } else if in_q == 0 && br == 0 && p >= 1 && lc != b'"' && prev(p, 1) == b'?' {
                        in_q = 0;
                        state = 0;
                    }
                }
                b'"' | b'\''
                    if p >= 1 && prev(p, 1) != b'\\' => {
                        if lc == c {
                            lc = 0;
                        } else if lc != b'\\' {
                            lc = c;
                        }
                        if p != 0 && (in_q == 0 || c == in_q) {
                            in_q = if in_q != 0 { 0 } else { c };
                        }
                    }
                b'l' | b'L'
                    // "<?xml": switch back to HTML.
                    if p > 4
                        && matches!(prev(p, 1), b'm' | b'M')
                        && matches!(prev(p, 2), b'x' | b'X')
                        && prev(p, 3) == b'?'
                        && prev(p, 4) == b'<'
                    => {
                        state = 1;
                        is_xml = true;
                    }
                _ => {}
            },
            3 => match c {
                b'>' => {
                    if depth > 0 {
                        depth -= 1;
                    } else if in_q == 0 {
                        in_q = 0;
                        state = 0;
                    }
                }
                b'"' | b'\''
                    if p != 0 && prev(p, 1) != b'\\' && (in_q == 0 || c == in_q) => {
                        in_q = if in_q != 0 { 0 } else { c };
                    }
                b'-'
                    if p >= 2 && prev(p, 1) == b'-' && prev(p, 2) == b'!' => {
                        state = 4;
                    }
                b'E' | b'e'
                    // !DOCTYPE exception.
                    if p > 6
                        && matches!(prev(p, 1), b'p' | b'P')
                        && matches!(prev(p, 2), b'y' | b'Y')
                        && matches!(prev(p, 3), b't' | b'T')
                        && matches!(prev(p, 4), b'c' | b'C')
                        && matches!(prev(p, 5), b'o' | b'O')
                        && matches!(prev(p, 6), b'd' | b'D')
                    => {
                        state = 1;
                    }
                _ => {}
            },
            _ => {
                // State 4: inside a comment.
                if c == b'>' && in_q == 0 && p >= 2 && prev(p, 1) == b'-' && prev(p, 2) == b'-' {
                    in_q = 0;
                    state = 0;
                }
            }
        }
        p += 1;
    }
    rp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(s: &str) -> String {
        let mut b = s.as_bytes().to_vec();
        let n = strip_tags(&mut b);
        String::from_utf8_lossy(&b[..n]).into_owned()
    }

    #[test]
    fn strip_tags_cases() {
        assert_eq!(st("<p>Hello</p> world"), "Hello world");
        assert_eq!(st("a <!-- c > d --> b"), "a  b");
        assert_eq!(st("x<?php echo 1; ?>y"), "xy");
        assert_eq!(st("1 > 0"), "1 > 0");
        assert_eq!(st("<a title=\"x>y\">t</a>"), "t");
        assert_eq!(st("a\0b"), "ab");
        assert_eq!(st("<!DOCTYPE html>x"), "x");
    }

    #[test]
    fn sanitizers() {
        assert_eq!(filter_encoded(b"a b/\xff", 0), b"a%20b%2F%FF");
        assert_eq!(
            filter_special_chars(b"<a>&\x01\x80", 0),
            b"&#60;a&#62;&#38;&#1;\x80"
        );
        assert_eq!(
            filter_number_float(b"1,234.5e3x", FILTER_FLAG_ALLOW_FRACTION),
            b"1234.53"
        );
        assert_eq!(add_slashes(b"a'b\"c\\\0"), b"a\\'b\\\"c\\\\\\0");
        assert!(matches!(
            filter_unsafe_raw(b"", FILTER_FLAG_EMPTY_STRING_NULL),
            Value::Null
        ));
        assert!(
            matches!(filter_string(b"<b>x</b>'", 0), Value::Str(s) if s.as_bytes() == b"x&#39;")
        );
        assert_eq!(filter_email(b"a(b)@c.d"), b"ab@c.d");
        assert_eq!(filter_url(b"http://a b"), b"http://ab");
        assert_eq!(filter_number_int(b"-1.5e3"), b"-153");
    }
}

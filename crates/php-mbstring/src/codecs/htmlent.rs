// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `HTML-ENTITIES`: ASCII text with everything else written as HTML
//! character references (`&name;` or `&#NNN;`).
//!
//! Ref: `libmbfl/filters/mbfilter_htmlent.c`, `libmbfl/filters/html_entities.c`.

use crate::convert::ConvertBuf;
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};
use crate::legacy::{self, Filter, Sink, Vtbl};
use crate::tables::html_entities::HTML_ENTITY_LIST;

/// `HTML-ENTITIES`.
pub static ENC_HTML_ENT: Encoding = Encoding {
    no: EncodingNo::HtmlEnt,
    name: "HTML-ENTITIES",
    mime_name: Some("HTML-ENTITIES"),
    aliases: &["HTML", "html"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: Some(&VTBL_HTML_WCHAR),
    output_filter: Some(&VTBL_WCHAR_HTML),
    to_wchar: htmlent_to_wchar,
    from_wchar: wchar_to_htmlent,
    check: None,
    cut: None,
};

/// `vtbl_wchar_html`.
pub static VTBL_WCHAR_HTML: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_html_enc,
    flush: filt_html_enc_flush,
};

/// `vtbl_html_wchar`. Its pending entity text lives in `Filter::opaque`
/// (cloning the filter copies it, like `mbfl_filt_conv_html_dec_copy`).
pub static VTBL_HTML_WCHAR: Vtbl = Vtbl {
    ctor: filt_html_dec_ctor,
    function: filt_html_dec,
    flush: filt_html_dec_flush,
};

/// Whether a code point is output as itself rather than as an entity
/// (`htmlentitifieds[c] != 1`: everything below 0x80, including `&<>"`).
fn is_direct(w: u32) -> bool {
    w < 0x80
}

/// The entity name for code point `w`, if it has one (first match wins).
fn entity_name(w: u32) -> Option<&'static str> {
    HTML_ENTITY_LIST
        .iter()
        .find(|&&(_, code)| code == w)
        .map(|&(name, _)| name)
}

/// The code point of a named entity, if known (first match wins).
fn entity_code(name: &[u8]) -> Option<u32> {
    HTML_ENTITY_LIST
        .iter()
        .find(|&&(n, _)| n.as_bytes() == name)
        .map(|&(_, code)| code)
}

/// `mbfl_filt_conv_html_enc`.
#[allow(clippy::cast_sign_loss)]
pub fn filt_html_enc(c: i32, _filter: &mut Filter, out: &mut dyn Sink) {
    let w = c as u32;
    if is_direct(w) {
        out.put(c);
        return;
    }
    out.put(i32::from(b'&'));
    if let Some(name) = entity_name(w) {
        for &b in name.as_bytes() {
            out.put(i32::from(b));
        }
    } else {
        out.put(i32::from(b'#'));
        for b in w.to_string().bytes() {
            out.put(i32::from(b));
        }
    }
    out.put(i32::from(b';'));
}

/// `mbfl_filt_conv_html_enc_flush`.
pub fn filt_html_enc_flush(filter: &mut Filter, out: &mut dyn Sink) {
    filter.status = 0;
    filter.opaque.clear();
    out.flush();
}

/// `html_enc_buffer_size`: maximum length of a pending entity.
const HTML_ENC_BUFFER_SIZE: usize = 16;

/// `html_entity_chars`, tested like C's `strchr` (which also "finds" the
/// terminating NUL, and compares only the low byte of `c`).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn is_html_entity_char_legacy(c: i32) -> bool {
    let b = c as u8;
    b == 0 || b == b'#' || b.is_ascii_alphanumeric()
}

/// `mbfl_filt_conv_html_dec_ctor`.
pub fn filt_html_dec_ctor(filter: &mut Filter) {
    filter.status = 0;
    filter.opaque = vec![0; HTML_ENC_BUFFER_SIZE + 1];
}

fn flush_pending(filter: &mut Filter, n: usize, out: &mut dyn Sink) {
    for &b in &filter.opaque[..n] {
        out.put(i32::from(b));
    }
    filter.status = 0;
}

/// `mbfl_filt_conv_html_dec`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
pub fn filt_html_dec(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    if filter.opaque.len() <= HTML_ENC_BUFFER_SIZE {
        filter.opaque.resize(HTML_ENC_BUFFER_SIZE + 1, 0);
    }
    if filter.status == 0 {
        if c == i32::from(b'&') {
            filter.status = 1;
            filter.opaque[0] = b'&';
        } else {
            out.put(c);
        }
        return;
    }

    let status = filter.status as usize;
    if c == i32::from(b';') {
        let buffer = &filter.opaque;
        if buffer[1] == b'#' {
            // Numeric entity; `u32::MAX` stands for C's `ent = -1`.
            let mut ent: u32 = 0;
            if status > 2 && (buffer[2] == b'x' || buffer[2] == b'X') {
                if status > 3 {
                    for &v in &buffer[3..status] {
                        let Some(d) = char::from(v).to_digit(16) else {
                            ent = u32::MAX;
                            break;
                        };
                        ent = ent.wrapping_mul(16).wrapping_add(d);
                    }
                } else {
                    ent = u32::MAX;
                }
            } else if status > 2 {
                for &v in &buffer[2..status] {
                    if ent > 0x1999_9999 {
                        ent = u32::MAX;
                        break;
                    }
                    let Some(d) = char::from(v).to_digit(10) else {
                        ent = u32::MAX;
                        break;
                    };
                    ent = ent.wrapping_mul(10).wrapping_add(d);
                }
            } else {
                ent = u32::MAX;
            }
            if ent < 0x11_0000 {
                out.put(ent as i32);
                filter.status = 0;
            } else {
                flush_pending(filter, status, out);
                out.put(c);
            }
        } else {
            // Named entity; the name ends at the first NUL (C string).
            filter.opaque[status] = 0;
            let name = &filter.opaque[1..status];
            let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())];
            if let Some(ent) = entity_code(name) {
                out.put(ent as i32);
                filter.status = 0;
            } else {
                filter.opaque[status] = b';';
                filter.opaque[status + 1] = 0;
                flush_pending(filter, status + 1, out);
            }
        }
    } else {
        filter.opaque[status] = c as u8;
        let mut status = status + 1;
        filter.status += 1;
        if !is_html_entity_char_legacy(c)
            || status + 1 == HTML_ENC_BUFFER_SIZE
            || (c == i32::from(b'#') && status > 2)
        {
            // Illegal character or end of buffer.
            if c == i32::from(b'&') {
                status -= 1;
            }
            filter.opaque[status] = 0;
            flush_pending(filter, status, out);
            if c == i32::from(b'&') {
                filter.opaque[0] = b'&';
                filter.status = 1;
            }
        }
    }
}

/// `mbfl_filt_conv_html_dec_flush`: emits any pending entity text as is.
#[allow(clippy::cast_sign_loss)]
pub fn filt_html_dec_flush(filter: &mut Filter, out: &mut dyn Sink) {
    let status = (filter.status.max(0) as usize).min(filter.opaque.len());
    flush_pending(filter, status, out);
    out.flush();
}

fn is_html_entity_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'#'
}

/// Parse the digits of a numeric entity (`&#...;` / `&#x...;`) with C's
/// wrapping `unsigned int` arithmetic; `None` for a bad digit or no digits.
fn numeric_entity(digits: &[u8]) -> Option<u32> {
    let (digits, radix) = match digits.first() {
        Some(b'x' | b'X') => (&digits[1..], 16),
        _ => (digits, 10),
    };
    if digits.is_empty() {
        return None;
    }
    digits.iter().try_fold(0u32, |value, &d| {
        char::from(d)
            .to_digit(radix)
            .map(|d| value.wrapping_mul(radix).wrapping_add(d))
    })
}

/// `mb_htmlent_to_wchar`.
pub fn htmlent_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let s = *input;
    let e = s.len();
    let limit = buf.len();
    let (mut p, mut out) = (0usize, 0usize);

    while p < e && out < limit {
        let c = s[p];
        p += 1;
        if c != b'&' {
            buf[out] = u32::from(c);
            out += 1;
            continue;
        }

        // Find the terminating ';' of the entity.
        let mut terminator = p;
        while terminator < e && is_html_entity_char(s[terminator]) {
            terminator += 1;
        }
        if terminator < e && s[terminator] == b';' {
            if s[p] == b'#' && e - p >= 2 {
                if let Some(value) = numeric_entity(&s[p + 1..terminator]) {
                    if value <= 0x10_FFFF {
                        buf[out] = value;
                        out += 1;
                        p = terminator + 1;
                        continue;
                    }
                }
            } else if terminator > p {
                if let Some(code) = entity_code(&s[p..terminator]) {
                    buf[out] = code;
                    out += 1;
                    p = terminator + 1;
                    continue;
                }
            }
        }

        // No ';' found, or the entity was not recognized.
        buf[out] = u32::from(b'&');
        out += 1;
        while p < terminator && out < limit {
            buf[out] = u32::from(s[p]);
            out += 1;
            p += 1;
        }
        if terminator < e && s[terminator] == b';' && out < limit {
            buf[out] = u32::from(s[p]);
            out += 1;
            p += 1;
        }
    }

    *input = &s[p..];
    out
}

/// `mb_wchar_to_htmlent`.
#[allow(clippy::cast_possible_truncation)]
pub fn wchar_to_htmlent(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        if is_direct(w) {
            buf.push(w as u8);
            continue;
        }
        buf.push(b'&');
        if let Some(name) = entity_name(w) {
            buf.extend(name.as_bytes());
        } else {
            buf.push(b'#');
            buf.extend(w.to_string().as_bytes());
        }
        buf.push(b';');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::{decode_all, encode_all, ErrorMode, BAD_INPUT};
    use crate::legacy::ByteSink;

    /// Collects the raw `i32` values a filter emits.
    #[derive(Default)]
    struct IntSink(Vec<i32>);
    impl Sink for IntSink {
        fn put(&mut self, c: i32) {
            self.0.push(c);
        }
    }

    fn legacy_decode(input: &[u8]) -> Vec<i32> {
        let mut f = Filter::new(&VTBL_HTML_WCHAR);
        let mut sink = IntSink::default();
        for &b in input {
            f.feed(i32::from(b), &mut sink);
        }
        f.flush(&mut sink);
        sink.0
    }

    fn legacy_encode(input: &[i32]) -> Vec<u8> {
        let mut f = Filter::new(&VTBL_WCHAR_HTML);
        let mut sink = ByteSink::default();
        for &c in input {
            f.feed(c, &mut sink);
        }
        f.flush(&mut sink);
        sink.0
    }

    fn chars(s: &str) -> Vec<i32> {
        s.bytes().map(i32::from).collect()
    }

    #[test]
    fn decode() {
        let w = decode_all(
            b"&lt;&#65;&#x42;&quot&amp;&bogus;&#x;&#;&#1114112;x",
            &ENC_HTML_ENT,
        );
        let mut want: Vec<u32> = vec![0x3C, 0x41, 0x42];
        want.extend("&quot".bytes().map(u32::from));
        want.push(0x26);
        want.extend("&bogus;&#x;&#;&#1114112;x".bytes().map(u32::from));
        assert_eq!(w, want);
        // Wrapping arithmetic, like C's unsigned int.
        assert_eq!(decode_all(b"&#x100000041;", &ENC_HTML_ENT), vec![0x41]);
        assert_eq!(decode_all(b"&#4294967361;", &ENC_HTML_ENT), vec![0x41]);
    }

    #[test]
    fn encode() {
        let out = encode_all(
            &[0x41, 0x26, 0xA0, 0x2665, 0x3042, BAD_INPUT],
            &ENC_HTML_ENT,
            0x3F,
            ErrorMode::Char,
        );
        assert_eq!(out, b"A&&nbsp;&hearts;&#12354;&#4294967295;");
    }

    #[test]
    fn legacy_encoder() {
        assert_eq!(
            legacy_encode(&[0x41, 0x26, 0xA0, 0x3042, -1]),
            b"A&&nbsp;&#12354;&#4294967295;"
        );
    }

    #[test]
    fn legacy_decoder() {
        assert_eq!(
            legacy_decode(b"a&lt;b&#65;&#x42;"),
            vec![0x61, 0x3C, 0x62, 0x41, 0x42]
        );
        assert_eq!(legacy_decode(b"&bogus;"), chars("&bogus;"));
        assert_eq!(legacy_decode(b"&amp"), chars("&amp"));
        assert_eq!(legacy_decode(b"&a&lt;"), [chars("&a"), vec![0x3C]].concat());
        assert_eq!(legacy_decode(b"&a b"), chars("&a b"));
        assert_eq!(legacy_decode(b"&#1114112;"), chars("&#1114112;"));
        assert_eq!(legacy_decode(b"&#9#;"), chars("&#9#;"));
        // The buffer holds at most 15 bytes.
        assert_eq!(
            legacy_decode(b"&abcdefghijklmnopq;"),
            chars("&abcdefghijklmnopq;")
        );
        // Cloning preserves the pending buffer.
        let mut f = Filter::new(&VTBL_HTML_WCHAR);
        let mut sink = IntSink::default();
        for &b in b"&am" {
            f.feed(i32::from(b), &mut sink);
        }
        let mut g = f.clone();
        for &b in b"p;" {
            g.feed(i32::from(b), &mut sink);
        }
        f.flush(&mut sink);
        assert_eq!(sink.0, [vec![0x26], chars("&am")].concat());
    }
}

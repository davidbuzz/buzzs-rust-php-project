//! `UTF-16`, `UTF-16BE`, `UTF-16LE`.
//!
//! Ports the portable (non-SIMD) paths of libmbfl; the AVX2 variants in C
//! produce identical results.
//!
//! Ref: `libmbfl/filters/mbfilter_utf16.c`.

use crate::convert::{illegal_output, ConvertBuf, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo};

/// `UTF-16`.
pub static ENC_UTF16: Encoding = Encoding {
    no: EncodingNo::Utf16,
    name: "UTF-16",
    mime_name: Some("UTF-16"),
    aliases: &["utf16"],
    mblen_table: None,
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: utf16_to_wchar,
    from_wchar: wchar_to_utf16be,
    check: None,
    cut: Some(cut_utf16),
};

/// `UTF-16BE`.
pub static ENC_UTF16BE: Encoding = Encoding {
    no: EncodingNo::Utf16be,
    name: "UTF-16BE",
    mime_name: Some("UTF-16BE"),
    aliases: &[],
    mblen_table: None,
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: utf16be_to_wchar,
    from_wchar: wchar_to_utf16be,
    check: None,
    cut: Some(cut_utf16be),
};

/// `UTF-16LE`.
pub static ENC_UTF16LE: Encoding = Encoding {
    no: EncodingNo::Utf16le,
    name: "UTF-16LE",
    mime_name: Some("UTF-16LE"),
    aliases: &[],
    mblen_table: None,
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: utf16le_to_wchar,
    from_wchar: wchar_to_utf16le,
    check: None,
    cut: Some(cut_utf16le),
};

const DETECTED_BE: u32 = 1;
const DETECTED_LE: u32 = 2;

/// `mb_utf16_to_wchar`: big-endian unless a little-endian BOM comes first; a
/// leading big-endian BOM is dropped.
pub fn utf16_to_wchar(input: &mut &[u8], buf: &mut [u32], state: &mut u32) -> usize {
    if *state == DETECTED_BE {
        return utf16be_to_wchar(input, buf, &mut 0);
    } else if *state == DETECTED_LE {
        return utf16le_to_wchar(input, buf, &mut 0);
    } else if input.len() >= 2 {
        let n = u16::from_be_bytes([input[0], input[1]]);
        if n == 0xFFFE {
            *input = &input[2..];
            *state = DETECTED_LE;
            return utf16le_to_wchar(input, buf, &mut 0);
        }
        if n == 0xFEFF {
            *input = &input[2..];
        }
    }
    *state = DETECTED_BE;
    utf16be_to_wchar(input, buf, &mut 0)
}

fn utf16_decode(input: &mut &[u8], buf: &mut [u32], be: bool) -> usize {
    let s = *input;
    let word = |p: usize| {
        let b = [s[p], s[p + 1]];
        u32::from(if be {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        })
    };
    // Only whole 16-bit words are read in the loop; a trailing byte is
    // handled at the end.
    let e = s.len() & !1;
    // One less than the real space: some iterations emit two code points.
    let limit = buf.len().saturating_sub(1);
    let (mut p, mut out) = (0usize, 0usize);

    while p < e && out < limit {
        let n = word(p);
        p += 2;
        if (0xD800..=0xDBFF).contains(&n) {
            if p < e {
                let n2 = word(p);
                p += 2;
                if (0xD800..=0xDBFF).contains(&n2) {
                    // A first half where the second half was expected.
                    buf[out] = BAD_INPUT;
                    out += 1;
                    p -= 2;
                } else if (0xDC00..=0xDFFF).contains(&n2) {
                    buf[out] = (((n & 0x3FF) << 10) | (n2 & 0x3FF)) + 0x1_0000;
                    out += 1;
                } else {
                    // The first half was followed by a 'normal' code point.
                    buf[out] = BAD_INPUT;
                    buf[out + 1] = n2;
                    out += 2;
                }
            } else {
                buf[out] = BAD_INPUT;
                out += 1;
            }
        } else if (0xDC00..=0xDFFF).contains(&n) {
            // Second half of a surrogate pair came first.
            buf[out] = BAD_INPUT;
            out += 1;
        } else {
            buf[out] = n;
            out += 1;
        }
    }

    if p == e && s.len() & 1 != 0 && out < limit {
        // An extra trailing byte.
        buf[out] = BAD_INPUT;
        out += 1;
        p += 1;
    }

    *input = &s[p..];
    out
}

/// `mb_utf16be_to_wchar`.
pub fn utf16be_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    utf16_decode(input, buf, true)
}

/// `mb_utf16le_to_wchar`.
pub fn utf16le_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    utf16_decode(input, buf, false)
}

#[allow(clippy::cast_possible_truncation)]
fn utf16_encode(input: &[u32], buf: &mut ConvertBuf, be: bool, this: crate::convert::FromWcharFn) {
    let put = |buf: &mut ConvertBuf, n: u16| {
        if be {
            buf.extend(&n.to_be_bytes());
        } else {
            buf.extend(&n.to_le_bytes());
        }
    };
    for &w in input {
        if w < 0x1_0000 {
            put(buf, w as u16);
        } else if w < 0x11_0000 {
            put(buf, (((w >> 10) - 0x40) | 0xD800) as u16);
            put(buf, ((w & 0x3FF) | 0xDC00) as u16);
        } else {
            illegal_output(w, this, buf);
        }
    }
}

/// `mb_wchar_to_utf16be`. (Like libmbfl, lone surrogate code points are
/// encoded as-is.)
pub fn wchar_to_utf16be(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    utf16_encode(input, buf, true, wchar_to_utf16be);
}

/// `mb_wchar_to_utf16le`.
pub fn wchar_to_utf16le(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    utf16_encode(input, buf, false, wchar_to_utf16le);
}

fn cut_utf16_impl(s: &[u8], from: usize, len: usize, be: bool) -> Vec<u8> {
    let word = |p: usize| {
        let b = [s[p], s[p + 1]];
        u32::from(if be {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        })
    };
    let mut len = len.min(s.len().saturating_sub(from));
    let from = from & !1;
    len &= !1;
    let start = from;
    if len < 2 || s.len().saturating_sub(start) < 2 {
        return Vec::new();
    }
    // libmbfl checks whether the first code unit is the second half of a
    // surrogate pair and then decrements `from`, but `start` has already
    // been computed, so that adjustment has no effect; the check is
    // therefore omitted here.
    let end = (start + len).min(s.len());
    let mut cut_end = end;
    if (0xD800..=0xDBFF).contains(&word(end - 2)) {
        cut_end -= 2;
    }
    s[start..cut_end].to_vec()
}

/// `mb_cut_utf16be`.
pub fn cut_utf16be(s: &[u8], from: usize, len: usize) -> Vec<u8> {
    cut_utf16_impl(s, from, len, true)
}

/// `mb_cut_utf16le`.
pub fn cut_utf16le(s: &[u8], from: usize, len: usize) -> Vec<u8> {
    cut_utf16_impl(s, from, len, false)
}

/// `mb_cut_utf16`: skips a leading BOM (which also selects the byte order).
pub fn cut_utf16(s: &[u8], from: usize, len: usize) -> Vec<u8> {
    if len < 2 || s.len() < 2 {
        return Vec::new();
    }
    let cp = u16::from_be_bytes([s[0], s[1]]);
    if cp == 0xFFFE {
        cut_utf16le(s, from.max(2), len)
    } else if cp == 0xFEFF {
        cut_utf16be(s, from.max(2), len)
    } else {
        cut_utf16be(s, from, len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::{decode_all, encode_all, ErrorMode};

    #[test]
    fn surrogates() {
        let w = decode_all(b"\xD8\x3D\xDE\x00", &ENC_UTF16BE);
        assert_eq!(w, vec![0x1F600]);
        let w = decode_all(b"\xD8\x00\xD8\x00\xDC\x00", &ENC_UTF16BE);
        assert_eq!(w, vec![BAD_INPUT, 0x10000]);
        let w = decode_all(b"\xD8\x00\x00A\xDC\x00", &ENC_UTF16BE);
        assert_eq!(w, vec![BAD_INPUT, 0x41, BAD_INPUT]);
        let w = decode_all(b"\x00\xD8\x01", &ENC_UTF16LE);
        assert_eq!(w, vec![BAD_INPUT, BAD_INPUT]);
        let w = decode_all(b"\xFF\xFE\x3D\xD8\x00\xDE", &ENC_UTF16);
        assert_eq!(w, vec![0x1F600]);
    }

    #[test]
    fn encode() {
        let out = encode_all(
            &[0x1F600, 0xD800, 0x11_0000],
            &ENC_UTF16LE,
            0x3F,
            ErrorMode::Char,
        );
        assert_eq!(out, b"\x3D\xD8\x00\xDE\x00\xD8?\x00");
    }

    #[test]
    fn cut() {
        let s = b"\x00A\xD8\x3D\xDE\x00\x00B";
        assert_eq!(cut_utf16be(s, 0, 4), b"\x00A");
        assert_eq!(cut_utf16be(s, 1, 6), b"\x00A\xD8\x3D\xDE\x00");
        // master: the start is never moved back onto a high surrogate
        // (PHP 8.3's legacy implementation returned `d83dde00` here).
        assert_eq!(cut_utf16be(s, 4, 4), b"\xDE\x00\x00B");
        assert_eq!(cut_utf16(b"\xFF\xFEA\x00B\x00", 0, 4), b"A\x00B\x00");
        assert_eq!(cut_utf16(b"\xFE\xFF\x00A", 0, 1), b"");
        // mb_strcut.phpt
        assert_eq!(cut_utf16(b"\xFF", 0, 100), b"");
        assert_eq!(cut_utf16(b"\xFF\x01", 1, 100), b"");
        assert_eq!(cut_utf16(b"\xD9\xFF", 0, 100), b"");
        assert_eq!(cut_utf16(b"\xD9\x00\x12C", 0, 100), b"\xD9\x00\x12C");
        assert_eq!(cut_utf16(b"\xFE\xFF", 0, 100), b"");
        assert_eq!(cut_utf16(b"\xFF\xFE", 0, 100), b"");
        assert_eq!(cut_utf16le(b"\xFF", 0, 100), b"");
        let le = b"\x1a\x04\x38\x04\x40\x04";
        assert_eq!(cut_utf16le(le, 0, 1), b"");
        assert_eq!(cut_utf16le(le, 0, 2), b"\x1a\x04");
        assert_eq!(cut_utf16le(le, 0, 3), b"\x1a\x04");
        assert_eq!(cut_utf16le(le, 1, 2), b"\x1a\x04");
        assert_eq!(cut_utf16le(le, 1, 3), b"\x1a\x04");
        assert_eq!(cut_utf16le(le, 1, 4), b"\x1a\x04\x38\x04");
    }
}

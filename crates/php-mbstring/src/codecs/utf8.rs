//! UTF-8.
//!
//! Ref: `libmbfl/filters/mbfilter_utf8.c` (the Japanese mobile variants live
//! in [`crate::codecs::cjk`]).

use crate::convert::{illegal_output, ConvertBuf, ErrorMode, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo};

/// Byte length of a UTF-8 character by its first byte (`mblen_table_utf8`).
#[rustfmt::skip]
pub static MBLEN_TABLE_UTF8: [u8; 256] = [
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
    4, 4, 4, 4, 4, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
];

/// `UTF-8`.
pub static ENC_UTF8: Encoding = Encoding {
    no: EncodingNo::Utf8,
    name: "UTF-8",
    mime_name: Some("UTF-8"),
    aliases: &["utf8"],
    mblen_table: Some(&MBLEN_TABLE_UTF8),
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: utf8_to_wchar,
    from_wchar: wchar_to_utf8,
    check: None,
    cut: Some(cut_utf8),
};

/// `mb_utf8_to_wchar`.
pub fn utf8_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let s = *input;
    let e = s.len();
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len();
    let cont = |b: u8| (b & 0xC0) == 0x80;

    while p < e && out < limit {
        let c = s[p];
        p += 1;
        if c < 0x80 {
            buf[out] = u32::from(c);
        } else if c < 0xC2 {
            buf[out] = BAD_INPUT;
        } else if c <= 0xDF {
            if p < e {
                let c2 = s[p];
                p += 1;
                if cont(c2) {
                    buf[out] = (u32::from(c & 0x1F) << 6) | u32::from(c2 & 0x3F);
                } else {
                    buf[out] = BAD_INPUT;
                    p -= 1;
                }
            } else {
                buf[out] = BAD_INPUT;
            }
        } else if c <= 0xEF {
            if e - p >= 2 {
                let c2 = s[p];
                let c3 = s[p + 1];
                p += 2;
                if !cont(c2) || (c == 0xE0 && c2 < 0xA0) || (c == 0xED && c2 >= 0xA0) {
                    buf[out] = BAD_INPUT;
                    p -= 2;
                } else if !cont(c3) {
                    buf[out] = BAD_INPUT;
                    p -= 1;
                } else {
                    buf[out] = (u32::from(c & 0xF) << 12)
                        | (u32::from(c2 & 0x3F) << 6)
                        | u32::from(c3 & 0x3F);
                }
            } else {
                buf[out] = BAD_INPUT;
                if p < e && (c != 0xE0 || s[p] >= 0xA0) && (c != 0xED || s[p] < 0xA0) && cont(s[p])
                {
                    p += 1;
                    if p < e && cont(s[p]) {
                        p += 1;
                    }
                }
            }
        } else if c <= 0xF4 {
            if e - p >= 3 {
                let c2 = s[p];
                let c3 = s[p + 1];
                let c4 = s[p + 2];
                p += 3;
                if !cont(c2) || (c == 0xF0 && c2 < 0x90) || (c == 0xF4 && c2 >= 0x90) {
                    buf[out] = BAD_INPUT;
                    p -= 3;
                } else if !cont(c3) {
                    buf[out] = BAD_INPUT;
                    p -= 2;
                } else if !cont(c4) {
                    buf[out] = BAD_INPUT;
                    p -= 1;
                } else {
                    buf[out] = (u32::from(c & 0x7) << 18)
                        | (u32::from(c2 & 0x3F) << 12)
                        | (u32::from(c3 & 0x3F) << 6)
                        | u32::from(c4 & 0x3F);
                }
            } else {
                buf[out] = BAD_INPUT;
                if p < e {
                    let c2 = s[p];
                    if (c == 0xF0 && c2 >= 0x90)
                        || (c == 0xF4 && c2 < 0x90)
                        || (0xF1..=0xF3).contains(&c)
                    {
                        while p < e && cont(s[p]) {
                            p += 1;
                        }
                    }
                }
            }
        } else {
            buf[out] = BAD_INPUT;
        }
        out += 1;
    }

    *input = &s[p..];
    out
}

/// Append the UTF-8 encoding of a valid code point (`< 0x110000`).
#[inline]
#[allow(clippy::cast_possible_truncation)]
pub fn push_utf8(out: &mut Vec<u8>, w: u32) {
    if w < 0x80 {
        out.push(w as u8);
    } else if w < 0x800 {
        out.extend([((w >> 6) & 0x1F) as u8 | 0xC0, (w & 0x3F) as u8 | 0x80]);
    } else if w < 0x10000 {
        out.extend([
            ((w >> 12) & 0xF) as u8 | 0xE0,
            ((w >> 6) & 0x3F) as u8 | 0x80,
            (w & 0x3F) as u8 | 0x80,
        ]);
    } else {
        out.extend([
            ((w >> 18) & 0x7) as u8 | 0xF0,
            ((w >> 12) & 0x3F) as u8 | 0x80,
            ((w >> 6) & 0x3F) as u8 | 0x80,
            (w & 0x3F) as u8 | 0x80,
        ]);
    }
}

/// `mb_wchar_to_utf8`.
pub fn wchar_to_utf8(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        if w < 0x800 {
            push_utf8(&mut buf.out, w);
        } else if (0xD800..=0xDFFF).contains(&w) && buf.error_mode != ErrorMode::BadUtf8 {
            illegal_output(w, wchar_to_utf8, buf);
        } else if w < 0x11_0000 {
            push_utf8(&mut buf.out, w);
        } else {
            illegal_output(w, wchar_to_utf8, buf);
        }
    }
}

/// `mb_cut_utf8`.
pub fn cut_utf8(s: &[u8], from: usize, len: usize) -> Vec<u8> {
    let is_cont = |b: u8| (0x80..0xC0).contains(&b);
    let mut start = from;
    while start > 0 && start < s.len() && is_cont(s[start]) {
        start -= 1;
    }
    let mut end = start + len;
    if end >= s.len() {
        return s[start..].to_vec();
    }
    while end > start && is_cont(s[end]) {
        end -= 1;
    }
    s[start..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::{decode_all, fast_convert};

    #[test]
    fn decode_invalid_sequences_like_libmbfl() {
        let w = decode_all(b"a\xC3\xA9\xE2\x82", &ENC_UTF8);
        assert_eq!(w, vec![0x61, 0xE9, BAD_INPUT]);
        let w = decode_all(b"\xF0\x9F\x98\x80\xED\xA0\x80", &ENC_UTF8);
        assert_eq!(w, vec![0x1F600, BAD_INPUT, BAD_INPUT, BAD_INPUT]);
        let w = decode_all(b"\xE0\x80\x80", &ENC_UTF8);
        assert_eq!(w, vec![BAD_INPUT, BAD_INPUT, BAD_INPUT]);
    }

    #[test]
    fn roundtrip_and_substitute() {
        let (out, errs) = fast_convert(
            b"x\xFFy",
            &ENC_UTF8,
            &ENC_UTF8,
            u32::from(b'?'),
            ErrorMode::Char,
        );
        assert_eq!(out, b"x?y");
        assert_eq!(errs, 1);
    }

    #[test]
    fn cut_aligns_to_boundaries() {
        let s = "aé日本".as_bytes();
        assert_eq!(cut_utf8(s, 2, 4), "é".as_bytes());
        assert_eq!(cut_utf8(s, 2, 5), "é日".as_bytes());
        assert_eq!(cut_utf8(s, 0, 2), b"a");
    }
}

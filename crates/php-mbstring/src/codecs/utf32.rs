// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `UTF-32`, `UTF-32BE`, `UTF-32LE`.
//!
//! Ref: `libmbfl/filters/mbfilter_utf32.c`.

use crate::codecs::ucs4::{bom32_to_wchar, decode32};
use crate::convert::{illegal_output, ConvertBuf};
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_WCS4};

/// `UTF-32`.
pub static ENC_UTF32: Encoding = Encoding {
    no: EncodingNo::Utf32,
    name: "UTF-32",
    mime_name: Some("UTF-32"),
    aliases: &["utf32"],
    mblen_table: None,
    flag: ENCTYPE_WCS4,
    input_filter: None,
    output_filter: None,
    to_wchar: utf32_to_wchar,
    from_wchar: wchar_to_utf32be,
    check: None,
    cut: None,
};

/// `UTF-32BE`.
pub static ENC_UTF32BE: Encoding = Encoding {
    no: EncodingNo::Utf32be,
    name: "UTF-32BE",
    mime_name: Some("UTF-32BE"),
    aliases: &[],
    mblen_table: None,
    flag: ENCTYPE_WCS4,
    input_filter: None,
    output_filter: None,
    to_wchar: utf32be_to_wchar,
    from_wchar: wchar_to_utf32be,
    check: None,
    cut: None,
};

/// `UTF-32LE`.
pub static ENC_UTF32LE: Encoding = Encoding {
    no: EncodingNo::Utf32le,
    name: "UTF-32LE",
    mime_name: Some("UTF-32LE"),
    aliases: &[],
    mblen_table: None,
    flag: ENCTYPE_WCS4,
    input_filter: None,
    output_filter: None,
    to_wchar: utf32le_to_wchar,
    from_wchar: wchar_to_utf32le,
    check: None,
    cut: None,
};

/// Valid Unicode scalar value (`< 0x110000`, not a surrogate).
fn is_scalar(w: u32) -> bool {
    w < 0x11_0000 && !(0xD800..=0xDFFF).contains(&w)
}

/// `mb_utf32_to_wchar`: big-endian unless a little-endian BOM comes first; a
/// leading big-endian BOM is dropped.
pub fn utf32_to_wchar(input: &mut &[u8], buf: &mut [u32], state: &mut u32) -> usize {
    bom32_to_wchar(input, buf, state, utf32be_to_wchar, utf32le_to_wchar)
}

/// `mb_utf32be_to_wchar`.
pub fn utf32be_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    decode32(input, buf, true, is_scalar)
}

/// `mb_utf32le_to_wchar`.
pub fn utf32le_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    decode32(input, buf, false, is_scalar)
}

/// `mb_wchar_to_utf32be`. (Like libmbfl, surrogate code points are encoded
/// as-is.)
pub fn wchar_to_utf32be(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        if w < 0x11_0000 {
            buf.extend(&w.to_be_bytes());
        } else {
            illegal_output(w, wchar_to_utf32be, buf);
        }
    }
}

/// `mb_wchar_to_utf32le`.
pub fn wchar_to_utf32le(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        if w < 0x11_0000 {
            buf.extend(&w.to_le_bytes());
        } else {
            illegal_output(w, wchar_to_utf32le, buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::{decode_all, encode_all, ErrorMode, BAD_INPUT};

    #[test]
    fn decode_rejects_surrogates_and_out_of_range() {
        let w = decode_all(
            b"\x00\x00\xD8\x00\x00\x11\x00\x00\x00\x10\xFF\xFF\x00",
            &ENC_UTF32BE,
        );
        assert_eq!(w, vec![BAD_INPUT, BAD_INPUT, 0x10_FFFF, BAD_INPUT]);
        assert_eq!(
            decode_all(b"\xFF\xFE\x00\x00\x00\xD8\x00\x00", &ENC_UTF32),
            vec![BAD_INPUT]
        );
    }

    #[test]
    fn encode() {
        let out = encode_all(&[0xD800, 0x11_0000], &ENC_UTF32LE, 0x3F, ErrorMode::Char);
        assert_eq!(out, b"\x00\xD8\x00\x00?\x00\x00\x00");
    }
}

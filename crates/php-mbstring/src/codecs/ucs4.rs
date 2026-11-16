// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `UCS-4`, `UCS-4BE`, `UCS-4LE`.
//!
//! Ref: `libmbfl/filters/mbfilter_ucs4.c`.

use crate::convert::{illegal_output, ConvertBuf, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_WCS4};

/// `UCS-4`.
pub static ENC_UCS4: Encoding = Encoding {
    no: EncodingNo::Ucs4,
    name: "UCS-4",
    mime_name: Some("UCS-4"),
    aliases: &["ISO-10646-UCS-4", "UCS4"],
    mblen_table: None,
    flag: ENCTYPE_WCS4,
    input_filter: None,
    output_filter: None,
    to_wchar: ucs4_to_wchar,
    from_wchar: wchar_to_ucs4be,
    check: None,
    cut: None,
};

/// `UCS-4BE`.
pub static ENC_UCS4BE: Encoding = Encoding {
    no: EncodingNo::Ucs4be,
    name: "UCS-4BE",
    mime_name: Some("UCS-4BE"),
    aliases: &["byte4be"],
    mblen_table: None,
    flag: ENCTYPE_WCS4,
    input_filter: None,
    output_filter: None,
    to_wchar: ucs4be_to_wchar,
    from_wchar: wchar_to_ucs4be,
    check: None,
    cut: None,
};

/// `UCS-4LE`.
pub static ENC_UCS4LE: Encoding = Encoding {
    no: EncodingNo::Ucs4le,
    name: "UCS-4LE",
    mime_name: Some("UCS-4LE"),
    aliases: &["byte4le"],
    mblen_table: None,
    flag: ENCTYPE_WCS4,
    input_filter: None,
    output_filter: None,
    to_wchar: ucs4le_to_wchar,
    from_wchar: wchar_to_ucs4le,
    check: None,
    cut: None,
};

pub(crate) const DETECTED_BE: u32 = 1;
pub(crate) const DETECTED_LE: u32 = 2;

/// Shared BOM sniffing of `mb_ucs4_to_wchar` / `mb_utf32_to_wchar`.
pub(crate) fn bom32_to_wchar(
    input: &mut &[u8],
    buf: &mut [u32],
    state: &mut u32,
    be: crate::convert::ToWcharFn,
    le: crate::convert::ToWcharFn,
) -> usize {
    if *state == DETECTED_BE {
        return be(input, buf, &mut 0);
    } else if *state == DETECTED_LE {
        return le(input, buf, &mut 0);
    } else if input.len() >= 4 {
        let w = u32::from_be_bytes([input[0], input[1], input[2], input[3]]);
        if w == 0xFFFE_0000 {
            *input = &input[4..];
            *state = DETECTED_LE;
            return le(input, buf, &mut 0);
        } else if w == 0xFEFF {
            *input = &input[4..];
        }
    }
    *state = DETECTED_BE;
    be(input, buf, &mut 0)
}

/// Shared 4-byte decoding loop; `valid` filters out values which become
/// [`BAD_INPUT`]. 1-3 trailing bytes are a single error.
pub(crate) fn decode32(
    input: &mut &[u8],
    buf: &mut [u32],
    be: bool,
    valid: fn(u32) -> bool,
) -> usize {
    let s = *input;
    let e = s.len() & !3;
    let limit = buf.len();
    let (mut p, mut out) = (0usize, 0usize);
    while p < e && out < limit {
        let bytes = [s[p], s[p + 1], s[p + 2], s[p + 3]];
        p += 4;
        let w = if be {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        };
        buf[out] = if valid(w) { w } else { BAD_INPUT };
        out += 1;
    }
    if p == e && s.len() & 3 != 0 && out < limit {
        // There are 1-3 trailing bytes, which shouldn't be there.
        buf[out] = BAD_INPUT;
        out += 1;
        p = s.len();
    }
    *input = &s[p..];
    out
}

/// `mb_ucs4_to_wchar`: big-endian unless a little-endian BOM comes first; a
/// leading big-endian BOM is dropped.
pub fn ucs4_to_wchar(input: &mut &[u8], buf: &mut [u32], state: &mut u32) -> usize {
    bom32_to_wchar(input, buf, state, ucs4be_to_wchar, ucs4le_to_wchar)
}

/// `mb_ucs4be_to_wchar`: every 32-bit value passes through unchanged.
pub fn ucs4be_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    decode32(input, buf, true, |_| true)
}

/// `mb_ucs4le_to_wchar`.
pub fn ucs4le_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    decode32(input, buf, false, |_| true)
}

/// `mb_wchar_to_ucs4be`: everything but [`BAD_INPUT`] is representable.
pub fn wchar_to_ucs4be(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        if w == BAD_INPUT {
            illegal_output(w, wchar_to_ucs4be, buf);
        } else {
            buf.extend(&w.to_be_bytes());
        }
    }
}

/// `mb_wchar_to_ucs4le`.
pub fn wchar_to_ucs4le(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        if w == BAD_INPUT {
            illegal_output(w, wchar_to_ucs4le, buf);
        } else {
            buf.extend(&w.to_le_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::{decode_all, encode_all, ErrorMode};

    #[test]
    fn bom_and_trailing_bytes() {
        assert_eq!(
            decode_all(b"\x00\x00\xFE\xFF\x00\x00\x00A", &ENC_UCS4),
            vec![0x41]
        );
        assert_eq!(
            decode_all(b"\xFF\xFE\x00\x00A\x00\x00\x00", &ENC_UCS4),
            vec![0x41]
        );
        assert_eq!(
            decode_all(b"\x00\x00\x00A\x00\x00", &ENC_UCS4),
            vec![0x41, BAD_INPUT]
        );
        assert_eq!(
            decode_all(b"\x7F\xFF\xFF\xFF", &ENC_UCS4BE),
            vec![0x7FFF_FFFF]
        );
        assert_eq!(decode_all(b"\x01", &ENC_UCS4LE), vec![BAD_INPUT]);
    }

    #[test]
    fn encode() {
        let out = encode_all(
            &[0x41, BAD_INPUT, 0x8000_0000],
            &ENC_UCS4LE,
            0x3F,
            ErrorMode::Char,
        );
        assert_eq!(out, b"A\x00\x00\x00?\x00\x00\x00\x00\x00\x00\x80");
    }
}

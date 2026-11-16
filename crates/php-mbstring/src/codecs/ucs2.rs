// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `UCS-2`, `UCS-2BE`, `UCS-2LE`.
//!
//! Ref: `libmbfl/filters/mbfilter_ucs2.c`.

use crate::convert::{illegal_output, ConvertBuf, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_WCS2};

/// `UCS-2`.
pub static ENC_UCS2: Encoding = Encoding {
    no: EncodingNo::Ucs2,
    name: "UCS-2",
    mime_name: Some("UCS-2"),
    aliases: &["ISO-10646-UCS-2", "UCS2", "UNICODE"],
    mblen_table: None,
    flag: ENCTYPE_WCS2,
    input_filter: None,
    output_filter: None,
    to_wchar: ucs2_to_wchar,
    from_wchar: wchar_to_ucs2be,
    check: None,
    cut: None,
};

/// `UCS-2BE`.
pub static ENC_UCS2BE: Encoding = Encoding {
    no: EncodingNo::Ucs2be,
    name: "UCS-2BE",
    mime_name: Some("UCS-2BE"),
    aliases: &["byte2be"],
    mblen_table: None,
    flag: ENCTYPE_WCS2,
    input_filter: None,
    output_filter: None,
    to_wchar: ucs2be_to_wchar,
    from_wchar: wchar_to_ucs2be,
    check: None,
    cut: None,
};

/// `UCS-2LE`.
pub static ENC_UCS2LE: Encoding = Encoding {
    no: EncodingNo::Ucs2le,
    name: "UCS-2LE",
    mime_name: Some("UCS-2LE"),
    aliases: &["byte2le"],
    mblen_table: None,
    flag: ENCTYPE_WCS2,
    input_filter: None,
    output_filter: None,
    to_wchar: ucs2le_to_wchar,
    from_wchar: wchar_to_ucs2le,
    check: None,
    cut: None,
};

const DETECTED_BE: u32 = 1;
const DETECTED_LE: u32 = 2;

/// `mb_ucs2_to_wchar`: big-endian unless a little-endian BOM comes first; a
/// leading big-endian BOM is dropped.
pub fn ucs2_to_wchar(input: &mut &[u8], buf: &mut [u32], state: &mut u32) -> usize {
    if *state == DETECTED_BE {
        return ucs2be_to_wchar(input, buf, &mut 0);
    } else if *state == DETECTED_LE {
        return ucs2le_to_wchar(input, buf, &mut 0);
    } else if input.len() >= 2 {
        let w = (u32::from(input[0]) << 8) | u32::from(input[1]);
        if w == 0xFFFE {
            *input = &input[2..];
            *state = DETECTED_LE;
            return ucs2le_to_wchar(input, buf, &mut 0);
        } else if w == 0xFEFF {
            *input = &input[2..];
        }
    }
    *state = DETECTED_BE;
    ucs2be_to_wchar(input, buf, &mut 0)
}

fn ucs2_decode(input: &mut &[u8], buf: &mut [u32], be: bool) -> usize {
    let s = *input;
    let e = s.len() & !1;
    let limit = buf.len();
    let (mut p, mut out) = (0usize, 0usize);
    while p < e && out < limit {
        let (c1, c2) = (u32::from(s[p]), u32::from(s[p + 1]));
        p += 2;
        buf[out] = if be { (c1 << 8) | c2 } else { (c2 << 8) | c1 };
        out += 1;
    }
    if p == e && s.len() & 1 != 0 && out < limit {
        // There is 1 trailing byte, which shouldn't be there.
        buf[out] = BAD_INPUT;
        out += 1;
        p += 1;
    }
    *input = &s[p..];
    out
}

/// `mb_ucs2be_to_wchar`.
pub fn ucs2be_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    ucs2_decode(input, buf, true)
}

/// `mb_ucs2le_to_wchar`.
pub fn ucs2le_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    ucs2_decode(input, buf, false)
}

/// `mb_wchar_to_ucs2be`.
#[allow(clippy::cast_possible_truncation)]
pub fn wchar_to_ucs2be(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        if w < 0x1_0000 {
            buf.extend(&(w as u16).to_be_bytes());
        } else {
            illegal_output(w, wchar_to_ucs2be, buf);
        }
    }
}

/// `mb_wchar_to_ucs2le`.
#[allow(clippy::cast_possible_truncation)]
pub fn wchar_to_ucs2le(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        if w < 0x1_0000 {
            buf.extend(&(w as u16).to_le_bytes());
        } else {
            illegal_output(w, wchar_to_ucs2le, buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::{decode_all, encode_all, ErrorMode};

    #[test]
    fn bom_detection() {
        assert_eq!(decode_all(b"\xFE\xFF\x00A", &ENC_UCS2), vec![0x41]);
        assert_eq!(decode_all(b"\xFF\xFEA\x00", &ENC_UCS2), vec![0x41]);
        assert_eq!(decode_all(b"\x00A\xFF\xFE", &ENC_UCS2), vec![0x41, 0xFFFE]);
        assert_eq!(decode_all(b"\x00A\x00", &ENC_UCS2), vec![0x41, BAD_INPUT]);
        assert_eq!(decode_all(b"A", &ENC_UCS2), vec![BAD_INPUT]);
    }

    #[test]
    fn endianness_and_errors() {
        assert_eq!(decode_all(b"\xD8\x00", &ENC_UCS2BE), vec![0xD800]);
        assert_eq!(
            decode_all(b"\x00\xD8\x01", &ENC_UCS2LE),
            vec![0xD800, BAD_INPUT]
        );
        let out = encode_all(
            &[0x41, 0x1_0000, 0xD800],
            &ENC_UCS2LE,
            0x3F,
            ErrorMode::Char,
        );
        assert_eq!(out, b"A\x00?\x00\x00\xD8");
        let out = encode_all(&[0x41, 0x1_0000], &ENC_UCS2BE, 0x3F, ErrorMode::Long);
        assert_eq!(out, b"\x00A\x00U\x00+\x001\x000\x000\x000\x000");
    }
}

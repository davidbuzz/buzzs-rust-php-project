// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `BASE64` (a transfer encoding: it converts raw bytes, not text).
//!
//! `mb_convert_encoding` treats the other side of a conversion from/to Base64
//! as `8bit` ([`crate::convert::effective_pair`]); the legacy filters used by
//! `mb_strcut` are the special 8bit<->Base64 filters [`VTBL_8BIT_B64`] and
//! [`VTBL_B64_8BIT`] (the [`Encoding`] itself has no legacy filters).
//!
//! Ref: `libmbfl/filters/mbfilter_base64.c`.

use crate::convert::{ConvertBuf, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};
use crate::legacy::{self, Filter, Sink, Vtbl, BAD_INPUT_I32};

/// `BASE64`.
pub static ENC_BASE64: Encoding = Encoding {
    no: EncodingNo::Base64,
    name: "BASE64",
    mime_name: Some("BASE64"),
    aliases: &[],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: base64_to_wchar,
    from_wchar: wchar_to_base64,
    check: None,
    cut: None,
};

/// `vtbl_8bit_b64`: bytes to Base64.
pub static VTBL_8BIT_B64: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_base64enc,
    flush: filt_base64enc_flush,
};

/// `vtbl_b64_8bit`: Base64 to bytes.
pub static VTBL_B64_8BIT: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_base64dec,
    flush: filt_base64dec_flush,
};

/// `mbfl_base64_table`.
const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn put_digit(v: i32, out: &mut dyn Sink) {
    #[allow(clippy::cast_sign_loss)]
    out.put(i32::from(TABLE[(v & 0x3F) as usize]));
}

/// `mbfl_filt_conv_base64enc`. `status` bits 0-7 count cached bytes, bits
/// 8-15 the current output line length.
pub fn filt_base64enc(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    let n = filter.status & 0xFF;
    if n == 0 {
        filter.status += 1;
        filter.cache = (c & 0xFF) << 16;
    } else if n == 1 {
        filter.status += 1;
        filter.cache |= (c & 0xFF) << 8;
    } else {
        filter.status &= !0xFF;
        if (filter.status & 0xFF00) >> 8 > 72 {
            out.put(0x0D);
            out.put(0x0A);
            filter.status &= !0xFF00;
        }
        filter.status += 0x400;
        let n = filter.cache | (c & 0xFF);
        put_digit(n >> 18, out);
        put_digit(n >> 12, out);
        put_digit(n >> 6, out);
        put_digit(n, out);
    }
}

/// `mbfl_filt_conv_base64enc_flush`.
pub fn filt_base64enc_flush(filter: &mut Filter, out: &mut dyn Sink) {
    let status = filter.status & 0xFF;
    let cache = filter.cache;
    let len = (filter.status & 0xFF00) >> 8;
    filter.status &= !0xFFFF;
    filter.cache = 0;
    if status >= 1 {
        if len > 72 {
            out.put(0x0D);
            out.put(0x0A);
        }
        put_digit(cache >> 18, out);
        put_digit(cache >> 12, out);
        if status == 1 {
            out.put(i32::from(b'='));
            out.put(i32::from(b'='));
        } else {
            put_digit(cache >> 6, out);
            out.put(i32::from(b'='));
        }
    }
    out.flush();
}

/// `mbfl_filt_conv_base64dec`.
pub fn filt_base64dec(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    // CR, LF, SPACE, HTAB and '=' are skipped.
    if matches!(c, 0x0D | 0x0A | 0x20 | 0x09 | 0x3D) {
        return;
    }
    let n = match c {
        0x41..=0x5A => c - 65,
        0x61..=0x7A => c - 71,
        0x30..=0x39 => c + 4,
        0x2B => 62,
        0x2F => 63,
        _ => {
            out.put(BAD_INPUT_I32);
            return;
        }
    } & 0x3F;

    match filter.status {
        0 => {
            filter.status = 1;
            filter.cache = n << 18;
        }
        1 => {
            filter.status = 2;
            filter.cache |= n << 12;
        }
        2 => {
            filter.status = 3;
            filter.cache |= n << 6;
        }
        _ => {
            filter.status = 0;
            let n = n | filter.cache;
            out.put((n >> 16) & 0xFF);
            out.put((n >> 8) & 0xFF);
            out.put(n & 0xFF);
        }
    }
}

/// `mbfl_filt_conv_base64dec_flush`.
pub fn filt_base64dec_flush(filter: &mut Filter, out: &mut dyn Sink) {
    let (status, cache) = (filter.status, filter.cache);
    filter.status = 0;
    filter.cache = 0;
    if status >= 2 {
        out.put((cache >> 16) & 0xFF);
        if status >= 3 {
            out.put((cache >> 8) & 0xFF);
        }
    }
    out.flush();
}

fn decode_base64(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some(u32::from(c - b'A')),
        b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// `mb_base64_to_wchar`: decoded bytes as code points `0..=0xFF`. Needs a
/// buffer of at least 3 code points.
pub fn base64_to_wchar(input: &mut &[u8], buf: &mut [u32], state: &mut u32) -> usize {
    debug_assert!(buf.len() >= 3);
    let s = *input;
    let e = s.len();
    let limit = buf.len();
    let (mut p, mut out) = (0usize, 0usize);
    let mut bits = *state & 0xFF;
    let mut cache = *state >> 8;

    while p < e && limit - out >= 3 {
        let c = s[p];
        p += 1;
        if matches!(c, b'\r' | b'\n' | b' ' | b'\t' | b'=') {
            continue;
        }
        if let Some(value) = decode_base64(c) {
            bits += 6;
            cache = (cache << 6) | (value & 0x3F);
            if bits == 24 {
                buf[out] = (cache >> 16) & 0xFF;
                buf[out + 1] = (cache >> 8) & 0xFF;
                buf[out + 2] = cache & 0xFF;
                out += 3;
                bits = 0;
                cache = 0;
            }
        } else {
            buf[out] = BAD_INPUT;
            out += 1;
        }
    }

    if p == e {
        // At least 3 slots remain free here.
        if bits == 18 {
            buf[out] = (cache >> 10) & 0xFF;
            buf[out + 1] = (cache >> 2) & 0xFF;
            out += 2;
        } else if bits == 12 {
            buf[out] = (cache >> 4) & 0xFF;
            out += 1;
        }
    } else {
        *state = (cache << 8) | (bits & 0xFF);
    }

    *input = &s[p..];
    out
}

/// `mb_wchar_to_base64`: input code points are taken as bytes (`& 0xFF`).
/// Lines are broken with CRLF after 76 output characters.
pub fn wchar_to_base64(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    let mut bits = (buf.state & 0x3) * 8;
    let mut chars_output = ((buf.state >> 2) & 0x3F) * 4;
    let mut cache = buf.state >> 8;
    let digit = |v: u32| TABLE[(v & 0x3F) as usize];

    for &w in input {
        cache = (cache << 8) | (w & 0xFF);
        bits += 8;
        if bits == 24 {
            if chars_output > 72 {
                buf.extend(b"\r\n");
                chars_output = 0;
            }
            buf.extend(&[
                digit(cache >> 18),
                digit(cache >> 12),
                digit(cache >> 6),
                digit(cache),
            ]);
            chars_output += 4;
            bits = 0;
            cache = 0;
        }
    }

    if end && bits != 0 {
        if chars_output > 72 {
            buf.extend(b"\r\n");
        }
        if bits == 8 {
            buf.extend(&[digit(cache >> 2), digit((cache & 0x3) << 4), b'=', b'=']);
        } else {
            buf.extend(&[
                digit(cache >> 10),
                digit(cache >> 4),
                digit((cache & 0xF) << 2),
                b'=',
            ]);
        }
    } else {
        buf.state = (cache << 8) | (((chars_output / 4) & 0x3F) << 2) | ((bits / 8) & 0x3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::{decode_all, encode_all, ErrorMode};
    use crate::legacy::ByteSink;

    fn legacy(vtbl: &Vtbl, input: &[u8]) -> Vec<u8> {
        let mut f = Filter::new(vtbl);
        let mut sink = ByteSink::default();
        for &b in input {
            f.feed(i32::from(b), &mut sink);
        }
        f.flush(&mut sink);
        sink.0
    }

    #[test]
    fn base64_encoding_phpt() {
        // base64_encoding.phpt
        let enc = |s: &[u8]| {
            encode_all(
                &s.iter().map(|&b| u32::from(b)).collect::<Vec<_>>(),
                &ENC_BASE64,
                0x3F,
                ErrorMode::Char,
            )
        };
        assert_eq!(enc(b""), b"");
        assert_eq!(enc(b"\x00"), b"AA==");
        assert_eq!(enc(b"\x00\x00"), b"AAA=");
        assert_eq!(enc(b"\x00\x00\x00"), b"AAAA");
        assert_eq!(enc(&[b'A'; 60]).len(), 82);
        let dec = |s: &[u8]| decode_all(s, &ENC_BASE64);
        assert_eq!(dec(b"AA=="), vec![0]);
        assert_eq!(dec(b"AAA="), vec![0, 0]);
        assert_eq!(dec(b"QU\r\nJD"), vec![0x41, 0x42, 0x43]);
        assert_eq!(dec(b"Q*"), vec![BAD_INPUT]);
    }

    #[test]
    fn legacy_filters_match_fast_path() {
        let data: Vec<u8> = (0..=255u8).cycle().take(300).collect();
        let fast = encode_all(
            &data.iter().map(|&b| u32::from(b)).collect::<Vec<_>>(),
            &ENC_BASE64,
            0x3F,
            ErrorMode::Char,
        );
        for n in [0, 1, 2, 3, 56, 57, 58, 59, 300] {
            let fast_n = encode_all(
                &data[..n].iter().map(|&b| u32::from(b)).collect::<Vec<_>>(),
                &ENC_BASE64,
                0x3F,
                ErrorMode::Char,
            );
            assert_eq!(legacy(&VTBL_8BIT_B64, &data[..n]), fast_n, "n={n}");
        }
        assert_eq!(legacy(&VTBL_B64_8BIT, &fast), data);
        assert_eq!(legacy(&VTBL_B64_8BIT, b"QUJ"), b"AB");
        assert_eq!(legacy(&VTBL_B64_8BIT, b"QU*"), b"\xFFA");
        assert_eq!(legacy(&VTBL_B64_8BIT, b"Q"), b"");
    }
}

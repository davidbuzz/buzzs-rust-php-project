// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `Quoted-Printable` (a transfer encoding: it converts raw bytes, not
//! text).
//!
//! `mb_convert_encoding` treats the other side of a conversion from/to
//! Quoted-Printable as `8bit` ([`crate::convert::effective_pair`]); the legacy
//! filters used by `mb_strcut` are the special 8bit/`QPrint` filters
//! [`VTBL_8BIT_QPRINT`] and [`VTBL_QPRINT_8BIT`] (the [`Encoding`] itself has
//! no legacy filters).
//!
//! Ref: `libmbfl/filters/mbfilter_qprint.c`.

use crate::convert::ConvertBuf;
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};
use crate::legacy::{self, Filter, Sink, Vtbl};

/// `Quoted-Printable`.
pub static ENC_QPRINT: Encoding = Encoding {
    no: EncodingNo::Qprint,
    name: "Quoted-Printable",
    mime_name: Some("Quoted-Printable"),
    aliases: &["qprint"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: qprint_to_wchar,
    from_wchar: wchar_to_qprint,
    check: None,
    cut: None,
};

/// `vtbl_8bit_qprint`: bytes to Quoted-Printable.
pub static VTBL_8BIT_QPRINT: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_qprintenc,
    flush: filt_qprintenc_flush,
};

/// `vtbl_qprint_8bit`: Quoted-Printable to bytes.
pub static VTBL_QPRINT_8BIT: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_qprintdec,
    flush: filt_qprintdec_flush,
};

/// `hex2code_map`: value of a hex digit (either case).
fn hex2code(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'F' => Some(c - b'A' + 10),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn hex2code_i32(c: i32) -> Option<i32> {
    hex2code((c & 0xFF) as u8).map(i32::from)
}

fn hex_digit(n: i32) -> i32 {
    if n < 10 {
        n + 48
    } else {
        n + 55
    }
}

/// `mbfl_filt_conv_qprintenc`: one byte of look-ahead is kept in `cache`
/// (to turn bare CR/LF into CRLF); `status` bits 8-15 hold the line length.
pub fn filt_qprintenc(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    if filter.status.trailing_zeros() >= 8 {
        filter.cache = c;
        filter.status += 1;
        return;
    }
    let s = filter.cache;
    filter.cache = c;
    let n = (filter.status & 0xFF00) >> 8;

    if s == 0 {
        out.put(s);
        filter.status &= !0xFF00;
        return;
    }
    if s == i32::from(b'\n') || (s == i32::from(b'\r') && c != i32::from(b'\n')) {
        out.put(i32::from(b'\r'));
        out.put(i32::from(b'\n'));
        filter.status &= !0xFF00;
        return;
    } else if s == 0x0D {
        return;
    }

    if n >= 72 {
        // soft line break
        out.put(i32::from(b'='));
        out.put(i32::from(b'\r'));
        out.put(i32::from(b'\n'));
        filter.status &= !0xFF00;
    }

    if s <= 0 || s >= 0x80 || s == i32::from(b'=') {
        out.put(i32::from(b'='));
        out.put(hex_digit((s >> 4) & 0xF));
        out.put(hex_digit(s & 0xF));
        filter.status += 0x300;
    } else {
        out.put(s);
        filter.status += 0x100;
    }
}

/// `mbfl_filt_conv_qprintenc_flush`: pushes out the cached byte by feeding
/// a NUL.
pub fn filt_qprintenc_flush(filter: &mut Filter, out: &mut dyn Sink) {
    filter.feed(0, out);
    filter.status &= !0xFFFF;
    filter.cache = 0;
    out.flush();
}

/// `mbfl_filt_conv_qprintdec`.
pub fn filt_qprintdec(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    match filter.status {
        1 => {
            if hex2code_i32(c).is_some() {
                filter.cache = c;
                filter.status = 2;
            } else if c == 0x0D {
                // soft line break
                filter.status = 3;
            } else if c == 0x0A {
                filter.status = 0;
            } else {
                out.put(0x3D);
                out.put(c);
                filter.status = 0;
            }
        }
        2 => {
            let n = if let Some(m) = hex2code_i32(c) {
                (hex2code_i32(filter.cache).unwrap_or(0) << 4) | m
            } else {
                out.put(0x3D);
                out.put(filter.cache);
                c
            };
            out.put(n);
            filter.status = 0;
        }
        3 => {
            if c != 0x0A {
                out.put(c);
            }
            filter.status = 0;
        }
        _ => {
            if c == 0x3D {
                filter.status = 1;
            } else {
                out.put(c);
            }
        }
    }
}

/// `mbfl_filt_conv_qprintdec_flush`.
pub fn filt_qprintdec_flush(filter: &mut Filter, out: &mut dyn Sink) {
    let (status, cache) = (filter.status, filter.cache);
    filter.status = 0;
    filter.cache = 0;
    if status == 1 {
        out.put(0x3D);
    } else if status == 2 {
        out.put(0x3D);
        out.put(cache);
    }
    out.flush();
}

/// `mb_qprint_to_wchar`: decoded bytes as code points `0..=0xFF`. Needs a
/// buffer of at least 3 code points.
#[allow(clippy::many_single_char_names)]
pub fn qprint_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let s = *input;
    let e = s.len();
    let limit = buf.len().saturating_sub(2);
    let (mut p, mut out) = (0usize, 0usize);
    let emit = |buf: &mut [u32], out: &mut usize, w: u8| {
        buf[*out] = u32::from(w);
        *out += 1;
    };

    while p < e && out < limit {
        let c = s[p];
        p += 1;
        if c == b'=' && p < e {
            let c2 = s[p];
            p += 1;
            if hex2code(c2).is_some() && p < e {
                let c3 = s[p];
                p += 1;
                if let (Some(h), Some(l)) = (hex2code(c2), hex2code(c3)) {
                    emit(buf, &mut out, (h << 4) | l);
                } else {
                    emit(buf, &mut out, b'=');
                    emit(buf, &mut out, c2);
                    emit(buf, &mut out, c3);
                }
            } else if c2 == b'\r' && p < e {
                let c3 = s[p];
                p += 1;
                if c3 != b'\n' {
                    emit(buf, &mut out, c3);
                }
            } else if c2 != b'\n' {
                emit(buf, &mut out, b'=');
                emit(buf, &mut out, c2);
            }
        } else {
            emit(buf, &mut out, c);
        }
    }

    *input = &s[p..];
    out
}

fn enc_nibble(nibble: u32) -> u8 {
    #[allow(clippy::cast_possible_truncation)]
    let n = nibble as u8;
    if n < 10 {
        n + b'0'
    } else {
        n - 10 + b'A'
    }
}

/// `mb_wchar_to_qprint`: input code points are taken as bytes. Lines are
/// soft-broken (`=` CRLF) after 72 characters; LF becomes CRLF and CR is
/// dropped.
#[allow(clippy::cast_possible_truncation)]
pub fn wchar_to_qprint(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    let mut chars_output = buf.state;
    for &w in input {
        if w == 0 {
            buf.push(0);
            chars_output = 0;
            continue;
        } else if w == u32::from(b'\n') {
            buf.extend(b"\r\n");
            chars_output = 0;
            continue;
        } else if w == u32::from(b'\r') {
            continue;
        }

        // QPrint mandates lines of at most 76 characters; mbstring stops
        // slightly short of that.
        if chars_output >= 72 {
            buf.extend(b"=\r\n");
            chars_output = 0;
        }

        if w >= 0x80 || w == u32::from(b'=') {
            buf.extend(&[b'=', enc_nibble((w >> 4) & 0xF), enc_nibble(w & 0xF)]);
            chars_output += 3;
        } else {
            buf.push(w as u8);
            chars_output += 1;
        }
    }
    buf.state = chars_output;
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

    fn enc(s: &[u8]) -> Vec<u8> {
        let w: Vec<u32> = s.iter().map(|&b| u32::from(b)).collect();
        encode_all(&w, &ENC_QPRINT, 0x3F, ErrorMode::Char)
    }

    #[test]
    fn qprint_basics() {
        assert_eq!(enc(b"a=b\xE9\nc\r\nd"), b"a=3Db=E9\r\nc\r\nd");
        assert_eq!(enc(&[b'x'; 80]).len(), 83);
        let dec = |s: &[u8]| decode_all(s, &ENC_QPRINT);
        assert_eq!(
            dec(b"a=3D=e9=\r\nb=\nc"),
            vec![0x61, 0x3D, 0xE9, 0x62, 0x63]
        );
        assert_eq!(dec(b"=4G="), vec![0x3D, 0x34, 0x47, 0x3D]);
        assert_eq!(dec(b"=4"), vec![0x3D, 0x34]);
    }

    #[test]
    fn legacy_filters() {
        // The legacy encoder agrees with the fast path.
        assert_eq!(legacy(&VTBL_8BIT_QPRINT, b"a=b\xE9\nc"), b"a=3Db=E9\r\nc");
        assert_eq!(legacy(&VTBL_8BIT_QPRINT, b"a\rb\r\n"), b"a\r\nb\r\n");
        let long = [b'x'; 80];
        assert_eq!(legacy(&VTBL_8BIT_QPRINT, &long), enc(&long));
        assert_eq!(
            legacy(&VTBL_QPRINT_8BIT, b"a=3D=e9=\r\nb=\nc=4G=4"),
            b"a=\xE9bc=4G=4"
        );
        assert_eq!(legacy(&VTBL_QPRINT_8BIT, b"x="), b"x=");
    }
}

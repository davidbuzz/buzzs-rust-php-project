// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ISO-2022-KR` (RFC 1557).
//!
//! The RFC says that `ESC $ ) C` must appear once in an ISO-2022-KR string,
//! at the beginning of a line, before any Shift In / Shift Out bytes (which
//! switch between ASCII and KS C 5601). This is not enforced for input.
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c` (`mbfl_filt_conv_2022kr_wchar`,
//! `mbfl_filt_conv_wchar_2022kr`, `mb_iso2022kr_to_wchar`,
//! `mb_wchar_to_iso2022kr`).

use super::euc::ucs_to_uhc;
use crate::convert::{illegal_output, ConvertBuf, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};
use crate::legacy::{self, Filter, Sink, Vtbl, BAD_INPUT_I32};
use crate::tables::cjk::uhc;

/// `ISO-2022-KR`.
pub static ENC_2022KR: Encoding = Encoding {
    no: EncodingNo::Iso2022Kr,
    name: "ISO-2022-KR",
    mime_name: Some("ISO-2022-KR"),
    aliases: &[],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: Some(&VTBL_2022KR_WCHAR),
    output_filter: Some(&VTBL_WCHAR_2022KR),
    to_wchar: iso2022kr_to_wchar,
    from_wchar: wchar_to_iso2022kr,
    check: None,
    cut: None,
};

/// `vtbl_2022kr_wchar`: legacy ISO-2022-KR decoder.
pub static VTBL_2022KR_WCHAR: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_2022kr_wchar,
    flush: filt_2022kr_wchar_flush,
};

/// `vtbl_wchar_2022kr`: legacy ISO-2022-KR encoder.
pub static VTBL_WCHAR_2022KR: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_wchar_2022kr,
    flush: filt_any_2022kr_flush,
};

#[inline]
#[allow(clippy::cast_sign_loss)]
fn uhc1(i: i32) -> i32 {
    usize::try_from(i)
        .ok()
        .and_then(|i| uhc::UHC1_UCS_TABLE.get(i))
        .map_or(0, |&x| i32::from(x))
}

/// `mbfl_filt_conv_2022kr_wchar`.
pub fn filt_2022kr_wchar(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    match filter.status & 0xF {
        // 0x00: ASCII, 0x10: KSC5601
        0 => {
            if c == 0x1B {
                filter.status += 2;
            } else if c == 0x0F {
                filter.status = 0; // shift in (ASCII)
            } else if c == 0x0E {
                filter.status = 0x10; // shift out (KSC5601)
            } else if (filter.status & 0x10) != 0 && c > 0x20 && c < 0x7F {
                // KSC5601 lead byte
                filter.cache = c;
                filter.status = 0x11;
            } else if (filter.status & 0x10) == 0 && (0..0x80).contains(&c) {
                out.put(c); // latin, CTLs
            } else {
                out.put(BAD_INPUT_I32);
            }
        }
        1 => {
            // DBCS second byte
            filter.status = 0x10;
            let c1 = filter.cache;
            let flag = if c1 > 0x20 && c1 < 0x47 {
                1
            } else if (0x47..=0x7E).contains(&c1) && c1 != 0x49 {
                2
            } else {
                0
            };

            if flag > 0 && c > 0x20 && c < 0x7F {
                let mut w = 0;
                if flag == 1 {
                    if c1 != 0x22 || c <= 0x65 {
                        w = uhc1((c1 - 1) * 190 + (c - 0x41) + 0x80);
                    }
                } else {
                    w = usize::try_from((c1 - 0x47) * 94 + c - 0x21)
                        .ok()
                        .and_then(|i| uhc::UHC3_UCS_TABLE.get(i))
                        .map_or(BAD_INPUT_I32, |&x| i32::from(x));
                }
                if w <= 0 {
                    w = BAD_INPUT_I32;
                }
                out.put(w);
            } else {
                out.put(BAD_INPUT_I32);
            }
        }
        2 => {
            // ESC
            if c == i32::from(b'$') {
                filter.status += 1;
            } else {
                filter.status &= !0xF;
                out.put(BAD_INPUT_I32);
            }
        }
        3 => {
            // ESC $
            if c == i32::from(b')') {
                filter.status += 1;
            } else {
                filter.status &= !0xF;
                out.put(BAD_INPUT_I32);
            }
        }
        _ => {
            // ESC $ )
            filter.status = 0;
            if c != i32::from(b'C') {
                out.put(BAD_INPUT_I32);
            }
        }
    }
}

/// `mbfl_filt_conv_2022kr_wchar_flush`.
pub fn filt_2022kr_wchar_flush(filter: &mut Filter, out: &mut dyn Sink) {
    if filter.status & 0xF != 0 {
        // 2-byte character was truncated
        out.put(BAD_INPUT_I32);
    }
    filter.status = 0;
    out.flush();
}

/// `mbfl_filt_conv_wchar_2022kr`.
#[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
pub fn filt_wchar_2022kr(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    if (filter.status & 0x100) == 0 {
        out.put(0x1B); // ESC
        out.put(i32::from(b'$'));
        out.put(i32::from(b')'));
        out.put(i32::from(b'C'));
        filter.status |= 0x100;
    }

    let mut s = if c >= 0 {
        ucs_to_uhc(c as u32) as i32
    } else {
        0
    };

    let c1 = (s >> 8) & 0xFF;
    let c2 = s & 0xFF;
    // exclude UHC extension area
    if c1 < 0xA1 || c2 < 0xA1 {
        s = c;
    } else if s & 0x8000 != 0 {
        s -= 0x8080;
    }

    if s <= 0 {
        s = if c == 0 { 0 } else { -1 };
    } else if (0x80..0x2121).contains(&s) || s > 0x8080 {
        s = -1;
    }

    if s >= 0 {
        if s < 0x80 {
            // ASCII
            if filter.status & 0x10 != 0 {
                out.put(0x0F); // shift in
                filter.status &= !0x10;
            }
            out.put(s);
        } else {
            if (filter.status & 0x10) == 0 {
                out.put(0x0E); // shift out
                filter.status |= 0x10;
            }
            out.put((s >> 8) & 0xFF);
            out.put(s & 0xFF);
        }
    } else {
        legacy::illegal_output(c, filter, out);
    }
}

/// `mbfl_filt_conv_any_2022kr_flush`.
pub fn filt_any_2022kr_flush(filter: &mut Filter, out: &mut dyn Sink) {
    if filter.status & 0xF != 0 {
        // Escape sequence or 2-byte character was truncated
        out.put(BAD_INPUT_I32);
    }
    // back to ascii
    if filter.status & 0x10 != 0 {
        out.put(0x0F); // shift in
    }
    filter.status = 0;
    filter.cache = 0;
    out.flush();
}

const ASCII: u32 = 0;
const KSC5601: u32 = 1;

/// `mb_iso2022kr_to_wchar`.
pub fn iso2022kr_to_wchar(input: &mut &[u8], buf: &mut [u32], state: &mut u32) -> usize {
    let src = *input;
    let end = src.len();
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len();

    while p < end && out < limit {
        let c = src[p];
        p += 1;

        if c == 0x1B {
            if end - p < 3 {
                buf[out] = BAD_INPUT;
                out += 1;
                if p < end {
                    let c2 = src[p];
                    p += 1;
                    if c2 == b'$' && p < end {
                        p += 1;
                    }
                }
                continue;
            }
            let (c2, c3, c4) = (src[p], src[p + 1], src[p + 2]);
            p += 3;
            if c2 == b'$' && c3 == b')' && c4 == b'C' {
                *state = ASCII;
            } else {
                if c3 != b')' {
                    p -= 1;
                    if c2 != b'$' {
                        p -= 1;
                    }
                }
                buf[out] = BAD_INPUT;
                out += 1;
            }
        } else if c == 0xF {
            *state = ASCII;
        } else if c == 0xE {
            *state = KSC5601;
        } else if (0x21..=0x7E).contains(&c) && *state == KSC5601 {
            if p == end {
                buf[out] = BAD_INPUT;
                out += 1;
                break;
            }
            let c2 = src[p];
            p += 1;

            if !(0x21..=0x7E).contains(&c2) {
                buf[out] = BAD_INPUT;
                out += 1;
                continue;
            }

            let mut w = 0u32;
            if c < 0x47 {
                if c != 0x22 || c2 <= 0x65 {
                    let k = usize::from(c - 1) * 190 + usize::from(c2) + 0x80 - 0x41;
                    w = uhc::UHC1_UCS_TABLE.get(k).map_or(0, |&x| u32::from(x));
                }
            } else if c != 0x49 && c <= 0x7D {
                let k = usize::from(c - 0x47) * 94 + usize::from(c2 - 0x21);
                w = uhc::UHC3_UCS_TABLE.get(k).map_or(0, |&x| u32::from(x));
            }

            buf[out] = if w == 0 { BAD_INPUT } else { w };
            out += 1;
        } else if c < 0x80 && *state == ASCII {
            buf[out] = u32::from(c);
            out += 1;
        } else {
            buf[out] = BAD_INPUT;
            out += 1;
        }
    }

    *input = &src[p..];
    out
}

/// `EMITTED_ESC_SEQUENCE` flag in the encoder state.
const EMITTED_ESC_SEQUENCE: u32 = 0x10;

/// `mb_wchar_to_iso2022kr`.
#[allow(clippy::cast_possible_truncation)]
pub fn wchar_to_iso2022kr(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    // The escape sequence is emitted once at the beginning of the output,
    // which is always "the beginning of a line".
    if !input.is_empty() && (buf.state & EMITTED_ESC_SEQUENCE) == 0 {
        buf.extend(&[0x1B, b'$', b')', b'C']);
        buf.state |= EMITTED_ESC_SEQUENCE;
    }

    for &w in input {
        let mut s = ucs_to_uhc(w);

        if ((s >> 8) & 0xFF) < 0xA1 || (s & 0xFF) < 0xA1 {
            s = w;
        } else {
            s -= 0x8080;
        }

        if (0x80..0x2121).contains(&s) || s > 0x8080 {
            illegal_output(w, wchar_to_iso2022kr, buf);
        } else if s < 0x80 {
            if (buf.state & 1) != ASCII {
                buf.push(0xF);
                buf.state &= !KSC5601;
            }
            buf.push(s as u8);
        } else {
            if (buf.state & 1) != KSC5601 {
                buf.push(0xE);
                buf.state |= KSC5601;
            }
            buf.extend(&[((s >> 8) & 0xFF) as u8, (s & 0xFF) as u8]);
        }
    }

    if end && (buf.state & 1) != ASCII {
        buf.push(0xF);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codecs::cjk::testutil::{
        check, code_bytes, data_file, decode, decode_bufsize, encode, encode_split, legacy_decode,
        legacy_encode, mapping_lines, run_filter_with,
    };
    use crate::convert::ErrorMode;

    const E: &Encoding = &ENC_2022KR;

    fn ksx() -> Vec<(Vec<u8>, u32)> {
        let Some(text) = data_file("KSX1001.txt") else {
            return Vec::new();
        };
        mapping_lines(&text)
            .into_iter()
            .filter(|t| t.len() >= 2)
            .map(|t| {
                #[allow(clippy::cast_possible_truncation)]
                (code_bytes(t[0][0]), t[1][0] as u32)
            })
            .collect()
    }

    #[test]
    fn ksx1001_mapping_fast_and_legacy() {
        let m = ksx();
        for (code, uni) in &m {
            let mut bytes = b"\x1B$)C\x0E".to_vec();
            bytes.extend_from_slice(code);
            bytes.push(0x0F);
            assert_eq!(decode(E, &bytes), vec![*uni], "{code:02x?}");
            assert_eq!(legacy_decode(E, &bytes), vec![*uni], "{code:02x?}");
            assert!(check(E, &bytes));
            let (out, errors) = encode(E, &[*uni]);
            assert_eq!(
                (out.as_slice(), errors),
                (bytes.as_slice(), 0),
                "U+{uni:04X}"
            );
            assert_eq!(legacy_encode(E, &[*uni]), bytes, "U+{uni:04X}");
        }
        assert!(m.is_empty() || m.len() > 8000);
    }

    #[test]
    fn ascii_and_shift_codes() {
        assert_eq!(decode(E, b""), Vec::<u32>::new());
        assert_eq!(decode(E, b"\x0Fa"), vec![0x61]);
        assert_eq!(decode(E, b"\x80"), vec![BAD_INPUT]);
        assert_eq!(decode(E, b"\x1B$)Cabc"), vec![0x61, 0x62, 0x63]);
        assert_eq!(decode(E, b"\x1B"), vec![BAD_INPUT]);
        assert_eq!(decode(E, b"\x1B$"), vec![BAD_INPUT]);
        assert_eq!(decode(E, b"\x1B$)"), vec![BAD_INPUT]);
        // Bad escape: only the bytes which could belong to it are consumed
        assert_eq!(decode(E, b"\x1BXYZ"), vec![BAD_INPUT, 0x59, 0x5A]);
        assert_eq!(decode(E, b"\x1B$X)"), vec![BAD_INPUT, 0x29]);
        assert_eq!(decode(E, b"\x1B$)D"), vec![BAD_INPUT]);
        // Truncated double-byte character
        assert_eq!(decode(E, b"\x0E\x30"), vec![BAD_INPUT]);
        assert!(!check(E, b"\x0E\x30"));
    }

    #[test]
    fn encoder_state() {
        assert_eq!(encode(E, &[]).0, b"");
        assert_eq!(encode(E, &[0x41]).0, b"\x1B$)CA");
        assert_eq!(
            encode(E, &[0xAC00, 0x41, 0xAC01]).0,
            b"\x1B$)C\x0E\x30\x21\x0FA\x0E\x30\x22\x0F"
        );
        // State is kept between calls; SI is only emitted at the very end
        assert_eq!(
            encode_split(E, &[0xAC00, 0xAC01]),
            b"\x1B$)C\x0E\x30\x21\x30\x22\x0F"
        );
        let (out, errors) = encode(E, &[0xAC00, 0x1F600]);
        assert_eq!(
            (out.as_slice(), errors),
            (&b"\x1B$)C\x0E\x30\x21\x0F?"[..], 1)
        );
        // Quirk: unmapped code points in 0x2121..=0x8080 are output as-is
        assert_eq!(
            encode(E, &[0x2603]),
            (b"\x1B$)C\x0E\x26\x03\x0F".to_vec(), 0)
        );
    }

    #[test]
    fn legacy_matches_fast_on_valid_text() {
        let text: Vec<u32> = "abc 가나다 漢字 xyz \u{3000}끝"
            .chars()
            .map(u32::from)
            .collect();
        let bytes = encode(E, &text).0;
        assert_eq!(legacy_encode(E, &text), bytes);
        assert_eq!(legacy_decode(E, &bytes), text);
        assert_eq!(decode(E, &bytes), text);
        for bs in [1, 2, 5, 8] {
            assert_eq!(decode_bufsize(E, &bytes, bs), text);
        }
        // Unencodable characters are substituted the same way
        let bad = [0x41, 0x2603, 0xAC00, 0x1F600, 0x42];
        assert_eq!(legacy_encode(E, &bad), encode(E, &bad).0);
    }

    #[test]
    fn legacy_decoder_errors() {
        let b = |s: &[u8]| legacy_decode(E, s);
        assert_eq!(b(b"\x80"), vec![BAD_INPUT]);
        assert_eq!(b(b"\x0E\x80"), vec![BAD_INPUT]);
        // ESC handling: a bad byte aborts the sequence (and is swallowed)
        assert_eq!(b(b"\x1BXa"), vec![BAD_INPUT, 0x61]);
        assert_eq!(b(b"\x1B$Xa"), vec![BAD_INPUT, 0x61]);
        assert_eq!(b(b"\x1B$)Da"), vec![BAD_INPUT, 0x61]);
        // Truncated at the end: flushed as bad input
        assert_eq!(b(b"\x1B$"), vec![BAD_INPUT]);
        assert_eq!(b(b"\x0E\x30"), vec![BAD_INPUT]);
        // Bad second byte
        assert_eq!(b(b"\x0E\x30\x0A"), vec![BAD_INPUT]);
        // 0x49 lead byte is unassigned
        assert_eq!(b(b"\x0E\x49\x21"), vec![BAD_INPUT]);
    }

    #[test]
    fn legacy_encoder_modes() {
        let input = [0x41_i32, 0x1F600];
        let (out, errs) = run_filter_with(&VTBL_WCHAR_2022KR, &input, ErrorMode::Long, 0x3F);
        let s: Vec<u8> = out.iter().map(|&c| u8::try_from(c).unwrap_or(0)).collect();
        assert_eq!(s, b"\x1B$)CAU+1F600");
        assert_eq!(errs, 1);
        let (out, _) = run_filter_with(&VTBL_WCHAR_2022KR, &[0x1F600], ErrorMode::Char, 0xAC00);
        let s: Vec<u8> = out.iter().map(|&c| u8::try_from(c).unwrap_or(0)).collect();
        assert_eq!(s, b"\x1B$)C\x0E\x30\x21\x0F");
    }

    #[test]
    fn oracle_blocks() {
        let n = super::super::euc::blocks::replay(|e| e == "ISO-2022-KR");
        assert!(n > 200, "{n}");
    }
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `EUC-JP`, `eucJP-win`, `CP51932`.
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c` (`mb_eucjp_to_wchar`,
//! `mb_eucjpwin_to_wchar`, `mb_cp51932_to_wchar` and their encoders).

use crate::convert::{illegal_output, ConvertBuf, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo};
use crate::tables::cjk::cp932::CP932EXT3_EUCJP_TABLE;
use crate::tables::cjk::cp932_ext as ext;
use crate::tables::cjk::jis as j;
use crate::tables::cjk::misc::MBLEN_TABLE_EUCJP;

/// `EUC-JP`.
pub static ENC_EUC_JP: Encoding = Encoding {
    no: EncodingNo::EucJp,
    name: "EUC-JP",
    mime_name: Some("EUC-JP"),
    aliases: &["EUC", "EUC_JP", "eucJP", "x-euc-jp"],
    mblen_table: Some(&MBLEN_TABLE_EUCJP),
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: eucjp_to_wchar,
    from_wchar: wchar_to_eucjp,
    check: None,
    cut: None,
};

/// `eucJP-win`.
pub static ENC_EUCJP_WIN: Encoding = Encoding {
    no: EncodingNo::EucJpWin,
    name: "eucJP-win",
    mime_name: Some("EUC-JP"),
    aliases: &["eucJP-open", "eucJP-ms"],
    mblen_table: Some(&MBLEN_TABLE_EUCJP),
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: eucjpwin_to_wchar,
    from_wchar: wchar_to_eucjpwin,
    check: None,
    cut: None,
};

/// `CP51932`.
pub static ENC_CP51932: Encoding = Encoding {
    no: EncodingNo::Cp51932,
    name: "CP51932",
    mime_name: Some("CP51932"),
    aliases: &["cp51932"],
    mblen_table: Some(&MBLEN_TABLE_EUCJP),
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: cp51932_to_wchar,
    from_wchar: wchar_to_cp51932,
    check: None,
    cut: None,
};

#[allow(clippy::cast_sign_loss)]
const fn u(x: i32) -> u32 {
    x as u32
}

const EXT1_MIN: u32 = u(ext::CP932EXT1_UCS_TABLE_MIN);
const EXT1_MAX: u32 = u(ext::CP932EXT1_UCS_TABLE_MAX);
const EXT2_MIN: u32 = u(ext::CP932EXT2_UCS_TABLE_MIN);
const EXT2_MAX: u32 = u(ext::CP932EXT2_UCS_TABLE_MAX);

/// Table value or `BAD_INPUT` for zero.
#[inline]
fn or_bad(w: u32) -> u32 {
    if w == 0 {
        BAD_INPUT
    } else {
        w
    }
}

/// Unicode -> JIS X 0208/0212 via the `ucs_{a1,a2,i,r}_jis_table` tables
/// (JIS X 0212 entries have bit 15 set); 0 if unmapped.
fn ucs_to_jis(w: u32) -> u32 {
    let lookup = |tbl: &[u16], min: i32| -> Option<u32> {
        let min = u(min);
        let idx = w.checked_sub(min)? as usize;
        tbl.get(idx).map(|&s| u32::from(s))
    };
    let r = if w < u(j::UCS_A1_JIS_TABLE_MAX) {
        lookup(&j::UCS_A1_JIS_TABLE, j::UCS_A1_JIS_TABLE_MIN)
    } else if (u(j::UCS_A2_JIS_TABLE_MIN)..u(j::UCS_A2_JIS_TABLE_MAX)).contains(&w) {
        lookup(&j::UCS_A2_JIS_TABLE, j::UCS_A2_JIS_TABLE_MIN)
    } else if (u(j::UCS_I_JIS_TABLE_MIN)..u(j::UCS_I_JIS_TABLE_MAX)).contains(&w) {
        lookup(&j::UCS_I_JIS_TABLE, j::UCS_I_JIS_TABLE_MIN)
    } else if (u(j::UCS_R_JIS_TABLE_MIN)..u(j::UCS_R_JIS_TABLE_MAX)).contains(&w) {
        lookup(&j::UCS_R_JIS_TABLE, j::UCS_R_JIS_TABLE_MIN)
    } else {
        None
    };
    r.unwrap_or(0)
}

/// Emit a JIS code `s` (as produced by [`ucs_to_jis`]) in EUC-JP form.
#[allow(clippy::cast_possible_truncation)]
fn push_euc(buf: &mut ConvertBuf, s: u32) {
    if s < 0x80 {
        buf.push(s as u8);
    } else if s < 0x100 {
        buf.extend(&[0x8E, s as u8]);
    } else if s < 0x8080 {
        buf.extend(&[((s >> 8) & 0xFF) as u8 | 0x80, (s & 0xFF) as u8 | 0x80]);
    } else {
        buf.extend(&[
            0x8F,
            ((s >> 8) & 0xFF) as u8 | 0x80,
            (s & 0xFF) as u8 | 0x80,
        ]);
    }
}

/// `mb_eucjp_to_wchar`.
pub fn eucjp_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let src = *input;
    let end = src.len();
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len();

    while p < end && out < limit {
        let c = src[p];
        p += 1;

        buf[out] = if c < 0x80 {
            u32::from(c)
        } else if (0xA1..=0xFE).contains(&c) && p < end {
            // JIS X 0208
            let c2 = src[p];
            p += 1;
            if (0xA1..=0xFE).contains(&c2) {
                let k = usize::from(c - 0xA1) * 94 + usize::from(c2 - 0xA1);
                j::JISX0208_UCS_TABLE
                    .get(k)
                    .map_or(BAD_INPUT, |&w| or_bad(u32::from(w)))
            } else {
                BAD_INPUT
            }
        } else if c == 0x8E && p < end {
            // Kana
            let c2 = src[p];
            p += 1;
            if (0xA1..=0xDF).contains(&c2) {
                0xFEC0 + u32::from(c2)
            } else {
                BAD_INPUT
            }
        } else if c == 0x8F {
            // JIS X 0212
            if end - p >= 2 {
                let c2 = src[p];
                let c3 = src[p + 1];
                p += 2;
                if (0xA1..=0xFE).contains(&c3) && (0xA1..=0xFE).contains(&c2) {
                    let k = usize::from(c2 - 0xA1) * 94 + usize::from(c3 - 0xA1);
                    j::JISX0212_UCS_TABLE
                        .get(k)
                        .map_or(BAD_INPUT, |&w| or_bad(u32::from(w)))
                } else {
                    BAD_INPUT
                }
            } else {
                p = end; // Jump to end of string
                BAD_INPUT
            }
        } else {
            BAD_INPUT
        };
        out += 1;
    }

    *input = &src[p..];
    out
}

/// `mb_wchar_to_eucjp`.
pub fn wchar_to_eucjp(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        let mut s = if w == 0xAF {
            0xA2B4 // MACRON: use JIS X 0212 overline
        } else {
            ucs_to_jis(w)
        };

        if s == 0 {
            s = match w {
                0xFF3C => 0x2140, // FULLWIDTH REVERSE SOLIDUS
                0x2225 => 0x2142, // PARALLEL TO
                0xFF0D => 0x215D, // FULLWIDTH HYPHEN-MINUS
                0xFFE0 => 0x2171, // FULLWIDTH CENT SIGN
                0xFFE1 => 0x2172, // FULLWIDTH POUND SIGN
                0xFFE2 => 0x224C, // FULLWIDTH NOT SIGN
                0 => {
                    buf.push(0);
                    continue;
                }
                _ => {
                    illegal_output(w, wchar_to_eucjp, buf);
                    continue;
                }
            };
        }

        push_euc(buf, s);
    }
}

/// Shared JIS X 0208 row decoding of `eucJP-win` and `CP51932`: the
/// Microsoft-style replacements for a few NEC/IBM-ambiguous code points.
fn ms_special_0208(s: usize) -> u32 {
    match s {
        31 => 0xFF3C,  // FULLWIDTH REVERSE SOLIDUS
        32 => 0xFF5E,  // FULLWIDTH TILDE
        33 => 0x2225,  // PARALLEL TO
        60 => 0xFF0D,  // FULLWIDTH HYPHEN-MINUS
        80 => 0xFFE0,  // FULLWIDTH CENT SIGN
        81 => 0xFFE1,  // FULLWIDTH POUND SIGN
        137 => 0xFFE2, // FULLWIDTH NOT SIGN
        _ => 0,
    }
}

/// `mb_eucjpwin_to_wchar`.
#[allow(clippy::cast_possible_truncation)]
pub fn eucjpwin_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let src = *input;
    let end = src.len();
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len();

    while p < end && out < limit {
        let c = src[p];
        p += 1;

        if c < 0x80 {
            buf[out] = u32::from(c);
        } else if (0xA1..=0xFE).contains(&c) && p < end {
            let c2 = src[p];
            p += 1;
            buf[out] = if (0xA1..=0xFE).contains(&c2) {
                let k = usize::from(c - 0xA1) * 94 + usize::from(c2 - 0xA1);
                let mut w = ms_special_0208(k);
                if w == 0 {
                    let k32 = k as u32;
                    if (EXT1_MIN..EXT1_MAX).contains(&k32) {
                        w = u32::from(ext::CP932EXT1_UCS_TABLE[(k32 - EXT1_MIN) as usize]);
                    } else if let Some(&x) = j::JISX0208_UCS_TABLE.get(k) {
                        w = u32::from(x);
                    } else if k >= 84 * 94 {
                        w = k32 - 84 * 94 + 0xE000;
                    }
                }
                or_bad(w)
            } else {
                BAD_INPUT
            };
        } else if c == 0x8E && p < end {
            let c2 = src[p];
            p += 1;
            buf[out] = if (0xA1..=0xDF).contains(&c2) {
                0xFEC0 + u32::from(c2)
            } else {
                BAD_INPUT
            };
        } else if c == 0x8F && p < end {
            let c2 = src[p];
            p += 1;
            if p == end {
                buf[out] = BAD_INPUT;
                out += 1;
                continue;
            }
            let c3 = src[p];
            p += 1;

            buf[out] = if (0xA1..=0xFE).contains(&c2) && (0xA1..=0xFE).contains(&c3) {
                let k = usize::from(c2 - 0xA1) * 94 + usize::from(c3 - 0xA1);
                let mut w = 0u32;
                if let Some(&x) = j::JISX0212_UCS_TABLE.get(k) {
                    w = u32::from(x);
                    if w == 0x7E {
                        w = 0xFF5E; // FULLWIDTH TILDE
                    }
                } else if (82 * 94..84 * 94).contains(&k) {
                    let code = (u16::from(c2) << 8) | u16::from(c3);
                    if let Some(i) = CP932EXT3_EUCJP_TABLE.iter().position(|&x| x == code) {
                        w = u32::from(ext::CP932EXT3_UCS_TABLE[i]);
                    }
                } else if k >= 84 * 94 {
                    w = k as u32 - 84 * 94 + 0xE000 + 94 * 10;
                }
                if w == 0xA6 {
                    w = 0xFFE4; // FULLWIDTH BROKEN BAR
                }
                or_bad(w)
            } else {
                BAD_INPUT
            };
        } else {
            buf[out] = BAD_INPUT;
        }
        out += 1;
    }

    *input = &src[p..];
    out
}

/// `mb_wchar_to_eucjpwin`.
#[allow(clippy::cast_possible_truncation)]
pub fn wchar_to_eucjpwin(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        let mut s: u32 = if w == 0 {
            buf.push(0);
            continue;
        } else if w == 0xAF {
            0xA2B4 // MACRON: use JIS X 0212 overline
        } else if w == 0x203E {
            0x7E
        } else if ucs_to_jis_in_range(w) {
            ucs_to_jis(w)
        } else if (0xE000..0xE000 + 10 * 94).contains(&w) {
            let s = w - 0xE000;
            ((s / 94 + 0x75) << 8) + (s % 94) + 0x21
        } else if (0xE000 + 10 * 94..0xE000 + 20 * 94).contains(&w) {
            let s = w - (0xE000 + 10 * 94);
            ((s / 94 + 0xF5) << 8) + (s % 94) + 0xA1
        } else {
            0
        };

        if s == 0xA2F1 {
            s = 0x2D62; // NUMERO SIGN
        }

        if s == 0 {
            s = match w {
                0xA5 => 0x5C,     // YEN SIGN
                0x2014 => 0x213D, // EM DASH
                0xFF3C => 0x2140, // FULLWIDTH REVERSE SOLIDUS
                0x2225 => 0x2142, // PARALLEL TO
                0xFF0D => 0x215D, // FULLWIDTH HYPHEN-MINUS
                0xFFE0 => 0x2171, // FULLWIDTH CENT SIGN
                0xFFE1 => 0x2172, // FULLWIDTH POUND SIGN
                0xFFE2 => 0x224C, // FULLWIDTH NOT SIGN
                _ => {
                    let ext1 = (EXT1_MAX - EXT1_MIN) as usize;
                    let ext3 =
                        u(ext::CP932EXT3_UCS_TABLE_MAX - ext::CP932EXT3_UCS_TABLE_MIN) as usize;
                    if let Some(i) = ext::CP932EXT1_UCS_TABLE[..ext1]
                        .iter()
                        .position(|&x| u32::from(x) == w)
                    {
                        let i = i as u32;
                        (((i / 94) + (EXT1_MIN / 94) + 0x21) << 8) + (i % 94) + 0x21
                    } else if let Some(i) = ext::CP932EXT3_UCS_TABLE[..ext3]
                        .iter()
                        .position(|&x| u32::from(x) == w)
                    {
                        u32::from(CP932EXT3_EUCJP_TABLE[i])
                    } else {
                        0
                    }
                }
            };
        }

        if s == 0 {
            illegal_output(w, wchar_to_eucjpwin, buf);
        } else {
            push_euc(buf, s);
        }
    }
}

/// Whether `w` falls into one of the `ucs_*_jis_table` ranges.
fn ucs_to_jis_in_range(w: u32) -> bool {
    w < u(j::UCS_A1_JIS_TABLE_MAX)
        || (u(j::UCS_A2_JIS_TABLE_MIN)..u(j::UCS_A2_JIS_TABLE_MAX)).contains(&w)
        || (u(j::UCS_I_JIS_TABLE_MIN)..u(j::UCS_I_JIS_TABLE_MAX)).contains(&w)
        || (u(j::UCS_R_JIS_TABLE_MIN)..u(j::UCS_R_JIS_TABLE_MAX)).contains(&w)
}

/// `mb_cp51932_to_wchar`.
#[allow(clippy::cast_possible_truncation)]
pub fn cp51932_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let src = *input;
    let end = src.len();
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len();

    while p < end && out < limit {
        let c = src[p];
        p += 1;

        buf[out] = if c < 0x80 {
            u32::from(c)
        } else if (0xA1..=0xFE).contains(&c) && p < end {
            let c2 = src[p];
            p += 1;
            if (0xA1..=0xFE).contains(&c2) {
                let k = usize::from(c - 0xA1) * 94 + usize::from(c2 - 0xA1);
                let mut w = ms_special_0208(k);
                if w == 0 {
                    let k32 = k as u32;
                    if (EXT1_MIN..EXT1_MAX).contains(&k32) {
                        w = u32::from(ext::CP932EXT1_UCS_TABLE[(k32 - EXT1_MIN) as usize]);
                    } else if let Some(&x) = j::JISX0208_UCS_TABLE.get(k) {
                        w = u32::from(x);
                    } else if (EXT2_MIN..EXT2_MAX).contains(&k32) {
                        w = u32::from(ext::CP932EXT2_UCS_TABLE[(k32 - EXT2_MIN) as usize]);
                    }
                }
                or_bad(w)
            } else {
                BAD_INPUT
            }
        } else if c == 0x8E && p < end {
            let c2 = src[p];
            p += 1;
            if (0xA1..=0xDF).contains(&c2) {
                0xFEC0 + u32::from(c2)
            } else {
                BAD_INPUT
            }
        } else {
            BAD_INPUT
        };
        out += 1;
    }

    *input = &src[p..];
    out
}

/// `mb_wchar_to_cp51932`.
#[allow(clippy::cast_possible_truncation)]
pub fn wchar_to_cp51932(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        if w == 0 {
            buf.push(0);
            continue;
        }
        let mut s = ucs_to_jis(w);
        if s >= 0x8080 {
            s = 0; // We don't support JIS X 0213
        }

        if s == 0 {
            s = match w {
                0xA5 => 0x216F,   // YEN SIGN -> FULLWIDTH YEN SIGN
                0xFF3C => 0x2140, // FULLWIDTH REVERSE SOLIDUS
                0x2225 => 0x2142, // PARALLEL TO
                0xFF0D => 0x215D, // FULLWIDTH HYPHEN-MINUS
                0xFFE0 => 0x2171, // FULLWIDTH CENT SIGN
                0xFFE1 => 0x2172, // FULLWIDTH POUND SIGN
                0xFFE2 => 0x224C, // FULLWIDTH NOT SIGN
                _ => {
                    let ext1 = (EXT1_MAX - EXT1_MIN) as usize;
                    let ext2 = (EXT2_MAX - EXT2_MIN) as usize;
                    if let Some(i) = ext::CP932EXT1_UCS_TABLE[..ext1]
                        .iter()
                        .position(|&x| u32::from(x) == w)
                    {
                        let i = i as u32;
                        ((i / 94 + 0x2D) << 8) + (i % 94) + 0x21
                    } else if let Some(i) = ext::CP932EXT2_UCS_TABLE[..ext2]
                        .iter()
                        .position(|&x| u32::from(x) == w)
                    {
                        let i = i as u32;
                        ((i / 94 + 0x79) << 8) + (i % 94) + 0x21
                    } else {
                        0
                    }
                }
            };
        }

        if s == 0 || s >= 0x8080 {
            illegal_output(w, wchar_to_cp51932, buf);
        } else if s < 0x80 {
            buf.push(s as u8);
        } else if s < 0x100 {
            buf.extend(&[0x8E, s as u8]);
        } else {
            buf.extend(&[((s >> 8) & 0xFF) as u8 | 0x80, (s & 0xFF) as u8 | 0x80]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codecs::cjk::testutil::{
        code_bytes, data_file, decode, decode_bufsize, encode, mapping_lines,
    };

    fn mapping(file: &str) -> Vec<(Vec<u8>, u32)> {
        let Some(text) = data_file(file) else {
            return Vec::new();
        };
        mapping_lines(&text)
            .into_iter()
            .filter(|t| t.len() >= 2 && t[1].len() == 1)
            .map(|t| {
                #[allow(clippy::cast_possible_truncation)]
                (code_bytes(t[0][0]), t[1][0] as u32)
            })
            .collect()
    }

    #[test]
    fn eucjp_mapping() {
        let m = mapping("EUC-JP.txt");
        for (bytes, uni) in &m {
            let mut uni = *uni;
            if bytes == b"\xA1\xC0" {
                uni = 0xFF3C;
            }
            assert_eq!(decode(&ENC_EUC_JP, bytes), vec![uni], "{bytes:02x?}");
            if bytes == b"\x8F\xA2\xB7" {
                continue; // irreversible: same as ASCII '~'
            }
            let (out, errors) = encode(&ENC_EUC_JP, &[uni]);
            assert_eq!(
                (out.as_slice(), errors),
                (bytes.as_slice(), 0),
                "U+{uni:04X}"
            );
        }
        assert!(m.is_empty() || m.len() > 12000);
    }

    #[test]
    fn eucjpwin_mapping() {
        let m = mapping("EUC-JP-MS.txt");
        for (bytes, uni) in &m {
            assert_eq!(decode(&ENC_EUCJP_WIN, bytes), vec![*uni], "{bytes:02x?}");
        }
        assert!(m.is_empty() || m.len() > 12000);
    }

    #[test]
    fn eucjp_phpt_cases() {
        assert_eq!(decode(&ENC_EUC_JP, b"\x8F\xA2\xB7"), vec![0x7E]);
        assert_eq!(decode(&ENC_EUC_JP, b"\x80"), vec![BAD_INPUT]);
        assert_eq!(decode(&ENC_EUC_JP, b"\xFE\xFF"), vec![BAD_INPUT]);
        // Truncated JIS X 0212: the rest of the input is skipped
        assert_eq!(decode(&ENC_EUC_JP, b"\x8F\xA2"), vec![BAD_INPUT]);
        assert_eq!(encode(&ENC_EUC_JP, &[0xAF]).0, b"\x8F\xA2\xB4");
        assert_eq!(encode(&ENC_EUCJP_WIN, &[0x203E]).0, b"~");
        assert_eq!(encode(&ENC_CP51932, &[0xA5]).0, b"\xA1\xEF");
        // CP51932 does not support JIS X 0212
        assert_eq!(encode(&ENC_CP51932, &[0x2F0]).1, 1);
    }

    #[test]
    fn small_buffers() {
        let s = b"\xA4\xA2\x8E\xB1\x8F\xB0\xA1x\xAD\xA1\xF5\xA1\x8F\xF5\xA1\x8F\xF3\xF3\x8F";
        for e in [&ENC_EUC_JP, &ENC_EUCJP_WIN, &ENC_CP51932] {
            let full = decode(e, s);
            for bs in [1, 2, 5, 8] {
                assert_eq!(decode_bufsize(e, s, bs), full, "{} bufsize {bs}", e.name);
            }
        }
    }

    #[test]
    fn oracle_blocks() {
        let n =
            super::super::euc::blocks::replay(|e| matches!(e, "EUC-JP" | "eucJP-win" | "CP51932"));
        assert!(n > 1000, "{n}");
    }
}

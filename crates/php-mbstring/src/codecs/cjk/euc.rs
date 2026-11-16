// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `EUC-CN`, `EUC-TW`, `EUC-KR`, `UHC`.
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c` (`mb_euccn_to_wchar`,
//! `mb_euctw_to_wchar`, `mb_euckr_to_wchar`, `mb_uhc_to_wchar` and their
//! encoders).

use crate::convert::{illegal_output, ConvertBuf, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo};
use crate::tables::cjk::cns11643 as cns;
use crate::tables::cjk::cp936 as gbk;
use crate::tables::cjk::misc::{MBLEN_TABLE_81_TO_FE, MBLEN_TABLE_EUCCN};
use crate::tables::cjk::uhc;

/// `EUC-CN`.
pub static ENC_EUC_CN: Encoding = Encoding {
    no: EncodingNo::EucCn,
    name: "EUC-CN",
    mime_name: Some("CN-GB"),
    aliases: &["CN-GB", "EUC_CN", "eucCN", "x-euc-cn", "gb2312"],
    mblen_table: Some(&MBLEN_TABLE_EUCCN),
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: euccn_to_wchar,
    from_wchar: wchar_to_euccn,
    check: None,
    cut: None,
};

/// `EUC-TW`.
pub static ENC_EUC_TW: Encoding = Encoding {
    no: EncodingNo::EucTw,
    name: "EUC-TW",
    mime_name: Some("EUC-TW"),
    aliases: &["EUC_TW", "eucTW", "x-euc-tw"],
    mblen_table: Some(&MBLEN_TABLE_EUCCN),
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: euctw_to_wchar,
    from_wchar: wchar_to_euctw,
    check: None,
    cut: None,
};

/// `EUC-KR`.
pub static ENC_EUC_KR: Encoding = Encoding {
    no: EncodingNo::EucKr,
    name: "EUC-KR",
    mime_name: Some("EUC-KR"),
    aliases: &["EUC_KR", "eucKR", "x-euc-kr"],
    mblen_table: Some(&MBLEN_TABLE_EUCCN),
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: euckr_to_wchar,
    from_wchar: wchar_to_euckr,
    check: None,
    cut: None,
};

/// `UHC`.
pub static ENC_UHC: Encoding = Encoding {
    no: EncodingNo::Uhc,
    name: "UHC",
    mime_name: Some("UHC"),
    aliases: &["CP949"],
    mblen_table: Some(&MBLEN_TABLE_81_TO_FE),
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: uhc_to_wchar,
    from_wchar: wchar_to_uhc,
    check: None,
    cut: None,
};

/// Look `w` up in a table covering `min..min + tbl.len()`.
#[inline]
#[allow(clippy::cast_sign_loss)]
fn lookup<T: Copy + Into<u32>>(w: u32, tbl: &[T], min: i32) -> Option<u32> {
    let idx = w.checked_sub(min as u32)? as usize;
    tbl.get(idx).map(|&x| x.into())
}

#[inline]
fn or_bad(w: u32) -> u32 {
    if w == 0 {
        BAD_INPUT
    } else {
        w
    }
}

/// `mb_euccn_to_wchar`.
pub fn euccn_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
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
        } else if ((0xA1..=0xA9).contains(&c) || (0xB0..=0xF7).contains(&c)) && p < end {
            let c2 = src[p];
            p += 1;
            if (0xA1..=0xFE).contains(&c2) {
                let w = usize::from(c - 0x81) * 192 + usize::from(c2) - 0x40;
                let w = if w == 0x1864 {
                    0x30FB
                } else if w == 0x186A {
                    0x2015
                } else if (0x1921..=0x192A).contains(&w)
                    || w == 0x1963
                    || (0x1C59..=0x1C7E).contains(&w)
                    || (0x1DBB..=0x1DC4).contains(&w)
                {
                    0
                } else {
                    gbk::CP936_UCS_TABLE.get(w).map_or(0, |&x| u32::from(x))
                };
                or_bad(w)
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

/// `mb_wchar_to_euccn`.
#[allow(clippy::cast_possible_truncation)]
pub fn wchar_to_euccn(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        let mut s = 0u32;

        if let Some(x) = lookup(w, &gbk::UCS_A1_CP936_TABLE, gbk::UCS_A1_CP936_TABLE_MIN) {
            if w != 0xB7 && w != 0x144 && w != 0x148 && w != 0x251 && w != 0x261 {
                s = x;
            }
        } else if let Some(x) = lookup(w, &gbk::UCS_A2_CP936_TABLE, gbk::UCS_A2_CP936_TABLE_MIN) {
            if w == 0x2015 {
                s = 0xA1AA;
            } else if w != 0x2014 && !(0x2170..=0x2179).contains(&w) {
                s = x;
            }
        } else if let Some(x) = lookup(w, &gbk::UCS_A3_CP936_TABLE, gbk::UCS_A3_CP936_TABLE_MIN) {
            s = if w == 0x30FB { 0xA1A4 } else { x };
        } else if let Some(x) = lookup(w, &gbk::UCS_I_CP936_TABLE, gbk::UCS_I_CP936_TABLE_MIN) {
            s = x;
        } else if (0xFF00..0xFFFF).contains(&w) {
            // ucs_hff_cp936_table_min..ucs_hff_cp936_table_max
            if w == 0xFF04 {
                s = 0xA1E7;
            } else if w == 0xFF5E {
                s = 0xA1AB;
            } else if (0xFF01..=0xFF5D).contains(&w) {
                s = w - 0xFF01 + 0xA3A1;
            } else if (0xFFE0..=0xFFE5).contains(&w) {
                s = u32::from(gbk::UCS_HFF_S_CP936_TABLE[(w - 0xFFE0) as usize]);
            }
        }

        // Exclude CP936 extensions
        if ((s >> 8) & 0xFF) < 0xA1 || (s & 0xFF) < 0xA1 {
            s = 0;
        }

        if s == 0 {
            if w < 0x80 {
                buf.push(w as u8);
            } else {
                illegal_output(w, wchar_to_euccn, buf);
            }
        } else if s < 0x80 {
            buf.push(s as u8);
        } else {
            buf.extend(&[((s >> 8) & 0xFF) as u8, (s & 0xFF) as u8]);
        }
    }
}

/// `mb_euctw_to_wchar`.
pub fn euctw_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
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
        } else if ((0xA1..=0xA6).contains(&c) || (0xC2..=0xFD).contains(&c)) && c != 0xC3 && p < end
        {
            let c2 = src[p];
            p += 1;
            buf[out] = if (0xA1..=0xFE).contains(&c2) {
                let w = usize::from(c - 0xA1) * 94 + usize::from(c2 - 0xA1);
                or_bad(
                    cns::CNS11643_1_UCS_TABLE
                        .get(w)
                        .map_or(0, |&x| u32::from(x)),
                )
            } else {
                BAD_INPUT
            };
        } else if c == 0x8E && p < end {
            let c2 = src[p];
            p += 1;
            buf[out] = BAD_INPUT;

            if (c2 == 0xA1 || c2 == 0xA2 || c2 == 0xAE) && p < end {
                // This is actually the CNS-11643 plane minus one
                let plane = c2 - 0xA1;
                let c3 = src[p];
                p += 1;

                if c3 >= 0xA1
                    && ((plane == 0
                        && ((0xA1..=0xA6).contains(&c3) || (0xC2..=0xFD).contains(&c3))
                        && c3 != 0xC3)
                        || (plane == 1 && c3 <= 0xF2)
                        || (plane == 13 && c3 <= 0xE7))
                    && p < end
                {
                    let c4 = src[p];
                    p += 1;

                    if c2 <= 0xAE && c4 > 0xA0 && c4 < 0xFF {
                        let k = usize::from(c3 - 0xA1) * 94 + usize::from(c4 - 0xA1);
                        // A later version of CNS-11643 moved all the characters
                        // in "plane 14" to "plane 3"; we only support the older
                        // version (like iconv from glibc 2.2).
                        let tbl: &[u16] = match plane {
                            0 => &cns::CNS11643_1_UCS_TABLE,
                            1 => &cns::CNS11643_2_UCS_TABLE,
                            _ => &cns::CNS11643_14_UCS_TABLE,
                        };
                        buf[out] = or_bad(tbl.get(k).map_or(0, |&x| u32::from(x)));
                    }
                }
            }
        } else {
            buf[out] = BAD_INPUT;
        }
        out += 1;
    }

    *input = &src[p..];
    out
}

/// `mb_wchar_to_euctw`.
#[allow(clippy::cast_possible_truncation)]
pub fn wchar_to_euctw(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        let s = lookup(
            w,
            &cns::UCS_A1_CNS11643_TABLE,
            cns::UCS_A1_CNS11643_TABLE_MIN,
        )
        .or_else(|| {
            lookup(
                w,
                &cns::UCS_A2_CNS11643_TABLE,
                cns::UCS_A2_CNS11643_TABLE_MIN,
            )
        })
        .or_else(|| {
            lookup(
                w,
                &cns::UCS_A3_CNS11643_TABLE,
                cns::UCS_A3_CNS11643_TABLE_MIN,
            )
        })
        .or_else(|| lookup(w, &cns::UCS_I_CNS11643_TABLE, cns::UCS_I_CNS11643_TABLE_MIN))
        .or_else(|| lookup(w, &cns::UCS_R_CNS11643_TABLE, cns::UCS_R_CNS11643_TABLE_MIN))
        .unwrap_or(0);

        if s == 0 {
            if w == 0 {
                buf.push(0);
            } else {
                illegal_output(w, wchar_to_euctw, buf);
            }
        } else {
            let plane = s >> 16;
            if plane <= 1 {
                if s < 0x80 {
                    buf.push(s as u8);
                } else {
                    buf.extend(&[((s >> 8) & 0xFF) as u8 | 0x80, (s & 0xFF) as u8 | 0x80]);
                }
            } else {
                buf.extend(&[
                    0x8E,
                    (0xA0 + plane) as u8,
                    ((s >> 8) & 0xFF) as u8 | 0x80,
                    (s & 0xFF) as u8 | 0x80,
                ]);
            }
        }
    }
}

/// Unicode -> UHC code via the `ucs_*_uhc_table` tables; 0 if unmapped.
/// (Shared with `ISO-2022-KR`.)
pub(super) fn ucs_to_uhc(w: u32) -> u32 {
    lookup(w, &uhc::UCS_A1_UHC_TABLE, uhc::UCS_A1_UHC_TABLE_MIN)
        .or_else(|| lookup(w, &uhc::UCS_A2_UHC_TABLE, uhc::UCS_A2_UHC_TABLE_MIN))
        .or_else(|| lookup(w, &uhc::UCS_A3_UHC_TABLE, uhc::UCS_A3_UHC_TABLE_MIN))
        .or_else(|| lookup(w, &uhc::UCS_I_UHC_TABLE, uhc::UCS_I_UHC_TABLE_MIN))
        .or_else(|| lookup(w, &uhc::UCS_S_UHC_TABLE, uhc::UCS_S_UHC_TABLE_MIN))
        .or_else(|| lookup(w, &uhc::UCS_R1_UHC_TABLE, uhc::UCS_R1_UHC_TABLE_MIN))
        .or_else(|| lookup(w, &uhc::UCS_R2_UHC_TABLE, uhc::UCS_R2_UHC_TABLE_MIN))
        .unwrap_or(0)
}

/// `mb_euckr_to_wchar`.
pub fn euckr_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
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
        } else if ((0xA1..=0xAC).contains(&c) || (0xB0..=0xFD).contains(&c)) && c != 0xC9 && p < end
        {
            let c2 = src[p];
            p += 1;
            if c2 < 0xA1 || c2 == 0xFF {
                BAD_INPUT
            } else if c <= 0xC6 {
                let w = usize::from(c - 0x81) * 190 + usize::from(c2) - 0x41;
                or_bad(uhc::UHC1_UCS_TABLE.get(w).map_or(0, |&x| u32::from(x)))
            } else {
                let w = usize::from(c - 0xC7) * 94 + usize::from(c2) - 0xA1;
                or_bad(uhc::UHC3_UCS_TABLE.get(w).map_or(0, |&x| u32::from(x)))
            }
        } else {
            BAD_INPUT
        };
        out += 1;
    }

    *input = &src[p..];
    out
}

/// `mb_wchar_to_euckr`.
#[allow(clippy::cast_possible_truncation)]
pub fn wchar_to_euckr(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        let mut s = ucs_to_uhc(w);

        // Exclude UHC extension area (although we are using the UHC tables)
        if ((s >> 8) & 0xFF) < 0xA1 || (s & 0xFF) < 0xA1 {
            s = 0;
        }

        if s == 0 {
            if w < 0x80 {
                buf.push(w as u8);
            } else {
                illegal_output(w, wchar_to_euckr, buf);
            }
        } else if s < 0x80 {
            buf.push(s as u8);
        } else {
            buf.extend(&[((s >> 8) & 0xFF) as u8, (s & 0xFF) as u8]);
        }
    }
}

/// `mb_uhc_to_wchar`.
pub fn uhc_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let src = *input;
    if src.is_empty() {
        return 0;
    }
    // Stop the main loop 1 byte short of the end of the input
    let end = src.len() - 1;
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len();

    while p < end && out < limit {
        let c = src[p];
        p += 1;

        if c < 0x80 {
            buf[out] = u32::from(c);
        } else if c > 0x80 && c < 0xFE {
            // No need to check p < end here, because of the `end - 1` above
            let c2 = src[p];
            p += 1;
            if c2 < 0x41 || c2 == 0xFF {
                buf[out] = BAD_INPUT;
                out += 1;
                continue;
            }
            let mut w = 0u32;
            if c <= 0xC6 {
                let k = usize::from(c - 0x81) * 190 + usize::from(c2 - 0x41);
                w = uhc::UHC1_UCS_TABLE.get(k).map_or(0, |&x| u32::from(x));
            } else if c2 >= 0xA1 {
                let k = usize::from(c - 0xC7) * 94 + usize::from(c2 - 0xA1);
                w = uhc::UHC3_UCS_TABLE.get(k).map_or(0, |&x| u32::from(x));
            }
            if w == 0 {
                // If c == 0xC9, we shouldn't have tried to read a 2-byte char
                // at all; fix up that rare case here.
                if c == 0xC9 {
                    p -= 1;
                }
                w = BAD_INPUT;
            }
            buf[out] = w;
        } else {
            buf[out] = BAD_INPUT;
        }
        out += 1;
    }

    // Finish up last byte of input string if there is one
    if p == end && out < limit {
        let c = src[p];
        p += 1;
        buf[out] = if c < 0x80 { u32::from(c) } else { BAD_INPUT };
        out += 1;
    }

    *input = &src[p..];
    out
}

/// `mb_wchar_to_uhc`.
#[allow(clippy::cast_possible_truncation)]
pub fn wchar_to_uhc(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        let s = ucs_to_uhc(w);

        if s == 0 {
            if w == 0 {
                buf.push(0);
            } else {
                illegal_output(w, wchar_to_uhc, buf);
            }
        } else if s < 0x80 {
            buf.push(s as u8);
        } else {
            buf.extend(&[((s >> 8) & 0xFF) as u8, (s & 0xFF) as u8]);
        }
    }
}

/// Replay of `tests/fixtures/cjk/euc_blocks.tsv` (block-wise CRC32 oracle),
/// shared with the `eucjp` and `iso2022kr` tests.
#[cfg(test)]
pub(super) mod blocks {
    use crate::codecs::cjk::testutil::{enc, fixture_path};
    use crate::convert::{fast_convert, ErrorMode};

    /// Standard CRC-32 (as PHP's `crc32()`).
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    }

    fn spec_input(spec: &str) -> Vec<u8> {
        let p: Vec<&str> = spec.split(':').collect();
        if p[0] == "pairs" {
            let (pre, suf) = (unhex(p[1]), unhex(p[2]));
            let mut s = Vec::new();
            for b in 0..=255u8 {
                s.extend_from_slice(&pre);
                s.push(b);
                s.extend_from_slice(&suf);
            }
            s
        } else {
            let start = u32::from_str_radix(p[1], 16).expect("start");
            let count: u32 = p[2].parse().expect("count");
            let mut s = String::new();
            for cp in start..start + count {
                if let Some(ch) = char::from_u32(cp) {
                    s.push(ch);
                }
            }
            s.into_bytes()
        }
    }

    /// Replay every block whose source or target encoding satisfies `want`;
    /// returns the number of blocks checked.
    pub fn replay(want: impl Fn(&str) -> bool) -> usize {
        let text = std::fs::read_to_string(fixture_path("euc_blocks.tsv")).expect("fixture");
        let mut failures = Vec::new();
        let mut n = 0;
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f[0] != "blk" || !(want(f[1]) || want(f[2])) {
                continue;
            }
            n += 1;
            let (mode, subst) = match f[3] {
                "none" => (ErrorMode::None, 0x3F),
                "long" => (ErrorMode::Long, 0x3F),
                "entity" => (ErrorMode::Entity, 0x3F),
                h => (ErrorMode::Char, u32::from_str_radix(h, 16).expect("mode")),
            };
            let input = spec_input(f[4]);
            let (out, errors) = fast_convert(&input, enc(f[1]), enc(f[2]), subst, mode);
            let got = format!("{:08x}\t{}\t{}", crc32(&out), out.len(), errors);
            let expected = format!("{}\t{}\t{}", f[5], f[6], f[7]);
            if got != expected {
                failures.push(format!("{line}\n    got {got}"));
            }
        }
        assert!(
            failures.is_empty(),
            "{} of {n} blocks failed:\n{}",
            failures.len(),
            failures[..failures.len().min(30)].join("\n")
        );
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codecs::cjk::testutil::{
        check, code_bytes, data_file, decode, decode_bufsize, encode, mapping_lines, replay_oracle,
    };

    /// Decode every character of a mapping file and encode it back.
    fn roundtrip_mapping(file: &str, e: &'static Encoding, skip_fffd: bool) -> usize {
        let Some(text) = data_file(file) else {
            return 0;
        };
        let mut n = 0;
        for toks in mapping_lines(&text) {
            if toks.len() < 2 || toks[1].len() != 1 {
                continue;
            }
            let (code, uni) = (toks[0][0], toks[1][0]);
            if skip_fffd && uni == 0xFFFD {
                continue;
            }
            let bytes = code_bytes(code);
            #[allow(clippy::cast_possible_truncation)]
            let uni = uni as u32;
            assert_eq!(decode(e, &bytes), vec![uni], "{} decode {code:#x}", e.name);
            let (enc_bytes, errors) = encode(e, &[uni]);
            assert_eq!(errors, 0, "{} encode U+{uni:04X}", e.name);
            assert_eq!(enc_bytes, bytes, "{} encode U+{uni:04X}", e.name);
            n += 1;
        }
        n
    }

    #[test]
    fn euccn_mapping() {
        let n = roundtrip_mapping("EUC-CN.txt", &ENC_EUC_CN, true);
        assert!(n == 0 || n > 7000, "{n}");
    }

    #[test]
    fn euckr_mapping() {
        let n = roundtrip_mapping("EUC-KR.txt", &ENC_EUC_KR, true);
        assert!(n == 0 || n > 8000, "{n}");
    }

    #[test]
    fn uhc_mapping() {
        let n = roundtrip_mapping("CP949.txt", &ENC_UHC, true);
        assert!(n == 0 || n > 17000, "{n}");
    }

    #[test]
    fn euctw_mapping_decode() {
        let Some(text) = data_file("EUC-TW.txt") else {
            return;
        };
        let mut n = 0;
        for toks in mapping_lines(&text) {
            if toks.len() < 2 {
                continue;
            }
            let bytes = code_bytes(toks[0][0]);
            #[allow(clippy::cast_possible_truncation)]
            let uni = toks[1][0] as u32;
            assert_eq!(decode(&ENC_EUC_TW, &bytes), vec![uni], "{:#x}", toks[0][0]);
            n += 1;
        }
        assert!(n > 20000, "{n}");
    }

    #[test]
    fn phpt_error_cases() {
        // euc_tw_encoding.phpt
        for bad in [
            &b"\x80"[..],
            b"\x8E\x20",
            b"\x8E\xA1\x20",
            b"\x8E\xA1\xA1\x20",
            b"\x8E\xA2\xA3\x20",
            b"\x8F",
            b"\xA1\x50",
            b"\xFD\xCC",
        ] {
            assert!(decode(&ENC_EUC_TW, bad).contains(&BAD_INPUT));
            assert!(!check(&ENC_EUC_TW, bad));
        }
        // uhc_encoding.phpt
        assert_eq!(decode(&ENC_UHC, b"\xE4\xA4\xB4<"), vec![0x751A, BAD_INPUT]);
        assert_eq!(decode(&ENC_UHC, b"\xC9\x9E\x98"), vec![BAD_INPUT, 0xC612]);
        assert_eq!(decode(&ENC_EUC_KR, b"\xA7\xF0"), vec![BAD_INPUT]);
        assert_eq!(decode(&ENC_EUC_CN, b"\xF7\xFF"), vec![BAD_INPUT]);
        assert_eq!(decode(&ENC_EUC_CN, b"\xA1\x50"), vec![BAD_INPUT]);
    }

    #[test]
    fn small_buffers() {
        let s = b"\xB0\xA1x\xC8\xFE\x81\x41\xC7\xA1zz\xFE";
        for e in [&ENC_UHC, &ENC_EUC_KR, &ENC_EUC_CN, &ENC_EUC_TW] {
            let full = decode(e, s);
            for bs in [1, 2, 5, 8] {
                assert_eq!(decode_bufsize(e, s, bs), full, "{} bufsize {bs}", e.name);
            }
        }
    }

    #[test]
    fn oracle_conversions() {
        assert!(replay_oracle("euc_oracle.tsv") > 1000);
    }

    #[test]
    fn oracle_blocks() {
        let n = blocks::replay(|e| matches!(e, "EUC-CN" | "EUC-TW" | "EUC-KR" | "UHC"));
        assert!(n > 1000, "{n}");
    }
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `BIG-5`, `CP950`.
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c` (section "BIG5/CP950").

use crate::convert::{illegal_output, ConvertBuf, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};
use crate::tables::cjk::big5 as t;
use crate::tables::cjk::misc::MBLEN_TABLE_81_TO_FE;

/// `BIG-5`.
pub static ENC_BIG5: Encoding = Encoding {
    no: EncodingNo::Big5,
    name: "BIG-5",
    mime_name: Some("BIG5"),
    aliases: &["CN-BIG5", "BIG-FIVE", "BIGFIVE"],
    mblen_table: Some(&MBLEN_TABLE_81_TO_FE),
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: big5_to_wchar,
    from_wchar: wchar_to_big5,
    check: None,
    cut: None,
};

/// `CP950`.
pub static ENC_CP950: Encoding = Encoding {
    no: EncodingNo::Cp950,
    name: "CP950",
    mime_name: Some("BIG5"),
    aliases: &[],
    mblen_table: Some(&MBLEN_TABLE_81_TO_FE),
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: cp950_to_wchar,
    from_wchar: wchar_to_cp950,
    check: None,
    cut: None,
};

// `cp950_pua_tbl`: `[first PUA code point, last PUA code point, first
// CP950 code, last CP950 code]` (63 + 94 = 157 or 94 codes per row).
use crate::tables::cjk::misc::CP950_PUA_TBL;

/// `is_in_cp950_pua`.
fn is_in_cp950_pua(c1: u8, c: u8) -> bool {
    if (0xFA..=0xFE).contains(&c1)
        || (0x8E..=0xA0).contains(&c1)
        || (0x81..=0x8D).contains(&c1)
        || (0xC7..=0xC8).contains(&c1)
    {
        (0x40..=0x7E).contains(&c) || (0xA1..=0xFE).contains(&c)
    } else if c1 == 0xC6 {
        (0xA1..=0xFE).contains(&c)
    } else {
        false
    }
}

/// Index into `big5_ucs_table` for a lead byte `c` and trail byte `c2`
/// (C `int` arithmetic; negative for lead bytes below 0xA1).
#[inline]
fn big5_index(c: u8, c2: u8) -> i32 {
    (i32::from(c) - 0xA1) * 157 + i32::from(c2) - if c2 <= 0x7E { 0x40 } else { 0xA1 - 0x3F }
}

/// `big5_ucs_table[idx]`, or 0 when out of range.
#[inline]
fn big5_ucs(idx: i32) -> u32 {
    usize::try_from(idx)
        .ok()
        .and_then(|i| t::BIG5_UCS_TABLE.get(i))
        .map_or(0, |&v| u32::from(v))
}

/// `mb_big5_to_wchar`.
#[allow(clippy::many_single_char_names)]
pub fn big5_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let s = *input;
    if s.is_empty() {
        return 0;
    }
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len();
    /* Stop the main loop 1 byte short of the end of the input */
    let e = s.len() - 1;

    while p < e && out < limit {
        let c = s[p];
        p += 1;

        buf[out] = if c <= 0x7F {
            u32::from(c)
        } else if c > 0xA0 && c <= 0xF9 {
            let c2 = s[p];
            p += 1;
            if (0x40..=0x7E).contains(&c2) || (0xA1..=0xFE).contains(&c2) {
                let w = big5_ucs(big5_index(c, c2));
                if w == 0 {
                    if c == 0xC8 {
                        p -= 1;
                    }
                    BAD_INPUT
                } else {
                    w
                }
            } else {
                BAD_INPUT
            }
        } else {
            BAD_INPUT
        };
        out += 1;
    }

    /* Finish up last byte of input string if there is one */
    if p == e && out < limit {
        let c = s[p];
        p += 1;
        buf[out] = if c <= 0x7F { u32::from(c) } else { BAD_INPUT };
        out += 1;
    }

    *input = &s[p..];
    out
}

/// Shared table lookup of the BIG5 and CP950 encoders.
fn big5_lookup(w: u32) -> u32 {
    #[allow(clippy::cast_sign_loss)]
    let ranges: [(&[u16], i32, i32); 6] = [
        (
            &t::UCS_A1_BIG5_TABLE,
            t::UCS_A1_BIG5_TABLE_MIN,
            t::UCS_A1_BIG5_TABLE_MAX,
        ),
        (
            &t::UCS_A2_BIG5_TABLE,
            t::UCS_A2_BIG5_TABLE_MIN,
            t::UCS_A2_BIG5_TABLE_MAX,
        ),
        (
            &t::UCS_A3_BIG5_TABLE,
            t::UCS_A3_BIG5_TABLE_MIN,
            t::UCS_A3_BIG5_TABLE_MAX,
        ),
        (
            &t::UCS_I_BIG5_TABLE,
            t::UCS_I_BIG5_TABLE_MIN,
            t::UCS_I_BIG5_TABLE_MAX,
        ),
        (
            &t::UCS_R1_BIG5_TABLE,
            t::UCS_R1_BIG5_TABLE_MIN,
            t::UCS_R1_BIG5_TABLE_MAX,
        ),
        (
            &t::UCS_R2_BIG5_TABLE,
            t::UCS_R2_BIG5_TABLE_MIN,
            t::UCS_R2_BIG5_TABLE_MAX,
        ),
    ];
    for (tbl, min, max) in ranges {
        #[allow(clippy::cast_sign_loss)]
        let (min, max) = (min as u32, max as u32);
        if w >= min && w < max {
            return tbl.get((w - min) as usize).map_or(0, |&v| u32::from(v));
        }
    }
    0
}

/// Emit `s` (or report `w` as unencodable when `s == 0`).
#[allow(clippy::cast_possible_truncation)]
#[inline]
fn emit_big5(w: u32, s: u32, buf: &mut ConvertBuf) {
    if s == 0 {
        if w == 0 {
            buf.push(0);
        } else {
            illegal_output(w, wchar_to_big5, buf);
        }
    } else if s <= 0x80 {
        buf.push(s as u8);
    } else {
        buf.extend(&[(s >> 8) as u8, s as u8]);
    }
}

/// `mb_wchar_to_big5`.
pub fn wchar_to_big5(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        emit_big5(w, big5_lookup(w), buf);
    }
}

/// `mb_cp950_to_wchar`.
#[allow(clippy::many_single_char_names)]
pub fn cp950_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let s = *input;
    let e = s.len();
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len();

    while p < e && out < limit {
        let c = s[p];
        p += 1;

        buf[out] = if c <= 0x7F {
            u32::from(c)
        } else if c > 0x80 && c <= 0xFE && p < e {
            let c2 = s[p];
            p += 1;

            if (0x40..=0x7E).contains(&c2) || (0xA1..=0xFE).contains(&c2) {
                let mut w = big5_ucs(big5_index(c, c2));

                if is_in_cp950_pua(c, c2) {
                    /* PUA for CP950 */
                    let sc = (u32::from(c) << 8) | u32::from(c2);
                    if let Some(row) = CP950_PUA_TBL
                        .iter()
                        .find(|r| sc >= u32::from(r[2]) && sc <= u32::from(r[3]))
                    {
                        w = if (row[2] & 0xFF) == 0x40 {
                            157 * (u32::from(c) - u32::from(row[2] >> 8)) + u32::from(c2)
                                - if c2 >= 0xA1 { 0x62 } else { 0x40 }
                                + u32::from(row[0])
                        } else {
                            sc - u32::from(row[2]) + u32::from(row[0])
                        };
                    }
                } else if c == 0xA1 {
                    w = match c2 {
                        0x45 => 0x2027,
                        0x4E => 0xFE51,
                        0x5A => 0x2574,
                        0xC2 => 0x00AF,
                        0xC3 => 0xFFE3,
                        0xC5 => 0x02CD,
                        0xE3 => 0xFF5E,
                        0xF2 => 0x2295,
                        0xF3 => 0x2299,
                        0xFE => 0xFF0F,
                        _ => w,
                    };
                } else if c == 0xA2 {
                    w = match c2 {
                        0x40 => 0xFF3C,
                        0x41 => 0x2215,
                        0x42 => 0xFE68,
                        0x46 => 0xFFE0,
                        0x47 => 0xFFE1,
                        0xCC => 0x5341,
                        0xCE => 0x5345,
                        _ => w,
                    };
                }

                if w == 0 {
                    BAD_INPUT
                } else {
                    w
                }
            } else {
                BAD_INPUT
            }
        } else {
            BAD_INPUT
        };
        out += 1;
    }

    *input = &s[p..];
    out
}

/// `mb_wchar_to_cp950`. Errors are reported through `mb_wchar_to_big5`, as
/// in the C code.
pub fn wchar_to_cp950(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        let mut s = big5_lookup(w);

        if (0xE000..=0xF848).contains(&w) {
            if let Some(row) = CP950_PUA_TBL.iter().find(|r| w <= u32::from(r[1])) {
                let mut c1 = w - u32::from(row[0]);
                if (row[2] & 0xFF) == 0x40 {
                    let c2 = u32::from(row[2] >> 8);
                    s = ((c1 / 157) + c2) << 8;
                    c1 %= 157;
                    s |= c1 + if c1 >= 0x3F { 0x62 } else { 0x40 };
                } else {
                    s = c1 + u32::from(row[2]);
                }
            }
        } else if w == 0xA2
            || w == 0xA3
            || w == 0x401
            || (0x414..=0x41C).contains(&w)
            || (0x423..=0x44F).contains(&w)
            || w == 0x451
            || w == 0x2022
            || w == 0x203E
            || w == 0x223C
            || (0x2460..=0x247D).contains(&w)
            || w == 0x2609
            || w == 0x2641
            || w == 0x3005
            || (0x302A..=0x30FF).contains(&w)
            || w == 0xFF64
        {
            s = 0;
        } else {
            s = match w {
                0xAF => 0xA1C2,
                0x2CD => 0xA1C5,
                0x2027 => 0xA145,
                0x2215 => 0xA241,
                0x2295 => 0xA1F2,
                0x2299 => 0xA1F3,
                0x2574 => 0xA15A,
                0xFE51 => 0xA14E,
                0xFE68 => 0xA242,
                0xFF3C => 0xA240,
                0xFF5E => 0xA1E3,
                0xFFE0 => 0xA246,
                0xFFE1 => 0xA247,
                0xFFE3 => 0xA1C3,
                0xFF0F => 0xA1FE,
                _ => s,
            };
        }

        emit_big5(w, s, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codecs::cjk::gb18030::phpt;
    use crate::codecs::cjk::testutil::{decode, decode_bufsize, encode, encode_split};

    /// Non-reversible mappings (two different BIG5 codepoints map to the same
    /// Unicode codepoint), from `big5_encoding.phpt`.
    const NON_REVERSIBLE: [(u32, [u8; 2]); 8] = [
        (0x2550, [0xA2, 0xA4]),
        (0x255E, [0xA2, 0xA5]),
        (0x2561, [0xA2, 0xA7]),
        (0x256A, [0xA2, 0xA6]),
        (0x256D, [0xA2, 0x7E]),
        (0x256E, [0xA2, 0xA1]),
        (0x256F, [0xA2, 0xA3]),
        (0x2570, [0xA2, 0xA2]),
    ];

    #[test]
    fn big5_mapping_file() {
        let Some((to, mut from)) = phpt::read_table("BIG5.txt", false) else {
            return;
        };
        for (cp, b) in NON_REVERSIBLE {
            from.insert(cp, b.to_vec());
        }
        phpt::test_valid_decode(&ENC_BIG5, &to);
        phpt::test_invalid_decode(&ENC_BIG5, &to, &|_| false);
        phpt::test_valid_encode(&ENC_BIG5, &from);
        phpt::test_invalid_encode(&ENC_BIG5, &from);
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn cp950_mapping_file() {
        let Some((mut to, mut from)) = phpt::read_table("CP950.txt", false) else {
            return;
        };
        for (cp, b) in NON_REVERSIBLE {
            from.insert(cp, b.to_vec());
        }
        let mut add_pua = |hi: u32, lo: u32, unicode: u32| {
            let b = vec![hi as u8, lo as u8];
            from.insert(unicode, b.clone());
            to.insert(b, unicode);
        };
        let rows: [(std::ops::RangeInclusive<u32>, u32, u32, bool); 4] = [
            (0x81..=0x8D, 0xEEB8, 0x81, true),
            (0x8E..=0xA0, 0xE311, 0x8E, true),
            (0xC7..=0xC8, 0xF672, 0xC6, true),
            (0xFA..=0xFE, 0xE000, 0xFA, true),
        ];
        for (his, base, first, _) in rows {
            for hi in his {
                for lo in 0x40..=0x7E {
                    add_pua(hi, lo, base + 157 * (hi - first) + (lo - 0x40));
                }
                for lo in 0xA1..=0xFE {
                    add_pua(hi, lo, base + 157 * (hi - first) + (lo - 0x62));
                }
            }
        }
        for lo in 0xA1..=0xFE {
            add_pua(0xC6, lo, 0xF672 + (lo - 0x62));
        }
        phpt::test_valid_decode(&ENC_CP950, &to);
        phpt::test_invalid_decode(&ENC_CP950, &to, &|_| false);
        phpt::test_valid_encode(&ENC_CP950, &from);
        phpt::test_invalid_encode(&ENC_CP950, &from);
    }

    #[test]
    fn quirks() {
        // 0xC8 lead with an unmapped trail: the trail byte is reprocessed
        assert_eq!(decode(&ENC_BIG5, b"\xC8\x41A"), vec![BAD_INPUT, 0x41, 0x41]);
        assert_eq!(decode(&ENC_BIG5, b"\xB0\x9F"), vec![BAD_INPUT]);
        assert_eq!(decode(&ENC_BIG5, b"A\xA1"), vec![0x41, BAD_INPUT]);
        assert_eq!(decode(&ENC_CP950, b"\xA1\x45"), vec![0x2027]);
        assert_eq!(
            decode(&ENC_CP950, b"\xF9\xFE"),
            decode(&ENC_BIG5, b"\xF9\xFE")
        );
        // CP950 reports errors through the BIG5 encoder
        assert_eq!(encode(&ENC_CP950, &[0x2609]), (b"?".to_vec(), 1));
        assert_eq!(encode(&ENC_BIG5, &[0]), (vec![0], 0));
    }

    #[test]
    fn small_buffers_and_split_encoding() {
        let input = b"a\xA4\x40\xC8\x41\xF9\xFE\x80\xA1\x45\xFE\xFEz\xA1".to_vec();
        for e in [&ENC_BIG5, &ENC_CP950] {
            let full = decode(e, &input);
            for size in [1, 2, 5, 8] {
                assert_eq!(
                    decode_bufsize(e, &input, size),
                    full,
                    "{} bufsize {size}",
                    e.name
                );
            }
            let cps = [0x41, 0x4E00, 0x2550, 0xE000, 0x2027, 0];
            assert_eq!(encode_split(e, &cps), encode(e, &cps).0);
        }
    }

    #[test]
    fn detail_oracle() {
        assert!(crate::codecs::cjk::testutil::replay_oracle("big5_oracle.tsv") > 500);
    }

    #[test]
    fn exhaustive_oracle() {
        assert!(crate::codecs::cjk::gb18030::exhaustive::replay("big5_exhaustive.tsv") > 500);
    }
}

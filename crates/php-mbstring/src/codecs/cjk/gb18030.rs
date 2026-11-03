//! `GB18030`, `GB18030-2022`, `CP936`.
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c` (section "GB18030/CP936").

use crate::convert::{illegal_output, ConvertBuf, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};
use crate::tables::cjk::cp936 as t;
use crate::tables::cjk::gb18030 as g;
use crate::tables::cjk::misc::{GB18030_2022_PUA_TBL3, GB18030_PUA_TBL3, MBLEN_TABLE_81_TO_FE};

/// `GB18030`.
pub static ENC_GB18030: Encoding = Encoding {
    no: EncodingNo::Gb18030,
    name: "GB18030",
    mime_name: Some("GB18030"),
    aliases: &["gb-18030", "gb-18030-2000"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: gb18030_to_wchar,
    from_wchar: wchar_to_gb18030,
    check: None,
    cut: Some(cut_gb18030),
};

/// `GB18030-2022`.
pub static ENC_GB18030_2022: Encoding = Encoding {
    no: EncodingNo::Gb18030_2022,
    name: "GB18030-2022",
    mime_name: Some("GB18030-2022"),
    aliases: &[],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: gb18030_2022_to_wchar,
    from_wchar: wchar_to_gb18030_2022,
    check: None,
    cut: Some(cut_gb18030),
};

/// `CP936`.
pub static ENC_CP936: Encoding = Encoding {
    no: EncodingNo::Cp936,
    name: "CP936",
    mime_name: Some("CP936"),
    aliases: &["CP-936", "GBK"],
    mblen_table: Some(&MBLEN_TABLE_81_TO_FE),
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: cp936_to_wchar,
    from_wchar: wchar_to_cp936,
    check: None,
    cut: None,
};

#[allow(clippy::cast_sign_loss)]
const fn umin(v: i32) -> u32 {
    v as u32
}

/// Look `w` up in a table covering `[min, max)` (the `ucs_*_cp936_table`s).
#[inline]
fn range_lookup(w: u32, tbl: &[u16], min: i32, max: i32) -> Option<u32> {
    if w >= umin(min) && w < umin(max) {
        Some(tbl.get((w - umin(min)) as usize).map_or(0, |&s| u32::from(s)))
    } else {
        None
    }
}

/// `cp936_ucs_table[w]`, `BAD_INPUT` if out of range (C asserts in range).
#[inline]
fn cp936_ucs(w: u32) -> u32 {
    t::CP936_UCS_TABLE
        .get(w as usize)
        .map_or(BAD_INPUT, |&v| u32::from(v))
}

/// The 4-byte GB18030 BMP code with linear index `w` (`<= 39419`).
#[inline]
fn gb4_bmp(w: u32) -> u32 {
    crate::codecs::bisec_srch(i64::from(w), &g::MBFL_GB2UNI_TBL)
        .map_or(BAD_INPUT, |i| w + u32::from(g::MBFL_GB_UNI_OFST[i]))
}

/// Shared decoder for GB18030 (2000/2005) and GB18030-2022
/// (`mb_gb18030_to_wchar` / `mb_gb18030_2022_to_wchar`).
#[allow(clippy::too_many_lines)]
fn gb18030_decode(input: &mut &[u8], buf: &mut [u32], is2022: bool) -> usize {
    let s = *input;
    let e = s.len();
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len();

    while p < e && out < limit {
        let c = s[p];
        p += 1;

        let w = if c < 0x80 {
            u32::from(c)
        } else if c == 0x80 || c == 0xFF {
            BAD_INPUT
        } else {
            if p == e {
                buf[out] = BAD_INPUT;
                out += 1;
                break;
            }
            let c2 = s[p];
            p += 1;

            if ((0x81..=0x84).contains(&c) || (0x90..=0xE3).contains(&c))
                && (0x30..=0x39).contains(&c2)
            {
                if p >= e {
                    buf[out] = BAD_INPUT;
                    out += 1;
                    break;
                }
                let c3 = s[p];
                p += 1;

                if (0x81..=0xFE).contains(&c3) && p < e {
                    let c4 = s[p];
                    p += 1;

                    if (0x30..=0x39).contains(&c4) {
                        let (c, c2, c3, c4) =
                            (u32::from(c), u32::from(c2), u32::from(c3), u32::from(c4));
                        if (0x90..=0xE3).contains(&c) {
                            let w = (((c - 0x90) * 10 + (c2 - 0x30)) * 126 + (c3 - 0x81)) * 10
                                + (c4 - 0x30)
                                + 0x10000;
                            if w > 0x10_FFFF {
                                BAD_INPUT
                            } else {
                                w
                            }
                        } else {
                            /* Unicode BMP */
                            let w = (((c - 0x81) * 10 + (c2 - 0x30)) * 126 + (c3 - 0x81)) * 10
                                + (c4 - 0x30);
                            if is2022 {
                                match w {
                                    0x98A4 => 0xE78D,
                                    0x98A6 => 0xE78E,
                                    0x98A5 => 0xE78F,
                                    0x98A7..=0x98AD => w + (0xE790 - 0x98A7),
                                    0x1D21 => 0xE7C7,
                                    0x4A71 => 0xE81E,
                                    0x4A72 => 0xE826,
                                    0x4A73..=0x4A74 => w + (0xE82B - 0x4A73),
                                    0x4A75 => 0xE832,
                                    0x4A76 => 0xE843,
                                    0x4A77 => 0xE854,
                                    0x4A78 => 0xE864,
                                    _ if w <= 0x99FB => gb4_bmp(w),
                                    _ => BAD_INPUT,
                                }
                            } else if w <= 39419 {
                                gb4_bmp(w)
                            } else {
                                BAD_INPUT
                            }
                        }
                    } else {
                        BAD_INPUT
                    }
                } else {
                    BAD_INPUT
                }
            } else if ((0xAA..=0xAF).contains(&c) || (0xF8..=0xFE).contains(&c))
                && (0xA1..=0xFE).contains(&c2)
            {
                /* UDA part 1, 2: U+E000-U+E4C5 */
                let row = if c >= 0xF8 { c - 0xF2 } else { c - 0xAA };
                94 * u32::from(row) + u32::from(c2 - 0xA1) + 0xE000
            } else if (0xA1..=0xA7).contains(&c) && (0x40..0xA1).contains(&c2) && c2 != 0x7F {
                /* UDA part 3: U+E4C6-U+E765 */
                96 * u32::from(c - 0xA1) + u32::from(c2) - if c2 >= 0x80 { 0x41 } else { 0x40 }
                    + 0xE4C6
            } else if c2 >= 0x40 && c2 != 0x7F && c2 != 0xFF {
                let w = u32::from(c - 0x81) * 192 + u32::from(c2) - 0x40;

                if w >= 0x192B {
                    if w <= 0x1EBE {
                        if w != 0x1963 && w != 0x1DBF && !(0x1E49..=0x1E55).contains(&w) && w != 0x1E7F
                        {
                            let tbl1: &[u16] = if is2022 {
                                &g::GB18030_2022_PUA_TBL1
                            } else {
                                &t::CP936_PUA_TBL1
                            };
                            buf[out] = u32::from(tbl1[(w - 0x192B) as usize]);
                            out += 1;
                            continue;
                        }
                    } else if w >= 0x413A {
                        if w <= 0x413E {
                            buf[out] = u32::from(t::CP936_PUA_TBL2[(w - 0x413A) as usize]);
                            out += 1;
                            continue;
                        } else if (0x5DD0..=0x5E20).contains(&w) {
                            let tbl3 = if is2022 {
                                &GB18030_2022_PUA_TBL3
                            } else {
                                &GB18030_PUA_TBL3
                            };
                            let c = tbl3[(w - 0x5DD0) as usize];
                            if c != 0 {
                                buf[out] = u32::from(c);
                                out += 1;
                                continue;
                            }
                        }
                    }
                }

                if (0x81..=0xA9).contains(&c)
                    || ((0xB0..=0xF7).contains(&c) && c2 >= 0xA1)
                    || ((0xAA..=0xFE).contains(&c) && c2 <= 0xA0)
                {
                    cp936_ucs(w)
                } else {
                    BAD_INPUT
                }
            } else {
                BAD_INPUT
            }
        };
        buf[out] = w;
        out += 1;
    }

    *input = &s[p..];
    out
}

/// `mb_gb18030_to_wchar`.
pub fn gb18030_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    gb18030_decode(input, buf, false)
}

/// `mb_gb18030_2022_to_wchar`.
pub fn gb18030_2022_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    gb18030_decode(input, buf, true)
}

/// Pack a linear 4-byte GB18030 index into its bytes (`s` as a `u32`),
/// first byte `base` + quotient.
#[inline]
fn gb4_pack(mut c1: u32, base: u32) -> u32 {
    let mut s = (c1 % 10) + 0x30;
    c1 /= 10;
    s |= ((c1 % 126) + 0x81) << 8;
    c1 /= 126;
    s |= ((c1 % 10) + 0x30) << 16;
    c1 /= 10;
    s |= (c1 + base) << 24;
    s
}

/// PUA U+E000-U+E864 (shared by GB18030 and CP936 encoders).
fn pua_to_gb(w: u32, pua_tbl: &[[u16; 3]]) -> u32 {
    if w < 0xE766 {
        if w < 0xE4C6 {
            let mut c1 = w - 0xE000;
            let mut s = (c1 % 94) + 0xA1;
            c1 /= 94;
            s |= (c1 + if c1 < 0x06 { 0xAA } else { 0xF2 }) << 8;
            s
        } else {
            let mut c1 = w - 0xE4C6;
            let mut s = ((c1 / 96) + 0xA1) << 8;
            c1 %= 96;
            s |= c1 + if c1 >= 0x3F { 0x41 } else { 0x40 };
            s
        }
    } else {
        /* U+E766-U+E864 */
        let (mut k1, mut k2) = (0usize, pua_tbl.len());
        while k1 < k2 {
            let k = (k1 + k2) >> 1;
            if w < u32::from(pua_tbl[k][0]) {
                k2 = k;
            } else if w > u32::from(pua_tbl[k][1]) {
                k1 = k + 1;
            } else {
                return w - u32::from(pua_tbl[k][0]) + u32::from(pua_tbl[k][2]);
            }
        }
        0
    }
}

/// U+FF00-U+FFFF halfwidth/fullwidth forms (shared lookup).
fn hff_to_gb(w: u32) -> u32 {
    match w {
        0xFF04 => 0xA1E7,
        0xFF5E => 0xA1AB,
        0xFF01..=0xFF5D => w - 0xFF01 + 0xA3A1,
        0xFFE0..=0xFFE5 => u32::from(t::UCS_HFF_S_CP936_TABLE[(w - 0xFFE0) as usize]),
        _ => 0,
    }
}

/// Shared encoder for GB18030 and GB18030-2022 (`mb_wchar_to_gb18030` /
/// `mb_wchar_to_gb18030_2022`). Both report errors through
/// `mb_wchar_to_gb18030`, as in the C code.
#[allow(clippy::too_many_lines, clippy::cast_possible_truncation)]
fn gb18030_encode(input: &[u32], buf: &mut ConvertBuf, is2022: bool) {
    for &w in input {
        let mut s: u32 = 0;

        if w == 0 {
            buf.push(0);
            continue;
        } else if let Some(v) = range_lookup(
            w,
            &t::UCS_A1_CP936_TABLE,
            t::UCS_A1_CP936_TABLE_MIN,
            t::UCS_A1_CP936_TABLE_MAX,
        ) {
            s = if w == 0x1F9 { 0xA8BF } else { v };
        } else if let Some(v) = range_lookup(
            w,
            &t::UCS_A2_CP936_TABLE,
            t::UCS_A2_CP936_TABLE_MIN,
            t::UCS_A2_CP936_TABLE_MAX,
        ) {
            s = if w == 0x20AC { 0xA2E3 } else { v };
        } else if let Some(v) = range_lookup(
            w,
            &t::UCS_A3_CP936_TABLE,
            t::UCS_A3_CP936_TABLE_MIN,
            t::UCS_A3_CP936_TABLE_MAX,
        ) {
            s = v;
        } else if is2022 && (0x9FB4..=0x9FBB).contains(&w) {
            /* Newly mapped in GB18030-2022 */
            s = match w {
                0x9FB4 => 0xFE59,
                0x9FB5 => 0xFE61,
                0x9FB6 => 0xFE66,
                0x9FB7 => 0xFE67,
                0x9FB8 => 0xFE6D,
                0x9FB9 => 0xFE7E,
                0x9FBA => 0xFE90,
                _ => 0xFEA0,
            };
        } else if let Some(v) = range_lookup(
            w,
            &t::UCS_I_CP936_TABLE,
            t::UCS_I_CP936_TABLE_MIN,
            t::UCS_I_CP936_TABLE_MAX,
        ) {
            s = v;
        } else if w >= umin(t::UCS_CI_CP936_TABLE_MIN) && w < umin(t::UCS_CI_CP936_TABLE_MAX) {
            /* U+F900-U+FA2F CJK Compatibility Ideographs */
            s = match w {
                0xF92C => 0xFD9C,
                0xF979 => 0xFD9D,
                0xF995 => 0xFD9E,
                0xF9E7 => 0xFD9F,
                0xF9F1 => 0xFDA0,
                0xFA0C..=0xFA29 => u32::from(t::UCS_CI_S_CP936_TABLE[(w - 0xFA0C) as usize]),
                _ => 0,
            };
        } else if let Some(v) = range_lookup(
            w,
            &t::UCS_CF_CP936_TABLE,
            t::UCS_CF_CP936_TABLE_MIN,
            t::UCS_CF_CP936_TABLE_MAX,
        ) {
            /* CJK Compatibility Forms */
            s = v;
        } else if let Some(v) = range_lookup(
            w,
            &t::UCS_SFV_CP936_TABLE,
            t::UCS_SFV_CP936_TABLE_MIN,
            t::UCS_SFV_CP936_TABLE_MAX,
        ) {
            /* U+FE50-U+FE6F Small Form Variants */
            s = v;
        } else if w >= umin(t::UCS_HFF_CP936_TABLE_MIN) && w < umin(t::UCS_HFF_CP936_TABLE_MAX) {
            /* U+FF00-U+FFFF HW/FW Forms */
            s = hff_to_gb(w);
        } else if (0xE000..=0xE864).contains(&w) {
            /* PUA */
            s = pua_to_gb(
                w,
                if is2022 {
                    &g::MBFL_GB18030_2022_PUA_TBL
                } else {
                    &g::MBFL_GB18030_PUA_TBL
                },
            );
        } else if is2022 && (0xFE10..=0xFE19).contains(&w) {
            /* Newly mapped codepoints in GB18030-2022 */
            s = match w {
                0xFE11 => 0xA6DB,
                0xFE12 => 0xA6DA,
                0xFE10..=0xFE16 => w - (0xFE10 - 0xA6D9),
                0xFE17..=0xFE18 => w - (0xFE17 - 0xA6EC),
                _ => 0xA6F3,
            };
        } else if is2022 && w == 0x1E3F {
            /* Newly mapped codepoint in GB18030-2022 */
            s = 0xA8BC;
        }

        /* While GB18030 and CP936 are very similar, some mappings are different between
         * these encodings; do a binary search in a table of differing codepoints */
        let keys = &g::MBFL_GB18030_C_TBL_KEY;
        if s == 0
            && w >= u32::from(keys[0])
            && w <= u32::from(keys[keys.len() - 1])
        {
            if let Some(i) = crate::codecs::bisec_srch2(i64::from(w), keys) {
                s = u32::from(g::MBFL_GB18030_C_TBL_VAL[i]);
            }
        }

        /* If we have not yet found a suitable mapping for this codepoint, it requires a
         * 4-byte code */
        if s == 0 && (0x80..=0xFFFF).contains(&w) {
            /* BMP */
            let (tbl, ofst): (&[u16], &[u16]) = if is2022 {
                (&g::MBFL_UNI2GB2022_TBL, &g::MBFL_GB2022_UNI_OFST)
            } else {
                (&g::MBFL_UNI2GB_TBL, &g::MBFL_GB_UNI_OFST)
            };
            if let Some(i) = crate::codecs::bisec_srch(i64::from(w), tbl) {
                s = gb4_pack(w - u32::from(ofst[i]), 0x81);
            }
        } else if (0x1_0000..=0x10_FFFF).contains(&w) {
            /* Code set 3: Unicode U+10000-U+10FFFF */
            s = gb4_pack(w - 0x10000, 0x90);
        }

        if s == 0 {
            illegal_output(w, wchar_to_gb18030, buf);
        } else if s < 0x80 {
            buf.push(s as u8);
        } else if s > 0xFF_FFFF {
            buf.extend(&s.to_be_bytes());
        } else {
            buf.extend(&[(s >> 8) as u8, s as u8]);
        }
    }
}

/// `mb_wchar_to_gb18030`.
pub fn wchar_to_gb18030(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    gb18030_encode(input, buf, false);
}

/// `mb_wchar_to_gb18030_2022`.
pub fn wchar_to_gb18030_2022(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    gb18030_encode(input, buf, true);
}

/// `mb_cp936_to_wchar`.
pub fn cp936_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let s = *input;
    let e = s.len();
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len();

    while p < e && out < limit {
        let c = s[p];
        p += 1;

        buf[out] = if c < 0x80 {
            u32::from(c)
        } else if c == 0x80 {
            0x20AC /* Euro sign */
        } else if c < 0xFF {
            if p >= e {
                BAD_INPUT
            } else {
                let c2 = s[p];
                p += 1;
                if c2 < 0x40 || c2 == 0x7F || c2 == 0xFF {
                    BAD_INPUT
                } else if ((0xAA..=0xAF).contains(&c) || (0xF8..=0xFE).contains(&c)) && c2 >= 0xA1
                {
                    /* UDA part 1, 2: U+E000-U+E4C5 */
                    let row = if c >= 0xF8 { c - 0xF2 } else { c - 0xAA };
                    94 * u32::from(row) + u32::from(c2 - 0xA1) + 0xE000
                } else if (0xA1..=0xA7).contains(&c) && c2 < 0xA1 {
                    /* UDA part 3: U+E4C6-U+E765 */
                    96 * u32::from(c - 0xA1) + u32::from(c2)
                        - if c2 >= 0x80 { 0x41 } else { 0x40 }
                        + 0xE4C6
                } else {
                    /* Convert c, c2 into GB 2312 table lookup index */
                    let w = u32::from(c - 0x81) * 192 + u32::from(c2) - 0x40;
                    if w >= 0x192B && w <= 0x1EBE {
                        u32::from(t::CP936_PUA_TBL1[(w - 0x192B) as usize])
                    } else if (0x413A..=0x413E).contains(&w) {
                        u32::from(t::CP936_PUA_TBL2[(w - 0x413A) as usize])
                    } else if (0x5DD0..=0x5E20).contains(&w) {
                        u32::from(t::CP936_PUA_TBL3[(w - 0x5DD0) as usize])
                    } else {
                        cp936_ucs(w)
                    }
                }
            }
        } else {
            0xF8F5
        };
        out += 1;
    }

    *input = &s[p..];
    out
}

/// `mb_wchar_to_cp936`.
#[allow(clippy::cast_possible_truncation)]
pub fn wchar_to_cp936(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        let s = if let Some(v) = range_lookup(
            w,
            &t::UCS_A1_CP936_TABLE,
            t::UCS_A1_CP936_TABLE_MIN,
            t::UCS_A1_CP936_TABLE_MAX,
        ) {
            /* U+0000-U+0451 */
            v
        } else if let Some(v) = range_lookup(
            w,
            &t::UCS_A2_CP936_TABLE,
            t::UCS_A2_CP936_TABLE_MIN,
            t::UCS_A2_CP936_TABLE_MAX,
        ) {
            /* U+2000-U+26FF */
            match w {
                0x203E => 0xA3FE,
                0x2218 => 0xA1E3,
                0x223C => 0xA1AB,
                _ => v,
            }
        } else if let Some(v) = range_lookup(
            w,
            &t::UCS_A3_CP936_TABLE,
            t::UCS_A3_CP936_TABLE_MIN,
            t::UCS_A3_CP936_TABLE_MAX,
        ) {
            v
        } else if let Some(v) = range_lookup(
            w,
            &t::UCS_I_CP936_TABLE,
            t::UCS_I_CP936_TABLE_MIN,
            t::UCS_I_CP936_TABLE_MAX,
        ) {
            v
        } else if (0xE000..=0xE864).contains(&w) {
            /* PUA */
            pua_to_gb(w, &t::MBFL_CP936_PUA_TBL)
        } else if w == 0xF8F5 {
            0xFF
        } else if let Some(v) = range_lookup(
            w,
            &t::UCS_CI_CP936_TABLE,
            t::UCS_CI_CP936_TABLE_MIN,
            t::UCS_CI_CP936_TABLE_MAX,
        ) {
            /* U+F900-U+FA2F CJK Compatibility Ideographs */
            v
        } else if let Some(v) = range_lookup(
            w,
            &t::UCS_CF_CP936_TABLE,
            t::UCS_CF_CP936_TABLE_MIN,
            t::UCS_CF_CP936_TABLE_MAX,
        ) {
            v
        } else if let Some(v) = range_lookup(
            w,
            &t::UCS_SFV_CP936_TABLE,
            t::UCS_SFV_CP936_TABLE_MIN,
            t::UCS_SFV_CP936_TABLE_MAX,
        ) {
            /* U+FE50-U+FE6F Small Form Variants */
            v
        } else if w >= umin(t::UCS_HFF_CP936_TABLE_MIN) && w < umin(t::UCS_HFF_CP936_TABLE_MAX) {
            /* U+FF00-U+FFFF HW/FW Forms */
            hff_to_gb(w)
        } else {
            0
        };

        if s == 0 {
            if w == 0 {
                buf.push(0);
            } else {
                illegal_output(w, wchar_to_cp936, buf);
            }
        } else if s <= 0x80 || s == 0xFF {
            buf.push(s as u8);
        } else {
            buf.extend(&[(s >> 8) as u8, s as u8]);
        }
    }
}

/// `step_through_gb18030_str`: the last position at or before `limit` which
/// directly follows the end of a character, stepping from `p`.
fn step_through_gb18030_str(s: &[u8], mut p: usize, limit: usize) -> usize {
    while p < limit {
        let c = s[p];
        if c < 0x81 || c == 0xFF {
            p += 1;
        } else {
            if limit - p == 1 {
                break;
            }
            let c2 = s[p + 1];
            /* For a 4-byte char, the 2nd byte will be 0x30-0x39 */
            let w = if (0x30..=0x39).contains(&c2) { 4 } else { 2 };
            if limit - p < w {
                break;
            }
            p += w;
        }
    }
    p
}

/// `mb_cut_gb18030`: `mb_strcut` for GB18030 and GB18030-2022.
pub fn cut_gb18030(s: &[u8], from: usize, len: usize) -> Vec<u8> {
    let end = s.len();
    let from = from.min(end);
    let start = step_through_gb18030_str(s, 0, from);
    let len = if from.saturating_add(len) > end {
        end - from
    } else {
        len
    };
    if start + len >= end {
        s[start..].to_vec()
    } else {
        let e = step_through_gb18030_str(s, start, start + len);
        s[start..e].to_vec()
    }
}

/// Replay of the hashed exhaustive oracle fixtures
/// (`tests/fixtures/cjk/gb_exhaustive_lib.php`), shared with the BIG5 and HZ
/// tests.
#[cfg(test)]
pub(crate) mod exhaustive {
    use crate::codecs::cjk::testutil::{enc, fixture_path};
    use crate::codecs::utf8::push_utf8;
    use crate::convert::{fast_convert, ErrorMode};

    fn fnv1a64(data: &[u8]) -> String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in data {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        format!("{h:016x}")
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("bad hex"))
            .collect()
    }

    fn num<T: std::str::FromStr>(s: &str) -> T
    where
        T::Err: std::fmt::Debug,
    {
        s.parse().expect("bad number in fixture")
    }

    /// Replay `name`; returns the number of lines checked.
    #[allow(clippy::cast_possible_truncation)]
    pub(crate) fn replay(name: &str) -> usize {
        let text = std::fs::read_to_string(fixture_path(name))
            .unwrap_or_else(|e| panic!("cannot read fixture {name}: {e}"));
        let mut failures = Vec::new();
        let mut n = 0;
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            n += 1;
            let (e, input, to_utf8, hash, errors) = match f[0] {
                "dec" => {
                    let mut input = unhex(f[2]);
                    let sep = unhex(f[3]);
                    let b1: u8 = num(f[4]);
                    for b2 in 0..=255u8 {
                        input.extend([b1, b2]);
                        input.extend(&sep);
                    }
                    (f[1], input, true, f[5], f[6])
                }
                "dec4" => {
                    let (b1, b2): (u8, u8) = (num(f[2]), num(f[3]));
                    let mut input = Vec::new();
                    for b3 in 0x81..=0xFEu8 {
                        for b4 in 0x30..=0x39u8 {
                            input.extend([b1, b2, b3, b4]);
                        }
                    }
                    (f[1], input, true, f[4], f[5])
                }
                "enc" => {
                    let (start, count): (u32, u32) = (num(f[2]), num(f[3]));
                    let mut input = Vec::new();
                    for cp in start..start + count {
                        if !(0xD800..=0xDFFF).contains(&cp) {
                            push_utf8(&mut input, cp);
                        }
                    }
                    (f[1], input, false, f[4], f[5])
                }
                other => panic!("unknown record {other}"),
            };
            let (out, errs) = if to_utf8 {
                fast_convert(&input, enc(e), enc("UTF-8"), 0x10_FFFD, ErrorMode::Char)
            } else {
                fast_convert(&input, enc("UTF-8"), enc(e), 0x3F, ErrorMode::Char)
            };
            let exp_err: u32 = num(errors);
            if fnv1a64(&out) != hash || errs != exp_err {
                failures.push(format!("{line} (got {} / {errs} errors)", fnv1a64(&out)));
            }
        }
        assert!(
            failures.is_empty(),
            "{} of {n} exhaustive cases in {name} failed:\n{}",
            failures.len(),
            failures.join("\n")
        );
        n
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn detail_oracle() {
        assert!(crate::codecs::cjk::testutil::replay_oracle("gb_oracle.tsv") > 500);
    }

    #[test]
    fn exhaustive_oracle() {
        assert!(super::exhaustive::replay("gb_exhaustive.tsv") > 1000);
    }
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Shift-JIS family: `SJIS`, `SJIS-mac`, `SJIS-Mobile#*`, `CP932`, `SJIS-win`.
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c` ("SJIS variants").

// The ports keep the C code's short variable names (c, c2, w, s, p, e...).
#![allow(clippy::many_single_char_names)]

use crate::codecs::bisec_srch2;
use crate::codecs::cjk::common::{
    binary_search_paired_sorted_table, nflags, sjis_emoji_docomo2unicode, sjis_emoji_kddi2unicode,
    sjis_emoji_sb2unicode, sjis_encode, NFLAGS_CODE_KDDI, NFLAGS_CODE_SB, NFLAGS_S,
};
use crate::convert::{illegal_output, ConvertBuf, FromWcharFn, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};
use crate::tables::cjk::cp932_ext as x;
use crate::tables::cjk::emoji as em;
use crate::tables::cjk::jis as j;
use crate::tables::cjk::misc as m;
use crate::tables::cjk::sjis_mac as mac;

/// `SJIS`.
pub static ENC_SJIS: Encoding = Encoding {
    no: EncodingNo::Sjis,
    name: "SJIS",
    mime_name: Some("Shift_JIS"),
    aliases: &["x-sjis", "SHIFT-JIS"],
    mblen_table: Some(&m::MBLEN_TABLE_SJIS),
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: sjis_to_wchar,
    from_wchar: wchar_to_sjis,
    check: None,
    cut: None,
};

/// `SJIS-mac`.
pub static ENC_SJIS_MAC: Encoding = Encoding {
    no: EncodingNo::SjisMac,
    name: "SJIS-mac",
    mime_name: Some("Shift_JIS"),
    aliases: &["MacJapanese", "x-Mac-Japanese"],
    mblen_table: Some(&m::MBLEN_TABLE_SJISMAC),
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: sjismac_to_wchar,
    from_wchar: wchar_to_sjismac,
    check: None,
    cut: None,
};

/// `SJIS-Mobile#DOCOMO`.
pub static ENC_SJIS_DOCOMO: Encoding = Encoding {
    no: EncodingNo::SjisDocomo,
    name: "SJIS-Mobile#DOCOMO",
    mime_name: Some("Shift_JIS"),
    aliases: &["SJIS-DOCOMO", "shift_jis-imode", "x-sjis-emoji-docomo"],
    mblen_table: Some(&m::MBLEN_TABLE_SJIS_MOBILE),
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: sjis_docomo_to_wchar,
    from_wchar: wchar_to_sjis_docomo,
    check: None,
    cut: None,
};

/// `SJIS-Mobile#KDDI`.
pub static ENC_SJIS_KDDI: Encoding = Encoding {
    no: EncodingNo::SjisKddi,
    name: "SJIS-Mobile#KDDI",
    mime_name: Some("Shift_JIS"),
    aliases: &["SJIS-KDDI", "shift_jis-kddi", "x-sjis-emoji-kddi"],
    mblen_table: Some(&m::MBLEN_TABLE_SJIS_MOBILE),
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: sjis_kddi_to_wchar,
    from_wchar: wchar_to_sjis_kddi,
    check: None,
    cut: None,
};

/// `SJIS-Mobile#SOFTBANK`.
pub static ENC_SJIS_SB: Encoding = Encoding {
    no: EncodingNo::SjisSb,
    name: "SJIS-Mobile#SOFTBANK",
    mime_name: Some("Shift_JIS"),
    aliases: &[
        "SJIS-SOFTBANK",
        "shift_jis-softbank",
        "x-sjis-emoji-softbank",
    ],
    mblen_table: Some(&m::MBLEN_TABLE_SJIS_MOBILE),
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: sjis_sb_to_wchar,
    from_wchar: wchar_to_sjis_sb,
    check: None,
    cut: None,
};

/// `CP932`.
pub static ENC_CP932: Encoding = Encoding {
    no: EncodingNo::Cp932,
    name: "CP932",
    mime_name: Some("Shift_JIS"),
    aliases: &["MS932", "Windows-31J", "MS_Kanji"],
    mblen_table: Some(&m::MBLEN_TABLE_SJISWIN),
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: cp932_to_wchar,
    from_wchar: wchar_to_cp932,
    check: None,
    cut: None,
};

/// `SJIS-win`.
pub static ENC_SJISWIN: Encoding = Encoding {
    no: EncodingNo::SjisWin,
    name: "SJIS-win",
    mime_name: Some("Shift_JIS"),
    aliases: &["SJIS-ms", "SJIS-open"],
    mblen_table: Some(&m::MBLEN_TABLE_SJISWIN),
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: cp932_to_wchar,
    from_wchar: wchar_to_sjiswin,
    check: None,
    cut: None,
};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

#[allow(clippy::cast_sign_loss)]
const fn u(v: i32) -> u32 {
    v as u32
}

const JISX0208_SIZE: u32 = u(j::JISX0208_UCS_TABLE_SIZE);
const EXT1_MIN: u32 = u(x::CP932EXT1_UCS_TABLE_MIN);
const EXT1_MAX: u32 = u(x::CP932EXT1_UCS_TABLE_MAX);
const EXT2_MIN: u32 = u(x::CP932EXT2_UCS_TABLE_MIN);
const EXT2_MAX: u32 = u(x::CP932EXT2_UCS_TABLE_MAX);
const EXT3_MIN: u32 = u(x::CP932EXT3_UCS_TABLE_MIN);
const EXT3_MAX: u32 = u(x::CP932EXT3_UCS_TABLE_MAX);

/// `sjis_decode_tbl1[c] + sjis_decode_tbl2[c2]`: (ku*94)+ten index of a
/// Shift-JIS byte pair (huge for illegal pairs).
#[inline]
fn sjis_index(c: u8, c2: u8) -> u32 {
    u32::from(m::SJIS_DECODE_TBL1[usize::from(c)]) + u32::from(m::SJIS_DECODE_TBL2[usize::from(c2)])
}

/// `sjis_mobile_decode_tbl1[c] + sjis_decode_tbl2[c2]` (lead bytes up to 0xFC).
#[inline]
fn sjis_mobile_index(c: u8, c2: u8) -> u32 {
    u32::from(m::SJIS_MOBILE_DECODE_TBL1[usize::from(c)])
        + u32::from(m::SJIS_DECODE_TBL2[usize::from(c2)])
}

#[inline]
fn at(tbl: &[u16], i: u32) -> u32 {
    u32::from(tbl[i as usize])
}

/// The `ucs_{a1,a2,i,r}_jis_table` lookup common to all Shift-JIS encoders:
/// JIS X 0208 row/cell (or JIS X 0212 | 0x8080, or a single byte) for `w`,
/// 0 if not found.
pub(crate) fn ucs_to_jis(w: u32) -> u32 {
    if w < u(j::UCS_A1_JIS_TABLE_MAX) {
        at(&j::UCS_A1_JIS_TABLE, w)
    } else if (u(j::UCS_A2_JIS_TABLE_MIN)..u(j::UCS_A2_JIS_TABLE_MAX)).contains(&w) {
        at(&j::UCS_A2_JIS_TABLE, w - u(j::UCS_A2_JIS_TABLE_MIN))
    } else if (u(j::UCS_I_JIS_TABLE_MIN)..u(j::UCS_I_JIS_TABLE_MAX)).contains(&w) {
        at(&j::UCS_I_JIS_TABLE, w - u(j::UCS_I_JIS_TABLE_MIN))
    } else if (u(j::UCS_R_JIS_TABLE_MIN)..u(j::UCS_R_JIS_TABLE_MAX)).contains(&w) {
        at(&j::UCS_R_JIS_TABLE, w - u(j::UCS_R_JIS_TABLE_MIN))
    } else {
        0
    }
}

/// Emit JIS X 0208 row/cell `s` (`> 0xFF`) as a Shift-JIS byte pair.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
fn push_sjis(buf: &mut ConvertBuf, s: u32) {
    let (s1, s2) = sjis_encode(((s >> 8) & 0xFF) as i32, (s & 0xFF) as i32);
    buf.push(s1 as u8);
    buf.push(s2 as u8);
}

/// Emit `s` as one byte if `<= 0xFF`, else as a Shift-JIS pair.
#[allow(clippy::cast_possible_truncation)]
fn emit(buf: &mut ConvertBuf, s: u32) {
    if s <= 0xFF {
        buf.push(s as u8);
    } else {
        push_sjis(buf, s);
    }
}

/// (ku*94)+ten value to JIS row/cell bytes.
#[inline]
const fn kuten_to_jis(s: u32) -> u32 {
    (((s / 94) + 0x21) << 8) | ((s % 94) + 0x21)
}

/// Unicode fullwidth forms used by the "Windows-style" Shift-JIS decoders for
/// row 1 cells which JIS X 0208 maps differently.
#[inline]
fn win_row1(w: u32) -> Option<u32> {
    match w {
        31 => Some(0xFF3C),  // FULLWIDTH REVERSE SOLIDUS
        32 => Some(0xFF5E),  // FULLWIDTH TILDE
        33 => Some(0x2225),  // PARALLEL TO
        60 => Some(0xFF0D),  // FULLWIDTH HYPHEN-MINUS
        80 => Some(0xFFE0),  // FULLWIDTH CENT SIGN
        81 => Some(0xFFE1),  // FULLWIDTH POUND SIGN
        137 => Some(0xFFE2), // FULLWIDTH NOT SIGN
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// SJIS
// ---------------------------------------------------------------------------

/// `mb_sjis_to_wchar`.
pub fn sjis_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let s = *input;
    if s.is_empty() {
        return 0;
    }
    // Stop the main loop 1 byte short of the end of the input
    let e = s.len() - 1;
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len();

    while p < e && out < limit {
        let c = s[p];
        p += 1;
        if c <= 0x7F {
            buf[out] = u32::from(c);
        } else if (0xA1..=0xDF).contains(&c) {
            buf[out] = 0xFEC0 + u32::from(c);
        } else {
            let c2 = s[p];
            p += 1;
            let w = sjis_index(c, c2);
            if w < JISX0208_SIZE {
                let w = at(&j::JISX0208_UCS_TABLE, w);
                buf[out] = if w == 0 { BAD_INPUT } else { w };
            } else {
                if c == 0x80 || c == 0xA0 || c > 0xEF {
                    p -= 1;
                }
                buf[out] = BAD_INPUT;
            }
        }
        out += 1;
    }

    // Finish up last byte of input string if there is one
    if p == e && out < limit {
        let c = s[p];
        p += 1;
        buf[out] = if c <= 0x7F {
            u32::from(c)
        } else if (0xA1..=0xDF).contains(&c) {
            0xFEC0 + u32::from(c)
        } else {
            BAD_INPUT
        };
        out += 1;
    }

    *input = &s[p..];
    out
}

/// `mb_wchar_to_sjis`.
pub fn wchar_to_sjis(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        let mut s = ucs_to_jis(w);
        if s == 0 {
            s = match w {
                0xA5 => 0x216F,          // YEN SIGN -> FULLWIDTH YEN SIGN
                0xAF | 0x203E => 0x2131, // FULLWIDTH MACRON
                0xFF3C => 0x2140,        // FULLWIDTH REVERSE SOLIDUS
                0x2225 => 0x2142,        // PARALLEL TO
                0xFF0D => 0x215D,        // FULLWIDTH HYPHEN-MINUS
                0xFFE0 => 0x2171,        // FULLWIDTH CENT SIGN
                0xFFE1 => 0x2172,        // FULLWIDTH POUND SIGN
                0xFFE2 => 0x224C,        // FULLWIDTH NOT SIGN
                0 => 0,
                _ => {
                    illegal_output(w, wchar_to_sjis, buf);
                    continue;
                }
            };
        } else if s >= 0x8080 {
            // JIS X 0212; not supported
            illegal_output(w, wchar_to_sjis, buf);
            continue;
        }
        emit(buf, s);
    }
}

// ---------------------------------------------------------------------------
// SJIS-mac (MacJapanese)
// ---------------------------------------------------------------------------

/// `mb_sjismac_to_wchar`. A single kuten code can decode to up to 5 code
/// points, so `buf` must hold at least 5.
#[allow(clippy::too_many_lines)]
pub fn sjismac_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    debug_assert!(buf.len() >= 5);
    let s = *input;
    let e = s.len();
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len();

    'main: while p < e && out < limit {
        let c = s[p];
        p += 1;

        if c <= 0x80 || c == 0xA0 {
            buf[out] = match c {
                0x5C => 0xA5,
                0x80 => 0x5C,
                _ => u32::from(c),
            };
            out += 1;
        } else if (0xA1..=0xDF).contains(&c) {
            buf[out] = 0xFEC0 + u32::from(c);
            out += 1;
        } else if c <= 0xED {
            if p == e {
                buf[out] = BAD_INPUT;
                out += 1;
                break;
            }
            let c2 = s[p];
            p += 1;
            let w = sjis_index(c, c2);

            if w <= 0x89 {
                let special = match w {
                    0x1C => Some(0x2014), // EM DASH
                    0x1F => Some(0xFF3C), // FULLWIDTH REVERSE SOLIDUS
                    0x20 => Some(0x301C), // FULLWIDTH TILDE
                    0x21 => Some(0x2016), // PARALLEL TO
                    0x3C => Some(0x2212), // FULLWIDTH HYPHEN-MINUS
                    0x50 => Some(0xA2),   // FULLWIDTH CENT SIGN
                    0x51 => Some(0xA3),   // FULLWIDTH POUND SIGN
                    0x89 => Some(0xAC),   // FULLWIDTH NOT SIGN
                    _ => None,
                };
                if let Some(v) = special {
                    buf[out] = v;
                    out += 1;
                    continue;
                }
            } else {
                if (0x2F0..=0x3A3).contains(&w) {
                    for row in &mac::CODE_TBL {
                        let (lo, hi, base) =
                            (u32::from(row[0]), u32::from(row[1]), u32::from(row[2]));
                        if w >= lo && w <= hi {
                            buf[out] = w - lo + base;
                            out += 1;
                            continue 'main;
                        }
                    }
                }

                if (0x340..=0x523).contains(&w) {
                    for row in &mac::CODE_TBL_M {
                        if w == u32::from(row[0]) {
                            let n = match row[1] {
                                0xF860 => 3,
                                0xF861 => 4,
                                _ => 5,
                            };
                            if limit - out < n {
                                p -= 2;
                                break 'main;
                            }
                            for &v in &row[1..=n] {
                                buf[out] = u32::from(v);
                                out += 1;
                            }
                            continue 'main;
                        }
                    }
                }

                if (0x3AC..=0x20A5).contains(&w) {
                    for (i, range) in mac::CODE_OFST_TBL.iter().enumerate() {
                        let (lo, hi) = (u32::from(range[0]), u32::from(range[1]));
                        if w >= lo && w <= hi {
                            let w2 = at(mac::CODE_MAP[i], w - lo);
                            if w2 == 0 {
                                buf[out] = BAD_INPUT;
                                out += 1;
                                continue 'main;
                            }
                            if limit - out < 2 {
                                p -= 2;
                                break 'main;
                            }
                            buf[out] = w2;
                            out += 1;
                            if (0x43E..=0x441).contains(&w) {
                                buf[out] = 0xF87A;
                                out += 1;
                            } else if w == 0x3B1 || w == 0x3B7 {
                                buf[out] = 0xF87F;
                                out += 1;
                            } else if w == 0x4B8 || w == 0x4B9 || w == 0x4C4 {
                                buf[out] = 0x20DD;
                                out += 1;
                            } else if w == 0x1ED9
                                || w == 0x1EDA
                                || w == 0x1EE8
                                || w == 0x1EF3
                                || (0x1EF5..=0x1EFB).contains(&w)
                                || w == 0x1F05
                                || w == 0x1F06
                                || w == 0x1F18
                                || (0x1FF2..=0x20A5).contains(&w)
                            {
                                buf[out] = 0xF87E;
                                out += 1;
                            }
                            continue 'main;
                        }
                    }
                }
            }

            buf[out] = if w < JISX0208_SIZE {
                match at(&j::JISX0208_UCS_TABLE, w) {
                    0 => BAD_INPUT,
                    v => v,
                }
            } else {
                BAD_INPUT
            };
            out += 1;
        } else if c == 0xFD {
            buf[out] = 0xA9;
            out += 1;
        } else if c == 0xFE {
            buf[out] = 0x2122;
            out += 1;
        } else if c == 0xFF {
            if limit - out < 2 {
                p -= 1;
                break;
            }
            buf[out] = 0x2026;
            buf[out + 1] = 0xF87F;
            out += 2;
        } else {
            buf[out] = BAD_INPUT;
            out += 1;
        }
    }

    *input = &s[p..];
    out
}

/// `process_s_form`: look up `w` followed by the variation selector-like code
/// point `w2` in the special-form table.
fn process_s_form(w: u32, w2: u32) -> Option<u32> {
    let range = match w2 {
        0xF87A => 34 + 3 + 3..34 + 3 + 3 + 4,
        0x20DD => 34 + 3..34 + 3 + 3,
        0xF87F => 34..34 + 3,
        0xF87E => 0..34,
        _ => return None,
    };
    range
        .into_iter()
        .find(|&i| w == u32::from(mac::S_FORM_TBL[i]))
        .map(|i| u32::from(mac::S_FORM_SJIS_TBL[i]))
}

/// For code points F860-F862, which are treated specially in `MacJapanese`
/// (`transcoding_hint_cp_width`).
const TRANSCODING_HINT_CP_WIDTH: [usize; 3] = [3, 4, 5];

/// Continue matching an Apple "transcoding hint" sequence (`w` is
/// U+F860..U+F862, row `i` of `code_tbl_m`, `index` code points matched).
/// Returns true if the encoder must return (input exhausted).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
fn sjismac_transcoding_hint(
    w: u32,
    i: usize,
    mut index: usize,
    input: &[u32],
    idx: &mut usize,
    buf: &mut ConvertBuf,
    end: bool,
) -> bool {
    let row = &mac::CODE_TBL_M[i];
    let expected = TRANSCODING_HINT_CP_WIDTH[(w - 0xF860) as usize];

    while index <= expected {
        if *idx == input.len() {
            if end {
                for &v in &row[1..index] {
                    illegal_output(u32::from(v), wchar_to_sjismac, buf);
                }
            } else {
                buf.state = ((i as u32) << 24) | ((index as u32) << 16) | (w & 0xFFFF);
            }
            return true;
        }
        let w2 = input[*idx];
        *idx += 1;
        if w2 != u32::from(row[index]) {
            // Didn't match
            for &v in &row[1..index] {
                illegal_output(u32::from(v), wchar_to_sjismac, buf);
            }
            illegal_output(w2, wchar_to_sjismac, buf);
            return false;
        }
        index += 1;
    }

    // Successful match, emit SJIS-mac bytes
    let s = u32::from(row[0]);
    let (s1, s2) = sjis_encode(((s / 94) + 0x21) as i32, ((s % 94) + 0x21) as i32);
    buf.push(s1 as u8);
    buf.push(s2 as u8);
    false
}

/// `mb_wchar_to_sjismac`.
#[allow(clippy::too_many_lines, clippy::cast_possible_truncation)]
pub fn wchar_to_sjismac(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    let mut idx = 0usize;
    let mut pending = None;

    if buf.state != 0 {
        let w = buf.state & 0xFFFF;
        if buf.state & 0xFF00_0000 != 0 {
            let i = (buf.state >> 24) as usize;
            let index = ((buf.state >> 16) & 0xFF) as usize;
            buf.state = 0;
            if sjismac_transcoding_hint(w, i, index, input, &mut idx, buf, end) {
                return;
            }
        } else {
            buf.state = 0;
            pending = Some(w);
        }
    }

    loop {
        let w = if let Some(w) = pending.take() {
            w
        } else if idx < input.len() {
            idx += 1;
            input[idx - 1]
        } else {
            break;
        };

        let mut s = 0u32;
        if w < u(j::UCS_A1_JIS_TABLE_MAX) {
            s = match w {
                0x5C => 0x80,
                0xA9 => 0xFD,
                _ => at(&j::UCS_A1_JIS_TABLE, w),
            };
        } else if (u(j::UCS_A2_JIS_TABLE_MIN)..u(j::UCS_A2_JIS_TABLE_MAX)).contains(&w) {
            s = match w {
                0x2122 => 0xFE,
                0x2014 => 0x213D,
                0x2116 => 0x2C1D,
                _ => at(&j::UCS_A2_JIS_TABLE, w - u(j::UCS_A2_JIS_TABLE_MIN)),
            };
        } else if (u(j::UCS_I_JIS_TABLE_MIN)..u(j::UCS_I_JIS_TABLE_MAX)).contains(&w) {
            s = at(&j::UCS_I_JIS_TABLE, w - u(j::UCS_I_JIS_TABLE_MIN));
        } else if (u(j::UCS_R_JIS_TABLE_MIN)..u(j::UCS_R_JIS_TABLE_MAX)).contains(&w) {
            s = at(&j::UCS_R_JIS_TABLE, w - u(j::UCS_R_JIS_TABLE_MIN));
        }

        if w >= 0x2000 {
            if let Some(i) = mac::S_FORM_TBL.iter().position(|&v| u32::from(v) == w) {
                if idx == input.len() {
                    if end {
                        let s = u32::from(mac::S_FORM_SJIS_FALLBACK_TBL[i]);
                        if s != 0 {
                            buf.push(((s >> 8) & 0xFF) as u8);
                            buf.push((s & 0xFF) as u8);
                        } else {
                            illegal_output(w, wchar_to_sjismac, buf);
                        }
                    } else {
                        buf.state = w;
                    }
                    return;
                }
                let w2 = input[idx];
                idx += 1;

                let s = if let Some(s) = process_s_form(w, w2) {
                    s
                } else {
                    idx -= 1;
                    u32::from(mac::S_FORM_SJIS_FALLBACK_TBL[i])
                };

                if s <= 0xFF {
                    buf.push(s as u8);
                } else {
                    buf.push(((s >> 8) & 0xFF) as u8);
                    buf.push((s & 0xFF) as u8);
                }
                continue;
            }

            if (0xF860..=0xF862).contains(&w) {
                // Apple 'transcoding hint' codepoints (from private use area)
                if idx == input.len() {
                    if end {
                        illegal_output(w, wchar_to_sjismac, buf);
                    } else {
                        buf.state = w;
                    }
                    return;
                }

                let w2 = input[idx];
                idx += 1;

                if let Some(i) = mac::CODE_TBL_M
                    .iter()
                    .position(|row| w == u32::from(row[1]) && w2 == u32::from(row[2]))
                {
                    // This might be a valid transcoding hint sequence
                    if sjismac_transcoding_hint(w, i, 3, input, &mut idx, buf, end) {
                        return;
                    }
                    continue;
                }

                // No valid transcoding hint sequence found
                idx -= 1;
                illegal_output(w, wchar_to_sjismac, buf);
                continue;
            }
        }

        if s == 0 {
            if w == 0xA0 {
                s = 0xA0;
            } else if w == 0xA5 {
                // YEN SIGN -> HALFWIDTH YEN SIGN
                s = 0x5C;
            } else if w == 0xFF3C {
                // FULLWIDTH REVERSE SOLIDUS
                s = 0x2140;
            } else {
                s = sjismac_lookup_r(w);
            }
        }

        if (s == 0 && w != 0) || s >= 0x8080 {
            illegal_output(w, wchar_to_sjismac, buf);
        } else {
            emit(buf, s);
        }
    }
}

/// The `wchar2sjis_mac_*` reverse tables of `mb_wchar_to_sjismac`: JIS
/// row/cell for `w`, or 0.
fn sjismac_lookup_r(w: u32) -> u32 {
    for row in &mac::WCHAR2SJIS_MAC_R_TBL {
        if w >= u32::from(row[0]) && w <= u32::from(row[1]) {
            return kuten_to_jis(w - u32::from(row[0]) + u32::from(row[2]));
        }
    }
    for (i, range) in mac::WCHAR2SJIS_MAC_R_MAP.iter().enumerate() {
        if w >= u32::from(range[0]) && w <= u32::from(range[1]) {
            let s = at(mac::WCHAR2SJIS_MAC_CODE_MAP[i], w - u32::from(range[0]));
            if s != 0 {
                return kuten_to_jis(s);
            }
        }
    }
    for row in &mac::WCHAR2SJIS_MAC_WCHAR_TBL {
        if w == u32::from(row[0]) {
            return kuten_to_jis(u32::from(row[1]));
        }
    }
    0
}

// ---------------------------------------------------------------------------
// SJIS-Mobile#DOCOMO / KDDI / SOFTBANK
// ---------------------------------------------------------------------------

/// Which mobile carrier's emoji a Shift-JIS variant uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Carrier {
    Docomo,
    Kddi,
    Softbank,
}

/// Decode the (ku*94)+ten index `w` of a mobile Shift-JIS byte pair `c c2`,
/// writing one or two code points. Returns the number of bytes to give back
/// (`p--`), 0 or 1. Ports the common body of `mb_sjis_{docomo,kddi,sb}_to_wchar`.
#[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
fn decode_mobile_pair(c: u8, c2: u8, carrier: Carrier, buf: &mut [u32], out: &mut usize) -> usize {
    let mut w = sjis_mobile_index(c, c2);

    if w <= 137 {
        if let Some(v) = win_row1(w) {
            buf[*out] = v;
            *out += 1;
            return 0;
        }
    }

    let emoji_range = match carrier {
        Carrier::Docomo => em::MB_TBL_CODE2UNI_DOCOMO1_MIN..=em::MB_TBL_CODE2UNI_DOCOMO1_MAX,
        Carrier::Kddi => em::MB_TBL_CODE2UNI_KDDI1_MIN..=em::MB_TBL_CODE2UNI_KDDI2_MAX,
        Carrier::Softbank => em::MB_TBL_CODE2UNI_SB1_MIN..=em::MB_TBL_CODE2UNI_SB3_MAX,
    };

    if emoji_range.contains(&(w as i32)) {
        let mut snd = 0;
        let r = match carrier {
            Carrier::Docomo => sjis_emoji_docomo2unicode(w as i32, &mut snd),
            Carrier::Kddi => sjis_emoji_kddi2unicode(w as i32, &mut snd),
            Carrier::Softbank => sjis_emoji_sb2unicode(w as i32, &mut snd),
        } as u32;
        if r == 0 && carrier != Carrier::Docomo {
            w = sjis_mobile_index(c, c2);
            if carrier == Carrier::Softbank && (EXT3_MIN..EXT3_MAX).contains(&w) {
                w = at(&x::CP932EXT3_UCS_TABLE, w - EXT3_MIN);
            } else if (94 * 94..114 * 94).contains(&w) {
                w = w - 94 * 94 + 0xE000;
            }
        } else {
            if snd != 0 {
                buf[*out] = snd as u32;
                *out += 1;
            }
            w = r;
        }
    } else if (EXT1_MIN..EXT1_MAX).contains(&w) {
        w = at(&x::CP932EXT1_UCS_TABLE, w - EXT1_MIN);
    } else if w < JISX0208_SIZE {
        w = at(&j::JISX0208_UCS_TABLE, w);
    } else if (EXT2_MIN..EXT2_MAX).contains(&w) {
        w = at(&x::CP932EXT2_UCS_TABLE, w - EXT2_MIN);
    } else if (EXT3_MIN..EXT3_MAX).contains(&w) {
        w = at(&x::CP932EXT3_UCS_TABLE, w - EXT3_MIN);
    } else if (94 * 94..114 * 94).contains(&w) {
        w = w - 94 * 94 + 0xE000;
    } else {
        buf[*out] = BAD_INPUT;
        *out += 1;
        return usize::from(c == 0x80 || c == 0xA0 || c >= 0xFD);
    }

    buf[*out] = if w == 0 { BAD_INPUT } else { w };
    *out += 1;
    0
}

/// Shared body of the `DoCoMo` and KDDI decoders.
fn sjis_mobile_to_wchar(input: &mut &[u8], buf: &mut [u32], carrier: Carrier) -> usize {
    let s = *input;
    let e = s.len();
    let mut p = 0usize;
    let mut out = 0usize;
    // Leave one extra space available in output buffer, since some iterations
    // of the main loop may emit two wchars
    let limit = buf.len() - 1;

    while p < e && out < limit {
        let c = s[p];
        p += 1;
        if c <= 0x7F {
            buf[out] = u32::from(c);
            out += 1;
        } else if (0xA1..=0xDF).contains(&c) {
            buf[out] = 0xFEC0 + u32::from(c);
            out += 1;
        } else {
            if p == e {
                buf[out] = BAD_INPUT;
                out += 1;
                break;
            }
            let c2 = s[p];
            p += 1;
            p -= decode_mobile_pair(c, c2, carrier, buf, &mut out);
        }
    }

    *input = &s[p..];
    out
}

/// `mb_sjis_docomo_to_wchar`.
pub fn sjis_docomo_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    sjis_mobile_to_wchar(input, buf, Carrier::Docomo)
}

/// `mb_sjis_kddi_to_wchar`.
pub fn sjis_kddi_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    sjis_mobile_to_wchar(input, buf, Carrier::Kddi)
}

/// `mb_sjis_sb_to_wchar`. `*state` holds the letter of a pending `ESC $ x`
/// `SoftBank` emoji escape sequence (0 if none).
#[allow(clippy::too_many_lines)]
pub fn sjis_sb_to_wchar(input: &mut &[u8], buf: &mut [u32], state: &mut u32) -> usize {
    let s = *input;
    let e = s.len();
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len() - 1;
    let mut in_escapes = *state != 0;

    loop {
        if in_escapes {
            // softbank_emoji_escapes:
            in_escapes = false;
            while p < e && out < limit {
                let c = s[p];
                p += 1;
                if c == 0xF {
                    *state = 0;
                    break;
                }
                let row = match (*state, c) {
                    (0x47, 0x21..=0x7A) => 0x91, // 'G'
                    (0x45, 0x21..=0x7A) => 0x8D, // 'E'
                    (0x46, 0x21..=0x7A) => 0x8E, // 'F'
                    (0x4F, 0x21..=0x6D) => 0x92, // 'O'
                    (0x50, 0x21..=0x6C) => 0x95, // 'P'
                    (0x51, 0x21..=0x5E) => 0x96, // 'Q'
                    _ => {
                        buf[out] = BAD_INPUT;
                        out += 1;
                        *state = 0;
                        break;
                    }
                };
                let code = (row - 0x21) * 94 + i32::from(c) - 0x21;
                let mut snd = 0;
                let w = sjis_emoji_sb2unicode(code, &mut snd);
                if w != 0 {
                    #[allow(clippy::cast_sign_loss)]
                    {
                        if snd != 0 {
                            buf[out] = snd as u32;
                            out += 1;
                        }
                        buf[out] = w as u32;
                        out += 1;
                    }
                } else {
                    buf[out] = BAD_INPUT;
                    out += 1;
                    *state = 0;
                    break;
                }
            }
            continue;
        }

        if !(p < e && out < limit) {
            break;
        }
        let c = s[p];
        p += 1;

        if c == 0x1B {
            // Escape sequence
            if p == e {
                buf[out] = BAD_INPUT;
                out += 1;
                continue;
            }
            let dollar = s[p];
            p += 1;
            if dollar != b'$' || p == e {
                buf[out] = BAD_INPUT;
                out += 1;
                continue;
            }
            let c2 = s[p];
            p += 1;
            if !(b'E'..=b'G').contains(&c2) && !(b'O'..=b'Q').contains(&c2) {
                buf[out] = BAD_INPUT;
                out += 1;
                continue;
            }
            // Escape sequence was valid, next should be a series of specially
            // encoded Softbank emoji
            *state = u32::from(c2);
            in_escapes = true;
        } else if c <= 0x7F {
            buf[out] = u32::from(c);
            out += 1;
        } else if (0xA1..=0xDF).contains(&c) {
            buf[out] = 0xFEC0 + u32::from(c);
            out += 1;
        } else {
            if p == e {
                buf[out] = BAD_INPUT;
                out += 1;
                break;
            }
            let c2 = s[p];
            p += 1;
            p -= decode_mobile_pair(c, c2, Carrier::Softbank, buf, &mut out);
        }
    }

    *input = &s[p..];
    out
}

/// Per-carrier parameters of the mobile Shift-JIS encoders.
struct MobileEnc {
    carrier: Carrier,
    /// (ku*94)+ten of the keypad emoji for '#', '0', '1'.
    keypad: [u32; 3],
    /// (ku*94)+ten of the copyright and registered signs.
    copyright: u32,
    registered: u32,
    this: FromWcharFn,
}

/// Look `w` up in the carrier's Unicode -> emoji tables (`mb_tbl_uni_*2code*`):
/// the (ku*94)+ten value, if any. Ranges are tested in order, exactly like the
/// C `else if` chain (so a hit in a range with no match yields `None`).
fn uni2emoji(carrier: Carrier, w: u32) -> Option<u32> {
    let t = match carrier {
        Carrier::Docomo => &DOCOMO_TABLES,
        Carrier::Kddi => &KDDI_TABLES,
        Carrier::Softbank => &SB_TABLES,
    };
    let w = i64::from(w);
    for (k, (key, val, min, max)) in t.iter().enumerate() {
        if w >= i64::from(*min) && w <= i64::from(*max) {
            let off = [0, 0x10000, 0xF0000][k];
            return bisec_srch2(w - off, key).map(|i| u32::from(val[i]));
        }
    }
    None
}

/// One `mb_tbl_uni_*2code*` table: (keys, values, min, max).
type UniEmojiTable = (&'static [u16], &'static [u16], i32, i32);

/// The BMP (`code2`), SMP (`code3`, keys minus 0x10000) and plane 15 PUA
/// (`code5`, keys minus 0xF0000) tables of one carrier.
static DOCOMO_TABLES: [UniEmojiTable; 3] = [
    (
        &em::MB_TBL_UNI_DOCOMO2CODE2_KEY,
        &em::MB_TBL_UNI_DOCOMO2CODE2_VALUE,
        em::MB_TBL_UNI_DOCOMO2CODE2_MIN,
        em::MB_TBL_UNI_DOCOMO2CODE2_MAX,
    ),
    (
        &em::MB_TBL_UNI_DOCOMO2CODE3_KEY,
        &em::MB_TBL_UNI_DOCOMO2CODE3_VALUE,
        em::MB_TBL_UNI_DOCOMO2CODE3_MIN,
        em::MB_TBL_UNI_DOCOMO2CODE3_MAX,
    ),
    (
        &em::MB_TBL_UNI_DOCOMO2CODE5_KEY,
        &em::MB_TBL_UNI_DOCOMO2CODE5_VAL,
        em::MB_TBL_UNI_DOCOMO2CODE5_MIN,
        em::MB_TBL_UNI_DOCOMO2CODE5_MAX,
    ),
];

static KDDI_TABLES: [UniEmojiTable; 3] = [
    (
        &em::MB_TBL_UNI_KDDI2CODE2_KEY,
        &em::MB_TBL_UNI_KDDI2CODE2_VALUE,
        em::MB_TBL_UNI_KDDI2CODE2_MIN,
        em::MB_TBL_UNI_KDDI2CODE2_MAX,
    ),
    (
        &em::MB_TBL_UNI_KDDI2CODE3_KEY,
        &em::MB_TBL_UNI_KDDI2CODE3_VALUE,
        em::MB_TBL_UNI_KDDI2CODE3_MIN,
        em::MB_TBL_UNI_KDDI2CODE3_MAX,
    ),
    (
        &em::MB_TBL_UNI_KDDI2CODE5_KEY,
        &em::MB_TBL_UNI_KDDI2CODE5_VAL,
        em::MB_TBL_UNI_KDDI2CODE5_MIN,
        em::MB_TBL_UNI_KDDI2CODE5_MAX,
    ),
];

static SB_TABLES: [UniEmojiTable; 3] = [
    (
        &em::MB_TBL_UNI_SB2CODE2_KEY,
        &em::MB_TBL_UNI_SB2CODE2_VALUE,
        em::MB_TBL_UNI_SB2CODE2_MIN,
        em::MB_TBL_UNI_SB2CODE2_MAX,
    ),
    (
        &em::MB_TBL_UNI_SB2CODE3_KEY,
        &em::MB_TBL_UNI_SB2CODE3_VALUE,
        em::MB_TBL_UNI_SB2CODE3_MIN,
        em::MB_TBL_UNI_SB2CODE3_MAX,
    ),
    (
        &em::MB_TBL_UNI_SB2CODE5_KEY,
        &em::MB_TBL_UNI_SB2CODE5_VAL,
        em::MB_TBL_UNI_SB2CODE5_MIN,
        em::MB_TBL_UNI_SB2CODE5_MAX,
    ),
];

/// Unicode -> (ku*94)+ten emoji lookup for one carrier (the `mb_tbl_uni_*`
/// tables): used by the mobile Shift-JIS, ISO-2022-JP-KDDI and mobile UTF-8
/// encoders. Returns the carrier's (ku*94)+ten code, if `w` is one of its
/// emoji (keypad and flag sequences are not handled here).
pub fn unicode_to_emoji_docomo(w: u32) -> Option<u32> {
    uni2emoji(Carrier::Docomo, w)
}

/// KDDI variant of [`unicode_to_emoji_docomo`].
pub fn unicode_to_emoji_kddi(w: u32) -> Option<u32> {
    uni2emoji(Carrier::Kddi, w)
}

/// `SoftBank` variant of [`unicode_to_emoji_docomo`].
pub fn unicode_to_emoji_sb(w: u32) -> Option<u32> {
    uni2emoji(Carrier::Softbank, w)
}

/// Is `w` a Regional Indicator which can start one of the flag emoji
/// (`NFLAGS('C')..=NFLAGS('U')`)?
#[allow(clippy::cast_sign_loss)]
fn is_flag_start(w: u32) -> bool {
    (nflags(b'C') as u32..=nflags(b'U') as u32).contains(&w)
}

/// The carrier's (ku*94)+ten code for the flag emoji `w w2`, if any.
#[allow(clippy::cast_sign_loss)]
fn flag_code(w: u32, w2: u32, codes: &[i32; 10]) -> Option<u32> {
    if !(nflags(b'B') as u32..=nflags(b'U') as u32).contains(&w2) {
        return None;
    }
    NFLAGS_S
        .iter()
        .position(|cc| w == nflags(cc[0]) as u32 && w2 == nflags(cc[1]) as u32)
        .map(|i| codes[i] as u32)
}

/// Shared body of `mb_wchar_to_sjis_{docomo,kddi,sb}`. `buf.state` holds a
/// code point which must be reprocessed on the next call (a possible keypad or
/// flag sequence start at the end of a buffer).
#[allow(clippy::too_many_lines)]
fn wchar_to_sjis_mobile(input: &[u32], buf: &mut ConvertBuf, end: bool, p: &MobileEnc) {
    let this = p.this;
    let mut idx = 0usize;
    let mut pending = None;
    if buf.state != 0 {
        // Continue what we were doing on the previous call
        pending = Some(buf.state);
        buf.state = 0;
    }

    loop {
        let w = if let Some(w) = pending.take() {
            w
        } else if idx < input.len() {
            idx += 1;
            input[idx - 1]
        } else {
            break;
        };

        let mut s;
        let mut skip_to_emoji = false;
        if (0xE000..0xE000 + 20 * 94).contains(&w) {
            // Private User Area (95ku - 114ku)
            let v = w - 0xE000;
            s = (((v / 94) + 0x7F) << 8) | ((v % 94) + 0x21);
            skip_to_emoji = true;
        } else {
            s = ucs_to_jis(w);
        }

        if !skip_to_emoji {
            if s == 0 {
                s = match w {
                    0xA5 => 0x216F,   // YEN SIGN -> FULLWIDTH YEN SIGN
                    0xFF3C => 0x2140, // FULLWIDTH REVERSE SOLIDUS
                    0x2225 => 0x2142, // PARALLEL TO
                    0xFF0D => 0x215D, // FULLWIDTH HYPHEN-MINUS
                    0xFFE0 => 0x2171, // FULLWIDTH CENT SIGN
                    0xFFE1 => 0x2172, // FULLWIDTH POUND SIGN
                    0xFFE2 => 0x224C, // FULLWIDTH NOT SIGN
                    _ => 0,
                };
            }

            if w != 0 && (s == 0 || s >= 0x8080) {
                s = cp932_ext12_lookup(w).unwrap_or(0);
            }
        }

        // process_emoji:
        // When converting SJIS-Mobile to Unicode, we convert keypad symbol
        // emoji to a sequence of 2 codepoints, one of which is a combining
        // character which adds the 'key' image around the other. In the other
        // direction, look for such sequences and convert them to a single emoji
        if w == u32::from(b'#') || (u32::from(b'0')..=u32::from(b'9')).contains(&w) {
            if idx == input.len() {
                if !end {
                    // Another buffer of codepoints is coming; remember to
                    // reprocess `w`
                    buf.state = w;
                    break;
                }
            } else {
                let w2 = input[idx];
                idx += 1;
                if w2 == 0x20E3 {
                    let k = if w == u32::from(b'#') {
                        p.keypad[0]
                    } else if w == u32::from(b'0') {
                        p.keypad[1]
                    } else {
                        // Previous character was '1'-'9'
                        p.keypad[2] + (w - u32::from(b'1'))
                    };
                    s = kuten_to_jis(k);
                } else {
                    idx -= 1;
                }
            }
        } else if p.carrier != Carrier::Docomo && is_flag_start(w) {
            // C for CN, U for US
            if idx == input.len() {
                if end {
                    illegal_output(w, this, buf);
                } else {
                    // Reprocess `w` when called again with another buffer
                    buf.state = w;
                }
                break;
            }
            let w2 = input[idx];
            idx += 1;
            let codes = if p.carrier == Carrier::Kddi {
                &NFLAGS_CODE_KDDI
            } else {
                &NFLAGS_CODE_SB
            };
            if let Some(code) = flag_code(w, w2, codes) {
                s = kuten_to_jis(code);
            } else {
                idx -= 1;
                illegal_output(w, this, buf);
                continue;
            }
        } else if w == 0xA9 {
            // Copyright sign
            s = kuten_to_jis(p.copyright);
        } else if w == 0xAE {
            // Registered sign
            s = kuten_to_jis(p.registered);
        } else if let Some(code) = uni2emoji(p.carrier, w) {
            s = kuten_to_jis(code);
        }

        // emit_output:
        if s == 0 && w != 0 {
            illegal_output(w, this, buf);
        } else {
            emit(buf, s);
        }
    }
}

/// Search the NEC row 13 and IBM extension tables (linear, like the C code)
/// for `w`; returns its JIS row/cell.
#[allow(clippy::cast_possible_truncation)]
fn cp932_ext12_lookup(w: u32) -> Option<u32> {
    let n1 = (EXT1_MAX - EXT1_MIN) as usize;
    if let Some(i) = x::CP932EXT1_UCS_TABLE[..n1]
        .iter()
        .position(|&v| u32::from(v) == w)
    {
        let i = i as u32;
        return Some((((i / 94) + 0x2D) << 8) + (i % 94) + 0x21);
    }
    let n2 = (EXT2_MAX - EXT2_MIN) as usize;
    if let Some(i) = x::CP932EXT2_UCS_TABLE[..n2]
        .iter()
        .position(|&v| u32::from(v) == w)
    {
        let i = i as u32;
        return Some((((i / 94) + 0x79) << 8) + (i % 94) + 0x21);
    }
    None
}

static DOCOMO_ENC: MobileEnc = MobileEnc {
    carrier: Carrier::Docomo,
    keypad: [0x2964, 0x296F, 0x2966],
    copyright: 0x29B5,
    registered: 0x29BA,
    this: wchar_to_sjis_docomo,
};

static KDDI_ENC: MobileEnc = MobileEnc {
    carrier: Carrier::Kddi,
    keypad: [0x25BC, 0x2830, 0x27A6],
    copyright: 0x27DC,
    registered: 0x27DD,
    this: wchar_to_sjis_kddi,
};

static SB_ENC: MobileEnc = MobileEnc {
    carrier: Carrier::Softbank,
    keypad: [0x2817, 0x282C, 0x2823],
    copyright: 0x2855,
    registered: 0x2856,
    this: wchar_to_sjis_sb,
};

/// `mb_wchar_to_sjis_docomo`.
pub fn wchar_to_sjis_docomo(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    wchar_to_sjis_mobile(input, buf, end, &DOCOMO_ENC);
}

/// `mb_wchar_to_sjis_kddi`.
pub fn wchar_to_sjis_kddi(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    wchar_to_sjis_mobile(input, buf, end, &KDDI_ENC);
}

/// `mb_wchar_to_sjis_sb`.
pub fn wchar_to_sjis_sb(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    wchar_to_sjis_mobile(input, buf, end, &SB_ENC);
}

// ---------------------------------------------------------------------------
// CP932 / SJIS-win
// ---------------------------------------------------------------------------

/// `mb_cp932_to_wchar` (also used for `SJIS-win`).
pub fn cp932_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let s = *input;
    let e = s.len();
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len();

    while p < e && out < limit {
        let c = s[p];
        p += 1;

        if c < 0x80 {
            buf[out] = u32::from(c);
        } else if c > 0xA0 && c < 0xE0 {
            // Kana
            buf[out] = 0xFEC0 + u32::from(c);
        } else {
            if p == e {
                buf[out] = BAD_INPUT;
                out += 1;
                break;
            }
            let c2 = s[p];
            p += 1;
            let s = sjis_mobile_index(c, c2);
            let mut w = if s <= 137 {
                win_row1(s).unwrap_or(0)
            } else {
                0
            };

            if w == 0 {
                if (EXT1_MIN..EXT1_MAX).contains(&s) {
                    w = at(&x::CP932EXT1_UCS_TABLE, s - EXT1_MIN);
                } else if s < JISX0208_SIZE {
                    w = at(&j::JISX0208_UCS_TABLE, s);
                } else if (EXT2_MIN..EXT2_MAX).contains(&s) {
                    w = at(&x::CP932EXT2_UCS_TABLE, s - EXT2_MIN);
                } else if (EXT3_MIN..EXT3_MAX).contains(&s) {
                    w = at(&x::CP932EXT3_UCS_TABLE, s - EXT3_MIN);
                } else if (94 * 94..114 * 94).contains(&s) {
                    w = s - 94 * 94 + 0xE000;
                }
            }

            if w == 0 {
                if c == 0x80 || c == 0xA0 || c >= 0xFD {
                    p -= 1;
                }
                w = BAD_INPUT;
            }
            buf[out] = w;
        }
        out += 1;
    }

    *input = &s[p..];
    out
}

/// Shared body of `mb_wchar_to_cp932` and `mb_wchar_to_sjiswin` (`win`): they
/// differ only in how U+00A5 and U+203E are mapped. Both use
/// `mb_wchar_to_cp932` to encode error markers.
#[allow(clippy::cast_possible_truncation)]
fn wchar_to_cp932_common(input: &[u32], buf: &mut ConvertBuf, win: bool) {
    for &w in input {
        let mut s2 = false;
        let mut s1 = if !win && w == 0x203E {
            0x7E
        } else if (0xE000..0xE000 + 20 * 94).contains(&w) {
            let v = w - 0xE000;
            s2 = true;
            ((v / 94 + 0x7F) << 8) | (v % 94 + 0x21)
        } else {
            ucs_to_jis(w)
        };

        match w {
            // YEN SIGN
            0xA5 => s1 = if win { 0x216F } else { 0x5C },
            0xFF3C => s1 = 0x2140, // FULLWIDTH REVERSE SOLIDUS
            0x2225 => s1 = 0x2142, // PARALLEL TO
            0xFF0D => s1 = 0x215D, // FULLWIDTH HYPHEN-MINUS
            0xFFE0 => s1 = 0x2171, // FULLWIDTH CENT SIGN
            0xFFE1 => s1 = 0x2172, // FULLWIDTH POUND SIGN
            0xFFE2 => s1 = 0x224C, // FULLWIDTH NOT SIGN
            0 => {
                buf.push(0);
                continue;
            }
            _ => {}
        }

        if s1 == 0 || (s1 >= 0x8080 && !s2) {
            // not found or X 0212
            if let Some(v) =
                binary_search_paired_sorted_table(w, &x::CP932EXT1_UCS_TABLE_PAIRED_SORTED)
            {
                let v = u32::from(v);
                s1 = ((v / 94 + 0x2D) << 8) + (v % 94 + 0x21);
            } else if let Some(v) =
                binary_search_paired_sorted_table(w, &x::CP932EXT3_UCS_TABLE_PAIRED_SORTED)
            {
                let v = u32::from(v);
                s1 = ((v / 94 + 0x93) << 8) + (v % 94 + 0x21);
            } else {
                illegal_output(w, wchar_to_cp932, buf);
                continue;
            }
        }

        if s1 < 0x100 {
            buf.push(s1 as u8);
        } else {
            push_sjis(buf, s1);
        }
    }
}

/// `mb_wchar_to_cp932`.
pub fn wchar_to_cp932(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    wchar_to_cp932_common(input, buf, false);
}

/// `mb_wchar_to_sjiswin`.
pub fn wchar_to_sjiswin(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    wchar_to_cp932_common(input, buf, true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codecs::cjk::testutil as tu;
    use std::collections::{BTreeMap, HashMap};

    type Valid = BTreeMap<Vec<u8>, Vec<u32>>;
    type FromUni = HashMap<Vec<u32>, Vec<u8>>;

    /// `readConversionTable`-like loader: (bytes -> cps, cps -> bytes with the
    /// last entry winning); `None` if the data file is missing.
    #[allow(clippy::cast_possible_truncation)]
    fn load(name: &str) -> Option<(Valid, FromUni, HashMap<Vec<u32>, usize>)> {
        let text = tu::data_file(name)?;
        let mut valid = Valid::new();
        let mut from = FromUni::new();
        let mut count = HashMap::new();
        for toks in tu::mapping_lines(&text) {
            if toks.len() < 2 || toks[1] == [0xFFFD] {
                continue;
            }
            let bytes = tu::code_bytes(toks[0][0]);
            let cps: Vec<u32> = toks[1].iter().map(|&c| c as u32).collect();
            valid.insert(bytes.clone(), cps.clone());
            *count.entry(cps.clone()).or_insert(0) += 1;
            from.insert(cps, bytes);
        }
        Some((valid, from, count))
    }

    fn check_decode_all(enc: &'static Encoding, valid: &Valid) {
        for (bytes, cps) in valid {
            assert_eq!(
                &tu::decode(enc, bytes),
                cps,
                "{} decoding {}",
                enc.name,
                tu::hex(bytes)
            );
        }
        // Everything concatenated, decoded with the smallest buffer
        let all: Vec<u8> = valid.keys().flatten().copied().collect();
        let expected: Vec<u32> = valid.values().flatten().copied().collect();
        assert_eq!(tu::decode_bufsize(enc, &all, 5), expected, "{}", enc.name);
    }

    fn check_encode(enc: &'static Encoding, cps: &[u32], bytes: &[u8]) {
        let (out, errors) = tu::encode(enc, cps);
        assert_eq!(
            (tu::hex(&out), errors),
            (tu::hex(bytes), 0),
            "{} encoding {cps:X?}",
            enc.name
        );
    }

    /// Every 2-byte sequence starting with one of `leads` which is not a valid
    /// character decodes to exactly one error marker (followed by the next
    /// character), and a lone lead byte is an error.
    fn check_invalid_pairs(enc: &'static Encoding, valid: &Valid, leads: &[u8]) {
        for &lead in leads {
            assert_eq!(
                tu::decode(enc, &[lead]),
                [BAD_INPUT],
                "{} lone {lead:02X}",
                enc.name
            );
            for trail in 0..=255u8 {
                let pair = [lead, trail];
                if valid.contains_key(&pair[..]) {
                    continue;
                }
                assert_eq!(
                    tu::decode(enc, &[lead, trail, b'A']),
                    [BAD_INPUT, 0x41],
                    "{} {lead:02X}{trail:02X}",
                    enc.name
                );
            }
        }
    }

    fn controls(valid: &mut Valid, from: &mut FromUni) {
        for c in (0..0x20).chain([0x7F]) {
            valid.insert(vec![c], vec![u32::from(c)]);
            from.insert(vec![u32::from(c)], vec![c]);
        }
    }

    #[test]
    fn sjis_mapping_table() {
        let Some((mut valid, mut from, _)) = load("SHIFTJIS.txt") else {
            return;
        };
        controls(&mut valid, &mut from);
        for (b, u) in [(&b"\x5C"[..], 0x5C), (b"\x7E", 0x7E), (b"\x81\x5F", 0xFF3C)] {
            valid.insert(b.to_vec(), vec![u]);
            from.insert(vec![u], b.to_vec());
        }
        for (u, b) in [
            (0x203E, &b"\x81\x50"[..]),
            (0xAF, b"\x81\x50"),
            (0xA5, b"\x81\x8F"),
            (0xFFE2, b"\x81\xCA"),
            (0xFFE1, b"\x81\x92"),
            (0xFFE0, b"\x81\x91"),
            (0xFF5E, b"\x81\x60"),
            (0xFF0D, b"\x81\x7C"),
            (0x2225, b"\x81\x61"),
        ] {
            from.insert(vec![u], b.to_vec());
        }
        check_decode_all(&ENC_SJIS, &valid);
        for (cps, bytes) in &from {
            check_encode(&ENC_SJIS, cps, bytes);
        }
        let leads: Vec<u8> = (0x81..=0x9F).chain(0xE0..=0xEF).collect();
        check_invalid_pairs(&ENC_SJIS, &valid, &leads);
        // Unmappable code points
        assert_eq!(
            tu::encode(&ENC_SJIS, &[0x41, 0x1F600, 0x42]),
            (b"A?B".to_vec(), 1)
        );
    }

    fn cp932_valid() -> Option<(Valid, FromUni, HashMap<Vec<u32>, usize>)> {
        let (mut valid, mut from, count) = load("CP932.txt")?;
        let mut cp = 0xE000;
        for i in 0xF0..=0xF9u8 {
            for j in 0x40..=0xFCu8 {
                if j != 0x7F {
                    valid.insert(vec![i, j], vec![cp]);
                    from.insert(vec![cp], vec![i, j]);
                    cp += 1;
                }
            }
        }
        Some((valid, from, count))
    }

    #[test]
    fn cp932_and_sjiswin_mapping_table() {
        let Some((valid, from, count)) = cp932_valid() else {
            return;
        };
        for (enc, yen, overline) in [
            (&ENC_CP932, &b"\x5C"[..], &b"\x7E"[..]),
            (&ENC_SJISWIN, b"\x81\x8F", b"\x81\x50"),
        ] {
            check_decode_all(enc, &valid);
            // Code points with a unique mapping encode to it; ambiguous ones
            // (NEC/IBM duplicates) at least round-trip
            for (cps, bytes) in &from {
                if matches!(cps[0], 0xA5 | 0x203E) {
                    continue;
                }
                if count.get(cps).copied().unwrap_or(1) == 1 {
                    check_encode(enc, cps, bytes);
                } else {
                    let (out, errors) = tu::encode(enc, cps);
                    assert_eq!(errors, 0);
                    assert_eq!(&tu::decode(enc, &out), cps);
                }
            }
            for (u, b) in [
                (0xA2, &b"\x81\x91"[..]),
                (0xA3, b"\x81\x92"),
                (0x301C, b"\x81\x60"),
                (0x2212, b"\x81\x7C"),
                (0x2016, b"\x81\x61"),
                (0xAC, b"\x81\xCA"),
                (0xAF, b"\x81\x50"),
                (0xA5, yen),
                (0x203E, overline),
                // Duplicates: prefer 0x81, then 0x87, then 0xFA..
                (0x2252, b"\x81\xE0"),
                (0x2235, b"\x81\xE6"),
                (0x2170, b"\xFA\x40"),
                (0xFFE2, b"\x81\xCA"),
            ] {
                check_encode(enc, &[u], b);
            }
            let leads: Vec<u8> = (0x81..=0x9F).chain(0xE0..=0xFC).collect();
            check_invalid_pairs(enc, &valid, &leads);
        }
        // SJIS-win reports errors through the CP932 encoder
        assert_eq!(tu::encode(&ENC_SJISWIN, &[0x1F600]), (b"?".to_vec(), 1));
    }

    #[test]
    fn sjismac_mapping_table() {
        let Some((mut valid, mut from, _)) = load("MacJapanese-SJIS.txt") else {
            return;
        };
        controls(&mut valid, &mut from);
        for (u, b) in [
            (0x2015, &b"\x81\x5C"[..]),
            (0x203E, b"\x81\x50"),
            (0xAF, b"\x81\x50"),
            (0xFF5E, b"\x81\x60"),
        ] {
            from.insert(vec![u], b.to_vec());
        }
        check_decode_all(&ENC_SJIS_MAC, &valid);
        for (cps, bytes) in &from {
            check_encode(&ENC_SJIS_MAC, cps, bytes);
            // Also when every code point arrives in a separate call (except
            // for the C quirk tested in `sjismac_split_state_quirk`)
            if cps[..] != [0xF860, 0x30, 0x2E] {
                assert_eq!(&tu::encode_split(&ENC_SJIS_MAC, cps), bytes, "{cps:X?}");
            }
        }
        let leads: Vec<u8> = (0x81..=0x9F).chain(0xE0..=0xED).collect();
        check_invalid_pairs(&ENC_SJIS_MAC, &valid, &leads);

        // From sjismac_encoding.phpt
        let e = &ENC_SJIS_MAC;
        check_encode(e, &[0x2026, 0x6BAA], b"\x81\x63\x9F\x6F");
        assert_eq!(tu::encode(e, &[0x2010, 0xF87A]), (b"\x81\x5D?".to_vec(), 1));
        assert_eq!(tu::encode(e, &[0x2010, 0x20DD]), (b"\x81\x5D?".to_vec(), 1));
        assert_eq!(tu::encode(e, &[0x2010, 0xF87F]), (b"\x81\x5D?".to_vec(), 1));
        assert_eq!(tu::encode(e, &[0x21E6, 0xF87E]), (b"\x86\xD0?".to_vec(), 1));
        assert_eq!(tu::encode(e, &[0xF860, 0x30, 0x1234]), (b"???".to_vec(), 3));
        assert_eq!(
            tu::encode(e, &[0xF861, 0x46, 0x41, 0x1234]),
            (b"????".to_vec(), 4)
        );
    }

    #[test]
    fn sjismac_split_state_quirk() {
        // Row 0 of code_tbl_m gives a saved state with a zero top byte, so
        // (as in C) the hint is restarted instead of resumed.
        let cps = [0xF860, 0x30, 0x2E];
        assert_eq!(tu::encode(&ENC_SJIS_MAC, &cps).0, b"\x85\x91");
        assert_eq!(tu::encode_split(&ENC_SJIS_MAC, &cps), b"?.");
        // Other rows resume properly
        let cps = [0xF862, 0x58, 0x49, 0x49, 0x49];
        assert_eq!(
            tu::encode_split(&ENC_SJIS_MAC, &cps),
            tu::encode(&ENC_SJIS_MAC, &cps).0
        );
    }

    /// `EmojiSources.txt`: (unicode sequence, docomo, kddi, softbank codes).
    #[allow(clippy::type_complexity)]
    fn emoji_sources() -> Option<Vec<(Vec<u32>, [Option<u16>; 3])>> {
        let text = tu::data_file("EmojiSources.txt")?;
        let mut out = Vec::new();
        for line in text.lines() {
            if line.starts_with('#') {
                continue;
            }
            let f: Vec<&str> = line.trim_end().split(';').collect();
            if f.len() < 4 {
                continue;
            }
            let cps: Vec<u32> = f[0]
                .split(' ')
                .filter_map(|h| u32::from_str_radix(h, 16).ok())
                .collect();
            let code = |s: &str| u16::from_str_radix(s, 16).ok();
            out.push((cps, [code(f[1]), code(f[2]), code(f[3])]));
        }
        Some(out)
    }

    #[test]
    fn mobile_mapping_tables() {
        let Some((valid, _, _)) = cp932_valid() else {
            return;
        };
        let Some(emoji) = emoji_sources() else {
            return;
        };
        let encs = [&ENC_SJIS_DOCOMO, &ENC_SJIS_KDDI, &ENC_SJIS_SB];
        let mut carrier_valid: Vec<Valid> = vec![Valid::new(); 3];
        for (k, enc) in encs.iter().enumerate() {
            // CP932 characters outside the emoji/user-defined area
            for (bytes, cps) in &valid {
                // (ESC starts an emoji escape sequence in SoftBank)
                if bytes[0] < 0xF0 && !(k == 2 && bytes[..] == [0x1B]) {
                    carrier_valid[k].insert(bytes.clone(), cps.clone());
                    assert_eq!(
                        &tu::decode(enc, bytes),
                        cps,
                        "{} {}",
                        enc.name,
                        tu::hex(bytes)
                    );
                }
            }
            for (cps, codes) in &emoji {
                if let Some(code) = codes[k] {
                    let bytes = code.to_be_bytes();
                    carrier_valid[k].insert(bytes.to_vec(), cps.clone());
                    assert_eq!(&tu::decode(enc, &bytes), cps, "{} {code:04X}", enc.name);
                    // Encoding the Unicode sequence gives back an emoji which
                    // decodes to the same sequence
                    let (out, errors) = tu::encode(enc, cps);
                    assert_eq!(errors, 0, "{} {cps:X?}", enc.name);
                    assert_eq!(&tu::decode(enc, &out), cps, "{} {cps:X?}", enc.name);
                    assert_eq!(tu::encode_split(enc, cps), out, "{} {cps:X?}", enc.name);
                }
            }
            // Smallest buffers give the same result
            let all: Vec<u8> = carrier_valid[k].keys().flatten().copied().collect();
            assert_eq!(tu::decode_bufsize(enc, &all, 5), tu::decode(enc, &all));
        }
        // Lone lead bytes and invalid trail bytes
        for enc in encs {
            assert_eq!(tu::decode(enc, b"\x81"), [BAD_INPUT]);
            assert_eq!(tu::decode(enc, b"\x81\x20A"), [BAD_INPUT, 0x41]);
            assert_eq!(tu::decode(enc, b"\x80A"), [BAD_INPUT, 0x41]);
            assert_eq!(tu::decode(enc, b"\xFD\x40"), [BAD_INPUT, 0x40]);
        }
    }

    #[test]
    fn mobile_keypad_flags_and_state() {
        let cases: &[(&Encoding, &[u32], &[u8])] = &[
            (&ENC_SJIS_DOCOMO, &[0x31, 0x20E3], b"\xF9\x87"),
            (&ENC_SJIS_DOCOMO, &[0x23, 0x20E3], b"\xF9\x85"),
            (&ENC_SJIS_DOCOMO, &[0x23], b"#"),
            (&ENC_SJIS_KDDI, &[0x30, 0x20E3], b"\xF7\xC9"),
            (&ENC_SJIS_KDDI, &[0x1F1EF, 0x1F1F5], b"\xF6\xA5"),
            (
                &ENC_SJIS_SB,
                &[0x31, 0x20E3, 0x23, 0x20E3],
                b"\xF7\xBC\xF7\xB0",
            ),
            (&ENC_SJIS_SB, &[0x1F1EF, 0x1F1F5], b"\xFB\xAB"),
        ];
        for &(enc, cps, bytes) in cases {
            check_encode(enc, cps, bytes);
            assert_eq!(tu::encode_split(enc, cps), bytes);
        }
        // A lone Regional Indicator is an error; a pending one is flushed at
        // the end of input
        assert_eq!(tu::encode(&ENC_SJIS_KDDI, &[0x1F1EF]), (b"?".to_vec(), 1));
        assert_eq!(
            tu::encode(&ENC_SJIS_SB, &[0x1F1EF, 0x41]),
            (b"?A".to_vec(), 1)
        );
        assert_eq!(tu::encode_split(&ENC_SJIS_SB, &[0x1F1EF]), b"?");
        // SoftBank escape sequences, also resumed across calls via `state`
        let mut state = 0;
        let mut buf = [0u32; 8];
        let mut input: &[u8] = b"\x1B$E\"";
        let n = sjis_sb_to_wchar(&mut input, &mut buf, &mut state);
        assert_eq!(state, u32::from(b'E'));
        let first = buf[..n].to_vec();
        let mut input: &[u8] = b"\"\x0FA";
        let n = sjis_sb_to_wchar(&mut input, &mut buf, &mut state);
        assert_eq!(state, 0);
        assert_eq!(first.len(), 1);
        assert_eq!(&buf[..n], &[first[0], 0x41]);
    }

    /// Decoding with the minimal buffer size gives the same result as with
    /// 128-code-point buffers, for every decoder input in the oracle fixture.
    #[test]
    fn small_buffers_match() {
        let text = std::fs::read_to_string(tu::fixture_path("sjis_oracle.tsv")).unwrap_or_default();
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f[0] != "conv" || f[2] != "UTF-8" {
                continue;
            }
            let enc = tu::enc(f[1]);
            let input: Vec<u8> = (0..f[4].len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&f[4][i..i + 2], 16).unwrap_or(0))
                .collect();
            for size in [5, 6, 8] {
                assert_eq!(
                    tu::decode_bufsize(enc, &input, size),
                    tu::decode(enc, &input),
                    "{} {} bufsize {size}",
                    f[1],
                    f[4]
                );
            }
        }
    }

    #[test]
    fn oracle_fixture() {
        let n = tu::replay_oracle("sjis_oracle.tsv");
        assert!(n > 1000);
    }

    #[test]
    fn mblen_tables() {
        assert_eq!(ENC_SJIS.mblen_table.map(|t| t[0x81]), Some(2));
        assert_eq!(ENC_SJIS.mblen_table.map(|t| t[0xF0]), Some(1));
        assert_eq!(ENC_SJIS_MAC.mblen_table.map(|t| t[0xEE]), Some(1));
        assert_eq!(ENC_SJIS_DOCOMO.mblen_table.map(|t| t[0xFC]), Some(2));
        assert_eq!(ENC_CP932.mblen_table.map(|t| t[0xFF]), Some(2));
    }
}

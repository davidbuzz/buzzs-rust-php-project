//! JIS X 0213:2004 encodings: `ISO-2022-JP-2004`, `EUC-JP-2004`,
//! `SJIS-2004`.
//!
//! JIS X 0213 contains some characters which Unicode represents as a base
//! character followed by a combining character; the encoders buffer such a
//! base character (in the encoder state) until they see what follows it.
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c` (`mbfl_filt_conv_jis2004_wchar`,
//! `mbfl_filt_conv_wchar_jis2004`, `mb_iso2022jp2004_to_wchar`,
//! `mb_wchar_to_iso2022jp2004`, `mb_sjis2004_to_wchar`,
//! `mb_wchar_to_sjis2004`, `mb_eucjp2004_to_wchar`, `mb_wchar_to_eucjp2004`).

use crate::codecs::cjk::common::{bisec_srch, bisec_srch2, sjis_decode, sjis_encode};
use crate::convert::{illegal_output, ConvertBuf, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};
use crate::legacy::{self, Filter, Sink, Vtbl, BAD_INPUT_I32};
use crate::tables::cjk::jis::JISX0208_UCS_TABLE;
use crate::tables::cjk::jis2004 as t;
use crate::tables::cjk::misc::{
    MBLEN_TABLE_EUCJP, MBLEN_TABLE_SJIS_MOBILE, SJIS_DECODE_TBL2, SJIS_MOBILE_DECODE_TBL1,
};

/// `ISO-2022-JP-2004`.
pub static ENC_2022JP_2004: Encoding = Encoding {
    no: EncodingNo::Iso2022Jp2004,
    name: "ISO-2022-JP-2004",
    mime_name: Some("ISO-2022-JP-2004"),
    aliases: &[],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: Some(&VTBL_2022JP_2004_WCHAR),
    output_filter: Some(&VTBL_WCHAR_2022JP_2004),
    to_wchar: iso2022jp2004_to_wchar,
    from_wchar: wchar_to_iso2022jp2004,
    check: None,
    cut: None,
};

/// `EUC-JP-2004`.
pub static ENC_EUCJP2004: Encoding = Encoding {
    no: EncodingNo::EucJp2004,
    name: "EUC-JP-2004",
    mime_name: Some("EUC-JP"),
    aliases: &["EUC_JP-2004"],
    mblen_table: Some(&MBLEN_TABLE_EUCJP),
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: eucjp2004_to_wchar,
    from_wchar: wchar_to_eucjp2004,
    check: None,
    cut: None,
};

/// `SJIS-2004`.
///
/// Leading byte values used for SJIS-2004 are the same as for the mobile
/// SJIS variants, hence the shared `mblen` table.
pub static ENC_SJIS2004: Encoding = Encoding {
    no: EncodingNo::Sjis2004,
    name: "SJIS-2004",
    mime_name: Some("Shift_JIS"),
    aliases: &["SJIS2004", "Shift_JIS-2004"],
    mblen_table: Some(&MBLEN_TABLE_SJIS_MOBILE),
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: sjis2004_to_wchar,
    from_wchar: wchar_to_sjis2004,
    check: None,
    cut: None,
};

/// `vtbl_2022jp_2004_wchar`.
pub static VTBL_2022JP_2004_WCHAR: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_jis2004_wchar,
    flush: filt_jis2004_wchar_flush,
};

/// `vtbl_wchar_2022jp_2004`.
pub static VTBL_WCHAR_2022JP_2004: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_wchar_jis2004,
    flush: filt_wchar_jis2004_flush,
};

// ---------------------------------------------------------------------------
// Shared table lookups

#[inline]
fn tbl(t: &[u16], i: u32) -> u32 {
    u32::from(t[i as usize])
}

/// Number of entries in `jisx0213_u2_key` (`jisx0213_u2_tbl_len`).
const U2_LEN: usize = t::JISX0213_U2_KEY.len();

/// Whether `w1` (JIS X 0213 plane 1 row/cell bytes as `(row << 8) | cell`)
/// may be a character which maps to a pair of Unicode code points.
#[inline]
fn is_combining_jis(w1: u32) -> bool {
    (0x2477..=0x2479).contains(&w1)
        || (0x2479..=0x247B).contains(&w1)
        || (0x2577..=0x257E).contains(&w1)
        || w1 == 0x2678
        || w1 == 0x2B44
        || (0x2B48..=0x2B4F).contains(&w1)
        || (0x2B65..=0x2B66).contains(&w1)
}

/// Whether `w` may be the first code point of a pair which JIS X 0213
/// represents as a single character.
#[inline]
fn is_combining_first(w: u32) -> bool {
    w == 0xE6
        || (0x254..=0x2E9).contains(&w)
        || (0x304B..=0x3053).contains(&w)
        || (0x30AB..=0x30C8).contains(&w)
        || w == 0x31F7
}

/// Look up `jisx0213_u2_key`-style index `k` (from a bisection over `keys`)
/// and return the two code points it maps to.
#[inline]
fn u2_pair(w1: u32, keys: &[u16]) -> Option<(u32, u32)> {
    #[allow(clippy::cast_possible_wrap)]
    let k = bisec_srch2(w1 as i32, keys);
    #[allow(clippy::cast_sign_loss)]
    (k >= 0).then(|| {
        let k = k as usize;
        (
            u32::from(t::JISX0213_U2_TBL[2 * k]),
            u32::from(t::JISX0213_U2_TBL[2 * k + 1]),
        )
    })
}

/// Code point for index `idx` of `jisx0213_ucs_table`, falling back to the
/// CJK Unified Ideographs extension B table; 0 if there is none.
#[inline]
fn jisx0213_lookup(idx: u32) -> u32 {
    let mut w = 0;
    if (idx as usize) < t::JISX0213_UCS_TABLE.len() {
        w = tbl(&t::JISX0213_UCS_TABLE, idx);
    }
    if w == 0 {
        #[allow(clippy::cast_possible_wrap)]
        let k = bisec_srch2(idx as i32, &t::JISX0213_JIS_U5_KEY);
        if k >= 0 {
            #[allow(clippy::cast_sign_loss)]
            {
                w = tbl(&t::JISX0213_JIS_U5_TBL, k as u32) + 0x20000;
            }
        }
    }
    w
}

/// Whether a zero-based JIS X 0213 plane 2 row/cell pair is in use.
#[inline]
fn plane2_valid(s1: u32, s2: u32) -> bool {
    ((s1 <= 4 && s1 != 1) || s1 == 7 || (11..=14).contains(&s1) || (77..94).contains(&s1))
        && s2 < 94
}

/// Decode a valid (see [`plane2_valid`]) zero-based plane 2 row/cell pair;
/// [`BAD_INPUT`] if it is unmapped.
fn plane2_to_ucs(s1: u32, s2: u32) -> u32 {
    // Calculate the offset from the row number. Every valid row is present
    // in `jisx0213_p2_ofst`.
    let k = t::JISX0213_P2_OFST
        .iter()
        .position(|&o| u32::from(o) == s1)
        .unwrap_or(t::JISX0213_P2_OFST.len() - 1);
    #[allow(clippy::cast_possible_truncation)]
    let k = (k as u32).wrapping_sub(u32::from(t::JISX0213_P2_OFST[k]));
    let s = (s1 + 94).wrapping_add(k) * 94 + s2;
    match jisx0213_lookup(s) {
        0 => BAD_INPUT,
        w => w,
    }
}

/// Decode JIS X 0213 plane 1 row/cell bytes (`0x21..=0x7E` each). Returns
/// one or two code points.
fn plane1_to_ucs(c1: u32, c2: u32) -> (u32, Option<u32>) {
    let w1 = (c1 << 8) | c2;
    if is_combining_jis(w1) {
        if let Some((a, b)) = u2_pair(w1, &t::JISX0213_U2_KEY) {
            return (a, Some(b));
        }
    }
    match jisx0213_lookup((c1 - 0x21) * 94 + c2 - 0x21) {
        0 => (BAD_INPUT, None),
        w => (w, None),
    }
}

/// Look `w` up in the Unicode -> JIS X 0213 tables (everything except the
/// combining sequences). Returns 0 if not found.
fn ucs_to_jisx0213(w: u32) -> u32 {
    let mut s = 0;

    // Check for major Japanese chars: U+4E00-U+9FFF (and other ranges)
    for (range, table) in t::UNI2JIS_TBL_RANGE.iter().zip(t::UNI2JIS_TBL.iter()) {
        #[allow(clippy::cast_sign_loss)]
        let (lo, hi) = (range[0] as u32, range[1] as u32);
        if (lo..=hi).contains(&w) {
            s = tbl(table, w - lo);
            break;
        }
    }

    // Check for Japanese chars in compressed mapping area: U+1E00-U+4DBF
    #[allow(clippy::cast_sign_loss)]
    if s == 0 && (t::UCS_C1_JISX0213_MIN as u32..=t::UCS_C1_JISX0213_MAX as u32).contains(&w) {
        #[allow(clippy::cast_possible_wrap)]
        let k = bisec_srch(w as i32, &t::UCS_C1_JISX0213_TBL);
        if k >= 0 {
            let k = k as u32;
            s = tbl(&t::UCS_C1_JISX0213_OFST, k) + w - tbl(&t::UCS_C1_JISX0213_TBL, 2 * k);
        }
    }

    // Check for Japanese chars in CJK Unified Ideographs ext B (U+2XXXX)
    #[allow(clippy::cast_sign_loss)]
    if s == 0 && (t::JISX0213_U5_TBL_MIN as u32..=t::JISX0213_U5_TBL_MAX as u32).contains(&w) {
        #[allow(clippy::cast_possible_wrap)]
        let k = bisec_srch2((w - 0x20000) as i32, &t::JISX0213_U5_JIS_KEY);
        if k >= 0 {
            s = tbl(&t::JISX0213_U5_JIS_TBL, k as u32);
        }
    }

    if s == 0 {
        // CJK Compatibility Forms: U+FE30-U+FE4F
        if w == 0xFE45 {
            s = 0x233E;
        } else if w == 0xFE46 {
            s = 0x233D;
        } else if (0xF91D..=0xF9DC).contains(&w) {
            // CJK Compatibility Ideographs: U+F900-U+F92A
            #[allow(clippy::cast_possible_wrap)]
            let k = bisec_srch2(w as i32, &t::UCS_R2B_JISX0213_CMAP_KEY);
            if k >= 0 {
                #[allow(clippy::cast_sign_loss)]
                {
                    s = tbl(&t::UCS_R2B_JISX0213_CMAP_VAL, k as u32);
                }
            }
        }
    }

    s
}

/// Outcome of checking for a code point pair which maps to a single JIS X
/// 0213 character.
enum Combining {
    /// `w` cannot start such a pair.
    No,
    /// `w` may start a pair (index `k` into `jisx0213_u2_tbl`), but the input
    /// ended; defer the decision to the next call.
    Defer(usize),
    /// The JIS X 0213 code to emit; `consumed_next` tells whether the
    /// following code point was used up.
    Found { s: u32, consumed_next: bool },
}

/// The combining-sequence check shared by the fast encoders.
fn check_combining(w: u32, next: Option<u32>, end: bool) -> Combining {
    if !is_combining_first(w) {
        return Combining::No;
    }
    for k in 0..U2_LEN {
        if w == u32::from(t::JISX0213_U2_TBL[2 * k]) {
            let mut k = k;
            match next {
                None => {
                    if !end {
                        return Combining::Defer(k);
                    }
                }
                Some(w2) => {
                    if matches!(w, 0x254 | 0x28C | 0x259 | 0x25A) && w2 == 0x301 {
                        k += 1;
                    }
                    if w2 == u32::from(t::JISX0213_U2_TBL[2 * k + 1]) {
                        return Combining::Found {
                            s: u32::from(t::JISX0213_U2_KEY[k]),
                            consumed_next: true,
                        };
                    }
                }
            }
            // Fallback
            return Combining::Found {
                s: u32::from(t::JISX0213_U2_FB_TBL[k]),
                consumed_next: false,
            };
        }
    }
    Combining::No
}

/// Emit the JIS X 0213 plane 2 code `s` (`0x7F21..`) as row/cell bytes.
#[inline]
#[allow(clippy::cast_possible_truncation)]
fn plane2_bytes(s: u32) -> (u8, u8) {
    let s2 = s & 0xFF;
    let k = ((s >> 8) & 0xFF) - 0x7F;
    let s1 = u32::from(t::JISX0213_P2_OFST[k as usize]) + 0x21;
    (s1 as u8, s2 as u8)
}

// ---------------------------------------------------------------------------
// ISO-2022-JP-2004 (fast)

const ASCII: u32 = 0;
const JISX0208: u32 = 1;
const JISX0213_PLANE1: u32 = 2;
const JISX0213_PLANE2: u32 = 3;

/// `mb_iso2022jp2004_to_wchar`.
#[allow(clippy::many_single_char_names, clippy::too_many_lines)]
pub fn iso2022jp2004_to_wchar(input: &mut &[u8], buf: &mut [u32], state: &mut u32) -> usize {
    let s = *input;
    let e = s.len();
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len() - 1;

    while p < e && out < limit {
        let c = s[p];
        p += 1;

        if c <= 0x7F {
            if c == 0x1B {
                if e - p < 2 {
                    buf[out] = BAD_INPUT;
                    out += 1;
                    p = e;
                    break;
                }
                let c2 = s[p];
                let c3 = s[p + 1];
                p += 2;
                if c2 == b'$' {
                    if c3 == b'B' {
                        *state = JISX0208;
                    } else if c3 == b'(' {
                        if p == e {
                            buf[out] = BAD_INPUT;
                            out += 1;
                            break;
                        }
                        let c4 = s[p];
                        p += 1;
                        if c4 == b'Q' {
                            *state = JISX0213_PLANE1;
                        } else if c4 == b'P' {
                            *state = JISX0213_PLANE2;
                        } else {
                            buf[out] = BAD_INPUT;
                            out += 1;
                        }
                    } else {
                        buf[out] = BAD_INPUT;
                        out += 1;
                    }
                } else if c2 == b'(' {
                    if c3 == b'B' {
                        *state = ASCII;
                    } else {
                        buf[out] = BAD_INPUT;
                        out += 1;
                    }
                } else {
                    p -= 1;
                    buf[out] = BAD_INPUT;
                    out += 1;
                }
            } else if *state >= JISX0208 && c > 0x20 && c < 0x7F {
                if p == e {
                    buf[out] = BAD_INPUT;
                    out += 1;
                    break;
                }
                let c2 = s[p];
                p += 1;
                if !(0x21..=0x7E).contains(&c2) {
                    buf[out] = BAD_INPUT;
                    out += 1;
                    continue;
                }
                let (c, c2) = (u32::from(c), u32::from(c2));

                if *state == JISX0213_PLANE1 {
                    let (w, w2) = plane1_to_ucs(c, c2);
                    buf[out] = w;
                    out += 1;
                    if let Some(w2) = w2 {
                        buf[out] = w2;
                        out += 1;
                    }
                } else if *state == JISX0213_PLANE2 {
                    let (s1, s2) = (c - 0x21, c2 - 0x21);
                    buf[out] = if plane2_valid(s1, s2) {
                        plane2_to_ucs(s1, s2)
                    } else {
                        BAD_INPUT
                    };
                    out += 1;
                } else {
                    // state == JISX0208
                    let s = (c - 0x21) * 94 + c2 - 0x21;
                    let mut w = 0;
                    if (s as usize) < JISX0208_UCS_TABLE.len() {
                        w = tbl(&JISX0208_UCS_TABLE, s);
                    }
                    buf[out] = if w == 0 { BAD_INPUT } else { w };
                    out += 1;
                }
            } else {
                buf[out] = u32::from(c);
                out += 1;
            }
        } else {
            buf[out] = BAD_INPUT;
            out += 1;
        }
    }

    *input = &s[p..];
    out
}

/// `mb_wchar_to_iso2022jp2004`.
///
/// `buf.state`: the current character set in the low byte; a pending
/// combining base character as `(k + 1) << 8` (index into
/// `jisx0213_u2_tbl`).
pub fn wchar_to_iso2022jp2004(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    let mut pending = None;
    if buf.state & 0xFF00 != 0 {
        let k = ((buf.state >> 8) - 1) as usize;
        pending = Some(u32::from(t::JISX0213_U2_TBL[2 * k]));
        buf.state &= 0xFF;
    }

    let mut i = 0;
    loop {
        let w = if let Some(w) = pending.take() {
            w
        } else if i < input.len() {
            i += 1;
            input[i - 1]
        } else {
            break;
        };

        let mut s = match check_combining(w, input.get(i).copied(), end) {
            Combining::Defer(k) => {
                #[allow(clippy::cast_possible_truncation)]
                {
                    buf.state |= (k as u32 + 1) << 8;
                }
                return;
            }
            Combining::Found { s, consumed_next } => {
                if consumed_next {
                    i += 1;
                }
                s
            }
            Combining::No => 0,
        };
        if s == 0 {
            s = ucs_to_jisx0213(w);
        }

        #[allow(clippy::cast_possible_truncation)]
        if s == 0 && w != 0 {
            illegal_output(w, wchar_to_iso2022jp2004, buf);
        } else if s <= 0x7F {
            if buf.state != ASCII {
                buf.extend(&[0x1B, b'(', b'B']);
                buf.state = ASCII;
            }
            buf.push(s as u8);
        } else if s <= 0xFF {
            illegal_output(w, wchar_to_iso2022jp2004, buf);
        } else if s <= 0x7EFF {
            if buf.state != JISX0213_PLANE1 {
                buf.extend(&[0x1B, b'$', b'(', b'Q']);
                buf.state = JISX0213_PLANE1;
            }
            buf.extend(&[((s >> 8) & 0xFF) as u8, (s & 0xFF) as u8]);
        } else {
            if buf.state != JISX0213_PLANE2 {
                buf.extend(&[0x1B, b'$', b'(', b'P']);
                buf.state = JISX0213_PLANE2;
            }
            let (s1, s2) = plane2_bytes(s);
            buf.extend(&[s1, s2]);
        }
    }

    if end && buf.state != ASCII {
        buf.extend(&[0x1B, b'(', b'B']);
    }
}

// ---------------------------------------------------------------------------
// SJIS-2004 (fast)

/// `mb_sjis2004_to_wchar`.
#[allow(clippy::many_single_char_names, clippy::too_many_lines)]
pub fn sjis2004_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let s = *input;
    let e = s.len();
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len() - 1;

    while p < e && out < limit {
        let c = s[p];
        p += 1;

        if c <= 0x7F {
            buf[out] = match c {
                0x5C => 0xA5,
                0x7E => 0x203E,
                _ => u32::from(c),
            };
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
            let w1 = u32::from(SJIS_MOBILE_DECODE_TBL1[usize::from(c)])
                + u32::from(SJIS_DECODE_TBL2[usize::from(c2)]);

            // Conversion for combining characters
            if (0x0170..=0x03F1).contains(&w1) {
                if let Some((a, b)) = u2_pair(w1, &t::JISX0213_U2_KEY_B) {
                    buf[out] = a;
                    buf[out + 1] = b;
                    out += 2;
                    continue;
                }
            }

            // Conversion for BMP and CJK Unified Ideographs extension B
            let w = jisx0213_lookup(w1);
            if w != 0 {
                buf[out] = w;
            } else {
                if c == 0x80 || c == 0xA0 || c >= 0xFD {
                    p -= 1;
                }
                buf[out] = BAD_INPUT;
            }
            out += 1;
        }
    }

    *input = &s[p..];
    out
}

/// `mb_wchar_to_sjis2004`. `buf.state` holds a pending combining base
/// character (or 0).
pub fn wchar_to_sjis2004(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    let mut pending = None;
    if buf.state != 0 {
        pending = Some(buf.state);
        buf.state = 0;
    }

    let mut i = 0;
    loop {
        let w = if let Some(w) = pending.take() {
            w
        } else if i < input.len() {
            i += 1;
            input[i - 1]
        } else {
            break;
        };

        let mut s = match check_combining(w, input.get(i).copied(), end) {
            Combining::Defer(_) => {
                buf.state = w;
                return;
            }
            Combining::Found { s, consumed_next } => {
                if consumed_next {
                    i += 1;
                }
                s
            }
            Combining::No => 0,
        };
        if s == 0 {
            s = ucs_to_jisx0213(w);
        }

        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_possible_wrap,
            clippy::cast_sign_loss
        )]
        if s == 0 && w != 0 {
            illegal_output(w, wchar_to_sjis2004, buf);
        } else if s <= 0xFF {
            buf.push(s as u8);
        } else {
            let (s1, s2) = sjis_encode(((s >> 8) & 0xFF) as i32, (s & 0xFF) as i32);
            buf.extend(&[s1 as u8, s2 as u8]);
        }
    }
}

// ---------------------------------------------------------------------------
// EUC-JP-2004 (fast)

/// `mb_eucjp2004_to_wchar`.
#[allow(clippy::many_single_char_names, clippy::too_many_lines)]
pub fn eucjp2004_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let s = *input;
    let e = s.len();
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len() - 1;

    while p < e && out < limit {
        let c = s[p];
        p += 1;

        if c <= 0x7F {
            buf[out] = u32::from(c);
            out += 1;
        } else if (0xA1..=0xFE).contains(&c) {
            // Kanji
            if p == e {
                buf[out] = BAD_INPUT;
                out += 1;
                break;
            }
            let c2 = s[p];
            p += 1;
            if c2 <= 0xA0 || c2 == 0xFF {
                buf[out] = BAD_INPUT;
                out += 1;
                continue;
            }
            let (w, w2) = plane1_to_ucs(u32::from(c) - 0x80, u32::from(c2) - 0x80);
            buf[out] = w;
            out += 1;
            if let Some(w2) = w2 {
                buf[out] = w2;
                out += 1;
            }
        } else if c == 0x8E && p < e {
            // Kana
            let c2 = s[p];
            p += 1;
            buf[out] = if (0xA1..=0xDF).contains(&c2) {
                0xFEC0 + u32::from(c2)
            } else {
                BAD_INPUT
            };
            out += 1;
        } else if c == 0x8F && p < e {
            let c2 = s[p];
            p += 1;
            if (c2 == 0xA1
                || (0xA3..=0xA5).contains(&c2)
                || c2 == 0xA8
                || (0xAC..=0xAF).contains(&c2)
                || (0xEE..=0xFE).contains(&c2))
                && p < e
            {
                let c3 = s[p];
                p += 1;
                if c3 < 0xA1 || c3 == 0xFF {
                    buf[out] = BAD_INPUT;
                    out += 1;
                    continue;
                }
                let (s1, s2) = (u32::from(c2) - 0xA1, u32::from(c3) - 0xA1);
                buf[out] = if plane2_valid(s1, s2) {
                    plane2_to_ucs(s1, s2)
                } else {
                    BAD_INPUT
                };
            } else {
                buf[out] = BAD_INPUT;
            }
            out += 1;
        } else {
            buf[out] = BAD_INPUT;
            out += 1;
        }
    }

    *input = &s[p..];
    out
}

/// `mb_wchar_to_eucjp2004`. `buf.state` holds a pending combining base
/// character (or 0).
pub fn wchar_to_eucjp2004(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    let mut pending = None;
    if buf.state != 0 {
        pending = Some(buf.state);
        buf.state = 0;
    }

    let mut i = 0;
    loop {
        let w = if let Some(w) = pending.take() {
            w
        } else if i < input.len() {
            i += 1;
            input[i - 1]
        } else {
            break;
        };

        let mut s = match check_combining(w, input.get(i).copied(), end) {
            Combining::Defer(_) => {
                buf.state = w;
                return;
            }
            Combining::Found { s, consumed_next } => {
                if consumed_next {
                    i += 1;
                }
                s
            }
            Combining::No => 0,
        };
        if s == 0 {
            s = ucs_to_jisx0213(w);
        }

        #[allow(clippy::cast_possible_truncation)]
        if s == 0 && w != 0 {
            illegal_output(w, wchar_to_eucjp2004, buf);
        } else if s <= 0x7F {
            buf.push(s as u8);
        } else if s <= 0xFF {
            buf.extend(&[0x8E, s as u8]);
        } else if s <= 0x7EFF {
            buf.extend(&[(((s >> 8) & 0xFF) + 0x80) as u8, ((s & 0xFF) + 0x80) as u8]);
        } else {
            let (s1, s2) = plane2_bytes(s);
            buf.extend(&[0x8F, s1 | 0x80, s2 | 0x80]);
        }
    }
}

// ---------------------------------------------------------------------------
// Legacy streaming filters (`mbfl_filt_conv_jis2004_wchar` & co.)
//
// In libmbfl these filters check `filter->from` / `filter->to` to serve
// EUC-JP-2004 and SJIS-2004 as well, but only ISO-2022-JP-2004 has vtables
// using them. The other branches are kept (selected by [`Variant`]) so the
// port stays faithful.

/// Which encoding a legacy JIS X 0213 filter serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variant {
    EucJp2004,
    Sjis2004,
    Iso2022Jp2004,
}

/// `mbfl_filt_conv_jis2004_wchar` (ISO-2022-JP-2004 to wchar).
pub fn filt_jis2004_wchar(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    jis2004_wchar(c, filter, out, Variant::Iso2022Jp2004);
}

#[allow(
    clippy::too_many_lines,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
fn jis2004_wchar(c: i32, filter: &mut Filter, out: &mut dyn Sink, from: Variant) {
    match filter.status & 0xF {
        0 => {
            if (0..0x80).contains(&c) {
                // latin
                match from {
                    Variant::EucJp2004 => out.put(c),
                    Variant::Sjis2004 => out.put(match c {
                        0x5C => 0xA5,
                        0x7E => 0x203E,
                        _ => c,
                    }),
                    Variant::Iso2022Jp2004 => {
                        if c == 0x1B {
                            filter.status += 6;
                        } else if matches!(filter.status, 0x80 | 0x90 | 0xA0)
                            && c > 0x20
                            && c < 0x7F
                        {
                            // kanji first char
                            filter.cache = c;
                            if filter.status == 0x90 {
                                filter.status += 1; // JIS X 0213 plane 1
                            } else if filter.status == 0xA0 {
                                filter.status += 4; // JIS X 0213 plane 2
                            } else {
                                filter.status += 5; // JIS X 0208
                            }
                        } else {
                            out.put(c);
                        }
                    }
                }
            } else {
                match from {
                    Variant::EucJp2004 => {
                        if c > 0xA0 && c < 0xFF {
                            // X 0213 plane 1 first char
                            filter.status = 1;
                            filter.cache = c;
                        } else if c == 0x8E {
                            // kana first char
                            filter.cache = 0x8E;
                            filter.status = 2;
                        } else if c == 0x8F {
                            // X 0213 plane 2 first char
                            filter.status = 3;
                        } else {
                            out.put(BAD_INPUT_I32);
                        }
                    }
                    Variant::Sjis2004 => {
                        if c > 0xA0 && c < 0xE0 {
                            // kana
                            out.put(0xFEC0 + c);
                        } else if c > 0x80 && c < 0xFD && c != 0xA0 {
                            // kanji first char
                            filter.status = 1;
                            filter.cache = c;
                        } else {
                            out.put(BAD_INPUT_I32);
                        }
                    }
                    Variant::Iso2022Jp2004 => out.put(BAD_INPUT_I32),
                }
            }
        }

        1 => {
            // kanji second char
            filter.status &= !0xF;
            let c1 = filter.cache;
            let (s1, s2) = match from {
                Variant::EucJp2004 => {
                    if c > 0xA0 && c < 0xFF {
                        (c1 - 0x80, c - 0x80)
                    } else {
                        out.put(BAD_INPUT_I32);
                        return;
                    }
                }
                Variant::Sjis2004 => {
                    if (0x40..=0xFC).contains(&c) && c != 0x7F {
                        sjis_decode(c1, c)
                    } else {
                        out.put(BAD_INPUT_I32);
                        return;
                    }
                }
                Variant::Iso2022Jp2004 => {
                    if (0x21..=0x7E).contains(&c) {
                        (c1, c)
                    } else {
                        out.put(BAD_INPUT_I32);
                        return;
                    }
                }
            };
            let mut w1 = (s1 << 8) | s2;
            let mut w: i32 = 0;

            // conversion for combining characters
            if w1 >= 0 && is_combining_jis(w1 as u32) {
                if let Some((a, b)) = u2_pair(w1 as u32, &t::JISX0213_U2_KEY) {
                    out.put(a as i32);
                    w = b as i32;
                }
            }

            // conversion for BMP
            if w <= 0 {
                w1 = (s1 - 0x21) * 94 + s2 - 0x21;
                if w1 >= 0 && (w1 as usize) < t::JISX0213_UCS_TABLE.len() {
                    w = i32::from(t::JISX0213_UCS_TABLE[w1 as usize]);
                }
            }

            // conversion for CJK Unified Ideographs ext.B (U+2XXXX)
            if w <= 0 {
                let k = bisec_srch2(w1, &t::JISX0213_JIS_U5_KEY);
                if k >= 0 {
                    w = i32::from(t::JISX0213_JIS_U5_TBL[k as usize]) + 0x20000;
                }
            }

            out.put(if w <= 0 { BAD_INPUT_I32 } else { w });
        }

        2 => {
            // got 0x8E: EUC-JP-2004 kana
            filter.status = 0;
            out.put(if c > 0xA0 && c < 0xE0 {
                0xFEC0 + c
            } else {
                BAD_INPUT_I32
            });
        }

        3 => {
            // X 0213 plane 2 first char: EUC-JP-2004 (0x8F)
            if c == 0xA1
                || (0xA3..=0xA5).contains(&c)
                || c == 0xA8
                || (0xAC..=0xAF).contains(&c)
                || (0xEE..=0xFE).contains(&c)
            {
                filter.cache = c - 0x80;
                filter.status += 1;
            } else {
                filter.status = 0;
                out.put(BAD_INPUT_I32);
            }
        }

        4 => {
            // X 0213 plane 2 second char: EUC-JP-2004, ISO-2022-JP-2004
            filter.status &= !0xF;
            let c1 = filter.cache;
            let c2 = if from == Variant::EucJp2004 {
                c - 0x80
            } else {
                c
            };
            if !(0x21..=0x7E).contains(&c2) {
                out.put(BAD_INPUT_I32);
                return;
            }
            let (s1, s2) = (c1 - 0x21, c2 - 0x21);
            if s1 >= 0 && plane2_valid(s1 as u32, s2 as u32) {
                out.put(plane2_to_ucs(s1 as u32, s2 as u32) as i32);
            } else {
                out.put(BAD_INPUT_I32);
            }
        }

        5 => {
            // X 0208: ISO-2022-JP-2004
            filter.status &= !0xF;
            let c1 = filter.cache;
            let mut w: i32 = 0;
            if c > 0x20 && c < 0x7F {
                let s = (c1 - 0x21) * 94 + c - 0x21;
                if s >= 0 && (s as usize) < JISX0208_UCS_TABLE.len() {
                    w = i32::from(JISX0208_UCS_TABLE[s as usize]);
                }
            }
            out.put(if w <= 0 { BAD_INPUT_I32 } else { w });
        }

        6 => {
            // ESC: ISO-2022-JP-2004
            if c == i32::from(b'$') {
                filter.status += 1;
            } else if c == i32::from(b'(') {
                filter.status += 3;
            } else {
                filter.status &= !0xF;
                out.put(BAD_INPUT_I32);
            }
        }

        7 => {
            // ESC $: ISO-2022-JP-2004
            if c == i32::from(b'B') {
                filter.status = 0x80; // JIS X 0208-1983
            } else if c == i32::from(b'(') {
                filter.status += 1;
            } else {
                filter.status &= !0xF;
                out.put(BAD_INPUT_I32);
            }
        }

        8 => {
            // ESC $ (: ISO-2022-JP-2004
            if c == i32::from(b'Q') {
                filter.status = 0x90; // JIS X 0213 plane 1
            } else if c == i32::from(b'P') {
                filter.status = 0xA0; // JIS X 0213 plane 2
            } else {
                filter.status &= !0xF;
                out.put(BAD_INPUT_I32);
            }
        }

        9 => {
            // ESC (: ISO-2022-JP-2004
            if c == i32::from(b'B') {
                filter.status = 0;
            } else {
                filter.status &= !0xF;
                out.put(BAD_INPUT_I32);
            }
        }

        // Unreachable in libmbfl (`ZEND_UNREACHABLE`).
        _ => {}
    }
}

/// `mbfl_filt_conv_jis2004_wchar_flush`.
pub fn filt_jis2004_wchar_flush(filter: &mut Filter, out: &mut dyn Sink) {
    if filter.status & 0xF != 0 {
        out.put(BAD_INPUT_I32);
    }
    filter.status = 0;
    out.flush();
}

/// `mbfl_filt_conv_wchar_jis2004` (wchar to ISO-2022-JP-2004).
pub fn filt_wchar_jis2004(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    wchar_jis2004(c, filter, out, Variant::Iso2022Jp2004);
}

/// Emit an `ESC $ ( <f>` designation unless already in a JIS X 0213 plane.
fn designate_x0213(filter: &mut Filter, out: &mut dyn Sink, f: u8) {
    if filter.status & 0xFF00 != 0x200 {
        out.put(0x1B);
        out.put(i32::from(b'$'));
        out.put(i32::from(b'('));
        out.put(i32::from(f));
    }
    filter.status = 0x200;
}

/// Convert a JIS X 0213 plane 1 code (`0x2121..0x7EFF`) to the two output
/// bytes for `to`, emitting any needed escape sequence first.
fn plane1_out(s: i32, filter: &mut Filter, out: &mut dyn Sink, to: Variant) -> (i32, i32) {
    match to {
        Variant::Sjis2004 => sjis_encode((s >> 8) & 0xFF, s & 0xFF),
        Variant::EucJp2004 => (((s >> 8) & 0xFF) + 0x80, (s & 0xFF) + 0x80),
        Variant::Iso2022Jp2004 => {
            designate_x0213(filter, out, b'Q');
            ((s >> 8) & 0xFF, s & 0xFF)
        }
    }
}

#[allow(
    clippy::too_many_lines,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation
)]
fn wchar_jis2004(c: i32, filter: &mut Filter, out: &mut dyn Sink, to: Variant) {
    loop {
        // `retry:`
        let mut s1: i32 = 0;

        // check for 1st char of combining characters
        if filter.status.trailing_zeros() >= 4 && is_combining_first(c as u32) {
            for k in 0..U2_LEN {
                if c == i32::from(t::JISX0213_U2_TBL[2 * k]) {
                    filter.status += 1;
                    filter.cache = k as i32;
                    return;
                }
            }
        }

        // check for 2nd char of combining characters
        if filter.status & 0xF == 1 && filter.cache >= 0 && (filter.cache as usize) < U2_LEN {
            let mut k = filter.cache as usize;
            filter.status &= !0xF;
            filter.cache = 0;

            let c1 = i32::from(t::JISX0213_U2_TBL[2 * k]);
            if matches!(c1, 0x0254 | 0x028C | 0x0259 | 0x025A) && c == 0x0301 {
                k += 1;
            }
            if c == i32::from(t::JISX0213_U2_TBL[2 * k + 1]) {
                s1 = i32::from(t::JISX0213_U2_KEY[k]);
            } else {
                // fallback
                let fb = i32::from(t::JISX0213_U2_FB_TBL[k]);
                let (b1, b2) = match to {
                    Variant::Sjis2004 => sjis_encode((fb >> 8) & 0xFF, fb & 0xFF),
                    Variant::EucJp2004 => (((fb >> 8) & 0xFF) + 0x80, (fb & 0xFF) + 0x80),
                    Variant::Iso2022Jp2004 => {
                        if filter.status != 0x200 {
                            out.put(0x1B);
                            out.put(i32::from(b'$'));
                            out.put(i32::from(b'('));
                            out.put(i32::from(b'Q'));
                        }
                        filter.status = 0x200;
                        ((fb >> 8) & 0x7F, fb & 0x7F)
                    }
                };
                // Flush out cached data
                out.put(b1);
                out.put(b2);
                continue; // goto retry
            }
        }

        // check for major japanese chars, compressed mapping area, ext B,
        // and compatibility forms/ideographs
        if s1 <= 0 {
            s1 = ucs_to_jisx0213(c as u32) as i32;
        }

        if s1 <= 0 {
            s1 = if c == 0 { 0 } else { -1 };
        }

        if s1 >= 0 {
            if s1 < 0x80 {
                // ASCII
                if to == Variant::Iso2022Jp2004 && filter.status & 0xFF00 != 0 {
                    out.put(0x1B);
                    out.put(i32::from(b'('));
                    out.put(i32::from(b'B'));
                }
                filter.status = 0;
                out.put(s1);
            } else if s1 < 0x100 {
                // latin or kana
                if to == Variant::EucJp2004 {
                    out.put(0x8E);
                    out.put(s1);
                } else if to == Variant::Sjis2004 && (0xA1..=0xDF).contains(&s1) {
                    out.put(s1);
                } else {
                    legacy::illegal_output(c, filter, out);
                }
            } else if s1 < 0x7F00 {
                // X 0213 plane 1
                let (b1, b2) = plane1_out(s1, filter, out, to);
                out.put(b1);
                out.put(b2);
            } else {
                // X 0213 plane 2
                let (b1, b2) = if to == Variant::Sjis2004 {
                    sjis_encode((s1 >> 8) & 0xFF, s1 & 0xFF)
                } else {
                    let mut b1 = s1;
                    let mut b2 = s1 & 0xFF;
                    let k = ((s1 >> 8) & 0xFF) - 0x7F;
                    if k >= 0 && (k as usize) < t::JISX0213_P2_OFST.len() {
                        b1 = i32::from(t::JISX0213_P2_OFST[k as usize]) + 0x21;
                    }
                    if to == Variant::EucJp2004 {
                        b2 |= 0x80;
                        b1 |= 0x80;
                        out.put(0x8F);
                    } else {
                        designate_x0213(filter, out, b'P');
                    }
                    (b1, b2)
                };
                out.put(b1);
                out.put(b2);
            }
        } else {
            legacy::illegal_output(c, filter, out);
        }
        return;
    }
}

/// `mbfl_filt_conv_wchar_jis2004_flush`.
pub fn filt_wchar_jis2004_flush(filter: &mut Filter, out: &mut dyn Sink) {
    wchar_jis2004_flush(filter, out, Variant::Iso2022Jp2004);
}

#[allow(clippy::cast_sign_loss)]
fn wchar_jis2004_flush(filter: &mut Filter, out: &mut dyn Sink, to: Variant) {
    let k = filter.cache;
    filter.cache = 0;

    // Note: like libmbfl, this only fires when no charset switch has been
    // emitted yet (`status == 1` exactly), and its bound is `<=`.
    if filter.status == 1 && k >= 0 && (k as usize) <= U2_LEN {
        let s = i32::from(t::JISX0213_U2_FB_TBL[k as usize]);
        let (b1, b2) = match to {
            Variant::Sjis2004 => sjis_encode((s >> 8) & 0xFF, s & 0xFF),
            Variant::EucJp2004 => (((s >> 8) & 0xFF) | 0x80, (s & 0xFF) | 0x80),
            Variant::Iso2022Jp2004 => {
                let b = ((s >> 8) & 0x7F, s & 0x7F);
                designate_x0213(filter, out, b'Q');
                b
            }
        };
        out.put(b1);
        out.put(b2);
    }

    // If we had switched to a different charset, go back to ASCII mode.
    // This makes it possible to concatenate arbitrary valid strings
    // together and get a valid string.
    if filter.status & 0xFF00 != 0 {
        out.put(0x1B);
        out.put(i32::from(b'('));
        out.put(i32::from(b'B'));
    }

    filter.status = 0;
    out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codecs::cjk::testutil::{
        self, check, code_bytes, data_file, decode, decode_bufsize, encode, encode_split,
        legacy_decode, legacy_encode, mapping_lines, run_filter,
    };
    use crate::convert::{fast_convert, ErrorMode};
    use std::collections::BTreeMap;

    /// Parse `EUC-JP-2004.txt` / `SJIS-2004.txt`: byte code -> code points,
    /// plus the raw line (for `Fullwidth:` annotations).
    type Mapping = Vec<(Vec<u8>, Vec<u32>, String)>;

    fn load(name: &str) -> Option<Mapping> {
        let text = data_file(name)?;
        let mut v = Vec::new();
        for line in text.lines() {
            if line.starts_with('#') {
                continue;
            }
            let Some(toks) = mapping_lines(line).pop() else {
                continue;
            };
            if toks.len() < 2 {
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            let cps: Vec<u32> = toks[1].iter().map(|&x| x as u32).collect();
            v.push((code_bytes(toks[0][0]), cps, line.to_string()));
        }
        Some(v)
    }

    fn utf8(cps: &[u32]) -> Vec<u8> {
        let mut out = Vec::new();
        for &c in cps {
            crate::codecs::utf8::push_utf8(&mut out, c);
        }
        out
    }

    /// Shared body of the EUC-JP-2004 / SJIS-2004 mapping tests (mirrors
    /// `testAllValidChars` in both directions).
    fn mapping_roundtrip(
        e: &'static Encoding,
        valid: &BTreeMap<Vec<u8>, Vec<u32>>,
        from_unicode: &BTreeMap<u32, Vec<u8>>,
    ) {
        // Every valid character, alone and all concatenated
        let mut all_in = Vec::new();
        let mut all_out = Vec::new();
        for (bytes, cps) in valid {
            assert_eq!(&decode(e, bytes), cps, "decoding {bytes:02x?}");
            assert!(check(e, bytes), "check {bytes:02x?}");
            all_in.extend_from_slice(bytes);
            all_out.extend_from_slice(cps);
        }
        assert_eq!(decode(e, &all_in), all_out);
        assert_eq!(decode_bufsize(e, &all_in, 5), all_out);
        // ... and back (combining pairs included)
        assert_eq!(encode(e, &all_out), (all_in.clone(), 0));
        assert_eq!(encode_split(e, &all_out), all_in);

        for (&cp, bytes) in from_unicode {
            assert_eq!(encode(e, &[cp]), (bytes.clone(), 0), "encoding U+{cp:04X}");
        }
    }

    #[test]
    fn eucjp2004_mapping_file() {
        let Some(lines) = load("EUC-JP-2004.txt") else {
            return;
        };
        let mut valid = BTreeMap::new();
        let mut from_unicode = BTreeMap::new();
        for (bytes, cps, _) in lines {
            if cps.len() == 1 && cps[0] <= 0xFFFF {
                from_unicode.insert(cps[0], bytes.clone());
            }
            valid.insert(bytes, cps);
        }
        valid.insert(vec![0xA1, 0xB1], vec![0xFFE3]);
        from_unicode.insert(0xFFE3, vec![0xA1, 0xB1]);
        valid.insert(vec![0xA1, 0xEF], vec![0xFFE5]);
        from_unicode.insert(0xFFE5, vec![0xA1, 0xEF]);
        from_unicode.insert(0xA5, vec![0x5C]);
        from_unicode.insert(0x203E, vec![0x7E]);
        mapping_roundtrip(&ENC_EUCJP2004, &valid, &from_unicode);
    }

    #[test]
    fn sjis2004_mapping_file() {
        let Some(lines) = load("SJIS-2004.txt") else {
            return;
        };
        let mut valid = BTreeMap::new();
        let mut from_unicode = BTreeMap::new();
        for (bytes, mut cps, line) in lines {
            if cps.len() == 1 {
                if let Some(pos) = line.find("Fullwidth: U+") {
                    let hex: String = line[pos + 13..]
                        .chars()
                        .take_while(char::is_ascii_hexdigit)
                        .collect();
                    cps[0] = u32::from_str_radix(&hex, 16).unwrap_or(cps[0]);
                }
                if cps[0] <= 0xFFFF {
                    from_unicode.insert(cps[0], bytes.clone());
                }
            }
            valid.insert(bytes, cps);
        }
        from_unicode.insert(0x7E, vec![0x7E]);
        from_unicode.insert(0x5C, vec![0x5C]);
        // The mapping file lists 0x5C/0x7E as U+005C/U+007E but the decoder
        // (unlike the encoder) maps them to U+00A5/U+203E.
        valid.insert(vec![0x5C], vec![0xA5]);
        valid.insert(vec![0x7E], vec![0x203E]);
        // Decoding: check each character separately; the concatenation is
        // not reversible because of the 0x5C/0x7E asymmetry.
        for (bytes, cps) in &valid {
            assert_eq!(&decode(&ENC_SJIS2004, bytes), cps, "decoding {bytes:02x?}");
        }
        for (&cp, bytes) in &from_unicode {
            assert_eq!(
                encode(&ENC_SJIS2004, &[cp]),
                (bytes.clone(), 0),
                "encoding U+{cp:04X}"
            );
        }
        // Pairs of code points mapping to one character
        assert_eq!(
            encode(&ENC_SJIS2004, &[0x304B, 0x41]),
            (b"\x82\xA9A".to_vec(), 0)
        );
    }

    #[test]
    fn iso2022jp2004_mapping_file() {
        let Some(text) = data_file("ISO-2022-JP-2004-JISX0213.txt") else {
            return;
        };
        let e = &ENC_2022JP_2004;
        let mut n = 0;
        for line in text.lines() {
            if line.starts_with('#') {
                continue;
            }
            let mut f = line.split('\t');
            let (Some(code), Some(uni)) = (f.next(), f.next()) else {
                continue;
            };
            let Some((ty, hexcode)) = code.split_once('-') else {
                continue;
            };
            let Ok(c) = u32::from_str_radix(hexcode, 16) else {
                continue;
            };
            let mut cps: Vec<u32> = uni
                .trim_start_matches("U+")
                .split('+')
                .filter_map(|h| u32::from_str_radix(h, 16).ok())
                .collect();
            if cps.is_empty() {
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            let bytes = [(c >> 8) as u8, c as u8];
            let esc: &[u8] = match ty {
                "3" => b"\x1B$(Q",
                "4" => b"\x1B$(P",
                _ => continue,
            };
            if ty == "3" && c == 0x2131 {
                cps = vec![0xFFE3];
            }
            if ty == "3" && c == 0x216F {
                cps = vec![0xFFE5];
            }
            let input = [esc, &bytes].concat();
            assert_eq!(decode(e, &input), cps, "decoding {ty}-{hexcode}");
            assert_eq!(
                legacy_decode(e, &input),
                cps,
                "legacy decoding {ty}-{hexcode}"
            );
            assert!(check(e, &input));
            if cps[0] > 0x100 {
                let expected = [esc, &bytes, b"\x1B(B"].concat();
                let (got, errs) = encode(e, &cps);
                // Plane 1 0x2131/0x216F: fullwidth forms map elsewhere in
                // the reverse direction; skip characters whose Unicode is
                // shared by several codes.
                if got != expected {
                    let back = decode(e, &got);
                    assert_eq!(back, cps, "encoding {ty}-{hexcode} gave {got:02x?}");
                }
                assert_eq!(errs, 0);
            }
            n += 1;
        }
        assert!(n > 10000);
    }

    #[test]
    fn iso2022jp2004_phpt_cases() {
        let e = &ENC_2022JP_2004;
        let utf16 = |s: &[u16]| s.iter().map(|&x| u32::from(x)).collect::<Vec<_>>();
        // plane 1 then plane 2, and the reverse
        assert_eq!(
            encode(e, &utf16(&[0x3000, 0x4E02])).0,
            b"\x1B$(Q\x21\x21\x1B$(P\x21\x22\x1B(B"
        );
        assert_eq!(
            encode(e, &utf16(&[0x4E02, 0x3000])).0,
            b"\x1B$(P\x21\x22\x1B$(Q\x21\x21\x1B(B"
        );
        assert_eq!(encode(e, &[0x254, 0x300]).0, b"\x1B$(Q+H\x1B(B");
        assert_eq!(encode(e, &[0x254, 0x41]).0, b"\x1B$(Q+8\x1B(BA");
        assert_eq!(encode(e, &[0xFF95]), (b"?".to_vec(), 1));
        for c in 0x80..=0x9F {
            assert_eq!(decode(e, &[c]), vec![BAD_INPUT]);
        }
        assert!(!check(e, b"\x1B"));
        assert!(!check(e, b"\x1B$"));
        assert!(!check(e, b"\x1B$("));
        assert_eq!(decode(e, b"\x1B$(X"), vec![BAD_INPUT]);
        assert_eq!(decode(e, b"\x1B$B!"), vec![BAD_INPUT]);
        assert_eq!(decode(e, b"\xE0"), vec![BAD_INPUT]);
    }

    #[test]
    fn small_buffers_split_combining_output() {
        // The decoders stop one short of the buffer end so a combining pair
        // always fits.
        let euc = b"\xA4\xF7\xA4\xF7\xA4\xF7\xA4\xF7";
        let w = decode_bufsize(&ENC_EUCJP2004, euc, 5);
        assert_eq!(w, [0x304B, 0x309A].repeat(4));
        let mut rest: &[u8] = euc;
        let mut buf = [0u32; 5];
        let mut state = 0;
        assert_eq!(eucjp2004_to_wchar(&mut rest, &mut buf, &mut state), 4);
        assert_eq!(rest.len(), 4);
    }

    #[test]
    fn encoder_state_across_calls() {
        for e in [&ENC_2022JP_2004, &ENC_EUCJP2004, &ENC_SJIS2004] {
            let text = [
                0x304B, 0x309A, 0x304B, 0x41, 0x254, 0x301, 0x254, 0x300, 0x304B,
            ];
            assert_eq!(encode_split(e, &text), encode(e, &text).0, "{}", e.name);
        }
    }

    #[test]
    fn oracle_fixtures() {
        assert!(testutil::replay_oracle("jis2004_oracle.tsv") > 500);
    }

    fn fnv(mut h: u32, s: &[u8]) -> u32 {
        for &b in s {
            h = (h ^ u32::from(b)).wrapping_mul(16_777_619);
        }
        h
    }

    fn rec(h: u32, input: &[u8], from: &'static Encoding, to: &'static Encoding) -> u32 {
        let (mut out, errors) = fast_convert(input, from, to, 0x25, ErrorMode::Char);
        out.extend_from_slice(format!("|{errors}|").as_bytes());
        out.push(if check(from, input) { b'1' } else { b'0' });
        out.push(b'\n');
        fnv(h, &out)
    }

    /// Exhaustive comparisons against digests computed by
    /// `jis2004_oracle.php digest` (every 1-/2-byte string, every code point).
    #[test]
    fn oracle_digests() {
        let text = std::fs::read_to_string(testutil::fixture_path("jis2004_digest.tsv"))
            .expect("digest fixture");
        let u8e = crate::encoding::utf8();
        let mut n = 0;
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            let e = testutil::enc(f[1]);
            let expected = f[f.len() - 1];
            let mut h: u32 = 0x811C_9DC5;
            match f[0] {
                "decode2" => {
                    for a in 0..=255u8 {
                        h = rec(h, &[a], e, u8e);
                    }
                    for a in 0..=255u8 {
                        for b in 0..=255u8 {
                            h = rec(h, &[a, b], e, u8e);
                        }
                    }
                }
                "decode3" => {
                    for b in 0xA1..=255u8 {
                        for c in 0..=255u8 {
                            h = rec(h, &[0x8F, b, c], e, u8e);
                        }
                    }
                }
                "decode_esc" => {
                    let esc: Vec<u8> = (0..f[2].len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&f[2][i..i + 2], 16).unwrap_or(0))
                        .collect();
                    for a in 0..=255u8 {
                        for b in 0..=255u8 {
                            h = rec(h, &[esc.as_slice(), &[a, b]].concat(), e, u8e);
                        }
                    }
                }
                "encode1" => {
                    for cp in (0..0x1_0000u32).chain(0x2_0000..0x2_A700) {
                        if (0xD800..=0xDFFF).contains(&cp) {
                            continue;
                        }
                        h = rec(h, &utf8(&[cp]), u8e, e);
                    }
                }
                "encode2" => {
                    let mut firsts = vec![0xE6, 0x31F7];
                    firsts.extend(0x254..=0x2E9);
                    firsts.extend(0x304B..=0x3053);
                    firsts.extend(0x30AB..=0x30C8);
                    let seconds = [
                        0x41, 0x300, 0x301, 0x309A, 0x2E5, 0x2E9, 0x304B, 0x254, 0x4E00, 0x20B9F,
                        0xFF95, 0x3000,
                    ];
                    for &a in &firsts {
                        for &b in &seconds {
                            h = rec(h, &utf8(&[a, b]), u8e, e);
                        }
                    }
                }
                other => panic!("unknown digest record {other}"),
            }
            assert_eq!(format!("{h:08x}"), expected, "digest mismatch: {line}");
            n += 1;
        }
        assert_eq!(n, 13);
    }

    #[test]
    fn legacy_filters_match_fast_codec_on_valid_text() {
        let e = &ENC_2022JP_2004;
        let texts: [&[u32]; 5] = [
            &[0x41, 0x6F22, 0x5B57, 0x42],
            &[0x304B, 0x309A, 0x304D],
            &[0x3000, 0x4E02, 0x3000, 0x41],
            &[0x20089, 0x2000B, 0x41],
            &[0x254, 0x301, 0x254, 0x300, 0x2E9, 0x2E5, 0x41],
        ];
        for text in texts {
            let bytes = encode(e, text).0;
            assert_eq!(legacy_decode(e, &bytes), decode(e, &bytes));
            assert_eq!(legacy_decode(e, &bytes), text);
        }
        // JIS X 0208 designation is only produced by other encoders
        assert_eq!(
            legacy_decode(e, b"\x1B$B\x30\x21\x1B(Bz"),
            vec![0x4E9C, 0x7A]
        );
        // Encoding: matches when the text does not end in a pending
        // combining base character after a charset switch.
        for text in [
            &[0x41, 0x6F22, 0x5B57, 0x42][..],
            &[0x304B, 0x309A, 0x304D, 0x41],
            &[0x3000, 0x4E00, 0x41],
            &[0x254, 0x301, 0x254, 0x300, 0x41],
        ] {
            assert_eq!(legacy_encode(e, text), encode(e, text).0, "{text:x?}");
        }
    }

    #[test]
    fn legacy_filter_quirks() {
        let e = &ENC_2022JP_2004;
        // Errors and truncation
        assert_eq!(legacy_decode(e, b"\x1B$(X"), vec![BAD_INPUT]);
        assert_eq!(legacy_decode(e, b"\x1B$(Q!"), vec![BAD_INPUT]);
        assert_eq!(legacy_decode(e, b"\x80"), vec![BAD_INPUT]);
        assert_eq!(legacy_decode(e, b"\x1B"), vec![BAD_INPUT]);
        // Unencodable characters
        assert_eq!(legacy_encode(e, &[0xFF95, 0x41]), b"?A");
        // A pending base character is flushed as its fallback...
        assert_eq!(legacy_encode(e, &[0x304B]), b"\x1B$(Q\x24\x2B\x1B(B");
        // ...but, as in libmbfl, dropped if a charset switch preceded it
        assert_eq!(
            legacy_encode(e, &[0x3000, 0x304B]),
            b"\x1B$(Q\x21\x21\x1B(B"
        );
        // Switching between planes 1 and 2: libmbfl's legacy filter does not
        // re-designate (it uses the same state for both)
        assert_eq!(
            legacy_encode(e, &[0x3000, 0x4E02]),
            b"\x1B$(Q\x21\x21\x21\x22\x1B(B"
        );
        assert_eq!(
            legacy_encode(e, &[0x4E02, 0x3000]),
            b"\x1B$(P\x21\x22\x21\x21\x1B(B"
        );
    }

    /// The EUC-JP-2004 / SJIS-2004 branches of the legacy filters (not
    /// reachable through any libmbfl vtable) agree with the fast codecs.
    #[test]
    fn legacy_variant_branches() {
        fn euc_in(c: i32, f: &mut Filter, o: &mut dyn Sink) {
            jis2004_wchar(c, f, o, Variant::EucJp2004);
        }
        fn sjis_in(c: i32, f: &mut Filter, o: &mut dyn Sink) {
            jis2004_wchar(c, f, o, Variant::Sjis2004);
        }
        fn euc_out(c: i32, f: &mut Filter, o: &mut dyn Sink) {
            wchar_jis2004(c, f, o, Variant::EucJp2004);
        }
        fn euc_out_flush(f: &mut Filter, o: &mut dyn Sink) {
            wchar_jis2004_flush(f, o, Variant::EucJp2004);
        }
        fn sjis_out(c: i32, f: &mut Filter, o: &mut dyn Sink) {
            wchar_jis2004(c, f, o, Variant::Sjis2004);
        }
        fn sjis_out_flush(f: &mut Filter, o: &mut dyn Sink) {
            wchar_jis2004_flush(f, o, Variant::Sjis2004);
        }
        let v = |function, flush| Vtbl {
            ctor: legacy::common_ctor,
            function,
            flush,
        };
        let text = [
            0x41, 0x304B, 0x309A, 0x6F22, 0x4E02, 0xFF71, 0x20089, 0x304B,
        ];
        #[allow(
            clippy::cast_possible_wrap,
            clippy::cast_sign_loss,
            clippy::cast_possible_truncation
        )]
        for (enc, vin, vout) in [
            (
                &ENC_EUCJP2004,
                v(euc_in, filt_jis2004_wchar_flush),
                v(euc_out, euc_out_flush),
            ),
            (
                &ENC_SJIS2004,
                v(sjis_in, filt_jis2004_wchar_flush),
                v(sjis_out, sjis_out_flush),
            ),
        ] {
            let bytes = encode(enc, &text).0;
            let w: Vec<i32> = text.iter().map(|&c| c as i32).collect();
            let legacy_bytes: Vec<u8> =
                run_filter(&vout, &w).into_iter().map(|c| c as u8).collect();
            assert_eq!(legacy_bytes, bytes, "{}", enc.name);
            let b: Vec<i32> = bytes.iter().map(|&c| i32::from(c)).collect();
            let back: Vec<u32> = run_filter(&vin, &b).into_iter().map(|c| c as u32).collect();
            assert_eq!(back, decode(enc, &bytes), "{}", enc.name);
        }
    }
}

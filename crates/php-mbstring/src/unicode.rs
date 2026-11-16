// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Unicode character properties and case conversion.
//!
//! Ref: `ext/mbstring/php_unicode.c`, `php_unicode.h`, `unicode_data.h`.

use crate::convert::{ConvertBuf, ErrorMode};
use crate::encoding::{Encoding, EncodingNo};
use crate::tables::eaw::{EAW_TABLE, FIRST_DOUBLEWIDTH_CODEPOINT};
use crate::tables::unicode_data as ud;

/// Unicode property identifiers (`UC_*` in `php_unicode.h`).
#[allow(missing_docs)]
pub mod prop {
    pub const MN: u32 = 0;
    pub const MC: u32 = 1;
    pub const ME: u32 = 2;
    pub const ND: u32 = 3;
    pub const NL: u32 = 4;
    pub const NO: u32 = 5;
    pub const ZS: u32 = 6;
    pub const ZL: u32 = 7;
    pub const ZP: u32 = 8;
    pub const OS: u32 = 9;
    pub const CO: u32 = 10;
    pub const CN: u32 = 11;
    pub const LU: u32 = 12;
    pub const LL: u32 = 13;
    pub const LT: u32 = 14;
    pub const LM: u32 = 15;
    pub const LO: u32 = 16;
    pub const SM: u32 = 17;
    pub const SC: u32 = 18;
    pub const SK: u32 = 19;
    pub const SO: u32 = 20;
    pub const L: u32 = 21;
    pub const R: u32 = 22;
    pub const EN: u32 = 23;
    pub const ES: u32 = 24;
    pub const ET: u32 = 25;
    pub const AN: u32 = 26;
    pub const CS: u32 = 27;
    pub const B: u32 = 28;
    pub const S: u32 = 29;
    pub const WS: u32 = 30;
    pub const ON: u32 = 31;
    pub const AL: u32 = 32;
    pub const C: u32 = 33;
    pub const P: u32 = 34;
    pub const CASED: u32 = 35;
    pub const CASE_IGNORABLE: u32 = 36;
}

/// `prop_lookup` / `php_unicode_is_prop1`.
pub fn is_prop1(code: u32, prop: u32) -> bool {
    let n = prop as usize;
    let mut l = i64::from(ud::UCPROP_OFFSETS[n]);
    let mut r = i64::from(ud::UCPROP_OFFSETS[n + 1]) - 1;
    while l <= r {
        let mut m = (l + r) >> 1;
        m -= m & 1;
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let mi = m as usize;
        if code > ud::UCPROP_RANGES[mi + 1] {
            l = m + 2;
        } else if code < ud::UCPROP_RANGES[mi] {
            r = m - 2;
        } else {
            return true;
        }
    }
    false
}

/// `php_unicode_is_prop`: true if `code` has any of `props`.
pub fn is_prop(code: u32, props: &[u32]) -> bool {
    props.iter().any(|&p| is_prop1(code, p))
}

/// `php_unicode_is_cased`.
pub fn is_cased(code: u32) -> bool {
    is_prop1(code, prop::CASED)
}

/// `php_unicode_is_case_ignorable`.
pub fn is_case_ignorable(code: u32) -> bool {
    is_prop1(code, prop::CASE_IGNORABLE)
}

/// `php_unicode_is_upper`.
pub fn is_upper(code: u32) -> bool {
    if code < 0x80 {
        (0x41..=0x5A).contains(&code)
    } else {
        is_prop1(code, prop::LU)
    }
}

/// `php_unicode_is_lower`.
pub fn is_lower(code: u32) -> bool {
    if code < 0x80 {
        (0x61..=0x7A).contains(&code)
    } else {
        is_prop1(code, prop::LL)
    }
}

/// Display width of a code point: 2 for East Asian wide/fullwidth, else 1
/// (`character_width` in `mbstring.c`).
pub fn character_width(c: u32) -> usize {
    if c < FIRST_DOUBLEWIDTH_CODEPOINT {
        return 1;
    }
    let (mut lo, mut hi) = (0usize, EAW_TABLE.len());
    while lo < hi {
        let probe = usize::midpoint(lo, hi);
        let (b, e) = EAW_TABLE[probe];
        if c < b {
            hi = probe;
        } else if c > e {
            lo = probe + 1;
        } else {
            return 2;
        }
    }
    1
}

/// Case conversion modes (`php_case_mode`, the `MB_CASE_*` constants).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseMode {
    /// `MB_CASE_UPPER`.
    Upper = 0,
    /// `MB_CASE_LOWER`.
    Lower = 1,
    /// `MB_CASE_TITLE`.
    Title = 2,
    /// `MB_CASE_FOLD`.
    Fold = 3,
    /// `MB_CASE_UPPER_SIMPLE`.
    UpperSimple = 4,
    /// `MB_CASE_LOWER_SIMPLE`.
    LowerSimple = 5,
    /// `MB_CASE_TITLE_SIMPLE`.
    TitleSimple = 6,
    /// `MB_CASE_FOLD_SIMPLE`.
    FoldSimple = 7,
}

impl CaseMode {
    /// From an `MB_CASE_*` constant value.
    pub fn from_i64(v: i64) -> Option<Self> {
        Some(match v {
            0 => Self::Upper,
            1 => Self::Lower,
            2 => Self::Title,
            3 => Self::Fold,
            4 => Self::UpperSimple,
            5 => Self::LowerSimple,
            6 => Self::TitleSimple,
            7 => Self::FoldSimple,
            _ => return None,
        })
    }
}

const CODE_NOT_FOUND: u32 = u32::MAX;

#[inline]
fn mph_hash(d: u32, x: u32) -> u32 {
    let x = x ^ d;
    ((x >> 16) ^ x).wrapping_mul(0x045d_9f3b)
}

#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
fn mph_lookup(code: u32, g_table: &[i16], table: &[u32]) -> u32 {
    let g_size = g_table.len() as u32;
    let t_size = (table.len() / 2) as u32;
    let g = i32::from(g_table[(mph_hash(0, code) % g_size) as usize]);
    let idx = if g <= 0 {
        (-g) as u32
    } else {
        mph_hash(g as u32, code) % t_size
    } as usize;
    if table[2 * idx] == code {
        table[2 * idx + 1]
    } else {
        CODE_NOT_FOUND
    }
}

fn is_8859_9(enc: &Encoding) -> bool {
    enc.no == EncodingNo::Iso8859_9
}

fn toupper_raw(code: u32, enc: &Encoding) -> u32 {
    if code < 0xB5 {
        if (0x61..=0x7A).contains(&code) {
            if is_8859_9(enc) && code == 0x69 {
                return 0x130;
            }
            return code - 0x20;
        }
        code
    } else {
        match mph_lookup(code, &ud::UCCASE_UPPER_G, &ud::UCCASE_UPPER_TABLE) {
            CODE_NOT_FOUND => code,
            n => n,
        }
    }
}

fn tolower_raw(code: u32, enc: &Encoding) -> u32 {
    if code < 0xC0 {
        if (0x41..=0x5A).contains(&code) {
            if is_8859_9(enc) && code == 0x49 {
                return 0x131;
            }
            return code + 0x20;
        }
        code
    } else {
        match mph_lookup(code, &ud::UCCASE_LOWER_G, &ud::UCCASE_LOWER_TABLE) {
            CODE_NOT_FOUND => code,
            _ if is_8859_9(enc) && code == 0x130 => 0x69,
            n => n,
        }
    }
}

fn totitle_raw(code: u32, enc: &Encoding) -> u32 {
    match mph_lookup(code, &ud::UCCASE_TITLE_G, &ud::UCCASE_TITLE_TABLE) {
        CODE_NOT_FOUND => toupper_raw(code, enc),
        n => n,
    }
}

fn tofold_raw(code: u32, enc: &Encoding) -> u32 {
    if code < 0xB5 {
        if (0x41..=0x5A).contains(&code) {
            if is_8859_9(enc) && code == 0x49 {
                return 0x131;
            }
            return code + 0x20;
        }
        code
    } else {
        match mph_lookup(code, &ud::UCCASE_FOLD_G, &ud::UCCASE_FOLD_TABLE) {
            CODE_NOT_FOUND => code,
            _ if is_8859_9(enc) && code == 0x130 => 0x69,
            n => n,
        }
    }
}

#[inline]
fn simple(code: u32) -> u32 {
    if code > 0xFF_FFFF {
        ud::UCCASE_EXTRA_TABLE[(code & 0xFF_FFFF) as usize]
    } else {
        code
    }
}

/// Simple (1:1) lowercase mapping.
pub fn tolower_simple(code: u32, enc: &Encoding) -> u32 {
    simple(tolower_raw(code, enc))
}
/// Simple (1:1) uppercase mapping.
pub fn toupper_simple(code: u32, enc: &Encoding) -> u32 {
    simple(toupper_raw(code, enc))
}
/// Simple (1:1) titlecase mapping.
pub fn totitle_simple(code: u32, enc: &Encoding) -> u32 {
    simple(totitle_raw(code, enc))
}
/// Simple (1:1) case folding.
pub fn tofold_simple(code: u32, enc: &Encoding) -> u32 {
    simple(tofold_raw(code, enc))
}

fn emit_special(w: u32, out: &mut [u32; 192], p: &mut usize) {
    let len = (w >> 24) as usize;
    let base = (w & 0xFF_FFFF) as usize;
    for k in 1..=len {
        out[*p] = ud::UCCASE_EXTRA_TABLE[base + k];
        *p += 1;
    }
}

fn scan_ahead_for_cased_letter(mut input: &[u8], mut state: u32, enc: &Encoding) -> bool {
    let mut buf = [0u32; 64];
    while !input.is_empty() {
        let n = (enc.to_wchar)(&mut input, &mut buf, &mut state);
        for &w in &buf[..n] {
            if is_cased(w) {
                return true;
            }
            if !is_case_ignorable(w) {
                return false;
            }
        }
    }
    false
}

/// `scan_back_for_cased_letter(begin, end)` over `buf[begin..end]`.
fn scan_back_for_cased_letter(buf: &[u32; 192], begin: usize, end: Option<usize>) -> bool {
    if let Some(end) = end {
        let mut e = end;
        while e > begin {
            e -= 1;
            let w = buf[e];
            if is_cased(w) {
                return true;
            }
            if !is_case_ignorable(w) {
                return false;
            }
        }
    }
    false
}

/// Whether the Greek capital sigma at `wchar_buf[i]` is word-final (and so
/// should become U+03C2).
#[allow(clippy::too_many_arguments)]
fn sigma_is_final(
    wchar_buf: &[u32],
    i: usize,
    converted: &[u32; 192],
    p: usize,
    converted_end: Option<usize>,
    rest: &[u8],
    state: u32,
    enc: &Encoding,
) -> bool {
    let mut j = i;
    while j > 0 && is_case_ignorable(wchar_buf[j - 1]) {
        j -= 1;
    }
    let preceded = if j > 0 {
        is_cased(wchar_buf[j - 1])
    } else {
        scan_back_for_cased_letter(converted, p, converted_end)
    };
    if !preceded {
        return false;
    }
    let mut j = i + 1;
    while j < wchar_buf.len() && is_case_ignorable(wchar_buf[j]) {
        j += 1;
    }
    if j >= wchar_buf.len() {
        !scan_ahead_for_cased_letter(rest, state, enc)
    } else {
        !is_cased(wchar_buf[j])
    }
}

/// `php_unicode_convert_case`: convert `src` (in `src_enc`) to the given case,
/// outputting in `dst_enc` with the given error handling.
#[allow(clippy::too_many_lines)]
pub fn convert_case(
    mode: CaseMode,
    src: &[u8],
    src_enc: &'static Encoding,
    dst_enc: &'static Encoding,
    illegal_mode: ErrorMode,
    illegal_substchar: u32,
) -> Vec<u8> {
    let mut wchar_buf = [0u32; 64];
    let mut converted = [0u32; 192];
    let mut state = 0u32;
    let mut title_mode = false;
    let mut input = src;
    let mut converted_end: Option<usize> = None;
    let mut buf = ConvertBuf::new(src.len() + 1, illegal_substchar, illegal_mode);

    while !input.is_empty() {
        let out_len = (src_enc.to_wchar)(&mut input, &mut wchar_buf, &mut state);
        let wb = &wchar_buf[..out_len];
        let mut p = 0usize;

        match mode {
            CaseMode::UpperSimple | CaseMode::LowerSimple | CaseMode::FoldSimple => {
                let f = match mode {
                    CaseMode::UpperSimple => toupper_simple,
                    CaseMode::LowerSimple => tolower_simple,
                    _ => tofold_simple,
                };
                for &w in wb {
                    converted[p] = if w > 0xFF_FFFF { w } else { f(w, src_enc) };
                    p += 1;
                }
            }
            CaseMode::TitleSimple => {
                for &w in wb {
                    if w > 0xFF_FFFF {
                        converted[p] = w;
                        p += 1;
                        continue;
                    }
                    converted[p] = if title_mode {
                        tolower_simple(w, src_enc)
                    } else {
                        totitle_simple(w, src_enc)
                    };
                    p += 1;
                    if !is_case_ignorable(w) {
                        title_mode = is_cased(w);
                    }
                }
            }
            CaseMode::Upper | CaseMode::Fold => {
                let f = if mode == CaseMode::Upper {
                    toupper_raw
                } else {
                    tofold_raw
                };
                for &w in wb {
                    if w > 0xFF_FFFF {
                        converted[p] = w;
                        p += 1;
                        continue;
                    }
                    let w = f(w, src_enc);
                    if w > 0xFF_FFFF {
                        emit_special(w, &mut converted, &mut p);
                    } else {
                        converted[p] = w;
                        p += 1;
                    }
                }
            }
            CaseMode::Lower => {
                for (i, &w) in wb.iter().enumerate() {
                    if w > 0xFF_FFFF {
                        converted[p] = w;
                        p += 1;
                        continue;
                    }
                    if w == 0x3A3
                        && sigma_is_final(
                            wb,
                            i,
                            &converted,
                            p,
                            converted_end,
                            input,
                            state,
                            src_enc,
                        )
                    {
                        converted[p] = 0x3C2;
                        p += 1;
                        continue;
                    }
                    let w = tolower_raw(w, src_enc);
                    if w > 0xFF_FFFF {
                        emit_special(w, &mut converted, &mut p);
                    } else {
                        converted[p] = w;
                        p += 1;
                    }
                }
            }
            CaseMode::Title => {
                for (i, &w) in wb.iter().enumerate() {
                    if w > 0xFF_FFFF {
                        converted[p] = w;
                        p += 1;
                        continue;
                    }
                    let final_sigma = title_mode
                        && w == 0x3A3
                        && sigma_is_final(
                            wb,
                            i,
                            &converted,
                            p,
                            converted_end,
                            input,
                            state,
                            src_enc,
                        );
                    if final_sigma {
                        converted[p] = 0x3C2;
                        p += 1;
                    } else {
                        let w2 = if title_mode {
                            tolower_raw(w, src_enc)
                        } else {
                            totitle_raw(w, src_enc)
                        };
                        if w2 > 0xFF_FFFF {
                            emit_special(w2, &mut converted, &mut p);
                        } else {
                            converted[p] = w2;
                            p += 1;
                        }
                    }
                    if !is_case_ignorable(w) {
                        title_mode = is_cased(w);
                    }
                }
            }
        }

        converted_end = Some(p);
        (dst_enc.from_wchar)(&converted[..p], &mut buf, input.is_empty());
    }

    buf.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::by_name;

    fn cc(mode: CaseMode, s: &str) -> String {
        let u = by_name(b"UTF-8").unwrap();
        String::from_utf8(convert_case(
            mode,
            s.as_bytes(),
            u,
            u,
            ErrorMode::Char,
            0x3F,
        ))
        .unwrap()
    }

    #[test]
    fn basic_case_modes() {
        assert_eq!(cc(CaseMode::Upper, "straße ǆ"), "STRASSE Ǆ");
        assert_eq!(cc(CaseMode::UpperSimple, "straße"), "STRAßE");
        assert_eq!(cc(CaseMode::Lower, "ΣΑΣ ΑΣ."), "σας ας.");
        assert_eq!(cc(CaseMode::Title, "hello wORLD ǆx"), "Hello World ǅx");
        assert_eq!(cc(CaseMode::Fold, "Straße"), "strasse");
        assert_eq!(cc(CaseMode::FoldSimple, "Straße"), "straße");
        assert_eq!(cc(CaseMode::Lower, "İ"), "i\u{307}");
    }

    #[test]
    fn properties_and_width() {
        assert!(is_upper(u32::from('Ä')));
        assert!(is_lower(u32::from('ä')));
        assert!(is_cased(u32::from('a')));
        assert!(is_case_ignorable(u32::from('\'')));
        assert_eq!(character_width(u32::from('a')), 1);
        assert_eq!(character_width(u32::from('日')), 2);
        assert_eq!(character_width(crate::BAD_INPUT), 1);
    }
}

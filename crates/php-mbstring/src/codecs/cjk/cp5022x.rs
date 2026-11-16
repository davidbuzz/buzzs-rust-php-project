// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `CP50220`, `CP50221`, `CP50222`: Microsoft's variants of ISO-2022-JP
//! (JIS X 0208 with the CP932 extensions, user-defined area mapped to the
//! PUA, JIS X 0201 kana via `ESC ( I` or SO/SI).
//!
//! `CP50220` converts halfwidth katakana to fullwidth on output (gluing
//! voiced sound marks), `CP50221` emits them with `ESC ( I`, and `CP50222`
//! emits them with SO/SI. All three decode identically.
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c`.

use super::kana::{convert_kana_codepoint, MBFL_HAN2ZEN_GLUE, MBFL_HAN2ZEN_KATAKANA};
use crate::convert::{illegal_output, ConvertBuf, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};
use crate::legacy::{self, Filter, Sink, Vtbl, BAD_INPUT_I32};
use crate::tables::cjk::cp932_ext as ext;
use crate::tables::cjk::jis;

/// `CP50220`.
pub static ENC_CP50220: Encoding = Encoding {
    no: EncodingNo::Cp50220,
    name: "CP50220",
    mime_name: Some("ISO-2022-JP"),
    aliases: &["cp50220raw", "cp50220-raw", "JIS-ms"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: Some(&VTBL_CP50220_WCHAR),
    output_filter: Some(&VTBL_WCHAR_CP50220),
    to_wchar: cp5022x_to_wchar,
    from_wchar: wchar_to_cp50220,
    check: None,
    cut: None,
};

/// `CP50221`.
pub static ENC_CP50221: Encoding = Encoding {
    no: EncodingNo::Cp50221,
    name: "CP50221",
    mime_name: Some("ISO-2022-JP"),
    aliases: &[],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: Some(&VTBL_CP50221_WCHAR),
    output_filter: Some(&VTBL_WCHAR_CP50221),
    to_wchar: cp5022x_to_wchar,
    from_wchar: wchar_to_cp50221,
    check: None,
    cut: None,
};

/// `CP50222`.
pub static ENC_CP50222: Encoding = Encoding {
    no: EncodingNo::Cp50222,
    name: "CP50222",
    mime_name: Some("ISO-2022-JP"),
    aliases: &[],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: Some(&VTBL_CP50222_WCHAR),
    output_filter: Some(&VTBL_WCHAR_CP50222),
    to_wchar: cp5022x_to_wchar,
    from_wchar: wchar_to_cp50222,
    check: None,
    cut: None,
};

/// `vtbl_cp50220_wchar`.
pub static VTBL_CP50220_WCHAR: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_cp5022x_wchar,
    flush: filt_cp5022x_wchar_flush,
};

/// `vtbl_wchar_cp50220`.
pub static VTBL_WCHAR_CP50220: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_wchar_cp50220,
    flush: filt_wchar_cp50220_flush,
};

/// `vtbl_cp50221_wchar`.
pub static VTBL_CP50221_WCHAR: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_cp5022x_wchar,
    flush: filt_cp5022x_wchar_flush,
};

/// `vtbl_wchar_cp50221`.
pub static VTBL_WCHAR_CP50221: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_wchar_cp50221,
    flush: filt_any_jis_flush,
};

/// `vtbl_cp50222_wchar`.
pub static VTBL_CP50222_WCHAR: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_cp5022x_wchar,
    flush: filt_cp5022x_wchar_flush,
};

/// `vtbl_wchar_cp50222`.
pub static VTBL_WCHAR_CP50222: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_wchar_cp50222,
    flush: filt_wchar_cp50222_flush,
};

/// Look up `tbl[i - min]` for `min <= i < min + tbl.len()`.
#[inline]
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
fn tbl_at(tbl: &[u16], min: i32, i: i64) -> Option<u16> {
    let idx = i - i64::from(min);
    if idx < 0 {
        return None;
    }
    tbl.get(idx as usize).copied()
}

/// JIS X 0208 (with the CP932 extensions and the user-defined area) kuten
/// index `s` to Unicode; 0 if unmapped.
fn jisx0208_ext_to_ucs(s: i64) -> u32 {
    if let Some(w) = tbl_at(&ext::CP932EXT1_UCS_TABLE, ext::CP932EXT1_UCS_TABLE_MIN, s) {
        u32::from(w)
    } else if let Some(w) = tbl_at(&jis::JISX0208_UCS_TABLE, 0, s) {
        u32::from(w)
    } else if let Some(w) = tbl_at(&ext::CP932EXT2_UCS_TABLE, ext::CP932EXT2_UCS_TABLE_MIN, s) {
        u32::from(w)
    } else if let Some(w) = tbl_at(&ext::CP932EXT3_UCS_TABLE, ext::CP932EXT3_UCS_TABLE_MIN, s) {
        u32::from(w)
    } else if (94 * 94..114 * 94).contains(&s) {
        // User-defined area => PUA (Microsoft extension)
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let w = (s - 94 * 94 + 0xE000) as u32;
        w
    } else {
        0
    }
}

/// JIS X 0212 kuten index `s` to Unicode; 0 if unmapped.
fn jisx0212_to_ucs(s: i64) -> u32 {
    tbl_at(&jis::JISX0212_UCS_TABLE, 0, s).map_or(0, u32::from)
}

// ---------------------------------------------------------------------------
// Legacy filters
// ---------------------------------------------------------------------------

/// `mbfl_filt_conv_cp5022x_wchar`.
pub fn filt_cp5022x_wchar(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    loop {
        match filter.status & 0xF {
            // ASCII (0x00), X 0201 latin (0x10), X 0201 kana (0x20),
            // X 0208 (0x80), X 0212 (0x90)
            0 => {
                if c == 0x1B {
                    filter.status += 2;
                } else if c == 0x0E {
                    filter.status = 0x20;
                } else if c == 0x0F {
                    filter.status = 0;
                } else if filter.status == 0x10 && c == 0x5C {
                    out.put(0xA5);
                } else if filter.status == 0x10 && c == 0x7E {
                    out.put(0x203E);
                } else if filter.status == 0x20 && c > 0x20 && c < 0x60 {
                    out.put(0xFF40 + c);
                } else if (filter.status == 0x80 || filter.status == 0x90) && c > 0x20 && c <= 0x97
                {
                    filter.cache = c;
                    filter.status += 1;
                } else if (0..0x80).contains(&c) {
                    out.put(c);
                } else if c > 0xA0 && c < 0xE0 {
                    out.put(0xFEC0 + c);
                } else {
                    out.put(BAD_INPUT_I32);
                }
            }
            // X 0208 / X 0212 second byte
            1 => {
                filter.status &= !0xF;
                let c1 = filter.cache;
                if c > 0x20 && c < 0x7F {
                    let s = i64::from((c1 - 0x21) * 94 + c - 0x21);
                    let w = if filter.status == 0x80 {
                        jisx0208_ext_to_ucs(s)
                    } else {
                        jisx0212_to_ucs(s)
                    };
                    #[allow(clippy::cast_possible_wrap)]
                    out.put(if w == 0 { BAD_INPUT_I32 } else { w as i32 });
                } else {
                    out.put(BAD_INPUT_I32);
                }
            }
            // ESC
            2 => {
                if c == 0x24 {
                    filter.status += 1;
                } else if c == 0x28 {
                    filter.status += 3;
                } else {
                    filter.status &= !0xF;
                    out.put(BAD_INPUT_I32);
                    continue;
                }
            }
            // ESC $
            3 => {
                if c == 0x40 || c == 0x42 {
                    filter.status = 0x80;
                } else if c == 0x28 {
                    filter.status += 1;
                } else {
                    filter.status &= !0xF;
                    out.put(BAD_INPUT_I32);
                    out.put(0x24);
                    continue;
                }
            }
            // ESC $ (
            4 => {
                if c == 0x40 || c == 0x42 {
                    filter.status = 0x80;
                } else if c == 0x44 {
                    filter.status = 0x90;
                } else {
                    filter.status &= !0xF;
                    out.put(BAD_INPUT_I32);
                    out.put(0x24);
                    out.put(0x28);
                    continue;
                }
            }
            // ESC (
            5 => {
                if c == 0x42 || c == 0x48 {
                    filter.status = 0;
                } else if c == 0x4A {
                    filter.status = 0x10;
                } else if c == 0x49 {
                    filter.status = 0x20;
                } else {
                    filter.status &= !0xF;
                    out.put(BAD_INPUT_I32);
                    out.put(0x28);
                    continue;
                }
            }
            // Unreachable in C (`ZEND_UNREACHABLE`); reset defensively.
            _ => filter.status &= !0xF,
        }
        return;
    }
}

/// `mbfl_filt_conv_cp5022x_wchar_flush`.
pub fn filt_cp5022x_wchar_flush(filter: &mut Filter, out: &mut dyn Sink) {
    if filter.status & 0xF != 0 {
        // Truncated 2-byte character or escape sequence
        out.put(BAD_INPUT_I32);
    }
    filter.status = 0;
    out.flush();
}

/// Unicode to JIS lookup shared by the legacy CP50221/CP50222 encoders
/// (the first half of `mbfl_filt_conv_wchar_cp5022{1,2}`). Returns the
/// kuten-derived code (`0x1xxxx` = JIS X 0201 latin), or -1 if unmappable.
#[allow(clippy::cast_sign_loss)]
fn legacy_lookup(c: i32) -> i32 {
    let ci = i64::from(c);
    let mut s: i32 = if let Some(v) = tbl_at(&jis::UCS_A1_JIS_TABLE, jis::UCS_A1_JIS_TABLE_MIN, ci)
    {
        i32::from(v)
    } else if c == 0x203E {
        0x1007E
    } else if let Some(v) = tbl_at(&jis::UCS_A2_JIS_TABLE, jis::UCS_A2_JIS_TABLE_MIN, ci) {
        i32::from(v)
    } else if let Some(v) = tbl_at(&jis::UCS_I_JIS_TABLE, jis::UCS_I_JIS_TABLE_MIN, ci) {
        i32::from(v)
    } else if let Some(v) = tbl_at(&jis::UCS_R_JIS_TABLE, jis::UCS_R_JIS_TABLE_MIN, ci) {
        i32::from(v)
    } else if (0xE000..=0xE757).contains(&c) {
        let s = c - 0xE000;
        (((s / 94) + 0x7F) << 8) | ((s % 94) + 0x21)
    } else {
        0
    };

    if s <= 0 {
        s = match c {
            0xA5 => 0x1005C,
            0xFF3C => 0x2140,
            0x2225 => 0x2142,
            0xFF0D => 0x215D,
            0xFFE0 => 0x2171,
            0xFFE1 => 0x2172,
            0xFFE2 => 0x224C,
            _ => s,
        };
    }

    // Not in JIS X 0208 (0), or a JIS X 0213 code: try the CP932 extensions
    if s == 0 || ((s & 0x8000) != 0 && (s & 0x80) != 0) {
        s = -1;
        let oh = ext::CP932EXT1_UCS_TABLE_MIN / 94;
        if let Some(i) = ext::CP932EXT1_UCS_TABLE
            .iter()
            .position(|&x| i32::from(x) == c)
        {
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            let i = i as i32;
            s = ((i / 94 + oh + 0x21) << 8) + (i % 94 + 0x21);
        }
        if s <= 0 {
            let oh = ext::CP932EXT2_UCS_TABLE_MIN / 94;
            if let Some(i) = ext::CP932EXT2_UCS_TABLE
                .iter()
                .position(|&x| i32::from(x) == c)
            {
                #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                let i = i as i32;
                s = ((i / 94 + oh + 0x21) << 8) + (i % 94 + 0x21);
            }
        }
        if c == 0 {
            s = 0;
        } else if s <= 0 {
            s = -1;
        }
    }
    s
}

fn put3(out: &mut dyn Sink, a: i32, b: i32, c: i32) {
    out.put(a);
    out.put(b);
    out.put(c);
}

/// `mbfl_filt_conv_wchar_cp50220`: buffer one code point so halfwidth kana
/// can be glued with a following voiced sound mark, then encode via
/// [`filt_wchar_cp50221`].
pub fn filt_wchar_cp50220(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    let mode = MBFL_HAN2ZEN_KATAKANA | MBFL_HAN2ZEN_GLUE;
    let mut consumed = false;

    if filter.cache != 0 {
        let mut second = 0;
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
        let s = convert_kana_codepoint(
            filter.cache as u32,
            c as u32,
            Some(&mut consumed),
            &mut second,
            mode,
        ) as i32;
        filter.cache = if consumed { 0 } else { c };
        // Terrible hack (as in C) to get CP50220 to emit error markers in
        // the proper position.
        filter.function = filt_wchar_cp50221;
        filt_wchar_cp50221(s, filter, out);
        filter.function = filt_wchar_cp50220;
        if c == 0 && !consumed {
            out.put(0);
        }
    } else if c == 0 {
        // `cache == 0` means nothing is cached, so NUL is handled here
        out.put(0);
    } else {
        filter.cache = c;
    }
}

/// `mbfl_filt_conv_any_jis_flush`: return to ASCII if needed.
pub fn filt_any_jis_flush(filter: &mut Filter, out: &mut dyn Sink) {
    if filter.status & 0xFF00 != 0 {
        put3(out, 0x1B, 0x28, 0x42);
    }
    filter.status = 0;
    out.flush();
}

/// `mbfl_filt_conv_wchar_cp50220_flush`.
pub fn filt_wchar_cp50220_flush(filter: &mut Filter, out: &mut dyn Sink) {
    let mode = MBFL_HAN2ZEN_KATAKANA | MBFL_HAN2ZEN_GLUE;
    if filter.cache != 0 {
        let mut second = 0;
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
        let s = convert_kana_codepoint(filter.cache as u32, 0, None, &mut second, mode) as i32;
        filter.function = filt_wchar_cp50221;
        filt_wchar_cp50221(s, filter, out);
        filter.function = filt_wchar_cp50220;
        filter.cache = 0;
    }
    filt_any_jis_flush(filter, out);
}

/// `mbfl_filt_conv_wchar_cp50221`.
pub fn filt_wchar_cp50221(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    let s = legacy_lookup(c);
    if s < 0 {
        legacy::illegal_output(c, filter, out);
    } else if s < 0x80 {
        if filter.status & 0xFF00 != 0 {
            put3(out, 0x1B, 0x28, 0x42);
            filter.status = 0;
        }
        out.put(s);
    } else if (0xA0..0xE0).contains(&s) {
        if filter.status & 0xFF00 != 0x500 {
            put3(out, 0x1B, 0x28, 0x49);
            filter.status = 0x500;
        }
        out.put(s - 0x80);
    } else if s <= 0x927E {
        if filter.status & 0xFF00 != 0x200 {
            put3(out, 0x1B, 0x24, 0x42);
            filter.status = 0x200;
        }
        out.put((s >> 8) & 0xFF);
        out.put(s & 0xFF);
    } else if s < 0x10000 {
        // JIS X 0212
        legacy::illegal_output(c, filter, out);
    } else {
        // JIS X 0201 latin
        if filter.status & 0xFF00 != 0x400 {
            put3(out, 0x1B, 0x28, 0x4A);
        }
        filter.status = 0x400;
        out.put(s & 0x7F);
    }
}

/// `mbfl_filt_conv_wchar_cp50222`.
pub fn filt_wchar_cp50222(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    let s = legacy_lookup(c);
    if s < 0 {
        legacy::illegal_output(c, filter, out);
    } else if s < 0x80 {
        if filter.status & 0xFF00 == 0x500 {
            out.put(0x0F);
            filter.status = 0;
        } else if filter.status & 0xFF00 != 0 {
            put3(out, 0x1B, 0x28, 0x42);
            filter.status = 0;
        }
        out.put(s);
    } else if (0xA0..0xE0).contains(&s) {
        if filter.status & 0xFF00 != 0x500 {
            out.put(0x0E);
            filter.status = 0x500;
        }
        out.put(s - 0x80);
    } else if s <= 0x927E {
        if filter.status & 0xFF00 == 0x500 {
            out.put(0x0F);
            filter.status = 0;
        }
        if filter.status & 0xFF00 != 0x200 {
            put3(out, 0x1B, 0x24, 0x42);
            filter.status = 0x200;
        }
        out.put((s >> 8) & 0xFF);
        out.put(s & 0xFF);
    } else if s < 0x10000 {
        // JIS X 0212
        legacy::illegal_output(c, filter, out);
    } else {
        // JIS X 0201 latin
        if filter.status & 0xFF00 == 0x500 {
            out.put(0x0F);
            filter.status = 0;
        }
        if filter.status & 0xFF00 != 0x400 {
            put3(out, 0x1B, 0x28, 0x4A);
        }
        filter.status = 0x400;
        out.put(s & 0x7F);
    }
}

/// `mbfl_filt_conv_wchar_cp50222_flush`.
pub fn filt_wchar_cp50222_flush(filter: &mut Filter, out: &mut dyn Sink) {
    if filter.status & 0xFF00 == 0x500 {
        out.put(0x0F);
    } else if filter.status & 0xFF00 != 0 {
        put3(out, 0x1B, 0x28, 0x42);
    }
    filter.status = 0;
    out.flush();
}

// ---------------------------------------------------------------------------
// Fast conversion
// ---------------------------------------------------------------------------

const ASCII: u32 = 0;
const JISX_0201_LATIN: u32 = 1;
const JISX_0201_KANA: u32 = 2;
const JISX_0208: u32 = 3;
const JISX_0212: u32 = 4;

/// `mb_cp5022x_to_wchar`.
#[allow(clippy::too_many_lines, clippy::many_single_char_names)]
pub fn cp5022x_to_wchar(input: &mut &[u8], buf: &mut [u32], state: &mut u32) -> usize {
    debug_assert!(buf.len() >= 3);
    let s = *input;
    let e = s.len();
    let limit = buf.len();
    let mut p = 0usize;
    let mut out = 0usize;

    while p < e && out < limit {
        let c = s[p];
        p += 1;

        if c == 0x1B {
            // Escape sequence
            if e - p < 2 {
                buf[out] = BAD_INPUT;
                out += 1;
                // Duplicate error-handling behavior of legacy code
                if p < e && (s[p] == b'(' || s[p] == b'$') {
                    p += 1;
                }
                continue;
            }
            let c2 = s[p];
            p += 1;
            if c2 == b'$' {
                let c3 = s[p];
                p += 1;
                if c3 == b'@' || c3 == b'B' {
                    *state = JISX_0208;
                } else if c3 == b'(' {
                    if p == e {
                        buf[out] = BAD_INPUT;
                        out += 1;
                        break;
                    }
                    let c4 = s[p];
                    p += 1;
                    if c4 == b'@' || c4 == b'B' {
                        *state = JISX_0208;
                    } else if c4 == b'D' {
                        *state = JISX_0212;
                    } else {
                        if limit - out < 3 {
                            p -= 4;
                            break;
                        }
                        buf[out] = BAD_INPUT;
                        buf[out + 1] = u32::from(b'$');
                        buf[out + 2] = u32::from(b'(');
                        out += 3;
                        p -= 1;
                    }
                } else {
                    if limit - out < 2 {
                        p -= 3;
                        break;
                    }
                    buf[out] = BAD_INPUT;
                    buf[out + 1] = u32::from(b'$');
                    out += 2;
                    p -= 1;
                }
            } else if c2 == b'(' {
                let c3 = s[p];
                p += 1;
                if c3 == b'B' || c3 == b'H' {
                    *state = ASCII;
                } else if c3 == b'J' {
                    *state = JISX_0201_LATIN;
                } else if c3 == b'I' {
                    *state = JISX_0201_KANA;
                } else {
                    if limit - out < 2 {
                        p -= 3;
                        break;
                    }
                    buf[out] = BAD_INPUT;
                    buf[out + 1] = u32::from(b'(');
                    out += 2;
                    p -= 1;
                }
            } else {
                buf[out] = BAD_INPUT;
                out += 1;
                p -= 1;
            }
        } else if c == 0x0E {
            *state = JISX_0201_KANA;
        } else if c == 0x0F {
            *state = ASCII;
        } else if *state == JISX_0201_LATIN && c == 0x5C {
            buf[out] = 0xA5;
            out += 1;
        } else if *state == JISX_0201_LATIN && c == 0x7E {
            buf[out] = 0x203E;
            out += 1;
        } else if *state == JISX_0201_KANA && c > 0x20 && c < 0x60 {
            buf[out] = 0xFF40 + u32::from(c);
            out += 1;
        } else if *state >= JISX_0208 && c > 0x20 && c <= 0x97 {
            if p == e {
                buf[out] = BAD_INPUT;
                out += 1;
                break;
            }
            let c2 = s[p];
            p += 1;
            if c2 > 0x20 && c2 < 0x7F {
                let k = (i64::from(c) - 0x21) * 94 + i64::from(c2) - 0x21;
                let w = if *state == JISX_0208 {
                    jisx0208_ext_to_ucs(k)
                } else {
                    jisx0212_to_ucs(k)
                };
                buf[out] = if w == 0 { BAD_INPUT } else { w };
            } else {
                buf[out] = BAD_INPUT;
            }
            out += 1;
        } else if c < 0x80 {
            buf[out] = u32::from(c);
            out += 1;
        } else if (0xA1..=0xDF).contains(&c) {
            buf[out] = 0xFEC0 + u32::from(c);
            out += 1;
        } else {
            buf[out] = BAD_INPUT;
            out += 1;
        }
    }

    *input = &s[p..];
    out
}

/// `lookup_wchar`: Unicode to JIS code for the `CP5022x` encoders (0 if not
/// representable, except for U+0000 which maps to 0 legitimately).
fn lookup_wchar(w: u32) -> u32 {
    let wi = i64::from(w);
    let mut s: u32 = if let Some(v) = tbl_at(&jis::UCS_A1_JIS_TABLE, jis::UCS_A1_JIS_TABLE_MIN, wi)
    {
        u32::from(v)
    } else if w == 0x203E {
        0x1007E
    } else if let Some(v) = tbl_at(&jis::UCS_A2_JIS_TABLE, jis::UCS_A2_JIS_TABLE_MIN, wi) {
        u32::from(v)
    } else if let Some(v) = tbl_at(&jis::UCS_I_JIS_TABLE, jis::UCS_I_JIS_TABLE_MIN, wi) {
        u32::from(v)
    } else if let Some(v) = tbl_at(&jis::UCS_R_JIS_TABLE, jis::UCS_R_JIS_TABLE_MIN, wi) {
        u32::from(v)
    } else if (0xE000..=0xE757).contains(&w) {
        let s = w - 0xE000;
        (((s / 94) + 0x7F) << 8) | ((s % 94) + 0x21)
    } else {
        0
    };

    if s == 0 {
        s = match w {
            0xA5 => 0x1005C,
            0xFF3C => 0x2140,
            0x2225 => 0x2142,
            0xFF0D => 0x215D,
            0xFFE0 => 0x2171,
            0xFFE1 => 0x2172,
            0xFFE2 => 0x224C,
            0 => return 0,
            _ => 0,
        };
    }

    if s == 0 || s >= 0x8080 {
        let found = |tbl: &[u16], min: i32| {
            tbl.iter().position(|&x| u32::from(x) == w).map(|i| {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let (i, oh) = (i as u32, (min / 94) as u32);
                (((i / 94) + oh + 0x21) << 8) + (i % 94) + 0x21
            })
        };
        if let Some(v) = found(&ext::CP932EXT1_UCS_TABLE, ext::CP932EXT1_UCS_TABLE_MIN) {
            return v;
        }
        if let Some(v) = found(&ext::CP932EXT2_UCS_TABLE, ext::CP932EXT2_UCS_TABLE_MIN) {
            return v;
        }
    }
    s
}

/// Encode one (already looked-up) code point as CP50220/CP50221 do; errors
/// go through `err_fn`.
#[allow(clippy::cast_possible_truncation)]
fn emit_cp50221(w: u32, buf: &mut ConvertBuf, err_fn: crate::convert::FromWcharFn) {
    let s = lookup_wchar(w);
    if s == 0 && w != 0 {
        illegal_output(w, err_fn, buf);
    } else if s < 0x80 {
        if buf.state != ASCII {
            buf.extend(b"\x1B(B");
            buf.state = ASCII;
        }
        buf.push(s as u8);
    } else if (0xA0..0xE0).contains(&s) {
        if buf.state != JISX_0201_KANA {
            buf.extend(b"\x1B(I");
            buf.state = JISX_0201_KANA;
        }
        buf.push((s - 0x80) as u8);
    } else if s <= 0x927E {
        if buf.state != JISX_0208 {
            buf.extend(b"\x1B$B");
            buf.state = JISX_0208;
        }
        buf.push(((s >> 8) & 0xFF) as u8);
        buf.push((s & 0xFF) as u8);
    } else if s >= 0x10000 {
        // JIS X 0201 latin, tagged by adding 0x10000
        if buf.state != JISX_0201_LATIN {
            buf.extend(b"\x1B(J");
            buf.state = JISX_0201_LATIN;
        }
        buf.push((s & 0x7F) as u8);
    } else {
        illegal_output(w, err_fn, buf);
    }
}

/// `mb_wchar_to_cp50220`: like CP50221, but halfwidth katakana are first
/// converted to fullwidth (combining voiced sound marks). A halfwidth kana
/// at the end of a non-final buffer is kept in `buf.state` (bits 8+) until
/// the next call.
pub fn wchar_to_cp50220(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    let len = input.len();
    let mut i = 0usize;
    let mut pending = None;

    if buf.state & 0xFF_FF00 != 0 {
        // Reprocess cached code point
        pending = Some(buf.state >> 8);
        buf.state &= 0xFF;
    }

    loop {
        let w = if let Some(w) = pending.take() {
            w
        } else if i < len {
            i += 1;
            input[i - 1]
        } else {
            break;
        };
        let remaining = len - i;

        if (0xFF61..=0xFF9F).contains(&w) && remaining == 0 && !end {
            // May need to combine with the next code point, which will come
            // in a separate buffer
            buf.state |= w << 8;
            break;
        }

        let mut consumed = false;
        let mut second = 0;
        let next = if remaining > 0 { input[i] } else { 0 };
        let w = convert_kana_codepoint(
            w,
            next,
            Some(&mut consumed),
            &mut second,
            MBFL_HAN2ZEN_KATAKANA | MBFL_HAN2ZEN_GLUE,
        );
        if consumed {
            // Two successive code points were converted into one
            i += 1;
        }

        emit_cp50221(w, buf, wchar_to_cp50221);
    }

    if end && buf.state != ASCII {
        buf.extend(b"\x1B(B");
    }
}

/// `mb_wchar_to_cp50221`.
pub fn wchar_to_cp50221(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    for &w in input {
        emit_cp50221(w, buf, wchar_to_cp50221);
    }
    if end && buf.state != ASCII {
        buf.extend(b"\x1B(B");
    }
}

/// `mb_wchar_to_cp50222`.
#[allow(clippy::cast_possible_truncation)]
pub fn wchar_to_cp50222(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    for &w in input {
        let s = lookup_wchar(w);
        if s == 0 && w != 0 {
            illegal_output(w, wchar_to_cp50222, buf);
        } else if s < 0x80 {
            if buf.state == JISX_0201_KANA {
                buf.push(0x0F);
                buf.state = ASCII;
            } else if buf.state != ASCII {
                buf.extend(b"\x1B(B");
                buf.state = ASCII;
            }
            buf.push(s as u8);
        } else if (0xA0..0xE0).contains(&s) {
            if buf.state != JISX_0201_KANA {
                buf.push(0x0E);
                buf.state = JISX_0201_KANA;
            }
            buf.push((s - 0x80) as u8);
        } else if s <= 0x927E {
            if buf.state == JISX_0201_KANA {
                buf.push(0x0F);
            }
            if buf.state != JISX_0208 {
                buf.extend(b"\x1B$B");
                buf.state = JISX_0208;
            }
            buf.push(((s >> 8) & 0xFF) as u8);
            buf.push((s & 0xFF) as u8);
        } else if s >= 0x10000 {
            if buf.state == JISX_0201_KANA {
                buf.push(0x0F);
            }
            if buf.state != JISX_0201_LATIN {
                buf.extend(b"\x1B(J");
                buf.state = JISX_0201_LATIN;
            }
            buf.push((s & 0x7F) as u8);
        } else {
            illegal_output(w, wchar_to_cp50222, buf);
        }
    }

    if end {
        if buf.state == JISX_0201_KANA {
            buf.push(0x0F);
        } else if buf.state != ASCII {
            buf.extend(b"\x1B(B");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codecs::cjk::testutil::{
        self, data_file, decode, decode_bufsize, encode, encode_split, legacy_decode,
        legacy_encode, mapping_lines,
    };
    use std::collections::HashMap;

    const ENCS: [&Encoding; 3] = [&ENC_CP50220, &ENC_CP50221, &ENC_CP50222];
    const B: u32 = BAD_INPUT;

    /// Deterministic xorshift PRNG for fuzz-style tests.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            #[allow(clippy::cast_possible_truncation)]
            let r = (self.0 % n as u64) as usize;
            r
        }
    }

    fn dec_both(e: &'static Encoding, bytes: &[u8]) -> Vec<u32> {
        let fast = decode(e, bytes);
        assert_eq!(
            legacy_decode(e, bytes),
            fast,
            "legacy decode of {bytes:02x?}"
        );
        fast
    }

    fn enc_both(e: &'static Encoding, cps: &[u32]) -> Vec<u8> {
        let (fast, _) = encode(e, cps);
        assert_eq!(encode_split(e, cps), fast, "split encode of {cps:x?}");
        assert_eq!(legacy_encode(e, cps), fast, "legacy encode of {cps:x?}");
        fast
    }

    /// `shiftJISDecode` from `cp5022x_encoding.phpt`: CP932 bytes -> kuten.
    fn shift_jis_decode(bytes: u64) -> [u8; 2] {
        let first = (bytes >> 8) & 0xFF;
        let second = bytes & 0xFF;
        let hi = first - if first > 0x9F { 0xE0 - 31 } else { 0x81 };
        let k = if second > 0x9E {
            (((hi << 1) + 0x22) << 8) + (second - 0x9F + 0x21)
        } else if second > 0x7F {
            (((hi << 1) + 0x21) << 8) + (second - 0x80 + 63 + 0x21)
        } else {
            (((hi << 1) + 0x21) << 8) + (second - 0x40 + 0x21)
        };
        #[allow(clippy::cast_possible_truncation)]
        [(k >> 8) as u8, k as u8]
    }

    /// (kuten -> code point) maps for CP932's double-byte part, as the
    /// .phpt builds them: (invertible, non-invertible).
    #[allow(clippy::type_complexity)]
    fn cp932_chars() -> Option<(HashMap<[u8; 2], u32>, HashMap<[u8; 2], u32>)> {
        let text = data_file("CP932.txt")?;
        let mut chars = HashMap::new();
        let mut non_inv = HashMap::new();
        let mut from_uni = std::collections::HashSet::new();
        for l in mapping_lines(&text) {
            if l.len() < 2 || l[0][0] < 256 {
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            let cp = l[1][0] as u32;
            let k = shift_jis_decode(l[0][0]);
            if from_uni.insert(cp) {
                chars.insert(k, cp);
            } else {
                non_inv.insert(k, cp);
            }
        }
        let mut cp = 0xE000;
        for i in 0xF0u64..=0xF9 {
            for j in 0x40u64..=0xFC {
                if j != 0x7F {
                    chars.insert(shift_jis_decode((i << 8) + j), cp);
                    cp += 1;
                }
            }
        }
        for (k, v) in [
            ([0x21, 0x41], 0x301C),
            ([0x21, 0x42], 0x2016),
            ([0x21, 0x5D], 0x2212),
            ([0x21, 0x71], 0xA2),
            ([0x21, 0x72], 0xA3),
            ([0x22, 0x4C], 0xAC),
        ] {
            chars.insert(k, v);
        }
        Some((chars, non_inv))
    }

    #[test]
    fn oracle_fixture() {
        assert!(testutil::replay_oracle("cp5022x_oracle.tsv") > 10_000);
    }

    #[test]
    fn ascii_and_invalid_single_bytes() {
        for e in ENCS {
            for i in 0u8..0x80 {
                if i == 0xE || i == 0xF || i == 0x1B {
                    continue;
                }
                let w = vec![u32::from(i)];
                assert_eq!(dec_both(e, &[i]), w);
                assert_eq!(dec_both(e, &[0x1B, b'(', b'B', i]), w);
                assert_eq!(dec_both(e, &[0x0F, i]), w);
                assert_eq!(enc_both(e, &w), vec![i]);
            }
            for i in 0x80u8..=0xFF {
                let expect = if (0xA1..=0xDF).contains(&i) {
                    0xFEC0 + u32::from(i)
                } else {
                    B
                };
                assert_eq!(dec_both(e, &[i]), vec![expect]);
                assert_eq!(dec_both(e, &[0x1B, b'(', b'I', i]), vec![expect]);
                assert_eq!(dec_both(e, &[0x1B, b'(', b'J', i]), vec![expect]);
            }
            assert_eq!(
                enc_both(e, &[0x3000, 0x61, 0x62, 0x63]),
                b"\x1B$B\x21\x21\x1B(Babc"
            );
        }
    }

    #[test]
    fn jisx0201() {
        for e in ENCS {
            for k in 0x21u8..0x60 {
                let w = vec![0xFF40 + u32::from(k)];
                assert_eq!(dec_both(e, &[0x1B, b'(', b'I', k]), w);
                assert_eq!(dec_both(e, &[0x0E, k]), w);
                assert_eq!(dec_both(e, &[0x1B, b'(', b'J', 0x0E, k]), w);
            }
            assert_eq!(dec_both(e, b"\x1B(J\x5C\x7E\x41"), vec![0xA5, 0x203E, 0x41]);
            assert_eq!(enc_both(e, &[0xA5, 0x203E]), b"\x1B(J\x5C\x7E\x1B(B");
        }
        assert_eq!(
            enc_both(&ENC_CP50221, &[0xFF61, 0x41]),
            b"\x1B(I\x21\x1B(BA"
        );
        assert_eq!(enc_both(&ENC_CP50222, &[0xFF61, 0x41]), b"\x0E\x21\x0FA");
        assert_eq!(
            enc_both(&ENC_CP50222, &[0xFF61, 0x2225]),
            b"\x0E\x21\x0F\x1B$B\x21\x42\x1B(B"
        );
        assert_eq!(
            enc_both(&ENC_CP50222, &[0xFF61, 0x203E]),
            b"\x0E\x21\x0F\x1B(J\x7E\x1B(B"
        );
        assert_eq!(enc_both(&ENC_CP50222, &[0xFF61]), b"\x0E\x21\x0F");
    }

    #[test]
    fn cp932_charset_both_ways() {
        let Some((chars, non_inv)) = cp932_chars() else {
            return;
        };
        for e in ENCS {
            for (k, &cp) in &chars {
                let bytes = [0x1B, b'$', b'B', k[0], k[1]];
                assert_eq!(dec_both(e, &bytes), vec![cp], "{} {k:02x?}", e.name);
                let mut back = bytes.to_vec();
                back.extend_from_slice(b"\x1B(B");
                assert_eq!(enc_both(e, &[cp]), back, "{} U+{cp:04X}", e.name);
            }
            for (k, &cp) in &non_inv {
                assert_eq!(dec_both(e, &[0x1B, b'$', b'B', k[0], k[1]]), vec![cp]);
            }
            for (cp, k) in [
                (0x2225, b"\x21\x42"),
                (0xFF0D, b"\x21\x5D"),
                (0xFFE0, b"\x21\x71"),
                (0xFFE1, b"\x21\x72"),
                (0xFFE2, b"\x22\x4C"),
            ] {
                let mut exp = b"\x1B$B".to_vec();
                exp.extend_from_slice(k);
                exp.extend_from_slice(b"\x1B(B");
                assert_eq!(enc_both(e, &[cp]), exp);
            }
        }
        // All invalid 2-byte characters (and truncated ones)
        for i in 0x21u8..=0x97 {
            for j in 0u8..=255 {
                let k = [i, j];
                if chars.contains_key(&k) || non_inv.contains_key(&k) {
                    continue;
                }
                assert_eq!(dec_both(&ENC_CP50221, &[0x1B, b'$', b'B', i, j]), vec![B]);
            }
            assert_eq!(dec_both(&ENC_CP50222, &[0x1B, b'$', b'B', i]), vec![B]);
        }
        assert_eq!(dec_both(&ENC_CP50220, b"\x1B$(B\x21\x21"), vec![0x3000]);
    }

    #[test]
    fn jisx0212_decoding() {
        let Some(text) = data_file("JISX0212.txt") else {
            return;
        };
        let mut chars = HashMap::new();
        for l in mapping_lines(&text) {
            #[allow(clippy::cast_possible_truncation)]
            chars.insert([(l[0][0] >> 8) as u8, l[0][0] as u8], l[1][0] as u32);
        }
        for i in 0x21u8..=0x97 {
            for j in 0u8..=255 {
                let expect = chars.get(&[i, j]).copied().unwrap_or(B);
                assert_eq!(
                    dec_both(&ENC_CP50221, &[0x1B, b'$', b'(', b'D', i, j]),
                    vec![expect]
                );
            }
            assert_eq!(
                dec_both(&ENC_CP50220, &[0x1B, b'$', b'(', b'D', i]),
                vec![B]
            );
        }
        // JIS X 0212 is not encodable
        for e in ENCS {
            assert_eq!(encode(e, &[0x02D8]), (b"?".to_vec(), 1));
        }
    }

    #[test]
    fn invalid_escape_sequences() {
        let q = |s: &str| -> Vec<u32> { s.chars().map(u32::from).collect() };
        for e in ENCS {
            for (input, rest) in [
                (&b"\x1B"[..], ""),
                (b"\x1BX", "X"),
                (b"\x1B(", ""),
                (b"\x1B(X", "(X"),
                (b"\x1B$", ""),
                (b"\x1B$(", ""),
                (b"\x1B$X", "$X"),
                (b"\x1B$(X", "$(X"),
            ] {
                let mut exp = vec![B];
                exp.extend(q(rest));
                assert_eq!(decode(e, input), exp, "{input:02x?}");
                assert!(!testutil::check(e, input));
            }
            assert_eq!(dec_both(e, b"\x1BC\xF5"), vec![B, 0x43, B]);
        }
        assert_eq!(dec_both(&ENC_CP50220, b"ab\x00"), vec![0x61, 0x62, 0]);
    }

    #[test]
    fn invalid_unicode_and_error_order() {
        for e in ENCS {
            assert_eq!(encode(e, &[0xD800]), (b"?".to_vec(), 1));
            assert_eq!(encode(e, &[0xFFFF, 0x26]), (b"?&".to_vec(), 1));
            assert_eq!(legacy_encode(e, &[0xFFFF, 0x26]), b"?&");
            assert_eq!(encode(e, &[B]), (b"?".to_vec(), 1));
        }
    }

    #[test]
    fn cp50220_folds_halfwidth_katakana() {
        let Some(phpt) = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../modules/php-src/ext/mbstring/tests/cp5022x_encoding.phpt"),
        )
        .ok() else {
            return;
        };
        let mut n = 0;
        for line in phpt.lines() {
            let t = line.trim();
            if !t.starts_with("0xFF") || !t.contains("=>") {
                continue;
            }
            let cp = u32::from_str_radix(&t[2..6], 16).expect("cp");
            let k = u32::from_str_radix(&t[t.find("=> 0x").expect("=>") + 5..][..4], 16)
                .expect("kuten");
            let mut exp = b"\x1B$B".to_vec();
            #[allow(clippy::cast_possible_truncation)]
            exp.extend([(k >> 8) as u8, k as u8]);
            exp.extend_from_slice(b"\x1B(B");
            assert_eq!(enc_both(&ENC_CP50220, &[cp]), exp, "U+{cp:04X}");
            n += 1;
        }
        assert_eq!(n, 63);
        // Glue a voiced sound mark, also across buffer boundaries
        for i in 0..256 {
            let mut input = vec![0x61; i];
            input.extend([0xFF76, 0xFF9E]);
            let mut exp = vec![b'a'; i];
            exp.extend_from_slice(b"\x1B$B%,\x1B(B");
            assert_eq!(enc_both(&ENC_CP50220, &input), exp);
            let (out, errs) = crate::convert::fast_convert(
                &crate::convert::encode_all(
                    &input,
                    crate::encoding::utf8(),
                    0x3F,
                    crate::convert::ErrorMode::Char,
                ),
                crate::encoding::utf8(),
                &ENC_CP50220,
                0x3F,
                crate::convert::ErrorMode::Char,
            );
            assert_eq!((out, errs), (exp, 0));
        }
        let input = [0xFF76, 0xFF9E, 0, 0x41];
        assert_eq!(encode(&ENC_CP50220, &input).0, b"\x1B$B%,\x1B(B\x00A");
        // The legacy filter emits NUL raw, without leaving JIS X 0208 mode
        // (faithful to `mbfl_filt_conv_wchar_cp50220`).
        assert_eq!(legacy_encode(&ENC_CP50220, &input), b"\x1B$B%,\x00\x1B(BA");
    }

    #[test]
    fn small_buffers_and_legacy_agree_on_fuzz() {
        let alpha: &[u8] =
            b"\x1B$(B@DJIH\x0E\x0F\x21\x30\x5C\x7E\x7F\x80\x97\xA1\xDF\xE0\x20\x2D\x79\x7C\x22a";
        let mut rng = Rng(0x5022_5022);
        for _ in 0..4000 {
            let len = 1 + rng.next(20);
            let s: Vec<u8> = (0..len).map(|_| alpha[rng.next(alpha.len())]).collect();
            let full = decode(&ENC_CP50221, &s);
            for n in [3, 4, 5, 8] {
                assert_eq!(
                    decode_bufsize(&ENC_CP50221, &s, n),
                    full,
                    "bufsize {n} {s:02x?}"
                );
            }
            assert_eq!(legacy_decode(&ENC_CP50221, &s), full, "legacy {s:02x?}");
        }
        let pool = [
            0x61, 0x5C, 0x7E, 0xA5, 0x203E, 0x3042, 0x4E00, 0xFF61, 0xFF71, 0xFF76, 0xFF73, 0xFF8A,
            0xFF9E, 0xFF9F, 0x2460, 0xE000, 0x02D8, 0x1F600, 0xFF3C, 0x30AB, 0x309B,
        ];
        for _ in 0..4000 {
            let len = 1 + rng.next(12);
            let cps: Vec<u32> = (0..len).map(|_| pool[rng.next(pool.len())]).collect();
            for e in ENCS {
                enc_both(e, &cps);
            }
        }
    }
}

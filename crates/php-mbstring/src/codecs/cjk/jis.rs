//! ISO-2022-JP family: `JIS`, `ISO-2022-JP`, `ISO-2022-JP-MOBILE#KDDI`,
//! `ISO-2022-JP-MS`.
//!
//! All four are stateful (escape sequences switch character sets), so besides
//! the fast buffer-oriented codecs they also provide the legacy streaming
//! filters which `mb_strcut` uses.
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c`.

use crate::codecs::cjk::common::{
    bisec_srch2, nflags, sjis_emoji_kddi2unicode, NFLAGS_CODE_KDDI, NFLAGS_S,
};
use crate::convert::{illegal_output, ConvertBuf, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};
use crate::legacy::{self, Filter, Sink, Vtbl, BAD_INPUT_I32};
use crate::tables::cjk::cp932_ext as ext;
use crate::tables::cjk::emoji as em;
use crate::tables::cjk::jis as t;

/* ------------------------------------------------------------------------ */
/* Encodings and legacy filter tables                                        */
/* ------------------------------------------------------------------------ */

/// `JIS`.
pub static ENC_JIS: Encoding = Encoding {
    no: EncodingNo::Jis,
    name: "JIS",
    mime_name: Some("ISO-2022-JP"),
    aliases: &[],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: Some(&VTBL_JIS_WCHAR),
    output_filter: Some(&VTBL_WCHAR_JIS),
    to_wchar: iso2022jp_to_wchar,
    from_wchar: wchar_to_jis,
    check: Some(check_jis),
    cut: None,
};

/// `ISO-2022-JP`.
pub static ENC_2022JP: Encoding = Encoding {
    no: EncodingNo::Iso2022Jp,
    name: "ISO-2022-JP",
    mime_name: Some("ISO-2022-JP"),
    aliases: &[],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: Some(&VTBL_2022JP_WCHAR),
    output_filter: Some(&VTBL_WCHAR_2022JP),
    to_wchar: iso2022jp_to_wchar,
    from_wchar: wchar_to_iso2022jp,
    check: Some(check_iso2022jp),
    cut: None,
};

/// `ISO-2022-JP-MOBILE#KDDI`.
pub static ENC_2022JP_KDDI: Encoding = Encoding {
    no: EncodingNo::Iso2022JpKddi,
    name: "ISO-2022-JP-MOBILE#KDDI",
    mime_name: Some("ISO-2022-JP"),
    aliases: &["ISO-2022-JP-KDDI"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: Some(&VTBL_2022JP_KDDI_WCHAR),
    output_filter: Some(&VTBL_WCHAR_2022JP_KDDI),
    to_wchar: iso2022jp_kddi_to_wchar,
    from_wchar: wchar_to_iso2022jp_kddi,
    check: None,
    cut: None,
};

/// `ISO-2022-JP-MS`.
pub static ENC_2022JPMS: Encoding = Encoding {
    no: EncodingNo::Iso2022JpMs,
    name: "ISO-2022-JP-MS",
    mime_name: Some("ISO-2022-JP"),
    aliases: &["ISO2022JPMS"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: Some(&VTBL_2022JPMS_WCHAR),
    output_filter: Some(&VTBL_WCHAR_2022JPMS),
    to_wchar: iso2022jpms_to_wchar,
    from_wchar: wchar_to_iso2022jpms,
    check: None,
    cut: None,
};

/// `vtbl_jis_wchar`.
pub static VTBL_JIS_WCHAR: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_jis_wchar,
    flush: filt_jis_wchar_flush,
};

/// `vtbl_wchar_jis`.
pub static VTBL_WCHAR_JIS: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_wchar_jis,
    flush: filt_any_jis_flush,
};

/// `vtbl_2022jp_wchar` (same functions as [`VTBL_JIS_WCHAR`]).
pub static VTBL_2022JP_WCHAR: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_jis_wchar,
    flush: filt_jis_wchar_flush,
};

/// `vtbl_wchar_2022jp`.
pub static VTBL_WCHAR_2022JP: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_wchar_2022jp,
    flush: filt_any_jis_flush,
};

/// `vtbl_2022jp_kddi_wchar`.
pub static VTBL_2022JP_KDDI_WCHAR: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_2022jp_mobile_wchar,
    flush: filt_2022jp_mobile_wchar_flush,
};

/// `vtbl_wchar_2022jp_kddi`.
pub static VTBL_WCHAR_2022JP_KDDI: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_wchar_2022jp_mobile,
    flush: filt_wchar_2022jp_mobile_flush,
};

/// `vtbl_2022jpms_wchar`.
pub static VTBL_2022JPMS_WCHAR: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_2022jpms_wchar,
    flush: filt_2022jpms_wchar_flush,
};

/// `vtbl_wchar_2022jpms`.
pub static VTBL_WCHAR_2022JPMS: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_wchar_2022jpms,
    flush: filt_any_2022jpms_flush,
};

/* ------------------------------------------------------------------------ */
/* Table helpers                                                              */
/* ------------------------------------------------------------------------ */

/// `tbl[w - min]` if `min <= w < min + tbl.len()`.
#[inline]
fn lookup(w: u32, tbl: &[u16], min: i32) -> Option<u32> {
    let i = i64::from(w) - i64::from(min);
    usize::try_from(i)
        .ok()
        .and_then(|i| tbl.get(i))
        .map(|&v| u32::from(v))
}

/// The `ucs_a1/a2/i/r_jis_table` lookup chain shared by every encoder here
/// (0 if `w` is outside all ranges; the ranges do not overlap).
#[inline]
fn ucs_to_jis(w: u32) -> u32 {
    lookup(w, &t::UCS_A1_JIS_TABLE, t::UCS_A1_JIS_TABLE_MIN)
        .or_else(|| lookup(w, &t::UCS_A2_JIS_TABLE, t::UCS_A2_JIS_TABLE_MIN))
        .or_else(|| lookup(w, &t::UCS_I_JIS_TABLE, t::UCS_I_JIS_TABLE_MIN))
        .or_else(|| lookup(w, &t::UCS_R_JIS_TABLE, t::UCS_R_JIS_TABLE_MIN))
        .unwrap_or(0)
}

/// [`ucs_to_jis`] with the C `int` interface of the legacy filters (negative
/// values match no table).
#[inline]
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
fn ucs_to_jis_i(c: i32) -> i32 {
    if c < 0 {
        0
    } else {
        ucs_to_jis(c as u32) as i32
    }
}

/// Special mappings tried by the ISO-2022-JP/JIS encoders when the tables
/// have none (`YEN SIGN` goes to JIS X 0201 Roman).
#[inline]
fn jis_fallback(w: u32) -> u32 {
    match w {
        0xA5 => 0x1005C,  /* YEN SIGN */
        0xFF3C => 0x2140, /* FULLWIDTH REVERSE SOLIDUS */
        0x2225 => 0x2142, /* PARALLEL TO */
        0xFF0D => 0x215D, /* FULLWIDTH HYPHEN-MINUS */
        0xFFE0 => 0x2171, /* FULLWIDTH CENT SIGN */
        0xFFE1 => 0x2172, /* FULLWIDTH POUND SIGN */
        0xFFE2 => 0x224C, /* FULLWIDTH NOT SIGN */
        _ => 0,
    }
}

/// Like [`jis_fallback`] but `YEN SIGN` becomes `FULLWIDTH YEN SIGN`
/// (KDDI and MS variants).
#[inline]
fn ms_fallback(w: u32) -> u32 {
    if w == 0xA5 {
        0x216F
    } else {
        jis_fallback(w)
    }
}

/// Microsoft-style decoding of the first rows of JIS X 0208 (row 1-2
/// characters which CP932 maps to fullwidth forms); 0 if not special.
#[inline]
fn ms_row1_special(s: u32) -> u32 {
    match s {
        31 => 0xFF3C,  /* FULLWIDTH REVERSE SOLIDUS */
        32 => 0xFF5E,  /* FULLWIDTH TILDE */
        33 => 0x2225,  /* PARALLEL TO */
        60 => 0xFF0D,  /* FULLWIDTH HYPHEN-MINUS */
        80 => 0xFFE0,  /* FULLWIDTH CENT SIGN */
        81 => 0xFFE1,  /* FULLWIDTH POUND SIGN */
        137 => 0xFFE2, /* FULLWIDTH NOT SIGN */
        _ => 0,
    }
}

#[inline]
fn jisx0208(s: u32) -> u32 {
    usize::try_from(s)
        .ok()
        .and_then(|i| t::JISX0208_UCS_TABLE.get(i))
        .map_or(0, |&v| u32::from(v))
}

#[inline]
fn jisx0212(s: u32) -> u32 {
    usize::try_from(s)
        .ok()
        .and_then(|i| t::JISX0212_UCS_TABLE.get(i))
        .map_or(0, |&v| u32::from(v))
}

/// Index of `w` in `cp932ext1_ucs_table` (linear search, like the C code).
#[inline]
fn cp932ext1_index(w: u32) -> Option<u32> {
    #[allow(clippy::cast_possible_truncation)]
    ext::CP932EXT1_UCS_TABLE
        .iter()
        .position(|&v| u32::from(v) == w)
        .map(|i| i as u32)
}

/// Index of `w` in `cp932ext3_ucs_table` (linear search, like the C code).
#[inline]
fn cp932ext3_index(w: u32) -> Option<u32> {
    #[allow(clippy::cast_possible_truncation)]
    ext::CP932EXT3_UCS_TABLE
        .iter()
        .position(|&v| u32::from(v) == w)
        .map(|i| i as u32)
}

/// JIS code of the `i`-th entry of `cp932ext1_ucs_table` (row 13).
#[inline]
const fn cp932ext1_jis(i: u32) -> u32 {
    (((i / 94) + 0x2D) << 8) + (i % 94) + 0x21
}

/// `sjistoidx(c1, c2)`.
const fn sjistoidx(c1: i32, c2: i32) -> i32 {
    let sub = if c2 > 0x7E { 0x41 } else { 0x40 };
    if c1 > 0x9F {
        (c1 - 0xC1) * 188 + c2 - sub
    } else {
        (c1 - 0x81) * 188 + c2 - sub
    }
}

/// `cp932ext3_cp932ext2_jis`: JIS code (in the NEC-selected IBM extension
/// rows 89-92) for the `c`-th entry of `cp932ext3_ucs_table`.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
const fn cp932ext3_cp932ext2_jis(c: u32) -> u32 {
    let mut idx = sjistoidx(0xFA, 0x40) + c as i32;
    if idx >= sjistoidx(0xFA, 0x5C) {
        idx -= sjistoidx(0xFA, 0x5C) - sjistoidx(0xED, 0x40);
    } else if idx >= sjistoidx(0xFA, 0x55) {
        idx -= sjistoidx(0xFA, 0x55) - sjistoidx(0xEE, 0xFA);
    } else if idx >= sjistoidx(0xFA, 0x40) {
        idx -= sjistoidx(0xFA, 0x40) - sjistoidx(0xEE, 0xEF);
    }
    ((((idx / 94) + 0x21) << 8) | ((idx % 94) + 0x21)) as u32
}

/// `(((s1 / 94) + 0x21) << 8) + ((s1 % 94) + 0x21) - 0x1600`: KDDI emoji
/// `(ku*94)+ten` value to the ISO-2022-JP-KDDI two-byte code.
#[inline]
const fn kddi_emoji_jis(s1: u32) -> u32 {
    (((s1 / 94) + 0x21) << 8) + ((s1 % 94) + 0x21) - 0x1600
}

/// KDDI emoji lookup by Unicode code point (the copyright/registered signs
/// and the `mb_tbl_uni_kddi2code{2,3,5}` tables); returns the `(ku*94)+ten`
/// value.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
fn kddi_emoji_lookup(w: u32) -> Option<u32> {
    let w_i = w as i32;
    let in_range = |min: i32, max: i32| w_i >= min && w_i <= max && w <= 0x7FFF_FFFF;
    if w == 0xA9 {
        Some(0x27DC)
    } else if w == 0xAE {
        Some(0x27DD)
    } else if in_range(em::MB_TBL_UNI_KDDI2CODE2_MIN, em::MB_TBL_UNI_KDDI2CODE2_MAX) {
        let i = bisec_srch2(w_i, &em::MB_TBL_UNI_KDDI2CODE2_KEY);
        (i >= 0).then(|| u32::from(em::MB_TBL_UNI_KDDI2CODE2_VALUE[i as usize]))
    } else if in_range(em::MB_TBL_UNI_KDDI2CODE3_MIN, em::MB_TBL_UNI_KDDI2CODE3_MAX) {
        let i = bisec_srch2(w_i - 0x10000, &em::MB_TBL_UNI_KDDI2CODE3_KEY);
        (i >= 0).then(|| u32::from(em::MB_TBL_UNI_KDDI2CODE3_VALUE[i as usize]))
    } else if in_range(em::MB_TBL_UNI_KDDI2CODE5_MIN, em::MB_TBL_UNI_KDDI2CODE5_MAX) {
        let i = bisec_srch2(w_i - 0xF0000, &em::MB_TBL_UNI_KDDI2CODE5_KEY);
        (i >= 0).then(|| u32::from(em::MB_TBL_UNI_KDDI2CODE5_VAL[i as usize]))
    } else {
        None
    }
}

/* ------------------------------------------------------------------------ */
/* Legacy filters: JIS / ISO-2022-JP                                          */
/* ------------------------------------------------------------------------ */

/// `mbfl_filt_conv_jis_wchar`: JIS (and ISO-2022-JP) bytes to code points.
///
/// `status` low nibble: 0 = normal, 1 = second byte of a kanji, 2 = after
/// ESC, 3 = after `ESC $`, 4 = after `ESC $ (`, 5 = after `ESC (`. High
/// part: 0x00 ASCII, 0x10 JIS X 0201 Roman, 0x20 JIS X 0201 kana,
/// 0x80 JIS X 0208, 0x90 JIS X 0212.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
pub fn filt_jis_wchar(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    loop {
        match filter.status & 0xF {
            0 => {
                if c == 0x1B {
                    filter.status += 2;
                } else if c == 0x0E {
                    filter.status = 0x20; /* "kana in" */
                } else if c == 0x0F {
                    filter.status = 0; /* "kana out" */
                } else if filter.status == 0x10 && c == 0x5C {
                    out.put(0xA5); /* YEN SIGN */
                } else if filter.status == 0x10 && c == 0x7E {
                    out.put(0x203E); /* OVER LINE */
                } else if filter.status == 0x20 && c > 0x20 && c < 0x60 {
                    out.put(0xFF40 + c); /* kana */
                } else if (filter.status == 0x80 || filter.status == 0x90) && c > 0x20 && c < 0x7F {
                    filter.cache = c; /* kanji first char */
                    filter.status += 1;
                } else if (0..0x80).contains(&c) {
                    out.put(c); /* latin, CTLs */
                } else if c > 0xA0 && c < 0xE0 {
                    out.put(0xFEC0 + c); /* GR kana */
                } else {
                    out.put(BAD_INPUT_I32);
                }
            }
            1 => {
                filter.status &= !0xF;
                let c1 = filter.cache;
                if c > 0x20 && c < 0x7F {
                    let s = (c1 - 0x21) * 94 + c - 0x21;
                    let w = if s < 0 {
                        0
                    } else if filter.status == 0x80 {
                        jisx0208(s as u32)
                    } else {
                        jisx0212(s as u32)
                    };
                    out.put(if w == 0 { BAD_INPUT_I32 } else { w as i32 });
                } else {
                    out.put(BAD_INPUT_I32);
                }
            }
            2 => {
                /* ESC */
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
            3 => {
                /* ESC $ */
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
            4 => {
                /* ESC $ ( */
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
            5 => {
                /* ESC ( */
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
            _ => {}
        }
        break;
    }
}

/// `mbfl_filt_conv_jis_wchar_flush`.
pub fn filt_jis_wchar_flush(filter: &mut Filter, out: &mut dyn Sink) {
    if filter.status & 0xF != 0 {
        /* 2-byte character or escape sequence was truncated */
        out.put(BAD_INPUT_I32);
    }
    filter.status = 0;
    out.flush();
}

/// Emit `s` (the result of a JIS table lookup) as JIS, switching character
/// set if necessary (shared tail of `mbfl_filt_conv_wchar_jis`).
#[allow(clippy::cast_possible_truncation)]
fn emit_jis_legacy(s: i32, filter: &mut Filter, out: &mut dyn Sink) {
    if s < 0x80 {
        /* ASCII */
        if filter.status & 0xFF00 != 0 {
            out.put(0x1B);
            out.put(0x28);
            out.put(0x42);
        }
        filter.status = 0;
        out.put(s);
    } else if s < 0x8080 {
        /* X 0208 */
        if filter.status & 0xFF00 != 0x200 {
            out.put(0x1B);
            out.put(0x24);
            out.put(0x42);
        }
        filter.status = 0x200;
        out.put((s >> 8) & 0x7F);
        out.put(s & 0x7F);
    } else if s < 0x10000 {
        /* X 0212 */
        if filter.status & 0xFF00 != 0x300 {
            out.put(0x1B);
            out.put(0x24);
            out.put(0x28);
            out.put(0x44);
        }
        filter.status = 0x300;
        out.put((s >> 8) & 0x7F);
        out.put(s & 0x7F);
    } else {
        /* X 0201 latin */
        if filter.status & 0xFF00 != 0x400 {
            out.put(0x1B);
            out.put(0x28);
            out.put(0x4A);
        }
        filter.status = 0x400;
        out.put(s & 0x7F);
    }
}

/// `mbfl_filt_conv_wchar_jis`: code points to JIS.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
pub fn filt_wchar_jis(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    let mut s = if c == 0x203E {
        0x1007E /* OVERLINE -> JIS X 0201 OVERLINE */
    } else {
        ucs_to_jis_i(c)
    };
    if s <= 0 {
        if c >= 0 {
            s = jis_fallback(c as u32) as i32;
        }
        if c == 0 {
            s = 0;
        } else if s <= 0 {
            s = -1;
        }
    }
    if s >= 0 {
        emit_jis_legacy(s, filter, out);
    } else {
        legacy::illegal_output(c, filter, out);
    }
}

/// `mbfl_filt_conv_wchar_2022jp`: code points to ISO-2022-JP.
///
/// Note: exactly like the C code, unconvertible characters are silently
/// dropped (no illegal-character substitution).
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
pub fn filt_wchar_2022jp(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    let mut s = ucs_to_jis_i(c);
    if s <= 0 {
        if c >= 0 {
            s = jis_fallback(c as u32) as i32;
        }
        if c == 0 {
            s = 0;
        } else if s <= 0 {
            s = -1;
        }
    } else if (0x80..0x2121).contains(&s) || s > 0x8080 {
        s = -1;
    }
    if s >= 0 {
        if s < 0x80 {
            if filter.status & 0xFF00 != 0 {
                out.put(0x1B);
                out.put(0x28);
                out.put(0x42);
            }
            filter.status = 0;
            out.put(s);
        } else if s < 0x10000 {
            if filter.status & 0xFF00 != 0x200 {
                out.put(0x1B);
                out.put(0x24);
                out.put(0x42);
            }
            filter.status = 0x200;
            out.put((s >> 8) & 0x7F);
            out.put(s & 0x7F);
        } else {
            if filter.status & 0xFF00 != 0x400 {
                out.put(0x1B);
                out.put(0x28);
                out.put(0x4A);
            }
            filter.status = 0x400;
            out.put(s & 0x7F);
        }
    }
}

/// `mbfl_filt_conv_any_jis_flush`: return to ASCII at the end.
pub fn filt_any_jis_flush(filter: &mut Filter, out: &mut dyn Sink) {
    if filter.status & 0xFF00 != 0 {
        out.put(0x1B);
        out.put(0x28);
        out.put(0x42);
    }
    filter.status = 0;
    out.flush();
}

/* ------------------------------------------------------------------------ */
/* Fast codecs: JIS / ISO-2022-JP                                             */
/* ------------------------------------------------------------------------ */

const ASCII: u32 = 0;
const JISX_0201_LATIN: u32 = 1;
const JISX_0201_KANA: u32 = 2;
const JISX_0208: u32 = 3;
const JISX_0212: u32 = 4;
const JISX_0201_KANA_SO: u32 = 5;

/// `mb_iso2022jp_to_wchar` (used for both `JIS` and `ISO-2022-JP`).
#[allow(clippy::many_single_char_names, clippy::too_many_lines)]
pub fn iso2022jp_to_wchar(input: &mut &[u8], buf: &mut [u32], state: &mut u32) -> usize {
    let s = *input;
    let e = s.len();
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len();

    macro_rules! emit {
        ($v:expr) => {{
            buf[out] = $v;
            out += 1;
        }};
    }

    while p < e && out < limit {
        let c = s[p];
        p += 1;

        if c == 0x1B {
            /* ESC seen; this is an escape sequence */
            if e - p < 2 {
                emit!(BAD_INPUT);
                if p != e && (s[p] == b'$' || s[p] == b'(') {
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
                        emit!(BAD_INPUT);
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
                        emit!(BAD_INPUT);
                        emit!(u32::from(b'$'));
                        emit!(u32::from(b'('));
                        p -= 1;
                    }
                } else {
                    if limit - out < 2 {
                        p -= 3;
                        break;
                    }
                    emit!(BAD_INPUT);
                    emit!(u32::from(b'$'));
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
                    emit!(BAD_INPUT);
                    emit!(u32::from(b'('));
                    p -= 1;
                }
            } else {
                emit!(BAD_INPUT);
                p -= 1;
            }
        } else if c == 0xE {
            /* "Kana In" marker (JIS7, also accepted for ISO-2022-JP) */
            *state = JISX_0201_KANA;
        } else if c == 0xF {
            /* "Kana Out" marker */
            *state = ASCII;
        } else if *state == JISX_0201_LATIN && c == 0x5C {
            emit!(0xA5); /* YEN SIGN */
        } else if *state == JISX_0201_LATIN && c == 0x7E {
            emit!(0x203E); /* OVER LINE */
        } else if *state == JISX_0201_KANA && c > 0x20 && c < 0x60 {
            emit!(0xFF40 + u32::from(c));
        } else if *state >= JISX_0208 && c > 0x20 && c < 0x7F {
            if p == e {
                emit!(BAD_INPUT);
                break;
            }
            let c2 = s[p];
            p += 1;
            if c2 > 0x20 && c2 < 0x7F {
                let sidx = (u32::from(c) - 0x21) * 94 + u32::from(c2) - 0x21;
                let w = if *state == JISX_0208 {
                    jisx0208(sidx)
                } else {
                    jisx0212(sidx)
                };
                emit!(if w == 0 { BAD_INPUT } else { w });
            } else {
                emit!(BAD_INPUT);
            }
        } else if c < 0x80 {
            emit!(u32::from(c));
        } else if (0xA1..=0xDF).contains(&c) {
            /* GR-invoked Kana (JIS8) */
            emit!(0xFEC0 + u32::from(c));
        } else {
            emit!(BAD_INPUT);
        }
    }

    *input = &s[p..];
    out
}

/// Emit a JIS code (`s` from the tables, already validated) for the fast
/// ISO-2022-JP/JIS encoders, switching character set as needed.
#[allow(clippy::cast_possible_truncation)]
fn emit_jis_fast(s: u32, buf: &mut ConvertBuf, kana: bool) {
    if s < 0x80 {
        if buf.state != ASCII {
            buf.extend(&[0x1B, b'(', b'B']);
            buf.state = ASCII;
        }
        buf.push(s as u8);
    } else if kana && (0xA1..=0xDF).contains(&s) {
        if buf.state != JISX_0201_KANA {
            buf.extend(&[0x1B, b'(', b'I']);
            buf.state = JISX_0201_KANA;
        }
        buf.push((s & 0x7F) as u8);
    } else if s < 0x8080 {
        if buf.state != JISX_0208 {
            buf.extend(&[0x1B, b'$', b'B']);
            buf.state = JISX_0208;
        }
        buf.extend(&[((s >> 8) & 0x7F) as u8, (s & 0x7F) as u8]);
    } else if s < 0x10000 {
        if buf.state != JISX_0212 {
            buf.extend(&[0x1B, b'$', b'(', b'D']);
            buf.state = JISX_0212;
        }
        buf.extend(&[((s >> 8) & 0x7F) as u8, (s & 0x7F) as u8]);
    } else {
        if buf.state != JISX_0201_LATIN {
            buf.extend(&[0x1B, b'(', b'J']);
            buf.state = JISX_0201_LATIN;
        }
        buf.push((s & 0x7F) as u8);
    }
}

/// `mb_wchar_to_iso2022jp`.
pub fn wchar_to_iso2022jp(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    for &w in input {
        let mut s = ucs_to_jis(w);
        if s == 0 {
            s = jis_fallback(w);
            if s == 0 && w != 0 {
                illegal_output(w, wchar_to_iso2022jp, buf);
                continue;
            }
        } else if (0x80..0x2121).contains(&s) || s > 0x8080 {
            illegal_output(w, wchar_to_iso2022jp, buf);
            continue;
        }
        emit_jis_fast(s, buf, false);
    }

    if end && buf.state != ASCII {
        buf.extend(&[0x1B, b'(', b'B']);
    }
}

/// `mb_wchar_to_jis`.
///
/// Like the C code, errors are reported through `mb_wchar_to_iso2022jp`
/// (which shares the same state values).
pub fn wchar_to_jis(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    for &w in input {
        let mut s = if w == 0x203E {
            0x1007E /* OVERLINE -> JIS X 0201 OVERLINE */
        } else {
            ucs_to_jis(w)
        };
        if s == 0 {
            s = jis_fallback(w);
            if s == 0 && w != 0 {
                illegal_output(w, wchar_to_iso2022jp, buf);
                continue;
            }
        }
        emit_jis_fast(s, buf, true);
    }

    if end && buf.state != ASCII {
        buf.extend(&[0x1B, b'(', b'B']);
    }
}

/// `mb_check_jis`.
#[allow(clippy::many_single_char_names, clippy::too_many_lines)]
pub fn check_jis(input: &[u8]) -> bool {
    let e = input.len();
    let mut p = 0usize;
    let mut state = ASCII;

    while p < e {
        let c = input[p];
        p += 1;
        if c == 0x1B {
            if state == JISX_0201_KANA_SO || e - p < 2 {
                return false;
            }
            let c2 = input[p];
            p += 1;
            if c2 == b'$' {
                let c3 = input[p];
                p += 1;
                if c3 == b'@' || c3 == b'B' {
                    state = JISX_0208;
                } else if c3 == b'(' {
                    if p == e {
                        return false;
                    }
                    let c4 = input[p];
                    p += 1;
                    if c4 == b'@' || c4 == b'B' {
                        state = JISX_0208;
                    } else if c4 == b'D' {
                        state = JISX_0212;
                    } else {
                        return false;
                    }
                } else {
                    return false;
                }
            } else if c2 == b'(' {
                let c3 = input[p];
                p += 1;
                /* ESC ( H is treated as switching to ASCII for historical reasons */
                if c3 == b'B' || c3 == b'H' {
                    state = ASCII;
                } else if c3 == b'J' {
                    state = JISX_0201_LATIN;
                } else if c3 == b'I' {
                    state = JISX_0201_KANA;
                } else {
                    return false;
                }
            } else {
                return false;
            }
        } else if c == 0xE {
            if state != ASCII {
                return false;
            }
            state = JISX_0201_KANA_SO;
        } else if c == 0xF {
            if state != JISX_0201_KANA_SO {
                return false;
            }
            state = ASCII;
        } else if (state == JISX_0208 || state == JISX_0212) && c > 0x20 && c < 0x7F {
            if p == e {
                return false;
            }
            let c2 = input[p];
            p += 1;
            if c2 > 0x20 && c2 < 0x7F {
                let s = (u32::from(c) - 0x21) * 94 + u32::from(c2) - 0x21;
                let w = if state == JISX_0208 {
                    jisx0208(s)
                } else {
                    jisx0212(s)
                };
                if w != 0 {
                    continue;
                }
            }
            return false;
        } else if !(c < 0x80 || (0xA1..=0xDF).contains(&c)) {
            return false;
        }
    }

    state == ASCII
}

/// `mb_check_iso2022jp`.
#[allow(clippy::many_single_char_names, clippy::too_many_lines)]
pub fn check_iso2022jp(input: &[u8]) -> bool {
    let e = input.len();
    let mut p = 0usize;
    let mut state = ASCII;

    while p < e {
        let c = input[p];
        p += 1;
        if c == 0x1B {
            if e - p < 2 {
                return false;
            }
            let c2 = input[p];
            let c3 = input[p + 1];
            p += 2;
            if c2 == b'$' {
                if c3 == b'@' || c3 == b'B' {
                    state = JISX_0208;
                } else {
                    return false;
                }
            } else if c2 == b'(' {
                if c3 == b'B' {
                    state = ASCII;
                } else if c3 == b'J' {
                    state = JISX_0201_LATIN;
                } else {
                    return false;
                }
            } else {
                return false;
            }
        } else if c == 0xE || c == 0xF {
            /* Kana In/Out is not accepted in ISO-2022-JP */
            return false;
        } else if state == JISX_0208 && c > 0x20 && c < 0x7F {
            if p == e {
                return false;
            }
            let c2 = input[p];
            p += 1;
            if c2 > 0x20 && c2 < 0x7F {
                let s = (u32::from(c) - 0x21) * 94 + u32::from(c2) - 0x21;
                if jisx0208(s) != 0 {
                    continue;
                }
            }
            return false;
        } else if c >= 0x80 {
            return false;
        }
    }

    state == ASCII
}

/* ------------------------------------------------------------------------ */
/* ISO-2022-JP-MOBILE#KDDI                                                    */
/* ------------------------------------------------------------------------ */

const KANA_0201: u32 = 0x20; /* JISX0201_KANA */
const KANJI_0208: u32 = 0x80; /* JISX0208_KANJI */
const UDC: u32 = 0xA0;

/// Decode a JIS X 0208 `(ku*94)+ten` index the KDDI way (with emoji in rows
/// 84-90); returns `(w, snd)` with 0 for "nothing".
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
fn kddi_decode(mut s: u32) -> (u32, u32) {
    let mut w = if s <= 137 { ms_row1_special(s) } else { 0 };
    let mut snd = 0i32;
    if (84 * 94..91 * 94).contains(&s) {
        s += 22 * 94;
        w = sjis_emoji_kddi2unicode(s as i32, &mut snd) as u32;
    }
    if w == 0 {
        if let Some(v) = lookup(s, &ext::CP932EXT1_UCS_TABLE, ext::CP932EXT1_UCS_TABLE_MIN) {
            w = v;
        } else {
            w = jisx0208(s);
        }
    }
    (w, if w != 0 { snd as u32 } else { 0 })
}

/// `mbfl_filt_conv_2022jp_mobile_wchar`: ISO-2022-JP-KDDI bytes to code
/// points.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
pub fn filt_2022jp_mobile_wchar(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    match filter.status & 0xF {
        0 => {
            if c == 0x1B {
                filter.status += 2;
            } else if filter.status == 0x20 && c > 0x20 && c < 0x60 {
                out.put(0xFF40 + c);
            } else if filter.status == 0x80 && c > 0x20 && c < 0x80 {
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
        1 => {
            /* JIS X 0208, second byte */
            filter.status &= !0xF;
            let c1 = filter.cache;
            if c > 0x20 && c < 0x7F {
                let s = (c1 - 0x21) * 94 + c - 0x21;
                let (w, snd) = if s < 0 { (0, 0) } else { kddi_decode(s as u32) };
                if snd > 0 {
                    out.put(snd as i32);
                }
                out.put(if w == 0 { BAD_INPUT_I32 } else { w as i32 });
            } else {
                out.put(BAD_INPUT_I32);
            }
        }
        2 => {
            /* ESC */
            if c == i32::from(b'$') {
                filter.status += 1;
            } else if c == i32::from(b'(') {
                filter.status += 3;
            } else {
                filter.status &= !0xF;
                out.put(BAD_INPUT_I32);
            }
        }
        3 => {
            /* ESC $ */
            if c == i32::from(b'@') || c == i32::from(b'B') {
                filter.status = 0x80;
            } else if c == i32::from(b'(') {
                filter.status += 1;
            } else {
                filter.status &= !0xF;
                out.put(BAD_INPUT_I32);
            }
        }
        4 => {
            /* ESC $ ( */
            if c == i32::from(b'@') || c == i32::from(b'B') {
                filter.status = 0x80;
            } else {
                filter.status &= !0xF;
                out.put(BAD_INPUT_I32);
            }
        }
        5 => {
            /* ESC ( */
            if c == i32::from(b'B') || c == i32::from(b'J') {
                filter.status = 0;
            } else if c == i32::from(b'I') {
                filter.status = 0x20;
            } else {
                filter.status &= !0xF;
                out.put(BAD_INPUT_I32);
            }
        }
        _ => {}
    }
}

/// `mbfl_filt_conv_2022jp_mobile_wchar_flush`.
pub fn filt_2022jp_mobile_wchar_flush(filter: &mut Filter, out: &mut dyn Sink) {
    if filter.status & 0xF != 0 {
        out.put(BAD_INPUT_I32);
    }
    filter.status = 0;
    out.flush();
}

/// `mbfilter_unicode2sjis_emoji_kddi` (filter-based variant): returns true
/// and sets `*s1` to the `(ku*94)+ten` code if `c` is (the end of) a KDDI
/// emoji. Keypad digits are buffered in `filter.cache`.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
fn unicode2sjis_emoji_kddi(c: i32, s1: &mut i32, filter: &mut Filter, out: &mut dyn Sink) -> bool {
    if filter.status & 0xF == 1 {
        let c1 = filter.cache;
        filter.cache = 0;
        filter.status &= !0xFF;
        if c == 0x20E3 {
            *s1 = if c1 == i32::from(b'#') {
                0x25BC
            } else if c1 == i32::from(b'0') {
                0x2830
            } else {
                0x27A6 + (c1 - i32::from(b'1'))
            };
            return true;
        }
        if filter.status & 0xFF00 != 0 {
            out.put(0x1B);
            out.put(i32::from(b'('));
            out.put(i32::from(b'B'));
        }
        out.put(c1);
        filter.status = 0;
    }

    if c == i32::from(b'#') || (i32::from(b'0')..=i32::from(b'9')).contains(&c) {
        filter.status |= 1;
        filter.cache = c;
        return false;
    }

    if c < 0 {
        return false;
    }
    match kddi_emoji_lookup(c as u32) {
        Some(v) => {
            *s1 = v as i32;
            true
        }
        None => false,
    }
}

/// `mbfl_filt_conv_wchar_2022jp_mobile`: code points to ISO-2022-JP-KDDI.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
pub fn filt_wchar_2022jp_mobile(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    let mut s1 = ucs_to_jis_i(c);
    let mut s2 = 0;

    if s1 <= 0 && c >= 0 {
        let f = ms_fallback(c as u32) as i32;
        if f != 0 {
            s1 = f;
        }
    }

    if unicode2sjis_emoji_kddi(c, &mut s1, filter, out) {
        /* CODE2JIS */
        let c1 = s1 / 94 + 0x21;
        let c2 = s1 - 94 * (c1 - 0x21) + 0x21;
        s1 = (c1 << 8) | c2;
        s2 = 1;
        s1 -= 0x1600;
    } else if filter.status & 0xFF == 1 && filter.cache != 0 {
        /* Processing one of KDDI's special emoji for a phone keypad button */
        return;
    }

    if s1 <= 0 || (s1 >= 0xA1A1 && s2 == 0) {
        /* not found or X 0212 */
        s1 = -1;
        if c >= 0 {
            if let Some(i) = cp932ext1_index(c as u32) {
                s1 = cp932ext1_jis(i) as i32;
            }
        }
        if c == 0 {
            s1 = 0;
        }
    }

    if s1 >= 0 {
        if s1 < 0x80 {
            if filter.status & 0xFF00 != 0 {
                out.put(0x1B);
                out.put(i32::from(b'('));
                out.put(i32::from(b'B'));
            }
            out.put(s1);
            filter.status = 0;
        } else if s1 > 0xA0 && s1 < 0xE0 {
            if filter.status & 0xFF00 != 0x100 {
                out.put(0x1B);
                out.put(i32::from(b'('));
                out.put(i32::from(b'I'));
            }
            filter.status = 0x100;
            out.put(s1 & 0x7F);
        } else if s1 < 0x7E7F {
            if filter.status & 0xFF00 != 0x200 {
                out.put(0x1B);
                out.put(i32::from(b'$'));
                out.put(i32::from(b'B'));
            }
            filter.status = 0x200;
            out.put((s1 >> 8) & 0xFF);
            out.put(s1 & 0x7F);
        }
    } else {
        legacy::illegal_output(c, filter, out);
    }
}

/// `mbfl_filt_conv_wchar_2022jp_mobile_flush`.
pub fn filt_wchar_2022jp_mobile_flush(filter: &mut Filter, out: &mut dyn Sink) {
    /* Go back to ASCII mode (so strings can be safely concatenated) */
    if filter.status & 0xFF00 != 0 {
        out.put(0x1B);
        out.put(i32::from(b'('));
        out.put(i32::from(b'B'));
    }
    let c1 = filter.cache;
    if filter.status & 0xFF == 1
        && (c1 == i32::from(b'#') || (i32::from(b'0')..=i32::from(b'9')).contains(&c1))
    {
        out.put(c1);
    }
    filter.status = 0;
    filter.cache = 0;
    out.flush();
}

/// `mb_iso2022jp_kddi_to_wchar`.
#[allow(clippy::many_single_char_names, clippy::too_many_lines)]
pub fn iso2022jp_kddi_to_wchar(input: &mut &[u8], buf: &mut [u32], state: &mut u32) -> usize {
    let s = *input;
    let e = s.len();
    let mut p = 0usize;
    let mut out = 0usize;
    /* Leave room for a 2-code point emoji */
    let limit = buf.len().saturating_sub(1);

    macro_rules! emit {
        ($v:expr) => {{
            buf[out] = $v;
            out += 1;
        }};
    }

    while p < e && out < limit {
        let c = s[p];
        p += 1;

        if c == 0x1B {
            if e - p < 2 {
                p = e;
                emit!(BAD_INPUT);
                break;
            }
            let c2 = s[p];
            let c3 = s[p + 1];
            p += 2;

            if c2 == b'$' {
                if c3 == b'@' || c3 == b'B' {
                    *state = KANJI_0208;
                } else if c3 == b'(' {
                    if p == e {
                        emit!(BAD_INPUT);
                        break;
                    }
                    let c4 = s[p];
                    p += 1;
                    if c4 == b'@' || c4 == b'B' {
                        *state = KANJI_0208;
                    } else {
                        emit!(BAD_INPUT);
                    }
                } else {
                    emit!(BAD_INPUT);
                }
            } else if c2 == b'(' {
                if c3 == b'B' || c3 == b'J' {
                    *state = ASCII;
                } else if c3 == b'I' {
                    *state = KANA_0201;
                } else {
                    emit!(BAD_INPUT);
                }
            } else {
                p -= 1;
                emit!(BAD_INPUT);
            }
        } else if *state == KANA_0201 && (0x21..=0x5F).contains(&c) {
            emit!(0xFF40 + u32::from(c));
        } else if *state == KANJI_0208 && (0x21..=0x7F).contains(&c) {
            if p == e {
                emit!(BAD_INPUT);
                break;
            }
            let c2 = s[p];
            p += 1;
            if (0x21..=0x7E).contains(&c2) {
                let sidx = (u32::from(c) - 0x21) * 94 + u32::from(c2) - 0x21;
                let (w, snd) = kddi_decode(sidx);
                if snd != 0 {
                    emit!(snd);
                }
                emit!(if w == 0 { BAD_INPUT } else { w });
            } else {
                emit!(BAD_INPUT);
            }
        } else if c <= 0x7F {
            emit!(u32::from(c));
        } else if (0xA1..=0xDF).contains(&c) {
            emit!(0xFEC0 + u32::from(c));
        } else {
            emit!(BAD_INPUT);
        }
    }

    *input = &s[p..];
    out
}

/// `mb_wchar_to_iso2022jp_kddi`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn wchar_to_iso2022jp_kddi(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    let mut i = 0usize;
    let n = input.len();
    while i < n {
        let w = input[i];
        i += 1;
        let mut s = ucs_to_jis(w);
        if s == 0 {
            s = ms_fallback(w);
        }

        if (w == u32::from(b'#') || (u32::from(b'0')..=u32::from(b'9')).contains(&w)) && i < n {
            let w2 = input[i];
            i += 1;
            if w2 == 0x20E3 {
                let s1 = if w == u32::from(b'#') {
                    0x25BC
                } else if w == u32::from(b'0') {
                    0x2830
                } else {
                    0x27A6 + (w - u32::from(b'1'))
                };
                s = kddi_emoji_jis(s1);
            } else {
                i -= 1;
            }
        } else if w >= nflags(b'C') as u32 && w <= nflags(b'U') as u32 && i < n {
            /* C for CN, U for US */
            let w2 = input[i];
            i += 1;
            let mut found = false;
            if w2 >= nflags(b'B') as u32 && w2 <= nflags(b'U') as u32 {
                /* B for GB, U for RU */
                for (k, cc) in NFLAGS_S.iter().enumerate() {
                    if w == nflags(cc[0]) as u32 && w2 == nflags(cc[1]) as u32 {
                        s = kddi_emoji_jis(NFLAGS_CODE_KDDI[k] as u32);
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                i -= 1;
            }
        }

        if let Some(s1) = kddi_emoji_lookup(w) {
            s = kddi_emoji_jis(s1);
        }

        if s == 0 || s >= 0xA1A1 {
            s = cp932ext1_index(w).map_or(0, cp932ext1_jis);
            if w == 0 {
                s = 0;
            }
        }

        if s == 0 && w != 0 {
            illegal_output(w, wchar_to_iso2022jp_kddi, buf);
        } else if s <= 0x7F {
            if buf.state != ASCII {
                buf.extend(&[0x1B, b'(', b'B']);
                buf.state = ASCII;
            }
            buf.push(s as u8);
        } else if (0xA1..=0xDF).contains(&s) {
            if buf.state != KANA_0201 {
                buf.extend(&[0x1B, b'(', b'I']);
                buf.state = KANA_0201;
            }
            buf.push((s & 0x7F) as u8);
        } else if s <= 0x7E7E {
            if buf.state != KANJI_0208 {
                buf.extend(&[0x1B, b'$', b'B']);
                buf.state = KANJI_0208;
            }
            buf.extend(&[((s >> 8) & 0xFF) as u8, (s & 0xFF) as u8]);
        } else {
            illegal_output(w, wchar_to_iso2022jp_kddi, buf);
        }
    }

    if end && buf.state != ASCII {
        buf.extend(&[0x1B, b'(', b'B']);
    }
}

/* ------------------------------------------------------------------------ */
/* ISO-2022-JP-MS                                                             */
/* ------------------------------------------------------------------------ */

/// Decode a JIS X 0208 `(ku*94)+ten` index the Microsoft way (with the NEC
/// row 13 and IBM rows 89-92 extensions); 0 if unmapped.
fn ms_decode(s: u32) -> u32 {
    let w = if s <= 137 { ms_row1_special(s) } else { 0 };
    if w != 0 {
        return w;
    }
    if let Some(v) = lookup(s, &ext::CP932EXT1_UCS_TABLE, ext::CP932EXT1_UCS_TABLE_MIN) {
        v
    } else if (s as usize) < t::JISX0208_UCS_TABLE.len() {
        jisx0208(s)
    } else {
        lookup(s, &ext::CP932EXT2_UCS_TABLE, ext::CP932EXT2_UCS_TABLE_MIN).unwrap_or(0)
    }
}

/// `mbfl_filt_conv_2022jpms_wchar`: ISO-2022-JP-MS bytes to code points.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
pub fn filt_2022jpms_wchar(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    match filter.status & 0xF {
        0 => {
            if c == 0x1B {
                filter.status += 2;
            } else if filter.status == 0x20 && c > 0x20 && c < 0x60 {
                out.put(0xFF40 + c);
            } else if (filter.status == 0x80 || filter.status == 0xA0) && c > 0x20 && c < 0x80 {
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
        1 => {
            /* Kanji, second byte */
            filter.status &= !0xF;
            let c1 = filter.cache;
            if c > 0x20 && c < 0x7F {
                let s = (c1 - 0x21) * 94 + c - 0x21;
                let w = if filter.status == 0x80 {
                    let w = if s < 0 { 0 } else { ms_decode(s as u32) };
                    if w == 0 {
                        BAD_INPUT_I32
                    } else {
                        w as i32
                    }
                } else if c1 > 0x20 && c1 < 0x35 {
                    0xE000 + (c1 - 0x21) * 94 + c - 0x21
                } else {
                    BAD_INPUT_I32
                };
                out.put(w);
            } else {
                out.put(BAD_INPUT_I32);
            }
        }
        2 => {
            /* ESC */
            if c == i32::from(b'$') {
                filter.status += 1;
            } else if c == i32::from(b'(') {
                filter.status += 3;
            } else {
                filter.status &= !0xF;
                out.put(BAD_INPUT_I32);
            }
        }
        3 => {
            /* ESC $ */
            if c == i32::from(b'@') || c == i32::from(b'B') {
                filter.status = 0x80;
            } else if c == i32::from(b'(') {
                filter.status += 1;
            } else {
                filter.status &= !0xF;
                out.put(BAD_INPUT_I32);
            }
        }
        4 => {
            /* ESC $ ( */
            if c == i32::from(b'@') || c == i32::from(b'B') {
                filter.status = 0x80;
            } else if c == i32::from(b'?') {
                filter.status = 0xA0;
            } else {
                filter.status &= !0xF;
                out.put(BAD_INPUT_I32);
            }
        }
        5 => {
            /* ESC ( */
            if c == i32::from(b'B') || c == i32::from(b'J') {
                filter.status = 0;
            } else if c == i32::from(b'I') {
                filter.status = 0x20;
            } else {
                filter.status &= !0xF;
                out.put(BAD_INPUT_I32);
            }
        }
        _ => {}
    }
}

/// `mbfl_filt_conv_2022jpms_wchar_flush`.
pub fn filt_2022jpms_wchar_flush(filter: &mut Filter, out: &mut dyn Sink) {
    if filter.status & 0xF != 0 {
        out.put(BAD_INPUT_I32);
    }
    filter.status = 0;
    out.flush();
}

/// Unicode to ISO-2022-JP-MS code (`0` = nothing found), shared by the
/// legacy and fast encoders.
fn ucs_to_jisms(w: u32) -> u32 {
    let mut s = ucs_to_jis(w);
    if s == 0 && (0xE000..0xE000 + 20 * 94).contains(&w) {
        /* Private User Area (95ku - 114ku) */
        s = ((((w - 0xE000) / 94) + 0x7F) << 8) | (((w - 0xE000) % 94) + 0x21);
    }
    if s == 0 {
        s = ms_fallback(w);
    }
    if s >= 0xA1A1 {
        /* JIS X 0212 */
        s = 0;
    }
    if s == 0 && w != 0 {
        if let Some(i) = cp932ext1_index(w) {
            s = cp932ext1_jis(i);
        } else if let Some(i) = cp932ext3_index(w) {
            s = cp932ext3_cp932ext2_jis(i);
        }
    }
    s
}

/// `mbfl_filt_conv_wchar_2022jpms`: code points to ISO-2022-JP-MS.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
pub fn filt_wchar_2022jpms(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    let s1: i32 = if c < 0 {
        -1
    } else {
        match ucs_to_jisms(c as u32) {
            0 if c != 0 => -1,
            s => s as i32,
        }
    };

    if s1 >= 0 {
        if s1 < 0x80 {
            if filter.status & 0xFF00 != 0 {
                out.put(0x1B);
                out.put(i32::from(b'('));
                out.put(i32::from(b'B'));
            }
            out.put(s1);
            filter.status = 0;
        } else if s1 > 0xA0 && s1 < 0xE0 {
            if filter.status & 0xFF00 != 0x100 {
                out.put(0x1B);
                out.put(i32::from(b'('));
                out.put(i32::from(b'I'));
            }
            filter.status = 0x100;
            out.put(s1 & 0x7F);
        } else if s1 < 0x7E7F {
            if filter.status & 0xFF00 != 0x200 {
                out.put(0x1B);
                out.put(i32::from(b'$'));
                out.put(i32::from(b'B'));
            }
            filter.status = 0x200;
            out.put((s1 >> 8) & 0xFF);
            out.put(s1 & 0x7F);
        } else if s1 < 0x927F {
            /* UDC */
            if filter.status & 0xFF00 != 0x800 {
                out.put(0x1B);
                out.put(i32::from(b'$'));
                out.put(i32::from(b'('));
                out.put(i32::from(b'?'));
            }
            filter.status = 0x800;
            out.put(((s1 >> 8) - 0x5E) & 0x7F);
            out.put(s1 & 0x7F);
        }
    } else {
        legacy::illegal_output(c, filter, out);
    }
}

/// `mbfl_filt_conv_any_2022jpms_flush`.
pub fn filt_any_2022jpms_flush(filter: &mut Filter, out: &mut dyn Sink) {
    /* Go back to ASCII (so strings can be safely concatenated) */
    if filter.status & 0xFF00 != 0 {
        out.put(0x1B);
        out.put(i32::from(b'('));
        out.put(i32::from(b'B'));
    }
    filter.status = 0;
    out.flush();
}

/// `mb_iso2022jpms_to_wchar`.
#[allow(clippy::many_single_char_names, clippy::too_many_lines)]
pub fn iso2022jpms_to_wchar(input: &mut &[u8], buf: &mut [u32], state: &mut u32) -> usize {
    let s = *input;
    let e = s.len();
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len();

    macro_rules! emit {
        ($v:expr) => {{
            buf[out] = $v;
            out += 1;
        }};
    }

    while p < e && out < limit {
        let c = s[p];
        p += 1;

        if c == 0x1B {
            if e - p < 2 {
                emit!(BAD_INPUT);
                p = e;
                break;
            }
            let c2 = s[p];
            let c3 = s[p + 1];
            p += 2;

            if c2 == b'$' {
                if c3 == b'@' || c3 == b'B' {
                    *state = KANJI_0208;
                } else if c3 == b'(' && p < e {
                    let c4 = s[p];
                    p += 1;
                    if c4 == b'@' || c4 == b'B' {
                        *state = KANJI_0208;
                    } else if c4 == b'?' {
                        *state = UDC;
                    } else {
                        emit!(BAD_INPUT);
                    }
                } else {
                    emit!(BAD_INPUT);
                }
            } else if c2 == b'(' {
                if c3 == b'B' || c3 == b'J' {
                    *state = ASCII;
                } else if c3 == b'I' {
                    *state = KANA_0201;
                } else {
                    emit!(BAD_INPUT);
                }
            } else {
                p -= 1;
                emit!(BAD_INPUT);
            }
        } else if *state == KANA_0201 && (0x21..=0x5F).contains(&c) {
            emit!(0xFF40 + u32::from(c));
        } else if (*state == KANJI_0208 || *state == UDC) && (0x21..=0x7F).contains(&c) {
            if p == e {
                emit!(BAD_INPUT);
                break;
            }
            let c2 = s[p];
            p += 1;
            let mut w = 0;
            if (0x21..=0x7E).contains(&c2) {
                let sidx = (u32::from(c) - 0x21) * 94 + u32::from(c2) - 0x21;
                if *state == KANJI_0208 {
                    w = ms_decode(sidx);
                } else if (0x21..=0x34).contains(&c) {
                    w = 0xE000 + sidx;
                }
                emit!(if w == 0 { BAD_INPUT } else { w });
            } else {
                emit!(BAD_INPUT);
            }
        } else if c <= 0x7F {
            emit!(u32::from(c));
        } else if (0xA1..=0xDF).contains(&c) {
            emit!(0xFEC0 + u32::from(c));
        } else {
            emit!(BAD_INPUT);
        }
    }

    *input = &s[p..];
    out
}

/// `mb_wchar_to_iso2022jpms`.
#[allow(clippy::cast_possible_truncation)]
pub fn wchar_to_iso2022jpms(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    for &w in input {
        let s = ucs_to_jisms(w);

        if s == 0 && w != 0 {
            illegal_output(w, wchar_to_iso2022jpms, buf);
        } else if s <= 0x7F {
            if buf.state != ASCII {
                buf.extend(&[0x1B, b'(', b'B']);
                buf.state = ASCII;
            }
            buf.push(s as u8);
        } else if (0xA1..=0xDF).contains(&s) {
            if buf.state != KANA_0201 {
                buf.extend(&[0x1B, b'(', b'I']);
                buf.state = KANA_0201;
            }
            buf.push((s & 0x7F) as u8);
        } else if s <= 0x7E7E {
            if buf.state != KANJI_0208 {
                buf.extend(&[0x1B, b'$', b'B']);
                buf.state = KANJI_0208;
            }
            buf.extend(&[((s >> 8) & 0xFF) as u8, (s & 0x7F) as u8]);
        } else if s < 0x927F {
            if buf.state != UDC {
                buf.extend(&[0x1B, b'$', b'(', b'?']);
                buf.state = UDC;
            }
            buf.extend(&[(((s >> 8) - 0x5E) & 0x7F) as u8, (s & 0x7F) as u8]);
        } else {
            illegal_output(w, wchar_to_iso2022jpms, buf);
        }
    }

    if end && buf.state != ASCII {
        buf.extend(&[0x1B, b'(', b'B']);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codecs::cjk::testutil::{
        data_file, decode, decode_bufsize, encode, encode_split, legacy_decode, legacy_encode,
        mapping_lines, replay_oracle, run_filter_with,
    };
    use crate::convert::ErrorMode;

    const ALL: [&Encoding; 4] = [&ENC_JIS, &ENC_2022JP, &ENC_2022JP_KDDI, &ENC_2022JPMS];

    fn u(s: &str) -> Vec<u32> {
        s.chars().map(u32::from).collect()
    }

    /// (JIS X 0208 code, Unicode) pairs from `JISX0208.txt`.
    fn jisx0208_map() -> Option<Vec<(u32, u32)>> {
        let text = data_file("JISX0208.txt")?;
        #[allow(clippy::cast_possible_truncation)]
        Some(
            mapping_lines(&text)
                .into_iter()
                .filter(|l| l.len() >= 3)
                .map(|l| (l[1][0] as u32, l[2][0] as u32))
                .collect(),
        )
    }

    #[test]
    fn oracle_fixture() {
        let n = replay_oracle("jis_oracle.tsv");
        assert!(n > 20_000);
    }

    #[test]
    fn jisx0208_both_ways() {
        let Some(map) = jisx0208_map() else { return };
        for (code, mut uni) in map {
            if code == 0x2140 {
                uni = 0xFF3C; /* mbstring decodes it as FULLWIDTH REVERSE SOLIDUS */
            }
            #[allow(clippy::cast_possible_truncation)]
            let bytes = [
                0x1B,
                b'$',
                b'B',
                (code >> 8) as u8,
                code as u8,
                0x1B,
                b'(',
                b'B',
            ];
            for e in [&ENC_JIS, &ENC_2022JP] {
                assert_eq!(decode(e, &bytes), vec![uni], "{} {code:04X}", e.name);
                assert_eq!(encode(e, &[uni]).0, bytes, "{} U+{uni:04X}", e.name);
                assert_eq!(legacy_decode(e, &bytes), vec![uni]);
                assert_eq!(legacy_encode(e, &[uni]), bytes);
            }
            for e in [&ENC_2022JP_KDDI, &ENC_2022JPMS] {
                let w = decode(e, &bytes);
                assert_eq!(w.len(), 1);
                assert_ne!(w[0], BAD_INPUT);
                assert_eq!(legacy_decode(e, &bytes), w);
                // Re-encoding gives a code which decodes to the same character
                let (re, errs) = encode(e, &w);
                assert_eq!(errs, 0);
                assert_eq!(decode(e, &re), w);
                assert_eq!(legacy_encode(e, &w), re, "{} U+{:04X}", e.name, w[0]);
            }
        }
    }

    #[test]
    fn jisx0212_only_in_jis() {
        let Some(text) = data_file("JISX0212.txt") else {
            return;
        };
        for l in mapping_lines(&text) {
            #[allow(clippy::cast_possible_truncation)]
            let (code, uni) = (l[0][0] as u32, l[1][0] as u32);
            #[allow(clippy::cast_possible_truncation)]
            let bytes = [
                0x1B,
                b'$',
                b'(',
                b'D',
                (code >> 8) as u8,
                code as u8,
                0x1B,
                b'(',
                b'B',
            ];
            assert_eq!(decode(&ENC_JIS, &bytes), vec![uni]);
            assert_eq!(legacy_decode(&ENC_JIS, &bytes), vec![uni]);
            let (out, errs) = encode(&ENC_JIS, &[uni]);
            // Some JIS X 0212 characters are encoded otherwise (e.g. U+00AF
            // goes to JIS X 0208 FULLWIDTH MACRON); PHP does not test that
            // direction either.
            assert_eq!(errs, 0, "U+{uni:04X}");
            if out.starts_with(b"\x1B$(D") {
                assert_eq!(out, bytes);
            }
            // ISO-2022-JP rejects JIS X 0212
            assert!(!check_iso2022jp(&bytes));
        }
    }

    #[test]
    fn jisx0201() {
        let Some(text) = data_file("JISX0201.txt") else {
            return;
        };
        for l in mapping_lines(&text) {
            #[allow(clippy::cast_possible_truncation)]
            let (code, uni) = (l[0][0] as u8, l[1][0] as u32);
            if code >= 0x80 {
                let k = code - 0x80;
                assert_eq!(
                    decode(&ENC_JIS, &[0x1B, b'(', b'I', k, 0x1B, b'(', b'B']),
                    vec![uni]
                );
                assert_eq!(decode(&ENC_JIS, &[0x0E, k, 0x0F]), vec![uni]);
                assert_eq!(decode(&ENC_JIS, &[code]), vec![uni]);
                assert_eq!(
                    encode(&ENC_JIS, &[uni]).0,
                    [0x1B, b'(', b'I', k, 0x1B, b'(', b'B']
                );
            } else {
                assert_eq!(
                    decode(&ENC_JIS, &[0x1B, b'(', b'J', code, 0x1B, b'(', b'B']),
                    vec![uni]
                );
            }
        }
    }

    #[test]
    fn error_markers_like_phpt() {
        // iso2022jp_encoding.phpt
        for e in [&ENC_JIS, &ENC_2022JP] {
            assert_eq!(decode(e, b"\x1B$"), vec![BAD_INPUT]);
            assert_eq!(decode(e, b"\x1B("), vec![BAD_INPUT]);
            assert_eq!(decode(e, b"\x1B,"), vec![BAD_INPUT, u32::from(b',')]);
            assert_eq!(
                decode(e, b"\x1B$(X"),
                [BAD_INPUT].into_iter().chain(u("$(X")).collect::<Vec<_>>()
            );
            assert_eq!(decode(e, b"\x1B$B!"), vec![BAD_INPUT]);
            assert_eq!(decode(e, b"\xE0"), vec![BAD_INPUT]);
            assert_eq!(encode(e, &[0x22, 0x2225]).0, b"\"\x1B$B!B\x1B(B");
            assert_eq!(encode(e, &[0xA5]).0, b"\x1B(J\\\x1B(B");
            assert_eq!(encode(e, &[0xFEFF]), (b"?".to_vec(), 1));
        }
        assert_eq!(encode(&ENC_2022JP, &[0x203E]).0, b"\x1B$B!1\x1B(B");
        assert_eq!(encode(&ENC_JIS, &[0x203E]).0, b"\x1B(J~\x1B(B");
        assert_eq!(encode(&ENC_JIS, &[0xA1]).0, b"\x1B$(D\"B\x1B(B");
        assert_eq!(encode(&ENC_2022JP, &[0xA1]), (b"?".to_vec(), 1));
        // GR-invoked kana are re-encoded as JIS X 0201 kana
        let w = decode(&ENC_JIS, b"\xB1");
        assert_eq!(encode(&ENC_JIS, &w).0, b"\x1B(I1\x1B(B");
        assert!(check_jis(b"\x1B(H"));
        assert!(!check_jis(b"\x0E!"));
        assert!(!check_iso2022jp(b"\x1B(I!\x1B(B"));
    }

    #[test]
    fn kddi_emoji() {
        // Keypad '#' (PHP itself does not decode this back to '#' U+20E3)
        let (out, errs) = encode(&ENC_2022JP_KDDI, &[0x23, 0x20E3]);
        assert_eq!(errs, 0);
        assert_eq!(out, b"\x1B$Bqi\x1B(B");
        let (out, errs) = encode(&ENC_2022JP_KDDI, &[0x1F1EF, 0x1F1F5]);
        assert_eq!(errs, 0);
        assert!(out.starts_with(b"\x1B$B"));
        // A digit alone is plain ASCII
        assert_eq!(encode(&ENC_2022JP_KDDI, &[0x35]).0, b"5");
        // Legacy filter buffers a digit until the next code point
        assert_eq!(legacy_encode(&ENC_2022JP_KDDI, &[0x35]), b"5");
        assert_eq!(legacy_encode(&ENC_2022JP_KDDI, &[0x35, 0x41]), b"5A");
        assert_eq!(
            legacy_encode(&ENC_2022JP_KDDI, &[0x23, 0x20E3]),
            encode(&ENC_2022JP_KDDI, &[0x23, 0x20E3]).0
        );
    }

    #[test]
    fn ms_udc_and_ibm_ext() {
        // User-defined characters map to the PUA
        let w = decode(&ENC_2022JPMS, b"\x1B$(?\x21\x21\x34\x7E\x1B(B");
        assert_eq!(w, vec![0xE000, 0xE000 + 19 * 94 + 93]);
        assert_eq!(
            encode(&ENC_2022JPMS, &w).0,
            b"\x1B$(?\x21\x21\x34\x7E\x1B(B"
        );
        assert_eq!(
            legacy_encode(&ENC_2022JPMS, &w),
            b"\x1B$(?\x21\x21\x34\x7E\x1B(B"
        );
        assert_eq!(
            legacy_decode(&ENC_2022JPMS, b"\x1B$(?\x21\x21\x34\x7E\x1B(B"),
            w
        );
        // IBM extension (CP932 0xFA40 = SMALL ROMAN NUMERAL ONE) via NEC-selected rows
        let (out, errs) = encode(&ENC_2022JPMS, &[0x2170]);
        assert_eq!(errs, 0);
        assert_eq!(decode(&ENC_2022JPMS, &out), vec![0x2170]);
    }

    #[test]
    fn small_buffers_give_same_result() {
        let samples: [&[u8]; 8] = [
            b"\x1B$(X\x1B$(X\x1B$(X\x1B$X\x1B(X",
            b"\x1B$B\x30\x21\x30\x22\x1B(B abc \x1B(I\x21\x1B(J\x5C",
            b"\x1B$B\x76\x21\x75\x21\x7A\x7B\x1B(B",
            b"\x1B$(?\x21\x21\x22\x22\x1B(B\x1B$(D\x22\x42",
            b"\x1B\x1B\x1B$\x1B(\x1B",
            b"\x0E\x21\x0F\xA1\xDF\xE0\xFF",
            b"\x1B$B\x30",
            b"\x1B$(",
        ];
        for e in ALL {
            for s in samples {
                let full = decode(e, s);
                for n in [5, 6, 8] {
                    assert_eq!(decode_bufsize(e, s, n), full, "{} {s:?} {n}", e.name);
                }
            }
        }
    }

    #[test]
    fn split_encoding_matches_whole() {
        let text = u("abc日本語ｱｲｳ¥‾\u{2170}\u{E000}xyz");
        for e in ALL {
            assert_eq!(encode_split(e, &text), encode(e, &text).0, "{}", e.name);
        }
    }

    #[test]
    #[allow(clippy::cast_possible_wrap)]
    fn legacy_filters_match_fast_codecs() {
        let inputs: [&[u8]; 6] = [
            b"plain ASCII",
            b"\x1B$B\x30\x21\x30\x22\x1B(Babc",
            b"\x1B(I\x21\x5F\x1B(B",
            b"\x1B$B!\x1B(B",
            b"\x1B,x",
            b"\xA1\xDFz",
        ];
        for e in ALL {
            for s in inputs {
                assert_eq!(legacy_decode(e, s), decode(e, s), "{} {s:?}", e.name);
            }
        }
        // Truncated character / escape at the end
        for e in ALL {
            assert_eq!(legacy_decode(e, b"\x1B$B\x30"), vec![BAD_INPUT]);
            assert_eq!(legacy_decode(e, b"\x1B$"), vec![BAD_INPUT]);
        }
        // JIS legacy decoder re-emits the bytes of a bad escape
        assert_eq!(
            legacy_decode(&ENC_JIS, b"\x1B$(X"),
            [BAD_INPUT].into_iter().chain(u("$(X")).collect::<Vec<_>>()
        );
        // Encoders
        let text = u("abc日本語¥xyz");
        for e in [&ENC_JIS, &ENC_2022JP, &ENC_2022JP_KDDI, &ENC_2022JPMS] {
            assert_eq!(legacy_encode(e, &text), encode(e, &text).0, "{}", e.name);
        }
        // Illegal characters: substitution for JIS/KDDI/MS, dropped by ISO-2022-JP
        #[allow(clippy::cast_possible_wrap)]
        let bad = [0x41, 0x10_0000_i32, 0x42];
        assert_eq!(
            run_filter_with(&VTBL_WCHAR_JIS, &bad, ErrorMode::Char, u32::from(b'?')),
            (vec![0x41, 0x3F, 0x42], 1)
        );
        assert_eq!(
            run_filter_with(&VTBL_WCHAR_2022JP, &bad, ErrorMode::Char, u32::from(b'?')),
            (vec![0x41, 0x42], 0)
        );
        assert_eq!(
            run_filter_with(&VTBL_WCHAR_2022JPMS, &bad, ErrorMode::Long, u32::from(b'?')).0,
            u("AU+100000B")
                .iter()
                .map(|&c| c as i32)
                .collect::<Vec<_>>()
        );
    }

    /// Replay the error-free oracle conversions through the legacy filters:
    /// on valid input they must agree with the fast codecs.
    #[test]
    fn legacy_filters_agree_on_valid_oracle_cases() {
        let path = crate::codecs::cjk::testutil::fixture_path("jis_oracle.tsv");
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let unhex = |h: &str| -> Vec<u8> {
            (0..h.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0))
                .collect()
        };
        let mut mismatches = Vec::new();
        let mut checked = 0;
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f[0] != "conv" || f[6] != "0" || f[1] == f[2] {
                continue;
            }
            if f[2] == "UTF-8" {
                let e = crate::codecs::cjk::testutil::enc(f[1]);
                let input = unhex(f[4]);
                checked += 1;
                if legacy_decode(e, &input) != decode(e, &input) {
                    mismatches.push(format!("decode {} {}", f[1], f[4]));
                }
            } else {
                let e = crate::codecs::cjk::testutil::enc(f[2]);
                let text = String::from_utf8(unhex(f[4])).unwrap_or_default();
                let w = u(&text);
                if e.no == EncodingNo::Jis && w.iter().any(|c| (0xFF61..=0xFF9F).contains(c)) {
                    // The legacy JIS encoder (like the C one) mangles
                    // halfwidth kana by sending them down the JIS X 0208 path.
                    continue;
                }
                if e.no == EncodingNo::Iso2022JpKddi
                    && w.iter().any(|c| (0x1F1E6..=0x1F1FF).contains(c))
                {
                    // Only the fast KDDI encoder knows the flag emoji.
                    continue;
                }
                checked += 1;
                if legacy_encode(e, &w) != encode(e, &w).0 {
                    mismatches.push(format!("encode {} {}", f[2], f[4]));
                }
            }
        }
        assert!(checked > 1000);
        assert!(
            mismatches.is_empty(),
            "{} mismatches:\n{}",
            mismatches.len(),
            mismatches[..mismatches.len().min(40)].join("\n")
        );
    }
}

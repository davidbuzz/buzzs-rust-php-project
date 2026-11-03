//! Japanese mobile UTF-8 variants: `UTF-8-Mobile#DOCOMO`,
//! `UTF-8-Mobile#KDDI-A`, `UTF-8-Mobile#KDDI-B`, `UTF-8-Mobile#SOFTBANK`.
//!
//! These are UTF-8 where the carriers' emoji are encoded as code points in
//! the Private Use Area; they are converted to/from standard Unicode emoji.
//!
//! Ref: `libmbfl/filters/mbfilter_utf8.c`.

use crate::codecs::cjk::common::{
    nflags, sjis_emoji_docomo2unicode, sjis_emoji_kddi2unicode, sjis_emoji_sb2unicode,
    NFLAGS_CODE_KDDI, NFLAGS_CODE_SB, NFLAGS_S,
};
use crate::codecs::cjk::sjis::{
    unicode_to_emoji_docomo, unicode_to_emoji_kddi, unicode_to_emoji_sb,
};
use crate::codecs::utf8::{cut_utf8, push_utf8, MBLEN_TABLE_UTF8};
use crate::convert::{illegal_output, ConvertBuf, FromWcharFn, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo};
use crate::tables::cjk::utf8_mobile as t;

/// `UTF-8-Mobile#DOCOMO`.
pub static ENC_UTF8_DOCOMO: Encoding = Encoding {
    no: EncodingNo::Utf8Docomo,
    name: "UTF-8-Mobile#DOCOMO",
    mime_name: Some("UTF-8"),
    aliases: &["UTF-8-DOCOMO", "UTF8-DOCOMO"],
    mblen_table: Some(&MBLEN_TABLE_UTF8),
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: utf8_docomo_to_wchar,
    from_wchar: wchar_to_utf8_docomo,
    check: None,
    cut: Some(cut_utf8),
};

/// `UTF-8-Mobile#KDDI-A`.
pub static ENC_UTF8_KDDI_A: Encoding = Encoding {
    no: EncodingNo::Utf8KddiA,
    name: "UTF-8-Mobile#KDDI-A",
    mime_name: Some("UTF-8"),
    aliases: &[],
    mblen_table: Some(&MBLEN_TABLE_UTF8),
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: utf8_kddi_a_to_wchar,
    from_wchar: wchar_to_utf8_kddi_a,
    check: None,
    cut: Some(cut_utf8),
};

/// `UTF-8-Mobile#KDDI-B`.
pub static ENC_UTF8_KDDI_B: Encoding = Encoding {
    no: EncodingNo::Utf8KddiB,
    name: "UTF-8-Mobile#KDDI-B",
    mime_name: Some("UTF-8"),
    aliases: &["UTF-8-Mobile#KDDI", "UTF-8-KDDI", "UTF8-KDDI"],
    mblen_table: Some(&MBLEN_TABLE_UTF8),
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: utf8_kddi_b_to_wchar,
    from_wchar: wchar_to_utf8_kddi_b,
    check: None,
    cut: Some(cut_utf8),
};

/// `UTF-8-Mobile#SOFTBANK`.
pub static ENC_UTF8_SB: Encoding = Encoding {
    no: EncodingNo::Utf8Sb,
    name: "UTF-8-Mobile#SOFTBANK",
    mime_name: Some("UTF-8"),
    aliases: &["UTF-8-SOFTBANK", "UTF8-SOFTBANK"],
    mblen_table: Some(&MBLEN_TABLE_UTF8),
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: utf8_sb_to_wchar,
    from_wchar: wchar_to_utf8_sb,
    check: None,
    cut: Some(cut_utf8),
};

type EmojiMap = [[u16; 3]];
type EmojiToUnicode = fn(i32, &mut i32) -> i32;

/// `mbfilter_conv_map_tbl`: (ku*94)+ten emoji code -> carrier PUA code point.
fn conv_map_tbl(c: u32, map: &EmojiMap) -> Option<u32> {
    map.iter()
        .find(|r| u32::from(r[0]) <= c && c <= u32::from(r[1]))
        .map(|r| c - u32::from(r[0]) + u32::from(r[2]))
}

/// `mbfilter_conv_r_map_tbl`: carrier PUA code point -> (ku*94)+ten code.
fn conv_r_map_tbl(c: u32, map: &EmojiMap) -> Option<u32> {
    map.iter()
        .find(|r| {
            let (a, b, base) = (u32::from(r[0]), u32::from(r[1]), u32::from(r[2]));
            base <= c && c + a <= base + b
        })
        .map(|r| c + u32::from(r[0]) - u32::from(r[2]))
}

/// `mb_mobile_utf8_to_wchar`: UTF-8 decoding, then carrier PUA emoji are
/// converted to Unicode (possibly as two code points).
#[allow(
    clippy::too_many_lines,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
fn mobile_utf8_to_wchar(
    input: &mut &[u8],
    buf: &mut [u32],
    emoji_map: &EmojiMap,
    convert_emoji: EmojiToUnicode,
) -> usize {
    let st = *input;
    let e = st.len();
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len() - 1;
    let cont = |b: u8| (b & 0xC0) == 0x80;

    while p < e && out < limit {
        let c = st[p];
        p += 1;
        let s: u32;

        if c <= 0x7F {
            buf[out] = u32::from(c);
            out += 1;
            continue;
        } else if (0xC2..=0xDF).contains(&c) && p < e {
            let c2 = st[p];
            p += 1;
            if cont(c2) {
                s = (u32::from(c & 0x1F) << 6) | u32::from(c2 & 0x3F);
            } else {
                buf[out] = BAD_INPUT;
                out += 1;
                p -= 1;
                continue;
            }
        } else if (0xE0..=0xEF).contains(&c) {
            if e - p < 2 {
                buf[out] = BAD_INPUT;
                out += 1;
                if p < e
                    && (c != 0xE0 || st[p] >= 0xA0)
                    && (c != 0xED || st[p] < 0xA0)
                    && cont(st[p])
                {
                    p += 1;
                    if p < e && cont(st[p]) {
                        p += 1;
                    }
                }
                continue;
            }
            let c2 = st[p];
            let c3 = st[p + 1];
            p += 2;
            if !cont(c2) || (c == 0xE0 && c2 < 0xA0) || (c == 0xED && c2 >= 0xA0) {
                buf[out] = BAD_INPUT;
                out += 1;
                p -= 2;
                continue;
            } else if !cont(c3) {
                buf[out] = BAD_INPUT;
                out += 1;
                p -= 1;
                continue;
            }
            s = (u32::from(c & 0xF) << 12) | (u32::from(c2 & 0x3F) << 6) | u32::from(c3 & 0x3F);
        } else if (0xF0..=0xF4).contains(&c) {
            if e - p < 3 {
                buf[out] = BAD_INPUT;
                out += 1;
                if p < e {
                    let c2 = st[p];
                    if (c == 0xF0 && c2 >= 0x90)
                        || (c == 0xF4 && c2 < 0x90)
                        || (0xF1..=0xF3).contains(&c)
                    {
                        while p < e && cont(st[p]) {
                            p += 1;
                        }
                    }
                }
                continue;
            }
            let (c2, c3, c4) = (st[p], st[p + 1], st[p + 2]);
            p += 3;
            if !cont(c2) || (c == 0xF0 && c2 < 0x90) || (c == 0xF4 && c2 >= 0x90) {
                buf[out] = BAD_INPUT;
                out += 1;
                p -= 3;
                continue;
            } else if !cont(c3) {
                buf[out] = BAD_INPUT;
                out += 1;
                p -= 2;
                continue;
            } else if !cont(c4) {
                buf[out] = BAD_INPUT;
                out += 1;
                p -= 1;
                continue;
            }
            s = (u32::from(c & 0x7) << 18)
                | (u32::from(c2 & 0x3F) << 12)
                | (u32::from(c3 & 0x3F) << 6)
                | u32::from(c4 & 0x3F);
        } else {
            buf[out] = BAD_INPUT;
            out += 1;
            continue;
        }

        let mut w = s;
        if let Some(s1) = conv_r_map_tbl(s, emoji_map) {
            let mut snd = 0;
            w = convert_emoji(s1 as i32, &mut snd) as u32;
            if snd != 0 {
                buf[out] = snd as u32;
                out += 1;
            }
        }
        buf[out] = w;
        out += 1;
    }

    *input = &st[p..];
    out
}

/// `mb_utf8_docomo_to_wchar`.
pub fn utf8_docomo_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    mobile_utf8_to_wchar(
        input,
        buf,
        &t::MBFL_DOCOMO2UNI_PUA,
        sjis_emoji_docomo2unicode,
    )
}

/// `mb_utf8_kddi_a_to_wchar`.
pub fn utf8_kddi_a_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    mobile_utf8_to_wchar(input, buf, &t::MBFL_KDDI2UNI_PUA, sjis_emoji_kddi2unicode)
}

/// `mb_utf8_kddi_b_to_wchar`.
pub fn utf8_kddi_b_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    mobile_utf8_to_wchar(input, buf, &t::MBFL_KDDI2UNI_PUA_B, sjis_emoji_kddi2unicode)
}

/// `mb_utf8_sb_to_wchar`.
pub fn utf8_sb_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    mobile_utf8_to_wchar(input, buf, &t::MBFL_SB2UNI_PUA, sjis_emoji_sb2unicode)
}

/// Per-carrier parameters of the mobile UTF-8 encoders.
struct MobileUtf8 {
    /// (ku*94)+ten of the keypad emoji for '#', '0', '1'.
    keypad: [u32; 3],
    /// Flag emoji codes, for carriers which have them.
    flags: Option<&'static [i32; 10]>,
    copyright: u32,
    registered: u32,
    lookup: fn(u32) -> Option<u32>,
    map: &'static EmojiMap,
    this: FromWcharFn,
}

/// Shared body of `mb_wchar_to_utf8_docomo`, `mb_wchar_to_utf8_kddi` and
/// `mb_wchar_to_utf8_sb`. Note: unlike the Shift-JIS variants, no state is
/// kept between calls, so keypad/flag sequences split across buffers are not
/// combined (as in C).
#[allow(clippy::cast_sign_loss)]
fn wchar_to_mobile_utf8(input: &[u32], buf: &mut ConvertBuf, p: &MobileUtf8) {
    let mut idx = 0usize;
    while idx < input.len() {
        let mut w = input[idx];
        idx += 1;
        let len = input.len() - idx;
        let mut s = 0u32;

        if w >= 0x11_0000 {
            illegal_output(w, p.this, buf);
            continue;
        }

        if (w == u32::from(b'#') || (u32::from(b'0')..=u32::from(b'9')).contains(&w)) && len != 0 {
            let w2 = input[idx];
            idx += 1;
            if w2 == 0x20E3 {
                s = if w == u32::from(b'#') {
                    p.keypad[0]
                } else if w == u32::from(b'0') {
                    p.keypad[1]
                } else {
                    p.keypad[2] + (w - u32::from(b'1'))
                };
            } else {
                idx -= 1;
            }
        } else if let (Some(codes), true) = (
            p.flags,
            (nflags(b'C') as u32..=nflags(b'U') as u32).contains(&w),
        ) {
            // C for CN, U for US
            let mut found = None;
            if len != 0 {
                let w2 = input[idx];
                idx += 1;
                // B for GB, U for RU
                if (nflags(b'B') as u32..=nflags(b'U') as u32).contains(&w2) {
                    found = NFLAGS_S
                        .iter()
                        .position(|cc| w == nflags(cc[0]) as u32 && w2 == nflags(cc[1]) as u32)
                        .map(|i| codes[i] as u32);
                }
                if found.is_none() {
                    idx -= 1;
                }
            }
            match found {
                Some(code) => s = code,
                None => {
                    illegal_output(w, p.this, buf);
                    continue;
                }
            }
        } else if w == 0xA9 {
            // Copyright sign
            s = p.copyright;
        } else if w == 0xAE {
            // Registered sign
            s = p.registered;
        } else if let Some(code) = (p.lookup)(w) {
            s = code;
        }

        // process_kuten:
        if s != 0 {
            if let Some(c1) = conv_map_tbl(s, p.map) {
                w = c1;
            }
        }
        push_utf8(&mut buf.out, w);
    }
}

static DOCOMO: MobileUtf8 = MobileUtf8 {
    keypad: [0x2964, 0x296F, 0x2966],
    flags: None,
    copyright: 0x29B5,
    registered: 0x29BA,
    lookup: unicode_to_emoji_docomo,
    map: &t::MBFL_DOCOMO2UNI_PUA,
    this: wchar_to_utf8_docomo,
};

static KDDI_A: MobileUtf8 = MobileUtf8 {
    keypad: [0x25BC, 0x2830, 0x27A6],
    flags: Some(&NFLAGS_CODE_KDDI),
    copyright: 0x27DC,
    registered: 0x27DD,
    lookup: unicode_to_emoji_kddi,
    map: &t::MBFL_KDDI2UNI_PUA,
    this: wchar_to_utf8_kddi_a,
};

static KDDI_B: MobileUtf8 = MobileUtf8 {
    keypad: [0x25BC, 0x2830, 0x27A6],
    flags: Some(&NFLAGS_CODE_KDDI),
    copyright: 0x27DC,
    registered: 0x27DD,
    lookup: unicode_to_emoji_kddi,
    map: &t::MBFL_KDDI2UNI_PUA_B,
    this: wchar_to_utf8_kddi_b,
};

static SB: MobileUtf8 = MobileUtf8 {
    keypad: [0x2817, 0x282C, 0x2823],
    flags: Some(&NFLAGS_CODE_SB),
    copyright: 0x2855,
    registered: 0x2856,
    lookup: unicode_to_emoji_sb,
    map: &t::MBFL_SB2UNI_PUA,
    this: wchar_to_utf8_sb,
};

/// `mb_wchar_to_utf8_docomo`.
pub fn wchar_to_utf8_docomo(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    wchar_to_mobile_utf8(input, buf, &DOCOMO);
}

/// `mb_wchar_to_utf8_kddi_a`.
pub fn wchar_to_utf8_kddi_a(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    wchar_to_mobile_utf8(input, buf, &KDDI_A);
}

/// `mb_wchar_to_utf8_kddi_b`.
pub fn wchar_to_utf8_kddi_b(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    wchar_to_mobile_utf8(input, buf, &KDDI_B);
}

/// `mb_wchar_to_utf8_sb`.
pub fn wchar_to_utf8_sb(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    wchar_to_mobile_utf8(input, buf, &SB);
}

#[cfg(test)]
mod tests {
    use crate::codecs::cjk::testutil as tu;

    #[test]
    fn oracle_fixture() {
        let n = tu::replay_oracle("utf8mobile_oracle.tsv");
        assert!(n > 100);
    }
}

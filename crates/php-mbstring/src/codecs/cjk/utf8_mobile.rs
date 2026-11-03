//! Japanese mobile UTF-8 variants: `UTF-8-Mobile#DOCOMO`,
//! `UTF-8-Mobile#KDDI-A`, `UTF-8-Mobile#KDDI-B`, `UTF-8-Mobile#SOFTBANK`.
//!
//! These are UTF-8 where the carriers' emoji are encoded as code points in
//! the Private Use Area; they are converted to/from standard Unicode emoji.
//!
//! Ref: `libmbfl/filters/mbfilter_utf8.c`.

// The ports keep the C code's short variable names (c, c2, w, s, p, e...).
#![allow(clippy::many_single_char_names)]

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
            let Some(code) = found else {
                illegal_output(w, p.this, buf);
                continue;
            };
            s = code;
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
    use super::*;
    use crate::codecs::cjk::testutil as tu;

    /// Parse a `UTF-8-*.txt` table: (code point, Some(UTF-8 bytes) or None
    /// for `BAD`).
    #[allow(clippy::cast_possible_truncation)]
    fn table(name: &str) -> Option<Vec<(u32, Option<Vec<u8>>)>> {
        let text = tu::data_file(name)?;
        let mut out = Vec::new();
        for line in text.lines() {
            let mut f = line.split('\t');
            let (Some(a), Some(b)) = (f.next(), f.next()) else {
                continue;
            };
            let Ok(cp) = u32::from_str_radix(a.trim_start_matches("0x"), 16) else {
                continue;
            };
            if b.starts_with("BAD") {
                out.push((cp, None));
            } else if let Ok(v) = u64::from_str_radix(b.trim().trim_start_matches("0x"), 16) {
                out.push((cp, Some(tu::code_bytes(v))));
            }
        }
        Some(out)
    }

    #[test]
    fn mapping_tables() {
        for (enc, file) in [
            (&ENC_UTF8_DOCOMO, "UTF-8-DOCOMO.txt"),
            (&ENC_UTF8_KDDI_A, "UTF-8-KDDI-A.txt"),
            (&ENC_UTF8_KDDI_B, "UTF-8-KDDI-B.txt"),
            (&ENC_UTF8_SB, "UTF-8-SOFTBANK.txt"),
        ] {
            let Some(tbl) = table(file) else {
                return;
            };
            let mut mapped = std::collections::HashSet::new();
            for (cp, bytes) in &tbl {
                mapped.insert(*cp);
                match bytes {
                    Some(bytes) => {
                        assert_eq!(
                            tu::encode(enc, &[*cp]),
                            (bytes.clone(), 0),
                            "{} U+{cp:X}",
                            enc.name
                        );
                        // U+FEE26..U+FEE28 and U+FEE2C are not reversible in DoCoMo
                        let irreversible = enc.no == EncodingNo::Utf8Docomo
                            && matches!(
                                &bytes[..],
                                b"\xEE\x9B\x80"
                                    | b"\xEE\x9B\x81"
                                    | b"\xEE\x9B\x82"
                                    | b"\xEE\x9B\x86"
                            );
                        if !irreversible {
                            assert_eq!(
                                tu::decode(enc, bytes),
                                [*cp],
                                "{} {}",
                                enc.name,
                                tu::hex(bytes)
                            );
                        }
                    }
                    None => {
                        assert_eq!(
                            tu::encode(enc, &[*cp]),
                            (b"?".to_vec(), 1),
                            "{} U+{cp:X}",
                            enc.name
                        );
                    }
                }
            }
            // Other code points encode exactly as in UTF-8
            let mut x = 12345u32;
            for _ in 0..2000 {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let cp = (x >> 8) % 0x11_0000;
                if mapped.contains(&cp) || (0xD800..0xE000).contains(&cp) {
                    continue;
                }
                let mut expected = Vec::new();
                push_utf8(&mut expected, cp);
                // (keypad starters are only special when followed by U+20E3)
                assert_eq!(tu::encode(enc, &[cp]).0, expected, "{} U+{cp:X}", enc.name);
            }
            assert_eq!(tu::encode(enc, &[0x11_0000]), (b"?".to_vec(), 1));
        }
    }

    #[test]
    fn bad_utf8_like_phpt() {
        let cases: &[(&[u8], &[u32])] = &[
            (b"\xF4\x90\x80\x80", &[BAD_INPUT; 4]),
            (b"\xF7\x80\x80\x80", &[BAD_INPUT; 4]),
            (b"\xED\xA0\x80", &[BAD_INPUT; 3]),
            (b"\xDF", &[BAD_INPUT]),
            (b"\xEF\xBF", &[BAD_INPUT]),
            (b"\xF0\xBF\xBF", &[BAD_INPUT]),
            (b"\xF1\x96\x80", &[BAD_INPUT]),
            (b"\xE0\x9F", &[BAD_INPUT, BAD_INPUT]),
            (b"\xED\xA6", &[BAD_INPUT, BAD_INPUT]),
            (b"\xDFA", &[BAD_INPUT, 0x41]),
            (b"\xF0\xBF\xBFA", &[BAD_INPUT, 0x41]),
            (b"\xEF\xBF\xDF\xBF", &[BAD_INPUT, 0x7FF]),
            (b"A\x80", &[0x41, BAD_INPUT]),
            (b"\xC1\xBF", &[BAD_INPUT; 2]),
            (b"\xE0\x9F\xBF", &[BAD_INPUT; 3]),
            (b"\xF0\x8F\xBF\xBF", &[BAD_INPUT; 4]),
        ];
        for enc in [
            &ENC_UTF8_DOCOMO,
            &ENC_UTF8_KDDI_A,
            &ENC_UTF8_KDDI_B,
            &ENC_UTF8_SB,
        ] {
            for &(input, expected) in cases {
                assert_eq!(
                    tu::decode(enc, input),
                    expected,
                    "{} {}",
                    enc.name,
                    tu::hex(input)
                );
                assert_eq!(tu::decode_bufsize(enc, input, 5), expected);
            }
        }
    }

    #[test]
    fn keypad_and_flags() {
        // DoCoMo has no flags; KDDI/SoftBank do
        assert_eq!(
            tu::encode(&ENC_UTF8_DOCOMO, &[0x31, 0x20E3]).0,
            "\u{E6E2}".as_bytes()
        );
        assert_eq!(
            tu::encode(&ENC_UTF8_KDDI_A, &[0x1F1EF, 0x1F1F5]).0,
            "\u{E4CC}".as_bytes()
        );
        assert_eq!(
            tu::encode(&ENC_UTF8_SB, &[0x1F1EF, 0x1F1F5]).0,
            "\u{E50B}".as_bytes()
        );
        assert_eq!(
            tu::encode(&ENC_UTF8_SB, &[0x1F1EF, 0x41]),
            (b"?A".to_vec(), 1)
        );
        assert_eq!(
            tu::decode(&ENC_UTF8_SB, "\u{E50B}".as_bytes()),
            [0x1F1EF, 0x1F1F5]
        );
        assert_eq!(
            tu::decode(&ENC_UTF8_DOCOMO, "\u{E6E2}".as_bytes()),
            [0x31, 0x20E3]
        );
    }

    #[test]
    fn small_buffers_match() {
        let text =
            std::fs::read_to_string(tu::fixture_path("utf8mobile_oracle.tsv")).unwrap_or_default();
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
            assert_eq!(tu::decode_bufsize(enc, &input, 5), tu::decode(enc, &input));
        }
    }

    #[test]
    fn oracle_fixture() {
        let n = tu::replay_oracle("utf8mobile_oracle.tsv");
        assert!(n > 100);
    }
}

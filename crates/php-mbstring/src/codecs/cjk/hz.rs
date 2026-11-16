// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `HZ` (RFC 1843: GB 2312 in 7-bit text, switched with `~{` / `~}`).
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c` (section "HZ").

use crate::convert::{illegal_output, ConvertBuf, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};
use crate::legacy::{self, Filter, Sink, Vtbl, BAD_INPUT_I32};
use crate::tables::cjk::cp936 as t;
use crate::tables::cjk::gb2312 as g;

/// `HZ`.
pub static ENC_HZ: Encoding = Encoding {
    no: EncodingNo::Hz,
    name: "HZ",
    mime_name: Some("HZ-GB-2312"),
    aliases: &[],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: Some(&VTBL_HZ_WCHAR),
    output_filter: Some(&VTBL_WCHAR_HZ),
    to_wchar: hz_to_wchar,
    from_wchar: wchar_to_hz,
    check: None,
    cut: None,
};

/// `vtbl_hz_wchar`.
pub static VTBL_HZ_WCHAR: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_conv_hz_wchar,
    flush: filt_conv_hz_wchar_flush,
};

/// `vtbl_wchar_hz`.
pub static VTBL_WCHAR_HZ: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_conv_wchar_hz,
    flush: filt_conv_any_hz_flush,
};

/// GB 2312 row/cell bytes (`0x21..=0x7E` each) to Unicode, via the CP936
/// table with HZ's exceptions; 0 if unmapped.
fn gb2312_to_ucs(c1: u32, c2: u32) -> u32 {
    let s = (c1 - 1) * 192 + c2 + 0x40; /* GB2312 */
    if s == 0x1864 {
        0x30FB
    } else if s == 0x186A {
        0x2015
    } else if s == 0x186C {
        0x2225
    } else if (0x1920..=0x192A).contains(&s)
        || s == 0x1963
        || (0x1C60..=0x1C7F).contains(&s)
        || (0x1DBB..=0x1DC4).contains(&s)
    {
        0
    } else {
        t::CP936_UCS_TABLE
            .get(s as usize)
            .map_or(0, |&v| u32::from(v))
    }
}

/// `mbfl_filt_conv_hz_wchar`.
#[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
pub fn filt_conv_hz_wchar(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    match filter.status & 0xF {
        /* case 0x00: ASCII */
        /* case 0x10: GB2312 */
        0 => {
            if c == i32::from(b'~') {
                filter.status += 2;
            } else if filter.status == 0x10
                && ((c > 0x20 && c <= 0x29) || (0x30..=0x77).contains(&c))
            {
                /* DBCS first char */
                filter.cache = c;
                filter.status += 1;
            } else if filter.status == 0 && (0..0x80).contains(&c) {
                /* latin, CTLs */
                out.put(c);
            } else {
                out.put(BAD_INPUT_I32);
            }
        }
        /* case 0x11: GB2312 second char */
        1 => {
            filter.status &= !0xF;
            let c1 = filter.cache;
            if c1 > 0x20 && c1 < 0x7F && c > 0x20 && c < 0x7F {
                let w = gb2312_to_ucs(c1 as u32, c as u32);
                out.put(if w == 0 { BAD_INPUT_I32 } else { w as i32 });
            } else {
                out.put(BAD_INPUT_I32);
            }
        }
        /* '~' */
        _ => {
            if c == i32::from(b'}') && filter.status == 0x12 {
                filter.status = 0;
            } else if c == i32::from(b'{') && filter.status == 2 {
                filter.status = 0x10;
            } else if c == i32::from(b'~') && filter.status == 2 {
                out.put(i32::from(b'~'));
                filter.status -= 2;
            } else if c == i32::from(b'\n') {
                /* "~\n" is a line continuation; no output is needed, nor should we shift modes */
                filter.status -= 2;
            } else {
                /* Invalid character after ~ */
                filter.status -= 2;
                out.put(BAD_INPUT_I32);
            }
        }
    }
}

/// `mbfl_filt_conv_hz_wchar_flush`.
pub fn filt_conv_hz_wchar_flush(filter: &mut Filter, out: &mut dyn Sink) {
    if filter.status == 0x11 {
        /* 2-byte character was truncated */
        out.put(BAD_INPUT_I32);
    }
    filter.status = 0;
    out.flush();
}

/// Unicode to GB 2312 (with the high bits of the EUC form possibly set) for
/// HZ; 0 if there is no mapping. Shared by both HZ encoders, which then
/// post-process the result differently.
fn ucs_to_hz(c: u32) -> u32 {
    #[allow(clippy::cast_sign_loss)]
    let r = |tbl: &[u16], min: i32, max: i32| -> Option<u32> {
        let (min, max) = (min as u32, max as u32);
        (c >= min && c < max).then(|| tbl.get((c - min) as usize).map_or(0, |&v| u32::from(v)))
    };

    if let Some(v) = r(
        &t::UCS_A1_CP936_TABLE,
        t::UCS_A1_CP936_TABLE_MIN,
        t::UCS_A1_CP936_TABLE_MAX,
    ) {
        if matches!(
            c,
            0xB7 | 0x144 | 0x148 | 0x251 | 0x261 | 0x2CA | 0x2CB | 0x2D9
        ) {
            0
        } else {
            v
        }
    } else if let Some(v) = r(
        &t::UCS_A2_CP936_TABLE,
        t::UCS_A2_CP936_TABLE_MIN,
        t::UCS_A2_CP936_TABLE_MAX,
    ) {
        if c == 0x2015 {
            0xA1AA
        } else if matches!(
            c,
            0x2010
                | 0x2013
                | 0x2014
                | 0x2016
                | 0x2025
                | 0x2035
                | 0x2105
                | 0x2109
                | 0x2121
                | 0x2170..=0x2179
                | 0x2196..=0x2199
                | 0x2215
                | 0x221F
                | 0x2223
                | 0x2252
                | 0x2266
                | 0x2267
                | 0x2295
                | 0x2550..=0x2573
                | 0x22BF
                | 0x2609
                | 0x2581..=0x258F
                | 0x2593..=0x2595
                | 0x25BC
                | 0x25BD
                | 0x25E2..=0x25E5
        ) {
            0
        } else {
            v
        }
    } else if let Some(v) = r(
        &t::UCS_A3_CP936_TABLE,
        t::UCS_A3_CP936_TABLE_MIN,
        t::UCS_A3_CP936_TABLE_MAX,
    ) {
        if c == 0x30FB {
            0xA1A4
        } else if c == 0x3006
            || c == 0x3007
            || c == 0x3012
            || c == 0x3231
            || c == 0x32A3
            || c >= 0x3300
            || (0x3018..=0x3040).contains(&c)
            || (0x309B..=0x309E).contains(&c)
            || (0x30FC..=0x30FE).contains(&c)
        {
            0
        } else {
            v
        }
    } else if let Some(v) = r(
        &g::UCS_I_GB2312_TABLE,
        g::UCS_I_GB2312_TABLE_MIN,
        g::UCS_I_GB2312_TABLE_MAX,
    ) {
        v
    } else if (0xFF00..0xFFFF).contains(&c) {
        /* ucs_hff_cp936_table_min..ucs_hff_cp936_table_max */
        match c {
            0xFF04 => 0xA1E7,
            0xFF5E => 0xA1AB,
            0xFF01..=0xFF5D => c - 0xFF01 + 0xA3A1,
            0xFFE0 | 0xFFE1 | 0xFFE3 | 0xFFE5 => {
                u32::from(t::UCS_HFF_S_CP936_TABLE[(c - 0xFFE0) as usize])
            }
            _ => 0,
        }
    } else {
        0
    }
}

/// `mbfl_filt_conv_wchar_hz`.
#[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
pub fn filt_conv_wchar_hz(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    let mut s = ucs_to_hz(c as u32) as i32;

    if s & 0x8000 != 0 {
        s -= 0x8080;
    }

    if s <= 0 {
        s = if c == 0 { 0 } else { -1 };
    } else if (0x80..0x2121).contains(&s) || s > 0x8080 {
        s = -1;
    }

    if s >= 0 {
        if s < 0x80 {
            /* ASCII */
            if (filter.status & 0xFF00) != 0 {
                out.put(i32::from(b'~'));
                out.put(i32::from(b'}'));
            }
            filter.status = 0;
            if s == 0x7E {
                out.put(i32::from(b'~'));
            }
            out.put(s);
        } else {
            /* GB 2312-80 */
            if (filter.status & 0xFF00) != 0x200 {
                out.put(i32::from(b'~'));
                out.put(i32::from(b'{'));
            }
            filter.status = 0x200;
            out.put((s >> 8) & 0x7F);
            out.put(s & 0x7F);
        }
    } else {
        legacy::illegal_output(c, filter, out);
    }
}

/// `mbfl_filt_conv_any_hz_flush`. (Like the C code, this does not flush
/// downstream.)
pub fn filt_conv_any_hz_flush(filter: &mut Filter, out: &mut dyn Sink) {
    /* back to latin */
    if filter.status & 0xFF00 != 0 {
        out.put(i32::from(b'~'));
        out.put(i32::from(b'}'));
    }
    filter.status = 0;
}

const ASCII: u32 = 0;
const GB2312: u32 = 1;

/// `mb_hz_to_wchar`.
#[allow(clippy::many_single_char_names)]
pub fn hz_to_wchar(input: &mut &[u8], buf: &mut [u32], state: &mut u32) -> usize {
    let s = *input;
    let e = s.len();
    let mut p = 0usize;
    let mut out = 0usize;
    let limit = buf.len();

    while p < e && out < limit {
        let c = s[p];
        p += 1;

        if c == b'~' {
            if p == e {
                break;
            }
            let c2 = s[p];
            p += 1;

            if c2 == b'}' && *state == GB2312 {
                *state = ASCII;
            } else if c2 == b'{' && *state == ASCII {
                *state = GB2312;
            } else if c2 == b'~' && *state == ASCII {
                buf[out] = u32::from(b'~');
                out += 1;
            } else if c2 == b'\n' {
                /* "~\n" is a line continuation; no output is needed, nor should we shift modes */
            } else {
                /* Invalid character after ~ */
                buf[out] = BAD_INPUT;
                out += 1;
            }
        } else if ((c > 0x20 && c <= 0x29) || (0x30..=0x77).contains(&c))
            && p < e
            && *state == GB2312
        {
            let c2 = s[p];
            p += 1;
            buf[out] = if c > 0x20 && c < 0x7F && c2 > 0x20 && c2 < 0x7F {
                match gb2312_to_ucs(u32::from(c), u32::from(c2)) {
                    0 => BAD_INPUT,
                    w => w,
                }
            } else {
                BAD_INPUT
            };
            out += 1;
        } else if c < 0x80 && *state == ASCII {
            buf[out] = u32::from(c);
            out += 1;
        } else {
            buf[out] = BAD_INPUT;
            out += 1;
        }
    }

    *input = &s[p..];
    out
}

/// `mb_wchar_to_hz`.
#[allow(clippy::cast_possible_truncation)]
pub fn wchar_to_hz(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    for &w in input {
        let s = ucs_to_hz(w) & !0x8080;

        if (s == 0 && w != 0) || (0x80..0x2121).contains(&s) {
            illegal_output(w, wchar_to_hz, buf);
        } else if s < 0x80 {
            /* ASCII */
            if buf.state != ASCII {
                buf.extend(b"~}");
                buf.state = ASCII;
            }
            if s == u32::from(b'~') {
                buf.extend(b"~~");
            } else {
                buf.push(s as u8);
            }
        } else {
            /* GB 2312-80 */
            if buf.state != GB2312 {
                buf.extend(b"~{");
                buf.state = GB2312;
            }
            buf.extend(&[((s >> 8) & 0x7F) as u8, (s & 0x7F) as u8]);
        }
    }

    if end && buf.state != ASCII {
        /* If not in ASCII state, need to emit closing control chars */
        buf.extend(b"~}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codecs::cjk::gb18030::phpt;
    use crate::codecs::cjk::testutil::{
        decode, decode_bufsize, encode, encode_split, fixture_path, legacy_decode, legacy_encode,
    };

    fn hz(inner: &[u8]) -> Vec<u8> {
        let mut v = b"~{".to_vec();
        v.extend(inner);
        v.extend(b"~}");
        v
    }

    #[test]
    fn escapes() {
        for c in 0..0x80u8 {
            if c != b'~' {
                assert_eq!(decode(&ENC_HZ, &[c]), vec![u32::from(c)]);
                assert_eq!(encode(&ENC_HZ, &[u32::from(c)]).0, vec![c]);
            }
        }
        for c in 0x80..=0xFFu8 {
            assert_eq!(decode(&ENC_HZ, &[c]), vec![BAD_INPUT]);
        }
        assert_eq!(decode(&ENC_HZ, b"~~"), vec![0x7E]);
        assert_eq!(encode(&ENC_HZ, &[0x7E]).0, b"~~");
        assert_eq!(decode(&ENC_HZ, b"~\n"), vec![]);
        assert_eq!(decode(&ENC_HZ, b"~{~}"), vec![]);
        assert_eq!(decode(&ENC_HZ, b"~{~\n~}"), vec![]);
        assert_eq!(decode(&ENC_HZ, b"~"), vec![]);
        for c in 0..0xFFu8 {
            if c == b'\n' {
                continue;
            }
            if c != b'~' && c != b'{' {
                assert_eq!(decode(&ENC_HZ, &[b'~', c]), vec![BAD_INPUT], "~{c:02x}");
            }
            if c != b'}' {
                assert_eq!(
                    decode(&ENC_HZ, &hz(&[b'~', c])),
                    vec![BAD_INPUT],
                    "~{{~{c:02x}~}}"
                );
            }
        }
        // Regression tests from hz_encoding.phpt
        assert_eq!(encode(&ENC_HZ, &[0x7AFA, 0]).0, b"~{\x73\x43~}\x00");
    }

    #[test]
    fn gb2312_mapping_file() {
        let Some((to, from)) = phpt::read_table("GB2312.txt", false) else {
            return;
        };
        for (b, &cp) in &to {
            assert_eq!(decode(&ENC_HZ, &hz(b)), vec![cp], "{b:02x?}");
            if b != &[0x21, 0x2C] {
                assert_eq!(encode(&ENC_HZ, &[cp]).0, hz(b), "U+{cp:04X}");
            }
        }
        // Invalid / truncated GB2312 characters within ~{ ~}
        let goods: Vec<(&Vec<u8>, &u32)> = to.iter().collect();
        let mut i = 0;
        for b1 in 0..=255u8 {
            let is_lead = to.keys().any(|k| k[0] == b1);
            let bads: Vec<Vec<u8>> = if is_lead {
                assert_eq!(
                    decode(&ENC_HZ, &[b"~{".as_slice(), &[b1]].concat()),
                    vec![BAD_INPUT]
                );
                (0..=255u8)
                    .map(|b2| vec![b1, b2])
                    .filter(|k| !to.contains_key(k))
                    .collect()
            } else if b1 == b'~' {
                vec![]
            } else {
                vec![vec![b1]]
            };
            for bad in bads {
                let (good, &cp) = goods[i % goods.len()];
                i += 1;
                let input = hz(&[bad.as_slice(), good].concat());
                assert_eq!(decode(&ENC_HZ, &input), vec![BAD_INPUT, cp], "{bad:02x?}");
            }
        }
        // Unencodable code points
        for cp in 0x80..0x10000u32 {
            if !from.contains_key(&cp) {
                let (out, errors) = encode(&ENC_HZ, &[cp, 0x3000]);
                assert_eq!((out, errors), (b"?~{!!~}".to_vec(), 1), "U+{cp:04X}");
            }
        }
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("bad hex"))
            .collect()
    }

    /// The legacy filters (used by `mb_strcut`) agree with the fast
    /// converters on every oracle input.
    #[test]
    fn legacy_filters_match_fast_converters() {
        let text = std::fs::read_to_string(fixture_path("hz_oracle.tsv")).expect("fixture");
        let mut n = 0;
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            match (f[0], f.get(1).copied(), f.get(2).copied()) {
                ("conv", Some("HZ"), Some("UTF-8")) | ("check", Some("HZ"), _) => {
                    let input = unhex(if f[0] == "conv" { f[4] } else { f[2] });
                    assert_eq!(
                        legacy_decode(&ENC_HZ, &input),
                        decode(&ENC_HZ, &input),
                        "{line}"
                    );
                    n += 1;
                }
                ("conv", Some("UTF-8"), Some("HZ")) if f[3] == "3f" => {
                    let cps =
                        crate::convert::decode_all(&unhex(f[4]), &crate::codecs::utf8::ENC_UTF8);
                    assert_eq!(legacy_encode(&ENC_HZ, &cps), unhex(f[5]), "{line}");
                    n += 1;
                }
                _ => {}
            }
        }
        assert!(n > 500);
        // Truncated character / dangling escape at the end
        assert_eq!(legacy_decode(&ENC_HZ, b"~{!"), vec![BAD_INPUT]);
        assert_eq!(legacy_decode(&ENC_HZ, b"a~"), vec![0x61]);
        assert_eq!(
            legacy_encode(&ENC_HZ, &[0x3000, 0x41, 0x3000]),
            b"~{!!~}A~{!!~}"
        );
    }

    #[test]
    fn small_buffers_and_split_encoding() {
        let input = b"a~~~{!!0!~}~\nz~{\x80!~}~A".to_vec();
        let full = decode(&ENC_HZ, &input);
        for size in [1, 2, 5, 8] {
            assert_eq!(
                decode_bufsize(&ENC_HZ, &input, size),
                full,
                "bufsize {size}"
            );
        }
        let cps = [0x41, 0x3000, 0x3000, 0x7E, 0x4E00, 0x1F600, 0x4E00];
        assert_eq!(encode_split(&ENC_HZ, &cps), encode(&ENC_HZ, &cps).0);
        assert_eq!(encode(&ENC_HZ, &cps).0, b"A~{!!!!~}~~~{R;~}?~{R;~}");
    }

    #[test]
    fn detail_oracle() {
        assert!(crate::codecs::cjk::testutil::replay_oracle("hz_oracle.tsv") > 500);
    }

    #[test]
    fn exhaustive_oracle() {
        assert!(crate::codecs::cjk::gb18030::exhaustive::replay("hz_exhaustive.tsv") > 500);
    }
}

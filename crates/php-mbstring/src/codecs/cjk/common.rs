//! Helpers shared by several CJK codecs (the macros and small functions at
//! the top of `mbfilter_cjk.c`, plus the mobile emoji decoders exported via
//! `mbfilter_cjk.h`).
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c`.

use crate::tables::cjk::emoji as e;

/// `NFLAGS(c)`: map an uppercase ASCII letter to the corresponding Regional
/// Indicator code point (two of them make a flag emoji).
#[inline]
pub const fn nflags(c: u8) -> i32 {
    0x1F1A5 + c as i32
}

/// `nflags_s`: country codes of the flag emoji known to the mobile carriers.
pub const NFLAGS_S: [&[u8; 2]; 10] = [
    b"CN", b"DE", b"ES", b"FR", b"GB", b"IT", b"JP", b"KR", b"RU", b"US",
];

/// `nflags_kddi`.
pub const NFLAGS_KDDI: [&[u8; 2]; 6] = [b"FR", b"DE", b"IT", b"GB", b"CN", b"KR"];

/// `nflags_sb`.
pub const NFLAGS_SB: [&[u8; 2]; 10] = [
    b"JP", b"US", b"FR", b"DE", b"IT", b"GB", b"ES", b"RU", b"CN", b"KR",
];

pub use crate::tables::cjk::misc::{NFLAGS_CODE_KDDI, NFLAGS_CODE_SB};

/// `DOCOMO_KEYPAD(n)`: (ku*94)+ten value of the telephone keypad emoji for
/// digit `n`.
#[inline]
pub const fn docomo_keypad(n: i32) -> i32 {
    if n == 0 {
        0x296F
    } else {
        0x2965 + n
    }
}

/// `DOCOMO_KEYPAD_HASH`.
pub const DOCOMO_KEYPAD_HASH: i32 = 0x2964;

/// `SJIS_ENCODE(c1, c2, s1, s2)`: JIS row/cell bytes (`0x21..=0x7E`) to a
/// Shift-JIS byte pair, returned as `(s1, s2)`.
#[inline]
pub const fn sjis_encode(c1: i32, c2: i32) -> (i32, i32) {
    let s1 = ((c1 - 1) >> 1) + if c1 < 0x5F { 0x71 } else { 0xB1 };
    let mut s2 = c2;
    if c1 & 1 != 0 {
        if c2 < 0x60 {
            s2 -= 1;
        }
        s2 += 0x20;
    } else {
        s2 += 0x7E;
    }
    (s1, s2)
}

/// `SJIS_DECODE(c1, c2, s1, s2)`: Shift-JIS byte pair to JIS row/cell bytes,
/// returned as `(s1, s2)`.
#[inline]
pub const fn sjis_decode(c1: i32, c2: i32) -> (i32, i32) {
    let mut s1 = if c1 < 0xA0 {
        ((c1 - 0x81) << 1) + 0x21
    } else {
        ((c1 - 0xC1) << 1) + 0x21
    };
    let mut s2 = c2;
    if c2 < 0x9F {
        if c2 < 0x7F {
            s2 += 1;
        }
        s2 -= 0x20;
    } else {
        s1 += 1;
        s2 -= 0x7E;
    }
    (s1, s2)
}

/// `mbfl_binary_search_paired_sorted_table`: find `w` among the first
/// elements of `tbl` (sorted by them) and return the paired second element.
pub fn binary_search_paired_sorted_table(w: u32, tbl: &[[u16; 2]]) -> Option<u16> {
    let (mut l, mut r) = (0usize, tbl.len());
    while l < r {
        let probe = (l + r) >> 1;
        let key = u32::from(tbl[probe][0]);
        match w.cmp(&key) {
            std::cmp::Ordering::Less => r = probe,
            std::cmp::Ordering::Greater => l = probe + 1,
            std::cmp::Ordering::Equal => return Some(tbl[probe][1]),
        }
    }
    None
}

/// `mbfl_bisec_srch` with C's `int` interface: index of the `[lo, hi]` range
/// in the flattened pair table `tbl` containing `w`, or -1.
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
pub fn bisec_srch(w: i32, tbl: &[u16]) -> i32 {
    crate::codecs::bisec_srch(i64::from(w), tbl).map_or(-1, |i| i as i32)
}

/// `mbfl_bisec_srch2` with C's `int` interface: index of `w` in the sorted
/// table `tbl`, or -1.
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
pub fn bisec_srch2(w: i32, tbl: &[u16]) -> i32 {
    crate::codecs::bisec_srch2(i64::from(w), tbl).map_or(-1, |i| i as i32)
}

/// `convert_emoji_cp`: emoji tables store 16 bits; recover the real code
/// point (SMP emoji above `0xF000`, carrier PUA above `0xE000`).
#[inline]
pub const fn convert_emoji_cp(cp: i32) -> i32 {
    if cp > 0xF000 {
        cp + 0x10000
    } else if cp > 0xE000 {
        cp + 0xF0000
    } else {
        cp
    }
}

#[inline]
fn tbl(t: &[u16], s: i32, min: i32) -> i32 {
    #[allow(clippy::cast_sign_loss)]
    i32::from(t[(s - min) as usize])
}

/// `EMIT_FLAG_EMOJI(country)`.
#[inline]
fn flag(country: [u8; 2], snd: &mut i32) -> i32 {
    *snd = nflags(country[0]);
    nflags(country[1])
}

/// `mbfilter_sjis_emoji_docomo2unicode`: convert a `DoCoMo` emoji (as a
/// `(ku*94)+ten` value) to Unicode. Returns the code point (0 if `s` is not
/// an emoji); `*snd` receives a preceding code point, or 0.
pub fn sjis_emoji_docomo2unicode(s: i32, snd: &mut i32) -> i32 {
    if (e::MB_TBL_CODE2UNI_DOCOMO1_MIN..=e::MB_TBL_CODE2UNI_DOCOMO1_MAX).contains(&s) {
        let w = convert_emoji_cp(tbl(
            &e::MB_TBL_CODE2UNI_DOCOMO1,
            s,
            e::MB_TBL_CODE2UNI_DOCOMO1_MIN,
        ));
        if (s >= docomo_keypad(1) && s <= docomo_keypad(9))
            || s == docomo_keypad(0)
            || s == DOCOMO_KEYPAD_HASH
        {
            *snd = w;
            return 0x20E3;
        }
        *snd = 0;
        return w;
    }
    0
}

/// `mbfilter_sjis_emoji_kddi2unicode`: like [`sjis_emoji_docomo2unicode`]
/// for KDDI emoji.
pub fn sjis_emoji_kddi2unicode(s: i32, snd: &mut i32) -> i32 {
    if (e::MB_TBL_CODE2UNI_KDDI1_MIN..=e::MB_TBL_CODE2UNI_KDDI1_MAX).contains(&s) {
        if s == 0x24C0 {
            return flag(*b"ES", snd);
        } else if s == 0x24C1 {
            return flag(*b"RU", snd);
        } else if (0x2545..=0x254A).contains(&s) {
            #[allow(clippy::cast_sign_loss)]
            return flag(*NFLAGS_KDDI[(s - 0x2545) as usize], snd);
        } else if s == 0x25BC {
            *snd = i32::from(b'#');
            return 0x20E3;
        }
        *snd = 0;
        return convert_emoji_cp(tbl(
            &e::MB_TBL_CODE2UNI_KDDI1,
            s,
            e::MB_TBL_CODE2UNI_KDDI1_MIN,
        ));
    } else if (e::MB_TBL_CODE2UNI_KDDI2_MIN..=e::MB_TBL_CODE2UNI_KDDI2_MAX).contains(&s) {
        if s == 0x2750 {
            return flag(*b"JP", snd);
        } else if (0x27A6..=0x27AE).contains(&s) {
            *snd = s - 0x27A6 + i32::from(b'1');
            return 0x20E3;
        } else if s == 0x27F7 {
            return flag(*b"US", snd);
        } else if s == 0x2830 {
            *snd = i32::from(b'0');
            return 0x20E3;
        }
        *snd = 0;
        return convert_emoji_cp(tbl(
            &e::MB_TBL_CODE2UNI_KDDI2,
            s,
            e::MB_TBL_CODE2UNI_KDDI2_MIN,
        ));
    }
    0
}

/// `mbfilter_sjis_emoji_sb2unicode`: like [`sjis_emoji_docomo2unicode`] for
/// `SoftBank` emoji.
pub fn sjis_emoji_sb2unicode(s: i32, snd: &mut i32) -> i32 {
    if (e::MB_TBL_CODE2UNI_SB1_MIN..=e::MB_TBL_CODE2UNI_SB1_MAX).contains(&s) {
        let raw = tbl(&e::MB_TBL_CODE2UNI_SB1, s, e::MB_TBL_CODE2UNI_SB1_MIN);
        if s == 0x2817 || (0x2823..=0x282C).contains(&s) {
            // Note: no `convert_emoji_cp` here, exactly as in the C code.
            *snd = raw;
            return 0x20E3;
        }
        *snd = 0;
        return convert_emoji_cp(raw);
    } else if (e::MB_TBL_CODE2UNI_SB2_MIN..=e::MB_TBL_CODE2UNI_SB2_MAX).contains(&s) {
        *snd = 0;
        return convert_emoji_cp(tbl(&e::MB_TBL_CODE2UNI_SB2, s, e::MB_TBL_CODE2UNI_SB2_MIN));
    } else if (e::MB_TBL_CODE2UNI_SB3_MIN..=e::MB_TBL_CODE2UNI_SB3_MAX).contains(&s) {
        if (0x2B02..=0x2B0B).contains(&s) {
            #[allow(clippy::cast_sign_loss)]
            return flag(*NFLAGS_SB[(s - 0x2B02) as usize], snd);
        }
        *snd = 0;
        return convert_emoji_cp(tbl(&e::MB_TBL_CODE2UNI_SB3, s, e::MB_TBL_CODE2UNI_SB3_MIN));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sjis_encode_decode_roundtrip() {
        for c1 in 0x21..=0x7E {
            for c2 in 0x21..=0x7E {
                let (s1, s2) = sjis_encode(c1, c2);
                assert_eq!(sjis_decode(s1, s2), (c1, c2));
            }
        }
        assert_eq!(sjis_encode(0x30, 0x21), (0x88, 0x9F));
    }

    #[test]
    fn emoji_decoders() {
        let mut snd = -1;
        // DoCoMo keypad '1' => '1' U+20E3
        assert_eq!(
            sjis_emoji_docomo2unicode(docomo_keypad(1), &mut snd),
            0x20E3
        );
        assert_eq!(snd, 0x31);
        // KDDI Japan flag
        assert_eq!(sjis_emoji_kddi2unicode(0x2750, &mut snd), nflags(b'P'));
        assert_eq!(snd, nflags(b'J'));
        // Not an emoji
        assert_eq!(sjis_emoji_sb2unicode(0x100, &mut snd), 0);
    }

    #[test]
    fn paired_search() {
        let t = [[1u16, 10], [3, 30], [7, 70]];
        assert_eq!(binary_search_paired_sorted_table(3, &t), Some(30));
        assert_eq!(binary_search_paired_sorted_table(4, &t), None);
    }
}

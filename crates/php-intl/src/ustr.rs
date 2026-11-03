//! UTF-8 ⇄ UTF-16 conversions with php-src's `intl_convert_*` semantics.

use crate::error::UErrorCode;

/// `intl_convert_utf8_to_utf16()`: strict conversion (`u_strFromUTF8`
/// fails with `U_INVALID_CHAR_FOUND` on ill-formed input).
pub fn to_u16(src: &[u8]) -> Result<Vec<u16>, UErrorCode> {
    if src.len() > i32::MAX as usize {
        return Err(UErrorCode::BUFFER_OVERFLOW_ERROR);
    }
    match std::str::from_utf8(src) {
        Ok(s) => Ok(s.encode_utf16().collect()),
        Err(_) => Err(UErrorCode::INVALID_CHAR_FOUND),
    }
}

/// UTF-16 of a Rust string (infallible).
pub fn str_u16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// `intl_convert_utf16_to_utf8()`: fails with `U_INVALID_CHAR_FOUND` on
/// unpaired surrogates.
pub fn from_u16(src: &[u16]) -> Result<String, UErrorCode> {
    String::from_utf16(src).map_err(|_| UErrorCode::INVALID_CHAR_FOUND)
}

/// Lossy UTF-16 → UTF-8 (U+FFFD for unpaired surrogates).
pub fn from_u16_lossy(src: &[u16]) -> String {
    String::from_utf16_lossy(src)
}

/// `intl_convert_utf8_offset_to_utf16()`.  Out-of-range positions are left
/// untouched; returns `Err` when the prefix is not valid UTF-8.
pub fn utf8_offset_to_utf16(s: &[u8], position: i32) -> Result<i32, UErrorCode> {
    let Ok(p) = usize::try_from(position) else {
        return Ok(position);
    };
    if p > s.len() {
        return Ok(position);
    }
    let prefix = &s[..p];
    match std::str::from_utf8(prefix) {
        Ok(t) => Ok(i32::try_from(t.encode_utf16().count()).unwrap_or(i32::MAX)),
        Err(e) => {
            // u_strFromUTF8 reports a truncated final sequence as an error too.
            let _ = e;
            Err(UErrorCode::INVALID_CHAR_FOUND)
        }
    }
}

/// `intl_convert_utf16_offset_to_utf8()`.
pub fn utf16_offset_to_utf8(units: &[u16], position: i32) -> i32 {
    let Ok(mut pos) = usize::try_from(position) else {
        return position;
    };
    if pos > units.len() {
        return position;
    }
    if (1..units.len()).contains(&pos) && is_lead(units[pos - 1]) && is_trail(units[pos]) {
        pos -= 1;
    }
    let prefix = &units[..pos];
    if String::from_utf16(prefix).is_err() {
        // u_strToUTF8 fails on unpaired surrogates; php keeps the position.
        return i32::try_from(pos).unwrap_or(position);
    }
    i32::try_from(u16_index_to_u8(units, pos)).unwrap_or(i32::MAX)
}

/// ICU's `U8_NEXT` on the start of `s` → `(code point or -1, bytes
/// consumed)`.  An empty input reads the implicit NUL terminator.
pub fn u8_next(s: &[u8]) -> (i32, usize) {
    let Some(&lead) = s.first() else {
        return (0, 1);
    };
    let (need, init, min) = match lead {
        0x00..=0x7f => return (i32::from(lead), 1),
        0xc2..=0xdf => (1, u32::from(lead & 0x1f), 0x80),
        0xe0..=0xef => (2, u32::from(lead & 0x0f), 0x800),
        0xf0..=0xf4 => (3, u32::from(lead & 0x07), 0x10000),
        _ => return (-1, 1),
    };
    let mut cp = init;
    let mut i = 1;
    while i <= need {
        match s.get(i) {
            Some(&t) if (0x80..0xc0).contains(&t) => {
                cp = (cp << 6) | u32::from(t & 0x3f);
                i += 1;
            }
            _ => return (-1, i),
        }
    }
    if cp < min || (0xd800..0xe000).contains(&cp) || cp > 0x0010_ffff {
        return (-1, i);
    }
    (i32::try_from(cp).unwrap_or(-1), i)
}

/// `U16_IS_LEAD`.
pub fn is_lead(u: u16) -> bool {
    (0xD800..0xDC00).contains(&u)
}

/// `U16_IS_TRAIL`.
pub fn is_trail(u: u16) -> bool {
    (0xDC00..0xE000).contains(&u)
}

/// `U16_IS_SURROGATE`.
pub fn is_surrogate(u: u16) -> bool {
    (0xD800..0xE000).contains(&u)
}

/// Number of UTF-16 code units of the first `n` bytes of valid UTF-8 `s`.
pub fn u8_prefix_u16_len(s: &str, n: usize) -> usize {
    s[..n].encode_utf16().count()
}

/// Converts a UTF-16 index of `u` back to a byte index in the UTF-8 string
/// it was converted from.
pub fn u16_index_to_u8(u: &[u16], idx: usize) -> usize {
    let mut n = 0usize;
    let mut i = 0usize;
    while i < idx && i < u.len() {
        let c = u[i];
        if is_lead(c) && i + 1 < u.len() && is_trail(u[i + 1]) {
            n += 4;
            i += 2;
        } else {
            n += if c < 0x80 {
                1
            } else if c < 0x800 {
                2
            } else {
                3
            };
            i += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let u = to_u16("aé😀".as_bytes()).unwrap();
        assert_eq!(u.len(), 4);
        assert_eq!(from_u16(&u).unwrap(), "aé😀");
        assert_eq!(to_u16(b"\xff"), Err(UErrorCode::INVALID_CHAR_FOUND));
        assert_eq!(from_u16(&[0xD800]), Err(UErrorCode::INVALID_CHAR_FOUND));
    }

    #[test]
    fn offsets() {
        let s = "aé😀b";
        assert_eq!(utf8_offset_to_utf16(s.as_bytes(), 3), Ok(2));
        assert_eq!(utf8_offset_to_utf16(s.as_bytes(), 7), Ok(4));
        assert_eq!(utf8_offset_to_utf16(s.as_bytes(), 99), Ok(99));
        let u = str_u16(s);
        assert_eq!(utf16_offset_to_utf8(&u, 4), 7);
        assert_eq!(utf16_offset_to_utf8(&u, 3), 3);
        assert_eq!(u16_index_to_u8(&u, 2), 3);
    }
}

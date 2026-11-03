//! lexbor's lenient UTF-8 helpers (`lxb_encoding_decode_valid_utf_8_single`
//! and friends). They trust lead bytes and do not validate continuation
//! bytes, which affects how invalid input is reported.

/// The code point lexbor returns for undecodable input.
pub const DECODE_ERROR: u32 = 0x1F_FFFF;

/// Decodes one code point starting at `*pos`, advancing `*pos`.
pub fn decode_valid_utf8_single(s: &[u8], pos: &mut usize) -> u32 {
    let p = *pos;
    let rest = s.len() - p;
    let c = s[p];
    let (len, init) = if c < 0x80 {
        (1, u32::from(c))
    } else if c & 0xE0 == 0xC0 {
        (2, u32::from(c & 0x1F))
    } else if c & 0xF0 == 0xE0 {
        (3, u32::from(c & 0x0F))
    } else if c & 0xF8 == 0xF0 {
        (4, u32::from(c & 0x07))
    } else {
        *pos += 1;
        return DECODE_ERROR;
    };
    if rest < len {
        *pos = s.len();
        return DECODE_ERROR;
    }
    let mut cp = init;
    for &b in &s[p + 1..p + len] {
        cp = (cp << 6) | u32::from(b ^ (0x80 & b));
    }
    *pos += len;
    cp
}

/// Decodes one code point backwards from `*end` (not before `begin`),
/// moving `*end` to its start (`lxb_encoding_decode_valid_utf_8_single_reverse`).
pub fn decode_valid_utf8_single_reverse(s: &[u8], end: &mut usize, begin: usize) -> u32 {
    let mut p = *end;
    while p > begin {
        p -= 1;
        let c = s[p];
        let len = if c < 0x80 {
            *end = p;
            return u32::from(c);
        } else if c & 0xE0 == 0xC0 {
            2
        } else if c & 0xF0 == 0xE0 {
            3
        } else if c & 0xF8 == 0xF0 {
            4
        } else {
            if *end - p >= 4 {
                break;
            }
            continue;
        };
        if *end - p < len {
            *end = p;
            return DECODE_ERROR;
        }
        let mask = match len {
            2 => 0x1F,
            3 => 0x0F,
            _ => 0x07,
        };
        let mut cp = u32::from(c & mask);
        for &b in &s[p + 1..p + len] {
            cp = (cp << 6) | u32::from(b ^ (0x80 & b));
        }
        *end = p;
        return cp;
    }
    *end = p;
    DECODE_ERROR
}

/// Appends the UTF-8 encoding of `cp` (no validation, like lexbor).
pub fn encode_utf8(cp: u32, out: &mut Vec<u8>) {
    // Truncating casts are the point here: each byte keeps 6 or fewer bits.
    #[allow(clippy::cast_possible_truncation)]
    if cp < 0x80 {
        out.push(cp as u8);
    } else if cp < 0x800 {
        out.push((0xC0 | (cp >> 6)) as u8);
        out.push((0x80 | (cp & 0x3F)) as u8);
    } else if cp < 0x1_0000 {
        out.push((0xE0 | (cp >> 12)) as u8);
        out.push((0x80 | ((cp >> 6) & 0x3F)) as u8);
        out.push((0x80 | (cp & 0x3F)) as u8);
    } else {
        out.push((0xF0 | (cp >> 18)) as u8);
        out.push((0x80 | ((cp >> 12) & 0x3F)) as u8);
        out.push((0x80 | ((cp >> 6) & 0x3F)) as u8);
        out.push((0x80 | (cp & 0x3F)) as u8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_forward_and_back() {
        let s = "aé€😀".as_bytes();
        let mut p = 0;
        let mut v = Vec::new();
        while p < s.len() {
            v.push(decode_valid_utf8_single(s, &mut p));
        }
        assert_eq!(v, vec![0x61, 0xE9, 0x20AC, 0x1F600]);
        let mut e = s.len();
        let mut r = Vec::new();
        while e > 0 {
            r.push(decode_valid_utf8_single_reverse(s, &mut e, 0));
        }
        assert_eq!(r, vec![0x1F600, 0x20AC, 0xE9, 0x61]);
    }

    #[test]
    fn invalid() {
        let mut p = 0;
        assert_eq!(decode_valid_utf8_single(b"\xFF", &mut p), DECODE_ERROR);
        assert_eq!(p, 1);
        let mut p = 0;
        assert_eq!(decode_valid_utf8_single(b"\xC3", &mut p), DECODE_ERROR);
        assert_eq!(p, 1);
    }

    #[test]
    fn encode() {
        let mut out = Vec::new();
        for c in [0x61, 0xE9, 0x20AC, 0x1F600] {
            encode_utf8(c, &mut out);
        }
        assert_eq!(out, "aé€😀".as_bytes());
    }
}

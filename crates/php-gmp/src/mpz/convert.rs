// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! String and binary conversions (`mpz_set_str`, `mpz_get_str`,
//! `mpz_import`, `mpz_export`).

use super::{mag, Mpz};

/// Word order for [`Mpz::import`] / [`Mpz::export`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordOrder {
    /// Most significant word first (`order = 1`).
    MostSignificantFirst,
    /// Least significant word first (`order = -1`).
    LeastSignificantFirst,
}

/// Byte order within a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    /// Most significant byte first.
    Big,
    /// Least significant byte first.
    Little,
    /// Host byte order.
    Native,
}

impl Endian {
    fn is_big(self) -> bool {
        match self {
            Self::Big => true,
            Self::Little => false,
            Self::Native => cfg!(target_endian = "big"),
        }
    }
}

/// C `isspace()` in the "C" locale.
pub(crate) fn is_c_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

/// GMP's `__gmp_digit_value_tab` lookup: 99 marks an invalid character.
fn digit_value(c: u8, base: u32) -> u32 {
    match c {
        b'0'..=b'9' => u32::from(c - b'0'),
        b'A'..=b'Z' => u32::from(c - b'A') + 10,
        b'a'..=b'z' if base > 36 => u32::from(c - b'a') + 36,
        b'a'..=b'z' => u32::from(c - b'a') + 10,
        _ => 99,
    }
}

const LOWER: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const UPPER: &[u8; 36] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const BIG: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Largest power of `base` fitting in a limb, and its exponent.
fn chunk(base: u64) -> (u64, usize) {
    let mut p = base;
    let mut k = 1;
    while let Some(n) = p.checked_mul(base) {
        p = n;
        k += 1;
    }
    (p, k)
}

impl Mpz {
    /// `mpz_set_str` semantics: `base` is 0 (auto-detect `0x`, `0b`, `0`
    /// prefixes) or 2..=62.  Leading whitespace and whitespace between
    /// digits are ignored; the string ends at the first NUL byte.
    /// Returns `None` where GMP returns -1.
    pub fn parse(s: &[u8], base: u32) -> Option<Self> {
        if base == 1 || base > 62 {
            return None;
        }
        let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
        let s = &s[..end];
        let mut i = 0;
        let mut next = || {
            let c = s.get(i).copied().unwrap_or(0);
            i += 1;
            c
        };
        let mut c = next();
        while is_c_space(c) {
            c = next();
        }
        let mut negative = false;
        if c == b'-' {
            negative = true;
            c = next();
        }
        let check_base = if base == 0 { 10 } else { base };
        if digit_value(c, base) >= check_base {
            return None;
        }
        let mut base = base;
        if base == 0 {
            base = 10;
            if c == b'0' {
                base = 8;
                c = next();
                if c == b'x' || c == b'X' {
                    base = 16;
                    c = next();
                } else if c == b'b' || c == b'B' {
                    base = 2;
                    c = next();
                }
            }
        }
        while c == b'0' || is_c_space(c) {
            c = next();
        }
        if c == 0 {
            return Some(Self::zero());
        }
        let mut digits = Vec::new();
        loop {
            if c == 0 {
                break;
            }
            if !is_c_space(c) {
                let d = digit_value(c, base);
                if d >= base {
                    return None;
                }
                digits.push(u8::try_from(d).unwrap_or(0));
            }
            c = next();
        }
        Some(Self::from_parts(negative, digits_to_mag(&digits, base)))
    }

    /// `mpz_get_str` semantics.  `base` is 2..=62 (lower-case digits up to
    /// 36, `0-9A-Za-z` above) or -2..=-36 (upper-case digits).
    ///
    /// # Panics
    /// Panics for any other base.
    pub fn to_str_radix(&self, base: i32) -> String {
        let (abs_base, table): (u32, &[u8]) = match base {
            2..=36 => (base.unsigned_abs(), LOWER),
            -36..=-2 => (base.unsigned_abs(), UPPER),
            37..=62 => (base.unsigned_abs(), BIG),
            _ => panic!("invalid base {base}"),
        };
        let mut out = Vec::new();
        if self.neg {
            out.push(b'-');
        }
        if self.mag.is_empty() {
            out.push(b'0');
        } else {
            let digits = mag_to_digits(&self.mag, abs_base);
            out.extend(digits.iter().map(|d| table[usize::from(*d)]));
        }
        String::from_utf8(out).unwrap_or_default()
    }

    /// `mpz_import` with `nails = 0`.  `data.len()` must be a multiple of
    /// `size`; the result is non-negative.
    pub fn import(data: &[u8], size: usize, order: WordOrder, endian: Endian) -> Self {
        assert!(size > 0 && data.len() % size == 0, "invalid import size");
        let big = endian.is_big();
        let count = data.len() / size;
        // Collect bytes least significant first.
        let mut bytes = Vec::with_capacity(data.len());
        for w in 0..count {
            let word_index = match order {
                WordOrder::LeastSignificantFirst => w,
                WordOrder::MostSignificantFirst => count - 1 - w,
            };
            let word = &data[word_index * size..(word_index + 1) * size];
            if big {
                bytes.extend(word.iter().rev());
            } else {
                bytes.extend_from_slice(word);
            }
        }
        let limbs = bytes
            .chunks(8)
            .map(|c| {
                let mut b = [0u8; 8];
                b[..c.len()].copy_from_slice(c);
                u64::from_le_bytes(b)
            })
            .collect();
        Self::from_parts(false, limbs)
    }

    /// `mpz_export` with `nails = 0` of `|self|`; zero exports as an empty
    /// string.
    pub fn export(&self, size: usize, order: WordOrder, endian: Endian) -> Vec<u8> {
        assert!(size > 0, "invalid export size");
        if self.mag.is_empty() {
            return Vec::new();
        }
        let nbits = self.bit_len();
        let bits_per_word = size as u64 * 8;
        let count = usize::try_from(nbits.div_ceil(bits_per_word)).expect("export too large");
        let mut le: Vec<u8> = self.mag.iter().flat_map(|l| l.to_le_bytes()).collect();
        le.resize(count * size, 0);
        let big = endian.is_big();
        let mut out = Vec::with_capacity(count * size);
        for w in 0..count {
            let word_index = match order {
                WordOrder::LeastSignificantFirst => w,
                WordOrder::MostSignificantFirst => count - 1 - w,
            };
            let word = &le[word_index * size..(word_index + 1) * size];
            if big {
                out.extend(word.iter().rev());
            } else {
                out.extend_from_slice(word);
            }
        }
        out
    }

    /// Number of digits of `|self|` in `base` (2..=62), exact; 1 for zero.
    pub fn size_in_base(&self, base: u32) -> usize {
        if self.mag.is_empty() {
            return 1;
        }
        if base.is_power_of_two() {
            let b = u64::from(base.trailing_zeros());
            return usize::try_from(self.bit_len().div_ceil(b)).unwrap_or(usize::MAX);
        }
        mag_to_digits(&self.mag, base).len()
    }
}

/// Converts base-`base` digit values (most significant first) to limbs.
fn digits_to_mag(digits: &[u8], base: u32) -> Vec<u64> {
    let b = u64::from(base);
    if base.is_power_of_two() {
        let bits = base.trailing_zeros();
        let mut out = vec![0u64; (digits.len() * bits as usize).div_ceil(64)];
        for (i, &d) in digits.iter().rev().enumerate() {
            let pos = i * bits as usize;
            let (li, off) = (pos / 64, pos % 64);
            out[li] |= u64::from(d) << off;
            if off + bits as usize > 64 {
                out[li + 1] |= u64::from(d) >> (64 - off);
            }
        }
        mag::normalize(&mut out);
        return out;
    }
    let (big, k) = chunk(b);
    if digits.len() > 4000 {
        // Divide and conquer: high * base^len(low) + low.
        let split = digits.len() / 2;
        let (hi, lo) = digits.split_at(split);
        let hi_v = digits_to_mag(hi, base);
        let lo_v = digits_to_mag(lo, base);
        let p = mag::pow(&[b], lo.len() as u64);
        return mag::add(&mag::mul(&hi_v, &p), &lo_v);
    }
    let mut out: Vec<u64> = Vec::new();
    let first = digits.len() % k;
    let push = |group: &[u8], mult: u64, out: &mut Vec<u64>| {
        let v = group.iter().fold(0u64, |acc, &d| acc * b + u64::from(d));
        *out = mag::mul_limb_add(out, mult, v);
    };
    if first > 0 {
        push(
            &digits[..first],
            b.pow(u32::try_from(first).unwrap_or(0)),
            &mut out,
        );
    }
    for group in digits[first..].chunks(k) {
        push(group, big, &mut out);
    }
    out
}

/// Converts limbs to base-`base` digit values, most significant first.
fn mag_to_digits(m: &[u64], base: u32) -> Vec<u8> {
    let b = u64::from(base);
    if base.is_power_of_two() {
        let bits = u64::from(base.trailing_zeros());
        let total = mag::bit_len(m);
        let n = total.div_ceil(bits);
        let mut out = Vec::with_capacity(usize::try_from(n).unwrap_or(0));
        for i in (0..n).rev() {
            let pos = i * bits;
            let li = usize::try_from(pos / 64).unwrap_or(usize::MAX);
            let off = pos % 64;
            let mut v = m[li] >> off;
            if off + bits > 64 {
                if let Some(next) = m.get(li + 1) {
                    v |= next << (64 - off);
                }
            }
            out.push(u8::try_from(v & ((1 << bits) - 1)).unwrap_or(0));
        }
        return out;
    }
    let (big, k) = chunk(b);
    if m.len() > 60 {
        // Divide and conquer on a power of the chunk base.
        let half_limbs = m.len() / 2;
        // Find base^e with roughly half_limbs limbs: e = half_limbs * k chunks.
        let e = (half_limbs as u64) * (k as u64);
        let p = mag::pow(&[b], e);
        let (q, r) = mag::divrem(m, &p);
        let mut hi = mag_to_digits(&q, base);
        let lo = mag_to_digits(&r, base);
        let width = usize::try_from(e).unwrap_or(0);
        if q.is_empty() {
            return lo;
        }
        hi.extend(std::iter::repeat_n(0u8, width - lo.len()));
        hi.extend(lo);
        return hi;
    }
    let mut groups = Vec::new();
    let mut cur = m.to_vec();
    while !cur.is_empty() {
        let (q, r) = mag::divrem_limb(&cur, big);
        groups.push(r);
        cur = q;
    }
    let mut out = Vec::new();
    for (gi, g) in groups.iter().rev().enumerate() {
        let mut buf = vec![0u8; k];
        let mut v = *g;
        for slot in buf.iter_mut().rev() {
            *slot = u8::try_from(v % b).unwrap_or(0);
            v /= b;
        }
        if gi == 0 {
            let skip = buf.iter().take_while(|d| **d == 0).count();
            out.extend_from_slice(&buf[skip..]);
        } else {
            out.extend_from_slice(&buf);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rules() {
        let p = |s: &str, b| Mpz::parse(s.as_bytes(), b).map(|v| v.to_string());
        assert_eq!(p("0x1F", 0).as_deref(), Some("31"));
        assert_eq!(p("-0b101", 0).as_deref(), Some("-5"));
        assert_eq!(p("017", 0).as_deref(), Some("15"));
        assert_eq!(p("  1 2 3", 10).as_deref(), Some("123"));
        assert_eq!(p("12a", 10), None);
        assert_eq!(p("", 10), None);
        assert_eq!(p("-", 10), None);
        assert_eq!(p("0x", 0).as_deref(), Some("0"));
        assert_eq!(p("zZ", 62).as_deref(), Some("3817"));
        assert_eq!(p("zZ", 36).as_deref(), Some("1295"));
        assert_eq!(p("08", 0), None);
    }

    #[test]
    fn format_round_trip() {
        let v = Mpz::parse(b"-123456789012345678901234567890123456789", 10).unwrap();
        for base in [2, 3, 7, 10, 16, 36, 37, 62, -16, -36] {
            let s = v.to_str_radix(base);
            let back = Mpz::parse(s.as_bytes(), base.unsigned_abs()).unwrap();
            assert_eq!(back, v, "base {base}");
        }
        assert_eq!(Mpz::from(255).to_str_radix(-16), "FF");
        assert_eq!(Mpz::from(255).to_str_radix(16), "ff");
        assert_eq!(Mpz::from(61).to_str_radix(62), "z");
        assert_eq!(Mpz::zero().to_str_radix(2), "0");
    }

    #[test]
    fn big_round_trip() {
        let v = Mpz::from(7).pow(30_000);
        let s = v.to_str_radix(10);
        assert_eq!(Mpz::parse(s.as_bytes(), 10).unwrap(), v);
        assert_eq!(s.len(), v.size_in_base(10));
        let s3 = v.to_str_radix(3);
        assert_eq!(Mpz::parse(s3.as_bytes(), 3).unwrap(), v);
    }

    #[test]
    fn import_export() {
        let v = Mpz::import(
            &[1, 2, 3, 4],
            2,
            WordOrder::MostSignificantFirst,
            Endian::Big,
        );
        assert_eq!(v, Mpz::from(0x0102_0304));
        assert_eq!(
            v.export(2, WordOrder::MostSignificantFirst, Endian::Big),
            vec![1, 2, 3, 4]
        );
        let w = Mpz::import(
            &[1, 2, 3, 4],
            2,
            WordOrder::LeastSignificantFirst,
            Endian::Little,
        );
        assert_eq!(w, Mpz::from(0x0403_0201));
        assert_eq!(
            Mpz::from(0x0102_0304).export(3, WordOrder::MostSignificantFirst, Endian::Big),
            vec![0, 0, 1, 2, 3, 4]
        );
    }
}

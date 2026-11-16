// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! bcrypt (`$2a$`, `$2b$`, `$2x$`, `$2y$`), ported from
//! `ext/standard/crypt_blowfish.c` (Solar Designer's `crypt_blowfish` 1.3),
//! including the `$2x$` sign-extension bug emulation and the `$2a$`
//! anti-collision safety measure.

// Cryptographic code ported from C: single-letter state variables, fixed-width
// truncating casts and indexed loops mirror the reference implementation.
#![allow(
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    clippy::needless_range_loop,
    clippy::explicit_counter_loop,
    clippy::trivially_copy_pass_by_ref
)]

use super::bf_tables::{INIT_P, INIT_S};

const BF_N: usize = 16;

const BF_ITOA64: &[u8; 64] = b"./ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

const BF_ATOI64: [u8; 0x60] = [
    64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 0, 1, 54, 55, 56, 57, 58, 59, 60, 61,
    62, 63, 64, 64, 64, 64, 64, 64, 64, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
    19, 20, 21, 22, 23, 24, 25, 26, 27, 64, 64, 64, 64, 64, 64, 28, 29, 30, 31, 32, 33, 34, 35, 36,
    37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 64, 64, 64, 64, 64,
];

/// `"OrpheanBeholderScryDoubt"` as big-endian words.
const BF_MAGIC_W: [u32; 6] = [
    0x4F72_7068,
    0x6561_6E42,
    0x6568_6F6C,
    0x6465_7253,
    0x6372_7944,
    0x6F75_6274,
];

/// `flags_by_subtype['a'..='z']`: bit 0 = bug emulation, bit 1 = safety,
/// non-zero = supported.
const FLAGS_BY_SUBTYPE: [u8; 26] = [
    2, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 4, 0,
];

fn subtype_flags(c: u8) -> u8 {
    if c.is_ascii_lowercase() {
        FLAGS_BY_SUBTYPE[usize::from(c - b'a')]
    } else {
        0
    }
}

struct Ctx {
    s: [[u32; 256]; 4],
    p: [u32; BF_N + 2],
}

impl Ctx {
    #[inline]
    fn f(&self, x: u32) -> u32 {
        let [a, b, c, d] = x.to_be_bytes();
        ((self.s[0][usize::from(a)].wrapping_add(self.s[1][usize::from(b)]))
            ^ self.s[2][usize::from(c)])
        .wrapping_add(self.s[3][usize::from(d)])
    }

    #[inline]
    fn encrypt(&self, mut l: u32, mut r: u32) -> (u32, u32) {
        l ^= self.p[0];
        let mut i = 0;
        while i < BF_N {
            r ^= self.f(l) ^ self.p[i + 1];
            l ^= self.f(r) ^ self.p[i + 2];
            i += 2;
        }
        (r ^ self.p[BF_N + 1], l)
    }

    /// `BF_body()`: re-key the whole state by encrypting a running block.
    fn body(&mut self) {
        let (mut l, mut r) = (0u32, 0u32);
        let mut i = 0;
        while i < BF_N + 2 {
            (l, r) = self.encrypt(l, r);
            self.p[i] = l;
            self.p[i + 1] = r;
            i += 2;
        }
        for b in 0..4 {
            let mut j = 0;
            while j < 256 {
                (l, r) = self.encrypt(l, r);
                self.s[b][j] = l;
                self.s[b][j + 1] = r;
                j += 2;
            }
        }
    }
}

/// `BF_decode()` of 16 bytes from 22 characters; `None` on an invalid char.
fn bf_decode(src: &[u8]) -> Option<[u8; 16]> {
    let get = |i: usize| -> Option<u32> {
        let c = src.get(i).copied().unwrap_or(0);
        let t = c.wrapping_sub(0x20);
        if t >= 0x60 {
            return None;
        }
        let v = BF_ATOI64[usize::from(t)];
        (v <= 63).then_some(u32::from(v))
    };
    let mut out = [0u8; 16];
    let mut d = 0;
    let mut s = 0;
    loop {
        let c1 = get(s)?;
        let c2 = get(s + 1)?;
        s += 2;
        out[d] = ((c1 << 2) | ((c2 & 0x30) >> 4)) as u8;
        d += 1;
        if d >= 16 {
            break;
        }
        let c3 = get(s)?;
        s += 1;
        out[d] = (((c2 & 0x0F) << 4) | ((c3 & 0x3C) >> 2)) as u8;
        d += 1;
        if d >= 16 {
            break;
        }
        let c4 = get(s)?;
        s += 1;
        out[d] = (((c3 & 0x03) << 6) | c4) as u8;
        d += 1;
        if d >= 16 {
            break;
        }
    }
    Some(out)
}

/// `BF_encode()`.
fn bf_encode(dst: &mut Vec<u8>, src: &[u8]) {
    let mut i = 0;
    while i < src.len() {
        let c1 = u32::from(src[i]);
        i += 1;
        dst.push(BF_ITOA64[(c1 >> 2) as usize]);
        let mut c1 = (c1 & 0x03) << 4;
        if i >= src.len() {
            dst.push(BF_ITOA64[c1 as usize]);
            break;
        }
        let c2 = u32::from(src[i]);
        i += 1;
        c1 |= c2 >> 4;
        dst.push(BF_ITOA64[c1 as usize]);
        c1 = (c2 & 0x0f) << 2;
        if i >= src.len() {
            dst.push(BF_ITOA64[c1 as usize]);
            break;
        }
        let c2 = u32::from(src[i]);
        i += 1;
        c1 |= c2 >> 6;
        dst.push(BF_ITOA64[c1 as usize]);
        dst.push(BF_ITOA64[(c2 & 0x3f) as usize]);
    }
}

/// `BF_set_key()`: returns `(expanded, initial)`.
fn set_key(key: &[u8], flags: u8) -> ([u32; BF_N + 2], [u32; BF_N + 2]) {
    let bug = usize::from(flags & 1);
    let safety = (u32::from(flags) & 2) << 15;
    let mut sign = 0u32;
    let mut diff = 0u32;
    let mut expanded = [0u32; BF_N + 2];
    let mut initial = [0u32; BF_N + 2];
    // The key is a C string: it is read cyclically including its NUL.
    let mut ptr = 0usize;
    let byte = |i: usize| key.get(i).copied().unwrap_or(0);
    for i in 0..BF_N + 2 {
        let mut tmp = [0u32; 2];
        for j in 0..4 {
            let c = byte(ptr);
            tmp[0] = (tmp[0] << 8) | u32::from(c);
            #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
            let ext = i32::from(c as i8) as u32;
            tmp[1] = (tmp[1] << 8) | ext;
            if j != 0 {
                sign |= tmp[1] & 0x80;
            }
            if c == 0 {
                ptr = 0;
            } else {
                ptr += 1;
            }
        }
        diff |= tmp[0] ^ tmp[1];
        expanded[i] = tmp[bug];
        initial[i] = INIT_P[i] ^ tmp[bug];
    }
    diff |= diff >> 16;
    diff &= 0xffff;
    diff = diff.wrapping_add(0xffff);
    sign <<= 9;
    sign &= !diff & safety;
    initial[0] ^= sign;
    (expanded, initial)
}

/// `BF_crypt()` with `min` the minimal iteration count (16 = cost 4).
fn bf_crypt(key: &[u8], setting: &[u8], min: u32) -> Option<Vec<u8>> {
    let at = |i: usize| setting.get(i).copied().unwrap_or(0);
    let flags = subtype_flags(at(2));
    if at(0) != b'$'
        || at(1) != b'2'
        || flags == 0
        || at(3) != b'$'
        || !(b'0'..=b'3').contains(&at(4))
        || !at(5).is_ascii_digit()
        || (at(4) == b'3' && at(5) > b'1')
        || at(6) != b'$'
    {
        return None;
    }
    let log = u32::from(at(4) - b'0') * 10 + u32::from(at(5) - b'0');
    let count0 = 1u32 << log;
    if count0 < min {
        return None;
    }
    let salt_bytes = bf_decode(setting.get(7..).unwrap_or(&[]))?;
    let mut salt = [0u32; 4];
    for (i, w) in salt.iter_mut().enumerate() {
        *w = u32::from_be_bytes([
            salt_bytes[4 * i],
            salt_bytes[4 * i + 1],
            salt_bytes[4 * i + 2],
            salt_bytes[4 * i + 3],
        ]);
    }

    let (expanded, initial) = set_key(key, flags);
    let mut ctx = Ctx {
        s: INIT_S,
        p: initial,
    };

    let (mut l, mut r) = (0u32, 0u32);
    let mut i = 0;
    while i < BF_N + 2 {
        l ^= salt[i & 2];
        r ^= salt[(i & 2) + 1];
        (l, r) = ctx.encrypt(l, r);
        ctx.p[i] = l;
        ctx.p[i + 1] = r;
        i += 2;
    }
    for b in 0..4 {
        let mut j = 0;
        while j < 256 {
            l ^= salt[(BF_N + 2) & 3];
            r ^= salt[(BF_N + 3) & 3];
            (l, r) = ctx.encrypt(l, r);
            ctx.s[b][j] = l;
            ctx.s[b][j + 1] = r;
            l ^= salt[(BF_N + 4) & 3];
            r ^= salt[(BF_N + 5) & 3];
            (l, r) = ctx.encrypt(l, r);
            ctx.s[b][j + 2] = l;
            ctx.s[b][j + 3] = r;
            j += 4;
        }
    }

    let mut count = count0;
    loop {
        for (p, k) in ctx.p.iter_mut().zip(expanded.iter()) {
            *p ^= *k;
        }
        ctx.body();
        let mut i = 0;
        while i < BF_N {
            ctx.p[i] ^= salt[0];
            ctx.p[i + 1] ^= salt[1];
            ctx.p[i + 2] ^= salt[2];
            ctx.p[i + 3] ^= salt[3];
            i += 4;
        }
        ctx.p[16] ^= salt[0];
        ctx.p[17] ^= salt[1];
        ctx.body();
        count -= 1;
        if count == 0 {
            break;
        }
    }

    let mut out_words = [0u32; 6];
    let mut i = 0;
    while i < 6 {
        let (mut l, mut r) = (BF_MAGIC_W[i], BF_MAGIC_W[i + 1]);
        for _ in 0..64 {
            (l, r) = ctx.encrypt(l, r);
        }
        out_words[i] = l;
        out_words[i + 1] = r;
        i += 2;
    }

    let mut output = Vec::with_capacity(60);
    output.extend_from_slice(&setting[..7 + 22 - 1]);
    let last = at(7 + 22 - 1);
    // Validated by bf_decode above, so the index is in range.
    let v = BF_ATOI64[usize::from(last.wrapping_sub(0x20) & 0x7f).min(0x5f)];
    output.push(BF_ITOA64[usize::from(v & 0x30)]);
    let mut bytes = [0u8; 24];
    for (i, w) in out_words.iter().enumerate() {
        bytes[4 * i..4 * i + 4].copy_from_slice(&w.to_be_bytes());
    }
    // Bug-compatible with the original: only 23 of the 24 bytes are encoded.
    bf_encode(&mut output, &bytes[..23]);
    Some(output)
}

/// `php_crypt_blowfish_rn()`: hashes the C-string `key` with the bcrypt
/// `setting` (`$2?$NN$` + 22 salt characters). Returns `None` for an
/// invalid setting.
#[must_use]
pub fn crypt_blowfish(key: &[u8], setting: &[u8]) -> Option<Vec<u8>> {
    bf_crypt(key, setting, 16)
}

#[cfg(test)]
mod tests {
    use super::{bf_crypt, set_key};

    #[test]
    fn self_test() {
        let key = b"8b \xd0\xc1\xd2\xcf\xcc\xd8";
        for (sub, expect) in [
            (b'a', &b"i1D709vfamulimlGcq0qq3UvuUasvEa"[..]),
            (b'b', b"i1D709vfamulimlGcq0qq3UvuUasvEa"),
            (b'y', b"i1D709vfamulimlGcq0qq3UvuUasvEa"),
            (b'x', b"VUrPmXD6q/nVSSp7pNDhCR9071IfIRe"),
        ] {
            let mut setting = b"$2a$00$abcdefghijklmnopqrstuu".to_vec();
            setting[2] = sub;
            let out = bf_crypt(key, &setting, 1).unwrap();
            assert_eq!(&out[..29], &setting[..29]);
            assert_eq!(&out[29..], expect);
        }
        let k = b"\xff\xa334\xff\xff\xff\xa3345";
        let (ae, mut ai) = set_key(k, 2);
        let (ye, yi) = set_key(k, 4);
        ai[0] ^= 0x10000;
        assert_eq!(ai[0], 0xdb9c_59bc);
        assert_eq!(ye[17], 0x3334_3500);
        assert_eq!(ae, ye);
        assert_eq!(ai, yi);
    }
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Traditional and `BSDi`-extended DES-based `crypt()`, ported from
//! `ext/standard/crypt_freesec.c` (`FreeSec` by David Burren, with Solar
//! Designer's fixes).
//!
//! * "old" style: a 2-character salt, the first 8 key bytes, 25 iterations;
//! * "new" style (`_` prefix): 4 characters of iteration count, 4 of salt and
//!   an unlimited key folded in 8-byte blocks.

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

use std::sync::OnceLock;

const IP: [u8; 64] = [
    58, 50, 42, 34, 26, 18, 10, 2, 60, 52, 44, 36, 28, 20, 12, 4, 62, 54, 46, 38, 30, 22, 14, 6,
    64, 56, 48, 40, 32, 24, 16, 8, 57, 49, 41, 33, 25, 17, 9, 1, 59, 51, 43, 35, 27, 19, 11, 3, 61,
    53, 45, 37, 29, 21, 13, 5, 63, 55, 47, 39, 31, 23, 15, 7,
];

const KEY_PERM: [u8; 56] = [
    57, 49, 41, 33, 25, 17, 9, 1, 58, 50, 42, 34, 26, 18, 10, 2, 59, 51, 43, 35, 27, 19, 11, 3, 60,
    52, 44, 36, 63, 55, 47, 39, 31, 23, 15, 7, 62, 54, 46, 38, 30, 22, 14, 6, 61, 53, 45, 37, 29,
    21, 13, 5, 28, 20, 12, 4,
];

const KEY_SHIFTS: [u8; 16] = [1, 1, 2, 2, 2, 2, 2, 2, 1, 2, 2, 2, 2, 2, 2, 1];

const COMP_PERM: [u8; 48] = [
    14, 17, 11, 24, 1, 5, 3, 28, 15, 6, 21, 10, 23, 19, 12, 4, 26, 8, 16, 7, 27, 20, 13, 2, 41, 52,
    31, 37, 47, 55, 30, 40, 51, 45, 33, 48, 44, 49, 39, 56, 34, 53, 46, 42, 50, 36, 29, 32,
];

const SBOX: [[u8; 64]; 8] = [
    [
        14, 4, 13, 1, 2, 15, 11, 8, 3, 10, 6, 12, 5, 9, 0, 7, 0, 15, 7, 4, 14, 2, 13, 1, 10, 6, 12,
        11, 9, 5, 3, 8, 4, 1, 14, 8, 13, 6, 2, 11, 15, 12, 9, 7, 3, 10, 5, 0, 15, 12, 8, 2, 4, 9,
        1, 7, 5, 11, 3, 14, 10, 0, 6, 13,
    ],
    [
        15, 1, 8, 14, 6, 11, 3, 4, 9, 7, 2, 13, 12, 0, 5, 10, 3, 13, 4, 7, 15, 2, 8, 14, 12, 0, 1,
        10, 6, 9, 11, 5, 0, 14, 7, 11, 10, 4, 13, 1, 5, 8, 12, 6, 9, 3, 2, 15, 13, 8, 10, 1, 3, 15,
        4, 2, 11, 6, 7, 12, 0, 5, 14, 9,
    ],
    [
        10, 0, 9, 14, 6, 3, 15, 5, 1, 13, 12, 7, 11, 4, 2, 8, 13, 7, 0, 9, 3, 4, 6, 10, 2, 8, 5,
        14, 12, 11, 15, 1, 13, 6, 4, 9, 8, 15, 3, 0, 11, 1, 2, 12, 5, 10, 14, 7, 1, 10, 13, 0, 6,
        9, 8, 7, 4, 15, 14, 3, 11, 5, 2, 12,
    ],
    [
        7, 13, 14, 3, 0, 6, 9, 10, 1, 2, 8, 5, 11, 12, 4, 15, 13, 8, 11, 5, 6, 15, 0, 3, 4, 7, 2,
        12, 1, 10, 14, 9, 10, 6, 9, 0, 12, 11, 7, 13, 15, 1, 3, 14, 5, 2, 8, 4, 3, 15, 0, 6, 10, 1,
        13, 8, 9, 4, 5, 11, 12, 7, 2, 14,
    ],
    [
        2, 12, 4, 1, 7, 10, 11, 6, 8, 5, 3, 15, 13, 0, 14, 9, 14, 11, 2, 12, 4, 7, 13, 1, 5, 0, 15,
        10, 3, 9, 8, 6, 4, 2, 1, 11, 10, 13, 7, 8, 15, 9, 12, 5, 6, 3, 0, 14, 11, 8, 12, 7, 1, 14,
        2, 13, 6, 15, 0, 9, 10, 4, 5, 3,
    ],
    [
        12, 1, 10, 15, 9, 2, 6, 8, 0, 13, 3, 4, 14, 7, 5, 11, 10, 15, 4, 2, 7, 12, 9, 5, 6, 1, 13,
        14, 0, 11, 3, 8, 9, 14, 15, 5, 2, 8, 12, 3, 7, 0, 4, 10, 1, 13, 11, 6, 4, 3, 2, 12, 9, 5,
        15, 10, 11, 14, 1, 7, 6, 0, 8, 13,
    ],
    [
        4, 11, 2, 14, 15, 0, 8, 13, 3, 12, 9, 7, 5, 10, 6, 1, 13, 0, 11, 7, 4, 9, 1, 10, 14, 3, 5,
        12, 2, 15, 8, 6, 1, 4, 11, 13, 12, 3, 7, 14, 10, 15, 6, 8, 0, 5, 9, 2, 6, 11, 13, 8, 1, 4,
        10, 7, 9, 5, 0, 15, 14, 2, 3, 12,
    ],
    [
        13, 2, 8, 4, 6, 15, 11, 1, 10, 9, 3, 14, 5, 0, 12, 7, 1, 15, 13, 8, 10, 3, 7, 4, 12, 5, 6,
        11, 0, 14, 9, 2, 7, 11, 4, 1, 9, 12, 14, 2, 0, 6, 10, 13, 15, 3, 5, 8, 2, 1, 14, 7, 4, 10,
        8, 13, 15, 12, 9, 0, 3, 5, 6, 11,
    ],
];

const PBOX: [u8; 32] = [
    16, 7, 20, 21, 29, 12, 28, 17, 1, 15, 23, 26, 5, 18, 31, 10, 2, 8, 24, 14, 32, 27, 3, 9, 19,
    13, 30, 6, 22, 11, 4, 25,
];

/// The crypt(3) base-64 alphabet.
pub(crate) const ASCII64: &[u8; 64] =
    b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

const fn bits32(i: usize) -> u32 {
    0x8000_0000 >> i
}

const fn bits8(i: usize) -> u32 {
    0x80 >> i
}

/// The precomputed lookup tables of `_crypt_extended_init()`.
struct Tables {
    m_sbox: Box<[[u8; 4096]; 4]>,
    psbox: [[u32; 256]; 4],
    ip_maskl: [[u32; 256]; 8],
    ip_maskr: [[u32; 256]; 8],
    fp_maskl: [[u32; 256]; 8],
    fp_maskr: [[u32; 256]; 8],
    key_perm_maskl: [[u32; 128]; 8],
    key_perm_maskr: [[u32; 128]; 8],
    comp_maskl: [[u32; 128]; 8],
    comp_maskr: [[u32; 128]; 8],
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(build_tables)
}

#[allow(clippy::too_many_lines)]
fn build_tables() -> Tables {
    let mut u_sbox = [[0u8; 64]; 8];
    for (i, row) in u_sbox.iter_mut().enumerate() {
        for (j, slot) in row.iter_mut().enumerate() {
            let b = (j & 0x20) | ((j & 1) << 4) | ((j >> 1) & 0xf);
            *slot = SBOX[i][b];
        }
    }
    let mut m_sbox = Box::new([[0u8; 4096]; 4]);
    for (b, table) in m_sbox.iter_mut().enumerate() {
        for i in 0..64 {
            for j in 0..64 {
                table[(i << 6) | j] = (u_sbox[b << 1][i] << 4) | u_sbox[(b << 1) + 1][j];
            }
        }
    }

    let mut init_perm = [0u8; 64];
    let mut final_perm = [0u8; 64];
    let mut inv_key_perm = [255u8; 64];
    let mut inv_comp_perm = [255u8; 56];
    for i in 0..64 {
        final_perm[i] = IP[i] - 1;
        init_perm[usize::from(final_perm[i])] = u8::try_from(i).unwrap_or(0);
    }
    for (i, &k) in KEY_PERM.iter().enumerate() {
        inv_key_perm[usize::from(k - 1)] = u8::try_from(i).unwrap_or(0);
    }
    for (i, &c) in COMP_PERM.iter().enumerate() {
        inv_comp_perm[usize::from(c - 1)] = u8::try_from(i).unwrap_or(0);
    }

    let mut t = Tables {
        m_sbox,
        psbox: [[0; 256]; 4],
        ip_maskl: [[0; 256]; 8],
        ip_maskr: [[0; 256]; 8],
        fp_maskl: [[0; 256]; 8],
        fp_maskr: [[0; 256]; 8],
        key_perm_maskl: [[0; 128]; 8],
        key_perm_maskr: [[0; 128]; 8],
        comp_maskl: [[0; 128]; 8],
        comp_maskr: [[0; 128]; 8],
    };
    // bits28 = bits32 + 4, bits24 = bits28 + 4.
    let bits28 = |i: usize| bits32(i + 4);
    let bits24 = |i: usize| bits32(i + 8);
    for k in 0..8 {
        for i in 0..256u32 {
            let (mut il, mut ir, mut fl, mut fr) = (0u32, 0u32, 0u32, 0u32);
            for j in 0..8 {
                let inbit = 8 * k + j;
                if i & bits8(j) != 0 {
                    let obit = usize::from(init_perm[inbit]);
                    if obit < 32 {
                        il |= bits32(obit);
                    } else {
                        ir |= bits32(obit - 32);
                    }
                    let obit = usize::from(final_perm[inbit]);
                    if obit < 32 {
                        fl |= bits32(obit);
                    } else {
                        fr |= bits32(obit - 32);
                    }
                }
            }
            t.ip_maskl[k][i as usize] = il;
            t.ip_maskr[k][i as usize] = ir;
            t.fp_maskl[k][i as usize] = fl;
            t.fp_maskr[k][i as usize] = fr;
        }
        for i in 0..128u32 {
            let (mut il, mut ir) = (0u32, 0u32);
            for j in 0..7 {
                let inbit = 8 * k + j;
                if i & bits8(j + 1) != 0 {
                    let obit = inv_key_perm[inbit];
                    if obit == 255 {
                        continue;
                    }
                    let obit = usize::from(obit);
                    if obit < 28 {
                        il |= bits28(obit);
                    } else {
                        ir |= bits28(obit - 28);
                    }
                }
            }
            t.key_perm_maskl[k][i as usize] = il;
            t.key_perm_maskr[k][i as usize] = ir;
            let (mut il, mut ir) = (0u32, 0u32);
            for j in 0..7 {
                let inbit = 7 * k + j;
                if i & bits8(j + 1) != 0 {
                    let obit = inv_comp_perm[inbit];
                    if obit == 255 {
                        continue;
                    }
                    let obit = usize::from(obit);
                    if obit < 24 {
                        il |= bits24(obit);
                    } else {
                        ir |= bits24(obit - 24);
                    }
                }
            }
            t.comp_maskl[k][i as usize] = il;
            t.comp_maskr[k][i as usize] = ir;
        }
    }

    let mut un_pbox = [0usize; 32];
    for (i, &p) in PBOX.iter().enumerate() {
        un_pbox[usize::from(p - 1)] = i;
    }
    for b in 0..4 {
        for i in 0..256u32 {
            let mut p = 0;
            for j in 0..8 {
                if i & bits8(j) != 0 {
                    p |= bits32(un_pbox[8 * b + j]);
                }
            }
            t.psbox[b][i as usize] = p;
        }
    }
    t
}

/// `ascii_to_bin()`: maps a salt character to 0..63 (with the original's
/// forgiving behaviour for out-of-alphabet characters).
pub(crate) fn ascii_to_bin(ch: u8) -> u32 {
    #[allow(clippy::cast_possible_wrap)]
    let sch = i32::from(ch as i8);
    let mut retval = sch - i32::from(b'.');
    if sch >= i32::from(b'A') {
        retval = sch - (i32::from(b'A') - 12);
        if sch >= i32::from(b'a') {
            retval = sch - (i32::from(b'a') - 38);
        }
    }
    #[allow(clippy::cast_sign_loss)]
    let r = (retval & 0x3f) as u32;
    r
}

fn ascii_is_unsafe(ch: u8) -> bool {
    ch == 0 || ch == b'\n' || ch == b':'
}

/// The per-call DES state (`struct php_crypt_extended_data`).
struct Des {
    saltbits: u32,
    en_keysl: [u32; 16],
    en_keysr: [u32; 16],
}

impl Des {
    fn new() -> Self {
        Des {
            saltbits: 0,
            en_keysl: [0; 16],
            en_keysr: [0; 16],
        }
    }

    fn setup_salt(&mut self, salt: u32) {
        let mut saltbits = 0;
        let mut saltbit = 1u32;
        let mut obit = 0x0080_0000u32;
        for _ in 0..24 {
            if salt & saltbit != 0 {
                saltbits |= obit;
            }
            saltbit <<= 1;
            obit >>= 1;
        }
        self.saltbits = saltbits;
    }

    fn setkey(&mut self, key: &[u8; 8]) {
        let t = tables();
        let rawkey0 = u32::from_be_bytes([key[0], key[1], key[2], key[3]]);
        let rawkey1 = u32::from_be_bytes([key[4], key[5], key[6], key[7]]);
        let idx = |r: u32, sh: u32| ((r >> sh) & 0x7f) as usize;
        let perm = |m: &[[u32; 128]; 8]| {
            m[0][(rawkey0 >> 25) as usize]
                | m[1][idx(rawkey0, 17)]
                | m[2][idx(rawkey0, 9)]
                | m[3][idx(rawkey0, 1)]
                | m[4][(rawkey1 >> 25) as usize]
                | m[5][idx(rawkey1, 17)]
                | m[6][idx(rawkey1, 9)]
                | m[7][idx(rawkey1, 1)]
        };
        let k0 = perm(&t.key_perm_maskl);
        let k1 = perm(&t.key_perm_maskr);
        let mut shifts = 0u32;
        for round in 0..16 {
            shifts += u32::from(KEY_SHIFTS[round]);
            let t0 = (k0 << shifts) | (k0 >> (28 - shifts));
            let t1 = (k1 << shifts) | (k1 >> (28 - shifts));
            let comp = |m: &[[u32; 128]; 8]| {
                m[0][idx(t0, 21)]
                    | m[1][idx(t0, 14)]
                    | m[2][idx(t0, 7)]
                    | m[3][idx(t0, 0)]
                    | m[4][idx(t1, 21)]
                    | m[5][idx(t1, 14)]
                    | m[6][idx(t1, 7)]
                    | m[7][idx(t1, 0)]
            };
            self.en_keysl[round] = comp(&t.comp_maskl);
            self.en_keysr[round] = comp(&t.comp_maskr);
        }
    }

    /// `do_des()` in encryption direction (`count > 0`).
    fn do_des(&self, l_in: u32, r_in: u32, count: u32) -> (u32, u32) {
        let t = tables();
        let b = |v: u32, sh: u32| ((v >> sh) & 0xff) as usize;
        let perm8 = |m: &[[u32; 256]; 8], a: u32, c: u32| {
            m[0][b(a, 24)]
                | m[1][b(a, 16)]
                | m[2][b(a, 8)]
                | m[3][b(a, 0)]
                | m[4][b(c, 24)]
                | m[5][b(c, 16)]
                | m[6][b(c, 8)]
                | m[7][b(c, 0)]
        };
        let mut l = perm8(&t.ip_maskl, l_in, r_in);
        let mut r = perm8(&t.ip_maskr, l_in, r_in);
        let saltbits = self.saltbits;
        let mut f = 0u32;
        for _ in 0..count {
            for round in 0..16 {
                let mut r48l = ((r & 0x0000_0001) << 23)
                    | ((r & 0xf800_0000) >> 9)
                    | ((r & 0x1f80_0000) >> 11)
                    | ((r & 0x01f8_0000) >> 13)
                    | ((r & 0x001f_8000) >> 15);
                let mut r48r = ((r & 0x0001_f800) << 7)
                    | ((r & 0x0000_1f80) << 5)
                    | ((r & 0x0000_01f8) << 3)
                    | ((r & 0x0000_001f) << 1)
                    | ((r & 0x8000_0000) >> 31);
                f = (r48l ^ r48r) & saltbits;
                r48l ^= f ^ self.en_keysl[round];
                r48r ^= f ^ self.en_keysr[round];
                f = t.psbox[0][usize::from(t.m_sbox[0][(r48l >> 12) as usize])]
                    | t.psbox[1][usize::from(t.m_sbox[1][(r48l & 0xfff) as usize])]
                    | t.psbox[2][usize::from(t.m_sbox[2][(r48r >> 12) as usize])]
                    | t.psbox[3][usize::from(t.m_sbox[3][(r48r & 0xfff) as usize])];
                f ^= l;
                l = r;
                r = f;
            }
            r = l;
            l = f;
        }
        (perm8(&t.fp_maskl, l, r), perm8(&t.fp_maskr, l, r))
    }

    /// `des_cipher()` with `count = 1`.
    fn cipher(&mut self, block: &[u8; 8], salt: u32) -> [u8; 8] {
        self.setup_salt(salt);
        let rawl = u32::from_be_bytes([block[0], block[1], block[2], block[3]]);
        let rawr = u32::from_be_bytes([block[4], block[5], block[6], block[7]]);
        let (l, r) = self.do_des(rawl, rawr, 1);
        let mut out = [0u8; 8];
        out[..4].copy_from_slice(&l.to_be_bytes());
        out[4..].copy_from_slice(&r.to_be_bytes());
        out
    }
}

/// Reads a byte of a C string, yielding NUL past its end.
fn at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

/// `_crypt_extended_r()`: `key` and `setting` are C strings (no NULs;
/// anything after a NUL must already be cut off). Returns `None` on an
/// invalid setting.
#[must_use]
pub fn crypt_extended(key: &[u8], setting: &[u8]) -> Option<Vec<u8>> {
    let mut des = Des::new();
    let mut keybuf = [0u8; 8];
    let mut kp = 0usize;
    for slot in &mut keybuf {
        let c = at(key, kp);
        *slot = c << 1;
        if c != 0 {
            kp += 1;
        }
    }
    des.setkey(&keybuf);

    let mut output = Vec::with_capacity(21);
    let (count, salt) = if at(setting, 0) == b'_' {
        let mut count = 0u32;
        for i in 1..5 {
            let value = ascii_to_bin(at(setting, i));
            if ASCII64[value as usize] != at(setting, i) {
                return None;
            }
            count |= value << ((i - 1) * 6);
        }
        if count == 0 {
            return None;
        }
        let mut salt = 0u32;
        for i in 5..9 {
            let value = ascii_to_bin(at(setting, i));
            if ASCII64[value as usize] != at(setting, i) {
                return None;
            }
            salt |= value << ((i - 5) * 6);
        }
        while at(key, kp) != 0 {
            keybuf = des.cipher(&keybuf, 0);
            let mut q = 0;
            while q < 8 && at(key, kp) != 0 {
                keybuf[q] ^= at(key, kp) << 1;
                q += 1;
                kp += 1;
            }
            des.setkey(&keybuf);
        }
        output.extend_from_slice(&setting[..9]);
        (count, salt)
    } else {
        let (s0, s1) = (at(setting, 0), at(setting, 1));
        if ascii_is_unsafe(s0) || ascii_is_unsafe(s1) {
            return None;
        }
        output.push(s0);
        output.push(s1);
        (25, (ascii_to_bin(s1) << 6) | ascii_to_bin(s0))
    };
    des.setup_salt(salt);
    let (r0, r1) = des.do_des(0, 0, count);

    let mut put = |l: u32, shifts: &[u32]| {
        for &s in shifts {
            output.push(ASCII64[((l >> s) & 0x3f) as usize]);
        }
    };
    put(r0 >> 8, &[18, 12, 6, 0]);
    put((r0 << 16) | ((r1 >> 16) & 0xffff), &[18, 12, 6, 0]);
    put(r1 << 2, &[12, 6, 0]);
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::crypt_extended;

    /// The self-test vectors of `crypt_freesec.c`.
    #[test]
    fn freesec_vectors() {
        let cases: &[(&[u8], &[u8])] = &[
            (b"_J9..CCCCXBrJUJV154M", b"U*U*U*U*"),
            (b"_J9..CCCCXUhOBTXzaiE", b"U*U***U"),
            (b"_J9..CCCC4gQ.mB/PffM", b"U*U***U*"),
            (b"_J9..XXXXvlzQGqpPPdk", b"*U*U*U*U"),
            (b"_J9..XXXXsqM/YSSP..Y", b"*U*U*U*U*"),
            (b"_J9..XXXXVL7qJCnku0I", b"*U*U*U*U*U*U*U*U"),
            (b"_J9..XXXXAj8cFbP5scI", b"*U*U*U*U*U*U*U*U*"),
            (b"_J9..SDizh.vll5VED9g", b"ab1234567"),
            (b"_J9..SDizRjWQ/zePPHc", b"cr1234567"),
            (b"_J9..SDizxmRI1GjnQuE", b"zxyDPWgydbQjgq"),
            (b"_K9..SaltNrQgIYUAeoY", b"726 even"),
            (b"_J9..SDSD5YGyRCr4W4c", b""),
            (b"CCNf8Sbh3HDfQ", b"U*U*U*U*"),
            (b"CCX.K.MFy4Ois", b"U*U***U"),
            (b"CC4rMpbg9AMZ.", b"U*U***U*"),
            (b"XXxzOu6maQKqQ", b"*U*U*U*U"),
            (b"SDbsugeBiC58A", b""),
            (b"./xZjzHv5vzVE", b"password"),
            (b"0A2hXM1rXbYgo", b"password"),
            (b"A9RXdR23Y.cY6", b"password"),
            (b"ZziFATVXHo2.6", b"password"),
            (b"zZDDIZ0NOlPzw", b"password"),
            (b"\x01\x02wyd0KZo65Jo", b"password"),
            (b"a_C10Dk/ExaG.", b"password"),
            (b"~\xff.5OTsRVjwLo", b"password"),
        ];
        for (hash, pw) in cases {
            assert_eq!(crypt_extended(pw, hash).as_deref(), Some(*hash));
        }
        for bad in [
            &b""[..],
            b" ",
            b"a:",
            b"\na",
            b"_/......",
            b"_........",
            b"_/!......",
            b"_/......!",
        ] {
            assert_eq!(crypt_extended(b"", bad), None, "{bad:?}");
        }
    }
}

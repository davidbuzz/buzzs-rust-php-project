// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! GOST R 34.11-94 with the test and `CryptoPro` parameter sets
//! (ref: `ext/hash/hash_gost.c`).

use super::gost_tables::{GOST_TABLES_CRYPTO, GOST_TABLES_TEST};
use super::{HashState, ImageReader, ImageWriter};

type Tables = [u32; 1024];

#[inline]
fn sbox(tables: &Tables, t: u32) -> u32 {
    tables[(t & 0xff) as usize]
        ^ tables[256 + ((t >> 8) & 0xff) as usize]
        ^ tables[512 + ((t >> 16) & 0xff) as usize]
        ^ tables[768 + (t >> 24) as usize]
}

/// The 32-round GOST 28147-89 encryption of `(h[i], h[i+1])` under `key`;
/// returns `(l, r)` after the final swap.
fn encrypt(tables: &Tables, key: &[u32; 8], r0: u32, l0: u32) -> (u32, u32) {
    let mut r = r0;
    let mut l = l0;
    let mut round = |k1: u32, k2: u32| {
        l ^= sbox(tables, k1.wrapping_add(r));
        r ^= sbox(tables, k2.wrapping_add(l));
    };
    for _ in 0..3 {
        round(key[0], key[1]);
        round(key[2], key[3]);
        round(key[4], key[5]);
        round(key[6], key[7]);
    }
    round(key[7], key[6]);
    round(key[5], key[4]);
    round(key[3], key[2]);
    round(key[1], key[0]);
    // t = r; r = l; l = t;
    (r, l)
}

fn p(w: &[u32; 8]) -> [u32; 8] {
    [
        (w[0] & 0x0000_00ff)
            | ((w[2] & 0x0000_00ff) << 8)
            | ((w[4] & 0x0000_00ff) << 16)
            | ((w[6] & 0x0000_00ff) << 24),
        ((w[0] & 0x0000_ff00) >> 8)
            | (w[2] & 0x0000_ff00)
            | ((w[4] & 0x0000_ff00) << 8)
            | ((w[6] & 0x0000_ff00) << 16),
        ((w[0] & 0x00ff_0000) >> 16)
            | ((w[2] & 0x00ff_0000) >> 8)
            | (w[4] & 0x00ff_0000)
            | ((w[6] & 0x00ff_0000) << 8),
        ((w[0] & 0xff00_0000) >> 24)
            | ((w[2] & 0xff00_0000) >> 16)
            | ((w[4] & 0xff00_0000) >> 8)
            | (w[6] & 0xff00_0000),
        (w[1] & 0x0000_00ff)
            | ((w[3] & 0x0000_00ff) << 8)
            | ((w[5] & 0x0000_00ff) << 16)
            | ((w[7] & 0x0000_00ff) << 24),
        ((w[1] & 0x0000_ff00) >> 8)
            | (w[3] & 0x0000_ff00)
            | ((w[5] & 0x0000_ff00) << 8)
            | ((w[7] & 0x0000_ff00) << 16),
        ((w[1] & 0x00ff_0000) >> 16)
            | ((w[3] & 0x00ff_0000) >> 8)
            | (w[5] & 0x00ff_0000)
            | ((w[7] & 0x00ff_0000) << 8),
        ((w[1] & 0xff00_0000) >> 24)
            | ((w[3] & 0xff00_0000) >> 16)
            | ((w[5] & 0xff00_0000) >> 8)
            | (w[7] & 0xff00_0000),
    ]
}

fn a(x: &mut [u32; 8]) {
    let l = x[0] ^ x[2];
    let r = x[1] ^ x[3];
    x.copy_within(2..8, 0);
    x[6] = l;
    x[7] = r;
}

fn aa(x: &mut [u32; 8]) {
    let l = x[0];
    let r = x[2];
    x[0] = x[4];
    x[2] = x[6];
    x[4] = l ^ r;
    x[6] = x[0] ^ r;
    let l = x[1];
    let r = x[3];
    x[1] = x[5];
    x[3] = x[7];
    x[5] = l ^ r;
    x[7] = x[1] ^ r;
}

fn c(x: &mut [u32; 8]) {
    x[0] ^= 0xff00_ff00;
    x[1] ^= 0xff00_ff00;
    x[2] ^= 0x00ff_00ff;
    x[3] ^= 0x00ff_00ff;
    x[4] ^= 0x00ff_ff00;
    x[5] ^= 0xff00_00ff;
    x[6] ^= 0x0000_00ff;
    x[7] ^= 0xff00_ffff;
}

#[allow(clippy::too_many_lines)]
fn gost(tables: &Tables, h: &mut [u32; 16], m: &[u32; 8]) {
    let mut u = [0u32; 8];
    u.copy_from_slice(&h[..8]);
    let mut v = *m;
    let mut s = [0u32; 8];
    for i in (0..8).step_by(2) {
        let mut w = [0u32; 8];
        for k in 0..8 {
            w[k] = u[k] ^ v[k];
        }
        let key = p(&w);
        let (l, r) = encrypt(tables, &key, h[i], h[i + 1]);
        s[i] = r;
        s[i + 1] = l;
        if i != 6 {
            a(&mut u);
            if i == 2 {
                c(&mut u);
            }
            aa(&mut v);
        }
    }

    // SHIFT12(u, m, s)
    u[0] = m[0] ^ s[6];
    u[1] = m[1] ^ s[7];
    u[2] = m[2]
        ^ (s[0] << 16)
        ^ (s[0] >> 16)
        ^ (s[0] & 0xffff)
        ^ (s[1] & 0xffff)
        ^ (s[1] >> 16)
        ^ (s[2] << 16)
        ^ s[6]
        ^ (s[6] << 16)
        ^ (s[7] & 0xffff_0000)
        ^ (s[7] >> 16);
    u[3] = m[3]
        ^ (s[0] & 0xffff)
        ^ (s[0] << 16)
        ^ (s[1] & 0xffff)
        ^ (s[1] << 16)
        ^ (s[1] >> 16)
        ^ (s[2] << 16)
        ^ (s[2] >> 16)
        ^ (s[3] << 16)
        ^ s[6]
        ^ (s[6] << 16)
        ^ (s[6] >> 16)
        ^ (s[7] & 0xffff)
        ^ (s[7] << 16)
        ^ (s[7] >> 16);
    u[4] = m[4]
        ^ (s[0] & 0xffff_0000)
        ^ (s[0] << 16)
        ^ (s[0] >> 16)
        ^ (s[1] & 0xffff_0000)
        ^ (s[1] >> 16)
        ^ (s[2] << 16)
        ^ (s[2] >> 16)
        ^ (s[3] << 16)
        ^ (s[3] >> 16)
        ^ (s[4] << 16)
        ^ (s[6] << 16)
        ^ (s[6] >> 16)
        ^ (s[7] & 0xffff)
        ^ (s[7] << 16)
        ^ (s[7] >> 16);
    u[5] = m[5]
        ^ (s[0] << 16)
        ^ (s[0] >> 16)
        ^ (s[0] & 0xffff_0000)
        ^ (s[1] & 0xffff)
        ^ s[2]
        ^ (s[2] >> 16)
        ^ (s[3] << 16)
        ^ (s[3] >> 16)
        ^ (s[4] << 16)
        ^ (s[4] >> 16)
        ^ (s[5] << 16)
        ^ (s[6] << 16)
        ^ (s[6] >> 16)
        ^ (s[7] & 0xffff_0000)
        ^ (s[7] << 16)
        ^ (s[7] >> 16);
    u[6] = m[6]
        ^ s[0]
        ^ (s[1] >> 16)
        ^ (s[2] << 16)
        ^ s[3]
        ^ (s[3] >> 16)
        ^ (s[4] << 16)
        ^ (s[4] >> 16)
        ^ (s[5] << 16)
        ^ (s[5] >> 16)
        ^ s[6]
        ^ (s[6] << 16)
        ^ (s[6] >> 16)
        ^ (s[7] << 16);
    u[7] = m[7]
        ^ (s[0] & 0xffff_0000)
        ^ (s[0] << 16)
        ^ (s[1] & 0xffff)
        ^ (s[1] << 16)
        ^ (s[2] >> 16)
        ^ (s[3] << 16)
        ^ s[4]
        ^ (s[4] >> 16)
        ^ (s[5] << 16)
        ^ (s[5] >> 16)
        ^ (s[6] >> 16)
        ^ (s[7] & 0xffff)
        ^ (s[7] << 16)
        ^ (s[7] >> 16);

    // SHIFT16(h, v, u)
    v[0] = h[0] ^ (u[1] << 16) ^ (u[0] >> 16);
    v[1] = h[1] ^ (u[2] << 16) ^ (u[1] >> 16);
    v[2] = h[2] ^ (u[3] << 16) ^ (u[2] >> 16);
    v[3] = h[3] ^ (u[4] << 16) ^ (u[3] >> 16);
    v[4] = h[4] ^ (u[5] << 16) ^ (u[4] >> 16);
    v[5] = h[5] ^ (u[6] << 16) ^ (u[5] >> 16);
    v[6] = h[6] ^ (u[7] << 16) ^ (u[6] >> 16);
    v[7] = h[7]
        ^ (u[0] & 0xffff_0000)
        ^ (u[0] << 16)
        ^ (u[7] >> 16)
        ^ (u[1] & 0xffff_0000)
        ^ (u[1] << 16)
        ^ (u[6] << 16)
        ^ (u[7] & 0xffff_0000);

    // SHIFT61(h, v)
    h[0] = (v[0] & 0xffff_0000)
        ^ (v[0] << 16)
        ^ (v[0] >> 16)
        ^ (v[1] >> 16)
        ^ (v[1] & 0xffff_0000)
        ^ (v[2] << 16)
        ^ (v[3] >> 16)
        ^ (v[4] << 16)
        ^ (v[5] >> 16)
        ^ v[5]
        ^ (v[6] >> 16)
        ^ (v[7] << 16)
        ^ (v[7] >> 16)
        ^ (v[7] & 0xffff);
    h[1] = (v[0] << 16)
        ^ (v[0] >> 16)
        ^ (v[0] & 0xffff_0000)
        ^ (v[1] & 0xffff)
        ^ v[2]
        ^ (v[2] >> 16)
        ^ (v[3] << 16)
        ^ (v[4] >> 16)
        ^ (v[5] << 16)
        ^ (v[6] << 16)
        ^ v[6]
        ^ (v[7] & 0xffff_0000)
        ^ (v[7] >> 16);
    h[2] = (v[0] & 0xffff)
        ^ (v[0] << 16)
        ^ (v[1] << 16)
        ^ (v[1] >> 16)
        ^ (v[1] & 0xffff_0000)
        ^ (v[2] << 16)
        ^ (v[3] >> 16)
        ^ v[3]
        ^ (v[4] << 16)
        ^ (v[5] >> 16)
        ^ v[6]
        ^ (v[6] >> 16)
        ^ (v[7] & 0xffff)
        ^ (v[7] << 16)
        ^ (v[7] >> 16);
    h[3] = (v[0] << 16)
        ^ (v[0] >> 16)
        ^ (v[0] & 0xffff_0000)
        ^ (v[1] & 0xffff_0000)
        ^ (v[1] >> 16)
        ^ (v[2] << 16)
        ^ (v[2] >> 16)
        ^ v[2]
        ^ (v[3] << 16)
        ^ (v[4] >> 16)
        ^ v[4]
        ^ (v[5] << 16)
        ^ (v[6] << 16)
        ^ (v[7] & 0xffff)
        ^ (v[7] >> 16);
    h[4] = (v[0] >> 16)
        ^ (v[1] << 16)
        ^ v[1]
        ^ (v[2] >> 16)
        ^ v[2]
        ^ (v[3] << 16)
        ^ (v[3] >> 16)
        ^ v[3]
        ^ (v[4] << 16)
        ^ (v[5] >> 16)
        ^ v[5]
        ^ (v[6] << 16)
        ^ (v[6] >> 16)
        ^ (v[7] << 16);
    h[5] = (v[0] << 16)
        ^ (v[0] & 0xffff_0000)
        ^ (v[1] << 16)
        ^ (v[1] >> 16)
        ^ (v[1] & 0xffff_0000)
        ^ (v[2] << 16)
        ^ v[2]
        ^ (v[3] >> 16)
        ^ v[3]
        ^ (v[4] << 16)
        ^ (v[4] >> 16)
        ^ v[4]
        ^ (v[5] << 16)
        ^ (v[6] << 16)
        ^ (v[6] >> 16)
        ^ v[6]
        ^ (v[7] << 16)
        ^ (v[7] >> 16)
        ^ (v[7] & 0xffff_0000);
    h[6] = v[0]
        ^ v[2]
        ^ (v[2] >> 16)
        ^ v[3]
        ^ (v[3] << 16)
        ^ v[4]
        ^ (v[4] >> 16)
        ^ (v[5] << 16)
        ^ (v[5] >> 16)
        ^ v[5]
        ^ (v[6] << 16)
        ^ (v[6] >> 16)
        ^ v[6]
        ^ (v[7] << 16)
        ^ v[7];
    h[7] = v[0]
        ^ (v[0] >> 16)
        ^ (v[1] << 16)
        ^ (v[1] >> 16)
        ^ (v[2] << 16)
        ^ (v[3] >> 16)
        ^ v[3]
        ^ (v[4] << 16)
        ^ v[4]
        ^ (v[5] >> 16)
        ^ v[5]
        ^ (v[6] << 16)
        ^ (v[6] >> 16)
        ^ (v[7] << 16)
        ^ v[7];
}

/// `PHP_GOST_CTX`.
#[derive(Clone, Debug)]
pub(crate) struct Gost {
    state: [u32; 16],
    count: [u32; 2],
    length: u8,
    buffer: [u8; 32],
    crypto: bool,
}

impl Gost {
    pub(crate) fn new(crypto: bool) -> Self {
        Self {
            state: [0; 16],
            count: [0; 2],
            length: 0,
            buffer: [0; 32],
            crypto,
        }
    }

    fn tables(&self) -> &'static Tables {
        if self.crypto {
            &GOST_TABLES_CRYPTO
        } else {
            &GOST_TABLES_TEST
        }
    }

    fn transform(&mut self, input: &[u8]) {
        let mut data = [0u32; 8];
        let mut temp = 0u32;
        for (i, c) in input.chunks_exact(4).enumerate() {
            data[i] = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            let sum = &mut self.state[i + 8];
            *sum = sum.wrapping_add(data[i]).wrapping_add(temp);
            temp = match (*sum).cmp(&data[i]) {
                std::cmp::Ordering::Less => 1,
                std::cmp::Ordering::Equal => temp,
                std::cmp::Ordering::Greater => 0,
            };
        }
        let tables = self.tables();
        gost(tables, &mut self.state, &data);
    }
}

impl HashState for Gost {
    fn update(&mut self, input: &[u8]) {
        let len = input.len();
        let count = ((u64::from(self.count[1]) << 32) | u64::from(self.count[0]))
            .wrapping_add((len as u64).wrapping_mul(8));
        #[allow(clippy::cast_possible_truncation)]
        {
            self.count[0] = count as u32;
            self.count[1] = (count >> 32) as u32;
        }
        let length = self.length as usize;
        if length + len < 32 {
            self.buffer[length..length + len].copy_from_slice(input);
            #[allow(clippy::cast_possible_truncation)]
            {
                self.length = self.length.wrapping_add(len as u8);
            }
        } else {
            let mut i = 0;
            let r = (length + len) % 32;
            if length != 0 {
                i = 32 - length;
                self.buffer[length..].copy_from_slice(&input[..i]);
                let block = self.buffer;
                self.transform(&block);
            }
            while i + 32 <= len {
                self.transform(&input[i..i + 32]);
                i += 32;
            }
            self.buffer[..r].copy_from_slice(&input[i..i + r]);
            self.buffer[r..].fill(0);
            #[allow(clippy::cast_possible_truncation)]
            {
                self.length = r as u8;
            }
        }
    }

    fn finish(&mut self, out: &mut [u8]) {
        if self.length != 0 {
            let block = self.buffer;
            self.transform(&block);
        }
        let tables = self.tables();
        let mut l = [0u32; 8];
        l[..2].copy_from_slice(&self.count);
        gost(tables, &mut self.state, &l);
        l.copy_from_slice(&self.state[8..]);
        gost(tables, &mut self.state, &l);
        super::put_le32(out, &self.state[..8]);
    }

    fn image(&self) -> Vec<u8> {
        ImageWriter::new()
            .u32s(&self.state)
            .u32s(&self.count)
            .u8(self.length)
            .bytes(&self.buffer)
            .finish(120)
    }

    fn load_image(&mut self, image: &[u8]) {
        let mut r = ImageReader::new(image);
        r.u32s(&mut self.state);
        r.u32s(&mut self.count);
        self.length = r.u8();
        r.bytes(&mut self.buffer);
    }

    fn is_valid(&self) -> bool {
        (self.length as usize) < self.buffer.len()
    }
}

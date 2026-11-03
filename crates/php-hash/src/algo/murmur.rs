//! `MurmurHash3` x86-32 ("murmur3a"), x86-128 ("murmur3c") and x64-128
//! ("murmur3f"), as incremental `PMurHash` ports (ref: ext/hash/murmur/).
//!
//! The carry words are reproduced exactly as the x86 (`UNALIGNED_SAFE`)
//! build of PHP leaves them, including the bits that do not influence the
//! digest, because they are visible through serialization.

use super::{HashState, ImageReader, ImageWriter};

const C1_32: u32 = 0xcc9e_2d51;
const C2_32: u32 = 0x1b87_3593;

fn fmix32(mut h: u32) -> u32 {
    h ^= h >> 16;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    h
}

fn fmix64(mut k: u64) -> u64 {
    k ^= k >> 33;
    k = k.wrapping_mul(0xff51_afd7_ed55_8ccd);
    k ^= k >> 33;
    k = k.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    k ^= k >> 33;
    k
}

fn read32(p: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([p[4 * i], p[4 * i + 1], p[4 * i + 2], p[4 * i + 3]])
}

fn read64(p: &[u8], i: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&p[8 * i..8 * i + 8]);
    u64::from_le_bytes(b)
}

// ---------------------------------------------------------------- murmur3a

fn doblock32(h1: &mut u32, k1: &mut u32) {
    *k1 = k1.wrapping_mul(C1_32);
    *k1 = k1.rotate_left(15);
    *k1 = k1.wrapping_mul(C2_32);
    *h1 ^= *k1;
    *h1 = h1.rotate_left(13);
    *h1 = h1.wrapping_mul(5).wrapping_add(0xe654_6b64);
}

/// `PHP_MURMUR3A_CTX`.
#[derive(Clone, Debug)]
pub(crate) struct Murmur3a {
    h: u32,
    carry: u32,
    len: u32,
}

impl Murmur3a {
    pub(crate) fn new(seed: u32) -> Self {
        Self {
            h: seed,
            carry: 0,
            len: 0,
        }
    }
}

impl HashState for Murmur3a {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss
    )]
    fn update(&mut self, data: &[u8]) {
        self.len = self.len.wrapping_add(data.len() as u32);
        let mut h1 = self.h;
        let mut c = self.carry;
        let mut n = (c & 3) as usize;
        // `int len` in the C code.
        let mut len = i64::from(data.len() as i32);
        let mut ptr = 0usize;

        let dobytes =
            |cnt: i64, h1: &mut u32, c: &mut u32, n: &mut usize, ptr: &mut usize, len: &mut i64| {
                for _ in 0..cnt {
                    *c = (*c >> 8) | (u32::from(data[*ptr]) << 24);
                    *ptr += 1;
                    *n += 1;
                    *len -= 1;
                    if *n == 4 {
                        doblock32(h1, c);
                        *n = 0;
                    }
                }
            };

        let i = ((4 - n) & 3) as i64;
        if i != 0 && i <= len {
            dobytes(i, &mut h1, &mut c, &mut n, &mut ptr, &mut len);
        }
        let end = ptr + (len.max(0) as usize & !3);
        while ptr < end {
            let mut k1 = read32(&data[ptr..], 0);
            doblock32(&mut h1, &mut k1);
            ptr += 4;
        }
        len -= len & !3;
        let cnt = len;
        dobytes(cnt, &mut h1, &mut c, &mut n, &mut ptr, &mut len);
        self.h = h1;
        self.carry = (c & !0xff) | n as u32;
    }

    fn finish(&mut self, out: &mut [u8]) {
        let mut h = self.h;
        let n = self.carry & 3;
        if n != 0 {
            let mut k1 = self.carry >> ((4 - n) * 8);
            k1 = k1.wrapping_mul(C1_32).rotate_left(15).wrapping_mul(C2_32);
            h ^= k1;
        }
        h ^= self.len;
        h = fmix32(h);
        self.h = h;
        out.copy_from_slice(&h.to_be_bytes());
    }

    fn image(&self) -> Vec<u8> {
        ImageWriter::new()
            .u32(self.h)
            .u32(self.carry)
            .u32(self.len)
            .finish(12)
    }

    fn load_image(&mut self, image: &[u8]) {
        let mut r = ImageReader::new(image);
        self.h = r.u32();
        self.carry = r.u32();
        self.len = r.u32();
    }
}

// ---------------------------------------------------------------- murmur3c

const K1_128: u32 = 0x239b_961b;
const K2_128: u32 = 0xab0e_9789;
const K3_128: u32 = 0x38b3_4ae5;
const K4_128: u32 = 0xa1e3_8b93;

fn doblock128x86(h: &mut [u32; 4], k: &mut [u32; 4]) {
    k[0] = k[0]
        .wrapping_mul(K1_128)
        .rotate_left(15)
        .wrapping_mul(K2_128);
    h[0] ^= k[0];
    h[0] = h[0]
        .rotate_left(19)
        .wrapping_add(h[1])
        .wrapping_mul(5)
        .wrapping_add(0x561c_cd1b);
    k[1] = k[1]
        .wrapping_mul(K2_128)
        .rotate_left(16)
        .wrapping_mul(K3_128);
    h[1] ^= k[1];
    h[1] = h[1]
        .rotate_left(17)
        .wrapping_add(h[2])
        .wrapping_mul(5)
        .wrapping_add(0x0bca_a747);
    k[2] = k[2]
        .wrapping_mul(K3_128)
        .rotate_left(17)
        .wrapping_mul(K4_128);
    h[2] ^= k[2];
    h[2] = h[2]
        .rotate_left(15)
        .wrapping_add(h[3])
        .wrapping_mul(5)
        .wrapping_add(0x96cd_1c35);
    k[3] = k[3]
        .wrapping_mul(K4_128)
        .rotate_left(18)
        .wrapping_mul(K1_128);
    h[3] ^= k[3];
    h[3] = h[3]
        .rotate_left(13)
        .wrapping_add(h[0])
        .wrapping_mul(5)
        .wrapping_add(0x32ac_3b17);
}

/// `PHP_MURMUR3C_CTX`.
#[derive(Clone, Debug)]
pub(crate) struct Murmur3c {
    h: [u32; 4],
    carry: [u32; 4],
    len: u32,
}

impl Murmur3c {
    pub(crate) fn new(seed: u32) -> Self {
        Self {
            h: [seed; 4],
            carry: [0; 4],
            len: 0,
        }
    }
}

fn dobytes128x86(
    cnt: i64,
    h: &mut [u32; 4],
    k: &mut [u32; 4],
    n: &mut usize,
    data: &[u8],
    ptr: &mut usize,
) {
    for _ in 0..cnt {
        let byte = u32::from(data[*ptr]) << 24;
        *ptr += 1;
        match *n {
            0..=3 => {
                k[0] = (k[0] >> 8) | byte;
                *n += 1;
            }
            4..=7 => {
                k[1] = (k[1] >> 8) | byte;
                *n += 1;
            }
            8..=11 => {
                k[2] = (k[2] >> 8) | byte;
                *n += 1;
            }
            12..=14 => {
                k[3] = (k[3] >> 8) | byte;
                *n += 1;
            }
            _ => {
                k[3] = (k[3] >> 8) | byte;
                doblock128x86(h, k);
                *n = 0;
            }
        }
    }
}

impl HashState for Murmur3c {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss
    )]
    fn update(&mut self, data: &[u8]) {
        self.len = self.len.wrapping_add(data.len() as u32);
        let mut h = self.h;
        let mut k = self.carry;
        let mut n = (k[3] & 15) as usize;
        let mut len = i64::from(data.len() as i32);
        let mut ptr = 0usize;
        let i = ((16 - n) & 15) as i64;
        if i != 0 && i <= len {
            dobytes128x86(i, &mut h, &mut k, &mut n, data, &mut ptr);
            len -= i;
        }
        let end = ptr + (len.max(0) as usize & !15);
        while ptr < end {
            let block = &data[ptr..];
            k = [
                read32(block, 0),
                read32(block, 1),
                read32(block, 2),
                read32(block, 3),
            ];
            doblock128x86(&mut h, &mut k);
            ptr += 16;
        }
        len -= len & !15;
        dobytes128x86(len, &mut h, &mut k, &mut n, data, &mut ptr);
        self.h = h;
        self.carry = [k[0], k[1], k[2], (k[3] & !0xff) | n as u32];
    }

    fn finish(&mut self, out: &mut [u8]) {
        let [mut h1, mut h2, mut h3, mut h4] = self.h;
        let pc = self.carry;
        let n = pc[3] & 15;
        let fin1 = |k1: u32| k1.wrapping_mul(K1_128).rotate_left(15).wrapping_mul(K2_128);
        let fin2 = |k2: u32| k2.wrapping_mul(K2_128).rotate_left(16).wrapping_mul(K3_128);
        let fin3 = |k3: u32| k3.wrapping_mul(K3_128).rotate_left(17).wrapping_mul(K4_128);
        let fin4 = |k4: u32| k4.wrapping_mul(K4_128).rotate_left(18).wrapping_mul(K1_128);
        match n {
            1..=4 => {
                h1 ^= fin1(pc[0] >> ((4 - n) * 8));
            }
            5..=8 => {
                h2 ^= fin2(pc[1] >> ((8 - n) * 8));
                h1 ^= fin1(pc[0]);
            }
            9..=12 => {
                h3 ^= fin3(pc[2] >> ((12 - n) * 8));
                h2 ^= fin2(pc[1]);
                h1 ^= fin1(pc[0]);
            }
            13..=15 => {
                h4 ^= fin4(pc[3] >> ((16 - n) * 8));
                h3 ^= fin3(pc[2]);
                h2 ^= fin2(pc[1]);
                h1 ^= fin1(pc[0]);
            }
            _ => {}
        }
        let len = self.len;
        h1 ^= len;
        h2 ^= len;
        h3 ^= len;
        h4 ^= len;
        h1 = h1.wrapping_add(h2).wrapping_add(h3).wrapping_add(h4);
        h2 = h2.wrapping_add(h1);
        h3 = h3.wrapping_add(h1);
        h4 = h4.wrapping_add(h1);
        h1 = fmix32(h1);
        h2 = fmix32(h2);
        h3 = fmix32(h3);
        h4 = fmix32(h4);
        h1 = h1.wrapping_add(h2).wrapping_add(h3).wrapping_add(h4);
        h2 = h2.wrapping_add(h1);
        h3 = h3.wrapping_add(h1);
        h4 = h4.wrapping_add(h1);
        super::put_be32(out, &[h1, h2, h3, h4]);
    }

    fn image(&self) -> Vec<u8> {
        ImageWriter::new()
            .u32s(&self.h)
            .u32s(&self.carry)
            .u32(self.len)
            .finish(36)
    }

    fn load_image(&mut self, image: &[u8]) {
        let mut r = ImageReader::new(image);
        r.u32s(&mut self.h);
        r.u32s(&mut self.carry);
        self.len = r.u32();
    }
}

// ---------------------------------------------------------------- murmur3f

const C1_64: u64 = 0x87c3_7b91_1142_53d5;
const C2_64: u64 = 0x4cf5_ad43_2745_937f;

fn doblock128x64(h: &mut [u64; 2], k: &mut [u64; 2]) {
    k[0] = k[0].wrapping_mul(C1_64).rotate_left(31).wrapping_mul(C2_64);
    h[0] ^= k[0];
    h[0] = h[0]
        .rotate_left(27)
        .wrapping_add(h[1])
        .wrapping_mul(5)
        .wrapping_add(0x52dc_e729);
    k[1] = k[1].wrapping_mul(C2_64).rotate_left(33).wrapping_mul(C1_64);
    h[1] ^= k[1];
    h[1] = h[1]
        .rotate_left(31)
        .wrapping_add(h[0])
        .wrapping_mul(5)
        .wrapping_add(0x3849_5ab5);
}

fn dobytes128x64(
    cnt: i64,
    h: &mut [u64; 2],
    k: &mut [u64; 2],
    n: &mut usize,
    data: &[u8],
    ptr: &mut usize,
) {
    for _ in 0..cnt {
        let byte = u64::from(data[*ptr]) << 56;
        *ptr += 1;
        match *n {
            0..=7 => {
                k[0] = (k[0] >> 8) | byte;
                *n += 1;
            }
            8..=14 => {
                k[1] = (k[1] >> 8) | byte;
                *n += 1;
            }
            _ => {
                k[1] = (k[1] >> 8) | byte;
                doblock128x64(h, k);
                *n = 0;
            }
        }
    }
}

/// `PHP_MURMUR3F_CTX`.
#[derive(Clone, Debug)]
pub(crate) struct Murmur3f {
    h: [u64; 2],
    carry: [u64; 2],
    len: u32,
}

impl Murmur3f {
    pub(crate) fn new(seed: u64) -> Self {
        Self {
            h: [seed; 2],
            carry: [0; 2],
            len: 0,
        }
    }
}

impl HashState for Murmur3f {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss
    )]
    fn update(&mut self, data: &[u8]) {
        self.len = self.len.wrapping_add(data.len() as u32);
        let mut h = self.h;
        let mut k = self.carry;
        let mut n = (k[1] & 15) as usize;
        let mut len = i64::from(data.len() as i32);
        let mut ptr = 0usize;
        let i = ((16 - n) & 15) as i64;
        if i != 0 && i <= len {
            dobytes128x64(i, &mut h, &mut k, &mut n, data, &mut ptr);
            len -= i;
        }
        let end = ptr + (len.max(0) as usize & !15);
        while ptr < end {
            let block = &data[ptr..];
            k = [read64(block, 0), read64(block, 1)];
            doblock128x64(&mut h, &mut k);
            ptr += 16;
        }
        len -= len & !15;
        dobytes128x64(len, &mut h, &mut k, &mut n, data, &mut ptr);
        self.h = h;
        self.carry = [k[0], (k[1] & !0xff) | n as u64];
    }

    fn finish(&mut self, out: &mut [u8]) {
        let [mut h1, mut h2] = self.h;
        let k2c = self.carry[1];
        let n = k2c & 15;
        if n != 0 {
            let mut k1 = self.carry[0];
            if n > 8 {
                let mut k2 = k2c >> ((16 - n) * 8);
                k2 = k2.wrapping_mul(C2_64).rotate_left(33).wrapping_mul(C1_64);
                h2 ^= k2;
            } else {
                k1 >>= (8 - n) * 8;
            }
            k1 = k1.wrapping_mul(C1_64).rotate_left(31).wrapping_mul(C2_64);
            h1 ^= k1;
        }
        let len = u64::from(self.len);
        h1 ^= len;
        h2 ^= len;
        h1 = h1.wrapping_add(h2);
        h2 = h2.wrapping_add(h1);
        h1 = fmix64(h1);
        h2 = fmix64(h2);
        h1 = h1.wrapping_add(h2);
        h2 = h2.wrapping_add(h1);
        out[..8].copy_from_slice(&h1.to_be_bytes());
        out[8..].copy_from_slice(&h2.to_be_bytes());
    }

    fn image(&self) -> Vec<u8> {
        ImageWriter::new()
            .u64s(&self.h)
            .u64s(&self.carry)
            .u32(self.len)
            .finish(40)
    }

    fn load_image(&mut self, image: &[u8]) {
        let mut r = ImageReader::new(image);
        r.u64s(&mut self.h);
        r.u64s(&mut self.carry);
        self.len = r.u32();
    }
}

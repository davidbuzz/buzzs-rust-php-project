//! RIPEMD-128/160/256/320 (ref: `ext/hash/hash_ripemd.c`).

use super::{le_words, md_update32, put_le32, HashState, ImageReader, ImageWriter, PADDING};

const K: [u32; 5] = [
    0x0000_0000,
    0x5A82_7999,
    0x6ED9_EBA1,
    0x8F1B_BCDC,
    0xA953_FD4E,
];
const KK: [u32; 4] = [0x50A2_8BE6, 0x5C4D_D124, 0x6D70_3EF3, 0x0000_0000];
const KK160: [u32; 5] = [
    0x50A2_8BE6,
    0x5C4D_D124,
    0x6D70_3EF3,
    0x7A6D_76E9,
    0x0000_0000,
];

const R: [usize; 80] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 7, 4, 13, 1, 10, 6, 15, 3, 12, 0, 9, 5,
    2, 14, 11, 8, 3, 10, 14, 4, 9, 15, 8, 1, 2, 7, 0, 6, 13, 11, 5, 12, 1, 9, 11, 10, 0, 8, 12, 4,
    13, 3, 7, 15, 14, 5, 6, 2, 4, 0, 5, 9, 7, 12, 2, 10, 14, 1, 3, 8, 11, 6, 15, 13,
];
const RR: [usize; 80] = [
    5, 14, 7, 0, 9, 2, 11, 4, 13, 6, 15, 8, 1, 10, 3, 12, 6, 11, 3, 7, 0, 13, 5, 10, 14, 15, 8, 12,
    4, 9, 1, 2, 15, 5, 1, 3, 7, 14, 6, 9, 11, 8, 12, 2, 10, 0, 4, 13, 8, 6, 4, 1, 3, 11, 15, 0, 5,
    12, 2, 13, 9, 7, 10, 14, 12, 15, 10, 4, 1, 5, 8, 7, 6, 2, 13, 14, 0, 3, 9, 11,
];
const S: [u32; 80] = [
    11, 14, 15, 12, 5, 8, 7, 9, 11, 13, 14, 15, 6, 7, 9, 8, 7, 6, 8, 13, 11, 9, 7, 15, 7, 12, 15,
    9, 11, 7, 13, 12, 11, 13, 6, 7, 14, 9, 13, 15, 14, 8, 13, 6, 5, 12, 7, 5, 11, 12, 14, 15, 14,
    15, 9, 8, 9, 14, 5, 6, 8, 6, 5, 12, 9, 15, 5, 11, 6, 8, 13, 12, 5, 12, 13, 14, 11, 8, 5, 6,
];
const SS: [u32; 80] = [
    8, 9, 9, 11, 13, 15, 15, 5, 7, 7, 8, 11, 14, 14, 12, 6, 9, 13, 15, 7, 12, 8, 9, 11, 7, 7, 12,
    7, 6, 15, 13, 11, 9, 7, 15, 11, 8, 6, 6, 14, 12, 13, 5, 14, 13, 13, 7, 5, 15, 5, 8, 11, 14, 14,
    6, 14, 6, 9, 12, 9, 12, 5, 15, 8, 8, 5, 12, 9, 12, 5, 14, 6, 8, 13, 6, 5, 15, 13, 11, 11,
];

/// Boolean function `F0`..`F4` selected by index.
fn f(n: usize, x: u32, y: u32, z: u32) -> u32 {
    match n {
        0 => x ^ y ^ z,
        1 => (x & y) | (!x & z),
        2 => (x | !y) ^ z,
        3 => (x & z) | (y & !z),
        _ => x ^ (y | !z),
    }
}

/// Four-word line step used by RIPEMD-128/256: returns the new `(a, b, c, d)`.
fn step4(fun: usize, k: u32, v: [u32; 4], x: u32, s: u32) -> [u32; 4] {
    let [a, b, c, d] = v;
    let t = a
        .wrapping_add(f(fun, b, c, d))
        .wrapping_add(x)
        .wrapping_add(k)
        .rotate_left(s);
    [d, t, b, c]
}

/// Five-word line step used by RIPEMD-160/320: returns the new `(a, b, c, d, e)`.
fn step5(fun: usize, k: u32, v: [u32; 5], x: u32, s: u32) -> [u32; 5] {
    let [a, b, c, d, e] = v;
    let t = a
        .wrapping_add(f(fun, b, c, d))
        .wrapping_add(x)
        .wrapping_add(k)
        .rotate_left(s)
        .wrapping_add(e);
    [e, t, b, c.rotate_left(10), d]
}

/// Runs round `r` (0-based, 16 steps) of both lines of the 4-word variants.
fn rounds4(r: usize, x: &[u32; 16], left: &mut [u32; 4], right: &mut [u32; 4]) {
    const LF: [usize; 4] = [0, 1, 2, 3];
    const RF: [usize; 4] = [3, 2, 1, 0];
    for j in 16 * r..16 * (r + 1) {
        *left = step4(LF[r], K[j >> 4], *left, x[R[j]], S[j]);
        *right = step4(RF[r], KK[j >> 4], *right, x[RR[j]], SS[j]);
    }
}

/// Runs round `r` (0-based, 16 steps) of both lines of the 5-word variants.
fn rounds5(r: usize, x: &[u32; 16], left: &mut [u32; 5], right: &mut [u32; 5]) {
    const LF: [usize; 5] = [0, 1, 2, 3, 4];
    const RF: [usize; 5] = [4, 3, 2, 1, 0];
    for j in 16 * r..16 * (r + 1) {
        *left = step5(LF[r], K[j >> 4], *left, x[R[j]], S[j]);
        *right = step5(RF[r], KK160[j >> 4], *right, x[RR[j]], SS[j]);
    }
}

fn transform128(state: &mut [u32], block: &[u8]) {
    let x: [u32; 16] = le_words(block);
    let mut l = [state[0], state[1], state[2], state[3]];
    let mut r = l;
    for round in 0..4 {
        rounds4(round, &x, &mut l, &mut r);
    }
    let [a, b, c, d] = l;
    let [aa, bb, cc, dd] = r;
    let tmp = state[1].wrapping_add(c).wrapping_add(dd);
    state[1] = state[2].wrapping_add(d).wrapping_add(aa);
    state[2] = state[3].wrapping_add(a).wrapping_add(bb);
    state[3] = state[0].wrapping_add(b).wrapping_add(cc);
    state[0] = tmp;
}

fn transform256(state: &mut [u32], block: &[u8]) {
    let x: [u32; 16] = le_words(block);
    let mut l = [state[0], state[1], state[2], state[3]];
    let mut r = [state[4], state[5], state[6], state[7]];
    for round in 0..4 {
        rounds4(round, &x, &mut l, &mut r);
        // After round n swap word n (a, b, c, d) between the lines.
        std::mem::swap(&mut l[round], &mut r[round]);
    }
    for i in 0..4 {
        state[i] = state[i].wrapping_add(l[i]);
        state[i + 4] = state[i + 4].wrapping_add(r[i]);
    }
}

fn transform160(state: &mut [u32], block: &[u8]) {
    let x: [u32; 16] = le_words(block);
    let mut l = [state[0], state[1], state[2], state[3], state[4]];
    let mut r = l;
    for round in 0..5 {
        rounds5(round, &x, &mut l, &mut r);
    }
    let [a, b, c, d, e] = l;
    let [aa, bb, cc, dd, ee] = r;
    let tmp = state[1].wrapping_add(c).wrapping_add(dd);
    state[1] = state[2].wrapping_add(d).wrapping_add(ee);
    state[2] = state[3].wrapping_add(e).wrapping_add(aa);
    state[3] = state[4].wrapping_add(a).wrapping_add(bb);
    state[4] = state[0].wrapping_add(b).wrapping_add(cc);
    state[0] = tmp;
}

fn transform320(state: &mut [u32], block: &[u8]) {
    // Words swapped between the lines after each round: b, d, a, c, e.
    const SWAP: [usize; 5] = [1, 3, 0, 2, 4];
    let x: [u32; 16] = le_words(block);
    let mut l = [state[0], state[1], state[2], state[3], state[4]];
    let mut r = [state[5], state[6], state[7], state[8], state[9]];
    for (round, &w) in SWAP.iter().enumerate() {
        rounds5(round, &x, &mut l, &mut r);
        std::mem::swap(&mut l[w], &mut r[w]);
    }
    for i in 0..5 {
        state[i] = state[i].wrapping_add(l[i]);
        state[i + 5] = state[i + 5].wrapping_add(r[i]);
    }
}

/// Which RIPEMD variant a [`Ripemd`] context computes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RipemdVariant {
    R128,
    R160,
    R256,
    R320,
}

/// `PHP_RIPEMD{128,160,256,320}_CTX`.
#[derive(Clone, Debug)]
pub(crate) struct Ripemd {
    variant: RipemdVariant,
    state: [u32; 10],
    count: [u32; 2],
    buffer: [u8; 64],
}

impl Ripemd {
    pub(crate) fn new(variant: RipemdVariant) -> Self {
        let mut state = [0u32; 10];
        let init: &[u32] = match variant {
            RipemdVariant::R128 => &[0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476],
            RipemdVariant::R160 => &[
                0x6745_2301,
                0xEFCD_AB89,
                0x98BA_DCFE,
                0x1032_5476,
                0xC3D2_E1F0,
            ],
            RipemdVariant::R256 => &[
                0x6745_2301,
                0xEFCD_AB89,
                0x98BA_DCFE,
                0x1032_5476,
                0x7654_3210,
                0xFEDC_BA98,
                0x89AB_CDEF,
                0x0123_4567,
            ],
            RipemdVariant::R320 => &[
                0x6745_2301,
                0xEFCD_AB89,
                0x98BA_DCFE,
                0x1032_5476,
                0xC3D2_E1F0,
                0x7654_3210,
                0xFEDC_BA98,
                0x89AB_CDEF,
                0x0123_4567,
                0x3C2D_1E0F,
            ],
        };
        state[..init.len()].copy_from_slice(init);
        Self {
            variant,
            state,
            count: [0; 2],
            buffer: [0; 64],
        }
    }

    fn words(&self) -> usize {
        match self.variant {
            RipemdVariant::R128 => 4,
            RipemdVariant::R160 => 5,
            RipemdVariant::R256 => 8,
            RipemdVariant::R320 => 10,
        }
    }
}

impl HashState for Ripemd {
    fn update(&mut self, data: &[u8]) {
        let n = self.words();
        let state = &mut self.state[..n];
        let transform: fn(&mut [u32], &[u8]) = match self.variant {
            RipemdVariant::R128 => transform128,
            RipemdVariant::R160 => transform160,
            RipemdVariant::R256 => transform256,
            RipemdVariant::R320 => transform320,
        };
        md_update32(&mut self.count, &mut self.buffer, data, |b| {
            transform(state, b);
        });
    }

    fn finish(&mut self, out: &mut [u8]) {
        let mut bits = [0u8; 8];
        put_le32(&mut bits, &self.count);
        let index = ((self.count[0] >> 3) & 0x3f) as usize;
        let pad_len = if index < 56 { 56 - index } else { 120 - index };
        self.update(&PADDING[..pad_len]);
        self.update(&bits);
        put_le32(out, &self.state);
    }

    fn image(&self) -> Vec<u8> {
        let n = self.words();
        ImageWriter::new()
            .u32s(&self.state[..n])
            .u32s(&self.count)
            .bytes(&self.buffer)
            .finish(4 * n + 72)
    }

    fn load_image(&mut self, image: &[u8]) {
        let n = self.words();
        let mut r = ImageReader::new(image);
        r.u32s(&mut self.state[..n]);
        r.u32s(&mut self.count);
        r.bytes(&mut self.buffer);
    }
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! SHA-1 (ref: ext/standard/sha1.c).

use super::{be_words, md_update32, put_be32, HashState, ImageReader, ImageWriter, PADDING};

/// `PHP_SHA1_CTX`.
#[derive(Clone, Debug)]
pub(crate) struct Sha1 {
    state: [u32; 5],
    count: [u32; 2],
    buffer: [u8; 64],
}

impl Sha1 {
    pub(crate) fn new() -> Self {
        Self {
            state: [
                0x6745_2301,
                0xefcd_ab89,
                0x98ba_dcfe,
                0x1032_5476,
                0xc3d2_e1f0,
            ],
            count: [0; 2],
            buffer: [0; 64],
        }
    }

    fn transform(state: &mut [u32; 5], block: &[u8]) {
        let mut w = [0u32; 80];
        w[..16].copy_from_slice(&be_words::<16>(block));
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = *state;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i / 20 {
                0 => ((b & c) | (!b & d), 0x5A82_7999),
                1 => (b ^ c ^ d, 0x6ED9_EBA1),
                2 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
    }
}

impl HashState for Sha1 {
    fn update(&mut self, data: &[u8]) {
        let state = &mut self.state;
        md_update32(&mut self.count, &mut self.buffer, data, |b| {
            Self::transform(state, b);
        });
    }

    fn finish(&mut self, out: &mut [u8]) {
        let mut bits = [0u8; 8];
        put_be32(&mut bits, &[self.count[1], self.count[0]]);
        let index = ((self.count[0] >> 3) & 0x3f) as usize;
        let pad_len = if index < 56 { 56 - index } else { 120 - index };
        self.update(&PADDING[..pad_len]);
        self.update(&bits);
        put_be32(out, &self.state);
    }

    fn image(&self) -> Vec<u8> {
        ImageWriter::new()
            .u32s(&self.state)
            .u32s(&self.count)
            .bytes(&self.buffer)
            .finish(92)
    }

    fn load_image(&mut self, image: &[u8]) {
        let mut r = ImageReader::new(image);
        r.u32s(&mut self.state);
        r.u32s(&mut self.count);
        r.bytes(&mut self.buffer);
    }
}

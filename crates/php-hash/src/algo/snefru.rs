// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Snefru-256 (ref: `ext/hash/hash_snefru.c`).

use super::snefru_tables::SNEFRU_TABLES;
use super::{HashState, ImageReader, ImageWriter};

fn snefru(input: &mut [u32; 16]) {
    const SHIFTS: [u32; 4] = [16, 8, 16, 24];
    let mut b = *input;
    for index in 0..8 {
        let t0 = &SNEFRU_TABLES[(2 * index) * 256..(2 * index + 1) * 256];
        let t1 = &SNEFRU_TABLES[(2 * index + 1) * 256..(2 * index + 2) * 256];
        for &shift in &SHIFTS {
            // round(L, C, N, SB): SBE = SB[C & 0xff]; L ^= SBE; N ^= SBE
            for i in 0..16 {
                let l = (i + 15) % 16;
                let n = (i + 1) % 16;
                let table = if (i / 2) % 2 == 0 { t0 } else { t1 };
                let sbe = table[(b[i] & 0xff) as usize];
                b[l] ^= sbe;
                b[n] ^= sbe;
            }
            for w in &mut b {
                *w = w.rotate_right(shift);
            }
        }
    }
    for i in 0..8 {
        input[i] ^= b[15 - i];
    }
}

/// `PHP_SNEFRU_CTX`.
#[derive(Clone, Debug)]
pub(crate) struct Snefru {
    state: [u32; 16],
    count: [u32; 2],
    length: u8,
    buffer: [u8; 32],
}

impl Snefru {
    pub(crate) fn new() -> Self {
        Self {
            state: [0; 16],
            count: [0; 2],
            length: 0,
            buffer: [0; 32],
        }
    }

    fn transform(state: &mut [u32; 16], input: &[u8]) {
        for (j, c) in input.chunks_exact(4).enumerate() {
            state[8 + j] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        }
        snefru(state);
        state[8..].fill(0);
    }
}

impl HashState for Snefru {
    fn update(&mut self, input: &[u8]) {
        let len = input.len();
        let count = ((u64::from(self.count[0]) << 32) | u64::from(self.count[1]))
            .wrapping_add((len as u64).wrapping_mul(8));
        #[allow(clippy::cast_possible_truncation)]
        {
            self.count[0] = (count >> 32) as u32;
            self.count[1] = count as u32;
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
                Self::transform(&mut self.state, &block);
            }
            while i + 32 <= len {
                Self::transform(&mut self.state, &input[i..i + 32]);
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
            Self::transform(&mut self.state, &block);
        }
        self.state[14] = self.count[0];
        self.state[15] = self.count[1];
        snefru(&mut self.state);
        super::put_be32(out, &self.state[..8]);
    }

    fn image(&self) -> Vec<u8> {
        ImageWriter::new()
            .u32s(&self.state)
            .u32s(&self.count)
            .u8(self.length)
            .bytes(&self.buffer)
            .finish(108)
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

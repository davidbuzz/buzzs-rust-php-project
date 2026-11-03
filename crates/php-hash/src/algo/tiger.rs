//! Tiger with 3 and 4 passes (ref: `ext/hash/hash_tiger.c`).

use super::tiger_tables::TIGER_TABLE;
use super::{HashState, ImageReader, ImageWriter};

#[inline]
fn t(n: usize, idx: u64) -> u64 {
    TIGER_TABLE[n * 256 + (idx & 0xff) as usize]
}

#[inline]
fn round(a: &mut u64, b: &mut u64, c: &mut u64, x: u64, mul: u64) {
    *c ^= x;
    let cv = *c;
    *a = a.wrapping_sub(t(0, cv) ^ t(1, cv >> 16) ^ t(2, cv >> 32) ^ t(3, cv >> 48));
    *b = b.wrapping_add(t(3, cv >> 8) ^ t(2, cv >> 24) ^ t(1, cv >> 40) ^ t(0, cv >> 56));
    *b = b.wrapping_mul(mul);
}

fn pass(a: &mut u64, b: &mut u64, c: &mut u64, x: &[u64; 8], mul: u64) {
    round(a, b, c, x[0], mul);
    round(b, c, a, x[1], mul);
    round(c, a, b, x[2], mul);
    round(a, b, c, x[3], mul);
    round(b, c, a, x[4], mul);
    round(c, a, b, x[5], mul);
    round(a, b, c, x[6], mul);
    round(b, c, a, x[7], mul);
}

fn key_schedule(x: &mut [u64; 8]) {
    x[0] = x[0].wrapping_sub(x[7] ^ 0xA5A5_A5A5_A5A5_A5A5);
    x[1] ^= x[0];
    x[2] = x[2].wrapping_add(x[1]);
    x[3] = x[3].wrapping_sub(x[2] ^ ((!x[1]) << 19));
    x[4] ^= x[3];
    x[5] = x[5].wrapping_add(x[4]);
    x[6] = x[6].wrapping_sub(x[5] ^ ((!x[4]) >> 23));
    x[7] ^= x[6];
    x[0] = x[0].wrapping_add(x[7]);
    x[1] = x[1].wrapping_sub(x[0] ^ ((!x[7]) << 19));
    x[2] ^= x[1];
    x[3] = x[3].wrapping_add(x[2]);
    x[4] = x[4].wrapping_sub(x[3] ^ ((!x[2]) >> 23));
    x[5] ^= x[4];
    x[6] = x[6].wrapping_add(x[5]);
    x[7] = x[7].wrapping_sub(x[6] ^ 0x0123_4567_89AB_CDEF);
}

fn compress(state: &mut [u64; 3], extra_passes: u32, block: &[u8]) {
    let mut x = [0u64; 8];
    for (xi, ch) in x.iter_mut().zip(block.chunks_exact(8)) {
        *xi = u64::from_le_bytes([ch[0], ch[1], ch[2], ch[3], ch[4], ch[5], ch[6], ch[7]]);
    }
    let [mut a, mut b, mut c] = *state;
    let (aa, bb, cc) = (a, b, c);
    pass(&mut a, &mut b, &mut c, &x, 5);
    key_schedule(&mut x);
    pass(&mut c, &mut a, &mut b, &x, 7);
    key_schedule(&mut x);
    pass(&mut b, &mut c, &mut a, &x, 9);
    for _ in 0..extra_passes {
        key_schedule(&mut x);
        pass(&mut a, &mut b, &mut c, &x, 9);
        let tmpa = a;
        a = c;
        c = b;
        b = tmpa;
    }
    a ^= aa;
    b = b.wrapping_sub(bb);
    c = c.wrapping_add(cc);
    *state = [a, b, c];
}

/// `PHP_TIGER_CTX`.
#[derive(Clone, Debug)]
pub(crate) struct Tiger {
    state: [u64; 3],
    passed: u64,
    buffer: [u8; 64],
    length: u32,
    /// The `passes:1` bit field: 0 for 3 passes, 1 for 4 passes.
    passes: u32,
}

impl Tiger {
    pub(crate) fn new(four_passes: bool) -> Self {
        Self {
            state: [
                0x0123_4567_89AB_CDEF,
                0xFEDC_BA98_7654_3210,
                0xF096_A5B4_C3B2_E187,
            ],
            passed: 0,
            buffer: [0; 64],
            length: 0,
            passes: u32::from(four_passes),
        }
    }
}

impl HashState for Tiger {
    fn update(&mut self, input: &[u8]) {
        let len = input.len();
        let length = self.length as usize;
        if length + len < 64 {
            self.buffer[length..length + len].copy_from_slice(input);
            #[allow(clippy::cast_possible_truncation)]
            {
                self.length += len as u32;
            }
        } else {
            let mut i = 0;
            let r = (length + len) % 64;
            if length != 0 {
                i = 64 - length;
                self.buffer[length..].copy_from_slice(&input[..i]);
                let block = self.buffer;
                compress(&mut self.state, self.passes, &block);
                self.buffer = [0; 64];
                self.passed = self.passed.wrapping_add(512);
            }
            while i + 64 <= len {
                self.buffer.copy_from_slice(&input[i..i + 64]);
                let block = self.buffer;
                compress(&mut self.state, self.passes, &block);
                self.passed = self.passed.wrapping_add(512);
                i += 64;
            }
            self.buffer[r..].fill(0);
            self.buffer[..r].copy_from_slice(&input[i..i + r]);
            #[allow(clippy::cast_possible_truncation)]
            {
                self.length = r as u32;
            }
        }
    }

    fn finish(&mut self, out: &mut [u8]) {
        self.passed = self.passed.wrapping_add(u64::from(self.length) << 3);
        let mut length = self.length as usize;
        self.buffer[length] = 0x1;
        length += 1;
        if length % 8 != 0 {
            let pad = 8 - length % 8;
            self.buffer[length..length + pad].fill(0);
            length += pad;
        }
        if length > 56 {
            self.buffer[length..].fill(0);
            let block = self.buffer;
            compress(&mut self.state, self.passes, &block);
            self.buffer[..56].fill(0);
        } else {
            self.buffer[length..56].fill(0);
        }
        self.buffer[56..].copy_from_slice(&self.passed.to_le_bytes());
        let block = self.buffer;
        compress(&mut self.state, self.passes, &block);
        for (i, o) in out.iter_mut().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            {
                *o = (self.state[i / 8] >> (8 * (i % 8))) as u8;
            }
        }
    }

    fn image(&self) -> Vec<u8> {
        ImageWriter::new()
            .u64s(&self.state)
            .u64(self.passed)
            .bytes(&self.buffer)
            .u32(self.length)
            .u32(self.passes)
            .finish(104)
    }

    fn load_image(&mut self, image: &[u8]) {
        let mut r = ImageReader::new(image);
        r.u64s(&mut self.state);
        self.passed = r.u64();
        r.bytes(&mut self.buffer);
        self.length = r.u32();
        self.passes = r.u32() & 1;
    }

    fn is_valid(&self) -> bool {
        (self.length as usize) < self.buffer.len()
    }
}

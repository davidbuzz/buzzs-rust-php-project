//! SHA3-224/256/384/512 (ref: `ext/hash/hash_sha3.c` with the bundled XKCP
//! `generic64lc` Keccak implementation).
//!
//! The state is kept in the canonical (non-complemented) form; the lane
//! complementing representation used by XKCP only matters for
//! serialization, where it is reproduced by [`COMPLEMENTED_LANES`].

use super::{HashState, ImageReader, ImageWriter};

const ROUND_CONSTANTS: [u64; 24] = [
    0x0000_0000_0000_0001,
    0x0000_0000_0000_8082,
    0x8000_0000_0000_808A,
    0x8000_0000_8000_8000,
    0x0000_0000_0000_808B,
    0x0000_0000_8000_0001,
    0x8000_0000_8000_8081,
    0x8000_0000_0000_8009,
    0x0000_0000_0000_008A,
    0x0000_0000_0000_0088,
    0x0000_0000_8000_8009,
    0x0000_0000_8000_000A,
    0x0000_0000_8000_808B,
    0x8000_0000_0000_008B,
    0x8000_0000_0000_8089,
    0x8000_0000_0000_8003,
    0x8000_0000_0000_8002,
    0x8000_0000_0000_0080,
    0x0000_0000_0000_800A,
    0x8000_0000_8000_000A,
    0x8000_0000_8000_8081,
    0x8000_0000_0000_8080,
    0x0000_0000_8000_0001,
    0x8000_0000_8000_8008,
];

const RHO: [u32; 25] = [
    0, 1, 62, 28, 27, 36, 44, 6, 55, 20, 3, 10, 43, 25, 39, 41, 45, 15, 21, 8, 18, 2, 61, 56, 14,
];

/// Lanes stored complemented by XKCP's lane-complementing implementation.
const COMPLEMENTED_LANES: [usize; 6] = [1, 2, 8, 12, 17, 20];

/// The `PHP_HASH_SERIALIZE_MAGIC_KECCAK` value for 64-bit builds.
pub(crate) const KECCAK_MAGIC: i64 = 100;

fn keccak_f1600(a: &mut [u64; 25]) {
    for &rc in &ROUND_CONSTANTS {
        // θ
        let mut c = [0u64; 5];
        for x in 0..5 {
            c[x] = a[x] ^ a[x + 5] ^ a[x + 10] ^ a[x + 15] ^ a[x + 20];
        }
        for x in 0..5 {
            let d = c[(x + 4) % 5] ^ c[(x + 1) % 5].rotate_left(1);
            for y in 0..5 {
                a[x + 5 * y] ^= d;
            }
        }
        // ρ and π
        let mut b = [0u64; 25];
        for x in 0..5 {
            for y in 0..5 {
                b[y + 5 * ((2 * x + 3 * y) % 5)] = a[x + 5 * y].rotate_left(RHO[x + 5 * y]);
            }
        }
        // χ
        for y in 0..5 {
            for x in 0..5 {
                a[x + 5 * y] = b[x + 5 * y] ^ (!b[(x + 1) % 5 + 5 * y] & b[(x + 2) % 5 + 5 * y]);
            }
        }
        // ι
        a[0] ^= rc;
    }
}

/// `Keccak_HashInstance` configured for SHA3-`bits`.
#[derive(Clone, Debug)]
pub(crate) struct Sha3 {
    lanes: [u64; 25],
    /// Rate in bits.
    rate: u32,
    byte_io_index: u32,
    squeezing: u32,
    fixed_output_length: u32,
}

impl Sha3 {
    pub(crate) fn new(bits: u32) -> Self {
        Self {
            lanes: [0; 25],
            rate: 1600 - 2 * bits,
            byte_io_index: 0,
            squeezing: 0,
            fixed_output_length: bits,
        }
    }

    fn rate_bytes(&self) -> usize {
        (self.rate / 8) as usize
    }

    fn xor_byte(&mut self, pos: usize, b: u8) {
        self.lanes[pos / 8] ^= u64::from(b) << (8 * (pos % 8));
    }
}

impl HashState for Sha3 {
    fn update(&mut self, data: &[u8]) {
        let rate = self.rate_bytes();
        for &b in data {
            self.xor_byte(self.byte_io_index as usize, b);
            self.byte_io_index += 1;
            if self.byte_io_index as usize == rate {
                keccak_f1600(&mut self.lanes);
                self.byte_io_index = 0;
            }
        }
    }

    fn finish(&mut self, out: &mut [u8]) {
        let rate = self.rate_bytes();
        // Delimited suffix 0x06 followed by the final bit of pad10*1.
        self.xor_byte(self.byte_io_index as usize, 0x06);
        self.xor_byte(rate - 1, 0x80);
        keccak_f1600(&mut self.lanes);
        let mut bytes = [0u8; 200];
        for (c, l) in bytes.chunks_exact_mut(8).zip(self.lanes) {
            c.copy_from_slice(&l.to_le_bytes());
        }
        // All SHA3 digests are shorter than the rate, so one squeeze suffices.
        out.copy_from_slice(&bytes[..out.len()]);
    }

    fn image(&self) -> Vec<u8> {
        let mut lanes = self.lanes;
        for &i in &COMPLEMENTED_LANES {
            lanes[i] = !lanes[i];
        }
        ImageWriter::new()
            .u64s(&lanes)
            .u32(self.rate)
            .u32(self.byte_io_index)
            .u32(self.squeezing)
            .u32(self.fixed_output_length)
            .u8(0x06)
            .finish(224)
    }

    fn load_image(&mut self, image: &[u8]) {
        let mut r = ImageReader::new(image);
        r.u64s(&mut self.lanes);
        for &i in &COMPLEMENTED_LANES {
            self.lanes[i] = !self.lanes[i];
        }
        self.rate = r.u32();
        self.byte_io_index = r.u32();
        self.squeezing = r.u32();
        self.fixed_output_length = r.u32();
    }

    fn is_valid(&self) -> bool {
        self.byte_io_index < self.rate / 8
    }
}

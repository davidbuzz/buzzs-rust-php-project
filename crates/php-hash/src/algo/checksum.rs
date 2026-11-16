// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Non-cryptographic checksums: Adler-32, the three CRC-32 flavours, FNV-1 /
//! FNV-1a and Jenkins one-at-a-time (ref: ext/hash/hash_{adler32,crc32,fnv,joaat}.c).

use super::{HashState, ImageReader, ImageWriter};

/// Builds a reflected (LSB-first) CRC-32 table for `poly`.
const fn reflected_table(poly: u32) -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        #[allow(clippy::cast_possible_truncation)]
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { (c >> 1) ^ poly } else { c >> 1 };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

/// Builds a normal (MSB-first) CRC-32 table for `poly`.
const fn normal_table(poly: u32) -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        #[allow(clippy::cast_possible_truncation)]
        let mut c = (i as u32) << 24;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ poly
            } else {
                c << 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

/// `crc32_table`: the bzip2 CRC (polynomial 0x04C11DB7, MSB first).
pub(crate) static CRC32_TABLE: [u32; 256] = normal_table(0x04C1_1DB7);
/// `crc32b_table`: the ITU-T V.42 / zlib CRC (reflected 0xEDB88320).
pub(crate) static CRC32B_TABLE: [u32; 256] = reflected_table(0xEDB8_8320);
/// `crc32c_table`: Castagnoli (reflected 0x82F63B78).
pub(crate) static CRC32C_TABLE: [u32; 256] = reflected_table(0x82F6_3B78);

/// Updates a raw (non-inverted) zlib-style CRC-32 register; this is what
/// `crc32()` and `hash('crc32b')` compute.
pub(crate) fn crc32b_update(mut state: u32, data: &[u8]) -> u32 {
    for &b in data {
        state = (state >> 8) ^ CRC32B_TABLE[((state ^ u32::from(b)) & 0xff) as usize];
    }
    state
}

/// Which of the 4-byte checksums a [`Checksum32`] computes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind32 {
    Adler32,
    Crc32,
    Crc32b,
    Crc32c,
    Fnv132,
    Fnv1a32,
    Joaat,
}

const FNV_32_PRIME: u32 = 0x0100_0193;
const FNV1_32_INIT: u32 = 0x811c_9dc5;
const FNV_64_PRIME: u64 = 0x0000_0100_0000_01b3;
const FNV1_64_INIT: u64 = 0xcbf2_9ce4_8422_2325;

/// Contexts consisting of a single `uint32_t state` (spec `"l."`).
#[derive(Clone, Debug)]
pub(crate) struct Checksum32 {
    kind: Kind32,
    state: u32,
}

impl Checksum32 {
    pub(crate) fn new(kind: Kind32) -> Self {
        let state = match kind {
            Kind32::Adler32 => 1,
            Kind32::Crc32 | Kind32::Crc32b | Kind32::Crc32c => !0,
            Kind32::Fnv132 | Kind32::Fnv1a32 => FNV1_32_INIT,
            Kind32::Joaat => 0,
        };
        Self { kind, state }
    }
}

impl HashState for Checksum32 {
    fn update(&mut self, data: &[u8]) {
        match self.kind {
            Kind32::Adler32 => {
                let mut s0 = self.state & 0xffff;
                let mut s1 = (self.state >> 16) & 0xffff;
                for &b in data {
                    s0 += u32::from(b);
                    s1 += s0;
                    if s1 >= 0x7fff_ffff {
                        s0 %= 65521;
                        s1 %= 65521;
                    }
                }
                s0 %= 65521;
                s1 %= 65521;
                self.state = s0 + (s1 << 16);
            }
            Kind32::Crc32 => {
                for &b in data {
                    self.state = (self.state << 8)
                        ^ CRC32_TABLE[((self.state >> 24) ^ u32::from(b)) as usize & 0xff];
                }
            }
            Kind32::Crc32b => self.state = crc32b_update(self.state, data),
            Kind32::Crc32c => {
                for &b in data {
                    self.state = (self.state >> 8)
                        ^ CRC32C_TABLE[((self.state ^ u32::from(b)) & 0xff) as usize];
                }
            }
            Kind32::Fnv132 => {
                for &b in data {
                    self.state = self.state.wrapping_mul(FNV_32_PRIME) ^ u32::from(b);
                }
            }
            Kind32::Fnv1a32 => {
                for &b in data {
                    self.state = (self.state ^ u32::from(b)).wrapping_mul(FNV_32_PRIME);
                }
            }
            Kind32::Joaat => {
                let mut h = self.state;
                for &b in data {
                    h = h.wrapping_add(u32::from(b));
                    h = h.wrapping_add(h << 10);
                    h ^= h >> 6;
                }
                self.state = h;
            }
        }
    }

    fn finish(&mut self, out: &mut [u8]) {
        let v = match self.kind {
            // PHP's "crc32" final writes the bytes in little-endian order:
            // the infamous byte order quirk.
            Kind32::Crc32 => (!self.state).swap_bytes(),
            Kind32::Crc32b | Kind32::Crc32c => !self.state,
            Kind32::Adler32 | Kind32::Fnv132 | Kind32::Fnv1a32 => self.state,
            Kind32::Joaat => {
                let mut h = self.state;
                h = h.wrapping_add(h << 3);
                h ^= h >> 11;
                h = h.wrapping_add(h << 15);
                h
            }
        };
        out.copy_from_slice(&v.to_be_bytes());
    }

    fn image(&self) -> Vec<u8> {
        ImageWriter::new().u32(self.state).finish(4)
    }

    fn load_image(&mut self, image: &[u8]) {
        self.state = ImageReader::new(image).u32();
    }
}

/// `PHP_FNV164_CTX` (spec `"q."`).
#[derive(Clone, Debug)]
pub(crate) struct Fnv64 {
    alternate: bool,
    state: u64,
}

impl Fnv64 {
    pub(crate) fn new(alternate: bool) -> Self {
        Self {
            alternate,
            state: FNV1_64_INIT,
        }
    }
}

impl HashState for Fnv64 {
    fn update(&mut self, data: &[u8]) {
        if self.alternate {
            for &b in data {
                self.state = (self.state ^ u64::from(b)).wrapping_mul(FNV_64_PRIME);
            }
        } else {
            for &b in data {
                self.state = self.state.wrapping_mul(FNV_64_PRIME) ^ u64::from(b);
            }
        }
    }

    fn finish(&mut self, out: &mut [u8]) {
        out.copy_from_slice(&self.state.to_be_bytes());
    }

    fn image(&self) -> Vec<u8> {
        ImageWriter::new().u64(self.state).finish(8)
    }

    fn load_image(&mut self, image: &[u8]) {
        self.state = ImageReader::new(image).u64();
    }
}

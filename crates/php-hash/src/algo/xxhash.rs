//! xxHash: XXH32, XXH64 (own streaming ports with the exact
//! `XXH32_state_t`/`XXH64_state_t` layouts, needed for serialization) and
//! XXH3-64/XXH3-128 (via `twox-hash`; PHP does not serialize those)
//! (ref: `ext/hash/hash_xxhash.c`, ext/hash/xxhash/xxhash.h).

use super::{HashState, ImageReader, ImageWriter};

const P32_1: u32 = 0x9E37_79B1;
const P32_2: u32 = 0x85EB_CA77;
const P32_3: u32 = 0xC2B2_AE3D;
const P32_4: u32 = 0x27D4_EB2F;
const P32_5: u32 = 0x1656_67B1;

const P64_1: u64 = 0x9E37_79B1_85EB_CA87;
const P64_2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const P64_3: u64 = 0x1656_67B1_9E37_79F9;
const P64_4: u64 = 0x85EB_CA77_C2B2_AE63;
const P64_5: u64 = 0x27D4_EB2F_1656_67C5;

/// Minimum XXH3 secret size (`XXH3_SECRET_SIZE_MIN`).
pub(crate) const XXH3_SECRET_SIZE_MIN: usize = 136;
/// Size of the secret buffer kept in `PHP_XXH3_CTX`.
pub(crate) const XXH3_SECRET_SIZE_MAX: usize = 256;

fn le32(p: &[u8]) -> u32 {
    u32::from_le_bytes([p[0], p[1], p[2], p[3]])
}

fn le64(p: &[u8]) -> u64 {
    u64::from_le_bytes([p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7]])
}

fn round32(acc: u32, input: u32) -> u32 {
    acc.wrapping_add(input.wrapping_mul(P32_2))
        .rotate_left(13)
        .wrapping_mul(P32_1)
}

fn round64(acc: u64, input: u64) -> u64 {
    acc.wrapping_add(input.wrapping_mul(P64_2))
        .rotate_left(31)
        .wrapping_mul(P64_1)
}

fn merge_round64(acc: u64, val: u64) -> u64 {
    (acc ^ round64(0, val))
        .wrapping_mul(P64_1)
        .wrapping_add(P64_4)
}

/// `XXH32_state_t`.
#[derive(Clone, Debug)]
pub(crate) struct Xxh32 {
    total_len_32: u32,
    large_len: u32,
    v: [u32; 4],
    mem: [u8; 16],
    memsize: u32,
    reserved: u32,
}

impl Xxh32 {
    pub(crate) fn new(seed: u32) -> Self {
        Self {
            total_len_32: 0,
            large_len: 0,
            v: [
                seed.wrapping_add(P32_1).wrapping_add(P32_2),
                seed.wrapping_add(P32_2),
                seed,
                seed.wrapping_sub(P32_1),
            ],
            mem: [0; 16],
            memsize: 0,
            reserved: 0,
        }
    }
}

impl HashState for Xxh32 {
    #[allow(clippy::cast_possible_truncation)]
    fn update(&mut self, input: &[u8]) {
        let len = input.len();
        self.total_len_32 = self.total_len_32.wrapping_add(len as u32);
        self.large_len |= u32::from(len >= 16 || self.total_len_32 >= 16);
        let memsize = self.memsize as usize;
        if memsize + len < 16 {
            self.mem[memsize..memsize + len].copy_from_slice(input);
            self.memsize += len as u32;
            return;
        }
        let mut p = 0;
        if memsize != 0 {
            self.mem[memsize..].copy_from_slice(&input[..16 - memsize]);
            for i in 0..4 {
                self.v[i] = round32(self.v[i], le32(&self.mem[4 * i..]));
            }
            p = 16 - memsize;
            self.memsize = 0;
        }
        while p + 16 <= len {
            for i in 0..4 {
                self.v[i] = round32(self.v[i], le32(&input[p + 4 * i..]));
            }
            p += 16;
        }
        if p < len {
            self.mem[..len - p].copy_from_slice(&input[p..]);
            self.memsize = (len - p) as u32;
        }
    }

    fn finish(&mut self, out: &mut [u8]) {
        let mut h32 = if self.large_len != 0 {
            self.v[0]
                .rotate_left(1)
                .wrapping_add(self.v[1].rotate_left(7))
                .wrapping_add(self.v[2].rotate_left(12))
                .wrapping_add(self.v[3].rotate_left(18))
        } else {
            self.v[2].wrapping_add(P32_5)
        };
        h32 = h32.wrapping_add(self.total_len_32);
        let mut len = (self.memsize & 15) as usize;
        let mut p = 0;
        while len >= 4 {
            h32 = h32.wrapping_add(le32(&self.mem[p..]).wrapping_mul(P32_3));
            h32 = h32.rotate_left(17).wrapping_mul(P32_4);
            p += 4;
            len -= 4;
        }
        while len > 0 {
            h32 = h32.wrapping_add(u32::from(self.mem[p]).wrapping_mul(P32_5));
            h32 = h32.rotate_left(11).wrapping_mul(P32_1);
            p += 1;
            len -= 1;
        }
        h32 ^= h32 >> 15;
        h32 = h32.wrapping_mul(P32_2);
        h32 ^= h32 >> 13;
        h32 = h32.wrapping_mul(P32_3);
        h32 ^= h32 >> 16;
        out.copy_from_slice(&h32.to_be_bytes());
    }

    fn image(&self) -> Vec<u8> {
        ImageWriter::new()
            .u32(self.total_len_32)
            .u32(self.large_len)
            .u32s(&self.v)
            .bytes(&self.mem)
            .u32(self.memsize)
            .u32(self.reserved)
            .finish(48)
    }

    fn load_image(&mut self, image: &[u8]) {
        let mut r = ImageReader::new(image);
        self.total_len_32 = r.u32();
        self.large_len = r.u32();
        r.u32s(&mut self.v);
        r.bytes(&mut self.mem);
        self.memsize = r.u32();
        self.reserved = r.u32();
    }

    fn is_valid(&self) -> bool {
        self.memsize < 16
    }
}

/// `XXH64_state_t`.
#[derive(Clone, Debug)]
pub(crate) struct Xxh64 {
    total_len: u64,
    v: [u64; 4],
    mem: [u8; 32],
    memsize: u32,
    reserved32: u32,
    reserved64: u64,
}

impl Xxh64 {
    pub(crate) fn new(seed: u64) -> Self {
        Self {
            total_len: 0,
            v: [
                seed.wrapping_add(P64_1).wrapping_add(P64_2),
                seed.wrapping_add(P64_2),
                seed,
                seed.wrapping_sub(P64_1),
            ],
            mem: [0; 32],
            memsize: 0,
            reserved32: 0,
            reserved64: 0,
        }
    }
}

impl HashState for Xxh64 {
    #[allow(clippy::cast_possible_truncation)]
    fn update(&mut self, input: &[u8]) {
        let len = input.len();
        self.total_len = self.total_len.wrapping_add(len as u64);
        let memsize = self.memsize as usize;
        if memsize + len < 32 {
            self.mem[memsize..memsize + len].copy_from_slice(input);
            self.memsize += len as u32;
            return;
        }
        let mut p = 0;
        if memsize != 0 {
            self.mem[memsize..].copy_from_slice(&input[..32 - memsize]);
            for i in 0..4 {
                self.v[i] = round64(self.v[i], le64(&self.mem[8 * i..]));
            }
            p = 32 - memsize;
            self.memsize = 0;
        }
        while p + 32 <= len {
            for i in 0..4 {
                self.v[i] = round64(self.v[i], le64(&input[p + 8 * i..]));
            }
            p += 32;
        }
        if p < len {
            self.mem[..len - p].copy_from_slice(&input[p..]);
            self.memsize = (len - p) as u32;
        }
    }

    fn finish(&mut self, out: &mut [u8]) {
        let mut h64 = if self.total_len >= 32 {
            let mut h = self.v[0]
                .rotate_left(1)
                .wrapping_add(self.v[1].rotate_left(7))
                .wrapping_add(self.v[2].rotate_left(12))
                .wrapping_add(self.v[3].rotate_left(18));
            for &v in &self.v {
                h = merge_round64(h, v);
            }
            h
        } else {
            self.v[2].wrapping_add(P64_5)
        };
        h64 = h64.wrapping_add(self.total_len);
        let mut len = (self.total_len & 31) as usize;
        let mut p = 0;
        while len >= 8 {
            h64 ^= round64(0, le64(&self.mem[p..]));
            h64 = h64.rotate_left(27).wrapping_mul(P64_1).wrapping_add(P64_4);
            p += 8;
            len -= 8;
        }
        if len >= 4 {
            h64 ^= u64::from(le32(&self.mem[p..])).wrapping_mul(P64_1);
            h64 = h64.rotate_left(23).wrapping_mul(P64_2).wrapping_add(P64_3);
            p += 4;
            len -= 4;
        }
        while len > 0 {
            h64 ^= u64::from(self.mem[p]).wrapping_mul(P64_5);
            h64 = h64.rotate_left(11).wrapping_mul(P64_1);
            p += 1;
            len -= 1;
        }
        h64 ^= h64 >> 33;
        h64 = h64.wrapping_mul(P64_2);
        h64 ^= h64 >> 29;
        h64 = h64.wrapping_mul(P64_3);
        h64 ^= h64 >> 32;
        out.copy_from_slice(&h64.to_be_bytes());
    }

    fn image(&self) -> Vec<u8> {
        ImageWriter::new()
            .u64(self.total_len)
            .u64s(&self.v)
            .bytes(&self.mem)
            .u32(self.memsize)
            .u32(self.reserved32)
            .u64(self.reserved64)
            .finish(88)
    }

    fn load_image(&mut self, image: &[u8]) {
        let mut r = ImageReader::new(image);
        self.total_len = r.u64();
        r.u64s(&mut self.v);
        r.bytes(&mut self.mem);
        self.memsize = r.u32();
        self.reserved32 = r.u32();
        self.reserved64 = r.u64();
    }

    fn is_valid(&self) -> bool {
        self.memsize < 32
    }
}

/// How an XXH3 context was seeded.
#[derive(Clone, Debug)]
pub(crate) enum Xxh3Init {
    Seed(u64),
    /// Custom secret (already truncated to [`XXH3_SECRET_SIZE_MAX`] bytes and
    /// at least [`XXH3_SECRET_SIZE_MIN`] long).
    Secret(Vec<u8>),
}

#[derive(Clone)]
enum Xxh3Inner {
    H64(twox_hash::XxHash3_64),
    H128(twox_hash::XxHash3_128),
}

/// `PHP_XXH3_64_CTX` / `PHP_XXH3_128_CTX`.
#[derive(Clone)]
pub(crate) struct Xxh3 {
    inner: Xxh3Inner,
}

impl std::fmt::Debug for Xxh3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let bits = match self.inner {
            Xxh3Inner::H64(_) => 64,
            Xxh3Inner::H128(_) => 128,
        };
        f.debug_struct("Xxh3")
            .field("bits", &bits)
            .finish_non_exhaustive()
    }
}

impl Xxh3 {
    /// Creates an XXH3 context; `wide` selects the 128-bit variant.
    pub(crate) fn new(wide: bool, init: &Xxh3Init) -> Self {
        let inner = match init {
            Xxh3Init::Seed(seed) => {
                if wide {
                    Xxh3Inner::H128(twox_hash::XxHash3_128::with_seed(*seed))
                } else {
                    Xxh3Inner::H64(twox_hash::XxHash3_64::with_seed(*seed))
                }
            }
            Xxh3Init::Secret(secret) => {
                let boxed: Box<[u8]> = secret.clone().into_boxed_slice();
                // The caller guarantees the minimum length, so these cannot
                // fail; fall back to the default seed defensively.
                if wide {
                    Xxh3Inner::H128(
                        twox_hash::XxHash3_128::with_seed_and_secret(0, boxed)
                            .unwrap_or_else(|_| twox_hash::XxHash3_128::with_seed(0)),
                    )
                } else {
                    Xxh3Inner::H64(
                        twox_hash::XxHash3_64::with_seed_and_secret(0, boxed)
                            .unwrap_or_else(|_| twox_hash::XxHash3_64::with_seed(0)),
                    )
                }
            }
        };
        Self { inner }
    }
}

impl HashState for Xxh3 {
    fn update(&mut self, data: &[u8]) {
        match &mut self.inner {
            Xxh3Inner::H64(h) => std::hash::Hasher::write(h, data),
            Xxh3Inner::H128(h) => h.write(data),
        }
    }

    fn finish(&mut self, out: &mut [u8]) {
        match &self.inner {
            Xxh3Inner::H64(h) => out.copy_from_slice(&std::hash::Hasher::finish(h).to_be_bytes()),
            Xxh3Inner::H128(h) => out.copy_from_slice(&h.finish_128().to_be_bytes()),
        }
    }
}

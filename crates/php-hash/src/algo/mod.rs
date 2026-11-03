//! The individual hash algorithms.
//!
//! Every algorithm keeps its running state in fields that mirror the C
//! context struct used by php-src, so that `HashContext::__serialize()` can
//! reproduce PHP's serialization byte for byte: [`HashState::image`] renders
//! the state as the in-memory image of the C struct (x86-64 layout, little
//! endian) and [`HashState::load_image`] parses it back.

pub(crate) mod checksum;
pub(crate) mod gost;
pub(crate) mod haval;
pub(crate) mod md;
pub(crate) mod murmur;
pub(crate) mod ripemd;
pub(crate) mod sha1;
pub(crate) mod sha2;
pub(crate) mod sha3;
pub(crate) mod snefru;
pub(crate) mod tiger;
pub(crate) mod whirlpool;
pub(crate) mod xxhash;

mod gost_tables;
mod snefru_tables;
mod tiger_tables;
mod whirlpool_tables;

use std::fmt::Debug;

/// Running state of one hash computation (the `void *context` of a
/// `php_hash_ops`).
pub(crate) trait HashState: StateClone + Debug + Send + Sync {
    /// Absorbs more input (`hash_update`).
    fn update(&mut self, data: &[u8]);

    /// Finishes the computation and writes exactly `digest_size` bytes into
    /// `out` (`hash_final`). The state must not be used afterwards except to
    /// be dropped or re-initialised.
    fn finish(&mut self, out: &mut [u8]);

    /// Renders the state as the byte image of the corresponding C struct.
    /// Algorithms that cannot be serialized return an empty vector.
    fn image(&self) -> Vec<u8> {
        Vec::new()
    }

    /// Loads the state from a byte image previously produced by
    /// [`HashState::image`] (and possibly patched by the unserializer).
    fn load_image(&mut self, _image: &[u8]) {}

    /// Post-unserialization sanity check (e.g. buffer fill counters must be in
    /// range). Mirrors the algorithm-specific `*_unserialize` callbacks.
    fn is_valid(&self) -> bool {
        true
    }
}

/// Object-safe cloning for boxed [`HashState`]s.
pub(crate) trait StateClone {
    fn box_clone(&self) -> Box<dyn HashState>;
}

impl<T: HashState + Clone + 'static> StateClone for T {
    fn box_clone(&self) -> Box<dyn HashState> {
        Box::new(self.clone())
    }
}

/// Sequential writer producing a C struct image.
#[derive(Debug, Default)]
pub(crate) struct ImageWriter(Vec<u8>);

impl ImageWriter {
    pub(crate) fn new() -> Self {
        Self(Vec::new())
    }

    /// Pads with zero bytes up to the given alignment.
    pub(crate) fn align(&mut self, alignment: usize) -> &mut Self {
        while self.0.len() % alignment != 0 {
            self.0.push(0);
        }
        self
    }

    pub(crate) fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }

    pub(crate) fn u16(&mut self, v: u16) -> &mut Self {
        self.align(2);
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub(crate) fn u32(&mut self, v: u32) -> &mut Self {
        self.align(4);
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub(crate) fn u32s(&mut self, vs: &[u32]) -> &mut Self {
        for &v in vs {
            self.u32(v);
        }
        self
    }

    pub(crate) fn u64(&mut self, v: u64) -> &mut Self {
        self.align(8);
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub(crate) fn u64s(&mut self, vs: &[u64]) -> &mut Self {
        for &v in vs {
            self.u64(v);
        }
        self
    }

    pub(crate) fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.0.extend_from_slice(b);
        self
    }

    /// Finishes the image, padding it to `size` bytes.
    pub(crate) fn finish(&mut self, size: usize) -> Vec<u8> {
        let mut v = std::mem::take(&mut self.0);
        v.resize(size, 0);
        v
    }
}

/// Sequential reader over a C struct image (the inverse of [`ImageWriter`]).
#[derive(Debug)]
pub(crate) struct ImageReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> ImageReader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn align(&mut self, alignment: usize) {
        while self.pos % alignment != 0 {
            self.pos += 1;
        }
    }

    fn take(&mut self, n: usize) -> &'a [u8] {
        let start = self.pos.min(self.buf.len());
        let end = (self.pos + n).min(self.buf.len());
        self.pos += n;
        &self.buf[start..end]
    }

    pub(crate) fn u8(&mut self) -> u8 {
        self.take(1).first().copied().unwrap_or(0)
    }

    pub(crate) fn u32(&mut self) -> u32 {
        self.align(4);
        let mut b = [0u8; 4];
        let s = self.take(4);
        b[..s.len()].copy_from_slice(s);
        u32::from_le_bytes(b)
    }

    pub(crate) fn u32s(&mut self, out: &mut [u32]) {
        for v in out {
            *v = self.u32();
        }
    }

    pub(crate) fn u64(&mut self) -> u64 {
        self.align(8);
        let mut b = [0u8; 8];
        let s = self.take(8);
        b[..s.len()].copy_from_slice(s);
        u64::from_le_bytes(b)
    }

    pub(crate) fn u64s(&mut self, out: &mut [u64]) {
        for v in out {
            *v = self.u64();
        }
    }

    pub(crate) fn bytes(&mut self, out: &mut [u8]) {
        let s = self.take(out.len());
        out[..s.len()].copy_from_slice(s);
    }
}

/// Merkle–Damgård style update shared by the MD4/SHA-1/SHA-256/RIPEMD/HAVAL
/// contexts, which all keep a bit count in two 32-bit words and a block
/// buffer. This is a literal port of the C update loop, so the bytes left in
/// `buffer` (which are visible through serialization) match PHP.
pub(crate) fn md_update32<const B: usize>(
    count: &mut [u32; 2],
    buffer: &mut [u8; B],
    input: &[u8],
    mut transform: impl FnMut(&[u8]),
) {
    let len = input.len();
    let mut index = ((count[0] >> 3) as usize) & (B - 1);
    // `(uint32_t) inputLen << 3`: truncate first, then shift.
    #[allow(clippy::cast_possible_truncation)]
    let bits = (len as u32).wrapping_shl(3);
    count[0] = count[0].wrapping_add(bits);
    if count[0] < bits {
        count[1] = count[1].wrapping_add(1);
    }
    #[allow(clippy::cast_possible_truncation)]
    {
        count[1] = count[1].wrapping_add((len >> 29) as u32);
    }
    let part = B - index;
    let mut i = 0;
    if len >= part {
        buffer[index..].copy_from_slice(&input[..part]);
        transform(&buffer[..]);
        i = part;
        while i + B <= len {
            transform(&input[i..i + B]);
            i += B;
        }
        index = 0;
    }
    buffer[index..index + (len - i)].copy_from_slice(&input[i..]);
}

/// Same as [`md_update32`] but with two 64-bit counter words (SHA-384/512).
pub(crate) fn md_update64<const B: usize>(
    count: &mut [u64; 2],
    buffer: &mut [u8; B],
    input: &[u8],
    mut transform: impl FnMut(&[u8]),
) {
    let len = input.len();
    let mut index = ((count[0] >> 3) as usize) & (B - 1);
    let bits = (len as u64).wrapping_shl(3);
    count[0] = count[0].wrapping_add(bits);
    if count[0] < bits {
        count[1] = count[1].wrapping_add(1);
    }
    count[1] = count[1].wrapping_add((len as u64) >> 61);
    let part = B - index;
    let mut i = 0;
    if len >= part {
        buffer[index..].copy_from_slice(&input[..part]);
        transform(&buffer[..]);
        i = part;
        while i + B <= len {
            transform(&input[i..i + B]);
            i += B;
        }
        index = 0;
    }
    buffer[index..index + (len - i)].copy_from_slice(&input[i..]);
}

/// Decodes little-endian 32-bit words.
pub(crate) fn le_words<const N: usize>(block: &[u8]) -> [u32; N] {
    let mut out = [0u32; N];
    for (w, c) in out.iter_mut().zip(block.chunks_exact(4)) {
        *w = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
    }
    out
}

/// Decodes big-endian 32-bit words.
pub(crate) fn be_words<const N: usize>(block: &[u8]) -> [u32; N] {
    let mut out = [0u32; N];
    for (w, c) in out.iter_mut().zip(block.chunks_exact(4)) {
        *w = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
    }
    out
}

/// Encodes 32-bit words little-endian into `out` (as many as fit).
pub(crate) fn put_le32(out: &mut [u8], words: &[u32]) {
    for (c, w) in out.chunks_mut(4).zip(words) {
        c.copy_from_slice(&w.to_le_bytes()[..c.len()]);
    }
}

/// Encodes 32-bit words big-endian into `out` (as many as fit).
pub(crate) fn put_be32(out: &mut [u8], words: &[u32]) {
    for (c, w) in out.chunks_mut(4).zip(words) {
        c.copy_from_slice(&w.to_be_bytes()[..c.len()]);
    }
}

/// The `PADDING` array used by the MD-family finalizers.
pub(crate) const PADDING: [u8; 128] = {
    let mut p = [0u8; 128];
    p[0] = 0x80;
    p
};

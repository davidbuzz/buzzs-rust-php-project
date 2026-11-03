//! Whirlpool (ref: `ext/hash/hash_whirlpool.c`).

use super::whirlpool_tables::{WHIRLPOOL_C, WHIRLPOOL_RC};
use super::{HashState, ImageReader, ImageWriter};

const DIGESTBITS: i32 = 512;
const WBLOCKBYTES: usize = 64;
const LENGTHBYTES: usize = 32;

#[inline]
fn c(table: usize, idx: u64) -> u64 {
    WHIRLPOOL_C[table * 256 + (idx & 0xff) as usize]
}

/// One application of the Whirlpool round function `ρ[k]` to `s`.
fn rho(s: &[u64; 8], key: &[u64; 8]) -> [u64; 8] {
    let mut l = [0u64; 8];
    for (i, li) in l.iter_mut().enumerate() {
        *li = c(0, s[i] >> 56)
            ^ c(1, s[(i + 7) % 8] >> 48)
            ^ c(2, s[(i + 6) % 8] >> 40)
            ^ c(3, s[(i + 5) % 8] >> 32)
            ^ c(4, s[(i + 4) % 8] >> 24)
            ^ c(5, s[(i + 3) % 8] >> 16)
            ^ c(6, s[(i + 2) % 8] >> 8)
            ^ c(7, s[(i + 1) % 8])
            ^ key[i];
    }
    l
}

/// `PHP_WHIRLPOOL_CTX`.
#[derive(Clone, Debug)]
pub(crate) struct Whirlpool {
    state: [u64; 8],
    bitlength: [u8; 32],
    pos: i32,
    bits: i32,
    data: [u8; 64],
}

impl Whirlpool {
    pub(crate) fn new() -> Self {
        Self {
            state: [0; 8],
            bitlength: [0; 32],
            pos: 0,
            bits: 0,
            data: [0; 64],
        }
    }

    fn transform(&mut self) {
        let mut block = [0u64; 8];
        for (b, chunk) in block.iter_mut().zip(self.data.chunks_exact(8)) {
            *b = u64::from_be_bytes([
                chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
            ]);
        }
        let mut k = self.state;
        let mut state = [0u64; 8];
        for i in 0..8 {
            state[i] = block[i] ^ k[i];
        }
        for &rc in &WHIRLPOOL_RC[1..] {
            let mut round_key = [0u64; 8];
            round_key[0] = rc;
            k = rho(&k, &round_key);
            state = rho(&state, &k);
        }
        for i in 0..8 {
            self.state[i] ^= state[i] ^ block[i];
        }
    }
}

impl HashState for Whirlpool {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss
    )]
    fn update(&mut self, input: &[u8]) {
        // Literal port of the bit-oriented reference update so that the
        // buffer contents (visible through serialization) match PHP.
        let mut source_bits = (input.len() as u64).wrapping_mul(8);
        let mut source_pos = 0usize;
        let source_gap = ((8 - (source_bits as i32 & 7)) & 7) as u32;
        let buffer_rem = (self.bits & 7) as u32;
        let mut buffer_bits = self.bits;
        let mut buffer_pos = self.pos as usize;

        let mut value = source_bits;
        let mut carry: u32 = 0;
        let mut i: i32 = 31;
        while i >= 0 && (carry != 0 || value != 0) {
            let idx = i as usize;
            carry += u32::from(self.bitlength[idx]) + (value as u32 & 0xff);
            self.bitlength[idx] = carry as u8;
            carry >>= 8;
            value >>= 8;
            i -= 1;
        }

        while source_bits > 8 {
            let b = ((u32::from(input[source_pos]) << source_gap) & 0xff)
                | (u32::from(input[source_pos + 1]) >> (8 - source_gap));
            self.data[buffer_pos] |= (b >> buffer_rem) as u8;
            buffer_pos += 1;
            buffer_bits += 8 - buffer_rem as i32;
            if buffer_bits == DIGESTBITS {
                self.transform();
                buffer_bits = 0;
                buffer_pos = 0;
            }
            self.data[buffer_pos] = (b << (8 - buffer_rem)) as u8;
            buffer_bits += buffer_rem as i32;
            source_bits -= 8;
            source_pos += 1;
        }
        let b = if source_bits > 0 {
            let b = (u32::from(input[source_pos]) << source_gap) & 0xff;
            self.data[buffer_pos] |= (b >> buffer_rem) as u8;
            b
        } else {
            0
        };
        if u64::from(buffer_rem) + source_bits < 8 {
            buffer_bits += source_bits as i32;
        } else {
            buffer_pos += 1;
            buffer_bits += 8 - buffer_rem as i32;
            source_bits -= u64::from(8 - buffer_rem);
            if buffer_bits == DIGESTBITS {
                self.transform();
                buffer_bits = 0;
                buffer_pos = 0;
            }
            self.data[buffer_pos] = (b << (8 - buffer_rem)) as u8;
            buffer_bits += source_bits as i32;
        }
        self.bits = buffer_bits;
        self.pos = buffer_pos as i32;
    }

    #[allow(clippy::cast_sign_loss)]
    fn finish(&mut self, out: &mut [u8]) {
        let mut buffer_pos = self.pos as usize;
        self.data[buffer_pos] |= 0x80u8 >> (self.bits & 7);
        buffer_pos += 1;
        if buffer_pos > WBLOCKBYTES - LENGTHBYTES {
            if buffer_pos < WBLOCKBYTES {
                self.data[buffer_pos..].fill(0);
            }
            self.transform();
            buffer_pos = 0;
        }
        if buffer_pos < WBLOCKBYTES - LENGTHBYTES {
            self.data[buffer_pos..WBLOCKBYTES - LENGTHBYTES].fill(0);
        }
        self.data[WBLOCKBYTES - LENGTHBYTES..].copy_from_slice(&self.bitlength);
        self.transform();
        for (chunk, s) in out.chunks_exact_mut(8).zip(self.state) {
            chunk.copy_from_slice(&s.to_be_bytes());
        }
    }

    #[allow(clippy::cast_sign_loss)]
    fn image(&self) -> Vec<u8> {
        ImageWriter::new()
            .u64s(&self.state)
            .bytes(&self.bitlength)
            .u32(self.pos as u32)
            .u32(self.bits as u32)
            .bytes(&self.data)
            .finish(168)
    }

    #[allow(clippy::cast_possible_wrap)]
    fn load_image(&mut self, image: &[u8]) {
        let mut r = ImageReader::new(image);
        r.u64s(&mut self.state);
        r.bytes(&mut self.bitlength);
        self.pos = r.u32() as i32;
        self.bits = r.u32() as i32;
        r.bytes(&mut self.data);
    }

    fn is_valid(&self) -> bool {
        usize::try_from(self.pos).is_ok_and(|p| p < self.data.len())
            && self.bits >= self.pos * 8
            && self.bits < self.pos * 8 + 8
    }
}

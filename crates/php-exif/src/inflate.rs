// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A small zlib (RFC 1950/1951) decoder reproducing the outcomes of zlib's
//! `uncompress()` that `php_handle_swc()` depends on.

/// Result of [`uncompress`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// `Z_OK`: the stream ended (and its Adler-32 matched) within the
    /// output limit.  Holds the whole output.
    Ok(Vec<u8>),
    /// `Z_BUF_ERROR`: the output limit was reached before the stream ended.
    /// Holds the `limit` bytes produced.
    BufError(Vec<u8>),
    /// `Z_DATA_ERROR` (also returned for truncated input).  Holds the bytes
    /// produced before the error.
    DataError(Vec<u8>),
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u32,
    nbits: u32,
}

#[derive(Debug)]
enum Stop {
    Truncated,
    Data,
    Full,
}

impl Bits<'_> {
    fn need(&mut self, n: u32) -> Result<(), Stop> {
        while self.nbits < n {
            let b = *self.data.get(self.pos).ok_or(Stop::Truncated)?;
            self.pos += 1;
            self.bit |= u32::from(b) << self.nbits;
            self.nbits += 8;
        }
        Ok(())
    }

    fn take(&mut self, n: u32) -> Result<u32, Stop> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.nbits -= n;
        Ok(v)
    }

    fn align(&mut self) {
        let r = self.nbits % 8;
        self.bit >>= r;
        self.nbits -= r;
    }
}

struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Self, Stop> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[usize::from(l)] += 1;
        }
        counts[0] = 0;
        let mut left: i32 = 1;
        for &c in &counts[1..] {
            left = left * 2 - i32::from(c);
            if left < 0 {
                return Err(Stop::Data);
            }
        }
        let mut offs = [0u16; 16];
        for i in 1..15 {
            offs[i + 1] = offs[i] + counts[i];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[usize::from(offs[usize::from(l)])] =
                    u16::try_from(sym).map_err(|_| Stop::Data)?;
                offs[usize::from(l)] += 1;
            }
        }
        Ok(Self { counts, symbols })
    }

    fn decode(&self, b: &mut Bits<'_>) -> Result<u16, Stop> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..16 {
            code |= i32::try_from(b.take(1)?).unwrap_or(0);
            let count = i32::from(self.counts[len]);
            if code - count < first {
                let i = usize::try_from(index + (code - first)).map_err(|_| Stop::Data)?;
                return self.symbols.get(i).copied().ok_or(Stop::Data);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(Stop::Data)
    }
}

const LBASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEXT: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DBASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DEXT: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn push(out: &mut Vec<u8>, limit: usize, b: u8) -> Result<(), Stop> {
    if out.len() >= limit {
        return Err(Stop::Full);
    }
    out.push(b);
    Ok(())
}

fn codes(
    b: &mut Bits<'_>,
    out: &mut Vec<u8>,
    limit: usize,
    lit: &Huffman,
    dist: &Huffman,
) -> Result<(), Stop> {
    loop {
        let sym = lit.decode(b)?;
        match sym {
            0..=255 => push(out, limit, u8::try_from(sym).map_err(|_| Stop::Data)?)?,
            256 => return Ok(()),
            _ => {
                let i = usize::from(sym - 257);
                if i >= 29 {
                    return Err(Stop::Data);
                }
                let len = usize::from(LBASE[i]) + b.take(u32::from(LEXT[i]))? as usize;
                let d = usize::from(dist.decode(b)?);
                if d >= 30 {
                    return Err(Stop::Data);
                }
                let distance = usize::from(DBASE[d]) + b.take(u32::from(DEXT[d]))? as usize;
                if distance > out.len() {
                    return Err(Stop::Data);
                }
                for _ in 0..len {
                    let c = out[out.len() - distance];
                    push(out, limit, c)?;
                }
            }
        }
    }
}

const ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

fn inflate(b: &mut Bits<'_>, out: &mut Vec<u8>, limit: usize) -> Result<(), Stop> {
    loop {
        let last = b.take(1)?;
        match b.take(2)? {
            0 => {
                b.align();
                let len = b.take(16)?;
                let nlen = b.take(16)?;
                if len != !nlen & 0xFFFF {
                    return Err(Stop::Data);
                }
                for _ in 0..len {
                    let c = b.take(8)?;
                    push(out, limit, u8::try_from(c).map_err(|_| Stop::Data)?)?;
                }
            }
            1 => {
                let mut l = [0u8; 288];
                l[..144].fill(8);
                l[144..256].fill(9);
                l[256..280].fill(7);
                l[280..].fill(8);
                let lit = Huffman::new(&l)?;
                let dist = Huffman::new(&[5u8; 30])?;
                codes(b, out, limit, &lit, &dist)?;
            }
            2 => {
                let nlen = b.take(5)? as usize + 257;
                let ndist = b.take(5)? as usize + 1;
                let ncode = b.take(4)? as usize + 4;
                if nlen > 286 || ndist > 30 {
                    return Err(Stop::Data);
                }
                let mut lengths = [0u8; 320];
                for &o in &ORDER[..ncode] {
                    lengths[o] = u8::try_from(b.take(3)?).map_err(|_| Stop::Data)?;
                }
                let lencode = Huffman::new(&lengths[..19])?;
                let mut idx = 0;
                while idx < nlen + ndist {
                    let sym = lencode.decode(b)?;
                    if sym < 16 {
                        lengths[idx] = u8::try_from(sym).map_err(|_| Stop::Data)?;
                        idx += 1;
                    } else {
                        let (val, rep) = match sym {
                            16 => {
                                if idx == 0 {
                                    return Err(Stop::Data);
                                }
                                (lengths[idx - 1], 3 + b.take(2)? as usize)
                            }
                            17 => (0, 3 + b.take(3)? as usize),
                            _ => (0, 11 + b.take(7)? as usize),
                        };
                        if idx + rep > nlen + ndist {
                            return Err(Stop::Data);
                        }
                        lengths[idx..idx + rep].fill(val);
                        idx += rep;
                    }
                }
                if lengths[256] == 0 {
                    return Err(Stop::Data);
                }
                let lit = Huffman::new(&lengths[..nlen])?;
                let dist = Huffman::new(&lengths[nlen..nlen + ndist])?;
                codes(b, out, limit, &lit, &dist)?;
            }
            _ => return Err(Stop::Data),
        }
        if last == 1 {
            return Ok(());
        }
    }
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &c in data {
        a = (a + u32::from(c)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// zlib's `uncompress()` with an output buffer of `limit` bytes.
pub(crate) fn uncompress(input: &[u8], limit: usize) -> Outcome {
    let mut out = Vec::new();
    if input.len() < 2 {
        return Outcome::DataError(out);
    }
    let (cmf, flg) = (input[0], input[1]);
    if cmf & 0x0F != 8 || (u16::from(cmf) << 8 | u16::from(flg)) % 31 != 0 || cmf >> 4 > 7 {
        return Outcome::DataError(out);
    }
    if flg & 0x20 != 0 {
        // Preset dictionary: Z_NEED_DICT becomes Z_DATA_ERROR.
        return Outcome::DataError(out);
    }
    let mut bits = Bits {
        data: &input[2..],
        pos: 0,
        bit: 0,
        nbits: 0,
    };
    match inflate(&mut bits, &mut out, limit) {
        Ok(()) => {
            bits.align();
            let mut sum = [0u8; 4];
            for s in &mut sum {
                match bits.take(8) {
                    Ok(v) => *s = u8::try_from(v).unwrap_or(0),
                    Err(_) => return Outcome::DataError(out),
                }
            }
            if u32::from_be_bytes(sum) == adler32(&out) {
                Outcome::Ok(out)
            } else {
                Outcome::DataError(out)
            }
        }
        Err(Stop::Full) => Outcome::BufError(out),
        Err(Stop::Truncated | Stop::Data) => Outcome::DataError(out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // zlib.compress(b"hello hello hello hello")
    const HELLO: &[u8] = b"x\x9c\xcbH\xcd\xc9\xc9W\xc8@'\x01h\x03\x08\xb1";

    #[test]
    fn roundtrip_and_errors() {
        let full = b"hello hello hello hello".to_vec();
        match uncompress(HELLO, 100) {
            Outcome::Ok(v) => assert_eq!(v, full),
            o => panic!("{o:?}"),
        }
        assert_eq!(uncompress(HELLO, 5), Outcome::BufError(b"hello".to_vec()));
        assert!(matches!(
            uncompress(&HELLO[..6], 100),
            Outcome::DataError(_)
        ));
        assert!(matches!(uncompress(b"garbage", 100), Outcome::DataError(_)));
    }

    #[test]
    fn stored_block() {
        let data = b"abc";
        let mut z = vec![0x78, 0x01, 0x01, 3, 0, 0xFC, 0xFF];
        z.extend_from_slice(data);
        z.extend_from_slice(&adler32(data).to_be_bytes());
        assert_eq!(uncompress(&z, 10), Outcome::Ok(data.to_vec()));
    }
}

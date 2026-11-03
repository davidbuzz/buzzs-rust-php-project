//! A small zlib (RFC 1950) / DEFLATE (RFC 1951) decoder for the compressed
//! client/server protocol (`CLIENT_COMPRESS`). Modelled on zlib's `puff.c`.
//! The client never needs to compress: it may always send packets with an
//! "uncompressed length" of 0, which the server accepts verbatim.

/// Why a stream could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InflateError {
    /// Input ended early.
    Truncated,
    /// Malformed zlib header or deflate data.
    Corrupt,
    /// Adler-32 mismatch.
    Checksum,
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    bitbuf: u32,
    bitcnt: u32,
}

impl Bits<'_> {
    fn bits(&mut self, need: u32) -> Result<u32, InflateError> {
        let mut val = self.bitbuf;
        while self.bitcnt < need {
            let b = *self.data.get(self.pos).ok_or(InflateError::Truncated)?;
            self.pos += 1;
            val |= u32::from(b) << self.bitcnt;
            self.bitcnt += 8;
        }
        self.bitbuf = if need >= 32 { 0 } else { val >> need };
        self.bitcnt -= need;
        Ok(if need >= 32 {
            val
        } else {
            val & ((1 << need) - 1)
        })
    }
}

const MAXBITS: usize = 15;

struct Huffman {
    count: [u16; MAXBITS + 1],
    symbol: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Self, InflateError> {
        let mut count = [0u16; MAXBITS + 1];
        for &l in lengths {
            count[usize::from(l)] += 1;
        }
        if usize::from(count[0]) == lengths.len() {
            return Ok(Self {
                count,
                symbol: vec![0; lengths.len()],
            });
        }
        let mut left: i32 = 1;
        for &c in &count[1..] {
            left <<= 1;
            left -= i32::from(c);
            if left < 0 {
                return Err(InflateError::Corrupt);
            }
        }
        let mut offs = [0u16; MAXBITS + 1];
        for len in 1..MAXBITS {
            offs[len + 1] = offs[len] + count[len];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                let o = &mut offs[usize::from(l)];
                #[allow(clippy::cast_possible_truncation)]
                {
                    symbol[usize::from(*o)] = sym as u16;
                }
                *o += 1;
            }
        }
        Ok(Self { count, symbol })
    }

    fn decode(&self, s: &mut Bits<'_>) -> Result<u16, InflateError> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..=MAXBITS {
            #[allow(clippy::cast_possible_wrap)]
            {
                code |= s.bits(1)? as i32;
            }
            let count = i32::from(self.count[len]);
            if code - count < first {
                #[allow(clippy::cast_sign_loss)]
                return Ok(self.symbol[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(InflateError::Corrupt)
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

fn codes(
    s: &mut Bits<'_>,
    out: &mut Vec<u8>,
    lencode: &Huffman,
    distcode: &Huffman,
) -> Result<(), InflateError> {
    loop {
        let sym = lencode.decode(s)?;
        match sym.cmp(&256) {
            std::cmp::Ordering::Less => {
                #[allow(clippy::cast_possible_truncation)]
                out.push(sym as u8);
                continue;
            }
            std::cmp::Ordering::Equal => return Ok(()),
            std::cmp::Ordering::Greater => {}
        }
        {
            let sym = usize::from(sym - 257);
            if sym >= 29 {
                return Err(InflateError::Corrupt);
            }
            let len = usize::from(LBASE[sym]) + s.bits(u32::from(LEXT[sym]))? as usize;
            let dsym = usize::from(distcode.decode(s)?);
            if dsym >= 30 {
                return Err(InflateError::Corrupt);
            }
            let dist = usize::from(DBASE[dsym]) + s.bits(u32::from(DEXT[dsym]))? as usize;
            if dist > out.len() {
                return Err(InflateError::Corrupt);
            }
            let start = out.len() - dist;
            for i in 0..len {
                let b = out[start + i];
                out.push(b);
            }
        }
    }
}

fn fixed_tables() -> Result<(Huffman, Huffman), InflateError> {
    let mut lengths = [0u8; 288];
    for (i, l) in lengths.iter_mut().enumerate() {
        *l = match i {
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    Ok((Huffman::new(&lengths)?, Huffman::new(&[5u8; 30])?))
}

fn dynamic_tables(s: &mut Bits<'_>) -> Result<(Huffman, Huffman), InflateError> {
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let nlen = s.bits(5)? as usize + 257;
    let ndist = s.bits(5)? as usize + 1;
    let ncode = s.bits(4)? as usize + 4;
    if nlen > 286 || ndist > 30 {
        return Err(InflateError::Corrupt);
    }
    let mut lengths = [0u8; 320];
    for &o in ORDER.iter().take(ncode) {
        #[allow(clippy::cast_possible_truncation)]
        {
            lengths[o] = s.bits(3)? as u8;
        }
    }
    let lencode = Huffman::new(&lengths[..19])?;
    let mut index = 0;
    while index < nlen + ndist {
        let sym = lencode.decode(s)?;
        if sym < 16 {
            #[allow(clippy::cast_possible_truncation)]
            {
                lengths[index] = sym as u8;
            }
            index += 1;
        } else {
            let (prev, rep) = match sym {
                16 => {
                    if index == 0 {
                        return Err(InflateError::Corrupt);
                    }
                    (lengths[index - 1], 3 + s.bits(2)? as usize)
                }
                17 => (0, 3 + s.bits(3)? as usize),
                _ => (0, 11 + s.bits(7)? as usize),
            };
            if index + rep > nlen + ndist {
                return Err(InflateError::Corrupt);
            }
            for _ in 0..rep {
                lengths[index] = prev;
                index += 1;
            }
        }
    }
    if lengths[256] == 0 {
        return Err(InflateError::Corrupt);
    }
    Ok((
        Huffman::new(&lengths[..nlen])?,
        Huffman::new(&lengths[nlen..nlen + ndist])?,
    ))
}

/// Decodes a raw DEFLATE stream.
pub fn inflate_raw(data: &[u8], size_hint: usize) -> Result<(Vec<u8>, usize), InflateError> {
    let mut s = Bits {
        data,
        pos: 0,
        bitbuf: 0,
        bitcnt: 0,
    };
    let mut out = Vec::with_capacity(size_hint);
    loop {
        let last = s.bits(1)?;
        match s.bits(2)? {
            0 => {
                s.bitbuf = 0;
                s.bitcnt = 0;
                let hdr = data.get(s.pos..s.pos + 4).ok_or(InflateError::Truncated)?;
                let len = usize::from(u16::from_le_bytes([hdr[0], hdr[1]]));
                let nlen = u16::from_le_bytes([hdr[2], hdr[3]]);
                #[allow(clippy::cast_possible_truncation)]
                if nlen != !(len as u16) {
                    return Err(InflateError::Corrupt);
                }
                s.pos += 4;
                let chunk = data
                    .get(s.pos..s.pos + len)
                    .ok_or(InflateError::Truncated)?;
                out.extend_from_slice(chunk);
                s.pos += len;
            }
            1 => {
                let (l, d) = fixed_tables()?;
                codes(&mut s, &mut out, &l, &d)?;
            }
            2 => {
                let (l, d) = dynamic_tables(&mut s)?;
                codes(&mut s, &mut out, &l, &d)?;
            }
            _ => return Err(InflateError::Corrupt),
        }
        if last == 1 {
            return Ok((out, s.pos));
        }
    }
}

/// Adler-32 checksum.
#[must_use]
pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += u32::from(x);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

/// zlib `uncompress()`: decodes a zlib-wrapped stream and verifies its
/// Adler-32 trailer.
pub fn uncompress(data: &[u8], size_hint: usize) -> Result<Vec<u8>, InflateError> {
    if data.len() < 2 {
        return Err(InflateError::Truncated);
    }
    let (cmf, flg) = (data[0], data[1]);
    if cmf & 0x0F != 8 || (u16::from(cmf) * 256 + u16::from(flg)) % 31 != 0 || flg & 0x20 != 0 {
        return Err(InflateError::Corrupt);
    }
    let (out, used) = inflate_raw(&data[2..], size_hint)?;
    let trailer = data
        .get(2 + used..2 + used + 4)
        .ok_or(InflateError::Truncated)?;
    if u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]) != adler32(&out) {
        return Err(InflateError::Checksum);
    }
    Ok(out)
}

/// zlib `compress()` using stored (uncompressed) blocks only. Valid zlib
/// output; used by tests and the mock server.
#[must_use]
pub fn compress_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut chunks = data.chunks(65535).peekable();
    if chunks.peek().is_none() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(c) = chunks.next() {
        out.push(u8::from(chunks.peek().is_none()));
        #[allow(clippy::cast_possible_truncation)]
        let len = c.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, clippy::format_collect)]
mod tests {
    use super::*;

    #[test]
    fn stored_roundtrip() {
        for n in [0usize, 1, 100, 70000] {
            let data: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
            assert_eq!(uncompress(&compress_stored(&data), n).unwrap(), data);
        }
    }

    #[test]
    fn fixed_huffman_known_vector() {
        // zlib.compress(b"hello hello hello hello") from CPython
        let z = [
            0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x27, 0x01, 0x68, 0x03,
            0x08, 0xb1,
        ];
        assert_eq!(uncompress(&z, 0).unwrap(), b"hello hello hello hello");
    }

    #[test]
    fn dynamic_huffman_known_vector() {
        let hex = "78da95965b56c3300c44ff598597604bb213b31b1ee50d814029b07a7ae2c902ee773b4791743da37539a57c99be1e0ee9e3f878f39caed7e5f496ee969ff4747c7dff4ccbf761dd7e7eb9fafb4db7cb7dca17e7bfa4c24465131913c5267226ea9b28e0e7b54d5599caeaa66a4ce5a3d6046731fa9a99aa8d1176a69ac7b60a04a364a141d930d583789418dd15a7cb1eb32c14923e36572826e2a4348a97ea41546c567f101637cdb353a0f5ba212f215e0cf212a17ad44ee6d19f415eaa8d791ae4a54e637f067969e2c5202f6daf077999f6fe202fd33e4f6a2fda9f435eba7871c84b179f4efd25eb41383598ac17e8d4618a9ebc576aa162c61b15cad41c42535c2eea33766d4d156253aa7222682e35811334989a923028399382372839b3723e7036e9ac081a4e59e4044da7a2a329683c59568f901cf3aca942722cf2d863cd3481454e85e458db2b42726c528fd569e86baa1592637d3f7e2139be9353e9015cc46a9df081a21ee965e37a8f959e367176807f784c731e";
        let z: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let expect: String = (0..60)
            .map(|i| {
                format!(
                    "row {i}: the quick brown fox jumps over the lazy dog {}\n",
                    i * i
                )
            })
            .collect();
        assert_eq!(uncompress(&z, 0).unwrap(), expect.as_bytes());
    }

    #[test]
    fn corrupt_inputs() {
        assert_eq!(uncompress(&[0x78], 0), Err(InflateError::Truncated));
        assert_eq!(
            uncompress(&[0x79, 0x9c, 0, 0], 0),
            Err(InflateError::Corrupt)
        );
        let mut z = compress_stored(b"abc");
        let n = z.len();
        z[n - 1] ^= 1;
        assert_eq!(uncompress(&z, 0), Err(InflateError::Checksum));
    }

    #[test]
    fn adler_known() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }
}

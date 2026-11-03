//! Byte-level helpers for the client/server protocol: little-endian
//! integers, length-encoded integers/strings (ref: `mysqlnd_wireprotocol.c`
//! `php_mysqlnd_net_field_length*`, `php_mysqlnd_net_store_length`).

/// Result of decoding a length-encoded field length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldLen {
    /// 0xFB: SQL NULL.
    Null,
    /// A length.
    Len(u64),
    /// Not enough bytes left (`MYSQLND_INVALID_NET_FIELD_LENGTH`).
    Invalid,
}

/// A forward-only reader over a packet payload.
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Starts reading `buf` at offset 0.
    #[must_use]
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Current offset.
    #[must_use]
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Bytes left.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// The unread tail.
    #[must_use]
    pub fn rest(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }

    /// Peeks the next byte.
    #[must_use]
    pub fn peek(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    /// Skips `n` bytes (saturating at the end).
    pub fn skip(&mut self, n: usize) {
        self.pos = (self.pos + n).min(self.buf.len());
    }

    /// Takes `n` bytes.
    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.remaining() < n {
            return None;
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Some(s)
    }

    /// Reads one byte.
    pub fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }

    /// Reads a little-endian u16.
    pub fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|s| u16::from_le_bytes([s[0], s[1]]))
    }

    /// Reads a little-endian 3-byte integer.
    pub fn u24(&mut self) -> Option<u32> {
        self.take(3)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], 0]))
    }

    /// Reads a little-endian u32.
    pub fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }

    /// Reads a little-endian u64.
    pub fn u64(&mut self) -> Option<u64> {
        self.take(8).map(|s| {
            let mut a = [0u8; 8];
            a.copy_from_slice(s);
            u64::from_le_bytes(a)
        })
    }

    /// Reads up to (excluding) the next NUL and consumes the NUL; `None` if
    /// there is no NUL.
    pub fn cstr(&mut self) -> Option<&'a [u8]> {
        let rest = self.rest();
        let end = rest.iter().position(|&b| b == 0)?;
        self.pos += end + 1;
        Some(&rest[..end])
    }

    /// `php_mysqlnd_net_field_length`: the 8-byte form only reads its low
    /// 4 bytes (as the C code does) but consumes all 9.
    pub fn field_len(&mut self) -> FieldLen {
        self.lenenc(false)
    }

    /// `php_mysqlnd_net_field_length_ll`: full 64-bit lengths.
    pub fn field_len_ll(&mut self) -> FieldLen {
        self.lenenc(true)
    }

    fn lenenc(&mut self, full: bool) -> FieldLen {
        let rest = self.rest();
        let Some(&b) = rest.first() else {
            return FieldLen::Invalid;
        };
        match b {
            0..=250 => {
                self.pos += 1;
                FieldLen::Len(u64::from(b))
            }
            251 => {
                self.pos += 1;
                FieldLen::Null
            }
            252 if rest.len() >= 3 => {
                self.pos += 3;
                FieldLen::Len(u64::from(u16::from_le_bytes([rest[1], rest[2]])))
            }
            253 if rest.len() >= 4 => {
                self.pos += 4;
                FieldLen::Len(u64::from(u32::from_le_bytes([
                    rest[1], rest[2], rest[3], 0,
                ])))
            }
            254 | 255 if rest.len() >= 9 => {
                self.pos += 9;
                let mut a = [0u8; 8];
                a.copy_from_slice(&rest[1..9]);
                let v = u64::from_le_bytes(a);
                FieldLen::Len(if full { v } else { v & 0xFFFF_FFFF })
            }
            _ => FieldLen::Invalid,
        }
    }
}

/// `php_mysqlnd_net_store_length`
pub fn put_lenenc(out: &mut Vec<u8>, len: u64) {
    if len < 251 {
        #[allow(clippy::cast_possible_truncation)]
        out.push(len as u8);
    } else if len < 65536 {
        out.push(252);
        #[allow(clippy::cast_possible_truncation)]
        out.extend_from_slice(&(len as u16).to_le_bytes());
    } else if len < 16_777_216 {
        out.push(253);
        out.extend_from_slice(&len.to_le_bytes()[..3]);
    } else {
        out.push(254);
        out.extend_from_slice(&len.to_le_bytes());
    }
}

/// `php_mysqlnd_net_store_length_size`
#[must_use]
pub fn lenenc_size(len: u64) -> usize {
    if len < 251 {
        1
    } else if len < 65536 {
        3
    } else if len < 16_777_216 {
        4
    } else {
        9
    }
}

/// Writes a length-encoded string.
pub fn put_lenenc_str(out: &mut Vec<u8>, s: &[u8]) {
    put_lenenc(out, s.len() as u64);
    out.extend_from_slice(s);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lenenc_roundtrip() {
        for v in [
            0u64,
            250,
            251,
            65535,
            65536,
            16_777_215,
            16_777_216,
            u64::from(u32::MAX) + 7,
        ] {
            let mut out = Vec::new();
            put_lenenc(&mut out, v);
            assert_eq!(out.len(), lenenc_size(v));
            let mut r = Reader::new(&out);
            assert_eq!(r.field_len_ll(), FieldLen::Len(v));
            assert_eq!(r.remaining(), 0);
        }
        // the short form truncates to 32 bits
        let mut out = Vec::new();
        put_lenenc(&mut out, (1u64 << 32) + 5);
        assert_eq!(Reader::new(&out).field_len(), FieldLen::Len(5));
    }

    #[test]
    fn special_markers() {
        assert_eq!(Reader::new(&[251]).field_len(), FieldLen::Null);
        assert_eq!(Reader::new(&[]).field_len(), FieldLen::Invalid);
        assert_eq!(Reader::new(&[252, 1]).field_len(), FieldLen::Invalid);
        assert_eq!(
            Reader::new(&[254, 1, 2, 3]).field_len_ll(),
            FieldLen::Invalid
        );
    }

    #[test]
    fn reader_basics() {
        let mut r = Reader::new(b"ab\0\x01\x02\x03\x04\x05\x06\x07");
        assert_eq!(r.cstr(), Some(&b"ab"[..]));
        assert_eq!(r.u16(), Some(0x0201));
        assert_eq!(r.u24(), Some(0x0005_0403));
        assert_eq!(r.u32(), None);
        assert_eq!(r.u16(), Some(0x0706));
        assert_eq!(r.cstr(), None);
    }
}

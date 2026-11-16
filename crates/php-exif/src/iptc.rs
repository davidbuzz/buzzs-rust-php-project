// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `iptcparse()` and `iptcembed()` (ref: `ext/standard/iptc.c`), which live
//! next to `getimagesize()` in ext/standard and share its JPEG marker walk.

// Bytes travel as C `int`s (EOF = -1) and are narrowed back like the C code.
#![allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]

use php_runtime::{ArrayKey, HashTable, Value, ZArray};

/// `iptcparse($iptc_block)`: `Value::Bool(false)` when no tag is found,
/// otherwise `["2#005" => [..values], ...]`.
pub fn iptcparse(buf: &[u8]) -> Value {
    let len = buf.len();
    let mut inx = 0usize;
    // Find the first tag (the C code may read one byte past the end here,
    // which is the terminating NUL of the zend_string).
    let at = |i: usize| buf.get(i).copied().unwrap_or(0);
    while inx < len {
        if at(inx) == 0x1c && (at(inx + 1) == 0x01 || at(inx + 1) == 0x02) {
            break;
        }
        inx += 1;
    }
    let mut out = HashTable::new();
    let mut found = 0u32;
    while inx < len {
        if at(inx) != 0x1c {
            break;
        }
        inx += 1;
        if inx + 4 >= len {
            break;
        }
        let dataset = at(inx);
        let recnum = at(inx + 1);
        inx += 2;
        let n: usize = if at(inx) & 0x80 != 0 {
            if inx + 6 >= len {
                break;
            }
            let v = u32::from_be_bytes([at(inx + 2), at(inx + 3), at(inx + 4), at(inx + 5)]);
            inx += 6;
            v as usize
        } else {
            let v = usize::from(at(inx)) << 8 | usize::from(at(inx + 1));
            inx += 2;
            v
        };
        if n > len || inx + n > len {
            break;
        }
        let key = ArrayKey::from_bytes(format!("{dataset}#{recnum:03}").as_bytes());
        let slot = out.lookup(key);
        if !matches!(slot, Value::Array(_)) {
            *slot = Value::empty_array();
        }
        if let Value::Array(a) = slot {
            let _ = a.make_mut().push(Value::string(&buf[inx..inx + n]));
        }
        inx += n;
        found += 1;
    }
    if found == 0 {
        Value::Bool(false)
    } else {
        Value::Array(ZArray::from_table(out))
    }
}

/// What `iptcembed()` produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IptcEmbed {
    /// Bytes written to the output when `$spool > 0` (to be echoed).
    pub spooled: Vec<u8>,
    /// The return value: `Some(bytes)` for `$spool < 2`, `None` for `true`
    /// (spool-only mode).  `Err` = PHP `false`.
    pub result: Result<Option<Vec<u8>>, ()>,
}

struct Io<'a> {
    input: &'a [u8],
    pos: usize,
    spool: i64,
    spooled: Vec<u8>,
    buf: Option<Vec<u8>>,
    cap: usize,
}

const EOF: i32 = -1;
const M_EOI: u32 = 0xD9;

impl Io<'_> {
    fn put1(&mut self, c: u8) -> bool {
        if self.spool > 0 {
            self.spooled.push(c);
        }
        if let Some(b) = self.buf.as_mut() {
            if b.len() >= self.cap {
                return false;
            }
            b.push(c);
        }
        true
    }

    fn get1(&mut self, spool: bool) -> i32 {
        let Some(&c) = self.input.get(self.pos) else {
            return EOF;
        };
        self.pos += 1;
        if spool {
            if self.spool > 0 {
                self.spooled.push(c);
            }
            if let Some(b) = self.buf.as_mut() {
                if b.len() >= self.cap {
                    return EOF;
                }
                b.push(c);
            }
        }
        i32::from(c)
    }

    fn read_remaining(&mut self) {
        while self.get1(true) != EOF {}
    }

    fn skip_variable(&mut self, spool: bool) {
        let c1 = self.get1(spool);
        if c1 == EOF {
            return;
        }
        let c2 = self.get1(spool);
        if c2 == EOF {
            return;
        }
        let mut length = ((c1 as u32) << 8).wrapping_add(c2 as u32).wrapping_sub(2);
        while length > 0 {
            length -= 1;
            if self.get1(spool) == EOF {
                return;
            }
        }
    }

    fn next_marker(&mut self) -> u32 {
        let mut c = self.get1(true);
        if c == EOF {
            return M_EOI;
        }
        while c != 0xFF {
            c = self.get1(true);
            if c == EOF {
                return M_EOI;
            }
        }
        loop {
            c = self.get1(false);
            if c == EOF {
                return M_EOI;
            }
            if c == 0xFF {
                self.put1(0xFF);
            } else {
                return c as u32;
            }
        }
    }
}

/// `iptcembed($iptc_data, $filename, $spool)` given the file contents.
pub fn iptcembed(iptcdata: &[u8], jpeg: &[u8], spool: i64) -> IptcEmbed {
    let mut io = Io {
        input: jpeg,
        pos: 0,
        spool,
        spooled: Vec::new(),
        buf: None,
        cap: 0,
    };
    if spool < 2 {
        io.cap = iptcdata.len() + 29 + 1024 + 1 + jpeg.len();
        io.buf = Some(Vec::new());
    }
    let fail = |io: Io<'_>| IptcEmbed {
        spooled: io.spooled,
        result: Err(()),
    };
    if io.get1(true) != 0xFF || io.get1(true) != 0xD8 {
        return fail(io);
    }
    let mut psheader = *b"\xFF\xED\0\0Photoshop 3.0\x008BIM\x04\x04\0\0\0\0";
    let mut written = false;
    loop {
        let marker = io.next_marker();
        if marker == M_EOI {
            break;
        }
        if marker != 0xED && !io.put1(marker as u8) {
            return fail(io);
        }
        match marker {
            0xED => {
                io.skip_variable(false);
                io.pos += 1; // fgetc(): skip the already copied 0xFF
                io.read_remaining();
                break;
            }
            0xE0 | 0xE1 => {
                if written {
                    continue;
                }
                written = true;
                io.skip_variable(true);
                let mut data = iptcdata.to_vec();
                if data.len() & 1 == 1 {
                    data.push(0);
                }
                let n = data.len();
                psheader[2] = ((n + 28) >> 8) as u8;
                psheader[3] = ((n + 28) & 0xFF) as u8;
                for &b in &psheader {
                    if !io.put1(b) {
                        return fail(io);
                    }
                }
                if !io.put1((n >> 8) as u8) || !io.put1((n & 0xFF) as u8) {
                    return fail(io);
                }
                for &b in &data {
                    if !io.put1(b) {
                        return fail(io);
                    }
                }
            }
            0xDA => {
                io.read_remaining();
                break;
            }
            _ => io.skip_variable(true),
        }
    }
    IptcEmbed {
        result: Ok(io.buf),
        spooled: io.spooled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        assert!(matches!(iptcparse(b""), Value::Bool(false)));
        // bug67250
        assert!(matches!(iptcparse(b"\x1C\x02_\x80___"), Value::Bool(false)));
        let v = iptcparse(b"\x1C\x02x\x00\x04Test\x1C\x02x\x00\x01Z");
        let a = v.as_array().expect("array");
        let list = a.get_str(b"2#120").and_then(Value::as_array).expect("list");
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn embed_roundtrip() {
        let jpeg = b"\xff\xd8\xff\xe0\x00\x04ab\xff\xda\x00\x02xyz";
        let r = iptcembed(b"\x1C\x02x\x00\x03abc", jpeg, 0);
        let out = r.result.expect("ok").expect("buffer");
        assert!(out.starts_with(b"\xff\xd8\xff\xe0\x00\x04ab\xff\xed"));
        assert!(out.ends_with(b"\xff\xda\x00\x02xyz"));
        assert!(r.spooled.is_empty());
        assert!(iptcembed(b"", b"nope", 0).result.is_err());
        let s = iptcembed(b"", jpeg, 2);
        assert_eq!(s.result, Ok(None));
        assert!(!s.spooled.is_empty());
    }
}

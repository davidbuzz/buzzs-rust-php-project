//! `mb_strcut` (the byte-oriented cut, `PHP_FUNCTION(mb_strcut)` and
//! libmbfl's `mbfl_strcut` for stateful encodings).
//!
//! Ref: `ext/mbstring/mbstring.c`, `libmbfl/mbfl/mbfilter.c`.

use crate::codecs;
use crate::convert::ErrorMode;
use crate::encoding::{Encoding, EncodingNo};
use crate::legacy::{Filter, Sink, Vtbl};

/// Cut `len` bytes from byte offset `from` (`0 <= from <= s.len()`,
/// `len > 0`), aligned to character boundaries as `mb_strcut` does.
pub fn strcut(s: &[u8], from: usize, len: usize, enc: &'static Encoding) -> Vec<u8> {
    if let Some(cut) = enc.cut {
        return cut(s, from, len);
    }
    if let Some(w) = enc.fixed_width() {
        let from = from & !(w - 1);
        let len = len.min(s.len() - from);
        return s[from..from + (len & !(w - 1))].to_vec();
    }
    if let Some(tab) = enc.mblen_table {
        let mbl = |b: u8| usize::from(tab[usize::from(b)]);
        let (mut p, mut m) = (0usize, 0usize);
        while p < from {
            m = mbl(s[p]);
            p += m;
        }
        if p > from {
            p -= m;
        }
        let start = p;
        let end = if len >= s.len() - start {
            s.len()
        } else {
            let q = p + len;
            while p < q {
                m = mbl(s[p]);
                p += m;
            }
            if p > q {
                p -= m;
            }
            p
        };
        return s[start..end].to_vec();
    }
    legacy_strcut(s, from, len, enc).unwrap_or_default()
}

/// `mbfl_convert_filter_get_vtbl(wchar, enc)`.
fn output_vtbl(enc: &'static Encoding) -> Option<&'static Vtbl> {
    match enc.no {
        EncodingNo::Base64 => Some(&codecs::base64::VTBL_8BIT_B64),
        EncodingNo::Qprint => Some(&codecs::qprint::VTBL_8BIT_QPRINT),
        _ => enc.output_filter,
    }
}

/// `mbfl_convert_filter_get_vtbl(enc, wchar)`.
fn input_vtbl(enc: &'static Encoding) -> Option<&'static Vtbl> {
    match enc.no {
        EncodingNo::Base64 => Some(&codecs::base64::VTBL_B64_8BIT),
        EncodingNo::Qprint => Some(&codecs::qprint::VTBL_QPRINT_8BIT),
        EncodingNo::Uuencode => Some(&codecs::uuencode::VTBL_UUENCODE_8BIT),
        _ => enc.input_filter,
    }
}

struct Device<'a>(&'a mut Vec<u8>);

impl Sink for Device<'_> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn put(&mut self, c: i32) {
        self.0.push(c as u8);
    }
}

struct DecoderSink<'a> {
    decoder: &'a mut Filter,
    device: &'a mut Vec<u8>,
}

impl Sink for DecoderSink<'_> {
    fn put(&mut self, c: i32) {
        self.decoder.feed(c, &mut Device(self.device));
    }
    fn flush(&mut self) {
        self.decoder.flush(&mut Device(self.device));
    }
}

#[derive(Clone)]
struct Snapshot {
    encoder: Filter,
    decoder: Filter,
    p: usize,
    pos: usize,
}

struct Chain {
    encoder: Filter,
    decoder: Filter,
    device: Vec<u8>,
}

impl Chain {
    fn feed(&mut self, b: u8) {
        let mut sink = DecoderSink {
            decoder: &mut self.decoder,
            device: &mut self.device,
        };
        self.encoder.feed(i32::from(b), &mut sink);
    }

    fn flush(&mut self) {
        let mut sink = DecoderSink {
            decoder: &mut self.decoder,
            device: &mut self.device,
        };
        self.encoder.flush(&mut sink);
    }

    fn snapshot(&self, p: usize) -> Snapshot {
        Snapshot {
            encoder: self.encoder.clone(),
            decoder: self.decoder.clone(),
            p,
            pos: self.device.len(),
        }
    }

    fn restore(&mut self, s: &Snapshot) -> usize {
        self.encoder = s.encoder.clone();
        self.decoder = s.decoder.clone();
        self.device.truncate(s.pos);
        s.p
    }
}

/// `mbfl_strcut`: cut via the legacy streaming filters, re-encoding the
/// selected characters (so stateful encodings get proper escape sequences).
fn legacy_strcut(s: &[u8], from: usize, length: usize, enc: &'static Encoding) -> Option<Vec<u8>> {
    let from = from.min(s.len());
    let decoder = Filter::new(output_vtbl(enc)?);
    let encoder = Filter::new(input_vtbl(enc)?);
    let mut chain = Chain {
        encoder,
        decoder,
        device: Vec::with_capacity(s.len().min(length) + 8),
    };

    // Search start position (output discarded).
    let mut p = 0usize;
    while p < from {
        chain.encoder.feed(i32::from(s[p]), &mut crate::legacy::NullSink);
        p += 1;
    }

    let q = s.len();
    let mut bk2 = chain.snapshot(p);
    let length = length.min(q - p);
    let mut bk;

    if length >= 20 {
        let r = p + length - 20;
        while p < r {
            chain.feed(s[p]);
            p += 1;
        }
        if chain.device.len() > length {
            p = chain.restore(&bk2);
            bk = bk2.clone();
        } else {
            bk = chain.snapshot(p);
            chain.flush();
            if chain.device.len() > length {
                p = chain.restore(&bk2);
                bk = bk2.clone();
            } else {
                p = chain.restore(&bk);
            }
        }
    } else {
        bk = bk2.clone();
    }

    while p < q {
        chain.feed(s[p]);
        if chain.device.len() > length {
            chain.restore(&bk);
            break;
        }
        p += 1;
        bk2 = chain.snapshot(p);
        chain.flush();
        if chain.device.len() > length {
            chain.restore(&bk);
            break;
        }
        p = chain.restore(&bk2);
        bk = bk2.clone();
    }

    chain.decoder.illegal_mode = ErrorMode::None;
    chain.flush();
    Some(chain.device)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::by_name;

    fn cut(s: &[u8], from: usize, len: usize, enc: &str) -> Vec<u8> {
        strcut(s, from, len, by_name(enc.as_bytes()).unwrap())
    }

    #[test]
    fn utf8_and_fixed_width() {
        assert_eq!(cut("日本語".as_bytes(), 1, 4, "UTF-8"), "日".as_bytes());
        assert_eq!(cut(b"\x00a\x00b\x00c", 1, 3, "UTF-16BE"), b"\x00a");
    }

    #[test]
    fn iso2022jp_reencodes_with_escapes() {
        // "あいう" in ISO-2022-JP.
        let s = b"\x1b$B$\"$$$&\x1b(B";
        let out = cut(s, 3, 9, "ISO-2022-JP");
        assert_eq!(out, b"\x1b$B$\"\x1b(B".to_vec());
    }
}

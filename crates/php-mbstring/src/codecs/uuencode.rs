//! `UUENCODE` (a transfer encoding).
//!
//! Decoding treats the other side as `8bit`
//! ([`crate::convert::effective_pair`]); encoding receives code points from
//! whatever the source encoding is and uses their low 8 bits. The legacy
//! decoder used by `mb_strcut` is the special filter [`VTBL_UUENCODE_8BIT`]
//! (the [`Encoding`] itself has no legacy filters, and libmbfl has no legacy
//! encoder).
//!
//! Ref: `libmbfl/filters/mbfilter_uuencode.c`.

use crate::convert::ConvertBuf;
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_SBCS};
use crate::legacy::{self, Filter, Sink, Vtbl};

/// `UUENCODE`.
pub static ENC_UUENCODE: Encoding = Encoding {
    no: EncodingNo::Uuencode,
    name: "UUENCODE",
    mime_name: Some("x-uuencode"),
    aliases: &[],
    mblen_table: None,
    flag: ENCTYPE_SBCS,
    input_filter: None,
    output_filter: None,
    to_wchar: uuencode_to_wchar,
    from_wchar: wchar_to_uuencode,
    check: None,
    cut: None,
};

/// `vtbl_uuencode_8bit`: uuencoded text to bytes.
pub static VTBL_UUENCODE_8BIT: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_uudec,
    flush: legacy::common_flush,
};

const BEGIN_TEXT: &[u8] = b"begin ";

// Decoder states (`uudec_state_*`).
const GROUND: i32 = 0;
const INBEGIN: i32 = 1;
const UNTIL_NEWLINE: i32 = 2;
const SIZE: i32 = 3;
const A: i32 = 4;
const B: i32 = 5;
const C: i32 = 6;
const D: i32 = 7;
const SKIP_NEWLINE: i32 = 8;

/// `UUDEC`: the 6-bit value of a uuencoded character.
fn uudec(c: i32) -> i32 {
    (c - i32::from(b' ')) & 0o77
}

/// `mbfl_filt_conv_uudec`.
#[allow(clippy::many_single_char_names)]
pub fn filt_uudec(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    match filter.status {
        GROUND => {
            // Looking for a "begin 0666 filename\n" line.
            if filter.cache == 0 && c == i32::from(b'b') {
                filter.status = INBEGIN;
                filter.cache = 1; // move to 'e'
            } else if c == i32::from(b'\n') {
                filter.cache = 0;
            } else {
                filter.cache += 1;
            }
        }
        INBEGIN => {
            #[allow(clippy::cast_sign_loss)]
            let expected = BEGIN_TEXT.get(filter.cache as usize).copied();
            filter.cache += 1;
            if expected.map(i32::from) != Some(c) {
                filter.status = GROUND;
                return;
            }
            if filter.cache == 5 {
                // That's good enough - wait for a newline.
                filter.status = UNTIL_NEWLINE;
                filter.cache = 0;
            }
        }
        UNTIL_NEWLINE if c == i32::from(b'\n') => filter.status = SIZE,
        SIZE => {
            filter.cache = uudec(c) << 24;
            filter.status = A;
        }
        A => {
            filter.cache |= uudec(c) << 16;
            filter.status = B;
        }
        B => {
            filter.cache |= uudec(c) << 8;
            filter.status = C;
        }
        C => {
            filter.cache |= uudec(c);
            filter.status = D;
        }
        D => {
            let d = uudec(c);
            let a = (filter.cache >> 16) & 0xFF;
            let b = (filter.cache >> 8) & 0xFF;
            let cc = filter.cache & 0xFF;
            let mut n = (filter.cache >> 24) & 0xFF;
            if n > 0 {
                out.put((a << 2) | (b >> 4));
            }
            n -= 1;
            if n > 0 {
                out.put((b << 4) | (cc >> 2));
            }
            n -= 1;
            if n > 0 {
                out.put((cc << 6) | d);
            }
            n -= 1;
            n = n.max(0);
            filter.cache = n << 24;
            filter.status = if n == 0 { SKIP_NEWLINE } else { A };
        }
        SKIP_NEWLINE => filter.status = SIZE,
        _ => {}
    }
}

/// `mb_uuencode_to_wchar`: decoded bytes as code points `0..=0xFF`. Needs a
/// buffer of at least 3 code points.
#[allow(clippy::many_single_char_names)]
pub fn uuencode_to_wchar(input: &mut &[u8], buf: &mut [u32], state: &mut u32) -> usize {
    const S_GROUND: u32 = 0;
    const S_SIZE: u32 = 3;
    const S_A: u32 = 4;
    const S_SKIP_NEWLINE: u32 = 8;
    let dec = |c: u8| u32::from(c.wrapping_sub(b' ') & 0o77);

    debug_assert!(buf.len() >= 3);
    let s = *input;
    let e = s.len();
    let limit = buf.len();
    let (mut p, mut out) = (0usize, 0usize);
    let mut st = *state & 0xFF;
    let mut size = *state >> 8;

    while p < e && limit - out >= 3 {
        let c = s[p];
        p += 1;
        match st {
            S_GROUND if c == b'b' && e - p >= 5 && &s[p..p + 5] == b"egin " => {
                p += 5;
                // Consume everything up to the newline.
                while p < e {
                    p += 1;
                    if s[p - 1] == b'\n' {
                        break;
                    }
                }
                st = S_SIZE;
            }
            S_SIZE => {
                size = dec(c);
                st = S_A;
            }
            S_A => {
                if e - p < 4 {
                    p = e;
                    continue;
                }
                let a = dec(c);
                let b = dec(s[p]);
                let cc = dec(s[p + 1]);
                let d = dec(s[p + 2]);
                p += 3;
                if size > 0 {
                    buf[out] = ((a << 2) | (b >> 4)) & 0xFF;
                    out += 1;
                    size -= 1;
                }
                if size > 0 {
                    buf[out] = ((b << 4) | (cc >> 2)) & 0xFF;
                    out += 1;
                    size -= 1;
                }
                if size > 0 {
                    buf[out] = ((cc << 6) | d) & 0xFF;
                    out += 1;
                    size -= 1;
                }
                st = if size != 0 { S_A } else { S_SKIP_NEWLINE };
            }
            S_SKIP_NEWLINE => st = S_SIZE,
            _ => {}
        }
    }

    *state = (size << 8) | st;
    *input = &s[p..];
    out
}

/// `uuencode_six_bits`.
#[allow(clippy::cast_possible_truncation)]
fn six(bits: u32) -> u8 {
    if bits == 0 {
        b'`'
    } else {
        (bits + 32) as u8
    }
}

/// The line-length byte for a line encoding `n` (at most 45) bytes.
#[allow(clippy::cast_possible_truncation)]
fn len_byte(n: usize) -> u8 {
    n.min(45) as u8 + 32
}

/// `mb_wchar_to_uuencode`. Emits a `begin 0644 filename` header, then lines
/// of at most 45 encoded bytes, each prefixed by its length byte. Because
/// input arrives in chunks, the length byte of a line which continues from
/// the previous call is patched in place, and up to 2 leftover input bytes'
/// bits are carried over in `buf.state`.
#[allow(clippy::cast_possible_truncation, clippy::too_many_lines)]
pub fn wchar_to_uuencode(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    let mut bytes_encoded = (buf.state >> 1) & 0x7F;
    let mut n_cached_bits = (buf.state >> 8) & 0xFF;
    let mut cached_bits = buf.state >> 16;
    let mut len = input.len();
    let mut i = 0usize;
    let mut next = || {
        let w = input[i];
        i += 1;
        w
    };
    // Set when the leftover bits of the previous call were flushed; the
    // first loop iteration then starts at C's `possible_line_break` label.
    let mut resume_at_line_break = false;

    if buf.state == 0 {
        buf.extend(b"begin 0644 filename\n");
        buf.push(len_byte(len));
        buf.state |= 1;
    } else if len == 0 && end && bytes_encoded == 0 && n_cached_bits == 0 {
        // Corner case: a final call with empty input would leave an unwanted
        // trailing length byte.
        buf.out.pop();
        return;
    } else {
        // Fix up the length byte of the line continued from the previous
        // call.
        let pending = match n_cached_bits {
            0 => 0,
            2 => 1,
            _ => 2,
        };
        let back = (bytes_encoded * 4 / 3) as usize + 1 + pending;
        if let Some(idx) = buf.out.len().checked_sub(back) {
            buf.out[idx] = len_byte(bytes_encoded as usize + len + pending);
        }

        if n_cached_bits != 0 {
            // Flush out bits which remained from the previous call.
            if n_cached_bits == 2 {
                let w = cached_bits;
                let (mut w2, mut w3) = (0, 0);
                if len != 0 {
                    w2 = next();
                    len -= 1;
                }
                if len != 0 {
                    w3 = next();
                    len -= 1;
                }
                buf.extend(&[
                    six((w << 4) + ((w2 >> 4) & 0xF)),
                    six(((w2 & 0xF) << 2) + ((w3 >> 6) & 0x3)),
                    six(w3 & 0x3F),
                ]);
            } else {
                let w2 = cached_bits;
                let mut w3 = 0;
                if len != 0 {
                    w3 = next();
                    len -= 1;
                }
                buf.extend(&[six((w2 << 2) + ((w3 >> 6) & 0x3)), six(w3 & 0x3F)]);
            }
            n_cached_bits = 0;
            cached_bits = 0;
            resume_at_line_break = true;
        }
    }

    loop {
        if !resume_at_line_break {
            if len == 0 {
                break;
            }
            len -= 1;
            let w = next();
            let (mut w2, mut w3) = (0, 0);

            if len == 0 {
                if !end {
                    buf.push(six((w >> 2) & 0x3F));
                    // Cache the 2 remaining bits of `w`.
                    cached_bits = w & 0x3;
                    n_cached_bits = 2;
                    break;
                }
            } else {
                w2 = next();
                len -= 1;
            }

            if len == 0 {
                if !end {
                    buf.extend(&[
                        six((w >> 2) & 0x3F),
                        six(((w & 0x3) << 4) + ((w2 >> 4) & 0xF)),
                    ]);
                    // Cache the 4 remaining bits of `w2`.
                    cached_bits = w2 & 0xF;
                    n_cached_bits = 4;
                    break;
                }
            } else {
                w3 = next();
                len -= 1;
            }

            buf.extend(&[
                six((w >> 2) & 0x3F),
                six(((w & 0x3) << 4) + ((w2 >> 4) & 0xF)),
                six(((w2 & 0xF) << 2) + ((w3 >> 6) & 0x3)),
                six(w3 & 0x3F),
            ]);
        }
        resume_at_line_break = false;

        // possible_line_break:
        bytes_encoded += 3;
        if bytes_encoded >= 45 {
            buf.push(b'\n');
            if len != 0 || !end {
                buf.push(len_byte(len));
            }
            bytes_encoded = 0;
        }
    }

    if bytes_encoded != 0 && end {
        buf.push(b'\n');
    }

    buf.state = ((cached_bits & 0xFF) << 16)
        | ((n_cached_bits & 0xFF) << 8)
        | ((bytes_encoded & 0x7F) << 1)
        | (buf.state & 1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::{decode_all, encode_all, ErrorMode};
    use crate::legacy::ByteSink;

    fn legacy_decode(input: &[u8]) -> Vec<u8> {
        let mut f = Filter::new(&VTBL_UUENCODE_8BIT);
        let mut sink = ByteSink::default();
        for &b in input {
            f.feed(i32::from(b), &mut sink);
        }
        f.flush(&mut sink);
        sink.0
    }

    const HELLO: &[u8] = b"begin 0644 filename\n%:&5L;&\\`\n`\nend\n";

    #[test]
    fn encode_and_decode() {
        let w: Vec<u32> = b"hello".iter().map(|&b| u32::from(b)).collect();
        assert_eq!(
            encode_all(&w, &ENC_UUENCODE, 0x3F, ErrorMode::Char),
            b"begin 0644 filename\n%:&5L;&\\`\n"
        );
        let d: Vec<u32> = decode_all(HELLO, &ENC_UUENCODE);
        assert_eq!(d, w);
    }

    #[test]
    fn legacy_decoder() {
        assert_eq!(legacy_decode(HELLO), b"hello");
        assert_eq!(legacy_decode(b"xbegin 0644 f\n%:&5L;&\\`\n"), b"");
        assert_eq!(legacy_decode(b"x\nbegin 0644 f\n#:&5L\n"), b"hel");
    }
}

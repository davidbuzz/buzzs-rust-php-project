//! Per-encoding decoders and encoders (ports of `libmbfl/filters/*.c`).

pub mod base64;
pub mod cjk;
pub mod eightbit;
pub mod htmlent;
pub mod qprint;
pub mod sevenbit;
pub mod singlebyte;
pub mod ucs2;
pub mod ucs4;
pub mod utf16;
pub mod utf32;
pub mod utf7;
pub mod utf7imap;
pub mod utf8;
pub mod uuencode;

use crate::convert::{ConvertBuf, BAD_INPUT};

/// Placeholder decoder for encodings not yet ported: every byte is bad input.
pub(crate) fn stub_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let n = input.len().min(buf.len());
    buf[..n].fill(BAD_INPUT);
    *input = &input[n..];
    n
}

/// Placeholder encoder for encodings not yet ported: everything is an error.
pub(crate) fn stub_from_wchar(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        crate::convert::illegal_output(w, stub_from_wchar_ascii, buf);
    }
}

fn stub_from_wchar_ascii(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        if w < 0x80 {
            #[allow(clippy::cast_possible_truncation)]
            buf.push(w as u8);
        }
    }
}

/// Binary search in a sorted `u16` key table (`mbfl_bisec_srch2`); returns
/// the index of `w`. Probes exactly like the C code, so tables with
/// duplicate keys resolve identically.
#[allow(dead_code, clippy::cast_possible_wrap)]
pub(crate) fn bisec_srch2(w: i64, tbl: &[u16]) -> Option<usize> {
    let (mut l, mut r) = (0i64, tbl.len() as i64 - 1);
    while l <= r {
        let probe = (l + r) >> 1;
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let val = i64::from(tbl[probe as usize]);
        match w.cmp(&val) {
            std::cmp::Ordering::Less => r = probe - 1,
            std::cmp::Ordering::Greater => l = probe + 1,
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            std::cmp::Ordering::Equal => return Some(probe as usize),
        }
    }
    None
}

/// `mbfl_bisec_srch`: search a table of `[start, end]` pairs (flattened, so
/// `tbl[2*i]` and `tbl[2*i+1]`) for the range containing `w`.
#[allow(dead_code, clippy::cast_possible_wrap)]
pub(crate) fn bisec_srch(w: i64, tbl: &[u16]) -> Option<usize> {
    let (mut l, mut r) = (0i64, (tbl.len() / 2) as i64 - 1);
    while l <= r {
        let probe = (l + r) >> 1;
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let (lo, hi) = (
            i64::from(tbl[2 * probe as usize]),
            i64::from(tbl[2 * probe as usize + 1]),
        );
        if w < lo {
            r = probe - 1;
        } else if w > hi {
            l = probe + 1;
        } else {
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            return Some(probe as usize);
        }
    }
    None
}

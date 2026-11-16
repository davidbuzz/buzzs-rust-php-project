// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The "fast" conversion machinery of libmbfl (`mb_convert_buf`,
//! `mb_illegal_output`, `mb_fast_convert`).
//!
//! Every encoding provides a decoder (`to_wchar`: bytes to Unicode code
//! points) and an encoder (`from_wchar`: code points to bytes). Decoders mark
//! malformed input with [`BAD_INPUT`]; encoders report unrepresentable code
//! points (and `BAD_INPUT` markers) through [`illegal_output`], which applies
//! the substitution policy stored in the [`ConvertBuf`].
//!
//! Ref: `libmbfl/mbfl/mbfl_convert.c`, `libmbfl/mbfl/mbfl_encoding.h`.

use crate::encoding::Encoding;

/// Marker emitted by decoders for an erroneous input byte sequence
/// (`MBFL_BAD_INPUT`, i.e. `(uint32_t)-1`).
pub const BAD_INPUT: u32 = 0xFFFF_FFFF;

/// Minimum size of a code point buffer passed to a decoder
/// (`MBSTRING_MIN_WCHAR_BUFSIZE`).
pub const MIN_WCHAR_BUFSIZE: usize = 5;

/// How an encoder handles input it cannot represent
/// (`MBFL_OUTPUTFILTER_ILLEGAL_MODE_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ErrorMode {
    /// Silently drop the offending character.
    None,
    /// Output the replacement character.
    #[default]
    Char,
    /// Output `U+XXXX` (or the replacement character for bad input).
    Long,
    /// Output `&#xXXXX;` (or the replacement character for bad input).
    Entity,
    /// Internal: output the byte `0xFF` (never valid UTF-8) as an error
    /// marker, so error markers never match real text when searching.
    BadUtf8,
}

/// Output buffer plus encoder state (`mb_convert_buf`).
#[derive(Debug, Clone, Default)]
pub struct ConvertBuf {
    /// Encoded output so far.
    pub out: Vec<u8>,
    /// Encoder-private state, preserved across calls.
    pub state: u32,
    /// Number of conversion errors encountered.
    pub errors: u32,
    /// Replacement character used in [`ErrorMode::Char`] (and for bad input
    /// in the long/entity modes).
    pub replacement_char: u32,
    /// Current error handling mode.
    pub error_mode: ErrorMode,
}

impl ConvertBuf {
    /// `mb_convert_buf_init`.
    pub fn new(capacity: usize, replacement_char: u32, error_mode: ErrorMode) -> Self {
        Self {
            out: Vec::with_capacity(capacity),
            state: 0,
            errors: 0,
            replacement_char,
            error_mode,
        }
    }

    /// Append one byte.
    #[inline]
    pub fn push(&mut self, b: u8) {
        self.out.push(b);
    }

    /// Append several bytes.
    #[inline]
    pub fn extend(&mut self, bytes: &[u8]) {
        self.out.extend_from_slice(bytes);
    }

    /// Number of bytes written (`mb_convert_buf_len`).
    pub fn len(&self) -> usize {
        self.out.len()
    }

    /// Whether nothing has been written yet.
    pub fn is_empty(&self) -> bool {
        self.out.is_empty()
    }

    /// Truncate the output to `len` bytes (`mb_convert_buf_reset`).
    pub fn reset(&mut self, len: usize) {
        self.out.truncate(len);
    }

    /// Take the finished output.
    pub fn into_bytes(self) -> Vec<u8> {
        self.out
    }
}

/// Decoder signature: consume bytes from `input` (advancing it), write at
/// most `buf.len()` code points into `buf`, return how many were written.
/// `state` persists between calls on the same input.
pub type ToWcharFn = fn(input: &mut &[u8], buf: &mut [u32], state: &mut u32) -> usize;

/// Encoder signature: encode `input` into `buf`. `end` is true for the final
/// call (encoders with pending state must flush it then).
pub type FromWcharFn = fn(input: &[u32], buf: &mut ConvertBuf, end: bool);

/// Optional fast validity checker.
pub type CheckFn = fn(input: &[u8]) -> bool;

/// Optional `mb_strcut` implementation: cut `len` bytes starting at byte
/// offset `from`, aligned to character boundaries.
pub type CutFn = fn(input: &[u8], from: usize, len: usize) -> Vec<u8>;

const HEX: &[u8; 16] = b"0123456789ABCDEF";

fn push_cp_hex(cp: u32, out: &mut Vec<u32>) {
    let mut nonzero = false;
    let mut shift: i32 = 28;
    while shift >= 0 {
        let n = (cp >> shift) & 0xF;
        if n != 0 || nonzero {
            nonzero = true;
            out.push(u32::from(HEX[n as usize]));
        }
        shift -= 4;
    }
    if !nonzero {
        out.push(u32::from(b'0'));
    }
}

/// `mb_illegal_marker`: the code points which replace `bad_cp`.
fn illegal_marker(bad_cp: u32, err_mode: ErrorMode, replacement_char: u32) -> Vec<u32> {
    let mut out = Vec::with_capacity(12);
    if bad_cp == BAD_INPUT {
        if err_mode != ErrorMode::None {
            out.push(replacement_char);
        }
    } else {
        match err_mode {
            ErrorMode::Char => out.push(replacement_char),
            ErrorMode::Long => {
                out.extend([u32::from(b'U'), u32::from(b'+')]);
                push_cp_hex(bad_cp, &mut out);
            }
            ErrorMode::Entity => {
                out.extend([u32::from(b'&'), u32::from(b'#'), u32::from(b'x')]);
                push_cp_hex(bad_cp, &mut out);
                out.push(u32::from(b';'));
            }
            ErrorMode::None | ErrorMode::BadUtf8 => {}
        }
    }
    out
}

/// `mb_illegal_output`: report that `bad_cp` could not be encoded by `f`,
/// emitting the substitution chosen by `buf.error_mode`.
pub fn illegal_output(bad_cp: u32, f: FromWcharFn, buf: &mut ConvertBuf) {
    buf.errors += 1;

    let repl_char = buf.replacement_char;
    let err_mode = buf.error_mode;

    if err_mode == ErrorMode::BadUtf8 {
        buf.push(0xFF);
        return;
    }

    let marker = illegal_marker(bad_cp, err_mode, repl_char);

    // Avoid infinite recursion if `f` cannot encode the replacement either.
    if err_mode == ErrorMode::Char && repl_char != u32::from(b'?') {
        buf.replacement_char = u32::from(b'?');
    } else {
        buf.error_mode = ErrorMode::None;
    }

    f(&marker, buf, false);

    buf.replacement_char = repl_char;
    buf.error_mode = err_mode;
}

/// Resolve the effective (from, to) pair for a conversion: transfer
/// encodings (`Base64`, `QPrint`, `UUencode`) operate on raw bytes
/// (`mbfl_convert_filter_get_vtbl` / `mb_fast_convert`).
pub fn effective_pair(
    from: &'static Encoding,
    to: &'static Encoding,
) -> (&'static Encoding, &'static Encoding) {
    use crate::encoding::EncodingNo as N;
    if matches!(to.no, N::Base64 | N::Qprint) {
        (crate::encoding::get(N::EightBit), to)
    } else if matches!(from.no, N::Base64 | N::Qprint | N::Uuencode) {
        (from, crate::encoding::get(N::EightBit))
    } else {
        (from, to)
    }
}

/// `mb_fast_convert`: convert `input` from one encoding to another. Returns
/// the converted bytes and the number of errors.
pub fn fast_convert(
    input: &[u8],
    from: &'static Encoding,
    to: &'static Encoding,
    replacement_char: u32,
    error_mode: ErrorMode,
) -> (Vec<u8>, u32) {
    let (from, to) = effective_pair(from, to);
    let mut wchar_buf = [0u32; 128];
    let mut state = 0u32;
    let mut buf = ConvertBuf::new(input.len(), replacement_char, error_mode);
    let mut rest = input;
    while !rest.is_empty() {
        let n = (from.to_wchar)(&mut rest, &mut wchar_buf, &mut state);
        (to.from_wchar)(&wchar_buf[..n], &mut buf, rest.is_empty());
    }
    let errors = buf.errors;
    (buf.into_bytes(), errors)
}

/// Decode an entire byte string to code points (with [`BAD_INPUT`] markers),
/// in 128-code-point chunks exactly as libmbfl's callers do.
pub fn decode_all(input: &[u8], enc: &'static Encoding) -> Vec<u32> {
    let mut out = Vec::with_capacity(input.len());
    let mut wchar_buf = [0u32; 128];
    let mut state = 0u32;
    let mut rest = input;
    while !rest.is_empty() {
        let n = (enc.to_wchar)(&mut rest, &mut wchar_buf, &mut state);
        out.extend_from_slice(&wchar_buf[..n]);
    }
    out
}

/// Encode a complete code point sequence.
pub fn encode_all(
    wchars: &[u32],
    enc: &'static Encoding,
    replacement_char: u32,
    error_mode: ErrorMode,
) -> Vec<u8> {
    let mut buf = ConvertBuf::new(wchars.len(), replacement_char, error_mode);
    (enc.from_wchar)(wchars, &mut buf, true);
    buf.into_bytes()
}

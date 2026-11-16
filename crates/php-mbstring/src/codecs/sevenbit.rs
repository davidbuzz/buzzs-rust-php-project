// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `7bit`.
//!
//! Ref: `libmbfl/filters/mbfilter_7bit.c`.

use crate::convert::{illegal_output, ConvertBuf, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_SBCS};
use crate::legacy::{self, Filter, Sink, Vtbl, BAD_INPUT_I32};

/// `7bit`.
pub static ENC_7BIT: Encoding = Encoding {
    no: EncodingNo::SevenBit,
    name: "7bit",
    mime_name: Some("7bit"),
    aliases: &[],
    mblen_table: None,
    flag: ENCTYPE_SBCS,
    input_filter: Some(&VTBL_7BIT_WCHAR),
    output_filter: Some(&VTBL_WCHAR_7BIT),
    to_wchar: sevenbit_to_wchar,
    from_wchar: wchar_to_7bit,
    check: None,
    cut: None,
};

/// `vtbl_7bit_wchar`.
pub static VTBL_7BIT_WCHAR: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_7bit_any,
    flush: legacy::common_flush,
};

/// `vtbl_wchar_7bit`.
pub static VTBL_WCHAR_7BIT: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_any_7bit,
    flush: legacy::common_flush,
};

/// `mbfl_filt_conv_7bit_any`.
pub fn filt_7bit_any(c: i32, _filter: &mut Filter, out: &mut dyn Sink) {
    out.put(if c < 0x80 { c } else { BAD_INPUT_I32 });
}

/// `mbfl_filt_conv_any_7bit`.
pub fn filt_any_7bit(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    if (0..0x80).contains(&c) {
        out.put(c);
    } else {
        legacy::illegal_output(c, filter, out);
    }
}

fn sevenbit_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let n = input.len().min(buf.len());
    for (o, &c) in buf.iter_mut().zip(&input[..n]) {
        *o = if c < 0x80 { u32::from(c) } else { BAD_INPUT };
    }
    *input = &input[n..];
    n
}

#[allow(clippy::cast_possible_truncation)]
fn wchar_to_7bit(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        if w <= 0x7F {
            buf.push(w as u8);
        } else {
            illegal_output(w, wchar_to_7bit, buf);
        }
    }
}

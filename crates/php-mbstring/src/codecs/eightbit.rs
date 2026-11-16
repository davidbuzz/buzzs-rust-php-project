// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `8bit` (binary) and the internal `pass` pseudo-encoding.
//!
//! Ref: `libmbfl/mbfl/mbfilter_8bit.c`, `libmbfl/mbfl/mbfilter_pass.c`.

use crate::convert::{illegal_output, ConvertBuf};
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_SBCS};
use crate::legacy::{self, Filter, Sink, Vtbl};

/// `8bit`.
pub static ENC_8BIT: Encoding = Encoding {
    no: EncodingNo::EightBit,
    name: "8bit",
    mime_name: Some("8bit"),
    aliases: &["binary"],
    mblen_table: None,
    flag: ENCTYPE_SBCS,
    input_filter: Some(&VTBL_8BIT_WCHAR),
    output_filter: Some(&VTBL_WCHAR_8BIT),
    to_wchar: eightbit_to_wchar,
    from_wchar: wchar_to_8bit,
    check: None,
    cut: None,
};

/// `pass`: no conversion. Only `mb_http_output` accepts it; it is not in the
/// public encoding list. Its codecs behave like `8bit`.
pub static ENC_PASS: Encoding = Encoding {
    no: EncodingNo::Pass,
    name: "pass",
    mime_name: None,
    aliases: &["none"],
    mblen_table: None,
    flag: 0,
    input_filter: Some(&legacy::VTBL_PASS),
    output_filter: Some(&legacy::VTBL_PASS),
    to_wchar: eightbit_to_wchar,
    from_wchar: wchar_to_8bit,
    check: None,
    cut: None,
};

/// `vtbl_8bit_wchar`.
pub static VTBL_8BIT_WCHAR: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: legacy::pass_function,
    flush: legacy::common_flush,
};

/// `vtbl_wchar_8bit`.
pub static VTBL_WCHAR_8BIT: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_wchar_8bit,
    flush: legacy::common_flush,
};

fn filt_wchar_8bit(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    if (0..0x100).contains(&c) {
        out.put(c);
    } else {
        legacy::illegal_output(c, filter, out);
    }
}

fn eightbit_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let n = input.len().min(buf.len());
    for (o, &c) in buf.iter_mut().zip(&input[..n]) {
        *o = u32::from(c);
    }
    *input = &input[n..];
    n
}

#[allow(clippy::cast_possible_truncation)]
fn wchar_to_8bit(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        if w <= 0xFF {
            buf.push(w as u8);
        } else {
            illegal_output(w, wchar_to_8bit, buf);
        }
    }
}

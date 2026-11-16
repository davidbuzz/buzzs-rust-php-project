// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The legacy byte/char-at-a-time conversion filters of libmbfl
//! (`mbfl_convert_filter`).
//!
//! Modern libmbfl converts with the buffer-oriented functions in
//! [`crate::convert`]; the old streaming filters survive only where PHP still
//! uses them: `mbfl_strcut`, i.e. `mb_strcut` for stateful encodings which
//! have neither a `cut` function, a fixed width nor an `mblen` table
//! (`ISO-2022-*`, `JIS`, `CP5022x`, `HZ`, `UTF-7`, `UTF7-IMAP`, `7bit`, `HTML-ENTITIES`,
//! `Base64`, `QPrint`, `UUencode`).
//!
//! A filter receives one value at a time (a byte, or a code point) and
//! pushes output values into a [`Sink`].
//!
//! Ref: `libmbfl/mbfl/mbfl_convert.c`, `libmbfl/mbfl/mbfl_convert.h`.

use crate::convert::{ErrorMode, BAD_INPUT};

/// Downstream consumer of a filter's output (`output_function` +
/// `flush_function`).
pub trait Sink {
    /// Receive one output value.
    fn put(&mut self, c: i32);
    /// Flush downstream (default: nothing to do).
    fn flush(&mut self) {}
}

/// A sink which discards everything (`mbfl_filter_output_null`).
#[derive(Debug, Default)]
pub struct NullSink;

impl Sink for NullSink {
    fn put(&mut self, _c: i32) {}
}

/// A sink collecting bytes (`mbfl_memory_device`).
#[derive(Debug, Default, Clone)]
pub struct ByteSink(pub Vec<u8>);

impl Sink for ByteSink {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn put(&mut self, c: i32) {
        self.0.push(c as u8);
    }
}

/// Filter step function: process one input value.
pub type FilterFn = fn(c: i32, filter: &mut Filter, out: &mut dyn Sink);
/// Filter flush function: emit any pending output, then flush downstream.
pub type FlushFn = fn(filter: &mut Filter, out: &mut dyn Sink);
/// Filter constructor (`filter_ctor`).
pub type CtorFn = fn(filter: &mut Filter);

/// A legacy conversion filter type (`mbfl_convert_vtbl`).
#[derive(Debug)]
pub struct Vtbl {
    /// Constructor; initialises `status`/`cache`/`opaque`.
    pub ctor: CtorFn,
    /// Per-value conversion function.
    pub function: FilterFn,
    /// End-of-input flush.
    pub flush: FlushFn,
}

/// A legacy conversion filter instance (`mbfl_convert_filter`).
#[derive(Debug, Clone)]
pub struct Filter {
    /// Main state variable.
    pub status: i32,
    /// Secondary state variable.
    pub cache: i32,
    /// Error handling mode.
    pub illegal_mode: ErrorMode,
    /// Substitute character for [`ErrorMode::Char`].
    pub illegal_substchar: u32,
    /// Number of illegal characters seen.
    pub num_illegalchar: usize,
    /// Extra private state (HTML entity decoder buffer).
    pub opaque: Vec<u8>,
    /// The step function in use (some filters swap it at runtime).
    pub function: FilterFn,
    /// The flush function in use.
    pub flush_fn: FlushFn,
}

impl Filter {
    /// `mbfl_convert_filter_new2`: instantiate a filter from its vtable.
    pub fn new(vtbl: &Vtbl) -> Self {
        let mut f = Self {
            status: 0,
            cache: 0,
            illegal_mode: ErrorMode::Char,
            illegal_substchar: u32::from(b'?'),
            num_illegalchar: 0,
            opaque: Vec::new(),
            function: vtbl.function,
            flush_fn: vtbl.flush,
        };
        (vtbl.ctor)(&mut f);
        f
    }

    /// Feed one value.
    pub fn feed(&mut self, c: i32, out: &mut dyn Sink) {
        (self.function)(c, self, out);
    }

    /// Flush the filter.
    pub fn flush(&mut self, out: &mut dyn Sink) {
        (self.flush_fn)(self, out);
    }

    /// Feed a sequence of ASCII bytes through this filter's own step function
    /// (`mbfl_convert_filter_strcat`).
    pub fn strcat(&mut self, s: &[u8], out: &mut dyn Sink) {
        for &b in s {
            self.feed(i32::from(b), out);
        }
    }
}

/// `mbfl_filt_conv_common_ctor`.
pub fn common_ctor(filter: &mut Filter) {
    filter.status = 0;
    filter.cache = 0;
}

/// `mbfl_filt_conv_common_flush`.
pub fn common_flush(_filter: &mut Filter, out: &mut dyn Sink) {
    out.flush();
}

/// `mbfl_filt_conv_pass`.
pub fn pass_function(c: i32, _filter: &mut Filter, out: &mut dyn Sink) {
    out.put(c);
}

/// `vtbl_pass`.
pub static VTBL_PASS: Vtbl = Vtbl {
    ctor: common_ctor,
    function: pass_function,
    flush: common_flush,
};

const HEX: &[u8; 16] = b"0123456789ABCDEF";

#[allow(clippy::cast_sign_loss)]
fn output_hex(w: u32, filter: &mut Filter, out: &mut dyn Sink) {
    let mut nonzero = false;
    let mut shift: i32 = 28;
    while shift >= 0 {
        let n = (w >> shift) & 0xF;
        if n != 0 || nonzero {
            nonzero = true;
            filter.feed(i32::from(HEX[n as usize]), out);
        }
        shift -= 4;
    }
    if !nonzero {
        filter.feed(i32::from(b'0'), out);
    }
}

/// `mbfl_filt_conv_illegal_output`: substitute an unconvertible value.
#[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
pub fn illegal_output(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    let w = c as u32;
    let mode_backup = filter.illegal_mode;
    let substchar_backup = filter.illegal_substchar;

    if filter.illegal_mode == ErrorMode::Char && filter.illegal_substchar != u32::from(b'?') {
        filter.illegal_substchar = u32::from(b'?');
    } else {
        filter.illegal_mode = ErrorMode::None;
    }

    match mode_backup {
        ErrorMode::Char => filter.feed(substchar_backup as i32, out),
        ErrorMode::Long => {
            if w == BAD_INPUT {
                filter.feed(substchar_backup as i32, out);
            } else {
                filter.strcat(b"U+", out);
                output_hex(w, filter, out);
            }
        }
        ErrorMode::Entity => {
            if w == BAD_INPUT {
                filter.feed(substchar_backup as i32, out);
            } else {
                filter.strcat(b"&#x", out);
                output_hex(w, filter, out);
                filter.strcat(b";", out);
            }
        }
        ErrorMode::None | ErrorMode::BadUtf8 => {}
    }

    filter.illegal_mode = mode_backup;
    filter.illegal_substchar = substchar_backup;
    filter.num_illegalchar += 1;
}

/// [`BAD_INPUT`] as the `int` libmbfl filters pass around.
#[allow(clippy::cast_possible_wrap)]
pub const BAD_INPUT_I32: i32 = BAD_INPUT as i32;

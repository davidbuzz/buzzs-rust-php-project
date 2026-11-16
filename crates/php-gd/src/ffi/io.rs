// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A `gdIOCtx` that writes into a Rust [`Write`] (PHP's
//! `create_output_context()` / `create_stream_context()` in gd.c).

use std::io::Write;
use std::os::raw::{c_int, c_void};

use super::sys;

/// The context: libgd only sees the leading `gdIOCtx`.
#[repr(C)]
pub(super) struct WriteCtx<'a> {
    ctx: sys::gdIOCtx,
    out: &'a mut dyn Write,
    error: Option<std::io::Error>,
}

impl<'a> WriteCtx<'a> {
    pub(super) fn new(out: &'a mut dyn Write) -> Self {
        WriteCtx {
            ctx: sys::gdIOCtx {
                getC: None,
                getBuf: None,
                putC: Some(put_c),
                putBuf: Some(put_buf),
                seek: None,
                tell: None,
                gd_free: Some(no_free),
                data: std::ptr::null_mut(),
            },
            out,
            error: None,
        }
    }

    /// The pointer handed to libgd.
    pub(super) fn as_ctx(&mut self) -> *mut sys::gdIOCtx {
        std::ptr::from_mut(self).cast()
    }

    /// The first write error, if any.
    pub(super) fn finish(self) -> std::io::Result<()> {
        match self.error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn write(&mut self, bytes: &[u8]) -> bool {
        if self.error.is_some() {
            return false;
        }
        match self.out.write_all(bytes) {
            Ok(()) => true,
            Err(e) => {
                self.error = Some(e);
                false
            }
        }
    }
}

/// Recovers the context from libgd's pointer.
///
/// # Safety
/// `ctx` must be the pointer returned by [`WriteCtx::as_ctx`] for a context
/// that is alive and not otherwise borrowed.
unsafe fn ctx_of<'a>(ctx: *mut sys::gdIOCtx) -> &'a mut WriteCtx<'a> {
    // SAFETY: `WriteCtx` is `repr(C)` with the `gdIOCtx` first, so the
    // pointers coincide; liveness is the caller's contract.
    unsafe { &mut *ctx.cast::<WriteCtx<'a>>() }
}

unsafe extern "C" fn put_c(ctx: *mut sys::gdIOCtx, c: c_int) {
    // SAFETY: libgd calls this with the context it was given.
    let w = unsafe { ctx_of(ctx) };
    // `(unsigned char) c`: only the low byte is written.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let b = c as u8;
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| w.write(&[b])));
}

unsafe extern "C" fn put_buf(ctx: *mut sys::gdIOCtx, buf: *const c_void, len: c_int) -> c_int {
    let Ok(n) = usize::try_from(len) else {
        return 0;
    };
    if buf.is_null() || n == 0 {
        return 0;
    }
    // SAFETY: libgd calls this with the context it was given.
    let w = unsafe { ctx_of(ctx) };
    // SAFETY: libgd passes `len` readable bytes at `buf`.
    let bytes = unsafe { std::slice::from_raw_parts(buf.cast::<u8>(), n) };
    let ok =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| w.write(bytes))).unwrap_or(false);
    if ok {
        len
    } else {
        0
    }
}

unsafe extern "C" fn no_free(_ctx: *mut sys::gdIOCtx) {}

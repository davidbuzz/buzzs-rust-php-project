// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! libgd's error callback (`gdSetErrorMethod`): messages are formatted with
//! the C `vsnprintf` and queued per thread, for the binding to raise as
//! `func(): message` diagnostics once the libgd call returns (PHP's
//! `php_gd_error_method()` raises them immediately; the order relative to
//! the caller's own diagnostics is the same).

use std::cell::RefCell;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_void};
use std::sync::Once;

use super::sys;

/// Severity of a libgd message, mapped like `php_gd_error_method()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// `GD_DEBUG`, `GD_INFO`, `GD_NOTICE` → `E_NOTICE`.
    Notice,
    /// `GD_WARNING` → `E_WARNING`.
    Warning,
    /// Anything else (`GD_ERROR`) → `E_ERROR`.
    Error,
}

/// One message emitted by libgd.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// Mapped severity.
    pub level: Level,
    /// The formatted text (PHP's rewrites of the libpng texts applied).
    pub text: Vec<u8>,
}

thread_local! {
    static QUEUE: RefCell<Vec<Message>> = const { RefCell::new(Vec::new()) };
}

/// `LOG_WARNING` / `LOG_NOTICE` / `LOG_INFO` / `LOG_DEBUG` (`gd_errors.h`).
const GD_WARNING: c_int = 4;
const GD_NOTICE: c_int = 5;
const GD_INFO: c_int = 6;
const GD_DEBUG: c_int = 7;

/// Installs the error method (once per process).
pub fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: `error_method` has the C ABI of `gdErrorMethod` (the
        // `va_list` parameter is passed as a pointer on the supported
        // targets, see `format`) and stays valid for the whole process.
        unsafe { sys::gdSetErrorMethod(error_method) };
    });
}

/// Takes the queued messages.
pub fn take() -> Vec<Message> {
    QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// Queues a message (also used by the pure-Rust parts of the crate).
pub fn push(level: Level, text: Vec<u8>) {
    QUEUE.with(|q| q.borrow_mut().push(Message { level, text }));
}

/// The callback handed to libgd.
unsafe extern "C" fn error_method(priority: c_int, fmt_ptr: *const c_char, ap: *mut c_void) {
    if fmt_ptr.is_null() {
        return;
    }
    // SAFETY: libgd passes a NUL-terminated format string.
    let fmt = unsafe { CStr::from_ptr(fmt_ptr) }.to_bytes();
    // `php_gd_error_method()`: keep PHP's historical libpng texts.
    let fmt_override: Option<&CStr> = if fmt == b"gd-png: fatal libpng error: %s\n" {
        Some(c"gd-png:  fatal libpng error: %s")
    } else if fmt.starts_with(b"gd-png error: setjmp returns error condition") {
        Some(c"gd-png error: setjmp returns error condition")
    } else if fmt.starts_with(b"GD2 image support has been disabled") {
        Some(c"gd-gd2: GD2 image truncated or corrupt")
    } else {
        None
    };
    let used = fmt_override.map_or(fmt_ptr, CStr::as_ptr);
    // SAFETY: `used` is a valid format string whose conversions are those
    // of the original (or a prefix of them), and `ap` is the `va_list`
    // libgd built for it; it is consumed exactly once.
    let text = unsafe { format(used, ap) };
    let level = match priority {
        GD_DEBUG | GD_INFO | GD_NOTICE => Level::Notice,
        GD_WARNING => Level::Warning,
        _ => Level::Error,
    };
    push(level, text);
}

/// Formats `fmt` with the C library.
///
/// # Safety
/// `fmt` must be a valid format string and `ap` the `va_list` holding its
/// arguments. On x86-64 and `AArch64` Linux a `va_list` parameter is passed
/// as a pointer (an array type on x86-64, an indirectly passed 32-byte
/// struct on `AArch64`), so forwarding the raw pointer to `vsnprintf` is
/// ABI-correct there; elsewhere only the format string is returned.
unsafe fn format(fmt: *const c_char, ap: *mut c_void) -> Vec<u8> {
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    {
        let mut buf = vec![0u8; 4096];
        // SAFETY: the buffer is writable for its whole length; see above
        // for `fmt`/`ap`.
        let n = unsafe { sys::vsnprintf(buf.as_mut_ptr().cast(), buf.len(), fmt, ap) };
        let n = usize::try_from(n).unwrap_or(0).min(buf.len() - 1);
        buf.truncate(n);
        buf
    }
    #[cfg(not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )))]
    {
        let _ = ap;
        // SAFETY: `fmt` is NUL-terminated.
        unsafe { CStr::from_ptr(fmt) }.to_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_round_trip() {
        let _ = take();
        push(Level::Warning, b"x".to_vec());
        let m = take();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].level, Level::Warning);
        assert!(take().is_empty());
    }
}

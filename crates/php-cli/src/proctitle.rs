// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Process title support (`sapi/cli/ps_title.c`, the Linux
//! "clobber argv" strategy): `ps` shows `/proc/<pid>/cmdline`, which is
//! the memory holding the original `argv` strings, so a new title is
//! written over that area and padded with NUL bytes.
//!
//! Unlike PHP the environment strings that follow `argv` are not moved
//! away, so the title is limited to the length of the original command
//! line; `cli_get_process_title()` still returns the full title.

use std::cell::RefCell;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::OnceLock;

static ARGC: AtomicUsize = AtomicUsize::new(0);
static ARGV: AtomicPtr<*mut libc::c_char> = AtomicPtr::new(std::ptr::null_mut());

/// glibc calls `.init_array` functions with `(argc, argv, envp)`; this
/// records where the original argument strings live.
extern "C" fn capture_args(
    count: libc::c_int,
    strings: *mut *mut libc::c_char,
    _env: *mut *mut libc::c_char,
) {
    ARGC.store(usize::try_from(count).unwrap_or(0), Ordering::Relaxed);
    ARGV.store(strings, Ordering::Relaxed);
}

#[used]
#[link_section = ".init_array.00099"]
static ARGV_INIT: extern "C" fn(libc::c_int, *mut *mut libc::c_char, *mut *mut libc::c_char) =
    capture_args;

thread_local! {
    static TITLE: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

/// The contiguous memory holding the `argv` strings, measured once (it
/// cannot be measured again after `-H` blanked the arguments).
fn argv_area() -> Option<(*mut u8, usize)> {
    static AREA: OnceLock<Option<(usize, usize)>> = OnceLock::new();
    AREA.get_or_init(|| measure_argv_area().map(|(p, n)| (p as usize, n)))
        .map(|(p, n)| (p as *mut u8, n))
}

fn measure_argv_area() -> Option<(*mut u8, usize)> {
    let count = ARGC.load(Ordering::Relaxed);
    let argv = ARGV.load(Ordering::Relaxed);
    if argv.is_null() || count == 0 {
        return None;
    }
    // SAFETY: `argv` and its `argc` entries were handed to us by the C
    // runtime and stay valid for the whole process; every entry is a
    // NUL-terminated string.
    unsafe {
        let start = (*argv).cast::<u8>();
        if start.is_null() {
            return None;
        }
        let mut end = start;
        for i in 0..count {
            let s = (*argv.add(i)).cast::<u8>();
            if s.is_null() || (i > 0 && s != end.add(1)) {
                break;
            }
            let len = libc::strlen(s.cast());
            end = s.add(len);
        }
        let size = usize::try_from(end.offset_from(start)).ok()?;
        Some((start, size))
    }
}

/// Whether titles can be set (`PHP_CLI_PROCESS_TITLE`).
#[must_use]
pub fn available() -> bool {
    argv_area().is_some_and(|(_, n)| n > 0)
}

/// `set_ps_title()`.
pub fn set(title: &[u8]) -> bool {
    let Some((start, size)) = argv_area() else {
        return false;
    };
    let n = title.len().min(size);
    // SAFETY: `start..start + size + 1` is the argv string area (the
    // last string's NUL terminator included), writable process memory
    // that nothing else references after startup (`std::env::args()` is
    // read before any title is set).
    unsafe {
        std::ptr::copy_nonoverlapping(title.as_ptr(), start, n);
        std::ptr::write_bytes(start.add(n), 0, size + 1 - n);
    }
    TITLE.with(|t| *t.borrow_mut() = Some(title.to_vec()));
    true
}

/// `-H`: blanks the arguments after `argv[0]` so tools such as `ps` do
/// not show them.
pub fn hide_args() {
    let _ = argv_area();
    let count = ARGC.load(Ordering::Relaxed);
    let argv = ARGV.load(Ordering::Relaxed);
    if argv.is_null() {
        return;
    }
    for i in 1..count {
        // SAFETY: entries `1..count` of the C runtime's `argv` are valid
        // NUL-terminated strings in writable memory; only their bytes
        // before the terminator are overwritten.
        unsafe {
            let s = *argv.add(i);
            if !s.is_null() {
                std::ptr::write_bytes(s.cast::<u8>(), 0, libc::strlen(s));
            }
        }
    }
}

/// `get_ps_title()`: the last title set, or the empty string before any
/// was (C's `ps_buffer_cur_len` starts at 0: `cli_get_process_title()`
/// returns `""`, not the command line); `None` when titles cannot be
/// set at all.
#[must_use]
pub fn get() -> Option<Vec<u8>> {
    if let Some(t) = TITLE.with(|t| t.borrow().clone()) {
        return Some(t);
    }
    argv_area()?;
    Some(Vec::new())
}

#[cfg(test)]
mod tests {
    #[test]
    fn captures_the_argument_area() {
        assert!(super::available());
    }

    /// php8.5 -r 'var_dump(cli_get_process_title());' prints string(0) "".
    #[test]
    fn title_is_empty_until_one_is_set() {
        assert_eq!(super::get(), Some(Vec::new()));
    }
}

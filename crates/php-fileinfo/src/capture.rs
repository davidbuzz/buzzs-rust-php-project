// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Capturing what libmagic writes to `stderr`.
//!
//! PHP's bundled libmagic routes `file_magwarn()` (magic-file parse
//! warnings such as ``offset `a' invalid``) through `php_error_docref()`.
//! The system library prints them to `stderr` instead, as
//! `"<file>, <line>: Warning: <message>\n"`.  To surface them as PHP
//! warnings we temporarily point file descriptor 2 at an anonymous
//! temporary file while the library runs, then parse what it wrote.
//!
//! The redirection is process-wide: anything another thread writes to fd 2
//! during the (short) window is captured too.  A global lock serialises
//! captures so they never nest.

#![allow(unsafe_code)]

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::sync::Mutex;

use crate::ffi;

static LOCK: Mutex<()> = Mutex::new(());

/// Runs `f` with `stderr` redirected and returns its result together with
/// everything written to fd 2 meanwhile.  When the redirection cannot be
/// set up, `f` still runs and the captured output is empty.
pub(crate) fn with_captured_stderr<T>(f: impl FnOnce() -> T) -> (T, Vec<u8>) {
    let guard = LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(mut sink) = temp_file() else {
        return (f(), Vec::new());
    };
    // SAFETY: `dup` only duplicates descriptor 2; a negative result is
    // handled below.
    let saved = unsafe { ffi::dup(2) };
    if saved < 0 {
        return (f(), Vec::new());
    }
    // SAFETY: `sink` is an open file we own for the whole capture; fd 2 is
    // restored from `saved` before `sink` is dropped.
    let redirected = unsafe { ffi::dup2(sink.as_raw_fd(), 2) } >= 0;
    let result = f();
    if redirected {
        // SAFETY: `saved` is the duplicate of the original fd 2 made above.
        unsafe { ffi::dup2(saved, 2) };
    }
    // SAFETY: `saved` was returned by `dup` and is closed exactly once.
    unsafe { ffi::close(saved) };
    drop(guard);
    let mut out = Vec::new();
    if redirected && sink.seek(SeekFrom::Start(0)).is_ok() {
        let _ = sink.read_to_end(&mut out);
    }
    (result, out)
}

/// An already-unlinked read/write temporary file.
fn temp_file() -> Option<File> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir();
    for _ in 0..16 {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(".php-fileinfo-{}-{n}", std::process::id()));
        if let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            let _ = std::fs::remove_file(&path);
            return Some(file);
        }
    }
    None
}

/// Turns captured libmagic diagnostics into PHP warning texts: each line's
/// `"<file>, <line>: "` prefix and `"Warning: "` tag are dropped, as PHP's
/// `file_magwarn()` reports only the message.
pub(crate) fn magwarn_messages(captured: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(captured);
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|line| {
            line.find("Warning: ")
                .map_or(line, |i| &line[i + "Warning: ".len()..])
                .to_owned()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_file_and_line_prefix() {
        let msgs = magwarn_messages(
            b"/x/bad.magic, 1: Warning: offset `string' invalid\nWarning: plain\n\nother\n",
        );
        assert_eq!(msgs, ["offset `string' invalid", "plain", "other"]);
    }

    #[test]
    fn captures_writes_to_fd2() {
        let ((), out) = with_captured_stderr(|| {
            use std::io::Write;
            let mut f = std::io::stderr();
            let _ = f.write_all(b"captured!\n");
        });
        assert_eq!(out, b"captured!\n");
    }
}

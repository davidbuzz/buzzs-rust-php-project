// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Capture of the VM's error log while a request runs.
//!
//! With `log_errors=1` and no `error_log` file, PHP hands each logged
//! message to the SAPI (`sapi_cli_server_log_message`), which prints it as
//! a timestamped console line.  The VM writes such messages straight to
//! file descriptor 2, so during a request fd 2 is pointed at an in-memory
//! file; [`write_stderr`] (all of the server's own console output) first
//! forwards whatever was captured, giving lines that start with `PHP ` the
//! `[time] ` prefix, and then writes to the real stderr.

use std::cell::RefCell;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// An active capture.
#[derive(Debug)]
struct Capture {
    /// The memory file fd 2 points at.
    mem: File,
    /// A duplicate of the original stderr.
    saved: File,
    /// Bytes of `mem` already forwarded.
    offset: u64,
    /// Whether the next forwarded byte starts a line.
    at_line_start: bool,
}

thread_local! {
    static ACTIVE: RefCell<Option<Capture>> = const { RefCell::new(None) };
}

fn dup(fd: i32) -> Option<OwnedFd> {
    // SAFETY: dup has no memory-safety preconditions; a valid result is a
    // fresh descriptor we take ownership of.
    let r = unsafe { libc::dup(fd) };
    // SAFETY: `r` is a newly created, owned descriptor.
    (r >= 0).then(|| unsafe { OwnedFd::from_raw_fd(r) })
}

fn dup2(from: i32, to: i32) -> bool {
    // SAFETY: dup2 only manipulates the descriptor table.
    unsafe { libc::dup2(from, to) >= 0 }
}

/// Starts capturing fd 2 on this thread's behalf (no-op if active or on
/// failure).
pub fn start() {
    ACTIVE.with(|a| {
        let mut a = a.borrow_mut();
        if a.is_some() {
            return;
        }
        let _ = std::io::stderr().flush();
        // SAFETY: memfd_create takes a NUL-terminated name and flags.
        let fd = unsafe { libc::memfd_create(c"php-cli-server-log".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return;
        }
        // SAFETY: `fd` is a fresh descriptor owned from here on.
        let mem = File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        let Some(saved) = dup(2) else {
            return;
        };
        if !dup2(mem.as_raw_fd(), 2) {
            return;
        }
        *a = Some(Capture {
            mem,
            saved: File::from(saved),
            offset: 0,
            at_line_start: true,
        });
    });
}

/// Splits captured bytes into console output: lines starting with `PHP `
/// get the log prefix.
fn forward(c: &mut Capture) {
    let mut data = Vec::new();
    if c.mem.seek(SeekFrom::Start(c.offset)).is_err() || c.mem.read_to_end(&mut data).is_err() {
        return;
    }
    c.offset += data.len() as u64;
    let mut out = Vec::with_capacity(data.len() + 64);
    let mut start_of_line = c.at_line_start;
    let mut i = 0;
    while i < data.len() {
        let end = data[i..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(data.len(), |p| i + p + 1);
        let line = &data[i..end];
        if start_of_line && line.starts_with(b"PHP ") {
            let body = line.strip_suffix(b"\n").unwrap_or(line);
            out.extend_from_slice(&crate::log::format_line(body));
            if !line.ends_with(b"\n") {
                out.pop();
            }
        } else {
            out.extend_from_slice(line);
        }
        start_of_line = line.ends_with(b"\n");
        i = end;
    }
    c.at_line_start = start_of_line;
    let _ = c.saved.write_all(&out);
}

/// Writes console output: forwards pending captured data first.
pub fn write_stderr(data: &[u8]) {
    let done = ACTIVE.with(|a| {
        let mut a = a.borrow_mut();
        let Some(c) = a.as_mut() else {
            return false;
        };
        forward(c);
        let _ = c.saved.write_all(data);
        true
    });
    if !done {
        crate::log::raw(data);
    }
}

/// Stops capturing: forwards the rest and restores fd 2.
pub fn stop() {
    ACTIVE.with(|a| {
        if let Some(mut c) = a.borrow_mut().take() {
            forward(&mut c);
            dup2(c.saved.as_raw_fd(), 2);
        }
    });
}

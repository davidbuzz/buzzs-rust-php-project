// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! PHP streams as the engine's [`Sink`] / [`Source`], and the local-file
//! opening of `ftp_get`/`ftp_put` (`php_stream_open_wrapper`).
//!
//! Every function here must run inside `php_ext_streams::bridge::enter`
//! so that user-space stream wrappers can call back into the VM.

use std::rc::Rc;

use php_ext_streams::bridge;
use php_ext_streams::res::StreamCell;
use php_ftp::{Sink, Source};
use php_streams::stream::{flags, Stream};
use php_streams::wrapper::REPORT_ERRORS;

/// `SEEK_SET`.
pub(crate) const SEEK_SET: i32 = 0;
/// `SEEK_END`.
pub(crate) const SEEK_END: i32 = 2;

/// A stream used by a transfer: owned by the extension (`ftp_get`,
/// `ftp_nb_put`, … open the local file themselves) or a user's stream
/// resource (`ftp_fget`, `ftp_nb_fput`, …).
#[derive(Debug)]
pub(crate) enum PhpStream {
    /// Opened (and closed) by the extension.
    Owned(Box<Stream>),
    /// The user's stream resource.
    Shared(Rc<StreamCell>),
}

impl PhpStream {
    /// Runs `f` on the stream; `None` when the resource was closed or is
    /// busy (re-entered from its own callback).
    pub(crate) fn with<R>(&mut self, f: impl FnOnce(&mut Stream) -> R) -> Option<R> {
        match self {
            PhpStream::Owned(s) => Some(f(s)),
            PhpStream::Shared(c) => c.get().map(|mut s| f(&mut s)),
        }
    }

    /// `php_stream_seek`.
    pub(crate) fn seek(&mut self, offset: i64, whence: i32) {
        self.with(|s| s.seek(offset, whence));
    }

    /// `php_stream_tell`.
    pub(crate) fn tell(&mut self) -> i64 {
        self.with(|s| s.tell()).unwrap_or(0)
    }

    /// `php_stream_close` for owned streams (a no-op for user resources).
    pub(crate) fn close(self) {
        if let PhpStream::Owned(mut s) = self {
            s.close();
        }
    }
}

impl Sink for PhpStream {
    fn write(&mut self, data: &[u8]) -> bool {
        let ok = matches!(self.with(|s| s.write(data)), Some(Ok(n)) if n == data.len());
        ok && !bridge::has_pending()
    }
}

impl Source for PhpStream {
    fn eof(&mut self) -> bool {
        self.with(Stream::eof).unwrap_or(true)
    }

    fn read(&mut self, buf: &mut [u8]) -> Option<usize> {
        if bridge::has_pending() {
            return None;
        }
        self.with(|s| s.read(buf).ok()).flatten()
    }

    fn get_line(&mut self, maxlen: usize) -> Option<Vec<u8>> {
        if bridge::has_pending() {
            return None;
        }
        // Force Unix line endings (`PHP_STREAM_FLAG_EOL_UNIX`) so the line
        // search looks for "\n", restoring the stream's own mode after.
        self.with(|s| {
            let mask = flags::DETECT_EOL | flags::EOL_MAC;
            let old = [flags::DETECT_EOL, flags::EOL_MAC]
                .into_iter()
                .filter(|&f| s.has_flag(f))
                .fold(0, |a, f| a | f);
            s.clear_flags(mask);
            let line = s.get_line(Some(maxlen));
            s.add_flags(old);
            line
        })
        .flatten()
    }
}

/// A sink that discards everything (a non-blocking transfer whose stream
/// vanished).
#[derive(Debug, Default)]
pub(crate) struct NullSink;

impl Sink for NullSink {
    fn write(&mut self, _data: &[u8]) -> bool {
        false
    }
}

impl Source for NullSink {
    fn eof(&mut self) -> bool {
        true
    }

    fn read(&mut self, _buf: &mut [u8]) -> Option<usize> {
        None
    }

    fn get_line(&mut self, _maxlen: usize) -> Option<Vec<u8>> {
        None
    }
}

/// `php_stream_open_wrapper(path, mode, REPORT_ERRORS, NULL)`.
pub(crate) fn open(path: &[u8], mode: &str) -> Option<Stream> {
    let mut opened = None;
    php_streams::open::open_wrapper(path, mode, REPORT_ERRORS, &mut opened, None)
}

/// Opens the local file of `ftp_get` / `ftp_nb_get`: for a resumed
/// download (`autoseek` and a non-zero position) the existing file is
/// opened for update and positioned (at its end for `FTP_AUTORESUME`,
/// updating `resumepos`); otherwise it is truncated.
pub(crate) fn open_for_download(
    local: &[u8],
    ascii: bool,
    autoseek: bool,
    resumepos: &mut i64,
) -> Option<PhpStream> {
    let (update, create) = if ascii { ("rt+", "wt") } else { ("rb+", "wb") };
    if autoseek && *resumepos != 0 {
        let s = open(local, update).or_else(|| open(local, create))?;
        let mut s = PhpStream::Owned(Box::new(s));
        if *resumepos == php_ftp::AUTORESUME {
            s.seek(0, SEEK_END);
            *resumepos = s.tell();
        } else {
            s.seek(*resumepos, SEEK_SET);
        }
        Some(s)
    } else {
        open(local, create).map(|s| PhpStream::Owned(Box::new(s)))
    }
}

/// `VCWD_UNLINK(local)` after a failed download (errors ignored).
pub(crate) fn unlink(local: &[u8]) {
    use std::os::unix::ffi::OsStrExt;
    let _ = std::fs::remove_file(std::ffi::OsStr::from_bytes(local));
}

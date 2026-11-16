// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `compress.zlib://` stream wrapper and the streams `gzopen()`,
//! `gzfile()` and `readgzfile()` open (`ext/zlib/zlib_fopen_wrapper.c`).
//!
//! Like PHP, the inner stream is opened through the wrapper layer
//! (`STREAM_MUST_SEEK | STREAM_WILL_CAST`), cast to a file descriptor, and
//! a [`GzStream`] runs over `dup(fd)`; the resulting stream is unbuffered
//! (`PHP_STREAM_FLAG_NO_BUFFER`). Register [`ZlibWrapper`] as
//! `"compress.zlib"`.

use std::any::Any;
use std::fs::File;
use std::os::fd::AsRawFd;

use php_streams::context::ContextRef;
use php_streams::diag::{self, Severity, StreamErrorCode};
use php_streams::open::open_wrapper;
use php_streams::stream::{
    flags, CastAs, OpFailed, Stream, StreamOps, StreamOption, StreamState, OPT_NOTIMPL, SEEK_END,
};
use php_streams::wrapper::{
    OpenError, Wrapper, DEFAULT_WRAPPER_NAME, REPORT_ERRORS, STREAM_MUST_SEEK, STREAM_WILL_CAST,
};

use crate::conv::{try_get_long, type_name};
use crate::ffi::dup_fd;
use crate::gz::{GzStream, SEEK_END_MESSAGE};

/// The wrapper and stream ops label.
pub const LABEL: &str = "ZLIB";

const Z_OK: i32 = 0;
const Z_SYNC_FLUSH: i32 = 2;

/// `php_stream_gzio_ops`: a PHP stream over a [`GzStream`].
#[derive(Debug)]
pub struct GzStreamOps {
    gz: Option<GzStream<File>>,
    inner: Option<Stream>,
}

impl GzStreamOps {
    /// Wraps an open [`GzStream`] and the inner stream it was created from
    /// (closed together with it).
    pub fn new(gz: GzStream<File>, inner: Option<Stream>) -> Self {
        GzStreamOps {
            gz: Some(gz),
            inner,
        }
    }

    /// The gzip stream (for `gzeof()`-style queries).
    pub fn gz(&self) -> Option<&GzStream<File>> {
        self.gz.as_ref()
    }
}

impl StreamOps for GzStreamOps {
    fn label(&self) -> &'static str {
        LABEL
    }

    fn read(&mut self, st: &mut StreamState, buf: &mut [u8]) -> Result<usize, OpFailed> {
        let Some(gz) = self.gz.as_mut() else {
            return Err(OpFailed);
        };
        let r = gz.read(buf);
        if gz.eof() {
            st.eof = true;
        }
        if let Ok(n) = r {
            Ok(n)
        } else {
            if st.flags & flags::SUPPRESS_ERRORS == 0 {
                if let Some(msg) = gz.io_error_report("Read", buf.len().min(i32::MAX as usize)) {
                    st.notice(StreamErrorCode::ReadFailed, msg);
                }
            }
            Err(OpFailed)
        }
    }

    fn write(&mut self, st: &mut StreamState, buf: &[u8]) -> Result<usize, OpFailed> {
        let Some(gz) = self.gz.as_mut() else {
            return Err(OpFailed);
        };
        let mut total = 0;
        for chunk in buf.chunks(i32::MAX as usize) {
            let n = gz.write(chunk);
            if n == 0 && !chunk.is_empty() {
                if st.flags & flags::SUPPRESS_ERRORS == 0 {
                    if let Some(msg) = gz.io_error_report("Write", chunk.len()) {
                        st.notice(StreamErrorCode::ReadFailed, msg);
                    }
                }
                break;
            }
            total += n;
        }
        Ok(total)
    }

    fn close(&mut self, _st: &mut StreamState, preserve_handle: bool) -> i32 {
        let mut ret = -1;
        if !preserve_handle {
            if let Some(gz) = self.gz.take() {
                ret = gz.close().1;
            }
            if let Some(mut inner) = self.inner.take() {
                inner.close();
            }
        }
        ret
    }

    fn flush(&mut self, _st: &mut StreamState) -> i32 {
        self.gz.as_mut().map_or(-1, |gz| gz.flush(Z_SYNC_FLUSH))
    }

    fn can_seek(&self) -> bool {
        true
    }

    fn seek(&mut self, _st: &mut StreamState, offset: i64, whence: i32, newpos: &mut i64) -> bool {
        if whence == SEEK_END {
            diag::stream_error(
                Severity::Warning,
                DEFAULT_WRAPPER_NAME,
                StreamErrorCode::SeekNotSupported,
                true,
                SEEK_END_MESSAGE,
            );
            return false;
        }
        let Some(gz) = self.gz.as_mut() else {
            return false;
        };
        match gz.seek(offset, whence) {
            Ok(pos) => {
                *newpos = pos;
                true
            }
            Err(_) => false,
        }
    }

    fn set_option(&mut self, _st: &mut StreamState, opt: &mut StreamOption<'_>) -> i64 {
        match opt {
            StreamOption::Locking(_) | StreamOption::LockSupported | StreamOption::MetaData(_) => {
                self.inner
                    .as_mut()
                    .map_or(OPT_NOTIMPL, |s| s.set_option(opt))
            }
            _ => OPT_NOTIMPL,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// `php_stream_wrapper_log_warn()`: reports now with `REPORT_ERRORS`,
/// otherwise records the message for the "Failed to open stream" warning.
fn log_warn(
    err: &mut OpenError,
    label: &str,
    options: u32,
    code: StreamErrorCode,
    msg: impl Into<Vec<u8>>,
) {
    if options & REPORT_ERRORS != 0 {
        diag::stream_error(Severity::Warning, label, code, true, msg);
    } else {
        err.messages.push(msg.into());
    }
}

fn strip_prefix(path: &[u8]) -> &[u8] {
    for prefix in [&b"compress.zlib://"[..], b"zlib:"] {
        if path.len() >= prefix.len() && path[..prefix.len()].eq_ignore_ascii_case(prefix) {
            return &path[prefix.len()..];
        }
    }
    path
}

/// `php_stream_gzopen()`. `via_wrapper` is true when called as the
/// `compress.zlib://` opener (PHP passes the wrapper), false for
/// `gzopen()`/`gzfile()`/`readgzfile()` (PHP passes `NULL`; their
/// `options` include `REPORT_ERRORS`).
pub fn gzopen(
    via_wrapper: bool,
    path: &[u8],
    mode: &str,
    options: u32,
    opened_path: &mut Option<Vec<u8>>,
    context: Option<&ContextRef>,
) -> Result<Stream, OpenError> {
    let label = if via_wrapper {
        LABEL
    } else {
        DEFAULT_WRAPPER_NAME
    };
    let mut err = OpenError::default();
    if mode.contains('+') {
        log_warn(
            &mut err,
            label,
            REPORT_ERRORS,
            StreamErrorCode::ModeNotSupported,
            "Cannot open a zlib stream for reading and writing at the same time!",
        );
        return Err(err);
    }
    let path = strip_prefix(path);
    let Some(mut inner) = open_wrapper(
        path,
        mode,
        STREAM_MUST_SEEK | options | STREAM_WILL_CAST,
        opened_path,
        context,
    ) else {
        return Err(err);
    };
    let Some(fd) = inner.cast(CastAs::Fd, true) else {
        inner.close();
        return Err(err);
    };
    let Some(file) = dup_fd(fd) else {
        log_warn(
            &mut err,
            label,
            options,
            StreamErrorCode::OpenFailed,
            "gzopen failed",
        );
        inner.close();
        return Err(err);
    };
    let dup = file.as_raw_fd();
    let mut gz = match GzStream::open(file, mode, dup) {
        Ok(gz) => gz,
        Err(_file) => {
            log_warn(
                &mut err,
                label,
                options,
                StreamErrorCode::OpenFailed,
                "gzopen failed",
            );
            inner.close();
            return Err(err);
        }
    };
    let level = context.and_then(|c| c.borrow().get("zlib", "level").cloned());
    if let Some(zlevel) = level {
        match try_get_long(&zlevel) {
            None => log_warn(
                &mut err,
                label,
                REPORT_ERRORS,
                StreamErrorCode::InvalidParam,
                format!(
                    "zlib \"level\" context option must be of type int, {} given",
                    type_name(&zlevel)
                ),
            ),
            Some(l) => {
                let l = i32::try_from(l).unwrap_or(i32::MAX);
                if gz.set_params(l, 0) != Z_OK {
                    log_warn(
                        &mut err,
                        label,
                        REPORT_ERRORS,
                        StreamErrorCode::Generic,
                        "failed setting compression level",
                    );
                }
            }
        }
    }
    let mut stream = Stream::new(Box::new(GzStreamOps::new(gz, Some(inner))), mode);
    stream.add_flags(flags::NO_BUFFER);
    Ok(stream)
}

/// The `compress.zlib://` wrapper (`php_stream_gzip_wrapper`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ZlibWrapper;

impl Wrapper for ZlibWrapper {
    fn label(&self) -> &'static str {
        LABEL
    }

    fn open(
        &self,
        path: &[u8],
        mode: &str,
        options: u32,
        opened_path: &mut Option<Vec<u8>>,
        context: Option<&ContextRef>,
    ) -> Result<Stream, OpenError> {
        gzopen(true, path, mode, options, opened_path, context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("php-zlib-{}-{name}", std::process::id()));
        p
    }

    #[test]
    fn write_then_read_through_streams() {
        let _ = diag::take();
        let path = tmp("rw.gz");
        let p = path.to_str().unwrap().as_bytes().to_vec();
        let mut opened = None;
        let mut s = gzopen(false, &p, "wb", REPORT_ERRORS, &mut opened, None).unwrap();
        assert_eq!(s.ops_label(), "ZLIB");
        s.write(b"hello world\n").unwrap();
        assert_eq!(s.close(), 0);
        let raw = std::fs::read(&path).unwrap();
        assert_eq!(
            raw.iter().fold(String::new(), |mut acc, b| {
                use std::fmt::Write as _;
                let _ = write!(acc, "{b:02x}");
                acc
            }),
            "1f8b0800000000000003ca48cdc9c95728cf2fca49e102000000ffff03002d3b08af0c000000"
        );
        let mut url = b"compress.zlib://".to_vec();
        url.extend(&p);
        let mut s = ZlibWrapper.open(&url, "rb", 0, &mut opened, None).unwrap();
        let mut buf = [0u8; 64];
        let n = s.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello world\n");
        assert_eq!(s.seek(6, 0), 0);
        let n = s.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"world\n");
        assert_eq!(s.seek(0, SEEK_END), -1);
        let d = diag::take();
        assert_eq!(d.last().unwrap().text(), "SEEK_END is not supported");
        s.close();
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn mode_errors() {
        let _ = diag::take();
        let mut opened = None;
        let err = gzopen(false, b"whatever", "r+", REPORT_ERRORS, &mut opened, None).unwrap_err();
        assert!(err.messages.is_empty());
        assert_eq!(
            diag::take()[0].text(),
            "Cannot open a zlib stream for reading and writing at the same time!"
        );
        let path = tmp("c-mode");
        let p = path.to_str().unwrap().as_bytes().to_vec();
        assert!(gzopen(false, &p, "c", REPORT_ERRORS, &mut opened, None).is_err());
        assert_eq!(diag::take().last().unwrap().text(), "gzopen failed");
        let err = gzopen(true, &p, "c", 0, &mut opened, None).unwrap_err();
        assert_eq!(err.messages, vec![b"gzopen failed".to_vec()]);
        let _ = std::fs::remove_file(&path);
    }
}

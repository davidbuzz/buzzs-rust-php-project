//! `bzopen()`, the `compress.bzip2://` wrapper (`_php_stream_bz2open()`),
//! the PHP stream ops over a [`BzFile`] (`php_stream_bz2io_ops`) and
//! `bzerrno()`/`bzerrstr()`/`bzerror()`/`bzread()` argument handling.
//! Register [`Bz2Wrapper`] as `"compress.bzip2"`.

use std::any::Any;
use std::fs::{File, OpenOptions};

use php_streams::context::ContextRef;
use php_streams::open::open_wrapper;
use php_streams::stream::{CastAs, OpFailed, Stream, StreamOps, StreamState};
use php_streams::wrapper::{OpenError, Wrapper, REPORT_ERRORS, STREAM_WILL_CAST};

use crate::error::{arg_type_error, arg_value_error, Error, Result};
use crate::ffi::dup_fd;
use crate::file::BzFile;

/// The wrapper and stream ops label.
pub const LABEL: &str = "BZip2";

/// `php_stream_bz2io_ops`.
#[derive(Debug)]
pub struct Bz2StreamOps {
    bz: Option<BzFile<File>>,
    inner: Option<Stream>,
}

impl Bz2StreamOps {
    /// Wraps a [`BzFile`]; `inner` is the stream it was opened from when
    /// this layer owns it (closed together with the bz2 stream).
    pub fn new(bz: BzFile<File>, inner: Option<Stream>) -> Self {
        Bz2StreamOps {
            bz: Some(bz),
            inner,
        }
    }

    /// `BZ2_bzerror()`: `(errno, errstr)`.
    pub fn error(&self) -> (i64, &'static str) {
        self.bz.as_ref().map_or((0, "OK"), |b| {
            let (n, s) = b.error();
            (i64::from(n), s)
        })
    }
}

impl StreamOps for Bz2StreamOps {
    fn label(&self) -> &'static str {
        LABEL
    }

    fn read(
        &mut self,
        st: &mut StreamState,
        buf: &mut [u8],
    ) -> std::result::Result<usize, OpFailed> {
        let Some(bz) = self.bz.as_mut() else {
            return Err(OpFailed);
        };
        let mut ret = 0usize;
        while ret < buf.len() {
            let to_read = (buf.len() - ret).min(i32::MAX as usize);
            match bz.read(&mut buf[ret..ret + to_read]) {
                Some(0) => {
                    st.eof = true;
                    break;
                }
                Some(n) => ret += n,
                None => {
                    // Not safe to keep reading after an error (#72613).
                    st.eof = true;
                    return if ret > 0 { Ok(ret) } else { Err(OpFailed) };
                }
            }
        }
        Ok(ret)
    }

    fn write(&mut self, _st: &mut StreamState, buf: &[u8]) -> std::result::Result<usize, OpFailed> {
        let Some(bz) = self.bz.as_mut() else {
            return Err(OpFailed);
        };
        let mut wrote = 0usize;
        while wrote < buf.len() {
            let to_write = (buf.len() - wrote).min(i32::MAX as usize);
            match bz.write(&buf[wrote..wrote + to_write]) {
                None => return if wrote == 0 { Err(OpFailed) } else { Ok(wrote) },
                Some(0) => break,
                Some(n) => wrote += n,
            }
        }
        Ok(wrote)
    }

    fn close(&mut self, _st: &mut StreamState, preserve_handle: bool) -> i32 {
        if !preserve_handle {
            if let Some(bz) = self.bz.take() {
                drop(bz.close());
            }
        }
        if let Some(mut inner) = self.inner.take() {
            inner.close_ex(preserve_handle);
        }
        -1
    }

    fn flush(&mut self, _st: &mut StreamState) -> i32 {
        self.bz.as_mut().map_or(0, BzFile::flush)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

fn strip_prefix(path: &[u8]) -> &[u8] {
    const P: &[u8] = b"compress.bzip2://";
    if path.len() >= P.len() && path[..P.len()].eq_ignore_ascii_case(P) {
        &path[P.len()..]
    } else {
        path
    }
}

/// `BZ2_bzopen(path, mode)`: `fopen(path, "rb"/"wb")` plus the BZFILE.
fn bzopen_file(path: &[u8], mode: &str) -> Option<BzFile<File>> {
    use std::os::unix::ffi::OsStrExt;
    let p = std::path::Path::new(std::ffi::OsStr::from_bytes(path));
    let writing = crate::file::OpenMode::parse(mode).writing;
    let file = if writing {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(p)
            .ok()?
    } else {
        // fopen() refuses directories only when reading from them; File
        // opens them, so reject explicitly.
        let f = File::open(p).ok()?;
        if f.metadata().ok()?.is_dir() {
            return None;
        }
        f
    };
    BzFile::open(file, mode)
}

/// `_php_stream_bz2open()`: tries the path as a plain file, then any
/// stream wrapper (cast to a file descriptor).
pub fn bz2open(
    path: &[u8],
    mode: &str,
    options: u32,
    opened_path: &mut Option<Vec<u8>>,
    context: Option<&ContextRef>,
) -> std::result::Result<Stream, OpenError> {
    let path = strip_prefix(path);
    let m = mode.as_bytes();
    if m.is_empty() || (m[0] != b'w' && m[0] != b'r' && m.len() > 1) {
        return Err(OpenError::default());
    }
    let mut bz = bzopen_file(path, mode);
    if bz.is_some() {
        *opened_path = Some(path.to_vec());
    }
    let mut stream = None;
    if bz.is_none() {
        stream = open_wrapper(path, mode, options | STREAM_WILL_CAST, opened_path, context);
        if let Some(s) = stream.as_mut() {
            if let Some(fd) = s.cast(CastAs::Fd, true) {
                bz = dup_fd(fd).and_then(|f| BzFile::open(f, mode));
            }
        }
        if bz.is_none() && m[0] == b'w' {
            if let Some(p) = opened_path.as_ref() {
                use std::os::unix::ffi::OsStrExt;
                let _ = std::fs::remove_file(std::ffi::OsStr::from_bytes(p));
            }
        }
    }
    if let Some(bz) = bz {
        Ok(Stream::new(Box::new(Bz2StreamOps::new(bz, stream)), mode))
    } else {
        if let Some(mut s) = stream {
            s.close();
        }
        Err(OpenError::default())
    }
}

/// Validates `bzopen()`'s `$mode`.
fn check_mode(mode: &[u8]) -> Result<u8> {
    if mode.len() != 1 || (mode[0] != b'r' && mode[0] != b'w') {
        return Err(arg_value_error(
            "bzopen",
            2,
            "mode",
            "must be either \"r\" or \"w\"",
        ));
    }
    Ok(mode[0])
}

/// `bzopen(string $file, string $mode)`. `Ok(None)` is `false` (the
/// warnings were already reported by the stream layer).
pub fn bzopen_path(
    file: &[u8],
    mode: &[u8],
    context: Option<&ContextRef>,
) -> Result<Option<Stream>> {
    let m = check_mode(mode)?;
    if file.is_empty() {
        return Err(arg_value_error("bzopen", 1, "file", "must not be empty"));
    }
    if file.contains(&0) {
        return Err(arg_type_error(
            "bzopen",
            1,
            "file",
            "must not contain null bytes",
        ));
    }
    let mode = if m == b'r' { "r" } else { "w" };
    let mut opened = None;
    Ok(bz2open(file, mode, REPORT_ERRORS, &mut opened, context).ok())
}

/// `bzopen($stream_resource, string $mode)`: the mode compatibility rules
/// and the BZFILE over `dup(fd)`. The returned stream does not own
/// `inner`; PHP closes the inner stream when the bz2 stream is closed, so
/// the VM must close it then. `Err(Error::Warning)` and `Ok(None)` both
/// mean `false` (the latter after the cast already reported).
pub fn bzopen_stream(inner: &mut Stream, mode: &[u8]) -> Result<Option<Stream>> {
    let m = check_mode(mode)?;
    let smode = inner.mode.as_bytes();
    let unsupported = || {
        Error::Warning(format!(
            "Cannot use stream opened in mode '{}'",
            String::from_utf8_lossy(smode)
        ))
    };
    let primary = match smode.len() {
        1 => smode[0],
        2 => {
            let (p, s) = if smode[0] == b'b' {
                (smode[1], smode[0])
            } else {
                (smode[0], smode[1])
            };
            if s != b'b' {
                return Err(unsupported());
            }
            p
        }
        _ => return Err(unsupported()),
    };
    if m == b'r' {
        if primary != b'r' {
            return Err(Error::Warning(
                "Cannot read from a stream opened in write only mode".into(),
            ));
        }
    } else if !b"wax".contains(&primary) {
        return Err(Error::Warning(
            "cannot write to a stream opened in read only mode".into(),
        ));
    }
    let Some(fd) = inner.cast(CastAs::Fd, true) else {
        return Ok(None);
    };
    let mode = if m == b'r' { "r" } else { "w" };
    Ok(dup_fd(fd)
        .and_then(|f| BzFile::open(f, mode))
        .map(|bz| Stream::new(Box::new(Bz2StreamOps::new(bz, None)), mode)))
}

/// `bzerrno()`/`bzerrstr()`/`bzerror()` (`php_bz2_error()`): the error
/// number and string of a bz2 stream, or the `TypeError` for other
/// streams.
pub fn bz_error(func: &str, stream: &Stream) -> Result<(i64, &'static str)> {
    stream
        .ops_as::<Bz2StreamOps>()
        .map(Bz2StreamOps::error)
        .ok_or_else(|| arg_type_error(func, 1, "bz", "must be a bz2 stream"))
}

/// `bzread()`'s `$length` validation.
pub fn bzread_length(length: i64) -> Result<usize> {
    usize::try_from(length)
        .map_err(|_| arg_value_error("bzread", 2, "length", "must be greater than or equal to 0"))
}

/// The `compress.bzip2://` wrapper (`php_stream_bzip2_wrapper`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Bz2Wrapper;

impl Wrapper for Bz2Wrapper {
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
    ) -> std::result::Result<Stream, OpenError> {
        bz2open(path, mode, options, opened_path, context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use php_streams::diag;

    fn tmp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("php-bz2-{}-{name}", std::process::id()));
        p
    }

    #[test]
    fn argument_errors() {
        let e = |r: Result<Option<Stream>>| r.unwrap_err().to_string();
        assert_eq!(
            e(bzopen_path(b"", b"r", None)),
            "ValueError: bzopen(): Argument #1 ($file) must not be empty"
        );
        assert_eq!(
            e(bzopen_path(b"x", b"", None)),
            "ValueError: bzopen(): Argument #2 ($mode) must be either \"r\" or \"w\""
        );
        assert_eq!(
            e(bzopen_path(b"x", b"x", None)),
            "ValueError: bzopen(): Argument #2 ($mode) must be either \"r\" or \"w\""
        );
        assert_eq!(
            e(bzopen_path(b"x", b"rw", None)),
            "ValueError: bzopen(): Argument #2 ($mode) must be either \"r\" or \"w\""
        );
        assert_eq!(
            e(bzopen_path(b"a\0b", b"r", None)),
            "TypeError: bzopen(): Argument #1 ($file) must not contain null bytes"
        );
        assert_eq!(
            bzread_length(-10).unwrap_err().to_string(),
            "ValueError: bzread(): Argument #2 ($length) must be greater than or equal to 0"
        );
        assert_eq!(bzread_length(0).unwrap(), 0);
    }

    #[test]
    fn missing_file_warns() {
        let _ = diag::take();
        let p = tmp("does-not-exist");
        let r = bzopen_path(p.to_str().unwrap().as_bytes(), b"r", None).unwrap();
        assert!(r.is_none());
        let d = diag::take();
        assert!(d.iter().any(|x| x
            .text()
            .contains("Failed to open stream: No such file or directory")));
    }

    #[test]
    fn write_read_and_errors() {
        let path = tmp("rw.bz2");
        let p = path.to_str().unwrap().as_bytes().to_vec();
        let mut w = bzopen_path(&p, b"w", None).unwrap().unwrap();
        assert_eq!(w.ops_label(), "BZip2");
        assert_eq!(bz_error("bzerrno", &w).unwrap(), (0, "OK"));
        w.write(b"Hello bz2 world").unwrap();
        w.close();
        let raw = std::fs::read(&path).unwrap();
        assert_eq!(
            crate::bzdecompress(&raw, false).unwrap(),
            crate::Output::Data(b"Hello bz2 world".to_vec())
        );
        let mut url = b"compress.bzip2://".to_vec();
        url.extend(&p);
        let mut opened = None;
        let mut r = Bz2Wrapper.open(&url, "r", 0, &mut opened, None).unwrap();
        let mut buf = [0u8; 64];
        let n = r.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"Hello bz2 world");
        r.close();
        // A plain text file: DATA_ERROR_MAGIC on read.
        std::fs::write(&path, b"plain text, not bzip2").unwrap();
        let mut r = bzopen_path(&p, b"r", None).unwrap().unwrap();
        assert!(r.read(&mut buf).is_err() || r.eof());
        assert_eq!(bz_error("bzerror", &r).unwrap(), (-5, "DATA_ERROR_MAGIC"));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn stream_resource_modes() {
        let path = tmp("modes.txt");
        std::fs::write(&path, b"").unwrap();
        let p = path.to_str().unwrap().as_bytes().to_vec();
        let mut opened = None;
        for (fmode, bmode, expect) in [
            ("w", "w", None),
            ("r", "r", None),
            (
                "r",
                "w",
                Some("cannot write to a stream opened in read only mode"),
            ),
            (
                "w",
                "r",
                Some("Cannot read from a stream opened in write only mode"),
            ),
            ("r+", "r", Some("Cannot use stream opened in mode 'r+'")),
            ("rb", "r", None),
            ("wb", "w", None),
            (
                "a",
                "r",
                Some("Cannot read from a stream opened in write only mode"),
            ),
            ("a", "w", None),
        ] {
            let mut s = open_wrapper(&p, fmode, REPORT_ERRORS, &mut opened, None).unwrap();
            match (bzopen_stream(&mut s, bmode.as_bytes()), expect) {
                (Ok(Some(st)), None) => assert_eq!(st.ops_label(), "BZip2"),
                (Err(Error::Warning(m)), Some(want)) => assert_eq!(m, want),
                (other, want) => panic!("{fmode}/{bmode}: {other:?} vs {want:?}"),
            }
            s.close();
        }
        let s = open_wrapper(&p, "r", REPORT_ERRORS, &mut opened, None).unwrap();
        assert_eq!(
            bz_error("bzerrstr", &s).unwrap_err().to_string(),
            "TypeError: bzerrstr(): Argument #1 ($bz) must be a bz2 stream"
        );
        std::fs::remove_file(&path).unwrap();
    }
}

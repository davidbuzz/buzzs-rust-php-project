//! Entry streams (`zip_stream.c`): the read-only `php_stream_zipio_ops`
//! returned by `ZipArchive::getStream*()` and the `zip://` wrapper
//! (`zip://archive.zip#entry`).
//!
//! libzip 1.7.3 has no `zip_file_is_seekable()`, so — like PHP built
//! against it — entry streams are never seekable.

use std::any::Any;
use std::rc::Rc;

use php_streams::context::ContextRef;
use php_streams::stat::StatBuf;
use php_streams::stream::{OpFailed, Stream, StreamOps, StreamState};
use php_streams::value::Value as StreamValue;
use php_streams::wrapper::{OpenError, Wrapper};

use crate::archive::{copy_cstr, cstr, error_string, Archive};
use crate::error::warning;
use crate::ffi::{self, zip_file_t, zip_t};
use crate::paths::MAXPATHLEN;

/// An open entry (`zip_file_t`), closed on drop.
#[derive(Debug)]
pub(crate) struct ZipFile(pub(crate) *mut zip_file_t);

impl ZipFile {
    /// `zip_fread()`: `None` on error.
    pub(crate) fn read(&self, buf: &mut [u8]) -> Option<usize> {
        // SAFETY: `self.0` is an open entry; `buf` is writable for its length.
        let n = unsafe { ffi::zip_fread(self.0, buf.as_mut_ptr().cast(), buf.len() as u64) };
        usize::try_from(n).ok()
    }

    /// The libzip error code of the entry.
    pub(crate) fn error_code(&self) -> i32 {
        // SAFETY: open entry; its error struct is owned by it.
        unsafe { ffi::zip_error_code_zip(ffi::zip_file_get_error(self.0)) }
    }

    /// `zip_error_strerror(zip_file_get_error(zf))`.
    pub(crate) fn error_message(&self) -> String {
        // SAFETY: open entry; its error struct is owned by it.
        unsafe { error_string(ffi::zip_file_get_error(self.0)) }
    }

    /// `zip_fclose()`, returning its result.
    pub(crate) fn close(mut self) -> i32 {
        let zf = std::mem::replace(&mut self.0, std::ptr::null_mut());
        // SAFETY: `zf` is open and closed exactly once.
        unsafe { ffi::zip_fclose(zf) }
    }
}

impl Drop for ZipFile {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: open entry, closed exactly once.
            unsafe { ffi::zip_fclose(self.0) };
        }
    }
}

/// `struct php_zip_stream_data_t` + `php_stream_zipio_ops`.
pub struct ZipStreamOps {
    zf: Option<ZipFile>,
    /// The archive opened by the `zip://` wrapper (closed with the stream).
    za: *mut zip_t,
    /// The `ZipArchive` state borrowed by `getStream*()` streams.
    archive: Option<Rc<Archive>>,
    cursor: u64,
    /// The path the stream was opened with (`stream->orig_path`).
    path: Option<Vec<u8>>,
}

impl std::fmt::Debug for ZipStreamOps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZipStreamOps")
            .field("open", &self.zf.is_some())
            .field("cursor", &self.cursor)
            .field(
                "path",
                &self.path.as_ref().map(|p| String::from_utf8_lossy(p)),
            )
            .finish_non_exhaustive()
    }
}

impl ZipStreamOps {
    /// A stream over an entry of an object's archive, keeping the archive
    /// alive (`php_stream_zip_open()`).
    pub(crate) fn for_archive(zf: *mut zip_file_t, archive: Rc<Archive>) -> Self {
        ZipStreamOps {
            zf: Some(ZipFile(zf)),
            za: std::ptr::null_mut(),
            archive: Some(archive),
            cursor: 0,
            path: None,
        }
    }

    /// The number of bytes read so far.
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    fn release(&mut self) {
        if let Some(zf) = self.zf.take() {
            zf.close();
        }
        if !self.za.is_null() {
            // SAFETY: the wrapper's own archive, closed exactly once.
            unsafe { ffi::zip_close(self.za) };
            self.za = std::ptr::null_mut();
        }
        if let Some(a) = self.archive.take() {
            let _ = Archive::release(a, None);
        }
    }
}

impl Drop for ZipStreamOps {
    fn drop(&mut self) {
        self.release();
    }
}

/// Splits `zip://archive#entry` (or `archive#entry`) into the archive
/// path and the entry name, as `php_stream_zip_opener()` does.
fn split_path(path: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let hash = path.iter().position(|&c| c == b'#')?;
    let fragment_len = path.len() - hash;
    let path = if path.len() >= 6 && path[..6].eq_ignore_ascii_case(b"zip://") {
        &path[6..]
    } else {
        path
    };
    if path.len() >= MAXPATHLEN || fragment_len > path.len() {
        return None;
    }
    let dir = path[..path.len() - fragment_len].to_vec();
    let entry = path[path.len() - fragment_len + 1..].to_vec();
    Some((dir, entry))
}

impl StreamOps for ZipStreamOps {
    fn label(&self) -> &'static str {
        "zip"
    }

    fn read(&mut self, st: &mut StreamState, buf: &mut [u8]) -> Result<usize, OpFailed> {
        let Some(zf) = &self.zf else {
            return Ok(0);
        };
        match zf.read(buf) {
            None => {
                st.eof = true;
                let mut m = b"Zip stream error: ".to_vec();
                m.extend_from_slice(zf.error_message().as_bytes());
                warning(m);
                Err(OpFailed)
            }
            Some(n) => {
                if n == 0 || n < buf.len() {
                    st.eof = true;
                } else {
                    self.cursor += n as u64;
                }
                Ok(n)
            }
        }
    }

    fn write(&mut self, _st: &mut StreamState, buf: &[u8]) -> Result<usize, OpFailed> {
        Ok(buf.len())
    }

    fn close(&mut self, _st: &mut StreamState, preserve_handle: bool) -> i32 {
        if preserve_handle {
            if let Some(a) = self.archive.take() {
                let _ = Archive::release(a, None);
            }
        } else {
            self.release();
        }
        libc::EOF
    }

    /// `php_zip_ops_stat()`: re-opens the archive named by the stream path.
    fn stat(&mut self, _st: &mut StreamState) -> Option<StatBuf> {
        let path = self.path.as_deref()?;
        stat_url(path)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// `php_zip_ops_stat()` for a `zip://archive#entry` path.
pub fn stat_url(path: &[u8]) -> Option<StatBuf> {
    let (dir, entry) = split_path(path)?;
    if !php_streams::paths::check_open_basedir(&dir) {
        return None;
    }
    let c = cstr(&dir);
    let mut err = 0;
    // SAFETY: C string path and a valid error output.
    let za = unsafe { ffi::zip_open(c.as_ptr(), ffi::ZIP_RDONLY, &raw mut err) };
    if za.is_null() {
        return None;
    }
    let ce = cstr(&entry);
    let mut sb = std::mem::MaybeUninit::<ffi::zip_stat_t>::zeroed();
    // SAFETY: open archive, C string, zeroed output struct.
    let r = unsafe { ffi::zip_stat(za, ce.as_ptr(), ffi::ZIP_FL_NOCASE, sb.as_mut_ptr()) };
    // SAFETY: the read-only archive is closed exactly once.
    unsafe { ffi::zip_close(za) };
    if r != 0 {
        return None;
    }
    // SAFETY: zip_stat() succeeded and filled the struct.
    let sb = unsafe { sb.assume_init() };
    let is_dir = path.last() == Some(&b'/');
    #[allow(clippy::cast_possible_wrap)]
    Some(StatBuf {
        size: if is_dir { 0 } else { sb.size as i64 },
        mode: if is_dir { 0o040_000 } else { 0o100_000 },
        mtime: sb.mtime,
        atime: sb.mtime,
        ctime: sb.mtime,
        nlink: 1,
        rdev: -1,
        blksize: -1,
        blocks: -1,
        ino: -1,
        ..StatBuf::default()
    })
}

/// The `zip://` stream wrapper (`php_stream_zip_wrapper`). Register it
/// as `"zip"`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ZipWrapper;

impl Wrapper for ZipWrapper {
    fn label(&self) -> &'static str {
        "zip wrapper"
    }

    /// `php_stream_zip_opener()`.
    fn open(
        &self,
        path: &[u8],
        mode: &str,
        _options: u32,
        opened_path: &mut Option<Vec<u8>>,
        context: Option<&ContextRef>,
    ) -> Result<Stream, OpenError> {
        let Some((dir, entry)) = split_path(path) else {
            return Err(OpenError::silent());
        };
        if !mode.starts_with('r') {
            return Err(OpenError::silent());
        }
        if !php_streams::paths::check_open_basedir(&dir) {
            return Err(OpenError::silent());
        }
        let c = cstr(&dir);
        let mut err = 0;
        // SAFETY: C string path and a valid error output.
        let za = unsafe { ffi::zip_open(c.as_ptr(), ffi::ZIP_RDONLY, &raw mut err) };
        if za.is_null() {
            return Err(OpenError::silent());
        }
        if let Some(ctx) = context {
            let ctx = ctx.borrow();
            if let Some(pw) = ctx.get("zip", "password") {
                let ok = match pw {
                    StreamValue::Str(s) => {
                        let cpw = cstr(s);
                        // SAFETY: open archive; the password is copied.
                        unsafe { ffi::zip_set_default_password(za, cpw.as_ptr()) == 0 }
                    }
                    _ => false,
                };
                if !ok {
                    warning("Can't set zip password");
                }
            }
        }
        let ce = cstr(&entry);
        // SAFETY: open archive and C string.
        let zf = unsafe { ffi::zip_fopen(za, ce.as_ptr(), 0) };
        if zf.is_null() {
            // SAFETY: closes the archive opened above.
            unsafe { ffi::zip_close(za) };
            return Err(OpenError::silent());
        }
        let stripped = if path.len() >= 6 && path[..6].eq_ignore_ascii_case(b"zip://") {
            &path[6..]
        } else {
            path
        };
        *opened_path = Some(stripped.to_vec());
        let ops = ZipStreamOps {
            zf: Some(ZipFile(zf)),
            za,
            archive: None,
            cursor: 0,
            path: Some(path.to_vec()),
        };
        Ok(Stream::new(Box::new(ops), mode))
    }
}

/// The entry name of a stat result as libzip reports it.
#[allow(dead_code)]
pub(crate) fn stat_name(sb: &ffi::zip_stat_t) -> Option<Vec<u8>> {
    // SAFETY: libzip fills `name` with a valid C string or NULL.
    unsafe { copy_cstr(sb.name) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits() {
        assert_eq!(
            split_path(b"zip://a/b.zip#c/d.txt"),
            Some((b"a/b.zip".to_vec(), b"c/d.txt".to_vec()))
        );
        assert_eq!(
            split_path(b"ZIP://x.zip#"),
            Some((b"x.zip".to_vec(), Vec::new()))
        );
        assert_eq!(
            split_path(b"x.zip#e"),
            Some((b"x.zip".to_vec(), b"e".to_vec()))
        );
        assert_eq!(split_path(b"zip://x.zip"), None);
    }
}

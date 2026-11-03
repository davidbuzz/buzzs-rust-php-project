//! Safe wrapper over a libmagic `magic_set` (`magic_t`).
//!
//! This is the raw library contract, without PHP's conventions; see
//! [`crate::Finfo`] for those.

#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fmt;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::ptr::NonNull;

use crate::ffi;

/// An error reported by libmagic (`magic_errno()` / `magic_error()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MagicError {
    /// `magic_errno()`: the `errno` value of the failure, or 0.
    pub errno: i32,
    /// `magic_error()`; `None` when the library recorded no message.
    pub message: Option<String>,
}

impl fmt::Display for MagicError {
    /// PHP's `"%d:%s"` rendering (glibc prints a null `%s` as `(null)`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}",
            self.errno,
            self.message.as_deref().unwrap_or("(null)")
        )
    }
}

impl std::error::Error for MagicError {}

/// `MAGIC_PARAM_*` tunables for [`Magic::param`] / [`Magic::set_param`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Param {
    /// `MAGIC_PARAM_INDIR_MAX`.
    IndirMax = 0,
    /// `MAGIC_PARAM_NAME_MAX`.
    NameMax = 1,
    /// `MAGIC_PARAM_ELF_PHNUM_MAX`.
    ElfPhnumMax = 2,
    /// `MAGIC_PARAM_ELF_SHNUM_MAX`.
    ElfShnumMax = 3,
    /// `MAGIC_PARAM_ELF_NOTES_MAX`.
    ElfNotesMax = 4,
    /// `MAGIC_PARAM_REGEX_MAX`.
    RegexMax = 5,
    /// `MAGIC_PARAM_BYTES_MAX`: how many leading bytes are examined.
    BytesMax = 6,
    /// `MAGIC_PARAM_ENCODING_MAX`.
    EncodingMax = 7,
}

/// An open libmagic cookie.
///
/// Compiled databases handed to [`Magic::load_buffers`] are owned by the
/// cookie because libmagic keeps pointing into them.
pub struct Magic {
    ms: NonNull<ffi::magic_set>,
    buffers: Vec<Vec<u8>>,
}

// SAFETY: a `magic_set` has no thread affinity (it is plain heap state);
// every access goes through `&mut self` or `&self` without interior
// mutation that could race, so moving it to another thread is sound.
unsafe impl Send for Magic {}

impl fmt::Debug for Magic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Magic")
            .field("flags", &self.flags())
            .finish_non_exhaustive()
    }
}

impl Drop for Magic {
    fn drop(&mut self) {
        // SAFETY: `ms` came from `magic_open` and is closed exactly once.
        unsafe { ffi::magic_close(self.ms.as_ptr()) }
    }
}

/// `magic_version()` of the linked library (e.g. `545`).
pub fn version() -> i32 {
    // SAFETY: no arguments, no preconditions.
    unsafe { ffi::magic_version() }
}

/// Copies a C string returned by libmagic (which owns it).
fn owned(p: *const c_char) -> Option<Vec<u8>> {
    if p.is_null() {
        None
    } else {
        // SAFETY: libmagic returns NUL-terminated strings that stay valid
        // until the next call on the same cookie; we copy immediately.
        Some(unsafe { CStr::from_ptr(p) }.to_bytes().to_vec())
    }
}

impl Magic {
    /// `magic_open(flags)`; `None` when the library refuses the flags.
    pub fn open(flags: i32) -> Option<Self> {
        // SAFETY: plain call; a null result is handled.
        let ms = unsafe { ffi::magic_open(flags) };
        NonNull::new(ms).map(|ms| Self {
            ms,
            buffers: Vec::new(),
        })
    }

    fn raw(&self) -> ffi::magic_t {
        self.ms.as_ptr()
    }

    /// The current error, if the last call failed.
    pub fn error(&self) -> MagicError {
        // SAFETY: `raw()` is a live cookie.
        let errno = unsafe { ffi::magic_errno(self.raw()) };
        // SAFETY: as above.
        let message = owned(unsafe { ffi::magic_error(self.raw()) })
            .map(|m| String::from_utf8_lossy(&m).into_owned());
        MagicError { errno, message }
    }

    /// `magic_getflags()`.
    pub fn flags(&self) -> i32 {
        // SAFETY: `raw()` is a live cookie.
        unsafe { ffi::magic_getflags(self.raw()) }
    }

    /// `magic_setflags()`; false when the library rejects the flags.
    pub fn set_flags(&mut self, flags: i32) -> bool {
        // SAFETY: `raw()` is a live cookie.
        unsafe { ffi::magic_setflags(self.raw(), flags) == 0 }
    }

    /// `magic_load()`: `None` loads the default database (`$MAGIC` or the
    /// system's compiled `magic.mgc`); otherwise a `:`-separated list of
    /// files/directories.
    pub fn load(&mut self, path: Option<&[u8]>) -> Result<(), MagicError> {
        let c = match path {
            Some(p) => Some(CString::new(p).map_err(|_| MagicError {
                errno: 0,
                message: Some("path contains a NUL byte".into()),
            })?),
            None => None,
        };
        let ptr = c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
        // SAFETY: `ptr` is null or a NUL-terminated string alive for the call.
        if unsafe { ffi::magic_load(self.raw(), ptr) } == 0 {
            self.buffers.clear();
            Ok(())
        } else {
            Err(self.error())
        }
    }

    /// `magic_load_buffers()` over compiled (`.mgc`) databases.
    pub fn load_buffers(&mut self, buffers: Vec<Vec<u8>>) -> Result<(), MagicError> {
        let mut buffers = buffers;
        let mut ptrs: Vec<*mut c_void> = buffers
            .iter_mut()
            .map(|b| b.as_mut_ptr().cast::<c_void>())
            .collect();
        let mut sizes: Vec<usize> = buffers.iter().map(Vec::len).collect();
        // SAFETY: `ptrs`/`sizes` describe `buffers.len()` live buffers.
        // libmagic keeps pointing into them, so they are stored in `self`
        // (moving a `Vec` does not move its heap allocation).
        let rc = unsafe {
            ffi::magic_load_buffers(
                self.raw(),
                ptrs.as_mut_ptr(),
                sizes.as_mut_ptr(),
                buffers.len(),
            )
        };
        if rc == 0 {
            self.buffers = buffers;
            Ok(())
        } else {
            Err(self.error())
        }
    }

    /// `magic_buffer()`: describes `data`.
    pub fn buffer(&mut self, data: &[u8]) -> Result<Vec<u8>, MagicError> {
        // SAFETY: `data` is valid for `data.len()` bytes during the call.
        let p = unsafe { ffi::magic_buffer(self.raw(), data.as_ptr().cast(), data.len()) };
        owned(p).ok_or_else(|| self.error())
    }

    /// `magic_descriptor()`: describes what can be read from `fd` (from its
    /// current offset, which libmagic restores afterwards).
    pub fn descriptor(&mut self, fd: BorrowedFd<'_>) -> Result<Vec<u8>, MagicError> {
        // SAFETY: `fd` is a valid open descriptor for the call's duration.
        let p = unsafe { ffi::magic_descriptor(self.raw(), fd.as_raw_fd()) };
        owned(p).ok_or_else(|| self.error())
    }

    /// `magic_getparam()`.
    pub fn param(&self, param: Param) -> Option<usize> {
        let mut v: usize = 0;
        // SAFETY: every MAGIC_PARAM_* value is a `size_t`.
        let rc = unsafe {
            ffi::magic_getparam(
                self.raw(),
                param as c_int,
                std::ptr::from_mut(&mut v).cast(),
            )
        };
        (rc == 0).then_some(v)
    }

    /// `magic_setparam()`.
    pub fn set_param(&mut self, param: Param, value: usize) -> bool {
        // SAFETY: every MAGIC_PARAM_* value is a `size_t`.
        unsafe {
            ffi::magic_setparam(
                self.raw(),
                param as c_int,
                std::ptr::from_ref(&value).cast(),
            ) == 0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_sane() {
        assert!(version() >= 500);
    }

    #[test]
    fn default_database_describes_buffers() {
        let mut m = Magic::open(0).expect("open");
        m.load(None).expect("load");
        assert_eq!(m.buffer(b"").unwrap(), b"empty");
        assert_eq!(
            m.buffer(b"hello").unwrap(),
            b"ASCII text, with no line terminators"
        );
        assert!(m.set_flags(0x10));
        assert_eq!(m.flags(), 0x10);
        assert_eq!(m.buffer(b"GIF89a\x01\x00\x01\x00").unwrap(), b"image/gif");
        assert_eq!(m.param(Param::BytesMax), Some(7 * 1024 * 1024));
        assert!(m.set_param(Param::BytesMax, 1024));
        assert_eq!(m.param(Param::BytesMax), Some(1024));
    }

    #[test]
    fn error_display() {
        let e = MagicError {
            errno: 0,
            message: None,
        };
        assert_eq!(e.to_string(), "0:(null)");
    }
}

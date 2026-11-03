//! The only module that talks to libbz2 (through `bzip2-sys`).
//!
//! `bzip2-sys` links the system `libbz2` when pkg-config knows it and
//! otherwise builds the bundled bzip2 1.0.8 sources — the same release
//! distributions ship, so compressed output is identical to PHP's. A
//! `bz_stream` keeps a back pointer checked on every call, so it is boxed;
//! buffers are passed as slices per call and the raw pointers never outlive
//! the call.

use std::ffi::{c_char, c_int, c_uint};
use std::ptr;

use bzip2_sys as bz;

/// What one `BZ2_bzCompress()`/`BZ2_bzDecompress()` call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    /// The libbz2 return code.
    pub status: i32,
    /// Input bytes consumed.
    pub consumed: usize,
    /// Output bytes produced.
    pub produced: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Compress,
    Decompress,
}

/// A compression or decompression stream.
pub struct RawStream {
    strm: Box<bz::bz_stream>,
    kind: Kind,
    live: bool,
}

impl std::fmt::Debug for RawStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawStream")
            .field("kind", &self.kind)
            .field("live", &self.live)
            .finish_non_exhaustive()
    }
}

// The stream must live at a stable address (libbz2 keeps a back pointer).
#[allow(clippy::unnecessary_box_returns)]
fn fresh() -> Box<bz::bz_stream> {
    Box::new(bz::bz_stream {
        next_in: ptr::null_mut(),
        avail_in: 0,
        total_in_lo32: 0,
        total_in_hi32: 0,
        next_out: ptr::null_mut(),
        avail_out: 0,
        total_out_lo32: 0,
        total_out_hi32: 0,
        state: ptr::null_mut(),
        bzalloc: None,
        bzfree: None,
        opaque: ptr::null_mut(),
    })
}

fn clamp(len: usize) -> c_uint {
    c_uint::try_from(len).unwrap_or(c_uint::MAX)
}

impl RawStream {
    /// `BZ2_bzCompressInit(block_size_100k, 0, work_factor)`.
    pub fn compress_init(block_size_100k: i32, work_factor: i32) -> Result<Self, i32> {
        let mut strm = fresh();
        // SAFETY: `strm` is a zeroed, boxed bz_stream with default
        // allocators; libbz2 validates the numeric parameters.
        let ret =
            unsafe { bz::BZ2_bzCompressInit(&raw mut *strm, block_size_100k, 0, work_factor) };
        if ret == bz::BZ_OK {
            Ok(RawStream {
                strm,
                kind: Kind::Compress,
                live: true,
            })
        } else {
            Err(ret)
        }
    }

    /// `BZ2_bzDecompressInit(0, small)`.
    pub fn decompress_init(small: bool) -> Result<Self, i32> {
        let mut strm = fresh();
        // SAFETY: as for compress_init.
        let ret = unsafe { bz::BZ2_bzDecompressInit(&raw mut *strm, 0, c_int::from(small)) };
        if ret == bz::BZ_OK {
            Ok(RawStream {
                strm,
                kind: Kind::Decompress,
                live: true,
            })
        } else {
            Err(ret)
        }
    }

    fn run(&mut self, input: &[u8], output: &mut [u8], action: i32) -> Step {
        if !self.live {
            return Step {
                status: bz::BZ_SEQUENCE_ERROR,
                consumed: 0,
                produced: 0,
            };
        }
        let avail_in = clamp(input.len());
        let avail_out = clamp(output.len());
        self.strm.next_in = input.as_ptr().cast::<c_char>().cast_mut();
        self.strm.avail_in = avail_in;
        self.strm.next_out = output.as_mut_ptr().cast::<c_char>();
        self.strm.avail_out = avail_out;
        // SAFETY: the stream is initialised; next_in/next_out are valid for
        // avail_in/avail_out bytes during the call and libbz2 never writes
        // through next_in.
        let status = unsafe {
            match self.kind {
                Kind::Compress => bz::BZ2_bzCompress(&raw mut *self.strm, action),
                Kind::Decompress => bz::BZ2_bzDecompress(&raw mut *self.strm),
            }
        };
        let consumed = (avail_in - self.strm.avail_in) as usize;
        let produced = (avail_out - self.strm.avail_out) as usize;
        self.strm.next_in = ptr::null_mut();
        self.strm.avail_in = 0;
        self.strm.next_out = ptr::null_mut();
        self.strm.avail_out = 0;
        Step {
            status,
            consumed,
            produced,
        }
    }

    /// `BZ2_bzCompress(action)` with `BZ_RUN`, `BZ_FLUSH` or `BZ_FINISH`.
    pub fn compress(&mut self, input: &[u8], output: &mut [u8], action: i32) -> Step {
        debug_assert_eq!(self.kind, Kind::Compress);
        self.run(input, output, action)
    }

    /// `BZ2_bzDecompress()`.
    pub fn decompress(&mut self, input: &[u8], output: &mut [u8]) -> Step {
        debug_assert_eq!(self.kind, Kind::Decompress);
        self.run(input, output, 0)
    }

    /// `total_out` (64-bit).
    #[cfg(test)]
    pub fn total_out(&self) -> u64 {
        (u64::from(self.strm.total_out_hi32) << 32) | u64::from(self.strm.total_out_lo32)
    }

    /// `BZ2_bzCompressEnd()` / `BZ2_bzDecompressEnd()`; idempotent.
    pub fn end(&mut self) {
        if !self.live {
            return;
        }
        self.live = false;
        // SAFETY: the stream is initialised and is not used again.
        unsafe {
            match self.kind {
                Kind::Compress => bz::BZ2_bzCompressEnd(&raw mut *self.strm),
                Kind::Decompress => bz::BZ2_bzDecompressEnd(&raw mut *self.strm),
            };
        }
    }
}

impl Drop for RawStream {
    fn drop(&mut self) {
        self.end();
    }
}

/// `dup(fd)` wrapped in a [`std::fs::File`] owning the duplicate (PHP's
/// `php_bz2_bzdopen()`). `None` when `dup()` fails.
pub fn dup_fd(fd: i32) -> Option<std::fs::File> {
    use std::os::fd::FromRawFd;
    // SAFETY: dup() has no memory-safety preconditions; an invalid fd
    // yields -1.
    let d = unsafe { libc::dup(fd) };
    if d < 0 {
        return None;
    }
    // SAFETY: `d` is a fresh descriptor owned by nobody else; the File
    // takes ownership and closes it on drop.
    Some(unsafe { std::fs::File::from_raw_fd(d) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut c = RawStream::compress_init(4, 0).unwrap();
        let mut out = vec![0u8; 700];
        let st = c.compress(b"hello hello hello", &mut out, bz::BZ_FINISH);
        assert_eq!(st.status, bz::BZ_STREAM_END);
        assert_eq!(&out[..4], b"BZh4");
        let mut d = RawStream::decompress_init(false).unwrap();
        let mut back = vec![0u8; 64];
        let st2 = d.decompress(&out[..st.produced], &mut back);
        assert_eq!(st2.status, bz::BZ_STREAM_END);
        assert_eq!(&back[..st2.produced], b"hello hello hello");
        assert_eq!(d.total_out(), 17);
    }

    #[test]
    fn bad_params() {
        assert_eq!(
            RawStream::compress_init(10, 0).unwrap_err(),
            bz::BZ_PARAM_ERROR
        );
        let mut d = RawStream::decompress_init(true).unwrap();
        let mut out = [0u8; 8];
        assert_eq!(
            d.decompress(b"xyz", &mut out).status,
            bz::BZ_DATA_ERROR_MAGIC
        );
    }
}

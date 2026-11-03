//! Incremental BLOB I/O streams (`SQLite3::openBlob()`,
//! `Pdo\Sqlite::openBlob()`): `php_stream_sqlite3_ops` /
//! `php_stream_pdosqlite3_ops` as a [`StreamOps`] implementation.

use crate::constants::SQLITE3_OPEN_READONLY;
use crate::ffi::{RawBlob, SQLITE_OK};
use php_streams::diag;
use php_streams::stat::StatBuf;
use php_streams::stream::{OpFailed, Stream, StreamOps, StreamState};
use std::any::Any;

const SEEK_SET: i32 = 0;
const SEEK_CUR: i32 = 1;
const SEEK_END: i32 = 2;

/// Which extension opened the blob (they differ in the ops label and in
/// whether seeking clears `fatal_error`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobFlavor {
    /// `ext/sqlite3` (`"SQLite3"`).
    Sqlite3,
    /// `ext/pdo_sqlite` (`"PDOSQLite"`).
    Pdo,
}

/// A stream over an open `SQLite` BLOB.
#[derive(Debug)]
pub struct BlobStream {
    blob: RawBlob,
    position: usize,
    size: usize,
    flags: i64,
    writable: bool,
    flavor: BlobFlavor,
}

impl BlobStream {
    pub(crate) fn new(blob: RawBlob, flags: i64, writable: bool, flavor: BlobFlavor) -> Self {
        let size = blob.bytes();
        BlobStream {
            blob,
            position: 0,
            size,
            flags,
            writable,
            flavor,
        }
    }

    /// The stream mode: `"r+b"` when opened for writing, else `"rb"`.
    pub fn mode(&self) -> &'static str {
        if self.writable {
            "r+b"
        } else {
            "rb"
        }
    }

    /// The BLOB size in bytes (`sqlite3_blob_bytes`).
    pub fn size(&self) -> usize {
        self.size
    }

    /// The current offset.
    pub fn position(&self) -> usize {
        self.position
    }

    /// Wraps the BLOB in a PHP stream (`php_stream_alloc`).
    pub fn into_stream(self) -> Stream {
        let mode = self.mode();
        Stream::new(Box::new(self), mode)
    }

    fn seek_ok(&self, st: &mut StreamState, newpos: &mut i64) -> bool {
        *newpos = i64::try_from(self.position).unwrap_or(i64::MAX);
        st.eof = false;
        if self.flavor == BlobFlavor::Sqlite3 {
            st.fatal_error = false;
        }
        true
    }

    fn seek_fail(&mut self, pos: usize, newpos: &mut i64) -> bool {
        self.position = pos;
        *newpos = -1;
        false
    }
}

impl StreamOps for BlobStream {
    fn label(&self) -> &'static str {
        match self.flavor {
            BlobFlavor::Sqlite3 => "SQLite3",
            BlobFlavor::Pdo => "PDOSQLite",
        }
    }

    fn read(&mut self, st: &mut StreamState, buf: &mut [u8]) -> Result<usize, OpFailed> {
        let mut count = buf.len();
        if self.position + count >= self.size {
            count = self.size.saturating_sub(self.position);
            st.eof = true;
        }
        if count > 0 {
            if self.blob.read(&mut buf[..count], self.position) != SQLITE_OK {
                return Err(OpFailed);
            }
            self.position += count;
        }
        Ok(count)
    }

    fn write(&mut self, st: &mut StreamState, buf: &[u8]) -> Result<usize, OpFailed> {
        if self.flags & SQLITE3_OPEN_READONLY != 0 {
            diag::warning("Can't write to blob stream: is open as read only");
            return Err(OpFailed);
        }
        let count = buf.len();
        if self.position + count > self.size {
            diag::warning("It is not possible to increase the size of a BLOB");
            return Err(OpFailed);
        }
        if self.blob.write(buf, self.position) != SQLITE_OK {
            return Err(OpFailed);
        }
        if self.position + count >= self.size {
            st.eof = true;
            self.position = self.size;
        } else {
            self.position += count;
        }
        Ok(count)
    }

    fn close(&mut self, _st: &mut StreamState, _preserve_handle: bool) -> i32 {
        self.blob.close();
        0
    }

    fn can_seek(&self) -> bool {
        true
    }

    fn seek(&mut self, st: &mut StreamState, offset: i64, whence: i32, newpos: &mut i64) -> bool {
        let size = self.size;
        match whence {
            SEEK_CUR => {
                if offset < 0 {
                    let back = usize::try_from(offset.unsigned_abs()).unwrap_or(usize::MAX);
                    if self.position < back {
                        return self.seek_fail(0, newpos);
                    }
                    self.position -= back;
                } else {
                    let fwd = usize::try_from(offset).unwrap_or(usize::MAX);
                    if self.position.saturating_add(fwd) > size {
                        return self.seek_fail(size, newpos);
                    }
                    self.position += fwd;
                }
                self.seek_ok(st, newpos)
            }
            SEEK_SET => {
                // `(size_t)offset`: negative offsets become huge.
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                let target = offset as u64 as usize;
                if size < target {
                    return self.seek_fail(size, newpos);
                }
                self.position = target;
                self.seek_ok(st, newpos)
            }
            SEEK_END => {
                if offset > 0 {
                    return self.seek_fail(size, newpos);
                }
                let back = usize::try_from(offset.unsigned_abs()).unwrap_or(usize::MAX);
                if size < back {
                    return self.seek_fail(0, newpos);
                }
                self.position = size - back;
                self.seek_ok(st, newpos)
            }
            _ => {
                *newpos = i64::try_from(self.position).unwrap_or(i64::MAX);
                false
            }
        }
    }

    fn stat(&mut self, _st: &mut StreamState) -> Option<StatBuf> {
        Some(StatBuf {
            size: i64::try_from(self.size).unwrap_or(i64::MAX),
            ..StatBuf::default()
        })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

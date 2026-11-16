// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `php_stream_statbuf` and helpers.

use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;

use crate::value::{Key, Value};

/// File type bits (`S_IFMT` and friends).
pub mod mode {
    /// Type mask.
    pub const S_IFMT: i64 = 0o170_000;
    /// Socket.
    pub const S_IFSOCK: i64 = 0o140_000;
    /// Symbolic link.
    pub const S_IFLNK: i64 = 0o120_000;
    /// Regular file.
    pub const S_IFREG: i64 = 0o100_000;
    /// Block device.
    pub const S_IFBLK: i64 = 0o060_000;
    /// Directory.
    pub const S_IFDIR: i64 = 0o040_000;
    /// Character device.
    pub const S_IFCHR: i64 = 0o020_000;
    /// FIFO.
    pub const S_IFIFO: i64 = 0o010_000;
}

/// The stat buffer of a file or stream (`zend_stat_t`), as plain integers.
///
/// [`StatBuf::to_value`] builds the 26-element array returned by `stat()`,
/// `lstat()` and `fstat()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(missing_docs)]
pub struct StatBuf {
    pub dev: i64,
    pub ino: i64,
    pub mode: i64,
    pub nlink: i64,
    pub uid: i64,
    pub gid: i64,
    pub rdev: i64,
    pub size: i64,
    pub atime: i64,
    pub mtime: i64,
    pub ctime: i64,
    pub blksize: i64,
    pub blocks: i64,
}

/// Field names in PHP's order.
pub const STAT_NAMES: [&str; 13] = [
    "dev", "ino", "mode", "nlink", "uid", "gid", "rdev", "size", "atime", "mtime", "ctime",
    "blksize", "blocks",
];

#[allow(clippy::cast_possible_wrap)]
impl StatBuf {
    /// Converts std metadata.
    pub fn from_metadata(m: &Metadata) -> Self {
        StatBuf {
            dev: m.dev() as i64,
            ino: m.ino() as i64,
            mode: i64::from(m.mode()),
            nlink: m.nlink() as i64,
            uid: i64::from(m.uid()),
            gid: i64::from(m.gid()),
            rdev: m.rdev() as i64,
            size: m.size() as i64,
            atime: m.atime(),
            mtime: m.mtime(),
            ctime: m.ctime(),
            blksize: m.blksize() as i64,
            blocks: m.blocks() as i64,
        }
    }

    /// The thirteen values in PHP's order.
    pub fn values(&self) -> [i64; 13] {
        [
            self.dev,
            self.ino,
            self.mode,
            self.nlink,
            self.uid,
            self.gid,
            self.rdev,
            self.size,
            self.atime,
            self.mtime,
            self.ctime,
            self.blksize,
            self.blocks,
        ]
    }

    /// `(key, value)` pairs of the 26-element array: numeric keys 0..12 then
    /// the named keys.
    pub fn entries(&self) -> Vec<(Key, i64)> {
        let vals = self.values();
        let mut out: Vec<(Key, i64)> = (0i64..).zip(vals).map(|(i, v)| (Key::Int(i), v)).collect();
        out.extend(STAT_NAMES.iter().zip(vals).map(|(n, v)| (Key::str(*n), v)));
        out
    }

    /// The `stat()` array as a [`Value`].
    pub fn to_value(&self) -> Value {
        Value::Array(
            self.entries()
                .into_iter()
                .map(|(k, v)| (k, Value::Int(v)))
                .collect(),
        )
    }

    /// Builds a stat buffer from a user-space `stat`/`url_stat` result array
    /// (`statbuf_from_array()` in userspace.c).
    pub fn from_value(v: &Value) -> Self {
        let mut sb = StatBuf::default();
        let get = |name: &str| v.get(name).map(Value::to_long);
        macro_rules! set {
            ($($f:ident),*) => { $( if let Some(x) = get(stringify!($f)) { sb.$f = x; } )* };
        }
        set!(dev, ino, mode, nlink, uid, gid, rdev, size, atime, mtime, ctime, blksize, blocks);
        sb
    }

    /// The file type bits.
    pub fn file_type(&self) -> i64 {
        self.mode & mode::S_IFMT
    }

    /// `S_ISREG`.
    pub fn is_file(&self) -> bool {
        self.file_type() == mode::S_IFREG
    }

    /// `S_ISDIR`.
    pub fn is_dir(&self) -> bool {
        self.file_type() == mode::S_IFDIR
    }

    /// `S_ISLNK`.
    pub fn is_link(&self) -> bool {
        self.file_type() == mode::S_IFLNK
    }

    /// `S_ISFIFO`.
    pub fn is_fifo(&self) -> bool {
        self.file_type() == mode::S_IFIFO
    }

    /// `S_ISCHR`.
    pub fn is_char(&self) -> bool {
        self.file_type() == mode::S_IFCHR
    }

    /// `filetype()` name, or `None` for an unknown type.
    pub fn type_name(&self) -> Option<&'static str> {
        Some(match self.file_type() {
            mode::S_IFLNK => "link",
            mode::S_IFIFO => "fifo",
            mode::S_IFCHR => "char",
            mode::S_IFDIR => "dir",
            mode::S_IFBLK => "block",
            mode::S_IFREG => "file",
            mode::S_IFSOCK => "socket",
            _ => return None,
        })
    }
}

/// `stat()` of a path (follows symlinks).
pub fn stat_path(path: &[u8]) -> std::io::Result<StatBuf> {
    std::fs::metadata(crate::util::os_path(path)).map(|m| StatBuf::from_metadata(&m))
}

/// `lstat()` of a path.
pub fn lstat_path(path: &[u8]) -> std::io::Result<StatBuf> {
    std::fs::symlink_metadata(crate::util::os_path(path)).map(|m| StatBuf::from_metadata(&m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_shape() {
        let sb = StatBuf {
            dev: 1,
            blocks: 13,
            ..StatBuf::default()
        };
        let e = sb.entries();
        assert_eq!(e.len(), 26);
        assert_eq!(e[0], (Key::Int(0), 1));
        assert_eq!(e[12], (Key::Int(12), 13));
        assert_eq!(e[13], (Key::str("dev"), 1));
        assert_eq!(e[25], (Key::str("blocks"), 13));
    }

    #[test]
    fn from_user_array() {
        let v = Value::Array(vec![
            (Key::str("size"), Value::str("42")),
            (Key::str("mode"), Value::Int(0o100_644)),
        ]);
        let sb = StatBuf::from_value(&v);
        assert_eq!(sb.size, 42);
        assert!(sb.is_file());
        assert_eq!(sb.type_name(), Some("file"));
    }

    #[test]
    fn stat_root() {
        let sb = stat_path(b"/").unwrap();
        assert!(sb.is_dir());
        assert!(stat_path(b"/nonexistent-php-streams").is_err());
    }
}

//! What `Phar::interceptFileFuncs()` changes (`func_interceptors.c`).
//!
//! Once enabled, the listed functions resolve *relative* paths against the
//! running phar (the executing file is `phar://...`). The VM calls the
//! helpers below from its implementations of those functions and falls
//! back to the original behaviour on [`None`] / [`InterceptStat::Skip`].

use php_streams::stat::{mode as m, StatBuf};

use crate::archive::Entry;
use crate::consts::ENT_PERM_MASK;
use crate::registry as reg;
use crate::util::{find, fix_filepath};

/// The intercepted functions, in `phar_intercept_functions_init()` order.
pub const INTERCEPTED_FUNCTIONS: &[&str] = &[
    "fopen",
    "file_get_contents",
    "is_file",
    "is_link",
    "is_dir",
    "opendir",
    "file_exists",
    "fileperms",
    "fileinode",
    "filesize",
    "fileowner",
    "filegroup",
    "fileatime",
    "filemtime",
    "filectime",
    "filetype",
    "is_writable",
    "is_readable",
    "is_executable",
    "lstat",
    "stat",
    "readfile",
];

fn is_phar_url(f: &[u8]) -> bool {
    f.len() >= 7 && f[..7].eq_ignore_ascii_case(b"phar://")
}

fn relative(filename: &[u8]) -> bool {
    filename.first() != Some(&b'/') && find(filename, b"://").is_none()
}

/// The fast-path guard shared by most interceptors.
fn active() -> bool {
    reg::intercepted() && reg::any_archive()
}

/// The running archive path for a relative lookup.
fn running_arch(executed: Option<&[u8]>) -> Option<Vec<u8>> {
    let f = executed?;
    if !is_phar_url(f) {
        return None;
    }
    reg::split_fname(f, 2, 0).ok().map(|(a, _)| a)
}

fn phar_url(arch: &[u8], entry: &[u8]) -> Vec<u8> {
    let mut n = b"phar://".to_vec();
    n.extend_from_slice(arch);
    if entry.first() != Some(&b'/') {
        n.push(b'/');
    }
    n.extend_from_slice(entry);
    n
}

/// `phar_opendir`: the `phar://` path to open instead, if any.
pub fn opendir_path(filename: &[u8], executed: Option<&[u8]>) -> Option<Vec<u8>> {
    if !active() || !relative(filename) {
        return None;
    }
    let arch = running_arch(executed)?;
    let entry = fix_filepath(filename, reg::cwd().as_deref());
    Some(phar_url(&arch, &entry))
}

/// `phar_get_name_for_relative_paths()` for `fopen()`,
/// `file_get_contents()` and `readfile()`: the `phar://` path to use
/// instead, if any.
pub fn relative_path(
    filename: &[u8],
    use_include_path: bool,
    executed: Option<&[u8]>,
    include_path: &[u8],
) -> Option<Vec<u8>> {
    if !active() || !(use_include_path || relative(filename)) {
        return None;
    }
    let exe = executed?;
    let arch = running_arch(executed)?;
    let phar = reg::get_archive(&arch, None).ok()??;
    if use_include_path {
        return reg::find_in_include_path(filename, exe, include_path);
    }
    let entry = fix_filepath(filename, reg::cwd().as_deref());
    let a = phar.borrow();
    let present = if entry.first() == Some(&b'/') {
        a.manifest.contains_key(&entry[1..])
    } else {
        a.manifest.contains_key(&entry)
    };
    present.then(|| phar_url(&arch, &entry))
}

/// The `php_stat()` operation being intercepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatKind {
    /// `fileperms()`.
    Perms,
    /// `fileinode()`.
    Inode,
    /// `filesize()`.
    Size,
    /// `fileowner()`.
    Owner,
    /// `filegroup()`.
    Group,
    /// `fileatime()`.
    Atime,
    /// `filemtime()`.
    Mtime,
    /// `filectime()`.
    Ctime,
    /// `filetype()`.
    Type,
    /// `is_writable()`.
    IsWritable,
    /// `is_readable()`.
    IsReadable,
    /// `is_executable()`.
    IsExecutable,
    /// `file_exists()`.
    Exists,
    /// `is_dir()`.
    IsDir,
    /// `lstat()`.
    Lstat,
    /// `stat()`.
    Stat,
}

impl StatKind {
    fn is_exists_check(self) -> bool {
        matches!(
            self,
            StatKind::Exists
                | StatKind::IsWritable
                | StatKind::IsReadable
                | StatKind::IsExecutable
                | StatKind::IsDir
        )
    }

    fn is_link_op(self) -> bool {
        matches!(self, StatKind::Type | StatKind::Lstat)
    }
}

/// The outcome of an intercepted stat-family call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterceptStat {
    /// Not intercepted: call the original function.
    Skip,
    /// Return this boolean directly.
    Bool(bool),
    /// Compute the result from this stat buffer (`phar_fancy_stat`).
    Stat(StatBuf),
    /// Emit this warning and return `false`.
    Warn(String),
}

fn entry_stat(e: &Entry, writeable: bool) -> StatBuf {
    let ts = i64::from(e.timestamp);
    let mut sb = StatBuf {
        mode: i64::from(e.flags & ENT_PERM_MASK),
        mtime: ts,
        atime: ts,
        ctime: ts,
        ..StatBuf::default()
    };
    if e.is_dir {
        sb.mode |= m::S_IFDIR;
        if e.symlink.is_some() {
            sb.mode |= m::S_IFLNK;
        }
    } else {
        sb.size = i64::from(e.uncompressed_filesize);
        sb.mode |= m::S_IFREG;
        if e.symlink.is_some() {
            sb.mode |= m::S_IFLNK;
        }
    }
    finish(&mut sb, writeable);
    sb.ino = i64::from(e.inode);
    sb
}

fn dir_stat(max_ts: u32, writeable: bool) -> StatBuf {
    let ts = i64::from(max_ts);
    let mut sb = StatBuf {
        mode: 0o777 | m::S_IFDIR,
        mtime: ts,
        atime: ts,
        ctime: ts,
        ..StatBuf::default()
    };
    finish(&mut sb, writeable);
    sb
}

fn finish(sb: &mut StatBuf, writeable: bool) {
    if !writeable {
        sb.mode = (sb.mode & 0o555) | (sb.mode & !0o777);
    }
    sb.nlink = 1;
    sb.rdev = -1;
    sb.dev = 0xc;
    sb.blksize = -1;
    sb.blocks = -1;
}

/// `phar_file_stat()`.
pub fn file_stat(filename: &[u8], kind: StatKind, executed: Option<&[u8]>) -> InterceptStat {
    if !reg::intercepted() {
        return InterceptStat::Skip;
    }
    if filename.is_empty() {
        return InterceptStat::Bool(false);
    }
    if !relative(filename) {
        return InterceptStat::Skip;
    }
    let Some(arch) = running_arch(executed) else {
        return InterceptStat::Skip;
    };
    let Ok(Some(phar)) = reg::get_archive(&arch, None) else {
        return InterceptStat::Skip;
    };
    let a = phar.borrow();
    let entry = fix_filepath(filename, reg::cwd().as_deref());
    if entry.first() == Some(&b'/') {
        if let Some(e) = a.manifest.get(&entry[1..]) {
            return InterceptStat::Stat(entry_stat(e, a.is_writeable));
        }
    } else {
        if let Some(e) = a.manifest.get(&entry) {
            return InterceptStat::Stat(entry_stat(e, a.is_writeable));
        }
        if a.virtual_dirs.contains_key(&entry) {
            if kind.is_exists_check() {
                return InterceptStat::Bool(true);
            }
            return InterceptStat::Stat(dir_stat(a.max_timestamp, a.is_writeable));
        }
    }
    // not found relative to the cwd: retry from the archive root
    let entry = fix_filepath(filename, None);
    let inner = entry.get(1..).unwrap_or_default();
    if let Some(e) = a.manifest.get(inner) {
        if kind.is_exists_check() {
            return InterceptStat::Bool(true);
        }
        return InterceptStat::Stat(entry_stat(e, a.is_writeable));
    }
    if a.virtual_dirs.contains_key(inner) {
        if kind.is_exists_check() {
            return InterceptStat::Bool(true);
        }
        return InterceptStat::Stat(dir_stat(a.max_timestamp, a.is_writeable));
    }
    if kind.is_exists_check() {
        return InterceptStat::Bool(false);
    }
    InterceptStat::Warn(format!(
        "{}stat failed for {}",
        if kind.is_link_op() { "L" } else { "" },
        String::from_utf8_lossy(filename)
    ))
}

fn lookup_flag(
    filename: &[u8],
    executed: Option<&[u8]>,
    f: impl Fn(&Entry) -> bool,
) -> Option<bool> {
    if !active() || !relative(filename) {
        return None;
    }
    let arch = running_arch(executed)?;
    let Ok(Some(phar)) = reg::get_archive(&arch, None) else {
        return Some(false);
    };
    let a = phar.borrow();
    let entry = fix_filepath(filename, reg::cwd().as_deref());
    let e = if entry.first() == Some(&b'/') {
        a.manifest.get(&entry[1..])
    } else {
        a.manifest.get(&entry)
    };
    Some(e.is_some_and(f))
}

/// `phar_is_file`: `Some(result)` when intercepted.
pub fn is_file(filename: &[u8], executed: Option<&[u8]>) -> Option<bool> {
    lookup_flag(filename, executed, |e| !e.is_dir)
}

/// `phar_is_link`: `Some(result)` when intercepted.
pub fn is_link(filename: &[u8], executed: Option<&[u8]>) -> Option<bool> {
    lookup_flag(filename, executed, |e| e.symlink.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_intercepted_by_default() {
        assert_eq!(
            file_stat(b"a.php", StatKind::Stat, Some(b"phar:///x.phar/i.php")),
            InterceptStat::Skip
        );
        assert_eq!(is_file(b"a.php", Some(b"phar:///x.phar/i.php")), None);
        assert_eq!(relative_path(b"a", false, None, b""), None);
        assert_eq!(INTERCEPTED_FUNCTIONS.len(), 22);
    }
}

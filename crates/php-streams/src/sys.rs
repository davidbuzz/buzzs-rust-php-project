//! Thin wrappers over the few POSIX calls std does not expose.
//!
//! All `unsafe` code of the crate lives in this module. Each block carries a
//! `SAFETY:` comment; the wrappers themselves are safe to call.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Returns `strerror(errno)` (the text PHP prints after "Failed to open
/// stream: ").
pub fn strerror(errno: i32) -> String {
    let mut buf = [0 as libc::c_char; 256];
    // SAFETY: `buf` is a valid writable buffer of the given length; the XSI
    // strerror_r always NUL-terminates on success.
    let rc = unsafe { libc::strerror_r(errno, buf.as_mut_ptr(), buf.len()) };
    if rc != 0 {
        return format!("Unknown error {errno}");
    }
    // SAFETY: strerror_r succeeded, so `buf` holds a NUL-terminated string.
    let s = unsafe { CStr::from_ptr(buf.as_ptr()) };
    s.to_string_lossy().into_owned()
}

/// The errno of an `io::Error` (EIO when unknown).
pub fn errno_of(err: &io::Error) -> i32 {
    err.raw_os_error().unwrap_or(libc::EIO)
}

/// `strerror()` of an `io::Error`.
pub fn error_text(err: &io::Error) -> String {
    strerror(errno_of(err))
}

/// The thread's current `errno`.
pub fn last_errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn cpath(path: &[u8]) -> io::Result<CString> {
    CString::new(path).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

/// Takes ownership of a raw file descriptor.
///
/// Used for the CLI standard streams (`php://stdin` on first open uses fd 0
/// itself, so closing the stream closes fd 0 exactly like PHP).
pub fn file_from_fd(fd: RawFd) -> File {
    // SAFETY: callers pass descriptors that are open and that the resulting
    // File is allowed to own (freshly dup()ed or the process' standard fds
    // which PHP likewise closes on fclose()).
    unsafe { File::from_raw_fd(fd) }
}

/// `dup(fd)`.
pub fn dup(fd: RawFd) -> io::Result<RawFd> {
    // SAFETY: dup has no memory-safety preconditions.
    let r = unsafe { libc::dup(fd) };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

/// `getdtablesize()`.
pub fn dtablesize() -> i64 {
    // SAFETY: getdtablesize has no preconditions.
    i64::from(unsafe { libc::getdtablesize() })
}

/// `flock(fd, op)`.
pub fn flock(fd: RawFd, op: i32) -> io::Result<()> {
    // SAFETY: flock has no memory-safety preconditions.
    let r = unsafe { libc::flock(fd, op) };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `flock` operation constants.
pub const LOCK_SH: i32 = libc::LOCK_SH;
/// Exclusive lock.
pub const LOCK_EX: i32 = libc::LOCK_EX;
/// Non-blocking.
pub const LOCK_NB: i32 = libc::LOCK_NB;
/// Unlock.
pub const LOCK_UN: i32 = libc::LOCK_UN;
/// `EWOULDBLOCK`.
pub const EWOULDBLOCK: i32 = libc::EWOULDBLOCK;

/// `umask(mask)`, returning the previous mask.
pub fn umask(mask: u32) -> u32 {
    // SAFETY: umask has no preconditions.
    unsafe { libc::umask(mask as libc::mode_t) }
}

/// Reads the current umask without changing it.
pub fn current_umask() -> u32 {
    let old = umask(0o022);
    umask(old);
    old
}

/// `isatty(fd)`.
pub fn isatty(fd: RawFd) -> bool {
    // SAFETY: isatty has no memory-safety preconditions.
    unsafe { libc::isatty(fd) == 1 }
}

/// Returns whether `O_NONBLOCK` is clear (i.e. the fd blocks).
pub fn is_blocking(fd: RawFd) -> io::Result<bool> {
    // SAFETY: F_GETFL takes no pointer argument.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(flags & libc::O_NONBLOCK == 0)
}

/// Sets or clears `O_NONBLOCK`; returns the previous blocking state.
pub fn set_blocking(fd: RawFd, blocking: bool) -> io::Result<bool> {
    // SAFETY: F_GETFL takes no pointer argument.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let old = flags & libc::O_NONBLOCK == 0;
    let new = if blocking {
        flags & !libc::O_NONBLOCK
    } else {
        flags | libc::O_NONBLOCK
    };
    // SAFETY: F_SETFL takes an integer argument.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, new) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(old)
}

/// `access(path, mode)`.
pub fn access(path: &[u8], mode: i32) -> io::Result<()> {
    let c = cpath(path)?;
    // SAFETY: `c` is a valid NUL-terminated string.
    if unsafe { libc::access(c.as_ptr(), mode) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `access()` mode bits.
pub const R_OK: i32 = libc::R_OK;
/// Write permission.
pub const W_OK: i32 = libc::W_OK;
/// Execute permission.
pub const X_OK: i32 = libc::X_OK;
/// Existence.
pub const F_OK: i32 = libc::F_OK;

/// `utime(path, {atime, mtime})`; `None` sets both to now.
pub fn utime(path: &[u8], times: Option<(i64, i64)>) -> io::Result<()> {
    let c = cpath(path)?;
    let r = match times {
        Some((atime, mtime)) => {
            let buf = libc::utimbuf {
                actime: atime as libc::time_t,
                modtime: mtime as libc::time_t,
            };
            // SAFETY: valid C string and a valid utimbuf pointer.
            unsafe { libc::utime(c.as_ptr(), &raw const buf) }
        }
        // SAFETY: valid C string; a NULL times pointer means "now".
        None => unsafe { libc::utime(c.as_ptr(), std::ptr::null()) },
    };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `statvfs` results used by `disk_free_space()` / `disk_total_space()`.
#[allow(clippy::cast_precision_loss)]
pub fn disk_space(path: &[u8]) -> io::Result<(f64, f64)> {
    let c = cpath(path)?;
    let mut buf = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: valid C string and a pointer to writable statvfs storage.
    let r = unsafe { libc::statvfs(c.as_ptr(), buf.as_mut_ptr()) };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statvfs succeeded and initialised the struct.
    let s = unsafe { buf.assume_init() };
    let frsize = s.f_frsize as f64;
    let free = frsize * s.f_bavail as f64;
    let total = frsize * s.f_blocks as f64;
    Ok((free, total))
}

/// Looks up a user id by name (`getpwnam_r`).
pub fn uid_by_name(name: &[u8]) -> Option<u32> {
    let c = CString::new(name).ok()?;
    let mut pwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut buf = vec![0 as libc::c_char; 16384];
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: all pointers reference live, correctly sized storage.
    let r = unsafe {
        libc::getpwnam_r(
            c.as_ptr(),
            pwd.as_mut_ptr(),
            buf.as_mut_ptr(),
            buf.len(),
            &raw mut result,
        )
    };
    if r != 0 || result.is_null() {
        return None;
    }
    // SAFETY: getpwnam_r succeeded and filled `pwd`.
    Some(unsafe { pwd.assume_init() }.pw_uid)
}

/// Looks up a group id by name (`getgrnam_r`).
pub fn gid_by_name(name: &[u8]) -> Option<u32> {
    let c = CString::new(name).ok()?;
    let mut grp = std::mem::MaybeUninit::<libc::group>::uninit();
    let mut buf = vec![0 as libc::c_char; 16384];
    let mut result: *mut libc::group = std::ptr::null_mut();
    // SAFETY: all pointers reference live, correctly sized storage.
    let r = unsafe {
        libc::getgrnam_r(
            c.as_ptr(),
            grp.as_mut_ptr(),
            buf.as_mut_ptr(),
            buf.len(),
            &raw mut result,
        )
    };
    if r != 0 || result.is_null() {
        return None;
    }
    // SAFETY: getgrnam_r succeeded and filled `grp`.
    Some(unsafe { grp.assume_init() }.gr_gid)
}

/// `chown`/`lchown` with `-1` meaning "unchanged".
pub fn chown(path: &[u8], uid: Option<u32>, gid: Option<u32>, follow: bool) -> io::Result<()> {
    let p = Path::new(std::ffi::OsStr::from_bytes(path));
    if follow {
        std::os::unix::fs::chown(p, uid, gid)
    } else {
        std::os::unix::fs::lchown(p, uid, gid)
    }
}

/// `mkstemp(template)`; returns the open file and the generated path.
pub fn mkstemp(template: &[u8]) -> io::Result<(File, Vec<u8>)> {
    let mut buf = template.to_vec();
    if buf.contains(&0) {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    buf.push(0);
    // SAFETY: `buf` is a writable NUL-terminated buffer ending in XXXXXX.
    let fd = unsafe { libc::mkstemp(buf.as_mut_ptr().cast::<libc::c_char>()) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    buf.pop();
    Ok((file_from_fd(fd), buf))
}

/// A directory handle iterating raw `readdir()` entries (including `.` and
/// `..`, in the kernel's order).
#[derive(Debug)]
pub struct DirHandle {
    dir: std::ptr::NonNull<libc::DIR>,
}

impl DirHandle {
    /// `opendir(path)`.
    pub fn open(path: &[u8]) -> io::Result<Self> {
        let c = cpath(path)?;
        // SAFETY: `c` is a valid C string.
        let dir = unsafe { libc::opendir(c.as_ptr()) };
        std::ptr::NonNull::new(dir)
            .map(|dir| DirHandle { dir })
            .ok_or_else(io::Error::last_os_error)
    }

    /// `readdir()`: the next entry name, or `None` at the end.
    pub fn read(&mut self) -> Option<Vec<u8>> {
        // SAFETY: `self.dir` is a live DIR* owned by this handle.
        let ent = unsafe { libc::readdir(self.dir.as_ptr()) };
        if ent.is_null() {
            return None;
        }
        // SAFETY: readdir returned a valid dirent whose d_name is
        // NUL-terminated; we copy it out before the next readdir call.
        let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) };
        Some(name.to_bytes().to_vec())
    }

    /// `rewinddir()`.
    pub fn rewind(&mut self) {
        // SAFETY: `self.dir` is a live DIR*.
        unsafe { libc::rewinddir(self.dir.as_ptr()) }
    }
}

impl Drop for DirHandle {
    fn drop(&mut self) {
        // SAFETY: `self.dir` is a live DIR* that is closed exactly once.
        unsafe {
            libc::closedir(self.dir.as_ptr());
        }
    }
}

/// `getcwd()` as bytes.
pub fn getcwd() -> Option<Vec<u8>> {
    std::env::current_dir()
        .ok()
        .map(|p| p.as_os_str().as_bytes().to_vec())
}

/// Linux `major()` of a device number.
pub fn dev_major(dev: u64) -> u64 {
    ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff)
}

/// Linux `minor()` of a device number.
pub fn dev_minor(dev: u64) -> u64 {
    (dev & 0xff) | ((dev >> 12) & !0xff)
}

/// `getuid()`.
pub fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// `getgid()`.
pub fn gid() -> u32 {
    // SAFETY: getgid has no preconditions and cannot fail.
    unsafe { libc::getgid() }
}

/// Whether `gid` is the process' group or one of its supplementary groups.
pub fn in_group(gid_to_check: i64) -> bool {
    if i64::from(gid()) == gid_to_check {
        return true;
    }
    // SAFETY: a zero-length query with a null buffer returns the count.
    let n = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    let Ok(len) = usize::try_from(n) else {
        return false;
    };
    if len == 0 {
        return false;
    }
    let mut groups = vec![0 as libc::gid_t; len];
    // SAFETY: `groups` has room for `n` entries.
    let got = unsafe { libc::getgroups(n, groups.as_mut_ptr()) };
    let got = usize::try_from(got).unwrap_or(0);
    groups[..got.min(len)]
        .iter()
        .any(|&g| i64::from(g) == gid_to_check)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strerror_texts() {
        assert_eq!(strerror(libc::ENOENT), "No such file or directory");
        assert_eq!(strerror(libc::EEXIST), "File exists");
        assert_eq!(strerror(libc::EACCES), "Permission denied");
    }

    #[test]
    fn umask_roundtrip() {
        let m = current_umask();
        assert_eq!(current_umask(), m);
    }

    #[test]
    fn readdir_includes_dots() {
        let mut d = DirHandle::open(b"/").unwrap();
        let mut names = Vec::new();
        while let Some(n) = d.read() {
            names.push(n);
        }
        assert!(names.iter().any(|n| n == b"."));
        assert!(names.iter().any(|n| n == b".."));
        d.rewind();
        assert!(d.read().is_some());
    }

    #[test]
    fn device_numbers() {
        // /dev/null is 1:3 on Linux.
        assert_eq!(dev_major(0x103), 1);
        assert_eq!(dev_minor(0x103), 3);
    }
}

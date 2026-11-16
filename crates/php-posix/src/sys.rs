// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Thin, safe wrappers over the libc calls used by `ext/posix`.
//!
//! This is the only module of the crate that contains `unsafe` code.  Every
//! wrapper converts C pointers/buffers into owned Rust data before returning
//! and reports failures as the raw `errno` value (`Err(errno)`).

#![allow(unsafe_code)]

use std::ffi::{CStr, CString};
use std::sync::Mutex;

/// Serialises calls to the non-reentrant libc functions we must use
/// (`getlogin`, `ctermid(NULL)` style statics) so concurrent Rust threads do
/// not clobber each other's static buffers.
static NON_REENTRANT: Mutex<()> = Mutex::new(());

/// Reads the calling thread's `errno`.
pub fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Sets the calling thread's `errno`.
pub fn set_errno(value: i32) {
    // SAFETY: `__errno_location` always returns a valid pointer to the
    // calling thread's errno slot.
    unsafe {
        *libc::__errno_location() = value;
    }
}

fn check(ret: libc::c_int) -> Result<(), i32> {
    if ret < 0 {
        Err(errno())
    } else {
        Ok(())
    }
}

/// Copies a NUL-terminated C string into an owned byte vector.
///
/// # Safety
/// `ptr` must be null or point to a valid NUL-terminated string.
unsafe fn cstr_bytes(ptr: *const libc::c_char) -> Option<Vec<u8>> {
    if ptr.is_null() {
        None
    } else {
        // SAFETY: guaranteed by the caller.
        Some(unsafe { CStr::from_ptr(ptr) }.to_bytes().to_vec())
    }
}

/// Copies a fixed-size, NUL-terminated `c_char` array (as found in
/// `struct utsname`) into a byte vector.
fn array_bytes(arr: &[libc::c_char]) -> Vec<u8> {
    #[allow(clippy::cast_sign_loss)]
    arr.iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect()
}

pub fn kill(pid: libc::pid_t, sig: libc::c_int) -> Result<(), i32> {
    // SAFETY: kill has no memory-safety preconditions.
    check(unsafe { libc::kill(pid, sig) })
}

pub fn getpid() -> i64 {
    // SAFETY: no preconditions.
    i64::from(unsafe { libc::getpid() })
}

pub fn getppid() -> i64 {
    // SAFETY: no preconditions.
    i64::from(unsafe { libc::getppid() })
}

pub fn getuid() -> i64 {
    // SAFETY: no preconditions.
    i64::from(unsafe { libc::getuid() })
}

pub fn geteuid() -> i64 {
    // SAFETY: no preconditions.
    i64::from(unsafe { libc::geteuid() })
}

pub fn getgid() -> i64 {
    // SAFETY: no preconditions.
    i64::from(unsafe { libc::getgid() })
}

pub fn getegid() -> i64 {
    // SAFETY: no preconditions.
    i64::from(unsafe { libc::getegid() })
}

pub fn setuid(uid: libc::uid_t) -> Result<(), i32> {
    // SAFETY: no memory-safety preconditions.
    check(unsafe { libc::setuid(uid) })
}

pub fn seteuid(uid: libc::uid_t) -> Result<(), i32> {
    // SAFETY: no memory-safety preconditions.
    check(unsafe { libc::seteuid(uid) })
}

pub fn setgid(gid: libc::gid_t) -> Result<(), i32> {
    // SAFETY: no memory-safety preconditions.
    check(unsafe { libc::setgid(gid) })
}

pub fn setegid(gid: libc::gid_t) -> Result<(), i32> {
    // SAFETY: no memory-safety preconditions.
    check(unsafe { libc::setegid(gid) })
}

pub fn getgroups() -> Result<Vec<i64>, i32> {
    // SAFETY: a zero-sized query with a null list is explicitly allowed.
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if count < 0 {
        return Err(errno());
    }
    let mut list: Vec<libc::gid_t> = vec![0; usize::try_from(count).unwrap_or(0)];
    // SAFETY: `list` has room for exactly `count` gids.
    let got = unsafe { libc::getgroups(count, list.as_mut_ptr()) };
    if got < 0 {
        return Err(errno());
    }
    list.truncate(usize::try_from(got).unwrap_or(0));
    Ok(list.into_iter().map(i64::from).collect())
}

pub fn getlogin() -> Result<Vec<u8>, i32> {
    let _guard = NON_REENTRANT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: getlogin returns null or a pointer to a static NUL-terminated
    // buffer that stays valid while we hold the lock and copy it.
    let p = unsafe { libc::getlogin() };
    // SAFETY: `p` is null or a valid C string (see above).
    unsafe { cstr_bytes(p) }.ok_or_else(errno)
}

pub fn getpgrp() -> i64 {
    // SAFETY: no preconditions.
    i64::from(unsafe { libc::getpgrp() })
}

pub fn setsid() -> i64 {
    // SAFETY: no memory-safety preconditions.
    i64::from(unsafe { libc::setsid() })
}

pub fn setpgid(pid: libc::pid_t, group: libc::pid_t) -> Result<(), i32> {
    // SAFETY: no memory-safety preconditions.
    check(unsafe { libc::setpgid(pid, group) })
}

pub fn getpgid(pid: libc::pid_t) -> Result<i64, i32> {
    // SAFETY: no memory-safety preconditions.
    let r = unsafe { libc::getpgid(pid) };
    if r < 0 {
        Err(errno())
    } else {
        Ok(i64::from(r))
    }
}

pub fn getsid(pid: libc::pid_t) -> Result<i64, i32> {
    // SAFETY: no memory-safety preconditions.
    let r = unsafe { libc::getsid(pid) };
    if r < 0 {
        Err(errno())
    } else {
        Ok(i64::from(r))
    }
}

/// Raw `struct utsname` contents.
pub struct RawUname {
    pub sysname: Vec<u8>,
    pub nodename: Vec<u8>,
    pub release: Vec<u8>,
    pub version: Vec<u8>,
    pub machine: Vec<u8>,
    pub domainname: Vec<u8>,
}

pub fn uname() -> Result<RawUname, i32> {
    // SAFETY: utsname is plain old data; all-zero is a valid value.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: `u` is a valid, writable utsname.
    check(unsafe { libc::uname(&raw mut u) })?;
    Ok(RawUname {
        sysname: array_bytes(&u.sysname),
        nodename: array_bytes(&u.nodename),
        release: array_bytes(&u.release),
        version: array_bytes(&u.version),
        machine: array_bytes(&u.machine),
        domainname: array_bytes(&u.domainname),
    })
}

/// Raw `times()` result: `(ticks, utime, stime, cutime, cstime)`.
pub fn times() -> Result<[i64; 5], i32> {
    // SAFETY: tms is plain old data.
    let mut t: libc::tms = unsafe { std::mem::zeroed() };
    // SAFETY: `t` is a valid, writable tms.
    let ticks = unsafe { libc::times(&raw mut t) };
    if ticks == -1 {
        return Err(errno());
    }
    Ok([ticks, t.tms_utime, t.tms_stime, t.tms_cutime, t.tms_cstime])
}

pub fn ctermid() -> Result<Vec<u8>, i32> {
    // L_ctermid is 9 on glibc; use a generous buffer.
    let mut buf = [0 as libc::c_char; 256];
    // SAFETY: `buf` is larger than L_ctermid, as ctermid requires.
    let p = unsafe { libc::ctermid(buf.as_mut_ptr()) };
    if p.is_null() {
        return Err(errno());
    }
    Ok(array_bytes(&buf))
}

pub fn ttyname(fd: libc::c_int) -> Result<Vec<u8>, i32> {
    let mut buf: Vec<libc::c_char> = vec![0; 64];
    loop {
        // SAFETY: `buf` is writable for `buf.len()` bytes.
        let err = unsafe { libc::ttyname_r(fd, buf.as_mut_ptr(), buf.len()) };
        match err {
            0 => return Ok(array_bytes(&buf)),
            libc::ERANGE => {
                let n = buf.len() * 2;
                buf.resize(n, 0);
            }
            e => return Err(e),
        }
    }
}

pub fn isatty(fd: libc::c_int) -> Result<(), i32> {
    // SAFETY: isatty has no memory-safety preconditions.
    if unsafe { libc::isatty(fd) } == 1 {
        Ok(())
    } else {
        Err(errno())
    }
}

pub fn mkfifo(path: &CStr, mode: libc::mode_t) -> Result<(), i32> {
    // SAFETY: `path` is a valid C string.
    check(unsafe { libc::mkfifo(path.as_ptr(), mode) })
}

pub fn mknod(path: &CStr, mode: libc::mode_t, dev: libc::dev_t) -> Result<(), i32> {
    // SAFETY: `path` is a valid C string.
    check(unsafe { libc::mknod(path.as_ptr(), mode, dev) })
}

pub fn makedev(major: libc::c_uint, minor: libc::c_uint) -> libc::dev_t {
    libc::makedev(major, minor)
}

pub fn access(path: &CStr, mode: libc::c_int) -> Result<(), i32> {
    // SAFETY: `path` is a valid C string.
    if unsafe { libc::access(path.as_ptr(), mode) } == 0 {
        Ok(())
    } else {
        Err(errno())
    }
}

pub fn eaccess(path: &CStr, mode: libc::c_int) -> Result<(), i32> {
    // SAFETY: `path` is a valid C string.
    if unsafe { libc::eaccess(path.as_ptr(), mode) } == 0 {
        Ok(())
    } else {
        Err(errno())
    }
}

/// Raw group database entry.
pub struct RawGroup {
    pub name: Option<Vec<u8>>,
    pub passwd: Option<Vec<u8>>,
    pub members: Vec<Vec<u8>>,
    pub gid: i64,
}

/// Raw password database entry.
pub struct RawPasswd {
    pub name: Option<Vec<u8>>,
    pub passwd: Option<Vec<u8>>,
    pub uid: i64,
    pub gid: i64,
    pub gecos: Option<Vec<u8>>,
    pub dir: Option<Vec<u8>>,
    pub shell: Option<Vec<u8>>,
}

/// Converts a filled `struct group` into owned data.
///
/// # Safety
/// All pointers inside `g` must be valid (as returned by `getgr*_r`).
unsafe fn group_from(g: &libc::group) -> RawGroup {
    let mut members = Vec::new();
    if !g.gr_mem.is_null() {
        let mut i = 0;
        loop {
            // SAFETY: gr_mem is a null-terminated array of C strings; we stop
            // at the terminating null. read_unaligned guards against the
            // misalignment PHP's source mentions for macOS.
            let p = unsafe { g.gr_mem.add(i).read_unaligned() };
            if p.is_null() {
                break;
            }
            // SAFETY: entries of gr_mem are valid C strings.
            members.push(unsafe { CStr::from_ptr(p) }.to_bytes().to_vec());
            i += 1;
        }
    }
    RawGroup {
        // SAFETY: valid or null C strings per the caller's contract.
        name: unsafe { cstr_bytes(g.gr_name) },
        // SAFETY: as above.
        passwd: unsafe { cstr_bytes(g.gr_passwd) },
        members,
        gid: i64::from(g.gr_gid),
    }
}

/// Converts a filled `struct passwd` into owned data.
///
/// # Safety
/// All pointers inside `pw` must be valid (as returned by `getpw*_r`).
unsafe fn passwd_from(pw: &libc::passwd) -> RawPasswd {
    // SAFETY: (all fields) valid or null C strings per the caller's contract.
    unsafe {
        RawPasswd {
            name: cstr_bytes(pw.pw_name),
            passwd: cstr_bytes(pw.pw_passwd),
            uid: i64::from(pw.pw_uid),
            gid: i64::from(pw.pw_gid),
            gecos: cstr_bytes(pw.pw_gecos),
            dir: cstr_bytes(pw.pw_dir),
            shell: cstr_bytes(pw.pw_shell),
        }
    }
}

/// Drives one of the `get*_r` functions with the ERANGE retry loop PHP uses.
///
/// When the entry is not found (`err == 0`, result null) the `errno` left
/// behind by the lookup is reported, which is what PHP's non-ZTS build
/// (using the non-reentrant variants) stores in `last_error`.
fn lookup_r<T, R>(
    mut call: impl FnMut(*mut T, *mut libc::c_char, usize, *mut *mut T) -> libc::c_int,
    convert: impl Fn(&T) -> R,
) -> Result<R, i32> {
    let mut buflen = 1024usize;
    loop {
        let mut buf: Vec<libc::c_char> = vec![0; buflen];
        // SAFETY: the C struct is plain old data; zeroed is valid.
        let mut entry: T = unsafe { std::mem::zeroed() };
        let mut result: *mut T = std::ptr::null_mut();
        set_errno(0);
        let err = call(&raw mut entry, buf.as_mut_ptr(), buflen, &raw mut result);
        if err == libc::ERANGE {
            buflen *= 2;
            continue;
        }
        if err != 0 {
            return Err(err);
        }
        if result.is_null() {
            return Err(errno());
        }
        // `entry` (and the strings it points into `buf`) are alive here.
        return Ok(convert(&entry));
    }
}

pub fn getgrnam(name: &CStr) -> Result<RawGroup, i32> {
    lookup_r(
        // SAFETY: all pointers are valid for the duration of the call and
        // `buflen` matches the buffer's size.
        |g, buf, len, res| unsafe { libc::getgrnam_r(name.as_ptr(), g, buf, len, res) },
        // SAFETY: the entry was filled by getgrnam_r.
        |g| unsafe { group_from(g) },
    )
}

pub fn getgrgid(gid: libc::gid_t) -> Result<RawGroup, i32> {
    lookup_r(
        // SAFETY: see getgrnam.
        |g, buf, len, res| unsafe { libc::getgrgid_r(gid, g, buf, len, res) },
        // SAFETY: the entry was filled by getgrgid_r.
        |g| unsafe { group_from(g) },
    )
}

pub fn getpwnam(name: &CStr) -> Result<RawPasswd, i32> {
    lookup_r(
        // SAFETY: see getgrnam.
        |p, buf, len, res| unsafe { libc::getpwnam_r(name.as_ptr(), p, buf, len, res) },
        // SAFETY: the entry was filled by getpwnam_r.
        |p| unsafe { passwd_from(p) },
    )
}

pub fn getpwuid(uid: libc::uid_t) -> Result<RawPasswd, i32> {
    lookup_r(
        // SAFETY: see getgrnam.
        |p, buf, len, res| unsafe { libc::getpwuid_r(uid, p, buf, len, res) },
        // SAFETY: the entry was filled by getpwuid_r.
        |p| unsafe { passwd_from(p) },
    )
}

/// `getrlimit`: returns `(rlim_cur, rlim_max)`.
pub fn getrlimit(resource: libc::c_int) -> Result<(u64, u64), i32> {
    let mut rl = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // glibc declares the resource parameter as an enum on some targets;
    // the libc crate exposes it as `__rlimit_resource_t`.
    #[allow(clippy::cast_sign_loss, clippy::useless_conversion)]
    let res = resource as libc::__rlimit_resource_t;
    // SAFETY: `rl` is a valid, writable rlimit.
    check(unsafe { libc::getrlimit(res, &raw mut rl) })?;
    Ok((rl.rlim_cur, rl.rlim_max))
}

pub fn setrlimit(resource: libc::c_int, cur: u64, max: u64) -> Result<(), i32> {
    let rl = libc::rlimit {
        rlim_cur: cur,
        rlim_max: max,
    };
    #[allow(clippy::cast_sign_loss, clippy::useless_conversion)]
    let res = resource as libc::__rlimit_resource_t;
    // SAFETY: `rl` is a valid rlimit.
    check(unsafe { libc::setrlimit(res, &raw const rl) })
}

/// `strerror()` for any value, including unknown ones
/// (`"Unknown error N"`).
pub fn strerror(code: libc::c_int) -> Vec<u8> {
    let mut buf = [0 as libc::c_char; 1024];
    // SAFETY: `buf` is writable for its whole length; the XSI strerror_r
    // always NUL-terminates what it writes (including the "Unknown error"
    // text it produces alongside EINVAL).
    unsafe { libc::strerror_r(code, buf.as_mut_ptr(), buf.len()) };
    array_bytes(&buf)
}

pub fn initgroups(name: &CStr, gid: libc::gid_t) -> Result<(), i32> {
    // SAFETY: `name` is a valid C string.
    check(unsafe { libc::initgroups(name.as_ptr(), gid) })
}

pub fn sysconf(name: libc::c_int) -> i64 {
    // SAFETY: no memory-safety preconditions.
    unsafe { libc::sysconf(name) }
}

/// `pathconf`; returns the raw value and the errno observed after the call
/// (errno is cleared beforehand so that "no limit" can be told apart).
pub fn pathconf(path: &CStr, name: libc::c_int) -> (i64, i32) {
    set_errno(0);
    // SAFETY: `path` is a valid C string.
    let r = unsafe { libc::pathconf(path.as_ptr(), name) };
    (r, errno())
}

/// `fpathconf`; see [`pathconf`].
pub fn fpathconf(fd: libc::c_int, name: libc::c_int) -> (i64, i32) {
    set_errno(0);
    // SAFETY: fpathconf has no memory-safety preconditions.
    let r = unsafe { libc::fpathconf(fd, name) };
    (r, errno())
}

/// Builds a C string, returning `None` when `bytes` has an interior NUL.
pub fn cstring(bytes: &[u8]) -> Option<CString> {
    CString::new(bytes).ok()
}

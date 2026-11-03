//! Thin safe wrappers over the libc calls this crate needs.
//!
//! This is the only module of the crate that contains `unsafe` code. Every
//! block documents why it is sound; the wrappers expose plain Rust types
//! (`OwnedFd`, `Vec<u8>`, `Result<_, i32>` with an errno) to the rest of
//! the crate.

use std::ffi::{CStr, CString};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::ptr;

/// The calling thread's `errno`.
#[must_use]
pub fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// `strerror(errnum)` as PHP prints it (without Rust's "(os error N)").
#[must_use]
pub fn strerror(errnum: i32) -> String {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is a valid writable buffer of the given length;
    // `strerror_r` (XSI) writes a NUL-terminated message into it.
    let r = unsafe { libc::strerror_r(errnum, buf.as_mut_ptr().cast(), buf.len()) };
    if r != 0 {
        return format!("Unknown error {errnum}");
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// Builds a C string, cutting at the first NUL like C would.
#[must_use]
pub fn cstring(bytes: &[u8]) -> CString {
    let end = bytes.iter().position(|&c| c == 0).unwrap_or(bytes.len());
    // No interior NUL remains after the cut.
    CString::new(&bytes[..end]).unwrap_or_default()
}

/// Copies a possibly-NULL C string.
fn from_cstr(p: *const libc::c_char) -> Option<Vec<u8>> {
    if p.is_null() {
        return None;
    }
    // SAFETY: the caller passes a pointer returned by libc that points to
    // a NUL-terminated string valid until the next call of that function;
    // we copy it immediately.
    Some(unsafe { CStr::from_ptr(p) }.to_bytes().to_vec())
}

// ------------------------------------------------------------- multibyte

extern "C" {
    fn mbrlen(s: *const libc::c_char, n: libc::size_t, ps: *mut libc::mbstate_t) -> libc::size_t;
}

/// `mbrlen()` in the current `LC_CTYPE` with a fresh shift state: the
/// length of the character at the start of `s` (0 for NUL, -1 invalid,
/// -2 incomplete).
#[must_use]
pub fn mblen(s: &[u8]) -> isize {
    if s.is_empty() {
        return 0;
    }
    // SAFETY: an all-zero mbstate_t is the initial conversion state.
    let mut st: libc::mbstate_t = unsafe { std::mem::zeroed() };
    // SAFETY: `s` is valid for `s.len()` bytes and `st` is a valid state.
    let r = unsafe { mbrlen(s.as_ptr().cast(), s.len(), &raw mut st) };
    #[allow(clippy::cast_possible_wrap)]
    let r = r as isize;
    r
}

/// `sysconf(_SC_ARG_MAX)`, or -1.
#[must_use]
pub fn arg_max() -> i64 {
    // SAFETY: sysconf has no memory-safety preconditions.
    unsafe { libc::sysconf(libc::_SC_ARG_MAX) }
}

// ------------------------------------------------------------- fds

/// Wraps a freshly created descriptor.
fn owned(fd: RawFd) -> OwnedFd {
    // SAFETY: only called with descriptors this module just created and
    // that nothing else owns.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

/// `pipe()`: `(read end, write end)`, both close-on-exec.
pub fn pipe() -> Result<(OwnedFd, OwnedFd), i32> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: `fds` has room for the two descriptors.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(errno());
    }
    Ok((owned(fds[0]), owned(fds[1])))
}

/// `socketpair(AF_UNIX, SOCK_STREAM)`, both close-on-exec.
pub fn socketpair() -> Result<(OwnedFd, OwnedFd), i32> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: `fds` has room for the two descriptors.
    let r = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    if r != 0 {
        return Err(errno());
    }
    Ok((owned(fds[0]), owned(fds[1])))
}

/// `openpty()`: `(master, slave)`, both close-on-exec.
pub fn openpty() -> Result<(OwnedFd, OwnedFd), i32> {
    let (mut master, mut slave) = (0 as RawFd, 0 as RawFd);
    // SAFETY: the out-pointers are valid; name/termios/winsize may be NULL.
    let r = unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
        )
    };
    if r != 0 {
        return Err(errno());
    }
    let (m, s) = (owned(master), owned(slave));
    set_cloexec(m.as_raw_fd());
    set_cloexec(s.as_raw_fd());
    Ok((m, s))
}

fn set_cloexec(fd: RawFd) {
    // SAFETY: fcntl on a descriptor we own; failure is harmless.
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
    }
}

/// `open("/dev/null", O_RDWR)` (close-on-exec).
pub fn open_dev_null() -> Result<OwnedFd, i32> {
    // SAFETY: the path is a valid NUL-terminated string.
    let fd = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(errno());
    }
    Ok(owned(fd))
}

/// `dup(fd)` (close-on-exec); `fd` need not be owned by the caller.
pub fn dup(fd: RawFd) -> Result<OwnedFd, i32> {
    // SAFETY: F_DUPFD_CLOEXEC creates a new descriptor or fails with EBADF.
    let r = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if r < 0 {
        return Err(errno());
    }
    Ok(owned(r))
}

// ------------------------------------------------------------- processes

/// A file action of `posix_spawn`.
#[derive(Debug, Clone)]
pub enum FileAction {
    /// `posix_spawn_file_actions_addclose`.
    Close(RawFd),
    /// `posix_spawn_file_actions_adddup2`.
    Dup2(RawFd, RawFd),
    /// `posix_spawn_file_actions_addchdir_np`.
    Chdir(Vec<u8>),
}

/// Which step of a spawn failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnError {
    /// Adding file action `index` failed with errno.
    Action(usize, i32),
    /// `posix_spawn`/`posix_spawnp` failed with errno.
    Spawn(i32),
}

/// `posix_spawn(p)`: runs `program` with `argv` and `envp` (`None` =
/// inherit `environ`), applying `actions` in the child. `use_path` selects
/// `posix_spawnp`. Returns the child's pid. Action failures are reported
/// through `on_action_error` (PHP only warns for `chdir`).
pub fn spawn(
    program: &[u8],
    argv: &[Vec<u8>],
    envp: Option<&[Vec<u8>]>,
    actions: &[FileAction],
    use_path: bool,
    on_action_error: &mut dyn FnMut(usize, i32) -> bool,
) -> Result<libc::pid_t, SpawnError> {
    let program = cstring(program);
    let argv_c: Vec<CString> = argv.iter().map(|a| cstring(a)).collect();
    let mut argv_p: Vec<*mut libc::c_char> = argv_c.iter().map(|a| a.as_ptr().cast_mut()).collect();
    argv_p.push(ptr::null_mut());
    let envp_c: Option<Vec<CString>> = envp.map(|e| e.iter().map(|a| cstring(a)).collect());
    let mut envp_p: Option<Vec<*mut libc::c_char>> = envp_c.as_ref().map(|e| {
        let mut v: Vec<_> = e.iter().map(|a| a.as_ptr().cast_mut()).collect();
        v.push(ptr::null_mut());
        v
    });
    let chdirs: Vec<Option<CString>> = actions
        .iter()
        .map(|a| match a {
            FileAction::Chdir(p) => Some(cstring(p)),
            _ => None,
        })
        .collect();

    // SAFETY: an all-zero value is a valid argument for _init, which
    // fully initialises the structure.
    let mut fa: libc::posix_spawn_file_actions_t = unsafe { std::mem::zeroed() };
    // SAFETY: `fa` is valid storage for the file actions object.
    unsafe { libc::posix_spawn_file_actions_init(&raw mut fa) };
    let mut result = Ok(0);
    for (i, a) in actions.iter().enumerate() {
        // SAFETY: `fa` was initialised above; paths are NUL-terminated and
        // outlive the spawn call (they live in `chdirs`).
        let r = unsafe {
            match a {
                FileAction::Close(fd) => libc::posix_spawn_file_actions_addclose(&raw mut fa, *fd),
                FileAction::Dup2(from, to) => {
                    libc::posix_spawn_file_actions_adddup2(&raw mut fa, *from, *to)
                }
                FileAction::Chdir(_) => libc::posix_spawn_file_actions_addchdir_np(
                    &raw mut fa,
                    chdirs[i].as_ref().map_or(ptr::null(), |c| c.as_ptr()),
                ),
            }
        };
        if r != 0 && !on_action_error(i, r) {
            result = Err(SpawnError::Action(i, r));
            break;
        }
    }
    if result.is_ok() {
        let mut pid: libc::pid_t = 0;
        let env_ptr = match envp_p.as_mut() {
            Some(v) => v.as_ptr(),
            // SAFETY: reading the `environ` pointer itself; posix_spawn
            // only reads the array.
            None => unsafe { environ_ptr() },
        };
        // SAFETY: all pointers are valid NUL-terminated strings / NULL-
        // terminated arrays that outlive the call; `fa` is initialised.
        let r = unsafe {
            if use_path {
                libc::posix_spawnp(
                    &raw mut pid,
                    program.as_ptr(),
                    &raw const fa,
                    ptr::null(),
                    argv_p.as_ptr(),
                    env_ptr,
                )
            } else {
                libc::posix_spawn(
                    &raw mut pid,
                    program.as_ptr(),
                    &raw const fa,
                    ptr::null(),
                    argv_p.as_ptr(),
                    env_ptr,
                )
            }
        };
        result = if r == 0 {
            Ok(pid)
        } else {
            Err(SpawnError::Spawn(r))
        };
    }
    // SAFETY: `fa` was initialised by _init.
    unsafe { libc::posix_spawn_file_actions_destroy(&raw mut fa) };
    result
}

extern "C" {
    static environ: *const *mut libc::c_char;
}

/// The process environment array.
///
/// # Safety
/// The returned pointer is only valid until the environment is modified.
unsafe fn environ_ptr() -> *const *mut libc::c_char {
    // SAFETY: reading a C global; see the function contract.
    unsafe { environ }
}

/// `waitpid(pid, &status, options)`: `Ok((pid, status))` or `Err(errno)`.
pub fn waitpid(pid: libc::pid_t, options: i32) -> Result<(libc::pid_t, i32), i32> {
    let mut status = 0;
    // SAFETY: `status` is a valid out-pointer.
    let r = unsafe { libc::waitpid(pid, &raw mut status, options) };
    if r < 0 {
        Err(errno())
    } else {
        Ok((r, status))
    }
}

/// `kill(pid, sig) == 0`.
#[must_use]
pub fn kill(pid: libc::pid_t, sig: i32) -> bool {
    // SAFETY: kill has no memory-safety preconditions.
    unsafe { libc::kill(pid, sig) == 0 }
}

/// `nice(inc)`: `Err(errno)` when errno was set.
pub fn nice(inc: i32) -> Result<(), i32> {
    // SAFETY: errno is thread-local; nice has no memory preconditions.
    unsafe {
        *libc::__errno_location() = 0;
        libc::nice(inc);
    }
    match errno() {
        0 => Ok(()),
        e => Err(e),
    }
}

// ------------------------------------------------------------- locale

/// `setlocale(category, locale)` (`None` queries). Returns the resulting
/// locale name, or `None` on failure.
#[must_use]
pub fn setlocale(category: i32, locale: Option<&[u8]>) -> Option<Vec<u8>> {
    let loc = locale.map(cstring);
    // SAFETY: the locale argument is NULL or a valid C string; the result
    // is copied before any other locale call can invalidate it.
    let r = unsafe { libc::setlocale(category, loc.as_ref().map_or(ptr::null(), |c| c.as_ptr())) };
    from_cstr(r)
}

/// The numeric and monetary fields of `localeconv()`.
#[derive(Debug, Clone, Default)]
pub struct Lconv {
    /// String fields in PHP's array order.
    pub strings: Vec<(&'static str, Vec<u8>)>,
    /// `char` fields in PHP's array order.
    pub chars: Vec<(&'static str, i8)>,
    /// `grouping` bytes.
    pub grouping: Vec<i8>,
    /// `mon_grouping` bytes.
    pub mon_grouping: Vec<i8>,
}

/// `localeconv()` of the current locale.
#[must_use]
pub fn localeconv() -> Lconv {
    // SAFETY: localeconv returns a pointer to a static structure valid
    // until the next localeconv/setlocale call; it is copied immediately.
    let l = unsafe { libc::localeconv() };
    if l.is_null() {
        return Lconv::default();
    }
    // SAFETY: `l` is non-NULL and points to a valid lconv.
    let l = unsafe { &*l };
    let s = |p: *const libc::c_char| from_cstr(p).unwrap_or_default();
    let g = |p: *const libc::c_char| -> Vec<i8> {
        #[allow(clippy::cast_possible_wrap)]
        s(p).into_iter().map(|b| b as i8).collect()
    };
    Lconv {
        strings: vec![
            ("decimal_point", s(l.decimal_point)),
            ("thousands_sep", s(l.thousands_sep)),
            ("int_curr_symbol", s(l.int_curr_symbol)),
            ("currency_symbol", s(l.currency_symbol)),
            ("mon_decimal_point", s(l.mon_decimal_point)),
            ("mon_thousands_sep", s(l.mon_thousands_sep)),
            ("positive_sign", s(l.positive_sign)),
            ("negative_sign", s(l.negative_sign)),
        ],
        #[allow(clippy::cast_possible_wrap)]
        chars: vec![
            ("int_frac_digits", l.int_frac_digits as i8),
            ("frac_digits", l.frac_digits as i8),
            ("p_cs_precedes", l.p_cs_precedes as i8),
            ("p_sep_by_space", l.p_sep_by_space as i8),
            ("n_cs_precedes", l.n_cs_precedes as i8),
            ("n_sep_by_space", l.n_sep_by_space as i8),
            ("p_sign_posn", l.p_sign_posn as i8),
            ("n_sign_posn", l.n_sign_posn as i8),
        ],
        grouping: g(l.grouping),
        mon_grouping: g(l.mon_grouping),
    }
}

/// `nl_langinfo(item)`.
#[must_use]
pub fn nl_langinfo(item: i32) -> Option<Vec<u8>> {
    // SAFETY: nl_langinfo accepts any item value and returns a pointer to
    // a string valid until the next locale change; it is copied at once.
    from_cstr(unsafe { libc::nl_langinfo(item) })
}

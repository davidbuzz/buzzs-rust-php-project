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

// ------------------------------------------------------------- network

extern "C" {
    fn inet_pton(af: libc::c_int, src: *const libc::c_char, dst: *mut libc::c_void) -> libc::c_int;
    fn inet_ntop(
        af: libc::c_int,
        src: *const libc::c_void,
        dst: *mut libc::c_char,
        size: libc::socklen_t,
    ) -> *const libc::c_char;
    fn gethostbyname_r(
        name: *const libc::c_char,
        ret: *mut libc::hostent,
        buf: *mut libc::c_char,
        buflen: libc::size_t,
        result: *mut *mut libc::hostent,
        h_errnop: *mut libc::c_int,
    ) -> libc::c_int;
    fn getnameinfo(
        sa: *const libc::sockaddr,
        salen: libc::socklen_t,
        host: *mut libc::c_char,
        hostlen: libc::socklen_t,
        serv: *mut libc::c_char,
        servlen: libc::socklen_t,
        flags: libc::c_int,
    ) -> libc::c_int;
    fn res_search(
        dname: *const libc::c_char,
        class: libc::c_int,
        typ: libc::c_int,
        answer: *mut u8,
        anslen: libc::c_int,
    ) -> libc::c_int;
    fn __h_errno_location() -> *mut libc::c_int;
}

/// `inet_pton(af, src)`: the packed address (4 or 16 bytes).
#[must_use]
pub fn inet_pton_raw(af: i32, src: &[u8]) -> Option<Vec<u8>> {
    let c = cstring(src);
    let mut buf = [0u8; 16];
    // SAFETY: `c` is NUL-terminated; `buf` has room for an in6_addr.
    let r = unsafe { inet_pton(af, c.as_ptr(), buf.as_mut_ptr().cast()) };
    if r <= 0 {
        return None;
    }
    Some(buf[..if af == libc::AF_INET { 4 } else { 16 }].to_vec())
}

/// `inet_ntop(af, src)`: `src` must be 4 (`AF_INET`) or 16 bytes.
#[must_use]
pub fn inet_ntop_raw(af: i32, src: &[u8]) -> Option<String> {
    let need = if af == libc::AF_INET { 4 } else { 16 };
    if src.len() != need {
        return None;
    }
    let mut buf = [0u8; 64];
    // SAFETY: `src` holds a full address of the family; `buf` is writable
    // for its stated length.
    let r = unsafe { inet_ntop(af, src.as_ptr().cast(), buf.as_mut_ptr().cast(), 64) };
    if r.is_null() {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(0);
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

/// `gethostname()`.
pub fn gethostname() -> Result<Vec<u8>, i32> {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is writable for `len` bytes (one spare for the NUL).
    let r = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len() - 1) };
    if r != 0 {
        return Err(errno());
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Ok(buf[..end].to_vec())
}

/// `gethostbyname(name)`: the IPv4 addresses of `h_addr_list`, or `None`.
#[must_use]
#[allow(clippy::many_single_char_names)]
pub fn gethostbyname(name: &[u8]) -> Option<Vec<[u8; 4]>> {
    let c = cstring(name);
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        // SAFETY: zeroed hostent is valid storage for the out-parameter.
        let mut he: libc::hostent = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::hostent = ptr::null_mut();
        let mut herr = 0;
        // SAFETY: all pointers are valid; `buf` is writable for its length.
        let r = unsafe {
            gethostbyname_r(
                c.as_ptr(),
                &raw mut he,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &raw mut result,
                &raw mut herr,
            )
        };
        if r == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if r != 0 || result.is_null() || he.h_addrtype != libc::AF_INET || he.h_length != 4 {
            return None;
        }
        let mut out = Vec::new();
        let mut i = 0;
        loop {
            // SAFETY: h_addr_list is a NULL-terminated array of pointers
            // to 4-byte addresses inside `buf`.
            let p = unsafe { *he.h_addr_list.add(i) };
            if p.is_null() {
                break;
            }
            let mut a = [0u8; 4];
            // SAFETY: each entry points to h_length (4) bytes.
            unsafe { ptr::copy_nonoverlapping(p.cast::<u8>(), a.as_mut_ptr(), 4) };
            out.push(a);
            i += 1;
        }
        return Some(out);
    }
}

/// Reverse lookup with `getnameinfo(..., NI_NAMEREQD)` of a packed IPv4
/// (4 bytes) or IPv6 (16 bytes) address.
#[must_use]
pub fn reverse_lookup(addr: &[u8]) -> Option<Vec<u8>> {
    let mut host = [0u8; 1025];
    let r = match addr.len() {
        4 => {
            // SAFETY: zeroed sockaddr_in is a valid value.
            let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
            sa.sin_family = libc::sa_family_t::try_from(libc::AF_INET).unwrap_or(2);
            sa.sin_addr.s_addr = u32::from_ne_bytes([addr[0], addr[1], addr[2], addr[3]]);
            // SAFETY: `sa` is a properly sized sockaddr_in; `host` is writable.
            unsafe {
                getnameinfo(
                    (&raw const sa).cast(),
                    libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_in>())
                        .unwrap_or(16),
                    host.as_mut_ptr().cast(),
                    1025,
                    ptr::null_mut(),
                    0,
                    libc::NI_NAMEREQD,
                )
            }
        }
        16 => {
            // SAFETY: zeroed sockaddr_in6 is a valid value.
            let mut sa: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
            sa.sin6_family = libc::sa_family_t::try_from(libc::AF_INET6).unwrap_or(10);
            sa.sin6_addr.s6_addr.copy_from_slice(addr);
            // SAFETY: as above for sockaddr_in6.
            unsafe {
                getnameinfo(
                    (&raw const sa).cast(),
                    libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_in6>())
                        .unwrap_or(28),
                    host.as_mut_ptr().cast(),
                    1025,
                    ptr::null_mut(),
                    0,
                    libc::NI_NAMEREQD,
                )
            }
        }
        _ => return None,
    };
    if r != 0 {
        return None;
    }
    let end = host.iter().position(|&c| c == 0).unwrap_or(0);
    Some(host[..end].to_vec())
}

/// `getservbyname(name, proto)`: the port in host order.
#[must_use]
pub fn getservbyname(name: &[u8], proto: &[u8]) -> Option<u16> {
    let (n, p) = (cstring(name), cstring(proto));
    // SAFETY: valid C strings; the static result is read immediately.
    let s = unsafe { libc::getservbyname(n.as_ptr(), p.as_ptr()) };
    if s.is_null() {
        return None;
    }
    // SAFETY: non-NULL servent from libc.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let port = unsafe { (*s).s_port } as u16;
    Some(u16::from_be(port))
}

/// `getservbyport(htons(port), proto)`: the service name.
#[must_use]
pub fn getservbyport(port: u16, proto: &[u8]) -> Option<Vec<u8>> {
    let p = cstring(proto);
    // SAFETY: valid C string; the static result is copied immediately.
    let s = unsafe { libc::getservbyport(i32::from(port.to_be()), p.as_ptr()) };
    if s.is_null() {
        return None;
    }
    // SAFETY: non-NULL servent from libc.
    from_cstr(unsafe { (*s).s_name })
}

/// `getprotobyname(name)`.
#[must_use]
pub fn getprotobyname(name: &[u8]) -> Option<i64> {
    let n = cstring(name);
    // SAFETY: valid C string; the static result is read immediately.
    let p = unsafe { libc::getprotobyname(n.as_ptr()) };
    if p.is_null() {
        return None;
    }
    // SAFETY: non-NULL protoent from libc.
    Some(i64::from(unsafe { (*p).p_proto }))
}

/// `getprotobynumber(proto)`.
#[must_use]
pub fn getprotobynumber(proto: i32) -> Option<Vec<u8>> {
    // SAFETY: no preconditions; the static result is copied immediately.
    let p = unsafe { libc::getprotobynumber(proto) };
    if p.is_null() {
        return None;
    }
    // SAFETY: non-NULL protoent from libc.
    from_cstr(unsafe { (*p).p_name })
}

/// A socket address from `getifaddrs()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SockAddr {
    /// IPv4 address bytes.
    V4([u8; 4]),
    /// IPv6 address bytes.
    V6([u8; 16]),
    /// Another family (only the family number is known).
    Other(i64),
}

impl SockAddr {
    /// The address family number.
    #[must_use]
    pub fn family(&self) -> i64 {
        match self {
            SockAddr::V4(_) => i64::from(libc::AF_INET),
            SockAddr::V6(_) => i64::from(libc::AF_INET6),
            SockAddr::Other(f) => *f,
        }
    }
}

/// One `struct ifaddrs` entry.
#[derive(Debug, Clone)]
pub struct IfAddr {
    /// Interface name.
    pub name: Vec<u8>,
    /// `ifa_flags`.
    pub flags: u32,
    /// `ifa_addr`.
    pub addr: Option<SockAddr>,
    /// `ifa_netmask`.
    pub netmask: Option<SockAddr>,
    /// `ifa_broadaddr` / `ifa_dstaddr` (the union).
    pub broad_or_dst: Option<SockAddr>,
}

fn sockaddr(p: *const libc::sockaddr) -> Option<SockAddr> {
    if p.is_null() {
        return None;
    }
    // SAFETY: non-NULL sockaddr from getifaddrs; the family tells which
    // larger structure it really is.
    let fam = i32::from(unsafe { (*p).sa_family });
    Some(match fam {
        libc::AF_INET => {
            // SAFETY: AF_INET entries are sockaddr_in; read unaligned to
            // not rely on the allocation's alignment.
            #[allow(clippy::cast_ptr_alignment)]
            let sa = unsafe { ptr::read_unaligned(p.cast::<libc::sockaddr_in>()) };
            SockAddr::V4(sa.sin_addr.s_addr.to_ne_bytes())
        }
        libc::AF_INET6 => {
            // SAFETY: AF_INET6 entries are sockaddr_in6 (read unaligned).
            #[allow(clippy::cast_ptr_alignment)]
            let sa = unsafe { ptr::read_unaligned(p.cast::<libc::sockaddr_in6>()) };
            SockAddr::V6(sa.sin6_addr.s6_addr)
        }
        f => SockAddr::Other(i64::from(f)),
    })
}

/// `getifaddrs()`.
pub fn getifaddrs() -> Result<Vec<IfAddr>, i32> {
    let mut addrs: *mut libc::ifaddrs = ptr::null_mut();
    // SAFETY: out-pointer is valid.
    if unsafe { libc::getifaddrs(&raw mut addrs) } != 0 {
        return Err(errno());
    }
    let mut out = Vec::new();
    let mut p = addrs;
    while !p.is_null() {
        // SAFETY: `p` walks the list returned by getifaddrs.
        let ifa = unsafe { &*p };
        out.push(IfAddr {
            name: from_cstr(ifa.ifa_name).unwrap_or_default(),
            flags: ifa.ifa_flags,
            addr: sockaddr(ifa.ifa_addr),
            netmask: sockaddr(ifa.ifa_netmask),
            broad_or_dst: sockaddr(ifa.ifa_ifu),
        });
        p = ifa.ifa_next;
    }
    // SAFETY: `addrs` came from getifaddrs and is freed once.
    unsafe { libc::freeifaddrs(addrs) };
    Ok(out)
}

/// `res_search(name, C_IN, type)`: the raw answer, or the resolver's
/// `h_errno` on failure.
pub fn res_query(name: &[u8], rtype: i32) -> Result<Vec<u8>, i32> {
    let c = cstring(name);
    let mut buf = vec![0u8; 65536];
    // SAFETY: `buf` is writable for 65536 bytes; `c` is NUL-terminated.
    let n = unsafe { res_search(c.as_ptr(), 1, rtype, buf.as_mut_ptr(), 65536) };
    if n < 0 {
        // SAFETY: h_errno location is thread-local and always valid.
        return Err(unsafe { *__h_errno_location() });
    }
    buf.truncate(usize::try_from(n).unwrap_or(0).min(65536));
    Ok(buf)
}

// ------------------------------------------------------------- syslog

/// `openlog(ident, option, facility)`; the identity string is kept alive.
pub fn openlog(ident: &[u8], option: i32, facility: i32) {
    static IDENT: std::sync::Mutex<Option<CString>> = std::sync::Mutex::new(None);
    let c = cstring(ident);
    let mut guard = IDENT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: the C string is stored in a static before and while syslog
    // uses it; the previous one is replaced only after openlog switched.
    unsafe { libc::openlog(c.as_ptr(), option, facility) };
    *guard = Some(c);
}

/// `syslog(priority, "%s", message)` (cut at the first NUL).
pub fn syslog(priority: i32, message: &[u8]) {
    let m = cstring(message);
    // SAFETY: "%s" with a valid C string argument.
    unsafe { libc::syslog(priority, c"%s".as_ptr(), m.as_ptr()) };
}

/// `closelog()`.
pub fn closelog() {
    // SAFETY: no preconditions.
    unsafe { libc::closelog() };
}

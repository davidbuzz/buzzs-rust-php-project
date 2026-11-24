// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The operating-system calls of `php-fpm` that the standard library has
//! no safe form for (ref: `sapi/fpm/fpm/fpm_signals.c`, `fpm_children.c`,
//! `fpm_sockets.c`, `fpm_unix.c`, `fpm_shm.c`, `fpm_clock.c`, `zlog.c`).
//!
//! This is the FFI module of `php-fpm`: every `unsafe` block below is one
//! libc call on plain integers, on C strings it owns, or on memory owned
//! by the caller; the shared scoreboard mapping is the only raw memory and
//! is described where it is created.

use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicI32, Ordering};

/// The last OS error number.
fn errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// `strerror(e)`.
pub fn strerror(e: i32) -> String {
    let s = io::Error::from_raw_os_error(e).to_string();
    match s.rfind(" (os error ") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

/// `gettimeofday()`: seconds and microseconds.
pub fn now() -> (i64, u32) {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (
        i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        d.subsec_micros(),
    )
}

/// The fields of `struct tm` the logs print.
#[derive(Debug, Clone, Copy, Default)]
pub struct Tm {
    pub year: i32,
    pub mon: i32,
    pub mday: i32,
    pub hour: i32,
    pub min: i32,
    pub sec: i32,
    /// `tm_gmtoff`.
    pub gmtoff: i64,
    /// `tm_wday`.
    pub wday: i32,
}

/// `localtime_r()`.
pub fn localtime(secs: i64) -> Tm {
    let t: libc::time_t = secs;
    // SAFETY: `tm` is plain old data, zeroed is valid; localtime_r writes
    // only into it and reads `t`.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the whole call.
    unsafe { libc::localtime_r(&raw const t, &raw mut tm) };
    Tm {
        year: tm.tm_year + 1900,
        mon: tm.tm_mon,
        mday: tm.tm_mday,
        hour: tm.tm_hour,
        min: tm.tm_min,
        sec: tm.tm_sec,
        gmtoff: tm.tm_gmtoff,
        wday: tm.tm_wday,
    }
}

/// `strftime(buf, 128, format, localtime(&secs))` (the access log's
/// `%{format}t`); an empty string when the result does not fit.
pub fn strftime(format: &str, secs: i64) -> String {
    let t: libc::time_t = secs;
    // SAFETY: as in `localtime`.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the whole call.
    unsafe { libc::localtime_r(&raw const t, &raw mut tm) };
    let Ok(fmt) = std::ffi::CString::new(format) else {
        return String::new();
    };
    let mut buf = [0u8; 129];
    // SAFETY: `buf` is valid for 128 bytes of output, `fmt` is a C string
    // and `tm` was filled by `localtime_r`.
    let n = unsafe {
        libc::strftime(
            buf.as_mut_ptr().cast(),
            buf.len() - 1,
            fmt.as_ptr(),
            &raw const tm,
        )
    };
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

/// `zend_quiet_write()`: all of `buf` to `fd`, errors ignored.
pub fn write_all(fd: RawFd, mut buf: &[u8]) {
    while !buf.is_empty() {
        // SAFETY: `buf` is valid for reads of its length.
        let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
        if n < 0 && errno() == libc::EINTR {
            continue;
        }
        let Ok(n) = usize::try_from(n) else {
            return;
        };
        if n == 0 {
            return;
        }
        buf = &buf[n..];
    }
}

/// `openlog()` for `error_log = syslog`.
pub fn openlog(ident: &str, facility: i32) {
    // The identity must outlive every later syslog() call.
    let ident = Box::leak(CString::new(ident).unwrap_or_default().into_boxed_c_str());
    // SAFETY: `ident` is a NUL-terminated string that is never freed.
    unsafe { libc::openlog(ident.as_ptr(), libc::LOG_PID | libc::LOG_CONS, facility) };
}

/// `php_syslog(priority, "%s", msg)`.
pub fn syslog(priority: i32, msg: &[u8]) {
    let msg = CString::new(msg.iter().copied().filter(|&b| b != 0).collect::<Vec<u8>>())
        .unwrap_or_default();
    // SAFETY: the format is a literal "%s" and `msg` a NUL-terminated
    // string, valid for the call.
    unsafe { libc::syslog(priority, c"%s".as_ptr(), msg.as_ptr()) };
}

/// `glob()` with `GLOB_ERR | GLOB_MARK`: the sorted matches (an empty
/// list when nothing matches), `Err` when globbing failed.
#[allow(clippy::many_single_char_names)] // glob(3)'s names
pub fn glob(pattern: &str) -> Result<Vec<String>, ()> {
    let Ok(p) = CString::new(pattern) else {
        return Err(());
    };
    // SAFETY: `glob_t` is plain old data; zeroed is its initial state.
    let mut g: libc::glob_t = unsafe { std::mem::zeroed() };
    // SAFETY: `p` is a NUL-terminated string, `g` valid for writes; no
    // error callback.
    let r = unsafe {
        libc::glob(
            p.as_ptr(),
            libc::GLOB_ERR | libc::GLOB_MARK,
            None,
            &raw mut g,
        )
    };
    let out = match r {
        0 => {
            let mut v = Vec::new();
            for i in 0..g.gl_pathc {
                // SAFETY: glob filled `gl_pathv` with `gl_pathc` valid C
                // strings.
                let s = unsafe { CStr::from_ptr(*g.gl_pathv.add(i)) };
                let s = s.to_string_lossy().into_owned();
                // Directories end in "/" (GLOB_MARK): not files to read.
                if !s.ends_with('/') {
                    v.push(s);
                }
            }
            Ok(v)
        }
        libc::GLOB_NOMATCH => Ok(Vec::new()),
        _ => Err(()),
    };
    // SAFETY: `g` was initialised by glob (or zeroed, which globfree
    // accepts).
    unsafe { libc::globfree(&raw mut g) };
    out
}

/// `geteuid()`.
pub fn geteuid() -> u32 {
    // SAFETY: no arguments.
    unsafe { libc::geteuid() }
}

/// `getpid()`.
pub fn getpid() -> i32 {
    // SAFETY: no arguments.
    unsafe { libc::getpid() }
}

/// `isatty(fd)`.
pub fn isatty(fd: RawFd) -> bool {
    // SAFETY: plain integer argument.
    unsafe { libc::isatty(fd) == 1 }
}

/// `close(fd)`.
pub fn close(fd: RawFd) {
    // SAFETY: plain integer argument; the caller owns `fd`.
    unsafe { libc::close(fd) };
}

/// `dup2(from, to)`.
pub fn dup2(from: RawFd, to: RawFd) -> bool {
    // SAFETY: plain integer arguments.
    unsafe { libc::dup2(from, to) >= 0 }
}

/// `dup(fd)`.
pub fn dup(fd: RawFd) -> RawFd {
    // SAFETY: plain integer argument.
    unsafe { libc::dup(fd) }
}

/// `pipe()` with both ends close-on-exec.
pub fn pipe() -> io::Result<(RawFd, RawFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` is valid for writes of two integers.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((fds[0], fds[1]))
}

/// `fd_set_blocked(fd, 0)`.
pub fn set_nonblocking(fd: RawFd) {
    // SAFETY: fcntl on a plain integer descriptor with integer flags.
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        if fl >= 0 {
            libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
    }
}

/// Clears `FD_CLOEXEC` (a listening socket a re-executed master keeps).
pub fn clear_cloexec(fd: RawFd) {
    // SAFETY: fcntl on a plain integer descriptor with integer flags.
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFD);
        if fl >= 0 {
            libc::fcntl(fd, libc::F_SETFD, fl & !libc::FD_CLOEXEC);
        }
    }
}

/// `read(fd, buf)`: `None` at end or on an error other than `EAGAIN`
/// (`Some(0)` then).
pub fn read(fd: RawFd, buf: &mut [u8]) -> Option<usize> {
    loop {
        // SAFETY: `buf` is valid for writes of its length.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            match errno() {
                libc::EINTR => continue,
                libc::EAGAIN => return Some(0),
                _ => return None,
            }
        }
        return match usize::try_from(n) {
            Ok(0) | Err(_) => None,
            Ok(n) => Some(n),
        };
    }
}

/// `times()` of `fpm_request_end()`: the CPU time this process and its
/// waited-for children used, in microseconds.
pub fn cpu_us() -> u64 {
    let mut total = 0u64;
    for who in [libc::RUSAGE_SELF, libc::RUSAGE_CHILDREN] {
        // SAFETY: an all-zero `rusage` is valid; `getrusage` fills it.
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: `ru` is valid for writes.
        if unsafe { libc::getrusage(who, &raw mut ru) } == 0 {
            for tv in [ru.ru_utime, ru.ru_stime] {
                total += u64::try_from(tv.tv_sec).unwrap_or(0) * 1_000_000
                    + u64::try_from(tv.tv_usec).unwrap_or(0);
            }
        }
    }
    total
}

/// `read(fd, buf)`: the byte count (0 at the end), or the `errno`
/// (`EINTR` retried).
pub fn read_status(fd: RawFd, buf: &mut [u8]) -> Result<usize, i32> {
    loop {
        // SAFETY: `buf` is valid for writes of its length.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            match errno() {
                libc::EINTR => continue,
                e => return Err(e),
            }
        }
        return Ok(usize::try_from(n).unwrap_or(0));
    }
}

// ---------------------------------------------------------------------
// Sockets (`fpm_sockets.c`)

/// `fpm_sockets_new_listening_socket()` for an IP address: `SO_REUSEADDR`,
/// `bind()` and `listen(backlog)` (`-1`: the system's maximum).
pub fn inet_listen(addr: &std::net::SocketAddr, backlog: i32) -> io::Result<RawFd> {
    let (family, storage, len) = sockaddr(addr);
    // SAFETY: socket(2) with integer arguments.
    let sock = unsafe { libc::socket(family, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if sock < 0 {
        return Err(io::Error::last_os_error());
    }
    let one: libc::c_int = 1;
    // SAFETY: `one` is a valid int for the option's length.
    unsafe {
        libc::setsockopt(
            sock,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            (&raw const one).cast(),
            libc::socklen_t::try_from(std::mem::size_of::<libc::c_int>()).unwrap_or(4),
        );
    }
    // SAFETY: `storage` holds a valid address of `len` bytes.
    if unsafe { libc::bind(sock, (&raw const storage).cast(), len) } < 0 {
        let e = io::Error::last_os_error();
        close(sock);
        return Err(e);
    }
    // SAFETY: plain integer arguments.
    if unsafe { libc::listen(sock, backlog) } < 0 {
        let e = io::Error::last_os_error();
        close(sock);
        return Err(e);
    }
    Ok(sock)
}

/// A `sockaddr_storage` for `addr`.
fn sockaddr(addr: &std::net::SocketAddr) -> (i32, libc::sockaddr_storage, libc::socklen_t) {
    // SAFETY: `sockaddr_storage` is plain old data; zeroed is valid.
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    match addr {
        std::net::SocketAddr::V4(a) => {
            let sin = libc::sockaddr_in {
                sin_family: libc::sa_family_t::try_from(libc::AF_INET).unwrap_or(2),
                sin_port: a.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(a.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            // SAFETY: a sockaddr_in fits in sockaddr_storage.
            unsafe { std::ptr::write((&raw mut storage).cast::<libc::sockaddr_in>(), sin) };
            (
                libc::AF_INET,
                storage,
                libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_in>()).unwrap_or(16),
            )
        }
        std::net::SocketAddr::V6(a) => {
            let sin6 = libc::sockaddr_in6 {
                sin6_family: libc::sa_family_t::try_from(libc::AF_INET6).unwrap_or(10),
                sin6_port: a.port().to_be(),
                sin6_flowinfo: a.flowinfo(),
                sin6_addr: libc::in6_addr {
                    s6_addr: a.ip().octets(),
                },
                sin6_scope_id: a.scope_id(),
            };
            // SAFETY: a sockaddr_in6 fits in sockaddr_storage.
            unsafe { std::ptr::write((&raw mut storage).cast::<libc::sockaddr_in6>(), sin6) };
            (
                libc::AF_INET6,
                storage,
                libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_in6>()).unwrap_or(28),
            )
        }
    }
}

/// A `sockaddr_un` for `path` (cut to `sun_path`'s size, as `strlcpy`).
fn sockaddr_un(path: &str) -> (libc::sockaddr_un, libc::socklen_t) {
    // SAFETY: `sockaddr_un` is plain old data; zeroed is valid.
    let mut sa: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    sa.sun_family = libc::sa_family_t::try_from(libc::AF_UNIX).unwrap_or(1);
    let max = sa.sun_path.len() - 1;
    for (d, s) in sa.sun_path.iter_mut().zip(path.bytes().take(max)) {
        *d = libc::c_char::from_ne_bytes([s]);
    }
    (
        sa,
        libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_un>()).unwrap_or(110),
    )
}

/// The size of `sun_path`.
pub fn sun_path_len() -> usize {
    // SAFETY: plain old data.
    let sa: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    sa.sun_path.len()
}

/// `fpm_socket_unix_test_connect()`: whether something already listens
/// on the Unix socket `path`.
pub fn unix_test_connect(path: &str) -> bool {
    let (sa, len) = sockaddr_un(path);
    // SAFETY: socket(2) with integer arguments.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return false;
    }
    // SAFETY: `sa` is a valid address of `len` bytes.
    let r = unsafe { libc::connect(fd, (&raw const sa).cast(), len) };
    close(fd);
    r == 0
}

/// `fpm_sockets_new_listening_socket()` for a Unix socket: the old file
/// goes, the socket is created with `mode` (`umask(0777 ^ mode)`), then
/// `listen(backlog)`.
pub fn unix_listen(path: &str, backlog: i32, mode: u32) -> io::Result<RawFd> {
    let (sa, len) = sockaddr_un(path);
    // SAFETY: socket(2) with integer arguments.
    let sock = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if sock < 0 {
        return Err(io::Error::last_os_error());
    }
    let _ = std::fs::remove_file(path);
    // SAFETY: umask(2) with an integer mode.
    let saved = unsafe { libc::umask(0o777 ^ (mode & 0o777)) };
    // SAFETY: `sa` is a valid address of `len` bytes.
    let r = unsafe { libc::bind(sock, (&raw const sa).cast(), len) };
    let bind_err = io::Error::last_os_error();
    // SAFETY: restores the previous mask.
    unsafe { libc::umask(saved) };
    if r < 0 {
        close(sock);
        return Err(bind_err);
    }
    // SAFETY: plain integer arguments.
    if unsafe { libc::listen(sock, backlog) } < 0 {
        let e = io::Error::last_os_error();
        close(sock);
        return Err(e);
    }
    Ok(sock)
}

/// `chown(path, uid, gid)` (`-1`: unchanged).
pub fn chown(path: &str, uid: Option<u32>, gid: Option<u32>) -> io::Result<()> {
    let p = CString::new(path).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `p` is a NUL-terminated path; ids are integers.
    let r = unsafe { libc::chown(p.as_ptr(), uid.unwrap_or(u32::MAX), gid.unwrap_or(u32::MAX)) };
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `getsockopt(fd, SOL_SOCKET, SO_ACCEPTCONN)` is not needed; the queue
/// length of a TCP listener (`fpm_socket_get_listening_queue()`, from
/// `TCP_INFO`'s `tcpi_unacked`).
pub fn listen_queue(fd: RawFd) -> Option<(u32, u32)> {
    // SAFETY: `tcp_info` is plain old data; zeroed is valid.
    let mut info: libc::tcp_info = unsafe { std::mem::zeroed() };
    let mut len = libc::socklen_t::try_from(std::mem::size_of::<libc::tcp_info>()).unwrap_or(0);
    // SAFETY: `info` and `len` are valid for writes.
    let r = unsafe {
        libc::getsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_INFO,
            (&raw mut info).cast(),
            &raw mut len,
        )
    };
    (r == 0).then_some((info.tcpi_unacked, info.tcpi_sacked))
}

// ---------------------------------------------------------------------
// Processes and signals (`fpm_children.c`, `fpm_signals.c`)

/// The result of [`fork`].
pub enum Fork {
    /// In the child.
    Child,
    /// In the parent: the child's PID.
    Parent(i32),
}

/// `fork()`.
pub fn fork() -> io::Result<Fork> {
    // SAFETY: fork(2); the processes are single-threaded here, so the
    // child continues with a consistent copy of this thread's state.
    match unsafe { libc::fork() } {
        -1 => Err(io::Error::last_os_error()),
        0 => Ok(Fork::Child),
        pid => Ok(Fork::Parent(pid)),
    }
}

/// How a child ended.
#[derive(Debug, Clone, Copy)]
pub enum Exit {
    /// `exit(code)`.
    Code(i32),
    /// Killed by a signal (and whether it dumped core).
    Signal(i32, bool),
    /// Stopped (traced).
    Stopped,
}

/// `waitpid(-1, &status, WNOHANG | WUNTRACED)`.
pub fn reap() -> Option<(i32, Exit)> {
    let mut status: libc::c_int = 0;
    // SAFETY: waitpid writes the status into a local integer.
    let pid = unsafe { libc::waitpid(-1, &raw mut status, libc::WNOHANG | libc::WUNTRACED) };
    if pid <= 0 {
        return None;
    }
    let exit = if libc::WIFEXITED(status) {
        Exit::Code(libc::WEXITSTATUS(status))
    } else if libc::WIFSIGNALED(status) {
        Exit::Signal(libc::WTERMSIG(status), libc::WCOREDUMP(status))
    } else {
        Exit::Stopped
    };
    Some((pid, exit))
}

/// `kill(pid, sig)`.
pub fn kill(pid: i32, sig: i32) -> bool {
    // SAFETY: plain integer arguments.
    unsafe { libc::kill(pid, sig) == 0 }
}

/// `fpm_signal_names[]`.
pub fn signal_name(sig: i32) -> &'static str {
    match sig {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        libc::SIGCHLD => "SIGCHLD",
        libc::SIGCONT => "SIGCONT",
        libc::SIGSTOP => "SIGSTOP",
        libc::SIGTSTP => "SIGTSTP",
        libc::SIGTTIN => "SIGTTIN",
        libc::SIGTTOU => "SIGTTOU",
        libc::SIGURG => "SIGURG",
        libc::SIGXCPU => "SIGXCPU",
        libc::SIGXFSZ => "SIGXFSZ",
        libc::SIGVTALRM => "SIGVTALRM",
        libc::SIGPROF => "SIGPROF",
        libc::SIGWINCH => "SIGWINCH",
        libc::SIGIO => "SIGIO",
        libc::SIGPWR => "SIGPWR",
        libc::SIGSYS => "SIGSYS",
        _ => "",
    }
}

/// The write end of the master's signal pipe (`sp[1]`).
static SIGNAL_PIPE: AtomicI32 = AtomicI32::new(-1);

extern "C" fn master_handler(signo: libc::c_int) {
    // `sig_handler()`: one byte per signal into the pipe.
    let c: u8 = match signo {
        libc::SIGTERM => b'T',
        libc::SIGINT => b'I',
        libc::SIGUSR1 => b'1',
        libc::SIGUSR2 => b'2',
        libc::SIGQUIT => b'Q',
        libc::SIGCHLD => b'C',
        _ => return,
    };
    let fd = SIGNAL_PIPE.load(Ordering::Relaxed);
    if fd >= 0 {
        let saved = errno();
        // SAFETY: write(2) is async-signal-safe; one byte from a local.
        unsafe { libc::write(fd, (&raw const c).cast(), 1) };
        // SAFETY: restores errno (async-signal-safe).
        unsafe { *libc::__errno_location() = saved };
    }
}

fn set_handler(signo: i32, handler: libc::sighandler_t, restart: bool) -> bool {
    // SAFETY: `sigaction` is plain old data; zeroed then filled in.
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    sa.sa_sigaction = handler;
    sa.sa_flags = if restart { libc::SA_RESTART } else { 0 };
    // SAFETY: valid pointers to locals for the duration of the calls.
    unsafe {
        libc::sigemptyset(&raw mut sa.sa_mask);
        libc::sigaction(signo, &raw const sa, std::ptr::null_mut()) == 0
    }
}

/// `fpm_signals_init_main()`: SIGTERM, SIGINT, SIGUSR1, SIGUSR2, SIGCHLD
/// and SIGQUIT write a byte into a pipe the event loop reads; returns its
/// read end.
/// `fpm_signals_init_mask()`'s `block_sigset`: blocked from the start and
/// around `execvp()`, so a reload signal arriving before the handlers are
/// installed does not end the master.
pub const BLOCK_SIGNALS: [libc::c_int; 3] = [libc::SIGUSR1, libc::SIGUSR2, libc::SIGCHLD];
/// `child_block_sigset`: blocked around `fork()`.
pub const CHILD_BLOCK_SIGNALS: [libc::c_int; 5] = [
    libc::SIGUSR1,
    libc::SIGUSR2,
    libc::SIGCHLD,
    libc::SIGTERM,
    libc::SIGQUIT,
];

/// `execv(path, args)` with this process's environment and signal mask;
/// returns only on failure.
pub fn execv(path: &std::ffi::OsStr, args: &[std::ffi::OsString]) -> io::Error {
    use std::os::unix::ffi::OsStrExt;
    let Ok(p) = CString::new(path.as_bytes()) else {
        return io::Error::from(io::ErrorKind::InvalidInput);
    };
    let Ok(cstrings) = args
        .iter()
        .map(|a| CString::new(a.as_bytes()))
        .collect::<Result<Vec<_>, _>>()
    else {
        return io::Error::from(io::ErrorKind::InvalidInput);
    };
    let mut pointers: Vec<*const libc::c_char> = cstrings.iter().map(|a| a.as_ptr()).collect();
    pointers.push(std::ptr::null());
    // SAFETY: `p` and every `pointers` entry are NUL-terminated strings
    // that live until the call returns; `pointers` ends with a null pointer.
    unsafe { libc::execv(p.as_ptr(), pointers.as_ptr()) };
    io::Error::last_os_error()
}

/// `fpm_signals_block()` / `fpm_signals_child_block()` for this thread.
pub fn block_signals(sigs: &[libc::c_int]) {
    // SAFETY: `set` is initialized by `sigemptyset` before use; the calls
    // only read and write it.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut set);
        for &s in sigs {
            libc::sigaddset(&raw mut set, s);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &raw const set, std::ptr::null_mut());
    }
}

/// `fpm_signals_unblock()`: every signal (the mask may come from before
/// `execve()`).
pub fn unblock_signals() {
    // SAFETY: as in `block_signals`.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&raw mut set);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &raw const set, std::ptr::null_mut());
    }
}

pub fn init_master_signals() -> io::Result<RawFd> {
    let (r, w) = pipe()?;
    set_nonblocking(r);
    set_nonblocking(w);
    SIGNAL_PIPE.store(w, Ordering::Relaxed);
    for s in [
        libc::SIGTERM,
        libc::SIGINT,
        libc::SIGUSR1,
        libc::SIGUSR2,
        libc::SIGCHLD,
        libc::SIGQUIT,
    ] {
        if !set_handler(s, master_handler as *const () as libc::sighandler_t, true) {
            return Err(io::Error::last_os_error());
        }
    }
    unblock_signals();
    Ok(r)
}

/// Set by SIGQUIT in a worker (`sig_soft_quit()`): finish the current
/// request, then exit.
static SOFT_QUIT: AtomicI32 = AtomicI32::new(0);

extern "C" fn child_soft_quit(_signo: libc::c_int) {
    SOFT_QUIT.store(1, Ordering::Relaxed);
    crate::sys::request_shutdown();
}

/// Whether the worker was asked to stop after its request.
pub fn soft_quit() -> bool {
    SOFT_QUIT.load(Ordering::Relaxed) != 0
}

/// `fpm_signals_init_child()`: the master's handlers go (SIGTERM, SIGINT,
/// SIGUSR1, SIGUSR2, SIGCHLD back to their defaults), SIGQUIT finishes
/// the request first, SIGPIPE stays ignored.
pub fn init_child_signals() {
    let w = SIGNAL_PIPE.swap(-1, Ordering::Relaxed);
    if w >= 0 {
        close(w);
    }
    for s in [
        libc::SIGTERM,
        libc::SIGINT,
        libc::SIGUSR1,
        libc::SIGUSR2,
        libc::SIGCHLD,
    ] {
        set_handler(s, libc::SIG_DFL, false);
    }
    set_handler(
        libc::SIGQUIT,
        child_soft_quit as *const () as libc::sighandler_t,
        false,
    );
    unblock_signals();
}

/// `poll()` on `fds` (descriptor, events); the returned events per entry,
/// `None` when interrupted.
pub fn poll(fds: &[(RawFd, i16)], timeout_ms: i32) -> Option<Vec<i16>> {
    let mut p: Vec<libc::pollfd> = fds
        .iter()
        .map(|&(fd, events)| libc::pollfd {
            fd,
            events,
            revents: 0,
        })
        .collect();
    // SAFETY: `p` is valid for reads and writes of `p.len()` entries.
    let r = unsafe {
        libc::poll(
            p.as_mut_ptr(),
            libc::nfds_t::try_from(p.len()).unwrap_or(0),
            timeout_ms,
        )
    };
    if r < 0 {
        return None;
    }
    Some(p.iter().map(|x| x.revents).collect())
}

/// `setsid()`.
pub fn setsid() {
    // SAFETY: no arguments.
    unsafe { libc::setsid() };
}

// ---------------------------------------------------------------------
// Users, limits, process attributes (`fpm_unix.c`)

/// `getpwnam(name)`: (uid, gid, home directory).
pub fn getpwnam(name: &str) -> Option<(u32, u32, String)> {
    let c = CString::new(name).ok()?;
    // SAFETY: `c` is a NUL-terminated name; the returned entry is static
    // storage of libc, read before the next call.
    let pw = unsafe { libc::getpwnam(c.as_ptr()) };
    if pw.is_null() {
        return None;
    }
    // SAFETY: `pw` points to a valid passwd entry.
    let pw = unsafe { &*pw };
    // SAFETY: `pw_dir` is a valid C string or null.
    let dir = if pw.pw_dir.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(pw.pw_dir) }
            .to_string_lossy()
            .into_owned()
    };
    Some((pw.pw_uid, pw.pw_gid, dir))
}

/// `getpwuid(uid)`'s name.
pub fn user_name(uid: u32) -> Option<String> {
    // SAFETY: integer argument; static storage read before the next call.
    let pw = unsafe { libc::getpwuid(uid) };
    if pw.is_null() {
        return None;
    }
    // SAFETY: `pw` points to a valid passwd entry whose name is a C string.
    Some(
        unsafe { CStr::from_ptr((*pw).pw_name) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// `getgrnam(name)`'s gid.
pub fn getgrnam(name: &str) -> Option<u32> {
    let c = CString::new(name).ok()?;
    // SAFETY: `c` is NUL-terminated; static storage read at once.
    let gr = unsafe { libc::getgrnam(c.as_ptr()) };
    if gr.is_null() {
        return None;
    }
    // SAFETY: `gr` points to a valid group entry.
    Some(unsafe { (*gr).gr_gid })
}

/// `setgid(gid)`, `initgroups(user, gid)` and `setuid(uid)`, in that
/// order (`fpm_unix_init_child()`).
pub fn drop_privileges(user: Option<&str>, uid: Option<u32>, gid: Option<u32>) -> io::Result<()> {
    if let Some(g) = gid {
        // SAFETY: integer argument.
        if unsafe { libc::setgid(g) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    if let (Some(u), Some(g)) = (user, gid) {
        let c = CString::new(u).unwrap_or_default();
        // SAFETY: NUL-terminated name and an integer gid.
        if unsafe { libc::initgroups(c.as_ptr(), g) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    if let Some(u) = uid {
        // SAFETY: integer argument.
        if unsafe { libc::setuid(u) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// `chroot(path)`.
pub fn chroot(path: &str) -> io::Result<()> {
    let c = CString::new(path).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: NUL-terminated path.
    if unsafe { libc::chroot(c.as_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `setrlimit(RLIMIT_NOFILE)` / `setrlimit(RLIMIT_CORE)` (`-1`:
/// unlimited).
pub fn set_rlimit(files: bool, value: i64) -> io::Result<()> {
    let v = if value < 0 {
        libc::RLIM_INFINITY
    } else {
        libc::rlim_t::try_from(value).unwrap_or(libc::RLIM_INFINITY)
    };
    let r = libc::rlimit {
        rlim_cur: v,
        rlim_max: v,
    };
    let which = if files {
        libc::RLIMIT_NOFILE
    } else {
        libc::RLIMIT_CORE
    };
    // SAFETY: `r` is a valid rlimit for the call.
    if unsafe { libc::setrlimit(which, &raw const r) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `setpriority(PRIO_PROCESS, 0, prio)`.
pub fn set_priority(prio: i32) -> io::Result<()> {
    // SAFETY: integer arguments.
    if unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, prio) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `prctl(PR_SET_DUMPABLE, 1)`.
pub fn set_dumpable() {
    // SAFETY: integer arguments.
    unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 1, 0, 0, 0) };
}

// ---------------------------------------------------------------------
// Shared memory (`fpm_shm.c`)

/// `fpm_shm_alloc()`: `size` zeroed bytes shared with every process forked
/// afterwards (`MAP_SHARED | MAP_ANONYMOUS`), never unmapped; `None` when
/// the mapping failed.
pub fn shm_alloc(size: usize) -> Option<&'static [std::sync::atomic::AtomicU64]> {
    let words = size.div_ceil(8).max(1);
    // SAFETY: an anonymous shared mapping of `words * 8` bytes; no file.
    let p = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            words * 8,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if p == libc::MAP_FAILED {
        return None;
    }
    // SAFETY: the mapping is page aligned, zero filled, `words * 8` bytes
    // long and lives for the rest of the process (and its children); an
    // `AtomicU64` has the size and alignment of a `u64` and every bit
    // pattern is valid, so the memory is a valid slice of atomics that
    // the processes sharing it access only atomically.
    Some(unsafe { std::slice::from_raw_parts(p.cast::<std::sync::atomic::AtomicU64>(), words) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strerror_has_no_rust_suffix() {
        assert_eq!(strerror(libc::ENOENT), "No such file or directory");
    }

    #[test]
    fn signal_names() {
        assert_eq!(signal_name(libc::SIGTERM), "SIGTERM");
        assert_eq!(signal_name(999), "");
    }

    #[test]
    fn shared_memory_is_zeroed() {
        let m = shm_alloc(64).expect("mmap");
        assert_eq!(m.len(), 8);
        assert!(m.iter().all(|w| w.load(Ordering::Relaxed) == 0));
    }
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Thin, safe wrappers over the libc process-control calls.
//!
//! This is the only module of the crate containing `unsafe` code.  Failures
//! are reported as the raw `errno` (`Err(errno)`).  The signal handler
//! installed for PHP callables ([`php_signal_handler`]) only copies the
//! `siginfo_t` and hands it to the lock-free queue in [`crate::signals`],
//! which is async-signal-safe.

#![allow(unsafe_code)]

use std::ffi::CString;
use std::os::raw::{c_int, c_long, c_void};

/// Reads the calling thread's `errno`.
pub fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Sets the calling thread's `errno`.
fn set_errno(value: i32) {
    // SAFETY: `__errno_location` returns the calling thread's errno slot.
    unsafe { *libc::__errno_location() = value };
}

/// `strerror()` text (`"Unknown error N"` for unknown values).
pub fn strerror(code: i32) -> String {
    let mut buf = [0 as libc::c_char; 1024];
    // SAFETY: `buf` is writable for its whole length; the XSI strerror_r
    // always NUL-terminates what it writes.
    unsafe { libc::strerror_r(code, buf.as_mut_ptr(), buf.len()) };
    #[allow(clippy::cast_sign_loss)]
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn check(ret: c_int) -> Result<c_int, i32> {
    if ret < 0 {
        Err(errno())
    } else {
        Ok(ret)
    }
}

/// `NSIG` as PHP computes `num_signals` (`max(NSIG, SIGRTMAX + 1)`).
pub fn num_signals() -> i32 {
    const NSIG: i32 = 65;
    NSIG.max(libc::SIGRTMAX() + 1)
}

/// `SIGRTMIN` (runtime value; glibc reserves the first few).
pub fn sigrtmin() -> i32 {
    libc::SIGRTMIN()
}

/// `SIGRTMAX`.
pub fn sigrtmax() -> i32 {
    libc::SIGRTMAX()
}

// ------------------------------------------------------------------ fork

/// `fork(2)`.  In a multi-threaded host the child may only perform
/// async-signal-safe operations until it execs or exits.
pub fn fork() -> Result<i32, i32> {
    // SAFETY: fork has no memory-safety preconditions; the caller is
    // responsible for what the child does (documented above).
    check(unsafe { libc::fork() })
}

/// `_exit(2)`: terminates immediately without running destructors.
#[cfg(test)]
pub fn exit_now(code: i32) -> ! {
    // SAFETY: always safe to call; never returns.
    unsafe { libc::_exit(code) }
}

/// `kill(2)`.
#[cfg(test)]
pub fn kill(pid: i32, sig: i32) -> Result<(), i32> {
    // SAFETY: no memory-safety preconditions.
    check(unsafe { libc::kill(pid, sig) }).map(|_| ())
}

/// `getpid(2)`.
pub fn getpid() -> i32 {
    // SAFETY: no preconditions.
    unsafe { libc::getpid() }
}

/// `getuid(2)`.
#[cfg(test)]
pub fn getuid() -> u32 {
    // SAFETY: no preconditions.
    unsafe { libc::getuid() }
}

/// `alarm(2)`.
pub fn alarm(seconds: u32) -> u32 {
    // SAFETY: no preconditions.
    unsafe { libc::alarm(seconds) }
}

// ------------------------------------------------------------------ wait

/// The fields of `struct rusage` PHP reports, in PHP's array order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rusage(pub [i64; 17]);

#[allow(clippy::useless_conversion)]
fn rusage_of(r: &libc::rusage) -> Rusage {
    Rusage([
        r.ru_oublock.into(),
        r.ru_inblock.into(),
        r.ru_msgsnd.into(),
        r.ru_msgrcv.into(),
        r.ru_maxrss.into(),
        r.ru_ixrss.into(),
        r.ru_idrss.into(),
        r.ru_minflt.into(),
        r.ru_majflt.into(),
        r.ru_nsignals.into(),
        r.ru_nvcsw.into(),
        r.ru_nivcsw.into(),
        r.ru_nswap.into(),
        r.ru_utime.tv_usec.into(),
        r.ru_utime.tv_sec.into(),
        r.ru_stime.tv_usec.into(),
        r.ru_stime.tv_sec.into(),
    ])
}

fn zeroed_rusage() -> libc::rusage {
    // SAFETY: rusage is plain old data; all-zero is valid.
    unsafe { std::mem::zeroed() }
}

/// `waitpid(2)` / `wait4(2)`: returns the raw result (`-1` → errno) and
/// updates `status` only when the kernel writes it.
pub fn waitpid(
    pid: i32,
    status: &mut c_int,
    options: c_int,
    rusage: bool,
) -> (i32, Option<Rusage>, i32) {
    if rusage {
        let mut ru = zeroed_rusage();
        // SAFETY: `status` and `ru` are valid, writable locations.
        let r = unsafe { libc::wait4(pid, status, options, &raw mut ru) };
        (r, Some(rusage_of(&ru)), errno())
    } else {
        // SAFETY: `status` is a valid, writable location.
        let r = unsafe { libc::waitpid(pid, status, options) };
        (r, None, errno())
    }
}

/// `wait(2)` / `wait3(2)` as `pcntl_wait()` chooses between them.
pub fn wait(status: &mut c_int, options: c_int, rusage: bool) -> (i32, Option<Rusage>, i32) {
    if rusage {
        let mut ru = zeroed_rusage();
        // SAFETY: `status` and `ru` are valid, writable locations.
        let r = unsafe { libc::wait4(-1, status, options, &raw mut ru) };
        (r, Some(rusage_of(&ru)), errno())
    } else if options != 0 {
        // SAFETY: `status` is valid; a null rusage is allowed.
        let r = unsafe { libc::wait4(-1, status, options, std::ptr::null_mut()) };
        (r, None, errno())
    } else {
        // SAFETY: `status` is a valid, writable location.
        let r = unsafe { libc::wait(status) };
        (r, None, errno())
    }
}

/// `waitid(2)` (the raw syscall when a rusage is wanted, as PHP does).
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
pub fn waitid(
    idtype: i64,
    id: i64,
    options: c_int,
    rusage: bool,
) -> Result<(RawSigInfo, Option<Rusage>), i32> {
    // SAFETY: siginfo_t is plain old data; all-zero is valid (PHP memsets).
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    set_errno(0);
    if rusage {
        let mut ru = zeroed_rusage();
        // SAFETY: the kernel's waitid takes (idtype, id, siginfo*, options,
        // rusage*); both pointers are valid and writable.
        let r = unsafe {
            libc::syscall(
                libc::SYS_waitid,
                idtype as c_int,
                id as libc::id_t,
                &raw mut info,
                options,
                &raw mut ru,
            )
        };
        if r == -1 {
            return Err(errno());
        }
        Ok((RawSigInfo::from_siginfo(&info), Some(rusage_of(&ru))))
    } else {
        // SAFETY: `info` is a valid, writable siginfo_t.
        let r = unsafe {
            libc::waitid(
                idtype as libc::idtype_t,
                id as libc::id_t,
                &raw mut info,
                options,
            )
        };
        if r == -1 {
            return Err(errno());
        }
        Ok((RawSigInfo::from_siginfo(&info), None))
    }
}

// ------------------------------------------------------------------ exec

/// `execv(2)`/`execve(2)`; only returns (with the errno) on failure.
pub fn exec(path: &CString, argv: &[CString], envp: Option<&[CString]>) -> i32 {
    let mut av: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    av.push(std::ptr::null());
    match envp {
        Some(env) => {
            let mut ev: Vec<*const libc::c_char> = env.iter().map(|e| e.as_ptr()).collect();
            ev.push(std::ptr::null());
            // SAFETY: path, argv and envp are NUL-terminated strings in
            // NULL-terminated arrays that outlive the call.
            unsafe { libc::execve(path.as_ptr(), av.as_ptr(), ev.as_ptr()) };
        }
        None => {
            // SAFETY: as above.
            unsafe { libc::execv(path.as_ptr(), av.as_ptr()) };
        }
    }
    errno()
}

// --------------------------------------------------------------- signals

/// A `siginfo_t` copied out of the kernel structure (128 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawSigInfo(pub [u64; 16]);

impl RawSigInfo {
    fn from_siginfo(info: &libc::siginfo_t) -> Self {
        let mut words = [0u64; 16];
        let n = std::mem::size_of::<libc::siginfo_t>().min(128);
        // SAFETY: copies `n <= 128` bytes from a valid siginfo_t into a
        // 128-byte buffer; both are plain bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(
                std::ptr::from_ref(info).cast::<u8>(),
                words.as_mut_ptr().cast::<u8>(),
                n,
            );
        }
        Self(words)
    }

    fn bytes(&self) -> [u8; 128] {
        let mut out = [0u8; 128];
        for (i, w) in self.0.iter().enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&w.to_ne_bytes());
        }
        out
    }

    fn int(&self, off: usize) -> i32 {
        let b = self.bytes();
        i32::from_ne_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
    }

    fn long(&self, off: usize) -> i64 {
        let b = self.bytes();
        if std::mem::size_of::<c_long>() == 8 {
            let mut a = [0u8; 8];
            a.copy_from_slice(&b[off..off + 8]);
            i64::from_ne_bytes(a)
        } else {
            i64::from(self.int(off))
        }
    }

    fn union_offset() -> usize {
        if std::mem::size_of::<c_long>() == 8 {
            16
        } else {
            12
        }
    }

    /// `si_signo`.
    pub fn signo(&self) -> i32 {
        self.int(0)
    }
    /// `si_errno`.
    pub fn errno(&self) -> i32 {
        self.int(4)
    }
    /// `si_code`.
    pub fn code(&self) -> i32 {
        self.int(8)
    }
    /// `si_pid`.
    pub fn pid(&self) -> i32 {
        self.int(Self::union_offset())
    }
    /// `si_uid`.
    pub fn uid(&self) -> u32 {
        #[allow(clippy::cast_sign_loss)]
        let uid = self.int(Self::union_offset() + 4) as u32;
        uid
    }
    /// `si_status`.
    pub fn status(&self) -> i32 {
        self.int(Self::union_offset() + 8)
    }
    /// `si_utime`.
    pub fn utime(&self) -> i64 {
        let l = std::mem::size_of::<c_long>();
        self.long(Self::union_offset() + if l == 8 { 16 } else { 12 })
    }
    /// `si_stime`.
    pub fn stime(&self) -> i64 {
        let l = std::mem::size_of::<c_long>();
        self.long(Self::union_offset() + if l == 8 { 24 } else { 16 })
    }
    /// `si_addr` as an integer.
    pub fn addr(&self) -> i64 {
        self.long(Self::union_offset())
    }
    /// `si_band`.
    pub fn band(&self) -> i64 {
        self.long(Self::union_offset())
    }
    /// `si_fd`.
    pub fn fd(&self) -> i32 {
        self.int(Self::union_offset() + std::mem::size_of::<c_long>())
    }
}

/// The handler installed for signals with a PHP callable
/// (`pcntl_signal_handler`): records the signal and returns.
extern "C" fn php_signal_handler(signo: c_int, info: *mut libc::siginfo_t, _ctx: *mut c_void) {
    let raw = if info.is_null() {
        RawSigInfo([0; 16])
    } else {
        // SAFETY: the kernel passes a valid siginfo_t to SA_SIGINFO handlers.
        RawSigInfo::from_siginfo(unsafe { &*info })
    };
    crate::signals::enqueue(signo, &raw);
}

/// What to install for a signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// `SIG_DFL`.
    Default,
    /// `SIG_IGN`.
    Ignore,
    /// The queueing handler (`restart` → `SA_RESTART`).
    Queue { restart: bool },
}

/// `sigaction(2)` as `php_signal4()` + `zend_sigaction()` configure it.
pub fn install(signo: c_int, disposition: Disposition) -> Result<(), i32> {
    // SAFETY: sigaction is plain old data; all-zero is valid.
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    match disposition {
        Disposition::Default => sa.sa_sigaction = libc::SIG_DFL,
        Disposition::Ignore => sa.sa_sigaction = libc::SIG_IGN,
        Disposition::Queue { restart } => {
            sa.sa_sigaction = php_signal_handler as *const () as usize;
            sa.sa_flags = libc::SA_ONSTACK | libc::SA_SIGINFO;
            if restart {
                sa.sa_flags |= libc::SA_RESTART;
            }
            // SAFETY: `sa.sa_mask` is a valid sigset_t.
            unsafe { libc::sigfillset(&raw mut sa.sa_mask) };
        }
    }
    // SAFETY: `sa` is fully initialised; a null old action is allowed.
    check(unsafe { libc::sigaction(signo, &raw const sa, std::ptr::null_mut()) }).map(|_| ())
}

fn sigset(signals: &[i32]) -> Result<libc::sigset_t, i32> {
    // SAFETY: sigset_t is plain old data; sigemptyset initialises it.
    let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: `set` is a valid sigset_t.
    check(unsafe { libc::sigemptyset(&raw mut set) })?;
    for &s in signals {
        set_errno(0);
        // SAFETY: `set` is a valid, initialised sigset_t.
        check(unsafe { libc::sigaddset(&raw mut set, s) })?;
    }
    Ok(set)
}

fn members(set: &libc::sigset_t) -> Vec<i32> {
    (1..num_signals())
        // SAFETY: `set` is a valid, initialised sigset_t.
        .filter(|&s| unsafe { libc::sigismember(set, s) } == 1)
        .collect()
}

/// `sigprocmask(2)` (per-thread in a multi-threaded process); returns the
/// members of the previous mask.
pub fn sigprocmask(how: c_int, signals: &[i32]) -> Result<Vec<i32>, i32> {
    let set = sigset(signals)?;
    let mut old = sigset(&[])?;
    // SAFETY: both sets are valid, initialised sigset_ts.
    let r = unsafe { libc::pthread_sigmask(how, &raw const set, &raw mut old) };
    if r != 0 {
        return Err(r);
    }
    Ok(members(&old))
}

/// The signal mask saved by [`block_all`].
#[derive(Debug, Clone, Copy)]
pub struct SavedMask(libc::sigset_t);

/// Blocks every signal, returning the previous mask.
pub fn block_all() -> SavedMask {
    // SAFETY: sigset_t is plain old data; sigfillset/sigemptyset initialise.
    let mut all: libc::sigset_t = unsafe { std::mem::zeroed() };
    let mut old: libc::sigset_t = all;
    // SAFETY: all pointers refer to valid sigset_ts.
    unsafe {
        libc::sigfillset(&raw mut all);
        libc::sigemptyset(&raw mut old);
        libc::pthread_sigmask(libc::SIG_BLOCK, &raw const all, &raw mut old);
    }
    SavedMask(old)
}

/// Restores a mask saved by [`block_all`].
pub fn restore_mask(saved: &SavedMask) {
    // SAFETY: `saved.0` is a valid sigset_t.
    unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &raw const saved.0, std::ptr::null_mut()) };
}

/// `sigwaitinfo(2)`.
pub fn sigwaitinfo(signals: &[i32]) -> Result<(i32, RawSigInfo), i32> {
    let set = sigset(signals)?;
    // SAFETY: siginfo_t is plain old data; all-zero is valid.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    set_errno(0);
    // SAFETY: `set` and `info` are valid.
    let r = check(unsafe { libc::sigwaitinfo(&raw const set, &raw mut info) })?;
    Ok((r, RawSigInfo::from_siginfo(&info)))
}

/// `sigtimedwait(2)`.
pub fn sigtimedwait(signals: &[i32], sec: i64, nsec: i64) -> Result<(i32, RawSigInfo), i32> {
    let set = sigset(signals)?;
    // SAFETY: siginfo_t is plain old data; all-zero is valid.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    #[allow(clippy::cast_possible_truncation, clippy::useless_conversion)]
    let ts = libc::timespec {
        tv_sec: sec as libc::time_t,
        tv_nsec: nsec as _,
    };
    set_errno(0);
    // SAFETY: `set`, `info` and `ts` are valid.
    let r = check(unsafe { libc::sigtimedwait(&raw const set, &raw mut info, &raw const ts) })?;
    Ok((r, RawSigInfo::from_siginfo(&info)))
}

/// `raise(3)` (tests).
#[cfg(test)]
pub fn raise(sig: i32) {
    // SAFETY: no memory-safety preconditions.
    unsafe { libc::raise(sig) };
}

// -------------------------------------------------------------- priority

/// `getpriority(2)`; errno is cleared first since any value is valid.
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
pub fn getpriority(which: i64, who: i64) -> Result<i32, i32> {
    set_errno(0);
    // SAFETY: no memory-safety preconditions.
    let r = unsafe { libc::getpriority(which as libc::__priority_which_t, who as libc::id_t) };
    match errno() {
        0 => Ok(r),
        e => Err(e),
    }
}

/// `setpriority(2)`.
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
pub fn setpriority(which: i64, who: i64, prio: i64) -> Result<(), i32> {
    // SAFETY: no memory-safety preconditions.
    check(unsafe {
        libc::setpriority(
            which as libc::__priority_which_t,
            who as libc::id_t,
            prio as c_int,
        )
    })
    .map(|_| ())
}

// ------------------------------------------------------------ namespaces

/// `unshare(2)`.
#[allow(clippy::cast_possible_truncation)]
pub fn unshare(flags: i64) -> Result<(), i32> {
    // SAFETY: no memory-safety preconditions.
    check(unsafe { libc::unshare(flags as c_int) }).map(|_| ())
}

/// `pidfd_open(2)`.
#[allow(clippy::cast_possible_truncation)]
pub fn pidfd_open(pid: i64) -> Result<c_int, i32> {
    // SAFETY: the syscall takes (pid, flags) and returns an fd or -1.
    let r = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    if r < 0 {
        Err(errno())
    } else {
        Ok(r as c_int)
    }
}

/// `setns(2)` then `close(fd)`; the errno of `setns` is preserved.
#[allow(clippy::cast_possible_truncation)]
pub fn setns_and_close(fd: c_int, nstype: i64) -> Result<(), i32> {
    // SAFETY: `fd` is an fd we own; setns has no memory preconditions.
    let r = unsafe { libc::setns(fd, nstype as c_int) };
    let e = errno();
    // SAFETY: closing the pidfd we opened.
    unsafe { libc::close(fd) };
    if r == -1 {
        Err(e)
    } else {
        Ok(())
    }
}

// --------------------------------------------------------------- affinity

/// `sysconf(_SC_NPROCESSORS_CONF)`.
pub fn nprocessors_conf() -> i64 {
    // SAFETY: no preconditions.
    unsafe { libc::sysconf(libc::_SC_NPROCESSORS_CONF) }
}

const CPU_SETSIZE: usize = 1024;

/// `sched_getaffinity(2)`; CPU ids below `maxcpus` that are set.
#[allow(clippy::cast_possible_truncation)]
pub fn getaffinity(pid: i64, maxcpus: usize) -> Result<Vec<usize>, i32> {
    // SAFETY: cpu_set_t is plain old data; all-zero is the empty set.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    // SAFETY: `set` is a valid cpu_set_t of the size passed.
    check(unsafe {
        libc::sched_getaffinity(
            pid as libc::pid_t,
            std::mem::size_of::<libc::cpu_set_t>(),
            &raw mut set,
        )
    })?;
    Ok((0..maxcpus.min(CPU_SETSIZE))
        // SAFETY: `i < CPU_SETSIZE`, within the set.
        .filter(|&i| unsafe { libc::CPU_ISSET(i, &set) })
        .collect())
}

/// `sched_setaffinity(2)` with the given CPU ids (ids beyond the fixed
/// `cpu_set_t` capacity are ignored).
#[allow(clippy::cast_possible_truncation)]
pub fn setaffinity(pid: i64, cpus: &[usize]) -> Result<(), i32> {
    // SAFETY: cpu_set_t is plain old data; all-zero is the empty set.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    for &c in cpus.iter().filter(|&&c| c < CPU_SETSIZE) {
        // SAFETY: `c < CPU_SETSIZE`, within the set.
        unsafe { libc::CPU_SET(c, &mut set) };
    }
    // SAFETY: `set` is a valid cpu_set_t of the size passed.
    check(unsafe {
        libc::sched_setaffinity(
            pid as libc::pid_t,
            std::mem::size_of::<libc::cpu_set_t>(),
            &raw const set,
        )
    })
    .map(|_| ())
}

/// `sched_getcpu(3)`.
pub fn getcpu() -> i32 {
    // SAFETY: no preconditions.
    unsafe { libc::sched_getcpu() }
}

/// `pause(2)` (tests: a child that waits to be killed).
#[cfg(test)]
pub fn pause() {
    // SAFETY: no preconditions.
    unsafe { libc::pause() };
}

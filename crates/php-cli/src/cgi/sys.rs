// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The operating-system calls of `FastCGI` mode that the standard library
//! has no safe form for (ref: `main/fastcgi.c` `fcgi_init()`,
//! `fcgi_setup_signals()`, `php_pollfd_for_ms()`; `sapi/cgi/cgi_main.c`
//! the `PHP_FCGI_CHILDREN` pre-fork loop and `fastcgi_cleanup()`).
//!
//! This is the FFI module of `php-cgi`: every `unsafe` block below is a
//! single libc call on plain integers or on memory owned by the caller.

use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// `in_shutdown`: set by SIGTERM/SIGUSR1 (`fcgi_signal_handler`).
static IN_SHUTDOWN: AtomicBool = AtomicBool::new(false);
/// `pgroup` of the pre-forking parent.
static PGROUP: AtomicI32 = AtomicI32::new(0);
/// `parent && parent_waiting`: the parent is waiting for its children.
static PARENT_WAITING: AtomicBool = AtomicBool::new(false);
/// `exit_signal`: the waiting parent was told to stop.
static EXIT_SIGNAL: AtomicBool = AtomicBool::new(false);

/// `fcgi_in_shutdown()`.
pub fn in_shutdown() -> bool {
    IN_SHUTDOWN.load(Ordering::SeqCst)
}

/// `fcgi_terminate()`: the request loop stops after the current request
/// (FPM's SIGQUIT handler calls it; one atomic store, async-signal-safe).
#[allow(dead_code)] // php-fpm's
pub fn request_shutdown() {
    IN_SHUTDOWN.store(true, Ordering::SeqCst);
}

/// The last OS error number (`errno`).
fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// `fcgi_init()` on Unix: standard input is a socket that is not
/// connected (`getpeername()` fails with `ENOTCONN`), i.e. a listening
/// socket a web server passed to us (`mod_fcgid`, spawn-fcgi).
pub fn stdin_is_listening_socket() -> bool {
    // SAFETY: `sockaddr_storage` is plain old data; zeroed is a valid
    // value, and getpeername writes at most `len` bytes into it.
    let mut sa: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut len = libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_storage>())
        .unwrap_or(libc::socklen_t::MAX);
    // SAFETY: `sa` and `len` are valid for writes for the whole call.
    let r = unsafe {
        libc::getpeername(
            0,
            std::ptr::addr_of_mut!(sa).cast::<libc::sockaddr>(),
            &raw mut len,
        )
    };
    r != 0 && errno() == libc::ENOTCONN
}

/// The address family of the socket on standard input.
fn stdin_family() -> i32 {
    // SAFETY: as in `stdin_is_listening_socket`.
    let mut sa: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut len = libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_storage>())
        .unwrap_or(libc::socklen_t::MAX);
    // SAFETY: `sa` and `len` are valid for writes for the whole call.
    let r = unsafe {
        libc::getsockname(
            0,
            std::ptr::addr_of_mut!(sa).cast::<libc::sockaddr>(),
            &raw mut len,
        )
    };
    if r == 0 {
        i32::from(sa.ss_family)
    } else {
        -1
    }
}

/// The listening socket on standard input (checked with
/// [`stdin_is_listening_socket`]).
pub fn stdin_listener() -> crate::fastcgi::Listener {
    if stdin_family() == libc::AF_UNIX {
        // SAFETY: descriptor 0 is a listening Unix socket that nothing
        // else in the process uses; the listener takes ownership of it.
        crate::fastcgi::Listener::Unix(unsafe { UnixListener::from_raw_fd(0) })
    } else {
        // SAFETY: descriptor 0 is a listening TCP socket that nothing
        // else in the process uses; the listener takes ownership of it.
        crate::fastcgi::Listener::Tcp(unsafe { std::net::TcpListener::from_raw_fd(0) })
    }
}

/// What [`poll_in`] saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Poll {
    /// Readable (`POLLIN`).
    Readable,
    /// Something other than data (hang-up, error) or the timeout.
    NotReadable,
    /// A signal arrived (`EINTR`).
    Interrupted,
}

/// `php_pollfd_for_ms(fd, POLLIN, timeout_ms)` (`-1` waits forever).
pub fn poll_in(fd: RawFd, timeout_ms: i32) -> Poll {
    let mut p = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid `pollfd` that lives for the whole call.
    let r = unsafe { libc::poll(&raw mut p, 1, timeout_ms) };
    if r < 0 {
        return if errno() == libc::EINTR {
            Poll::Interrupted
        } else {
            Poll::NotReadable
        };
    }
    if r > 0 && p.revents & libc::POLLIN != 0 {
        Poll::Readable
    } else {
        Poll::NotReadable
    }
}

extern "C" fn shutdown_handler(signo: libc::c_int) {
    if signo == libc::SIGUSR1 || signo == libc::SIGTERM {
        IN_SHUTDOWN.store(true, Ordering::SeqCst);
    }
}

/// Installs `handler` for `signo` without `SA_RESTART`, so that a
/// blocking call is interrupted (`sigaction` with `sa_flags = 0`).
fn set_handler(signo: libc::c_int, handler: extern "C" fn(libc::c_int)) {
    // SAFETY: `sigaction` is plain old data; zeroed is a valid value and
    // the fields that matter are set below.
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    sa.sa_sigaction = handler as usize;
    sa.sa_flags = 0;
    // SAFETY: `sa.sa_mask` is valid for writes; the handler only touches
    // atomics and calls async-signal-safe functions.
    unsafe {
        libc::sigemptyset(&raw mut sa.sa_mask);
        libc::sigaction(signo, &raw const sa, std::ptr::null_mut());
    }
}

/// The signals php-cgi acts on: the shutdown requests
/// (`fcgi_signal_handler`, `fastcgi_cleanup`) and the children's exits.
const PROCESS_SIGNALS: [libc::c_int; 5] = [
    libc::SIGTERM,
    libc::SIGUSR1,
    libc::SIGINT,
    libc::SIGQUIT,
    libc::SIGCHLD,
];

/// Blocks (or unblocks) [`PROCESS_SIGNALS`] in the calling thread. The
/// main thread blocks them before it starts the VM thread, which unblocks
/// them: a signal sent to the process then reaches the thread waiting in
/// `accept()` or `wait()` and interrupts it, as in the single-threaded C
/// program (delivered to the main thread instead, the handler ran but the
/// server went on waiting for a connection).
pub fn mask_process_signals(block: bool) {
    // SAFETY: `sigset_t` is plain old data, initialised by `sigemptyset`
    // before use; `pthread_sigmask` changes only the calling thread's mask.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut set);
        for signo in PROCESS_SIGNALS {
            libc::sigaddset(&raw mut set, signo);
        }
        let how = if block {
            libc::SIG_BLOCK
        } else {
            libc::SIG_UNBLOCK
        };
        libc::pthread_sigmask(how, &raw const set, std::ptr::null_mut());
    }
}

/// `fcgi_setup_signals()`: SIGTERM and SIGUSR1 ask for a shutdown after
/// the current request (SIGPIPE is already ignored by `main`).
pub fn setup_signals() {
    set_handler(libc::SIGUSR1, shutdown_handler);
    set_handler(libc::SIGTERM, shutdown_handler);
}

extern "C" fn cleanup_handler(_signo: libc::c_int) {
    // `sigaction(SIGTERM, &old_term, 0)`: the handler before this one
    // was `fcgi_signal_handler`.
    set_handler(libc::SIGTERM, shutdown_handler);
    // SAFETY: kill(2) is async-signal-safe; the group is our own.
    unsafe {
        libc::kill(-PGROUP.load(Ordering::SeqCst), libc::SIGTERM);
    }
    if PARENT_WAITING.load(Ordering::SeqCst) {
        EXIT_SIGNAL.store(true, Ordering::SeqCst);
    } else {
        // SAFETY: _exit(2) is async-signal-safe and ends the process.
        unsafe { libc::_exit(0) }
    }
}

/// Who returns from [`prefork`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// A child: serve requests.
    Child,
    /// The parent after a shutdown signal (`goto parent_out`).
    Parent,
}

/// The `PHP_FCGI_CHILDREN` loop of `main()`: a new process group, then
/// `children` children that serve requests, each replaced when it exits,
/// until SIGTERM, SIGINT or SIGQUIT (`fastcgi_cleanup`), which also stops
/// the children.
pub fn prefork(children: u32) -> Role {
    // SAFETY: setsid/getpgrp take no arguments; setsid fails harmlessly
    // when we already lead a process group.
    unsafe {
        libc::setsid();
        PGROUP.store(libc::getpgrp(), Ordering::SeqCst);
    }
    for signo in [libc::SIGTERM, libc::SIGINT, libc::SIGQUIT] {
        set_handler(signo, cleanup_handler);
    }
    if in_shutdown() {
        return Role::Parent;
    }
    let mut running = 0u32;
    loop {
        while running < children {
            // SAFETY: fork(2); the child continues on this (the only)
            // thread with copies of this thread's state.
            match unsafe { libc::fork() } {
                0 => {
                    // "don't catch our signals": back to the FastCGI
                    // handlers (SIGINT and SIGQUIT to their defaults).
                    setup_signals();
                    // SAFETY: plain signal(2) calls restoring defaults.
                    unsafe {
                        libc::signal(libc::SIGINT, libc::SIG_DFL);
                        libc::signal(libc::SIGQUIT, libc::SIG_DFL);
                    }
                    php_vm::ext::php_child_init();
                    return Role::Child;
                }
                -1 => {
                    let e = std::io::Error::last_os_error();
                    eprintln!("php (pre-forking): {}", strerror(&e));
                    std::process::exit(1);
                }
                _ => running += 1,
            }
        }
        PARENT_WAITING.store(true, Ordering::SeqCst);
        loop {
            let mut status = 0;
            // SAFETY: wait(2) writes the status into a local integer.
            if unsafe { libc::wait(&raw mut status) } >= 0 {
                running -= 1;
                break;
            } else if EXIT_SIGNAL.load(Ordering::SeqCst) {
                return Role::Parent;
            }
        }
    }
}

/// `strerror(errno)` of `e` (without Rust's " (os error N)" suffix).
pub fn strerror(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

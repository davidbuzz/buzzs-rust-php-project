//! Process-level glue the `php` host needs for pcntl semantics.
//!
//! The CLI runs the VM on a spawned thread (for a large stack) while the
//! main thread waits in `join()`.  That breaks two pieces of PHP's
//! single-threaded process model, which this module restores:
//!
//! * **Signal routing.** The kernel delivers a process-directed signal to
//!   any thread that does not block it, preferring the main thread.  PHP
//!   code blocking a signal with `pcntl_sigprocmask()` (to collect it with
//!   `pcntl_sigwaitinfo()`) would then see it handled — or the process
//!   killed — on the idle main thread.  [`route_signals_to_vm_thread`]
//!   blocks every signal on the main thread, so they all reach the VM
//!   thread.
//! * **Exit status of forked children.** After `fork()` the child consists
//!   of the VM thread alone; when that thread's closure returns, the last
//!   thread exits and the process ends with status 0 instead of the
//!   script's exit status.  [`ForkedChild`] terminates the child with the
//!   recorded status from a thread-local destructor, after the VM (and
//!   its buffered standard output) has been dropped.

use std::cell::Cell;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;
use std::time::{Duration, Instant};

/// Set by [`block_all_on_return`] once it ran on the main thread.
static MAIN_BLOCKED: AtomicBool = AtomicBool::new(false);
static ROUTE_ONCE: Once = Once::new();

/// The kernel thread id of the calling thread.
fn gettid() -> libc::pid_t {
    // SAFETY: gettid(2) has no preconditions.
    unsafe { libc::gettid() }
}

/// Signal handler run on the main thread: makes the mask restored by
/// `sigreturn` (the one saved in the `ucontext`) block every signal that
/// can be blocked, except the synchronous fault signals.
extern "C" fn block_all_on_return(
    _sig: libc::c_int,
    _info: *mut libc::siginfo_t,
    ctx: *mut libc::c_void,
) {
    let uc = ctx.cast::<libc::ucontext_t>();
    if !uc.is_null() {
        // SAFETY: with SA_SIGINFO the third handler argument points to the
        // interrupted context, writable by the handler; `sigfillset` and
        // `sigdelset` are async-signal-safe (POSIX) and only touch that
        // memory.  glibc's `sigfillset` leaves out its internal
        // SIGCANCEL/SIGSETXID, so setuid() broadcasts keep working.
        unsafe {
            let mask = &raw mut (*uc).uc_sigmask;
            libc::sigfillset(mask);
            for s in [
                libc::SIGSEGV,
                libc::SIGBUS,
                libc::SIGFPE,
                libc::SIGILL,
                libc::SIGTRAP,
                libc::SIGABRT,
            ] {
                libc::sigdelset(mask, s);
            }
        }
    }
    MAIN_BLOCKED.store(true, Ordering::SeqCst);
}

/// Blocks all signals on the process's main thread when the VM runs on
/// another thread (once per process).  The main thread cannot be asked to
/// change its own mask, so it is sent a private real-time signal whose
/// handler edits the mask restored when the handler returns.
pub(crate) fn route_signals_to_vm_thread() {
    // SAFETY: getpid(2) has no preconditions.
    let pid = unsafe { libc::getpid() };
    if gettid() == pid {
        return; // The VM is the main thread (or a forked child).
    }
    ROUTE_ONCE.call_once(|| {
        let sig = libc::SIGRTMAX();
        // SAFETY: plain sigaction(2)/tgkill(2) calls on zero-initialised
        // structures; the previous disposition is restored afterwards.
        unsafe {
            let mut act: libc::sigaction = std::mem::zeroed();
            let mut old: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = block_all_on_return as *const () as libc::sighandler_t;
            act.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
            libc::sigemptyset(&raw mut act.sa_mask);
            if libc::sigaction(sig, &raw const act, &raw mut old) != 0 {
                return;
            }
            if libc::syscall(libc::SYS_tgkill, pid, pid, sig) == 0 {
                let start = Instant::now();
                while !MAIN_BLOCKED.load(Ordering::SeqCst)
                    && start.elapsed() < Duration::from_secs(1)
                {
                    std::thread::sleep(Duration::from_micros(50));
                }
            }
            libc::sigaction(sig, &raw const old, std::ptr::null_mut());
        }
    });
}

thread_local! {
    /// Present (with the exit status to use) in a forked child.
    static CHILD: ForkedChild = const { ForkedChild { status: Cell::new(None) } };
}

/// Marker living in the VM thread of a forked child.
pub(crate) struct ForkedChild {
    status: Cell<Option<i32>>,
}

impl ForkedChild {
    /// Called in the child right after `fork()`: arms the destructor.
    pub(crate) fn arm() {
        CHILD.with(|c| c.status.set(Some(255)));
    }

    /// Records the script's exit status (at request shutdown).
    pub(crate) fn record(status: i32) {
        CHILD.with(|c| {
            if c.status.get().is_some() {
                c.status.set(Some(status));
            }
        });
    }
}

impl Drop for ForkedChild {
    fn drop(&mut self) {
        if let Some(status) = self.status.get() {
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
            // SAFETY: `_exit` ends the process immediately; it is what the
            // child's process would do on returning from `main`, minus the
            // atexit handlers of the parent's runtime it inherited.
            unsafe { libc::_exit(status) };
        }
    }
}

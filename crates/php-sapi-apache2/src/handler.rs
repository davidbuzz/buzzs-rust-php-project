//! The content handler on the httpd side (`php_handler()`): request
//! filtering, the per-process request lock, the request thread, the
//! `max_execution_time` watchdog and the error-log capture.
//!
//! # The watchdog
//! php-src arms `ITIMER_PROF` and its `SIGPROF` handler flags the timeout
//! for the executor.  Here the httpd thread that waits for the request
//! thread polls that thread's CPU clock instead ([`watchdog`]); on expiry
//! it flags the timeout through [`php_vm::interrupt::sapi_timer_expired`]
//! and the executor raises the "Maximum execution time" fatal at its next
//! safe point, exactly as the signal handler would.  Only `hard_timeout`
//! (a second expiry finding the first unhandled: a native call that does
//! not return) ends the child process, as `zend_timeout_handler` does.
//!
//! # Threading
//! The VM is single-threaded (`Rc` everywhere, thread-local state in the
//! extension crates), so every PHP request runs on a fresh thread (fresh
//! thread-locals, 256 MiB stack like the CLI) while the httpd worker
//! thread waits for it.  Requests of one process are serialised by a
//! process-wide mutex: under `mpm_prefork` (one request per process) this
//! costs nothing; under `mpm_worker`/`mpm_event` — only allowed with
//! `PhpRustAllowThreads On` — PHP requests of a child run one at a time,
//! so throughput is bounded by the number of child processes.

use crate::backend::{Backend, IntField, LogLevel, StrField};
use crate::request::{Expiry, RunParams, Timer};
use std::cell::Cell;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

/// httpd's `OK`.
pub const OK: i32 = 0;
/// httpd's `DECLINED`.
pub const DECLINED: i32 = -1;

const PHP_MAGIC_TYPE: &[u8] = b"application/x-httpd-php";
const PHP_SOURCE_MAGIC_TYPE: &[u8] = b"application/x-httpd-php-source";
const PHP_SCRIPT: &[u8] = b"php-script";

/// Stack size of request threads.
const STACK_SIZE: usize = 256 << 20;

static REQUEST_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    /// Set on request threads: a handler invocation there is a
    /// sub-request issued by `virtual()`.
    static IN_REQUEST: Cell<bool> = const { Cell::new(false) };
}

/// `child_init` hook.
pub fn child_init() {
    // Nothing is shared between requests; make sure the configuration is
    // materialised before the first request.  php-memlimit's request heap
    // is not bound here, unlike php-cli's single request thread: every
    // request runs on a thread of its own, which takes the heap for the
    // request (`request::run`); an httpd thread owning it would leave
    // every request on the inner allocator.
    let _ = crate::config::current();
}

/// A factory creating the request's backend on the request thread.
pub type BackendFactory = Box<dyn FnOnce() -> Option<Box<dyn Backend>> + Send>;

/// `php_handler()`.
pub fn handle(r: &dyn Backend, make: BackendFactory) -> i32 {
    let handler = r.str_field(StrField::Handler).unwrap_or_default();
    let is_php =
        handler == PHP_MAGIC_TYPE || handler == PHP_SOURCE_MAGIC_TYPE || handler == PHP_SCRIPT;
    let cfg = crate::config::current();
    let dir = r.dir_config();
    let xbithack = cfg.effective_bool("xbithack", &dir, false)
        && handler == b"text/html"
        && r.int_field(IntField::UserExec) != 0;
    if !(is_php || xbithack) {
        return DECLINED;
    }
    if r.int_field(IntField::PathInfoRejected) != 0 {
        return 404;
    }
    if !cfg.effective_bool("engine", &dir, true) {
        return DECLINED;
    }
    let filename = r.str_field(StrField::Filename).unwrap_or_default();
    match r.int_field(IntField::FileType) {
        0 => {
            log_with_file(r, "script '", &filename, "' not found or unable to stat");
            return 404;
        }
        2 => {
            log_with_file(r, "attempt to invoke directory '", &filename, "' as script");
            return 403;
        }
        _ => {}
    }
    if r.int_field(IntField::IsMain) != 0 || r.int_field(IntField::EnvDiffers) != 0 {
        r.add_cgi_vars();
    }
    let params = RunParams {
        config: cfg,
        timer: Arc::new(Timer::default()),
        highlight: handler.starts_with(PHP_SOURCE_MAGIC_TYPE),
    };
    if IN_REQUEST.with(Cell::get) {
        // virtual(): a nested request on the parent's request thread.
        return run_thread(make, params, None);
    }
    let _guard = REQUEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let capture = StderrCapture::start();
    let status = run_thread(make, params, Some((r, capture.as_ref())));
    if let Some(c) = capture {
        forward_log(r, &c.finish());
    }
    status
}

fn log_with_file(r: &dyn Backend, pre: &str, file: &[u8], post: &str) {
    let mut msg = pre.as_bytes().to_vec();
    msg.extend_from_slice(file);
    msg.extend_from_slice(post.as_bytes());
    r.log(LogLevel::Err, &msg);
}

/// Runs a request on its own thread with the `max_execution_time`
/// watchdog, as [`handle`] does once its checks passed; `r` receives the
/// log entries.  Public for the SAPI's own tests.
pub fn run_watched(r: &dyn Backend, make: BackendFactory, params: RunParams) -> i32 {
    run_thread(make, params, Some((r, None)))
}

/// Runs the request on a new thread and waits for it, enforcing
/// `max_execution_time` when `watch` is given.
fn run_thread(
    make: BackendFactory,
    params: RunParams,
    watch: Option<(&dyn Backend, Option<&StderrCapture>)>,
) -> i32 {
    let timer = params.timer.clone();
    let (tx, rx) = mpsc::channel::<()>();
    let spawned = std::thread::Builder::new()
        .name("php-request".into())
        .stack_size(STACK_SIZE)
        .spawn(move || {
            IN_REQUEST.with(|c| c.set(true));
            if let Some(backend) = make() {
                crate::request::run(backend, &params);
            }
            let _ = tx.send(());
        });
    let Ok(handle) = spawned else {
        return 500;
    };
    if let Some((r, capture)) = watch {
        if let Some(clock) = cpu_clock(&handle) {
            let mut sink = EngineTimeout { r, capture };
            watchdog(&timer, &rx, &mut || clock_ns(clock), &mut sink);
        }
    }
    match handle.join() {
        Ok(()) => OK,
        Err(_) => 500,
    }
}

/// Where the watchdog reports: the engine's interrupt flags in production
/// ([`EngineTimeout`]), a recorder in tests.
pub(crate) trait TimeoutSink {
    /// `zend_timeout_handler()`: flags the timeout for the executor.
    fn expired(&mut self);
    /// Whether the flagged timeout is still unhandled (`EG(timed_out)`).
    fn pending(&self) -> bool;
    /// The hard timeout: ends the process (`_exit(124)`).
    fn kill(&mut self, secs: i64, hard: i64);
}

/// The production [`TimeoutSink`]: php-vm's interrupt module.
struct EngineTimeout<'a> {
    r: &'a dyn Backend,
    capture: Option<&'a StderrCapture>,
}

impl TimeoutSink for EngineTimeout<'_> {
    fn expired(&mut self) {
        php_vm::interrupt::sapi_timer_expired();
    }

    fn pending(&self) -> bool {
        php_vm::interrupt::timeout_pending()
    }

    fn kill(&mut self, secs: i64, hard: i64) {
        hard_timeout(self.r, self.capture, secs, hard);
    }
}

/// The watchdog loop: polls the request thread's CPU clock (`now`) every
/// 50 ms until the request ends (`done` receives, or its sender is gone).
/// A soft expiry is reported to the engine; a hard expiry that finds the
/// soft one still unhandled kills the process (after which the loop ends,
/// for a test sink that returns).
pub(crate) fn watchdog(
    timer: &Timer,
    done: &mpsc::Receiver<()>,
    now: &mut dyn FnMut() -> i64,
    sink: &mut dyn TimeoutSink,
) {
    loop {
        match done.recv_timeout(Duration::from_millis(50)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => match timer.check(now()) {
                Expiry::None => {}
                Expiry::Soft => sink.expired(),
                Expiry::Hard => {
                    if sink.pending() {
                        sink.kill(timer.limit(), timer.hard_limit());
                        return;
                    }
                }
            },
        }
    }
}

/// The CPU-time clock of a thread.
fn cpu_clock(handle: &std::thread::JoinHandle<()>) -> Option<libc::clockid_t> {
    use std::os::unix::thread::JoinHandleExt;
    let mut clock: libc::clockid_t = 0;
    // SAFETY: the thread is alive (joined later), the out-pointer is ours.
    let rc = unsafe { libc::pthread_getcpuclockid(handle.as_pthread_t(), &raw mut clock) };
    (rc == 0).then_some(clock)
}

fn clock_ns(clock: libc::clockid_t) -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes into the timespec we own.
    unsafe {
        libc::clock_gettime(clock, &raw mut ts);
    }
    ts.tv_sec
        .saturating_mul(1_000_000_000)
        .saturating_add(ts.tv_nsec)
}

/// `hard_timeout` expired while the executor had not reached a safe point
/// since the `max_execution_time` fatal was flagged (a native call that
/// does not return).  `zend_timeout_handler` writes its message to fd 2 —
/// Apache's error log — and ends the process with `_exit(124)`; httpd
/// starts a replacement child and the client sees the response cut off.
/// The executing file and line belong to the request thread, so they are
/// reported as `Unknown` / 0, as php-src does when nothing executes.
fn hard_timeout(r: &dyn Backend, capture: Option<&StderrCapture>, secs: i64, hard: i64) -> ! {
    use std::io::Write;
    if let Some(c) = capture {
        c.restore();
        forward_log(r, &c.snapshot());
    }
    let msg = format!(
        "\nFatal error: Maximum execution time of {secs}+{hard} seconds exceeded (terminated) in Unknown on line 0\n"
    );
    let _ = std::io::stderr().write_all(msg.as_bytes());
    // SAFETY: terminating the process without running destructors is the
    // intent: other threads may hold locks and the request_rec.
    unsafe { libc::_exit(124) }
}

/// Forwards captured stderr once stderr is restored: lines httpd itself
/// logged during the request (the main server's error log *is* stderr)
/// are written back verbatim; the rest (the VM's error log when
/// `error_log` is unset) becomes `ap_log_rerror` entries, one per PHP
/// message, at the level `php_error_cb` would use.
fn forward_log(r: &dyn Backend, data: &[u8]) {
    use std::io::Write;
    let mut pending: Vec<u8> = Vec::new();
    let flush_pending = |pending: &mut Vec<u8>| {
        for msg in split_log_messages(pending) {
            r.log(log_level_of(&msg), &msg);
        }
        pending.clear();
    };
    for line in data.split_inclusive(|&c| c == b'\n') {
        if is_httpd_log_line(line) {
            flush_pending(&mut pending);
            let mut line = line.to_vec();
            if line.last() != Some(&b'\n') {
                line.push(b'\n');
            }
            let _ = std::io::stderr().write_all(&line);
        } else {
            pending.extend_from_slice(line);
        }
    }
    flush_pending(&mut pending);
}

/// Whether a captured line was written by httpd's logger
/// (`[date] [module:level] ...`).
fn is_httpd_log_line(line: &[u8]) -> bool {
    line.first() == Some(&b'[') && line.windows(3).take(64).any(|w| w == b"] [")
}

/// The httpd level `php_error_cb` uses for a logged PHP message.
pub fn log_level_of(msg: &[u8]) -> LogLevel {
    const LEVELS: &[(&[u8], LogLevel)] = &[
        (b"PHP Fatal error:", LogLevel::Err),
        (b"PHP Recoverable fatal error:", LogLevel::Err),
        (b"PHP Parse error:", LogLevel::Err),
        (b"PHP Warning:", LogLevel::Warning),
        (b"PHP Notice:", LogLevel::Notice),
        (b"PHP Deprecated:", LogLevel::Info),
    ];
    LEVELS
        .iter()
        .find(|(p, _)| msg.starts_with(p))
        .map_or(LogLevel::Err, |(_, l)| *l)
}

/// Splits captured stderr into log messages.
pub fn split_log_messages(data: &[u8]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    for line in data.split(|&c| c == b'\n') {
        if line.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(prev) if !line.starts_with(b"PHP ") => {
                prev.push(b'\n');
                prev.extend_from_slice(line);
            }
            _ => out.push(line.to_vec()),
        }
    }
    out
}

/// Redirects the process's stderr into a memory file for the duration of
/// a request.
#[derive(Debug)]
struct StderrCapture {
    saved: libc::c_int,
    memfd: libc::c_int,
    forwarded: std::sync::atomic::AtomicI64,
}

impl StderrCapture {
    fn start() -> Option<Self> {
        // SAFETY: plain fd syscalls on descriptors we own; on failure
        // everything opened so far is closed again.
        unsafe {
            let memfd = libc::memfd_create(c"phprust-stderr".as_ptr(), libc::MFD_CLOEXEC);
            if memfd < 0 {
                return None;
            }
            let saved = libc::dup(2);
            if saved < 0 {
                libc::close(memfd);
                return None;
            }
            if libc::dup2(memfd, 2) < 0 {
                libc::close(saved);
                libc::close(memfd);
                return None;
            }
            Some(StderrCapture {
                saved,
                memfd,
                forwarded: std::sync::atomic::AtomicI64::new(0),
            })
        }
    }

    /// The bytes written so far and not yet returned by a snapshot.
    fn snapshot(&self) -> Vec<u8> {
        use std::sync::atomic::Ordering;
        let from = self.forwarded.load(Ordering::SeqCst);
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];
        let mut off = from;
        loop {
            // SAFETY: pread into our buffer from our memfd.
            let n = unsafe { libc::pread(self.memfd, buf.as_mut_ptr().cast(), buf.len(), off) };
            let Ok(n) = usize::try_from(n) else {
                break;
            };
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            off += i64::try_from(n).unwrap_or(0);
        }
        self.forwarded.store(off, Ordering::SeqCst);
        out
    }

    /// Points stderr back at the original descriptor.
    fn restore(&self) {
        // SAFETY: `saved` is a descriptor we own (dup of the original 2).
        unsafe {
            libc::dup2(self.saved, 2);
        }
    }

    /// Restores stderr and returns the captured bytes.
    fn finish(self) -> Vec<u8> {
        self.restore();
        let data = self.snapshot();
        // SAFETY: closing the descriptors we own.
        unsafe {
            libc::close(self.saved);
            libc::close(self.memfd);
        }
        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::MockBackend;

    #[test]
    fn declines_foreign_handlers() {
        let m = MockBackend::default().with_str(StrField::Handler, b"text/plain");
        assert_eq!(handle(&m, Box::new(|| None)), DECLINED);
    }

    #[test]
    fn missing_scripts_are_404() {
        let m = MockBackend::default()
            .with_str(StrField::Handler, b"application/x-httpd-php")
            .with_str(StrField::Filename, b"/nope.php");
        assert_eq!(handle(&m, Box::new(|| None)), 404);
        assert_eq!(
            m.logs.borrow()[0].1,
            b"script '/nope.php' not found or unable to stat"
        );
        let m = MockBackend::default()
            .with_str(StrField::Handler, b"php-script")
            .with_str(StrField::Filename, b"/dir")
            .with_int(IntField::FileType, 2);
        assert_eq!(handle(&m, Box::new(|| None)), 403);
        let m = MockBackend::default()
            .with_str(StrField::Handler, b"php-script")
            .with_int(IntField::PathInfoRejected, 1);
        assert_eq!(handle(&m, Box::new(|| None)), 404);
    }

    #[test]
    fn engine_off_declines() {
        let m = MockBackend::default()
            .with_str(StrField::Handler, b"php-script")
            .with_int(IntField::FileType, 1);
        m.dir.borrow_mut().push(crate::backend::DirEntry {
            name: "engine".into(),
            value: b"0".to_vec(),
            status: crate::backend::DirStatus::PerDir,
            htaccess: true,
        });
        assert_eq!(handle(&m, Box::new(|| None)), DECLINED);
    }

    #[test]
    fn httpd_lines_are_recognised() {
        assert!(is_httpd_log_line(
            b"[Tue Nov 03 20:03:58.704347 2026] [php:notice] [pid 1] x\n"
        ));
        assert!(!is_httpd_log_line(b"PHP Warning:  x"));
        assert!(!is_httpd_log_line(b"[just brackets]"));
    }

    #[test]
    fn log_levels() {
        assert_eq!(log_level_of(b"PHP Warning:  x"), LogLevel::Warning);
        assert_eq!(log_level_of(b"PHP Deprecated:  x"), LogLevel::Info);
        assert_eq!(log_level_of(b"PHP Fatal error:  x"), LogLevel::Err);
        assert_eq!(log_level_of(b"other"), LogLevel::Err);
    }

    /// A [`TimeoutSink`] recording what the watchdog reported.
    #[derive(Default)]
    struct Recorder {
        /// Soft expiries reported.
        expired: usize,
        /// The executor handled the flagged timeout (cleared
        /// `EG(timed_out)`) before the next poll.
        handled: bool,
        /// The hard-timeout kill, with its `(max_execution_time,
        /// hard_timeout)`.
        killed: Option<(i64, i64)>,
    }

    impl TimeoutSink for Recorder {
        fn expired(&mut self) {
            self.expired += 1;
        }

        fn pending(&self) -> bool {
            self.expired > 0 && !self.handled
        }

        fn kill(&mut self, secs: i64, hard: i64) {
            self.killed = Some((secs, hard));
        }
    }

    const SEC: i64 = 1_000_000_000;

    /// Runs the watchdog over a scripted CPU clock; the request "ends"
    /// (the done channel's sender is dropped) once the clock runs out.
    fn run_watchdog(timer: &Timer, clock: &[i64], sink: &mut Recorder) {
        let (tx, rx) = mpsc::channel::<()>();
        let mut tx = Some(tx);
        let mut ticks = clock.iter();
        let mut now = || match ticks.next() {
            Some(&t) => t,
            None => {
                drop(tx.take());
                i64::MIN
            }
        };
        watchdog(timer, &rx, &mut now, sink);
    }

    #[test]
    fn watchdog_reports_the_soft_expiry_once() {
        let timer = Timer::default();
        timer.reset_at(0, 2, 0);
        let mut sink = Recorder::default();
        run_watchdog(&timer, &[SEC, 3 * SEC, 4 * SEC, 5 * SEC], &mut sink);
        assert_eq!(sink.expired, 1);
        assert_eq!(sink.killed, None, "no hard_timeout: nothing is killed");
        assert!(timer.timed_out());
    }

    #[test]
    fn watchdog_kills_on_an_unhandled_hard_timeout() {
        let timer = Timer::default();
        timer.reset_at(0, 1, 2);
        let mut sink = Recorder::default();
        // Soft at 1 s, nothing at 2 s, hard at 3 s with the soft one still
        // unhandled: the kill ends the watch before the clock runs out.
        run_watchdog(&timer, &[SEC, 2 * SEC, 3 * SEC, 9 * SEC], &mut sink);
        assert_eq!(sink.expired, 1);
        assert_eq!(sink.killed, Some((1, 2)));
    }

    #[test]
    fn watchdog_spares_a_handled_timeout() {
        let timer = Timer::default();
        timer.reset_at(0, 1, 2);
        let mut sink = Recorder {
            handled: true,
            ..Recorder::default()
        };
        run_watchdog(&timer, &[SEC, 3 * SEC, 4 * SEC], &mut sink);
        assert_eq!(sink.expired, 1);
        assert_eq!(sink.killed, None, "zend_timeout() disarmed the timer");
    }

    #[test]
    fn watchdog_ends_with_the_request() {
        let timer = Timer::default();
        timer.reset_at(0, 0, 0);
        let (tx, rx) = mpsc::channel::<()>();
        tx.send(()).unwrap();
        let mut sink = Recorder::default();
        let mut now = || -> i64 { unreachable!("the request already ended") };
        watchdog(&timer, &rx, &mut now, &mut sink);
        assert_eq!(sink.expired, 0);
    }

    #[test]
    fn log_messages_are_grouped() {
        let v = split_log_messages(
            b"PHP Warning:  a in x on line 1\nPHP Fatal error:  Uncaught E\nStack trace:\n#0 {main}\nplain\n",
        );
        assert_eq!(v.len(), 2);
        assert_eq!(
            v[1],
            b"PHP Fatal error:  Uncaught E\nStack trace:\n#0 {main}\nplain"
        );
    }
}

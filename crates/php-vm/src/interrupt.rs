// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! VM interrupts (`EG(vm_interrupt)`), ticks (`declare(ticks=N)`,
//! `register_tick_function`) and `max_execution_time`.
//!
//! Anything that must run "soon" from outside the executor — a signal
//! handler (pcntl async signals), the execution-time timer, a SAPI noticing
//! a client abort — sets [`request_interrupt`]; the executor polls the flag
//! after every opcode and runs the registered interrupt handlers.
//!
//! # The safe-point flag
//!
//! The executor has several kinds of work that wait for the end of the
//! current instruction: queued destructors, a due garbage collection, a
//! delayed `memory_limit` fatal and the interrupt handlers above. Instead of
//! polling each source after every instruction, whoever raises one of them
//! also sets one combined flag (`raise_safe_point`, php-src's
//! `EG(vm_interrupt)`), and the executor polls only that flag. When it is
//! set, the executor runs its `Vm::safe_point` work, which
//! checks every source in the same order as before, and then
//! `resync_safe_point` clears the flag unless a source is still set.
//! A spuriously set flag costs one pass over the sources and nothing else.
//!
//! The flag is process-wide, like the interrupt flag (the timer thread and
//! signal handlers on other threads must reach it), while the destructor
//! and collector sources are per thread. A thread only clears it when it is
//! the only thread executing PHP code (`ExecutionGuard` counts them), so
//! it can never clear a flag another executing thread still needs.
//!
//! The delayed `memory_limit` violation is recorded by `php-memlimit`,
//! which raises the flag through its pending hook
//! ([`install_memory_limit_hook`], once per process), so the executor's
//! per-instruction poll reads the flag alone; [`php_memlimit::is_pending`]
//! is only consulted by the safe-point work itself, which checks each
//! source on its own.
//!
//! # Linkage of the flags
//!
//! The per-instruction poll is one load of `SAFE_POINT`, and it is only a
//! RIP-relative load while the static has internal linkage. rustc exports a
//! private static (and the executor then reaches it through the GOT, one
//! more instruction per opcode) as soon as some body that other crates may
//! inline names it, including closures and small leaf functions, which are
//! cross-crate inlinable without `#[inline]`. So the only inlinable code
//! that touches `SAFE_POINT` is the poll (`safe_point_due`, called from the
//! executor's private loop); every raiser and every bookkeeping path goes
//! through a function that is never inlined (`raise_safe_point`,
//! `resync_safe_point`, the `ExecutionGuard` entry and exit helpers).
//! `INTERRUPT` is kept internal the same way: its only inlinable reader is
//! the call-boundary poll `Vm::check_interrupt`.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use php_runtime::Value;

use crate::vm::{Vm, VmResult};

/// The process-wide interrupt flag (async-signal-safe to set).
///
/// Only touched by functions that other crates cannot inline (see
/// [`request_interrupt`]), so the executor reads it with one RIP-relative
/// load instead of going through the GOT.
static INTERRUPT: AtomicBool = AtomicBool::new(false);
/// `EG(vm_interrupt)`: whether some safe-point work may be due (see the
/// module docs), and how many threads execute PHP code. Like
/// [`INTERRUPT`], only touched by the executor's poll and by this crate's
/// non-inlinable code ("Linkage of the flags" in the module docs), so the
/// per-instruction poll is a single direct load.
static SAFE_POINT: SafePoint = SafePoint::new();

/// The address of the safe point's `due` flag, for the trace JIT's
/// machine code to poll per loop iteration (the poll is `cmp byte
/// [addr], 0`: `AtomicBool` is one byte, and the flag is process-wide,
/// so an absolute address is stable for the process's lifetime).
pub(crate) fn jit_poll_addr() -> *const u8 {
    &SAFE_POINT.due as *const AtomicBool as *const u8
}

thread_local! {
    /// How deeply this thread nests [`Vm::execute`] (see [`ExecutionGuard`]).
    static EXECUTION_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// The combined safe-point flag plus the count of threads executing PHP
/// code, which decides whether a thread may clear the flag.
struct SafePoint {
    /// Set by raisers, cleared by [`SafePoint::resync`].
    due: AtomicBool,
    /// Threads currently inside [`Vm::execute`] (each counted once,
    /// however deeply it nests executions).
    executing: AtomicUsize,
}

impl SafePoint {
    const fn new() -> Self {
        Self {
            due: AtomicBool::new(false),
            executing: AtomicUsize::new(0),
        }
    }

    #[inline]
    fn is_due(&self) -> bool {
        self.due.load(Ordering::Relaxed)
    }

    #[inline]
    fn raise(&self) {
        self.due.store(true, Ordering::Release);
    }

    /// Clears the flag unless `sources_set()` (evaluated after the clear)
    /// reports work still waiting or another thread executes PHP code (its
    /// sources are its own; this thread cannot see them).
    ///
    /// The clear first, the sources after: a raiser sets its source before
    /// the flag, so either the swap reads (and synchronizes with) that store
    /// and `sources_set` sees the source, or the raiser's store comes later
    /// and the flag ends up set.
    fn resync(&self, sources_set: impl FnOnce() -> bool) {
        if self.executing.load(Ordering::Acquire) > 1 {
            // Keep it (and skip the swap): another thread may need it.
            return;
        }
        let _ = self.due.swap(false, Ordering::AcqRel);
        if sources_set() || self.executing.load(Ordering::Acquire) > 1 {
            self.raise();
        }
    }

    fn thread_entered(&self) {
        self.executing.fetch_add(1, Ordering::AcqRel);
    }

    fn thread_left(&self) {
        self.executing.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Whether the executor must run its safe-point work after the current
/// instruction (one relaxed load: the per-instruction poll).
#[inline]
pub(crate) fn safe_point_due() -> bool {
    SAFE_POINT.is_due()
}

/// Asks for a safe point after the current instruction: called by whoever
/// queues work for it (a destructor, a due collection), after setting its
/// own source. Async-signal-safe.
///
/// Never inlined, so that no inlinable body of a raiser (the destructor
/// queue, the collector's root buffering behind the inlinable
/// `gc::possible_root`, the execution guard) names `SAFE_POINT` and gets
/// it exported ("Linkage of the flags" in the module docs). In the release
/// binaries of 4e1c2df8 and of the 13:33 acceptance run the static was a global
/// symbol polled through the GOT, although every reference to it was
/// php-vm code. Raising is rare (a destructor queued, a collection due, a
/// signal), so the call costs nothing measurable.
#[inline(never)]
pub(crate) fn raise_safe_point() {
    SAFE_POINT.raise();
}

/// `php-memlimit`'s pending hook: a `memory_limit` violation was just
/// recorded (an allocation over the soft limit, or a failed
/// `try_reserve`), possibly inside the allocator, so this only raises the
/// flag: no allocation, no panic. The safe point then `take()`s the
/// violation, and [`resync_safe_point`] keeps the flag up while one is
/// pending.
#[inline(never)]
fn memory_limit_pending() {
    raise_safe_point();
}

/// Installs [`memory_limit_pending`] as `php-memlimit`'s pending hook.
/// Idempotent: the hook is installed once per process, and later calls
/// (every `Vm::new`) find it in place.
pub(crate) fn install_memory_limit_hook() {
    let _ = php_memlimit::set_pending_hook(memory_limit_pending);
}

/// Ends a safe point: clears the flag unless one of its sources is still
/// set (work left behind by an error, or raised while the work ran) or
/// another thread is executing PHP code.
#[cold]
#[inline(never)]
pub(crate) fn resync_safe_point() {
    SAFE_POINT.resync(|| {
        crate::object::has_pending_destructors()
            || crate::gc::wants_run()
            || INTERRUPT.load(Ordering::Acquire)
            || php_memlimit::is_pending()
    });
}

/// Counts this thread as executing PHP code while alive (created by
/// [`Vm::execute`]): [`resync_safe_point`] only clears the flag while a
/// single thread executes. A guard leaked by an abandoned native stack only
/// overcounts, which keeps the flag set (slower, never wrong).
pub(crate) struct ExecutionGuard {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl ExecutionGuard {
    /// Enters an execution on this thread.
    #[inline]
    #[must_use]
    pub(crate) fn enter() -> Self {
        let outer = EXECUTION_DEPTH.with(|d| {
            let n = d.get();
            d.set(n.saturating_add(1));
            n == 0
        });
        if outer {
            first_execution_on_thread();
        }
        Self {
            _not_send: std::marker::PhantomData,
        }
    }
}

impl Drop for ExecutionGuard {
    #[inline]
    fn drop(&mut self) {
        let last = EXECUTION_DEPTH.with(|d| {
            let n = d.get();
            d.set(n.saturating_sub(1));
            n == 1
        });
        if last {
            last_execution_on_thread();
        }
    }
}

/// This thread starts executing PHP code: it is counted, and work it queued
/// while not executing (when another thread may have cleared the flag) is
/// flagged again.
#[cold]
#[inline(never)]
fn first_execution_on_thread() {
    SAFE_POINT.thread_entered();
    if crate::object::has_pending_destructors() || crate::gc::wants_run() {
        raise_safe_point();
    }
}

/// This thread's outermost execution ended: it is no longer counted. Out
/// of line, like [`first_execution_on_thread`], so that the guard's
/// inlinable `drop` never names `SAFE_POINT` (module docs, "Linkage of the
/// flags").
#[cold]
#[inline(never)]
fn last_execution_on_thread() {
    SAFE_POINT.thread_left();
}

/// Set by the timer thread when `max_execution_time` elapsed
/// (`EG(timed_out)`).
static TIMED_OUT: AtomicBool = AtomicBool::new(false);

/// Asks the executor to run the interrupt handlers at its next safe point
/// (`zend_atomic_bool_store(&EG(vm_interrupt), true)`). Safe to call from
/// signal handlers and other threads.
///
/// Not inlinable into other crates (`#[inline(never)]`): an inlinable body
/// would export the `INTERRUPT` and `SAFE_POINT` statics and turn the executor's
/// per-instruction poll into a GOT load.
#[inline(never)]
pub fn request_interrupt() {
    INTERRUPT.store(true, Ordering::Release);
    raise_safe_point();
}

/// The interrupt-request flag itself. Storing `true` into it directly is
/// seen at the next call boundary (`check_interrupt` after a builtin call,
/// `zend_call_function`); use [`request_interrupt`] to be seen after the
/// current instruction.
#[inline(never)]
pub fn interrupt_flag() -> &'static AtomicBool {
    &INTERRUPT
}

/// An interrupt handler (`zend_interrupt_function`).
pub type InterruptFn = fn(&mut Vm) -> VmResult<()>;

/// A native tick handler (`php_add_tick_function`); receives N of
/// `declare(ticks=N)`.
pub type TickFn = fn(&mut Vm, i64) -> VmResult<()>;

/// A `register_tick_function()` entry.
#[derive(Clone, Debug)]
pub struct UserTick {
    pub(crate) callable: Value,
    pub(crate) args: Vec<Value>,
    pub(crate) calling: bool,
}

/// Nanoseconds per second.
const NS: i64 = 1_000_000_000;

/// The execution timer (`zend_set_timeout()`): armed by the executing
/// thread, checked by the process's watchdog thread ([`watchdog`]).
///
/// Like php-src's `ITIMER_PROF` timer on Linux it counts the process's CPU
/// time, so a script blocked in `sleep()` or on I/O does not time out.
struct TimerCells {
    /// The process CPU time, in nanoseconds, at which the timer expires;
    /// 0 when disarmed.
    deadline: AtomicI64,
    /// `EG(hard_timeout)`: each expiry re-arms the timer for that many
    /// seconds (0: it disarms).
    hard: AtomicI64,
    /// `EG(timeout_seconds)` of the armed timer, for the hard timeout's
    /// message.
    secs: AtomicI64,
}

impl TimerCells {
    const fn new() -> Self {
        Self {
            deadline: AtomicI64::new(0),
            hard: AtomicI64::new(0),
            secs: AtomicI64::new(0),
        }
    }
}

static TIMER: TimerCells = TimerCells::new();

/// This process's watchdog thread: the process id it runs in (threads do
/// not survive `fork()`, so a forked child starts its own) and its handle.
static WATCHDOG: Mutex<Option<(u32, std::thread::Thread)>> = Mutex::new(None);

/// `(Re)arms` the execution-time limit (`zend_unset_timeout()` then
/// `zend_set_timeout()`): after `seconds` of CPU time (0 = none) the
/// executor raises "Maximum execution time of N seconds exceeded".
///
/// `hard` is `hard_timeout`: like `zend_timeout_handler`, each expiry
/// re-arms the timer for that many seconds, so code that still runs after
/// the timeout fatal (shutdown functions, destructors) times out again,
/// and an expiry that finds the previous one still unhandled (the executor
/// is stuck in native code) ends the process ([`hard_timeout_exit`]).
pub(crate) fn arm_timer(seconds: i64, hard: i64) {
    TIMED_OUT.store(false, Ordering::Release);
    // `zend_set_timeout_ex()`: an out-of-range limit sets no timer.
    if seconds <= 0 || seconds > 999_999_999 {
        TIMER.deadline.store(0, Ordering::Release);
        return;
    }
    let Some(now) = process_cpu_ns() else {
        return;
    };
    TIMER.secs.store(seconds, Ordering::Relaxed);
    TIMER.hard.store(hard.max(0), Ordering::Relaxed);
    // Release: the watchdog that sees the deadline sees the two above.
    let deadline = now.saturating_add(seconds.saturating_mul(NS)).max(1);
    TIMER.deadline.store(deadline, Ordering::Release);
    wake_watchdog();
}

/// Starts this process's watchdog thread, or wakes it to read a new
/// deadline.
fn wake_watchdog() {
    let pid = std::process::id();
    let mut w = WATCHDOG.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((p, t)) = w.as_ref() {
        if *p == pid {
            t.unpark();
            return;
        }
    }
    if let Ok(h) = std::thread::Builder::new()
        .name("php-timeout".into())
        .spawn(watchdog)
    {
        *w = Some((pid, h.thread().clone()));
    }
}

/// The watchdog thread: sleeps until the armed deadline and performs each
/// expiry ([`timer_tick`]).
fn watchdog() {
    loop {
        let Some(now) = process_cpu_ns() else {
            std::thread::park();
            continue;
        };
        match timer_tick(&TIMER, &TIMED_OUT, now) {
            Tick::Idle => std::thread::park(),
            // CPU time runs no faster than the clock while one thread is
            // busy: waking after the CPU time left is never late for it,
            // and an early wake checks again.
            Tick::Wait(ns) => {
                std::thread::park_timeout(Duration::from_nanos(u64::try_from(ns).unwrap_or(0)));
            }
            Tick::Fired => request_interrupt(),
            Tick::Kill { secs, hard } => hard_timeout_exit(secs, hard),
        }
    }
}

/// What the watchdog found at one check of the timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tick {
    /// Disarmed: wait to be re-armed.
    Idle,
    /// Not expired: check again in that many nanoseconds.
    Wait(i64),
    /// Expired: the timeout is flagged for the executor.
    Fired,
    /// Expired while the previous expiry is still unhandled: the hard
    /// timeout.
    Kill { secs: i64, hard: i64 },
}

/// One check of the timer at process CPU time `now`; when it expired,
/// `zend_timeout_handler()`: re-arms it for `hard_timeout` (or disarms it)
/// and flags the timeout, or reports the hard timeout when the previous
/// expiry is still flagged.
fn timer_tick(t: &TimerCells, timed_out: &AtomicBool, now: i64) -> Tick {
    let deadline = t.deadline.load(Ordering::Acquire);
    if deadline == 0 {
        return Tick::Idle;
    }
    if now < deadline {
        return Tick::Wait(deadline - now);
    }
    let hard = t.hard.load(Ordering::Relaxed);
    let next = if hard > 0 {
        now.saturating_add(hard.saturating_mul(NS)).max(1)
    } else {
        0
    };
    // A re-arm since the load wins: check its deadline instead.
    if t.deadline
        .compare_exchange(deadline, next, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Tick::Wait(0);
    }
    if timed_out.load(Ordering::Acquire) {
        return Tick::Kill {
            secs: t.secs.load(Ordering::Relaxed),
            hard,
        };
    }
    timed_out.store(true, Ordering::Release);
    Tick::Fired
}

/// The hard timeout (`zend_timeout_handler()` finding `EG(timed_out)` still
/// set): writes the "(terminated)" fatal to standard error and ends the
/// process with status 124 at once. php-src reads the executing file and
/// line from the stuck thread; this thread cannot read the executor's
/// state, so it reports them as php-src does when nothing executes.
fn hard_timeout_exit(secs: i64, hard: i64) -> ! {
    let msg = format!(
        "\nFatal error: Maximum execution time of {secs}+{hard} seconds exceeded (terminated) in Unknown on line 0\n"
    );
    // SAFETY: write(2) reads `msg.len()` bytes from the live buffer `msg`;
    // a short or failed write is ignored, as `zend_quiet_write()` does.
    // Writing to descriptor 2 directly takes no lock the stuck thread
    // might hold (`std::io::stderr()` would).
    let _ = unsafe { libc::write(2, msg.as_ptr().cast(), msg.len()) };
    // SAFETY: _exit(2) ends the process without running exit handlers or
    // destructors, which could wait for locks the stuck thread holds; it
    // is what `zend_timeout_handler()` calls.
    unsafe { libc::_exit(124) }
}

/// The CPU time of the whole process in nanoseconds (what `ITIMER_PROF`
/// counts).
fn process_cpu_ns() -> Option<i64> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes only into the timespec we own.
    let r = unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &raw mut ts) };
    (r == 0).then(|| ts.tv_sec.saturating_mul(NS).saturating_add(ts.tv_nsec))
}

impl Vm {
    /// Runs the interrupt handlers when an interrupt was requested
    /// (`zend_interrupt_helper`). Called at the executor's safe point (once
    /// the combined flag is set) and at call boundaries.
    #[inline]
    pub(crate) fn check_interrupt(&mut self) -> VmResult<()> {
        if INTERRUPT.load(Ordering::Relaxed) {
            self.handle_interrupt()?;
        }
        Ok(())
    }

    #[cold]
    fn handle_interrupt(&mut self) -> VmResult<()> {
        INTERRUPT.store(false, Ordering::Release);
        if TIMED_OUT.swap(false, Ordering::AcqRel) {
            // `zend_timeout`.
            let n = self.timeout_seconds;
            let msg = format!(
                "Maximum execution time of {n} second{} exceeded",
                if n == 1 { "" } else { "s" }
            );
            return self.error(crate::errors::level::ERROR, &msg);
        }
        for h in self.interrupt_handlers.clone() {
            h(self)?;
        }
        Ok(())
    }

    /// `ZEND_TICKS`.
    pub(crate) fn op_ticks(&mut self, n: u32) -> VmResult<()> {
        self.ticks_count += 1;
        if self.ticks_count < n {
            return Ok(());
        }
        self.ticks_count = 0;
        if self.tick_hooks.is_empty() && self.user_ticks.is_empty() {
            return Ok(());
        }
        self.fiber_switch_block += 1;
        let r = self.run_ticks(i64::from(n));
        self.fiber_switch_block -= 1;
        r
    }

    fn run_ticks(&mut self, n: i64) -> VmResult<()> {
        for h in self.tick_hooks.clone() {
            h(self, n)?;
        }
        // `run_user_tick_functions`: no re-entrant call of the same entry.
        let mut i = 0;
        while i < self.user_ticks.len() {
            if self.user_ticks[i].calling {
                i += 1;
                continue;
            }
            self.user_ticks[i].calling = true;
            let (cb, args) = (
                self.user_ticks[i].callable.clone(),
                self.user_ticks[i].args.clone(),
            );
            let r = self.call_value(&cb, args);
            if let Some(t) = self.user_ticks.get_mut(i) {
                t.calling = false;
            }
            r?;
            i += 1;
        }
        Ok(())
    }

    /// `php_request_startup()`'s `zend_set_timeout()`: `max_input_time`
    /// limits the request startup (reading the input) unless it is -1, in
    /// which case `max_execution_time` covers it; [`Vm::script_timer`]
    /// then hands over to `max_execution_time` when the script starts.
    pub fn request_timer(&mut self) {
        let input = self.ini.get_long("max_input_time");
        let t = if input == -1 {
            self.ini.get_long("max_execution_time")
        } else {
            input
        };
        self.set_time_limit(t);
    }

    /// `php_execute_script()`: after a `max_input_time` startup phase
    /// ([`Vm::request_timer`]), the script runs under `max_execution_time`.
    pub fn script_timer(&mut self) {
        if self.ini.get_long("max_input_time") != -1 {
            let t = self.ini.get_long("max_execution_time");
            self.set_time_limit(t);
        }
    }

    /// `set_time_limit()` / `max_execution_time` (`zend_set_timeout`):
    /// restarts the timer. A SAPI that runs the timer itself
    /// ([`Vm::timer_hook`]) is told the new limit instead; it reports
    /// expiry with [`sapi_timer_expired`].
    pub fn set_time_limit(&mut self, seconds: i64) {
        self.timeout_seconds = seconds;
        if let Some(hook) = self.timer_hook {
            // Cancels a wall-clock timer armed before the hook was
            // installed and clears a flagged timeout (`zend_set_timeout`
            // resets `EG(timed_out)`).
            arm_timer(0, 0);
            hook(self, seconds);
            return;
        }
        arm_timer(seconds, self.ini.get_long("hard_timeout"));
    }
}

/// A timer run by the SAPI ([`Vm::timer_hook`]) expired: the
/// `zend_timeout_handler` half the VM's own timer thread performs in
/// `arm_timer`. Flags the timeout and requests an interrupt, so the
/// executor raises the "Maximum execution time" fatal at its next safe
/// point. Returns `false` when the previous expiry is still unhandled:
/// php-src's hard-timeout condition, on which it kills the process.
#[inline(never)]
pub fn sapi_timer_expired() -> bool {
    if TIMED_OUT.swap(true, Ordering::AcqRel) {
        return false;
    }
    request_interrupt();
    true
}

/// Whether a flagged timeout still waits for the executor (`EG(timed_out)`).
#[inline(never)]
pub fn timeout_pending() -> bool {
    TIMED_OUT.load(Ordering::Acquire)
}

/// Clears pending timeout and interrupt flags for a new request.
pub fn clear_interrupt() {
    TIMED_OUT.store(false, Ordering::Release);
    INTERRUPT.store(false, Ordering::Release);
}

/// `register_tick_function(callable $callback, mixed ...$args): bool`.
pub(crate) fn register_tick_function(vm: &mut Vm, a: &mut crate::func::Args) -> VmResult<Value> {
    let cb = a.at(0);
    let args: Vec<Value> = a.values.iter().skip(1).map(Value::deref).collect();
    vm.user_ticks.push(UserTick {
        callable: cb,
        args,
        calling: false,
    });
    Ok(Value::Bool(true))
}

/// `unregister_tick_function(callable $callback): void`.
pub(crate) fn unregister_tick_function(vm: &mut Vm, a: &mut crate::func::Args) -> VmResult<Value> {
    let cb = a.at(0);
    let target = resolve_fcc(vm, &cb)?;
    let mut i = 0;
    while i < vm.user_ticks.len() {
        let registered = vm.user_ticks[i].callable.clone();
        let equal = match (&target, resolve_fcc(vm, &registered)?) {
            (Some(x), Some(y)) => fcc_equals(x, &y),
            _ => callables_equal(&registered, &cb),
        };
        if equal {
            if vm.user_ticks[i].calling {
                return Err(vm.make_throw(
                    "Error",
                    "Registered tick function cannot be unregistered while it is being executed",
                ));
            }
            vm.user_ticks.remove(i);
            continue;
        }
        i += 1;
    }
    Ok(Value::Null)
}

/// The resolved call of a tick callable (`zend_fcall_info_cache`), or
/// `None` when it is not callable.
fn resolve_fcc(vm: &mut Vm, v: &Value) -> VmResult<Option<crate::vm::PendingCall>> {
    Ok(vm.zend_is_callable(v, true)?.ok())
}

/// `zend_fcc_equals()` (`Zend/zend_API.h`) on resolved calls: the same
/// `$this` and called scope, and the same function: a real closure only
/// equals itself; a first-class-callable (fake) closure equals the
/// function or method it wraps (`zend_fcc_closure_equals_ex`), and
/// trampolines compare by the called method name.
fn fcc_equals(a: &crate::vm::PendingCall, b: &crate::vm::PendingCall) -> bool {
    use php_runtime::Obj;
    use std::rc::Rc;
    // `PendingCall::closure` is only set for real closures
    // (`Vm::closure_call`).
    let real_closure = |c: &crate::vm::PendingCall| c.closure.clone();
    let same_this = match (&a.this, &b.this) {
        (None, None) => true,
        (Some(x), Some(y)) => Obj::ptr_eq(x, y),
        _ => false,
    };
    let same_called = match (&a.called, &b.called) {
        (None, None) => true,
        (Some(x), Some(y)) => Rc::ptr_eq(x, y),
        _ => false,
    };
    if !same_this || !same_called {
        return false;
    }
    match (real_closure(a), real_closure(b)) {
        (Some(x), Some(y)) => Obj::ptr_eq(&x, &y),
        (None, None) => crate::func::Func::ptr_eq(&a.func, &b.func) && a.magic_name == b.magic_name,
        _ => false,
    }
}

/// `zend_fcc_equals` approximated on the callable values: function names
/// case-insensitively, `[object|class, method]` pairs, closures by identity.
fn callables_equal(a: &Value, b: &Value) -> bool {
    fn lc(v: &Value) -> Option<Vec<u8>> {
        v.as_str().map(|s| {
            let b = s.as_bytes();
            b.strip_prefix(b"\\").unwrap_or(b).to_ascii_lowercase()
        })
    }
    match (a.deref(), b.deref()) {
        (Value::Str(_), Value::Str(_)) => lc(a) == lc(b),
        (Value::Object(x), Value::Object(y)) => php_runtime::Obj::ptr_eq(&x, &y),
        (Value::Array(x), Value::Array(y)) => {
            let (Some(x0), Some(x1), Some(y0), Some(y1)) = (
                x.get(&php_runtime::ArrayKey::Int(0)),
                x.get(&php_runtime::ArrayKey::Int(1)),
                y.get(&php_runtime::ArrayKey::Int(0)),
                y.get(&php_runtime::ArrayKey::Int(1)),
            ) else {
                return false;
            };
            let same_target = match (x0.deref(), y0.deref()) {
                (Value::Object(p), Value::Object(q)) => php_runtime::Obj::ptr_eq(&p, &q),
                (Value::Str(_), Value::Str(_)) => lc(x0) == lc(y0),
                _ => false,
            };
            same_target && lc(x1) == lc(y1)
        }
        _ => false,
    }
}

/// `set_time_limit(int $seconds): bool`.
pub(crate) fn set_time_limit(vm: &mut Vm, a: &mut crate::func::Args) -> VmResult<Value> {
    let s = a.int(0);
    let mut sink = Vec::new();
    vm.ini
        .ini_set("max_execution_time", s.to_string().as_bytes(), &mut sink);
    vm.set_time_limit(s);
    Ok(Value::Bool(true))
}

#[cfg(test)]
mod tests {
    use super::{
        process_cpu_ns, request_interrupt, timer_tick, ExecutionGuard, SafePoint, Tick, TimerCells,
        EXECUTION_DEPTH, INTERRUPT, NS,
    };
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn safe_point_flag_is_cleared_only_without_sources() {
        let sp = SafePoint::new();
        assert!(!sp.is_due());
        sp.raise();
        assert!(sp.is_due());
        // Work still waiting keeps the flag.
        sp.resync(|| true);
        assert!(sp.is_due());
        // Nothing left: cleared.
        sp.resync(|| false);
        assert!(!sp.is_due());
        // A stale resync never sets it without a source.
        sp.resync(|| false);
        assert!(!sp.is_due());
    }

    #[test]
    fn safe_point_sources_are_read_after_the_clear() {
        let sp = SafePoint::new();
        sp.raise();
        // A source raised while the check runs (here: inside the closure,
        // i.e. after the swap) leaves the flag set.
        sp.resync(|| {
            assert!(!sp.is_due());
            sp.raise();
            false
        });
        assert!(sp.is_due());
    }

    #[test]
    fn safe_point_flag_stays_while_another_thread_executes() {
        let sp = SafePoint::new();
        sp.thread_entered();
        sp.thread_entered();
        sp.raise();
        sp.resync(|| false);
        assert!(sp.is_due(), "the other thread may still need it");
        sp.thread_left();
        sp.resync(|| false);
        assert!(!sp.is_due());
        sp.thread_left();
    }

    #[test]
    fn execution_guard_counts_nesting_per_thread() {
        // Run on a fresh thread so no other test's executions interfere.
        std::thread::spawn(|| {
            let depth = || EXECUTION_DEPTH.with(std::cell::Cell::get);
            assert_eq!(depth(), 0);
            let outer = ExecutionGuard::enter();
            assert_eq!(depth(), 1);
            let inner = ExecutionGuard::enter();
            assert_eq!(depth(), 2);
            drop(inner);
            assert_eq!(depth(), 1);
            drop(outer);
            assert_eq!(depth(), 0);
        })
        .join()
        .expect("guard thread");
    }

    #[test]
    fn flag_round_trip() {
        request_interrupt();
        assert!(INTERRUPT.swap(false, Ordering::AcqRel));
    }

    #[test]
    fn timer_rearms_for_hard_timeout_after_a_handled_timeout() {
        let t = TimerCells::new();
        let timed_out = AtomicBool::new(false);
        let arm = |deadline: i64, hard: i64| {
            t.secs.store(1, Ordering::Relaxed);
            t.hard.store(hard, Ordering::Relaxed);
            t.deadline.store(deadline, Ordering::Release);
        };
        // Disarmed: nothing to wait for.
        assert_eq!(timer_tick(&t, &timed_out, 5), Tick::Idle);
        arm(10 * NS, 2);
        // Not expired: wait for the CPU time left.
        assert_eq!(timer_tick(&t, &timed_out, 4 * NS), Tick::Wait(6 * NS));
        // First expiry: flagged, re-armed for hard_timeout.
        assert_eq!(timer_tick(&t, &timed_out, 10 * NS), Tick::Fired);
        assert!(timed_out.load(Ordering::Acquire));
        assert_eq!(t.deadline.load(Ordering::Acquire), 12 * NS);
        // Handled (zend_timeout clears the flag): it fires again, so a
        // shutdown function times out too (tests/lang/045).
        timed_out.store(false, Ordering::Release);
        assert_eq!(timer_tick(&t, &timed_out, 12 * NS), Tick::Fired);
        // Not handled: the hard timeout.
        assert_eq!(
            timer_tick(&t, &timed_out, 14 * NS),
            Tick::Kill { secs: 1, hard: 2 }
        );
        // hard_timeout=0: one expiry, then disarmed.
        timed_out.store(false, Ordering::Release);
        arm(20 * NS, 0);
        assert_eq!(timer_tick(&t, &timed_out, 21 * NS), Tick::Fired);
        assert_eq!(timer_tick(&t, &timed_out, 99 * NS), Tick::Idle);
    }

    #[test]
    fn a_rearm_during_the_check_wins() {
        let t = TimerCells::new();
        let timed_out = AtomicBool::new(false);
        t.deadline.store(10 * NS, Ordering::Release);
        // The check read 10 s; set_time_limit() moved it before the swap.
        let seen = t.deadline.load(Ordering::Acquire);
        t.deadline.store(50 * NS, Ordering::Release);
        assert!(t
            .deadline
            .compare_exchange(seen, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_err());
        assert_eq!(timer_tick(&t, &timed_out, 11 * NS), Tick::Wait(39 * NS));
        assert!(!timed_out.load(Ordering::Acquire));
    }

    #[test]
    fn process_cpu_time_advances() {
        let a = process_cpu_ns().expect("cpu clock");
        let mut x = 0u64;
        for i in 0..2_000_000u64 {
            x = std::hint::black_box(x.wrapping_add(i));
        }
        let b = process_cpu_ns().expect("cpu clock");
        assert!(b > a, "{a} -> {b} ({x})");
    }
}

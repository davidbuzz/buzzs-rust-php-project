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
//! also sets one combined flag ([`raise_safe_point`], php-src's
//! `EG(vm_interrupt)`), and the executor polls only that flag. When it is
//! set, the executor runs [`Vm::safe_point`](crate::vm::Vm) work, which
//! checks every source in the same order as before, and then
//! [`resync_safe_point`] clears the flag unless a source is still set.
//! A spuriously set flag costs one pass over the sources and nothing else.
//!
//! The flag is process-wide, like the interrupt flag (the timer thread and
//! signal handlers on other threads must reach it), while the destructor
//! and collector sources are per thread. A thread only clears it when it is
//! the only thread executing PHP code ([`ExecutionGuard`] counts them), so
//! it can never clear a flag another executing thread still needs.
//!
//! The delayed `memory_limit` violation is recorded by `php-memlimit`,
//! which cannot see this flag yet, so the executor still polls
//! [`php_memlimit::is_pending`] next to it.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
/// [`INTERRUPT`], only touched by this crate's non-inlinable code, so the
/// per-instruction poll is a single direct load.
static SAFE_POINT: SafePoint = SafePoint::new();

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
#[inline]
pub(crate) fn raise_safe_point() {
    SAFE_POINT.raise();
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
            let n = d.get().saturating_sub(1);
            d.set(n);
            n == 0
        });
        if last {
            SAFE_POINT.thread_left();
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

/// Set by the timer thread when `max_execution_time` elapsed.
static TIMED_OUT: AtomicBool = AtomicBool::new(false);
/// Generation of the armed timer (re-arming cancels older timers).
static TIMER_GEN: AtomicU64 = AtomicU64::new(0);

/// Asks the executor to run the interrupt handlers at its next safe point
/// (`zend_atomic_bool_store(&EG(vm_interrupt), true)`). Safe to call from
/// signal handlers and other threads.
///
/// Not inlinable into other crates (`#[inline(never)]`): an inlinable body
/// would export [`INTERRUPT`] and [`SAFE_POINT`] and turn the executor's
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

/// `(Re)arms` the execution-time limit: after `seconds` (0 = none) the
/// executor raises "Maximum execution time of N seconds exceeded".
///
/// `hard` is `hard_timeout`: like `zend_timeout_handler`, each expiry
/// re-arms the timer for that many seconds, so code that still runs after
/// the timeout fatal (shutdown functions, destructors) times out again.
/// php-src kills the process (exit 124) when that second expiry finds the
/// first one still unhandled; this timer counts wall-clock time rather than
/// `ITIMER_PROF` CPU time (a blocking builtin is not burning CPU), so it
/// stops instead.
pub(crate) fn arm_timer(seconds: i64, hard: i64) {
    let gen = TIMER_GEN.fetch_add(1, Ordering::AcqRel) + 1;
    TIMED_OUT.store(false, Ordering::Release);
    let Ok(secs) = u64::try_from(seconds) else {
        return;
    };
    if secs == 0 {
        return;
    }
    let hard = u64::try_from(hard).unwrap_or(0);
    let _ = std::thread::Builder::new()
        .name("php-timeout".into())
        .spawn(move || {
            let mut wait = secs;
            loop {
                std::thread::sleep(Duration::from_secs(wait));
                match timer_expired(&TIMER_GEN, &TIMED_OUT, gen, hard) {
                    Some(next) => wait = next,
                    None => return,
                }
            }
        });
}

/// One expiry of the timer armed as generation `gen`
/// (`zend_timeout_handler`): flags the timeout and requests an interrupt
/// unless the timer was re-armed since or the previous timeout is still
/// pending. Returns the seconds until the next expiry (`hard_timeout`), or
/// `None` when the timer thread should stop.
fn timer_expired(current: &AtomicU64, timed_out: &AtomicBool, gen: u64, hard: u64) -> Option<u64> {
    if current.load(Ordering::Acquire) != gen || timed_out.load(Ordering::Acquire) {
        return None;
    }
    timed_out.store(true, Ordering::Release);
    request_interrupt();
    (hard > 0).then_some(hard)
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

    /// `set_time_limit()` / `max_execution_time`: restarts the timer.
    pub(crate) fn set_time_limit(&mut self, seconds: i64) {
        self.timeout_seconds = seconds;
        arm_timer(seconds, self.ini.get_long("hard_timeout"));
    }
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
        request_interrupt, timer_expired, ExecutionGuard, SafePoint, EXECUTION_DEPTH, INTERRUPT,
    };
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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
        let gen = AtomicU64::new(7);
        let timed_out = AtomicBool::new(false);
        // First expiry: flagged, re-armed for hard_timeout.
        assert_eq!(timer_expired(&gen, &timed_out, 7, 2), Some(2));
        assert!(timed_out.load(Ordering::Acquire));
        // Not handled yet: the timer stops (php-src would hard-kill).
        assert_eq!(timer_expired(&gen, &timed_out, 7, 2), None);
        // Handled (zend_timeout clears the flag): it fires again.
        timed_out.store(false, Ordering::Release);
        assert_eq!(timer_expired(&gen, &timed_out, 7, 2), Some(2));
        // hard_timeout=0: one expiry only.
        timed_out.store(false, Ordering::Release);
        assert_eq!(timer_expired(&gen, &timed_out, 7, 0), None);
        assert!(timed_out.load(Ordering::Acquire));
        // Re-armed since (set_time_limit): the old timer does nothing.
        timed_out.store(false, Ordering::Release);
        assert_eq!(timer_expired(&gen, &timed_out, 6, 2), None);
        assert!(!timed_out.load(Ordering::Acquire));
    }
}

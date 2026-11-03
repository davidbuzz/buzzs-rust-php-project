//! VM interrupts (`EG(vm_interrupt)`), ticks (`declare(ticks=N)`,
//! `register_tick_function`) and `max_execution_time`.
//!
//! Anything that must run "soon" from outside the executor — a signal
//! handler (pcntl async signals), the execution-time timer, a SAPI noticing
//! a client abort — sets [`request_interrupt`]; the executor polls the flag
//! after every opcode and runs the registered interrupt handlers.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use php_runtime::Value;

use crate::vm::{Vm, VmResult};

/// The process-wide interrupt flag (async-signal-safe to set).
static INTERRUPT: AtomicBool = AtomicBool::new(false);
/// Set by the timer thread when `max_execution_time` elapsed.
static TIMED_OUT: AtomicBool = AtomicBool::new(false);
/// Generation of the armed timer (re-arming cancels older timers).
static TIMER_GEN: AtomicU64 = AtomicU64::new(0);

/// Asks the executor to run the interrupt handlers at its next safe point
/// (`zend_atomic_bool_store(&EG(vm_interrupt), true)`). Safe to call from
/// signal handlers and other threads.
pub fn request_interrupt() {
    INTERRUPT.store(true, Ordering::Release);
}

/// The interrupt flag itself (for SAPIs that want to set it directly).
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
pub(crate) fn arm_timer(seconds: i64) {
    let gen = TIMER_GEN.fetch_add(1, Ordering::AcqRel) + 1;
    TIMED_OUT.store(false, Ordering::Release);
    let Ok(secs) = u64::try_from(seconds) else {
        return;
    };
    if secs == 0 {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("php-timeout".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(secs));
            if TIMER_GEN.load(Ordering::Acquire) == gen {
                TIMED_OUT.store(true, Ordering::Release);
                request_interrupt();
            }
        });
}

impl Vm {
    /// Polled after each opcode: runs the interrupt handlers when the flag
    /// is set (`zend_interrupt_helper`).
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
        arm_timer(seconds);
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
    let mut i = 0;
    while i < vm.user_ticks.len() {
        if callables_equal(&vm.user_ticks[i].callable, &cb) {
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
    use super::{request_interrupt, INTERRUPT};
    use std::sync::atomic::Ordering;

    #[test]
    fn flag_round_trip() {
        request_interrupt();
        assert!(INTERRUPT.swap(false, Ordering::AcqRel));
    }
}

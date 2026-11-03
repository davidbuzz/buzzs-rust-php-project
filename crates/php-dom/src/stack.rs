//! Recursion guard standing in for `zend_call_stack_overflowed()`.
//!
//! The recursive tree algorithms (serializers, `normalize`, …) check how
//! much native stack they consumed since the outermost guarded entry and
//! fail with PHP's "Maximum call stack size reached" error instead of
//! overflowing.  A host may install an absolute limit with
//! [`set_stack_limit`] (the lowest stack address recursion may reach).

use std::cell::Cell;

/// The default stack budget for one guarded operation.
pub const DEFAULT_STACK_BUDGET: usize = 512 * 1024;

/// The message of the `Error` thrown on overflow.
pub const STACK_OVERFLOW_MESSAGE: &str = "Maximum call stack size reached. Infinite recursion?";

thread_local! {
    static HOST_LIMIT: Cell<usize> = const { Cell::new(0) };
    static BUDGET: Cell<usize> = const { Cell::new(DEFAULT_STACK_BUDGET) };
    static BASE: Cell<usize> = const { Cell::new(0) };
}

/// Installs an absolute lower stack address bound (0 removes it) for the
/// current thread.
pub fn set_stack_limit(lowest_address: usize) {
    HOST_LIMIT.with(|l| l.set(lowest_address));
}

/// Sets the per-operation stack budget used when no absolute limit is set.
pub fn set_stack_budget(bytes: usize) {
    BUDGET.with(|b| b.set(bytes));
}

#[inline(never)]
fn approx_sp() -> usize {
    let marker = 0u8;
    std::hint::black_box(std::ptr::addr_of!(marker)) as usize
}

/// Marks the outermost entry of a guarded operation.
pub(crate) struct StackScope {
    outer: bool,
}

impl StackScope {
    pub(crate) fn enter() -> Self {
        let outer = BASE.with(|b| {
            if b.get() == 0 {
                b.set(approx_sp());
                true
            } else {
                false
            }
        });
        Self { outer }
    }
}

impl Drop for StackScope {
    fn drop(&mut self) {
        if self.outer {
            BASE.with(|b| b.set(0));
        }
    }
}

thread_local! {
    static PENDING: Cell<Option<&'static str>> = const { Cell::new(None) };
}

/// `zend_call_stack_overflowed()` plus `zend_throw_error()` when no error
/// is pending yet: records `message` and returns `true` on overflow.
pub(crate) fn check(message: &'static str) -> bool {
    if overflowed() {
        PENDING.with(|p| {
            if p.get().is_none() {
                p.set(Some(message));
            }
        });
        true
    } else {
        false
    }
}

/// The error recorded by [`check`], if any (clears it).
pub(crate) fn take_error() -> Option<crate::error::DomError> {
    PENDING.with(Cell::take).map(|m| crate::error::DomError::Error(m.into()))
}

/// `zend_call_stack_overflowed(EG(stack_limit))`.
pub(crate) fn overflowed() -> bool {
    let sp = approx_sp();
    let limit = HOST_LIMIT.with(Cell::get);
    if limit != 0 {
        return sp < limit;
    }
    let base = BASE.with(Cell::get);
    base != 0 && base.saturating_sub(sp) > BUDGET.with(Cell::get)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recurse(depth: usize) -> Option<usize> {
        if overflowed() {
            return Some(depth);
        }
        let pad = std::hint::black_box([0u8; 256]);
        recurse(depth + 1).map(|d| d + usize::from(pad[0]))
    }

    #[test]
    fn detects_deep_recursion() {
        let _s = StackScope::enter();
        let d = recurse(0).expect("overflow detected");
        assert!(d > 100);
    }

    #[test]
    fn no_overflow_without_scope() {
        assert!(!overflowed());
    }
}

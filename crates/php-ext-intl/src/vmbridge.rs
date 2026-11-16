// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Lends the VM to library callbacks that have no access to it (the
//! `UConverter` ICU callbacks invoke PHP methods), following the pattern
//! of `php-ext-streams`' bridge.

use std::cell::{Cell, RefCell};

use php_vm::ext::{Throw, VmResult};
use php_vm::Vm;

thread_local! {
    /// The VM lent for the duration of [`enter`].
    static VM_PTR: Cell<*mut Vm> = const { Cell::new(std::ptr::null_mut()) };
    /// The first exception raised inside a callback.
    static PENDING: RefCell<Option<Throw>> = const { RefCell::new(None) };
}

/// Restores the previous pointer when a scope ends (also on unwind).
struct Restore(*mut Vm);

impl Drop for Restore {
    fn drop(&mut self) {
        VM_PTR.with(|p| p.set(self.0));
    }
}

/// Runs `f` with `vm` reachable through [`with_vm`], then re-raises the
/// first exception a callback threw.
pub(crate) fn enter<R>(vm: &mut Vm, f: impl FnOnce() -> R) -> VmResult<R> {
    let ptr: *mut Vm = vm;
    let prev = VM_PTR.with(|p| p.replace(ptr));
    let r = {
        let _restore = Restore(prev);
        f()
    };
    match PENDING.with(|p| p.borrow_mut().take()) {
        Some(t) => Err(t),
        None => Ok(r),
    }
}

/// Calls `f` with the VM lent by the innermost [`enter`] (`None` outside).
pub(crate) fn with_vm<R>(f: impl FnOnce(&mut Vm) -> R) -> Option<R> {
    let ptr = VM_PTR.with(|p| p.replace(std::ptr::null_mut()));
    if ptr.is_null() {
        return None;
    }
    let _restore = Restore(ptr);
    // SAFETY: `ptr` comes from the `&mut Vm` passed to `enter`, whose frame
    // is still active (the slot is restored when it returns).  That borrow
    // is not used while `enter`'s closure runs, and the slot is nulled for
    // the duration of `f`, so this is the only live mutable path to the VM.
    let vm = unsafe { &mut *ptr };
    Some(f(vm))
}

/// Records a throw raised inside a callback (the first one wins).
pub(crate) fn set_pending(t: Throw) {
    PENDING.with(|p| {
        let mut p = p.borrow_mut();
        if p.is_none() {
            *p = Some(t);
        }
    });
}

/// Whether a callback threw.
pub(crate) fn has_pending() -> bool {
    PENDING.with(|p| p.borrow().is_some())
}

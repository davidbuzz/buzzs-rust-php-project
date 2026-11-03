//! Running php-dom operations with the VM lent to callbacks, and
//! forwarding their diagnostics.
//!
//! php-dom reports its own `php_error_docref` diagnostics through
//! `php_dom::error`, libxml2's go through `php_libxml::error`. Both are
//! collected into one queue in the order they were raised and emitted
//! once the library call returns (`func(): message` warnings and notices,
//! verbatim deprecations).

use std::cell::{Cell, RefCell};

use php_dom::error::Level;
use php_ext_streams::bridge;
use php_libxml::error::DiagnosticLevel;
use php_vm::ext::{Args, VmResult};
use php_vm::Vm;

/// `E_NOTICE`.
const E_NOTICE: i64 = 8;
/// `E_DEPRECATED`.
const E_DEPRECATED: i64 = 8192;

/// A queued diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Warning,
    Notice,
    Deprecated,
}

thread_local! {
    static QUEUE: RefCell<Vec<(Kind, Vec<u8>)>> = const { RefCell::new(Vec::new()) };
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
}

/// Moves libxml2's queued diagnostics behind the ones already queued.
fn drain_libxml() {
    let ds = php_libxml::error::take_diagnostics();
    if ds.is_empty() {
        return;
    }
    QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        for d in ds {
            let k = match d.level {
                DiagnosticLevel::Warning => Kind::Warning,
                DiagnosticLevel::Notice => Kind::Notice,
            };
            q.push((k, d.message));
        }
    });
}

/// Installs the php-dom diagnostics handler on this thread (idempotent).
pub(crate) fn ensure_installed() {
    if INSTALLED.with(Cell::get) {
        return;
    }
    INSTALLED.with(|i| i.set(true));
    php_ext_xml::libxml::ensure_hooks();
    php_dom::error::set_diagnostic_handler(Some(Box::new(|d| {
        drain_libxml();
        let k = match d.level {
            Level::Warning => Kind::Warning,
            Level::Notice => Kind::Notice,
            Level::Deprecated => Kind::Deprecated,
        };
        QUEUE.with(|q| q.borrow_mut().push((k, d.message.into_bytes())));
    })));
}

/// Drops every queued diagnostic (an exception is pending).
fn discard() {
    drain_libxml();
    QUEUE.with(|q| q.borrow_mut().clear());
}

/// Emits the queued diagnostics.
pub(crate) fn flush(vm: &mut Vm) -> VmResult<()> {
    drain_libxml();
    let items = QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()));
    let mut it = items.into_iter();
    while let Some((k, m)) = it.next() {
        let r = match k {
            Kind::Warning => vm.php_warning_bytes(&m),
            Kind::Notice => vm.php_error_bytes(E_NOTICE, &m),
            Kind::Deprecated => vm.raise_bytes(E_DEPRECATED, &m),
        };
        if let Err(t) = r {
            drop(it);
            return Err(t);
        }
    }
    Ok(())
}

/// Runs a library operation outside of a native function frame (property
/// handlers, iteration).
pub(crate) fn lib<R>(vm: &mut Vm, f: impl FnOnce() -> R) -> VmResult<R> {
    ensure_installed();
    let r = bridge::run(vm, || {
        let r = f();
        drain_libxml();
        r
    });
    match r {
        Ok(v) => {
            flush(vm)?;
            Ok(v)
        }
        Err(t) => {
            discard();
            Err(t)
        }
    }
}

/// Runs a library operation that cannot reach PHP code or the stream
/// layer (no VM is lent; only diagnostics are forwarded). This is the fast
/// path for the bulk of DOM calls.
pub(crate) fn direct<R>(vm: &mut Vm, f: impl FnOnce() -> R) -> VmResult<R> {
    ensure_installed();
    let r = f();
    flush(vm)?;
    Ok(r)
}

/// Runs a library operation of the native function described by `a`.
pub(crate) fn call<R>(vm: &mut Vm, a: &Args, f: impl FnOnce() -> R) -> VmResult<R> {
    ensure_installed();
    let r = php_ext_xml::libxml::run(vm, a, || {
        let r = f();
        drain_libxml();
        r
    });
    match r {
        Ok(v) => {
            flush(vm)?;
            Ok(v)
        }
        Err(t) => {
            discard();
            Err(t)
        }
    }
}

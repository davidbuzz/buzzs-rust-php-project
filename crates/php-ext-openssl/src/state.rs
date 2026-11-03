//! The per-request library state and the glue that runs one library call:
//! INI synchronisation, diagnostics and thrown errors.

use std::cell::RefCell;

use php_openssl::{Diagnostic, Level, PhpResult, Throw};
use php_vm::ext::{Args, VmResult};
use php_vm::Vm;

/// `E_DEPRECATED`.
pub(crate) const E_DEPRECATED: i64 = 8192;

thread_local! {
    /// `OPENSSL_G(...)`: created on first use, dropped at RSHUTDOWN.
    static STATE: RefCell<Option<php_openssl::OpenSsl>> = const { RefCell::new(None) };
}

/// RSHUTDOWN: forgets the error queue.
pub(crate) fn reset(_vm: &mut Vm) {
    STATE.with(|s| *s.borrow_mut() = None);
}

/// Runs `f` on the request state with the current `openssl.*` and
/// `open_basedir` INI values, then raises the diagnostics it queued and
/// converts a thrown error into a PHP throwable for the function of `a`.
pub(crate) fn run<T>(
    vm: &mut Vm,
    a: &Args,
    f: impl FnOnce(&mut php_openssl::OpenSsl) -> PhpResult<T>,
) -> VmResult<T> {
    let cafile = vm.ini_string("openssl.cafile").unwrap_or_default();
    let capath = vm.ini_string("openssl.capath").unwrap_or_default();
    let basedir = vm.ini_string("open_basedir").filter(|b| !b.is_empty());
    let (result, diags) = STATE.with(|s| {
        let mut slot = s.borrow_mut();
        let ssl = slot.get_or_insert_with(php_openssl::OpenSsl::new);
        ssl.ini_cafile = cafile;
        ssl.ini_capath = capath;
        ssl.open_basedir = basedir;
        let r = f(ssl);
        (r, ssl.take_diagnostics())
    });
    emit(vm, diags)?;
    result.map_err(|t| throw(vm, a, &t))
}

/// Like [`run`] for library calls that cannot throw.
pub(crate) fn run_ok<T>(
    vm: &mut Vm,
    a: &Args,
    f: impl FnOnce(&mut php_openssl::OpenSsl) -> T,
) -> VmResult<T> {
    run(vm, a, |s| Ok(f(s)))
}

/// Raises queued diagnostics (`php_error_docref`, prefixed).
pub(crate) fn emit(vm: &mut Vm, diags: Vec<Diagnostic>) -> VmResult<()> {
    for d in diags {
        match d.level {
            Level::Warning => vm.php_warning(&d.message)?,
            Level::Deprecated => vm.php_deprecated(&d.message)?,
            Level::Notice => vm.php_notice(&d.message)?,
        }
    }
    Ok(())
}

/// Converts a library [`Throw`] into the VM throwable.
pub(crate) fn throw(vm: &mut Vm, a: &Args, t: &Throw) -> php_vm::ext::Throw {
    match t {
        Throw::ArgumentValueError { arg, message } => {
            a.value_error(vm, *arg as usize, message)
        }
        Throw::ArgumentMustNotBeEmpty { arg } => a.value_error(vm, *arg as usize, "must not be empty"),
        Throw::ValueError(m) => vm.throw("ValueError", m),
        Throw::Exception(m) => vm.throw("Exception", m),
    }
}

/// The engine's `#[\Deprecated]` notice for the `*_free` functions.
pub(crate) fn deprecated_function(vm: &mut Vm, a: &Args, message: &str) -> VmResult<()> {
    vm.raise(
        E_DEPRECATED,
        &format!("Function {}() is deprecated since 8.0, {message}", a.func_name()),
    )
}

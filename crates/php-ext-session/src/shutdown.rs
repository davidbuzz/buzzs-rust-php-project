//! Request shutdown (`RSHUTDOWN`) and the `session_shutdown` user shutdown
//! function registered by `session_set_save_handler($handler)`.

use php_runtime::{Obj, RuntimeError, Value, ZStr};
use php_vm::{Throw, Vm};

use crate::host::{set_phase, with_host, Phase};

/// The function the `session_shutdown` entry calls.
const REGISTER_SHUTDOWN: &str = "session_register_shutdown";

fn is_session_shutdown(cb: &Value) -> bool {
    matches!(cb, Value::Str(s) if s.as_bytes().eq_ignore_ascii_case(REGISTER_SHUTDOWN.as_bytes()))
}

/// `register_user_shutdown_function("session_shutdown", …)`: replaces an
/// existing entry in place, or appends one.
pub(crate) fn register(vm: &mut Vm) {
    let entry = (Value::string(REGISTER_SHUTDOWN), Vec::new());
    if let Some(slot) = vm
        .shutdown_functions
        .iter_mut()
        .find(|(cb, _)| is_session_shutdown(cb))
    {
        *slot = entry;
    } else {
        vm.shutdown_functions.push(entry);
    }
}

/// `remove_user_shutdown_function("session_shutdown")`.
pub(crate) fn remove(vm: &mut Vm) {
    vm.shutdown_functions.retain(|(cb, _)| !is_session_shutdown(cb));
}

/// `append_user_shutdown_function()` calling `session_write_close()`.
pub(crate) fn append_write_close(vm: &mut Vm) {
    vm.shutdown_functions
        .push((Value::string("session_write_close"), Vec::new()));
}

/// The `RSHUTDOWN` hook: writes and closes an active session, frees the
/// user handlers and restores the directives changed at runtime.
pub(crate) fn request_shutdown(vm: &mut Vm) {
    set_phase(Phase::Shutdown);
    let err = with_host(vm, |h| {
        let r = php_session::request_shutdown(h);
        php_session::ini::deactivate(h);
        let late = h.state().take_exception();
        r.err().or(late)
    });
    crate::headers::reset();
    if let Some(e) = err {
        report(vm, e);
    }
}

/// Reports an exception that escaped a save handler during shutdown, where
/// no PHP frame can catch it (`zend_exception_error` → "Uncaught …").
fn report(vm: &mut Vm, e: RuntimeError) {
    let t = crate::host::to_throw(vm, e);
    if let Throw::Exception(o) = t {
        report_uncaught(vm, &o);
    }
}

fn report_uncaught(vm: &mut Vm, e: &Obj) {
    let text = match vm.call_method_by_name(e, "__toString", Vec::new()) {
        Ok(Value::Str(s)) => s.to_string_lossy(),
        _ => String::new(),
    };
    let file = match vm.call_method_by_name(e, "getFile", Vec::new()) {
        Ok(Value::Str(s)) if !s.is_empty() => s,
        _ => ZStr::from("Unknown"),
    };
    let line = match vm.call_method_by_name(e, "getLine", Vec::new()) {
        Ok(Value::Int(n)) => u32::try_from(n).unwrap_or(0),
        _ => 0,
    };
    let _ = vm.error_at(1, &format!("Uncaught {text}\n  thrown"), &file, line);
}

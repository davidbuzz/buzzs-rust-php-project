// buzzs-rust-php-project - Copyright (c) 2026 Aleks <al-git001@none.at>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `php_handle_aborted_connection()` for every SAPI, and the functions that
//! read and set the connection state: `connection_aborted()`,
//! `connection_status()` and `ignore_user_abort()`.
//!
//! The state itself is [`php_sapi::connection`]: a SAPI whose write fails
//! without a VM at hand calls [`note_abort`], and the VM finishes the abort
//! at its next safe point ([`finish_pending_abort`]); a SAPI that holds
//! the VM calls [`handle_aborted_connection`] directly.

use crate::ext::{Args, VmResult};
use crate::{Throw, Vm};
use php_runtime::Value;
use php_sapi::connection;
use php_support::output::{status as output_status, OutputHost};

/// The SAPI found the client gone without a VM at hand: records
/// `PHP_CONNECTION_ABORTED` (and the `EG(exit_status)` the SAPI sets, such
/// as the CLI's 255) and asks the VM for an interrupt, after which
/// [`finish_pending_abort`] does the rest of
/// `php_handle_aborted_connection()`.
pub fn note_abort(exit_status: Option<i32>) {
    if connection::note_abort(exit_status) {
        crate::interrupt::request_interrupt();
    }
}

/// `php_handle_aborted_connection()`: `PG(connection_status) =
/// PHP_CONNECTION_ABORTED`, the output disabled for the rest of the request
/// (`php_output_set_status(PHP_OUTPUT_DISABLED)`), and a bailout — the
/// script stops, shutdown functions and destructors still run — unless
/// `ignore_user_abort` is on.
pub fn handle_aborted_connection(vm: &mut Vm) -> VmResult<()> {
    connection::abort_now();
    stop(vm)
}

/// The rest of `php_handle_aborted_connection()` once the abort is
/// recorded and marked handled.
fn stop(vm: &mut Vm) -> VmResult<()> {
    // Spelled out: `Vm` has an inherent `output(&[u8])` method.
    OutputHost::output(vm).set_status(output_status::DISABLED);
    if vm.ini_bool("ignore_user_abort") {
        Ok(())
    } else {
        Err(Throw::Exit)
    }
}

/// Finishes `php_handle_aborted_connection()` once for an abort a SAPI
/// recorded ([`note_abort`], [`php_sapi::connection::record_abort`]).
/// php-src bails out from inside `ub_write`; the writer here has no VM, so
/// the VM calls this at its next safe point, and so may a SAPI's own
/// interrupt handler that found the client gone.
pub fn finish_pending_abort(vm: &mut Vm) -> VmResult<()> {
    let Some(exit_status) = connection::take_unhandled() else {
        return Ok(());
    };
    if let Some(status) = exit_status {
        vm.exit_status = status;
    }
    stop(vm)
}

/// `connection_aborted()`.
#[allow(clippy::unnecessary_wraps)] // native handler signature
pub fn connection_aborted(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(connection::aborted()))
}

/// `connection_status()`: the `NORMAL`, `ABORTED` and `TIMEOUT` bits.
#[allow(clippy::unnecessary_wraps)] // native handler signature
pub fn connection_status(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(connection::status()))
}

/// `ignore_user_abort($enable)`: the old setting; with `$enable`,
/// `zend_alter_ini_entry_chars(..., PHP_INI_USER, PHP_INI_STAGE_RUNTIME)`.
#[allow(clippy::unnecessary_wraps)] // native handler signature
pub fn ignore_user_abort(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let old = vm.ini_bool("ignore_user_abort");
    if let Some(v) = a.opt(0) {
        let on = matches!(v, Value::Bool(true));
        let mut sink: Vec<php_support::Diagnostic> = Vec::new();
        let altered = vm.ini.alter(
            "ignore_user_abort",
            if on { b"1" } else { b"0" },
            php_support::ini::Access::USER,
            php_support::ini::Stage::Runtime,
            false,
            &mut sink,
        );
        // php-src ignores the result too: the entry is `PHP_INI_ALL` and
        // `OnUpdateBool` takes any value, so the change cannot be refused.
        debug_assert!(altered, "ignore_user_abort is PHP_INI_ALL");
    }
    Ok(Value::Bool(old))
}

// buzzs-rust-php-project - Copyright (c) 2026 Aleks <al-git001@none.at>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `PG(connection_status)` of the request this thread serves, for every SAPI.
//!
//! A SAPI's writer often has no VM at hand when the client turns out to be
//! gone (`ub_write` fails deep inside the output layer). It records the
//! abort here with [`note_abort`] and asks the VM for an interrupt; the VM
//! then runs the rest of `php_handle_aborted_connection()` once, at its next
//! safe point (`php_vm::connection`). A VM thread serves one request at a
//! time, so the state is per thread, as `PG()` is per thread in ZTS builds.

use std::cell::Cell;

/// `PHP_CONNECTION_NORMAL`.
pub const NORMAL: i64 = 0;
/// `PHP_CONNECTION_ABORTED`.
pub const ABORTED: i64 = 1;
/// `PHP_CONNECTION_TIMEOUT`.
pub const TIMEOUT: i64 = 2;

thread_local! {
    /// `PG(connection_status)`.
    static STATUS: Cell<i64> = const { Cell::new(NORMAL) };
    /// `php_handle_aborted_connection()` ran for the recorded abort.
    static HANDLED: Cell<bool> = const { Cell::new(false) };
    /// The `EG(exit_status)` the SAPI sets when its write fails (the CLI's
    /// 255 in `sapi_cli_ub_write()`); `None` leaves it.
    static EXIT_STATUS: Cell<Option<i32>> = const { Cell::new(None) };
}

/// `PG(connection_status)`.
#[must_use]
pub fn status() -> i64 {
    STATUS.with(Cell::get)
}

/// `connection_aborted()`: `PG(connection_status) & PHP_CONNECTION_ABORTED`.
#[must_use]
pub fn aborted() -> bool {
    status() & ABORTED != 0
}

/// `php_request_startup()`: `PG(connection_status) = PHP_CONNECTION_NORMAL`.
pub fn request_startup() {
    STATUS.with(|s| s.set(NORMAL));
    HANDLED.with(|h| h.set(false));
    EXIT_STATUS.with(|e| e.set(None));
}

/// `php_on_timeout()`: `PG(connection_status) |= PHP_CONNECTION_TIMEOUT`.
pub fn timed_out() {
    STATUS.with(|s| s.set(s.get() | TIMEOUT));
}

/// The SAPI found the client gone (a failed write or flush, a closed or
/// aborted connection). Records `PHP_CONNECTION_ABORTED` and the
/// `exit_status` to set, if any; `true` the first time in the request, when
/// the caller asks the VM for an interrupt.
pub fn note_abort(exit_status: Option<i32>) -> bool {
    if exit_status.is_some() {
        EXIT_STATUS.with(|e| e.set(exit_status));
    }
    let first = !aborted();
    record_abort();
    first
}

/// `PG(connection_status) = PHP_CONNECTION_ABORTED`: an assignment, as in
/// `php_handle_aborted_connection()`, so a `TIMEOUT` seen before is cleared.
/// The abort stays unhandled; see [`note_abort`] to have it handled.
pub fn record_abort() {
    if !aborted() {
        STATUS.with(|s| s.set(ABORTED));
    }
}

/// What `php_handle_aborted_connection()` has left to do: `Some(exit
/// status to set)` once per recorded abort, after which it counts as
/// handled; `None` when there is no abort or it was handled.
#[must_use]
pub fn take_unhandled() -> Option<Option<i32>> {
    if !aborted() || HANDLED.with(|h| h.replace(true)) {
        return None;
    }
    Some(EXIT_STATUS.with(Cell::get))
}

/// `php_handle_aborted_connection()` called by a SAPI that holds the VM:
/// records the abort and marks it handled; the caller does the rest.
pub fn abort_now() {
    record_abort();
    HANDLED.with(|h| h.set(true));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_abort_is_handled_once() {
        request_startup();
        assert_eq!(status(), NORMAL);
        assert_eq!(take_unhandled(), None, "no abort");
        assert!(note_abort(None));
        assert!(!note_abort(None), "the second write failure");
        assert_eq!(take_unhandled(), Some(None));
        assert_eq!(take_unhandled(), None, "handled");
        assert!(aborted());
        request_startup();
        assert!(!aborted(), "the next request starts normal");
    }

    #[test]
    fn an_abort_clears_an_earlier_timeout() {
        request_startup();
        timed_out();
        assert_eq!(status(), TIMEOUT);
        assert!(note_abort(None));
        assert_eq!(status(), ABORTED, "PG(connection_status) = ABORTED");
        timed_out();
        assert_eq!(status(), ABORTED | TIMEOUT);
    }

    #[test]
    fn the_sapi_exit_status_is_kept() {
        request_startup();
        assert!(note_abort(Some(255)));
        assert_eq!(take_unhandled(), Some(Some(255)));
        request_startup();
        abort_now();
        assert!(aborted());
        assert_eq!(take_unhandled(), None, "the caller handled it");
    }
}

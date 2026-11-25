// Derived from php-src ext/mysqli/php_mysqli_structs.h. Similarity metric to original: 28%
//   Copyright © The PHP Group and Contributors.
//   Authors: Georg Richter <georg@php.net>
//            Andrey Hristov <andrey@php.net>
//            Ulf Wendel <uw@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! The `mysqli` module globals (`MyG(...)` in `php_mysqli_structs.h`) and
//! the snapshot of the `PHP_INI_SYSTEM` directives that object destructors
//! need without access to the VM.

use std::cell::RefCell;

use php_vm::Vm;

/// `MYSQLI_REPORT_ERROR`
pub const REPORT_ERROR: i64 = 1;
/// `MYSQLI_REPORT_STRICT`
pub const REPORT_STRICT: i64 = 2;
/// `MYSQLI_REPORT_INDEX`
pub const REPORT_INDEX: i64 = 4;

/// `zend_mysqli_globals`
#[derive(Debug, Clone)]
pub struct Globals {
    /// `report_mode` (`mysqli_report()` / `mysqli_driver::$report_mode`).
    pub report_mode: i64,
    /// `error_no` of the last connect attempt.
    pub error_no: i64,
    /// `error_msg` of the last connect attempt.
    pub error_msg: Option<Vec<u8>>,
    /// `num_links`
    pub num_links: i64,
    /// `num_active_persistent`
    pub num_active_persistent: i64,
    /// `num_inactive_persistent`
    pub num_inactive_persistent: i64,
    /// `mysqli.max_links`
    pub max_links: i64,
    /// `mysqli.max_persistent`
    pub max_persistent: i64,
    /// `mysqli.allow_persistent`
    pub allow_persistent: bool,
    /// `mysqli.rollback_on_cached_plink`
    pub rollback_on_cached_plink: bool,
    /// `mysqli.allow_local_infile`
    pub allow_local_infile: bool,
    /// `mysqli.local_infile_directory`
    pub local_infile_directory: Option<Vec<u8>>,
}

impl Default for Globals {
    fn default() -> Self {
        Self {
            report_mode: REPORT_ERROR | REPORT_STRICT,
            error_no: 0,
            error_msg: None,
            num_links: 0,
            num_active_persistent: 0,
            num_inactive_persistent: 0,
            max_links: -1,
            max_persistent: -1,
            allow_persistent: true,
            rollback_on_cached_plink: false,
            allow_local_infile: false,
            local_infile_directory: None,
        }
    }
}

thread_local! {
    static G: RefCell<Globals> = RefCell::new(Globals::default());
}

/// Runs `f` with mutable access to the globals.
pub fn with<R>(f: impl FnOnce(&mut Globals) -> R) -> R {
    G.with(|g| f(&mut g.borrow_mut()))
}

/// The current report mode.
#[must_use]
pub fn report_mode() -> i64 {
    with(|g| g.report_mode)
}

/// `php_mysqli_set_error`: remembers the error of the last connect.
pub fn set_connect_error(errno: u32, msg: &[u8]) {
    with(|g| {
        g.error_no = i64::from(errno);
        g.error_msg = if msg.is_empty() {
            None
        } else {
            Some(msg.to_vec())
        };
    });
}

/// `PHP_RINIT_FUNCTION(mysqli)`: resets per-request state and snapshots
/// the system-level INI directives.
pub fn request_startup(vm: &mut Vm) {
    let max_links = vm.ini_long("mysqli.max_links");
    let max_persistent = vm.ini_long("mysqli.max_persistent");
    let allow_persistent = vm.ini_bool("mysqli.allow_persistent");
    let rollback = vm.ini_bool("mysqli.rollback_on_cached_plink");
    let allow_local_infile = vm.ini_bool("mysqli.allow_local_infile");
    let dir = vm
        .ini_string("mysqli.local_infile_directory")
        .filter(|d| !d.is_empty());
    with(|g| {
        g.error_msg = None;
        g.error_no = 0;
        g.report_mode = REPORT_ERROR | REPORT_STRICT;
        g.max_links = max_links;
        g.max_persistent = max_persistent;
        g.allow_persistent = allow_persistent;
        g.rollback_on_cached_plink = rollback;
        g.allow_local_infile = allow_local_infile;
        g.local_infile_directory = dir;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_error_roundtrip() {
        set_connect_error(1045, b"Access denied");
        with(|g| {
            assert_eq!(g.error_no, 1045);
            assert_eq!(g.error_msg.as_deref(), Some(&b"Access denied"[..]));
        });
        set_connect_error(0, b"");
        with(|g| assert!(g.error_msg.is_none()));
        assert_eq!(report_mode(), REPORT_ERROR | REPORT_STRICT);
    }
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! INI modify handlers owned by the streams layer: `OnUpdateBaseDir`
//! (`open_basedir` may only be tightened at runtime) and the
//! `open_basedir` check of `OnUpdateErrorLog`.

use std::cell::RefCell;

use php_streams::paths;
use php_vm::ext::{ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::bridge;

thread_local! {
    /// The last runtime `open_basedir` value and its expanded form (PHP
    /// stores the resolved directories).
    static EXPANDED: RefCell<Option<(Vec<u8>, Vec<u8>)>> = const { RefCell::new(None) };
}

/// The effective `open_basedir` for an INI value.
pub fn effective_open_basedir(raw: Vec<u8>) -> Vec<u8> {
    EXPANDED.with(|e| match &*e.borrow() {
        Some((r, x)) if *r == raw => x.clone(),
        _ => raw,
    })
}

/// `OnUpdateBaseDir` at runtime.
fn on_open_basedir(vm: &mut Vm, new: &[u8]) -> VmResult<bool> {
    if new.is_empty() {
        return Ok(false);
    }
    let r = bridge::run(vm, || {
        let mut out: Vec<u8> = Vec::new();
        for part in new.split(|&c| c == b':') {
            if part.is_empty() {
                continue;
            }
            let resolved = paths::expand_filepath(part)?;
            if !paths::check_open_basedir_ex(&resolved, false) {
                return None;
            }
            if !out.is_empty() {
                out.push(b':');
            }
            out.extend_from_slice(&resolved);
        }
        Some(out)
    })?;
    match r {
        Some(expanded) => {
            EXPANDED.with(|e| *e.borrow_mut() = Some((new.to_vec(), expanded)));
            bridge::invalidate_settings();
            Ok(true)
        }
        None => Ok(false),
    }
}

/// `OnUpdateErrorLog` at runtime: the log file must be inside
/// `open_basedir` (with the usual warning).
fn on_error_log(vm: &mut Vm, new: &[u8]) -> VmResult<bool> {
    if new.is_empty() || new == b"syslog" {
        return Ok(true);
    }
    bridge::run(vm, || paths::check_open_basedir(new))
}

/// Registers the handlers.
pub fn register(m: &mut ModuleBuilder<'_>) {
    m.ini_on_modify("open_basedir", on_open_basedir)
        .ini_on_modify("error_log", on_error_log);
}

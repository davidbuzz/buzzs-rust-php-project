// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `session.*` INI directives.
//!
//! `php-session` owns the directives: its [`IniStore`] runs the on-modify
//! handlers (`OnUpdateSaveHandler`, `OnUpdateSidLength`, …) and is the
//! source of truth. The directives are also registered in the VM registry
//! (for `ini_get()`, `ini_get_all()` and `-d` values), with an
//! `ini_on_modify` hook each:
//!
//! * `ini_set()`/`ini_alter()`: the hook runs the library's handler before
//!   the VM stores the value, and rejects the change when it fails, as
//!   `zend_alter_ini_entry_ex()` would.
//! * `ini_restore()`: the VM calls the hook after restoring; a thin
//!   `ini_restore()` wrapper flags the call so the hook restores the
//!   library entry instead of setting it.
//! * After every session entry point, [`sync`] copies changed library
//!   values into the VM registry (session functions such as
//!   `session_name()` or `session_start(['name' => …])` alter directives
//!   internally).
//!
//! [`IniStore`]: php_session::ini::IniStore

use std::cell::Cell;

use php_runtime::Value;
use php_session::ini::DIRECTIVES;
use php_support::ini::{Access, IniDirective, ModifyHandler, Stage};
use php_support::Diagnostic;
use php_vm::ext::{Args, ModuleBuilder, NativeFn, VmResult};
use php_vm::Vm;

use crate::host::{to_throw, with_host_unsynced, VmHost};

thread_local! {
    static ORIG_INI_RESTORE: Cell<Option<NativeFn>> = const { Cell::new(None) };
    /// `ini_restore()` is running: the modify hooks restore.
    static RESTORING: Cell<bool> = const { Cell::new(false) };
}

/// Whether the current modify-hook call comes from `ini_restore()`.
pub(crate) fn restoring() -> bool {
    RESTORING.with(Cell::get)
}

/// The modify hook of `DIRECTIVES[I]`. Per-directory configuration (`mod_php`'s
/// `php_value`, `.htaccess`) reaches it at the activation stages with
/// `PHP_INI_PERDIR` access, which `PHP_INI_PERDIR` directives such as
/// `session.upload_progress.*` accept and `ini_set()` does not.
fn on_modify<const I: usize>(vm: &mut Vm, value: &[u8]) -> VmResult<bool> {
    use php_session::ini::{alter, IniStage as SessionStage, INI_PERDIR};
    use php_vm::ext::IniStage;
    let name = DIRECTIVES[I].name;
    let stage = vm.ini_modify_stage();
    with_host_unsynced(vm, |h| {
        let ok = if restoring() {
            php_session::ini::ini_restore(h, name);
            true
        } else {
            match stage {
                IniStage::Activate => {
                    alter(h, name, value, INI_PERDIR, SessionStage::Activate) == Some(true)
                }
                IniStage::Htaccess => {
                    alter(h, name, value, INI_PERDIR, SessionStage::Htaccess) == Some(true)
                }
                IniStage::Runtime | IniStage::Restore => {
                    php_session::ini::ini_set(h, name, value) == Some(true)
                }
            }
        };
        match h.state().take_exception() {
            Some(e) => Err(to_throw(h.vm, e)),
            None => Ok(ok),
        }
    })
}

macro_rules! hooks {
    ($($i:literal)*) => {
        [$(on_modify::<$i> as php_vm::ext::IniModifyFn),*]
    };
}

/// One hook per directive, in [`DIRECTIVES`] order.
const HOOKS: [php_vm::ext::IniModifyFn; 31] =
    hooks!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30);

/// Registers the directives in the VM registry (module `session`) with
/// their modify hooks, and the `ini_restore()` wrapper.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    debug_assert_eq!(DIRECTIVES.len(), HOOKS.len());
    for (d, hook) in DIRECTIVES.iter().zip(HOOKS) {
        m.ini(IniDirective::new(
            d.name,
            Some(d.default),
            Access(d.access),
            ModifyHandler::None,
            "session",
        ));
        m.ini_on_modify(d.name, hook);
    }
    let vm = m.vm();
    if let Some(b) = vm.native_function("ini_restore") {
        ORIG_INI_RESTORE.with(|c| c.set(Some(b.handler)));
        vm.register_function("ini_restore(string $option): void", ini_restore_hook);
    }
}

/// The configuration value (`php.ini`/`-d`) of a directive.
pub(crate) fn config_value(vm: &Vm, name: &str) -> Option<Vec<u8>> {
    vm.ini.config_value(name).map(<[u8]>::to_vec)
}

/// Copies the library's values into the VM registry.
pub(crate) fn sync(h: &mut VmHost<'_>) {
    let mut sink: Vec<Diagnostic> = Vec::new();
    for d in DIRECTIVES {
        let Some(e) = h.state().ini.entry(d.name).cloned() else {
            continue;
        };
        if !e.modified {
            h.vm.ini.restore(d.name, Stage::Deactivate, &mut sink);
        }
        if h.vm.ini.raw(d.name) != Some(e.value.as_slice()) {
            h.vm.ini.alter(
                d.name,
                &e.value,
                Access::SYSTEM,
                Stage::Runtime,
                true,
                &mut sink,
            );
        }
    }
}

fn ini_restore_hook(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(orig) = ORIG_INI_RESTORE.with(Cell::get) else {
        return Ok(Value::Null);
    };
    let was = RESTORING.with(|r| r.replace(true));
    let res = orig(vm, a);
    RESTORING.with(|r| r.set(was));
    res
}

//! The `session.*` INI directives.
//!
//! `php-session` owns the directives: its [`IniStore`] runs the on-modify
//! handlers (`OnUpdateSaveHandler`, `OnUpdateSidLength`, …) and is the
//! source of truth. The VM's INI registry has no extension callbacks, so
//! the directives are also registered there without a modify handler (for
//! `ini_get()`, `ini_get_all()` and `-d` values) and kept in sync:
//!
//! * `ini_set()`/`ini_alter()`/`ini_restore()` are wrapped; for a session
//!   directive the library runs the handler and decides, as
//!   `zend_alter_ini_entry_ex()` would.
//! * After every session entry point, [`sync`] copies changed library
//!   values into the VM registry (session functions such as
//!   `session_name()` or `session_start(['name' => …])` alter directives
//!   internally).
//!
//! [`IniStore`]: php_session::ini::IniStore

use std::cell::Cell;

use php_runtime::Value;
use php_session::ini::{directive_index, DIRECTIVES};
use php_support::ini::{Access, IniDirective, ModifyHandler, Stage};
use php_support::Diagnostic;
use php_vm::ext::{Args, ModuleBuilder, NativeFn, VmResult};
use php_vm::Vm;

use crate::host::{to_throw, with_host, VmHost};

thread_local! {
    static ORIG_INI_SET: Cell<Option<NativeFn>> = const { Cell::new(None) };
    static ORIG_INI_RESTORE: Cell<Option<NativeFn>> = const { Cell::new(None) };
}

/// Registers the directives in the VM registry (module `session`).
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    for d in DIRECTIVES {
        m.ini(IniDirective::new(
            d.name,
            Some(d.default),
            Access(d.access),
            ModifyHandler::None,
            "session",
        ));
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
            h.vm
                .ini
                .alter(d.name, &e.value, Access::SYSTEM, Stage::Runtime, true, &mut sink);
        }
    }
}

/// Installs the `ini_set()`/`ini_alter()`/`ini_restore()` wrappers.
pub(crate) fn install_hooks(m: &mut ModuleBuilder<'_>) {
    let vm = m.vm();
    if let Some(b) = vm.native_function("ini_set") {
        ORIG_INI_SET.with(|c| c.set(Some(b.handler)));
        vm.register_function(
            "ini_set(string $option, string|int|float|bool|null $value): string|false",
            ini_set_hook,
        );
        vm.register_function(
            "ini_alter(string $option, string|int|float|bool|null $value): string|false",
            ini_set_hook,
        );
    }
    if let Some(b) = vm.native_function("ini_restore") {
        ORIG_INI_RESTORE.with(|c| c.set(Some(b.handler)));
        vm.register_function("ini_restore(string $option): void", ini_restore_hook);
    }
}

/// The string `ini_set()` stores for its `$value`.
fn ini_value(vm: &mut Vm, v: &Value) -> VmResult<Vec<u8>> {
    Ok(match v {
        Value::Bool(true) => b"1".to_vec(),
        Value::Bool(false) | Value::Null => Vec::new(),
        other => vm.value_to_string(other)?.as_bytes().to_vec(),
    })
}

fn run_orig(
    cell: &'static std::thread::LocalKey<Cell<Option<NativeFn>>>,
    vm: &mut Vm,
    a: &mut Args,
) -> VmResult<Value> {
    match cell.with(Cell::get) {
        Some(f) => f(vm, a),
        None => Ok(Value::Bool(false)),
    }
}

fn ini_set_hook(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let name = a.str(0).to_string_lossy();
    if directive_index(&name).is_none() {
        return run_orig(&ORIG_INI_SET, vm, a);
    }
    let value = ini_value(vm, &a.at(1))?;
    with_host(vm, |h| {
        let old = h.state().ini.get(&name).map(<[u8]>::to_vec);
        let ok = php_session::ini::ini_set(h, &name, &value);
        if let Some(e) = h.state().take_exception() {
            return Err(to_throw(h.vm, e));
        }
        Ok(match (ok, old) {
            (Some(true), Some(old)) => Value::string(old),
            _ => Value::Bool(false),
        })
    })
}

fn ini_restore_hook(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let name = a.str(0).to_string_lossy();
    if directive_index(&name).is_none() {
        return run_orig(&ORIG_INI_RESTORE, vm, a);
    }
    with_host(vm, |h| {
        php_session::ini::ini_restore(h, &name);
        if let Some(e) = h.state().take_exception() {
            return Err(to_throw(h.vm, e));
        }
        Ok(Value::Null)
    })
}

//! The `phar.*` INI directives.
//!
//! The VM's INI registry has no extension modify handlers, so the
//! directives are registered without one (for `ini_get()`, `-d` values and
//! `ini_get_all()`) and mirrored into `php-phar`'s registry:
//!
//! * at startup the (possibly `-d`-overridden) values become the
//!   "original" values (`PHAR_G(readonly_orig)`);
//! * `ini_set()` / `ini_alter()` / `ini_restore()` are wrapped so that
//!   `phar_ini_modify_handler` decides (a value that was originally on
//!   cannot be turned off at runtime).

use std::cell::Cell;

use php_phar::registry as reg;
use php_runtime::Value;
use php_support::ini::{parse_bool, Access, IniDirective, ModifyHandler};
use php_vm::ext::{Args, ModuleBuilder, NativeFn, VmResult};
use php_vm::Vm;

thread_local! {
    static ORIG_INI_SET: Cell<Option<NativeFn>> = const { Cell::new(None) };
    static ORIG_INI_ALTER: Cell<Option<NativeFn>> = const { Cell::new(None) };
    static ORIG_INI_RESTORE: Cell<Option<NativeFn>> = const { Cell::new(None) };
}

const BOOL_DIRECTIVES: [&str; 2] = ["phar.readonly", "phar.require_hash"];

/// Registers the directives and the `ini_set()` family wrappers.
pub fn register(m: &mut ModuleBuilder<'_>) {
    for name in BOOL_DIRECTIVES {
        m.ini(IniDirective::new(
            name,
            Some("1"),
            Access::ALL,
            ModifyHandler::None,
            "Phar",
        ));
    }
    m.ini(IniDirective::new(
        "phar.cache_list",
        Some(""),
        Access::SYSTEM,
        ModifyHandler::None,
        "Phar",
    ));
    let vm = m.vm();
    for name in BOOL_DIRECTIVES {
        let v = vm.ini_string(name).unwrap_or_default();
        reg::set_ini(name, parse_bool(&v), true);
    }
    if let Some(list) = vm.ini_string("phar.cache_list") {
        if !list.is_empty() {
            reg::load_cache_list(&list);
        }
    }
    install_hooks(vm);
}

fn install_hooks(vm: &mut Vm) {
    if let Some(b) = vm.native_function("ini_set") {
        ORIG_INI_SET.with(|c| c.set(Some(b.handler)));
        vm.register_function(
            "ini_set(string $option, string|int|float|bool|null $value): string|false",
            ini_set_hook,
        );
    }
    if let Some(b) = vm.native_function("ini_alter") {
        ORIG_INI_ALTER.with(|c| c.set(Some(b.handler)));
        vm.register_function(
            "ini_alter(string $option, string|int|float|bool|null $value): string|false",
            ini_alter_hook,
        );
    }
    if let Some(b) = vm.native_function("ini_restore") {
        ORIG_INI_RESTORE.with(|c| c.set(Some(b.handler)));
        vm.register_function("ini_restore(string $option): void", ini_restore_hook);
    }
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

fn is_bool_directive(name: &[u8]) -> Option<&'static str> {
    BOOL_DIRECTIVES
        .iter()
        .copied()
        .find(|d| d.as_bytes() == name)
}

/// The string `ini_set()` stores for its `$value`.
fn ini_value(vm: &mut Vm, v: &Value) -> VmResult<Vec<u8>> {
    Ok(match v {
        Value::Bool(true) => b"1".to_vec(),
        Value::Bool(false) | Value::Null => Vec::new(),
        other => vm.value_to_string(other)?.as_bytes().to_vec(),
    })
}

fn set_common(
    cell: &'static std::thread::LocalKey<Cell<Option<NativeFn>>>,
    vm: &mut Vm,
    a: &mut Args,
) -> VmResult<Value> {
    let name = a.str(0);
    let Some(directive) = is_bool_directive(name.as_bytes()) else {
        return run_orig(cell, vm, a);
    };
    let value = ini_value(vm, &a.at(1))?;
    let old_lib = if directive == "phar.readonly" {
        reg::readonly()
    } else {
        reg::require_hash()
    };
    if !reg::set_ini(directive, parse_bool(&value), false) {
        return Ok(Value::Bool(false));
    }
    let r = run_orig(cell, vm, a)?;
    if matches!(r, Value::Bool(false)) {
        // The VM refused the change: undo ours.
        reg::set_ini(directive, old_lib, false);
    }
    Ok(r)
}

fn ini_set_hook(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    set_common(&ORIG_INI_SET, vm, a)
}

fn ini_alter_hook(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    set_common(&ORIG_INI_ALTER, vm, a)
}

fn ini_restore_hook(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let name = a.str(0);
    let r = run_orig(&ORIG_INI_RESTORE, vm, a)?;
    if let Some(directive) = is_bool_directive(name.as_bytes()) {
        let v = vm.ini_string(directive).unwrap_or_default();
        reg::set_ini(directive, parse_bool(&v), false);
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::is_bool_directive;

    #[test]
    fn directives() {
        assert_eq!(is_bool_directive(b"phar.readonly"), Some("phar.readonly"));
        assert_eq!(is_bool_directive(b"phar.cache_list"), None);
    }
}

//! The `mbstring.*` INI table and its on-modify semantics.
//!
//! The VM's INI registry has no extension callbacks, so the directives are
//! registered without a modify handler and the `OnUpdate_mbstring_*`
//! handlers of `php-mbstring` ([`MbState::ini_set`]) are run by wrapping
//! `ini_set()`/`ini_alter()`/`ini_restore()`: the wrapper runs the handler
//! first and only lets the VM store the value when it accepts it, exactly
//! like `zend_alter_ini_entry_ex()`. PHP's own `default_charset`,
//! `internal_encoding`, `input_encoding` and `output_encoding` run
//! mbstring's `php_internal_encoding_changed` hook the same way.

use std::cell::Cell;

use php_mbstring::MbState;
use php_runtime::{Value, ZStr};
use php_support::ini::{Access, IniDirective, ModifyHandler};
use php_support::Diagnostic;
use php_vm::ext::{Args, ModuleBuilder, NativeFn, VmResult};
use php_vm::Vm;

use crate::state;

/// `E_WARNING`.
const E_WARNING: i64 = 2;
/// `E_NOTICE`.
const E_NOTICE: i64 = 8;
/// `E_DEPRECATED`.
const E_DEPRECATED: i64 = 8192;

/// Directives whose handler lives in `php-mbstring`, in registration order.
const HANDLED: [&str; 9] = [
    "mbstring.language",
    "mbstring.detect_order",
    "mbstring.http_input",
    "mbstring.http_output",
    "mbstring.internal_encoding",
    "mbstring.substitute_character",
    "mbstring.encoding_translation",
    "mbstring.http_output_conv_mimetypes",
    "mbstring.strict_detection",
];

/// PHP core directives that trigger mbstring's encoding hook.
const CORE_HOOKED: [&str; 4] = [
    "default_charset",
    "internal_encoding",
    "input_encoding",
    "output_encoding",
];

thread_local! {
    static ORIG_INI_SET: Cell<Option<NativeFn>> = const { Cell::new(None) };
    static ORIG_INI_RESTORE: Cell<Option<NativeFn>> = const { Cell::new(None) };
}

/// Registers the INI entries (`PHP_INI_BEGIN` in `mbstring.c`).
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    let none = ModifyHandler::None;
    let dirs = [
        ("mbstring.language", Some("neutral"), Access::ALL, none),
        ("mbstring.detect_order", None, Access::ALL, none),
        ("mbstring.http_input", None, Access::ALL, none),
        ("mbstring.http_output", None, Access::ALL, none),
        ("mbstring.internal_encoding", None, Access::ALL, none),
        ("mbstring.substitute_character", None, Access::ALL, none),
        (
            "mbstring.encoding_translation",
            Some("0"),
            Access(Access::SYSTEM.0 | Access::PERDIR.0),
            ModifyHandler::Bool,
        ),
        (
            "mbstring.http_output_conv_mimetypes",
            Some(r"^(text/|application/xhtml\+xml)"),
            Access::ALL,
            none,
        ),
        ("mbstring.strict_detection", Some("0"), Access::ALL, ModifyHandler::Bool),
        ("mbstring.regex_stack_limit", Some("100000"), Access::ALL, ModifyHandler::Long),
        ("mbstring.regex_retry_limit", Some("1000000"), Access::ALL, ModifyHandler::Long),
    ];
    for (name, default, access, handler) in dirs {
        m.ini(IniDirective::new(name, default, access, handler, "mbstring"));
    }
}

/// `MINIT` + `RINIT`: builds the state from the startup INI values and
/// reports the handlers' diagnostics as `PHP Startup` errors.
pub(crate) fn startup(vm: &mut Vm) {
    let mut settings: Vec<(&str, Vec<u8>)> = Vec::new();
    for name in CORE_HOOKED.iter().chain(HANDLED.iter()) {
        if let Some(v) = vm.ini.raw(name) {
            settings.push((name, v.to_vec()));
        }
    }
    let refs: Vec<(&str, &[u8])> = settings.iter().map(|(k, v)| (*k, v.as_slice())).collect();
    let mut st = MbState::with_ini(&refs);
    if let Some(v) = raw(vm, "mbstring.http_output_conv_mimetypes") {
        st.ini_set("mbstring.http_output_conv_mimetypes", Some(&v), false);
    }
    let diags = st.take_diagnostics();
    state::replace(st);
    // Startup errors are displayed before the request: they do not count
    // as sent headers (other modules' INI handlers check that).
    let headers_sent = vm.headers_sent;
    for d in diags {
        let level = match d.level {
            php_mbstring::Level::Warning => E_WARNING,
            php_mbstring::Level::Notice => E_NOTICE,
            php_mbstring::Level::Deprecated => E_DEPRECATED,
        };
        let _ = vm.error_at(
            level,
            &format!("PHP Startup: {}", d.message),
            &ZStr::from("Unknown"),
            0,
        );
    }
    vm.headers_sent = headers_sent;
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

/// Stores a value without running any handler (`zend_alter_ini_entry` from
/// `mb_language()`, whose handler already ran).
pub(crate) fn store_raw(vm: &mut Vm, name: &str, value: &[u8]) {
    let mut sink: Vec<Diagnostic> = Vec::new();
    let _ = vm.ini.ini_set(name, value, &mut sink);
}

/// The stored value (`None` for `NULL`).
fn raw(vm: &Vm, name: &str) -> Option<Vec<u8>> {
    vm.ini.raw(name).map(<[u8]>::to_vec)
}

fn is_hooked(name: &str) -> bool {
    HANDLED.contains(&name) || CORE_HOOKED.contains(&name)
}

/// The string `ini_set()` stores for its `$value`.
fn ini_value(vm: &mut Vm, v: &Value) -> VmResult<Vec<u8>> {
    Ok(match v {
        Value::Bool(true) => b"1".to_vec(),
        Value::Bool(false) | Value::Null => Vec::new(),
        other => vm.value_to_string(other)?.as_bytes().to_vec(),
    })
}

fn run_orig(cell: &'static std::thread::LocalKey<Cell<Option<NativeFn>>>, vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    match cell.with(Cell::get) {
        Some(f) => f(vm, a),
        None => Ok(Value::Bool(false)),
    }
}

fn ini_set_hook(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let name = a.str(0).to_string_lossy();
    if !is_hooked(&name) || !vm.ini.contains(&name) {
        return run_orig(&ORIG_INI_SET, vm, a);
    }
    let value = ini_value(vm, &a.at(1))?;
    if CORE_HOOKED.contains(&name.as_str()) || name == "mbstring.encoding_translation" {
        let r = run_orig(&ORIG_INI_SET, vm, a)?;
        if !matches!(r, Value::Bool(false)) {
            let stored = raw(vm, &name);
            state::with(|s| s.ini_set(&name, stored.as_deref(), true));
            state::flush_diagnostics(vm)?;
        }
        return Ok(r);
    }
    // The handler runs before the value is stored (and its side effects
    // stay even when it fails, as in PHP).
    let ok = state::with(|s| s.ini_set(&name, Some(&value), true));
    state::flush_diagnostics(vm)?;
    if !ok {
        return Ok(Value::Bool(false));
    }
    run_orig(&ORIG_INI_SET, vm, a)
}

fn ini_restore_hook(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let name = a.str(0).to_string_lossy();
    let r = run_orig(&ORIG_INI_RESTORE, vm, a)?;
    if is_hooked(&name) {
        let stored = raw(vm, &name);
        state::with(|s| s.ini_set(&name, stored.as_deref(), true));
        state::flush_diagnostics(vm)?;
    }
    Ok(r)
}

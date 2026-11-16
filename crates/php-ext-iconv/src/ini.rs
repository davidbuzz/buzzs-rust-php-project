// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `iconv.*` INI entries and their on-modify semantics.
//!
//! The VM's INI registry has no extension callbacks, so the directives are
//! registered without a handler and `OnUpdateInputEncoding` & co. are run
//! by wrapping `ini_set()`/`ini_alter()`/`ini_restore()` (the wrapper
//! chains to whatever handler was registered before, e.g. mbstring's):
//! values of [`php_iconv::CSNMAXLEN`] bytes or more are rejected, and
//! runtime changes raise `Use of iconv.* is deprecated`.
//!
//! The library's [`Iconv`] state is rebuilt from the stored INI values on
//! every call ([`state`]), so it can never get out of sync.

use std::cell::Cell;

use php_iconv::{CoreEncodings, Iconv, IconvIni, CSNMAXLEN};
use php_runtime::Value;
use php_support::ini::{Access, IniDirective, ModifyHandler};
use php_vm::ext::{Args, ModuleBuilder, NativeFn, VmResult};
use php_vm::Vm;

/// The directives, in registration order.
pub(crate) const DIRECTIVES: [&str; 3] = [
    "iconv.input_encoding",
    "iconv.output_encoding",
    "iconv.internal_encoding",
];

/// What PHP's heap reports for an idle request; [`state`] subtracts it
/// (plus the inputs) from `memory_limit` to model the engine's
/// out-of-memory check for the large buffers iconv allocates.
const BASE_USAGE: usize = 512 * 1024;

thread_local! {
    /// Bit `i` set: `DIRECTIVES[i]` was changed at runtime (the entry's
    /// `modified` flag, which decides whether `ini_restore()` runs the
    /// handler).
    static MODIFIED: Cell<u8> = const { Cell::new(0) };
    static ORIG_INI_SET: Cell<Option<NativeFn>> = const { Cell::new(None) };
    static ORIG_INI_ALTER: Cell<Option<NativeFn>> = const { Cell::new(None) };
    static ORIG_INI_RESTORE: Cell<Option<NativeFn>> = const { Cell::new(None) };
}

/// Registers the entries (`PHP_INI_BEGIN` in `iconv.c`) and installs the
/// `ini_set()` wrappers.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    for name in DIRECTIVES {
        m.ini(IniDirective::new(
            name,
            Some(""),
            Access::ALL,
            ModifyHandler::None,
            "iconv",
        ));
    }
    let vm = m.vm();
    if let Some(b) = vm.native_function("ini_set") {
        ORIG_INI_SET.with(|c| c.set(Some(b.handler)));
        vm.register_function(
            "ini_set(string $option, mixed $value): string|false",
            ini_set_hook,
        );
    }
    if let Some(b) = vm.native_function("ini_alter") {
        ORIG_INI_ALTER.with(|c| c.set(Some(b.handler)));
        vm.register_function(
            "ini_alter(string $option, mixed $value): string|false",
            ini_alter_hook,
        );
    }
    if let Some(b) = vm.native_function("ini_restore") {
        ORIG_INI_RESTORE.with(|c| c.set(Some(b.handler)));
        vm.register_function("ini_restore(string $option): void", ini_restore_hook);
    }
}

/// Parses `memory_limit` (`zend_ini_parse_quantity`); `None` = unlimited.
fn memory_limit(vm: &Vm) -> Option<usize> {
    let raw = vm.ini_string("memory_limit")?;
    let s = String::from_utf8_lossy(&raw);
    let s = s.trim();
    let (digits, mult) = match s.as_bytes().last()? {
        b'k' | b'K' => (&s[..s.len() - 1], 1usize << 10),
        b'm' | b'M' => (&s[..s.len() - 1], 1 << 20),
        b'g' | b'G' => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    let n: i64 = digits.trim().parse().ok()?;
    usize::try_from(n)
        .ok()
        .filter(|&n| n > 0)
        .map(|n| n.saturating_mul(mult))
}

/// The configured `memory_limit` in bytes, for the fatal error text.
pub(crate) fn configured_memory_limit(vm: &Vm) -> usize {
    memory_limit(vm).unwrap_or(0)
}

/// Heap size of a `zend_string` of `len` bytes (header + NUL, 8-aligned).
pub(crate) fn zstr_cost(len: usize) -> usize {
    (24 + len + 1 + 7) & !7
}

/// Heap size of a value built by this extension (strings, and arrays of
/// strings with 32-byte buckets).
pub(crate) fn value_cost(v: &Value) -> usize {
    match v {
        Value::Str(s) => zstr_cost(s.len()),
        Value::Array(a) => 56 + a.values().map(|e| 32 + value_cost(e)).sum::<usize>(),
        _ => 0,
    }
}

/// Models the engine's `memory_limit` check for a result: the VM does not
/// account memory, so the idle-request baseline plus the live inputs plus
/// the result are compared with the limit, raising PHP's out-of-memory
/// fatal error (`requested` is the size of the result) when they exceed it.
pub(crate) fn check_memory(vm: &mut Vm, input_len: usize, result: &Value) -> VmResult<()> {
    let Some(limit) = memory_limit(vm) else {
        return Ok(());
    };
    let cost = value_cost(result);
    if BASE_USAGE + input_len + cost > limit {
        return Err(vm.fatal(&format!(
            "Allowed memory size of {limit} bytes exhausted (tried to allocate {} bytes)",
            match result {
                Value::Str(_) => cost,
                _ => 32,
            }
        )));
    }
    Ok(())
}

/// Builds the module state from the current INI values. `input_len` is
/// the size of the argument strings already held in memory.
pub(crate) fn state(vm: &Vm, input_len: usize) -> Iconv {
    let get = |name: &str| vm.ini_string(name).unwrap_or_default();
    Iconv {
        ini: IconvIni {
            input_encoding: get("iconv.input_encoding"),
            output_encoding: get("iconv.output_encoding"),
            internal_encoding: get("iconv.internal_encoding"),
        },
        core: CoreEncodings {
            default_charset: get("default_charset"),
            internal_encoding: get("internal_encoding"),
            input_encoding: get("input_encoding"),
            output_encoding: get("output_encoding"),
        },
        memory_limit: memory_limit(vm)
            .map(|l| l.saturating_sub(BASE_USAGE).saturating_sub(input_len)),
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

/// Runs the on-modify handler for a runtime change of `name` to `value`:
/// `false` when the value is rejected, otherwise raises the deprecation.
pub(crate) fn on_modify(vm: &mut Vm, name: &str, value: &[u8]) -> VmResult<bool> {
    if value.len() >= CSNMAXLEN {
        return Ok(false);
    }
    vm.php_deprecated(&format!("Use of {name} is deprecated"))?;
    Ok(true)
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

fn index_of(name: &[u8]) -> Option<usize> {
    DIRECTIVES.iter().position(|d| d.as_bytes() == name)
}

/// Records a successful runtime change of `name`.
pub(crate) fn mark_modified(name: &str) {
    if let Some(i) = index_of(name.as_bytes()) {
        MODIFIED.with(|m| m.set(m.get() | (1 << i)));
    }
}

/// `RINIT`: nothing is modified at request start.
pub(crate) fn reset() {
    MODIFIED.with(|m| m.set(0));
}

fn set_hook(
    cell: &'static std::thread::LocalKey<Cell<Option<NativeFn>>>,
    vm: &mut Vm,
    a: &mut Args,
) -> VmResult<Value> {
    let name = a.str(0);
    if index_of(name.as_bytes()).is_none() {
        return run_orig(cell, vm, a);
    }
    let name = name.to_string_lossy();
    let value = ini_value(vm, &a.at(1))?;
    if !on_modify(vm, &name, &value)? {
        return Ok(Value::Bool(false));
    }
    let r = run_orig(cell, vm, a)?;
    if !matches!(r, Value::Bool(false)) {
        mark_modified(&name);
    }
    Ok(r)
}

fn ini_set_hook(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    set_hook(&ORIG_INI_SET, vm, a)
}

fn ini_alter_hook(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    set_hook(&ORIG_INI_ALTER, vm, a)
}

fn ini_restore_hook(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let name = a.str(0);
    if let Some(i) = index_of(name.as_bytes()) {
        let bit = 1u8 << i;
        if MODIFIED.with(Cell::get) & bit != 0 {
            MODIFIED.with(|m| m.set(m.get() & !bit));
            // zend_restore_ini_entry() runs the handler (stage RUNTIME)
            // with the original value, which passes the length check.
            vm.php_deprecated(&format!("Use of {} is deprecated", name.to_string_lossy()))?;
        }
    }
    run_orig(&ORIG_INI_RESTORE, vm, a)
}

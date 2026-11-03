//! `gc_collect_cycles()`, `gc_enable()`, `gc_disable()`, `gc_enabled()`,
//! `gc_status()` and the `zend.enable_gc` modify handler
//! (`Zend/zend_builtin_functions.c`, `OnUpdateGCEnabled`).

// Builtins have the fixed `BuiltinFn` signature.
#![allow(clippy::unnecessary_wraps)]

use crate::builtins::reg;
use crate::func::Args;
use crate::vm::{Vm, VmResult};
use php_runtime::{ArrayKey, HashTable, Value, ZArray, ZStr};

fn gc_collect_cycles(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    vm.gc_collect_cycles().map(Value::Int)
}

fn gc_enabled(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(super::enabled()))
}

/// `zend_alter_ini_entry_chars("zend.enable_gc", ..., ZEND_INI_USER)`.
fn set_ini(vm: &mut Vm, on: bool) {
    let v: &[u8] = if on { b"1" } else { b"0" };
    let mut sink: Vec<php_support::Diagnostic> = Vec::new();
    // `ini_set` of the registry does not run the VM's modify hooks.
    let _ = vm.ini.ini_set("zend.enable_gc", v, &mut sink);
    super::set_enabled(on);
}

fn gc_enable(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    set_ini(vm, true);
    Ok(Value::Null)
}

fn gc_disable(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    set_ini(vm, false);
    Ok(Value::Null)
}

fn gc_status(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let st = super::status();
    let mut t = HashTable::with_capacity(12);
    let mut put = |k: &str, v: Value| {
        t.insert(ArrayKey::Str(ZStr::from(k)), v);
    };
    let int = |n: u64| Value::Int(i64::try_from(n).unwrap_or(i64::MAX));
    put("running", Value::Bool(st.running));
    put("protected", Value::Bool(st.protected));
    put("full", Value::Bool(st.full));
    put("runs", int(st.runs));
    put("collected", int(st.collected));
    put("threshold", int(st.threshold as u64));
    put("buffer_size", int(st.buffer_size as u64));
    put("roots", int(st.roots as u64));
    put("application_time", Value::Float(st.application_time));
    put("collector_time", Value::Float(st.collector_time));
    put("destructor_time", Value::Float(st.destructor_time));
    put("free_time", Value::Float(st.free_time));
    Ok(Value::Array(ZArray::from_table(t)))
}

/// `OnUpdateGCEnabled`: `ini_set('zend.enable_gc', ...)` toggles the
/// collector.
fn on_update_enable_gc(_vm: &mut Vm, v: &[u8]) -> VmResult<bool> {
    super::set_enabled(php_support::ini::parse_bool(v));
    Ok(true)
}

pub(super) fn register(vm: &mut Vm) {
    reg(vm, "gc_collect_cycles(): int", gc_collect_cycles);
    reg(vm, "gc_enabled(): bool", gc_enabled);
    reg(vm, "gc_enable(): void", gc_enable);
    reg(vm, "gc_disable(): void", gc_disable);
    reg(vm, "gc_status(): array", gc_status);
    vm.ini_modify_hooks
        .insert("zend.enable_gc".to_string(), on_update_enable_gc);
}

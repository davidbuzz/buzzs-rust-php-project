//! Helpers for builtin implementations.

use crate::func::Args;
use crate::vm::{Vm, VmResult};
use php_runtime::{Value, ZArray, ZStr};

/// String argument (already coerced by the signature).
pub(crate) fn s(a: &Args, i: usize) -> ZStr {
    match a.at(i) {
        Value::Str(s) => s,
        Value::Int(n) => ZStr::from(n.to_string()),
        _ => ZStr::empty(),
    }
}

/// Integer argument.
pub(crate) fn i(a: &Args, i: usize) -> i64 {
    match a.at(i) {
        Value::Int(n) => n,
        Value::Bool(b) => i64::from(b),
        #[allow(clippy::cast_possible_truncation)]
        Value::Float(f) => f as i64,
        _ => 0,
    }
}

/// Float argument.
pub(crate) fn f(a: &Args, i: usize) -> f64 {
    match a.at(i) {
        Value::Float(f) => f,
        #[allow(clippy::cast_precision_loss)]
        Value::Int(n) => n as f64,
        _ => 0.0,
    }
}

/// Bool argument.
pub(crate) fn b(a: &Args, i: usize) -> bool {
    matches!(a.at(i), Value::Bool(true))
}

/// Array argument.
pub(crate) fn arr(a: &Args, i: usize) -> ZArray {
    match a.at(i) {
        Value::Array(x) => x,
        _ => ZArray::new(),
    }
}

/// Optional argument (passed and not null).
pub(crate) fn opt(a: &Args, i: usize) -> Option<Value> {
    a.get(i).filter(|v| !v.is_null())
}

/// A string value.
pub(crate) fn sv(s: impl Into<ZStr>) -> Value {
    Value::Str(s.into())
}

/// The running builtin's display name: `Class::method` for methods
/// (from the builtin frame's scope), else the function name.
fn builtin_display(vm: &Vm, a: &Args) -> String {
    let Some(b) = &a.func else {
        return String::new();
    };
    if let Some(f) = vm.frames.last() {
        if f.kind == crate::vm::FrameKind::Builtin
            && f.builtin_name.as_ref().is_some_and(|n| n == &b.name)
            && f.scope.is_some()
        {
            if let Some(func) = &f.func {
                return vm.func_display(func, f.scope.as_ref());
            }
        }
    }
    a.func_name()
}

/// Throws a `ValueError` "func(): Argument #n ($name) msg".
pub(crate) fn value_error(vm: &mut Vm, a: &Args, n: usize, msg: &str) -> crate::vm::Throw {
    let fname = builtin_display(vm, a);
    let pname = a
        .func
        .as_ref()
        .and_then(|f| f.params.get(n - 1).map(|p| p.name.to_string_lossy()))
        .unwrap_or_default();
    vm.make_throw(
        "ValueError",
        &format!("{fname}(): Argument #{n} (${pname}) {msg}"),
    )
}

/// Throws a `TypeError` "func(): Argument #n ($name) msg".
pub(crate) fn type_error(vm: &mut Vm, a: &Args, n: usize, msg: &str) -> crate::vm::Throw {
    let fname = builtin_display(vm, a);
    let pname = a
        .func
        .as_ref()
        .and_then(|f| f.params.get(n - 1).map(|p| p.name.to_string_lossy()))
        .unwrap_or_default();
    vm.make_throw(
        "TypeError",
        &format!("{fname}(): Argument #{n} (${pname}) {msg}"),
    )
}

/// Converts any value to a string (with `__toString`).
pub(crate) fn to_str(vm: &mut Vm, v: &Value) -> VmResult<ZStr> {
    vm.to_zstr(v)
}

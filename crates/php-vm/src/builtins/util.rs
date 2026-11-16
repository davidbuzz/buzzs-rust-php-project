// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Helpers for builtin implementations.

use crate::func::Args;
use crate::vm::{Vm, VmResult};
use php_runtime::{Value, ZArray, ZStr};

/// String argument (already coerced by the signature).
pub(crate) fn s(a: &Args, i: usize) -> ZStr {
    match a.values.get(i) {
        Some(Value::Str(s)) => s.clone(),
        Some(Value::Int(n)) => ZStr::from(n.to_string()),
        Some(Value::Reference(r)) => match r.get() {
            Value::Str(s) => s,
            Value::Int(n) => ZStr::from(n.to_string()),
            _ => ZStr::empty(),
        },
        _ => ZStr::empty(),
    }
}

/// String argument as a byte slice without cloning the string.
pub(crate) fn str_bytes<'a>(a: &'a Args, i: usize) -> Option<&'a [u8]> {
    match a.values.get(i) {
        Some(Value::Str(s)) => Some(s.as_bytes()),
        _ => None,
    }
}

/// Integer argument.
pub(crate) fn i(a: &Args, i: usize) -> i64 {
    match a.values.get(i) {
        Some(Value::Int(n)) => *n,
        Some(Value::Bool(b)) => i64::from(*b),
        #[allow(clippy::cast_possible_truncation)]
        Some(Value::Float(f)) => *f as i64,
        Some(Value::Reference(r)) => match r.get() {
            Value::Int(n) => n,
            Value::Bool(b) => i64::from(b),
            #[allow(clippy::cast_possible_truncation)]
            Value::Float(f) => f as i64,
            _ => 0,
        },
        _ => 0,
    }
}

/// Float argument.
pub(crate) fn f(a: &Args, i: usize) -> f64 {
    match a.values.get(i) {
        Some(Value::Float(f)) => *f,
        #[allow(clippy::cast_precision_loss)]
        Some(Value::Int(n)) => *n as f64,
        Some(Value::Reference(r)) => match r.get() {
            Value::Float(f) => f,
            #[allow(clippy::cast_precision_loss)]
            Value::Int(n) => n as f64,
            _ => 0.0,
        },
        _ => 0.0,
    }
}

/// Bool argument.
pub(crate) fn b(a: &Args, i: usize) -> bool {
    match a.values.get(i) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Reference(r)) => matches!(r.get(), Value::Bool(true)),
        _ => false,
    }
}

/// Array argument.
pub(crate) fn arr(a: &Args, i: usize) -> ZArray {
    match a.values.get(i) {
        Some(Value::Array(x)) => x.clone(),
        Some(Value::Reference(r)) => match r.get() {
            Value::Array(x) => x,
            _ => ZArray::new(),
        },
        _ => ZArray::new(),
    }
}

/// Optional argument (passed and not null).
pub(crate) fn opt(a: &Args, i: usize) -> Option<Value> {
    if !a.has(i) {
        return None;
    }
    match a.values.get(i) {
        Some(Value::Null) | None => None,
        Some(Value::Reference(r)) => {
            let v = r.get();
            if v.is_null() {
                None
            } else {
                Some(v)
            }
        }
        Some(v) => Some(v.clone()),
    }
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

/// Throws a `ValueError` "`func()`: Argument #n ($name) msg".
pub(crate) fn value_error(vm: &mut Vm, a: &Args, n: usize, msg: &str) -> crate::vm::Throw {
    let fname = builtin_display(vm, a);
    let pname = pname_of(a, n);
    vm.make_throw(
        "ValueError",
        &format!("{fname}(): Argument #{n} (${pname}) {msg}"),
    )
}

/// Parameter name for a 1-based argument position, if available.
fn pname_of(a: &Args, n: usize) -> std::string::String {
    a.func
        .as_ref()
        .and_then(|f| f.params.get(n.checked_sub(1)?).map(|p| p.name.to_string()))
        .unwrap_or_default()
}

/// Throws a `TypeError` "`func()`: Argument #n ($name) msg".
pub(crate) fn type_error(vm: &mut Vm, a: &Args, n: usize, msg: &str) -> crate::vm::Throw {
    let fname = builtin_display(vm, a);
    let pname = pname_of(a, n);
    vm.make_throw(
        "TypeError",
        &format!("{fname}(): Argument #{n} (${pname}) {msg}"),
    )
}

/// Converts any value to a string (with `__toString`).
#[allow(dead_code)]
pub(crate) fn to_str(vm: &mut Vm, v: &Value) -> VmResult<ZStr> {
    vm.to_zstr(v)
}

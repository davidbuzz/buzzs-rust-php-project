// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Shared helpers: running library calls, throwing [`PharError`]s and
//! argument conversions.

use php_ext_streams::bridge;
use php_phar::error::{PharError, PharResult};
use php_runtime::{Value, ZStr};
use php_vm::ext::{Args, Throw, VmResult};
use php_vm::Vm;

/// Throws a library exception.
pub fn throw(vm: &mut Vm, e: &PharError) -> Throw {
    vm.throw(e.class.name(), &e.message)
}

/// Runs a library call with the VM reachable from stream callbacks, emits
/// the queued stream warnings (prefixed with the running method) and turns
/// the library error into a PHP exception.
pub fn run<R>(vm: &mut Vm, f: impl FnOnce() -> PharResult<R>) -> VmResult<R> {
    match bridge::run(vm, f)? {
        Ok(r) => Ok(r),
        Err(e) => Err(throw(vm, &e)),
    }
}

/// Like [`run`] for calls that cannot fail.
pub fn run_ok<R>(vm: &mut Vm, f: impl FnOnce() -> R) -> VmResult<R> {
    bridge::run(vm, f)
}

/// A binary string value.
pub fn bytes(b: impl AsRef<[u8]>) -> Value {
    Value::Str(ZStr::from(b.as_ref()))
}

/// The file executing user code (`zend_get_executed_filename()`), if any.
pub fn executed(vm: &Vm) -> Option<Vec<u8>> {
    if !vm.is_executing() {
        return None;
    }
    Some(vm.current_location().0.as_bytes().to_vec())
}

/// Whether `__COMPILER_HALT_OFFSET__` is defined for the executing file.
pub fn halt_defined(vm: &Vm) -> bool {
    let file = vm.current_location().0;
    vm.ext.halt_offsets.contains_key(&file)
}

/// The PHP type name used in `TypeError` messages (`zend_zval_type_name`).
pub fn type_name(v: &Value) -> String {
    match v.deref() {
        Value::Null => "null".into(),
        Value::Bool(_) => "bool".into(),
        Value::Int(_) => "int".into(),
        Value::Float(_) => "float".into(),
        Value::Str(_) => "string".into(),
        Value::Array(_) => "array".into(),
        Value::Object(o) => php_vm::object::display_name(&o.class_name()).to_string_lossy(),
        Value::Resource(_) => "resource".into(),
        Value::Reference(_) => "reference".into(),
    }
}

/// `Class::method` of the running method (for argument errors).
pub fn method_name(a: &Args) -> String {
    let class = a
        .this
        .as_ref()
        .map(|o| php_vm::object::class_of(o).clone())
        .or_else(|| a.called.clone());
    let class = match class {
        Some(c) if c.instance_of_name(b"pharfileinfo") => "PharFileInfo",
        Some(c) if c.instance_of_name(b"phardata") => "PharData",
        _ => "Phar",
    };
    format!("{class}::{}", a.func_name())
}

/// Throws `$class: Class::method(): Argument #n ($name) $msg` (`n` is
/// 0-based).
pub fn arg_error(vm: &mut Vm, a: &Args, class: &str, n: usize, msg: &str) -> Throw {
    let text = format!(
        "{}(): Argument #{} (${}) {msg}",
        method_name(a),
        n + 1,
        param_name(a, n)
    );
    vm.throw(class, &text)
}

/// `Z_PARAM_PATH` checks for already-coerced string arguments: none of
/// the listed (0-based) arguments may contain NUL bytes.
pub fn check_paths(vm: &mut Vm, a: &Args, idx: &[usize]) -> VmResult<()> {
    for &n in idx {
        if let Value::Str(s) = a.at(n) {
            if s.as_bytes().contains(&0) {
                return Err(arg_error(
                    vm,
                    a,
                    "ValueError",
                    n,
                    "must not contain any null bytes",
                ));
            }
        }
    }
    Ok(())
}

/// `Z_PARAM_PATH` / `Z_PARAM_STR` for an untyped stub parameter: converts
/// scalars (and `Stringable` objects) to a string with PHP's argument
/// errors; `path` additionally rejects NUL bytes.
pub fn string_arg(vm: &mut Vm, a: &Args, n: usize, path: bool) -> VmResult<Vec<u8>> {
    let v = a.at(n);
    let s = match &v {
        Value::Str(s) => s.as_bytes().to_vec(),
        Value::Int(_) | Value::Float(_) | Value::Bool(_) => {
            vm.value_to_string(&v)?.as_bytes().to_vec()
        }
        Value::Null => {
            let name = param_name(a, n);
            vm.php_deprecated(&format!(
                "Passing null to parameter #{} (${name}) of type string is deprecated",
                n + 1
            ))?;
            Vec::new()
        }
        Value::Object(o)
            if php_vm::object::class_of(o)
                .find_method(b"__tostring")
                .is_some() =>
        {
            vm.value_to_string(&v)?.as_bytes().to_vec()
        }
        other => {
            let t = type_name(other);
            return Err(arg_error(
                vm,
                a,
                "TypeError",
                n,
                &format!("must be of type string, {t} given"),
            ));
        }
    };
    if path && s.contains(&0) {
        return Err(arg_error(
            vm,
            a,
            "ValueError",
            n,
            "must not contain any null bytes",
        ));
    }
    Ok(s)
}

/// Throws `class` with `message`, chaining `prev` (an exception thrown by
/// a callback) as its previous exception, like `zend_throw_exception()`
/// while another exception is pending.
pub fn throw_chained(vm: &mut Vm, class: &str, message: &str, prev: Throw) -> Throw {
    let Throw::Exception(prev_obj) = prev else {
        return prev;
    };
    let Some(cls) = vm.find_class(class.as_bytes()) else {
        return Throw::Exception(prev_obj);
    };
    match vm.new_object_with_args(
        &cls,
        vec![bytes(message), Value::Int(0), Value::Object(prev_obj)],
    ) {
        Ok(o) => Throw::Exception(o),
        Err(t) => t,
    }
}

/// The name of parameter `n` of the running function.
pub fn param_name(a: &Args, n: usize) -> String {
    a.func
        .as_ref()
        .and_then(|f| f.params.get(n))
        .map(|p| p.name.to_string_lossy())
        .unwrap_or_default()
}

/// An optional string argument (`?string`, `null` or missing → `None`).
pub fn opt_str(a: &Args, n: usize) -> Option<Vec<u8>> {
    a.opt(n).map(|v| match v {
        Value::Str(s) => s.as_bytes().to_vec(),
        _ => Vec::new(),
    })
}

/// An optional int argument (`?int`).
pub fn opt_int(a: &Args, n: usize) -> Option<i64> {
    a.opt(n).map(|v| match v {
        Value::Int(i) => i,
        _ => 0,
    })
}

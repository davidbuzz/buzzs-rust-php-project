// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Argument helpers shared by the functions: C `int` conversions, ZPP of
//! the extra `imagefilter()` arguments, `zval_try_get_long()`, and the
//! replay of libgd's queued messages.

use php_gd::errors::{self, Level};
use php_runtime::numeric::{is_numeric_string_ex, Number};
use php_runtime::Value;
use php_vm::ext::{Args, VmResult};
use php_vm::Vm;

/// `E_ERROR`.
const E_ERROR: i64 = 1;
/// `E_WARNING`.
const E_WARNING: i64 = 2;
/// `E_NOTICE`.
const E_NOTICE: i64 = 8;
/// `E_DEPRECATED`.
pub(crate) const E_DEPRECATED: i64 = 8192;

/// `INT_MIN` / `INT_MAX` as PHP integers.
pub(crate) const INT_MIN: i64 = i32::MIN as i64;
pub(crate) const INT_MAX: i64 = i32::MAX as i64;

/// The implicit `zend_long` → `int` conversion of C (two's complement
/// truncation).
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn ci(v: i64) -> i32 {
    v as i32
}

/// `ZEND_LONG_INT_OVFL` / `ZEND_LONG_EXCEEDS_INT`.
pub(crate) fn exceeds_int(v: i64) -> bool {
    !(INT_MIN..=INT_MAX).contains(&v)
}

/// Raises the messages libgd queued during the last calls, like
/// `php_gd_error_method()`.
pub(crate) fn flush(vm: &mut Vm) -> VmResult<()> {
    for m in errors::take() {
        let level = match m.level {
            Level::Notice => E_NOTICE,
            Level::Warning => E_WARNING,
            Level::Error => E_ERROR,
        };
        vm.php_error_bytes(level, &m.text)?;
    }
    Ok(())
}

/// `php_error_docref(NULL, E_WARNING, ...)` with a binary message.
pub(crate) fn warn(vm: &mut Vm, msg: &[u8]) -> VmResult<()> {
    vm.php_warning_bytes(msg)
}

/// The running function's name.
pub(crate) fn fname(a: &Args) -> String {
    a.func
        .as_ref()
        .map_or_else(String::new, |f| f.name.to_string_lossy())
}

/// An error about an argument that has no declared name (an extra
/// variadic argument of `imagefilter()`): `func(): Argument #n msg`.
pub(crate) fn unnamed_arg_error(
    vm: &mut Vm,
    a: &Args,
    class: &str,
    n: usize,
    msg: &str,
) -> php_vm::ext::Throw {
    vm.throw(class, &format!("{}(): Argument #{n} {msg}", fname(a)))
}

/// `ArgumentCountError` for a fixed arity range (`zend_wrong_parameters_count_error`).
pub(crate) fn arity(vm: &mut Vm, a: &Args, min: usize, max: usize) -> VmResult<()> {
    let n = a.len();
    if n >= min && n <= max {
        return Ok(());
    }
    let (kind, expected) = if min == max {
        ("exactly", min)
    } else if n < min {
        ("at least", min)
    } else {
        ("at most", max)
    };
    let plural = if expected == 1 { "" } else { "s" };
    Err(vm.throw(
        "ArgumentCountError",
        &format!(
            "{}() expects {kind} {expected} argument{plural}, {n} given",
            fname(a)
        ),
    ))
}

/// ZPP `l` for argument `n` (0-based) outside the declared signature.
pub(crate) fn zpp_long(vm: &mut Vm, a: &Args, n: usize) -> VmResult<i64> {
    let v = a.at(n);
    if let Value::Int(i) = v {
        return Ok(i);
    }
    if !matches!(
        v,
        Value::Array(_) | Value::Object(_) | Value::Resource(_) | Value::Null
    ) {
        if let Some(i) = vm.parse_long_weak(&v)? {
            return Ok(i);
        }
    }
    if matches!(v, Value::Null) {
        vm.php_error_bytes(
            E_DEPRECATED,
            format!(
                "Passing null to parameter #{} of type int is deprecated",
                n + 1
            )
            .as_bytes(),
        )?;
        return Ok(0);
    }
    Err(unnamed_arg_error(
        vm,
        a,
        "TypeError",
        n + 1,
        &format!("must be of type int, {} given", v.value_name()),
    ))
}

/// ZPP `d` for argument `n` (0-based) outside the declared signature.
pub(crate) fn zpp_double(vm: &mut Vm, a: &Args, n: usize) -> VmResult<f64> {
    let v = a.at(n);
    #[allow(clippy::cast_precision_loss)]
    match &v {
        Value::Float(f) => return Ok(*f),
        Value::Int(i) => return Ok(*i as f64),
        Value::Bool(b) => return Ok(f64::from(u8::from(*b))),
        Value::Str(s) => {
            if let Some(num) = is_numeric_string_ex(s.as_bytes(), true) {
                if num.trailing_data {
                    // "A well formed numeric value" is required by ZPP.
                } else {
                    return Ok(match num.value {
                        Number::Int(i) => i as f64,
                        Number::Float(f) => f,
                    });
                }
            }
        }
        Value::Null => {
            vm.php_error_bytes(
                E_DEPRECATED,
                format!(
                    "Passing null to parameter #{} of type float is deprecated",
                    n + 1
                )
                .as_bytes(),
            )?;
            return Ok(0.0);
        }
        _ => {}
    }
    Err(unnamed_arg_error(
        vm,
        a,
        "TypeError",
        n + 1,
        &format!("must be of type float, {} given", v.value_name()),
    ))
}

/// ZPP `b` for argument `n` (0-based) outside the declared signature.
pub(crate) fn zpp_bool(vm: &mut Vm, a: &Args, n: usize) -> VmResult<bool> {
    let v = a.at(n);
    match &v {
        Value::Bool(b) => Ok(*b),
        Value::Int(_) | Value::Float(_) | Value::Str(_) => vm.value_to_bool(&v),
        Value::Null => {
            vm.php_error_bytes(
                E_DEPRECATED,
                format!(
                    "Passing null to parameter #{} of type bool is deprecated",
                    n + 1
                )
                .as_bytes(),
            )?;
            Ok(false)
        }
        _ => Err(unnamed_arg_error(
            vm,
            a,
            "TypeError",
            n + 1,
            &format!("must be of type bool, {} given", v.value_name()),
        )),
    }
}

/// `zval_try_get_long()`: `None` when the value cannot be used as an
/// integer (arrays, objects without a numeric cast, non-numeric strings).
pub(crate) fn try_get_long(vm: &mut Vm, v: &Value) -> VmResult<Option<i64>> {
    Ok(match v.deref() {
        Value::Null => Some(0),
        Value::Bool(b) => Some(i64::from(b)),
        Value::Int(i) => Some(i),
        Value::Float(f) => Some(php_runtime::convert::dval_to_lval_cap(f)),
        Value::Str(s) => match is_numeric_string_ex(s.as_bytes(), true) {
            None => None,
            Some(num) => {
                if num.trailing_data {
                    vm.raise(E_WARNING, "A non-numeric value encountered")?;
                }
                Some(match num.value {
                    Number::Int(i) => i,
                    Number::Float(f) => php_runtime::convert::dval_to_lval_cap(f),
                })
            }
        },
        Value::Resource(r) => Some(r.handle()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_int_truncation() {
        assert_eq!(ci(5), 5);
        assert_eq!(ci(0x1_0000_0001), 1);
        assert_eq!(ci(-1), -1);
        assert!(exceeds_int(INT_MAX + 1));
        assert!(!exceeds_int(INT_MIN));
    }
}

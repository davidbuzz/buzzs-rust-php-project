// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The manual `int|resource $file_descriptor` argument handling shared by
//! `posix_isatty()`, `posix_ttyname()` and `posix_fpathconf()`.

use php_ext_streams::res;
use php_runtime::float::format_double;
use php_runtime::numeric::{is_numeric_string_ex, Number};
use php_runtime::Value;
use php_vm::ext::{Args, VmResult};
use php_vm::Vm;

/// `E_DEPRECATED`.
const E_DEPRECATED: i64 = 8192;

/// The decoded file-descriptor argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FdArg {
    /// A descriptor (not range-checked yet for integers).
    Fd(i64),
    /// The function must return `false` (uncastable stream after its
    /// warning, or a stream busy in its own callback).
    Fail,
    /// Neither int-like nor a resource: the `zend_zval_value_name` of it.
    WrongType(String),
}

/// `ZEND_DOUBLE_FITS_LONG`.
fn fits_long(d: f64) -> bool {
    #[allow(clippy::cast_precision_loss)]
    let max = i64::MAX as f64;
    #[allow(clippy::cast_precision_loss)]
    let min = i64::MIN as f64;
    d >= min && d < max
}

/// Converts a float for `zend_parse_arg_long_weak`, `None` when it does not
/// fit; `lossy` tells whether the precision-loss deprecation applies.
fn float_to_long(d: f64) -> Option<(i64, bool)> {
    if d.is_nan() || !fits_long(d) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    let l = d as i64;
    #[allow(clippy::cast_precision_loss, clippy::float_cmp)]
    let lossy = l as f64 != d;
    Some((l, lossy))
}

/// `zend_parse_arg_long(arg, …, check_null = false, arg_num)` in weak mode,
/// including the null and precision-loss deprecations. `Ok(None)` when the
/// value is not accepted.
#[allow(clippy::many_single_char_names)]
pub(crate) fn parse_long_weak(vm: &mut Vm, a: &Args, n: usize, v: &Value) -> VmResult<Option<i64>> {
    Ok(match v {
        Value::Int(i) => Some(*i),
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Null => {
            let name = a
                .func
                .as_ref()
                .and_then(|f| f.params.get(n).map(|p| p.name.to_string_lossy()))
                .unwrap_or_default();
            vm.php_deprecated(&format!(
                "Passing null to parameter #{} (${name}) of type int is deprecated",
                n + 1
            ))?;
            Some(0)
        }
        Value::Float(d) => match float_to_long(*d) {
            Some((l, lossy)) => {
                if lossy {
                    let s = format_double(*d, -1, false);
                    vm.raise(
                        E_DEPRECATED,
                        &format!("Implicit conversion from float {s} to int loses precision"),
                    )?;
                }
                Some(l)
            }
            None => None,
        },
        Value::Str(s) => match is_numeric_string_ex(s.as_bytes(), false).map(|n| n.value) {
            Some(Number::Int(i)) => Some(i),
            Some(Number::Float(d)) => match float_to_long(d) {
                Some((l, lossy)) => {
                    if lossy {
                        vm.raise(
                            E_DEPRECATED,
                            &format!(
                                "Implicit conversion from float-string \"{}\" to int loses precision",
                                s.to_string_lossy()
                            ),
                        )?;
                    }
                    Some(l)
                }
                None => None,
            },
            None => None,
        },
        Value::Reference(r) => {
            let inner = r.get();
            return parse_long_weak(vm, a, n, &inner);
        }
        _ => None,
    })
}

/// `php_posix_stream_get_fd()` / `zend_parse_arg_long()` on argument `n`.
pub(crate) fn fd_arg(vm: &mut Vm, a: &Args, n: usize) -> VmResult<FdArg> {
    let v = a.at(n);
    if let Value::Resource(r) = &v {
        // php_stream_from_zval_no_verify(): zend_fetch_resource2_ex()
        // throws for anything but an open stream.
        let cell = res::stream_of_resource(vm, a, r)?;
        let cast = res::with_stream(vm, &cell, |s| s.any_fd().ok_or_else(|| s.ops_label()))?;
        return match cast {
            Some(Ok(fd)) => Ok(FdArg::Fd(i64::from(fd))),
            Some(Err(label)) => {
                vm.php_warning(&php_posix::stream_cast_warning(label))?;
                Ok(FdArg::Fail)
            }
            None => Ok(FdArg::Fail),
        };
    }
    match parse_long_weak(vm, a, n, &v)? {
        Some(fd) => Ok(FdArg::Fd(fd)),
        None => Ok(FdArg::WrongType(v.value_name())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_conversion() {
        assert_eq!(float_to_long(1.0), Some((1, false)));
        assert_eq!(float_to_long(5.5), Some((5, true)));
        assert_eq!(float_to_long(f64::NAN), None);
        assert_eq!(float_to_long(1e30), None);
    }
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `intl_*()` error functions.

use php_runtime::Value;
use php_vm::ext::{Args, NativeFn, VmResult};
use php_vm::Vm;

use crate::core::{self, s};

fn get_error_code(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(i64::from(core::global_error().code.0)))
}

fn get_error_message(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(s(core::global_error().get_message()))
}

fn is_failure(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(php_intl::error::intl_is_failure(a.int(0))))
}

fn error_name(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(s(php_intl::error::intl_error_name(a.int(0))))
}

/// Handlers by stub key.
pub(crate) fn handlers() -> Vec<(&'static str, NativeFn)> {
    vec![
        ("intl_get_error_code", get_error_code),
        ("intl_get_error_message", get_error_message),
        ("intl_is_failure", is_failure),
        ("intl_error_name", error_name),
    ]
}

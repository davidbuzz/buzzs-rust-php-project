// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ext/shmop` (ref: `ext/shmop/shmop.c`).

use php_runtime::Value;
use php_sysv::shmop::Shmop;
use php_sysv::SysvResult;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::objects::{register_class, with_state, wrap};
use crate::{bytes, failure};

/// The class name.
const CLASS: &str = "Shmop";

/// Runs `f` on the segment of argument 0.
fn seg<R>(a: &Args, f: impl FnOnce(&mut Shmop) -> SysvResult<R>) -> Option<SysvResult<R>> {
    with_state::<Shmop, _>(a, 0, f)
}

fn shmop_open(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mode = a.str(1);
    match Shmop::open(a.int(0), mode.as_bytes(), a.int(2), a.int(3)) {
        Ok(s) => wrap(vm, CLASS, s),
        Err(e) => failure(vm, a, e),
    }
}

fn shmop_read(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (offset, size) = (a.int(1), a.int(2));
    match seg(a, |s| s.read(offset, size)) {
        Some(Ok(d)) => Ok(bytes(&d)),
        Some(Err(e)) => failure(vm, a, e),
        None => Ok(Value::Bool(false)),
    }
}

fn shmop_close(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Null)
}

fn shmop_size(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(
        seg(a, |s| Ok(s.size())).and_then(Result::ok).unwrap_or(0),
    ))
}

fn shmop_write(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (data, offset) = (a.str(1), a.int(2));
    match seg(a, |s| s.write(data.as_bytes(), offset)) {
        Some(Ok(n)) => Ok(Value::Int(n)),
        Some(Err(e)) => failure(vm, a, e),
        None => Ok(Value::Bool(false)),
    }
}

fn shmop_delete(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    match seg(a, |s| s.delete()) {
        Some(Ok(())) => Ok(Value::Bool(true)),
        Some(Err(e)) => failure(vm, a, e),
        None => Ok(Value::Bool(false)),
    }
}

/// Registers the class and functions.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    register_class(m, CLASS);
    m.function_static(
        "shmop_open(int $key, string $mode, int $permissions, int $size): Shmop|false",
        shmop_open,
    )
    .function_static(
        "shmop_read(Shmop $shmop, int $offset, int $size): string",
        shmop_read,
    )
    .function_static("shmop_close(Shmop $shmop): void", shmop_close)
    .deprecated("8.0", "as Shmop objects are freed automatically")
    .function_static("shmop_size(Shmop $shmop): int", shmop_size)
    .function_static(
        "shmop_write(Shmop $shmop, string $data, int $offset): int",
        shmop_write,
    )
    .function_static("shmop_delete(Shmop $shmop): bool", shmop_delete);
}

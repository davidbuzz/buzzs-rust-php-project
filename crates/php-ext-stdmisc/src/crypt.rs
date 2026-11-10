//! `crypt()` and the `password_*` API.

use crate::ctx::RtExt;
use php_runtime::{Value, ZArray, ZStr};
use php_stdmisc::crypt::{self, password, password::AlgoArg};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

fn algo_arg(v: &Value) -> AlgoArg<'_> {
    match v {
        Value::Str(s) => AlgoArg::Str(s.as_bytes()),
        Value::Int(i) => AlgoArg::Int(*i),
        _ => AlgoArg::Null,
    }
}

#[allow(clippy::unnecessary_wraps)] // native handler signature
fn crypt_fn(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Str(ZStr::from(crypt::crypt(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
    ))))
}

fn password_hash(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let pw = a.str(0);
    let algo = a.at(1);
    let opts: Option<ZArray> = a.has(2).then(|| a.array(2));
    password::password_hash(pw.as_bytes(), algo_arg(&algo), opts.as_ref(), vm)
        .vm(vm)
        .map(Value::Str)
}

fn password_needs_rehash(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let hash = a.str(0);
    let algo = a.at(1);
    let opts: Option<ZArray> = a.has(2).then(|| a.array(2));
    password::password_needs_rehash(hash.as_bytes(), algo_arg(&algo), opts.as_ref(), vm)
        .vm(vm)
        .map(Value::Bool)
}

#[allow(clippy::unnecessary_wraps)] // native handler signature
fn password_verify(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(password::password_verify(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
    )))
}

#[allow(clippy::unnecessary_wraps)] // native handler signature
fn password_get_info(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(password::password_get_info(
        a.str(0).as_bytes(),
    )))
}

#[allow(clippy::unnecessary_wraps)] // native handler signature
fn password_algos(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(password::password_algos()))
}

/// Registers the functions and constants.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    for (name, v) in crypt::CONSTANTS {
        m.constant(name, Value::Int(*v));
    }
    for (name, v) in password::INT_CONSTANTS {
        m.constant(name, Value::Int(*v));
    }
    for (name, v) in password::STR_CONSTANTS {
        m.constant(name, Value::from(*v));
    }
    m.function_static("crypt(#[\\SensitiveParameter] string $string, string $salt): string", crypt_fn)
        .function_static(
            "password_hash(#[\\SensitiveParameter] string $password, string|int|null $algo, array $options = []): string",
            password_hash,
        )
        .function_static(
            "password_needs_rehash(string $hash, string|int|null $algo, array $options = []): bool",
            password_needs_rehash,
        )
        .function_static(
            "password_verify(#[\\SensitiveParameter] string $password, string $hash): bool",
            password_verify,
        )
        .function_static("password_get_info(string $hash): array", password_get_info)
        .function_static("password_algos(): array", password_algos);
}

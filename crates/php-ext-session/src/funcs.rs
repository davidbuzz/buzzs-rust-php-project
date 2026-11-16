// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `session_*()` functions (`session.stub.php`), each a thin adapter
//! from VM arguments to [`php_session::api`].

use php_runtime::{RtResult, Value, ZStr};
use php_session::api;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::{Throw, Vm};

use crate::host::{with_host, VmHost};

/// Registers the 23 functions.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function_static(
        "session_name(?string $name = null): string|false",
        session_name,
    )
    .function_static(
        "session_module_name(?string $module = null): string|false",
        session_module_name,
    )
    .function_static(
        "session_save_path(?string $path = null): string|false",
        session_save_path,
    )
    .function_static("session_id(?string $id = null): string|false", session_id)
    .function_static(
        "session_create_id(string $prefix = \"\"): string|false",
        session_create_id,
    )
    .function_static(
        "session_regenerate_id(bool $delete_old_session = false): bool",
        session_regenerate_id,
    )
    .function_static("session_decode(string $data): bool", session_decode)
    .function_static("session_encode(): string|false", session_encode)
    .function_static("session_destroy(): bool", session_destroy)
    .function_static("session_unset(): bool", session_unset)
    .function_static("session_gc(): int|false", session_gc)
    .function_static(
        "session_get_cookie_params(): array",
        session_get_cookie_params,
    )
    .function_static("session_write_close(): bool", session_write_close)
    .function_static("session_abort(): bool", session_abort)
    .function_static("session_reset(): bool", session_reset)
    .function_static("session_status(): int", session_status)
    .function_static(
        "session_register_shutdown(): void",
        session_register_shutdown,
    )
    .function_static("session_commit(): bool", session_commit)
    .function_static(
        "session_set_save_handler($open, $close = UNKNOWN, $read = UNKNOWN, \
             $write = UNKNOWN, $destroy = UNKNOWN, $gc = UNKNOWN, $create_sid = UNKNOWN, \
             $validate_sid = UNKNOWN, $update_timestamp = UNKNOWN): bool",
        session_set_save_handler,
    )
    .function_static(
        "session_cache_limiter(?string $value = null): string|false",
        session_cache_limiter,
    )
    .function_static(
        "session_cache_expire(?int $value = null): int|false",
        session_cache_expire,
    )
    .function_static(
        "session_set_cookie_params(array|int $lifetime_or_options, ?string $path = null, \
             ?string $domain = null, ?bool $secure = null, ?bool $httponly = null): bool",
        session_set_cookie_params,
    )
    .function_static("session_start(array $options = []): bool", session_start);
}

/// A `?string` argument (absent or `null` → `None`).
fn opt_str(a: &Args, n: usize) -> Option<ZStr> {
    match a.opt(n) {
        Some(Value::Str(s)) => Some(s),
        _ => None,
    }
}

/// A `?bool` argument.
fn opt_bool(a: &Args, n: usize) -> Option<bool> {
    match a.opt(n) {
        Some(Value::Bool(b)) => Some(b),
        _ => None,
    }
}

/// Runs a library entry point and converts its result. Values released by
/// the module (e.g. an invalid save-handler return value) are destroyed
/// before the function returns, so their destructors' exceptions surface
/// from the call, as in PHP.
pub(crate) fn call<T>(vm: &mut Vm, f: impl FnOnce(&mut VmHost<'_>) -> RtResult<T>) -> VmResult<T> {
    let r = with_host(vm, |h| {
        let r = f(h);
        h.vm_result(r)
    });
    let d = vm.run_released_destructors();
    match (r, d) {
        // An exception thrown while another is pending replaces it (PHP
        // chains the earlier one as `previous`).
        (Ok(_) | Err(Throw::Exception(_)), Err(t @ Throw::Exception(_))) | (Ok(_), Err(t)) => {
            Err(t)
        }
        (r, _) => r,
    }
}

fn bool_v(r: VmResult<bool>) -> VmResult<Value> {
    r.map(Value::Bool)
}

fn session_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let name = opt_str(a, 0);
    call(vm, |h| api::session_name(h, name.as_ref()))
}

fn session_module_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let module = opt_str(a, 0);
    call(vm, |h| api::session_module_name(h, module.as_ref()))
}

fn session_save_path(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let path = opt_str(a, 0);
    call(vm, |h| api::session_save_path(h, path.as_ref()))
}

fn session_id(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let id = opt_str(a, 0);
    call(vm, |h| api::session_id(h, id.as_ref()))
}

fn session_create_id(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let prefix = a.has(0).then(|| a.str(0));
    call(vm, |h| api::session_create_id(h, prefix.as_ref()))
}

fn session_regenerate_id(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let delete = a.bool(0);
    bool_v(call(vm, |h| api::session_regenerate_id(h, delete)))
}

fn session_decode(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let data = a.str(0);
    bool_v(call(vm, |h| api::session_decode(h, data.as_bytes())))
}

fn session_encode(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    call(vm, |h| api::session_encode(h))
}

fn session_destroy(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    bool_v(call(vm, |h| api::session_destroy(h)))
}

fn session_unset(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    bool_v(call(vm, |h| api::session_unset(h)))
}

fn session_gc(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    call(vm, |h| api::session_gc(h))
}

fn session_get_cookie_params(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    call(vm, |h| Ok(Value::Array(api::session_get_cookie_params(h))))
}

fn session_write_close(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    bool_v(call(vm, |h| api::session_write_close(h)))
}

fn session_commit(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    bool_v(call(vm, |h| api::session_commit(h)))
}

fn session_abort(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    bool_v(call(vm, |h| api::session_abort(h)))
}

fn session_reset(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    bool_v(call(vm, |h| api::session_reset(h)))
}

fn session_status(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    call(vm, |h| Ok(Value::Int(api::session_status(h))))
}

fn session_register_shutdown(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    call(vm, |h| api::session_register_shutdown(h)).map(|()| Value::Null)
}

fn session_set_save_handler(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = a.passed.iter().rposition(|&p| p).map_or(0, |i| i + 1);
    let args: Vec<Value> = (0..n).map(|i| a.at(i)).collect();
    bool_v(call(vm, |h| api::session_set_save_handler(h, &args)))
}

fn session_cache_limiter(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let v = opt_str(a, 0);
    call(vm, |h| api::session_cache_limiter(h, v.as_ref()))
}

fn session_cache_expire(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let v = match a.opt(0) {
        Some(Value::Int(i)) => Some(i),
        _ => None,
    };
    call(vm, |h| api::session_cache_expire(h, v))
}

fn session_set_cookie_params(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let first = a.at(0);
    let path = opt_str(a, 1);
    let domain = opt_str(a, 2);
    let secure = opt_bool(a, 3);
    let httponly = opt_bool(a, 4);
    bool_v(call(vm, |h| {
        api::session_set_cookie_params(h, &first, path.as_ref(), domain.as_ref(), secure, httponly)
    }))
}

fn session_start(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let options = a.has(0).then(|| a.array(0));
    bool_v(call(vm, |h| api::session_start(h, options.as_ref())))
}

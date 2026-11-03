//! `ext/json` for the phprust VM: binds the `php-json` library through the
//! `php_vm::ext` extension API.
//!
//! * `json_encode`, `json_decode`, `json_validate`, `json_last_error`,
//!   `json_last_error_msg` with php-src's exact `ValueError`s and
//!   `JSON_THROW_ON_ERROR` handling (all in [`php_json::functions`]).
//! * The `JSON_*` constants.
//! * `JsonSerializable` / `JsonException` (registered here only when the VM
//!   does not already provide them).
//!
//! The per-request last-error state (`JSON_G(error_details)`) lives in a
//! thread-local [`JsonState`], reset at request shutdown.

mod host;

use host::VmJsonHost;
use php_json::{functions, JsonState};
use php_runtime::Value;
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, Extension, ModuleBuilder, VmResult};
use php_vm::Vm;
use std::cell::RefCell;

thread_local! {
    /// `JSON_G(error_details)` of the running request.
    static STATE: RefCell<JsonState> = RefCell::new(JsonState::new());
}

/// Runs `f` with the request's [`JsonState`] taken out of the thread-local
/// slot, so that nested `json_*` calls made from `jsonSerialize()` see (and
/// update) a consistent state.
fn with_state<R>(f: impl FnOnce(&mut JsonState) -> R) -> R {
    let mut st = STATE.with(|s| *s.borrow());
    let r = f(&mut st);
    STATE.with(|s| *s.borrow_mut() = st);
    r
}

/// The `json` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Json;

fn json_encode(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let value = a.at(0);
    let flags = if a.has(1) { a.int(1) } else { 0 };
    let depth = if a.has(2) {
        a.int(2)
    } else {
        php_json::flags::DEFAULT_DEPTH
    };
    let mut st = STATE.with(|s| *s.borrow());
    let r = {
        let mut h = VmJsonHost::new(vm);
        functions::json_encode(&mut st, &mut h, &value, flags, depth)
    };
    STATE.with(|s| *s.borrow_mut() = st);
    r.map_err(|e| host::rt_throw(vm, e))
}

fn json_decode(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let json = a.str(0);
    let assoc = a.opt(1).map(|v| matches!(v, Value::Bool(true)));
    let depth = if a.has(2) {
        a.int(2)
    } else {
        php_json::flags::DEFAULT_DEPTH
    };
    let flags = if a.has(3) { a.int(3) } else { 0 };
    let mut st = STATE.with(|s| *s.borrow());
    let r = {
        let mut h = VmJsonHost::new(vm);
        functions::json_decode(&mut st, &mut h, json.as_bytes(), assoc, depth, flags)
    };
    STATE.with(|s| *s.borrow_mut() = st);
    r.map_err(|e| host::rt_throw(vm, e))
}

fn json_validate(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let json = a.str(0);
    let depth = if a.has(1) {
        a.int(1)
    } else {
        php_json::flags::DEFAULT_DEPTH
    };
    let flags = if a.has(2) { a.int(2) } else { 0 };
    with_state(|st| functions::json_validate(st, json.as_bytes(), depth, flags))
        .map(Value::Bool)
        .map_err(|e| host::rt_throw(vm, e))
}

#[allow(clippy::unnecessary_wraps)] // native handler signature
fn json_last_error(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(with_state(|st| functions::json_last_error(st))))
}

#[allow(clippy::unnecessary_wraps)] // native handler signature
fn json_last_error_msg(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::from(with_state(|st| {
        functions::json_last_error_msg(st)
    })))
}

impl Extension for Json {
    fn name(&self) -> &'static str {
        "json"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        for (name, value) in php_json::flags::CONSTANTS {
            m.constant(name, Value::Int(*value));
        }
        m.function(
            "json_encode(mixed $value, int $flags = 0, int $depth = 512): string|false",
            json_encode,
        )
        .function(
            "json_decode(string $json, ?bool $associative = null, int $depth = 512, int $flags = 0): mixed",
            json_decode,
        )
        .function(
            "json_validate(string $json, int $depth = 512, int $flags = 0): bool",
            json_validate,
        )
        .function("json_last_error(): int", json_last_error)
        .function("json_last_error_msg(): string", json_last_error_msg);

        let vm = m.vm();
        if vm.find_class(b"jsonserializable").is_none() {
            ClassBuilder::new(php_json::JSON_SERIALIZABLE_INTERFACE)
                .flags(class_flags::INTERFACE)
                .abstract_method("jsonSerialize(): mixed", acc::PUBLIC)
                .register(vm);
        }
        if vm.find_class(b"jsonexception").is_none() {
            ClassBuilder::new(php_json::JSON_EXCEPTION_CLASS)
                .extends(vm, "Exception")
                .register(vm);
        }
        m.on_request_shutdown(|_| STATE.with(|s| s.borrow_mut().clear()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trip() {
        with_state(|st| st.last_error = php_json::JsonError::new(php_json::ErrorCode::Syntax));
        assert_eq!(with_state(|st| functions::json_last_error(st)), 4);
        with_state(JsonState::clear);
        assert_eq!(with_state(|st| functions::json_last_error(st)), 0);
    }

    #[test]
    fn constants_complete() {
        assert_eq!(php_json::flags::CONSTANTS.len(), 30);
    }
}

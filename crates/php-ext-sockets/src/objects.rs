// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `Socket` and `AddressInfo` classes and their native state.

use php_runtime::{Obj, Value};
use php_sockets::{AddressInfo, Socket};
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ClassHooks, ModuleBuilder, VmResult};
use php_vm::{object, Vm};

/// `ZEND_ACC_NO_DYNAMIC_PROPERTIES` (`@strict-properties`); not re-exported
/// by `php_vm::ext::class_flags`, so spelled out with the value of
/// `php_vm::class::cflags::NO_DYNAMIC_PROPERTIES`.
const NO_DYNAMIC_PROPERTIES: u32 = 1 << 14;

/// `php_socket`: the library socket plus the associated stream resource
/// (`zstream`), set by `socket_import_stream()`/`socket_export_stream()`.
#[derive(Debug)]
pub(crate) struct SockObj {
    /// The library state.
    pub(crate) sock: Socket,
    /// The stream resource sharing the descriptor, if any.
    pub(crate) stream: Option<Value>,
}

fn ctor(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let class = a
        .this
        .as_ref()
        .map(|o| object::class_of(o).name.to_string())
        .unwrap_or_default();
    let msg = match class.as_str() {
        "Socket" => "Cannot directly construct Socket, use socket_create() instead",
        "AddressInfo" => {
            "Cannot directly construct AddressInfo, use socket_addrinfo_lookup() instead"
        }
        _ => "Cannot directly construct object",
    };
    Err(vm.throw("Error", msg))
}

/// `zend_objects_not_comparable`: an instance only equals itself.
#[allow(clippy::unnecessary_wraps)] // object handler signature
fn not_comparable(_vm: &mut Vm, a: &Value, b: &Value) -> VmResult<Option<i32>> {
    Ok(Some(match (a, b) {
        (Value::Object(x), Value::Object(y)) if Obj::ptr_eq(x, y) => 0,
        _ => 1,
    }))
}

/// Registers both classes.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    for name in ["Socket", "AddressInfo"] {
        ClassBuilder::new(name)
            .flags(
                class_flags::FINAL
                    | class_flags::NOT_SERIALIZABLE
                    | class_flags::NO_CLONE
                    | NO_DYNAMIC_PROPERTIES,
            )
            .method("__construct()", acc::PUBLIC, ctor)
            .hooks(ClassHooks {
                compare: Some(not_comparable),
                ..Default::default()
            })
            .register(m.vm());
    }
}

fn wrap<T: 'static>(vm: &mut Vm, class: &str, state: T) -> VmResult<Value> {
    let Some(cls) = vm.find_class(class.as_bytes()) else {
        return Err(vm.throw("Error", &format!("Class \"{class}\" not found")));
    };
    let o = vm.new_object(&cls)?;
    object::set_internal(&o, state);
    Ok(Value::Object(o))
}

/// A new `Socket` object.
pub(crate) fn new_socket(vm: &mut Vm, sock: Socket) -> VmResult<Value> {
    wrap(vm, "Socket", SockObj { sock, stream: None })
}

/// A new `Socket` object associated with `stream`.
pub(crate) fn new_socket_with_stream(vm: &mut Vm, sock: Socket, stream: Value) -> VmResult<Value> {
    wrap(
        vm,
        "Socket",
        SockObj {
            sock,
            stream: Some(stream),
        },
    )
}

/// A new `AddressInfo` object.
pub(crate) fn new_addrinfo(vm: &mut Vm, info: AddressInfo) -> VmResult<Value> {
    wrap(vm, "AddressInfo", info)
}

/// The object passed as argument `n`.
pub(crate) fn obj_arg(a: &Args, n: usize) -> Option<Obj> {
    match a.at(n) {
        Value::Object(o) => Some(o),
        _ => None,
    }
}

/// Runs `f` on the socket of argument `n`.
pub(crate) fn with_sock<R>(a: &Args, n: usize, f: impl FnOnce(&mut SockObj) -> R) -> Option<R> {
    obj_arg(a, n).and_then(|o| object::internal::<SockObj, R>(&o, f))
}

/// Runs `f` on the address info of argument `n`.
pub(crate) fn with_addrinfo<R>(
    a: &Args,
    n: usize,
    f: impl FnOnce(&mut AddressInfo) -> R,
) -> Option<R> {
    obj_arg(a, n).and_then(|o| object::internal::<AddressInfo, R>(&o, f))
}

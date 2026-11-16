// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The opaque handle classes shared by the four extensions.

use php_runtime::{Obj, Value};
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ClassHooks, ModuleBuilder, VmResult};
use php_vm::{object, Vm};

/// `ZEND_ACC_NO_DYNAMIC_PROPERTIES` (`@strict-properties`); not re-exported
/// by `php_vm::ext::class_flags`, so spelled out with the value of
/// `php_vm::class::cflags::NO_DYNAMIC_PROPERTIES`.
const NO_DYNAMIC_PROPERTIES: u32 = 1 << 14;

/// The `get_constructor` handler texts, per class.
const CONSTRUCT_ERRORS: [(&str, &str); 4] = [
    (
        "SysvMessageQueue",
        "Cannot directly construct SysvMessageQueue, use msg_get_queue() instead",
    ),
    (
        "SysvSemaphore",
        "Cannot directly construct SysvSemaphore, use sem_get() instead",
    ),
    (
        "SysvSharedMemory",
        "Cannot directly construct SysvSharedMemory, use shm_attach() instead",
    ),
    (
        "Shmop",
        "Cannot directly construct Shmop, use shmop_open() instead",
    ),
];

/// `get_constructor`: direct construction is an `Error`.
fn ctor(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let class = a
        .this
        .as_ref()
        .map(|o| object::class_of(o).name.to_string())
        .unwrap_or_default();
    let msg = CONSTRUCT_ERRORS
        .iter()
        .find(|(c, _)| *c == class)
        .map_or("Cannot directly construct object", |(_, m)| *m);
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

/// Registers the final, uncloneable, unserializable class `name`.
pub(crate) fn register_class(m: &mut ModuleBuilder<'_>, name: &str) {
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

/// A new instance of `class` carrying `state`.
pub(crate) fn wrap<T: 'static>(vm: &mut Vm, class: &str, state: T) -> VmResult<Value> {
    let Some(cls) = vm.find_class(class.as_bytes()) else {
        return Err(vm.throw("Error", &format!("Class \"{class}\" not found")));
    };
    let o = vm.new_object(&cls)?;
    object::set_internal(&o, state);
    Ok(Value::Object(o))
}

/// The object passed as argument `n` (its type was checked by ZPP).
pub(crate) fn obj_arg(a: &Args, n: usize) -> Option<Obj> {
    match a.at(n) {
        Value::Object(o) => Some(o),
        _ => None,
    }
}

/// Runs `f` on the native state of argument `n`.  `None` when the object
/// carries no state of that type (impossible for objects created by the
/// extension functions).
pub(crate) fn with_state<T: 'static, R>(
    a: &Args,
    n: usize,
    f: impl FnOnce(&mut T) -> R,
) -> Option<R> {
    obj_arg(a, n).and_then(|o| object::internal::<T, R>(&o, f))
}

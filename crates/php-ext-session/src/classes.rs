//! `SessionHandlerInterface`, `SessionIdInterface`,
//! `SessionUpdateTimestampHandlerInterface` and `SessionHandler`
//! (`mod_user_class.c`), plus the `PHP_SESSION_*` constants.

use php_runtime::Value;
use php_session::class;
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::host::with_host;

/// Registers the interfaces, the class and the constants.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    for (name, v) in class::CONSTANTS {
        m.constant(name, Value::Int(v));
    }
    let vm = m.vm();
    ClassBuilder::new("SessionHandlerInterface")
        .flags(class_flags::INTERFACE)
        .abstract_method("open(string $path, string $name): bool", acc::PUBLIC)
        .abstract_method("close(): bool", acc::PUBLIC)
        .abstract_method("read(string $id): string|false", acc::PUBLIC)
        .abstract_method("write(string $id, string $data): bool", acc::PUBLIC)
        .abstract_method("destroy(string $id): bool", acc::PUBLIC)
        .abstract_method("gc(int $max_lifetime): int|false", acc::PUBLIC)
        .register(vm);
    ClassBuilder::new("SessionIdInterface")
        .flags(class_flags::INTERFACE)
        .abstract_method("create_sid(): string", acc::PUBLIC)
        .register(vm);
    ClassBuilder::new("SessionUpdateTimestampHandlerInterface")
        .flags(class_flags::INTERFACE)
        .abstract_method("validateId(string $id): bool", acc::PUBLIC)
        .abstract_method("updateTimestamp(string $id, string $data): bool", acc::PUBLIC)
        .register(vm);
    ClassBuilder::new("SessionHandler")
        .implements(vm, &["SessionHandlerInterface", "SessionIdInterface"])
        .method("open(string $path, string $name): bool", acc::PUBLIC, open)
        .method("close(): bool", acc::PUBLIC, close)
        .method("read(string $id): string|false", acc::PUBLIC, read)
        .method("write(string $id, string $data): bool", acc::PUBLIC, write)
        .method("destroy(string $id): bool", acc::PUBLIC, destroy)
        .method("gc(int $max_lifetime): int|false", acc::PUBLIC, gc)
        .method("create_sid(): string", acc::PUBLIC, create_sid)
        .method("validateId(string $id): bool", acc::PUBLIC, validate_id)
        .register(vm);
}

fn open(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (path, name) = (a.str(0), a.str(1));
    with_host(vm, |h| {
        let r = class::session_handler_open(h, path.as_bytes(), name.as_bytes());
        h.vm_result(r).map(Value::Bool)
    })
}

fn close(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    with_host(vm, |h| {
        let r = class::session_handler_close(h);
        h.vm_result(r).map(Value::Bool)
    })
}

fn read(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let id = a.str(0);
    with_host(vm, |h| {
        let r = class::session_handler_read(h, id.as_bytes());
        h.vm_result(r)
    })
}

fn write(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (id, data) = (a.str(0), a.str(1));
    with_host(vm, |h| {
        let r = class::session_handler_write(h, id.as_bytes(), data.as_bytes());
        h.vm_result(r).map(Value::Bool)
    })
}

fn destroy(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let id = a.str(0);
    with_host(vm, |h| {
        let r = class::session_handler_destroy(h, id.as_bytes());
        h.vm_result(r).map(Value::Bool)
    })
}

fn gc(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let max = a.int(0);
    with_host(vm, |h| {
        let r = class::session_handler_gc(h, max);
        h.vm_result(r)
    })
}

fn create_sid(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    with_host(vm, |h| {
        let r = class::session_handler_create_sid(h);
        h.vm_result(r)
    })
}

fn validate_id(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let id = a.str(0);
    with_host(vm, |h| {
        let r = class::session_handler_validate_id(h, id.as_bytes());
        h.vm_result(r).map(Value::Bool)
    })
}

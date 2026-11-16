// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `SessionHandlerInterface`, `SessionIdInterface`,
//! `SessionUpdateTimestampHandlerInterface` and `SessionHandler`
//! (`mod_user_class.c`), plus the `PHP_SESSION_*` constants.

use std::rc::Rc;

use php_runtime::{ErrorLevel, Value, ZStr};
use php_session::class;
use php_vm::class::Class;
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ClassHooks, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::funcs::call;

/// Registers the interfaces, the class and the constants.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    for (name, v) in class::CONSTANTS {
        m.constant(name, Value::Int(v));
    }
    let vm = m.vm();
    ClassBuilder::new("SessionHandlerInterface")
        .flags(class_flags::INTERFACE)
        .hooks(ClassHooks {
            interface_gets_implemented: Some(handler_interface_implemented),
            ..ClassHooks::default()
        })
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
        .abstract_method(
            "updateTimestamp(string $id, string $data): bool",
            acc::PUBLIC,
        )
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

/// `session_handler_interface_gets_implemented()` (session.c): the PHP 9
/// `create_sid()` / `validateId()` warnings for a class implementing
/// `SessionHandlerInterface`. The method lookups see inherited methods;
/// the interface checks use `instanceof` on every implemented interface
/// (`session_interfaces_include`), so the interface order does not matter.
fn handler_interface_implemented(vm: &mut Vm, cls: &Rc<Class>) -> VmResult<()> {
    let implements = |lc: &[u8]| cls.interfaces.iter().any(|i| i.instance_of_name(lc));
    let warnings = class::handler_interface_implemented(
        &php_vm::object::display_name(&cls.name).to_string_lossy(),
        cls.find_method(b"create_sid").is_some(),
        implements(b"sessionidinterface"),
        cls.find_method(b"validateid").is_some(),
        implements(b"sessionupdatetimestamphandlerinterface"),
    );
    if warnings.is_empty() {
        return Ok(());
    }
    // `zend_error()` while the class is compiled: the declaration's file
    // and line.
    let file = cls
        .filename
        .clone()
        .unwrap_or_else(|| ZStr::from("Unknown"));
    for w in warnings {
        vm.error_at(ErrorLevel::Warning.bits(), &w, &file, cls.line_start)?;
    }
    Ok(())
}

fn open(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (path, name) = (a.str(0), a.str(1));
    call(vm, |h| {
        class::session_handler_open(h, path.as_bytes(), name.as_bytes())
    })
    .map(Value::Bool)
}

fn close(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    call(vm, |h| class::session_handler_close(h)).map(Value::Bool)
}

fn read(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let id = a.str(0);
    call(vm, |h| class::session_handler_read(h, id.as_bytes()))
}

fn write(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (id, data) = (a.str(0), a.str(1));
    call(vm, |h| {
        class::session_handler_write(h, id.as_bytes(), data.as_bytes())
    })
    .map(Value::Bool)
}

fn destroy(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let id = a.str(0);
    call(vm, |h| class::session_handler_destroy(h, id.as_bytes())).map(Value::Bool)
}

fn gc(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let max = a.int(0);
    call(vm, |h| class::session_handler_gc(h, max))
}

fn create_sid(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    call(vm, |h| class::session_handler_create_sid(h))
}

fn validate_id(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let id = a.str(0);
    call(vm, |h| class::session_handler_validate_id(h, id.as_bytes())).map(Value::Bool)
}

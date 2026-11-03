//! Core interfaces and classes (`zend_interfaces.c`, `zend_attributes.c`,
//! `stdClass`, enums interfaces).

use super::classbuilder::ClassBuilder;
use crate::class::cflags;
use crate::func::{flags, Args};
use crate::vm::{Vm, VmResult};
use php_runtime::{Value, ZStr};

const PUB: u32 = flags::PUBLIC;

fn attribute_construct(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let f = if a.has(0) { a.at(0) } else { Value::Int(63) };
    let d = crate::object::data(&o);
    if let Some(slot) = d.slots.borrow_mut().get_mut(0) {
        *slot = Some(f);
    }
    Ok(Value::Null)
}

fn noop_construct(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Null)
}

fn sensitive_value_construct(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    crate::object::set_internal(&o, a.at(0));
    Ok(Value::Null)
}

fn sensitive_value_get(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    Ok(crate::object::internal::<Value, _>(&o, |v| v.clone()).unwrap_or_default())
}

fn sensitive_value_debug_info(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(php_runtime::ZArray::new()))
}

fn deprecated_construct(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let d = crate::object::data(&o);
    let mut slots = d.slots.borrow_mut();
    if slots.len() >= 2 {
        slots[0] = Some(a.get(0).unwrap_or(Value::Null));
        slots[1] = Some(a.get(1).unwrap_or(Value::Null));
    }
    Ok(Value::Null)
}

/// Registers interfaces and core classes.
pub(crate) fn register(vm: &mut Vm) {
    ClassBuilder::new("stdClass").flags(cflags::ALLOW_DYNAMIC).register(vm);
    ClassBuilder::new("Traversable").flags(cflags::INTERFACE).register(vm);
    ClassBuilder::new("Iterator")
        .flags(cflags::INTERFACE)
        .implements(vm, &["Traversable"])
        .abstract_method("current(): mixed", PUB)
        .abstract_method("next(): void", PUB)
        .abstract_method("key(): mixed", PUB)
        .abstract_method("valid(): bool", PUB)
        .abstract_method("rewind(): void", PUB)
        .register(vm);
    ClassBuilder::new("IteratorAggregate")
        .flags(cflags::INTERFACE)
        .implements(vm, &["Traversable"])
        .abstract_method("getIterator(): Iterator", PUB)
        .register(vm);
    ClassBuilder::new("ArrayAccess")
        .flags(cflags::INTERFACE)
        .abstract_method("offsetExists(mixed $offset): bool", PUB)
        .abstract_method("offsetGet(mixed $offset): mixed", PUB)
        .abstract_method("offsetSet(mixed $offset, mixed $value): void", PUB)
        .abstract_method("offsetUnset(mixed $offset): void", PUB)
        .register(vm);
    ClassBuilder::new("Countable")
        .flags(cflags::INTERFACE)
        .abstract_method("count(): int", PUB)
        .register(vm);
    ClassBuilder::new("Stringable")
        .flags(cflags::INTERFACE)
        .abstract_method("__toString(): string", PUB)
        .register(vm);
    ClassBuilder::new("Serializable")
        .flags(cflags::INTERFACE)
        .abstract_method("serialize()", PUB)
        .abstract_method("unserialize(string $data)", PUB)
        .register(vm);
    ClassBuilder::new("JsonSerializable")
        .flags(cflags::INTERFACE)
        .abstract_method("jsonSerialize(): mixed", PUB)
        .register(vm);
    ClassBuilder::new("UnitEnum")
        .flags(cflags::INTERFACE)
        .abstract_method("cases(): array", PUB | flags::STATIC)
        .register(vm);
    ClassBuilder::new("BackedEnum")
        .flags(cflags::INTERFACE)
        .implements(vm, &["UnitEnum"])
        .abstract_method("from(int|string $value): static", PUB | flags::STATIC)
        .abstract_method("tryFrom(int|string $value): ?static", PUB | flags::STATIC)
        .register(vm);
    crate::exceptions::register(vm);
    for (name, parent) in [
        ("LogicException", "Exception"),
        ("BadFunctionCallException", "LogicException"),
        ("BadMethodCallException", "BadFunctionCallException"),
        ("DomainException", "LogicException"),
        ("InvalidArgumentException", "LogicException"),
        ("LengthException", "LogicException"),
        ("OutOfRangeException", "LogicException"),
        ("RuntimeException", "Exception"),
        ("OutOfBoundsException", "RuntimeException"),
        ("OverflowException", "RuntimeException"),
        ("RangeException", "RuntimeException"),
        ("UnderflowException", "RuntimeException"),
        ("UnexpectedValueException", "RuntimeException"),
        ("JsonException", "Exception"),
        ("AssertionError", "Error"),
    ] {
        ClassBuilder::new(name).extends(vm, parent).register(vm);
    }
    crate::closure::register(vm);
    crate::generator::register(vm);
    crate::fiber::register(vm);
    ClassBuilder::new("Attribute")
        .flags(cflags::FINAL)
        .prop("flags", PUB, Some("int"), None)
        .constant("TARGET_CLASS", Value::Int(1))
        .constant("TARGET_FUNCTION", Value::Int(2))
        .constant("TARGET_METHOD", Value::Int(4))
        .constant("TARGET_PROPERTY", Value::Int(8))
        .constant("TARGET_CLASS_CONSTANT", Value::Int(16))
        .constant("TARGET_PARAMETER", Value::Int(32))
        .constant("TARGET_CONSTANT", Value::Int(64))
        .constant("TARGET_ALL", Value::Int(127))
        .constant("IS_REPEATABLE", Value::Int(128))
        .method("__construct(int $flags = Attribute::TARGET_ALL)", PUB, attribute_construct)
        .register(vm);
    for n in ["ReturnTypeWillChange", "AllowDynamicProperties", "SensitiveParameter", "Override"] {
        ClassBuilder::new(n)
            .flags(cflags::FINAL)
            .method("__construct()", PUB, noop_construct)
            .register(vm);
    }
    ClassBuilder::new("Deprecated")
        .flags(cflags::FINAL)
        .prop("message", PUB | flags::READONLY, Some("?string"), None)
        .prop("since", PUB | flags::READONLY, Some("?string"), None)
        .method("__construct(?string $message = null, ?string $since = null)", PUB, deprecated_construct)
        .register(vm);
    ClassBuilder::new("NoDiscard")
        .flags(cflags::FINAL)
        .prop("message", PUB | flags::READONLY, Some("?string"), None)
        .method("__construct(?string $message = null)", PUB, deprecated_construct)
        .register(vm);
    ClassBuilder::new("SensitiveParameterValue")
        .flags(cflags::FINAL | cflags::NOT_SERIALIZABLE)
        .method("__construct(mixed $value)", PUB, sensitive_value_construct)
        .method("getValue(): mixed", PUB, sensitive_value_get)
        .method("__debugInfo(): array", PUB, sensitive_value_debug_info)
        .register(vm);
    ClassBuilder::new("__PHP_Incomplete_Class")
        .flags(cflags::FINAL | cflags::ALLOW_DYNAMIC)
        .register(vm);
    let _ = ZStr::empty();
}

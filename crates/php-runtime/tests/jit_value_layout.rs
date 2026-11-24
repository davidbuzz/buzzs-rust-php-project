// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `Value` memory layout the trace JIT reads through: the tag byte
//! at offset 0 and an `Int`'s payload at offset 8. `php-vm`'s JIT guards
//! compiled-variable slots by comparing that byte directly; if rustc
//! ever lays `Value` out differently, this test fails first.
//!
//! x86-64 only: the trace JIT runs nowhere else, so other targets need not
//! lay `Value` out this way. The AArch64 CI's build (rustc 1.100) does not:
//! its `None` tag is 255, not 9.
#![cfg(target_arch = "x86_64")]

use php_runtime::{Obj, Value};

fn bytes(v: &Value) -> [u8; 16] {
    // SAFETY: `Value` is 16 bytes (`value_is_16_bytes` in the crate's
    // own tests); this only copies it.
    unsafe { std::mem::transmute_copy(v) }
}

#[test]
fn tag_byte_and_int_payload() {
    // The tag byte (rustc's discriminant, first) for each variant the
    // JIT's guards name, in enum order plus `Option`'s `None` tag.
    assert_eq!(bytes(&Value::Null)[0], 0);
    assert_eq!(bytes(&Value::Bool(false))[0], 1);
    assert_eq!(bytes(&Value::Int(7))[0], 2);
    assert_eq!(bytes(&Value::Float(1.0))[0], 3);
    assert_eq!(bytes(&Value::string("s"))[0], 4);

    // An `Int`'s payload: the i64 at offset 8. (The bytes between the
    // tag byte and the payload are padding: rustc does not guarantee
    // they read as anything, so the JIT never looks at them.)
    let v = Value::Int(-0x1122_3344_5566_7788i64);
    let b = bytes(&v);
    assert_eq!(
        u64::from_le_bytes(b[8..16].try_into().unwrap()),
        v.as_int().unwrap() as u64
    );

    // `None` (an undefined compiled variable) has its own tag; `Some`
    // shares the variant's.
    let n: Option<Value> = None;
    // SAFETY: `Option<Value>` is 16 bytes, as `value_is_16_bytes` pins.
    let b: [u8; 16] = unsafe { std::mem::transmute_copy(&n) };
    assert_eq!(b[0], 9);
    let b = bytes(&n.unwrap_or(Value::Int(42)));
    assert_eq!(b[0], 2);
    assert_eq!(u64::from_le_bytes(b[8..16].try_into().unwrap()), 42);
}

#[test]
fn object_tag_and_payload_reach() {
    // The `Object` tag (variant ordinal): the JIT's receiver guard
    // compares it before dereferencing the handle.
    assert_eq!(bytes(&Value::Object(Obj::dummy("C")))[0], 6);

    // The handle payload is the allocation's base; the `ObjCell<T>`
    // payload (what an `ObjectData` pointer names) sits one fixed,
    // align-8 displacement behind it: past the 32-byte header (the fat
    // `Rc<dyn PhpObject>` plus its `TypeId`), the JIT's `OBJ_PAYLOAD`.
    assert_eq!(php_runtime::value::obj_payload_offset::<u64>(), 32);
}

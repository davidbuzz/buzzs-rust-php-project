//! The internal array pointer: `current`/`pos`, `key`, `next`, `prev`,
//! `reset`, `end`.
//!
//! Objects are accepted with the "Calling X() on an object is deprecated"
//! deprecation and operate on a snapshot of the object's property table
//! (the VM owns property tables, so pointer movement on objects does not
//! persist).

use crate::util::deprecated;
use crate::ArrayContext;
use php_runtime::{HashTable, PhpRef, PropertyPurpose, RtResult, RuntimeError, Value, ZArray};

fn object_table<C: ArrayContext + ?Sized>(
    ctx: &mut C,
    func: &str,
    v: &Value,
) -> RtResult<Option<ZArray>> {
    match v {
        Value::Array(a) => Ok(Some(a.clone())),
        Value::Object(o) => {
            deprecated(
                ctx,
                func,
                &format!("Calling {func}() on an object is deprecated"),
            )?;
            Ok(Some(
                ctx.object_properties(o, PropertyPurpose::ArrayCast)?
                    .unwrap_or_default(),
            ))
        }
        Value::Reference(r) => object_table(ctx, func, &r.get()),
        other => Err(RuntimeError::TypeError(format!(
            "{func}(): Argument #1 ($array) must be of type array, {} given",
            other.value_name()
        ))),
    }
}

/// `php_array_iter_return_current`: the dereferenced value at the internal
/// pointer, or `false`.
fn current_value(ht: &HashTable) -> Value {
    ht.current().map_or(Value::Bool(false), crate::util::deref)
}

fn move_pointer<C: ArrayContext + ?Sized>(
    ctx: &mut C,
    func: &str,
    array: &PhpRef,
    op: impl FnOnce(&mut HashTable),
) -> RtResult<Value> {
    let v = array.get();
    if let Value::Array(_) = v {
        drop(v);
        // Clone the raw element while the reference is borrowed; dereference
        // it afterwards (it may be a reference to this very variable).
        let raw = {
            let mut slot = array.borrow_mut();
            let Value::Array(a) = &mut *slot else {
                return Ok(Value::Bool(false));
            };
            if a.is_empty() {
                return Ok(Value::Bool(false));
            }
            let ht = a.make_mut();
            op(ht);
            ht.current().cloned()
        };
        return Ok(raw.map_or(Value::Bool(false), |v| crate::util::deref(&v)));
    }
    let Some(mut a) = object_table(ctx, func, &v)? else {
        return Ok(Value::Bool(false));
    };
    if a.is_empty() {
        return Ok(Value::Bool(false));
    }
    let ht = a.make_mut();
    op(ht);
    Ok(current_value(ht))
}

/// `end(array|object &$array): mixed`.
pub fn end<C: ArrayContext + ?Sized>(ctx: &mut C, array: &PhpRef) -> RtResult<Value> {
    move_pointer(ctx, "end", array, HashTable::end)
}

/// `prev(array|object &$array): mixed`.
pub fn prev<C: ArrayContext + ?Sized>(ctx: &mut C, array: &PhpRef) -> RtResult<Value> {
    move_pointer(ctx, "prev", array, HashTable::move_prev)
}

/// `next(array|object &$array): mixed`.
pub fn next<C: ArrayContext + ?Sized>(ctx: &mut C, array: &PhpRef) -> RtResult<Value> {
    move_pointer(ctx, "next", array, HashTable::move_next)
}

/// `reset(array|object &$array): mixed`.
pub fn reset<C: ArrayContext + ?Sized>(ctx: &mut C, array: &PhpRef) -> RtResult<Value> {
    move_pointer(ctx, "reset", array, HashTable::reset)
}

/// `current(array|object $array): mixed` (and its alias `pos`, whose name
/// is passed as `func` for the object deprecation).
pub fn current<C: ArrayContext + ?Sized>(
    ctx: &mut C,
    func: &str,
    array: &Value,
) -> RtResult<Value> {
    if let Value::Array(a) = array {
        return Ok(current_value(a));
    }
    Ok(object_table(ctx, func, array)?.map_or(Value::Bool(false), |a| current_value(&a)))
}

/// `key(array|object $array): int|string|null`.
pub fn key<C: ArrayContext + ?Sized>(ctx: &mut C, array: &Value) -> RtResult<Value> {
    let k = |a: &HashTable| {
        a.current_key()
            .map_or(Value::Null, php_runtime::ArrayKey::to_value)
    };
    if let Value::Array(a) = array {
        return Ok(k(a));
    }
    Ok(object_table(ctx, "key", array)?.map_or(Value::Null, |a| k(&a)))
}

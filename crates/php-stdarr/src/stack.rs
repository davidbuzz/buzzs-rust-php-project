//! `array_push`, `array_pop`, `array_shift`, `array_unshift`,
//! `array_splice`.

use crate::util::{cannot_add_element, deref, with_array};
use php_runtime::{PhpRef, RtResult, RuntimeContext, Value, ZArray};

fn len_i64(a: &ZArray) -> i64 {
    i64::try_from(a.len()).unwrap_or(i64::MAX)
}

/// `array_push(array &$array, mixed ...$values): int`.
pub fn array_push(array: &PhpRef, values: &[Value]) -> RtResult<i64> {
    with_array(array, "array_push", 1, "array", |a| {
        if !values.is_empty() {
            let t = a.make_mut();
            for v in values {
                t.push(v.clone()).map_err(|_| cannot_add_element())?;
            }
        }
        Ok(len_i64(a))
    })
}

/// `array_pop(array &$array): mixed`.
pub fn array_pop(array: &PhpRef) -> RtResult<Value> {
    let v = with_array(array, "array_pop", 1, "array", |a| {
        if a.is_empty() {
            return Ok(Value::Null);
        }
        Ok(a.make_mut().pop().unwrap_or_default())
    })?;
    Ok(deref(&v))
}

/// `array_shift(array &$array): mixed`.
pub fn array_shift(array: &PhpRef) -> RtResult<Value> {
    let v = with_array(array, "array_shift", 1, "array", |a| {
        if a.is_empty() {
            return Ok(Value::Null);
        }
        Ok(a.make_mut().shift().unwrap_or_default())
    })?;
    Ok(deref(&v))
}

/// `array_unshift(array &$array, mixed ...$values): int`.
pub fn array_unshift(array: &PhpRef, values: &[Value]) -> RtResult<i64> {
    with_array(array, "array_unshift", 1, "array", |a| {
        let t = a.make_mut();
        t.unshift(values.to_vec());
        t.reset();
        Ok(len_i64(a))
    })
}

/// `array_splice(array &$array, int $offset, ?int $length = null, mixed $replacement = []): array`.
///
/// `replacement` is converted with `(array)` semantics when given.
pub fn array_splice<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    array: &PhpRef,
    offset: i64,
    length: Option<i64>,
    replacement: Option<&Value>,
) -> RtResult<ZArray> {
    let repl = match replacement {
        Some(v) => Some(v.to_array(ctx)?),
        None => None,
    };
    with_array(array, "array_splice", 1, "array", |a| {
        let num_in = len_i64(a);
        let length = length.unwrap_or(num_in);
        let removed = a
            .make_mut()
            .splice(offset, length, repl.as_deref(), true)
            .unwrap_or_default();
        Ok(ZArray::from_table(removed))
    })
}

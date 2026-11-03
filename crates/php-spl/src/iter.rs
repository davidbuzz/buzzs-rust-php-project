//! Iteration over arbitrary traversables (`get_iterator`) and the
//! `iterator_to_array()`, `iterator_count()`, `iterator_apply()` functions.

use crate::args::{value_name, Args};
use crate::exceptions::{error, exception};
use crate::host::{MethodIterator, ObjectIterator, SplHost};
use php_runtime::convert::dval_to_lval_safe;
use php_runtime::{ArrayKey, ErrorLevel, HashTable, Obj, RtResult, Value, ZArray, ZStr};

/// `ce->get_iterator(ce, obj, by_ref)` for any object: the VM's foreign
/// iterators, SPL native iterators, `Iterator` objects (method calls) and
/// `IteratorAggregate` objects (`getIterator()`, recursively).
pub fn get_iterator(
    host: &mut dyn SplHost,
    obj: &Obj,
    by_ref: bool,
) -> RtResult<Box<dyn ObjectIterator>> {
    if let Some(it) = host.foreign_iterator(obj) {
        return it;
    }
    if let Some(it) = crate::handlers::native_iterator(host, obj, by_ref)? {
        return Ok(it);
    }
    if host.instance_of(obj, "Iterator") {
        if by_ref {
            return Err(error(
                "An iterator cannot be used with foreach by reference",
            ));
        }
        return Ok(Box::new(MethodIterator::new(obj.clone())));
    }
    if host.instance_of(obj, "IteratorAggregate") {
        let r = host.call_method(obj, "getIterator", &[])?;
        return match r.deref() {
            Value::Object(o) if host.instance_of(&o, "Traversable") => {
                get_iterator(host, &o, by_ref)
            }
            _ => Err(exception(format!(
                "Objects returned by {}::getIterator() must be traversable or implement interface Iterator",
                obj.class_name()
            ))),
        };
    }
    Err(error(format!(
        "Object of type {} is not traversable",
        obj.class_name()
    )))
}

/// `zend_create_internal_iterator_zval`: an `InternalIterator` over the
/// object's native iterator.
pub fn internal_iterator_object(host: &mut dyn SplHost, obj: &Obj) -> RtResult<Value> {
    let it = get_iterator(host, obj, false)?;
    host.new_internal_iterator(it)
}

/// `array_set_zval_key`: stores `value` under an arbitrary key value.
pub fn set_zval_key(
    host: &mut dyn SplHost,
    ht: &mut HashTable,
    key: &Value,
    value: Value,
) -> RtResult<()> {
    let k = match key.deref() {
        Value::Str(s) => ArrayKey::from_zstr(&s),
        Value::Resource(r) => {
            host.diagnostic(
                ErrorLevel::Warning,
                &format!(
                    "Resource ID#{} used as offset, casting to integer ({})",
                    r.handle(),
                    r.handle()
                ),
            )?;
            ArrayKey::Int(r.handle())
        }
        Value::Bool(b) => ArrayKey::Int(i64::from(b)),
        Value::Int(n) => ArrayKey::Int(n),
        Value::Float(d) => ArrayKey::Int(dval_to_lval_safe(d, host)?),
        Value::Null => {
            host.diagnostic(
                ErrorLevel::Deprecated,
                "Using null as an array offset is deprecated, use an empty string instead",
            )?;
            ArrayKey::Str(ZStr::empty())
        }
        other => {
            return Err(crate::exceptions::type_error(format!(
                "Cannot access offset of type {} on array",
                other.type_name()
            )))
        }
    };
    ht.insert(k, value);
    Ok(())
}

/// `spl_iterator_apply`: drives an object iterator, calling `f` for each
/// element until it returns `false`.
pub fn apply(
    host: &mut dyn SplHost,
    obj: &Obj,
    f: &mut dyn FnMut(&mut dyn SplHost, &mut dyn ObjectIterator) -> RtResult<bool>,
) -> RtResult<()> {
    let mut it = get_iterator(host, obj, false)?;
    it.rewind(host)?;
    while it.valid(host)? {
        if !f(host, it.as_mut())? {
            break;
        }
        it.next(host)?;
    }
    Ok(())
}

fn iterable_arg(a: &Args<'_>, i: usize, ty: &str, host: &mut dyn SplHost) -> RtResult<Value> {
    match a.raw(i) {
        v @ Value::Array(_) => Ok(v),
        Value::Object(o) if host.instance_of(&o, "Traversable") => Ok(Value::Object(o)),
        _ => Err(a.type_error(i, ty)),
    }
}

/// `iterator_to_array(Traversable|array $iterator, bool $preserve_keys = true): array`.
pub fn iterator_to_array(host: &mut dyn SplHost, args: &[Value]) -> RtResult<Value> {
    let a = Args::new(
        "iterator_to_array",
        args,
        &["iterator", "preserve_keys"],
        1,
        Some(2),
    )?;
    let it = iterable_arg(&a, 0, "Traversable|array", host)?;
    let use_keys = a.opt_bool(host, 1, true)?;
    let obj = match it {
        Value::Array(arr) => {
            return Ok(Value::Array(if use_keys {
                arr
            } else {
                ZArray::from_table(arr.to_list())
            }))
        }
        Value::Object(o) => o,
        _ => unreachable!("checked by iterable_arg"),
    };
    let mut out = HashTable::new();
    apply(host, &obj, &mut |host, it| {
        let Some(data) = it.current(host)? else {
            return Ok(false);
        };
        if use_keys {
            match it.key(host)? {
                Some(k) => set_zval_key(host, &mut out, &k, data)?,
                None => {
                    let _ = out.push(data);
                }
            }
        } else {
            let _ = out.push(data);
        }
        Ok(true)
    })?;
    Ok(Value::Array(ZArray::from_table(out)))
}

/// `iterator_count(Traversable|array $iterator): int`.
pub fn iterator_count(host: &mut dyn SplHost, args: &[Value]) -> RtResult<Value> {
    let a = Args::new("iterator_count", args, &["iterator"], 1, Some(1))?;
    let it = iterable_arg(&a, 0, "Traversable|array", host)?;
    let obj = match it {
        Value::Array(arr) => return Ok(Value::Int(crate::util::to_long(arr.len()))),
        Value::Object(o) => o,
        _ => unreachable!("checked by iterable_arg"),
    };
    let mut count: i64 = 0;
    apply(host, &obj, &mut |_, _| {
        if count == i64::MAX {
            return Ok(false);
        }
        count += 1;
        Ok(true)
    })?;
    Ok(Value::Int(count))
}

/// `iterator_apply(Traversable $iterator, callable $callback, ?array $args = null): int`.
pub fn iterator_apply(host: &mut dyn SplHost, args: &[Value]) -> RtResult<Value> {
    let a = Args::new(
        "iterator_apply",
        args,
        &["iterator", "callback", "args"],
        2,
        Some(3),
    )?;
    let obj = a.object_of(host, 0, "Traversable")?;
    let callback = a.callable(host, 1)?;
    let params: Vec<Value> = match a.raw(2) {
        Value::Null => Vec::new(),
        Value::Array(p) => p.values().cloned().collect(),
        _ => return Err(a.type_error(2, "?array")),
    };
    let mut count: i64 = 0;
    apply(host, &obj, &mut |host, _| {
        count += 1;
        let r = host.call_callable(&callback, &params)?;
        r.to_bool(host)
    })?;
    Ok(Value::Int(count))
}

/// The type name for "must be of type X, Y given" messages.
#[must_use]
pub fn given(v: &Value) -> String {
    value_name(v)
}

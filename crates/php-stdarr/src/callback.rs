//! Callback-driven functions: `array_walk`, `array_walk_recursive`,
//! `array_map`, `array_filter`, `array_reduce`, `array_find`,
//! `array_find_key`, `array_any`, `array_all`.

use crate::util::{arg_value_error, copy_elem, deprecated, deref, not_array_error, Guard};
use crate::Callback;
use crate::{ARRAY_FILTER_USE_BOTH, ARRAY_FILTER_USE_KEY, ARRAY_FILTER_USE_VALUE};
use php_runtime::{
    ArrayKey, HashTable, IterId, PhpRef, PropertyPurpose, RtResult, RuntimeContext, RuntimeError,
    Value, ZArray,
};

// ---------------------------------------------------------------------------
// array_walk
// ---------------------------------------------------------------------------

/// One step of the walk: the key and the (now referenced) element at the
/// iterator's position, advancing the iterator. `Ok(None)` at the end.
fn walk_fetch(target: &PhpRef, iter: IterId) -> RtResult<Option<(ArrayKey, PhpRef)>> {
    let mut slot = target.borrow_mut();
    let Value::Array(a) = &mut *slot else {
        return Err(RuntimeError::TypeError(
            "Iterated value is no longer an array or object".to_string(),
        ));
    };
    let ht = a.make_mut();
    let pos = ht.iterator_pos(iter);
    let p = ht.valid_pos(pos);
    let Some((key, _)) = ht.at(p) else {
        return Ok(None);
    };
    let key = key.clone();
    let Some(elem) = ht.at_mut(p) else {
        return Ok(None);
    };
    let r = elem.make_ref();
    let next = ht.pos_forward(p);
    ht.iterator_set_pos(iter, next);
    Ok(Some((key, r)))
}

fn walk_inner<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    target: &PhpRef,
    callback: &mut Callback<'_, C>,
    userdata: Option<&Value>,
    recursive: bool,
    guard: &mut Guard,
) -> RtResult<()> {
    let iter = {
        let mut slot = target.borrow_mut();
        let Value::Array(a) = &mut *slot else {
            return Ok(());
        };
        if a.is_empty() {
            return Ok(());
        }
        let ht = a.make_mut();
        let pos = ht.valid_pos(0);
        ht.iterator_add(pos)
    };
    let mut result = Ok(());
    loop {
        let (key, elem) = match walk_fetch(target, iter) {
            Ok(Some(step)) => step,
            Ok(None) => break,
            Err(e) => {
                result = Err(e);
                break;
            }
        };
        let is_array = matches!(&*elem.borrow(), Value::Array(_));
        let step = if recursive && is_array {
            let addr = {
                let mut b = elem.borrow_mut();
                match &mut *b {
                    Value::Array(a) => {
                        a.make_mut();
                        a.addr()
                    }
                    _ => 0,
                }
            };
            if guard.contains(addr) {
                Err(RuntimeError::Error("Recursion detected".to_string()))
            } else {
                guard.push(addr);
                let r = walk_inner(ctx, &elem, callback, userdata, recursive, guard);
                guard.pop(addr);
                r
            }
        } else {
            let mut args = Vec::with_capacity(3);
            args.push(Value::Reference(elem));
            args.push(key.to_value());
            if let Some(u) = userdata {
                args.push(u.clone());
            }
            callback(ctx, &args).map(drop)
        };
        if let Err(e) = step {
            result = Err(e);
            break;
        }
    }
    if let Value::Array(a) = &mut *target.borrow_mut() {
        a.make_mut().iterator_del(iter);
    }
    result
}

fn php_walk<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    func: &str,
    array: &PhpRef,
    callback: &mut Callback<'_, C>,
    userdata: Option<&Value>,
    recursive: bool,
) -> RtResult<()> {
    let snapshot = match array.get() {
        Value::Array(_) => None,
        Value::Object(o) => {
            deprecated(
                ctx,
                func,
                &format!(
                    "Passing an object for argument #1 $array to {func}() is deprecated, call get_object_vars() first instead"
                ),
            )?;
            let props = ctx
                .object_properties(&o, PropertyPurpose::ArrayCast)?
                .unwrap_or_default();
            Some(PhpRef::new(Value::Array(props)))
        }
        other => return Err(not_array_error(func, 1, Some("array"), &other)),
    };
    let target = snapshot.as_ref().unwrap_or(array);
    walk_inner(
        ctx,
        target,
        callback,
        userdata,
        recursive,
        &mut Guard::default(),
    )
}

/// `array_walk(array|object &$array, callable $callback, mixed $arg = UNKNOWN): true`.
///
/// The callback receives `(&$value, $key[, $arg])`; `$value` is passed as a
/// [`Value::Reference`] to the element, so a by-reference parameter
/// modifies the array.
pub fn array_walk<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    array: &PhpRef,
    callback: &mut Callback<'_, C>,
    userdata: Option<&Value>,
) -> RtResult<()> {
    php_walk(ctx, "array_walk", array, callback, userdata, false)
}

/// `array_walk_recursive(array|object &$array, callable $callback, mixed $arg = UNKNOWN): true`.
pub fn array_walk_recursive<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    array: &PhpRef,
    callback: &mut Callback<'_, C>,
    userdata: Option<&Value>,
) -> RtResult<()> {
    php_walk(ctx, "array_walk_recursive", array, callback, userdata, true)
}

// ---------------------------------------------------------------------------
// array_map
// ---------------------------------------------------------------------------

/// `array_map(?callable $callback, array $array, array ...$arrays): array`.
///
/// `arrays` are the array arguments (argument #2 onwards).
pub fn array_map<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    callback: Option<&mut Callback<'_, C>>,
    arrays: &[Value],
) -> RtResult<ZArray> {
    const F: &str = "array_map";
    if let [single] = arrays {
        let Value::Array(input) = single else {
            return Err(not_array_error(F, 2, Some("array"), single));
        };
        let Some(cb) = callback else {
            return Ok(input.clone());
        };
        if input.is_empty() {
            return Ok(input.clone());
        }
        let mut out = HashTable::with_capacity(input.len());
        if input.is_packed() {
            for pos in 0..input.used() {
                if let Some((_, v)) = input.at(pos) {
                    let r = cb(ctx, std::slice::from_ref(v))?;
                    out.insert(i64::from(pos), r);
                }
            }
        } else {
            for (k, v) in input.iter() {
                let r = cb(ctx, std::slice::from_ref(v))?;
                let _ = out.add(k.clone(), r);
            }
        }
        return Ok(ZArray::from_table(out));
    }
    let mut tables = Vec::with_capacity(arrays.len());
    let mut maxlen = 0usize;
    for (i, a) in arrays.iter().enumerate() {
        let Value::Array(t) = a else {
            return Err(not_array_error(
                F,
                i + 2,
                if i == 0 { Some("array") } else { None },
                a,
            ));
        };
        maxlen = maxlen.max(t.len());
        tables.push(t);
    }
    if maxlen == 0 {
        return Ok(ZArray::new());
    }
    let mut iters: Vec<_> = tables.iter().map(|t| t.values()).collect();
    let mut out = HashTable::with_capacity(maxlen);
    let mut cb = callback;
    for _ in 0..maxlen {
        let params: Vec<Value> = iters
            .iter_mut()
            .map(|it| it.next().cloned().unwrap_or(Value::Null))
            .collect();
        let v = match cb.as_deref_mut() {
            None => Value::Array(ZArray::from_values(params)),
            Some(cb) => cb(ctx, &params)?,
        };
        let _ = out.push(v);
    }
    Ok(ZArray::from_table(out))
}

// ---------------------------------------------------------------------------
// array_filter / array_reduce
// ---------------------------------------------------------------------------

/// `array_filter(array $array, ?callable $callback = null, int $mode = 0): array`.
pub fn array_filter<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    array: &HashTable,
    callback: Option<&mut Callback<'_, C>>,
    mode: i64,
) -> RtResult<ZArray> {
    if ![
        ARRAY_FILTER_USE_VALUE,
        ARRAY_FILTER_USE_BOTH,
        ARRAY_FILTER_USE_KEY,
    ]
    .contains(&mode)
    {
        return Err(arg_value_error(
            "array_filter",
            3,
            Some("mode"),
            "must be one of ARRAY_FILTER_USE_VALUE, ARRAY_FILTER_USE_KEY, or ARRAY_FILTER_USE_BOTH",
        ));
    }
    if array.is_empty() {
        return Ok(ZArray::new());
    }
    let mut out = HashTable::new();
    let mut cb = callback;
    for (k, v) in array {
        let keep = match cb.as_deref_mut() {
            Some(cb) => {
                let args: Vec<Value> = match mode {
                    ARRAY_FILTER_USE_KEY => vec![k.to_value()],
                    ARRAY_FILTER_USE_BOTH => vec![v.clone(), k.to_value()],
                    _ => vec![v.clone()],
                };
                let r = cb(ctx, &args)?;
                r.to_bool(ctx)?
            }
            None => v.to_bool(ctx)?,
        };
        if keep {
            let _ = out.add(k.clone(), copy_elem(v));
        }
    }
    Ok(ZArray::from_table(out))
}

/// `array_reduce(array $array, callable $callback, mixed $initial = null): mixed`.
pub fn array_reduce<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    array: &HashTable,
    callback: &mut Callback<'_, C>,
    initial: Option<&Value>,
) -> RtResult<Value> {
    let mut acc = initial.cloned().unwrap_or(Value::Null);
    for v in array.values() {
        let r = callback(ctx, &[acc, v.clone()])?;
        acc = deref(&r);
    }
    Ok(acc)
}

// ---------------------------------------------------------------------------
// array_find family
// ---------------------------------------------------------------------------

/// `php_array_find`: the first `(key, value)` for which the callback's
/// result (converted to bool, xor `negate`) is true.
pub fn php_array_find<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    array: &HashTable,
    callback: &mut Callback<'_, C>,
    negate: bool,
) -> RtResult<Option<(ArrayKey, Value)>> {
    for (k, v) in array {
        let r = callback(ctx, &[v.clone(), k.to_value()])?;
        if r.to_bool(ctx)? ^ negate {
            return Ok(Some((k.clone(), deref(v))));
        }
    }
    Ok(None)
}

/// `array_find(array $array, callable $callback): mixed`.
pub fn array_find<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    array: &HashTable,
    callback: &mut Callback<'_, C>,
) -> RtResult<Value> {
    Ok(php_array_find(ctx, array, callback, false)?.map_or(Value::Null, |(_, v)| v))
}

/// `array_find_key(array $array, callable $callback): mixed`.
pub fn array_find_key<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    array: &HashTable,
    callback: &mut Callback<'_, C>,
) -> RtResult<Value> {
    Ok(php_array_find(ctx, array, callback, false)?.map_or(Value::Null, |(k, _)| k.to_value()))
}

/// `array_any(array $array, callable $callback): bool`.
pub fn array_any<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    array: &HashTable,
    callback: &mut Callback<'_, C>,
) -> RtResult<bool> {
    Ok(php_array_find(ctx, array, callback, false)?.is_some())
}

/// `array_all(array $array, callable $callback): bool`.
pub fn array_all<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    array: &HashTable,
    callback: &mut Callback<'_, C>,
) -> RtResult<bool> {
    Ok(php_array_find(ctx, array, callback, true)?.is_none())
}

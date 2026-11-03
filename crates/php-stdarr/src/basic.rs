//! Non-callback array functions: `count`, `min`/`max`, `in_array`,
//! `array_search`, `array_keys`, `array_values`, `array_count_values`,
//! `array_flip`, `array_change_key_case`, `array_key_first`/`last`,
//! `array_first`/`last`, `array_is_list`, `array_key_exists`,
//! `array_fill`, `array_fill_keys`, `array_pad`, `array_reverse`,
//! `array_slice`, `array_chunk`, `array_combine`, `array_column`,
//! `array_sum`, `array_product`.

use crate::util::{
    arg_type_error, arg_value_error, cannot_add_element, copy_elem, deref, not_array_error, warn,
    Guard,
};
use crate::{ArrayContext, CASE_LOWER, CASE_UPPER, COUNT_NORMAL, COUNT_RECURSIVE, HT_MAX_SIZE};
use php_runtime::convert::dval_to_lval_silent;
use php_runtime::{
    ops, ArrayKey, CastTarget, HashTable, RtResult, RuntimeContext, RuntimeError, Value, ZArray,
    ZStr,
};

// ---------------------------------------------------------------------------
// count
// ---------------------------------------------------------------------------

fn count_recursive_inner<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    func: &str,
    a: &ZArray,
    guard: &mut Guard,
) -> RtResult<i64> {
    let addr = a.addr();
    if guard.contains(addr) {
        warn(ctx, func, "Recursion detected")?;
        return Ok(0);
    }
    guard.push(addr);
    let mut cnt = i64::try_from(a.len()).unwrap_or(i64::MAX);
    let mut res = Ok(());
    for v in a.values() {
        if let Value::Array(sub) = deref(v) {
            match count_recursive_inner(ctx, func, &sub, guard) {
                Ok(n) => cnt += n,
                Err(e) => {
                    res = Err(e);
                    break;
                }
            }
        }
    }
    guard.pop(addr);
    res.map(|()| cnt)
}

/// `php_count_recursive`: the number of elements of `a` and of all nested
/// arrays, warning "Recursion detected" on cycles. `func` is the PHP
/// function name used as the warning prefix.
pub fn php_count_recursive<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    func: &str,
    a: &ZArray,
) -> RtResult<i64> {
    count_recursive_inner(ctx, func, a, &mut Guard::default())
}

/// `count(Countable|array $value, int $mode = COUNT_NORMAL): int` (and its
/// alias `sizeof`, whose name is passed as `func`).
pub fn count<C: ArrayContext + ?Sized>(
    ctx: &mut C,
    func: &str,
    value: &Value,
    mode: i64,
) -> RtResult<i64> {
    if mode != COUNT_NORMAL && mode != COUNT_RECURSIVE {
        return Err(arg_value_error(
            func,
            2,
            Some("mode"),
            "must be either COUNT_NORMAL or COUNT_RECURSIVE",
        ));
    }
    match value {
        Value::Array(a) => {
            if mode == COUNT_RECURSIVE {
                php_count_recursive(ctx, func, a)
            } else {
                Ok(i64::try_from(a.len()).unwrap_or(i64::MAX))
            }
        }
        Value::Reference(r) => count(ctx, func, &r.get(), mode),
        Value::Object(o) => match ctx.count_object(o)? {
            Some(n) => Ok(n),
            None => Err(countable_error(func, value)),
        },
        _ => Err(countable_error(func, value)),
    }
}

fn countable_error(func: &str, value: &Value) -> RuntimeError {
    arg_type_error(
        func,
        1,
        Some("value"),
        &format!(
            "must be of type Countable|array, {} given",
            value.value_name()
        ),
    )
}

// ---------------------------------------------------------------------------
// min / max
// ---------------------------------------------------------------------------

fn minmax_array<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    func: &str,
    arg: &Value,
    max: bool,
) -> RtResult<Value> {
    let Value::Array(a) = arg else {
        return Err(not_array_error(func, 1, Some("value"), arg));
    };
    let mut result: Option<&Value> = None;
    for v in a.values() {
        match result {
            None => result = Some(v),
            Some(r) => {
                let cmp = ops::compare(r, v, ctx)?;
                if (max && cmp < 0) || (!max && cmp > 0) {
                    result = Some(v);
                }
            }
        }
    }
    match result {
        Some(v) => Ok(deref(v)),
        None => Err(arg_value_error(
            func,
            1,
            Some("value"),
            "must contain at least one element",
        )),
    }
}

#[allow(clippy::cast_precision_loss)]
fn minmax<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    func: &str,
    args: &[Value],
    max: bool,
) -> RtResult<Value> {
    enum Mode {
        Long(i64),
        Double(f64),
        Generic,
    }
    match args {
        [] => Err(RuntimeError::Error(format!(
            "{func}() expects at least 1 argument, 0 given"
        ))),
        [one] => minmax_array(ctx, func, one, max),
        [first, rest @ ..] => {
            let better_f = |cur: f64, x: f64| if max { cur < x } else { cur > x };
            let mut idx = 0usize;
            let mut mode = match first {
                Value::Int(l) => Mode::Long(*l),
                Value::Float(d) => Mode::Double(*d),
                _ => Mode::Generic,
            };
            for (j, arg) in rest.iter().enumerate() {
                let i = j + 1;
                loop {
                    match mode {
                        Mode::Long(l) => match arg {
                            Value::Int(x) => {
                                if (max && l < *x) || (!max && l > *x) {
                                    mode = Mode::Long(*x);
                                    idx = i;
                                }
                            }
                            Value::Float(_) if dval_to_lval_silent(l as f64) == l => {
                                mode = Mode::Double(l as f64);
                                continue;
                            }
                            _ => {
                                mode = Mode::Generic;
                                continue;
                            }
                        },
                        Mode::Double(d) => match arg {
                            Value::Float(x) => {
                                if better_f(d, *x) {
                                    mode = Mode::Double(*x);
                                    idx = i;
                                }
                            }
                            Value::Int(x) if dval_to_lval_silent(*x as f64) == *x => {
                                if better_f(d, *x as f64) {
                                    mode = Mode::Double(*x as f64);
                                    idx = i;
                                }
                            }
                            _ => {
                                mode = Mode::Generic;
                                continue;
                            }
                        },
                        Mode::Generic => {
                            let c = ops::compare(arg, &args[idx], ctx)?;
                            if (max && c > 0) || (!max && c < 0) {
                                idx = i;
                            }
                        }
                    }
                    break;
                }
            }
            Ok(args[idx].clone())
        }
    }
}

/// `min(mixed $value, mixed ...$values): mixed`.
pub fn min<C: RuntimeContext + ?Sized>(ctx: &mut C, args: &[Value]) -> RtResult<Value> {
    minmax(ctx, "min", args, false)
}

/// `max(mixed $value, mixed ...$values): mixed`.
pub fn max<C: RuntimeContext + ?Sized>(ctx: &mut C, args: &[Value]) -> RtResult<Value> {
    minmax(ctx, "max", args, true)
}

// ---------------------------------------------------------------------------
// Searching
// ---------------------------------------------------------------------------

/// `_php_search_array`: the key of the first element equal (`==`, or `===`
/// with `strict`) to `needle`.
pub fn search<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    needle: &Value,
    haystack: &HashTable,
    strict: bool,
) -> RtResult<Option<ArrayKey>> {
    for (k, v) in haystack {
        let found = if strict {
            match (needle, &deref(v)) {
                (Value::Int(a), Value::Int(b)) => a == b,
                (Value::Int(_), _) => false,
                (_, e) => ops::is_identical(needle, e)?,
            }
        } else {
            ops::is_equal(needle, v, ctx)?
        };
        if found {
            return Ok(Some(k.clone()));
        }
    }
    Ok(None)
}

/// `in_array(mixed $needle, array $haystack, bool $strict = false): bool`.
pub fn in_array<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    needle: &Value,
    haystack: &HashTable,
    strict: bool,
) -> RtResult<bool> {
    Ok(search(ctx, needle, haystack, strict)?.is_some())
}

/// `array_search(mixed $needle, array $haystack, bool $strict = false): int|string|false`.
pub fn array_search<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    needle: &Value,
    haystack: &HashTable,
    strict: bool,
) -> RtResult<Value> {
    Ok(search(ctx, needle, haystack, strict)?.map_or(Value::Bool(false), |k| k.to_value()))
}

/// `array_keys(array $array, mixed $filter_value = UNKNOWN, bool $strict = false): array`.
pub fn array_keys<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    array: &HashTable,
    filter: Option<&Value>,
    strict: bool,
) -> RtResult<ZArray> {
    if array.is_empty() {
        return Ok(ZArray::new());
    }
    let mut out = HashTable::with_capacity(if filter.is_some() { 0 } else { array.len() });
    for (k, v) in array {
        let keep = match filter {
            None => true,
            Some(f) if strict => ops::is_identical(f, &deref(v))?,
            Some(f) => ops::is_equal(f, v, ctx)?,
        };
        if keep {
            let _ = out.push(k.to_value());
        }
    }
    Ok(ZArray::from_table(out))
}

/// `array_values(array $array): array`.
#[must_use]
pub fn array_values(array: &ZArray) -> ZArray {
    if array.is_empty() {
        return ZArray::new();
    }
    let len = i64::try_from(array.len()).unwrap_or(i64::MAX);
    if array.is_packed() && array.is_without_holes() && array.next_free_element() == len {
        return array.clone();
    }
    ZArray::from_table(array.to_list())
}

/// `array_count_values(array $array): array`.
pub fn array_count_values<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    array: &HashTable,
) -> RtResult<ZArray> {
    let mut out = HashTable::new();
    for v in array.values() {
        let key = match deref(v) {
            Value::Int(i) => ArrayKey::Int(i),
            Value::Str(s) => ArrayKey::from_zstr(&s),
            _ => {
                warn(
                    ctx,
                    "array_count_values",
                    "Can only count string and integer values, entry skipped",
                )?;
                continue;
            }
        };
        let slot = out.lookup(key);
        *slot = match slot {
            Value::Int(n) => Value::Int(*n + 1),
            _ => Value::Int(1),
        };
    }
    Ok(ZArray::from_table(out))
}

/// `array_flip(array $array): array`.
pub fn array_flip<C: RuntimeContext + ?Sized>(ctx: &mut C, array: &HashTable) -> RtResult<ZArray> {
    let mut out = HashTable::with_capacity(array.len());
    for (k, v) in array {
        let key = match deref(v) {
            Value::Int(i) => ArrayKey::Int(i),
            Value::Str(s) => ArrayKey::from_zstr(&s),
            _ => {
                warn(
                    ctx,
                    "array_flip",
                    "Can only flip string and integer values, entry skipped",
                )?;
                continue;
            }
        };
        out.insert(key, k.to_value());
    }
    Ok(ZArray::from_table(out))
}

/// `array_change_key_case(array $array, int $case = CASE_LOWER): array`.
pub fn array_change_key_case(array: &HashTable, case: i64) -> RtResult<ZArray> {
    if case != CASE_LOWER && case != CASE_UPPER {
        return Err(arg_value_error(
            "array_change_key_case",
            2,
            Some("case"),
            "must be either CASE_LOWER or CASE_UPPER",
        ));
    }
    let mut out = HashTable::with_capacity(array.len());
    for (k, v) in array {
        match k {
            ArrayKey::Int(i) => {
                let _ = out.add(ArrayKey::Int(*i), copy_elem(v));
            }
            ArrayKey::Str(s) => {
                let nk = if case == CASE_UPPER {
                    s.to_ascii_uppercase()
                } else {
                    s.to_ascii_lowercase()
                };
                out.insert(ArrayKey::Str(nk), copy_elem(v));
            }
        }
    }
    Ok(ZArray::from_table(out))
}

/// `array_key_first(array $array): int|string|null`.
#[must_use]
pub fn array_key_first(array: &HashTable) -> Value {
    array.first().map_or(Value::Null, |(k, _)| k.to_value())
}

/// `array_key_last(array $array): int|string|null`.
#[must_use]
pub fn array_key_last(array: &HashTable) -> Value {
    array.last().map_or(Value::Null, |(k, _)| k.to_value())
}

/// `array_first(array $array): mixed` (PHP 8.5).
#[must_use]
pub fn array_first(array: &HashTable) -> Value {
    array.first().map_or(Value::Null, |(_, v)| deref(v))
}

/// `array_last(array $array): mixed` (PHP 8.5).
#[must_use]
pub fn array_last(array: &HashTable) -> Value {
    array.last().map_or(Value::Null, |(_, v)| deref(v))
}

/// `array_is_list(array $array): bool`.
#[must_use]
pub fn array_is_list(array: &HashTable) -> bool {
    array.is_list()
}

/// `array_key_exists($key, array $array): bool` (and its alias
/// `key_exists`, whose name is passed as `func`).
pub fn array_key_exists<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    func: &str,
    key: &Value,
    array: &HashTable,
) -> RtResult<bool> {
    Ok(match key {
        Value::Str(s) => array.get_sym(s.as_bytes()).is_some(),
        Value::Int(i) => array.get_int(*i).is_some(),
        Value::Null => {
            ctx.diagnostic(
                php_runtime::ErrorLevel::Deprecated,
                "Using null as the key parameter for array_key_exists() is deprecated, use an empty string instead",
            )?;
            array.get_str(b"").is_some()
        }
        Value::Float(d) => {
            let i = php_runtime::convert::dval_to_lval_safe(*d, ctx)?;
            array.get_int(i).is_some()
        }
        Value::Bool(b) => array.get_int(i64::from(*b)).is_some(),
        Value::Resource(r) => {
            ctx.diagnostic(
                php_runtime::ErrorLevel::Warning,
                &format!(
                    "Resource ID#{} used as offset, casting to integer ({})",
                    r.handle(),
                    r.handle()
                ),
            )?;
            array.get_int(r.handle()).is_some()
        }
        Value::Reference(r) => return array_key_exists(ctx, func, &r.get(), array),
        _ => {
            return Err(arg_type_error(
                func,
                1,
                Some("key"),
                "must be a valid array offset type",
            ))
        }
    })
}

// ---------------------------------------------------------------------------
// Construction
// ---------------------------------------------------------------------------

/// `array_fill(int $start_index, int $count, mixed $value): array`.
pub fn array_fill(start: i64, num: i64, value: &Value) -> RtResult<ZArray> {
    match num {
        0 => Ok(ZArray::new()),
        n if n < 0 => Err(arg_value_error(
            "array_fill",
            2,
            Some("count"),
            "must be greater than or equal to 0",
        )),
        n if n > i64::from(i32::MAX) => Err(arg_value_error(
            "array_fill",
            2,
            Some("count"),
            "is too large",
        )),
        n => {
            if start > i64::MAX - n + 1 {
                return Err(cannot_add_element());
            }
            let mut out = HashTable::with_capacity(usize::try_from(n).unwrap_or(0));
            out.insert(start, value.clone());
            for _ in 1..n {
                out.push(value.clone()).map_err(|_| cannot_add_element())?;
            }
            Ok(ZArray::from_table(out))
        }
    }
}

/// `array_fill_keys(array $keys, mixed $value): array`.
pub fn array_fill_keys<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    keys: &HashTable,
    value: &Value,
) -> RtResult<ZArray> {
    let mut out = HashTable::with_capacity(keys.len());
    for k in keys.values() {
        let key = match deref(k) {
            Value::Int(i) => ArrayKey::Int(i),
            other => ArrayKey::from_zstr(&other.to_zstr(ctx)?),
        };
        let _ = out.add(key, value.clone());
    }
    Ok(ZArray::from_table(out))
}

/// `array_pad(array $array, int $length, mixed $value): array`.
pub fn array_pad(array: &ZArray, length: i64, value: &Value) -> RtResult<ZArray> {
    let max = i64::from(HT_MAX_SIZE);
    if !(-max..=max).contains(&length) {
        return Err(arg_value_error(
            "array_pad",
            2,
            Some("length"),
            "must not exceed the maximum allowed array size",
        ));
    }
    let input_size = i64::try_from(array.len()).unwrap_or(i64::MAX);
    let abs = length.abs();
    if input_size >= abs {
        return Ok(array.clone());
    }
    let pads = abs - input_size;
    let mut out = HashTable::with_capacity(usize::try_from(abs).unwrap_or(0));
    let pad = |out: &mut HashTable| {
        for _ in 0..pads {
            let _ = out.push(value.clone());
        }
    };
    if length < 0 {
        pad(&mut out);
    }
    let packed = array.is_packed();
    for (k, v) in array.iter() {
        match k {
            ArrayKey::Str(s) if !packed => {
                let _ = out.add(ArrayKey::Str(s.clone()), v.clone());
            }
            _ => {
                let _ = out.push(v.clone());
            }
        }
    }
    if length > 0 {
        pad(&mut out);
    }
    Ok(ZArray::from_table(out))
}

/// `array_reverse(array $array, bool $preserve_keys = false): array`.
#[must_use]
pub fn array_reverse(array: &HashTable, preserve_keys: bool) -> ZArray {
    let mut out = HashTable::with_capacity(array.len());
    let packed = array.is_packed();
    for (k, v) in array.iter().rev() {
        match k {
            ArrayKey::Str(s) if !packed => {
                let _ = out.add(ArrayKey::Str(s.clone()), copy_elem(v));
            }
            ArrayKey::Int(i) if preserve_keys => {
                let _ = out.add(ArrayKey::Int(*i), copy_elem(v));
            }
            _ => {
                let _ = out.push(copy_elem(v));
            }
        }
    }
    ZArray::from_table(out)
}

/// `array_slice(array $array, int $offset, ?int $length = null, bool $preserve_keys = false): array`.
#[must_use]
pub fn array_slice(
    array: &HashTable,
    offset: i64,
    length: Option<i64>,
    preserve_keys: bool,
) -> ZArray {
    let num_in = i64::try_from(array.len()).unwrap_or(i64::MAX);
    let mut length = length.unwrap_or(num_in);
    let mut offset = offset;
    if offset > num_in {
        return ZArray::new();
    } else if offset < 0 {
        offset += num_in;
        if offset < 0 {
            offset = 0;
        }
    }
    if length < 0 {
        length += num_in - offset;
    } else if offset.unsigned_abs().saturating_add(length.unsigned_abs()) > num_in.unsigned_abs() {
        length = num_in - offset;
    }
    if length <= 0 {
        return ZArray::new();
    }
    let skip = usize::try_from(offset).unwrap_or(0);
    let take = usize::try_from(length).unwrap_or(usize::MAX);
    let mut out = HashTable::with_capacity(take);
    let packed = array.is_packed();
    let list = packed && (!preserve_keys || (offset == 0 && array.is_without_holes()));
    for (k, v) in array.iter().skip(skip).take(take) {
        match k {
            _ if list => {
                let _ = out.push(copy_elem(v));
            }
            ArrayKey::Str(s) => {
                let _ = out.add(ArrayKey::Str(s.clone()), copy_elem(v));
            }
            ArrayKey::Int(i) if preserve_keys => {
                let _ = out.add(ArrayKey::Int(*i), copy_elem(v));
            }
            ArrayKey::Int(_) => {
                let _ = out.push(copy_elem(v));
            }
        }
    }
    ZArray::from_table(out)
}

/// `array_chunk(array $array, int $length, bool $preserve_keys = false): array`.
pub fn array_chunk(array: &HashTable, length: i64, preserve_keys: bool) -> RtResult<ZArray> {
    if length < 1 {
        return Err(arg_value_error(
            "array_chunk",
            2,
            Some("length"),
            "must be greater than 0",
        ));
    }
    if array.is_empty() {
        return Ok(ZArray::new());
    }
    let size = usize::try_from(length)
        .unwrap_or(usize::MAX)
        .min(array.len());
    let mut out = HashTable::with_capacity(array.len().div_ceil(size));
    let mut chunk = HashTable::with_capacity(size);
    for (k, v) in array {
        let v = copy_elem(v);
        if preserve_keys {
            let _ = chunk.add(k.clone(), v);
        } else {
            let _ = chunk.push(v);
        }
        if chunk.len() == size {
            let full = std::mem::replace(&mut chunk, HashTable::with_capacity(size));
            let _ = out.push(Value::Array(ZArray::from_table(full)));
        }
    }
    if !chunk.is_empty() {
        let _ = out.push(Value::Array(ZArray::from_table(chunk)));
    }
    Ok(ZArray::from_table(out))
}

/// `array_combine(array $keys, array $values): array`.
pub fn array_combine<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    keys: &HashTable,
    values: &HashTable,
) -> RtResult<ZArray> {
    if keys.len() != values.len() {
        return Err(arg_value_error(
            "array_combine",
            1,
            Some("keys"),
            "and argument #2 ($values) must have the same number of elements",
        ));
    }
    if keys.is_empty() {
        return Ok(ZArray::new());
    }
    let mut out = HashTable::with_capacity(keys.len());
    for (k, v) in keys.values().zip(values.values()) {
        let key = match k {
            Value::Int(i) => ArrayKey::Int(*i),
            other => ArrayKey::from_zstr(&other.to_zstr(ctx)?),
        };
        out.insert(key, copy_elem(v));
    }
    Ok(ZArray::from_table(out))
}

/// The `$column_key` / `$index_key` argument of `array_column`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnKey {
    /// An integer key.
    Int(i64),
    /// A string key (kept verbatim for object property names; numeric
    /// strings address integer keys of arrays).
    Str(ZStr),
}

impl ColumnKey {
    /// Converts an `int|string` argument value.
    #[must_use]
    pub fn from_value(v: &Value) -> Option<Self> {
        match v {
            Value::Int(i) => Some(ColumnKey::Int(*i)),
            Value::Str(s) => Some(ColumnKey::Str(s.clone())),
            Value::Reference(r) => Self::from_value(&r.get()),
            _ => None,
        }
    }

    fn array_key(&self) -> ArrayKey {
        match self {
            ColumnKey::Int(i) => ArrayKey::Int(*i),
            ColumnKey::Str(s) => ArrayKey::from_zstr(s),
        }
    }

    fn prop_name(&self) -> ZStr {
        match self {
            ColumnKey::Int(i) => ZStr::from(i.to_string()),
            ColumnKey::Str(s) => s.clone(),
        }
    }
}

/// `array_column_fetch_prop`.
fn column_fetch<C: ArrayContext + ?Sized>(
    ctx: &mut C,
    data: &Value,
    key: &ColumnKey,
) -> RtResult<Option<Value>> {
    match data {
        Value::Object(o) => Ok(ctx.column_property(o, &key.prop_name())?.map(|v| deref(&v))),
        Value::Array(a) => Ok(a.get(&key.array_key()).map(deref)),
        _ => Ok(None),
    }
}

/// `array_column(array $array, int|string|null $column_key, int|string|null $index_key = null): array`.
pub fn array_column<C: ArrayContext + ?Sized>(
    ctx: &mut C,
    input: &HashTable,
    column: Option<&ColumnKey>,
    index: Option<&ColumnKey>,
) -> RtResult<ZArray> {
    if input.is_empty() {
        return Ok(ZArray::new());
    }
    let mut out = HashTable::with_capacity(input.len());
    for data in input.values() {
        let data = deref(data);
        let colval = match column {
            None => data.clone(),
            Some(c) => match column_fetch(ctx, &data, c)? {
                Some(v) => v,
                None => continue,
            },
        };
        let keyval = match index {
            None => None,
            Some(i) => column_fetch(ctx, &data, i)?,
        };
        match keyval {
            Some(k) => {
                let Some(key) = k.to_array_key(ctx)? else {
                    return Err(RuntimeError::TypeError(format!(
                        "Cannot access offset of type {} on array",
                        k.type_name()
                    )));
                };
                out.insert(key, colval);
            }
            None => {
                out.push(colval).map_err(|_| cannot_add_element())?;
            }
        }
    }
    Ok(ZArray::from_table(out))
}

// ---------------------------------------------------------------------------
// array_sum / array_product
// ---------------------------------------------------------------------------

fn binop<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    func: &str,
    op_name: &str,
    input: &HashTable,
    initial: i64,
    op: fn(&Value, &Value, &mut C) -> RtResult<Value>,
) -> RtResult<Value> {
    let mut acc = Value::Int(initial);
    for entry in input.values() {
        if let Value::Object(o) = entry {
            match ctx.cast_object(o, CastTarget::Number)? {
                Some(dst @ (Value::Int(_) | Value::Float(_))) => {
                    acc = op(&acc, &dst, ctx)?;
                }
                _ => warn(
                    ctx,
                    func,
                    &format!("{op_name} is not supported on type {}", entry.type_name()),
                )?,
            }
            continue;
        }
        match op(&acc, entry, ctx) {
            Ok(v) => acc = v,
            Err(_) => {
                match entry {
                    Value::Resource(r) => acc = op(&acc, &Value::Int(r.handle()), ctx)?,
                    Value::Str(_) => acc = op(&acc, &Value::Int(0), ctx)?,
                    _ => {}
                }
                warn(
                    ctx,
                    func,
                    &format!("{op_name} is not supported on type {}", entry.type_name()),
                )?;
            }
        }
    }
    Ok(acc)
}

fn add_op<C: RuntimeContext + ?Sized>(a: &Value, b: &Value, ctx: &mut C) -> RtResult<Value> {
    ops::add(a, b, ctx)
}

fn mul_op<C: RuntimeContext + ?Sized>(a: &Value, b: &Value, ctx: &mut C) -> RtResult<Value> {
    ops::mul(a, b, ctx)
}

/// `array_sum(array $array): int|float`.
pub fn array_sum<C: RuntimeContext + ?Sized>(ctx: &mut C, input: &HashTable) -> RtResult<Value> {
    binop(ctx, "array_sum", "Addition", input, 0, add_op::<C>)
}

/// `array_product(array $array): int|float`.
pub fn array_product<C: RuntimeContext + ?Sized>(
    ctx: &mut C,
    input: &HashTable,
) -> RtResult<Value> {
    binop(
        ctx,
        "array_product",
        "Multiplication",
        input,
        1,
        mul_op::<C>,
    )
}

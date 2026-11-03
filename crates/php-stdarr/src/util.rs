//! Small helpers shared by the array functions.

use php_runtime::{
    ErrorLevel, HashTable, PhpRef, RtResult, RuntimeContext, RuntimeError, Value, ZArray,
};

/// The value to store when copying an element into another array
/// (`zval_add_ref` / the `Z_REFCOUNT == 1` unwrapping done by most array
/// functions): references held only by the source array are unwrapped,
/// shared references stay shared.
#[must_use]
pub fn copy_elem(v: &Value) -> Value {
    match v {
        Value::Reference(r) if r.refcount() == 1 => r.get(),
        other => other.clone(),
    }
}

/// `ZVAL_DEREF` + copy.
#[must_use]
pub fn deref(v: &Value) -> Value {
    match v {
        Value::Reference(r) => r.get(),
        other => other.clone(),
    }
}

/// Emits an `E_WARNING` with the `func(): ` prefix (`php_error_docref`).
pub fn warn<C: RuntimeContext + ?Sized>(ctx: &mut C, func: &str, msg: &str) -> RtResult<()> {
    ctx.diagnostic(ErrorLevel::Warning, &format!("{func}(): {msg}"))
}

/// Emits an `E_DEPRECATED` with the `func(): ` prefix.
pub fn deprecated<C: RuntimeContext + ?Sized>(ctx: &mut C, func: &str, msg: &str) -> RtResult<()> {
    ctx.diagnostic(ErrorLevel::Deprecated, &format!("{func}(): {msg}"))
}

/// Formats the `Argument #n ($name)` part of argument errors
/// (`zend_argument_error_variadic`); `name` is `None` for variadic
/// arguments, which PHP prints without a name.
#[must_use]
pub fn arg_label(n: usize, name: Option<&str>) -> String {
    match name {
        Some(name) => format!("Argument #{n} (${name})"),
        None => format!("Argument #{n}"),
    }
}

/// `zend_argument_value_error`: a `ValueError` "`func(): Argument #n ($name) msg`".
#[must_use]
pub fn arg_value_error(func: &str, n: usize, name: Option<&str>, msg: &str) -> RuntimeError {
    RuntimeError::ValueError(format!("{func}(): {} {msg}", arg_label(n, name)))
}

/// `zend_argument_type_error`: a `TypeError` "`func(): Argument #n ($name) msg`".
#[must_use]
pub fn arg_type_error(func: &str, n: usize, name: Option<&str>, msg: &str) -> RuntimeError {
    RuntimeError::TypeError(format!("{func}(): {} {msg}", arg_label(n, name)))
}

/// The "must be of type array, X given" `TypeError` for a non-array argument.
#[must_use]
pub fn not_array_error(func: &str, n: usize, name: Option<&str>, given: &Value) -> RuntimeError {
    arg_type_error(
        func,
        n,
        name,
        &format!("must be of type array, {} given", given.value_name()),
    )
}

/// "Cannot add element to the array as the next element is already occupied".
#[must_use]
pub fn cannot_add_element() -> RuntimeError {
    RuntimeError::Error(
        "Cannot add element to the array as the next element is already occupied".to_string(),
    )
}

/// Takes the array held by a by-reference parameter out of its reference,
/// leaving `null` behind until [`put_array`] restores it. This avoids a
/// copy-on-write separation while the function works on the table.
pub fn take_array(r: &PhpRef, func: &str, n: usize, name: &str) -> RtResult<ZArray> {
    let v = r.set(Value::Null);
    match v {
        Value::Array(a) => Ok(a),
        other => {
            let err = not_array_error(func, n, Some(name), &other);
            r.set(other);
            Err(err)
        }
    }
}

/// Stores an array back into a by-reference parameter.
pub fn put_array(r: &PhpRef, a: ZArray) {
    r.set(Value::Array(a));
}

/// Runs `f` on the array held by `r` (see [`take_array`]), always storing it
/// back, even when `f` fails (PHP's array functions modify in place, so a
/// partially processed array stays visible after an exception).
pub fn with_array<R>(
    r: &PhpRef,
    func: &str,
    n: usize,
    name: &str,
    f: impl FnOnce(&mut ZArray) -> RtResult<R>,
) -> RtResult<R> {
    let mut a = take_array(r, func, n, name)?;
    let res = f(&mut a);
    put_array(r, a);
    res
}

/// Runs `f` on the array held by a by-reference parameter while the
/// variable stays bound to it, like `zend_array_sort` (which adds a
/// reference to the table for the duration of the sort): user code running
/// meanwhile (comparison callbacks, `__toString`, user random engines) sees
/// the variable unchanged, and a self-referencing element still reaches the
/// array. `f` works on a separated copy of the table; the result replaces
/// the variable's array only if the variable still holds the original one —
/// when user code wrote to the variable, the write separated it and PHP
/// discards the sorted table (Zend/tests/gh16648.phpt).
pub fn with_bound_array<R>(
    r: &PhpRef,
    func: &str,
    n: usize,
    name: &str,
    f: impl FnOnce(&mut HashTable) -> RtResult<R>,
) -> RtResult<R> {
    let orig = match r.get() {
        Value::Array(a) => a,
        other => return Err(not_array_error(func, n, Some(name), &other)),
    };
    let mut work: HashTable = (*orig).clone();
    let res = f(&mut work);
    let unchanged = r
        .try_borrow()
        .is_some_and(|v| matches!(&*v, Value::Array(a) if ZArray::ptr_eq(a, &orig)));
    drop(orig);
    if unchanged {
        r.set(Value::Array(ZArray::from_table(work)));
    }
    res
}

/// `ZEND_THREEWAY_COMPARE` on doubles (NaN compares as greater).
#[must_use]
#[allow(clippy::float_cmp)]
pub fn threeway_f(a: f64, b: f64) -> i32 {
    if a == b {
        0
    } else if a < b {
        -1
    } else {
        1
    }
}

/// `ZEND_NORMALIZE_BOOL` on an integer.
#[must_use]
pub fn normalize(n: i64) -> i32 {
    i32::from(n > 0) - i32::from(n < 0)
}

/// Collects the first error raised inside a comparator that cannot itself
/// fail (the sorting primitives take `i32` comparators). Once an error is
/// recorded, further comparisons are short-circuited to `0`, mirroring
/// PHP, where a pending exception prevents further user calls.
#[derive(Debug, Default)]
pub struct ErrSlot(pub Option<RuntimeError>);

impl ErrSlot {
    /// Runs a fallible comparison.
    pub fn run(&mut self, f: impl FnOnce() -> RtResult<i32>) -> i32 {
        if self.0.is_some() {
            return 0;
        }
        match f() {
            Ok(v) => v,
            Err(e) => {
                self.0 = Some(e);
                0
            }
        }
    }

    /// Whether an error was recorded.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.0.is_some()
    }

    /// Converts into a result.
    pub fn into_result(self) -> RtResult<()> {
        match self.0 {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// Identity-tracking stack used for recursion protection
/// (`GC_PROTECT_RECURSION`).
#[derive(Debug, Default)]
pub struct Guard(Vec<usize>);

impl Guard {
    /// Whether `addr` is currently protected.
    #[must_use]
    pub fn contains(&self, addr: usize) -> bool {
        self.0.contains(&addr)
    }

    /// Protects `addr`.
    pub fn push(&mut self, addr: usize) {
        self.0.push(addr);
    }

    /// Unprotects `addr`.
    pub fn pop(&mut self, addr: usize) {
        if let Some(p) = self.0.iter().rposition(|&a| a == addr) {
            self.0.remove(p);
        }
    }
}

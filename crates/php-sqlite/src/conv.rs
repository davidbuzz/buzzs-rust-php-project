//! Conversions between `SQLite` values and PHP values shared by both
//! extensions.

use crate::ffi::{ArgValue, FnResult, RawStatement, SQLITE_FLOAT, SQLITE_INTEGER, SQLITE_NULL};
use crate::host::Cx;
use php_runtime::{Value, ZStr};

/// Converts a user-defined function argument (`sqlite3_do_callback` /
/// `do_callback` argument marshalling).
pub(crate) fn arg_to_value(arg: ArgValue) -> Value {
    match arg {
        ArgValue::Int(i) => Value::Int(i),
        ArgValue::Float(f) => Value::Float(f),
        ArgValue::Null => Value::Null,
        ArgValue::Text(t) => Value::Str(ZStr::from(t)),
    }
}

/// Converts a callback's return value into the SQL result: `int`, `null`
/// and `float` map directly, everything else is converted to a string
/// (`zval_try_get_tmp_string`); a failed conversion leaves the result
/// unset with the exception pending.
pub(crate) fn value_to_result(cx: &mut Cx<'_>, v: &Value) -> FnResult {
    match v.deref() {
        Value::Int(i) => FnResult::Int(i),
        Value::Null => FnResult::Null,
        Value::Float(f) => FnResult::Float(f),
        other => {
            let r = other.to_zstr(cx.host());
            match cx.check(r) {
                Some(s) => FnResult::Text(s.as_bytes().to_vec()),
                None => FnResult::Unset,
            }
        }
    }
}

/// The result set when a callback could not be invoked.
pub(crate) fn failed_to_invoke() -> FnResult {
    FnResult::Error(b"failed to invoke callback".to_vec(), 0)
}

/// Nullable string argument (`ZVAL_NULLABLE_STRING`).
pub(crate) fn nullable_str(s: Option<&[u8]>) -> Value {
    s.map_or(Value::Null, |b| Value::Str(ZStr::from(b)))
}

/// The outcome of reading one column the `ext/sqlite3` way
/// (`sqlite_value_to_zval`).
pub(crate) enum ColumnValue {
    /// The value.
    Value(Value),
    /// `SQLITE_NOMEM` while reading: the caller reports "Failed to retrieve
    /// column value due to out of memory" and uses `NULL`.
    OutOfMemory,
}

/// `sqlite_value_to_zval()`.
pub(crate) fn sqlite3_column_value(stmt: &RawStatement, col: i32) -> ColumnValue {
    match stmt.column_type(col) {
        SQLITE_INTEGER => ColumnValue::Value(Value::Int(stmt.column_int64(col))),
        SQLITE_FLOAT => ColumnValue::Value(Value::Float(stmt.column_double(col))),
        SQLITE_NULL => ColumnValue::Value(Value::Null),
        crate::ffi::SQLITE_TEXT => match stmt.column_text(col) {
            Some(t) => ColumnValue::Value(Value::Str(ZStr::from(t))),
            None => ColumnValue::OutOfMemory,
        },
        _ => match stmt.column_blob(col) {
            Some(b) => ColumnValue::Value(Value::Str(ZStr::from(b))),
            None if stmt.db_errcode() == crate::ffi::SQLITE_NOMEM => ColumnValue::OutOfMemory,
            None => ColumnValue::Value(Value::Str(ZStr::empty())),
        },
    }
}

/// Sets a (possibly by-reference) parameter slot, like converting the zval
/// in place does in C.
pub(crate) fn set_slot(slot: &mut Value, new: Value) {
    match slot {
        Value::Reference(r) => {
            r.set(new);
        }
        other => *other = new,
    }
}

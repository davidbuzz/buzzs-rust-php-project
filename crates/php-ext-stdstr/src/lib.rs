//! `ext/standard` string, HTML, URL, math, formatted-print, pack and scanf
//! functions for the phprust VM.
//!
//! The byte-level algorithms live in `php-stdstr` (and the printf /
//! `number_format` / `round` engines in `php-support`); this crate only
//! translates between PHP values and those libraries: the array forms of
//! `str_replace()`/`substr_replace()`/`strtr()`, by-reference outputs
//! (`&$count`, `&$percent`, `sscanf()` variables), INI lookups
//! (`default_charset`) and the conversion of library errors/notices into
//! thrown exceptions and emitted diagnostics.

mod formatted;
mod html;
mod math;
mod misc;
mod strings;
mod url;

use php_runtime::{ArrayKey, HashTable, Value, ZArray, ZStr};
use php_stdstr::error::{Level, Notices, StdError, StdResult};
use php_vm::ext::{Extension, ModuleBuilder, Throw, VmResult};
use php_vm::Vm;

/// The string/math/url part of `ext/standard`.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdStr;

impl Extension for StdStr {
    fn name(&self) -> &'static str {
        "standard"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        strings::register(m);
        html::register(m);
        url::register(m);
        math::register(m);
        misc::register(m);
        formatted::register(m);
    }
}

/// `E_WARNING`.
pub(crate) const E_WARNING: i64 = 2;
/// `E_NOTICE`.
pub(crate) const E_NOTICE: i64 = 8;
/// `E_DEPRECATED`.
pub(crate) const E_DEPRECATED: i64 = 8192;

/// Emits the diagnostics a `php-stdstr` call collected, in order.
///
/// Diagnostics attributed to a function are emitted like
/// `php_error_docref()` (prefixed with the *running* function's name, so
/// aliases such as `chop` report their own name); the others like
/// `zend_error()`.
pub(crate) fn emit(vm: &mut Vm, notices: Notices) -> VmResult<()> {
    for n in notices {
        match (n.func, n.level) {
            (Some(_), Level::Warning) => vm.php_warning(&n.message)?,
            (Some(_), Level::Notice) => vm.php_notice(&n.message)?,
            (Some(_), Level::Deprecated) => vm.php_deprecated(&n.message)?,
            (None, Level::Warning) => vm.raise(E_WARNING, &n.message)?,
            (None, Level::Notice) => vm.raise(E_NOTICE, &n.message)?,
            (None, Level::Deprecated) => vm.raise(E_DEPRECATED, &n.message)?,
        }
    }
    Ok(())
}

/// Converts a library error into a thrown exception.
pub(crate) fn throw(vm: &mut Vm, e: &StdError) -> Throw {
    vm.throw(e.kind.class_name(), &e.to_string())
}

/// Unwraps a library result, throwing on error.
pub(crate) fn check<T>(vm: &mut Vm, r: StdResult<T>) -> VmResult<T> {
    r.map_err(|e| throw(vm, &e))
}

/// Emits the `#[\Deprecated]` notice of a deprecated internal function.
pub(crate) fn deprecated_function(
    vm: &mut Vm,
    name: &str,
    since: &str,
    message: &str,
) -> VmResult<()> {
    vm.raise(
        E_DEPRECATED,
        &format!("Function {name}() is deprecated since {since}, {message}"),
    )
}

/// A PHP string value from bytes.
pub(crate) fn bytes(v: impl Into<ZStr>) -> Value {
    Value::Str(v.into())
}

/// `string|false`.
pub(crate) fn opt_bytes(v: Option<Vec<u8>>) -> Value {
    v.map_or(Value::Bool(false), bytes)
}

/// A packed list of strings.
pub(crate) fn str_list(items: Vec<Vec<u8>>) -> Value {
    Value::Array(ZArray::from_values(items.into_iter().map(bytes)))
}

/// An array from ordered key/value pairs.
pub(crate) fn assoc<K: Into<ArrayKey>>(pairs: impl IntoIterator<Item = (K, Value)>) -> Value {
    let mut t = HashTable::new();
    for (k, v) in pairs {
        t.insert(k, v);
    }
    Value::Array(ZArray::from_table(t))
}

/// The type name PHP uses in "must be of type X, Y given" messages
/// (`zend_zval_value_name`).
pub(crate) fn type_name(v: &Value) -> String {
    v.value_name()
}

/// Converts a value with `zval_get_string()` semantics; plain strings and
/// integers avoid the VM round trip.
pub(crate) fn to_str(vm: &mut Vm, v: &Value) -> VmResult<ZStr> {
    match v {
        Value::Str(s) => Ok(s.clone()),
        Value::Reference(r) => to_str(vm, &r.get()),
        other => vm.value_to_string(other),
    }
}

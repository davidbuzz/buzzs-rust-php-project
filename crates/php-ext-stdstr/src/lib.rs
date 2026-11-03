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

// Every handler has the fixed `NativeFn` signature, so infallible ones still
// return `VmResult`, and the short names (`a` for the arguments, `s`, `n`,
// `f`, `l`, `r` …) follow php-src's own variable names.
#![allow(clippy::unnecessary_wraps, clippy::many_single_char_names)]

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

pub use formatted::format as formatted_print;

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

/// Approximate Zend MM accounting for the allocations this crate makes on
/// behalf of PHP code, so that `memory_limit` is enforced for the large
/// results string functions can build (the VM has no allocator-level
/// limit).
pub(crate) mod mem {
    use php_vm::ext::VmResult;
    use php_vm::Vm;

    /// Size of a hash bucket holding one zval (`Bucket`).
    pub(crate) const BUCKET: usize = 32;

    /// Allocation size of a `zend_string` of `len` bytes.
    pub(crate) const fn zstr(len: usize) -> usize {
        (24 + len + 1 + 7) & !7
    }

    /// Bytes held by the elements of a list of strings.
    pub(crate) fn str_list<T: AsRef<[u8]>>(items: &[T]) -> usize {
        items.iter().fold(0usize, |acc, s| {
            acc.saturating_add(BUCKET + zstr(s.as_ref().len()))
        })
    }

    /// The effective `memory_limit` in bytes (`None` for unlimited).
    fn limit(vm: &Vm) -> Option<usize> {
        let raw = vm
            .ini_string("memory_limit")
            .unwrap_or_else(|| b"128M".to_vec());
        let (v, _) = php_support::ini::parse_quantity(&raw);
        usize::try_from(v).ok().filter(|&v| v > 0)
    }

    /// Raises PHP's out-of-memory fatal error when `live` bytes (the
    /// inputs still referenced) plus a new allocation of `alloc` bytes
    /// exceed `memory_limit`.
    pub(crate) fn check(vm: &mut Vm, live: usize, alloc: usize) -> VmResult<()> {
        let Some(limit) = limit(vm) else {
            return Ok(());
        };
        if live.saturating_add(alloc) > limit {
            return Err(vm.fatal(&format!(
                "Allowed memory size of {limit} bytes exhausted (tried to allocate {alloc} bytes)"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::mem;

    #[test]
    fn zend_string_sizes() {
        assert_eq!(mem::zstr(0), 32);
        assert_eq!(mem::zstr(7), 32);
        assert_eq!(mem::zstr(8), 40);
        assert_eq!(
            mem::str_list(&[b"a".as_slice(), b"bc".as_slice()]),
            2 * (mem::BUCKET + 32)
        );
    }
}

//! PHP's array functions (`ext/standard/array.c`) on top of `php-runtime`.
//!
//! Every function of `array.c` is ported with PHP's exact semantics: key
//! preservation and renumbering rules, reference preservation (references
//! with a refcount of 1 are unwrapped when copied, shared references stay
//! shared), result order, flags, and the exact warning / `ValueError` /
//! `TypeError` texts.
//!
//! # Calling convention
//!
//! The VM performs argument parsing (`zpp`) and passes typed Rust values:
//!
//! * the context is any `C: ArrayContext + ?Sized` ([`ArrayContext`] extends
//!   [`php_runtime::RuntimeContext`] with the few object hooks array
//!   functions need). Diagnostics go through it; thrown errors are returned
//!   as `Err(RuntimeError)`;
//! * by-value array parameters are `&ZArray`; by-reference parameters
//!   (`array &$array`) are the [`PhpRef`](php_runtime::PhpRef) the VM binds
//!   to the argument;
//! * variadic parameters whose type PHP checks inside the function body
//!   (`array_merge(...$arrays)`, the `array_diff` family, `array_map`,
//!   `min`/`max`, `array_multisort`) take `&[Value]`, so the exact
//!   "must be of type array, X given" errors are produced here;
//! * user callbacks are [`Callback`]s: `FnMut(&mut C, &[Value]) -> RtResult<Value>`.
//!   The context is handed back to the callback so the VM can run user code
//!   while this crate holds no borrow of it. Arguments are passed exactly as
//!   PHP passes them (array elements that are references stay
//!   `Value::Reference`, `array_walk` passes a reference to the element);
//!   the VM's call machinery binds them to by-value or by-reference
//!   parameters;
//! * randomness (`shuffle`, `array_rand`, `str_shuffle`) comes from a
//!   [`RangeFn`] closure implementing the engine's `range(min, max)`;
//! * `extract()` / `compact()` access the caller's variables through the
//!   [`SymbolTable`](symbols::SymbolTable) trait.
//!
//! Functions are grouped by module: [`sort`], [`pointer`] (internal
//! pointer), [`basic`], [`stack`] (push/pop/shift/unshift/splice),
//! [`merge`], [`setops`] (unique/intersect/diff), [`callback`]
//! (walk/map/filter/reduce/find), [`symbols`], [`random`], [`range`].
//! The comparison functions in [`compare`] are public for reuse by SPL.

pub mod basic;
pub mod callback;
pub mod compare;
pub mod merge;
pub mod pointer;
pub mod random;
pub mod range;
pub mod setops;
pub mod sort;
pub mod stack;
pub mod symbols;
pub mod util;

use php_runtime::{CollectingContext, Obj, RtResult, RuntimeContext, Value, ZStr};

/// A user callback: receives the context and the arguments, returns the
/// callback's return value or the exception it threw.
pub type Callback<'a, C> = dyn FnMut(&mut C, &[Value]) -> RtResult<Value> + 'a;

/// A random engine's `range(min, max)` (inclusive), e.g.
/// `php_mt_rand_range` for the default engine. Errors model engine failures
/// (a user engine throwing).
pub type RangeFn<'a> = dyn FnMut(i64, i64) -> RtResult<i64> + 'a;

/// The services array functions need beyond [`RuntimeContext`].
///
/// All methods have defaults suitable for a context without objects; the
/// VM overrides them.
pub trait ArrayContext: RuntimeContext {
    /// Whether `obj` is an enum case (`ZEND_ACC_ENUM`), used by the
    /// `SORT_REGULAR` comparator to group enum cases in `array_unique`.
    fn is_enum(&self, obj: &Obj) -> bool {
        let _ = obj;
        false
    }

    /// `count()` on an object: the `count_elements` handler or
    /// `Countable::count()` (converted with `zval_get_long`). `Ok(None)`
    /// means the object is not countable (a `TypeError` follows).
    fn count_object(&mut self, obj: &Obj) -> RtResult<Option<i64>> {
        let _ = obj;
        Ok(None)
    }

    /// `array_column()` property fetch: if `has_property(EXISTS)` or
    /// `has_property(ISSET)` (which may call `__isset`), returns the result
    /// of `read_property` (which may call `__get`), dereferenced; otherwise
    /// `None`.
    fn column_property(&mut self, obj: &Obj, name: &ZStr) -> RtResult<Option<Value>> {
        let _ = (obj, name);
        Ok(None)
    }

    /// `compact('this')` when `$this` is not a variable of the symbol
    /// table: the current `$this` object, if any.
    fn this_object(&mut self) -> Option<Value> {
        None
    }
}

impl ArrayContext for CollectingContext {}

/// `SORT_REGULAR`
pub const SORT_REGULAR: i64 = 0;
/// `SORT_NUMERIC`
pub const SORT_NUMERIC: i64 = 1;
/// `SORT_STRING`
pub const SORT_STRING: i64 = 2;
/// `SORT_DESC`
pub const SORT_DESC: i64 = 3;
/// `SORT_ASC`
pub const SORT_ASC: i64 = 4;
/// `SORT_LOCALE_STRING`
pub const SORT_LOCALE_STRING: i64 = 5;
/// `SORT_NATURAL`
pub const SORT_NATURAL: i64 = 6;
/// `SORT_FLAG_CASE`
pub const SORT_FLAG_CASE: i64 = 8;
/// `COUNT_NORMAL`
pub const COUNT_NORMAL: i64 = 0;
/// `COUNT_RECURSIVE`
pub const COUNT_RECURSIVE: i64 = 1;
/// `EXTR_OVERWRITE`
pub const EXTR_OVERWRITE: i64 = 0;
/// `EXTR_SKIP`
pub const EXTR_SKIP: i64 = 1;
/// `EXTR_PREFIX_SAME`
pub const EXTR_PREFIX_SAME: i64 = 2;
/// `EXTR_PREFIX_ALL`
pub const EXTR_PREFIX_ALL: i64 = 3;
/// `EXTR_PREFIX_INVALID`
pub const EXTR_PREFIX_INVALID: i64 = 4;
/// `EXTR_PREFIX_IF_EXISTS`
pub const EXTR_PREFIX_IF_EXISTS: i64 = 5;
/// `EXTR_IF_EXISTS`
pub const EXTR_IF_EXISTS: i64 = 6;
/// `EXTR_REFS`
pub const EXTR_REFS: i64 = 0x100;
/// `CASE_LOWER`
pub const CASE_LOWER: i64 = 0;
/// `CASE_UPPER`
pub const CASE_UPPER: i64 = 1;
/// `ARRAY_FILTER_USE_VALUE` (not a PHP constant; the default mode).
pub const ARRAY_FILTER_USE_VALUE: i64 = 0;
/// `ARRAY_FILTER_USE_BOTH`
pub const ARRAY_FILTER_USE_BOTH: i64 = 1;
/// `ARRAY_FILTER_USE_KEY`
pub const ARRAY_FILTER_USE_KEY: i64 = 2;
/// `HT_MAX_SIZE` on 64-bit platforms.
pub const HT_MAX_SIZE: u32 = 0x4000_0000;

#[cfg(test)]
mod tests;

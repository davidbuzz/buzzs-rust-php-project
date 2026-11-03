//! The interface to the embedding VM.

use php_runtime::{RtResult, RuntimeContext, Value};

/// A compiled PCRE pattern as used by `ZipArchive::addPattern()`:
/// returns whether a file name matches (`pcre2_match() >= 0`).
pub type RegexMatcher = Box<dyn FnMut(&[u8]) -> bool>;

/// What this crate needs from the PHP VM.
///
/// The supertrait [`RuntimeContext`] provides value conversions
/// (`zval_try_get_long()` for the option arrays and the cancel callback's
/// return value) and their diagnostics.
pub trait ZipHost: RuntimeContext {
    /// Calls a PHP callable (`zend_call_known_fcc()`).
    ///
    /// `Err(RuntimeError::Fatal(_))` models a bailout (`exit()` or a fatal
    /// error inside the callback); any other `Err` is an exception thrown by
    /// the callee — or one already pending, in which case PHP does not call
    /// the function at all.
    fn call(&mut self, callable: &Value, args: &[Value]) -> RtResult<Value>;

    /// `pcre_get_compiled_regex()`: compiles a PHP regular expression
    /// (with delimiters and modifiers) through the VM's PCRE cache,
    /// emitting PCRE's own compile warnings. `None` when it does not
    /// compile.
    fn compile_regex(&mut self, regex: &[u8]) -> Option<RegexMatcher>;
}

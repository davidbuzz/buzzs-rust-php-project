// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! [`FilterHost`]: what the filter extension needs from the VM.

use crate::url_parse::ParsedUrl;
use php_runtime::{RtResult, RuntimeContext, RuntimeError, Value, ZStr};

/// The class of the exceptions thrown with `FILTER_THROW_ON_FAILURE`.
pub const FILTER_FAILED_EXCEPTION_CLASS: &str = "Filter\\FilterFailedException";
/// The parent class of [`FILTER_FAILED_EXCEPTION_CLASS`] (extends `Exception`).
pub const FILTER_EXCEPTION_CLASS: &str = "Filter\\FilterException";

/// The components of a parsed URI as ext/uri's `php_uri` struct exposes them
/// (raw read mode). Absent components are `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UriComponents {
    /// The scheme.
    pub scheme: Option<Vec<u8>>,
    /// The user name.
    pub user: Option<Vec<u8>>,
    /// The password.
    pub password: Option<Vec<u8>>,
    /// The host.
    pub host: Option<Vec<u8>>,
    /// The port.
    pub port: Option<i64>,
    /// The path.
    pub path: Option<Vec<u8>>,
    /// The query.
    pub query: Option<Vec<u8>>,
    /// The fragment.
    pub fragment: Option<Vec<u8>>,
}

impl From<ParsedUrl> for UriComponents {
    fn from(u: ParsedUrl) -> Self {
        UriComponents {
            scheme: u.scheme,
            user: u.user,
            password: u.pass,
            host: u.host,
            // uri_parser_php_parse_url_port_read: port 0 reads as null.
            port: (u.port != 0).then_some(i64::from(u.port)),
            path: u.path,
            query: u.query,
            fragment: u.fragment,
        }
    }
}

/// The result of [`FilterHost::uri_parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UriParseResult {
    /// No URI parser is registered under that name (`php_uri_get_parser`
    /// returned `NULL`): `FILTER_VALIDATE_URL` throws a `ValueError`.
    UnknownParser,
    /// The parser rejected the URI.
    Invalid,
    /// The parsed components.
    Parsed(UriComponents),
}

/// The services the filter extension needs from the VM.
///
/// It extends [`RuntimeContext`], which provides diagnostics (warnings are
/// emitted with their full `"filter_var(): ..."` text) and the
/// `__toString` conversion of objects (`cast_object`). Every other method
/// has a default suitable for a VM without the corresponding feature.
pub trait FilterHost: RuntimeContext {
    /// `FILTER_VALIDATE_REGEXP`: compiles `pattern` through the preg cache
    /// (`pcre_get_compiled_regex`, which emits its own warnings such as
    /// "`filter_var()`: Delimiter must not be alphanumeric, backslash, or
    /// NUL byte") and matches it against `subject`.
    ///
    /// Returns `Ok(None)` when the pattern does not compile, `Ok(Some(true))`
    /// when it matches and `Ok(Some(false))` when it does not (or matching
    /// failed, e.g. on a backtrack limit or bad UTF-8). The default, for a
    /// VM without PCRE, reports a compile failure.
    fn regex_match(&mut self, pattern: &ZStr, subject: &[u8]) -> RtResult<Option<bool>> {
        let _ = (pattern, subject);
        Ok(None)
    }

    /// `zend_is_callable_ex(.., IS_CALLABLE_SUPPRESS_DEPRECATIONS, ..)` for
    /// the `FILTER_CALLBACK` option.
    fn is_callable(&mut self, callable: &Value) -> bool {
        let _ = callable;
        false
    }

    /// Calls a `FILTER_CALLBACK` callable with one argument (the value
    /// converted to string). An undefined return value is `null`.
    fn call_callback(&mut self, callable: &Value, arg: Value) -> RtResult<Value> {
        let _ = (callable, arg);
        Ok(Value::Null)
    }

    /// Builds the `Filter\FilterFailedException` thrown with
    /// `FILTER_THROW_ON_FAILURE` (code 0). The default produces a plain
    /// `Error` carrying the message, for VMs that do not register the class.
    fn filter_failed_exception(&mut self, message: String) -> RuntimeError {
        RuntimeError::Error(message)
    }

    /// `zend_call_stack_overflowed()` in `php_zval_filter_recursive` at a
    /// nesting of `depth` arrays: the error to throw
    /// (`zend_call_stack_size_error()`), if the C stack would be exhausted.
    /// The default never overflows.
    fn stack_overflowed(&mut self, depth: usize) -> Option<RuntimeError> {
        let _ = depth;
        None
    }

    /// Parses `uri` with the ext/uri parser registered as `parser` (for the
    /// `uri_parser_class` option of `FILTER_VALIDATE_URL`; the built-in
    /// `"parse_url"` parser never reaches the host). The default knows no
    /// other parser.
    fn uri_parse(&mut self, parser: &[u8], uri: &[u8]) -> RtResult<UriParseResult> {
        let _ = (parser, uri);
        Ok(UriParseResult::UnknownParser)
    }

    /// `php_escape_html_entities_ex(s, all: 1, quotes ? ENT_QUOTES :
    /// ENT_NOQUOTES, charset: default, double_encode: 0, quiet: 0)` for
    /// `FILTER_SANITIZE_FULL_SPECIAL_CHARS`. The default implements the
    /// UTF-8 `default_charset`; a VM with another `default_charset` can
    /// delegate to its `htmlentities()`.
    fn escape_html_entities(&mut self, s: &[u8], quotes: bool) -> Vec<u8> {
        crate::html::htmlentities_utf8_no_double_encode(s, quotes)
    }
}

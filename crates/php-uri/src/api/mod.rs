//! PHP-level semantics of the `ext/uri` classes.
//!
//! Every PHP method maps to one Rust method here; the VM only converts
//! values. Strings are byte vectors (PHP strings), `null` is `None`, and
//! thrown exceptions are [`UriError`](crate::UriError) values.

pub mod rfc3986;
pub mod whatwg;

pub use self::rfc3986::{Rfc3986Uri, Rfc3986UriBuilder, UriHostType, UriType};
pub use self::whatwg::{WhatWgUrl, WhatWgUrlBuilder};

use crate::{ErrorClass, UriError};

/// A value in a `__debugInfo()` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugValue {
    /// `null`.
    Null,
    /// A string.
    Str(Vec<u8>),
    /// An integer.
    Int(i64),
}

impl DebugValue {
    fn from_opt(v: Option<Vec<u8>>) -> Self {
        v.map_or(DebugValue::Null, DebugValue::Str)
    }
}

/// The key under which `__serialize()` stores the URI string.
pub const SERIALIZE_URI_FIELD: &str = "uri";

/// `Exception: Invalid serialization data for <class> object`.
pub fn invalid_serialization_data(class_name: &str) -> UriError {
    UriError::new(
        ErrorClass::Exception,
        format!("Invalid serialization data for {class_name} object"),
    )
}

/// `Error: Cannot modify readonly object of class <class>` (calling the
/// constructor or `__unserialize()` on an initialized object).
pub fn cannot_modify_readonly_object(class_name: &str) -> UriError {
    UriError::new(
        ErrorClass::Error,
        format!("Cannot modify readonly object of class {class_name}"),
    )
}

/// The `ValueError` thrown by `Uri\WhatWg\InvalidUrlException::__construct()`
/// when `$errors` is not a list of `Uri\WhatWg\UrlValidationError`.
pub fn invalid_url_exception_errors_value_error() -> UriError {
    UriError::new(
        ErrorClass::ValueError,
        "Uri\\WhatWg\\InvalidUrlException::__construct(): Argument #2 ($errors) must be a list of Uri\\WhatWg\\UrlValidationError",
    )
}

/// PHP's `port_str_to_zend_long_checked`: the port text as an integer, or
/// -1 when it exceeds `PHP_INT_MAX`.
pub(crate) fn port_str_to_long(s: &[u8]) -> i64 {
    let mut s = s;
    while s.len() > 1 && s[0] == b'0' {
        s = &s[1..];
    }
    if s.len() > 20 {
        return -1;
    }
    let mut v: u64 = 0;
    for &c in s {
        if !c.is_ascii_digit() {
            break;
        }
        match v
            .checked_mul(10)
            .and_then(|x| x.checked_add(u64::from(c - b'0')))
        {
            Some(n) => v = n,
            None => return -1,
        }
    }
    i64::try_from(v).unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_conversion() {
        assert_eq!(port_str_to_long(b"0080"), 80);
        assert_eq!(port_str_to_long(b"0"), 0);
        assert_eq!(port_str_to_long(b"9223372036854775807"), i64::MAX);
        assert_eq!(port_str_to_long(b"9223372036854775808"), -1);
        assert_eq!(port_str_to_long(b"000000000000000000000000001"), 1);
        assert_eq!(port_str_to_long(b"999999999999999999999"), -1);
    }

    #[test]
    fn messages() {
        assert_eq!(
            invalid_serialization_data("Uri\\Rfc3986\\Uri").message,
            "Invalid serialization data for Uri\\Rfc3986\\Uri object"
        );
        assert_eq!(cannot_modify_readonly_object("X").class, ErrorClass::Error);
    }
}

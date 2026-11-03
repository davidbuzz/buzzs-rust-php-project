//! Errors reported by the bz2 functions.

use std::fmt;

/// How a zlib function failed.
///
/// `Warning` corresponds to `php_error_docref(NULL, E_WARNING, …)` followed
/// by `RETURN_FALSE`: the message does **not** include the `function(): `
/// prefix (the VM adds it, as `php_error_docref()` does). The exception
/// variants carry the complete `getMessage()` text, including the
/// `function(): Argument #n ($name) ` prefix where PHP produces one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// `E_WARNING`; the function returns `false`.
    Warning(String),
    /// A thrown `ValueError`.
    ValueError(String),
    /// A thrown `TypeError`.
    TypeError(String),
    /// A thrown `Error`.
    Error(String),
}

impl Error {
    /// The message text.
    pub fn message(&self) -> &str {
        match self {
            Error::Warning(m) | Error::ValueError(m) | Error::TypeError(m) | Error::Error(m) => m,
        }
    }

    /// Whether this is an exception (as opposed to a warning + `false`).
    pub fn is_exception(&self) -> bool {
        !matches!(self, Error::Warning(_))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Error::Warning(_) => "Warning",
            Error::ValueError(_) => "ValueError",
            Error::TypeError(_) => "TypeError",
            Error::Error(_) => "Error",
        };
        write!(f, "{kind}: {}", self.message())
    }
}

impl std::error::Error for Error {}

/// `zend_argument_value_error()` text.
pub(crate) fn arg_value_error(func: &str, arg: u32, name: &str, msg: &str) -> Error {
    Error::ValueError(format!("{func}(): Argument #{arg} (${name}) {msg}"))
}

/// `zend_argument_type_error()` text.
pub(crate) fn arg_type_error(func: &str, arg: u32, name: &str, msg: &str) -> Error {
    Error::TypeError(format!("{func}(): Argument #{arg} (${name}) {msg}"))
}

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts() {
        let e = arg_value_error("bzcompress", 2, "block_size", "must be between 1 and 9");
        assert_eq!(
            e.to_string(),
            "ValueError: bzcompress(): Argument #2 ($block_size) must be between 1 and 9"
        );
        assert!(e.is_exception());
        assert!(!Error::Warning("x".into()).is_exception());
        let t = arg_type_error("f", 1, "x", "must be y");
        assert_eq!(t.message(), "f(): Argument #1 ($x) must be y");
    }
}

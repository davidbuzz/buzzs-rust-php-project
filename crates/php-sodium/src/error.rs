//! Failures, carrying PHP's exact messages.

use std::fmt;

/// What a `sodium_*` function throws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// `SodiumException` (a plain `Exception` subclass) with this message.
    Sodium(String),
    /// `ValueError` with this message (argument validation of the
    /// `pwhash` family and of `password_hash()` options).
    Value(String),
}

impl Error {
    /// The exception message.
    pub fn message(&self) -> &str {
        match self {
            Self::Sodium(m) | Self::Value(m) => m,
        }
    }

    /// The PHP class to instantiate.
    pub fn class_name(&self) -> &'static str {
        match self {
            Self::Sodium(_) => "SodiumException",
            Self::Value(_) => "ValueError",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.class_name(), self.message())
    }
}

impl std::error::Error for Error {}

/// `Result` with [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// `zend_argument_error(sodium_exception_ce, n, what)`.
pub(crate) fn arg(func: &str, n: u32, name: &str, what: &str) -> Error {
    Error::Sodium(format!("{func}(): Argument #{n} (${name}) {what}"))
}

/// `zend_argument_value_error(n, what)`.
pub(crate) fn value_arg(func: &str, n: u32, name: &str, what: &str) -> Error {
    Error::Value(format!("{func}(): Argument #{n} (${name}) {what}"))
}

/// `zend_throw_exception(sodium_exception_ce, msg, 0)`.
pub(crate) fn exc(msg: &str) -> Error {
    Error::Sodium(msg.to_owned())
}

/// "internal error".
pub(crate) fn internal() -> Error {
    exc("internal error")
}

/// "arithmetic overflow".
pub(crate) fn overflow() -> Error {
    exc("arithmetic overflow")
}

/// Checks `buf.len() == want`, else the "must be `CONST` bytes long" error.
pub(crate) fn check_len(
    func: &str,
    n: u32,
    name: &str,
    buf: &[u8],
    want: usize,
    constant: &str,
) -> Result<()> {
    if buf.len() == want {
        Ok(())
    } else {
        Err(arg(
            func,
            n,
            name,
            &format!("must be {constant} bytes long"),
        ))
    }
}

/// The `SodiumException` for a by-reference `$state` argument that does not
/// hold a string (`generichash_update/final`, `secretstream_*`).
pub fn not_a_state_reference(func: &str) -> Error {
    arg(func, 1, "state", "must be a reference to a state")
}

/// `sodium_memzero()` / `sodium_increment()` on a non-string variable.
pub const STRING_REQUIRED: &str = "a PHP string is required";
/// `sodium_add()` on a non-string variable.
pub const STRINGS_REQUIRED: &str = "PHP strings are required";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(
            arg(
                "sodium_crypto_secretbox",
                2,
                "nonce",
                "must be X bytes long"
            )
            .message(),
            "sodium_crypto_secretbox(): Argument #2 ($nonce) must be X bytes long"
        );
        let e = value_arg("f", 1, "length", "must be greater than 0");
        assert_eq!(e.class_name(), "ValueError");
        assert_eq!(
            e.to_string(),
            "ValueError: f(): Argument #1 ($length) must be greater than 0"
        );
        assert_eq!(
            not_a_state_reference("sodium_crypto_generichash_update").message(),
            "sodium_crypto_generichash_update(): Argument #1 ($state) must be a reference to a state"
        );
        assert!(check_len("f", 1, "k", b"ab", 2, "K").is_ok());
        assert_eq!(
            check_len("f", 1, "k", b"a", 2, "K").unwrap_err().message(),
            "f(): Argument #1 ($k) must be K bytes long"
        );
    }
}

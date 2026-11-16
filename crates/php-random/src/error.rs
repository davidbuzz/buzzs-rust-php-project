// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Error type shared by every fallible operation of the crate.

use std::fmt;

/// An error raised by a random operation, mirroring the PHP throwable that
/// the C implementation would throw.
///
/// The message strings are complete and byte-identical to PHP's (including
/// the `function(): Argument #n ($name)` prefix for argument errors), so the
/// VM only has to pick the class from the variant and pass the message along.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RandomError {
    /// A `\ValueError`.
    ValueError(String),
    /// A `Random\BrokenRandomEngineError` (extends `Random\RandomError`).
    BrokenEngine(String),
    /// A `Random\RandomException` (CSPRNG failures).
    RandomException(String),
    /// A plain `\Exception` (e.g. invalid serialization data).
    Exception(String),
    /// A userland `Random\Engine::generate()` threw; the exception is already
    /// pending inside the VM and only needs to be propagated.
    Propagated,
}

impl RandomError {
    /// Builds the message of `zend_argument_value_error()`:
    /// `"{func}(): Argument #{arg} (${name}) {msg}"`.
    pub fn argument_value(func: &str, arg: u32, name: &str, msg: &str) -> Self {
        Self::ValueError(format!("{func}(): Argument #{arg} (${name}) {msg}"))
    }

    /// Builds the `Random\BrokenRandomEngineError` thrown after
    /// [`RANGE_ATTEMPTS`](crate::RANGE_ATTEMPTS) failed rejection rounds.
    pub fn too_many_attempts() -> Self {
        Self::BrokenEngine(format!(
            "Failed to generate an acceptable random number in {} attempts",
            crate::RANGE_ATTEMPTS
        ))
    }

    /// Builds the `\Exception` thrown by the engines' `__unserialize()`.
    pub fn invalid_serialization(class: &str) -> Self {
        Self::Exception(format!("Invalid serialization data for {class} object"))
    }

    /// The message carried by the error (empty for [`RandomError::Propagated`]).
    pub fn message(&self) -> &str {
        match self {
            Self::ValueError(m)
            | Self::BrokenEngine(m)
            | Self::RandomException(m)
            | Self::Exception(m) => m,
            Self::Propagated => "",
        }
    }

    /// The fully-qualified PHP class name of the throwable.
    pub fn class_name(&self) -> &'static str {
        match self {
            Self::ValueError(_) => "ValueError",
            Self::BrokenEngine(_) => "Random\\BrokenRandomEngineError",
            Self::RandomException(_) => "Random\\RandomException",
            Self::Exception(_) => "Exception",
            Self::Propagated => "",
        }
    }
}

impl fmt::Display for RandomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Propagated => f.write_str("exception thrown by user engine"),
            other => write!(f, "{}: {}", other.class_name(), other.message()),
        }
    }
}

impl std::error::Error for RandomError {}

/// Convenience alias.
pub type Result<T, E = RandomError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_value_message() {
        let e = RandomError::argument_value(
            "Random\\Randomizer::getInt",
            2,
            "max",
            "must be greater than or equal to argument #1 ($min)",
        );
        assert_eq!(
            e.message(),
            "Random\\Randomizer::getInt(): Argument #2 ($max) must be greater than or equal to argument #1 ($min)"
        );
        assert_eq!(e.class_name(), "ValueError");
    }

    #[test]
    fn attempts_message() {
        assert_eq!(
            RandomError::too_many_attempts().message(),
            "Failed to generate an acceptable random number in 50 attempts"
        );
        assert_eq!(
            RandomError::invalid_serialization("Random\\Engine\\Mt19937").to_string(),
            "Exception: Invalid serialization data for Random\\Engine\\Mt19937 object"
        );
        assert_eq!(RandomError::Propagated.message(), "");
    }
}

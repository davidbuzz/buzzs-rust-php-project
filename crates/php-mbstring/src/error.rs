//! Errors (PHP exceptions) and diagnostics (warnings/deprecations).

use std::fmt;

/// A PHP exception raised by an `mb_*` function.
///
/// Argument errors render (via `Display`) as PHP's
/// `zend_argument_value_error` text *without* the leading `"func(): "`,
/// e.g. `Argument #4 ($encoding) must be a valid encoding, "X" given`; the
/// VM prepends the function name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MbError {
    /// `ValueError` about a specific argument.
    ArgValue {
        /// 1-based argument number.
        arg: u32,
        /// Parameter name without `$`.
        name: &'static str,
        /// Message after `Argument #n ($name) `.
        msg: String,
    },
    /// `TypeError` about a specific argument.
    ArgType {
        /// 1-based argument number.
        arg: u32,
        /// Parameter name without `$`.
        name: &'static str,
        /// Message after `Argument #n ($name) `.
        msg: String,
    },
    /// A plain `ValueError` with the full message (no argument prefix).
    Value(String),
    /// A plain `Error` with the full message.
    Error(String),
}

impl MbError {
    /// Construct an argument `ValueError`.
    pub fn arg_value(arg: u32, name: &'static str, msg: impl Into<String>) -> Self {
        Self::ArgValue {
            arg,
            name,
            msg: msg.into(),
        }
    }

    /// Construct an argument `TypeError`.
    pub fn arg_type(arg: u32, name: &'static str, msg: impl Into<String>) -> Self {
        Self::ArgType {
            arg,
            name,
            msg: msg.into(),
        }
    }

    /// `must be a valid encoding, "X" given` for argument `arg`.
    pub fn invalid_encoding(arg: u32, name: &'static str, given: &[u8]) -> Self {
        Self::arg_value(
            arg,
            name,
            format!(
                "must be a valid encoding, \"{}\" given",
                String::from_utf8_lossy(given)
            ),
        )
    }

    /// `must not be empty` for argument `arg`
    /// (`zend_argument_must_not_be_empty_error`).
    pub fn must_not_be_empty(arg: u32, name: &'static str) -> Self {
        Self::arg_value(arg, name, "must not be empty")
    }

    /// The PHP exception class name.
    pub fn class(&self) -> &'static str {
        match self {
            Self::ArgValue { .. } | Self::Value(_) => "ValueError",
            Self::ArgType { .. } => "TypeError",
            Self::Error(_) => "Error",
        }
    }

    /// The full exception message for function `func`.
    pub fn message_for(&self, func: &str) -> String {
        match self {
            Self::Value(m) | Self::Error(m) => m.clone(),
            _ => format!("{func}(): {self}"),
        }
    }
}

impl fmt::Display for MbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ArgValue { arg, name, msg } | Self::ArgType { arg, name, msg } => {
                write!(f, "Argument #{arg} (${name}) {msg}")
            }
            Self::Value(m) | Self::Error(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for MbError {}

/// Severity of a non-fatal diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// `E_WARNING`.
    Warning,
    /// `E_NOTICE`.
    Notice,
    /// `E_DEPRECATED`.
    Deprecated,
}

/// A warning/notice/deprecation emitted by an `mb_*` function. The message
/// is the text after `"func(): "` (as produced by `php_error_docref`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Severity.
    pub level: Level,
    /// Message text.
    pub message: String,
}

/// Result alias.
pub type MbResult<T> = Result<T, MbError>;

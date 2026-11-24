// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Error (thrown) and diagnostic (warning/deprecation) channels.
//!
//! PHP functions report problems in two ways:
//!
//! * by **throwing** (`ValueError`, `TypeError`, `DivisionByZeroError`, …) —
//!   modelled by returning `Err(StdError)`;
//! * by **emitting** an `E_WARNING` / `E_NOTICE` / `E_DEPRECATED` and carrying
//!   on — modelled by pushing a [`Notice`] onto a caller-supplied
//!   [`Notices`] buffer.
//!
//! Both carry the name of the PHP function that produced them separately
//! from the message so that the VM can substitute an alias name (`chop` for
//! `rtrim`, `join` for `implode`, …).  [`std::fmt::Display`] renders the
//! message exactly as PHP prints it, e.g.
//! `str_pad(): Argument #3 ($pad_string) must be a non-empty string`.

use std::fmt;

/// The PHP exception class an error is thrown as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// `\ValueError`.
    ValueError,
    /// `\TypeError`.
    TypeError,
    /// `\ArgumentCountError`.
    ArgumentCountError,
    /// `\ArithmeticError`.
    ArithmeticError,
    /// `\DivisionByZeroError`.
    DivisionByZeroError,
    /// `\Error`.
    Error,
    /// `\Exception` (`zend_throw_exception(NULL, ...)`).
    Exception,
}

impl ErrorKind {
    /// The PHP class name of the exception.
    pub const fn class_name(self) -> &'static str {
        match self {
            Self::ValueError => "ValueError",
            Self::TypeError => "TypeError",
            Self::ArgumentCountError => "ArgumentCountError",
            Self::ArithmeticError => "ArithmeticError",
            Self::DivisionByZeroError => "DivisionByZeroError",
            Self::Error => "Error",
            Self::Exception => "Exception",
        }
    }
}

/// An exception a PHP function throws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdError {
    /// Exception class.
    pub kind: ErrorKind,
    /// The function whose name prefixes the message (`"str_pad"` →
    /// `str_pad(): …`), or `None` for messages PHP throws without a prefix
    /// (e.g. `Division by zero`).
    pub func: Option<&'static str>,
    /// The message without the `func(): ` prefix.
    pub message: String,
}

impl StdError {
    /// A prefix-less error.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            func: None,
            message: message.into(),
        }
    }

    /// An error prefixed with `func(): `.
    pub fn in_func(kind: ErrorKind, func: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            func: Some(func),
            message: message.into(),
        }
    }

    /// `zend_argument_value_error(arg_num, "must be …")`:
    /// `func(): Argument #N ($name) must be …` as a `ValueError`.
    pub fn arg_value(func: &'static str, arg_num: u32, arg_name: &str, msg: &str) -> Self {
        Self::in_func(
            ErrorKind::ValueError,
            func,
            format!("Argument #{arg_num} (${arg_name}) {msg}"),
        )
    }

    /// `zend_argument_type_error(arg_num, "must be …")` as a `TypeError`.
    pub fn arg_type(func: &'static str, arg_num: u32, arg_name: &str, msg: &str) -> Self {
        Self::in_func(
            ErrorKind::TypeError,
            func,
            format!("Argument #{arg_num} (${arg_name}) {msg}"),
        )
    }

    /// A `ValueError` with a `func(): ` prefix.
    pub fn value(func: &'static str, message: impl Into<String>) -> Self {
        Self::in_func(ErrorKind::ValueError, func, message)
    }

    /// Returns a copy that reports `func` as the function name (for aliases).
    #[must_use]
    pub fn with_func(mut self, func: &'static str) -> Self {
        if self.func.is_some() {
            self.func = Some(func);
        }
        self
    }
}

impl fmt::Display for StdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(func) = self.func {
            write!(f, "{func}(): {}", self.message)
        } else {
            f.write_str(&self.message)
        }
    }
}

impl std::error::Error for StdError {}

/// Severity of a non-fatal diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Level {
    /// `E_WARNING`.
    Warning,
    /// `E_NOTICE`.
    Notice,
    /// `E_DEPRECATED`.
    Deprecated,
}

impl Level {
    /// The label PHP prints before the message (`Warning`, `Notice`,
    /// `Deprecated`).
    pub const fn label(self) -> &'static str {
        match self {
            Self::Warning => "Warning",
            Self::Notice => "Notice",
            Self::Deprecated => "Deprecated",
        }
    }
}

/// A warning/notice/deprecation emitted while the function keeps running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Severity.
    pub level: Level,
    /// Function name prefix (`php_error_docref` adds `func(): `), if any.
    pub func: Option<&'static str>,
    /// Message text without the prefix.
    pub message: String,
}

impl Notice {
    /// An `E_WARNING` from `func`.
    pub fn warning(func: &'static str, message: impl Into<String>) -> Self {
        Self {
            level: Level::Warning,
            func: Some(func),
            message: message.into(),
        }
    }

    /// An `E_NOTICE` from `func`.
    pub fn e_notice(func: &'static str, message: impl Into<String>) -> Self {
        Self {
            level: Level::Notice,
            func: Some(func),
            message: message.into(),
        }
    }

    /// An `E_DEPRECATED` from `func`.
    pub fn deprecated(func: &'static str, message: impl Into<String>) -> Self {
        Self {
            level: Level::Deprecated,
            func: Some(func),
            message: message.into(),
        }
    }
}

impl fmt::Display for Notice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(func) = self.func {
            write!(f, "{func}(): {}", self.message)
        } else {
            f.write_str(&self.message)
        }
    }
}

/// Buffer that collects the diagnostics a call emits, in order.
pub type Notices = Vec<Notice>;

/// Shorthand result type.
pub type StdResult<T> = Result<T, StdError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_prefixes() {
        let e = StdError::arg_value("str_pad", 3, "pad_string", "must be a non-empty string");
        assert_eq!(
            e.to_string(),
            "str_pad(): Argument #3 ($pad_string) must be a non-empty string"
        );
        assert_eq!(e.kind.class_name(), "ValueError");
        let e = StdError::new(ErrorKind::DivisionByZeroError, "Division by zero");
        assert_eq!(e.to_string(), "Division by zero");
        assert_eq!(e.clone().with_func("x").to_string(), "Division by zero");
        let n = Notice::warning("trim", "boom");
        assert_eq!(n.to_string(), "trim(): boom");
        assert_eq!(n.level.label(), "Warning");
    }
}

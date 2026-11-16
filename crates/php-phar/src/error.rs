// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Exceptions thrown by the phar classes.

use std::fmt;

/// The class of a thrown exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExceptionClass {
    /// `UnexpectedValueException`.
    UnexpectedValue,
    /// `BadMethodCallException`.
    BadMethodCall,
    /// `PharException`.
    Phar,
    /// `RuntimeException`.
    Runtime,
    /// `InvalidArgumentException`.
    InvalidArgument,
    /// `ValueError` (from `zend_argument_value_error`; the message lacks
    /// the `Class::method(): Argument #n ($name) ` prefix, which the VM
    /// adds — see [`PharError::arg`]).
    ValueError,
}

impl ExceptionClass {
    /// The PHP class name.
    pub fn name(self) -> &'static str {
        match self {
            ExceptionClass::UnexpectedValue => "UnexpectedValueException",
            ExceptionClass::BadMethodCall => "BadMethodCallException",
            ExceptionClass::Phar => "PharException",
            ExceptionClass::Runtime => "RuntimeException",
            ExceptionClass::InvalidArgument => "InvalidArgumentException",
            ExceptionClass::ValueError => "ValueError",
        }
    }
}

/// A thrown exception.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PharError {
    /// Exception class.
    pub class: ExceptionClass,
    /// Message.
    pub message: String,
    /// For `ValueError`s: the 1-based argument number.
    pub arg: Option<u32>,
}

impl PharError {
    /// A new exception.
    pub fn new(class: ExceptionClass, message: impl Into<String>) -> Self {
        PharError {
            class,
            message: message.into(),
            arg: None,
        }
    }

    /// `UnexpectedValueException`.
    pub fn unexpected(m: impl Into<String>) -> Self {
        Self::new(ExceptionClass::UnexpectedValue, m)
    }

    /// `BadMethodCallException`.
    pub fn bad_method(m: impl Into<String>) -> Self {
        Self::new(ExceptionClass::BadMethodCall, m)
    }

    /// `PharException`.
    pub fn phar(m: impl Into<String>) -> Self {
        Self::new(ExceptionClass::Phar, m)
    }

    /// `RuntimeException`.
    pub fn runtime(m: impl Into<String>) -> Self {
        Self::new(ExceptionClass::Runtime, m)
    }

    /// `InvalidArgumentException`.
    pub fn invalid_argument(m: impl Into<String>) -> Self {
        Self::new(ExceptionClass::InvalidArgument, m)
    }

    /// `zend_argument_value_error(arg, message)`.
    pub fn arg(arg: u32, m: impl Into<String>) -> Self {
        PharError {
            class: ExceptionClass::ValueError,
            message: m.into(),
            arg: Some(arg),
        }
    }
}

impl fmt::Display for PharError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.class.name(), self.message)
    }
}

impl std::error::Error for PharError {}

/// Result alias for method implementations.
pub type PharResult<T> = Result<T, PharError>;

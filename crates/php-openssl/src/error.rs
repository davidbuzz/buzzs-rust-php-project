// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Diagnostics (warnings/deprecations), thrown errors, and PHP's private
//! OpenSSL error ring buffer behind `openssl_error_string()`.

use std::fmt;

/// Severity of a non-fatal diagnostic raised while running a function.
///
/// The VM turns these into `E_WARNING` / `E_DEPRECATED` / `E_NOTICE`
/// messages prefixed by `"<function>(): "`, exactly like `php_error_docref`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// `E_WARNING`.
    Warning,
    /// `E_DEPRECATED`.
    Deprecated,
    /// `E_NOTICE`.
    Notice,
}

/// A diagnostic message emitted by an `openssl_*` function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Severity.
    pub level: Level,
    /// Message text without the `"func(): "` prefix.
    pub message: String,
}

/// An exception thrown by an `openssl_*` function.
///
/// For [`Throw::ArgumentValueError`] the VM formats the message as
/// `"<func>(): Argument #<arg> ($<name>) <message>"`, mirroring
/// `zend_argument_value_error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Throw {
    /// `zend_argument_value_error(arg, message)`.
    ArgumentValueError {
        /// 1-based argument number.
        arg: u32,
        /// Message after the argument designation.
        message: String,
    },
    /// `zend_argument_must_not_be_empty_error(arg)`: a `ValueError`
    /// "must not be empty" for the given argument.
    ArgumentMustNotBeEmpty {
        /// 1-based argument number.
        arg: u32,
    },
    /// `zend_value_error(message)`: a plain `ValueError`.
    ValueError(String),
    /// `zend_throw_exception(zend_ce_exception, message, 0)`.
    Exception(String),
}

impl fmt::Display for Throw {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Throw::ArgumentValueError { arg, message } => {
                write!(f, "Argument #{arg} {message}")
            }
            Throw::ArgumentMustNotBeEmpty { arg } => write!(f, "Argument #{arg} must not be empty"),
            Throw::ValueError(m) | Throw::Exception(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Throw {}

/// Result type of every `openssl_*` entry point: `Err` means an exception
/// was thrown (the PHP function then returns nothing).
pub type PhpResult<T> = Result<T, Throw>;

/// `PHP_OPENSSL_ERR_BUFFER_SIZE`.
pub const ERR_BUFFER_SIZE: usize = 16;

/// PHP's ring buffer of OpenSSL error codes (`struct php_openssl_errors`).
///
/// Codes are kept as C `int`s (PHP truncates `unsigned long` to `int`), so
/// system errors (`0x8000_0000 | errno`) later print sign-extended, e.g.
/// `error:FFFFFFFF80000002:system library::No such file or directory`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorQueue {
    buffer: [i32; ERR_BUFFER_SIZE],
    top: usize,
    bottom: usize,
}

impl Default for ErrorQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl ErrorQueue {
    /// An empty queue.
    pub const fn new() -> Self {
        Self {
            buffer: [0; ERR_BUFFER_SIZE],
            top: 0,
            bottom: 0,
        }
    }

    /// Pushes one code, dropping the oldest when full.
    pub fn push(&mut self, code: i32) {
        self.top = (self.top + 1) % ERR_BUFFER_SIZE;
        if self.top == self.bottom {
            self.bottom = (self.bottom + 1) % ERR_BUFFER_SIZE;
        }
        self.buffer[self.top] = code;
    }

    /// Pops the oldest code (`openssl_error_string` order).
    pub fn pop(&mut self) -> Option<i32> {
        if self.top == self.bottom {
            return None;
        }
        self.bottom = (self.bottom + 1) % ERR_BUFFER_SIZE;
        Some(self.buffer[self.bottom])
    }

    /// True when nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.top == self.bottom
    }

    /// Empties the queue (`top = bottom = 0`).
    pub fn clear(&mut self) {
        self.top = 0;
        self.bottom = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_keeps_last_fifteen() {
        let mut q = ErrorQueue::new();
        for i in 1..=20 {
            q.push(i);
        }
        let mut out = vec![];
        while let Some(c) = q.pop() {
            out.push(c);
        }
        assert_eq!(out, (6..=20).collect::<Vec<_>>());
        assert!(q.is_empty());
    }

    #[test]
    fn throw_display() {
        let t = Throw::ArgumentValueError {
            arg: 1,
            message: "must be greater than 0".into(),
        };
        assert_eq!(t.to_string(), "Argument #1 must be greater than 0");
    }
}

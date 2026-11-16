// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The SPL exception hierarchy (`spl_exceptions.c`) and helpers to raise
//! SPL exceptions as [`RuntimeError::Custom`].

use php_runtime::{RuntimeError, ZStr};

/// One SPL exception class: its name and parent class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExceptionClass {
    /// Class name.
    pub name: &'static str,
    /// Parent class name (`Exception` or another SPL exception).
    pub parent: &'static str,
}

/// The 13 SPL exception classes in registration order.
pub const EXCEPTION_CLASSES: [ExceptionClass; 13] = [
    ExceptionClass {
        name: "LogicException",
        parent: "Exception",
    },
    ExceptionClass {
        name: "BadFunctionCallException",
        parent: "LogicException",
    },
    ExceptionClass {
        name: "BadMethodCallException",
        parent: "BadFunctionCallException",
    },
    ExceptionClass {
        name: "DomainException",
        parent: "LogicException",
    },
    ExceptionClass {
        name: "InvalidArgumentException",
        parent: "LogicException",
    },
    ExceptionClass {
        name: "LengthException",
        parent: "LogicException",
    },
    ExceptionClass {
        name: "OutOfRangeException",
        parent: "LogicException",
    },
    ExceptionClass {
        name: "RuntimeException",
        parent: "Exception",
    },
    ExceptionClass {
        name: "OutOfBoundsException",
        parent: "RuntimeException",
    },
    ExceptionClass {
        name: "OverflowException",
        parent: "RuntimeException",
    },
    ExceptionClass {
        name: "RangeException",
        parent: "RuntimeException",
    },
    ExceptionClass {
        name: "UnderflowException",
        parent: "RuntimeException",
    },
    ExceptionClass {
        name: "UnexpectedValueException",
        parent: "RuntimeException",
    },
];

/// The parent of an SPL exception class (case-insensitive lookup).
#[must_use]
pub fn exception_parent(name: &str) -> Option<&'static str> {
    EXCEPTION_CLASSES
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(name))
        .map(|c| c.parent)
}

/// Whether `class` is `ancestor` or derives from it within the SPL
/// exception hierarchy (`Exception` is the root of every SPL exception).
#[must_use]
pub fn exception_is_a(class: &str, ancestor: &str) -> bool {
    let mut cur = class;
    loop {
        if cur.eq_ignore_ascii_case(ancestor) {
            return true;
        }
        match exception_parent(cur) {
            Some(p) => cur = p,
            None => return false,
        }
    }
}

/// Builds an exception of the given class with `message`
/// (`zend_throw_exception(ce, message, 0)`).
#[must_use]
pub fn throw(class: &str, message: impl Into<String>) -> RuntimeError {
    RuntimeError::Custom {
        class: ZStr::from(class),
        message: message.into(),
    }
}

/// `\Exception` (`zend_throw_exception(NULL, ...)`).
#[must_use]
pub fn exception(message: impl Into<String>) -> RuntimeError {
    throw("Exception", message)
}

/// `LogicException`.
#[must_use]
pub fn logic(message: impl Into<String>) -> RuntimeError {
    throw("LogicException", message)
}

/// `BadMethodCallException`.
#[must_use]
pub fn bad_method_call(message: impl Into<String>) -> RuntimeError {
    throw("BadMethodCallException", message)
}

/// `InvalidArgumentException`.
#[must_use]
pub fn invalid_argument(message: impl Into<String>) -> RuntimeError {
    throw("InvalidArgumentException", message)
}

/// `OutOfRangeException`.
#[must_use]
pub fn out_of_range(message: impl Into<String>) -> RuntimeError {
    throw("OutOfRangeException", message)
}

/// `OutOfBoundsException`.
#[must_use]
pub fn out_of_bounds(message: impl Into<String>) -> RuntimeError {
    throw("OutOfBoundsException", message)
}

/// `RuntimeException`.
#[must_use]
pub fn runtime(message: impl Into<String>) -> RuntimeError {
    throw("RuntimeException", message)
}

/// `UnexpectedValueException`.
#[must_use]
pub fn unexpected_value(message: impl Into<String>) -> RuntimeError {
    throw("UnexpectedValueException", message)
}

/// `\Error`.
#[must_use]
pub fn error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::Error(message.into())
}

/// `\TypeError`.
#[must_use]
pub fn type_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::TypeError(message.into())
}

/// `\ValueError`.
#[must_use]
pub fn value_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::ValueError(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hierarchy() {
        assert_eq!(
            exception_parent("outofboundsexception"),
            Some("RuntimeException")
        );
        assert!(exception_is_a("BadMethodCallException", "LogicException"));
        assert!(exception_is_a("BadMethodCallException", "Exception"));
        assert!(!exception_is_a("RangeException", "LogicException"));
        assert_eq!(
            runtime("x").exception_class().as_deref(),
            Some("RuntimeException")
        );
    }
}

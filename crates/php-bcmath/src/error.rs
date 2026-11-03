//! Errors raised by the bcmath functions, carrying PHP's exact messages.

use std::fmt;

/// The PHP exception class an error must be thrown as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// `ValueError`.
    ValueError,
    /// `DivisionByZeroError`.
    DivisionByZeroError,
    /// `TypeError`.
    TypeError,
    /// `Error` (readonly property violations).
    Error,
    /// `Exception` (invalid serialization data).
    Exception,
    /// The operation would need more memory than PHP's allocator permits
    /// (PHP raises a fatal "memory size exhausted" error). The message holds
    /// a description including the attempted allocation size.
    OutOfMemory,
}

impl ErrorKind {
    /// The PHP class name.
    pub fn class_name(self) -> &'static str {
        match self {
            Self::ValueError => "ValueError",
            Self::DivisionByZeroError => "DivisionByZeroError",
            Self::TypeError => "TypeError",
            Self::Error | Self::OutOfMemory => "Error",
            Self::Exception => "Exception",
        }
    }
}

/// A PHP exception to be thrown by the VM: the class and the full message
/// (e.g. `bcadd(): Argument #1 ($num1) is not well-formed`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BcError {
    /// Exception class.
    pub kind: ErrorKind,
    /// Message, byte-for-byte as PHP prints it.
    pub message: String,
}

impl BcError {
    /// Builds an error of the given kind.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// A plain `ValueError` (`zend_value_error`).
    pub fn value(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::ValueError, message)
    }

    /// A `DivisionByZeroError`.
    pub fn division_by_zero(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::DivisionByZeroError, message)
    }

    /// An allocation of `bytes` bytes would exceed the memory limit.
    pub fn out_of_memory(bytes: usize) -> Self {
        Self::new(
            ErrorKind::OutOfMemory,
            format!("Out of memory (tried to allocate {bytes} bytes)"),
        )
    }

    /// `zend_argument_value_error`: `"{func}(): Argument #{n} (${name}) {msg}"`.
    pub fn argument_value(func: &str, arg_num: u32, arg_name: &str, msg: &str) -> Self {
        Self::value(format!("{func}(): Argument #{arg_num} (${arg_name}) {msg}"))
    }

    /// `zend_argument_type_error` as used by `BcMath\Number` methods for an
    /// operand of the wrong type (`given` is `zend_zval_value_name`, e.g.
    /// `array`, `stdClass`).
    pub fn number_argument_type(func: &str, arg_num: u32, arg_name: &str, given: &str) -> Self {
        Self::new(
            ErrorKind::TypeError,
            format!(
                "{func}(): Argument #{arg_num} (${arg_name}) must be of type int, string, or BcMath\\Number, {given} given"
            ),
        )
    }

    /// `Cannot modify readonly property BcMath\Number::$<prop>` (`Error`).
    pub fn readonly_modification(prop: &str) -> Self {
        Self::new(
            ErrorKind::Error,
            format!("Cannot modify readonly property BcMath\\Number::${prop}"),
        )
    }

    /// `Cannot unset readonly property BcMath\Number::$<prop>` (`Error`).
    pub fn readonly_unset(prop: &str) -> Self {
        Self::new(
            ErrorKind::Error,
            format!("Cannot unset readonly property BcMath\\Number::${prop}"),
        )
    }
}

impl fmt::Display for BcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.class_name(), self.message)
    }
}

impl std::error::Error for BcError {}

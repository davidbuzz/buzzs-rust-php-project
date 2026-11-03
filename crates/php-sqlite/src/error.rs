//! Exceptions raised by the extension.

use php_runtime::RuntimeError;
use std::fmt;

/// The class of an exception thrown by this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExceptionClass {
    /// `\Exception` (`zend_ce_exception`).
    Exception,
    /// `\Error` (`zend_throw_error(NULL, ...)`).
    Error,
    /// `\TypeError`.
    TypeError,
    /// `\ValueError`.
    ValueError,
    /// `\SQLite3Exception`.
    Sqlite3Exception,
    /// `\PDOException` (thrown directly by the driver, without `errorInfo`).
    PdoException,
}

impl ExceptionClass {
    /// The PHP class name.
    pub fn name(self) -> &'static str {
        match self {
            ExceptionClass::Exception => "Exception",
            ExceptionClass::Error => "Error",
            ExceptionClass::TypeError => "TypeError",
            ExceptionClass::ValueError => "ValueError",
            ExceptionClass::Sqlite3Exception => "SQLite3Exception",
            ExceptionClass::PdoException => "PDOException",
        }
    }
}

/// An exception to be instantiated and thrown by the VM.
#[derive(Debug, Clone)]
pub struct Exception {
    /// The class.
    pub class: ExceptionClass,
    /// `getMessage()`.
    pub message: String,
    /// `getCode()`.
    pub code: i64,
    /// `getPrevious()`.
    pub previous: Option<Box<PhpError>>,
}

impl Exception {
    /// A new exception without a previous one.
    pub fn new(class: ExceptionClass, message: impl Into<String>, code: i64) -> Self {
        Exception {
            class,
            message: message.into(),
            code,
            previous: None,
        }
    }
}

/// An error leaving an API call: either an exception raised by this crate or
/// one propagated from the runtime / user code.
#[derive(Debug, Clone)]
pub enum PhpError {
    /// An exception raised by the extension.
    Exception(Exception),
    /// A runtime error (`TypeError` from a conversion, an exception thrown
    /// by a user callback, ...).
    Runtime(RuntimeError),
}

impl PhpError {
    /// Shorthand for an extension exception.
    pub fn new(class: ExceptionClass, message: impl Into<String>, code: i64) -> Self {
        PhpError::Exception(Exception::new(class, message, code))
    }

    /// `\Error` with the given message.
    pub fn error(message: impl Into<String>) -> Self {
        PhpError::new(ExceptionClass::Error, message, 0)
    }

    /// The class name (`None` for an already-thrown user object).
    pub fn class_name(&self) -> Option<String> {
        match self {
            PhpError::Exception(e) => Some(e.class.name().to_string()),
            PhpError::Runtime(r) => r.exception_class(),
        }
    }

    /// The message (`None` for an already-thrown user object).
    pub fn message(&self) -> Option<&str> {
        match self {
            PhpError::Exception(e) => Some(&e.message),
            PhpError::Runtime(r) => r.message(),
        }
    }

    /// `getPrevious()` of an extension exception.
    pub fn previous(&self) -> Option<&PhpError> {
        match self {
            PhpError::Exception(e) => e.previous.as_deref(),
            PhpError::Runtime(_) => None,
        }
    }

    /// `zend_exception_set_previous()`: appends `prev` at the end of this
    /// exception's previous chain (runtime errors cannot carry one; the
    /// new error then simply replaces it, like an uncatchable chain).
    pub(crate) fn set_innermost_previous(&mut self, prev: PhpError) {
        match self {
            PhpError::Exception(e) => match &mut e.previous {
                Some(p) => p.set_innermost_previous(prev),
                None => e.previous = Some(Box::new(prev)),
            },
            PhpError::Runtime(_) => {}
        }
    }
}

impl From<RuntimeError> for PhpError {
    fn from(e: RuntimeError) -> Self {
        PhpError::Runtime(e)
    }
}

impl From<Exception> for PhpError {
    fn from(e: Exception) -> Self {
        PhpError::Exception(e)
    }
}

impl fmt::Display for PhpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.class_name(), self.message()) {
            (Some(c), Some(m)) => write!(f, "{c}: {m}"),
            _ => write!(f, "{self:?}"),
        }
    }
}

impl std::error::Error for PhpError {}

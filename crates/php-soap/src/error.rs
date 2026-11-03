//! Error propagation inside the extension.
//!
//! `soap_error*(E_ERROR, …)` and `php_error_docref(…, E_ERROR, …)` abort
//! the current operation; the SOAP error handler of `soap.c` then turns
//! the message into a `SoapFault` (client with exceptions), a fault
//! response (server) or a plain fatal error. [`SoapError::Fatal`] carries
//! such a message up to the entry point that applies those rules;
//! [`SoapError::Runtime`] carries exceptions and other engine errors
//! raised by user code or the host.

use php_runtime::RuntimeError;

/// An aborted operation.
#[derive(Debug, Clone)]
pub enum SoapError {
    /// An `E_ERROR` with its full message (e.g. `"SOAP-ERROR: Encoding: …"`).
    Fatal(String),
    /// An exception or engine error to propagate unchanged.
    Runtime(RuntimeError),
}

impl From<RuntimeError> for SoapError {
    fn from(e: RuntimeError) -> Self {
        match e {
            RuntimeError::Fatal(m) => SoapError::Fatal(m),
            other => SoapError::Runtime(other),
        }
    }
}

impl std::fmt::Display for SoapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SoapError::Fatal(m) => f.write_str(m),
            SoapError::Runtime(e) => write!(f, "{e:?}"),
        }
    }
}

impl std::error::Error for SoapError {}

/// Result alias.
pub type SoapResult<T> = Result<T, SoapError>;

/// `soap_error(E_ERROR, fmt…)`: a fatal error prefixed with `SOAP-ERROR: `.
#[must_use]
pub fn soap_fatal(msg: impl std::fmt::Display) -> SoapError {
    SoapError::Fatal(format!("SOAP-ERROR: {msg}"))
}

/// `soap_error` returning `Err` directly.
#[macro_export]
#[doc(hidden)]
macro_rules! bail {
    ($($arg:tt)*) => {
        return Err($crate::error::soap_fatal(format_args!($($arg)*)))
    };
}

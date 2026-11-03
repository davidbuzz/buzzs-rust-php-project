//! Exception and diagnostic helpers.
//!
//! Exceptions are reported as [`RuntimeError`]s carrying PHP's complete
//! `getMessage()` text (including the `ZipArchive::method(): Argument #n
//! ($name) ` prefix where PHP produces one). Warnings and deprecations are
//! emitted through the streams layer's diagnostics channel
//! ([`php_streams::diag`]) without the `function(): ` prefix, which the VM
//! adds like `php_error_docref()` does.

use php_runtime::RuntimeError;
use php_streams::diag::{self, Diagnostic, Severity};

/// The class name used in messages.
pub const CLASS_NAME: &str = "ZipArchive";

/// `zend_value_error("Invalid or uninitialized Zip object")`.
pub fn uninitialized() -> RuntimeError {
    RuntimeError::ValueError("Invalid or uninitialized Zip object".to_string())
}

/// `zend_throw_error(NULL, "Already being closed")`.
pub fn already_closing() -> RuntimeError {
    RuntimeError::Error("Already being closed".to_string())
}

/// `zend_argument_value_error(arg, msg)` inside `func`.
pub fn arg_value_error(func: &str, arg: u32, name: &str, msg: &str) -> RuntimeError {
    RuntimeError::ValueError(format!("{func}(): Argument #{arg} (${name}) {msg}"))
}

/// `zend_argument_type_error(arg, msg)` inside `func`.
pub fn arg_type_error(func: &str, arg: u32, name: &str, msg: &str) -> RuntimeError {
    RuntimeError::TypeError(format!("{func}(): Argument #{arg} (${name}) {msg}"))
}

/// `zend_argument_must_not_be_empty_error(arg)` inside `func`.
pub fn must_not_be_empty(func: &str, arg: u32, name: &str) -> RuntimeError {
    arg_value_error(func, arg, name, "must not be empty")
}

/// A plain `\Exception` with `message`.
pub fn exception(message: impl Into<String>) -> RuntimeError {
    RuntimeError::Custom {
        class: "Exception".into(),
        message: message.into(),
    }
}

/// `ZipArchive::__serialize()`'s exception for an object of `class`.
pub fn serialize_not_allowed(class: &str) -> RuntimeError {
    exception(format!(
        "Serialization of '{class}' is not allowed, override __serialize() and __unserialize() to implement it"
    ))
}

/// `ZipArchive::__unserialize()`'s exception for an object of `class`.
pub fn unserialize_not_allowed(class: &str) -> RuntimeError {
    exception(format!(
        "Unserialization of '{class}' is not allowed, override __serialize() and __unserialize() to implement it"
    ))
}

/// The `Error` thrown when writing one of the virtual properties.
pub fn readonly_property(class: &str, name: &str) -> RuntimeError {
    RuntimeError::Error(format!(
        "Cannot write read-only property {class}::${name}"
    ))
}

/// `php_error_docref(NULL, E_WARNING, msg)`.
pub(crate) fn warning(message: impl Into<Vec<u8>>) {
    diag::emit(Diagnostic::new(Severity::Warning, message));
}

/// `php_error_docref(NULL, E_DEPRECATED, msg)`.
pub(crate) fn deprecated(message: impl Into<Vec<u8>>) {
    diag::emit(Diagnostic::new(Severity::Deprecated, message).terminating(false));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts() {
        assert_eq!(
            must_not_be_empty("ZipArchive::open", 1, "filename").to_string(),
            "ValueError: ZipArchive::open(): Argument #1 ($filename) must not be empty"
        );
        assert_eq!(
            readonly_property("ZipArchive", "lastId").to_string(),
            "Error: Cannot write read-only property ZipArchive::$lastId"
        );
        assert_eq!(
            serialize_not_allowed("ZipArchive").to_string(),
            "Exception: Serialization of 'ZipArchive' is not allowed, override __serialize() and __unserialize() to implement it"
        );
        assert_eq!(uninitialized().message(), Some("Invalid or uninitialized Zip object"));
        assert_eq!(already_closing().class_name(), Some("Error"));
        assert_eq!(
            arg_type_error("f", 2, "files", "must x").to_string(),
            "TypeError: f(): Argument #2 ($files) must x"
        );
    }
}

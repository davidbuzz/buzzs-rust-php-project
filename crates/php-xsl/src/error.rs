// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Exceptions raised by `XSLTProcessor` methods.
//!
//! Methods return `Result<_, XslError>`; an `Err` is the exception PHP
//! would throw. Warnings are not errors: libxslt/libxml2 messages go
//! through `php_libxml::error` (the VM prefixes them with the active
//! method, as `php_error_docref` does), and the few warnings the extension
//! raises itself go through [`crate::XslHost::warning`].

use std::fmt;

/// The class of a thrown exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// `Error`.
    Error,
    /// `TypeError`.
    TypeError,
    /// `ValueError`.
    ValueError,
}

impl ErrorClass {
    /// The PHP class name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Error => "Error",
            Self::TypeError => "TypeError",
            Self::ValueError => "ValueError",
        }
    }
}

/// An exception thrown by the extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XslError {
    /// `zend_throw_error(NULL, …)`.
    Error(String),
    /// `zend_type_error(…)`.
    TypeError(String),
    /// `zend_value_error(…)`.
    ValueError(String),
    /// `zend_argument_value_error(arg, …)` raised by `function`.
    ArgumentValue {
        /// Method name without class (`"setParameter"`).
        function: &'static str,
        /// 1-based argument number.
        arg: u32,
        /// Parameter name without `$`.
        param: &'static str,
        /// The text after `"Argument #n ($param) "`.
        message: String,
    },
    /// `zend_argument_type_error(arg, …)` raised by `function`.
    ArgumentType {
        /// Method name without class.
        function: &'static str,
        /// 1-based argument number.
        arg: u32,
        /// Parameter name without `$`.
        param: &'static str,
        /// The text after `"Argument #n ($param) "`.
        message: String,
    },
}

/// The class every method belongs to (argument errors use the function's
/// scope, not the object's class).
pub const CLASS_NAME: &str = "XSLTProcessor";

impl XslError {
    /// The exception class.
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Error(_) => ErrorClass::Error,
            Self::TypeError(_) | Self::ArgumentType { .. } => ErrorClass::TypeError,
            Self::ValueError(_) | Self::ArgumentValue { .. } => ErrorClass::ValueError,
        }
    }

    /// The exception message, exactly as PHP formats it.
    pub fn message(&self) -> String {
        match self {
            Self::Error(m) | Self::TypeError(m) | Self::ValueError(m) => m.clone(),
            Self::ArgumentValue {
                function,
                arg,
                param,
                message,
            }
            | Self::ArgumentType {
                function,
                arg,
                param,
                message,
            } => format!("{CLASS_NAME}::{function}(): Argument #{arg} (${param}) {message}"),
        }
    }

    pub(crate) fn arg_value(
        function: &'static str,
        arg: u32,
        param: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self::ArgumentValue {
            function,
            arg,
            param,
            message: message.into(),
        }
    }

    pub(crate) fn arg_type(
        function: &'static str,
        arg: u32,
        param: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self::ArgumentType {
            function,
            arg,
            param,
            message: message.into(),
        }
    }

    /// `Z_PARAM_PATH*`: "must not contain any null bytes".
    pub(crate) fn null_bytes(function: &'static str, arg: u32, param: &'static str) -> Self {
        Self::arg_value(function, arg, param, "must not contain any null bytes")
    }

    /// `php_xsl_apply_stylesheet` / `importStylesheet`: the argument is
    /// not an XML node (`php_libxml_import_node` failed).
    pub fn invalid_node(function: &'static str) -> Self {
        let param = if function == "importStylesheet" {
            "stylesheet"
        } else {
            "document"
        };
        Self::arg_type(function, 1, param, "must be a valid XML node")
    }

    /// `importStylesheet`: the object has no clone handler.
    pub fn not_cloneable() -> Self {
        Self::arg_type(
            "importStylesheet",
            1,
            "stylesheet",
            "must be a cloneable node",
        )
    }

    /// `importStylesheet`: a node that is not part of a document.
    pub fn not_in_document() -> Self {
        Self::arg_value(
            "importStylesheet",
            1,
            "stylesheet",
            "must be part of a document",
        )
    }

    /// `importStylesheet`: `__clone()` kept a reference to the clone.
    pub fn clone_retained() -> Self {
        Self::arg_value(
            "importStylesheet",
            1,
            "stylesheet",
            "must not have its clone retained by __clone()",
        )
    }

    /// `transformToDoc`: `$returnClass` is not compatible with the input
    /// object's class hierarchy root.
    pub fn incompatible_return_class(current_class: &str, given: &str) -> Self {
        Self::arg_type(
            "transformToDoc",
            2,
            "returnClass",
            format!("must be a class name compatible with {current_class}, {given} given"),
        )
    }

    /// `transformToDoc`: `C!` parsing of an unknown class name.
    pub fn unknown_return_class(given: &str) -> Self {
        Self::arg_type(
            "transformToDoc",
            2,
            "returnClass",
            format!("must be a valid class name or null, \"{given}\" given"),
        )
    }

    /// `XSLTProcessor::$prop must be greater than or equal to 0`.
    pub fn negative_limit(property: &str) -> Self {
        Self::ValueError(format!(
            "{CLASS_NAME}::${property} must be greater than or equal to 0"
        ))
    }

    /// `Indirect modification of XSLTProcessor::$prop is not allowed`.
    pub fn indirect_modification(property: &str) -> Self {
        Self::Error(format!(
            "Indirect modification of {CLASS_NAME}::${property} is not allowed"
        ))
    }

    /// `Cannot unset XSLTProcessor::$prop`.
    pub fn cannot_unset(property: &str) -> Self {
        Self::Error(format!("Cannot unset {CLASS_NAME}::${property}"))
    }
}

impl fmt::Display for XslError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.class().name(), self.message())
    }
}

impl std::error::Error for XslError {}

/// Result alias.
pub type XslResult<T> = Result<T, XslError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_messages() {
        let e = XslError::null_bytes("setParameter", 2, "name");
        assert_eq!(e.class(), ErrorClass::ValueError);
        assert_eq!(
            e.message(),
            "XSLTProcessor::setParameter(): Argument #2 ($name) must not contain any null bytes"
        );
        assert_eq!(
            XslError::invalid_node("importStylesheet").message(),
            "XSLTProcessor::importStylesheet(): Argument #1 ($stylesheet) must be a valid XML node"
        );
        assert_eq!(
            XslError::incompatible_return_class("SimpleXMLElement", "DOMDocument").message(),
            "XSLTProcessor::transformToDoc(): Argument #2 ($returnClass) must be a class name compatible with SimpleXMLElement, DOMDocument given"
        );
        assert_eq!(
            XslError::unknown_return_class("NonExistent").message(),
            "XSLTProcessor::transformToDoc(): Argument #2 ($returnClass) must be a valid class name or null, \"NonExistent\" given"
        );
    }

    #[test]
    fn property_messages() {
        assert_eq!(
            XslError::negative_limit("maxTemplateDepth").message(),
            "XSLTProcessor::$maxTemplateDepth must be greater than or equal to 0"
        );
        assert_eq!(
            XslError::indirect_modification("maxTemplateVars").message(),
            "Indirect modification of XSLTProcessor::$maxTemplateVars is not allowed"
        );
        assert_eq!(
            XslError::cannot_unset("maxTemplateVars").to_string(),
            "Error: Cannot unset XSLTProcessor::$maxTemplateVars"
        );
    }
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Diagnostics (warnings, notices, …) produced by the support services.
//!
//! PHP reports problems either through `zend_error()` (plain message) or
//! through `php_error_docref()`, which prefixes the message with the name of
//! the currently executing internal function (`"ob_end_clean(): ..."`).  The
//! services in this crate do not know which userland function is active, so
//! they record whether the message is docref-style and let the VM render it.

use std::fmt;

/// A PHP error level (`E_*` constant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ErrorLevel(pub i32);

impl ErrorLevel {
    /// `E_ERROR` — fatal run-time error.
    pub const ERROR: Self = Self(1);
    /// `E_WARNING`.
    pub const WARNING: Self = Self(2);
    /// `E_PARSE`.
    pub const PARSE: Self = Self(4);
    /// `E_NOTICE`.
    pub const NOTICE: Self = Self(8);
    /// `E_CORE_ERROR`.
    pub const CORE_ERROR: Self = Self(16);
    /// `E_CORE_WARNING`.
    pub const CORE_WARNING: Self = Self(32);
    /// `E_COMPILE_ERROR`.
    pub const COMPILE_ERROR: Self = Self(64);
    /// `E_COMPILE_WARNING`.
    pub const COMPILE_WARNING: Self = Self(128);
    /// `E_USER_ERROR`.
    pub const USER_ERROR: Self = Self(256);
    /// `E_USER_WARNING`.
    pub const USER_WARNING: Self = Self(512);
    /// `E_USER_NOTICE`.
    pub const USER_NOTICE: Self = Self(1024);
    /// `E_STRICT` (no longer emitted by PHP 8 but still defined).
    pub const STRICT: Self = Self(2048);
    /// `E_RECOVERABLE_ERROR`.
    pub const RECOVERABLE_ERROR: Self = Self(4096);
    /// `E_DEPRECATED`.
    pub const DEPRECATED: Self = Self(8192);
    /// `E_USER_DEPRECATED`.
    pub const USER_DEPRECATED: Self = Self(16384);
    /// `E_ALL`.
    pub const ALL: Self = Self(30719);

    /// The label PHP prints in front of a displayed error (`"Warning"`,
    /// `"Fatal error"`, …), as in `php_error_cb()`.
    pub fn label(self) -> &'static str {
        match self.0 {
            1 | 16 | 64 | 256 => "Fatal error",
            4096 => "Recoverable fatal error",
            2 | 32 | 128 | 512 => "Warning",
            4 => "Parse error",
            8 | 1024 => "Notice",
            2048 => "Strict Standards",
            8192 | 16384 => "Deprecated",
            _ => "Unknown error",
        }
    }

    /// Whether PHP aborts the request (bails out) after reporting an error
    /// of this level.
    pub fn is_fatal(self) -> bool {
        matches!(self.0, 1 | 16 | 64 | 256)
    }
}

/// A diagnostic message raised by one of the support services.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Severity.
    pub level: ErrorLevel,
    /// Message text exactly as PHP formats it (without any function prefix).
    pub message: String,
    /// `true` when PHP raises it with `php_error_docref()`, i.e. the message
    /// is displayed as `"<active function>(): <message>"`.
    pub docref: bool,
}

impl Diagnostic {
    /// A `zend_error()`-style diagnostic (no function prefix).
    pub fn new(level: ErrorLevel, message: impl Into<String>) -> Self {
        Self {
            level,
            message: message.into(),
            docref: false,
        }
    }

    /// A `php_error_docref()`-style diagnostic (prefixed with the active
    /// function name when displayed).
    pub fn docref(level: ErrorLevel, message: impl Into<String>) -> Self {
        Self {
            level,
            message: message.into(),
            docref: true,
        }
    }

    /// Renders the message as PHP would pass it to the error callback, given
    /// the name of the currently executing internal function (if any).
    pub fn render(&self, active_function: Option<&str>) -> String {
        match (self.docref, active_function) {
            (true, Some(f)) => format!("{f}(): {}", self.message),
            _ => self.message.clone(),
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.level.label(), self.message)
    }
}

/// Something that accepts diagnostics.  Implemented for closures and for
/// `Vec<Diagnostic>` (which simply collects them).
pub trait DiagnosticSink {
    /// Reports one diagnostic.
    fn report(&mut self, diagnostic: Diagnostic);
}

impl DiagnosticSink for Vec<Diagnostic> {
    fn report(&mut self, diagnostic: Diagnostic) {
        self.push(diagnostic);
    }
}

impl<F: FnMut(Diagnostic)> DiagnosticSink for F {
    fn report(&mut self, diagnostic: Diagnostic) {
        self(diagnostic);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(ErrorLevel::WARNING.label(), "Warning");
        assert_eq!(ErrorLevel::NOTICE.label(), "Notice");
        assert_eq!(ErrorLevel::DEPRECATED.label(), "Deprecated");
        assert_eq!(ErrorLevel::ERROR.label(), "Fatal error");
        assert!(ErrorLevel::ERROR.is_fatal());
        assert!(!ErrorLevel::WARNING.is_fatal());
    }

    #[test]
    fn render_prefix() {
        let d = Diagnostic::docref(
            ErrorLevel::NOTICE,
            "Failed to delete buffer. No buffer to delete",
        );
        assert_eq!(
            d.render(Some("ob_end_clean")),
            "ob_end_clean(): Failed to delete buffer. No buffer to delete"
        );
        assert_eq!(
            d.render(None),
            "Failed to delete buffer. No buffer to delete"
        );
        let z = Diagnostic::new(ErrorLevel::WARNING, "x");
        assert_eq!(z.render(Some("ini_set")), "x");
        assert_eq!(z.to_string(), "Warning: x");
    }

    #[test]
    fn sinks() {
        let mut v: Vec<Diagnostic> = Vec::new();
        v.report(Diagnostic::new(ErrorLevel::NOTICE, "a"));
        assert_eq!(v.len(), 1);
        let mut n = 0;
        let mut f = |_d: Diagnostic| n += 1;
        f.report(Diagnostic::new(ErrorLevel::NOTICE, "b"));
        assert_eq!(n, 1);
    }
}

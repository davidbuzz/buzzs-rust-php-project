//! ICU status codes and PHP's intl error model.
//!
//! php-src records the *last error* of every intl call in a global
//! `intl_error` (code + optional message) and, for instance methods, also in
//! the object.  This crate is stateless in that respect: every fallible API
//! returns [`Error`], and the embedding VM performs the bookkeeping with
//! [`ErrorState`] and [`IntlSettings::notices`] exactly like
//! `intl_errors_set()` does:
//!
//! * [`Error::Intl`] — `intl_error_set(code, msg)`: store the code and the
//!   message prefixed with the active function name (`"Foo::bar(): msg"`),
//!   emit a diagnostic at `intl.error_level` and/or throw `IntlException`
//!   when `intl.use_exceptions` is on, then return `false` (or `null` for
//!   factories).  A message of `None` means only the code is recorded
//!   (`intl_error_set_code`), with no diagnostic.
//! * the other variants are engine exceptions thrown directly
//!   (`ValueError`, `TypeError`, `Error`, …) and leave the intl error alone.

use std::borrow::Cow;
use std::fmt::{self, Write as _};

/// ICU's `UErrorCode`.  Negative values are warnings, positive values
/// failures.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct UErrorCode(pub i32);

macro_rules! codes {
    ($($name:ident = $v:expr;)*) => {
        impl UErrorCode {
            $(
                #[doc = concat!("`U_", stringify!($name), "`")]
                pub const $name: UErrorCode = UErrorCode($v);
            )*
        }
    };
}

codes! {
    USING_FALLBACK_WARNING = -128;
    USING_DEFAULT_WARNING = -127;
    SAFECLONE_ALLOCATED_WARNING = -126;
    STATE_OLD_WARNING = -125;
    STRING_NOT_TERMINATED_WARNING = -124;
    SORT_KEY_TOO_SHORT_WARNING = -123;
    AMBIGUOUS_ALIAS_WARNING = -122;
    DIFFERENT_UCA_VERSION = -121;
    ZERO = 0;
    ILLEGAL_ARGUMENT_ERROR = 1;
    MISSING_RESOURCE_ERROR = 2;
    INVALID_FORMAT_ERROR = 3;
    FILE_ACCESS_ERROR = 4;
    INTERNAL_PROGRAM_ERROR = 5;
    MESSAGE_PARSE_ERROR = 6;
    MEMORY_ALLOCATION_ERROR = 7;
    INDEX_OUTOFBOUNDS_ERROR = 8;
    PARSE_ERROR = 9;
    INVALID_CHAR_FOUND = 10;
    TRUNCATED_CHAR_FOUND = 11;
    ILLEGAL_CHAR_FOUND = 12;
    INVALID_TABLE_FORMAT = 13;
    INVALID_TABLE_FILE = 14;
    BUFFER_OVERFLOW_ERROR = 15;
    UNSUPPORTED_ERROR = 16;
    RESOURCE_TYPE_MISMATCH = 17;
    ILLEGAL_ESCAPE_SEQUENCE = 18;
    UNSUPPORTED_ESCAPE_SEQUENCE = 19;
    NO_SPACE_AVAILABLE = 20;
    INVALID_STATE_ERROR = 27;
    MALFORMED_RULE = 0x10001;
    INVALID_ID = 0x10021;
    UNEXPECTED_TOKEN = 0x10100;
    PATTERN_SYNTAX_ERROR = 0x10107;
    UNMATCHED_BRACES = 0x10109;
    ARGUMENT_TYPE_MISMATCH = 0x1010C;
    DUPLICATE_KEYWORD = 0x1010D;
    UNDEFINED_KEYWORD = 0x1010E;
    DEFAULT_KEYWORD_MISSING = 0x1010F;
    DECIMAL_NUMBER_SYNTAX_ERROR = 0x10110;
    FORMAT_INEXACT_ERROR = 0x10111;
    NUMBER_ARG_OUTOFBOUNDS_ERROR = 0x10112;
    NUMBER_SKELETON_SYNTAX_ERROR = 0x10113;
    BRK_RULE_SYNTAX = 0x10203;
}

impl UErrorCode {
    /// `U_FAILURE(code)`.
    pub fn is_failure(self) -> bool {
        self.0 > 0
    }

    /// `U_SUCCESS(code)`.
    pub fn is_success(self) -> bool {
        self.0 <= 0
    }

    /// `u_errorName(code)` — e.g. `"U_ILLEGAL_ARGUMENT_ERROR"`.
    pub fn name(self) -> String {
        crate::ffi::error_name(self.0)
    }

    /// Converts to `Err(self)` when this is a failure code.
    pub fn check(self) -> Result<(), UErrorCode> {
        if self.is_failure() {
            Err(self)
        } else {
            Ok(())
        }
    }
}

impl fmt::Debug for UErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({})", self.name(), self.0)
    }
}

impl fmt::Display for UErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

/// Which `intl_error` slots an [`IntlError`] is recorded in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    /// Code and message go to both the object (if any) and the global
    /// error (`INTL_METHOD_CHECK_STATUS`, `intl_errors_set`).
    #[default]
    Both,
    /// The object gets code and message, the global error only the message
    /// (its code stays as reset, i.e. `U_ZERO_ERROR`).  This is what
    /// `intl_errors_set_custom_msg()` after an ICU call that wrote the
    /// object's status does.
    ObjectCode,
    /// Only the global error is set (`intl_error_set(NULL, …)`, used for
    /// argument checks performed before the object is fetched and by
    /// static methods).
    Global,
}

/// An intl-level error: what `intl_error_set()` / `intl_errors_set()`
/// records.  `message` is the unprefixed custom message; `None` means the
/// call only sets the code (no diagnostic is emitted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntlError {
    /// The ICU status recorded.
    pub code: UErrorCode,
    /// The custom message (without the `"func(): "` prefix).
    pub message: Option<Cow<'static, str>>,
    /// Where the error is recorded.
    pub scope: Scope,
    /// Recorded with `intl.use_exceptions` forced on and
    /// `intl.error_level` forced to 0: the VM always throws an
    /// `IntlException` with the prefixed message and never warns
    /// (`IntlNumberRangeFormatter`).
    pub always_throw: bool,
    /// The exact message bytes when they are not valid UTF-8 (user data
    /// quoted in the message); `message` then holds a lossy copy.
    pub raw_message: Option<Vec<u8>>,
}

impl IntlError {
    /// An error with a code and message.
    pub fn new(code: UErrorCode, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            code,
            message: Some(message.into()),
            scope: Scope::Both,
            always_throw: false,
            raw_message: None,
        }
    }

    /// An error that only sets the code.
    pub fn code_only(code: UErrorCode) -> Self {
        Self {
            code,
            message: None,
            scope: Scope::Both,
            always_throw: false,
            raw_message: None,
        }
    }

    /// Changes the scope.
    #[must_use]
    pub fn with_scope(mut self, scope: Scope) -> Self {
        self.scope = scope;
        self
    }

    /// An error whose message is built from raw bytes (see
    /// [`IntlError::raw_message`]).
    pub fn from_bytes(code: UErrorCode, message: Vec<u8>) -> Self {
        match String::from_utf8(message) {
            Ok(m) => Self::new(code, m),
            Err(e) => {
                let raw = e.into_bytes();
                let mut out = Self::new(code, String::from_utf8_lossy(&raw).into_owned());
                out.raw_message = Some(raw);
                out
            }
        }
    }

    /// The message as bytes (exact, even when not valid UTF-8).
    pub fn message_bytes(&self) -> Option<&[u8]> {
        self.raw_message
            .as_deref()
            .or_else(|| self.message.as_deref().map(str::as_bytes))
    }

    /// Marks the error as always throwing (see [`IntlError::always_throw`]).
    #[must_use]
    pub fn throwing(mut self) -> Self {
        self.always_throw = true;
        self
    }
}

/// Every way an intl API can fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// An intl error (see the module docs).
    Intl(IntlError),
    /// `zend_argument_value_error(arg, message)` → `ValueError`
    /// `"func(): Argument #<arg> ($name) <message>"`.  `arg` is numbered as
    /// in the *method* form; procedural aliases taking the object as first
    /// argument add one.
    ArgumentValue {
        /// 1-based argument position (method form).
        arg: u32,
        /// Text after the argument name.
        message: String,
    },
    /// `zend_argument_type_error(arg, message)` → `TypeError`.
    ArgumentType {
        /// 1-based argument position (method form).
        arg: u32,
        /// Text after the argument name.
        message: String,
    },
    /// `zend_value_error(message)` → `ValueError` without argument context
    /// (the VM prefixes nothing).
    Value(String),
    /// `zend_throw_error(NULL, message)` → plain `Error`.
    Thrown(String),
    /// `zend_argument_count_error(message)` → `ArgumentCountError`.
    ArgumentCount(String),
    /// `zend_throw_exception(IntlException, message)` (thrown directly,
    /// independent of `intl.use_exceptions`).
    IntlException(String),
    /// `IntlException` whose message the VM prefixes with the active
    /// function name: `"func(): message"`.
    IntlExceptionInFunction(String),
}

impl Error {
    /// Shorthand for an [`Error::Intl`] with a message.
    pub fn intl(code: UErrorCode, message: impl Into<Cow<'static, str>>) -> Self {
        Self::Intl(IntlError::new(code, message))
    }

    /// An [`Error::Intl`] recorded in the global error only.
    pub fn global(code: UErrorCode, message: impl Into<Cow<'static, str>>) -> Self {
        Self::Intl(IntlError::new(code, message).with_scope(Scope::Global))
    }

    /// An [`Error::Intl`] whose code is only stored in the object.
    pub fn object_code(code: UErrorCode, message: impl Into<Cow<'static, str>>) -> Self {
        Self::Intl(IntlError::new(code, message).with_scope(Scope::ObjectCode))
    }

    /// Shorthand for an [`Error::Intl`] that only records a code.
    pub fn code(code: UErrorCode) -> Self {
        Self::Intl(IntlError::code_only(code))
    }

    /// Shorthand for [`Error::ArgumentValue`].
    pub fn arg_value(arg: u32, message: impl Into<String>) -> Self {
        Self::ArgumentValue {
            arg,
            message: message.into(),
        }
    }

    /// Shorthand for [`Error::ArgumentType`].
    pub fn arg_type(arg: u32, message: impl Into<String>) -> Self {
        Self::ArgumentType {
            arg,
            message: message.into(),
        }
    }

    /// The intl error code carried (`U_ZERO_ERROR` for engine exceptions).
    pub fn intl_code(&self) -> UErrorCode {
        match self {
            Self::Intl(e) => e.code,
            _ => UErrorCode::ZERO,
        }
    }

    /// The intl message carried, if any.
    pub fn intl_message(&self) -> Option<&str> {
        match self {
            Self::Intl(e) => e.message.as_deref(),
            _ => None,
        }
    }

    /// The intl message as exact bytes (see [`IntlError::raw_message`]).
    pub fn intl_message_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Intl(e) => e.message_bytes(),
            _ => None,
        }
    }

    /// Shifts the argument number of argument errors by `by` (used for
    /// procedural aliases that take the object as first parameter).
    #[must_use]
    pub fn shift_arg(self, by: u32) -> Self {
        match self {
            Self::ArgumentValue { arg, message } => Self::ArgumentValue {
                arg: arg + by,
                message,
            },
            Self::ArgumentType { arg, message } => Self::ArgumentType {
                arg: arg + by,
                message,
            },
            other => other,
        }
    }
}

impl From<IntlError> for Error {
    fn from(e: IntlError) -> Self {
        Self::Intl(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Intl(e) => match &e.message {
                Some(m) => write!(f, "{m}: {}", e.code.name()),
                None => f.write_str(&e.code.name()),
            },
            Self::ArgumentValue { arg, message } | Self::ArgumentType { arg, message } => {
                write!(f, "Argument #{arg} {message}")
            }
            Self::Value(m)
            | Self::Thrown(m)
            | Self::ArgumentCount(m)
            | Self::IntlException(m)
            | Self::IntlExceptionInFunction(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

/// Result alias used throughout the crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Helper: turns a raw ICU status into an [`Error::Intl`] with `msg`.
pub(crate) trait StatusExt<T> {
    fn or_intl(self, msg: &'static str) -> Result<T>;
}

impl<T> StatusExt<T> for std::result::Result<T, UErrorCode> {
    fn or_intl(self, msg: &'static str) -> Result<T> {
        self.map_err(|c| Error::intl(c, msg))
    }
}

/// An `intl_error` slot (global or per object) as the VM keeps it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorState {
    /// Last error code.
    pub code: UErrorCode,
    /// Last custom message, already prefixed with the function name.
    pub message: Option<String>,
}

impl ErrorState {
    /// `intl_error_reset()`.
    pub fn reset(&mut self) {
        self.code = UErrorCode::ZERO;
        self.message = None;
    }

    /// `intl_error_set_code()`.
    pub fn set_code(&mut self, code: UErrorCode) {
        self.code = code;
    }

    /// `intl_error_set_custom_msg()` with the active function name
    /// (`None` when no function is executing).
    pub fn set_message(&mut self, function: Option<&str>, msg: &str) {
        self.message = Some(prefix_message(function, msg));
    }

    /// Records an [`IntlError`] like `intl_error_set()`.
    pub fn record(&mut self, function: Option<&str>, err: &IntlError) {
        self.code = err.code;
        if let Some(m) = &err.message {
            self.set_message(function, m);
        }
    }

    /// `intl_error_get_message()`: `"<custom>: U_NAME"` or `"U_NAME"`.
    pub fn get_message(&self) -> String {
        let name = self.code.name();
        match &self.message {
            Some(m) => format!("{m}: {name}"),
            None => name,
        }
    }
}

/// `"func(): msg"` — the prefix php-src adds to custom intl messages.
pub fn prefix_message(function: Option<&str>, msg: &str) -> String {
    match function {
        Some(f) => format!("{f}(): {msg}"),
        None => msg.to_owned(),
    }
}

/// The `intl.error_level` / `intl.use_exceptions` INI settings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IntlSettings {
    /// `intl.error_level` (an `E_*` level; 0 = silent).
    pub error_level: i64,
    /// `intl.use_exceptions`.
    pub use_exceptions: bool,
}

/// A diagnostic the VM must raise when a global intl error message is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// `php_error_docref(NULL, level, "%s", msg)` (the docref machinery
    /// prefixes the function name itself, so `message` is unprefixed).
    Diagnostic {
        /// The `E_*` level.
        level: i64,
        /// Unprefixed message.
        message: String,
    },
    /// Throw `IntlException` with this (prefixed) message.
    Exception(String),
}

impl IntlSettings {
    /// What `intl_error_set_custom_msg(NULL, msg)` emits.
    pub fn notices(&self, function: Option<&str>, msg: &str) -> Vec<Notice> {
        let mut out = Vec::new();
        if self.error_level != 0 {
            out.push(Notice::Diagnostic {
                level: self.error_level,
                message: msg.to_owned(),
            });
        }
        if self.use_exceptions {
            out.push(Notice::Exception(prefix_message(function, msg)));
        }
        out
    }
}

/// The deprecation php-src raises when `intl.error_level` is set to a
/// non-zero value.
pub const ERROR_LEVEL_DEPRECATION: &str = "Using a value different than 0 for intl.error_level is deprecated, as the intl.error_level INI setting is deprecated. Instead the intl.use_exceptions INI setting should be enabled to throw exceptions on errors or intl_get_error_code()/intl_get_error_message() should be used to manually deal with errors";

/// `intl_error_name()` — accepts any integer.
pub fn intl_error_name(code: i64) -> String {
    i32::try_from(code).map_or_else(
        |_| "[BOGUS UErrorCode]".to_owned(),
        |c| UErrorCode(c).name(),
    )
}

/// `intl_is_failure()`.
pub fn intl_is_failure(code: i64) -> bool {
    code > 0
}

/// `intl_parse_error_to_string()`: renders a `UParseError`.
pub fn parse_error_to_string(pe: &crate::ffi::UParseError) -> String {
    let mut ret = String::from("parse error ");
    let mut any = false;
    if pe.line > 0 {
        let _ = write!(ret, "on line {}", pe.line);
        any = true;
    }
    if pe.offset >= 0 {
        ret.push_str(if any { ", " } else { "at " });
        let _ = write!(ret, "offset {}", pe.offset);
        any = true;
    }
    let ctx = |c: &[u16; 16]| {
        let n = c.iter().position(|&u| u == 0).unwrap_or(16);
        String::from_utf16(&c[..n]).ok()
    };
    if pe.preContext[0] != 0 {
        if any {
            ret.push_str(", ");
        }
        ret.push_str("after \"");
        ret.push_str(
            &ctx(&pe.preContext).unwrap_or_else(|| {
                "(could not convert parser error pre-context to UTF-8)".to_owned()
            }),
        );
        ret.push('"');
        any = true;
    }
    if pe.postContext[0] != 0 {
        if any {
            ret.push_str(", ");
        }
        ret.push_str("before or at \"");
        ret.push_str(&ctx(&pe.postContext).unwrap_or_else(|| {
            "(could not convert parser error post-context to UTF-8)".to_owned()
        }));
        ret.push('"');
        any = true;
    }
    if !any {
        return "no parse error".to_owned();
    }
    ret
}

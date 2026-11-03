//! Exceptions and diagnostics raised by DOM operations.
//!
//! Operations return `Result<_, DomError>`: an `Err` corresponds to an
//! exception PHP would throw. Non-fatal diagnostics (`php_error_docref`
//! warnings and notices, deprecations) are delivered through
//! [`set_diagnostic_handler`] or queued for [`take_diagnostics`]; libxml2's
//! own messages go through `php_libxml::error` instead.

use std::any::Any;
use std::borrow::Cow;
use std::cell::RefCell;

/// `DOMException` codes (`dom_exception_code`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum DomExceptionCode {
    /// `DOM_PHP_ERR`.
    PhpErr = 0,
    /// `DOM_INDEX_SIZE_ERR`.
    IndexSize = 1,
    /// `DOMSTRING_SIZE_ERR`.
    DomStringSize = 2,
    /// `DOM_HIERARCHY_REQUEST_ERR`.
    HierarchyRequest = 3,
    /// `DOM_WRONG_DOCUMENT_ERR`.
    WrongDocument = 4,
    /// `DOM_INVALID_CHARACTER_ERR`.
    InvalidCharacter = 5,
    /// `DOM_NO_DATA_ALLOWED_ERR`.
    NoDataAllowed = 6,
    /// `DOM_NO_MODIFICATION_ALLOWED_ERR`.
    NoModificationAllowed = 7,
    /// `DOM_NOT_FOUND_ERR`.
    NotFound = 8,
    /// `DOM_NOT_SUPPORTED_ERR`.
    NotSupported = 9,
    /// `DOM_INUSE_ATTRIBUTE_ERR`.
    InuseAttribute = 10,
    /// `DOM_INVALID_STATE_ERR`.
    InvalidState = 11,
    /// `DOM_SYNTAX_ERR`.
    Syntax = 12,
    /// `DOM_INVALID_MODIFICATION_ERR`.
    InvalidModification = 13,
    /// `DOM_NAMESPACE_ERR`.
    Namespace = 14,
    /// `DOM_INVALID_ACCESS_ERR`.
    InvalidAccess = 15,
    /// `DOM_VALIDATION_ERR`.
    Validation = 16,
}

impl DomExceptionCode {
    /// The default message of `php_dom_throw_error`.
    pub fn message(self) -> &'static str {
        match self {
            Self::IndexSize => "Index Size Error",
            Self::DomStringSize => "DOM String Size Error",
            Self::HierarchyRequest => "Hierarchy Request Error",
            Self::WrongDocument => "Wrong Document Error",
            Self::InvalidCharacter => "Invalid Character Error",
            Self::NoDataAllowed => "No Data Allowed Error",
            Self::NoModificationAllowed => "No Modification Allowed Error",
            Self::NotFound => "Not Found Error",
            Self::NotSupported => "Not Supported Error",
            Self::InuseAttribute => "In Use Attribute Error",
            Self::InvalidState => "Invalid State Error",
            Self::Syntax => "Syntax Error",
            Self::InvalidModification => "Invalid Modification Error",
            Self::Namespace => "Namespace Error",
            Self::InvalidAccess => "Invalid Access Error",
            Self::Validation => "Validation Error",
            Self::PhpErr => "Unhandled Error",
        }
    }

    /// The numeric code.
    pub fn code(self) -> i64 {
        i64::from(self as u16)
    }
}

/// An exception raised by a DOM operation.
#[derive(Debug)]
pub enum DomError {
    /// `DOMException` with the given code and message.
    Dom {
        /// The `code` property.
        code: DomExceptionCode,
        /// The `message` property.
        message: Cow<'static, str>,
    },
    /// `TypeError` with the full message.
    Type(String),
    /// `ValueError` with the full message.
    Value(String),
    /// Plain `Error` with the full message.
    Error(String),
    /// Plain `Exception` with the full message (`zend_throw_exception_ex(NULL, …)`).
    Exception(String),
    /// `TypeError` about an argument: the VM prefixes
    /// `"Class::method(): Argument #N ($name) "`.
    ArgType {
        /// 1-based argument number.
        arg: u32,
        /// Message after the prefix (e.g. `"must be of type string, int given"`).
        message: String,
    },
    /// `ValueError` about an argument (same prefixing as `ArgType`).
    ArgValue {
        /// 1-based argument number.
        arg: u32,
        /// Message after the prefix (e.g. `"must not be empty"`).
        message: String,
    },
    /// `Error("Couldn't fetch <class>")`: the object is not bound to a node.
    CouldntFetch,
    /// An exception thrown by user code called back from DOM (`XPath` PHP
    /// functions, node class constructors); the payload is the embedder's.
    Host(Box<dyn Any>),
    /// No exception: a warning was already emitted (non-strict error
    /// checking, or a `php_error_docref` warning) and the PHP method
    /// returns `false`.
    Silent,
}

impl DomError {
    /// A `DOMException` with the default message for `code`.
    pub fn dom(code: DomExceptionCode) -> Self {
        Self::Dom {
            code,
            message: Cow::Borrowed(code.message()),
        }
    }

    /// A `DOMException` with a custom message.
    pub fn dom_msg(code: DomExceptionCode, message: impl Into<Cow<'static, str>>) -> Self {
        Self::Dom {
            code,
            message: message.into(),
        }
    }

    /// `zend_argument_value_error`.
    pub fn arg_value(arg: u32, message: impl Into<String>) -> Self {
        Self::ArgValue {
            arg,
            message: message.into(),
        }
    }

    /// `zend_argument_type_error`.
    pub fn arg_type(arg: u32, message: impl Into<String>) -> Self {
        Self::ArgType {
            arg,
            message: message.into(),
        }
    }

    /// The `DOMException` code, if this is one.
    pub fn dom_code(&self) -> Option<DomExceptionCode> {
        match self {
            Self::Dom { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// The message (prefix-less for argument errors).
    pub fn message(&self) -> String {
        match self {
            Self::Dom { message, .. } => message.to_string(),
            Self::Type(m) | Self::Value(m) | Self::Error(m) | Self::Exception(m) => m.clone(),
            Self::ArgType { message, .. } | Self::ArgValue { message, .. } => message.clone(),
            Self::CouldntFetch => "Couldn't fetch".into(),
            Self::Host(_) => "<host exception>".into(),
            Self::Silent => String::new(),
        }
    }
}

impl std::fmt::Display for DomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for DomError {}

/// Result alias.
pub type DomResult<T> = Result<T, DomError>;

/// Converts [`DomError::Silent`] into `Ok(None)` (PHP's `RETURN_FALSE`
/// after a warning).
pub trait SilentExt<T> {
    /// `Err(Silent)` → `Ok(None)`.
    fn or_false(self) -> DomResult<Option<T>>;
}

impl<T> SilentExt<T> for DomResult<T> {
    fn or_false(self) -> DomResult<Option<T>> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(DomError::Silent) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Severity of a DOM diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// `E_WARNING`.
    Warning,
    /// `E_NOTICE`.
    Notice,
    /// `E_DEPRECATED`.
    Deprecated,
}

/// A diagnostic PHP would display (without the `"func(): "` prefix for
/// warnings/notices; deprecations are displayed verbatim by `zend_error`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Severity.
    pub level: Level,
    /// Message text.
    pub message: String,
}

type Handler = Box<dyn FnMut(Diagnostic)>;

thread_local! {
    static QUEUE: RefCell<Vec<Diagnostic>> = const { RefCell::new(Vec::new()) };
    static HANDLER: RefCell<Option<Handler>> = const { RefCell::new(None) };
    static SUPPRESS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Installs the diagnostics handler; returns the previous one.
pub fn set_diagnostic_handler(handler: Option<Handler>) -> Option<Handler> {
    HANDLER.with(|h| std::mem::replace(&mut *h.borrow_mut(), handler))
}

/// Drains diagnostics queued while no handler was installed.
pub fn take_diagnostics() -> Vec<Diagnostic> {
    QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

pub(crate) fn emit(level: Level, message: impl Into<String>) {
    let d = Diagnostic {
        level,
        message: message.into(),
    };
    let handler = HANDLER.with(|h| h.borrow_mut().take());
    match handler {
        Some(mut h) => {
            h(d);
            HANDLER.with(|slot| {
                let mut slot = slot.borrow_mut();
                if slot.is_none() {
                    *slot = Some(h);
                }
            });
        }
        None => QUEUE.with(|q| q.borrow_mut().push(d)),
    }
}

/// `php_error_docref(NULL, E_WARNING, ...)`.
pub(crate) fn warning(message: impl Into<String>) {
    emit(Level::Warning, message);
}

/// `php_error_docref(NULL, E_NOTICE, ...)`.
pub(crate) fn notice(message: impl Into<String>) {
    emit(Level::Notice, message);
}

/// `PHP_DOM_DEPRECATED_PROPERTY`: suppressed while building debug info.
pub(crate) fn deprecated(message: impl Into<String>) {
    if !SUPPRESS.with(std::cell::Cell::get) {
        emit(Level::Deprecated, message);
    }
}

/// Runs `f` with deprecation warnings suppressed (`DOM_G(suppress_warnings)`).
pub(crate) fn suppressed<R>(f: impl FnOnce() -> R) -> R {
    let old = SUPPRESS.with(|s| s.replace(true));
    let r = f();
    SUPPRESS.with(|s| s.set(old));
    r
}

/// `php_dom_throw_error_with_message`: an exception when `strict`,
/// otherwise a libxml-style warning (`php_libxml_issue_error`).
pub(crate) fn throw(code: DomExceptionCode, strict: bool) -> DomError {
    throw_msg(code, code.message(), strict)
}

/// `php_dom_throw_error_with_message`.
pub(crate) fn throw_msg(code: DomExceptionCode, message: &'static str, strict: bool) -> DomError {
    if strict {
        DomError::dom_msg(code, message)
    } else {
        php_libxml::error::issue_error(
            php_libxml::error::DiagnosticLevel::Warning,
            message.as_bytes(),
        );
        DomError::Silent
    }
}

//! Diagnostics (warnings and notices) raised by the streams layer.
//!
//! PHP reports stream problems through `php_error_docref()` and the
//! master-branch stream error API (`main/streams/stream_errors.c`). Each
//! report here becomes a [`Diagnostic`] that is handed to a thread-local
//! handler installed by the VM ([`set_handler`]); without a handler,
//! diagnostics are queued and can be collected with [`take`].
//!
//! Messages never include the `function(): ` prefix — `php_error_docref()`
//! derives it from the currently executing internal function, which only the
//! VM knows. When PHP uses `php_error_docref1()` / `php_error_docref2()` the
//! extra parameters shown inside the parentheses are carried in
//! [`Diagnostic::params`].

use std::cell::RefCell;
use std::fmt;

/// The PHP error level of a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    /// `E_WARNING`.
    Warning,
    /// `E_NOTICE`.
    Notice,
    /// `E_DEPRECATED`.
    Deprecated,
    /// `E_ERROR` (fatal).
    Error,
}

impl Severity {
    /// The `E_*` constant value.
    pub fn level(self) -> i32 {
        match self {
            Severity::Error => 1,
            Severity::Warning => 2,
            Severity::Notice => 8,
            Severity::Deprecated => 8192,
        }
    }
}

macro_rules! error_codes {
    ($($name:ident),* $(,)?) => {
        /// `StreamErrorCode` cases (`main/streams/stream_errors.stub.php`).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum StreamErrorCode { $($name),* }

        impl StreamErrorCode {
            /// All case names, in declaration order (the enum's case ids
            /// are the 1-based positions).
            pub const NAMES: &'static [&'static str] = &[$(stringify!($name)),*];

            /// The enum case name.
            pub fn name(self) -> &'static str {
                match self { $(StreamErrorCode::$name => stringify!($name)),* }
            }
        }
    };
}

error_codes!(
    None,
    Generic,
    ReadFailed,
    WriteFailed,
    SeekFailed,
    SeekNotSupported,
    FlushFailed,
    TruncateFailed,
    ConnectFailed,
    BindFailed,
    ListenFailed,
    AcceptFailed,
    NotWritable,
    NotReadable,
    Disabled,
    NotFound,
    PermissionDenied,
    AlreadyExists,
    InvalidPath,
    PathTooLong,
    OpenFailed,
    CreateFailed,
    DupFailed,
    UnlinkFailed,
    RenameFailed,
    MkdirFailed,
    RmdirFailed,
    StatFailed,
    MetaFailed,
    ChmodFailed,
    ChownFailed,
    CopyFailed,
    TouchFailed,
    InvalidMode,
    InvalidMeta,
    ModeNotSupported,
    Readonly,
    RecursionDetected,
    NotImplemented,
    NoOpener,
    PersistentNotSupported,
    WrapperNotFound,
    WrapperDisabled,
    ProtocolUnsupported,
    WrapperRegistrationFailed,
    WrapperUnregistrationFailed,
    WrapperRestorationFailed,
    FilterNotFound,
    FilterFailed,
    CastFailed,
    CastNotSupported,
    MakeSeekableFailed,
    BufferedDataLost,
    NetworkSendFailed,
    NetworkRecvFailed,
    SslNotSupported,
    ResumptionFailed,
    SocketPathTooLong,
    OobNotSupported,
    ProtocolError,
    InvalidUrl,
    InvalidResponse,
    InvalidHeader,
    InvalidParam,
    RedirectLimit,
    AuthFailed,
    TimeOut,
    ArchivingFailed,
    EncodingFailed,
    DecodingFailed,
    InvalidFormat,
    AllocationFailed,
    TemporaryFileFailed,
    LockFailed,
    LockNotSupported,
    UserspaceNotImplemented,
    UserspaceInvalidReturn,
    UserspaceCallFailed,
);

/// One warning/notice raised by the streams layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Error level.
    pub severity: Severity,
    /// Stream error code (master `StreamErrorCode`); [`StreamErrorCode::None`]
    /// for plain `php_error_docref()` reports.
    pub code: StreamErrorCode,
    /// The wrapper label (`"plainfile"`, `"PHP"`, `"user-space"`, …) or
    /// `":na"` / `""` when not applicable.
    pub wrapper: String,
    /// Whether the error terminates the operation (stream error API).
    pub terminating: bool,
    /// Extra docref parameters shown as `function(p1,p2): ` by
    /// `php_error_docref1/2()`; empty for plain `php_error_docref()`.
    pub params: Vec<Vec<u8>>,
    /// The message text (binary safe: paths may be arbitrary bytes).
    pub message: Vec<u8>,
}

impl Diagnostic {
    /// A plain `php_error_docref()` diagnostic.
    pub fn new(severity: Severity, message: impl Into<Vec<u8>>) -> Self {
        Diagnostic {
            severity,
            code: StreamErrorCode::None,
            wrapper: String::new(),
            terminating: severity == Severity::Warning,
            params: Vec::new(),
            message: message.into(),
        }
    }

    /// Sets the stream error code.
    #[must_use]
    pub fn with_code(mut self, code: StreamErrorCode) -> Self {
        self.code = code;
        self
    }

    /// Sets the wrapper label.
    #[must_use]
    pub fn with_wrapper(mut self, wrapper: impl Into<String>) -> Self {
        self.wrapper = wrapper.into();
        self
    }

    /// Sets the terminating flag.
    #[must_use]
    pub fn terminating(mut self, terminating: bool) -> Self {
        self.terminating = terminating;
        self
    }

    /// Adds a docref parameter (`php_error_docref1/2`).
    #[must_use]
    pub fn with_param(mut self, param: impl Into<Vec<u8>>) -> Self {
        self.params.push(param.into());
        self
    }

    /// The message as (lossy) UTF-8, for tests and debugging.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.message).into_owned()
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let level = match self.severity {
            Severity::Warning => "Warning",
            Severity::Notice => "Notice",
            Severity::Deprecated => "Deprecated",
            Severity::Error => "Fatal error",
        };
        write!(f, "{level}: {}", self.text())
    }
}

/// A diagnostics sink installed by the VM.
pub type Handler = Box<dyn FnMut(Diagnostic)>;

thread_local! {
    static HANDLER: RefCell<Option<Handler>> = const { RefCell::new(None) };
    static QUEUE: RefCell<Vec<Diagnostic>> = const { RefCell::new(Vec::new()) };
}

/// Installs the diagnostics handler for this thread, returning the previous
/// one. Pass `None` to queue diagnostics instead.
pub fn set_handler(handler: Option<Handler>) -> Option<Handler> {
    HANDLER.with(|h| std::mem::replace(&mut *h.borrow_mut(), handler))
}

/// Reports a diagnostic.
///
/// The handler is taken out of its slot while it runs, so a handler that
/// re-enters the streams layer (e.g. a user error handler doing file I/O)
/// queues nested diagnostics instead of deadlocking.
pub fn emit(diag: Diagnostic) {
    let handler = HANDLER.with(|h| h.borrow_mut().take());
    if let Some(mut handler) = handler {
        handler(diag);
        HANDLER.with(|h| {
            let mut slot = h.borrow_mut();
            if slot.is_none() {
                *slot = Some(handler);
            }
        });
    } else {
        QUEUE.with(|q| q.borrow_mut().push(diag));
    }
}

/// Takes all queued diagnostics.
pub fn take() -> Vec<Diagnostic> {
    QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// Puts a diagnostic back at the front of the queue (an enclosing
/// operation's error kept while a nested one reports its own).
pub fn push_front(diag: Diagnostic) {
    QUEUE.with(|q| q.borrow_mut().insert(0, diag));
}

/// Emits an `E_WARNING` via plain `php_error_docref()`.
pub fn warning(message: impl Into<Vec<u8>>) {
    emit(Diagnostic::new(Severity::Warning, message));
}

/// Emits an `E_NOTICE` via plain `php_error_docref()`.
pub fn notice(message: impl Into<Vec<u8>>) {
    emit(Diagnostic::new(Severity::Notice, message).terminating(false));
}

/// Emits a wrapper/stream error (`php_stream_wrapper_warn` family).
pub fn stream_error(
    severity: Severity,
    wrapper: &str,
    code: StreamErrorCode,
    terminating: bool,
    message: impl Into<Vec<u8>>,
) {
    emit(
        Diagnostic::new(severity, message)
            .with_code(code)
            .with_wrapper(wrapper)
            .terminating(terminating),
    );
}

/// Concatenates byte-string pieces into a message.
pub(crate) fn msg(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn queue_and_handler() {
        let _ = take();
        warning("a");
        notice("b");
        let got = take();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].text(), "a");
        assert_eq!(got[1].severity, Severity::Notice);

        let seen = Rc::new(RefCell::new(Vec::new()));
        let s2 = Rc::clone(&seen);
        set_handler(Some(Box::new(move |d| {
            // nested emits are queued
            if d.text() == "outer" {
                warning("inner");
            }
            s2.borrow_mut().push(d.text());
        })));
        warning("outer");
        set_handler(None);
        assert_eq!(&*seen.borrow(), &["outer".to_string()]);
        assert_eq!(take()[0].text(), "inner");
    }

    #[test]
    fn code_names() {
        assert_eq!(StreamErrorCode::OpenFailed.name(), "OpenFailed");
        assert_eq!(Severity::Warning.level(), 2);
    }
}

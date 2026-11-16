// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! PHP's libxml error model (`ext/libxml/libxml.c`).
//!
//! * With `libxml_use_internal_errors(true)` every libxml2 error is copied
//!   into a per-thread list ([`get_errors`], [`get_last_error`],
//!   [`clear_errors`]) and nothing is displayed.
//! * Otherwise an error is displayed the way the handler libxml2 would have
//!   chosen in PHP does it: parser contexts registered with
//!   [`ParserChannels::CTX`](crate::raw::errors::ParserChannels::CTX)
//!   produce `"<msg> in Entity, line: N"` warnings (notices for libxml
//!   warnings), errors outside a parser produce plain `"<msg>"` warnings,
//!   and ext/xml parsers stay silent.
//!
//! Displayed messages are delivered as [`Diagnostic`]s: to the handler
//! installed with [`set_diagnostic_handler`] when there is one, otherwise
//! queued for [`take_diagnostics`]. The VM prefixes them with the active
//! function name (`"DOMDocument::loadXML(): "`) as `php_error_docref`
//! does, and must drop them while an exception is pending (PHP checks
//! `EG(exception)` before reporting).
//!
//! Like PHP's `error_buffer`, message fragments that do not end in a
//! newline are accumulated and reported together with the next fragment
//! that does.

use std::cell::RefCell;

use crate::ffi;
use crate::raw;
use crate::raw::errors::ParserChannels;

/// Error levels (`LIBXML_ERR_*`).
pub mod level {
    /// `LIBXML_ERR_NONE`.
    pub const NONE: i32 = 0;
    /// `LIBXML_ERR_WARNING`.
    pub const WARNING: i32 = 1;
    /// `LIBXML_ERR_ERROR`.
    pub const ERROR: i32 = 2;
    /// `LIBXML_ERR_FATAL`.
    pub const FATAL: i32 = 3;
}

/// The data of a `LibXMLError` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibXmlError {
    /// libxml2 error domain (`XML_FROM_*`; not exposed by PHP).
    pub domain: i32,
    /// `LIBXML_ERR_WARNING`, `LIBXML_ERR_ERROR` or `LIBXML_ERR_FATAL`.
    pub level: i32,
    /// libxml2 error code (`xmlParserErrors`).
    pub code: i32,
    /// Column (libxml2's `int2`).
    pub column: i32,
    /// Message, usually ending in `"\n"`.
    pub message: Vec<u8>,
    /// File name; PHP exposes `None` as `""`.
    pub file: Option<Vec<u8>>,
    /// Line number.
    pub line: i32,
}

impl LibXmlError {
    /// The `file` property as PHP exposes it (`""` when unknown).
    pub fn file_or_empty(&self) -> &[u8] {
        self.file.as_deref().unwrap_or_default()
    }

    /// The `(name, value)` pairs of the `LibXMLError` object, in property
    /// order: level, code, column, message, file, line.
    pub fn properties(&self) -> [(&'static str, ErrorProperty<'_>); 6] {
        [
            ("level", ErrorProperty::Int(i64::from(self.level))),
            ("code", ErrorProperty::Int(i64::from(self.code))),
            ("column", ErrorProperty::Int(i64::from(self.column))),
            ("message", ErrorProperty::Str(&self.message)),
            ("file", ErrorProperty::Str(self.file_or_empty())),
            ("line", ErrorProperty::Int(i64::from(self.line))),
        ]
    }

    fn internal(message: Vec<u8>, line: i32, column: i32) -> Self {
        Self {
            domain: ffi::XML_FROM_NONE,
            level: level::ERROR,
            code: ffi::XML_ERR_INTERNAL_ERROR,
            column,
            message,
            file: None,
            line,
        }
    }
}

/// A property value of a `LibXMLError` object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorProperty<'a> {
    /// An `int` property.
    Int(i64),
    /// A `string` property.
    Str(&'a [u8]),
}

/// Severity of a displayed diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticLevel {
    /// `E_WARNING`.
    Warning,
    /// `E_NOTICE` (libxml warnings reported through a parser context).
    Notice,
}

/// A message PHP would display through `php_error_docref`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Severity.
    pub level: DiagnosticLevel,
    /// Message text without the `"function(): "` prefix.
    pub message: Vec<u8>,
}

/// The reporting channels PHP installs (`php_libxml_error_level`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// `php_libxml_error_handler`: `E_WARNING "%s"`.
    Generic,
    /// `php_libxml_ctx_error`: `E_WARNING "%s in Entity, line: %d"`.
    CtxError,
    /// `php_libxml_ctx_warning`: `E_NOTICE "%s in Entity, line: %d"`.
    CtxWarning,
}

/// Input position of a parser context at the time of an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParserPosition {
    /// `ctxt->input->filename`.
    pub filename: Option<Vec<u8>>,
    /// `ctxt->input->line`.
    pub line: i32,
    /// `ctxt->input->col`.
    pub column: i32,
}

/// Where a structured error came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ErrorOrigin {
    /// A parser-family error raised with a parser context.
    Parser {
        channels: ParserChannels,
        position: Option<ParserPosition>,
    },
    /// A parser context created internally by libxml2 (default SAX
    /// handlers): the pieces `xmlReportError` writes to the generic channel.
    Report(Vec<Vec<u8>>),
    /// Anything else (I/O, writer, tree, encoding…).
    Other,
}

type DiagnosticHandler = Box<dyn FnMut(Diagnostic)>;

#[derive(Default)]
struct State {
    /// `LIBXML(error_list)`: present iff internal errors are enabled.
    list: Option<Vec<LibXmlError>>,
    /// `LIBXML(error_buffer)`.
    buffer: Vec<u8>,
    /// Queued diagnostics (when no handler is installed).
    queue: Vec<Diagnostic>,
    handler: Option<DiagnosticHandler>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// Installs (or removes) the handler receiving displayed diagnostics.
/// Returns the previous handler.
pub fn set_diagnostic_handler(
    handler: Option<Box<dyn FnMut(Diagnostic)>>,
) -> Option<Box<dyn FnMut(Diagnostic)>> {
    STATE.with(|s| std::mem::replace(&mut s.borrow_mut().handler, handler))
}

/// Drains diagnostics queued while no handler was installed.
pub fn take_diagnostics() -> Vec<Diagnostic> {
    STATE.with(|s| std::mem::take(&mut s.borrow_mut().queue))
}

/// Emits a diagnostic to the handler (outside of any `STATE` borrow, so
/// the handler may call back into this module) or queues it.
pub(crate) fn emit(diagnostic: Diagnostic) {
    let handler = STATE.with(|s| s.borrow_mut().handler.take());
    match handler {
        Some(mut h) => {
            h(diagnostic);
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                if s.handler.is_none() {
                    s.handler = Some(h);
                }
            });
        }
        None => STATE.with(|s| s.borrow_mut().queue.push(diagnostic)),
    }
}

/// `libxml_use_internal_errors()`: with `None` only queries; returns the
/// previous setting.
pub fn use_internal_errors(enable: Option<bool>) -> bool {
    raw::ensure_init();
    let previous = uses_internal_errors();
    match enable {
        None => {}
        Some(false) => {
            let had_list = STATE.with(|s| s.borrow_mut().list.take().is_some());
            if had_list {
                // libxml_get_last_error() falls back to libxml's own last
                // error once the list is gone.
                raw::errors::reset_last_error();
            }
        }
        Some(true) => STATE.with(|s| {
            let mut s = s.borrow_mut();
            if s.list.is_none() {
                s.list = Some(Vec::new());
            }
        }),
    }
    previous
}

/// `php_libxml_uses_internal_errors()`.
pub fn uses_internal_errors() -> bool {
    STATE.with(|s| s.borrow().list.is_some())
}

/// `libxml_get_errors()`.
pub fn get_errors() -> Vec<LibXmlError> {
    STATE.with(|s| s.borrow().list.clone().unwrap_or_default())
}

/// `libxml_get_last_error()`.
pub fn get_last_error() -> Option<LibXmlError> {
    let from_list = STATE.with(|s| s.borrow().list.as_ref().map(|l| l.last().cloned()));
    match from_list {
        Some(last) => last,
        None => raw::errors::last_error(),
    }
}

/// `libxml_clear_errors()`.
pub fn clear_errors() {
    raw::errors::reset_last_error();
    STATE.with(|s| {
        if let Some(list) = s.borrow_mut().list.as_mut() {
            list.clear();
        }
    });
}

/// Request shutdown (`php_libxml_post_deactivate` + RINIT/RSHUTDOWN):
/// disables internal errors, drops the error buffer and libxml's last
/// error, the entity loader callback, the stream context and re-enables
/// the entity loader.
pub fn reset_request_state() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.list = None;
        s.buffer.clear();
        s.queue.clear();
    });
    raw::errors::reset_last_error();
    crate::io::reset_request_state();
}

/// `php_libxml_issue_error(level, msg)`: appended to the error list when
/// internal errors are on, displayed otherwise.
pub fn issue_error(level: DiagnosticLevel, message: &[u8]) {
    let pushed = STATE.with(|s| {
        let mut s = s.borrow_mut();
        match s.list.as_mut() {
            Some(list) => {
                list.push(LibXmlError::internal(message.to_vec(), 0, 0));
                true
            }
            None => false,
        }
    });
    if !pushed {
        emit(Diagnostic {
            level,
            message: message.to_vec(),
        });
    }
}

/// `php_libxml_ctx_error(ctx, msg)` for a parser at `position`.
pub fn ctx_error(position: Option<&ParserPosition>, message: &[u8]) {
    internal_error_handler(Channel::CtxError, position, message);
}

/// `php_libxml_ctx_warning(ctx, msg)` for a parser at `position`.
pub fn ctx_warning(position: Option<&ParserPosition>, message: &[u8]) {
    internal_error_handler(Channel::CtxWarning, position, message);
}

/// `php_libxml_pretend_ctx_error_ex(file, line, column, msg)`: a context
/// error for a parser that is not libxml2's (e.g. the HTML5 parser).
pub fn pretend_ctx_error(file: &[u8], line: i32, column: i32, message: &[u8]) {
    internal_error_handler_ex(Channel::CtxError, None, message, line, column);
    STATE.with(|s| {
        if let Some(last) = s.borrow_mut().list.as_mut().and_then(|l| l.last_mut()) {
            if last.file.is_none() {
                last.file = Some(file.to_vec());
            }
        }
    });
}

/// `php_libxml_error_handler`: text libxml2 writes to its generic error
/// channel (`xmlGenericError`), buffered until a newline, then appended to
/// the error list (internal errors on) or emitted as a warning.
pub(crate) fn generic_error(message: &[u8]) {
    internal_error_handler(Channel::Generic, None, message);
}

/// `php_libxml_error_handler_va`: line/column come from the parser
/// position for context channels.
fn internal_error_handler(channel: Channel, position: Option<&ParserPosition>, message: &[u8]) {
    let (line, column) = match (channel, position) {
        (Channel::CtxError | Channel::CtxWarning, Some(p)) => (p.line, p.column),
        _ => (0, 0),
    };
    internal_error_handler_ex(channel, position, message, line, column);
}

/// `php_libxml_internal_error_handler_ex`.
fn internal_error_handler_ex(
    channel: Channel,
    position: Option<&ParserPosition>,
    message: &[u8],
    line: i32,
    column: i32,
) {
    let mut len = message.len();
    let mut output = false;
    while len > 0 && message[len - 1] == b'\n' {
        len -= 1;
        output = true;
    }
    let complete = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.buffer.extend_from_slice(&message[..len]);
        if !output {
            return None;
        }
        let text = std::mem::take(&mut s.buffer);
        if let Some(list) = s.list.as_mut() {
            list.push(LibXmlError::internal(text, line, column));
            return None;
        }
        Some(text)
    });
    let Some(text) = complete else { return };
    let diagnostic = match channel {
        Channel::CtxError => Diagnostic {
            level: DiagnosticLevel::Warning,
            message: ctx_format(&text, position, line),
        },
        Channel::CtxWarning => Diagnostic {
            level: DiagnosticLevel::Notice,
            message: ctx_format(&text, position, line),
        },
        Channel::Generic => Diagnostic {
            level: DiagnosticLevel::Warning,
            message: text,
        },
    };
    emit(diagnostic);
}

/// `php_libxml_ctx_error_level`'s message format.
fn ctx_format(text: &[u8], position: Option<&ParserPosition>, line: i32) -> Vec<u8> {
    let mut out = text.to_vec();
    if let Some(p) = position {
        match &p.filename {
            Some(f) => {
                out.extend_from_slice(b" in ");
                out.extend_from_slice(f);
                out.extend_from_slice(format!(", line: {line}").as_bytes());
            }
            None => out.extend_from_slice(format!(" in Entity, line: {line}").as_bytes()),
        }
    }
    out
}

/// Entry point of the structured error trampoline.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn dispatch(error: LibXmlError, origin: ErrorOrigin) {
    let stored = STATE.with(|s| {
        let mut s = s.borrow_mut();
        match s.list.as_mut() {
            Some(list) => {
                list.push(error.clone());
                true
            }
            None => false,
        }
    });
    if stored {
        return;
    }
    match origin {
        ErrorOrigin::Parser { channels, position } => {
            let channel = if error.level == level::WARNING {
                channels.warning
            } else {
                channels.error
            };
            if let Some(channel) = channel {
                internal_error_handler(channel, position.as_ref(), &error.message);
            }
        }
        ErrorOrigin::Report(segments) => {
            for segment in segments {
                internal_error_handler(Channel::Generic, None, &segment);
            }
        }
        ErrorOrigin::Other => internal_error_handler(Channel::Generic, None, &error.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset() {
        reset_request_state();
        set_diagnostic_handler(None);
        take_diagnostics();
    }

    #[test]
    fn use_internal_errors_round_trip() {
        reset();
        // ext/libxml/tests/001.phpt
        assert!(!use_internal_errors(Some(false)));
        assert!(!use_internal_errors(Some(true)));
        assert!(use_internal_errors(None));
        assert!(get_errors().is_empty());
        assert_eq!(get_last_error(), None);
        clear_errors();
        reset();
    }

    #[test]
    fn issue_error_goes_to_list_or_display() {
        reset();
        issue_error(DiagnosticLevel::Warning, b"boom");
        assert_eq!(
            take_diagnostics(),
            vec![Diagnostic {
                level: DiagnosticLevel::Warning,
                message: b"boom".to_vec()
            }]
        );
        use_internal_errors(Some(true));
        issue_error(DiagnosticLevel::Warning, b"boom");
        let errors = get_errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, 1);
        assert_eq!(errors[0].level, 2);
        assert_eq!(errors[0].file_or_empty(), b"");
        assert!(take_diagnostics().is_empty());
        reset();
    }

    #[test]
    fn ctx_messages_are_buffered_until_newline() {
        reset();
        let pos = ParserPosition {
            filename: None,
            line: 4,
            column: 2,
        };
        ctx_error(
            Some(&pos),
            b"Call to user entity loader callback 'f' has failed",
        );
        assert!(take_diagnostics().is_empty());
        ctx_error(Some(&pos), b"Failed to load external entity \"x\"\n");
        assert_eq!(
            take_diagnostics(),
            vec![Diagnostic {
                level: DiagnosticLevel::Warning,
                message: b"Call to user entity loader callback 'f' has failedFailed to load external entity \"x\" in Entity, line: 4"
                    .to_vec()
            }]
        );
        let file_pos = ParserPosition {
            filename: Some(b"/tmp/a.xml".to_vec()),
            line: 7,
            column: 0,
        };
        ctx_warning(Some(&file_pos), b"careful\n");
        assert_eq!(
            take_diagnostics(),
            vec![Diagnostic {
                level: DiagnosticLevel::Notice,
                message: b"careful in /tmp/a.xml, line: 7".to_vec()
            }]
        );
        reset();
    }

    #[test]
    fn handler_receives_diagnostics_and_may_reenter() {
        reset();
        let seen = std::rc::Rc::new(RefCell::new(Vec::new()));
        let sink = seen.clone();
        set_diagnostic_handler(Some(Box::new(move |d: Diagnostic| {
            // Re-entering the module from a handler must not deadlock.
            let _ = uses_internal_errors();
            sink.borrow_mut().push(d.message);
        })));
        issue_error(DiagnosticLevel::Warning, b"x");
        assert_eq!(seen.borrow().as_slice(), &[b"x".to_vec()]);
        reset();
    }

    #[test]
    fn pretend_ctx_error_sets_file() {
        reset();
        use_internal_errors(Some(true));
        pretend_ctx_error(b"doc.html", 3, 9, b"tokenizer error\n");
        let e = get_last_error().expect("error recorded");
        assert_eq!(e.file.as_deref(), Some(&b"doc.html"[..]));
        assert_eq!((e.line, e.column), (3, 9));
        assert_eq!(e.message, b"tokenizer error");
        reset();
    }
}

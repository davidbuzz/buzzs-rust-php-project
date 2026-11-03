//! The per-thread structured error handler and the parser-context
//! registry.
//!
//! PHP decides how a libxml2 error is reported from which handler libxml2
//! picks: the structured handler when `libxml_use_internal_errors(true)`
//! is active, otherwise the SAX `error`/`warning` slot of the parser
//! context (DOM installs `php_libxml_ctx_error`, ext/xml leaves it NULL),
//! otherwise the generic handler (`php_libxml_error_handler`). We cannot
//! install variadic C callbacks from Rust, so this crate always installs a
//! structured handler and replays PHP's choice in
//! [`crate::error::dispatch`] using the channels each live parser context
//! was registered with.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;

use super::{guard, opt_bytes};
use crate::error::{dispatch, Channel, ErrorOrigin, LibXmlError, ParserPosition};
use crate::ffi;

/// How a parser context reports errors when internal errors are off:
/// the equivalents of `ctxt->sax->error` and `ctxt->sax->warning`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ParserChannels {
    /// Channel for `XML_ERR_ERROR`/`XML_ERR_FATAL` (None = silent).
    pub error: Option<Channel>,
    /// Channel for `XML_ERR_WARNING` (None = silent).
    pub warning: Option<Channel>,
}

impl ParserChannels {
    /// ext/xml: no SAX error callbacks at all.
    pub const SILENT: Self = Self {
        error: None,
        warning: None,
    };
    /// DOM/SimpleXML: `php_libxml_ctx_error` / `php_libxml_ctx_warning`.
    pub const CTX: Self = Self {
        error: Some(Channel::CtxError),
        warning: Some(Channel::CtxWarning),
    };
}

thread_local! {
    static REGISTRY: RefCell<HashMap<usize, ParserChannels>> = RefCell::new(HashMap::new());
}

/// Registration of a live parser context; unregisters on drop.
#[derive(Debug)]
pub(crate) struct CtxtRegistration {
    addr: usize,
}

impl CtxtRegistration {
    pub(crate) fn new(ctxt: *mut ffi::xmlParserCtxt, channels: ParserChannels) -> Self {
        let addr = ctxt as usize;
        REGISTRY.with(|r| r.borrow_mut().insert(addr, channels));
        Self { addr }
    }
}

impl Drop for CtxtRegistration {
    fn drop(&mut self) {
        REGISTRY.with(|r| r.borrow_mut().remove(&self.addr));
    }
}

fn registered(addr: usize) -> Option<ParserChannels> {
    REGISTRY.with(|r| r.borrow().get(&addr).copied())
}

fn is_parser_domain(domain: i32) -> bool {
    matches!(
        domain,
        ffi::XML_FROM_PARSER
            | ffi::XML_FROM_HTML
            | ffi::XML_FROM_DTD
            | ffi::XML_FROM_NAMESPACE
            | ffi::XML_FROM_IO
            | ffi::XML_FROM_VALID
    )
}

/// Copies an `xmlError` into the crate's owned representation.
///
/// # Safety
/// `e` must point to a valid `xmlError` whose string fields are NULL or
/// NUL-terminated.
unsafe fn copy_error(e: &ffi::xmlError) -> LibXmlError {
    // SAFETY: string fields are valid C strings or NULL per the contract.
    unsafe {
        LibXmlError {
            domain: e.domain,
            level: e.level,
            code: e.code,
            column: e.int2,
            message: opt_bytes(e.message).unwrap_or_default(),
            file: opt_bytes(e.file),
            line: e.line,
        }
    }
}

/// Reads the current input position of a parser context.
///
/// # Safety
/// `ctxt` must be NULL or a live parser context.
pub(crate) unsafe fn parser_position(ctxt: *mut ffi::xmlParserCtxt) -> Option<ParserPosition> {
    if ctxt.is_null() {
        return None;
    }
    // SAFETY: ctxt is live; its input pointer is NULL or a live input.
    unsafe {
        let input = (*ctxt).input;
        if input.is_null() {
            return None;
        }
        Some(ParserPosition {
            filename: opt_bytes((*input).filename),
            line: (*input).line,
            column: (*input).col,
        })
    }
}

/// The structured error handler installed on every thread.
pub(crate) unsafe extern "C" fn structured_trampoline(_user: *mut c_void, err: *mut ffi::xmlError) {
    if err.is_null() {
        return;
    }
    // SAFETY: libxml2 passes a valid error record for the duration of the
    // call. The context pointer is only dereferenced when it is registered
    // as a live parser context created by this crate.
    let (error, origin) = unsafe {
        let e = &*err;
        let error = copy_error(e);
        let origin = if is_parser_domain(e.domain) && !e.ctxt.is_null() {
            let addr = e.ctxt as usize;
            match registered(addr) {
                Some(channels) => ErrorOrigin::Parser {
                    channels,
                    position: parser_position(e.ctxt.cast()),
                },
                None => ErrorOrigin::Parser {
                    channels: ParserChannels::SILENT,
                    position: None,
                },
            }
        } else {
            ErrorOrigin::Other
        };
        (error, origin)
    };
    guard::catch((), || dispatch(error, origin));
}

/// `xmlGetLastError()`, copied.
pub fn last_error() -> Option<LibXmlError> {
    super::ensure_init();
    // SAFETY: xmlGetLastError returns NULL or the thread's last-error record.
    unsafe {
        let e = ffi::xmlGetLastError();
        if e.is_null() || (*e).code == ffi::XML_ERR_OK {
            None
        } else {
            Some(copy_error(&*e))
        }
    }
}

/// `xmlResetLastError()`.
pub fn reset_last_error() {
    super::ensure_init();
    // SAFETY: plain call into libxml2's per-thread state.
    unsafe { ffi::xmlResetLastError() }
}

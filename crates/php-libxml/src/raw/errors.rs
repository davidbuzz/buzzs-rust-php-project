// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
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

fn domain_label(domain: i32) -> &'static [u8] {
    match domain {
        1 | 13 => b"parser ",
        3 => b"namespace ",
        4 | 23 => b"validity ",
        5 => b"HTML parser ",
        6 => b"memory ",
        7 => b"output ",
        8 => b"I/O ",
        11 => b"XInclude ",
        12 => b"XPath ",
        14 => b"regexp ",
        16 => b"Schemas parser ",
        17 => b"Schemas validity ",
        18 => b"Relax-NG parser ",
        19 => b"Relax-NG validity ",
        20 => b"Catalog ",
        21 => b"C14N ",
        22 => b"XSLT ",
        26 => b"module ",
        27 => b"encoding ",
        28 => b"schematron ",
        29 => b"internal buffer ",
        30 => b"URI ",
        _ => b"",
    }
}

/// `xmlParserPrintFileContextInternal`: the source line and a caret line.
///
/// # Safety
/// `input` must be NULL or a live parser input whose `base..cur` range is
/// readable and NUL-terminated after `cur`.
unsafe fn file_context(input: *const ffi::xmlParserInput, out: &mut Vec<Vec<u8>>) {
    const SIZE: usize = 81;
    // SAFETY: guaranteed by the caller.
    unsafe {
        if input.is_null() || (*input).cur.is_null() {
            return;
        }
        let base = (*input).base;
        let mut cur = (*input).cur;
        while cur > base && (*cur == b'\n' || *cur == b'\r') {
            cur = cur.sub(1);
        }
        let mut n = 0;
        while n < SIZE - 1 && cur > base && *cur != b'\n' && *cur != b'\r' {
            n += 1;
            cur = cur.sub(1);
        }
        if *cur == b'\n' || *cur == b'\r' {
            cur = cur.add(1);
        }
        let col = usize::try_from((*input).cur.offset_from(cur)).unwrap_or(0);
        let mut content = Vec::new();
        while *cur != 0 && *cur != b'\n' && *cur != b'\r' && content.len() < SIZE - 1 {
            content.push(*cur);
            cur = cur.add(1);
        }
        let mut line = content.clone();
        line.push(b'\n');
        out.push(line);
        let mut caret = Vec::new();
        let mut i = 0;
        while i < col && i < SIZE - 2 && i < content.len() {
            caret.push(if content[i] == b'\t' { b'\t' } else { b' ' });
            i += 1;
        }
        caret.extend_from_slice(b"^\n");
        out.push(caret);
    }
}

/// `xmlReportError`'s output, split into the pieces handed to the
/// generic error channel.
///
/// # Safety
/// `ctxt` must be NULL or a live parser context.
unsafe fn report_segments(e: &ffi::xmlError, ctxt: *mut ffi::xmlParserCtxt) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    if e.code == ffi::XML_ERR_OK {
        return out;
    }
    // SAFETY: ctxt is live (or NULL); inputs and nodes are owned by it.
    unsafe {
        let mut input: *mut ffi::xmlParserInput = std::ptr::null_mut();
        let mut cur: *mut ffi::xmlParserInput = std::ptr::null_mut();
        if ctxt.is_null() {
            if let Some(f) = opt_bytes(e.file) {
                out.push(format!("{}:{}: ", String::from_utf8_lossy(&f), e.line).into_bytes());
            } else if e.line != 0 && matches!(e.domain, 1 | 4 | 16 | 17 | 18 | 19) {
                out.push(format!("Entity: line {}: ", e.line).into_bytes());
            }
        } else {
            input = (*ctxt).input;
            if !input.is_null() && (*input).filename.is_null() && (*ctxt).inputNr > 1 {
                cur = input;
                let idx = usize::try_from((*ctxt).inputNr - 2).unwrap_or(0);
                input = *(*ctxt).inputTab.add(idx);
            }
            if !input.is_null() {
                if let Some(f) = opt_bytes((*input).filename) {
                    out.push(
                        format!("{}:{}: ", String::from_utf8_lossy(&f), (*input).line).into_bytes(),
                    );
                } else if e.line != 0 && e.domain == ffi::XML_FROM_PARSER {
                    out.push(format!("Entity: line {}: ", (*input).line).into_bytes());
                }
            }
        }
        let node = e.node.cast::<ffi::xmlNode>();
        if !node.is_null() && (*node).type_ == ffi::XML_ELEMENT_NODE {
            let mut seg = b"element ".to_vec();
            seg.extend_from_slice(super::opt_slice((*node).name).unwrap_or_default());
            seg.extend_from_slice(b": ");
            out.push(seg);
        }
        out.push(domain_label(e.domain).to_vec());
        out.push(
            match e.level {
                ffi::XML_ERR_WARNING => &b"warning : "[..],
                ffi::XML_ERR_ERROR | ffi::XML_ERR_FATAL => b"error : ",
                _ => b": ",
            }
            .to_vec(),
        );
        let mut msg = opt_bytes(e.message).unwrap_or_else(|| b"out of memory error".to_vec());
        if msg.last() != Some(&b'\n') {
            msg.push(b'\n');
        }
        out.push(msg);
        if !ctxt.is_null() {
            file_context(input, &mut out);
            if !cur.is_null() {
                if let Some(f) = opt_bytes((*cur).filename) {
                    out.push(
                        format!("{}:{}: \n", String::from_utf8_lossy(&f), (*cur).line).into_bytes(),
                    );
                } else if e.line != 0 && e.domain == ffi::XML_FROM_PARSER {
                    out.push(format!("Entity: line {}: \n", (*cur).line).into_bytes());
                }
                file_context(cur, &mut out);
            }
        }
    }
    out
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
                // A context libxml2 created internally (e.g. for an
                // external entity) with its default SAX handlers: PHP sees
                // xmlReportError's output through the generic handler.
                None => ErrorOrigin::Report(report_segments(e, e.ctxt.cast())),
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

/// The `fopencookie` write callback behind the generic error channel.
pub(crate) unsafe extern "C" fn generic_write(
    _cookie: *mut c_void,
    buf: *const std::ffi::c_char,
    size: usize,
) -> isize {
    super::guard::catch((), || {
        if !buf.is_null() && size > 0 {
            // SAFETY: libc passes `size` valid bytes.
            let bytes = unsafe { std::slice::from_raw_parts(buf.cast::<u8>(), size) };
            crate::error::generic_error(bytes);
        }
    });
    isize::try_from(size).unwrap_or(isize::MAX)
}

/// Routes libxml2's generic error channel (`xmlGenericError`, used for
/// messages outside the structured error path, e.g. "xmlXPathCompOpEval:
/// function … not found") of the current thread into PHP's error model:
/// the default handler `vfprintf`s into an unbuffered `FILE*` whose writes
/// land in [`generic_write`].
pub(crate) fn install_generic_channel() {
    let funcs = ffi::cookie_io_functions_t {
        read: None,
        write: Some(generic_write),
        seek: None,
        close: None,
    };
    // SAFETY: a cookie stream with a valid write callback and no state; it
    // lives for the rest of the thread (libxml2 keeps the pointer as the
    // per-thread generic error context). A NULL handler selects libxml2's
    // default handler, which writes to the context stream.
    unsafe {
        let file = ffi::fopencookie(std::ptr::null_mut(), c"w".as_ptr(), funcs);
        if file.is_null() {
            return;
        }
        ffi::setvbuf(file, std::ptr::null_mut(), ffi::IONBF, 0);
        ffi::xmlSetGenericErrorFunc(file, None);
    }
}

#[cfg(test)]
mod generic_channel_tests {
    use std::ffi::{c_char, c_int, c_void};

    extern "C" {
        fn __xmlGenericErrorContext() -> *mut *mut c_void;
        fn fputs(s: *const c_char, stream: *mut c_void) -> c_int;
    }

    #[test]
    fn generic_channel_reaches_the_error_model() {
        crate::raw::ensure_init();
        crate::error::reset_request_state();
        crate::error::take_diagnostics();
        crate::error::use_internal_errors(Some(false));
        // What xmlGenericError() does with the default handler: write to the
        // per-thread context stream.
        // SAFETY: libxml2's per-thread context is our cookie stream.
        unsafe {
            let ctx = *__xmlGenericErrorContext();
            assert!(!ctx.is_null());
            fputs(c"xmlXPathCompOpEval: function ".as_ptr(), ctx);
            fputs(c"test not found\n".as_ptr(), ctx);
        }
        let d = crate::error::take_diagnostics();
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].message, b"xmlXPathCompOpEval: function test not found");
        assert_eq!(d[0].level, crate::error::DiagnosticLevel::Warning);

        // Internal errors: collected instead of displayed.
        crate::error::use_internal_errors(Some(true));
        // SAFETY: as above.
        unsafe {
            fputs(c"generic message\n".as_ptr(), *__xmlGenericErrorContext());
        }
        assert!(crate::error::take_diagnostics().is_empty());
        let errors = crate::error::get_errors();
        assert_eq!(
            errors.last().map(|e| e.message.clone()),
            Some(b"generic message".to_vec())
        );
        crate::error::use_internal_errors(Some(false));
        crate::error::reset_request_state();
    }
}

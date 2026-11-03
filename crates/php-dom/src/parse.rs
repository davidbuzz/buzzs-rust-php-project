//! Parsing documents with libxml2 (`dom_document_parser`, `dom_load_html`)
//! and the error-routing scope used around every libxml2 call that may
//! report errors.
//!
//! PHP decides how a libxml2 error is displayed from the handler libxml2
//! picks: DOM installs `php_libxml_ctx_error`/`_warning` on its parser
//! contexts ("… in Entity, line: N"), errors raised before those handlers
//! are installed (or by parsers created internally by libxml2) go through
//! libxml2's default formatter (`"I/O warning : …"`) to PHP's generic
//! handler, and everything else is displayed verbatim. `php-libxml` always
//! installs a structured handler; while a [`ErrorScope`] is active this
//! module swaps in its own handler that replays PHP's choice for the
//! parser context it owns and forwards everything else.

use std::cell::RefCell;
use std::ffi::{c_void, CStr};
use std::ptr;

use php_libxml::error::{self as lxerr, DiagnosticLevel, ParserPosition};

use crate::ffi;
use crate::raw::{cs, opt_cstr};

struct ScopeState {
    ctxt: *mut ffi::xmlParserCtxt,
    handlers_installed: bool,
    prev: ffi::xmlStructuredErrorFunc,
    prev_data: *mut c_void,
}

thread_local! {
    static SCOPES: RefCell<Vec<ScopeState>> = const { RefCell::new(Vec::new()) };
}

/// While alive, libxml2 errors are routed like PHP's DOM extension does.
pub(crate) struct ErrorScope {
    _private: (),
}

impl ErrorScope {
    pub(crate) fn new() -> Self {
        php_libxml::raw::ensure_init();
        // SAFETY: reading/writing libxml2's per-thread globals.
        let (prev, prev_data) = unsafe {
            let p = *ffi::__xmlStructuredError();
            let d = *ffi::__xmlStructuredErrorContext();
            php_libxml::ffi::xmlSetStructuredErrorFunc(ptr::null_mut(), Some(trampoline));
            (p, d)
        };
        SCOPES.with(|s| {
            s.borrow_mut().push(ScopeState {
                ctxt: ptr::null_mut(),
                handlers_installed: false,
                prev,
                prev_data,
            });
        });
        ErrorScope { _private: () }
    }

    /// Declares the parser context created inside this scope.
    #[allow(clippy::unused_self)]
    pub(crate) fn set_ctxt(&self, ctxt: *mut ffi::xmlParserCtxt) {
        SCOPES.with(|s| {
            if let Some(top) = s.borrow_mut().last_mut() {
                top.ctxt = ctxt;
            }
        });
    }

    /// From now on errors of the owned context use the DOM context channels.
    #[allow(clippy::unused_self)]
    pub(crate) fn install_ctx_handlers(&self) {
        SCOPES.with(|s| {
            if let Some(top) = s.borrow_mut().last_mut() {
                top.handlers_installed = true;
            }
        });
    }
}

impl Drop for ErrorScope {
    fn drop(&mut self) {
        let state = SCOPES.with(|s| s.borrow_mut().pop());
        if let Some(state) = state {
            // SAFETY: restoring the previous per-thread handler.
            unsafe { php_libxml::ffi::xmlSetStructuredErrorFunc(state.prev_data, state.prev) };
        }
        if SCOPES.with(|s| s.borrow().is_empty()) && !std::thread::panicking() {
            resume_panic();
        }
    }
}

fn is_parser_domain(domain: i32) -> bool {
    matches!(domain, 1 | 3 | 4 | 5 | 8 | 23)
}

fn domain_label(domain: i32) -> &'static str {
    match domain {
        1 | 13 => "parser ",
        2 => "tree ",
        3 => "namespace ",
        4 | 23 => "validity ",
        5 => "HTML parser ",
        6 => "memory ",
        7 => "output ",
        8 => "I/O ",
        11 => "XInclude ",
        12 => "XPath ",
        14 => "regexp ",
        15 => "Schemas datatype ",
        16 => "Schemas parser ",
        17 => "Schemas validity ",
        18 => "Relax-NG parser ",
        19 => "Relax-NG validity ",
        20 => "Catalog ",
        21 => "C14N ",
        22 => "XSLT ",
        30 => "encoding ",
        _ => "",
    }
}

/// `xmlParserPrintFileContextInternal`: the source line and a caret line.
fn file_context(input: *mut ffi::xmlParserInput) -> Vec<Vec<u8>> {
    // SAFETY: input is NULL or a live parser input whose cur/base point
    // into its NUL-terminated buffer.
    unsafe {
        if input.is_null() || (*input).cur.is_null() {
            return Vec::new();
        }
        let base = (*input).base;
        let mut cur = (*input).cur;
        while cur > base && (*cur == b'\n' || *cur == b'\r') {
            cur = cur.sub(1);
        }
        let mut n = 0;
        loop {
            let cont = n < 80 && cur > base && *cur != b'\n' && *cur != b'\r';
            n += 1;
            if !cont {
                break;
            }
            cur = cur.sub(1);
        }
        if *cur == b'\n' || *cur == b'\r' {
            cur = cur.add(1);
        }
        let col = usize::try_from((*input).cur.offset_from(cur)).unwrap_or(0);
        let mut content = Vec::new();
        let mut p = cur;
        while *p != 0 && *p != b'\n' && *p != b'\r' && content.len() < 80 {
            content.push(*p);
            p = p.add(1);
        }
        let mut caret = Vec::new();
        let mut i = 0;
        while i < col && i < 79 && i < content.len() {
            caret.push(if content[i] == b'\t' { b'\t' } else { b' ' });
            i += 1;
        }
        caret.push(b'^');
        vec![content, caret]
    }
}

/// `xmlReportError`'s formatting: the message plus source-context lines.
fn report_error_format(e: &ffi::xmlError, ctxt: *mut ffi::xmlParserCtxt) -> Vec<Vec<u8>> {
    let mut lines = vec![report_error_head(e, ctxt)];
    if !ctxt.is_null() {
        // SAFETY: live context.
        let input = unsafe {
            let mut input = (*ctxt).input;
            if !input.is_null() && (*input).filename.is_null() && (*ctxt).inputNr > 1 {
                input = *(*ctxt)
                    .inputTab
                    .add(usize::try_from((*ctxt).inputNr - 2).unwrap_or(0));
            }
            input
        };
        lines.extend(file_context(input));
    }
    lines
}

fn report_error_head(e: &ffi::xmlError, ctxt: *mut ffi::xmlParserCtxt) -> Vec<u8> {
    let mut out = Vec::new();
    // SAFETY: e is a valid error record; ctxt is NULL or a live context.
    unsafe {
        if ctxt.is_null() {
            if !e.file.is_null() {
                out.extend_from_slice(CStr::from_ptr(e.file).to_bytes());
                out.extend_from_slice(format!(":{}: ", e.line).as_bytes());
            } else if e.line != 0 && matches!(e.domain, 1 | 4 | 16 | 17 | 18 | 19) {
                out.extend_from_slice(format!("Entity: line {}: ", e.line).as_bytes());
            }
        } else {
            let mut input = (*ctxt).input;
            if !input.is_null() && (*input).filename.is_null() && (*ctxt).inputNr > 1 {
                input = *(*ctxt)
                    .inputTab
                    .add(usize::try_from((*ctxt).inputNr - 2).unwrap_or(0));
            }
            if !input.is_null() {
                if !(*input).filename.is_null() {
                    out.extend_from_slice(CStr::from_ptr((*input).filename).to_bytes());
                    out.extend_from_slice(format!(":{}: ", (*input).line).as_bytes());
                } else if e.line != 0 && e.domain == 1 {
                    out.extend_from_slice(format!("Entity: line {}: ", (*input).line).as_bytes());
                }
            }
        }
        let node = e.node.cast::<ffi::xmlNode>();
        if !node.is_null() && (*node).type_ == ffi::XML_ELEMENT_NODE {
            out.extend_from_slice(b"element ");
            out.extend_from_slice(crate::raw::cstr((*node).name));
            out.extend_from_slice(b": ");
        }
    }
    out.extend_from_slice(domain_label(e.domain).as_bytes());
    out.extend_from_slice(match e.level {
        1 => b"warning : ".as_slice(),
        2 | 3 => b"error : ",
        _ => b": ",
    });
    let msg = opt_cstr(e.message.cast()).unwrap_or(b"out of memory error");
    let mut msg = msg.to_vec();
    while msg.last() == Some(&b'\n') {
        msg.pop();
    }
    out.extend_from_slice(&msg);
    out
}

fn parser_position(ctxt: *mut ffi::xmlParserCtxt) -> Option<ParserPosition> {
    if ctxt.is_null() {
        return None;
    }
    // SAFETY: ctxt is the live parser context owned by the scope.
    unsafe {
        let input = (*ctxt).input;
        if input.is_null() {
            return None;
        }
        Some(ParserPosition {
            filename: opt_cstr((*input).filename.cast()).map(<[u8]>::to_vec),
            line: (*input).line,
            column: (*input).col,
        })
    }
}

thread_local! {
    static PENDING_PANIC: RefCell<Option<Box<dyn std::any::Any + Send>>> = const { RefCell::new(None) };
}

/// Re-raises a panic caught in a libxml2 callback of this crate.
pub(crate) fn resume_panic() {
    if let Some(p) = PENDING_PANIC.with(|x| x.borrow_mut().take()) {
        std::panic::resume_unwind(p);
    }
}

/// Runs a callback body, catching panics at the FFI boundary.
pub(crate) fn guard<R>(fallback: R, f: impl FnOnce() -> R) -> R {
    if PENDING_PANIC.with(|x| x.borrow().is_some()) {
        return fallback;
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => {
            PENDING_PANIC.with(|x| *x.borrow_mut() = Some(p));
            fallback
        }
    }
}

unsafe extern "C" fn trampoline(_user: *mut c_void, err: *mut ffi::xmlError) {
    guard((), || {
        // SAFETY: forwarded from libxml2.
        unsafe { trampoline_inner(err) }
    });
}

unsafe fn trampoline_inner(err: *mut ffi::xmlError) {
    if err.is_null() {
        return;
    }
    let top = SCOPES.with(|s| {
        s.borrow()
            .last()
            .map(|t| (t.ctxt, t.handlers_installed, t.prev, t.prev_data))
    });
    let Some((ctxt, installed, prev, prev_data)) = top else {
        return;
    };
    // SAFETY: libxml2 passes a valid error record for the call's duration.
    let e = unsafe { &*err };
    let forward = || {
        if let Some(f) = prev {
            // SAFETY: forwarding to the previously installed handler.
            unsafe { f(prev_data, err) };
        }
    };
    if lxerr::uses_internal_errors() {
        forward();
        return;
    }
    let from_parser_ctxt = is_parser_domain(e.domain) && !e.ctxt.is_null();
    if from_parser_ctxt && e.ctxt.cast() == ctxt && installed {
        // LIBXML_NOERROR / LIBXML_NOWARNING clear the SAX channels.
        // SAFETY: ctxt is the live context owned by the scope.
        let silenced = unsafe {
            let sax = (*ctxt).sax;
            !sax.is_null()
                && if e.level == 1 {
                    (*sax).warning.is_none()
                } else {
                    (*sax).error.is_none()
                }
        };
        if silenced {
            return;
        }
        let msg = opt_cstr(e.message.cast()).unwrap_or_default().to_vec();
        let pos = parser_position(ctxt);
        if e.level == 1 {
            lxerr::ctx_warning(pos.as_ref(), &msg);
        } else {
            lxerr::ctx_error(pos.as_ref(), &msg);
        }
        return;
    }
    if from_parser_ctxt {
        let c: *mut ffi::xmlParserCtxt = if e.ctxt.cast() == ctxt {
            ctxt
        } else {
            e.ctxt.cast()
        };
        // A context whose SAX error channel was cleared (LIBXML_NOERROR /
        // LIBXML_NOWARNING) reports nothing.
        // SAFETY: e.ctxt is the live parser context raising the error.
        let silenced = !matches!(e.domain, 4 | 23)
            && unsafe {
                let sax = (*c).sax;
                !sax.is_null()
                    && if e.level == 1 {
                        (*sax).warning.is_none()
                    } else {
                        (*sax).error.is_none()
                    }
            };
        if silenced {
            return;
        }
        for line in report_error_format(e, c) {
            lxerr::issue_error(DiagnosticLevel::Warning, &line);
        }
        return;
    }
    forward();
}

/// Runs `f` inside an [`ErrorScope`] without an owned parser context.
pub(crate) fn with_generic_errors<R>(f: impl FnOnce() -> R) -> R {
    let _scope = ErrorScope::new();
    f()
}

/// `php_libxml_sanitize_parse_ctxt_options` (crate-visible).
pub(crate) fn sanitize_ctxt_pub(ctxt: *mut ffi::xmlParserCtxt) {
    sanitize_ctxt(ctxt);
}

/// `php_libxml_sanitize_parse_ctxt_options`.
fn sanitize_ctxt(ctxt: *mut ffi::xmlParserCtxt) {
    // SAFETY: live parser context.
    unsafe {
        (*ctxt).loadsubset = 0;
        (*ctxt).validate = 0;
        (*ctxt).pedantic = 0;
        (*ctxt).replaceEntities = 0;
        (*ctxt).linenumbers = 0;
        (*ctxt).keepBlanks = 1;
        (*ctxt).options = 0;
    }
}

/// Parse settings derived from the document properties.
#[derive(Debug, Clone, Copy, Default)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct ParseFlags {
    pub validate: bool,
    pub resolve_externals: bool,
    pub keep_blanks: bool,
    pub substitute_entities: bool,
    pub recover: bool,
}

/// Result of [`document_parser`].
#[derive(Debug)]
pub(crate) enum ParseOutcome {
    /// libxml2 could not create a context or document.
    Failed,
    /// The document was not well-formed (and recovery was off).
    Malformed,
    /// A new document.
    Doc(*mut ffi::xmlDoc),
}

/// `dom_get_valid_file_path`: `None` when the path cannot be resolved.
pub(crate) fn valid_file_path(source: &[u8]) -> Option<Vec<u8>> {
    let c = cs(source);
    // SAFETY: URI helpers on valid C strings; everything freed below.
    let has_scheme = unsafe {
        let uri = ffi::xmlCreateURI();
        if uri.is_null() {
            return None;
        }
        let esc = ffi::xmlURIEscapeStr(c.as_ptr().cast(), c":".as_ptr().cast());
        ffi::xmlParseURIReference(uri, esc.cast());
        crate::raw::xml_free(esc.cast());
        let scheme = (*uri.cast::<php_libxml::ffi::xmlURI>()).scheme;
        let has = !scheme.is_null();
        ffi::xmlFreeURI(uri);
        has
    };
    let mut src = source.to_vec();
    let mut is_file_uri = false;
    if has_scheme {
        let lower: Vec<u8> = source.iter().take(17).map(u8::to_ascii_lowercase).collect();
        if lower.starts_with(b"file:///") {
            is_file_uri = true;
            src = source[7..].to_vec();
        } else if lower.starts_with(b"file://localhost/") {
            is_file_uri = true;
            src = source[16..].to_vec();
        }
    }
    if !has_scheme || is_file_uri {
        let r = realpath_or_expand(&src);
        // MAXPATHLEN: longer paths cannot be resolved by PHP.
        if r.len() >= 4096 {
            return None;
        }
        return Some(r);
    }
    Some(src)
}

/// `VCWD_REALPATH` falling back to `expand_filepath`.
fn realpath_or_expand(path: &[u8]) -> Vec<u8> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let p = std::ffi::OsStr::from_bytes(path);
    if let Ok(r) = std::fs::canonicalize(p) {
        return r.into_os_string().into_vec();
    }
    if path.starts_with(b"/") {
        return path.to_vec();
    }
    match std::env::current_dir() {
        Ok(cwd) => {
            let mut v = cwd.into_os_string().into_vec();
            if !v.ends_with(b"/") {
                v.push(b'/');
            }
            let mut rest = path;
            while let Some(r) = rest.strip_prefix(b"./") {
                rest = r;
            }
            v.extend_from_slice(rest);
            v
        }
        Err(_) => path.to_vec(),
    }
}

/// The current working directory with a trailing slash, canonicalised
/// by libxml2 (`ctxt->directory` for in-memory loads).
fn cwd_directory() -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStringExt;
    let mut v = std::env::current_dir().ok()?.into_os_string().into_vec();
    if !v.ends_with(b"/") {
        v.push(b'/');
    }
    Some(v)
}

/// `dom_document_parser`.
pub(crate) fn document_parser(
    from_file: bool,
    source: &[u8],
    mut options: i32,
    flags: ParseFlags,
    encoding: Option<&[u8]>,
) -> ParseOutcome {
    let scope = ErrorScope::new();
    let recover = flags.recover || (options & ffi::XML_PARSE_RECOVER) == ffi::XML_PARSE_RECOVER;
    let ctxt = if from_file {
        let Some(dest) = valid_file_path(source) else {
            return ParseOutcome::Failed;
        };
        let c = cs(&dest);
        // SAFETY: valid C string.
        unsafe { ffi::xmlCreateFileParserCtxt(c.as_ptr()) }
    } else {
        // SAFETY: source valid for its length.
        unsafe {
            ffi::xmlCreateMemoryParserCtxt(source.as_ptr().cast(), crate::raw::c_len(source.len()))
        }
    };
    if ctxt.is_null() {
        return ParseOutcome::Failed;
    }
    scope.set_ctxt(ctxt);
    if let Some(enc) = encoding {
        let e = cs(enc);
        // SAFETY: handler lookup; ownership passes to the context.
        unsafe {
            let h = ffi::xmlFindCharEncodingHandler(e.as_ptr());
            if !h.is_null() {
                ffi::xmlSwitchToEncoding(ctxt, h);
            }
        }
    }
    if !from_file {
        if let Some(dir) = cwd_directory() {
            let d = cs(&dir);
            // SAFETY: the context owns `directory` (freed with xmlFree).
            unsafe {
                if !(*ctxt).directory.is_null() {
                    crate::raw::xml_free((*ctxt).directory.cast());
                }
                (*ctxt).directory = ffi::xmlCanonicPath(d.as_ptr().cast()).cast();
            }
        }
    }
    scope.install_ctx_handlers();
    if flags.validate {
        options |= ffi::XML_PARSE_DTDVALID;
    }
    if flags.resolve_externals {
        options |= ffi::XML_PARSE_DTDATTR;
    }
    if flags.substitute_entities {
        options |= ffi::XML_PARSE_NOENT;
    }
    if !flags.keep_blanks {
        options |= ffi::XML_PARSE_NOBLANKS;
    }
    if recover {
        options |= ffi::XML_PARSE_RECOVER;
    }
    sanitize_ctxt(ctxt);
    // SAFETY: live context.
    unsafe {
        ffi::xmlCtxtUseOptions(ctxt, options);
        ffi::xmlParseDocument(ctxt);
    }
    // SAFETY: live context; the document (if any) is taken over.
    let outcome = unsafe {
        let well_formed = (*ctxt).wellFormed != 0;
        if well_formed || recover {
            let doc = (*ctxt).myDoc;
            if !doc.is_null() && (*doc).URL.is_null() && !(*ctxt).directory.is_null() {
                (*doc).URL = ffi::xmlStrdup((*ctxt).directory.cast());
            }
            (*ctxt).myDoc = ptr::null_mut();
            if doc.is_null() {
                ParseOutcome::Failed
            } else {
                ParseOutcome::Doc(doc)
            }
        } else {
            ffi::xmlFreeDoc((*ctxt).myDoc);
            (*ctxt).myDoc = ptr::null_mut();
            ParseOutcome::Malformed
        }
    };
    // SAFETY: the context is freed exactly once.
    unsafe { ffi::xmlFreeParserCtxt(ctxt) };
    drop(scope);
    outcome
}

/// `dom_load_html`'s parsing part: the new document or NULL.
pub(crate) fn load_html(from_file: bool, source: &[u8], options: i32) -> *mut ffi::xmlDoc {
    let scope = ErrorScope::new();
    let ctxt = if from_file {
        let c = cs(source);
        // SAFETY: valid C string.
        unsafe { ffi::htmlCreateFileParserCtxt(c.as_ptr(), ptr::null()) }
    } else {
        // SAFETY: source valid for its length.
        unsafe {
            ffi::htmlCreateMemoryParserCtxt(source.as_ptr().cast(), crate::raw::c_len(source.len()))
        }
    };
    if ctxt.is_null() {
        return ptr::null_mut();
    }
    scope.set_ctxt(ctxt);
    scope.install_ctx_handlers();
    sanitize_ctxt(ctxt);
    // SAFETY: live context.
    let doc = unsafe {
        if options != 0 {
            ffi::htmlCtxtUseOptions(ctxt, options);
        }
        ffi::htmlParseDocument(ctxt);
        let d = (*ctxt).myDoc;
        (*ctxt).myDoc = ptr::null_mut();
        ffi::htmlFreeParserCtxt(ctxt);
        d
    };
    drop(scope);
    doc
}

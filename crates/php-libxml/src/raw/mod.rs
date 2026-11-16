// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The unsafe boundary of the crate.
//!
//! Every `unsafe` block of the crate lives either in [`crate::ffi`]
//! (declarations only) or in this module tree. Each submodule wraps one
//! libxml2 facility in types whose private fields encode libxml2's
//! ownership rules, so that the safe API they expose is sound:
//!
//! * [`errors`] — the per-thread structured error trampoline and the
//!   registry of live parser contexts (used to decide how PHP would have
//!   reported an error).
//! * [`io`] — the external entity loader trampoline and the
//!   input/output buffer factories that route libxml2 file I/O through
//!   user hooks (PHP streams).
//! * [`sax`] — the expat-compatible push parser (`ext/xml/compat.c`).
//! * [`tree`] — reference-counted documents and node handles.
//! * [`writer`] — `xmlTextWriter` over an in-memory or `Write` sink.
//!
//! Panics raised by Rust code invoked from a libxml2 callback are caught
//! at the FFI boundary (unwinding through C frames is undefined
//! behaviour), stashed, and resumed once control is back in Rust (see
//! [`guard`]).

pub mod errors;
pub mod io;
pub mod sax;
pub mod tree;
pub mod writer;

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void, CStr, CString};
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::{Once, OnceLock};

use crate::ffi;

static GLOBAL_INIT: Once = Once::new();

/// The entity loader that was installed before ours (libxml2's default).
static DEFAULT_ENTITY_LOADER: OnceLock<DefaultLoader> = OnceLock::new();

/// Wrapper making the loaded function pointer storable in a static.
#[derive(Clone, Copy)]
struct DefaultLoader(ffi::xmlExternalEntityLoader);

thread_local! {
    static THREAD_INIT: Cell<bool> = const { Cell::new(false) };
    static PENDING_PANIC: RefCell<Option<Box<dyn Any + Send>>> = const { RefCell::new(None) };
}

/// Initialises libxml2 once per process and installs this crate's hooks
/// for the calling thread (structured error handler, I/O factories).
///
/// Called at the start of every public entry point; cheap after the first
/// call on a thread.
pub fn ensure_init() {
    GLOBAL_INIT.call_once(|| {
        // SAFETY: xmlInitParser is idempotent and thread-safe; the entity
        // loader we install is a valid `extern "C"` function for the whole
        // process lifetime. The default loader is read before ours is set.
        unsafe {
            ffi::xmlInitParser();
            let previous = ffi::xmlGetExternalEntityLoader();
            let _ = DEFAULT_ENTITY_LOADER.set(DefaultLoader(previous));
            ffi::xmlSetExternalEntityLoader(Some(io::entity_loader_trampoline));
        }
    });
    THREAD_INIT.with(|done| {
        if !done.get() {
            done.set(true);
            // SAFETY: these setters write libxml2's per-thread global state;
            // the installed callbacks are valid `extern "C"` functions.
            unsafe {
                ffi::xmlSetStructuredErrorFunc(
                    std::ptr::null_mut(),
                    Some(errors::structured_trampoline),
                );
                errors::install_generic_channel();
                ffi::xmlParserInputBufferCreateFilenameDefault(Some(io::input_buffer_trampoline));
                ffi::xmlOutputBufferCreateFilenameDefault(Some(io::output_buffer_trampoline));
            }
        }
    });
}

/// libxml2's default external entity loader (before ours was installed).
pub(crate) fn default_entity_loader() -> ffi::xmlExternalEntityLoader {
    DEFAULT_ENTITY_LOADER.get().and_then(|l| l.0)
}

/// The libxml2 version string actually loaded (`xmlParserVersion`, e.g.
/// `"20914"`).
pub fn loaded_version() -> String {
    ensure_init();
    // SAFETY: xmlParserVersion is a static NUL-terminated string.
    unsafe {
        let p = ffi::xmlParserVersion;
        if p.is_null() {
            String::new()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

/// Panic plumbing for callbacks invoked by libxml2.
pub(crate) mod guard {
    use super::{catch_unwind, resume_unwind, AssertUnwindSafe, PENDING_PANIC};

    /// Runs `f`, catching a panic and stashing it for [`resume`]. Returns
    /// `fallback` when `f` panicked or when an earlier panic is pending
    /// (no Rust code runs after a panic until control returns to Rust).
    pub fn catch<R>(fallback: R, f: impl FnOnce() -> R) -> R {
        if pending() {
            return fallback;
        }
        match catch_unwind(AssertUnwindSafe(f)) {
            Ok(r) => r,
            Err(payload) => {
                PENDING_PANIC.with(|p| *p.borrow_mut() = Some(payload));
                fallback
            }
        }
    }

    /// Whether a panic is waiting to be resumed.
    pub fn pending() -> bool {
        PENDING_PANIC.with(|p| p.borrow().is_some())
    }

    /// Re-raises a panic caught inside a libxml2 callback, if any.
    pub fn resume() {
        if let Some(payload) = PENDING_PANIC.with(|p| p.borrow_mut().take()) {
            resume_unwind(payload);
        }
    }
}

/// Builds a C string from PHP bytes, truncating at the first NUL byte
/// (what the C extension does when it hands a `char *` to libxml2).
pub(crate) fn c_string(bytes: &[u8]) -> CString {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    // The slice has no interior NUL by construction.
    CString::new(&bytes[..end]).unwrap_or_default()
}

/// Copies a possibly-NULL C string.
///
/// # Safety
/// `p` must be NULL or point to a NUL-terminated string valid for reads.
pub(crate) unsafe fn opt_bytes(p: *const c_char) -> Option<Vec<u8>> {
    if p.is_null() {
        None
    } else {
        // SAFETY: guaranteed by the caller.
        Some(unsafe { CStr::from_ptr(p) }.to_bytes().to_vec())
    }
}

/// Borrows a possibly-NULL C string.
///
/// # Safety
/// `p` must be NULL or point to a NUL-terminated string that outlives `'a`.
pub(crate) unsafe fn opt_slice<'a>(p: *const u8) -> Option<&'a [u8]> {
    if p.is_null() {
        None
    } else {
        // SAFETY: guaranteed by the caller.
        Some(unsafe { CStr::from_ptr(p.cast::<c_char>()) }.to_bytes())
    }
}

/// Takes ownership of a libxml2-allocated string, copying it and releasing
/// it with `xmlFree`.
///
/// # Safety
/// `p` must be NULL or a NUL-terminated string allocated by libxml2's
/// allocator and not used afterwards.
pub(crate) unsafe fn take_xml_string(p: *mut u8) -> Option<Vec<u8>> {
    if p.is_null() {
        return None;
    }
    // SAFETY: p is a valid C string per the caller's contract.
    let bytes = unsafe { CStr::from_ptr(p.cast::<c_char>()) }
        .to_bytes()
        .to_vec();
    // SAFETY: p was allocated by libxml2 and is released exactly once.
    unsafe { xml_free(p.cast::<c_void>()) };
    Some(bytes)
}

/// Releases memory allocated by libxml2.
///
/// # Safety
/// `p` must be NULL or a pointer obtained from libxml2's allocator that is
/// not freed elsewhere.
pub(crate) unsafe fn xml_free(p: *mut c_void) {
    if p.is_null() {
        return;
    }
    // SAFETY: xmlFree is an initialised global function pointer; `p` was
    // allocated by libxml2 per the caller's contract.
    unsafe {
        if let Some(free) = ffi::xmlFree {
            free(p);
        }
    }
}

/// `xmlValidateName(name, 0) == 0`: whether `name` is a valid XML Name.
pub fn validate_name(name: &[u8]) -> bool {
    ensure_init();
    let c = c_string(name);
    // SAFETY: c is a valid NUL-terminated string for the call's duration.
    unsafe { ffi::xmlValidateName(c.as_ptr().cast(), 0) == 0 }
}

/// `xmlParseCharEncoding(name)` mapped to PHP's sniffing rule
/// (`<= XML_CHAR_ENCODING_NONE` collapses to `NONE`).
pub(crate) fn parse_char_encoding(name: &[u8]) -> i32 {
    let c = c_string(name);
    // SAFETY: c is a valid C string.
    let enc = unsafe { ffi::xmlParseCharEncoding(c.as_ptr()) };
    enc.max(ffi::XML_CHAR_ENCODING_NONE)
}

/// Thin RFC 3986 helpers backed by libxml2's URI module.
pub mod uri {
    use super::{c_string, ensure_init, take_xml_string};
    use crate::ffi;

    /// Result of `xmlParseURI`: whether it parsed and the scheme, if any.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ParsedUri {
        /// The URI scheme (`file`, `http`, …), if present.
        pub scheme: Option<Vec<u8>>,
    }

    /// `xmlParseURI(s)`; `None` when libxml2 rejects the string.
    pub fn parse(s: &[u8]) -> Option<ParsedUri> {
        ensure_init();
        let c = c_string(s);
        // SAFETY: c is a valid C string; the returned URI is freed below.
        unsafe {
            let uri = ffi::xmlParseURI(c.as_ptr());
            if uri.is_null() {
                return None;
            }
            let scheme = super::opt_bytes((*uri).scheme);
            ffi::xmlFreeURI(uri);
            Some(ParsedUri { scheme })
        }
    }

    /// Scheme detection used by `XMLWriter`'s path resolution: escapes the
    /// source with `xmlURIEscapeStr(source, ":")` and parses it as a URI
    /// reference.
    pub fn reference_scheme(s: &[u8]) -> Option<Vec<u8>> {
        ensure_init();
        let c = c_string(s);
        // SAFETY: all pointers are checked for NULL; the escaped string
        // and the URI are released before returning.
        unsafe {
            let uri = ffi::xmlCreateURI();
            if uri.is_null() {
                return None;
            }
            let esc = ffi::xmlURIEscapeStr(c.as_ptr().cast(), c":".as_ptr().cast());
            if !esc.is_null() {
                ffi::xmlParseURIReference(uri, esc.cast());
                super::xml_free(esc.cast());
            }
            let scheme = super::opt_bytes((*uri).scheme);
            ffi::xmlFreeURI(uri);
            scheme
        }
    }

    /// `xmlURIUnescapeString(s, 0, NULL)`.
    pub fn unescape(s: &[u8]) -> Option<Vec<u8>> {
        ensure_init();
        let c = c_string(s);
        // SAFETY: the result is a fresh libxml2 allocation (or NULL).
        unsafe {
            take_xml_string(ffi::xmlURIUnescapeString(c.as_ptr(), 0, std::ptr::null_mut()).cast())
        }
    }
}

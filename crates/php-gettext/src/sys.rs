//! Thin, safe wrappers over glibc's built-in libintl (`gettext(3)` family).
//!
//! This is the only module of the crate containing `unsafe` code.  All
//! wrappers take already-validated, NUL-free [`CStr`]s and copy the returned
//! C strings into owned buffers before returning, so no libintl-owned memory
//! escapes.

#![allow(unsafe_code)]

use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_ulong};

extern "C" {
    fn textdomain(domainname: *const c_char) -> *mut c_char;
    fn gettext(msgid: *const c_char) -> *mut c_char;
    fn dgettext(domainname: *const c_char, msgid: *const c_char) -> *mut c_char;
    fn dcgettext(domainname: *const c_char, msgid: *const c_char, category: c_int) -> *mut c_char;
    fn ngettext(msgid1: *const c_char, msgid2: *const c_char, n: c_ulong) -> *mut c_char;
    fn dngettext(
        domainname: *const c_char,
        msgid1: *const c_char,
        msgid2: *const c_char,
        n: c_ulong,
    ) -> *mut c_char;
    fn dcngettext(
        domainname: *const c_char,
        msgid1: *const c_char,
        msgid2: *const c_char,
        n: c_ulong,
        category: c_int,
    ) -> *mut c_char;
    fn bindtextdomain(domainname: *const c_char, dirname: *const c_char) -> *mut c_char;
    fn bind_textdomain_codeset(domainname: *const c_char, codeset: *const c_char) -> *mut c_char;
}

/// Copies a possibly-null C string.
///
/// # Safety
/// `ptr` must be null or point to a valid NUL-terminated string.
unsafe fn owned(ptr: *const c_char) -> Option<Vec<u8>> {
    if ptr.is_null() {
        None
    } else {
        // SAFETY: guaranteed by the caller.
        Some(unsafe { CStr::from_ptr(ptr) }.to_bytes().to_vec())
    }
}

/// Like [`owned`] but reports `None` when libintl handed back the very
/// `msgid` pointer it was given (meaning "no translation"), which PHP uses to
/// return its original — possibly NUL-containing — string unchanged.
///
/// # Safety
/// As for [`owned`].
unsafe fn translated(ptr: *const c_char, msgid: &CStr) -> Option<Vec<u8>> {
    if std::ptr::eq(ptr, msgid.as_ptr()) {
        None
    } else {
        // SAFETY: guaranteed by the caller.
        unsafe { owned(ptr) }
    }
}

/// `textdomain(3)`; `None` queries.  Returns `None` on failure.
pub fn set_textdomain(domain: Option<&CStr>) -> Option<Vec<u8>> {
    let p = domain.map_or(std::ptr::null(), CStr::as_ptr);
    // SAFETY: `p` is null or a valid C string; the result is a valid C string
    // owned by libintl (or null), copied immediately.
    unsafe { owned(textdomain(p)) }
}

/// `gettext(3)`; `None` when untranslated.
pub fn get(msgid: &CStr) -> Option<Vec<u8>> {
    // SAFETY: valid C string in; result is msgid itself or a catalog string.
    unsafe { translated(gettext(msgid.as_ptr()), msgid) }
}

/// `dgettext(3)`; `None` when untranslated.
pub fn d_get(domain: &CStr, msgid: &CStr) -> Option<Vec<u8>> {
    // SAFETY: valid C strings in; see `get`.
    unsafe { translated(dgettext(domain.as_ptr(), msgid.as_ptr()), msgid) }
}

/// `dcgettext(3)`; `None` when untranslated.
pub fn dc_get(domain: &CStr, msgid: &CStr, category: c_int) -> Option<Vec<u8>> {
    // SAFETY: valid C strings in; glibc validates the category itself.
    unsafe { translated(dcgettext(domain.as_ptr(), msgid.as_ptr(), category), msgid) }
}

/// `ngettext(3)`.
pub fn n_get(msgid1: &CStr, msgid2: &CStr, n: c_ulong) -> Vec<u8> {
    // SAFETY: valid C strings in; the result is one of them or a catalog
    // string, never null.
    unsafe { owned(ngettext(msgid1.as_ptr(), msgid2.as_ptr(), n)) }.unwrap_or_default()
}

/// `dngettext(3)`.
pub fn dn_get(domain: &CStr, msgid1: &CStr, msgid2: &CStr, n: c_ulong) -> Vec<u8> {
    // SAFETY: as for `n_get`.
    unsafe {
        owned(dngettext(
            domain.as_ptr(),
            msgid1.as_ptr(),
            msgid2.as_ptr(),
            n,
        ))
    }
    .unwrap_or_default()
}

/// `dcngettext(3)`.
pub fn dcn_get(
    domain: &CStr,
    msgid1: &CStr,
    msgid2: &CStr,
    n: c_ulong,
    category: c_int,
) -> Vec<u8> {
    // SAFETY: as for `n_get`.
    unsafe {
        owned(dcngettext(
            domain.as_ptr(),
            msgid1.as_ptr(),
            msgid2.as_ptr(),
            n,
            category,
        ))
    }
    .unwrap_or_default()
}

/// `bindtextdomain(3)`; `dir == None` queries.  `None` on failure.
pub fn bind(domain: &CStr, dir: Option<&CStr>) -> Option<Vec<u8>> {
    let d = dir.map_or(std::ptr::null(), CStr::as_ptr);
    // SAFETY: valid (or null) C strings in; result copied immediately.
    unsafe { owned(bindtextdomain(domain.as_ptr(), d)) }
}

/// `bind_textdomain_codeset(3)`; `codeset == None` queries.
pub fn bind_codeset(domain: &CStr, codeset: Option<&CStr>) -> Option<Vec<u8>> {
    let c = codeset.map_or(std::ptr::null(), CStr::as_ptr);
    // SAFETY: valid (or null) C strings in; result copied immediately.
    unsafe { owned(bind_textdomain_codeset(domain.as_ptr(), c)) }
}

/// `setlocale(category, locale)` for the tests (PHP's own `setlocale()` lives
/// in ext/standard).  Returns whether the locale was accepted.
#[cfg(test)]
pub fn setlocale(category: c_int, locale: &CStr) -> bool {
    // SAFETY: valid C string in; the result pointer is only null-checked.
    !unsafe { libc::setlocale(category, locale.as_ptr()) }.is_null()
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Transfer callbacks: the [`Handler`] trait the embedding implements and
//! the `extern "C"` trampolines libcurl calls.
//!
//! libcurl only invokes callbacks from inside `curl_easy_perform()`,
//! `curl_multi_perform()` and a few other entry points. Those entry points
//! are wrapped by [`with_handler`], which lends a `&mut dyn Handler` to the
//! trampolines through a thread-local slot for the duration of the call.
//! Every callback identifies its easy handle by the *key* installed as the
//! callback's user data (see [`crate::Easy::key`]).

use crate::consts::{
    CURLKHMATCH_MISMATCH, CURL_FNMATCHFUNC_FAIL, CURL_PREREQFUNC_OK, CURL_PUSH_DENY, CURL_PUSH_OK,
    CURL_SEEKFUNC_CANTSEEK,
};
use crate::easy::Easy;
use crate::ffi;
use std::cell::Cell;
use std::ffi::CStr;
use std::os::raw::{c_char, c_double, c_int, c_void};

/// Callbacks into the embedding during a transfer. Every method receives
/// the key of the easy handle the callback is for; the defaults are what
/// libcurl does without a callback.
pub trait Handler {
    /// `CURLOPT_WRITEFUNCTION`: returns the number of bytes handled.
    fn write(&mut self, key: u64, data: &[u8]) -> usize {
        let _ = key;
        data.len()
    }
    /// `CURLOPT_HEADERFUNCTION`: returns the number of bytes handled.
    fn header(&mut self, key: u64, data: &[u8]) -> usize {
        let _ = key;
        data.len()
    }
    /// `CURLOPT_READFUNCTION`: fills `buf`, returns the byte count or
    /// `CURL_READFUNC_ABORT`/`CURL_READFUNC_PAUSE`.
    fn read(&mut self, key: u64, buf: &mut [u8]) -> usize {
        let _ = (key, buf);
        0
    }
    /// `CURLOPT_SEEKFUNCTION`.
    fn seek(&mut self, key: u64, offset: i64, origin: i32) -> i32 {
        let _ = (key, offset, origin);
        cint(CURL_SEEKFUNC_CANTSEEK)
    }
    /// `CURLOPT_PROGRESSFUNCTION`: non-zero aborts.
    fn progress(&mut self, key: u64, dltotal: f64, dlnow: f64, ultotal: f64, ulnow: f64) -> i32 {
        let _ = (key, dltotal, dlnow, ultotal, ulnow);
        0
    }
    /// `CURLOPT_XFERINFOFUNCTION`: non-zero aborts.
    fn xferinfo(&mut self, key: u64, dltotal: i64, dlnow: i64, ultotal: i64, ulnow: i64) -> i32 {
        let _ = (key, dltotal, dlnow, ultotal, ulnow);
        0
    }
    /// `CURLOPT_FNMATCH_FUNCTION`.
    fn fnmatch(&mut self, key: u64, pattern: &[u8], string: &[u8]) -> i32 {
        let _ = (key, pattern, string);
        cint(CURL_FNMATCHFUNC_FAIL)
    }
    /// `CURLOPT_DEBUGFUNCTION` (`kind` is a `CURLINFO_TEXT`.. value).
    fn debug(&mut self, key: u64, kind: i32, data: &[u8]) {
        let _ = (key, kind, data);
    }
    /// `CURLOPT_PREREQFUNCTION`.
    fn prereq(
        &mut self,
        key: u64,
        primary_ip: &[u8],
        local_ip: &[u8],
        primary_port: i32,
        local_port: i32,
    ) -> i32 {
        let _ = (key, primary_ip, local_ip, primary_port, local_port);
        cint(CURL_PREREQFUNC_OK)
    }
    /// `CURLOPT_SSH_HOSTKEYFUNCTION`.
    fn ssh_hostkey(&mut self, key: u64, keytype: i32, hostkey: &[u8]) -> i32 {
        let _ = (key, keytype, hostkey);
        cint(CURLKHMATCH_MISMATCH)
    }
    /// `CURLMOPT_PUSHFUNCTION`: a server push for the transfer `parent`.
    /// Returns whether the push is accepted (the implementation then owns
    /// `child`). When denying, libcurl frees the handle itself, so the
    /// implementation must call [`Easy::disown`] on it.
    fn push(&mut self, parent: u64, child: Easy, headers: Vec<Vec<u8>>) -> bool {
        let _ = (parent, headers);
        child.disown();
        false
    }
}

/// A [`Handler`] using every default (for calls made without an embedding).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoHandler;

impl Handler for NoHandler {}

/// Narrows one of the generated `i64` constants to a C `int`.
pub(crate) fn cint(v: i64) -> c_int {
    c_int::try_from(v).unwrap_or(c_int::MAX)
}

thread_local! {
    /// Points at the `&mut dyn Handler` lent by the innermost
    /// [`with_handler`], or null.
    static CURRENT: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
}

/// Restores the previous slot value when a scope ends (also on unwind).
struct Restore(*mut c_void);

impl Drop for Restore {
    fn drop(&mut self) {
        CURRENT.with(|c| c.set(self.0));
    }
}

/// Runs `f` (a libcurl entry point) with `h` reachable from the callbacks.
pub(crate) fn with_handler<R>(h: &mut dyn Handler, f: impl FnOnce() -> R) -> R {
    let mut slot: &mut dyn Handler = h;
    let ptr = std::ptr::addr_of_mut!(slot).cast::<c_void>();
    let prev = CURRENT.with(|c| c.replace(ptr));
    let _restore = Restore(prev);
    f()
}

/// Calls `f` with the handler lent by the innermost [`with_handler`], or
/// returns `None` when no transfer is running.
fn dispatch<R>(f: impl FnOnce(&mut dyn Handler) -> R) -> Option<R> {
    let ptr = CURRENT.with(|c| c.replace(std::ptr::null_mut()));
    if ptr.is_null() {
        return None;
    }
    let _restore = Restore(ptr);
    // SAFETY: `ptr` was produced by `with_handler` from a live stack slot
    // holding a `&mut dyn Handler`; that frame is still active because the
    // slot is reset when it returns. The slot is nulled while `f` runs, so
    // this is the only access path to the handler (re-entrant callbacks see
    // no handler), and the borrow does not escape `f`.
    let slot = unsafe { &mut *ptr.cast::<&mut dyn Handler>() };
    Some(f(&mut **slot))
}

/// The handle key stored as callback user data.
fn key_of(userdata: *mut c_void) -> u64 {
    userdata as usize as u64
}

/// A byte slice from a (pointer, length) pair handed over by libcurl.
fn bytes<'a>(ptr: *const c_char, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: libcurl passes a buffer of at least `len` readable bytes that
    // stays valid for the duration of the callback.
    unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) }
}

/// The bytes of a NUL-terminated string handed over by libcurl.
fn cstr_bytes<'a>(ptr: *const c_char) -> &'a [u8] {
    if ptr.is_null() {
        return &[];
    }
    // SAFETY: libcurl passes a valid NUL-terminated string that outlives
    // the callback.
    unsafe { CStr::from_ptr(ptr) }.to_bytes()
}

pub(crate) extern "C" fn write_cb(
    ptr: *mut c_char,
    size: usize,
    nmemb: usize,
    ud: *mut c_void,
) -> usize {
    let len = size.saturating_mul(nmemb);
    let data = bytes(ptr, len);
    dispatch(|h| h.write(key_of(ud), data)).unwrap_or(len)
}

pub(crate) extern "C" fn header_cb(
    ptr: *mut c_char,
    size: usize,
    nmemb: usize,
    ud: *mut c_void,
) -> usize {
    let len = size.saturating_mul(nmemb);
    let data = bytes(ptr, len);
    dispatch(|h| h.header(key_of(ud), data)).unwrap_or(len)
}

pub(crate) extern "C" fn read_cb(
    ptr: *mut c_char,
    size: usize,
    nmemb: usize,
    ud: *mut c_void,
) -> usize {
    let len = size.saturating_mul(nmemb);
    if ptr.is_null() || len == 0 {
        return 0;
    }
    // SAFETY: libcurl hands over a writable buffer of `size * nmemb` bytes
    // that is not aliased during the callback.
    let buf = unsafe { std::slice::from_raw_parts_mut(ptr.cast::<u8>(), len) };
    dispatch(|h| h.read(key_of(ud), buf)).unwrap_or(0)
}

pub(crate) extern "C" fn seek_cb(ud: *mut c_void, offset: ffi::curl_off_t, origin: c_int) -> c_int {
    dispatch(|h| h.seek(key_of(ud), offset, origin)).unwrap_or(cint(CURL_SEEKFUNC_CANTSEEK))
}

pub(crate) extern "C" fn progress_cb(
    ud: *mut c_void,
    dt: c_double,
    dn: c_double,
    ut: c_double,
    un: c_double,
) -> c_int {
    dispatch(|h| h.progress(key_of(ud), dt, dn, ut, un)).unwrap_or(0)
}

pub(crate) extern "C" fn xferinfo_cb(
    ud: *mut c_void,
    dt: ffi::curl_off_t,
    dn: ffi::curl_off_t,
    ut: ffi::curl_off_t,
    un: ffi::curl_off_t,
) -> c_int {
    dispatch(|h| h.xferinfo(key_of(ud), dt, dn, ut, un)).unwrap_or(0)
}

pub(crate) extern "C" fn fnmatch_cb(
    ud: *mut c_void,
    pattern: *const c_char,
    string: *const c_char,
) -> c_int {
    let (p, s) = (cstr_bytes(pattern), cstr_bytes(string));
    dispatch(|h| h.fnmatch(key_of(ud), p, s)).unwrap_or(cint(CURL_FNMATCHFUNC_FAIL))
}

pub(crate) extern "C" fn debug_cb(
    _handle: *mut ffi::CURL,
    kind: c_int,
    data: *mut c_char,
    size: usize,
    ud: *mut c_void,
) -> c_int {
    let d = bytes(data, size);
    dispatch(|h| h.debug(key_of(ud), kind, d));
    0
}

pub(crate) extern "C" fn prereq_cb(
    ud: *mut c_void,
    primary_ip: *mut c_char,
    local_ip: *mut c_char,
    primary_port: c_int,
    local_port: c_int,
) -> c_int {
    let (p, l) = (cstr_bytes(primary_ip), cstr_bytes(local_ip));
    dispatch(|h| h.prereq(key_of(ud), p, l, primary_port, local_port))
        .unwrap_or(cint(CURL_PREREQFUNC_OK))
}

pub(crate) extern "C" fn sshhostkey_cb(
    ud: *mut c_void,
    keytype: c_int,
    key: *const c_char,
    keylen: usize,
) -> c_int {
    let k = bytes(key, keylen);
    dispatch(|h| h.ssh_hostkey(key_of(ud), keytype, k)).unwrap_or(cint(CURLKHMATCH_MISMATCH))
}

pub(crate) extern "C" fn push_cb(
    parent: *mut ffi::CURL,
    easy: *mut ffi::CURL,
    num_headers: usize,
    headers: *mut ffi::curl_pushheaders,
    _ud: *mut c_void,
) -> c_int {
    let Some(parent_key) = crate::easy::key_of_raw(parent) else {
        return cint(CURL_PUSH_DENY);
    };
    let list: Vec<Vec<u8>> = (0..num_headers)
        .map(|i| {
            // SAFETY: `headers` is the push-header set libcurl passed to
            // this callback and `i` is below `num_headers`.
            let p = unsafe { ffi::curl_pushheader_bynum(headers, i) };
            cstr_bytes(p).to_vec()
        })
        .collect();
    let accepted = dispatch(move |h| {
        let child = Easy::adopt(easy)?;
        Some(h.push(parent_key, child, list))
    });
    if accepted == Some(Some(true)) {
        cint(CURL_PUSH_OK)
    } else {
        cint(CURL_PUSH_DENY)
    }
}

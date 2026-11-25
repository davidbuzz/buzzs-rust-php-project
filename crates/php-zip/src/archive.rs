// Derived from php-src ext/zip/php_zip.c. Similarity metric to original: 7%
//   Copyright © The PHP Group and Contributors. Author: Piere-Alain Joye <pierre@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! The `ZipArchive` object (`ze_zip_object` + `php_zip_archive`) and every
//! method of the class (`PHP_METHOD(ZipArchive, …)` in `php_zip.c`).
//!
//! Arguments arrive already parsed by the VM (zpp types, `P` paths without
//! NUL bytes); each method performs the remaining checks in the exact order
//! of the C code, so the same exception wins when several apply. Methods
//! take `&self` and keep their state in cells, so PHP code running inside a
//! progress/cancel callback or an error handler may re-enter the same
//! object (and gets `Error: Already being closed` where PHP throws it).

// The libzip API takes PHP's zend_long arguments through C casts
// (`(zip_flags_t)flags`, `(zip_uint64_t)index`, …); these are mirrored with
// `as` on purpose.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString};
use std::ptr::{self, NonNull};
use std::rc::Rc;

use libc::{c_int, c_void};
use php_runtime::{convert, RtResult, RuntimeError, Value, ZArray};
use php_streams::stream::Stream;

use crate::consts;
use crate::error::{
    already_closing, arg_type_error, arg_value_error, deprecated, must_not_be_empty, uninitialized,
    warning,
};
use crate::ffi::{self, zip_error_t, zip_stat_t, zip_t};
use crate::host::ZipHost;
use crate::paths::{self, MAXPATHLEN};
use crate::source::{self, OutStr};
use crate::stream::ZipStreamOps;

/// `INT_MAX`.
const INT_MAX: i64 = i32::MAX as i64;
/// `USHRT_MAX`.
const USHRT_MAX: i64 = u16::MAX as i64;

/// Makes a C string from PHP bytes the way C sees a `char *` argument:
/// everything after the first NUL byte is ignored.
pub(crate) fn cstr(bytes: &[u8]) -> CString {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    CString::new(&bytes[..end]).unwrap_or_default()
}

/// Copies a NUL-terminated string returned by libzip.
///
/// # Safety
/// `p` must be NULL or point to a NUL-terminated string.
pub(crate) unsafe fn copy_cstr(p: *const libc::c_char) -> Option<Vec<u8>> {
    if p.is_null() {
        None
    } else {
        // SAFETY: guaranteed by the caller.
        Some(unsafe { CStr::from_ptr(p) }.to_bytes().to_vec())
    }
}

/// `zip_error_strerror()` of an error struct, copied.
///
/// # Safety
/// `err` must point to a valid `zip_error_t`.
pub(crate) unsafe fn error_string(err: *mut zip_error_t) -> String {
    // SAFETY: guaranteed by the caller; the returned string lives in `err`.
    let s = unsafe { copy_cstr(ffi::zip_error_strerror(err)) }.unwrap_or_default();
    String::from_utf8_lossy(&s).into_owned()
}

/// The message of a libzip error code pair (`zip_error_init` +
/// `zip_error_set` + `zip_error_strerror`).
pub fn error_message(zip_err: i32, sys_err: i32) -> String {
    let mut err = zip_error_t {
        zip_err: 0,
        sys_err: 0,
        str_: ptr::null_mut(),
    };
    // SAFETY: the struct is a valid, initialised stack error which is
    // finalised before returning.
    unsafe {
        ffi::zip_error_init(&raw mut err);
        ffi::zip_error_set(&raw mut err, zip_err, sys_err);
        let s = error_string(&raw mut err);
        ffi::zip_error_fini(&raw mut err);
        s
    }
}

/// Error codes of an archive (`zip_get_error()` + codes + fini).
fn archive_error_codes(za: *mut zip_t) -> (i32, i32) {
    // SAFETY: `za` is an open archive; the error struct belongs to it.
    unsafe {
        let err = ffi::zip_get_error(za);
        let codes = (
            ffi::zip_error_code_zip(err),
            ffi::zip_error_code_system(err),
        );
        ffi::zip_error_fini(err);
        codes
    }
}

/// The result of `ZipArchive::open()` / `openString()`: `true`, `false`
/// or a libzip error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenResult {
    /// `true`.
    Opened,
    /// `false` (after a warning, or an `open_basedir` refusal).
    False,
    /// The `ZipArchive::ER_*` code returned as `int`.
    Error(i64),
}

/// `ZipArchive::statIndex()` / `statName()` data (`struct zip_stat`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryStat {
    /// Entry name.
    pub name: Vec<u8>,
    /// Index in the archive.
    pub index: i64,
    /// CRC-32 of the uncompressed data.
    pub crc: i64,
    /// Uncompressed size.
    pub size: i64,
    /// Modification time.
    pub mtime: i64,
    /// Compressed size.
    pub comp_size: i64,
    /// `ZipArchive::CM_*`.
    pub comp_method: i64,
    /// `ZipArchive::EM_*`.
    pub encryption_method: i64,
}

impl EntryStat {
    fn from_raw(sb: &zip_stat_t) -> Self {
        EntryStat {
            // SAFETY: libzip fills `name` with a NUL-terminated string (or
            // NULL) owned by the archive.
            name: unsafe { copy_cstr(sb.name) }.unwrap_or_default(),
            index: sb.index as i64,
            crc: i64::from(sb.crc),
            size: sb.size as i64,
            mtime: sb.mtime,
            comp_size: sb.comp_size as i64,
            comp_method: i64::from(sb.comp_method),
            encryption_method: i64::from(sb.encryption_method),
        }
    }

    /// The PHP array (`RETURN_SB`), keys in PHP's order.
    pub fn to_array(&self) -> ZArray {
        let mut a = ZArray::new();
        let h = a.make_mut();
        h.insert("name", Value::from(&self.name[..]));
        h.insert("index", Value::Int(self.index));
        h.insert("crc", Value::Int(self.crc));
        h.insert("size", Value::Int(self.size));
        h.insert("mtime", Value::Int(self.mtime));
        h.insert("comp_size", Value::Int(self.comp_size));
        h.insert("comp_method", Value::Int(self.comp_method));
        h.insert("encryption_method", Value::Int(self.encryption_method));
        a
    }
}

fn empty_stat() -> zip_stat_t {
    zip_stat_t {
        valid: 0,
        name: ptr::null(),
        index: 0,
        size: 0,
        comp_size: 0,
        mtime: 0,
        crc: 0,
        comp_method: 0,
        encryption_method: 0,
        flags: 0,
    }
}

fn stat_index(za: *mut zip_t, index: i64, flags: i64) -> Option<zip_stat_t> {
    let mut sb = empty_stat();
    // SAFETY: `za` is an open archive and `sb` a valid output struct.
    let r = unsafe { ffi::zip_stat_index(za, index as u64, flags as u32, &raw mut sb) };
    (r == 0).then_some(sb)
}

fn stat_name(za: *mut zip_t, name: &[u8], flags: i64) -> Option<zip_stat_t> {
    let mut sb = empty_stat();
    let c = cstr(name);
    // SAFETY: `za` is an open archive, `c` a C string, `sb` a valid output.
    let r = unsafe { ffi::zip_stat(za, c.as_ptr(), flags as u32, &raw mut sb) };
    (r == 0).then_some(sb)
}

fn name_locate(za: *mut zip_t, name: &[u8], flags: i64) -> i64 {
    let c = cstr(name);
    // SAFETY: `za` is an open archive and `c` a C string.
    unsafe { ffi::zip_name_locate(za, c.as_ptr(), flags as u32) }
}

/// Shared native archive state (`php_zip_archive`): owned by the object
/// and by every stream opened from it.
pub(crate) struct Archive {
    za: Cell<*mut zip_t>,
    out_str: RefCell<Option<OutStr>>,
    from_string: Cell<bool>,
    bailout: Cell<bool>,
    closing: Cell<bool>,
    progress: RefCell<Option<Value>>,
    cancel: RefCell<Option<Value>>,
    /// The VM while `zip_close()` may call back into PHP.
    host: Cell<Option<NonNull<dyn ZipHost>>>,
    /// An exception (or bailout) raised by a callback, rethrown once the
    /// running operation returns (`EG(exception)`).
    pending: RefCell<Option<RuntimeError>>,
}

impl std::fmt::Debug for Archive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Archive")
            .field("open", &!self.za.get().is_null())
            .field("from_string", &self.from_string.get())
            .field("closing", &self.closing.get())
            .finish_non_exhaustive()
    }
}

/// Erases the lifetime of a host borrow so it can be stored while
/// `zip_close()` runs.
fn erase_host<'a>(host: &'a mut (dyn ZipHost + 'a)) -> NonNull<dyn ZipHost> {
    let p: *mut (dyn ZipHost + 'a) = host;
    // SAFETY: only the trait object's lifetime bound changes (identical
    // layout). The pointer is stored in `Archive::host` exclusively for the
    // duration of a `zip_close()` call made while the borrow is live (see
    // `Archive::zip_close_with`), and is cleared before that borrow ends.
    #[allow(clippy::transmute_ptr_to_ptr)] // `as` cannot change a trait object's lifetime bound
    let p: *mut (dyn ZipHost + 'static) = unsafe { std::mem::transmute(p) };
    // SAFETY: derived from a reference, hence non-null.
    unsafe { NonNull::new_unchecked(p) }
}

unsafe extern "C" fn progress_trampoline(_za: *mut zip_t, state: f64, ud: *mut c_void) {
    // SAFETY: `ud` is the `Archive` registered in
    // `ZipArchive::register_progress_callback`; it outlives its zip_t.
    let archive = unsafe { &*ud.cast::<Archive>() };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| archive.on_progress(state)));
}

unsafe extern "C" fn cancel_trampoline(_za: *mut zip_t, ud: *mut c_void) -> c_int {
    // SAFETY: as for `progress_trampoline`.
    let archive = unsafe { &*ud.cast::<Archive>() };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| archive.on_cancel())).unwrap_or(-1)
}

impl Archive {
    fn new(za: *mut zip_t) -> Rc<Self> {
        Rc::new(Archive {
            za: Cell::new(za),
            out_str: RefCell::new(None),
            from_string: Cell::new(false),
            bailout: Cell::new(false),
            closing: Cell::new(false),
            progress: RefCell::new(None),
            cancel: RefCell::new(None),
            host: Cell::new(None),
            pending: RefCell::new(None),
        })
    }

    pub(crate) fn za(&self) -> *mut zip_t {
        self.za.get()
    }

    fn set_pending(&self, e: RuntimeError) {
        let mut p = self.pending.borrow_mut();
        if p.is_none() {
            *p = Some(e);
        }
    }

    fn take_pending(&self) -> Option<RuntimeError> {
        self.pending.borrow_mut().take()
    }

    /// `php_zip_progress_callback()`.
    fn on_progress(&self, state: f64) {
        if self.bailout.get() || self.pending.borrow().is_some() {
            return;
        }
        let Some(mut host) = self.host.get() else {
            return;
        };
        let Some(cb) = self.progress.borrow().clone() else {
            return;
        };
        // SAFETY: `host` is set only while the borrow it came from is live
        // and nothing else uses it during the callback (see erase_host).
        let host = unsafe { host.as_mut() };
        match host.call(&cb, &[Value::Float(state)]) {
            Ok(_) => {}
            Err(e @ RuntimeError::Fatal(_)) => {
                self.bailout.set(true);
                self.set_pending(e);
            }
            Err(e) => self.set_pending(e),
        }
    }

    /// `php_zip_cancel_callback()`.
    fn on_cancel(&self) -> c_int {
        if self.bailout.get() {
            return 0;
        }
        let Some(mut host) = self.host.get() else {
            return 0;
        };
        if self.pending.borrow().is_some() {
            // zend_call_function() does not run with an exception pending;
            // the return value stays undefined, which cancels.
            return -1;
        }
        let Some(cb) = self.cancel.borrow().clone() else {
            return 0;
        };
        // SAFETY: see `on_progress`.
        let host = unsafe { host.as_mut() };
        match host.call(&cb, &[]) {
            Err(e @ RuntimeError::Fatal(_)) => {
                self.bailout.set(true);
                self.set_pending(e);
                -1
            }
            Err(e) => {
                self.set_pending(e);
                -1
            }
            Ok(v) => {
                let v = v.deref();
                match convert::try_get_long(&v, host) {
                    Ok(Some(n)) => n as c_int,
                    Ok(None) => {
                        self.set_pending(RuntimeError::TypeError(format!(
                            "Return value of callback provided to ZipArchive::registerCancelCallback() must be of type int, {} returned",
                            v.value_name()
                        )));
                        -1
                    }
                    Err(e) => {
                        self.set_pending(e);
                        -1
                    }
                }
            }
        }
    }

    /// `zip_close()` with the VM installed for the callbacks.
    fn zip_close_with(&self, host: Option<&mut dyn ZipHost>) -> c_int {
        let za = self.za.get();
        if za.is_null() {
            return 0;
        }
        let previous = self.host.replace(host.map(erase_host));
        // SAFETY: `za` is open; libzip frees it on success.
        let r = unsafe { ffi::zip_close(za) };
        self.host.set(previous);
        r
    }

    /// The part of `php_zip_archive_release()` run when the last reference
    /// goes away: closes a still open archive. Returns the bailout flag.
    fn finalize(&self, host: Option<&mut dyn ZipHost>) -> bool {
        let za = self.za.get();
        if !za.is_null() {
            self.closing.set(true);
            let err = self.zip_close_with(host);
            if err != 0 {
                if !self.bailout.get() {
                    // SAFETY: the failed zip_close() left `za` valid.
                    let msg = unsafe { copy_cstr(ffi::zip_strerror(za)) }.unwrap_or_default();
                    let mut m = b"Cannot destroy the zip context: ".to_vec();
                    m.extend_from_slice(&msg);
                    warning(m);
                }
                // SAFETY: `za` is still valid after the failed close.
                unsafe { ffi::zip_discard(za) };
            }
            self.za.set(ptr::null_mut());
        }
        self.progress.borrow_mut().take();
        self.cancel.borrow_mut().take();
        self.out_str.borrow_mut().take();
        self.bailout.get()
    }

    /// `php_zip_archive_release()`: drops one reference, closing the
    /// archive when it was the last one. Returns `(bailout, exception)`.
    pub(crate) fn release(
        this: Rc<Self>,
        host: Option<&mut dyn ZipHost>,
    ) -> (bool, Option<RuntimeError>) {
        let result = if Rc::strong_count(&this) == 1 {
            let bailout = this.finalize(host);
            (bailout, this.take_pending())
        } else {
            (false, None)
        };
        // Dropping the reference is the point of this function.
        drop(this);
        result
    }
}

impl Drop for Archive {
    fn drop(&mut self) {
        self.finalize(None);
    }
}

/// Files selected by `ZipArchive::extractTo()`'s second argument.
#[derive(Debug, Clone, Copy)]
pub enum ExtractFiles<'a> {
    /// `null`: every entry.
    All,
    /// A single entry name.
    One(&'a [u8]),
    /// An array of entry names.
    Many(&'a ZArray),
}

/// The parsed `$options` of `addGlob()` / `addPattern()` (`zip_options`).
#[derive(Debug, Clone)]
struct ZipOptions {
    remove_all_path: bool,
    remove_path: Option<Vec<u8>>,
    add_path: Option<Vec<u8>>,
    flags: u32,
    comp_method: i32,
    comp_flags: u32,
    enc_method: i16,
    enc_password: Option<Vec<u8>>,
}

impl Default for ZipOptions {
    fn default() -> Self {
        ZipOptions {
            remove_all_path: false,
            remove_path: None,
            add_path: None,
            flags: consts::FL_OVERWRITE as u32,
            comp_method: -1,
            comp_flags: 0,
            enc_method: -1,
            enc_password: None,
        }
    }
}

fn option_long(host: &mut dyn ZipHost, name: &str, v: &Value) -> RtResult<i64> {
    match convert::try_get_long(v, host)? {
        Some(n) => Ok(n),
        None => Err(RuntimeError::TypeError(format!(
            "Option \"{name}\" must be of type int, {} given",
            v.value_name()
        ))),
    }
}

fn option_path(name: &str, v: &Value) -> RtResult<Vec<u8>> {
    let Value::Str(s) = v else {
        return Err(RuntimeError::TypeError(format!(
            "Option \"{name}\" must be of type string, {} given",
            v.value_name()
        )));
    };
    if s.is_empty() {
        return Err(RuntimeError::ValueError(format!(
            "Option \"{name}\" must not be empty"
        )));
    }
    if s.len() >= MAXPATHLEN {
        return Err(RuntimeError::ValueError(format!(
            "Option \"{name}\" must be less than {} bytes",
            MAXPATHLEN - 1
        )));
    }
    Ok(s.as_bytes().to_vec())
}

/// `php_zip_parse_options()`.
fn parse_options(host: &mut dyn ZipHost, options: &ZArray) -> RtResult<ZipOptions> {
    let mut opts = ZipOptions::default();
    let get = |k: &str| options.get_str(k.as_bytes()).map(Value::deref);
    if let Some(v) = get("remove_all_path") {
        match v {
            Value::Bool(b) => opts.remove_all_path = b,
            other => {
                return Err(RuntimeError::TypeError(format!(
                    "Option \"remove_all_path\" must be of type bool, {} given",
                    other.value_name()
                )))
            }
        }
    }
    if let Some(v) = get("comp_method") {
        let m = option_long(host, "comp_method", &v)?;
        if !(0..=INT_MAX).contains(&m) {
            warning(format!(
                "Option \"comp_method\" must be between 0 and {INT_MAX}"
            ));
        }
        opts.comp_method = m as i32;
        if let Some(v) = get("comp_flags") {
            let f = option_long(host, "comp_flags", &v)?;
            if !(0..=USHRT_MAX).contains(&f) {
                warning(format!(
                    "Option \"comp_flags\" must be between 0 and {USHRT_MAX}"
                ));
            }
            opts.comp_flags = f as u32;
        }
    }
    if let Some(v) = get("enc_method") {
        opts.enc_method = option_long(host, "enc_method", &v)? as i16;
        if let Some(v) = get("enc_password") {
            let Value::Str(s) = &v else {
                return Err(RuntimeError::TypeError(format!(
                    "Option \"enc_password\" must be of type string, {} given",
                    v.value_name()
                )));
            };
            opts.enc_password = Some(s.as_bytes().to_vec());
        }
    }
    if let Some(v) = get("remove_path") {
        opts.remove_path = Some(option_path("remove_path", &v)?);
    }
    if let Some(v) = get("add_path") {
        opts.add_path = Some(option_path("add_path", &v)?);
    }
    if let Some(v) = get("flags") {
        opts.flags = option_long(host, "flags", &v)? as u32;
    }
    Ok(opts)
}

/// Outcome of `php_zipobj_close()`.
struct Closed {
    success: bool,
    out: Option<Vec<u8>>,
    bailout: bool,
    exception: Option<RuntimeError>,
}

/// A `ZipArchive` instance (`ze_zip_object`).
#[derive(Debug)]
pub struct ZipArchive {
    archive: RefCell<Option<Rc<Archive>>>,
    filename: RefCell<Option<Vec<u8>>>,
    last_id: Cell<i64>,
    err_zip: Cell<i32>,
    err_sys: Cell<i32>,
}

impl Default for ZipArchive {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ZipArchive {
    /// `php_zip_object_free_storage()`.
    fn drop(&mut self) {
        let _ = self.close_internal(None, false);
    }
}

/// The names of the virtual properties, in `var_dump()` order.
pub const PROPERTY_NAMES: [&str; 6] = [
    "lastId",
    "status",
    "statusSys",
    "numFiles",
    "filename",
    "comment",
];

/// Combines a result with an exception raised by a callback meanwhile.
fn with_pending<T>(r: T, exception: Option<RuntimeError>) -> RtResult<T> {
    match exception {
        Some(e) => Err(e),
        None => Ok(r),
    }
}

impl ZipArchive {
    /// `php_zip_object_new()`: a fresh, unopened object.
    pub fn new() -> Self {
        ZipArchive {
            archive: RefCell::new(None),
            filename: RefCell::new(None),
            last_id: Cell::new(-1),
            err_zip: Cell::new(0),
            err_sys: Cell::new(0),
        }
    }

    pub(crate) fn archive(&self) -> Option<Rc<Archive>> {
        self.archive.borrow().clone()
    }

    /// The open `zip_t` (`php_zip_object_za()`), or NULL.
    fn raw(&self) -> *mut zip_t {
        self.archive
            .borrow()
            .as_ref()
            .map_or(ptr::null_mut(), |a| a.za())
    }

    /// `ZIP_FROM_OBJECT`.
    fn za(&self) -> RtResult<*mut zip_t> {
        let za = self.raw();
        if za.is_null() {
            Err(uninitialized())
        } else {
            Ok(za)
        }
    }

    /// `php_zipobj_closing()`.
    fn check_closing(&self) -> RtResult<()> {
        match self.archive.borrow().as_ref() {
            Some(a) if a.closing.get() => Err(already_closing()),
            _ => Ok(()),
        }
    }

    /// Whether an archive is open.
    pub fn is_open(&self) -> bool {
        !self.raw().is_null()
    }

    /// `php_zipobj_close()`.
    fn close_internal(&self, mut host: Option<&mut dyn ZipHost>, want_out: bool) -> Closed {
        let archive = self.archive();
        let mut success = false;
        if let Some(a) = &archive {
            let za = a.za();
            if !za.is_null() {
                a.closing.set(true);
                let err = a.zip_close_with(host.as_mut().map(|h| &mut **h as &mut dyn ZipHost));
                if err == 0 {
                    a.za.set(ptr::null_mut());
                    self.err_zip.set(0);
                    self.err_sys.set(0);
                } else {
                    // SAFETY: the failed zip_close() left `za` valid.
                    let msg = unsafe { copy_cstr(ffi::zip_strerror(za)) }.unwrap_or_default();
                    warning(msg);
                    let (ze, se) = archive_error_codes(za);
                    self.err_zip.set(ze);
                    self.err_sys.set(se);
                    // SAFETY: `za` is still valid; discarding frees it.
                    unsafe { ffi::zip_discard(za) };
                    a.za.set(ptr::null_mut());
                }
                success = err == 0;
            }
        }
        if self.filename.borrow_mut().take().is_some() {
            php_streams::state::clear_stat_cache();
        }
        let mut out = None;
        let mut bailout = false;
        let mut exception = None;
        if let Some(a) = archive {
            if let Some(o) = a.out_str.borrow_mut().take() {
                if want_out {
                    out = Some(o.borrow().clone());
                }
            }
            a.za.set(ptr::null_mut());
            a.closing.set(false);
            bailout = a.bailout.replace(false);
            exception = a.take_pending();
            self.archive.borrow_mut().take();
            let (b, e) = Archive::release(a, host);
            bailout |= b;
            exception = exception.or(e);
        }
        Closed {
            success,
            out,
            bailout,
            exception,
        }
    }

    /// Closes the archive that is currently open before (re)opening one;
    /// returns the exception to rethrow afterwards.
    fn close_before_open(&self, host: &mut dyn ZipHost) -> RtResult<Option<RuntimeError>> {
        let closed = self.close_internal(Some(host), false);
        if closed.bailout {
            return Err(closed
                .exception
                .unwrap_or_else(|| RuntimeError::Fatal(String::new())));
        }
        Ok(closed.exception)
    }

    /// `ZipArchive::open(string $filename, int $flags = 0): bool|int`.
    pub fn open(
        &self,
        host: &mut dyn ZipHost,
        filename: &[u8],
        flags: i64,
    ) -> RtResult<OpenResult> {
        if filename.is_empty() {
            return Err(must_not_be_empty("ZipArchive::open", 1, "filename"));
        }
        if !php_streams::paths::check_open_basedir(filename) {
            return Ok(OpenResult::False);
        }
        let Some(resolved) = php_streams::paths::expand_filepath(filename) else {
            warning("No such file or directory");
            return Ok(OpenResult::False);
        };
        self.check_closing()?;
        let pending = self.close_before_open(host)?;
        let mut flags = flags;
        if flags & (consts::OVERWRITE | consts::RDONLY) == 0 {
            let path = std::path::Path::new(std::ffi::OsStr::from_bytes_compat(&resolved));
            if std::fs::metadata(path).is_ok_and(|m| m.len() == 0) {
                deprecated("Using empty file as ZipArchive is deprecated");
                flags |= consts::OVERWRITE;
            }
        }
        let c = cstr(&resolved);
        let mut err: c_int = 0;
        // SAFETY: `c` is a C string and `err` a valid output.
        let za = unsafe { ffi::zip_open(c.as_ptr(), flags as c_int, &raw mut err) };
        if za.is_null() || err != 0 {
            return with_pending(OpenResult::Error(i64::from(err)), pending);
        }
        *self.filename.borrow_mut() = Some(resolved);
        *self.archive.borrow_mut() = Some(Archive::new(za));
        with_pending(OpenResult::Opened, pending)
    }

    /// `ZipArchive::openString(string $data = '', int $flags = 0): bool|int`.
    pub fn open_string(
        &self,
        host: &mut dyn ZipHost,
        data: &[u8],
        flags: i64,
    ) -> RtResult<OpenResult> {
        self.check_closing()?;
        let pending = self.close_before_open(host)?;
        let out: OutStr = Rc::new(RefCell::new(Vec::new()));
        let src = match source::create(data, Some(Rc::clone(&out))) {
            Ok(src) => src,
            Err((ze, se)) => {
                self.err_zip.set(ze);
                self.err_sys.set(se);
                return with_pending(OpenResult::Error(i64::from(ze)), pending);
            }
        };
        let mut err = zip_error_t {
            zip_err: 0,
            sys_err: 0,
            str_: ptr::null_mut(),
        };
        // SAFETY: `src` is a fresh source; `err` a valid stack error which is
        // finalised on every path.
        unsafe {
            ffi::zip_error_init(&raw mut err);
            let za = ffi::zip_open_from_source(src, flags as c_int, &raw mut err);
            if za.is_null() {
                self.err_zip.set(ffi::zip_error_code_zip(&raw const err));
                self.err_sys.set(ffi::zip_error_code_system(&raw const err));
                ffi::zip_error_fini(&raw mut err);
                ffi::zip_source_free(src);
                return with_pending(OpenResult::Error(i64::from(self.err_zip.get())), pending);
            }
            ffi::zip_error_fini(&raw mut err);
            let archive = Archive::new(za);
            *archive.out_str.borrow_mut() = Some(out);
            archive.from_string.set(true);
            *self.archive.borrow_mut() = Some(archive);
        }
        with_pending(OpenResult::Opened, pending)
    }

    /// `ZipArchive::setPassword(string $password): bool`.
    pub fn set_password(&self, password: &[u8]) -> RtResult<bool> {
        let za = self.za()?;
        self.check_closing()?;
        if password.is_empty() {
            return Ok(false);
        }
        let c = cstr(password);
        // SAFETY: open archive, C string copied by libzip.
        Ok(unsafe { ffi::zip_set_default_password(za, c.as_ptr()) } == 0)
    }

    fn finish_close(closed: Closed) -> RtResult<Closed> {
        if closed.bailout {
            return Err(closed
                .exception
                .unwrap_or_else(|| RuntimeError::Fatal(String::new())));
        }
        if let Some(e) = closed.exception {
            return Err(e);
        }
        Ok(closed)
    }

    /// `ZipArchive::close(): bool`.
    pub fn close(&self, host: &mut dyn ZipHost) -> RtResult<bool> {
        self.za()?;
        self.check_closing()?;
        Ok(Self::finish_close(self.close_internal(Some(host), false))?.success)
    }

    /// `ZipArchive::closeString(): string|false` (`None` is `false`).
    pub fn close_string(&self, host: &mut dyn ZipHost) -> RtResult<Option<Vec<u8>>> {
        self.za()?;
        if !self.archive().is_some_and(|a| a.from_string.get()) {
            return Err(RuntimeError::Error(
                "ZipArchive::closeString can only be called on an archive opened with ZipArchive::openString"
                    .to_string(),
            ));
        }
        self.check_closing()?;
        let closed = Self::finish_close(self.close_internal(Some(host), true))?;
        Ok(if closed.success { closed.out } else { None })
    }

    /// `php_zip_object_dtor()`: run when the object is destroyed (before
    /// it is freed); releases the archive, closing it with callbacks.
    pub fn destroy(&self, host: &mut dyn ZipHost) -> RtResult<()> {
        let Some(a) = self.archive() else {
            return Ok(());
        };
        // Like the C code, the object keeps pointing at the archive while
        // it is released, so callbacks re-entering the object see it as
        // "being closed" rather than uninitialised.
        let (bailout, exception) = if Rc::strong_count(&a) == 2 {
            let bailout = a.finalize(Some(host));
            (bailout, a.take_pending())
        } else {
            (false, None)
        };
        self.archive.borrow_mut().take();
        drop(a);
        if bailout {
            return Err(exception.unwrap_or_else(|| RuntimeError::Fatal(String::new())));
        }
        match exception {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// `php_zip_object_free_storage()` with the VM available: closes the
    /// archive like `close()` would (warning on failure).
    pub fn free(&self, host: &mut dyn ZipHost) -> RtResult<()> {
        Self::finish_close(self.close_internal(Some(host), false)).map(|_| ())
    }

    /// `ZipArchive::count(): int`.
    pub fn count(&self) -> RtResult<i64> {
        let za = self.za()?;
        // SAFETY: open archive.
        Ok(unsafe { ffi::zip_get_num_entries(za, 0) })
    }

    /// `ZipArchive::getStatusString(): string`.
    pub fn get_status_string(&self) -> String {
        let za = self.raw();
        if za.is_null() {
            error_message(self.err_zip.get(), self.err_sys.get())
        } else {
            // SAFETY: open archive; its error struct is finalised after use.
            unsafe {
                let err = ffi::zip_get_error(za);
                let s = error_string(err);
                ffi::zip_error_fini(err);
                s
            }
        }
    }

    /// `ZipArchive::clearError(): void`.
    pub fn clear_error(&self) {
        let za = self.raw();
        if za.is_null() {
            self.err_zip.set(0);
            self.err_sys.set(0);
        } else {
            // SAFETY: open archive.
            unsafe { ffi::zip_error_clear(za) };
        }
    }

    /// `ZipArchive::addEmptyDir(string $dirname, int $flags = 0): bool`.
    pub fn add_empty_dir(&self, dirname: &[u8], flags: i64) -> RtResult<bool> {
        let za = self.za()?;
        self.check_closing()?;
        if dirname.is_empty() {
            return Ok(false);
        }
        let mut s = dirname.to_vec();
        if s.last() != Some(&b'/') {
            s.push(b'/');
        }
        let c = cstr(&s);
        // SAFETY: open archive, C string.
        let id = unsafe { ffi::zip_dir_add(za, c.as_ptr(), flags as u32) };
        self.last_id.set(id);
        if id == -1 {
            return Ok(false);
        }
        // SAFETY: open archive.
        unsafe { ffi::zip_error_clear(za) };
        Ok(true)
    }

    /// `php_zip_add_file()`; `Ok(false)` is `FAILURE`.
    #[allow(clippy::too_many_arguments)]
    fn add_file_internal(
        &self,
        filename: &[u8],
        entry_name: &[u8],
        start: i64,
        length: i64,
        replace: i64,
        flags: i64,
    ) -> bool {
        let za = self.raw();
        if !php_streams::paths::check_open_basedir(filename) {
            return false;
        }
        let resolved = match php_streams::paths::expand_filepath(filename) {
            Some(r) if r.len() < MAXPATHLEN => r,
            _ => {
                warning("No such file or directory");
                return false;
            }
        };
        if php_streams::open::stat_path(&resolved, php_streams::wrapper::URL_STAT_QUIET, None)
            .is_none()
        {
            warning("No such file or directory");
            return false;
        }
        let mut flags = flags as u32;
        let c = cstr(&resolved);
        let zs = if flags & (consts::FL_OPEN_FILE_NOW as u32) != 0 {
            // SAFETY: C strings for path and mode.
            let fd = unsafe { libc::fopen(c.as_ptr(), c"rb".as_ptr()) };
            if fd.is_null() {
                return false;
            }
            flags ^= consts::FL_OPEN_FILE_NOW as u32;
            let zs = if za.is_null() {
                ptr::null_mut()
            } else {
                // SAFETY: open archive and FILE; libzip owns `fd` on success.
                unsafe { ffi::zip_source_filep(za, fd, start as u64, length) }
            };
            if zs.is_null() {
                // SAFETY: `fd` was not taken over.
                unsafe { libc::fclose(fd) };
                return false;
            }
            zs
        } else {
            if za.is_null() {
                return false;
            }
            // SAFETY: open archive and C string.
            let zs = unsafe { ffi::zip_source_file(za, c.as_ptr(), start as u64, length) };
            if zs.is_null() {
                return false;
            }
            zs
        };
        if replace >= 0 {
            // SAFETY: open archive and fresh source.
            if unsafe { ffi::zip_file_replace(za, replace as u64, zs, flags) } < 0 {
                // SAFETY: the source was not taken over.
                unsafe { ffi::zip_source_free(zs) };
                return false;
            }
            // SAFETY: open archive.
            unsafe { ffi::zip_error_clear(za) };
            return true;
        }
        let name = cstr(entry_name);
        // SAFETY: open archive, C string and fresh source.
        let id = unsafe { ffi::zip_file_add(za, name.as_ptr(), zs, flags) };
        self.last_id.set(id);
        if id < 0 {
            // SAFETY: the source was not taken over.
            unsafe { ffi::zip_source_free(zs) };
            return false;
        }
        // SAFETY: open archive.
        unsafe { ffi::zip_error_clear(za) };
        true
    }

    /// `ZipArchive::addFile(string $filepath, string $entryname = "", int
    /// $start = 0, int $length = ZipArchive::LENGTH_TO_END, int $flags =
    /// ZipArchive::FL_OVERWRITE): bool`.
    pub fn add_file(
        &self,
        filepath: &[u8],
        entryname: &[u8],
        start: i64,
        length: i64,
        flags: i64,
    ) -> RtResult<bool> {
        if filepath.is_empty() {
            return Err(must_not_be_empty("ZipArchive::addFile", 1, "filepath"));
        }
        let entry = if entryname.is_empty() {
            filepath
        } else {
            entryname
        };
        self.check_closing()?;
        Ok(self.add_file_internal(filepath, entry, start, length, -1, flags))
    }

    /// `ZipArchive::replaceFile(string $filepath, int $index, int $start =
    /// 0, int $length = ZipArchive::LENGTH_TO_END, int $flags = 0): bool`.
    pub fn replace_file(
        &self,
        filepath: &[u8],
        index: i64,
        start: i64,
        length: i64,
        flags: i64,
    ) -> RtResult<bool> {
        if filepath.is_empty() {
            return Err(must_not_be_empty("ZipArchive::replaceFile", 1, "filepath"));
        }
        if index < 0 {
            return Err(arg_value_error(
                "ZipArchive::replaceFile",
                2,
                "index",
                "must be greater than or equal to 0",
            ));
        }
        self.check_closing()?;
        Ok(self.add_file_internal(filepath, b"", start, length, index, flags))
    }

    /// `ZipArchive::addFromString(string $name, string $content, int $flags
    /// = ZipArchive::FL_OVERWRITE): bool`.
    pub fn add_from_string(&self, name: &[u8], content: &[u8], flags: i64) -> RtResult<bool> {
        let za = self.za()?;
        self.check_closing()?;
        let Ok(zs) = source::create(content, None) else {
            return Ok(false);
        };
        let c = cstr(name);
        // SAFETY: open archive, C string and fresh source.
        let id = unsafe { ffi::zip_file_add(za, c.as_ptr(), zs, flags as u32) };
        self.last_id.set(id);
        if id == -1 {
            // SAFETY: the source was not taken over.
            unsafe { ffi::zip_source_free(zs) };
            return Ok(false);
        }
        // SAFETY: open archive.
        unsafe { ffi::zip_error_clear(za) };
        Ok(true)
    }

    /// Adds the files found by `addGlob()` / `addPattern()`.
    fn add_found(&self, files: Vec<Vec<u8>>, opts: &ZipOptions) -> Option<Vec<Vec<u8>>> {
        let za = self.raw();
        for file in &files {
            let stripped: &[u8] = if opts.remove_all_path {
                paths::basename(file)
            } else if let Some(rp) = opts
                .remove_path
                .as_deref()
                .filter(|rp| file.len() > rp.len() && file.starts_with(rp))
            {
                if file[rp.len()] == b'/' {
                    &file[rp.len() + 1..]
                } else {
                    &file[rp.len()..]
                }
            } else {
                file
            };
            let entry: Vec<u8> = if let Some(ap) = &opts.add_path {
                if ap.len() + stripped.len() > MAXPATHLEN {
                    warning(format!(
                        "Entry name too long (max: {}, {} given)",
                        MAXPATHLEN - 1,
                        ap.len() + stripped.len()
                    ));
                    return None;
                }
                [ap.as_slice(), stripped].concat()
            } else {
                stripped.to_vec()
            };
            if !self.add_file_internal(file, &entry, 0, 0, -1, i64::from(opts.flags)) {
                return None;
            }
            let id = self.last_id.get();
            if opts.comp_method >= 0 {
                // SAFETY: the add above succeeded, so `za` is open.
                let r = unsafe {
                    ffi::zip_set_file_compression(za, id as u64, opts.comp_method, opts.comp_flags)
                };
                if r != 0 {
                    return None;
                }
            }
            if opts.enc_method >= 0
                && !set_encryption(
                    za,
                    id,
                    i64::from(opts.enc_method),
                    opts.enc_password.as_deref(),
                )
            {
                return None;
            }
        }
        Some(files)
    }

    /// `ZipArchive::addGlob(string $pattern, int $flags = 0, array $options
    /// = []): array|false` (`None` is `false`).
    pub fn add_glob(
        &self,
        host: &mut dyn ZipHost,
        pattern: &[u8],
        flags: i64,
        options: Option<&ZArray>,
    ) -> RtResult<Option<Vec<Vec<u8>>>> {
        if pattern.is_empty() {
            return Err(must_not_be_empty("ZipArchive::addGlob", 1, "pattern"));
        }
        let opts = match options {
            Some(o) if !o.is_empty() => parse_options(host, o)?,
            _ => ZipOptions::default(),
        };
        self.check_closing()?;
        Ok(match crate::pattern::glob(pattern, flags) {
            Some(files) => self.add_found(files, &opts),
            None => None,
        })
    }

    /// `ZipArchive::addPattern(string $pattern, string $path = ".", array
    /// $options = []): array|false` (`None` is `false`).
    pub fn add_pattern(
        &self,
        host: &mut dyn ZipHost,
        pattern: &[u8],
        path: &[u8],
        options: Option<&ZArray>,
    ) -> RtResult<Option<Vec<Vec<u8>>>> {
        if pattern.is_empty() {
            return Err(must_not_be_empty("ZipArchive::addPattern", 1, "pattern"));
        }
        let opts = match options {
            Some(o) if !o.is_empty() => parse_options(host, o)?,
            _ => ZipOptions::default(),
        };
        self.check_closing()?;
        Ok(match crate::pattern::pcre(host, pattern, path) {
            Some(files) => self.add_found(files, &opts),
            None => None,
        })
    }

    /// `ZipArchive::renameIndex(int $index, string $new_name): bool`.
    pub fn rename_index(&self, index: i64, new_name: &[u8]) -> RtResult<bool> {
        if index < 0 {
            return Ok(false);
        }
        if new_name.is_empty() {
            return Err(must_not_be_empty("ZipArchive::renameIndex", 2, "new_name"));
        }
        let za = self.za()?;
        self.check_closing()?;
        let c = cstr(new_name);
        // SAFETY: open archive and C string.
        Ok(unsafe { ffi::zip_file_rename(za, index as u64, c.as_ptr(), 0) } == 0)
    }

    /// `ZipArchive::renameName(string $name, string $new_name): bool`.
    pub fn rename_name(&self, name: &[u8], new_name: &[u8]) -> RtResult<bool> {
        let za = self.za()?;
        self.check_closing()?;
        if new_name.is_empty() {
            return Err(must_not_be_empty("ZipArchive::renameName", 2, "new_name"));
        }
        if name.is_empty() {
            return Err(must_not_be_empty("ZipArchive::renameName", 1, "name"));
        }
        let Some(sb) = stat_name(za, name, 0) else {
            return Ok(false);
        };
        let c = cstr(new_name);
        // SAFETY: open archive and C string.
        Ok(unsafe { ffi::zip_file_rename(za, sb.index, c.as_ptr(), 0) } == 0)
    }

    /// `ZipArchive::setArchiveComment(string $comment): bool`.
    pub fn set_archive_comment(&self, comment: &[u8]) -> RtResult<bool> {
        let za = self.za()?;
        self.check_closing()?;
        if comment.len() > 0xffff {
            return Err(arg_value_error(
                "ZipArchive::setArchiveComment",
                1,
                "comment",
                "must be less than 65535 bytes",
            ));
        }
        // SAFETY: open archive; `comment` is valid for its length.
        Ok(unsafe {
            ffi::zip_set_archive_comment(za, comment.as_ptr().cast(), comment.len() as u16)
        } == 0)
    }

    /// `ZipArchive::getArchiveComment(int $flags = 0): string|false`.
    pub fn get_archive_comment(&self, flags: i64) -> RtResult<Option<Vec<u8>>> {
        let za = self.za()?;
        Ok(archive_comment(za, flags as c_int as u32))
    }

    /// `ZipArchive::setArchiveFlag(int $flag, int $value): bool`.
    pub fn set_archive_flag(&self, flag: i64, value: i64) -> RtResult<bool> {
        let za = self.za()?;
        self.check_closing()?;
        // SAFETY: open archive.
        Ok(unsafe { ffi::zip_set_archive_flag(za, flag as u32, value as c_int) } == 0)
    }

    /// `ZipArchive::getArchiveFlag(int $flag, int $flags = 0): int`.
    pub fn get_archive_flag(&self, flag: i64, flags: i64) -> RtResult<i64> {
        let za = self.za()?;
        // SAFETY: open archive.
        Ok(i64::from(unsafe {
            ffi::zip_get_archive_flag(za, flag as u32, flags as u32)
        }))
    }

    /// `ZipArchive::setCommentIndex(int $index, string $comment): bool`.
    pub fn set_comment_index(&self, index: i64, comment: &[u8]) -> RtResult<bool> {
        let za = self.za()?;
        self.check_closing()?;
        if comment.len() > 0xffff {
            return Err(arg_value_error(
                "ZipArchive::setCommentIndex",
                2,
                "comment",
                "must be less than 65535 bytes",
            ));
        }
        if stat_index(za, index, 0).is_none() {
            return Ok(false);
        }
        Ok(set_file_comment(za, index as u64, comment))
    }

    /// `ZipArchive::setCommentName(string $name, string $comment): bool`.
    pub fn set_comment_name(&self, name: &[u8], comment: &[u8]) -> RtResult<bool> {
        if name.is_empty() {
            return Err(must_not_be_empty("ZipArchive::setCommentName", 1, "name"));
        }
        let za = self.za()?;
        self.check_closing()?;
        if comment.len() > 0xffff {
            return Err(arg_value_error(
                "ZipArchive::setCommentName",
                2,
                "comment",
                "must be less than 65535 bytes",
            ));
        }
        let idx = name_locate(za, name, 0);
        if idx < 0 {
            return Ok(false);
        }
        Ok(set_file_comment(za, idx as u64, comment))
    }

    /// `ZipArchive::setMtimeIndex(int $index, int $timestamp, int $flags =
    /// 0): bool`.
    pub fn set_mtime_index(&self, index: i64, timestamp: i64, flags: i64) -> RtResult<bool> {
        let za = self.za()?;
        self.check_closing()?;
        // SAFETY: open archive.
        Ok(unsafe { ffi::zip_file_set_mtime(za, index as u64, timestamp, flags as u32) } == 0)
    }

    /// `ZipArchive::setMtimeName(string $name, int $timestamp, int $flags =
    /// 0): bool`.
    pub fn set_mtime_name(&self, name: &[u8], timestamp: i64, flags: i64) -> RtResult<bool> {
        if name.is_empty() {
            return Err(must_not_be_empty("ZipArchive::setMtimeName", 1, "name"));
        }
        let za = self.za()?;
        self.check_closing()?;
        let idx = name_locate(za, name, 0);
        if idx < 0 {
            return Ok(false);
        }
        // SAFETY: open archive.
        Ok(unsafe { ffi::zip_file_set_mtime(za, idx as u64, timestamp, flags as u32) } == 0)
    }

    /// `ZipArchive::getCommentIndex(int $index, int $flags = 0):
    /// string|false`.
    pub fn get_comment_index(&self, index: i64, flags: i64) -> RtResult<Option<Vec<u8>>> {
        let za = self.za()?;
        if stat_index(za, index, 0).is_none() {
            return Ok(None);
        }
        Ok(Some(file_comment(za, index as u64, flags as u32)))
    }

    /// `ZipArchive::getCommentName(string $name, int $flags = 0):
    /// string|false`.
    pub fn get_comment_name(&self, name: &[u8], flags: i64) -> RtResult<Option<Vec<u8>>> {
        let za = self.za()?;
        if name.is_empty() {
            return Err(must_not_be_empty("ZipArchive::getCommentName", 1, "name"));
        }
        // PHP stores the index in an `int`.
        let idx = name_locate(za, name, 0) as c_int;
        if idx < 0 {
            return Ok(None);
        }
        Ok(Some(file_comment(za, idx as u64, flags as u32)))
    }

    /// `ZipArchive::deleteIndex(int $index): bool`.
    pub fn delete_index(&self, index: i64) -> RtResult<bool> {
        let za = self.za()?;
        self.check_closing()?;
        if index < 0 {
            return Ok(false);
        }
        // SAFETY: open archive.
        Ok(unsafe { ffi::zip_delete(za, index as u64) } == 0)
    }

    /// `ZipArchive::deleteName(string $name): bool`.
    pub fn delete_name(&self, name: &[u8]) -> RtResult<bool> {
        if name.is_empty() {
            return Ok(false);
        }
        let za = self.za()?;
        self.check_closing()?;
        let Some(sb) = stat_name(za, name, 0) else {
            return Ok(false);
        };
        // SAFETY: open archive.
        Ok(unsafe { ffi::zip_delete(za, sb.index) } == 0)
    }

    /// `ZipArchive::statName(string $name, int $flags = 0): array|false`.
    pub fn stat_name(&self, name: &[u8], flags: i64) -> RtResult<Option<EntryStat>> {
        let za = self.za()?;
        if name.is_empty() {
            return Err(must_not_be_empty("ZipArchive::statName", 1, "name"));
        }
        Ok(stat_name(za, name, flags).as_ref().map(EntryStat::from_raw))
    }

    /// `ZipArchive::statIndex(int $index, int $flags = 0): array|false`.
    pub fn stat_index(&self, index: i64, flags: i64) -> RtResult<Option<EntryStat>> {
        let za = self.za()?;
        Ok(stat_index(za, index, flags)
            .as_ref()
            .map(EntryStat::from_raw))
    }

    /// `ZipArchive::locateName(string $name, int $flags = 0): int|false`.
    pub fn locate_name(&self, name: &[u8], flags: i64) -> RtResult<Option<i64>> {
        let za = self.za()?;
        if name.is_empty() {
            return Ok(None);
        }
        let idx = name_locate(za, name, flags);
        Ok((idx >= 0).then_some(idx))
    }

    /// `ZipArchive::getNameIndex(int $index, int $flags = 0): string|false`.
    pub fn get_name_index(&self, index: i64, flags: i64) -> RtResult<Option<Vec<u8>>> {
        let za = self.za()?;
        // SAFETY: open archive; the returned name is copied immediately.
        Ok(unsafe { copy_cstr(ffi::zip_get_name(za, index as u64, flags as u32)) })
    }

    /// `ZipArchive::unchangeArchive(): bool`.
    pub fn unchange_archive(&self) -> RtResult<bool> {
        let za = self.za()?;
        self.check_closing()?;
        // SAFETY: open archive.
        Ok(unsafe { ffi::zip_unchange_archive(za) } == 0)
    }

    /// `ZipArchive::unchangeAll(): bool`.
    pub fn unchange_all(&self) -> RtResult<bool> {
        let za = self.za()?;
        self.check_closing()?;
        // SAFETY: open archive.
        Ok(unsafe { ffi::zip_unchange_all(za) } == 0)
    }

    /// `ZipArchive::unchangeIndex(int $index): bool`.
    pub fn unchange_index(&self, index: i64) -> RtResult<bool> {
        if index < 0 {
            return Ok(false);
        }
        let za = self.za()?;
        self.check_closing()?;
        // SAFETY: open archive.
        Ok(unsafe { ffi::zip_unchange(za, index as u64) } == 0)
    }

    /// `ZipArchive::unchangeName(string $name): bool`.
    pub fn unchange_name(&self, name: &[u8]) -> RtResult<bool> {
        if name.is_empty() {
            return Ok(false);
        }
        let za = self.za()?;
        self.check_closing()?;
        let Some(sb) = stat_name(za, name, 0) else {
            return Ok(false);
        };
        // SAFETY: open archive.
        Ok(unsafe { ffi::zip_unchange(za, sb.index) } == 0)
    }

    /// `ZipArchive::extractTo(string $pathto, array|string|null $files =
    /// null): bool`.
    pub fn extract_to(&self, pathto: &[u8], files: ExtractFiles<'_>) -> RtResult<bool> {
        if pathto.is_empty() {
            return Ok(false);
        }
        if php_streams::open::stat_path(pathto, php_streams::wrapper::URL_STAT_QUIET, None)
            .is_none()
            && !php_streams::open::mkdir(pathto, 0o777, php_streams::wrapper::MKDIR_RECURSIVE, None)
        {
            return Ok(false);
        }
        let za = self.za()?;
        match files {
            ExtractFiles::One(name) => Ok(crate::extract::extract_file(za, pathto, name, -1)),
            ExtractFiles::Many(list) => {
                if list.is_empty() {
                    return Ok(false);
                }
                for v in list.values() {
                    match v.deref() {
                        Value::Str(s) => {
                            if !crate::extract::extract_file(za, pathto, s.as_bytes(), -1) {
                                return Ok(false);
                            }
                        }
                        other => {
                            return Err(arg_type_error(
                                "ZipArchive::extractTo",
                                2,
                                "files",
                                &format!(
                                    "must only have elements of type string, {} given",
                                    other.value_name()
                                ),
                            ))
                        }
                    }
                }
                Ok(true)
            }
            ExtractFiles::All => {
                // SAFETY: open archive.
                let count = unsafe { ffi::zip_get_num_entries(za, 0) };
                if count == -1 {
                    warning("Illegal archive");
                    return Ok(false);
                }
                for i in 0..count {
                    // SAFETY: open archive; the name is copied immediately.
                    let name = unsafe {
                        copy_cstr(ffi::zip_get_name(za, i as u64, consts::FL_UNCHANGED as u32))
                    };
                    match name {
                        Some(n) if crate::extract::extract_file(za, pathto, &n, i) => {}
                        _ => return Ok(false),
                    }
                }
                Ok(true)
            }
        }
    }

    /// `php_zip_get_from()`.
    fn get_from(
        za: *mut zip_t,
        name: Option<&[u8]>,
        index: i64,
        sb: &zip_stat_t,
        len: i64,
        flags: i64,
    ) -> Option<Vec<u8>> {
        let len = if len < 1 { sb.size } else { len as u64 };
        let zf = match name {
            Some(n) => {
                let c = cstr(n);
                // SAFETY: open archive and C string.
                unsafe { ffi::zip_fopen(za, c.as_ptr(), flags as u32) }
            }
            // SAFETY: open archive.
            None => unsafe { ffi::zip_fopen_index(za, index as u64, flags as u32) },
        };
        if zf.is_null() {
            return None;
        }
        let file = crate::stream::ZipFile(zf);
        let mut buf: Vec<u8> = Vec::new();
        let mut failed = false;
        let mut chunk = vec![0u8; 8192];
        while (buf.len() as u64) < len {
            let want = usize::try_from((len - buf.len() as u64).min(chunk.len() as u64))
                .unwrap_or(chunk.len());
            match file.read(&mut chunk[..want]) {
                None => {
                    failed = true;
                    break;
                }
                Some(0) => break,
                Some(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        if !failed && buf.len() as u64 == sb.size {
            // Read past the last byte so libzip validates the CRC.
            let mut one = [0u8; 1];
            if file.read(&mut one).is_none() {
                failed = true;
            }
        }
        if failed {
            let mut m = b"Cannot read entry: ".to_vec();
            m.extend_from_slice(file.error_message().as_bytes());
            warning(m);
            return None;
        }
        Some(buf)
    }

    /// `ZipArchive::getFromName(string $name, int $len = 0, int $flags =
    /// 0): string|false`.
    pub fn get_from_name(&self, name: &[u8], len: i64, flags: i64) -> RtResult<Option<Vec<u8>>> {
        let za = self.za()?;
        if name.is_empty() {
            return Err(must_not_be_empty("ZipArchive::getFromName", 1, "name"));
        }
        let Some(sb) = stat_name(za, name, flags) else {
            return Ok(None);
        };
        Ok(Self::get_from(za, Some(name), -1, &sb, len, flags))
    }

    /// `ZipArchive::getFromIndex(int $index, int $len = 0, int $flags = 0):
    /// string|false`.
    pub fn get_from_index(&self, index: i64, len: i64, flags: i64) -> RtResult<Option<Vec<u8>>> {
        let za = self.za()?;
        let Some(sb) = stat_index(za, index, flags) else {
            return Ok(None);
        };
        if index >= 0 {
            Ok(Self::get_from(za, None, index, &sb, len, flags))
        } else {
            // zip_fopen() with the (unset) name of the C code.
            Ok(None)
        }
    }

    /// `php_zip_get_stream()` after the lookup.
    fn open_stream(&self, sb: &zip_stat_t, flags: i64) -> Option<Stream> {
        let archive = self.archive()?;
        let za = archive.za();
        if za.is_null() {
            return None;
        }
        // SAFETY: open archive.
        let zf = unsafe { ffi::zip_fopen_index(za, sb.index, flags as u32) };
        if zf.is_null() {
            return None;
        }
        let ops = ZipStreamOps::for_archive(zf, archive);
        let mut stream = Stream::new(Box::new(ops), "rb");
        // SAFETY: libzip's stat name is a valid C string (or NULL).
        stream.orig_path = unsafe { copy_cstr(sb.name) };
        Some(stream)
    }

    /// `ZipArchive::getStreamName(string $name, int $flags = 0)`: a read
    /// stream (`None` is `false`).
    pub fn get_stream_name(&self, name: &[u8], flags: i64) -> RtResult<Option<Stream>> {
        let za = self.za()?;
        self.check_closing()?;
        if name.is_empty() {
            return Err(must_not_be_empty("ZipArchive::getStreamName", 1, "name"));
        }
        let Some(sb) = stat_name(za, name, flags) else {
            return Ok(None);
        };
        Ok(self.open_stream(&sb, flags))
    }

    /// `ZipArchive::getStream(string $name)`.
    pub fn get_stream(&self, name: &[u8]) -> RtResult<Option<Stream>> {
        let za = self.za()?;
        self.check_closing()?;
        if name.is_empty() {
            return Err(must_not_be_empty("ZipArchive::getStream", 1, "name"));
        }
        let Some(sb) = stat_name(za, name, 0) else {
            return Ok(None);
        };
        Ok(self.open_stream(&sb, 0))
    }

    /// `ZipArchive::getStreamIndex(int $index, int $flags = 0)`.
    pub fn get_stream_index(&self, index: i64, flags: i64) -> RtResult<Option<Stream>> {
        let za = self.za()?;
        self.check_closing()?;
        let Some(sb) = stat_index(za, index, flags) else {
            return Ok(None);
        };
        Ok(self.open_stream(&sb, flags))
    }

    /// `ZipArchive::setExternalAttributesName(string $name, int $opsys, int
    /// $attr, int $flags = 0): bool`.
    pub fn set_external_attributes_name(
        &self,
        name: &[u8],
        opsys: i64,
        attr: i64,
        flags: i64,
    ) -> RtResult<bool> {
        let za = self.za()?;
        self.check_closing()?;
        if name.is_empty() {
            return Err(must_not_be_empty(
                "ZipArchive::setExternalAttributesName",
                1,
                "name",
            ));
        }
        let idx = name_locate(za, name, 0);
        // SAFETY: open archive.
        Ok(idx >= 0
            && unsafe {
                ffi::zip_file_set_external_attributes(
                    za,
                    idx as u64,
                    flags as u32,
                    (opsys & 0xff) as u8,
                    attr as u32,
                )
            } >= 0)
    }

    /// `ZipArchive::setExternalAttributesIndex(int $index, int $opsys, int
    /// $attr, int $flags = 0): bool`.
    pub fn set_external_attributes_index(
        &self,
        index: i64,
        opsys: i64,
        attr: i64,
        flags: i64,
    ) -> RtResult<bool> {
        let za = self.za()?;
        self.check_closing()?;
        if stat_index(za, index, 0).is_none() {
            return Ok(false);
        }
        // SAFETY: open archive.
        Ok(unsafe {
            ffi::zip_file_set_external_attributes(
                za,
                index as u64,
                flags as u32,
                (opsys & 0xff) as u8,
                attr as u32,
            )
        } == 0)
    }

    /// `ZipArchive::getExternalAttributesName(string $name, &$opsys, &$attr,
    /// int $flags = 0): bool` — `Some((opsys, attr))` for `true` (the VM
    /// assigns the references).
    pub fn get_external_attributes_name(
        &self,
        name: &[u8],
        flags: i64,
    ) -> RtResult<Option<(i64, i64)>> {
        let za = self.za()?;
        if name.is_empty() {
            return Err(must_not_be_empty(
                "ZipArchive::getExternalAttributesName",
                1,
                "name",
            ));
        }
        let idx = name_locate(za, name, 0);
        if idx < 0 {
            return Ok(None);
        }
        Ok(external_attributes(za, idx as u64, flags as u32))
    }

    /// `ZipArchive::getExternalAttributesIndex(int $index, &$opsys, &$attr,
    /// int $flags = 0): bool`.
    pub fn get_external_attributes_index(
        &self,
        index: i64,
        flags: i64,
    ) -> RtResult<Option<(i64, i64)>> {
        let za = self.za()?;
        if stat_index(za, index, 0).is_none() {
            return Ok(None);
        }
        Ok(external_attributes(za, index as u64, flags as u32))
    }

    /// `ZipArchive::setCompressionName(string $name, int $method, int
    /// $compflags = 0): bool`.
    pub fn set_compression_name(&self, name: &[u8], method: i64, compflags: i64) -> RtResult<bool> {
        const F: &str = "ZipArchive::setCompressionName";
        if name.is_empty() {
            return Err(must_not_be_empty(F, 1, "name"));
        }
        check_compression_args(F, method, compflags)?;
        let za = self.za()?;
        self.check_closing()?;
        let idx = name_locate(za, name, 0);
        if idx < 0 {
            return Ok(false);
        }
        // SAFETY: open archive.
        Ok(unsafe {
            ffi::zip_set_file_compression(za, idx as u64, method as i32, compflags as u32)
        } == 0)
    }

    /// `ZipArchive::setCompressionIndex(int $index, int $method, int
    /// $compflags = 0): bool`.
    pub fn set_compression_index(&self, index: i64, method: i64, compflags: i64) -> RtResult<bool> {
        if index < 0 {
            return Ok(false);
        }
        check_compression_args("ZipArchive::setCompressionIndex", method, compflags)?;
        let za = self.za()?;
        self.check_closing()?;
        // SAFETY: open archive.
        Ok(unsafe {
            ffi::zip_set_file_compression(za, index as u64, method as i32, compflags as u32)
        } == 0)
    }

    /// `ZipArchive::setEncryptionName(string $name, int $method, ?string
    /// $password = null): bool`.
    pub fn set_encryption_name(
        &self,
        name: &[u8],
        method: i64,
        password: Option<&[u8]>,
    ) -> RtResult<bool> {
        let za = self.za()?;
        self.check_closing()?;
        if name.is_empty() {
            return Err(must_not_be_empty(
                "ZipArchive::setEncryptionName",
                1,
                "name",
            ));
        }
        let idx = name_locate(za, name, 0);
        if idx < 0 {
            return Ok(false);
        }
        Ok(set_encryption(za, idx, method, password))
    }

    /// `ZipArchive::setEncryptionIndex(int $index, int $method, ?string
    /// $password = null): bool`.
    pub fn set_encryption_index(
        &self,
        index: i64,
        method: i64,
        password: Option<&[u8]>,
    ) -> RtResult<bool> {
        let za = self.za()?;
        self.check_closing()?;
        Ok(set_encryption(za, index, method, password))
    }

    /// `ZipArchive::registerProgressCallback(float $rate, callable
    /// $callback): bool`.
    pub fn register_progress_callback(&self, rate: f64, callback: Value) -> RtResult<bool> {
        let za = self.za()?;
        self.check_closing()?;
        let Some(archive) = self.archive() else {
            return Err(uninitialized());
        };
        // SAFETY: open archive; the state pointer is the Archive, which
        // outlives its zip_t (it is closed in finalize/close_internal).
        let r = unsafe {
            ffi::zip_register_progress_callback_with_state(
                za,
                rate,
                Some(progress_trampoline),
                None,
                Rc::as_ptr(&archive).cast_mut().cast(),
            )
        };
        if r != 0 {
            return Ok(false);
        }
        *archive.progress.borrow_mut() = Some(callback);
        Ok(true)
    }

    /// `ZipArchive::registerCancelCallback(callable $callback): bool`.
    pub fn register_cancel_callback(&self, callback: Value) -> RtResult<bool> {
        let za = self.za()?;
        self.check_closing()?;
        let Some(archive) = self.archive() else {
            return Err(uninitialized());
        };
        // SAFETY: as for register_progress_callback.
        let r = unsafe {
            ffi::zip_register_cancel_callback_with_state(
                za,
                Some(cancel_trampoline),
                None,
                Rc::as_ptr(&archive).cast_mut().cast(),
            )
        };
        if r != 0 {
            return Ok(false);
        }
        *archive.cancel.borrow_mut() = Some(callback);
        Ok(true)
    }

    /// `ZipArchive::isCompressionMethodSupported(int $method, bool $enc =
    /// true): bool`.
    pub fn is_compression_method_supported(method: i64, enc: bool) -> RtResult<bool> {
        if !(-1..=INT_MAX).contains(&method) {
            return Err(arg_value_error(
                "ZipArchive::isCompressionMethodSupported",
                1,
                "method",
                &format!("must be between -1 and {INT_MAX}"),
            ));
        }
        Ok(consts::compression_method_supported(method as i32, enc))
    }

    /// `ZipArchive::isEncryptionMethodSupported(int $method, bool $enc =
    /// true): bool`.
    pub fn is_encryption_method_supported(method: i64, enc: bool) -> RtResult<bool> {
        if !(0..=USHRT_MAX).contains(&method) {
            return Err(arg_value_error(
                "ZipArchive::isEncryptionMethodSupported",
                1,
                "method",
                &format!("must be between 0 and {USHRT_MAX}"),
            ));
        }
        Ok(consts::encryption_method_supported(method as u16, enc))
    }

    /// `ZipArchive::__serialize()` always throws.
    pub fn serialize(class: &str) -> RuntimeError {
        crate::error::serialize_not_allowed(class)
    }

    /// `ZipArchive::__unserialize()` always throws.
    pub fn unserialize(class: &str) -> RuntimeError {
        crate::error::unserialize_not_allowed(class)
    }

    /// The `lastId` property.
    pub fn last_id(&self) -> i64 {
        self.last_id.get()
    }

    /// The `status` property (`php_zip_status()`).
    pub fn status(&self) -> i64 {
        let za = self.raw();
        if za.is_null() {
            i64::from(self.err_zip.get())
        } else {
            i64::from(archive_error_codes(za).0)
        }
    }

    /// The `statusSys` property.
    pub fn status_sys(&self) -> i64 {
        let za = self.raw();
        if za.is_null() {
            i64::from(self.err_sys.get())
        } else {
            i64::from(archive_error_codes(za).1)
        }
    }

    /// The `numFiles` property.
    pub fn num_files(&self) -> i64 {
        let za = self.raw();
        if za.is_null() {
            0
        } else {
            // SAFETY: open archive.
            unsafe { ffi::zip_get_num_entries(za, 0) }
        }
    }

    /// The `filename` property (the resolved path of the open archive).
    pub fn filename(&self) -> Vec<u8> {
        self.filename.borrow().clone().unwrap_or_default()
    }

    /// The `comment` property.
    pub fn comment(&self) -> Vec<u8> {
        let za = self.raw();
        if za.is_null() {
            Vec::new()
        } else {
            archive_comment(za, 0).unwrap_or_default()
        }
    }

    /// Reads a virtual property (`php_zip_read_property()`); `None` when
    /// `name` is not one of [`PROPERTY_NAMES`].
    pub fn read_property(&self, name: &[u8]) -> Option<Value> {
        Some(match name {
            b"lastId" => Value::Int(self.last_id()),
            b"status" => Value::Int(self.status()),
            b"statusSys" => Value::Int(self.status_sys()),
            b"numFiles" => Value::Int(self.num_files()),
            b"filename" => Value::from(&self.filename()[..]),
            b"comment" => Value::from(&self.comment()[..]),
            _ => return None,
        })
    }

    /// `php_zip_has_property()` for a virtual property: with `not_empty`
    /// (`ZEND_PROPERTY_NOT_EMPTY`, used by `empty()`) whether the value is
    /// truthy, otherwise (`isset()` / `property_exists()`) always true.
    /// `None` for other names.
    pub fn has_property(&self, name: &[u8], not_empty: bool) -> Option<bool> {
        let v = self.read_property(name)?;
        Some(if not_empty {
            match v {
                Value::Int(i) => i != 0,
                Value::Str(s) => !(s.is_empty() || s.as_bytes() == b"0"),
                _ => false,
            }
        } else {
            true
        })
    }

    /// Whether `name` is a virtual (read-only) property.
    pub fn is_virtual_property(name: &[u8]) -> bool {
        PROPERTY_NAMES.iter().any(|p| p.as_bytes() == name)
    }

    /// The virtual properties in `get_properties` order (`var_dump()`,
    /// casts, `get_object_vars()`).
    pub fn properties(&self) -> Vec<(&'static str, Value)> {
        PROPERTY_NAMES
            .iter()
            .filter_map(|n| self.read_property(n.as_bytes()).map(|v| (*n, v)))
            .collect()
    }
}

fn check_compression_args(func: &str, method: i64, compflags: i64) -> RtResult<()> {
    if !(-1..=INT_MAX).contains(&method) {
        return Err(arg_value_error(
            func,
            2,
            "method",
            &format!("must be between -1 and {INT_MAX}"),
        ));
    }
    if !(0..=USHRT_MAX).contains(&compflags) {
        return Err(arg_value_error(
            func,
            3,
            "compflags",
            &format!("must be between 0 and {USHRT_MAX}"),
        ));
    }
    Ok(())
}

fn archive_comment(za: *mut zip_t, flags: u32) -> Option<Vec<u8>> {
    let mut len: c_int = 0;
    // SAFETY: open archive; the comment is copied immediately.
    let p = unsafe { ffi::zip_get_archive_comment(za, &raw mut len, flags) };
    if p.is_null() {
        return None;
    }
    let n = usize::try_from(len).unwrap_or(0);
    // SAFETY: libzip returns `len` readable bytes.
    Some(unsafe { std::slice::from_raw_parts(p.cast::<u8>(), n) }.to_vec())
}

fn file_comment(za: *mut zip_t, index: u64, flags: u32) -> Vec<u8> {
    let mut len: u32 = 0;
    // SAFETY: open archive; the comment is copied immediately.
    let p = unsafe { ffi::zip_file_get_comment(za, index, &raw mut len, flags) };
    if p.is_null() {
        return Vec::new();
    }
    // SAFETY: libzip returns `len` readable bytes.
    unsafe { std::slice::from_raw_parts(p.cast::<u8>(), len as usize) }.to_vec()
}

/// `php_zip_set_file_comment()`.
fn set_file_comment(za: *mut zip_t, index: u64, comment: &[u8]) -> bool {
    let mut len: u32 = 0;
    // SAFETY: open archive.
    let cur =
        unsafe { ffi::zip_file_get_comment(za, index, &raw mut len, consts::FL_ENC_RAW as u32) };
    if !cur.is_null() && len as usize == comment.len() {
        // SAFETY: libzip returns `len` readable bytes.
        let cur = unsafe { std::slice::from_raw_parts(cur.cast::<u8>(), len as usize) };
        if cur == comment {
            return true;
        }
    }
    if comment.is_empty() {
        // SAFETY: open archive; NULL removes the comment.
        return unsafe { ffi::zip_file_set_comment(za, index, ptr::null(), 0, 0) } == 0;
    }
    // SAFETY: open archive; `comment` is valid for its length (<= 0xffff).
    unsafe {
        ffi::zip_file_set_comment(za, index, comment.as_ptr().cast(), comment.len() as u16, 0) == 0
    }
}

fn external_attributes(za: *mut zip_t, index: u64, flags: u32) -> Option<(i64, i64)> {
    let mut opsys: u8 = 0;
    let mut attr: u32 = 0;
    // SAFETY: open archive and valid output pointers.
    let r = unsafe {
        ffi::zip_file_get_external_attributes(za, index, flags, &raw mut opsys, &raw mut attr)
    };
    (r >= 0).then(|| (i64::from(opsys), i64::from(attr)))
}

/// `php_zip_file_set_encryption()`.
fn set_encryption(za: *mut zip_t, index: i64, method: i64, password: Option<&[u8]>) -> bool {
    // SAFETY: open archive; resetting with EM_NONE and no password.
    if unsafe { ffi::zip_file_set_encryption(za, index as u64, 0, ptr::null()) } < 0 {
        warning("password reset failed");
        return false;
    }
    let pw = password.map(cstr);
    let pw_ptr = pw.as_ref().map_or(ptr::null(), |c| c.as_ptr());
    // SAFETY: open archive; the password is copied by libzip.
    unsafe { ffi::zip_file_set_encryption(za, index as u64, method as u16, pw_ptr) == 0 }
}

/// `OsStr` from raw bytes (Unix).
trait OsStrBytes {
    fn from_bytes_compat(b: &[u8]) -> &std::ffi::OsStr;
}

impl OsStrBytes for std::ffi::OsStr {
    fn from_bytes_compat(b: &[u8]) -> &std::ffi::OsStr {
        std::os::unix::ffi::OsStrExt::from_bytes(b)
    }
}

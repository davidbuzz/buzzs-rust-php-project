// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! TLS session resumption: the `session_data`, `session_new_cb`,
//! `session_get_cb`, `session_remove_cb`, `session_cache*`,
//! `session_timeout` and `session_id_context` options.
//!
//! OpenSSL callbacks must be `Send + Sync`, so the PHP callables live in a
//! thread-local registry and the closures only capture its index. They run
//! inside `php_ext_streams::bridge::run` (handshakes and reads), which
//! lends them the VM.

use std::cell::{Cell, RefCell};

use openssl::ssl::{SslContextBuilder, SslSessionCacheMode};
use php_runtime::Value;
use php_streams::diag::StreamErrorCode as Code;
use php_vm::ext::{Throw, VmResult};
use php_vm::Vm;

use super::opts::SslOpts;
use super::sock::warn;

/// The PHP callables of one `SSL_CTX` (`php_openssl_session_callbacks_t`).
#[allow(clippy::struct_field_names)]
#[derive(Debug, Default, Clone)]
struct Callbacks {
    new_cb: Option<Value>,
    get_cb: Option<Value>,
    remove_cb: Option<Value>,
}

thread_local! {
    static REGISTRY: RefCell<Vec<Callbacks>> = const { RefCell::new(Vec::new()) };
    /// The id of the stream whose handshake or I/O is running.
    static CURRENT: Cell<u64> = const { Cell::new(0) };
    /// An exception raised while configuring (`zend_type_error` & co.):
    /// thrown once the transport has reported its failure.
    static PENDING: RefCell<Option<Throw>> = const { RefCell::new(None) };
    /// A PHP callback was installed in the context being built.
    static CALLBACKS_SET: Cell<bool> = const { Cell::new(false) };
}

/// Notes that a PHP callback (session, PSK or early data) was installed
/// in the context being built.
pub(crate) fn note_callbacks() {
    CALLBACKS_SET.with(|c| c.set(true));
}

/// Whether a callback was installed since the last call (which resets it).
pub(crate) fn callbacks_set() -> bool {
    CALLBACKS_SET.with(|c| c.replace(false))
}

/// Records the stream id for callbacks fired during I/O on it.
pub(crate) fn set_current(id: u64) {
    CURRENT.with(|c| c.set(id));
}

/// Remembers a configuration exception.
pub(crate) fn set_pending(t: Throw) {
    PENDING.with(|p| {
        let mut p = p.borrow_mut();
        if p.is_none() {
            *p = Some(t);
        }
    });
}

/// Takes the configuration exception, if any.
pub(crate) fn take_pending() -> Option<Throw> {
    PENDING.with(|p| p.borrow_mut().take())
}

/// Forgets this request's callables ([`super::reset_request`]).
pub(super) fn reset_request() {
    REGISTRY.with(|r| r.borrow_mut().clear());
    CURRENT.with(|c| c.set(0));
}

fn register(cbs: Callbacks) -> usize {
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        r.push(cbs);
        r.len() - 1
    })
}

fn callback(id: usize, pick: fn(&Callbacks) -> Option<Value>) -> Option<Value> {
    REGISTRY.with(|r| r.borrow().get(id).and_then(pick))
}

/// The stream resource of the running operation (`stream->res`).
pub(crate) fn current_stream() -> Value {
    let id = CURRENT.with(Cell::get);
    php_ext_streams::res::stream_value_by_id(id).unwrap_or(Value::Null)
}

/// Calls a PHP callback with the lent VM; exceptions become pending in
/// the bridge.
fn call(cb: &Value, args: Vec<Value>) -> Option<Value> {
    php_ext_streams::bridge::with_vm(|vm| match vm.call_value(cb, args) {
        Ok(v) => Some(v),
        Err(t) => {
            php_ext_streams::bridge::set_pending(t);
            None
        }
    })
    .flatten()
}

/// Calls a PHP callback of the running operation with
/// `PHP_STREAM_FLAG_NO_FCLOSE` set on its stream: `fclose()` refuses the
/// stream the callback gets (it is in use).
pub(crate) fn call_guarded(vm: &mut Vm, cb: &Value, args: Vec<Value>) -> VmResult<Value> {
    let id = CURRENT.with(Cell::get);
    php_streams::user::without_fclose(id, || vm.call_value(cb, args))
}

/// [`call`] with [`call_guarded`]'s flag.
fn call_no_fclose(cb: &Value, args: Vec<Value>) -> Option<Value> {
    php_ext_streams::bridge::with_vm(|vm| match call_guarded(vm, cb, args) {
        Ok(v) => Some(v),
        Err(t) => {
            php_ext_streams::bridge::set_pending(t);
            None
        }
    })
    .flatten()
}

/// `php_openssl_validate_and_allocate_session_callback`.
fn validate(vm: &mut Vm, name: &str, cb: &Value, persistent: bool) -> Option<Value> {
    if persistent {
        warn(
            Code::PersistentNotSupported,
            format!("{name} is not supported for persistent streams"),
        );
        return None;
    }
    if !vm.is_callable(cb) {
        let detail = match cb {
            Value::Str(s) => format!(
                "function \"{}\" not found or invalid function name",
                s.to_string_lossy()
            ),
            Value::Array(_) => "array callback must have exactly two members".to_string(),
            _ => "no array or string given".to_string(),
        };
        let t = vm.throw(
            "TypeError",
            &format!("{name} must be a valid callback, {detail}"),
        );
        set_pending(t);
        return None;
    }
    Some(cb.clone())
}

fn new_session_cb(b: &mut SslContextBuilder, id: usize) {
    b.set_new_session_callback(move |_ssl, session| {
        let Some(cb) = callback(id, |c| c.new_cb.clone()) else {
            return;
        };
        let stream = current_stream();
        let obj =
            php_ext_streams::bridge::with_vm(|vm| crate::session::session_value(vm, session).ok())
                .flatten();
        if let Some(obj) = obj {
            let _ = call_no_fclose(&cb, vec![stream, obj]);
        }
    });
}

/// `php_openssl_setup_client_session`; `false` on failure.
pub(crate) fn setup_client(
    vm: &mut Vm,
    b: &mut SslContextBuilder,
    opts: &SslOpts,
    persistent: bool,
) -> VmResult<bool> {
    let mut enable_cache = false;
    if let Some(v) = opts.get("session_data") {
        if crate::session::session_of(&v).is_some() {
            enable_cache = true;
        } else if !matches!(v, Value::Null) {
            set_pending(vm.throw(
                "TypeError",
                "session_data must be an Openssl\\Session instance",
            ));
            return Ok(false);
        }
    }
    if let Some(v) = opts.get("session_new_cb") {
        let Some(cb) = validate(vm, "session_new_cb", &v, persistent) else {
            return Ok(false);
        };
        let id = register(Callbacks {
            new_cb: Some(cb),
            ..Callbacks::default()
        });
        note_callbacks();
        new_session_cb(b, id);
        enable_cache = true;
    }
    if enable_cache {
        b.set_session_cache_mode(SslSessionCacheMode::CLIENT | SslSessionCacheMode::NO_INTERNAL);
    }
    Ok(true)
}

/// `php_openssl_is_session_cache_enabled`.
pub(crate) fn cache_enabled(vm: &mut Vm, opts: &SslOpts, internal_only: bool) -> VmResult<bool> {
    if let Some(v) = opts.bool(vm, "session_cache")? {
        return Ok(v);
    }
    Ok(!internal_only && opts.get("session_get_cb").is_some())
}

/// `SSL_CTX_set_timeout`.
fn set_timeout(b: &mut SslContextBuilder, secs: i64) {
    extern "C" {
        fn SSL_CTX_set_timeout(ctx: *mut openssl_sys::SSL_CTX, t: libc::c_long) -> libc::c_long;
    }
    // SAFETY: `b` owns a live SSL_CTX; the setter only stores the value.
    unsafe {
        SSL_CTX_set_timeout(
            b.as_ptr(),
            libc::c_long::try_from(secs).unwrap_or(libc::c_long::MAX),
        );
    }
}

/// `php_openssl_setup_server_session`; `false` on failure.
#[allow(clippy::too_many_lines)]
pub(crate) fn setup_server(
    vm: &mut Vm,
    b: &mut SslContextBuilder,
    opts: &SslOpts,
    persistent: bool,
    verify_peer: bool,
) -> VmResult<bool> {
    let mut cbs = Callbacks::default();
    if let Some(v) = opts.get("session_get_cb") {
        let Some(cb) = validate(vm, "session_get_cb", &v, persistent) else {
            return Ok(false);
        };
        cbs.get_cb = Some(cb);
    }
    let mut has_sid_ctx = false;
    if let Some(v) = opts.get("session_id_context") {
        let Value::Str(s) = &v else {
            set_pending(vm.throw("TypeError", "session_id_context must be a non empty string"));
            return Ok(false);
        };
        if s.is_empty() {
            set_pending(vm.throw("TypeError", "session_id_context must be a non empty string"));
            return Ok(false);
        }
        let _ = b.set_session_id_context(s.as_bytes());
        has_sid_ctx = true;
    }
    if let Some(v) = opts.get("session_new_cb") {
        let Some(cb) = validate(vm, "session_new_cb", &v, persistent) else {
            return Ok(false);
        };
        cbs.new_cb = Some(cb);
        if !has_sid_ctx && verify_peer {
            set_pending(vm.throw(
                "ValueError",
                "session_id_context must be set if session_new_cb is set",
            ));
            return Ok(false);
        }
    }
    if cbs.get_cb.is_some() && cbs.new_cb.is_none() {
        set_pending(vm.throw(
            "ValueError",
            "session_new_cb is required when session_get_cb is provided",
        ));
        return Ok(false);
    }
    if let Some(v) = opts.get("session_remove_cb") {
        let Some(cb) = validate(vm, "session_remove_cb", &v, persistent) else {
            return Ok(false);
        };
        cbs.remove_cb = Some(cb);
    }
    let has_get = cbs.get_cb.is_some();
    let has_new = cbs.new_cb.is_some();
    let has_remove = cbs.remove_cb.is_some();
    let id = register(cbs);
    if has_get || has_new || has_remove {
        note_callbacks();
    }
    if has_get {
        b.set_session_cache_mode(SslSessionCacheMode::SERVER | SslSessionCacheMode::NO_INTERNAL);
        new_session_cb(b, id);
        // SAFETY: the returned sessions were created by connections of this
        // context (PHP hands back what `session_new_cb` stored).
        unsafe {
            b.set_get_session_callback(move |_ssl, sid| {
                let cb = callback(id, |c| c.get_cb.clone())?;
                let stream = current_stream();
                let sid = Value::Str(php_runtime::ZStr::from(sid));
                let ret = call_no_fclose(&cb, vec![stream, sid])?;
                if let Some(s) = crate::session::session_of(&ret) {
                    s
                } else {
                    if !matches!(ret, Value::Null) {
                        php_ext_streams::bridge::with_vm(|vm| {
                            let t = vm.throw(
                                "TypeError",
                                "session_get_cb return type must be null or Openssl\\Session",
                            );
                            php_ext_streams::bridge::set_pending(t);
                        });
                    }
                    None
                }
            });
        }
        if has_remove {
            b.set_remove_session_callback(move |_ctx, session| {
                let Some(cb) = callback(id, |c| c.remove_cb.clone()) else {
                    return;
                };
                let sid = Value::Str(php_runtime::ZStr::from(session.id()));
                let _ = call(&cb, vec![current_stream(), sid]);
            });
        }
        b.set_options(openssl::ssl::SslOptions::NO_TICKET);
        if opts.bool(vm, "no_ticket")? == Some(false) {
            set_pending(vm.throw(
                "ValueError",
                "Session tickets cannot be enabled when session_get_cb is set",
            ));
        }
    } else if cache_enabled(vm, opts, true)? {
        if !has_sid_ctx && verify_peer {
            set_pending(vm.throw(
                "ValueError",
                "session_id_context must be set for internal session cache",
            ));
        }
        b.set_session_cache_mode(SslSessionCacheMode::SERVER);
        match opts.long(vm, "session_cache_size")? {
            Some(n) if n > 0 => {
                b.set_session_cache_size(i32::try_from(n).unwrap_or(i32::MAX));
            }
            Some(_) => set_pending(vm.throw("ValueError", "session_cache_size must be positive")),
            None => {
                b.set_session_cache_size(20480);
            }
        }
        match opts.long(vm, "session_timeout")? {
            Some(t) if t > 0 => set_timeout(b, t),
            Some(_) => set_pending(vm.throw("ValueError", "session_timeout must be positive")),
            None => set_timeout(b, 300),
        }
        if has_new {
            new_session_cb(b, id);
        }
    } else {
        b.set_session_cache_mode(SslSessionCacheMode::OFF);
    }
    Ok(true)
}

/// `php_openssl_apply_client_session_data`; `false` on failure.
pub(crate) fn apply_client_data(
    vm: &mut Vm,
    ssl: &mut openssl::ssl::Ssl,
    opts: &SslOpts,
) -> VmResult<bool> {
    let Some(v) = opts.get("session_data") else {
        return Ok(true);
    };
    match crate::session::session_of(&v) {
        Some(Some(session)) => {
            // SAFETY: OpenSSL accepts sessions of other contexts for
            // clients; a mismatch only prevents resumption.
            if unsafe { ssl.set_session(&session) }.is_err() {
                warn(
                    Code::ResumptionFailed,
                    "Failed to set session for resumption, falling back to full handshake",
                );
                return Ok(false);
            }
            Ok(true)
        }
        Some(None) => {
            warn(
                Code::Generic,
                "Invalid Openssl\\Session object, falling back to full handshake",
            );
            Ok(false)
        }
        None if matches!(v, Value::Null) => Ok(true),
        None => {
            set_pending(vm.throw(
                "TypeError",
                "session_data must be an Openssl\\Session instance",
            ));
            Ok(false)
        }
    }
}

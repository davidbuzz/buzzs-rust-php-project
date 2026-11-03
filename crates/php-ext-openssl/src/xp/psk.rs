//! Pre-shared keys: the `psk_client_cb` / `psk_server_cb` options for
//! TLS 1.2 (`SSL_CTX_set_psk_*_callback`) and TLS 1.3
//! (`SSL_CTX_set_psk_use_session_callback` /
//! `SSL_CTX_set_psk_find_session_callback`, which the `openssl` crate does
//! not wrap, hence the small FFI block below).

use std::cell::RefCell;
use std::collections::HashMap;

use foreign_types::ForeignTypeRef;
use openssl::error::ErrorStack;
use openssl::ssl::SslContextBuilder;
use openssl_sys::{EVP_MD, SSL, SSL_CIPHER, SSL_SESSION};
use php_runtime::{Value, ZStr};
use php_streams::diag::StreamErrorCode as Code;
use php_vm::ext::VmResult;
use php_vm::Vm;

use super::opts::SslOpts;
use super::sock::warn;

/// `PHP_OPENSSL_PSK_MAX_IDENTITY_LEN`.
const MAX_IDENTITY_LEN: usize = 128;
/// `TLS1_3_VERSION`.
const TLS1_3_VERSION: libc::c_int = 0x0304;
/// TLS 1.3 cipher suite ids.
const TLS13_AES128_GCM_SHA256: [u8; 2] = [0x13, 0x01];
const TLS13_AES256_GCM_SHA384: [u8; 2] = [0x13, 0x02];

type UseSessionCb = unsafe extern "C" fn(
    *mut SSL,
    *const EVP_MD,
    *mut *const u8,
    *mut usize,
    *mut *mut SSL_SESSION,
) -> libc::c_int;
type FindSessionCb =
    unsafe extern "C" fn(*mut SSL, *const u8, usize, *mut *mut SSL_SESSION) -> libc::c_int;

extern "C" {
    fn SSL_CTX_set_psk_use_session_callback(
        ctx: *mut openssl_sys::SSL_CTX,
        cb: Option<UseSessionCb>,
    );
    fn SSL_CTX_set_psk_find_session_callback(
        ctx: *mut openssl_sys::SSL_CTX,
        cb: Option<FindSessionCb>,
    );
    fn SSL_SESSION_new() -> *mut SSL_SESSION;
    fn SSL_SESSION_set1_master_key(s: *mut SSL_SESSION, key: *const u8, len: usize) -> libc::c_int;
    fn SSL_SESSION_set_cipher(s: *mut SSL_SESSION, c: *const SSL_CIPHER) -> libc::c_int;
    fn SSL_SESSION_set_protocol_version(s: *mut SSL_SESSION, v: libc::c_int) -> libc::c_int;
    fn SSL_CIPHER_find(ssl: *mut SSL, ptr: *const u8) -> *const SSL_CIPHER;
}

#[derive(Default, Clone)]
struct Callbacks {
    client: Option<Value>,
    server: Option<Value>,
}

thread_local! {
    /// Callables by `SSL_CTX` address.
    static REGISTRY: RefCell<HashMap<usize, Callbacks>> = RefCell::new(HashMap::new());
    /// TLS 1.3 client identities, which must outlive the callback
    /// (`psk_identity_buf`), by `SSL` address.
    static IDENTITIES: RefCell<HashMap<usize, Vec<u8>>> = RefCell::new(HashMap::new());
}

fn callbacks_of_ssl(ssl: *const SSL) -> Callbacks {
    // SAFETY: `ssl` is the live handle OpenSSL passed to the callback.
    let ctx = unsafe { openssl_sys::SSL_get_SSL_CTX(ssl) } as usize;
    REGISTRY.with(|r| r.borrow().get(&ctx).cloned().unwrap_or_default())
}

/// The values of an `Openssl\Psk` returned by a callback.
struct PskValue {
    psk: Option<Vec<u8>>,
    identity: Option<Vec<u8>>,
}

fn pending(class: &str, msg: &str) {
    php_ext_streams::bridge::with_vm(|vm| {
        let t = vm.throw(class, msg);
        php_ext_streams::bridge::set_pending(t);
    });
}

/// `php_openssl_call_psk_cb`: `Err(())` = FAILURE (exception pending),
/// `Ok(None)` = the callback returned `null`.
fn call_psk_cb(cb: &Value, identity: Option<&[u8]>) -> Result<Option<PskValue>, ()> {
    php_ext_streams::bridge::with_vm(|vm| {
        let stream = super::sess::current_stream();
        let mut args = vec![stream];
        if let Some(id) = identity {
            args.push(Value::Str(ZStr::from(id)));
        }
        let ret = match vm.call_value(cb, args) {
            Ok(v) => v,
            Err(t) => {
                php_ext_streams::bridge::set_pending(t);
                return Err(());
            }
        };
        match &ret {
            Value::Null => Ok(None),
            Value::Object(o) if php_vm::object::class_of(o).name.as_bytes() == b"Openssl\\Psk" => {
                let get = |vm: &mut Vm, name: &str| match vm.get_property(o, name) {
                    Ok(Value::Str(s)) => Some(s.as_bytes().to_vec()),
                    _ => None,
                };
                let psk = get(vm, "psk");
                let identity = get(vm, "identity");
                Ok(Some(PskValue { psk, identity }))
            }
            _ => {
                let t = vm.throw("TypeError", "PSK callback must return Openssl\\Psk or null");
                php_ext_streams::bridge::set_pending(t);
                Err(())
            }
        }
    })
    .unwrap_or(Err(()))
}

/// `php_openssl_psk_build_session`.
unsafe fn build_session(ssl: *mut SSL, psk: &[u8], md: *const EVP_MD) -> *mut SSL_SESSION {
    // SAFETY (whole function): `ssl` is live; every returned pointer is
    // checked before use and the new session is freed on failure.
    unsafe {
        let mut cipher = SSL_CIPHER_find(ssl, TLS13_AES128_GCM_SHA256.as_ptr());
        if cipher.is_null() {
            return std::ptr::null_mut();
        }
        if !md.is_null() && openssl_sys::SSL_CIPHER_get_handshake_digest(cipher) != md {
            cipher = SSL_CIPHER_find(ssl, TLS13_AES256_GCM_SHA384.as_ptr());
            if cipher.is_null() || openssl_sys::SSL_CIPHER_get_handshake_digest(cipher) != md {
                return std::ptr::null_mut();
            }
        }
        let sess = SSL_SESSION_new();
        if sess.is_null() {
            return sess;
        }
        if SSL_SESSION_set1_master_key(sess, psk.as_ptr(), psk.len()) == 0
            || SSL_SESSION_set_cipher(sess, cipher) == 0
            || SSL_SESSION_set_protocol_version(sess, TLS1_3_VERSION) == 0
        {
            openssl_sys::SSL_SESSION_free(sess);
            return std::ptr::null_mut();
        }
        let max_early = openssl_sys::SSL_get_max_early_data(ssl);
        if max_early > 0 {
            openssl_sys::SSL_SESSION_set_max_early_data(sess, max_early);
        }
        sess
    }
}

/// `php_openssl_psk_use_session_cb` (TLS 1.3 client).
unsafe extern "C" fn use_session_cb(
    ssl: *mut SSL,
    md: *const EVP_MD,
    id: *mut *const u8,
    idlen: *mut usize,
    sess: *mut *mut SSL_SESSION,
) -> libc::c_int {
    // SAFETY: OpenSSL passes valid out-pointers.
    unsafe {
        *id = std::ptr::null();
        *idlen = 0;
        *sess = std::ptr::null_mut();
    }
    let Some(cb) = callbacks_of_ssl(ssl).client else {
        return 1;
    };
    let Ok(result) = call_psk_cb(&cb, None) else {
        return 0;
    };
    let Some(v) = result else {
        return 1;
    };
    let Some(psk) = v.psk else {
        return 0;
    };
    let Some(identity) = v.identity else {
        pending(
            "ValueError",
            "Client PSK callback must return Openssl\\Psk with a non-null identity",
        );
        return 0;
    };
    // SAFETY: `ssl`/`md` come from OpenSSL.
    let session = unsafe { build_session(ssl, &psk, md) };
    if session.is_null() {
        return i32::from(!md.is_null());
    }
    if identity.len() > MAX_IDENTITY_LEN {
        pending(
            "ValueError",
            &format!("PSK identity must not exceed {MAX_IDENTITY_LEN} bytes"),
        );
        // SAFETY: the session was created above and is not shared.
        unsafe { openssl_sys::SSL_SESSION_free(session) };
        return 0;
    }
    let ptr = IDENTITIES.with(|m| {
        let mut m = m.borrow_mut();
        let buf = m.entry(ssl as usize).or_default();
        *buf = identity;
        (buf.as_ptr(), buf.len())
    });
    // SAFETY: out-pointers from OpenSSL; the identity buffer lives in the
    // thread-local map until the next handshake of this handle.
    unsafe {
        *id = ptr.0;
        *idlen = ptr.1;
        *sess = session;
    }
    1
}

/// `php_openssl_psk_find_session_cb` (TLS 1.3 server).
unsafe extern "C" fn find_session_cb(
    ssl: *mut SSL,
    identity: *const u8,
    identity_len: usize,
    sess: *mut *mut SSL_SESSION,
) -> libc::c_int {
    // SAFETY: valid out-pointer from OpenSSL.
    unsafe { *sess = std::ptr::null_mut() };
    let Some(cb) = callbacks_of_ssl(ssl).server else {
        return 1;
    };
    // SAFETY: OpenSSL passes `identity_len` readable bytes.
    let id = unsafe { std::slice::from_raw_parts(identity, identity_len) }.to_vec();
    let Ok(result) = call_psk_cb(&cb, Some(&id)) else {
        return 0;
    };
    let Some(v) = result else {
        return 1;
    };
    let Some(psk) = v.psk else {
        return 0;
    };
    // SAFETY: `ssl` comes from OpenSSL.
    let session = unsafe { build_session(ssl, &psk, std::ptr::null()) };
    if session.is_null() {
        return 0;
    }
    // SAFETY: valid out-pointer.
    unsafe { *sess = session };
    1
}

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
        super::sess::set_pending(t);
        return None;
    }
    Some(cb.clone())
}

fn set_registry(b: &SslContextBuilder, f: impl FnOnce(&mut Callbacks)) {
    let key = b.as_ptr() as usize;
    REGISTRY.with(|r| f(r.borrow_mut().entry(key).or_default()));
}

/// `php_openssl_setup_client_psk`; `false` on failure.
pub(crate) fn setup_client(
    vm: &mut Vm,
    b: &mut SslContextBuilder,
    opts: &SslOpts,
    persistent: bool,
) -> VmResult<bool> {
    let Some(v) = opts.get("psk_client_cb") else {
        return Ok(true);
    };
    let Some(cb) = validate(vm, "psk_client_cb", &v, persistent) else {
        return Ok(false);
    };
    set_registry(b, |c| c.client = Some(cb));
    b.set_psk_client_callback(|ssl, _hint, identity_out, psk_out| {
        let Some(cb) = callbacks_of_ssl(ssl.as_ptr()).client else {
            return Ok(0);
        };
        let Ok(Some(v)) = call_psk_cb(&cb, None) else {
            return Ok(0);
        };
        let Some(psk) = v.psk else {
            return Ok(0);
        };
        let Some(identity) = v.identity else {
            pending(
                "ValueError",
                "Client PSK callback must return Openssl\\Psk with a non-null identity",
            );
            return Ok(0);
        };
        if identity.len() >= identity_out.len() {
            pending(
                "ValueError",
                &format!(
                    "PSK identity must be shorter than {} bytes",
                    identity_out.len()
                ),
            );
            return Ok(0);
        }
        if psk.len() > psk_out.len() {
            pending(
                "ValueError",
                &format!("PSK key must not exceed {} bytes", psk_out.len()),
            );
            return Ok(0);
        }
        identity_out[..identity.len()].copy_from_slice(&identity);
        identity_out[identity.len()] = 0;
        psk_out[..psk.len()].copy_from_slice(&psk);
        Ok::<usize, ErrorStack>(psk.len())
    });
    // SAFETY: `b` owns a live SSL_CTX; the callback is a plain function.
    unsafe { SSL_CTX_set_psk_use_session_callback(b.as_ptr(), Some(use_session_cb)) };
    Ok(true)
}

/// `php_openssl_setup_server_psk`; `false` on failure.
pub(crate) fn setup_server(
    vm: &mut Vm,
    b: &mut SslContextBuilder,
    opts: &SslOpts,
    persistent: bool,
) -> VmResult<bool> {
    let Some(v) = opts.get("psk_server_cb") else {
        return Ok(true);
    };
    let Some(cb) = validate(vm, "psk_server_cb", &v, persistent) else {
        return Ok(false);
    };
    set_registry(b, |c| c.server = Some(cb));
    b.set_psk_server_callback(|ssl, identity, psk_out| {
        // SAFETY: `ssl` is live for the callback.
        if unsafe { openssl_sys::SSL_version(ssl.as_ptr()) } >= TLS1_3_VERSION {
            return Ok(0);
        }
        let Some(cb) = callbacks_of_ssl(ssl.as_ptr()).server else {
            return Ok(0);
        };
        let Some(identity) = identity else {
            return Ok(0);
        };
        let id = identity.split(|&c| c == 0).next().unwrap_or_default();
        let Ok(Some(v)) = call_psk_cb(&cb, Some(id)) else {
            return Ok(0);
        };
        let Some(psk) = v.psk else {
            return Ok(0);
        };
        if psk.len() > psk_out.len() {
            pending(
                "ValueError",
                &format!("PSK key must not exceed {} bytes", psk_out.len()),
            );
            return Ok(0);
        }
        psk_out[..psk.len()].copy_from_slice(&psk);
        Ok::<usize, ErrorStack>(psk.len())
    });
    // SAFETY: `b` owns a live SSL_CTX; the callback is a plain function.
    unsafe { SSL_CTX_set_psk_find_session_callback(b.as_ptr(), Some(find_session_cb)) };
    if opts.get("session_id_context").is_none() {
        let _ = b.set_session_id_context(b"PHP_PSK");
    }
    Ok(true)
}

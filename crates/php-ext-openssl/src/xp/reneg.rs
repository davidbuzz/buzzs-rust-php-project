//! Server-side renegotiation rate limiting (`reneg_limit`,
//! `reneg_window`, `reneg_limit_callback`): a leaky bucket fed by the
//! `SSL_CB_HANDSHAKE_START` info callback
//! (`php_openssl_limit_handshake_reneg`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use foreign_types::ForeignTypeRef;
use php_runtime::Value;
use php_streams::diag::StreamErrorCode as Code;
use php_vm::ext::VmResult;
use php_vm::Vm;

use super::opts::SslOpts;
use super::sock::warn;

/// `OPENSSL_DEFAULT_RENEG_LIMIT`.
const DEFAULT_LIMIT: i64 = 2;
/// `OPENSSL_DEFAULT_RENEG_WINDOW`.
const DEFAULT_WINDOW: i64 = 300;
/// `SSL_CB_HANDSHAKE_START`.
const SSL_CB_HANDSHAKE_START: libc::c_int = 0x10;

type InfoCb = unsafe extern "C" fn(*const openssl_sys::SSL, libc::c_int, libc::c_int);

extern "C" {
    fn SSL_set_info_callback(ssl: *mut openssl_sys::SSL, cb: Option<InfoCb>);
}

/// `php_openssl_handshake_bucket_t` plus the callback option.
#[derive(Debug, Clone)]
struct Bucket {
    prev_handshake: i64,
    limit: i64,
    window: i64,
    tokens: f32,
    should_close: bool,
    callback: Option<Value>,
}

thread_local! {
    /// Buckets by `SSL` address.
    static BUCKETS: RefCell<HashMap<usize, Bucket>> = RefCell::new(HashMap::new());
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// `php_openssl_limit_handshake_reneg`.
fn limit_handshake(key: usize) {
    let fire = BUCKETS.with(|b| {
        let mut b = b.borrow_mut();
        let bucket = b.get_mut(&key)?;
        let t = now();
        if bucket.prev_handshake == 0 {
            bucket.prev_handshake = t;
            return None;
        }
        let elapsed = t - bucket.prev_handshake;
        bucket.prev_handshake = t;
        let rate = if bucket.window == 0 {
            0
        } else {
            bucket.limit / bucket.window
        };
        #[allow(clippy::cast_precision_loss)]
        {
            bucket.tokens -= (elapsed * rate) as f32;
        }
        if bucket.tokens < 0.0 {
            bucket.tokens = 0.0;
        }
        bucket.tokens += 1.0;
        #[allow(clippy::cast_precision_loss)]
        let over = bucket.tokens > bucket.limit as f32;
        if over {
            bucket.should_close = true;
            Some(bucket.callback.clone())
        } else {
            None
        }
    });
    let Some(cb) = fire else {
        return;
    };
    let Some(cb) = cb else {
        warn(
            Code::ProtocolError,
            "SSL: client-initiated handshake rate limit exceeded by peer",
        );
        return;
    };
    let keep_open = php_ext_streams::bridge::with_vm(|vm| {
        if !vm.is_callable(&cb) {
            warn(
                Code::UserspaceCallFailed,
                "SSL: failed invoking reneg limit notification callback",
            );
            return false;
        }
        let stream = super::sess::current_stream();
        match vm.call_value(&cb, vec![stream]) {
            Ok(Value::Bool(true)) => true,
            Ok(_) => false,
            Err(t) => {
                php_ext_streams::bridge::set_pending(t);
                false
            }
        }
    })
    .unwrap_or(false);
    if keep_open {
        BUCKETS.with(|b| {
            if let Some(bucket) = b.borrow_mut().get_mut(&key) {
                bucket.should_close = false;
            }
        });
    }
}

/// `php_openssl_info_callback`.
unsafe extern "C" fn info_callback(
    ssl: *const openssl_sys::SSL,
    where_: libc::c_int,
    _ret: libc::c_int,
) {
    if where_ & SSL_CB_HANDSHAKE_START != 0 {
        limit_handshake(ssl as usize);
    }
}

/// `php_openssl_init_server_reneg_limit`.
pub(crate) fn init(vm: &mut Vm, ssl: &mut openssl::ssl::Ssl, opts: &SslOpts) -> VmResult<()> {
    let limit = opts.long(vm, "reneg_limit")?.unwrap_or(DEFAULT_LIMIT);
    if limit < 0 {
        return Ok(());
    }
    let window = opts.long(vm, "reneg_window")?.unwrap_or(DEFAULT_WINDOW);
    let key = ssl.as_ptr() as usize;
    BUCKETS.with(|b| {
        b.borrow_mut().insert(
            key,
            Bucket {
                prev_handshake: 0,
                limit,
                window,
                tokens: 0.0,
                should_close: false,
                callback: opts.get("reneg_limit_callback"),
            },
        );
    });
    // SAFETY: `ssl` is a live handle; the callback is a plain function.
    unsafe { SSL_set_info_callback(ssl.as_ptr(), Some(info_callback)) };
    Ok(())
}

/// Whether the rate limit asked to close the connection of `ssl`.
pub(crate) fn should_close(ssl: &openssl::ssl::SslRef) -> bool {
    let key = ssl.as_ptr() as usize;
    BUCKETS.with(|b| b.borrow().get(&key).is_some_and(|x| x.should_close))
}

/// Forgets the bucket of a closed handle.
pub(crate) fn forget(ssl: &openssl::ssl::SslRef) {
    let key = ssl.as_ptr() as usize;
    BUCKETS.with(|b| {
        b.borrow_mut().remove(&key);
    });
}

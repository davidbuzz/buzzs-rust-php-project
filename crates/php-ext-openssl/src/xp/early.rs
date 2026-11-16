// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! TLS 1.3 early data (0-RTT): the client `early_data` payload and the
//! server `max_early_data` / `early_data_cb` options.

use std::cell::RefCell;
use std::collections::HashMap;
use std::net::TcpStream;

use foreign_types::ForeignTypeRef;
use openssl::ssl::{SslContextBuilder, SslSessionCacheMode, SslStream};
use php_runtime::{Value, ZStr};
use php_streams::diag::StreamErrorCode as Code;
use php_vm::ext::VmResult;
use php_vm::Vm;

use super::opts::SslOpts;
use super::sock::warn;

/// `PHP_OPENSSL_EARLY_DATA_CHUNK`.
const CHUNK: usize = 16384;

/// `php_openssl_early_data_state_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum State {
    /// Not attempted.
    #[default]
    None,
    /// Being sent / received during the handshake.
    Active,
    /// Finished (reported in the crypto meta data).
    Done,
}

extern "C" {
    fn SSL_CTX_set_recv_max_early_data(ctx: *mut openssl_sys::SSL_CTX, n: u32) -> libc::c_int;
    fn SSL_get_early_data_status(ssl: *const openssl_sys::SSL) -> libc::c_int;
}

thread_local! {
    /// `early_data_cb` callables by `SSL_CTX` address.
    static CALLBACKS: RefCell<HashMap<usize, Value>> = RefCell::new(HashMap::new());
}

/// `php_openssl_setup_client_early_data`: the payload, or `Err(())` after
/// queueing the `TypeError`.
pub(crate) fn client_payload(vm: &mut Vm, opts: &SslOpts) -> Result<Option<Vec<u8>>, ()> {
    match opts.get("early_data") {
        None => Ok(None),
        Some(Value::Str(s)) => Ok((!s.is_empty()).then(|| s.as_bytes().to_vec())),
        Some(_) => {
            super::sess::set_pending(vm.throw("TypeError", "early_data must be a string"));
            Err(())
        }
    }
}

/// `php_openssl_setup_server_early_data`; `false` on failure.
pub(crate) fn setup_server(
    vm: &mut Vm,
    b: &mut SslContextBuilder,
    opts: &SslOpts,
    persistent: bool,
) -> VmResult<bool> {
    let mut max = 0i64;
    if let Some(m) = opts.long(vm, "max_early_data")? {
        if !(0..=i64::from(u32::MAX)).contains(&m) {
            super::sess::set_pending(vm.throw(
                "ValueError",
                &format!("max_early_data must be between 0 and {}", u32::MAX),
            ));
            return Ok(false);
        }
        max = m;
    }
    if let Some(cb) = opts.get("early_data_cb") {
        if persistent {
            warn(
                Code::PersistentNotSupported,
                "early_data_cb is not supported for persistent streams",
            );
            return Ok(false);
        }
        if !vm.is_callable(&cb) {
            let t = vm.throw(
                "TypeError",
                "early_data_cb must be a valid callback, no array or string given",
            );
            super::sess::set_pending(t);
            return Ok(false);
        }
        let key = b.as_ptr() as usize;
        CALLBACKS.with(|c| c.borrow_mut().insert(key, cb));
        if max == 0 {
            max = i64::try_from(CHUNK).unwrap_or(0);
        }
    }
    if max > 0 {
        let n = u32::try_from(max).unwrap_or(u32::MAX);
        let _ = b.set_max_early_data(n);
        // SAFETY: `b` owns a live SSL_CTX; the setter stores the value.
        unsafe { SSL_CTX_set_recv_max_early_data(b.as_ptr(), n) };
        let prev = b.set_session_cache_mode(SslSessionCacheMode::SERVER);
        if prev != SslSessionCacheMode::OFF {
            b.set_session_cache_mode(prev);
        }
    }
    Ok(true)
}

/// Whether the server context enables early data.
pub(crate) fn server_enabled(vm: &mut Vm, opts: &SslOpts) -> VmResult<bool> {
    if opts.get("early_data_cb").is_some() {
        return Ok(true);
    }
    Ok(opts.long(vm, "max_early_data")?.is_some_and(|m| m > 0))
}

/// The initial state for a new handshake.
pub(crate) fn initial_state(
    ssl: &SslStream<TcpStream>,
    is_client: bool,
    has_payload: bool,
) -> State {
    let active = if is_client {
        has_payload && ssl.ssl().session().is_some_and(|s| s.max_early_data() > 0)
    } else {
        // SAFETY: the handle is live.
        unsafe { openssl_sys::SSL_get_max_early_data(ssl.ssl().as_ptr()) > 0 }
    };
    if active {
        State::Active
    } else {
        State::None
    }
}

/// `php_openssl_handshake_client_early_data` / `_server_early_data`
/// followed by the handshake step.
pub(crate) fn handshake(
    ssl: &mut SslStream<TcpStream>,
    is_client: bool,
    state: &mut State,
    payload: &[u8],
    offset: &mut usize,
) -> Result<(), openssl::ssl::Error> {
    if *state == State::Active {
        if is_client {
            while *state == State::Active {
                let n = ssl.write_early_data(&payload[*offset..])?;
                *offset += n;
                if *offset >= payload.len() {
                    *state = State::Done;
                }
            }
        } else {
            let mut buf = vec![0u8; CHUNK];
            let key = ssl.ssl().ssl_context().as_ptr() as usize;
            while *state == State::Active {
                let n = ssl.read_early_data(&mut buf)?;
                if n == 0 {
                    *state = State::Done;
                    break;
                }
                let cb = CALLBACKS.with(|c| c.borrow().get(&key).cloned());
                if let Some(cb) = cb {
                    let data = Value::Str(ZStr::from(&buf[..n]));
                    let stream = super::sess::current_stream();
                    php_ext_streams::bridge::with_vm(|vm| {
                        if let Err(t) = vm.call_value(&cb, vec![stream, data]) {
                            php_ext_streams::bridge::set_pending(t);
                        }
                    });
                }
            }
        }
    }
    ssl.do_handshake()
}

/// `SSL_get_early_data_status` as reported in the meta data.
pub(crate) fn status(ssl: &SslStream<TcpStream>) -> &'static str {
    // SAFETY: the handle is live.
    match unsafe { SSL_get_early_data_status(ssl.ssl().as_ptr()) } {
        2 => "accepted",
        1 => "rejected",
        _ => "not_sent",
    }
}

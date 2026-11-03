//! The `ssl://` / `tls://` / `tlsv1.x://` socket transports and the
//! crypto hooks behind `stream_socket_enable_crypto()`,
//! `stream_socket_get_crypto_status()` and `stream_get_meta_data()["crypto"]`
//! — a port of `ext/openssl/xp_ssl.c` (client and server side) on the
//! `openssl` crate, plugged into `php_ext_streams::xport`.
//!
//! Not ported yet: renegotiation rate limiting (`reneg_limit*`), session
//! resumption callbacks (`session_new_cb` …), PSK and TLS 1.3 early data.

// The port keeps xp_ssl.c's names (`ssl`/`ssls`, `ops`/`opts`).
#![allow(clippy::similar_names)]

mod ctx;
mod opts;
mod sock;

use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::os::fd::BorrowedFd;
use std::time::{Duration, Instant};

use openssl::ssl::{ErrorCode, Ssl, SslStream};
use php_ext_streams::xport::{self, CryptoHooks, Transport, XportError, XportRequest};
use php_runtime::{Value, ZArray, ZStr};
use php_streams::diag::StreamErrorCode as Code;
use php_streams::stream::{flags, CastAs, Stream};
use php_vm::ext::VmResult;
use php_vm::Vm;

use opts::SslOpts;
use sock::{handle_ssl_error, poll_fd, warn, Conn, TlsOps};

/// `STREAM_CRYPTO_IS_CLIENT`.
const IS_CLIENT: i64 = 1;
/// `STREAM_CRYPTO_METHOD_TLS_ANY_CLIENT`.
const TLS_ANY_CLIENT: i64 = (1 << 3) | (1 << 4) | (1 << 5) | (1 << 6) | 1;

/// Registers the transports and crypto hooks.
pub(crate) fn register() {
    for scheme in ["ssl", "tls", "tlsv1.0", "tlsv1.1", "tlsv1.2", "tlsv1.3"] {
        xport::register_transport(
            scheme,
            Transport {
                create,
                accept: Some(accept),
            },
        );
    }
    xport::set_crypto_hooks(CryptoHooks {
        enable: Some(enable_hook),
        status: Some(status),
        meta: Some(meta),
    });
}

fn default_timeout(vm: &Vm) -> Duration {
    Duration::from_secs(u64::try_from(vm.ini_long("default_socket_timeout")).unwrap_or(60))
}

/// Emits the queued stream warnings (`func(): msg`).
fn flush(vm: &mut Vm) {
    php_ext_streams::bridge::flush_diags(vm);
}

fn io_error(e: &std::io::Error) -> XportError {
    let errno = e.raw_os_error().unwrap_or(0);
    XportError {
        errno,
        message: if errno == 0 {
            e.to_string()
        } else {
            php_streams::sys::strerror(errno)
        },
    }
}

/// `php_openssl_get_crypto_method`.
fn crypto_method(vm: &mut Vm, opts: &SslOpts, default: i64) -> VmResult<i64> {
    Ok(match opts.long(vm, "crypto_method")? {
        Some(m) => m | IS_CLIENT,
        None => default,
    })
}

/// Connects a TCP client socket (`php_network_connect_socket_to_host`).
fn connect(host: &str, port: u16, timeout: Option<Duration>) -> Result<TcpStream, XportError> {
    let addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|e| XportError {
            errno: 0,
            message: format!("php_network_getaddresses: getaddrinfo for {host} failed: {e}"),
        })?
        .collect();
    let mut last = std::io::Error::from_raw_os_error(libc::ECONNREFUSED);
    for a in addrs {
        let r = match timeout {
            Some(t) if !t.is_zero() => TcpStream::connect_timeout(&a, t),
            _ => TcpStream::connect(a),
        };
        match r {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(io_error(&last))
}

/// Builds the stream around socket ops.
fn new_stream(ops: TlsOps) -> Stream {
    let mut s = Stream::new(Box::new(ops), "r+");
    s.add_flags(flags::NO_SEEK);
    s
}

/// `php_openssl_ssl_socket_factory` + connect/listen.
fn create(vm: &mut Vm, req: &XportRequest) -> VmResult<Result<Stream, XportError>> {
    let opts = SslOpts::from_context(req.context.as_ref());
    let method = match req.scheme.as_str() {
        "tlsv1.0" => (1 << 3) | IS_CLIENT,
        "tlsv1.1" => (1 << 4) | IS_CLIENT,
        "tlsv1.2" => (1 << 5) | IS_CLIENT,
        "tlsv1.3" => (1 << 6) | IS_CLIENT,
        _ => crypto_method(vm, &opts, TLS_ANY_CLIENT)?,
    };
    let host = req.host.trim_end_matches('.');
    let url_name = (!host.is_empty()).then(|| host.to_string());
    if req.server {
        let bind = if req.host.contains(':') {
            format!("[{}]:{}", req.host, req.port)
        } else {
            format!("{}:{}", req.host, req.port)
        };
        let listener = match TcpListener::bind(bind) {
            Ok(l) => l,
            Err(e) => return Ok(Err(io_error(&e))),
        };
        let _ = listener.set_nonblocking(true);
        let mut ops = TlsOps::new(Conn::Listener(listener), Some(default_timeout(vm)));
        ops.enable_on_connect = true;
        ops.method = method;
        ops.url_name = url_name;
        let mut s = new_stream(ops);
        s.orig_path = Some(req.address.as_bytes().to_vec());
        s.context.clone_from(&req.context);
        return Ok(Ok(s));
    }
    let tcp = match connect(&req.host, req.port, req.timeout) {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    let _ = tcp.set_nonblocking(true);
    let mut ops = TlsOps::new(Conn::Plain(tcp), Some(default_timeout(vm)));
    ops.connect_timeout = req.timeout;
    ops.enable_on_connect = true;
    ops.method = method;
    ops.url_name = url_name;
    let session = session_stream(&opts);
    let ok = setup(vm, &mut ops, &opts, method, session.as_ref())?
        && enable(vm, &mut ops, &opts, true)? >= 0;
    if !ok {
        warn(Code::ProtocolError, "Failed to enable crypto");
        flush(vm);
        return Ok(Err(XportError {
            errno: 0,
            message: "Unknown error".into(),
        }));
    }
    flush(vm);
    let mut s = new_stream(ops);
    s.orig_path = Some(req.address.as_bytes().to_vec());
    s.context.clone_from(&req.context);
    Ok(Ok(s))
}

/// The `session_stream` context option's TLS state (cloned context only).
fn session_stream(opts: &SslOpts) -> Option<SessionParent> {
    let Some(Value::Resource(r)) = opts.get("session_stream") else {
        return None;
    };
    let cell = php_ext_streams::res::cell_of(&r)?;
    let s = cell.get()?;
    Some(SessionParent::of(&s))
}

/// What setup needs from a `$session_stream`.
pub(crate) struct SessionParent {
    is_tls: bool,
    ctx: Option<openssl::ssl::SslContext>,
    session: Option<openssl::ssl::SslSession>,
    has_handle: bool,
}

impl SessionParent {
    fn of(s: &Stream) -> Self {
        match s.ops_as::<TlsOps>() {
            Some(o) => SessionParent {
                is_tls: true,
                ctx: o.ctx.clone(),
                session: o
                    .tls()
                    .and_then(|t| t.ssl().session().map(ToOwned::to_owned)),
                has_handle: o.tls().is_some(),
            },
            None => SessionParent {
                is_tls: false,
                ctx: None,
                session: None,
                has_handle: false,
            },
        }
    }
}

/// Attaches a new `SSL` handle to the socket.
fn attach(ops: &mut TlsOps, ssl: Ssl) -> bool {
    let Conn::Plain(tcp) = std::mem::replace(&mut ops.conn, Conn::None) else {
        warn(Code::CreateFailed, "SSL handle creation failure");
        return false;
    };
    if let Ok(s) = SslStream::new(ssl, tcp) {
        ops.conn = Conn::Tls(s);
        true
    } else {
        warn(Code::CreateFailed, "SSL handle creation failure");
        false
    }
}

/// Prepares a fresh `SSL` handle: client SNI and connect/accept state
/// (`state_set` in `php_openssl_enable_crypto`).
fn prepare_ssl(vm: &mut Vm, ops: &TlsOps, opts: &SslOpts, ssl: &mut Ssl) -> VmResult<()> {
    if ops.is_client {
        if opts.bool(vm, "SNI_enabled")? != Some(false) {
            let name = opts
                .string(vm, "peer_name")?
                .or_else(|| ops.url_name.clone());
            if let Some(n) = name {
                let _ = ssl.set_hostname(&n);
            }
        }
        ssl.set_connect_state();
    } else {
        ssl.set_accept_state();
    }
    Ok(())
}

/// `php_openssl_setup_crypto`.
fn setup(
    vm: &mut Vm,
    ops: &mut TlsOps,
    opts: &SslOpts,
    method: i64,
    session: Option<&SessionParent>,
) -> VmResult<bool> {
    if ops.tls().is_some() {
        if ops.is_blocked {
            warn(Code::Generic, "SSL/TLS already set-up for this stream");
            return Ok(false);
        }
        return Ok(true);
    }
    ops.is_client = method & IS_CLIENT != 0;
    let method_flags = method & !IS_CLIENT;
    if let Some(parent) = session {
        if !parent.is_tls {
            warn(
                Code::SslNotSupported,
                "Supplied session stream must be an SSL enabled stream",
            );
        } else if parent.ctx.is_none() {
            warn(
                Code::SslNotSupported,
                "Supplied SSL session stream is not set up",
            );
        } else if ops.is_client && !parent.has_handle {
            warn(
                Code::SslNotSupported,
                "Supplied SSL session stream is not initialized",
            );
        } else if let Some(ctx) = &parent.ctx {
            let Ok(mut ssl) = Ssl::new(ctx) else {
                warn(Code::CreateFailed, "SSL handle creation failure");
                return Ok(false);
            };
            if ops.is_client {
                match &parent.session {
                    // SAFETY: the session comes from a connection made with
                    // the same SSL_CTX as `ssl`.
                    Some(sess) if unsafe { ssl.set_session(sess) }.is_ok() => {}
                    _ => warn(
                        Code::CreateFailed,
                        "SSL session copying failed creation failure",
                    ),
                }
            }
            prepare_ssl(vm, ops, opts, &mut ssl)?;
            ops.ctx = Some(ctx.clone());
            return Ok(attach(ops, ssl));
        }
    }
    let Some(ctx) = ctx::create_ctx(vm, opts, ops.is_client, method_flags)? else {
        return Ok(false);
    };
    let Ok(mut ssl) = Ssl::new(&ctx) else {
        warn(Code::CreateFailed, "SSL handle creation failure");
        return Ok(false);
    };
    prepare_ssl(vm, ops, opts, &mut ssl)?;
    ops.ctx = Some(ctx);
    Ok(attach(ops, ssl))
}

/// `php_openssl_enable_crypto`: 1 on success, 0 when a non-blocking
/// handshake would block, -1 on failure.
fn enable(vm: &mut Vm, ops: &mut TlsOps, opts: &SslOpts, activate: bool) -> VmResult<i32> {
    ops.last_status = 0;
    if activate && !ops.ssl_active {
        let Some(fd) = ops.fd() else {
            return Ok(-1);
        };
        let blocked = ops.is_blocked;
        let timeout = if ops.is_client {
            ops.connect_timeout
        } else {
            ops.timeout
        };
        let has_timeout = timeout.is_some_and(|t| !t.is_zero());
        let start = Instant::now();
        let mut eagain = false;
        let mut n;
        loop {
            let Conn::Tls(ssl) = &mut ops.conn else {
                return Ok(-1);
            };
            let r = ssl.do_handshake();
            let elapsed = start.elapsed();
            if has_timeout && timeout.is_some_and(|t| elapsed > t) {
                warn(Code::TimeOut, "SSL: Handshake timed out");
                return Ok(-1);
            }
            match r {
                Ok(()) => {
                    n = 1;
                    break;
                }
                Err(e) => {
                    n = -1;
                    let out = handle_ssl_error(&e, blocked, false, &mut ops.last_status);
                    if let Some(a) = out.eagain {
                        eagain = a;
                    }
                    if !out.retry {
                        break;
                    }
                    let ev = if e.code() == ErrorCode::WANT_READ {
                        libc::POLLIN | libc::POLLPRI
                    } else {
                        libc::POLLOUT
                    };
                    let left = timeout.map(|t| t.saturating_sub(elapsed));
                    poll_fd(fd, ev, if has_timeout { left } else { None });
                }
            }
        }
        let (peer, ssl_ref_ok) = match &ops.conn {
            Conn::Tls(s) => (s.ssl().peer_certificate(), true),
            _ => (None, false),
        };
        if !ssl_ref_ok {
            return Ok(-1);
        }
        if n == 1 {
            if let (Some(p), true) = (&peer, opts.has_context()) {
                capture(vm, ops, p, opts)?;
            }
            let policy = ctx::Policy::read(vm, opts, ops.is_client, ops.url_name.as_deref())?;
            let Conn::Tls(s) = &mut ops.conn else {
                return Ok(-1);
            };
            if ctx::apply_policy(s.ssl(), peer.as_deref(), &policy) {
                ops.ssl_active = true;
            } else {
                let _ = s.shutdown();
                n = -1;
            }
        } else if eagain {
            n = 0;
        } else if let (Some(p), true) = (&peer, opts.has_context()) {
            capture(vm, ops, p, opts)?;
        }
        return Ok(n);
    }
    if !activate && ops.ssl_active {
        if let Conn::Tls(s) = &mut ops.conn {
            let _ = s.shutdown();
        }
        ops.ssl_active = false;
    }
    Ok(-1)
}

fn capture(vm: &mut Vm, ops: &TlsOps, peer: &openssl::x509::X509, opts: &SslOpts) -> VmResult<()> {
    if let Some(s) = ops.tls() {
        ctx::capture_peer_certs(vm, s.ssl(), peer, opts)?;
    }
    Ok(())
}

/// The accept hook: wraps an accepted connection on an `ssl://` server.
fn accept(
    vm: &mut Vm,
    listener: &mut Stream,
    conn: Stream,
    _timeout: Option<Duration>,
) -> VmResult<Result<Stream, XportError>> {
    let Some(lops) = listener.ops_as::<TlsOps>() else {
        return Ok(Ok(conn));
    };
    let (enable_on_connect, method, timeout) = (lops.enable_on_connect, lops.method, lops.timeout);
    let mut conn = conn;
    let Some(tcp) = dup_socket(&mut conn) else {
        return Ok(Err(XportError {
            errno: 0,
            message: "Cannot enable crypto".into(),
        }));
    };
    drop(conn);
    let mut ops = TlsOps::new(Conn::Plain(tcp), timeout);
    ops.connect_timeout = None;
    let opts = SslOpts::of_stream(listener);
    if enable_on_connect {
        let method = method & !IS_CLIENT;
        ops.method = method;
        let ok =
            setup(vm, &mut ops, &opts, method, None)? && enable(vm, &mut ops, &opts, true)? >= 0;
        if !ok {
            warn(Code::ProtocolError, "Failed to enable crypto");
            flush(vm);
            return Ok(Err(XportError {
                errno: 0,
                message: "Cannot enable crypto".into(),
            }));
        }
    }
    flush(vm);
    let mut s = new_stream(ops);
    s.context.clone_from(&listener.context);
    Ok(Ok(s))
}

/// Duplicates the descriptor of a plain socket stream into a
/// non-blocking `TcpStream` (the stream itself is dropped afterwards).
fn dup_socket(s: &mut Stream) -> Option<TcpStream> {
    let fd = s.cast(CastAs::FdForSelect, false)?;
    // SAFETY: `fd` belongs to `s`, which stays alive for this call; the
    // borrowed descriptor is only used to duplicate it.
    let owned = unsafe { BorrowedFd::borrow_raw(fd) }
        .try_clone_to_owned()
        .ok()?;
    let tcp = TcpStream::from(owned);
    let _ = tcp.set_nonblocking(true);
    Some(tcp)
}

fn is_socket_stream(s: &Stream) -> bool {
    matches!(
        s.ops_label(),
        "tcp_socket" | "unix_socket" | "udp_socket" | "generic_socket" | "udg_socket"
    )
}

/// `stream_socket_enable_crypto()`.
fn enable_hook(
    vm: &mut Vm,
    stream: &mut Stream,
    activate: bool,
    method: Option<i64>,
    session: Option<&mut Stream>,
) -> VmResult<(Value, Option<Stream>)> {
    let opts = SslOpts::of_stream(stream);
    let method = match method {
        Some(m) => Some(m),
        None if activate => match opts.get("crypto_method") {
            Some(v) => Some(vm.value_to_int(&v)?),
            None => {
                return Err(vm.throw(
                    "ValueError",
                    "stream_socket_enable_crypto(): Argument #3 ($crypto_method) must be specified when enabling encryption",
                ))
            }
        },
        None => None,
    };
    let parent = session.map(|s| SessionParent::of(s));
    let ret = |n: i32| match n {
        -1 => Value::Bool(false),
        0 => Value::Int(0),
        _ => Value::Bool(true),
    };
    if let Some(ops) = stream.ops_as_mut::<TlsOps>() {
        let mut n = -1;
        if activate {
            let m = method.unwrap_or(0);
            if setup(vm, ops, &opts, m, parent.as_ref())? {
                n = enable(vm, ops, &opts, true)?;
            }
        } else {
            n = enable(vm, ops, &opts, false)?;
        }
        flush(vm);
        return Ok((ret(n), None));
    }
    if !is_socket_stream(stream) {
        warn(
            Code::SslNotSupported,
            "This stream does not support SSL/crypto",
        );
        flush(vm);
        return Ok((Value::Bool(false), None));
    }
    if !activate {
        return Ok((Value::Bool(false), None));
    }
    let blocked = stream
        .meta_data()
        .iter()
        .find(|(k, _)| k == "blocked")
        .is_none_or(|(_, v)| !matches!(v, php_streams::value::Value::Bool(false)));
    let Some(tcp) = dup_socket(stream) else {
        return Ok((Value::Bool(false), None));
    };
    let mut ops = TlsOps::new(Conn::Plain(tcp), Some(default_timeout(vm)));
    ops.is_blocked = blocked;
    let m = method.unwrap_or(0);
    ops.method = m;
    let mut n = -1;
    if setup(vm, &mut ops, &opts, m, parent.as_ref())? {
        n = enable(vm, &mut ops, &opts, true)?;
    }
    flush(vm);
    if n < 0 {
        return Ok((Value::Bool(false), None));
    }
    let mut s = new_stream(ops);
    s.context.clone_from(&stream.context);
    s.orig_path.clone_from(&stream.orig_path);
    Ok((ret(n), Some(s)))
}

/// `stream_socket_get_crypto_status()`.
fn status(s: &mut Stream) -> i64 {
    s.ops_as::<TlsOps>().map_or(0, |o| o.last_status)
}

/// `stream_get_meta_data()["crypto"]`.
fn meta(s: &Stream) -> Option<Value> {
    let ops = s.ops_as::<TlsOps>()?;
    if !ops.ssl_active {
        return None;
    }
    let ssl = ops.tls()?.ssl();
    let mut arr = ZArray::new();
    let t = arr.make_mut();
    let proto = match ssl.version_str() {
        v @ ("TLSv1.3" | "TLSv1.2" | "TLSv1.1" | "TLSv1" | "SSLv3") => v,
        _ => "UNKNOWN",
    };
    t.insert(ZStr::from("protocol"), Value::string(proto));
    if let Some(c) = ssl.current_cipher() {
        t.insert(ZStr::from("cipher_name"), Value::string(c.name()));
        t.insert(
            ZStr::from("cipher_bits"),
            Value::Int(i64::from(c.bits().secret)),
        );
        t.insert(ZStr::from("cipher_version"), Value::string(c.version()));
    }
    t.insert(
        ZStr::from("session_reused"),
        Value::Bool(ssl.session_reused()),
    );
    if let Some(p) = ssl.selected_alpn_protocol() {
        t.insert(ZStr::from("alpn_protocol"), Value::Str(ZStr::from(p)));
    }
    Some(Value::Array(arr))
}

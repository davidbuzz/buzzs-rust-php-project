// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `ssl://` / `tls://` / `tlsv1.x://` socket transports and the
//! crypto hooks behind `stream_socket_enable_crypto()`,
//! `stream_socket_get_crypto_status()` and `stream_get_meta_data()["crypto"]`
//! — a port of `ext/openssl/xp_ssl.c` (client and server side) on the
//! `openssl` crate, plugged into `php_ext_streams::xport`.
//!
//! Session resumption lives in `sess`, PSK in `psk`, 0-RTT in `early`
//! and renegotiation limiting in `reneg`; `tcp` is registered too, as
//! ext/openssl does.

// The port keeps xp_ssl.c's names (`ssl`/`ssls`, `ops`/`opts`).
#![allow(clippy::similar_names)]

mod ctx;
mod early;
mod opts;
mod psk;
mod reneg;
mod sess;
mod sock;

/// `RSHUTDOWN`: forgets the PHP callables (session, PSK and early-data
/// callbacks) and PSK identities registered for this request's contexts.
/// php-src keeps them with the stream and frees them when it closes;
/// every stream that has them is closed by the end of the request
/// (persistent streams cannot have them), and a long-running worker's
/// next request must neither keep them alive nor find them under a
/// reused `SSL_CTX` address.
pub(crate) fn reset_request() {
    psk::reset_request();
    sess::reset_request();
    early::reset_request();
}

use std::net::{TcpListener, TcpStream};
use std::os::fd::{BorrowedFd, FromRawFd};
use std::time::{Duration, Instant};

use openssl::ssl::{ErrorCode, Ssl, SslStream};
use php_ext_streams::xport::{
    self, ConnectError, CryptoHooks, Transport, XportError, XportRequest,
};
use php_runtime::{Value, ZArray, ZStr};
use php_streams::diag::StreamErrorCode as Code;
use php_streams::sockopts::{SockOpts, XportKind};
use php_streams::stream::{flags, CastAs, Stream};
use php_vm::ext::VmResult;
use php_vm::Vm;

use opts::SslOpts;
use sock::{handle_ssl_error, poll_fd, warn, Conn, TlsOps};

/// `STREAM_CLIENT_CONNECT`.
const STREAM_CLIENT_CONNECT: i64 = 4;
/// `STREAM_CLIENT_ASYNC_CONNECT`.
const STREAM_CLIENT_ASYNC_CONNECT: i64 = 2;
/// `STREAM_CRYPTO_IS_CLIENT`.
const IS_CLIENT: i64 = 1;
/// `STREAM_CRYPTO_METHOD_TLS_ANY_CLIENT`.
const TLS_ANY_CLIENT: i64 = (1 << 3) | (1 << 4) | (1 << 5) | (1 << 6) | 1;

/// Registers the transports and crypto hooks.
pub(crate) fn register() {
    // Like ext/openssl, take over `tcp` too: plain TCP sockets are
    // `tcp_socket/ssl` streams that can be upgraded in place, and accepted
    // connections inherit the listener's context.
    for scheme in [
        "tcp", "ssl", "tls", "tlsv1.0", "tlsv1.1", "tlsv1.2", "tlsv1.3",
    ] {
        xport::register_transport(
            scheme,
            Transport {
                create,
                accept: Some(accept),
                connect: Some(handshake),
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

/// Connects a TCP client socket through the streams library (same error
/// texts as plain `tcp://`) and takes over its descriptor; with the
/// resource number that stream took.
fn connect(req: &XportRequest) -> Result<(TcpStream, Option<i64>), XportError> {
    let (_, rest) = xport::split_scheme(&req.address);
    let target = format!("tcp://{rest}");
    // `STREAM_XPORT_OP_CONNECT_ASYNC`: the connection may still be pending.
    let asynchronous = req.flags & STREAM_CLIENT_ASYNC_CONNECT != 0;
    let mut plain = php_streams::transports::socket_client_ex(
        target.as_bytes(),
        req.timeout,
        asynchronous,
        req.context.as_ref(),
    )
    .map_err(|e| XportError {
        errno: e.errno,
        message: e.message,
    })?;
    let tcp = dup_socket(&mut plain).ok_or_else(|| XportError {
        errno: 0,
        message: String::new(),
    })?;
    let number = plain.resource_number();
    drop(plain);
    Ok((tcp, number))
}

/// `socket(AF_INET, SOCK_STREAM)` without connecting.
fn unconnected_socket() -> Option<TcpStream> {
    // SAFETY: plain socket(2) call.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return None;
    }
    // SAFETY: `fd` is a fresh descriptor owned by nobody else.
    let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
    Some(TcpStream::from(owned))
}

/// Builds the stream around socket ops.
fn new_stream(ops: TlsOps) -> Stream {
    new_stream_numbered(ops, None)
}

/// [`new_stream`] in place of the plain socket stream that took `number`
/// ([`Stream::new_numbered`]).
fn new_stream_numbered(ops: TlsOps, number: Option<i64>) -> Stream {
    let mut s = Stream::new_numbered(Box::new(ops), "r+", number);
    s.add_flags(flags::NO_SEEK);
    s
}

/// `php_openssl_ssl_socket_factory` + connect/listen.
#[allow(clippy::too_many_lines)]
fn create(vm: &mut Vm, req: &XportRequest) -> VmResult<Result<Stream, XportError>> {
    let opts = SslOpts::from_context(req.context.as_ref());
    let plain_tcp = req.scheme == "tcp";
    let method = match req.scheme.as_str() {
        "tcp" => 0,
        "tlsv1.0" => (1 << 3) | IS_CLIENT,
        "tlsv1.1" => (1 << 4) | IS_CLIENT,
        "tlsv1.2" => (1 << 5) | IS_CLIENT,
        "tlsv1.3" => (1 << 6) | IS_CLIENT,
        _ => crypto_method(vm, &opts, TLS_ANY_CLIENT)?,
    };
    let host = req.host.trim_end_matches('.');
    let url_name = (!host.is_empty()).then(|| host.to_string());
    if req.server {
        let (_, rest) = xport::split_scheme(&req.address);
        if xport::parse_host_port(rest).is_none() {
            return Ok(Err(XportError {
                errno: 0,
                message: format!("Failed to parse address \"{rest}\""),
            }));
        }
        let bind = if req.host.contains(':') {
            format!("[{}]:{}", req.host, req.port)
        } else {
            format!("{}:{}", req.host, req.port)
        };
        // `php_network_bind_socket_to_local_addr()`: SO_REUSEADDR unless
        // `so_reuseaddr` is false, `so_reuseport`, then `listen(backlog)`.
        let sockopts = match SockOpts::from_context(req.context.as_ref(), XportKind::Tcp, true) {
            Ok(o) => o,
            Err(message) => return Ok(Err(XportError { errno: 0, message })),
        };
        let listener = std::net::ToSocketAddrs::to_socket_addrs(&bind)
            .and_then(|mut it| {
                it.next()
                    .ok_or_else(|| std::io::Error::from_raw_os_error(libc::EINVAL))
            })
            .and_then(|addr| {
                php_streams::sys::bind_ip_socket(
                    &addr,
                    libc::SOCK_STREAM,
                    sockopts.backlog,
                    &|fd| sockopts.apply_pre_bind(fd),
                )
                .map_err(std::io::Error::from_raw_os_error)
            });
        let listener = match listener {
            Ok(fd) => TcpListener::from(fd),
            Err(e) => return Ok(Err(io_error(&e))),
        };
        let _ = listener.set_nonblocking(true);
        let mut ops = TlsOps::new(Conn::Listener(listener), Some(default_timeout(vm)));
        ops.enable_on_connect = !plain_tcp;
        ops.method = method;
        ops.url_name = url_name;
        ops.persistent = req.persistent_id.is_some();
        // Share one SSL_CTX between accepted connections when a session
        // cache is configured (`STREAM_XPORT_OP_LISTEN`).
        if !plain_tcp
            && (sess::cache_enabled(vm, &opts, false)? || early::server_enabled(vm, &opts)?)
        {
            ops.ctx = ctx::create_ctx(vm, &opts, false, method & !IS_CLIENT, ops.persistent)?;
            flush(vm);
            if let Some(t) = take_exception() {
                return Err(t);
            }
            if ops.ctx.is_none() {
                return Ok(Err(XportError {
                    errno: 0,
                    message: String::new(),
                }));
            }
        }
        let mut s = new_stream(ops);
        s.orig_path = Some(req.address.as_bytes().to_vec());
        s.context.clone_from(&req.context);
        return Ok(Ok(s));
    }
    if req.flags & (STREAM_CLIENT_CONNECT | STREAM_CLIENT_ASYNC_CONNECT) == 0 {
        // Neither STREAM_CLIENT_CONNECT nor STREAM_CLIENT_ASYNC_CONNECT: an
        // unconnected socket (`php_stream_xport_create()` connects for
        // either flag).
        let Some(tcp) = unconnected_socket() else {
            return Ok(Err(io_error(&std::io::Error::last_os_error())));
        };
        let mut ops = TlsOps::new(Conn::Plain(tcp), Some(default_timeout(vm)));
        ops.method = method;
        ops.url_name = url_name;
        let mut s = new_stream(ops);
        s.context.clone_from(&req.context);
        return Ok(Ok(s));
    }
    let (tcp, number) = match connect(req) {
        Ok(t) => t,
        Err(e) => return Ok(Err(e)),
    };
    let _ = tcp.set_nonblocking(true);
    let mut ops = TlsOps::new(Conn::Plain(tcp), Some(default_timeout(vm)));
    ops.connect_timeout = req.timeout;
    ops.enable_on_connect = !plain_tcp;
    ops.method = method;
    ops.url_name = url_name;
    ops.persistent = req.persistent_id.is_some();
    if plain_tcp {
        let mut s = new_stream_numbered(ops, number);
        s.orig_path = Some(req.address.as_bytes().to_vec());
        s.context.clone_from(&req.context);
        return Ok(Ok(s));
    }
    ops.handshake = Some(Handshake {
        parent: session_stream(&opts),
        failure: "",
    });
    let mut s = new_stream_numbered(ops, number);
    s.orig_path = Some(req.address.as_bytes().to_vec());
    s.context.clone_from(&req.context);
    Ok(Ok(s))
}

/// An exception raised by configuration or by a PHP callback during the
/// handshake (`EG(exception)` once the function returns).
fn take_exception() -> Option<php_vm::ext::Throw> {
    sess::take_pending().or_else(php_ext_streams::bridge::take_pending)
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
    callbacks: bool,
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
                callbacks: o.callbacks,
            },
            None => SessionParent {
                is_tls: false,
                ctx: None,
                session: None,
                has_handle: false,
                callbacks: false,
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
        ops.early_state = early::initial_state(&s, ops.is_client, ops.early_send.is_some());
        ops.conn = Conn::Tls(s);
        true
    } else {
        warn(Code::CreateFailed, "SSL handle creation failure");
        false
    }
}

/// Prepares a fresh `SSL` handle: client SNI and connect/accept state
/// (`state_set` in `php_openssl_enable_crypto`).
fn prepare_ssl(vm: &mut Vm, ops: &TlsOps, opts: &SslOpts, ssl: &mut Ssl) -> VmResult<bool> {
    if ops.is_client {
        if !sess::apply_client_data(vm, ssl, opts)? {
            return Ok(false);
        }
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
        reneg::init(vm, ssl, opts)?;
        ssl.set_accept_state();
    }
    Ok(true)
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
        } else if ops.persistent && parent.callbacks {
            // GH-25172: the callbacks are request data a persistent stream
            // would outlive.
            warn(
                Code::PersistentNotSupported,
                "Supplied SSL session stream with callbacks is not supported for persistent streams",
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
            if !prepare_ssl(vm, ops, opts, &mut ssl)? {
                return Ok(false);
            }
            ops.ctx = Some(ctx.clone());
            return Ok(attach(ops, ssl));
        }
    }
    sess::callbacks_set();
    let Some(ctx) = ctx::create_ctx(vm, opts, ops.is_client, method_flags, ops.persistent)? else {
        return Ok(false);
    };
    ops.callbacks = sess::callbacks_set();
    if ops.is_client {
        match early::client_payload(vm, opts) {
            Ok(p) => ops.early_send = p,
            Err(()) => return Ok(false),
        }
    }
    let Ok(mut ssl) = Ssl::new(&ctx) else {
        warn(Code::CreateFailed, "SSL handle creation failure");
        return Ok(false);
    };
    if !prepare_ssl(vm, ops, opts, &mut ssl)? {
        return Ok(false);
    }
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
            let is_client = ops.is_client;
            let payload = ops.early_send.as_deref().unwrap_or_default();
            let (state, offset) = (&mut ops.early_state, &mut ops.early_offset);
            let r = php_ext_streams::bridge::enter(vm, || {
                early::handshake(ssl, is_client, state, payload, offset)
            });
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
    let callbacks = lops.callbacks;
    let parent = lops.ctx.clone().map(|ctx| SessionParent {
        is_tls: true,
        ctx: Some(ctx),
        session: None,
        has_handle: true,
        callbacks,
    });
    let mut conn = conn;
    let number = conn.resource_number();
    let Some(tcp) = dup_socket(&mut conn) else {
        return Ok(Err(XportError {
            errno: 0,
            message: "Cannot enable crypto".into(),
        }));
    };
    drop(conn);
    let mut ops = TlsOps::new(Conn::Plain(tcp), timeout);
    ops.connect_timeout = None;
    if enable_on_connect {
        ops.method = method & !IS_CLIENT;
        ops.handshake = Some(Handshake {
            parent,
            failure: "Cannot enable crypto",
        });
    }
    flush(vm);
    if let Some(t) = take_exception() {
        return Err(t);
    }
    let mut s = new_stream_numbered(ops, number);
    s.context.clone_from(&listener.context);
    Ok(Ok(s))
}

/// A handshake left for when the connection's stream is a resource.
pub(crate) struct Handshake {
    /// The `SSL_CTX` and session to start from: the `session_stream`
    /// option's of a client, the server's of an accepted connection.
    parent: Option<SessionParent>,
    /// The error the caller reports when the handshake fails.
    failure: &'static str,
}

/// The transports' [`php_ext_streams::xport::ConnectHook`]: the TLS
/// handshake `create` or `accept` left for when the connection's stream is
/// a resource. php-src enables crypto on connect on the stream it has
/// allocated (`STREAM_XPORT_OP_CONNECT`, `php_openssl_tcp_sockop_accept`),
/// so the PSK and session callbacks get that stream.
fn handshake(vm: &mut Vm, s: &mut Stream) -> VmResult<Result<(), ConnectError>> {
    let opts = SslOpts::of_stream(s);
    let id = s.id();
    let Some(ops) = s.ops_as_mut::<TlsOps>() else {
        return Ok(Ok(()));
    };
    let Some(h) = ops.handshake.take() else {
        return Ok(Ok(()));
    };
    let method = ops.method;
    sess::set_current(id);
    let ok =
        setup(vm, ops, &opts, method, h.parent.as_ref())? && enable(vm, ops, &opts, true)? >= 0;
    if !ok {
        warn(Code::ProtocolError, "Failed to enable crypto");
    }
    flush(vm);
    let thrown = take_exception();
    if ok {
        return thrown.map_or(Ok(Ok(())), Err);
    }
    Ok(Err(ConnectError {
        error: XportError {
            errno: 0,
            message: h.failure.into(),
        },
        thrown,
    }))
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
    sess::set_current(stream.id());
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
        if let Some(t) = take_exception() {
            return Err(t);
        }
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
    if let Some(t) = take_exception() {
        return Err(t);
    }
    if n < 0 {
        return Ok((Value::Bool(false), None));
    }
    let mut s = new_stream_numbered(ops, stream.resource_number());
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
    if ops.early_state == early::State::Done {
        if let Some(tls) = ops.tls() {
            t.insert(ZStr::from("early_data"), Value::string(early::status(tls)));
        }
    }
    if let Some(p) = ssl.selected_alpn_protocol() {
        t.insert(ZStr::from("alpn_protocol"), Value::Str(ZStr::from(p)));
    }
    Some(Value::Array(arr))
}

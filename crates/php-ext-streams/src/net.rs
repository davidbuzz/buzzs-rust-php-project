// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Socket functions: `fsockopen()`, `stream_socket_*()`.
//!
//! Clients go through the library's transports; servers, accepted
//! connections and socket pairs use [`FdSockOps`], a socket stream over a
//! raw descriptor.

use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::time::Duration;

use php_runtime::{Value, ZArray};
use php_streams::sockopts::{SockOpts, XportKind};
use php_streams::stream::{
    flags, CastAs, OpFailed, Stream, StreamOps, StreamOption, StreamState, OPT_ERR, OPT_NOTIMPL,
    OPT_OK,
};
use php_streams::transports;
use php_streams::value::Value as SValue;
use php_vm::ext::{Args, ModuleBuilder, Throw, VmResult};
use php_vm::Vm;

use php_streams::context::ContextRef;

use crate::bridge::{self, bytes};
use crate::res::{self, str_arg, stream_arg};
use crate::xport::{self, XportError, XportRequest};

/// A socket stream over a descriptor (`php_netstream_data_t`).
#[derive(Debug)]
pub struct FdSockOps {
    fd: Option<OwnedFd>,
    label: &'static str,
    timeout: Option<Duration>,
    timed_out: bool,
    blocked: bool,
}

impl FdSockOps {
    fn new(fd: OwnedFd, label: &'static str, timeout: Option<Duration>) -> Self {
        FdSockOps {
            fd: Some(fd),
            label,
            timeout,
            timed_out: false,
            blocked: true,
        }
    }

    fn raw(&self) -> Option<RawFd> {
        self.fd.as_ref().map(AsRawFd::as_raw_fd)
    }
}

/// Waits until `fd` is ready for `events`; `None` timeout waits forever.
fn wait_fd(fd: RawFd, events: i16, timeout: Option<Duration>) -> bool {
    let ms = timeout.map_or(-1, |t| i32::try_from(t.as_millis()).unwrap_or(i32::MAX));
    let mut p = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    // SAFETY: `p` is a valid pollfd for the duration of the call.
    let rc = unsafe { libc::poll(&raw mut p, 1, ms) };
    rc > 0
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

impl StreamOps for FdSockOps {
    fn label(&self) -> &'static str {
        self.label
    }

    fn read(&mut self, st: &mut StreamState, buf: &mut [u8]) -> Result<usize, OpFailed> {
        let fd = self.raw().ok_or(OpFailed)?;
        self.timed_out = false;
        let buffered = st.has_buffered_data;
        let mut flags = 0;
        if self.blocked {
            if buffered {
                flags = libc::MSG_DONTWAIT;
            } else if !wait_fd(fd, libc::POLLIN, self.timeout) {
                self.timed_out = true;
                return Err(OpFailed);
            }
        }
        // SAFETY: `buf` is a valid writable buffer of `buf.len()` bytes.
        let n = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), flags) };
        if n < 0 {
            let e = errno();
            if e == libc::EAGAIN || e == libc::EWOULDBLOCK || e == libc::EINTR {
                return Ok(0);
            }
            st.eof = true;
            return Err(OpFailed);
        }
        if n == 0 {
            st.eof = true;
        }
        Ok(usize::try_from(n).unwrap_or(0))
    }

    fn can_write(&self) -> bool {
        true
    }

    fn write(&mut self, _st: &mut StreamState, buf: &[u8]) -> Result<usize, OpFailed> {
        let fd = self.raw().ok_or(OpFailed)?;
        // SAFETY: `buf` is a valid readable buffer of `buf.len()` bytes.
        let n = unsafe { libc::send(fd, buf.as_ptr().cast(), buf.len(), libc::MSG_NOSIGNAL) };
        if n < 0 {
            let e = errno();
            if e == libc::EAGAIN || e == libc::EWOULDBLOCK {
                return Ok(0);
            }
            return Err(OpFailed);
        }
        Ok(usize::try_from(n).unwrap_or(0))
    }

    fn close(&mut self, _st: &mut StreamState, preserve: bool) -> i32 {
        if let Some(fd) = self.fd.take() {
            if preserve {
                let _ = fd.into_raw_fd();
            }
        }
        0
    }

    fn set_option(&mut self, st: &mut StreamState, opt: &mut StreamOption<'_>) -> i64 {
        let Some(fd) = self.raw() else {
            return OPT_ERR;
        };
        match opt {
            StreamOption::Blocking(b) => {
                let old = i64::from(self.blocked);
                if php_streams::sys::set_blocking(fd, *b).is_err() {
                    return OPT_ERR;
                }
                self.blocked = *b;
                old
            }
            StreamOption::ReadTimeout(sec, usec) => {
                let micros = sec.saturating_mul(1_000_000).saturating_add(*usec);
                self.timeout = u64::try_from(micros).ok().map(Duration::from_micros);
                self.timed_out = false;
                OPT_OK
            }
            StreamOption::CheckLiveness => {
                if wait_fd(fd, libc::POLLIN | libc::POLLPRI, Some(Duration::ZERO)) {
                    let mut b = [0u8; 1];
                    // SAFETY: one-byte valid buffer.
                    let n = unsafe { libc::recv(fd, b.as_mut_ptr().cast(), 1, libc::MSG_PEEK) };
                    if n == 0 || (n < 0 && errno() != libc::EWOULDBLOCK) {
                        return OPT_ERR;
                    }
                }
                OPT_OK
            }
            StreamOption::MetaData(out) => {
                out.push(("timed_out".into(), SValue::Bool(self.timed_out)));
                out.push(("blocked".into(), SValue::Bool(self.blocked)));
                out.push(("eof".into(), SValue::Bool(st.eof)));
                OPT_OK
            }
            _ => OPT_NOTIMPL,
        }
    }

    fn cast(&mut self, _st: &mut StreamState, _kind: CastAs, _report: bool) -> Option<RawFd> {
        self.raw()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn sock_stream(fd: OwnedFd, label: &'static str, timeout: Option<Duration>) -> Stream {
    let mut s = Stream::new(Box::new(FdSockOps::new(fd, label, timeout)), "r+");
    s.add_flags(flags::NO_SEEK);
    s
}

fn default_timeout(vm: &Vm) -> Duration {
    Duration::from_secs(u64::try_from(vm.ini_long("default_socket_timeout")).unwrap_or(60))
}

/// `PHP_TIMEOUT_ULL_MAX / 1000000.0`.
const TIMEOUT_MAX: f64 = 18_446_744_073_709.55;

/// Converts a validated timeout in seconds; negative or huge values wait
/// forever (`tv_pointer = NULL`).
fn to_duration(t: f64) -> Option<Duration> {
    if !(0.0..TIMEOUT_MAX).contains(&t) {
        return None;
    }
    Duration::try_from_secs_f64(t).ok()
}

/// A `?float $timeout` argument of `stream_socket_client()` /
/// `stream_socket_accept()` ("must be a finite value").
fn finite_timeout(vm: &mut Vm, a: &Args, n: usize) -> VmResult<Option<Duration>> {
    if a.opt(n).is_none() {
        return Ok(Some(default_timeout(vm)));
    }
    let t = a.float(n);
    if !t.is_finite() {
        return Err(a.value_error(vm, n + 1, "must be a finite value"));
    }
    Ok(to_duration(t))
}

fn set_ref(a: &mut Args, n: usize, v: Value) {
    if a.has(n) {
        a.set_ref(n, v);
    }
}

/// The `ssl` options of a context as PHP values.
fn ssl_options(ctx: Option<&ContextRef>) -> Value {
    let Some(c) = ctx else {
        return Value::Array(ZArray::new());
    };
    if let Some(cell) = res::ctx_cell_of_lib(c) {
        if let Some(v) = cell.options.borrow().get_str(b"ssl") {
            return v.clone();
        }
    }
    Value::Array(ZArray::new())
}

/// `php_stream_xport_create()`: a client or server stream for `address`,
/// set up completely (a stream that is no resource: the TLS handshake's
/// callbacks get `null` for it).
pub(crate) fn xport_create(
    vm: &mut Vm,
    address: &str,
    timeout: Option<Duration>,
    server: bool,
    flags: i64,
    persistent_id: Option<String>,
    ctx: Option<ContextRef>,
) -> VmResult<Result<Stream, XportError>> {
    let (r, hook) = xport_open(vm, address, timeout, server, flags, persistent_id, ctx)?;
    let Some(hook) = hook else {
        return Ok(r);
    };
    let mut s = match r {
        Ok(s) => s,
        Err(e) => return Ok(Err(e)),
    };
    let r = hook(vm, &mut s);
    if !matches!(r, Ok(Ok(()))) {
        s.close();
    }
    match r? {
        Ok(()) => Ok(Ok(s)),
        Err(xport::ConnectError {
            thrown: Some(t), ..
        }) => {
            // C reports the connect failure before the exception surfaces.
            vm.php_warning(&format!("Unable to connect to {address} (Unknown error)"))?;
            Err(t)
        }
        Err(e) => Ok(Err(e.error)),
    }
}

/// A client connection from [`xport_connect`].
pub(crate) enum Connected {
    /// The stream resource.
    Stream(Value),
    /// The connection failed: the error, and the exception to throw once
    /// the caller has reported the failure (a PHP callback of the
    /// handshake threw).
    Failed(XportError, Option<Throw>),
}

/// `php_stream_xport_create()` for a client that connects
/// (`stream_socket_client()`, `fsockopen()`): the stream is a resource
/// before the transport finishes connecting (the TLS handshake of `ssl://`
/// & co., whose callbacks get the stream, see [`xport::ConnectHook`]), and
/// is closed again when that fails.
pub(crate) fn xport_connect(
    vm: &mut Vm,
    address: &str,
    timeout: Option<Duration>,
    flags: i64,
    persistent_id: Option<String>,
    ctx: Option<ContextRef>,
) -> VmResult<Connected> {
    let pid = persistent_id.clone();
    let (r, hook) = xport_open(vm, address, timeout, false, flags, persistent_id, ctx)?;
    let s = match r {
        Ok(s) => s,
        Err(e) => return Ok(Connected::Failed(e, None)),
    };
    let v = res::new_persistent(s, pid);
    let Some(hook) = hook else {
        return Ok(Connected::Stream(v));
    };
    Ok(match finish_connect(vm, &v, hook)? {
        Ok(()) => Connected::Stream(v),
        Err(e) => Connected::Failed(e.error, e.thrown),
    })
}

/// Runs a transport's [`xport::ConnectHook`] on the stream of resource
/// `v`; a failure closes the stream (`php_stream_close()`).
fn finish_connect(
    vm: &mut Vm,
    v: &Value,
    hook: xport::ConnectHook,
) -> VmResult<Result<(), xport::ConnectError>> {
    let Some(cell) = (match v {
        Value::Resource(r) => res::cell_of(r),
        _ => None,
    }) else {
        return Ok(Ok(()));
    };
    let r = match cell.get() {
        Some(mut s) => hook(vm, &mut s),
        None => Ok(Ok(())),
    };
    if !matches!(r, Ok(Ok(()))) {
        res::close_failed(v);
    }
    r
}

/// [`xport_create`] up to what is left to do once the stream is a
/// resource: the transport's [`xport::ConnectHook`], for a client.
fn xport_open(
    vm: &mut Vm,
    address: &str,
    timeout: Option<Duration>,
    server: bool,
    flags: i64,
    persistent_id: Option<String>,
    ctx: Option<ContextRef>,
) -> VmResult<(Result<Stream, XportError>, Option<xport::ConnectHook>)> {
    let hook = if server {
        None
    } else {
        xport::find(&xport::split_scheme(address).0).and_then(|t| t.connect)
    };
    let (scheme, rest) = xport::split_scheme(address);
    let kind = XportKind::of_scheme(&scheme);
    let opts = match SockOpts::from_context(ctx.as_ref(), kind, server) {
        Ok(o) => o,
        Err(message) => return Ok((Err(XportError { errno: 0, message }), None)),
    };
    let r = xport_create_inner(
        vm,
        address,
        &scheme,
        rest,
        timeout,
        server,
        flags,
        persistent_id,
        ctx,
        &opts,
    )?;
    let r = r.map(|mut s| {
        // Options settable on the created socket (keepalive, linger,
        // buffers); bind-time ones were applied before `bind()` where this
        // module creates the socket.
        if let Some(fd) = s.cast(CastAs::FdForSelect, false) {
            opts.apply(fd);
        }
        s
    });
    Ok((r, hook))
}

/// [`xport_create`] after the `socket` context options were validated.
#[allow(clippy::too_many_arguments)]
fn xport_create_inner(
    vm: &mut Vm,
    address: &str,
    scheme: &str,
    rest: &str,
    timeout: Option<Duration>,
    server: bool,
    flags: i64,
    persistent_id: Option<String>,
    ctx: Option<ContextRef>,
    opts: &SockOpts,
) -> VmResult<Result<Stream, XportError>> {
    let scheme = scheme.to_string();
    if let Some(t) = xport::find(&scheme) {
        let (host, port) = xport::parse_host_port(rest).unwrap_or_else(|| (rest.to_string(), 0));
        let req = XportRequest {
            scheme: scheme.clone(),
            address: address.to_string(),
            host,
            port,
            timeout,
            server,
            flags,
            persistent_id,
            ssl_options: ssl_options(ctx.as_ref()),
            context: ctx.clone(),
        };
        let r = (t.create)(vm, &req);
        // Warnings the transport queued (php_network_getaddresses()).
        bridge::flush_diags(vm);
        let r = r?;
        if let Ok(s) = &r {
            if server {
                xport::note_server(s.id(), &scheme);
            }
        }
        return Ok(r.map(|mut s| {
            if s.context.is_none() {
                s.context = ctx;
            }
            s
        }));
    }
    if !server && flags & 4 == 0 && matches!(scheme.as_str(), "tcp" | "udp" | "unix") {
        // No STREAM_CLIENT_CONNECT: an unconnected socket (xp_socket parses
        // the address only when connecting).
        let domain = if scheme == "unix" {
            libc::AF_UNIX
        } else {
            libc::AF_INET
        };
        let ty = if scheme == "udp" {
            libc::SOCK_DGRAM
        } else {
            libc::SOCK_STREAM
        };
        // SAFETY: plain socket(2) call.
        let fd = unsafe { libc::socket(domain, ty, 0) };
        if fd >= 0 {
            // SAFETY: fresh descriptor.
            let owned = unsafe { OwnedFd::from_raw_fd(fd) };
            let label = match scheme.as_str() {
                "udp" => "udp_socket",
                "unix" => "unix_socket",
                _ => "tcp_socket",
            };
            let mut s = sock_stream(owned, label, timeout);
            s.context = ctx;
            return Ok(Ok(s));
        }
    }
    if server {
        let r = bridge::run(vm, || server_socket(&scheme, rest, address, opts))?;
        return Ok(r.map(|mut s| {
            s.context = ctx;
            s
        }));
    }
    let r = bridge::run(vm, || {
        transports::socket_client(address.as_bytes(), timeout)
    })?;
    Ok(r.map(|mut s| {
        s.context = ctx;
        s
    })
    .map_err(|e| XportError {
        errno: e.errno,
        message: e.message,
    }))
}

fn fsockopen_impl(vm: &mut Vm, a: &mut Args, persistent: bool) -> VmResult<Value> {
    let host = str_arg(a, 0);
    let port = a.int(1);
    let timeout = if a.opt(4).is_none() {
        Some(default_timeout(vm))
    } else {
        {
            let t = a.float(4);
            #[allow(clippy::float_cmp)]
            let minus_one = t == -1.0;
            if !minus_one && !(0.0..=TIMEOUT_MAX).contains(&t) {
                return Err(a.value_error(vm, 5, "must be -1 or between 0 and 18446744073709"));
            }
            if minus_one {
                None
            } else {
                to_duration(t)
            }
        }
    };
    let host_s = String::from_utf8_lossy(&host).into_owned();
    let target = if port > 0 {
        if host_s.contains(':') && !host_s.contains("://") && !host_s.starts_with('[') {
            format!("[{host_s}]:{port}")
        } else {
            format!("{host_s}:{port}")
        }
    } else {
        host_s.clone()
    };
    set_ref(a, 2, Value::Int(0));
    set_ref(a, 3, bytes(""));
    let ctx = None;
    let pid = persistent.then(|| format!("pfsockopen__{host_s}:{port}"));
    if let Some(v) = pid.as_deref().and_then(res::persistent_stream) {
        return Ok(v);
    }
    match xport_connect(vm, &target, timeout, 4, pid, ctx)? {
        Connected::Stream(v) => Ok(v),
        Connected::Failed(e, thrown) => {
            // `%s` stops at an embedded NUL.
            let shown = host_s.split('\0').next().unwrap_or_default();
            vm.php_warning(&format!(
                "Unable to connect to {shown}:{port} ({})",
                e.shown()
            ))?;
            if let Some(t) = thrown {
                return Err(t);
            }
            set_ref(a, 2, Value::Int(i64::from(e.errno)));
            if !e.message.is_empty() {
                set_ref(a, 3, bytes(e.message.as_bytes()));
            }
            Ok(Value::Bool(false))
        }
    }
}

fn fsockopen(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    fsockopen_impl(vm, a, false)
}

fn pfsockopen(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    fsockopen_impl(vm, a, true)
}

fn stream_socket_client(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let addr = String::from_utf8_lossy(&str_arg(a, 0)).into_owned();
    let timeout = finite_timeout(vm, a, 3)?;
    let flags = if a.has(4) { a.int(4) } else { 4 };
    let ctx = res::context_arg(vm, a, 5, flags & 16 != 0)?;
    set_ref(a, 1, Value::Int(0));
    set_ref(a, 2, bytes(""));
    let persistent = flags & 1 != 0;
    let pid = persistent.then(|| format!("stream_socket_client__{addr}"));
    if let Some(v) = pid.as_deref().and_then(res::persistent_stream) {
        return Ok(v);
    }
    match xport_connect(vm, &addr, timeout, flags, pid, ctx)? {
        Connected::Stream(v) => Ok(v),
        Connected::Failed(e, thrown) => {
            vm.php_warning(&format!(
                "Unable to connect to {} ({})",
                addslashes(&addr),
                e.shown()
            ))?;
            if let Some(t) = thrown {
                return Err(t);
            }
            set_ref(a, 1, Value::Int(i64::from(e.errno)));
            if !e.message.is_empty() {
                set_ref(a, 2, bytes(e.message.as_bytes()));
            }
            Ok(Value::Bool(false))
        }
    }
}

/// `php_addslashes()` for messages.
fn addslashes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\0' => out.push_str("\\0"),
            '\'' | '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// A listening socket for the built-in transports.
fn server_socket(
    scheme: &str,
    rest: &str,
    address: &str,
    opts: &SockOpts,
) -> Result<Stream, XportError> {
    let io_err = |e: &std::io::Error| XportError {
        errno: e.raw_os_error().unwrap_or(0),
        message: e
            .raw_os_error()
            .map_or_else(|| e.to_string(), php_streams::sys::strerror),
    };
    let r: std::io::Result<(OwnedFd, &'static str)> = match scheme {
        "tcp" | "udp" => {
            let Some((host, port)) = xport::parse_host_port(rest) else {
                return Err(XportError {
                    errno: 0,
                    message: format!("Failed to parse address \"{rest}\""),
                });
            };
            let bind = if host.contains(':') {
                format!("[{host}]:{port}")
            } else {
                format!("{host}:{port}")
            };
            let (ty, label) = if scheme == "tcp" {
                (libc::SOCK_STREAM, "tcp_socket")
            } else {
                (libc::SOCK_DGRAM, "udp_socket")
            };
            std::net::ToSocketAddrs::to_socket_addrs(&bind)
                .and_then(|mut it| {
                    it.next()
                        .ok_or_else(|| std::io::Error::from_raw_os_error(libc::EINVAL))
                })
                .and_then(|addr| {
                    php_streams::sys::bind_ip_socket(&addr, ty, opts.backlog, &|fd| {
                        opts.apply_pre_bind(fd);
                    })
                    .map_err(std::io::Error::from_raw_os_error)
                })
                .map(|fd| (fd, label))
        }
        "udg" => transports::unix_socket_addr(rest.as_bytes())
            .and_then(|a| std::os::unix::net::UnixDatagram::bind_addr(&a))
            .map(|l| (OwnedFd::from(l), "udg_socket")),
        "unix" => transports::unix_socket_addr(rest.as_bytes())
            .and_then(|a| std::os::unix::net::UnixListener::bind_addr(&a))
            .map(|l| (OwnedFd::from(l), "unix_socket")),
        _ => {
            let shown: String = scheme.chars().take(31).collect();
            return Err(XportError {
                errno: 0,
                message: format!(
                    "Unable to find the socket transport \"{shown}\" - did you forget to enable it when you configured PHP?"
                ),
            });
        }
    };
    r.map(|(fd, label)| {
        let mut s = sock_stream(fd, label, None);
        s.orig_path = Some(address.as_bytes().to_vec());
        s
    })
    .map_err(|e| io_err(&e))
}

fn stream_socket_server(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let addr = String::from_utf8_lossy(&str_arg(a, 0)).into_owned();
    let flags = if a.has(3) { a.int(3) } else { 12 };
    let ctx = res::context_arg(vm, a, 4, false)?;
    set_ref(a, 1, Value::Int(0));
    set_ref(a, 2, bytes(""));
    match xport_create(vm, &addr, None, true, flags, None, ctx)? {
        Ok(s) => Ok(res::new_stream(s, None)),
        Err(e) => {
            vm.php_warning(&format!("Unable to connect to {addr} ({})", e.shown()))?;
            set_ref(a, 1, Value::Int(i64::from(e.errno)));
            if !e.message.is_empty() {
                set_ref(a, 2, bytes(e.message.as_bytes()));
            }
            Ok(Value::Bool(false))
        }
    }
}

fn stream_socket_enable_crypto(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cell = stream_arg(vm, a, 0)?;
    let enable = a.bool(1);
    let method = a.opt(2).map(|_| a.int(2));
    let session = match a.opt(3) {
        Some(_) => Some(stream_arg(vm, a, 3)?),
        None => None,
    };
    let Some(hook) = xport::crypto_hooks().enable else {
        vm.php_warning("SSL/TLS support is not available in this build")?;
        return Ok(Value::Bool(false));
    };
    let Some(mut s) = cell.take() else {
        return Ok(Value::Bool(false));
    };
    let mut sess = session.as_ref().and_then(|c| c.take());
    // The hook's warnings are emitted once the stream is back in its
    // resource (a user error handler may use or close it).
    let r = bridge::deferring_flush(|| hook(vm, &mut s, enable, method, sess.as_mut()));
    if let (Some(c), Some(ss)) = (session.as_ref(), sess) {
        c.replace_stream(ss);
    }
    let r = match r {
        Ok((v, replacement)) => {
            match replacement {
                Some(new) => {
                    let mut old = s;
                    old.add_flags(flags::NO_CLOSE);
                    drop(old);
                    cell.replace_stream(new);
                }
                None => {
                    cell.replace_stream(s);
                }
            }
            Ok(v)
        }
        Err(t) => {
            cell.replace_stream(s);
            Err(t)
        }
    };
    bridge::flush_diags(vm);
    if let Some(t) = bridge::take_pending() {
        return Err(t);
    }
    r
}

fn stream_socket_get_crypto_status(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cell = stream_arg(vm, a, 0)?;
    let st = xport::crypto_hooks()
        .status
        .and_then(|f| cell.get().map(|mut s| f(&mut s)))
        .unwrap_or(0);
    Ok(Value::Int(st))
}

fn sockaddr_name(fd: RawFd, remote: bool) -> Option<String> {
    // SAFETY: zeroed sockaddr_storage is a valid out-parameter.
    let mut ss: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut len = libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_storage>()).ok()?;
    let p = std::ptr::addr_of_mut!(ss).cast::<libc::sockaddr>();
    // SAFETY: `p`/`len` describe `ss`, which outlives the call.
    let rc = unsafe {
        if remote {
            libc::getpeername(fd, p, &raw mut len)
        } else {
            libc::getsockname(fd, p, &raw mut len)
        }
    };
    if rc != 0 {
        return None;
    }
    match i32::from(ss.ss_family) {
        libc::AF_INET => {
            // SAFETY: family is AF_INET so the storage holds a sockaddr_in.
            let sin = unsafe { &*std::ptr::addr_of!(ss).cast::<libc::sockaddr_in>() };
            let ip = std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
            Some(format!("{ip}:{}", u16::from_be(sin.sin_port)))
        }
        libc::AF_INET6 => {
            // SAFETY: family is AF_INET6 so the storage holds a sockaddr_in6.
            let sin = unsafe { &*std::ptr::addr_of!(ss).cast::<libc::sockaddr_in6>() };
            let ip = std::net::Ipv6Addr::from(sin.sin6_addr.s6_addr);
            Some(format!("[{ip}]:{}", u16::from_be(sin.sin6_port)))
        }
        libc::AF_UNIX => {
            // SAFETY: family is AF_UNIX so the storage holds a sockaddr_un.
            let sun = unsafe { &*std::ptr::addr_of!(ss).cast::<libc::sockaddr_un>() };
            let path: Vec<u8> = sun
                .sun_path
                .iter()
                .take_while(|&&c| c != 0)
                .map(|&c| c.to_ne_bytes()[0])
                .collect();
            Some(String::from_utf8_lossy(&path).into_owned())
        }
        _ => None,
    }
}

fn stream_socket_accept(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cell = stream_arg(vm, a, 0)?;
    let timeout = finite_timeout(vm, a, 1)?;
    let fd = cell
        .get()
        .and_then(|mut s| s.cast(CastAs::FdForSelect, false));
    let Some(fd) = fd else {
        return Ok(Value::Bool(false));
    };
    if !wait_fd(fd, libc::POLLIN, timeout) {
        vm.php_warning("Accept failed: Connection timed out")?;
        return Ok(Value::Bool(false));
    }
    // SAFETY: plain accept on a listening descriptor; null address args.
    let nfd = unsafe { libc::accept(fd, std::ptr::null_mut(), std::ptr::null_mut()) };
    if nfd < 0 {
        let e = errno();
        vm.php_warning(&format!("Accept failed: {}", php_streams::sys::strerror(e)))?;
        return Ok(Value::Bool(false));
    }
    // SAFETY: `nfd` is a fresh descriptor owned by nobody else.
    let owned = unsafe { OwnedFd::from_raw_fd(nfd) };
    let label = cell.get().map_or("tcp_socket", |s| s.ops_label());
    if a.has(2) {
        let peer = sockaddr_name(nfd, true).map_or(Value::Null, |p| bytes(p.as_bytes()));
        a.set_ref(2, peer);
    }
    let listener_ctx = cell.get().and_then(|s| s.context.clone());
    if let Ok(o) = SockOpts::from_context(listener_ctx.as_ref(), XportKind::Tcp, true) {
        if label.starts_with("tcp") {
            o.for_accepted().apply(nfd);
        }
    }
    let mut conn = sock_stream(owned, label, Some(default_timeout(vm)));
    let transport = xport::server_transport(cell.id());
    if let Some(hook) = transport.and_then(|t| t.accept) {
        let Some(mut listener) = cell.take() else {
            return Ok(Value::Bool(false));
        };
        conn.context.clone_from(&listener.context);
        let r = hook(vm, &mut listener, conn, timeout);
        cell.replace_stream(listener);
        let accepted = match r? {
            // The server handshake runs once the connection is a resource
            // (`php_openssl_tcp_sockop_accept()` allocates it first).
            Ok(stream) => {
                let client = res::new_stream(stream, None);
                match transport.and_then(|t| t.connect) {
                    Some(connect) => match finish_connect(vm, &client, connect)? {
                        Ok(()) => Ok(client),
                        Err(xport::ConnectError {
                            thrown: Some(t), ..
                        }) => return Err(t),
                        Err(e) => Err(e.error),
                    },
                    None => Ok(client),
                }
            }
            Err(e) => Err(e),
        };
        return match accepted {
            Ok(client) => Ok(client),
            Err(e) => {
                vm.php_warning(&format!("Accept failed: {}", e.shown()))?;
                Ok(Value::Bool(false))
            }
        };
    }
    Ok(res::new_stream(conn, None))
}

fn stream_socket_get_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cell = stream_arg(vm, a, 0)?;
    let remote = a.bool(1);
    let fd = cell
        .get()
        .and_then(|mut s| s.cast(CastAs::FdForSelect, false));
    let name = fd.and_then(|fd| sockaddr_name(fd, remote));
    Ok(match name {
        Some(n) if !n.is_empty() => bytes(n.as_bytes()),
        _ => Value::Bool(false),
    })
}

fn stream_socket_shutdown(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cell = stream_arg(vm, a, 0)?;
    let how = a.int(1);
    let how = match how {
        0 => libc::SHUT_RD,
        1 => libc::SHUT_WR,
        2 => libc::SHUT_RDWR,
        _ => {
            return Err(a.value_error(
                vm,
                2,
                "must be one of STREAM_SHUT_RD, STREAM_SHUT_WR, or STREAM_SHUT_RDWR",
            ))
        }
    };
    let fd = cell
        .get()
        .and_then(|mut s| s.cast(CastAs::FdForSelect, false));
    // SAFETY: shutdown on a descriptor owned by the stream.
    let ok = fd.is_some_and(|fd| unsafe { libc::shutdown(fd, how) } == 0);
    Ok(Value::Bool(ok))
}

fn stream_socket_pair(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let domain = i32::try_from(a.int(0)).unwrap_or(-1);
    let ty = i32::try_from(a.int(1)).unwrap_or(-1);
    let proto = i32::try_from(a.int(2)).unwrap_or(-1);
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` has room for the two descriptors.
    let rc = unsafe { libc::socketpair(domain, ty, proto, fds.as_mut_ptr()) };
    if rc != 0 {
        let e = errno();
        vm.php_warning(&format!(
            "Failed to create sockets: [{e}]: {}",
            php_streams::sys::strerror(e)
        ))?;
        return Ok(Value::Bool(false));
    }
    let mut arr = ZArray::new();
    for fd in fds {
        // SAFETY: fresh descriptors from socketpair.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        let _ = arr.make_mut().push(res::new_stream(
            sock_stream(owned, "generic_socket", Some(default_timeout(vm))),
            None,
        ));
    }
    Ok(Value::Array(arr))
}

fn stream_socket_recvfrom(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cell = stream_arg(vm, a, 0)?;
    let len = a.int(1);
    if len <= 0 {
        return Err(a.value_error(vm, 2, "must be greater than 0"));
    }
    let flags = i32::try_from(a.int(2)).unwrap_or(0);
    set_ref(a, 3, Value::Null);
    let fd = cell
        .get()
        .and_then(|mut s| s.cast(CastAs::FdForSelect, false));
    let Some(fd) = fd else {
        return Ok(Value::Bool(false));
    };
    let mut buf = vec![0u8; usize::try_from(len).unwrap_or(0)];
    // SAFETY: `buf` is valid for `buf.len()` bytes.
    let n = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), flags) };
    if n < 0 {
        return Ok(Value::Bool(false));
    }
    buf.truncate(usize::try_from(n).unwrap_or(0));
    if a.has(3) {
        let peer = sockaddr_name(fd, true).map_or(Value::Null, |p| bytes(p.as_bytes()));
        a.set_ref(3, peer);
    }
    Ok(bytes(buf))
}

fn stream_socket_sendto(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cell = stream_arg(vm, a, 0)?;
    let data = str_arg(a, 1);
    let flags = i32::try_from(a.int(2)).unwrap_or(0);
    let fd = cell
        .get()
        .and_then(|mut s| s.cast(CastAs::FdForSelect, false));
    let Some(fd) = fd else {
        return Ok(Value::Bool(false));
    };
    let target = if a.has(3) { str_arg(a, 3) } else { Vec::new() };
    if !target.is_empty() {
        let Some(addr) = parse_network_address(&target) else {
            vm.php_warning(&format!(
                "Failed to parse `{}' into a valid network address",
                String::from_utf8_lossy(&target)
            ))?;
            return Ok(Value::Bool(false));
        };
        let n = php_streams::sys::sendto(fd, &data, flags | libc::MSG_NOSIGNAL, &addr);
        return Ok(n.map_or(Value::Bool(false), |n| {
            Value::Int(i64::try_from(n).unwrap_or(0))
        }));
    }
    // SAFETY: `data` is valid for `data.len()` bytes.
    let n = unsafe {
        libc::send(
            fd,
            data.as_ptr().cast(),
            data.len(),
            flags | libc::MSG_NOSIGNAL,
        )
    };
    Ok(if n < 0 {
        Value::Bool(false)
    } else {
        Value::Int(i64::try_from(n).unwrap_or(0))
    })
}

/// `php_network_parse_network_address_with_port()`: `host:port` or
/// `[v6]:port` with a port in `0..=65535` (`strtol`).
fn parse_network_address(addr: &[u8]) -> Option<std::net::SocketAddr> {
    let (host, port) = if addr.first() == Some(&b'[') {
        let close = addr[1..].iter().position(|&c| c == b']')? + 1;
        if addr.get(close + 1) != Some(&b':') {
            return None;
        }
        (&addr[1..close], &addr[close + 2..])
    } else {
        let colon = addr.iter().position(|&c| c == b':')?;
        (&addr[..colon], &addr[colon + 1..])
    };
    let (port, _) = php_streams::util::strtol(port);
    let port = u16::try_from(port).ok()?;
    let host = std::str::from_utf8(host).ok()?;
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Some(std::net::SocketAddr::new(ip, port));
    }
    std::net::ToSocketAddrs::to_socket_addrs(&(host, port))
        .ok()?
        .next()
}

/// Registers the functions and `STREAM_*` socket constants.
pub fn register(m: &mut ModuleBuilder<'_>) {
    m.function_static(
        "fsockopen(string $hostname, int $port = -1, &$error_code = null, &$error_message = null, ?float $timeout = null)",
        fsockopen,
    )
    .function_static(
        "pfsockopen(string $hostname, int $port = -1, &$error_code = null, &$error_message = null, ?float $timeout = null)",
        pfsockopen,
    )
    .function_static(
        "stream_socket_client(string $address, &$error_code = null, &$error_message = null, ?float $timeout = null, int $flags = STREAM_CLIENT_CONNECT, $context = null)",
        stream_socket_client,
    )
    .function_static(
        "stream_socket_server(string $address, &$error_code = null, &$error_message = null, int $flags = STREAM_SERVER_BIND | STREAM_SERVER_LISTEN, $context = null)",
        stream_socket_server,
    )
    .function_static(
        "stream_socket_accept($socket, ?float $timeout = null, &$peer_name = null)",
        stream_socket_accept,
    )
    .function_static("stream_socket_get_name($socket, bool $remote): string|false", stream_socket_get_name)
    .function_static(
        "stream_socket_enable_crypto($stream, bool $enable, ?int $crypto_method = null, $session_stream = null): int|bool",
        stream_socket_enable_crypto,
    )
    .function_static("stream_socket_get_crypto_status($stream): int", stream_socket_get_crypto_status)
    .function_static("stream_socket_shutdown($stream, int $mode): bool", stream_socket_shutdown)
    .function_static(
        "stream_socket_pair(int $domain, int $type, int $protocol, $context = null): array|false",
        stream_socket_pair,
    )
    .function_static(
        "stream_socket_recvfrom($socket, int $length, int $flags = 0, &$address = null): string|false",
        stream_socket_recvfrom,
    )
    .function_static(
        "stream_socket_sendto($socket, string $data, int $flags = 0, string $address = \"\"): int|false",
        stream_socket_sendto,
    );
    for (n, v) in [
        ("STREAM_PF_INET", i64::from(libc::AF_INET)),
        ("STREAM_PF_INET6", i64::from(libc::AF_INET6)),
        ("STREAM_PF_UNIX", i64::from(libc::AF_UNIX)),
        ("STREAM_IPPROTO_IP", 0),
        ("STREAM_IPPROTO_TCP", i64::from(libc::IPPROTO_TCP)),
        ("STREAM_IPPROTO_UDP", i64::from(libc::IPPROTO_UDP)),
        ("STREAM_IPPROTO_ICMP", i64::from(libc::IPPROTO_ICMP)),
        ("STREAM_IPPROTO_RAW", i64::from(libc::IPPROTO_RAW)),
        ("STREAM_SOCK_STREAM", i64::from(libc::SOCK_STREAM)),
        ("STREAM_SOCK_DGRAM", i64::from(libc::SOCK_DGRAM)),
        ("STREAM_SOCK_RAW", i64::from(libc::SOCK_RAW)),
        ("STREAM_SOCK_SEQPACKET", i64::from(libc::SOCK_SEQPACKET)),
        ("STREAM_SOCK_RDM", i64::from(libc::SOCK_RDM)),
        ("STREAM_PEEK", i64::from(libc::MSG_PEEK)),
        ("STREAM_OOB", i64::from(libc::MSG_OOB)),
        ("STREAM_SERVER_BIND", 4),
        ("STREAM_SERVER_LISTEN", 8),
        ("STREAM_CLIENT_PERSISTENT", 1),
        ("STREAM_CLIENT_ASYNC_CONNECT", 2),
        ("STREAM_CLIENT_CONNECT", 4),
        ("STREAM_SHUT_RD", 0),
        ("STREAM_SHUT_WR", 1),
        ("STREAM_SHUT_RDWR", 2),
    ] {
        m.constant(n, Value::Int(v));
    }
}

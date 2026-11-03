//! Socket functions: `fsockopen()`, `stream_socket_*()`.
//!
//! Clients go through the library's transports; servers, accepted
//! connections and socket pairs use [`FdSockOps`], a socket stream over a
//! raw descriptor.

use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::time::Duration;

use php_runtime::{Value, ZArray};
use php_streams::stream::{
    flags, CastAs, OpFailed, Stream, StreamOps, StreamOption, StreamState, OPT_ERR,
    OPT_NOTIMPL, OPT_OK,
};
use php_streams::transports;
use php_streams::value::Value as SValue;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::bridge::{self, bytes};
use crate::res::{self, str_arg, stream_arg};

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
    let rc = unsafe { libc::poll(&mut p, 1, ms) };
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
        if self.blocked && !wait_fd(fd, libc::POLLIN, self.timeout) {
            self.timed_out = true;
            return Ok(0);
        }
        // SAFETY: `buf` is a valid writable buffer of `buf.len()` bytes.
        let n = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
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

fn timeout_arg(a: &Args, n: usize, vm: &Vm) -> Option<Duration> {
    match a.opt(n) {
        None => Some(default_timeout(vm)),
        Some(_) => {
            let t = a.float(n);
            if t < 0.0 {
                None
            } else {
                Some(Duration::from_secs_f64(t))
            }
        }
    }
}

fn set_ref(a: &mut Args, n: usize, v: Value) {
    if a.has(n) {
        a.set_ref(n, v);
    }
}

fn fsockopen_impl(vm: &mut Vm, a: &mut Args, persistent: bool) -> VmResult<Value> {
    let host = str_arg(a, 0);
    let port = a.int(1);
    let timeout = timeout_arg(a, 4, vm);
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
    let r = bridge::run(vm, || transports::socket_client(target.as_bytes(), timeout))?;
    match r {
        Ok(s) => Ok(res::new_stream_ex(s, None, persistent)),
        Err(e) => {
            vm.php_warning(&format!(
                "Unable to connect to {host_s}:{port} ({})",
                e.message
            ))?;
            set_ref(a, 2, Value::Int(i64::from(e.errno)));
            set_ref(a, 3, bytes(e.message.as_bytes()));
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
    let addr = str_arg(a, 0);
    let timeout = timeout_arg(a, 3, vm);
    let ctx = res::context_arg(vm, a, 5, false)?;
    set_ref(a, 1, Value::Int(0));
    set_ref(a, 2, bytes(""));
    let r = bridge::run(vm, || transports::socket_client(&addr, timeout))?;
    match r {
        Ok(mut s) => {
            s.context = ctx;
            Ok(res::new_stream(s, None))
        }
        Err(e) => {
            let shown = String::from_utf8_lossy(&addr).into_owned();
            vm.php_warning(&format!("Unable to connect to {shown} ({})", e.message))?;
            set_ref(a, 1, Value::Int(i64::from(e.errno)));
            set_ref(a, 2, bytes(e.message.as_bytes()));
            Ok(Value::Bool(false))
        }
    }
}

/// Splits `proto://rest` (default `tcp`).
fn split_url(addr: &str) -> (&str, &str) {
    match addr.find("://") {
        Some(i) => (&addr[..i], &addr[i + 3..]),
        None => ("tcp", addr),
    }
}

fn stream_socket_server(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let addr = String::from_utf8_lossy(&str_arg(a, 0)).into_owned();
    let flags = if a.has(3) { a.int(3) } else { 12 };
    set_ref(a, 1, Value::Int(0));
    set_ref(a, 2, bytes(""));
    let (proto, rest) = split_url(&addr);
    let r: std::io::Result<(OwnedFd, &'static str)> = match proto {
        "tcp" => std::net::TcpListener::bind(rest).map(|l| (OwnedFd::from(l), "tcp_socket")),
        "udp" => std::net::UdpSocket::bind(rest).map(|l| (OwnedFd::from(l), "udp_socket")),
        "unix" => {
            std::os::unix::net::UnixListener::bind(rest).map(|l| (OwnedFd::from(l), "unix_socket"))
        }
        _ => Err(std::io::Error::other(format!(
            "Unable to find the socket transport \"{proto}\" - did you forget to enable it when you configured PHP?"
        ))),
    };
    let _ = flags;
    match r {
        Ok((fd, label)) => Ok(res::new_stream(sock_stream(fd, label, None), None)),
        Err(e) => {
            let msg = e
                .raw_os_error()
                .map_or_else(|| e.to_string(), php_streams::sys::strerror);
            vm.php_warning(&format!("Unable to connect to {addr} ({msg})"))?;
            set_ref(a, 1, Value::Int(i64::from(e.raw_os_error().unwrap_or(0))));
            set_ref(a, 2, bytes(msg.as_bytes()));
            Ok(Value::Bool(false))
        }
    }
}

fn sockaddr_name(fd: RawFd, remote: bool) -> Option<String> {
    // SAFETY: zeroed sockaddr_storage is a valid out-parameter.
    let mut ss: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut len = libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_storage>()).ok()?;
    let p = std::ptr::addr_of_mut!(ss).cast::<libc::sockaddr>();
    // SAFETY: `p`/`len` describe `ss`, which outlives the call.
    let rc = unsafe {
        if remote {
            libc::getpeername(fd, p, &mut len)
        } else {
            libc::getsockname(fd, p, &mut len)
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
    let timeout = timeout_arg(a, 1, vm);
    let fd = cell.get().and_then(|mut s| s.cast(CastAs::FdForSelect, false));
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
        vm.php_warning(&format!(
            "Accept failed: {}",
            php_streams::sys::strerror(e)
        ))?;
        return Ok(Value::Bool(false));
    }
    // SAFETY: `nfd` is a fresh descriptor owned by nobody else.
    let owned = unsafe { OwnedFd::from_raw_fd(nfd) };
    let label = cell.get().map_or("tcp_socket", |s| s.ops_label());
    if a.has(2) {
        let peer = sockaddr_name(nfd, true).map_or(Value::Null, |p| bytes(p.as_bytes()));
        a.set_ref(2, peer);
    }
    Ok(res::new_stream(sock_stream(owned, label, Some(default_timeout(vm))), None))
}

fn stream_socket_get_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cell = stream_arg(vm, a, 0)?;
    let remote = a.bool(1);
    let fd = cell.get().and_then(|mut s| s.cast(CastAs::FdForSelect, false));
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
    let fd = cell.get().and_then(|mut s| s.cast(CastAs::FdForSelect, false));
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
    let fd = cell.get().and_then(|mut s| s.cast(CastAs::FdForSelect, false));
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
    let fd = cell.get().and_then(|mut s| s.cast(CastAs::FdForSelect, false));
    let Some(fd) = fd else {
        return Ok(Value::Bool(false));
    };
    // SAFETY: `data` is valid for `data.len()` bytes.
    let n = unsafe { libc::send(fd, data.as_ptr().cast(), data.len(), flags | libc::MSG_NOSIGNAL) };
    Ok(if n < 0 { Value::Bool(false) } else { Value::Int(i64::try_from(n).unwrap_or(0)) })
}

/// Registers the functions and `STREAM_*` socket constants.
pub fn register(m: &mut ModuleBuilder<'_>) {
    m.function(
        "fsockopen(string $hostname, int $port = -1, &$error_code = null, &$error_message = null, ?float $timeout = null)",
        fsockopen,
    )
    .function(
        "pfsockopen(string $hostname, int $port = -1, &$error_code = null, &$error_message = null, ?float $timeout = null)",
        pfsockopen,
    )
    .function(
        "stream_socket_client(string $address, &$error_code = null, &$error_message = null, ?float $timeout = null, int $flags = STREAM_CLIENT_CONNECT, $context = null)",
        stream_socket_client,
    )
    .function(
        "stream_socket_server(string $address, &$error_code = null, &$error_message = null, int $flags = STREAM_SERVER_BIND | STREAM_SERVER_LISTEN, $context = null)",
        stream_socket_server,
    )
    .function(
        "stream_socket_accept($socket, ?float $timeout = null, &$peer_name = null)",
        stream_socket_accept,
    )
    .function("stream_socket_get_name($socket, bool $remote): string|false", stream_socket_get_name)
    .function("stream_socket_shutdown($stream, int $mode): bool", stream_socket_shutdown)
    .function(
        "stream_socket_pair(int $domain, int $type, int $protocol, $context = null): array|false",
        stream_socket_pair,
    )
    .function(
        "stream_socket_recvfrom($socket, int $length, int $flags = 0, &$address = null): string|false",
        stream_socket_recvfrom,
    )
    .function(
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

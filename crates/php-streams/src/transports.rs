// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Socket transports (`main/streams/transports.c`, `xp_socket.c`).
//!
//! A minimal client side: `stream_socket_client()` for `tcp://`, `udp://`
//! and `unix://` using `std::net` / `std::os::unix::net`, with TCP's
//! asynchronous connect (`STREAM_CLIENT_ASYNC_CONNECT`). Servers and crypto
//! (`ssl://`, `tls://`) live in the extension crates.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::time::Duration;

use crate::stream::{
    CastAs, OpFailed, Stream, StreamOps, StreamOption, StreamState, OPT_ERR, OPT_NOTIMPL, OPT_OK,
};
use crate::sys;
use crate::value::Value;

/// `stream_get_transports()` for the transports implemented here.
pub fn transports() -> Vec<&'static str> {
    vec!["tcp", "udp", "unix", "udg"]
}

#[derive(Debug)]
enum Sock {
    Tcp(TcpStream),
    Udp(UdpSocket),
    Unix(UnixStream),
    Udg(UnixDatagram),
}

impl Sock {
    fn fd(&self) -> RawFd {
        match self {
            Sock::Tcp(s) => s.as_raw_fd(),
            Sock::Udp(s) => s.as_raw_fd(),
            Sock::Unix(s) => s.as_raw_fd(),
            Sock::Udg(s) => s.as_raw_fd(),
        }
    }

    fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        match self {
            Sock::Tcp(s) => s.set_read_timeout(t),
            Sock::Udp(s) => s.set_read_timeout(t),
            Sock::Unix(s) => s.set_read_timeout(t),
            Sock::Udg(s) => s.set_read_timeout(t),
        }
    }

    fn set_nonblocking(&self, nb: bool) -> io::Result<()> {
        match self {
            Sock::Tcp(s) => s.set_nonblocking(nb),
            Sock::Udp(s) => s.set_nonblocking(nb),
            Sock::Unix(s) => s.set_nonblocking(nb),
            Sock::Udg(s) => s.set_nonblocking(nb),
        }
    }

    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Sock::Tcp(s) => s.read(buf),
            Sock::Udp(s) => s.recv(buf),
            Sock::Unix(s) => s.read(buf),
            Sock::Udg(s) => s.recv(buf),
        }
    }

    fn send(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Sock::Tcp(s) => s.write(buf),
            Sock::Udp(s) => s.send(buf),
            Sock::Unix(s) => s.write(buf),
            Sock::Udg(s) => s.send(buf),
        }
    }
}

/// Socket stream ops (`php_stream_socket_ops` and friends).
#[derive(Debug)]
pub struct SocketOps {
    sock: Option<Sock>,
    label: &'static str,
    timeout: Option<Duration>,
    timed_out: bool,
    is_blocked: bool,
}

impl StreamOps for SocketOps {
    fn label(&self) -> &'static str {
        self.label
    }

    fn read(&mut self, st: &mut StreamState, buf: &mut [u8]) -> Result<usize, OpFailed> {
        let Some(sock) = self.sock.as_mut() else {
            return Err(OpFailed);
        };
        self.timed_out = false;
        // With data already returned by this read, a blocking socket does
        // not wait for more (`php_sockop_read` checks has_buffered_data).
        let dont_wait = self.is_blocked && st.has_buffered_data;
        if dont_wait {
            let _ = sock.set_nonblocking(true);
        } else if self.is_blocked {
            let _ = sock.set_read_timeout(self.timeout);
        }
        let r = sock.recv(buf);
        if dont_wait {
            let _ = sock.set_nonblocking(false);
            if matches!(&r, Err(e) if e.kind() == io::ErrorKind::WouldBlock) {
                return Ok(0);
            }
        }
        match r {
            Ok(0) => {
                st.eof = true;
                Ok(0)
            }
            Ok(n) => Ok(n),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                if self.is_blocked {
                    self.timed_out = true;
                    Err(OpFailed)
                } else {
                    Ok(0)
                }
            }
            Err(_) => {
                st.eof = true;
                Err(OpFailed)
            }
        }
    }

    fn write(&mut self, _st: &mut StreamState, buf: &[u8]) -> Result<usize, OpFailed> {
        let sock = self.sock.as_mut().ok_or(OpFailed)?;
        match sock.send(buf) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(0),
            Err(_) => Err(OpFailed),
        }
    }

    fn close(&mut self, _st: &mut StreamState, _preserve: bool) -> i32 {
        self.sock = None;
        0
    }

    fn set_option(&mut self, st: &mut StreamState, opt: &mut StreamOption<'_>) -> i64 {
        let Some(sock) = self.sock.as_ref() else {
            return OPT_ERR;
        };
        match opt {
            StreamOption::Blocking(b) => {
                let old = i64::from(self.is_blocked);
                if sock.set_nonblocking(!*b).is_err() {
                    return OPT_ERR;
                }
                self.is_blocked = *b;
                old
            }
            StreamOption::ReadTimeout(sec, usec) => {
                let micros = sec.saturating_mul(1_000_000).saturating_add(*usec);
                self.timeout = u64::try_from(micros)
                    .ok()
                    .filter(|m| *m > 0)
                    .map(Duration::from_micros);
                self.timed_out = false;
                OPT_OK
            }
            StreamOption::MetaData(out) => {
                out.push(("timed_out".into(), Value::Bool(self.timed_out)));
                out.push(("blocked".into(), Value::Bool(self.is_blocked)));
                out.push(("eof".into(), Value::Bool(st.eof)));
                OPT_OK
            }
            _ => OPT_NOTIMPL,
        }
    }

    fn cast(&mut self, _st: &mut StreamState, _kind: CastAs, _report: bool) -> Option<RawFd> {
        self.sock.as_ref().map(Sock::fd)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// A connection failure: `(errno, errstr)` as returned through
/// `stream_socket_client()`'s by-reference arguments. The VM emits
/// `Unable to connect to {remote} ({errstr})`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectError {
    /// `$error_code`.
    pub errno: i32,
    /// `$error_message`.
    pub message: String,
}

fn io_err(e: &io::Error) -> ConnectError {
    let errno = e.raw_os_error().unwrap_or(0);
    ConnectError {
        errno,
        message: if errno != 0 {
            sys::strerror(errno)
        } else {
            e.to_string()
        },
    }
}

/// `sizeof(((struct sockaddr_un *)0)->sun_path)`.
const SUN_PATH_LEN: usize = 108;

/// The address of a `unix://` socket name (`php_sockop_bind`/`connect`):
/// a leading NUL selects Linux's abstract namespace; names that do not fit
/// `sun_path` are truncated with PHP's notice.
pub fn unix_socket_addr(name: &[u8]) -> io::Result<std::os::unix::net::SocketAddr> {
    use std::os::linux::net::SocketAddrExt;
    let abstract_ns = name.first() == Some(&0);
    let max = if abstract_ns {
        SUN_PATH_LEN
    } else {
        SUN_PATH_LEN - 1
    };
    let name = if name.len() > max {
        crate::diag::emit(
            crate::diag::Diagnostic::new(
                crate::diag::Severity::Notice,
                format!(
                    "socket path exceeded the maximum allowed length of {max} bytes and was truncated"
                ),
            )
            .with_code(crate::diag::StreamErrorCode::InvalidPath)
            .terminating(false),
        );
        &name[..max]
    } else {
        name
    };
    if abstract_ns {
        std::os::unix::net::SocketAddr::from_abstract_name(&name[1..])
    } else {
        std::os::unix::net::SocketAddr::from_pathname(crate::util::os_path(name))
    }
}

/// `parse_port()`: `strtol()` semantics, rejecting values outside 0..=65535.
fn parse_port(p: &str) -> Option<u16> {
    let (v, _) = crate::util::strtol(p.as_bytes());
    u16::try_from(v).ok()
}

/// `parse_ip_address_ex()`.
fn parse_host_port(addr: &str) -> Result<(String, u16), ConnectError> {
    let fail = |m: String| ConnectError {
        errno: 0,
        message: m,
    };
    if addr.contains('\0') {
        return Err(fail("The hostname must not contain null bytes".into()));
    }
    let b = addr.as_bytes();
    if b.first() == Some(&b'[') && b.len() > 1 {
        // memchr(str + 1, ']', len - 2): the last byte is not searched.
        let end = b[1..b.len() - 1]
            .iter()
            .position(|&c| c == b']')
            .map(|e| e + 1);
        let Some(end) = end.filter(|&e| b.get(e + 1) == Some(&b':')) else {
            return Err(fail(format!("Failed to parse IPv6 address \"{addr}\"")));
        };
        let port = parse_port(&addr[end + 2..])
            .ok_or_else(|| fail(format!("Failed to parse port of address \"{addr}\"")))?;
        return Ok((addr[1..end].to_string(), port));
    }
    let colon = b[..b.len().saturating_sub(1)]
        .iter()
        .position(|&c| c == b':');
    let Some(c) = colon else {
        return Err(fail(format!("Failed to parse address \"{addr}\"")));
    };
    let port = parse_port(&addr[c + 1..])
        .ok_or_else(|| fail(format!("Failed to parse port of address \"{addr}\"")))?;
    Ok((addr[..c].to_string(), port))
}

/// `stream_socket_client($remote, ..., $timeout)`: connects and returns a
/// socket stream. `timeout` defaults to `default_socket_timeout`.
pub fn socket_client(remote: &[u8], timeout: Option<Duration>) -> Result<Stream, ConnectError> {
    socket_client_ex(remote, timeout, false)
}

/// Starts a TCP connection to the first of `addrs` that accepts one
/// without waiting (`php_network_connect_socket_to_host()` with
/// `asynchronous`): the stream is returned while the connection is still
/// pending (`EINPROGRESS`), and the caller learns the outcome by waiting
/// for the socket to become writable, as with C's
/// `STREAM_CLIENT_ASYNC_CONNECT`. Each address whose `connect()` fails at
/// once is skipped, like C's loop over `php_network_getaddresses()`.
fn connect_tcp_async(addrs: Vec<std::net::SocketAddr>) -> io::Result<TcpStream> {
    let mut last = io::Error::from_raw_os_error(libc::ECONNREFUSED);
    for a in addrs {
        match sys::connect_ip_socket_async(&a, libc::SOCK_STREAM) {
            Ok((fd, _pending)) => {
                let s = TcpStream::from(fd);
                // C leaves the descriptor non-blocking but the stream
                // blocking (`is_blocked`), and its reads and writes wait
                // with poll() for a blocking stream. Our socket ops rely on
                // the descriptor's mode instead, so it goes back to
                // blocking: the connection carries on in the background
                // and a blocking send() waits for it, as C's poll() would.
                s.set_nonblocking(false)?;
                return Ok(s);
            }
            Err(e) => last = io::Error::from_raw_os_error(e),
        }
    }
    Err(last)
}

/// [`socket_client`]; `asynchronous` is `STREAM_CLIENT_ASYNC_CONNECT`
/// (`STREAM_XPORT_OP_CONNECT_ASYNC`, `main/streams/xp_socket.c`): a TCP
/// connection is started but not waited for, and `timeout` does not apply
/// to it.
pub fn socket_client_ex(
    remote: &[u8],
    timeout: Option<Duration>,
    asynchronous: bool,
) -> Result<Stream, ConnectError> {
    let remote_s = String::from_utf8_lossy(remote).into_owned();
    let n = crate::wrapper::scheme_len(remote);
    let (proto, name) = if remote.get(n..n + 3) == Some(b"://") && n > 1 {
        (&remote_s[..n], &remote_s[n + 3..])
    } else {
        ("tcp", remote_s.as_str())
    };
    let default = Duration::from_secs(
        u64::try_from(crate::state::settings().default_socket_timeout).unwrap_or(60),
    );
    let timeout = timeout.unwrap_or(default);
    let (sock, label) = match proto {
        "tcp" | "udp" => {
            let (host, port) = parse_host_port(name)?;
            let addrs: Vec<_> = (host.as_str(), port)
                .to_socket_addrs()
                .map_err(|e| {
                    // `gai_strerror()` text, without std's prefix.
                    let e = e.to_string();
                    let reason = e
                        .strip_prefix("failed to lookup address information: ")
                        .unwrap_or(&e);
                    let message = format!(
                        "php_network_getaddresses: getaddrinfo for {host} failed: {reason}"
                    );
                    // php_network_getaddresses() also warns directly.
                    crate::diag::warning(message.clone());
                    ConnectError { errno: 0, message }
                })?
                .collect();
            if proto == "tcp" && asynchronous {
                (
                    Sock::Tcp(connect_tcp_async(addrs).map_err(|e| io_err(&e))?),
                    "tcp_socket",
                )
            } else if proto == "tcp" {
                let mut last = io::Error::from_raw_os_error(libc::ECONNREFUSED);
                let mut found = None;
                for a in addrs {
                    let r = if timeout.is_zero() {
                        TcpStream::connect(a)
                    } else {
                        TcpStream::connect_timeout(&a, timeout)
                    };
                    match r {
                        Ok(s) => {
                            found = Some(s);
                            break;
                        }
                        Err(e) => last = e,
                    }
                }
                (Sock::Tcp(found.ok_or_else(|| io_err(&last))?), "tcp_socket")
            } else {
                let a = addrs
                    .first()
                    .ok_or_else(|| io_err(&io::Error::from_raw_os_error(libc::EINVAL)))?;
                let bind = if a.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
                let s = UdpSocket::bind(bind).map_err(|e| io_err(&e))?;
                s.connect(a).map_err(|e| io_err(&e))?;
                (Sock::Udp(s), "udp_socket")
            }
        }
        "unix" => {
            let raw = if remote.get(n..n + 3) == Some(b"://") {
                &remote[n + 3..]
            } else {
                remote
            };
            let addr = unix_socket_addr(raw).map_err(|e| io_err(&e))?;
            (
                Sock::Unix(UnixStream::connect_addr(&addr).map_err(|e| io_err(&e))?),
                "unix_socket",
            )
        }
        "udg" => {
            let addr = unix_socket_addr(&remote[n + 3..]).map_err(|e| io_err(&e))?;
            let s = UnixDatagram::unbound().map_err(|e| io_err(&e))?;
            s.connect_addr(&addr).map_err(|e| io_err(&e))?;
            (Sock::Udg(s), "udg_socket")
        }
        other => {
            let shown: String = other.chars().take(31).collect();
            return Err(ConnectError {
                errno: 0,
                message: format!(
                    "Unable to find the socket transport \"{shown}\" - did you forget to enable it when you configured PHP?"
                ),
            });
        }
    };
    let ops = SocketOps {
        sock: Some(sock),
        label,
        timeout: Some(timeout).filter(|t| !t.is_zero()),
        timed_out: false,
        is_blocked: true,
    };
    let mut s = Stream::new(Box::new(ops), "r+");
    s.add_flags(crate::stream::flags::NO_SEEK);
    s.position = 0;
    s.orig_path = Some(remote.to_vec());
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn tcp_roundtrip_and_short_reads() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut b = [0u8; 4];
            c.read_exact(&mut b).unwrap();
            c.write_all(b"pong").unwrap();
        });
        let mut s = socket_client(format!("tcp://127.0.0.1:{port}").as_bytes(), None).unwrap();
        assert_eq!(s.ops_label(), "tcp_socket");
        s.write(b"ping").unwrap();
        // socket reads are not greedy: fread returns what is available
        let got = s.read_vec(100).unwrap();
        assert!(!got.is_empty() && b"pong".starts_with(&got));
        t.join().unwrap();
        let meta = s.meta_data();
        assert_eq!(meta[0], ("timed_out".to_string(), Value::Bool(false)));
        assert!(!s.is_seekable());
    }

    #[test]
    fn connect_errors() {
        let e = socket_client(b"tcp://127.0.0.1:1", Some(Duration::from_secs(1))).unwrap_err();
        assert_eq!(e.errno, libc::ECONNREFUSED);
        assert_eq!(e.message, "Connection refused");
        let e = socket_client(b"foo://x", None).unwrap_err();
        assert_eq!(
            e.message,
            "Unable to find the socket transport \"foo\" - did you forget to enable it when you configured PHP?"
        );
        let e = socket_client(b"unix:///nonexist-php-streams", None).unwrap_err();
        assert_eq!(
            (e.errno, e.message.as_str()),
            (2, "No such file or directory")
        );
        let e = socket_client(b"tcp://localhost", None).unwrap_err();
        assert_eq!(e.message, "Failed to parse address \"localhost\"");
    }

    /// `STREAM_CLIENT_ASYNC_CONNECT` alone still connects (C's
    /// `php_stream_xport_create()` connects for `STREAM_XPORT_CONNECT` or
    /// `STREAM_XPORT_CONNECT_ASYNC`); the socket becomes writable once the
    /// connection is up, and then carries data both ways.
    #[test]
    fn async_connect_connects() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let mut s = socket_client_ex(
            format!("tcp://127.0.0.1:{port}").as_bytes(),
            Some(Duration::from_secs(5)),
            true,
        )
        .unwrap();
        assert_eq!(s.ops_label(), "tcp_socket");
        let fd = s.cast(CastAs::FdForSelect, false).unwrap();
        let ready = sys::poll_fds(&[(fd, libc::POLLOUT)], 5000).unwrap();
        assert_ne!(ready[0] & libc::POLLOUT, 0);
        let (mut c, _) = l.accept().unwrap();
        assert_eq!(s.write(b"ping").unwrap(), 4);
        let mut b = [0u8; 4];
        c.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"ping");
        c.write_all(b"pong").unwrap();
        let got = s.read_vec(100).unwrap();
        assert!(!got.is_empty() && b"pong".starts_with(&got));
        // The stream is a blocking one, as in C (`"blocked" => true`).
        let meta = s.meta_data();
        assert_eq!(meta[1], ("blocked".to_string(), Value::Bool(true)));
    }

    /// A refused asynchronous connect is reported by the socket (readable
    /// and writable, with the error pending), not by the connect.
    #[test]
    fn async_connect_refused_later() {
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        match socket_client_ex(format!("tcp://127.0.0.1:{port}").as_bytes(), None, true) {
            Ok(mut s) => {
                let fd = s.cast(CastAs::FdForSelect, false).unwrap();
                let ready = sys::poll_fds(&[(fd, libc::POLLOUT)], 5000).unwrap();
                assert_ne!(
                    ready[0] & (libc::POLLOUT | libc::POLLERR | libc::POLLHUP),
                    0
                );
            }
            // Loopback may refuse at once, as C's connect() would see.
            Err(e) => assert_eq!(e.errno, libc::ECONNREFUSED),
        }
    }

    #[test]
    fn read_timeout() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let mut s = socket_client(format!("127.0.0.1:{port}").as_bytes(), None).unwrap();
        let _c = l.accept().unwrap();
        assert!(crate::file::stream_set_timeout(&mut s, 0, 50_000));
        assert!(s.read_vec(10).is_none());
        let meta = s.meta_data();
        assert_eq!(meta[0].1, Value::Bool(true));
    }
}

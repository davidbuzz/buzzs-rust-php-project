// Derived from php-src main/fastcgi.c. Similarity metric to original: 16%
//   Copyright © The PHP Group and Contributors. Authors: Dmitry Stogov <dmitry@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! `FastCGI` for `php-cgi` (ref: `main/fastcgi.c`): the listening socket
//! (`-b <address:port>|<port>|<path>`, or one a web server passes as
//! standard input), the record protocol (`BEGIN_REQUEST`, `PARAMS`,
//! `STDIN` in; `STDOUT`, `STDERR`, `END_REQUEST` out), `GET_VALUES`
//! management records, `FCGI_KEEP_CONN` connections and
//! `FCGI_WEB_SERVER_ADDRS`.
//!
//! A connection is one [`Connection`]; while a request runs, the request
//! context holds it as its [`FastCgiStreams`].

use crate::sys;
use php_cli_server::sapi::FastCgiStreams;
use std::cell::{Cell, RefCell};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};

/// `FCGI_VERSION_1`.
pub const VERSION_1: u8 = 1;
/// `FCGI_BEGIN_REQUEST`.
pub const BEGIN_REQUEST: u8 = 1;
/// `FCGI_END_REQUEST`.
pub const END_REQUEST: u8 = 3;
/// `FCGI_PARAMS`.
pub const PARAMS: u8 = 4;
/// `FCGI_STDIN`.
pub const STDIN: u8 = 5;
/// `FCGI_STDOUT`.
pub const STDOUT: u8 = 6;
/// `FCGI_STDERR`.
pub const STDERR: u8 = 7;
/// `FCGI_GET_VALUES`.
pub const GET_VALUES: u8 = 9;
/// `FCGI_GET_VALUES_RESULT`.
pub const GET_VALUES_RESULT: u8 = 10;
/// `FCGI_RESPONDER`.
pub const RESPONDER: u16 = 1;
/// `FCGI_AUTHORIZER`.
pub const AUTHORIZER: u16 = 2;
/// `FCGI_FILTER`.
pub const FILTER: u16 = 3;
/// `FCGI_KEEP_CONN`.
pub const KEEP_CONN: u8 = 1;
/// `FCGI_MAX_LENGTH`.
pub const MAX_LENGTH: usize = 0xffff;
/// `sizeof(req->out_buf)`.
const OUT_BUF: usize = 1024 * 8;
/// `sizeof(fcgi_header)`.
const HEADER_LEN: usize = 8;

/// `FCGI_ERROR` (the `ZLOG_ERROR` value).
pub const LOG_ERROR: i32 = 4;

/// `fcgi_set_logger()`: FPM sends the library's messages to its log.
static LOGGER: std::sync::OnceLock<fn(i32, &str)> = std::sync::OnceLock::new();

/// `fcgi_set_logger()` (php-fpm's).
#[allow(dead_code)]
pub fn set_logger(f: fn(i32, &str)) {
    let _ = LOGGER.set(f);
}

/// `fcgi_log(FCGI_ERROR, ...)`: by default to standard error as given
/// (some without a newline, as in php-src).
fn log(msg: &str) {
    match LOGGER.get() {
        Some(f) => f(LOG_ERROR, msg),
        None => {
            let _ = std::io::stderr().write_all(msg.as_bytes());
        }
    }
}

/// `fcgi_header`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// `version`.
    pub version: u8,
    /// `type`.
    pub kind: u8,
    /// `requestId`.
    pub id: u16,
    /// `contentLength`.
    pub len: usize,
    /// `paddingLength`.
    pub padding: usize,
}

impl Header {
    /// A header from its 8 bytes.
    pub fn parse(b: [u8; HEADER_LEN]) -> Header {
        Header {
            version: b[0],
            kind: b[1],
            id: u16::from_be_bytes([b[2], b[3]]),
            len: usize::from(u16::from_be_bytes([b[4], b[5]])),
            padding: usize::from(b[6]),
        }
    }
}

/// `fcgi_make_header()`: the header of a `len`-byte record, padded to a
/// multiple of 8 bytes, and the padding length.
pub fn make_header(kind: u8, id: u16, len: usize) -> ([u8; HEADER_LEN], usize) {
    let pad = ((len + 7) & !7) - len;
    let l = u16::try_from(len).unwrap_or(u16::MAX).to_be_bytes();
    let i = id.to_be_bytes();
    let p = u8::try_from(pad).unwrap_or(0);
    ([VERSION_1, kind, i[0], i[1], l[0], l[1], p, 0], pad)
}

/// A whole record: header, `content` (at most [`MAX_LENGTH`] bytes) and
/// padding.
pub fn record(kind: u8, id: u16, content: &[u8]) -> Vec<u8> {
    let (h, pad) = make_header(kind, id, content.len());
    let mut out = Vec::with_capacity(HEADER_LEN + content.len() + pad);
    out.extend_from_slice(&h);
    out.extend_from_slice(content);
    out.resize(out.len() + pad, 0);
    out
}

/// One name-value pair in `PARAMS` encoding (1-byte lengths below 128,
/// else 4 bytes with the high bit set).
pub fn encode_param(out: &mut Vec<u8>, name: &[u8], value: &[u8]) {
    for len in [name.len(), value.len()] {
        if len < 0x80 {
            out.push(u8::try_from(len).unwrap_or(0));
        } else {
            let l = u32::try_from(len).unwrap_or(u32::MAX) | 0x8000_0000;
            out.extend_from_slice(&l.to_be_bytes());
        }
    }
    out.extend_from_slice(name);
    out.extend_from_slice(value);
}

/// The environment of a request (`fcgi_hash`), in the order the names
/// were first set.
pub type Env = Vec<(Vec<u8>, Vec<u8>)>;

/// `fcgi_hash_set()`: a new name is appended, an existing one changes in
/// place.
pub fn hash_set(env: &mut Env, name: &[u8], value: &[u8]) {
    match env.iter_mut().find(|(k, _)| k == name) {
        Some(e) => value.clone_into(&mut e.1),
        None => env.push((name.to_vec(), value.to_vec())),
    }
}

/// A `PARAMS` length at `body[*p..]`; `None` when truncated (with
/// php-src's bound for the 4-byte form).
fn param_len(body: &[u8], p: &mut usize) -> Option<usize> {
    let first = *body.get(*p)?;
    *p += 1;
    if first < 128 {
        return Some(usize::from(first));
    }
    if *p + 3 >= body.len() {
        return None;
    }
    let len = (usize::from(first & 0x7f) << 24)
        | (usize::from(body[*p]) << 16)
        | (usize::from(body[*p + 1]) << 8)
        | usize::from(body[*p + 2]);
    *p += 3;
    Some(len)
}

/// `fcgi_get_params()`: the name-value pairs of `body` into `env`;
/// `false` when the body is malformed.
pub fn get_params(env: &mut Env, body: &[u8]) -> bool {
    let mut p = 0;
    while p < body.len() {
        let Some(name_len) = param_len(body, &mut p) else {
            return false;
        };
        if p >= body.len() {
            return false;
        }
        let Some(val_len) = param_len(body, &mut p) else {
            return false;
        };
        if name_len + val_len > body.len() - p {
            return false;
        }
        let name = &body[p..p + name_len];
        let value = &body[p + name_len..p + name_len + val_len];
        hash_set(env, name, value);
        p += name_len + val_len;
    }
    true
}

/// A listening socket.
#[derive(Debug)]
pub enum Listener {
    /// TCP (`-b [host:]port`).
    Tcp(TcpListener),
    /// A Unix socket (`-b <path>`).
    Unix(UnixListener),
}

impl Listener {
    fn raw_fd(&self) -> RawFd {
        match self {
            Listener::Tcp(l) => l.as_raw_fd(),
            Listener::Unix(l) => l.as_raw_fd(),
        }
    }

    fn set_nonblocking(&self) -> std::io::Result<()> {
        match self {
            Listener::Tcp(l) => l.set_nonblocking(true),
            Listener::Unix(l) => l.set_nonblocking(true),
        }
    }

    fn accept(&self) -> std::io::Result<(Stream, Option<SocketAddr>)> {
        match self {
            Listener::Tcp(l) => l.accept().map(|(s, a)| (Stream::Tcp(s), Some(a))),
            Listener::Unix(l) => l.accept().map(|(s, _)| (Stream::Unix(s), None)),
        }
    }
}

/// `is_port_number()`: only digits (an empty string counts).
fn is_port_number(s: &str) -> bool {
    s.bytes().all(|c| c.is_ascii_digit())
}

/// `fcgi_listen()`: binds `path` (`[host:]port`, `*:port` or a Unix
/// socket path) and starts listening, logging the reason when it fails.
/// The second value is `FCGI_WEB_SERVER_ADDRS` (TCP only): the clients
/// allowed to connect, `None` for any.
pub fn listen(path: &str) -> Option<(Listener, Option<Vec<IpAddr>>)> {
    // `short port = atoi(...)`: the value as a 16-bit port.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let port_of = |s: &str| php_support::atoi(s.as_bytes()) as u16;
    let mut tcp: Option<(String, u16)> = None;
    if let Some(colon) = path.find(':') {
        let port = port_of(&path[colon + 1..]);
        if port != 0 {
            tcp = Some((path[..colon].to_string(), port));
        }
    } else if is_port_number(path) {
        let port = port_of(path);
        if port != 0 {
            tcp = Some((String::new(), port));
        }
    }

    let bind_failed = |e: &std::io::Error| {
        log(&format!(
            "Cannot bind/listen socket - [{}] {}.\n",
            e.raw_os_error().unwrap_or(0),
            sys::strerror(e)
        ));
    };
    let Some((host, port)) = tcp else {
        if path.len() >= 108 {
            log("Listening socket's path name is too long.\n");
            return None;
        }
        let _ = std::fs::remove_file(path);
        let l = match UnixListener::bind(path) {
            Ok(l) => l,
            Err(e) => {
                bind_failed(&e);
                return None;
            }
        };
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777));
        return Some((Listener::Unix(l), None));
    };

    let ip = if host.is_empty() || host.starts_with('*') {
        Ipv4Addr::UNSPECIFIED
    } else if let Ok(ip) = host.parse::<Ipv4Addr>() {
        ip
    } else {
        // `php_network_gethostbyname()`: exactly one IPv4 address.
        let mut addrs: Vec<Ipv4Addr> = (host.as_str(), 0)
            .to_socket_addrs()
            .map(|it| {
                it.filter_map(|a| match a.ip() {
                    IpAddr::V4(v4) => Some(v4),
                    IpAddr::V6(_) => None,
                })
                .collect()
            })
            .unwrap_or_default();
        addrs.dedup();
        match addrs.as_slice() {
            [] => {
                log(&format!("Cannot resolve host name '{host}'!\n"));
                return None;
            }
            [one] => *one,
            _ => {
                log(&format!(
                    "Host '{host}' has multiple addresses. You must choose one explicitly!\n"
                ));
                return None;
            }
        }
    };
    let l = match TcpListener::bind(SocketAddr::from((ip, port))) {
        Ok(l) => l,
        Err(e) => {
            bind_failed(&e);
            return None;
        }
    };
    let allowed = std::env::var("FCGI_WEB_SERVER_ADDRS")
        .ok()
        .map(|list| allowed_clients(&list));
    Some((Listener::Tcp(l), allowed))
}

/// The `FCGI_WEB_SERVER_ADDRS` list (comma-separated IPv4/IPv6
/// addresses), logging the entries that are not addresses.
pub fn allowed_clients(list: &str) -> Vec<IpAddr> {
    let mut out = Vec::new();
    for cur in list.split(',') {
        match cur.parse::<IpAddr>() {
            Ok(ip) => out.push(ip),
            Err(_) => log(&format!(
                "Wrong IP address '{cur}' in listen.allowed_clients"
            )),
        }
    }
    if out.is_empty() {
        log("There are no allowed addresses");
    }
    out
}

/// `fcgi_is_allowed()`: Unix sockets always are; an IPv4-mapped IPv6
/// client matches its IPv4 address.
fn is_allowed(allowed: Option<&[IpAddr]>, client: Option<SocketAddr>) -> bool {
    let (Some(allowed), Some(client)) = (allowed, client) else {
        return true;
    };
    let ip = client.ip();
    allowed.iter().any(|a| match (a, ip) {
        (IpAddr::V4(a), IpAddr::V4(c)) => *a == c,
        (IpAddr::V6(a), IpAddr::V6(c)) => *a == c,
        (IpAddr::V4(a), IpAddr::V6(c)) => c.to_ipv4_mapped() == Some(*a),
        (IpAddr::V6(_), IpAddr::V4(_)) => false,
    })
}

/// `fcgi_get_last_client_ip()`.
fn client_ip(client: Option<SocketAddr>) -> String {
    match client.map(|c| c.ip()) {
        Some(IpAddr::V6(v6)) => v6
            .to_ipv4_mapped()
            .map_or_else(|| v6.to_string(), |v4| v4.to_string()),
        Some(ip) => ip.to_string(),
        None => String::new(),
    }
}

/// A connected socket.
#[derive(Debug)]
pub enum Stream {
    /// TCP.
    Tcp(TcpStream),
    /// A Unix socket.
    Unix(UnixStream),
}

impl Stream {
    fn raw_fd(&self) -> RawFd {
        match self {
            Stream::Tcp(s) => s.as_raw_fd(),
            Stream::Unix(s) => s.as_raw_fd(),
        }
    }

    fn shutdown_write(&self) {
        let _ = match self {
            Stream::Tcp(s) => s.shutdown(Shutdown::Write),
            Stream::Unix(s) => s.shutdown(Shutdown::Write),
        };
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.read(buf),
            Stream::Unix(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.write(buf),
            Stream::Unix(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The management variables `GET_VALUES` answers (`fcgi_mgmt_vars`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MgmtVars(pub Vec<(String, String)>);

impl MgmtVars {
    /// `FCGI_MPXS_CONNS=0` (`fcgi_init()`) plus `FCGI_MAX_CONNS` and
    /// `FCGI_MAX_REQS`: the `PHP_FCGI_CHILDREN` value as given, else `1`.
    pub fn new(children: Option<&str>) -> Self {
        let n = children.unwrap_or("1").to_string();
        MgmtVars(vec![
            ("FCGI_MPXS_CONNS".to_string(), "0".to_string()),
            ("FCGI_MAX_CONNS".to_string(), n.clone()),
            ("FCGI_MAX_REQS".to_string(), n),
        ])
    }

    fn get(&self, name: &[u8]) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k.as_bytes() == name)
            .map(|(_, v)| v.as_str())
    }
}

/// What [`Connection::read_request`] read.
#[derive(Debug, PartialEq, Eq)]
pub enum Incoming {
    /// A request: its environment (`FCGI_ROLE` and the `PARAMS`).
    Request(Env),
    /// A management record was answered; read the next one.
    Management,
    /// The connection is unusable (closed, malformed, unknown role).
    Failed,
}

/// `fcgi_request`: one connection and the request on it.
#[derive(Debug)]
pub struct Connection {
    stream: RefCell<Option<Stream>>,
    tcp: bool,
    nodelay: Cell<bool>,
    id: Cell<u16>,
    keep: Cell<bool>,
    ended: Cell<bool>,
    in_len: Cell<usize>,
    in_pad: Cell<usize>,
    /// `out_buf`: records not sent yet.
    out: RefCell<Vec<u8>>,
    /// Offset and type of the open record in `out` (`out_hdr`).
    out_hdr: Cell<Option<(usize, u8)>>,
}

impl Connection {
    /// A connection on `stream`.
    pub fn new(stream: Stream) -> Self {
        let tcp = matches!(stream, Stream::Tcp(_));
        Connection {
            stream: RefCell::new(Some(stream)),
            tcp,
            nodelay: Cell::new(false),
            id: Cell::new(0),
            keep: Cell::new(false),
            ended: Cell::new(false),
            in_len: Cell::new(0),
            in_pad: Cell::new(0),
            out: RefCell::new(Vec::with_capacity(OUT_BUF + HEADER_LEN * 2)),
            out_hdr: Cell::new(None),
        }
    }

    /// `fcgi_is_closed()`.
    pub fn is_closed(&self) -> bool {
        self.stream.borrow().is_none()
    }

    /// Whether the web server asked to keep the connection (`req->keep`).
    #[cfg(test)]
    fn keep(&self) -> bool {
        self.keep.get()
    }

    /// `safe_read()` of exactly `buf.len()` bytes.
    fn read_exact(&self, buf: &mut [u8]) -> bool {
        match self.stream.borrow_mut().as_mut() {
            Some(s) => s.read_exact(buf).is_ok(),
            None => false,
        }
    }

    /// `safe_write()` of all of `buf`.
    fn write_all(&self, buf: &[u8]) -> bool {
        match self.stream.borrow_mut().as_mut() {
            Some(s) => s.write_all(buf).is_ok(),
            None => false,
        }
    }

    fn read_header(&self) -> Option<Header> {
        let mut b = [0u8; HEADER_LEN];
        if !self.read_exact(&mut b) {
            return None;
        }
        let h = Header::parse(b);
        (h.version >= VERSION_1).then_some(h)
    }

    /// `fcgi_read_request()`. A `STDIN` record the previous request left
    /// partly unread (a body shorter than its record, `CONTENT_LENGTH`
    /// below what was sent) ends the connection: its rest must not be
    /// parsed as another request (GHSA-qc49-hpvq-g2w6, CVE-2026-129289).
    pub fn read_request(&self, mgmt: &MgmtVars) -> Incoming {
        if self.in_len.get() != 0 {
            return Incoming::Failed;
        }
        self.keep.set(false);
        self.ended.set(false);
        self.in_len.set(0);
        self.out.borrow_mut().clear();
        self.out_hdr.set(None);
        let mut env = Env::new();

        let Some(mut hdr) = self.read_header() else {
            return Incoming::Failed;
        };
        // The empty STDIN record that ended the previous request.
        while hdr.kind == STDIN && hdr.len == 0 {
            match self.read_header() {
                Some(h) => hdr = h,
                None => return Incoming::Failed,
            }
        }
        if hdr.len + hdr.padding > MAX_LENGTH {
            return Incoming::Failed;
        }
        self.id.set(hdr.id);
        let mut buf = vec![0u8; MAX_LENGTH + HEADER_LEN];

        if hdr.kind == BEGIN_REQUEST && hdr.len == 8 {
            if !self.read_exact(&mut buf[..hdr.len + hdr.padding]) {
                return Incoming::Failed;
            }
            self.keep.set(buf[2] & KEEP_CONN != 0);
            if self.keep.get() && self.tcp && !self.nodelay.get() {
                if let Some(Stream::Tcp(s)) = self.stream.borrow().as_ref() {
                    let _ = s.set_nodelay(true);
                }
                self.nodelay.set(true);
            }
            let role: &[u8] = match u16::from_be_bytes([buf[0], buf[1]]) {
                RESPONDER => b"RESPONDER",
                AUTHORIZER => b"AUTHORIZER",
                FILTER => b"FILTER",
                _ => return Incoming::Failed,
            };
            hash_set(&mut env, b"FCGI_ROLE", role);
            let Some(mut hdr) = self.read_header() else {
                return Incoming::Failed;
            };
            while hdr.kind == PARAMS && hdr.len > 0 {
                if hdr.len + hdr.padding > MAX_LENGTH
                    || !self.read_exact(&mut buf[..hdr.len + hdr.padding])
                    || !get_params(&mut env, &buf[..hdr.len])
                {
                    self.keep.set(false);
                    return Incoming::Failed;
                }
                let Some(h) = self.read_header() else {
                    self.keep.set(false);
                    return Incoming::Failed;
                };
                hdr = h;
            }
            Incoming::Request(env)
        } else if hdr.kind == GET_VALUES {
            if !self.read_exact(&mut buf[..hdr.len + hdr.padding])
                || !get_params(&mut env, &buf[..hdr.len])
            {
                self.keep.set(false);
                return Incoming::Failed;
            }
            // The asked names that are management variables, newest
            // first (the order `req->env.list` visits them).
            let mut body = Vec::new();
            for (name, _) in env.iter().rev() {
                let Some(value) = mgmt.get(name) else {
                    continue;
                };
                if body.len() + 4 + 4 + name.len() + value.len() >= MAX_LENGTH {
                    break;
                }
                encode_param(&mut body, name, value.as_bytes());
            }
            if !self.write_all(&record(GET_VALUES_RESULT, 0, &body)) {
                self.keep.set(false);
                return Incoming::Failed;
            }
            Incoming::Management
        } else {
            Incoming::Failed
        }
    }

    /// `fcgi_read()`: up to `buf.len()` bytes of the `STDIN` stream; 0 at
    /// its end.
    pub fn read(&self, buf: &mut [u8]) -> usize {
        let mut n = 0;
        while n < buf.len() {
            if self.in_len.get() == 0 {
                let Some(h) = self.read_header().filter(|h| h.kind == STDIN) else {
                    self.keep.set(false);
                    return n;
                };
                self.in_len.set(h.len);
                self.in_pad.set(h.padding);
                if h.len == 0 {
                    return n;
                }
            }
            let want = (buf.len() - n).min(self.in_len.get());
            let got = match self.stream.borrow_mut().as_mut() {
                Some(s) => s.read(&mut buf[n..n + want]).unwrap_or(0),
                None => 0,
            };
            if got == 0 {
                self.keep.set(false);
                return n;
            }
            self.in_len.set(self.in_len.get() - got);
            n += got;
            if self.in_len.get() == 0 {
                let pad = self.in_pad.get();
                if pad > 0 && !self.read_exact(&mut vec![0u8; pad]) {
                    self.keep.set(false);
                    return n;
                }
            } else {
                return n;
            }
        }
        n
    }

    /// `sapi_fcgi_read_post()` until `content_length` bytes or the end of
    /// the `STDIN` stream.
    pub fn read_body(&self, content_length: usize) -> Vec<u8> {
        // Grown as the data arrives: CONTENT_LENGTH is the client's claim.
        let mut body = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        while body.len() < content_length {
            let want = (content_length - body.len()).min(chunk.len());
            let n = self.read(&mut chunk[..want]);
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
        body
    }

    /// `close_packet()`: fills in the open record's header and pads it.
    fn close_packet(&self) {
        if let Some((at, kind)) = self.out_hdr.take() {
            let mut out = self.out.borrow_mut();
            let len = out.len() - at - HEADER_LEN;
            let (h, pad) = make_header(kind, self.id.get(), len);
            out[at..at + HEADER_LEN].copy_from_slice(&h);
            let end = out.len() + pad;
            out.resize(end, 0);
        }
    }

    /// `fcgi_flush()`: sends the buffered records, with `END_REQUEST`
    /// (application status 0, `FCGI_REQUEST_COMPLETE`) when `end`.
    fn flush_out(&self, end: bool) -> bool {
        self.close_packet();
        let mut out = std::mem::take(&mut *self.out.borrow_mut());
        if end {
            out.extend_from_slice(&record(END_REQUEST, self.id.get(), &[0; 8]));
        }
        let ok = out.is_empty() || self.write_all(&out);
        if !ok {
            self.keep.set(false);
        }
        out.clear();
        *self.out.borrow_mut() = out;
        ok
    }

    /// `fcgi_write()`: `data` as records of type `kind`, buffered up to
    /// [`OUT_BUF`] bytes.
    fn write_records(&self, kind: u8, data: &[u8]) -> bool {
        // After `fastcgi_finish_request()` the script's output goes nowhere.
        if data.is_empty() || self.ended.get() {
            return true;
        }
        if self.out_hdr.get().is_some_and(|(_, t)| t != kind) {
            self.close_packet();
        }
        let mut rest = data;
        while !rest.is_empty() {
            if self.out_hdr.get().is_none() {
                // Room for a header and at least one byte: an empty
                // record would end the stream.
                if OUT_BUF.saturating_sub(self.out.borrow().len()) <= HEADER_LEN
                    && !self.flush_out(false)
                {
                    return false;
                }
                let mut out = self.out.borrow_mut();
                self.out_hdr.set(Some((out.len(), kind)));
                out.extend_from_slice(&[0; HEADER_LEN]);
            }
            let limit = OUT_BUF.saturating_sub(self.out.borrow().len());
            if rest.len() < limit {
                self.out.borrow_mut().extend_from_slice(rest);
                return true;
            }
            self.out.borrow_mut().extend_from_slice(&rest[..limit]);
            rest = &rest[limit..];
            if !self.flush_out(false) {
                return false;
            }
        }
        true
    }

    /// `fcgi_finish_request()`: ends the request (`fcgi_end()`: the
    /// buffered output and `END_REQUEST`, once) and closes the connection
    /// unless the web server keeps it (always with `force_close`).
    pub fn finish_request(&self, force_close: bool) -> bool {
        if self.is_closed() {
            return true;
        }
        let mut ok = true;
        if !self.ended.get() {
            ok = self.flush_out(true);
            self.ended.set(true);
        }
        self.close(force_close);
        ok
    }

    /// `fcgi_close()`: a graceful close shuts down the sending side and
    /// reads what the web server still sends.
    pub fn close(&self, force: bool) {
        if !force && self.keep.get() {
            return;
        }
        let Some(mut s) = self.stream.borrow_mut().take() else {
            return;
        };
        // A partly read STDIN record does not outlive its connection.
        self.in_len.set(0);
        if !force {
            s.shutdown_write();
            let mut buf = [0u8; 8];
            while s.read(&mut buf).is_ok_and(|n| n > 0) {}
        }
        self.nodelay.set(false);
    }
}

impl FastCgiStreams for Connection {
    fn stdout(&self, data: &[u8]) -> bool {
        self.write_records(STDOUT, data)
    }

    fn stderr(&self, data: &[u8]) -> bool {
        self.write_records(STDERR, data)
    }

    fn flush(&self) -> bool {
        self.ended.get() || self.flush_out(false)
    }

    fn is_finished(&self) -> bool {
        self.is_closed() || self.ended.get()
    }

    fn finish(&self) -> bool {
        if self.is_finished() {
            return false;
        }
        let ok = self.flush_out(true);
        self.ended.set(true);
        self.close(false);
        ok
    }
}

/// The `FastCGI` side of `main()`: the listening socket and what
/// `fcgi_accept_request()` needs.
#[derive(Debug)]
pub struct Server {
    listener: Listener,
    allowed: Option<Vec<IpAddr>>,
    mgmt: MgmtVars,
    /// `client_sa`: the address of the last accepted client.
    last_client: std::cell::Cell<Option<SocketAddr>>,
}

impl Server {
    /// A server on `listener`. Accepting is non-blocking underneath, so
    /// that SIGTERM ends the wait (`accept()` without `SA_RESTART`).
    pub fn new(listener: Listener, allowed: Option<Vec<IpAddr>>, mgmt: MgmtVars) -> Self {
        let _ = listener.set_nonblocking();
        Server {
            listener,
            allowed,
            mgmt,
            last_client: std::cell::Cell::new(None),
        }
    }

    /// `fcgi_get_last_client_ip()`: `None` for a Unix socket client
    /// (php-fpm's access log).
    #[allow(dead_code)]
    pub fn last_client_ip(&self) -> Option<String> {
        self.last_client.get().map(|c| client_ip(Some(c)))
    }

    /// `fcgi_accept_request()`: the next request, on `conn` while it stays
    /// open, else on a new connection; `None` at shutdown or when
    /// accepting fails.
    pub fn accept_request(&self, conn: &mut Option<std::rc::Rc<Connection>>) -> Option<Env> {
        loop {
            if conn.as_ref().is_none_or(|c| c.is_closed()) {
                *conn = Some(std::rc::Rc::new(self.accept()?));
            } else if sys::in_shutdown() {
                return None;
            }
            let c = conn.as_ref()?;
            match c.read_request(&self.mgmt) {
                Incoming::Request(env) => return Some(env),
                Incoming::Management => {}
                Incoming::Failed => {
                    c.close(true);
                    *conn = None;
                }
            }
        }
    }

    /// The accept part of `fcgi_accept_request()`: a connection from an
    /// allowed client that sends something within 5 seconds.
    fn accept(&self) -> Option<Connection> {
        loop {
            if sys::in_shutdown() {
                return None;
            }
            // A signal ends the wait (then `in_shutdown` is checked); an
            // error shows in `accept()`.
            if sys::poll_in(self.listener.raw_fd(), -1) == sys::Poll::Interrupted {
                continue;
            }
            let (stream, client) = match self.listener.accept() {
                Ok(x) => x,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::Interrupted
                            | std::io::ErrorKind::ConnectionAborted
                    ) =>
                {
                    continue
                }
                Err(_) => return None,
            };
            self.last_client.set(client);
            if !is_allowed(self.allowed.as_deref(), client) {
                log(&format!(
                    "Connection disallowed: IP address '{}' has been dropped.",
                    client_ip(client)
                ));
                continue;
            }
            loop {
                match sys::poll_in(stream.raw_fd(), 5000) {
                    sys::Poll::Interrupted => {}
                    sys::Poll::Readable => return Some(Connection::new(stream)),
                    sys::Poll::NotReadable => break,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_pad_to_eight_bytes() {
        let (h, pad) = make_header(STDOUT, 1, 5);
        assert_eq!(h, [1, 6, 0, 1, 0, 5, 3, 0]);
        assert_eq!(pad, 3);
        assert_eq!(make_header(STDOUT, 0x102, 16).1, 0);
        assert_eq!(record(STDERR, 1, b"abc").len(), 16);
        let h = Header::parse([1, 4, 0, 7, 1, 2, 6, 0]);
        assert_eq!((h.kind, h.id, h.len, h.padding), (PARAMS, 7, 258, 6));
    }

    #[test]
    fn params_round_trip_both_length_forms() {
        let long = vec![b'v'; 300];
        let mut body = Vec::new();
        encode_param(&mut body, b"SCRIPT_FILENAME", b"/srv/x.php");
        encode_param(&mut body, b"LONG", &long);
        encode_param(&mut body, b"SCRIPT_FILENAME", b"/srv/y.php");
        let mut env = Env::new();
        assert!(get_params(&mut env, &body));
        assert_eq!(
            env,
            vec![
                (b"SCRIPT_FILENAME".to_vec(), b"/srv/y.php".to_vec()),
                (b"LONG".to_vec(), long),
            ]
        );
    }

    #[test]
    fn malformed_params_are_rejected() {
        let mut env = Env::new();
        // A name length with no value length after it.
        assert!(!get_params(&mut env, &[3]));
        // Lengths beyond the body.
        assert!(!get_params(&mut env, &[3, 3, b'a', b'b']));
        // A 4-byte length cut short.
        assert!(!get_params(&mut env, &[0x80, 0, 0]));
    }

    #[test]
    fn allowed_client_lists() {
        let v4: SocketAddr = "10.0.0.2:1".parse().unwrap();
        let mapped: SocketAddr = "[::ffff:10.0.0.2]:1".parse().unwrap();
        let other: SocketAddr = "10.0.0.3:1".parse().unwrap();
        let list = allowed_clients("10.0.0.2,::1");
        assert!(is_allowed(Some(&list), Some(v4)));
        assert!(is_allowed(Some(&list), Some(mapped)));
        assert!(!is_allowed(Some(&list), Some(other)));
        assert!(is_allowed(None, Some(other)));
        assert!(
            is_allowed(Some(&[]), None),
            "Unix sockets are always allowed"
        );
        assert!(!is_allowed(Some(&[]), Some(v4)));
        assert_eq!(client_ip(Some(mapped)), "10.0.0.2");
    }

    /// GHSA-qc49-hpvq-g2w6: a body record longer than `CONTENT_LENGTH`
    /// whose rest is a whole second request (here `SCRIPT_FILENAME` of
    /// the attacker's choice) on a kept connection: the next read refuses
    /// it instead of running it.
    #[test]
    fn unread_body_is_not_a_second_request() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let conn = Connection::new(Stream::Unix(ours));
        let mut params = Vec::new();
        encode_param(&mut params, b"CONTENT_LENGTH", b"2");
        let mut smuggled_params = Vec::new();
        encode_param(&mut smuggled_params, b"SCRIPT_FILENAME", b"/tmp/evil.php");
        let mut smuggled = record(BEGIN_REQUEST, 1, &[0, 1, KEEP_CONN, 0, 0, 0, 0, 0]);
        smuggled.extend(record(PARAMS, 1, &smuggled_params));
        smuggled.extend(record(PARAMS, 1, b""));
        smuggled.extend(record(STDIN, 1, b""));
        let mut body = b"ok".to_vec();
        body.extend_from_slice(&smuggled);
        let mut wire = record(BEGIN_REQUEST, 1, &[0, 1, KEEP_CONN, 0, 0, 0, 0, 0]);
        wire.extend(record(PARAMS, 1, &params));
        wire.extend(record(PARAMS, 1, b""));
        wire.extend(record(STDIN, 1, &body));
        wire.extend(record(STDIN, 1, b""));
        theirs.write_all(&wire).unwrap();
        let mgmt = MgmtVars::new(None);
        assert!(matches!(conn.read_request(&mgmt), Incoming::Request(_)));
        assert_eq!(conn.read_body(2), b"ok");
        assert!(conn.keep());
        conn.finish_request(false);
        assert!(
            matches!(conn.read_request(&mgmt), Incoming::Failed),
            "the rest of the body record was parsed as a request"
        );
    }

    /// A request over a socket pair: params, body, output records and
    /// `END_REQUEST`, then a graceful close.
    #[test]
    fn request_on_a_socket_pair() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let conn = Connection::new(Stream::Unix(ours));
        let mut params = Vec::new();
        encode_param(&mut params, b"CONTENT_LENGTH", b"5");
        let mut wire = record(BEGIN_REQUEST, 3, &[0, 1, KEEP_CONN, 0, 0, 0, 0, 0]);
        wire.extend(record(PARAMS, 3, &params));
        wire.extend(record(PARAMS, 3, b""));
        wire.extend(record(STDIN, 3, b"hel"));
        wire.extend(record(STDIN, 3, b"lo"));
        wire.extend(record(STDIN, 3, b""));
        theirs.write_all(&wire).unwrap();

        let Incoming::Request(env) = conn.read_request(&MgmtVars::new(None)) else {
            panic!("no request");
        };
        assert_eq!(env[0], (b"FCGI_ROLE".to_vec(), b"RESPONDER".to_vec()));
        assert_eq!(env[1], (b"CONTENT_LENGTH".to_vec(), b"5".to_vec()));
        assert!(conn.keep());
        assert_eq!(conn.read_body(5), b"hello");
        assert!(conn.stdout(b"Content-type: text/html\r\n\r\nhi"));
        assert!(conn.stderr(b"logged\n"));
        assert!(conn.finish_request(false));
        assert!(!conn.is_closed(), "FCGI_KEEP_CONN keeps the connection");

        let mut got = vec![0u8; 8 + 32 + 8 + 8 + 8 + 8];
        theirs.read_exact(&mut got).unwrap();
        let h = Header::parse(got[..8].try_into().unwrap());
        assert_eq!((h.kind, h.id, h.len, h.padding), (STDOUT, 3, 29, 3));
        assert_eq!(&got[8..37], b"Content-type: text/html\r\n\r\nhi");
        let h = Header::parse(got[40..48].try_into().unwrap());
        assert_eq!((h.kind, h.len), (STDERR, 7));
        assert_eq!(&got[48..55], b"logged\n");
        let h = Header::parse(got[56..64].try_into().unwrap());
        assert_eq!((h.kind, h.id, h.len), (END_REQUEST, 3, 8));
        assert_eq!(&got[64..72], &[0; 8]);
        conn.finish_request(true);
        assert!(conn.is_closed());
    }

    /// `GET_VALUES` is answered with the management variables asked for.
    #[test]
    fn get_values_management_record() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let conn = Connection::new(Stream::Unix(ours));
        let mut q = Vec::new();
        encode_param(&mut q, b"FCGI_MAX_CONNS", b"");
        encode_param(&mut q, b"UNKNOWN", b"");
        encode_param(&mut q, b"FCGI_MPXS_CONNS", b"");
        theirs.write_all(&record(GET_VALUES, 0, &q)).unwrap();
        assert_eq!(
            conn.read_request(&MgmtVars::new(Some("4"))),
            Incoming::Management
        );
        let mut h = [0u8; 8];
        theirs.read_exact(&mut h).unwrap();
        let h = Header::parse(h);
        assert_eq!((h.kind, h.id), (GET_VALUES_RESULT, 0));
        let mut body = vec![0u8; h.len + h.padding];
        theirs.read_exact(&mut body).unwrap();
        let mut env = Env::new();
        assert!(get_params(&mut env, &body[..h.len]));
        assert_eq!(
            env,
            vec![
                (b"FCGI_MPXS_CONNS".to_vec(), b"0".to_vec()),
                (b"FCGI_MAX_CONNS".to_vec(), b"4".to_vec()),
            ]
        );
    }

    /// An unknown role ends the connection.
    #[test]
    fn unknown_roles_fail() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let conn = Connection::new(Stream::Unix(ours));
        theirs
            .write_all(&record(BEGIN_REQUEST, 1, &[0, 9, 0, 0, 0, 0, 0, 0]))
            .unwrap();
        assert_eq!(conn.read_request(&MgmtVars::new(None)), Incoming::Failed);
    }

    /// Output larger than the buffer goes out in several records.
    #[test]
    fn large_output_is_split() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let conn = Connection::new(Stream::Unix(ours));
        let data = vec![b'x'; 20_000];
        let reader = std::thread::spawn(move || {
            let mut all = Vec::new();
            theirs.read_to_end(&mut all).unwrap();
            all
        });
        assert!(conn.stdout(&data));
        assert!(conn.finish_request(true));
        drop(conn);
        let all = reader.join().unwrap();
        let mut p = 0;
        let mut body = Vec::new();
        let mut ended = false;
        while p < all.len() {
            let h = Header::parse(all[p..p + 8].try_into().unwrap());
            p += 8;
            match h.kind {
                STDOUT => body.extend_from_slice(&all[p..p + h.len]),
                END_REQUEST => ended = true,
                k => panic!("unexpected record {k}"),
            }
            p += h.len + h.padding;
        }
        assert!(ended);
        assert_eq!(body, data);
    }
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `tcp_socket/ssl` stream ops (`php_openssl_socket_ops`): a socket
//! that is either plain or carries an OpenSSL session, with PHP's
//! blocking/timeout emulation over a non-blocking descriptor.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsRawFd, RawFd};
use std::time::{Duration, Instant};

use openssl::error::ErrorStack;
use openssl::ssl::{ErrorCode, SslContext, SslStream};
use php_streams::diag::{Severity, StreamErrorCode};
use php_streams::stream::{CastAs, OpFailed, StreamOps, StreamOption, StreamState};
use php_streams::stream::{OPT_ERR, OPT_NOTIMPL, OPT_OK};
use php_streams::value::Value as SValue;

/// The ops label (`stream_type`).
pub(crate) const LABEL: &str = "tcp_socket/ssl";

/// `STREAM_CRYPTO_STATUS_WANT_READ`.
pub(crate) const STATUS_WANT_READ: i64 = 1;
/// `STREAM_CRYPTO_STATUS_WANT_WRITE`.
pub(crate) const STATUS_WANT_WRITE: i64 = 2;
/// `SSL_R_NO_SHARED_CIPHER`.
const SSL_R_NO_SHARED_CIPHER: u64 = 193;

/// `php_stream_warn(stream, code, msg)`.
pub(crate) fn warn(code: StreamErrorCode, msg: impl Into<Vec<u8>>) {
    php_streams::diag::stream_error(Severity::Warning, "", code, true, msg);
}

/// `php_pollfd_for()`: waits until `fd` is ready; `None` waits forever.
pub(crate) fn poll_fd(fd: RawFd, events: i16, timeout: Option<Duration>) -> bool {
    let ms = timeout.map_or(-1, |t| i32::try_from(t.as_millis()).unwrap_or(i32::MAX));
    let mut p = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    // SAFETY: `p` is a valid, initialised pollfd for the duration of the call.
    let rc = unsafe { libc::poll(&raw mut p, 1, ms) };
    rc > 0
}

/// The socket underneath: plain, or wrapped in a TLS session.
#[derive(Debug)]
pub(crate) enum Conn {
    /// No descriptor (closed).
    None,
    /// A plain connected socket.
    Plain(TcpStream),
    /// A listening socket.
    Listener(TcpListener),
    /// A socket with an `SSL` handle attached (`ssl_handle != NULL`).
    Tls(SslStream<TcpStream>),
}

/// `php_openssl_netstream_data_t`.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct TlsOps {
    /// The socket.
    pub(crate) conn: Conn,
    /// The `SSL_CTX` (kept for session streams and accepted connections).
    pub(crate) ctx: Option<SslContext>,
    /// `enable_on_connect`.
    pub(crate) enable_on_connect: bool,
    /// `is_client`.
    pub(crate) is_client: bool,
    /// `ssl_active`.
    pub(crate) ssl_active: bool,
    /// `last_status`.
    pub(crate) last_status: i64,
    /// `method`.
    pub(crate) method: i64,
    /// `url_name` (host of the URL, for SNI and peer name checks).
    pub(crate) url_name: Option<String>,
    /// `s.is_blocked`.
    pub(crate) is_blocked: bool,
    /// `s.timeout` (`None`: wait forever).
    pub(crate) timeout: Option<Duration>,
    /// `connect_timeout`.
    pub(crate) connect_timeout: Option<Duration>,
    /// `s.timeout_event`.
    pub(crate) timed_out: bool,
    /// `persistent_id != NULL`.
    pub(crate) persistent: bool,
    /// The context has PHP callbacks (`session_callbacks`, `psk_callbacks`
    /// or `early_data_callbacks` set).
    pub(crate) callbacks: bool,
    /// `early_data_state`.
    pub(crate) early_state: super::early::State,
    /// `early_data_send`.
    pub(crate) early_send: Option<Vec<u8>>,
    /// `early_data_offset`.
    pub(crate) early_offset: usize,
    /// The handshake of a connection with crypto enabled on connect, left
    /// for when its stream is a resource ([`super::handshake`]).
    pub(crate) handshake: Option<super::Handshake>,
}

impl std::fmt::Debug for TlsOps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsOps")
            .field("is_client", &self.is_client)
            .field("ssl_active", &self.ssl_active)
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

/// Outcome of `php_openssl_handle_ssl_error`.
pub(crate) struct SslErrorOutcome {
    /// `retry`.
    pub(crate) retry: bool,
    /// Whether `errno` was set to `EAGAIN` (`Some(false)`: set to 0;
    /// `None`: untouched).
    pub(crate) eagain: Option<bool>,
    /// `stream->eof = 1` requested.
    pub(crate) eof: bool,
}

/// `ERR_error_string_n` lines of an error stack.
fn error_lines(stack: &ErrorStack) -> Vec<String> {
    stack
        .errors()
        .iter()
        .map(|e| php_openssl::ffi::err_error_string(e.code()))
        .collect()
}

/// `php_openssl_handle_ssl_error()`.
pub(crate) fn handle_ssl_error(
    e: &openssl::ssl::Error,
    is_init: bool,
    is_blocked: bool,
    last_status: &mut i64,
) -> SslErrorOutcome {
    let code = e.code();
    let mut out = SslErrorOutcome {
        retry: true,
        eagain: None,
        eof: false,
    };
    if code == ErrorCode::ZERO_RETURN {
        out.retry = false;
        return out;
    }
    if code == ErrorCode::WANT_READ || code == ErrorCode::WANT_WRITE {
        out.eagain = Some(true);
        out.retry = is_init || is_blocked;
        if !out.retry {
            *last_status = if code == ErrorCode::WANT_READ {
                STATUS_WANT_READ
            } else {
                STATUS_WANT_WRITE
            };
        }
        return out;
    }
    let stack_empty = e.ssl_error().is_none_or(|s| s.errors().is_empty());
    if code == ErrorCode::SYSCALL && stack_empty {
        if let Some(io) = e.io_error() {
            let errno = io.raw_os_error().unwrap_or(0);
            warn(
                StreamErrorCode::ProtocolError,
                format!("SSL: {}", php_streams::sys::strerror(errno)),
            );
        } else {
            out.eof = true;
        }
        out.retry = false;
        return out;
    }
    let lines = e.ssl_error().map(error_lines).unwrap_or_default();
    let first_reason = e
        .ssl_error()
        .and_then(|s| s.errors().first().map(|x| x.code() & 0x7F_FFFF));
    if first_reason == Some(SSL_R_NO_SHARED_CIPHER) {
        warn(
            StreamErrorCode::ProtocolError,
            "SSL_R_NO_SHARED_CIPHER: no suitable shared cipher could be used.  This could be because the server is missing an SSL certificate (local_cert context option)",
        );
    } else {
        let raw = code.as_raw();
        let msg = if lines.is_empty() {
            format!("SSL operation failed with code {raw}. ")
        } else {
            format!(
                "SSL operation failed with code {raw}. OpenSSL Error messages:\n{}",
                lines.join("\n")
            )
        };
        warn(StreamErrorCode::ProtocolError, msg);
    }
    out.retry = false;
    out.eagain = Some(false);
    out
}

impl TlsOps {
    /// A new socket structure (`php_openssl_ssl_socket_factory`).
    pub(crate) fn new(conn: Conn, timeout: Option<Duration>) -> Self {
        TlsOps {
            conn,
            ctx: None,
            enable_on_connect: false,
            is_client: false,
            ssl_active: false,
            last_status: 0,
            method: 0,
            url_name: None,
            is_blocked: true,
            timeout,
            connect_timeout: timeout,
            timed_out: false,
            persistent: false,
            callbacks: false,
            early_state: super::early::State::None,
            early_send: None,
            early_offset: 0,
            handshake: None,
        }
    }

    /// The descriptor.
    pub(crate) fn fd(&self) -> Option<RawFd> {
        match &self.conn {
            Conn::None => None,
            Conn::Plain(s) => Some(s.as_raw_fd()),
            Conn::Listener(l) => Some(l.as_raw_fd()),
            Conn::Tls(s) => Some(s.get_ref().as_raw_fd()),
        }
    }

    /// The SSL stream, when set up.
    pub(crate) fn tls(&self) -> Option<&SslStream<TcpStream>> {
        match &self.conn {
            Conn::Tls(s) => Some(s),
            _ => None,
        }
    }

    fn plain_io(
        &mut self,
        st: &mut StreamState,
        read: Option<&mut [u8]>,
        write: &[u8],
    ) -> Result<usize, OpFailed> {
        let fd = self.fd().ok_or(OpFailed)?;
        let blocked = self.is_blocked;
        let timeout = self.timeout;
        let sock: &mut TcpStream = match &mut self.conn {
            Conn::Plain(s) => s,
            Conn::Tls(s) => s.get_mut(),
            Conn::None | Conn::Listener(_) => return Err(OpFailed),
        };
        self.timed_out = false;
        if let Some(buf) = read {
            // `php_sockop_read`: no wait once this read returned data
            // (`has_buffered_data`) or with a zero timeout; the descriptor
            // is non-blocking, so the read then returns what is there.
            let dont_wait = st.has_buffered_data || timeout.is_some_and(|t| t.is_zero());
            if blocked && !dont_wait && !poll_fd(fd, libc::POLLIN | libc::POLLPRI, timeout) {
                self.timed_out = true;
                return Err(OpFailed);
            }
            return match sock.read(buf) {
                Ok(0) => {
                    st.eof = true;
                    Ok(0)
                }
                Ok(n) => Ok(n),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    Ok(0)
                }
                Err(_) => {
                    st.eof = true;
                    Err(OpFailed)
                }
            };
        }
        loop {
            match sock.write(write) {
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if !blocked {
                        return Ok(0);
                    }
                    if !poll_fd(fd, libc::POLLOUT, timeout) {
                        self.timed_out = true;
                        return Err(OpFailed);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Err(OpFailed),
            }
        }
    }

    /// `php_openssl_sockop_io()` with SSL active.
    fn ssl_io(
        &mut self,
        st: &mut StreamState,
        read: Option<&mut [u8]>,
        write: &[u8],
    ) -> Result<usize, OpFailed> {
        let Some(fd) = self.fd() else {
            return Err(OpFailed);
        };
        let began_blocked = self.is_blocked;
        let is_read = read.is_some();
        // Some data was already returned: just try to fill the buffer
        // more, without retrying or blocking.
        let supplemental = is_read && st.has_buffered_data;
        let timeout = if began_blocked && !supplemental {
            self.timeout
        } else {
            None
        };
        let has_timeout = timeout.is_some_and(|t| !t.is_zero());
        let start = Instant::now();
        let mut rbuf = read;
        let Conn::Tls(ssl) = &mut self.conn else {
            return Err(OpFailed);
        };
        let mut nr: usize = 0;
        super::sess::set_current(st.id);
        loop {
            let elapsed = start.elapsed();
            if has_timeout && timeout.is_some_and(|t| elapsed > t) {
                self.timed_out = true;
                return Err(OpFailed);
            }
            self.last_status = 0;
            let r = match rbuf.as_deref_mut() {
                Some(buf) => {
                    let r = ssl.ssl_read(buf);
                    if super::reneg::should_close(ssl.ssl()) {
                        // Renegotiation rate limiting triggered.
                        let _ = ssl.get_ref().shutdown(std::net::Shutdown::Both);
                        st.eof = true;
                        nr = 0;
                        break;
                    }
                    r
                }
                None => ssl.ssl_write(write),
            };
            let left = timeout.map(|t| t.saturating_sub(elapsed));
            match r {
                Ok(n) => {
                    nr = n;
                    break;
                }
                Err(e) => {
                    let code = e.code();
                    // The descriptor is non-blocking for the operation, so
                    // `is_blocked` is false inside `handle_ssl_error`.
                    let out = handle_ssl_error(&e, false, false, &mut self.last_status);
                    let mut retry = out.retry;
                    let eagain = out.eagain == Some(true);
                    if eagain && code == ErrorCode::WANT_READ && is_read {
                        retry = true;
                    }
                    if eagain && code == ErrorCode::WANT_WRITE && !is_read {
                        retry = true;
                    }
                    if is_read {
                        st.eof = !retry
                            && code != ErrorCode::WANT_READ
                            && code != ErrorCode::WANT_WRITE
                            && ssl.ssl().pending() == 0;
                    }
                    if !began_blocked || supplemental {
                        break;
                    }
                    if !retry {
                        break;
                    }
                    let ev = if (code == ErrorCode::WANT_WRITE) == is_read {
                        libc::POLLOUT | libc::POLLPRI
                    } else {
                        libc::POLLIN | libc::POLLPRI
                    };
                    poll_fd(fd, ev, if has_timeout { left } else { None });
                }
            }
        }
        // A supplemental read that found nothing is not WANT_READ.
        if supplemental && nr == 0 && self.last_status == STATUS_WANT_READ {
            self.last_status = 0;
        }
        Ok(nr)
    }

    /// `PHP_STREAM_OPTION_CHECK_LIVENESS`.
    fn alive(&mut self) -> bool {
        match &mut self.conn {
            Conn::None => false,
            Conn::Listener(_) => true,
            Conn::Tls(ssl) if self.ssl_active => {
                let mut b = [0u8; 1];
                match ssl.ssl_peek(&mut b) {
                    Ok(_) => true,
                    Err(e) => match e.code() {
                        ErrorCode::WANT_READ | ErrorCode::WANT_WRITE => true,
                        ErrorCode::SYSCALL => e
                            .io_error()
                            .is_some_and(|io| io.kind() == io::ErrorKind::WouldBlock),
                        _ => false,
                    },
                }
            }
            conn => {
                let s = match conn {
                    Conn::Plain(s) => s,
                    Conn::Tls(t) => t.get_mut(),
                    Conn::None | Conn::Listener(_) => return false,
                };
                let mut b = [0u8; 1];
                match s.peek(&mut b) {
                    Ok(0) => false,
                    Ok(_) => true,
                    Err(e) => e.kind() == io::ErrorKind::WouldBlock,
                }
            }
        }
    }
}

impl StreamOps for TlsOps {
    fn label(&self) -> &'static str {
        LABEL
    }

    fn read(&mut self, st: &mut StreamState, buf: &mut [u8]) -> Result<usize, OpFailed> {
        if matches!(self.conn, Conn::Listener(_)) {
            return Err(OpFailed);
        }
        if self.ssl_active {
            self.ssl_io(st, Some(buf), &[])
        } else {
            self.plain_io(st, Some(buf), &[])
        }
    }

    fn write(&mut self, st: &mut StreamState, buf: &[u8]) -> Result<usize, OpFailed> {
        if matches!(self.conn, Conn::Listener(_)) {
            return Err(OpFailed);
        }
        if self.ssl_active {
            self.ssl_io(st, None, buf)
        } else {
            self.plain_io(st, None, buf)
        }
    }

    fn close(&mut self, _st: &mut StreamState, _preserve: bool) -> i32 {
        if let Conn::Tls(s) = &self.conn {
            super::reneg::forget(s.ssl());
        }
        if self.ssl_active {
            if let Conn::Tls(s) = &mut self.conn {
                let _ = s.shutdown();
            }
            self.ssl_active = false;
        }
        self.conn = Conn::None;
        self.ctx = None;
        0
    }

    fn set_option(&mut self, st: &mut StreamState, opt: &mut StreamOption<'_>) -> i64 {
        match opt {
            StreamOption::Blocking(b) => {
                let old = i64::from(self.is_blocked);
                self.is_blocked = *b;
                old
            }
            StreamOption::ReadTimeout(sec, usec) => {
                let micros = sec.saturating_mul(1_000_000).saturating_add(*usec);
                self.timeout = u64::try_from(micros).ok().map(Duration::from_micros);
                self.timed_out = false;
                OPT_OK
            }
            StreamOption::CheckLiveness => {
                if self.alive() {
                    OPT_OK
                } else {
                    OPT_ERR
                }
            }
            StreamOption::MetaData(out) => {
                out.push(("timed_out".into(), SValue::Bool(self.timed_out)));
                out.push(("blocked".into(), SValue::Bool(self.is_blocked)));
                out.push(("eof".into(), SValue::Bool(st.eof)));
                OPT_OK
            }
            _ => OPT_NOTIMPL,
        }
    }

    fn cast(&mut self, _st: &mut StreamState, kind: CastAs, _report: bool) -> Option<RawFd> {
        match kind {
            CastAs::FdForSelect => self.fd(),
            CastAs::Stdio | CastAs::Fd | CastAs::SocketFd => {
                if self.ssl_active {
                    None
                } else {
                    self.fd()
                }
            }
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

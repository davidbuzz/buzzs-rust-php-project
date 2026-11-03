//! The socket stream `socket_export_stream()` creates
//! (`php_stream_sock_open_from_socket()` / the transport's socket ops).

use std::os::fd::RawFd;
use std::time::Duration;

use php_streams::stream::{
    flags, CastAs, OpFailed, Stream, StreamOps, StreamOption, StreamState, OPT_ERR, OPT_NOTIMPL,
    OPT_OK,
};
use php_streams::value::Value as SValue;

/// `php_netstream_data_t` over a descriptor the `Socket` object shares:
/// closing the stream closes the descriptor.
#[derive(Debug)]
pub(crate) struct SocketStreamOps {
    fd: RawFd,
    label: &'static str,
    timeout: Option<Duration>,
    timed_out: bool,
    blocked: bool,
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Waits until `fd` is ready for `events`; `None` waits forever.
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

/// A stream for `fd`; `protocol` is the library's
/// [`php_sockets::Socket::export_protocol`].
pub(crate) fn socket_stream(
    fd: RawFd,
    protocol: Option<&str>,
    blocked: bool,
    timeout_secs: i64,
) -> Stream {
    let label = match protocol {
        Some("tcp://") => "tcp_socket",
        Some("udp://") => "udp_socket",
        Some("unix://") => "unix_socket",
        Some("udg://") => "udg_socket",
        _ => "generic_socket",
    };
    let ops = SocketStreamOps {
        fd,
        label,
        timeout: u64::try_from(timeout_secs).ok().map(Duration::from_secs),
        timed_out: false,
        blocked,
    };
    let mut s = Stream::new(Box::new(ops), "r+");
    s.add_flags(flags::NO_SEEK);
    s
}

impl StreamOps for SocketStreamOps {
    fn label(&self) -> &'static str {
        self.label
    }

    fn read(&mut self, st: &mut StreamState, buf: &mut [u8]) -> Result<usize, OpFailed> {
        if self.fd < 0 {
            return Err(OpFailed);
        }
        self.timed_out = false;
        if self.blocked && !wait_fd(self.fd, libc::POLLIN, self.timeout) {
            self.timed_out = true;
            return Err(OpFailed);
        }
        // SAFETY: `buf` is a valid writable buffer of `buf.len()` bytes.
        let n = unsafe { libc::recv(self.fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
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
        if self.fd < 0 {
            return Err(OpFailed);
        }
        // SAFETY: `buf` is a valid readable buffer of `buf.len()` bytes.
        let n = unsafe { libc::send(self.fd, buf.as_ptr().cast(), buf.len(), libc::MSG_NOSIGNAL) };
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
        if self.fd >= 0 && !preserve {
            // SAFETY: the descriptor belongs to this stream (the Socket
            // object was marked stream-owned when the stream was created).
            unsafe { libc::close(self.fd) };
        }
        self.fd = -1;
        0
    }

    fn set_option(&mut self, st: &mut StreamState, opt: &mut StreamOption<'_>) -> i64 {
        if self.fd < 0 {
            return OPT_ERR;
        }
        match opt {
            StreamOption::Blocking(b) => {
                let old = i64::from(self.blocked);
                if php_streams::sys::set_blocking(self.fd, *b).is_err() {
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
        (self.fd >= 0).then_some(self.fd)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

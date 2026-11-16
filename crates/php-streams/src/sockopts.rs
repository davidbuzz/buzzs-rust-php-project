// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `socket` context options of socket transports
//! (`php_sockop_parse_sockvals`, the `STREAM_SOCKOP_*` flags of
//! `php_tcp_sockop_bind`/`connect`/`accept` and
//! `php_network_apply_sockvals` in `main/network.c`).

use std::os::fd::RawFd;

use crate::context::ContextRef;
use crate::sys;
use crate::value::Value;

/// The kind of transport the options are applied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XportKind {
    /// `tcp://` (and the TLS transports over it).
    Tcp,
    /// `udp://`.
    Udp,
    /// `unix://`.
    Unix,
    /// `udg://`.
    Udg,
}

impl XportKind {
    /// The kind of a transport scheme.
    pub fn of_scheme(scheme: &str) -> Self {
        match scheme {
            "udp" => XportKind::Udp,
            "unix" => XportKind::Unix,
            "udg" => XportKind::Udg,
            _ => XportKind::Tcp,
        }
    }
}

/// Parsed `socket` context options.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // independent socket flags
pub struct SockOpts {
    /// `so_linger` (TCP).
    pub linger: Option<i32>,
    /// `so_rcvbuf`.
    pub rcvbuf: Option<i32>,
    /// `so_sndbuf`.
    pub sndbuf: Option<i32>,
    /// `tcp_keepidle`.
    pub keepidle: Option<i32>,
    /// `tcp_keepintvl`.
    pub keepintvl: Option<i32>,
    /// `tcp_keepcnt`.
    pub keepcnt: Option<i32>,
    /// `so_keepalive` (TCP).
    pub keepalive: bool,
    /// `tcp_nodelay` (TCP clients and accepted connections).
    pub nodelay: bool,
    /// `so_broadcast` (UDP).
    pub broadcast: bool,
    /// `so_reuseport` (servers).
    pub reuseport: bool,
    /// `SO_REUSEADDR` (servers; on unless `so_reuseaddr` is false).
    pub reuseaddr: bool,
    /// `backlog` (servers; 32 by default).
    pub backlog: i32,
    /// Whether these are a listening socket's options.
    pub server: bool,
}

#[allow(clippy::cast_possible_truncation)] // `(int)zval_get_long()`
fn int(v: &Value) -> i32 {
    v.to_long() as i32
}

impl SockOpts {
    /// Reads the options for a `kind` socket; `server` selects the bind
    /// options. The error is the transport's error text.
    ///
    /// # Errors
    /// `so_rcvbuf`/`so_sndbuf` outside `1..=INT_MAX`.
    pub fn from_context(
        ctx: Option<&ContextRef>,
        kind: XportKind,
        server: bool,
    ) -> Result<Self, String> {
        let mut o = SockOpts {
            server,
            reuseaddr: server,
            backlog: 32,
            ..SockOpts::default()
        };
        let Some(ctx) = ctx else {
            return Ok(o);
        };
        let c = ctx.borrow();
        let get = |name: &str| c.get("socket", name).cloned();
        let tcp = kind == XportKind::Tcp;
        if tcp {
            o.linger = get("so_linger").map(|v| int(&v));
        }
        for (name, slot) in [("so_rcvbuf", &mut o.rcvbuf), ("so_sndbuf", &mut o.sndbuf)] {
            if let Some(v) = get(name) {
                let n = v.to_long();
                if !(1..=i64::from(i32::MAX)).contains(&n) {
                    return Err(format!(
                        "{name} context option must be between 1 and {}",
                        i32::MAX
                    ));
                }
                *slot = i32::try_from(n).ok();
            }
        }
        if tcp {
            o.keepalive = get("so_keepalive").is_some_and(|v| v.is_true());
            o.keepidle = get("tcp_keepidle").map(|v| int(&v));
            o.keepintvl = get("tcp_keepintvl").map(|v| int(&v));
            o.keepcnt = get("tcp_keepcnt").map(|v| int(&v));
            o.nodelay = get("tcp_nodelay").is_some_and(|v| v.is_true());
        }
        if kind == XportKind::Udp {
            o.broadcast = get("so_broadcast").is_some_and(|v| v.is_true());
        }
        if server {
            o.reuseport = get("so_reuseport").is_some_and(|v| v.is_true());
            o.reuseaddr = get("so_reuseaddr").is_none_or(|v| v.is_true());
            if let Some(b) = get("backlog") {
                o.backlog = int(&b);
            }
        }
        Ok(o)
    }

    /// The options that must be set before `bind()`.
    pub fn apply_pre_bind(&self, fd: RawFd) {
        if self.reuseaddr {
            sys::setsockopt_int(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, 1);
        }
        if self.reuseport {
            sys::setsockopt_int(fd, libc::SOL_SOCKET, libc::SO_REUSEPORT, 1);
        }
        if self.broadcast {
            sys::setsockopt_int(fd, libc::SOL_SOCKET, libc::SO_BROADCAST, 1);
        }
    }

    /// `SO_KEEPALIVE`/`TCP_NODELAY` and `php_network_apply_sockvals()`.
    pub fn apply(&self, fd: RawFd) {
        if self.broadcast {
            sys::setsockopt_int(fd, libc::SOL_SOCKET, libc::SO_BROADCAST, 1);
        }
        // TCP_NODELAY is a connect/accept option, not a listener one.
        if self.nodelay && !self.server {
            sys::setsockopt_int(fd, libc::IPPROTO_TCP, libc::TCP_NODELAY, 1);
        }
        if self.keepalive {
            sys::setsockopt_int(fd, libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1);
        }
        self.apply_sockvals(fd);
    }

    /// `php_network_apply_sockvals()`.
    pub fn apply_sockvals(&self, fd: RawFd) {
        if let Some(l) = self.linger {
            let secs = l.min(i32::from(u16::MAX));
            sys::setsockopt_linger(fd, l > 0, if l > 0 { secs } else { 0 });
        }
        for (v, name) in [
            (self.keepidle, libc::TCP_KEEPIDLE),
            (self.keepintvl, libc::TCP_KEEPINTVL),
            (self.keepcnt, libc::TCP_KEEPCNT),
        ] {
            if let Some(v) = v {
                sys::setsockopt_int(fd, libc::IPPROTO_TCP, name, v);
            }
        }
        if let Some(v) = self.rcvbuf {
            sys::setsockopt_int(fd, libc::SOL_SOCKET, libc::SO_RCVBUF, v);
        }
        if let Some(v) = self.sndbuf {
            sys::setsockopt_int(fd, libc::SOL_SOCKET, libc::SO_SNDBUF, v);
        }
    }

    /// The options applied to a connection accepted on a server with this
    /// context (`php_tcp_sockop_accept`: `tcp_nodelay`, `tcp_keepidle`).
    #[must_use]
    pub fn for_accepted(&self) -> SockOpts {
        SockOpts {
            nodelay: self.nodelay,
            keepidle: self.keepidle,
            ..SockOpts::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::StreamContext;

    fn ctx(opts: &[(&str, Value)]) -> ContextRef {
        let mut c = StreamContext::new();
        for (k, v) in opts {
            c.set("socket", k, v.clone());
        }
        c.shared()
    }

    #[test]
    fn parse_and_validate() {
        let c = ctx(&[("so_rcvbuf", Value::Int(0))]);
        assert_eq!(
            SockOpts::from_context(Some(&c), XportKind::Tcp, true),
            Err("so_rcvbuf context option must be between 1 and 2147483647".to_string())
        );
        let c = ctx(&[
            ("so_keepalive", Value::str("1")),
            ("tcp_keepidle", Value::str("80")),
            ("so_reuseaddr", Value::Bool(false)),
            ("so_linger", Value::Int(10)),
        ]);
        let o = SockOpts::from_context(Some(&c), XportKind::Tcp, true).expect("ok");
        assert!(o.keepalive && !o.reuseaddr);
        assert_eq!((o.keepidle, o.linger), (Some(80), Some(10)));
        let o = SockOpts::from_context(Some(&c), XportKind::Unix, false).expect("ok");
        assert!(!o.keepalive && o.linger.is_none());
        assert!(
            SockOpts::from_context(None, XportKind::Tcp, true)
                .expect("ok")
                .reuseaddr
        );
    }

    #[test]
    fn applies_to_a_socket() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let fd = std::os::fd::AsRawFd::as_raw_fd(&l);
        let o = SockOpts {
            keepalive: true,
            linger: Some(5),
            ..SockOpts::default()
        };
        o.apply(fd);
    }
}

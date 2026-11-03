//! Network helpers of `ext/standard`: `inet_pton`/`inet_ntop`,
//! `ip2long`/`long2ip`, `gethostname`, `gethostbyname(l)`,
//! `gethostbyaddr`, `getservbyname`/`getservbyport`,
//! `getprotobyname`/`getprotobynumber`, `net_get_interfaces` (`net.c`) and
//! `openlog`/`syslog`/`closelog` (`syslog.c`, `main/php_syslog.c`).

use php_runtime::{
    ArrayKey, ErrorLevel, RtResult, RuntimeContext, RuntimeError, Value, ZArray, ZStr,
};

use crate::sys::{self, SockAddr};

/// `MAXFQDNLEN`.
pub const MAXFQDNLEN: usize = 255;

fn path_arg(func: &str, n: u32, name: &str, s: &[u8]) -> RtResult<()> {
    if s.contains(&0) {
        return Err(RuntimeError::ValueError(format!(
            "{func}(): Argument #{n} (${name}) must not contain any null bytes"
        )));
    }
    Ok(())
}

/// `inet_ntop($ip)`: `false` unless `ip` is 4 or 16 bytes.
#[must_use]
pub fn inet_ntop(ip: &[u8]) -> Value {
    let af = match ip.len() {
        16 => libc::AF_INET6,
        4 => libc::AF_INET,
        _ => return Value::Bool(false),
    };
    sys::inet_ntop_raw(af, ip).map_or(Value::Bool(false), Value::from)
}

/// `inet_pton($ip)`.
pub fn inet_pton(ip: &[u8]) -> RtResult<Value> {
    path_arg("inet_pton", 1, "ip", ip)?;
    let af = if ip.contains(&b':') {
        libc::AF_INET6
    } else if ip.contains(&b'.') {
        libc::AF_INET
    } else {
        return Ok(Value::Bool(false));
    };
    Ok(sys::inet_pton_raw(af, ip).map_or(Value::Bool(false), |b| Value::Str(ZStr::new(b))))
}

/// `ip2long($ip)`.
pub fn ip2long(ip: &[u8]) -> RtResult<Value> {
    path_arg("ip2long", 1, "ip", ip)?;
    if ip.is_empty() {
        return Err(RuntimeError::ValueError(
            "ip2long(): Argument #1 ($ip) must not be empty".into(),
        ));
    }
    Ok(match sys::inet_pton_raw(libc::AF_INET, ip) {
        Some(b) => Value::Int(i64::from(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))),
        None => Value::Bool(false),
    })
}

/// `long2ip($ip)` (the low 32 bits are used).
#[must_use]
pub fn long2ip(ip: i64) -> String {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let v = ip as u32;
    let b = v.to_be_bytes();
    format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])
}

/// `gethostname()`.
pub fn gethostname(ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
    match sys::gethostname() {
        Ok(h) => Ok(Value::Str(ZStr::new(h))),
        Err(e) => {
            ctx.diagnostic(
                ErrorLevel::Warning,
                &format!(
                    "gethostname(): Unable to fetch host [{e}]: {}",
                    sys::strerror(e)
                ),
            )?;
            Ok(Value::Bool(false))
        }
    }
}

/// `gethostbyname($hostname)`: the first IPv4 address, or the name itself.
pub fn gethostbyname(hostname: &[u8], ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<ZStr> {
    path_arg("gethostbyname", 1, "hostname", hostname)?;
    if hostname.len() > MAXFQDNLEN {
        ctx.diagnostic(
            ErrorLevel::Warning,
            &format!("gethostbyname(): Host name cannot be longer than {MAXFQDNLEN} characters"),
        )?;
        return Ok(ZStr::new(hostname));
    }
    match sys::gethostbyname(hostname).and_then(|l| l.first().copied()) {
        Some(a) => Ok(ZStr::from(format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3]))),
        None => Ok(ZStr::new(hostname)),
    }
}

/// `gethostbynamel($hostname)`.
pub fn gethostbynamel(
    hostname: &[u8],
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<Value> {
    path_arg("gethostbynamel", 1, "hostname", hostname)?;
    if hostname.len() > MAXFQDNLEN {
        ctx.diagnostic(
            ErrorLevel::Warning,
            &format!("gethostbynamel(): Host name cannot be longer than {MAXFQDNLEN} characters"),
        )?;
        return Ok(Value::Bool(false));
    }
    Ok(match sys::gethostbyname(hostname) {
        None => Value::Bool(false),
        Some(list) => {
            Value::Array(ZArray::from_values(list.iter().map(|a| {
                Value::from(format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3]))
            })))
        }
    })
}

/// `gethostbyaddr($ip)`.
pub fn gethostbyaddr(ip: &[u8], ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
    path_arg("gethostbyaddr", 1, "ip", ip)?;
    let packed =
        sys::inet_pton_raw(libc::AF_INET6, ip).or_else(|| sys::inet_pton_raw(libc::AF_INET, ip));
    let Some(packed) = packed else {
        ctx.diagnostic(
            ErrorLevel::Warning,
            "gethostbyaddr(): Address is not a valid IPv4 or IPv6 address",
        )?;
        return Ok(Value::Bool(false));
    };
    Ok(Value::Str(ZStr::new(
        sys::reverse_lookup(&packed).unwrap_or_else(|| ip.to_vec()),
    )))
}

/// `getservbyname($service, $protocol)`.
pub fn getservbyname(service: &[u8], protocol: &[u8]) -> RtResult<Value> {
    path_arg("getservbyname", 1, "service", service)?;
    path_arg("getservbyname", 2, "protocol", protocol)?;
    Ok(sys::getservbyname(service, protocol)
        .map_or(Value::Bool(false), |p| Value::Int(i64::from(p))))
}

/// `getservbyport($port, $protocol)`.
pub fn getservbyport(port: i64, protocol: &[u8]) -> RtResult<Value> {
    path_arg("getservbyport", 2, "protocol", protocol)?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let port = port as u16;
    Ok(sys::getservbyport(port, protocol).map_or(Value::Bool(false), |s| Value::Str(ZStr::new(s))))
}

/// `getprotobyname($protocol)`.
pub fn getprotobyname(protocol: &[u8]) -> RtResult<Value> {
    path_arg("getprotobyname", 1, "protocol", protocol)?;
    Ok(sys::getprotobyname(protocol).map_or(Value::Bool(false), Value::Int))
}

/// `getprotobynumber($protocol)`.
#[must_use]
pub fn getprotobynumber(protocol: i64) -> Value {
    #[allow(clippy::cast_possible_truncation)]
    let p = protocol as i32;
    sys::getprotobynumber(p).map_or(Value::Bool(false), |s| Value::Str(ZStr::new(s)))
}

/// `php_inet_ntop()` of a `getifaddrs` address.
fn sockaddr_str(a: Option<&SockAddr>) -> Option<String> {
    match a? {
        SockAddr::V4(b) => sys::inet_ntop_raw(libc::AF_INET, b),
        SockAddr::V6(b) => sys::inet_ntop_raw(libc::AF_INET6, b),
        SockAddr::Other(_) => None,
    }
}

/// `net_get_interfaces()`.
pub fn net_get_interfaces(ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
    let addrs = match sys::getifaddrs() {
        Ok(a) => a,
        Err(e) => {
            ctx.diagnostic(
                ErrorLevel::Warning,
                &format!("getifaddrs() failed {e}: {}", sys::strerror(e)),
            )?;
            return Ok(Value::Bool(false));
        }
    };
    Ok(Value::Array(interfaces_array(&addrs)))
}

/// Builds the `net_get_interfaces()` array from `getifaddrs` entries.
#[must_use]
pub fn interfaces_array(addrs: &[sys::IfAddr]) -> ZArray {
    let mut out = ZArray::new();
    let t = out.make_mut();
    for a in addrs {
        let key = ArrayKey::Str(ZStr::new(a.name.as_slice()));
        let iface = t.lookup(key);
        if !matches!(iface, Value::Array(_)) {
            *iface = Value::Array(ZArray::new());
        }
        let Value::Array(iface) = iface else { continue };
        let it = iface.make_mut();
        let unicast = it.lookup("unicast");
        if !matches!(unicast, Value::Array(_)) {
            *unicast = Value::Array(ZArray::new());
        }
        let mut u = ZArray::new();
        let ut = u.make_mut();
        ut.insert("flags", Value::Int(i64::from(a.flags)));
        if let Some(addr) = &a.addr {
            ut.insert("family", Value::Int(addr.family()));
            if let Some(s) = sockaddr_str(Some(addr)) {
                ut.insert("address", Value::from(s));
            }
        }
        if let Some(s) = sockaddr_str(a.netmask.as_ref()) {
            ut.insert("netmask", Value::from(s));
        }
        #[allow(clippy::cast_sign_loss)]
        let (bcast, ptp) = (libc::IFF_BROADCAST as u32, libc::IFF_POINTOPOINT as u32);
        if a.flags & bcast != 0 {
            if let Some(s) = sockaddr_str(a.broad_or_dst.as_ref()) {
                ut.insert("broadcast", Value::from(s));
            }
        }
        if a.flags & ptp != 0 {
            if let Some(s) = sockaddr_str(a.broad_or_dst.as_ref()) {
                ut.insert("ptp", Value::from(s));
            }
        }
        if let Value::Array(un) = unicast {
            let _ = un.make_mut().push(Value::Array(u));
        }
        if it.get_str(b"up").is_none() {
            #[allow(clippy::cast_sign_loss)]
            let up = a.flags & (libc::IFF_UP as u32) != 0;
            it.insert("up", Value::Bool(up));
        }
    }
    out
}

// ---------------------------------------------------------------- syslog

/// `syslog.filter` values (`PHP_SYSLOG_FILTER_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyslogFilter {
    /// `all`: every byte passes except NUL-free newline splitting.
    All,
    /// `no-ctrl` (default): control characters are escaped.
    NoCtrl,
    /// `ascii`: control and 8-bit characters are escaped.
    Ascii,
    /// `raw`: sent as is.
    Raw,
}

/// The `LOG_*` constants.
pub const SYSLOG_CONSTANTS: &[(&str, i64)] = &[
    ("LOG_EMERG", libc::LOG_EMERG as i64),
    ("LOG_ALERT", libc::LOG_ALERT as i64),
    ("LOG_CRIT", libc::LOG_CRIT as i64),
    ("LOG_ERR", libc::LOG_ERR as i64),
    ("LOG_WARNING", libc::LOG_WARNING as i64),
    ("LOG_NOTICE", libc::LOG_NOTICE as i64),
    ("LOG_INFO", libc::LOG_INFO as i64),
    ("LOG_DEBUG", libc::LOG_DEBUG as i64),
    ("LOG_KERN", libc::LOG_KERN as i64),
    ("LOG_USER", libc::LOG_USER as i64),
    ("LOG_MAIL", libc::LOG_MAIL as i64),
    ("LOG_DAEMON", libc::LOG_DAEMON as i64),
    ("LOG_AUTH", libc::LOG_AUTH as i64),
    ("LOG_SYSLOG", libc::LOG_SYSLOG as i64),
    ("LOG_LPR", libc::LOG_LPR as i64),
    ("LOG_NEWS", libc::LOG_NEWS as i64),
    ("LOG_UUCP", libc::LOG_UUCP as i64),
    ("LOG_CRON", libc::LOG_CRON as i64),
    ("LOG_AUTHPRIV", libc::LOG_AUTHPRIV as i64),
    ("LOG_LOCAL0", libc::LOG_LOCAL0 as i64),
    ("LOG_LOCAL1", libc::LOG_LOCAL1 as i64),
    ("LOG_LOCAL2", libc::LOG_LOCAL2 as i64),
    ("LOG_LOCAL3", libc::LOG_LOCAL3 as i64),
    ("LOG_LOCAL4", libc::LOG_LOCAL4 as i64),
    ("LOG_LOCAL5", libc::LOG_LOCAL5 as i64),
    ("LOG_LOCAL6", libc::LOG_LOCAL6 as i64),
    ("LOG_LOCAL7", libc::LOG_LOCAL7 as i64),
    ("LOG_PID", libc::LOG_PID as i64),
    ("LOG_CONS", libc::LOG_CONS as i64),
    ("LOG_ODELAY", libc::LOG_ODELAY as i64),
    ("LOG_NDELAY", libc::LOG_NDELAY as i64),
    ("LOG_NOWAIT", libc::LOG_NOWAIT as i64),
    ("LOG_PERROR", libc::LOG_PERROR as i64),
];

/// `php_syslog_str()`'s filtering: the lines sent to `syslog()`.
#[must_use]
pub fn syslog_lines(message: &[u8], filter: SyslogFilter) -> Vec<Vec<u8>> {
    if filter == SyslogFilter::Raw {
        return vec![message.to_vec()];
    }
    let mut out = Vec::new();
    let mut buf = Vec::new();
    for &c in message {
        if (0x20..=0x7e).contains(&c) || (c >= 0x80 && filter != SyslogFilter::Ascii) {
            buf.push(c);
        } else if c == b'\n' {
            out.push(std::mem::take(&mut buf));
        } else if c < 0x20 && filter == SyslogFilter::All {
            buf.push(c);
        } else {
            const X: &[u8; 16] = b"0123456789abcdef";
            buf.extend_from_slice(&[b'\\', b'x', X[usize::from(c >> 4)], X[usize::from(c & 15)]]);
        }
    }
    out.push(buf);
    out
}

/// Per-request syslog state (`PG(have_called_openlog)`,
/// `BG(syslog_device)`).
#[derive(Debug, Default)]
pub struct SyslogState {
    have_called_openlog: bool,
}

impl SyslogState {
    /// `openlog($prefix, $flags, $facility)`.
    pub fn openlog(&mut self, prefix: &[u8], flags: i64, facility: i64) -> RtResult<bool> {
        path_arg("openlog", 1, "prefix", prefix)?;
        #[allow(clippy::cast_possible_truncation)]
        sys::openlog(prefix, flags as i32, facility as i32);
        self.have_called_openlog = true;
        Ok(true)
    }

    /// `closelog()`.
    pub fn closelog(&mut self) -> bool {
        sys::closelog();
        self.have_called_openlog = false;
        true
    }

    /// `syslog($priority, $message)`; `ident`/`facility` are the
    /// `syslog.ident`/`syslog.facility` INI values used when `openlog()`
    /// was not called.
    pub fn syslog(
        &mut self,
        priority: i64,
        message: &[u8],
        filter: SyslogFilter,
        ident: &[u8],
        facility: i64,
    ) -> bool {
        if !self.have_called_openlog {
            #[allow(clippy::cast_possible_truncation)]
            sys::openlog(ident, 0, facility as i32);
            self.have_called_openlog = true;
        }
        #[allow(clippy::cast_possible_truncation)]
        let prio = priority as i32;
        for line in syslog_lines(message, filter) {
            sys::syslog(prio, &line);
        }
        true
    }

    /// Request shutdown.
    pub fn request_shutdown(&mut self) {
        if self.have_called_openlog {
            self.closelog();
        }
    }
}

//! `conversions.c`: PHP arrays ⇄ `struct msghdr`/`cmsghdr`/`sockaddr`/
//! `in6_pktinfo`/`ucred`, with PHP's error paths
//! (`"error converting user data (path: msghdr > control > element #0 >
//! data): ..."`), plus `sendrecvmsg.c` (the ancillary registry,
//! `socket_sendmsg()`, `socket_recvmsg()`, the IPv6 RFC 3542 options).

use crate::sys::{self, SockAddr};
use crate::value::{find, is_numeric, Key, Numeric, OutValue, Value};
use crate::{Socket, SocketError, SocketResult, Sockets};

/// `MAX_USER_BUFF_SIZE`.
const MAX_USER_BUFF_SIZE: i64 = 100 * 1024 * 1024;
/// `DEFAULT_BUFF_SIZE`.
const DEFAULT_BUFF_SIZE: usize = 8192;
/// `sizeof(struct in6_pktinfo)`.
const PKTINFO_SIZE: usize = 20;
/// `sizeof(struct ucred)`.
const UCRED_SIZE: usize = 12;

/// How an ancillary payload is (de)serialised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AncKind {
    Pktinfo,
    Int,
    Ucred,
    Rights,
}

/// An entry of the ancillary registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ancillary {
    pub size: usize,
    pub var_el_size: usize,
    kind: AncKind,
}

/// `get_ancillary_reg_entry()`.
pub(crate) fn ancillary(level: i32, ty: i32) -> Option<Ancillary> {
    let e = |size, var_el_size, kind| Some(Ancillary { size, var_el_size, kind });
    match (level, ty) {
        (libc::IPPROTO_IPV6, libc::IPV6_PKTINFO) => e(PKTINFO_SIZE, 0, AncKind::Pktinfo),
        (libc::IPPROTO_IPV6, libc::IPV6_HOPLIMIT | libc::IPV6_TCLASS) => e(4, 0, AncKind::Int),
        (libc::SOL_SOCKET, libc::SCM_CREDENTIALS) => e(UCRED_SIZE, 0, AncKind::Ucred),
        (libc::SOL_SOCKET, libc::SCM_RIGHTS) => e(0, 4, AncKind::Rights),
        _ => None,
    }
}

// ----------------------------------------------------------- user → native

/// `ser_context`.
struct Ser<'a> {
    keys: Vec<String>,
    err: Option<String>,
    thrown: Option<SocketError>,
    st: &'a mut Sockets,
    sock: &'a mut Socket,
    fill_sockaddr: bool,
}

impl Ser<'_> {
    fn failed(&self) -> bool {
        self.err.is_some() || self.thrown.is_some()
    }

    fn error(&mut self, msg: impl Into<String>) {
        if self.err.is_some() {
            return;
        }
        let path = self.keys.join(" > ");
        let path = if path.is_empty() { "unavailable".to_owned() } else { path };
        self.err = Some(format!("error converting user data (path: {path}): {}", msg.into()));
    }

    fn with_key<T>(&mut self, key: &str, f: impl FnOnce(&mut Self) -> T) -> T {
        self.keys.push(key.to_owned());
        let r = f(self);
        self.keys.pop();
        r
    }

    /// `zval_get_tmp_string()`.
    fn string(&mut self, v: &Value) -> Vec<u8> {
        match v.to_php_string() {
            Ok((s, array_warning)) => {
                if array_warning {
                    self.st.warn("Array to string conversion");
                }
                s
            }
            Err(e) => {
                if self.thrown.is_none() {
                    self.thrown = Some(SocketError::Error(e.0));
                }
                Vec::new()
            }
        }
    }

    /// `from_zval_integer_common()`.
    fn integer(&mut self, v: &Value) -> i64 {
        match v {
            Value::Int(i) => *i,
            Value::Float(f) => crate::value::dval_to_lval(*f),
            Value::Str(_) | Value::Object { .. } => {
                let s = self.string(v);
                if self.failed() {
                    return 0;
                }
                match is_numeric(&s) {
                    Some(Numeric::Int(i)) => i,
                    Some(Numeric::Float(f)) => crate::value::dval_to_lval(f),
                    None => {
                        self.error(format!(
                            "expected an integer, but got a non numeric string (possibly from a converted object): '{}'",
                            String::from_utf8_lossy(&s)
                        ));
                        0
                    }
                }
            }
            _ => {
                self.error("expected an integer, either of a PHP integer type or of a convertible type");
                0
            }
        }
    }

    fn int(&mut self, v: &Value) -> i32 {
        let l = self.integer(v);
        if self.failed() {
            return 0;
        }
        i32::try_from(l).unwrap_or_else(|_| {
            self.error("given PHP integer is out of bounds for a native int");
            0
        })
    }

    fn uint32(&mut self, v: &Value) -> u32 {
        let l = self.integer(v);
        if self.failed() {
            return 0;
        }
        u32::try_from(l).unwrap_or_else(|_| {
            self.error("given PHP integer is out of bounds for an unsigned 32-bit integer");
            0
        })
    }

    fn uint16(&mut self, v: &Value) -> u16 {
        let l = self.integer(v);
        if self.failed() {
            return 0;
        }
        u16::try_from(l).unwrap_or_else(|_| {
            self.error("given PHP integer is out of bounds for an unsigned 16-bit integer");
            0
        })
    }

    fn sa_family(&mut self, v: &Value) -> u16 {
        let l = self.integer(v);
        if self.failed() {
            return 0;
        }
        u16::try_from(l).unwrap_or_else(|_| {
            self.error("given PHP integer is out of bounds for a sa_family_t value");
            0
        })
    }

    fn pid(&mut self, v: &Value) -> i32 {
        let l = self.integer(v);
        if self.failed() {
            return 0;
        }
        match i32::try_from(l) {
            Ok(p) if p >= 0 => p,
            _ => {
                self.error("given PHP integer is out of bounds for a pid_t value");
                0
            }
        }
    }

    fn uid(&mut self, v: &Value) -> u32 {
        let l = self.integer(v);
        if self.failed() {
            return 0;
        }
        u32::try_from(l).unwrap_or_else(|_| {
            self.error("given PHP integer is out of bounds for a uid_t value");
            0
        })
    }

    fn sin_addr(&mut self, v: &Value) -> [u8; 4] {
        let s = self.string(v);
        if self.thrown.is_some() {
            return [0; 4];
        }
        if let Some(ip) = self.st.inet_addr(self.sock, &s) {
            ip
        } else {
            self.error(format!(
                "could not resolve address '{}' to get an AF_INET address",
                String::from_utf8_lossy(&sys::cstr(&s).into_bytes())
            ));
            [0; 4]
        }
    }

    fn sin6_addr(&mut self, v: &Value) -> [u8; 16] {
        let s = self.string(v);
        if self.thrown.is_some() {
            return [0; 16];
        }
        if let Some((ip, _)) = self.st.inet6_addr(self.sock, &s) {
            ip
        } else {
            self.error(format!(
                "could not resolve address '{}' to get an AF_INET6 address",
                String::from_utf8_lossy(&sys::cstr(&s).into_bytes())
            ));
            [0; 16]
        }
    }

    fn ifindex(&mut self, v: &Value) -> u32 {
        if let Value::Int(i) = v {
            return u32::try_from(*i).unwrap_or_else(|_| {
                self.error(format!(
                    "the interface index cannot be negative or larger than {}; given {i}",
                    u32::MAX
                ));
                0
            });
        }
        let s = self.string(v);
        if self.thrown.is_some() {
            return 0;
        }
        let name = sys::cstr(&s);
        match sys::if_nametoindex(&name) {
            0 => {
                self.error(format!(
                    "no interface with name \"{}\" could be found",
                    String::from_utf8_lossy(name.as_bytes())
                ));
                0
            }
            i => i,
        }
    }

    /// `from_zval_write_aggregation()`: calls `f(self, key, value)` for
    /// each present descriptor key, in descriptor order.
    fn aggregate(
        &mut self,
        container: &Value,
        fields: &[(&str, bool)],
        mut f: impl FnMut(&mut Self, &str, &Value),
    ) {
        let Value::Array(t) = container else {
            self.error("expected an array here");
            return;
        };
        for &(name, required) in fields {
            if self.failed() {
                break;
            }
            if let Some(v) = find(t, name) {
                self.with_key(name, |s| f(s, name, v));
            } else if required {
                self.error(format!("The key '{name}' is required"));
                break;
            }
        }
    }

    // --------------------------------------------------------- sockaddr

    fn sockaddr(&mut self, container: &Value) -> Option<SockAddr> {
        let Value::Array(t) = container else {
            self.error("expected an array here");
            return None;
        };
        let family = match find(t, "family") {
            Some(v) if *v != Value::Null => {
                let f = self.with_key("family", |s| s.int(v));
                if self.failed() {
                    return None;
                }
                f
            }
            _ => self.sock.family,
        };
        let sock_family = self.sock.family;
        match family {
            libc::AF_INET => {
                if sock_family != libc::AF_INET && sock_family != libc::AF_INET6 {
                    self.error(format!("the specified family (number {family}) is not supported on this socket"));
                    return None;
                }
                if !self.fill_sockaddr {
                    return Some(SockAddr::from_raw(&[], std::mem::size_of::<libc::sockaddr_in>()));
                }
                let (mut ip, mut port) = ([0u8; 4], 0u16);
                self.aggregate(container, &[("family", false), ("addr", false), ("port", false)], |s, k, v| match k {
                    "family" => {
                        s.sa_family(v);
                    }
                    "addr" => ip = s.sin_addr(v),
                    _ => port = s.uint16(v),
                });
                Some(SockAddr::inet(ip, port))
            }
            libc::AF_INET6 => {
                if sock_family != libc::AF_INET6 {
                    self.error("the specified family (AF_INET6) is not supported on this socket");
                    return None;
                }
                if !self.fill_sockaddr {
                    return Some(SockAddr::from_raw(&[], std::mem::size_of::<libc::sockaddr_in6>()));
                }
                let (mut ip, mut port, mut flow, mut scope) = ([0u8; 16], 0u16, 0u32, 0u32);
                self.aggregate(
                    container,
                    &[("family", false), ("addr", false), ("port", false), ("flowinfo", false), ("scope_id", false)],
                    |s, k, v| match k {
                        "family" => {
                            s.sa_family(v);
                        }
                        "addr" => ip = s.sin6_addr(v),
                        "port" => port = s.uint16(v),
                        "flowinfo" => flow = s.uint32(v),
                        _ => scope = s.uint32(v),
                    },
                );
                Some(SockAddr::inet6(ip, port, flow, scope))
            }
            libc::AF_UNIX => {
                if sock_family != libc::AF_UNIX {
                    self.error("the specified family (AF_UNIX) is not supported on this socket");
                    return None;
                }
                if !self.fill_sockaddr {
                    return Some(SockAddr::from_raw(&[], std::mem::size_of::<libc::sockaddr_un>()));
                }
                let mut path: Vec<u8> = Vec::new();
                self.aggregate(container, &[("family", false), ("path", false)], |s, k, v| {
                    if k == "family" {
                        s.sa_family(v);
                        return;
                    }
                    let p = s.string(v);
                    if s.thrown.is_some() {
                        return;
                    }
                    if p.is_empty() {
                        s.error("the path is must not be empty");
                    } else if p.len() >= sys::SUN_PATH_LEN {
                        s.error(format!(
                            "the path is too long, the maximum permitted length is {}",
                            sys::SUN_PATH_LEN - 1
                        ));
                    } else {
                        path = p;
                    }
                });
                let strlen = |b: &[u8]| b.iter().position(|&c| c == 0).unwrap_or(b.len());
                let len = if path.first() == Some(&0) {
                    1 + strlen(&path[1..])
                } else {
                    strlen(&path)
                };
                Some(SockAddr::unix(&path, sys::SUN_PATH_OFFSET + len))
            }
            _ => {
                self.error("the only families currently supported are AF_INET, AF_INET6 and AF_UNIX");
                None
            }
        }
    }

    // ------------------------------------------------------- ancillary

    fn pktinfo(&mut self, v: &Value) -> Vec<u8> {
        let (mut ip, mut idx) = ([0u8; 16], 0u32);
        self.aggregate(v, &[("addr", true), ("ifindex", true)], |s, k, v| {
            if k == "addr" {
                ip = s.sin6_addr(v);
            } else {
                idx = s.ifindex(v);
            }
        });
        let mut out = ip.to_vec();
        out.extend_from_slice(&idx.to_ne_bytes());
        out
    }

    fn ucred(&mut self, v: &Value) -> Vec<u8> {
        let (mut pid, mut uid, mut gid) = (0i32, 0u32, 0u32);
        self.aggregate(v, &[("pid", true), ("uid", true), ("gid", true)], |s, k, v| match k {
            "pid" => pid = s.pid(v),
            "uid" => uid = s.uid(v),
            _ => gid = s.uid(v),
        });
        let mut out = pid.to_ne_bytes().to_vec();
        out.extend_from_slice(&uid.to_ne_bytes());
        out.extend_from_slice(&gid.to_ne_bytes());
        out
    }

    fn fd_array(&mut self, v: &Value) -> Vec<u8> {
        let Value::Array(t) = v else {
            self.error("expected an array here");
            return Vec::new();
        };
        let mut out = Vec::new();
        for (i, (_, elem)) in t.iter().enumerate() {
            let fd = self.with_key(&format!("element #{}", i + 1), |s| match elem {
                Value::Socket { fd } if *fd < 0 => {
                    s.error("socket is already closed");
                    0
                }
                Value::Socket { fd } => *fd,
                Value::Stream { fd: Some(fd) } => *fd,
                Value::Stream { fd: None } => {
                    s.error("cast stream to file descriptor failed");
                    0
                }
                Value::Resource => {
                    s.error("resource is not a stream");
                    0
                }
                _ => {
                    s.error("expected a Socket object or a stream resource");
                    0
                }
            });
            if self.failed() {
                break;
            }
            out.extend_from_slice(&fd.to_ne_bytes());
        }
        out
    }

    fn control_element(&mut self, elem: &Value, buf: &mut Vec<u8>) {
        let mut level = 0;
        self.aggregate(elem, &[("level", false)], |s, _, v| level = s.int(v));
        if self.failed() {
            return;
        }
        let mut ty = 0;
        self.aggregate(elem, &[("type", false)], |s, _, v| ty = s.int(v));
        if self.failed() {
            return;
        }
        let Some(entry) = ancillary(level, ty) else {
            self.error(format!("cmsghdr with level {level} and type {ty} not supported"));
            return;
        };
        let data_len = if entry.kind == AncKind::Rights {
            let Some(data) = elem.get("data") else {
                self.error("cmsghdr should have a 'data' element here");
                return;
            };
            match data {
                Value::Array(a) if a.is_empty() => {
                    self.error("expected at least one element in this array");
                    return;
                }
                Value::Array(a) => a.len() * 4,
                _ => {
                    self.error("expected an array here");
                    return;
                }
            }
        } else {
            entry.size
        };
        let mut payload = vec![0u8; data_len];
        self.aggregate(elem, &[("data", false)], |s, _, v| {
            let bytes = match entry.kind {
                AncKind::Pktinfo => s.pktinfo(v),
                AncKind::Int => s.int(v).to_ne_bytes().to_vec(),
                AncKind::Ucred => s.ucred(v),
                AncKind::Rights => s.fd_array(v),
            };
            let n = bytes.len().min(payload.len());
            payload[..n].copy_from_slice(&bytes[..n]);
        });
        let space = sys::cmsg_space(data_len);
        let start = buf.len();
        buf.resize(start + space, 0);
        let len = sys::cmsg_len(data_len);
        buf[start..start + 8].copy_from_slice(&(len as u64).to_ne_bytes());
        buf[start + 8..start + 12].copy_from_slice(&level.to_ne_bytes());
        buf[start + 12..start + 16].copy_from_slice(&ty.to_ne_bytes());
        let data_off = start + sys::cmsg_len(0);
        buf[data_off..data_off + data_len].copy_from_slice(&payload);
    }

    fn control_array(&mut self, v: &Value) -> Vec<u8> {
        let Value::Array(t) = v else {
            self.error("expected an array here");
            return Vec::new();
        };
        let mut buf = Vec::new();
        for (i, (_, elem)) in t.iter().enumerate() {
            if self.failed() {
                break;
            }
            self.with_key(&format!("element #{i}"), |s| s.control_element(elem, &mut buf));
        }
        buf
    }
}

/// A `struct msghdr` for `sendmsg()`.
#[derive(Debug, Default)]
struct SendMsg {
    name: Option<SockAddr>,
    iov: Vec<Vec<u8>>,
    control: Vec<u8>,
}

/// The buffers to give `recvmsg()`.
#[derive(Debug)]
struct RecvSpec {
    name_len: Option<usize>,
    iov: Vec<usize>,
    controllen: usize,
}

fn finish<T>(st: &mut Sockets, ser_err: Option<String>, thrown: Option<SocketError>, v: T) -> SocketResult<T> {
    if let Some(t) = thrown {
        return Err(t);
    }
    if let Some(e) = ser_err {
        st.warn(e);
        return Err(SocketError::Failed);
    }
    Ok(v)
}

// ----------------------------------------------------------- native → user

/// `res_context`.
struct Res {
    keys: Vec<String>,
    err: Option<String>,
}

impl Res {
    fn error(&mut self, msg: impl Into<String>) {
        if self.err.is_some() {
            return;
        }
        let path = self.keys.join(" > ");
        let path = if path.is_empty() { "unavailable".to_owned() } else { path };
        self.err = Some(format!("error converting native data (path: {path}): {}", msg.into()));
    }

    fn with_key<T>(&mut self, key: &str, f: impl FnOnce(&mut Self) -> T) -> T {
        self.keys.push(key.to_owned());
        let r = f(self);
        self.keys.pop();
        r
    }

    fn sockaddr(&mut self, a: &SockAddr) -> OutValue {
        let fam = a.family();
        let family = || OutValue::Int(fam.into());
        match fam {
            0 => OutValue::Null,
            libc::AF_INET => OutValue::assoc(vec![
                ("family", family()),
                ("addr", OutValue::Str(sys::ntop4(a.ip4()).into_bytes())),
                ("port", OutValue::Int(a.port().into())),
            ]),
            libc::AF_INET6 => OutValue::assoc(vec![
                ("family", family()),
                ("addr", OutValue::Str(sys::ntop6(a.ip6()).into_bytes())),
                ("port", OutValue::Int(a.port().into())),
                ("flowinfo", OutValue::Int(a.flowinfo().into())),
                ("scope_id", OutValue::Int(a.scope_id().into())),
            ]),
            libc::AF_UNIX => {
                let p = a.sun_path();
                let path = match p.iter().position(|&b| b == 0) {
                    Some(end) => OutValue::Str(p[..end].to_vec()),
                    None => {
                        self.with_key("path", |s| s.error("could not find a NUL in the path"));
                        OutValue::Null
                    }
                };
                OutValue::assoc(vec![("family", family()), ("path", path)])
            }
            _ => {
                self.error(format!("cannot read struct sockaddr with family {fam}; not supported"));
                OutValue::Array(Vec::new())
            }
        }
    }

    fn pktinfo(data: &[u8]) -> OutValue {
        let mut ip = [0u8; 16];
        ip.copy_from_slice(&data[..16]);
        let idx = u32::from_ne_bytes([data[16], data[17], data[18], data[19]]);
        OutValue::assoc(vec![
            ("addr", OutValue::Str(sys::ntop6(ip).into_bytes())),
            ("ifindex", OutValue::Int(idx.into())),
        ])
    }

    fn fd_array(&mut self, data: &[u8], st: &mut Sockets) -> OutValue {
        let mut out = Vec::new();
        for (i, chunk) in data.chunks_exact(4).enumerate() {
            let fd = i32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            let v = match sys::is_socket(fd) {
                Err(e) => {
                    self.error(format!(
                        "error creating resource for received file descriptor {fd}: fstat() call failed with errno {e}"
                    ));
                    break;
                }
                Ok(true) => match st.import_fd(fd) {
                    Ok(mut s) => {
                        s.set_stream_owned(false);
                        OutValue::Socket(s)
                    }
                    Err(_) => {
                        self.error(format!(
                            "error getting protocol descriptor {fd}: getsockopt() call failed with errno {}",
                            sys::errno()
                        ));
                        break;
                    }
                },
                Ok(false) => OutValue::StreamFd(fd),
            };
            out.push((Key::Int(i64::try_from(i).unwrap_or(0)), v));
        }
        OutValue::Array(out)
    }

    fn control(&mut self, ctl: &[u8], st: &mut Sockets) -> OutValue {
        let hdr = sys::cmsg_len(0);
        let mut out = Vec::new();
        let mut i = 1u32;
        let read_len = |o: usize| -> usize {
            let mut b = [0u8; 8];
            b.copy_from_slice(&ctl[o..o + 8]);
            usize::try_from(u64::from_ne_bytes(b)).unwrap_or(usize::MAX)
        };
        let align = |n: usize| (n + 7) & !7;
        // CMSG_FIRSTHDR
        let mut cur = (ctl.len() >= hdr).then_some(0);
        while let Some(o) = cur {
            if self.err.is_some() {
                break;
            }
            let len = read_len(o);
            let level = i32::from_ne_bytes([ctl[o + 8], ctl[o + 9], ctl[o + 10], ctl[o + 11]]);
            let ty = i32::from_ne_bytes([ctl[o + 12], ctl[o + 13], ctl[o + 14], ctl[o + 15]]);
            let elem = self.with_key(&format!("element #{i}"), |s| {
                let data = s.with_key("data", |s| {
                    let Some(entry) = ancillary(level, ty) else {
                        s.error(format!("cmsghdr with level {level} and type {ty} not supported"));
                        return OutValue::Null;
                    };
                    if sys::cmsg_len(entry.size) > len {
                        s.error(format!(
                            "the cmsghdr structure is unexpectedly small; expected a length of at least {}, but got {len}",
                            sys::cmsg_len(entry.size)
                        ));
                        return OutValue::Null;
                    }
                    let end = (o + len).min(ctl.len());
                    let data = &ctl[(o + hdr).min(end)..end];
                    match entry.kind {
                        AncKind::Pktinfo => Self::pktinfo(data),
                        AncKind::Int => OutValue::Int(i32::from_ne_bytes([data[0], data[1], data[2], data[3]]).into()),
                        AncKind::Ucred => OutValue::assoc(vec![
                            ("pid", OutValue::Int(i32::from_ne_bytes([data[0], data[1], data[2], data[3]]).into())),
                            ("uid", OutValue::Int(u32::from_ne_bytes([data[4], data[5], data[6], data[7]]).into())),
                            ("gid", OutValue::Int(u32::from_ne_bytes([data[8], data[9], data[10], data[11]]).into())),
                        ]),
                        AncKind::Rights => s.fd_array(data, st),
                    }
                });
                OutValue::assoc(vec![
                    ("level", OutValue::Int(level.into())),
                    ("type", OutValue::Int(ty.into())),
                    ("data", data),
                ])
            });
            out.push((Key::Int(i64::from(i - 1)), elem));
            i += 1;
            // CMSG_NXTHDR
            cur = if len < hdr {
                None
            } else {
                let next = o + align(len);
                if next + hdr > ctl.len() || next + align(read_len(next)) > ctl.len() {
                    None
                } else {
                    Some(next)
                }
            };
        }
        OutValue::Array(out)
    }
}

impl Sockets {
    /// `socket_sendmsg(Socket $socket, array $message, int $flags = 0)`.
    pub fn sendmsg(&mut self, socket: &mut Socket, message: &Value, flags: i64) -> SocketResult<i64> {
        const F: &str = "socket_sendmsg";
        let flags = i32::try_from(flags).map_err(|_| {
            crate::arg_value_error(F, 3, "flags", &format!("must be between {} and {}", i32::MIN, i32::MAX))
        })?;
        crate::ensure_open(F, socket)?;
        let mut msg = SendMsg::default();
        let (err, thrown) = {
            let mut s = Ser { keys: vec!["msghdr".into()], err: None, thrown: None, st: self, sock: socket, fill_sockaddr: true };
            s.aggregate(message, &[("name", false), ("iov", false), ("control", false)], |s, k, v| match k {
                "name" => msg.name = s.sockaddr(v),
                "iov" => {
                    let Value::Array(t) = v else {
                        s.error("expected an array here");
                        return;
                    };
                    for (i, (_, e)) in t.iter().enumerate() {
                        let b = s.with_key(&format!("element #{}", i + 1), |s| s.string(e));
                        if s.failed() {
                            break;
                        }
                        msg.iov.push(b);
                    }
                }
                _ => msg.control = s.control_array(v),
            });
            (s.err, s.thrown)
        };
        let msg = finish(self, err, thrown, msg)?;
        match sys::sendmsg(socket.fd(), msg.name.as_ref(), &msg.iov, &msg.control, flags) {
            Ok(n) => Ok(i64::try_from(n).unwrap_or(i64::MAX)),
            Err(e) => {
                self.sock_error(socket, "Error in sendmsg", e);
                Err(SocketError::Failed)
            }
        }
    }

    /// `socket_recvmsg(Socket $socket, array &$message, int $flags = 0)`.
    ///
    /// On success returns the byte count and the new value of `$message`
    /// ([`OutValue::False`] when converting the received data failed — a
    /// warning is then queued but the count is still returned).
    pub fn recvmsg(&mut self, socket: &mut Socket, message: &Value, flags: i64) -> SocketResult<(i64, OutValue)> {
        const F: &str = "socket_recvmsg";
        let flags = i32::try_from(flags).map_err(|_| {
            crate::arg_value_error(F, 3, "flags", &format!("must be between {} and {}", i32::MIN, i32::MAX))
        })?;
        crate::ensure_open(F, socket)?;
        let mut spec = RecvSpec { name_len: None, iov: Vec::new(), controllen: 0 };
        let (err, thrown) = {
            let mut s = Ser { keys: vec!["msghdr".into()], err: None, thrown: None, st: self, sock: socket, fill_sockaddr: false };
            s.aggregate(message, &[("name", false), ("buffer_size", false), ("controllen", true)], |s, k, v| match k {
                "name" => spec.name_len = s.sockaddr(v).map(|a| a.raw().1),
                "buffer_size" => {
                    let l = s.integer(v);
                    if s.failed() {
                        return;
                    }
                    if !(0..=MAX_USER_BUFF_SIZE).contains(&l) {
                        s.error(format!("the buffer size must be between 1 and {MAX_USER_BUFF_SIZE}; given {l}"));
                        return;
                    }
                    spec.iov = vec![usize::try_from(l).unwrap_or(0)];
                }
                _ => {
                    let len = s.uint32(v);
                    if s.failed() {
                        return;
                    }
                    if len == 0 {
                        s.error("controllen cannot be 0");
                        return;
                    }
                    spec.controllen = len as usize;
                }
            });
            (s.err, s.thrown)
        };
        let mut spec = finish(self, err, thrown, spec)?;
        if spec.iov.is_empty() {
            spec.iov = vec![DEFAULT_BUFF_SIZE];
        }
        let r = match sys::recvmsg(socket.fd(), spec.name_len, &spec.iov, spec.controllen, flags) {
            Ok(r) => r,
            Err(e) => {
                self.last_error = e;
                self.warn(format!("Error in recvmsg [{e}]: {}", crate::sockets_strerror(e)));
                return Err(SocketError::Failed);
            }
        };
        let mut res = Res { keys: vec!["msghdr".into()], err: None };
        let name = res.with_key("name", |s| r.name.as_ref().map_or(OutValue::Null, |a| s.sockaddr(a)));
        let control = if res.err.is_none() {
            res.with_key("control", |s| s.control(&r.control, self))
        } else {
            OutValue::Null
        };
        let mut iov = Vec::new();
        let mut left = r.len;
        for (i, b) in r.iov.iter().enumerate() {
            if left == 0 {
                break;
            }
            let n = b.len().min(left);
            iov.push((Key::Int(i64::try_from(i).unwrap_or(0)), OutValue::Str(b[..n].to_vec())));
            left -= n;
        }
        let count = i64::try_from(r.len).unwrap_or(i64::MAX);
        if let Some(e) = res.err {
            self.warn(e);
            return Ok((count, OutValue::False));
        }
        Ok((
            count,
            OutValue::assoc(vec![
                ("name", name),
                ("control", control),
                ("iov", OutValue::Array(iov)),
                ("flags", OutValue::Int(r.flags.into())),
            ]),
        ))
    }

    /// `php_do_setsockopt_ipv6_rfc3542()` for `IPV6_PKTINFO`.
    pub(crate) fn set_ipv6_pktinfo(&mut self, socket: &mut Socket, value: &Value) -> SocketResult<()> {
        let (bytes, err, thrown) = {
            let mut s = Ser { keys: vec!["in6_pktinfo".into()], err: None, thrown: None, st: self, sock: socket, fill_sockaddr: true };
            let b = s.pktinfo(value);
            (b, s.err, s.thrown)
        };
        let bytes = finish(self, err, thrown, bytes)?;
        sys::setsockopt(socket.fd(), libc::IPPROTO_IPV6, libc::IPV6_PKTINFO, &bytes).map_err(|e| {
            self.sock_error(socket, "Unable to set socket option", e);
            SocketError::Failed
        })
    }

    /// `php_do_getsockopt_ipv6_rfc3542()` for `IPV6_PKTINFO`.
    pub(crate) fn get_ipv6_pktinfo(&mut self, socket: &mut Socket) -> SocketResult<OutValue> {
        let mut buf = [0u8; PKTINFO_SIZE];
        match sys::getsockopt(socket.fd(), libc::IPPROTO_IPV6, libc::IPV6_PKTINFO, &mut buf) {
            Ok(_) => Ok(Res::pktinfo(&buf)),
            Err(e) => {
                self.sock_error(socket, "unable to get socket option", e);
                Err(SocketError::Failed)
            }
        }
    }
}

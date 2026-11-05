//! `socket_get_option()`/`socket_set_option()` (and the aliases
//! `socket_getopt()`/`socket_setopt()`), including `multicast.c`.

use crate::sys::{self, SockAddr};
use crate::value::{OutValue, Value};
use crate::{
    arg_type_error, arg_value_error, ensure_open, Socket, SocketError, SocketResult, Sockets,
};

const SO_LINGER: i64 = libc::SO_LINGER as i64;
const SO_RCVTIMEO: i64 = libc::SO_RCVTIMEO as i64;
const SO_SNDTIMEO: i64 = libc::SO_SNDTIMEO as i64;
const SO_BINDTODEVICE: i64 = libc::SO_BINDTODEVICE as i64;
const SO_ATTACH_REUSEPORT_CBPF: i64 = libc::SO_ATTACH_REUSEPORT_CBPF as i64;
const SO_DETACH_REUSEPORT_BPF: i32 = 68;
const SO_MEMINFO: i64 = libc::SO_MEMINFO as i64;
const UDP_SEGMENT: i64 = 103;
const TCP_CONGESTION: i64 = libc::TCP_CONGESTION as i64;
const TCP_USER_TIMEOUT: i64 = libc::TCP_USER_TIMEOUT as i64;
const SKF_AD_CPU: u32 = 36;
const SKF_AD_QUEUE: u32 = 8;

const MCAST_JOIN_GROUP: i64 = libc::MCAST_JOIN_GROUP as i64;
const MCAST_BLOCK_SOURCE: i64 = libc::MCAST_BLOCK_SOURCE as i64;
const MCAST_UNBLOCK_SOURCE: i64 = libc::MCAST_UNBLOCK_SOURCE as i64;
const MCAST_LEAVE_GROUP: i64 = libc::MCAST_LEAVE_GROUP as i64;
const MCAST_JOIN_SOURCE_GROUP: i64 = libc::MCAST_JOIN_SOURCE_GROUP as i64;
const MCAST_LEAVE_SOURCE_GROUP: i64 = libc::MCAST_LEAVE_SOURCE_GROUP as i64;

fn level_is(level: i64, l: i32) -> bool {
    level == i64::from(l)
}

#[allow(clippy::cast_possible_truncation)]
fn c_int(v: i64) -> i32 {
    v as i32
}

/// Outcome of a level-specific handler (`HANDLE_SUBCALL`).
enum Sub {
    Done(SocketResult<()>),
    NotHandled,
}

impl Sockets {
    fn retrieve_err(&mut self, socket: &mut Socket, e: i32) -> SocketError {
        self.sock_error(socket, "Unable to retrieve socket option", e);
        SocketError::Failed
    }

    /// `socket_get_option(Socket $socket, int $level, int $option)`;
    /// `func` is the name the script called (`socket_get_option` or the
    /// alias `socket_getopt`).
    pub fn get_option(
        &mut self,
        func: &str,
        socket: &mut Socket,
        level: i64,
        option: i64,
    ) -> SocketResult<OutValue> {
        ensure_open(func, socket)?;
        let fd = socket.fd;
        let (lvl, opt) = (c_int(level), c_int(option));
        if level_is(level, libc::IPPROTO_IP) && option == i64::from(libc::IP_MULTICAST_IF) {
            let mut b = [0u8; 4];
            sys::getsockopt(fd, lvl, opt, &mut b).map_err(|e| self.retrieve_err(socket, e))?;
            return self.addr4_to_if_index(b).map(|i| OutValue::Int(i.into()));
        }
        if level_is(level, libc::IPPROTO_IPV6) && option == i64::from(libc::IPV6_PKTINFO) {
            return self.get_ipv6_pktinfo(socket);
        }
        if level_is(level, libc::IPPROTO_TCP) && option == TCP_CONGESTION {
            let mut name = [0u8; 16];
            sys::getsockopt(fd, lvl, opt, &mut name).map_err(|e| self.retrieve_err(socket, e))?;
            let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
            return Ok(OutValue::assoc(vec![(
                "name",
                OutValue::Str(name[..end].to_vec()),
            )]));
        }
        if level_is(level, libc::SOL_SOCKET) {
            match option {
                SO_LINGER => {
                    let mut b = [0u8; 8];
                    sys::getsockopt(fd, lvl, opt, &mut b)
                        .map_err(|e| self.retrieve_err(socket, e))?;
                    return Ok(OutValue::assoc(vec![
                        (
                            "l_onoff",
                            OutValue::Int(i32::from_ne_bytes([b[0], b[1], b[2], b[3]]).into()),
                        ),
                        (
                            "l_linger",
                            OutValue::Int(i32::from_ne_bytes([b[4], b[5], b[6], b[7]]).into()),
                        ),
                    ]));
                }
                SO_RCVTIMEO | SO_SNDTIMEO => {
                    let mut b = [0u8; 16];
                    sys::getsockopt(fd, lvl, opt, &mut b)
                        .map_err(|e| self.retrieve_err(socket, e))?;
                    let word = |o: usize| {
                        let mut w = [0u8; 8];
                        w.copy_from_slice(&b[o..o + 8]);
                        i64::from_ne_bytes(w)
                    };
                    return Ok(OutValue::assoc(vec![
                        ("sec", OutValue::Int(word(0))),
                        ("usec", OutValue::Int(word(8))),
                    ]));
                }
                SO_MEMINFO => {
                    let mut b = [0u8; 36];
                    let n = sys::getsockopt(fd, lvl, opt, &mut b)
                        .map_err(|e| self.retrieve_err(socket, e))?;
                    if n != b.len() {
                        self.warn("Unable to retrieve all socket meminfo data");
                        return Err(SocketError::Failed);
                    }
                    let names = [
                        "rmem_alloc",
                        "rcvbuf",
                        "wmem_alloc",
                        "sndbuf",
                        "fwd_alloc",
                        "wmem_queued",
                        "optmem",
                        "backlog",
                        "drops",
                    ];
                    return Ok(OutValue::assoc(
                        names
                            .iter()
                            .enumerate()
                            .map(|(i, n)| {
                                let v = u32::from_ne_bytes([
                                    b[i * 4],
                                    b[i * 4 + 1],
                                    b[i * 4 + 2],
                                    b[i * 4 + 3],
                                ]);
                                (*n, OutValue::Int(v.into()))
                            })
                            .collect(),
                    ));
                }
                _ => {}
            }
        }
        let mut b = [0u8; 4];
        let n = sys::getsockopt(fd, lvl, opt, &mut b).map_err(|e| self.retrieve_err(socket, e))?;
        if n == 1 {
            return Ok(OutValue::Int(b[0].into()));
        }
        Ok(OutValue::Int(i32::from_ne_bytes(b).into()))
    }

    fn set_err(&mut self, socket: &mut Socket, e: i32) -> SocketError {
        self.sock_error(socket, "Unable to set socket option", e);
        SocketError::Failed
    }

    /// `socket_set_option(Socket $socket, int $level, int $option, $value)`;
    /// `func` is `socket_set_option` or the alias `socket_setopt`.
    #[allow(clippy::too_many_lines)]
    pub fn set_option(
        &mut self,
        func: &str,
        socket: &mut Socket,
        level: i64,
        option: i64,
        value: &Value,
    ) -> SocketResult<()> {
        ensure_open(func, socket)?;
        sys::set_errno(0);
        let fd = socket.fd;
        let (lvl, mut opt) = (c_int(level), c_int(option));
        let mut skip_switch = false;
        if level_is(level, libc::IPPROTO_IP) {
            match self.ip_mcast(func, socket, option, value) {
                Sub::Done(r) => return r,
                Sub::NotHandled => skip_switch = true,
            }
        } else if level_is(level, libc::IPPROTO_IPV6) {
            match self.ipv6_mcast(func, socket, option, value) {
                Sub::Done(r) => return r,
                Sub::NotHandled => {
                    if option == i64::from(libc::IPV6_PKTINFO) {
                        return self.set_ipv6_pktinfo(socket, value);
                    }
                    skip_switch = true;
                }
            }
        }
        if !skip_switch && level_is(level, libc::IPPROTO_TCP) {
            if option == TCP_CONGESTION {
                let Value::Str(s) = value else {
                    return Err(arg_type_error(func, 4, "value", &format!(
                        "must be of type string when argument #3 ($option) is TCP_CONGESTION, {} given",
                        value.value_name()
                    )));
                };
                return sys::setsockopt(fd, lvl, opt, s).map_err(|e| self.set_err(socket, e));
            }
            if option == TCP_USER_TIMEOUT {
                let t = self.long_of(value);
                let t = u32::try_from(t).map_err(|_| {
                    arg_value_error(
                        func,
                        4,
                        "value",
                        &format!("must be between 0 and {}", u32::MAX),
                    )
                })?;
                return sys::setsockopt(fd, lvl, opt, &t.to_ne_bytes())
                    .map_err(|e| self.set_err(socket, e));
            }
        }
        let bytes: Vec<u8> = match option {
            _ if skip_switch => c_int(self.long_of(value)).to_ne_bytes().to_vec(),
            SO_LINGER => {
                let t = Self::option_table(func, value, "SO_LINGER")?;
                let onoff = crate::value::find(t, "l_onoff").ok_or_else(|| {
                    arg_value_error(func, 4, "value", "must have key \"l_onoff\"")
                })?;
                let linger = crate::value::find(t, "l_linger").ok_or_else(|| {
                    arg_value_error(func, 4, "value", "must have key \"l_linger\"")
                })?;
                let (on, li) = (self.long_of(onoff), self.long_of(linger));
                if !(0..=65535).contains(&on) {
                    return Err(arg_value_error(
                        func,
                        4,
                        "value",
                        "\"l_onoff\" must be between 0 and 65535",
                    ));
                }
                if !(0..=65535).contains(&li) {
                    return Err(arg_value_error(
                        func,
                        4,
                        "value",
                        "\"l_linger\" must be between 0 and 65535",
                    ));
                }
                let mut b = c_int(on).to_ne_bytes().to_vec();
                b.extend_from_slice(&c_int(li).to_ne_bytes());
                b
            }
            SO_RCVTIMEO | SO_SNDTIMEO => {
                let name = if option == SO_RCVTIMEO {
                    "SO_RCVTIMEO"
                } else {
                    "SO_SNDTIMEO"
                };
                let t = Self::option_table(func, value, name)?;
                let sec = crate::value::find(t, "sec")
                    .ok_or_else(|| arg_value_error(func, 4, "value", "must have key \"sec\""))?;
                let usec = crate::value::find(t, "usec")
                    .ok_or_else(|| arg_value_error(func, 4, "value", "must have key \"usec\""))?;
                let (sec, usec) = (self.long_of(sec), self.long_of(usec));
                let mut b = sec.to_ne_bytes().to_vec();
                b.extend_from_slice(&usec.to_ne_bytes());
                b
            }
            SO_BINDTODEVICE => {
                let Value::Str(s) = value else {
                    return Err(arg_type_error(func, 4, "value", &format!(
                        "must be of type string when argument #3 ($option) is SO_BINDTODEVICE, {} given",
                        value.value_name()
                    )));
                };
                s.clone()
            }
            SO_ATTACH_REUSEPORT_CBPF => {
                if !level_is(level, libc::SOL_SOCKET) {
                    self.warn("Invalid level");
                    return Err(SocketError::Failed);
                }
                let Value::Int(v) = value else {
                    return Err(arg_type_error(func, 4, "value", &format!(
                        "must be of type int when argument #3 ($option) is SO_ATTACH_REUSEPORT_CBPF, {} given",
                        value.value_name()
                    )));
                };
                if *v == 0 {
                    opt = SO_DETACH_REUSEPORT_BPF;
                    1i32.to_ne_bytes().to_vec()
                } else {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let k = *v as u32;
                    if k != SKF_AD_CPU && k != SKF_AD_QUEUE {
                        self.warn("Unsupported CBPF filter");
                        return Err(SocketError::Failed);
                    }
                    return sys::attach_reuseport_cbpf(fd, k).map_err(|e| self.set_err(socket, e));
                }
            }
            UDP_SEGMENT => {
                let ov = c_int(self.long_of(value));
                if !(0..=65535).contains(&ov) {
                    return Err(arg_value_error(
                        func,
                        4,
                        "value",
                        "must be between 0 and 65535",
                    ));
                }
                ov.to_ne_bytes().to_vec()
            }
            _ => c_int(self.long_of(value)).to_ne_bytes().to_vec(),
        };
        sys::setsockopt(fd, lvl, opt, &bytes).map_err(|e| self.set_err(socket, e))
    }

    /// The array (or object properties) of an option value.
    fn option_table<'v>(
        func: &str,
        value: &'v Value,
        name: &str,
    ) -> SocketResult<&'v [(crate::Key, Value)]> {
        match value {
            Value::Array(t) | Value::Object { props: t, .. } => Ok(t),
            _ => Err(arg_type_error(
                func,
                4,
                "value",
                &format!(
                    "must be of type array when argument #3 ($option) is {name}, {} given",
                    value.value_name()
                ),
            )),
        }
    }

    // --------------------------------------------------------- multicast

    /// `php_add4_to_if_index()`.
    fn addr4_to_if_index(&mut self, ip: [u8; 4]) -> SocketResult<u32> {
        if ip == [0; 4] {
            return Ok(0);
        }
        match sys::addr4_to_if_index(ip) {
            Ok(Some(i)) => Ok(i),
            Ok(None) => {
                self.warn(format!(
                    "The interface with IP address {} was not found",
                    sys::ntop4(ip)
                ));
                Err(SocketError::Failed)
            }
            Err(e) => {
                self.warn(format!("Failed obtaining interfaces list: error {e}"));
                Err(SocketError::Failed)
            }
        }
    }

    /// `php_if_index_to_addr4()`.
    fn if_index_to_addr4(&mut self, socket: &Socket, index: u32) -> SocketResult<[u8; 4]> {
        if index == 0 {
            return Ok([0; 4]);
        }
        sys::if_index_to_addr4(socket.fd, index).map_err(|e| {
            self.warn(format!(
                "Failed obtaining address for interface {index}: error {e}"
            ));
            SocketError::Failed
        })
    }

    /// `php_get_if_index_from_zval()`.
    fn if_index_from_value(&mut self, value: &Value) -> SocketResult<u32> {
        if let Value::Int(i) = value {
            return u32::try_from(*i).map_err(|_| {
                SocketError::ValueError(format!("Index must be between 0 and {}", u32::MAX))
            });
        }
        let (s, warn) = value.to_php_string().map_err(|e| SocketError::Error(e.0))?;
        if warn {
            self.warn("Array to string conversion");
        }
        self.string_to_if_index(&s).ok_or(SocketError::Failed)
    }

    /// `php_get_address_from_array()` + `php_set_inet46_addr()`.
    fn mcast_address(
        &mut self,
        socket: &mut Socket,
        t: &[(crate::Key, Value)],
        key: &str,
    ) -> SocketResult<SockAddr> {
        let v = crate::value::find(t, key)
            .ok_or_else(|| SocketError::ValueError(format!("No key \"{key}\" passed in optval")))?;
        let (s, warn) = v.to_php_string().map_err(|e| SocketError::Error(e.0))?;
        if warn {
            self.warn("Array to string conversion");
        }
        match socket.family {
            libc::AF_INET => self
                .inet_addr(socket, &s)
                .map(|ip| SockAddr::inet(ip, 0))
                .ok_or(SocketError::Failed),
            libc::AF_INET6 => self
                .inet6_addr(socket, &s)
                .map(|(ip, scope)| SockAddr::inet6(ip, 0, 0, scope.unwrap_or(0)))
                .ok_or(SocketError::Failed),
            _ => Err(SocketError::ValueError(
                "IP address used in the context of an unexpected type of socket".into(),
            )),
        }
    }

    /// `php_do_mcast_opt()` (RFC 3678 API: `group_req`/`group_source_req`).
    fn mcast_opt(
        &mut self,
        func: &str,
        socket: &mut Socket,
        level: i32,
        option: i64,
        value: &Value,
    ) -> SocketResult<()> {
        let source_op = !matches!(option, MCAST_JOIN_GROUP | MCAST_LEAVE_GROUP);
        let (Value::Array(t) | Value::Object { props: t, .. }) = value else {
            {
                let name = if source_op {
                    "MCAST_LEAVE_SOURCE_GROUP"
                } else {
                    "MCAST_LEAVE_GROUP"
                };
                return Err(arg_type_error(
                    func,
                    4,
                    "value",
                    &format!(
                        "must be of type array when argument #3 ($option) is {name}, {} given",
                        value.value_name()
                    ),
                ));
            }
        };
        let group = self.mcast_address(socket, t, "group")?;
        let source = if source_op {
            Some(self.mcast_address(socket, t, "source")?)
        } else {
            None
        };
        let if_index = match crate::value::find(t, "interface") {
            None => 0,
            Some(v) => self.if_index_from_value(v)?,
        };
        let ss = std::mem::size_of::<libc::sockaddr_storage>();
        let mut req = vec![0u8; 8 + ss * if source_op { 2 } else { 1 }];
        req[..4].copy_from_slice(&if_index.to_ne_bytes());
        let (g, glen) = group.raw();
        req[8..8 + glen].copy_from_slice(&g[..glen]);
        if let Some(src) = source {
            let (s, slen) = src.raw();
            req[8 + ss..8 + ss + slen].copy_from_slice(&s[..slen]);
        }
        sys::setsockopt(socket.fd, level, c_int(option), &req).map_err(|e| self.set_err(socket, e))
    }

    fn is_mcast_group_opt(option: i64) -> bool {
        matches!(
            option,
            MCAST_JOIN_GROUP
                | MCAST_LEAVE_GROUP
                | MCAST_BLOCK_SOURCE
                | MCAST_UNBLOCK_SOURCE
                | MCAST_JOIN_SOURCE_GROUP
                | MCAST_LEAVE_SOURCE_GROUP
        )
    }

    /// `php_do_setsockopt_ip_mcast()`.
    fn ip_mcast(&mut self, func: &str, socket: &mut Socket, option: i64, value: &Value) -> Sub {
        let level = libc::IPPROTO_IP;
        if Self::is_mcast_group_opt(option) {
            return Sub::Done(self.mcast_opt(func, socket, level, option, value));
        }
        let bytes: Vec<u8> = match c_int(option) {
            _ if option != i64::from(c_int(option)) => return Sub::NotHandled,
            libc::IP_MULTICAST_IF => {
                let r = self
                    .if_index_from_value(value)
                    .and_then(|i| self.if_index_to_addr4(socket, i));
                match r {
                    Ok(a) => a.to_vec(),
                    Err(e) => return Sub::Done(Err(e)),
                }
            }
            libc::IP_MULTICAST_LOOP => vec![u8::from(value.is_true())],
            libc::IP_MULTICAST_TTL => {
                let v = value.to_long();
                if !(0..=255).contains(&v) {
                    return Sub::Done(Err(arg_value_error(
                        func,
                        4,
                        "value",
                        "must be between 0 and 255",
                    )));
                }
                vec![u8::try_from(v).unwrap_or(0)]
            }
            _ => return Sub::NotHandled,
        };
        Sub::Done(
            sys::setsockopt(socket.fd, level, c_int(option), &bytes)
                .map_err(|e| self.set_err(socket, e)),
        )
    }

    /// `php_do_setsockopt_ipv6_mcast()`.
    fn ipv6_mcast(&mut self, func: &str, socket: &mut Socket, option: i64, value: &Value) -> Sub {
        let level = libc::IPPROTO_IPV6;
        if Self::is_mcast_group_opt(option) {
            return Sub::Done(self.mcast_opt(func, socket, level, option, value));
        }
        let bytes: Vec<u8> = match c_int(option) {
            _ if option != i64::from(c_int(option)) => return Sub::NotHandled,
            libc::IPV6_MULTICAST_IF => match self.if_index_from_value(value) {
                Ok(i) => i.to_ne_bytes().to_vec(),
                Err(e) => return Sub::Done(Err(e)),
            },
            libc::IPV6_MULTICAST_LOOP => i32::from(value.is_true()).to_ne_bytes().to_vec(),
            libc::IPV6_MULTICAST_HOPS => {
                let v = value.to_long();
                if !(-1..=255).contains(&v) {
                    return Sub::Done(Err(arg_value_error(
                        func,
                        4,
                        "value",
                        "must be between -1 and 255",
                    )));
                }
                c_int(v).to_ne_bytes().to_vec()
            }
            _ => return Sub::NotHandled,
        };
        Sub::Done(
            sys::setsockopt(socket.fd, level, c_int(option), &bytes)
                .map_err(|e| self.set_err(socket, e)),
        )
    }
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Each pool's listening socket (ref: `sapi/fpm/fpm/fpm_sockets.c`,
//! `fpm_unix.c` `fpm_unix_set_socket_permissions()`): `listen = port`,
//! `host:port`, `[v6]:port` or a Unix socket path, with
//! `listen.backlog`, `listen.owner`/`group`/`mode`.

use super::conf::Pool;
use super::log;
use super::os as sys;
use std::os::fd::RawFd;

/// `fpm_sockets_domain_from_address()`: a Unix socket unless the address
/// has a `:` or is all digits.
pub fn is_unix_address(addr: &str) -> bool {
    !(addr.contains(':') || (!addr.is_empty() && addr.bytes().all(|c| c.is_ascii_digit())))
}

/// `struct listening_socket_s`.
#[derive(Debug, Clone)]
struct Ls {
    key: String,
    fd: RawFd,
    unix: bool,
    refcount: u32,
}

thread_local! {
    /// `sockets_list`: the listening sockets by address.
    static SOCKETS: std::cell::RefCell<Vec<Ls>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// `FPM_ENV_SOCKET_SET_SIZE`.
const SOCKET_SET_SIZE: usize = 128;
/// `FPM_ENV_SOCKET_SET_MAX`.
const SOCKET_SET_MAX: usize = 256;

/// `fpm_sockets_get_env_name()`.
fn env_name(i: usize) -> String {
    if i == 0 {
        "FPM_SOCKETS".into()
    } else {
        format!("FPM_SOCKETS_{i}")
    }
}

/// `fpm_sockets_init_main()`'s import of the sockets a reloading master
/// passed on (`FPM_SOCKETS=addr=fd,addr=fd`).
pub fn import_inherited() {
    for i in 0..SOCKET_SET_MAX {
        let Ok(inherited) = std::env::var(env_name(i)) else {
            break;
        };
        for item in inherited.split(',').filter(|s| !s.is_empty()) {
            let Some((path, fd)) = item.split_once('=') else {
                continue;
            };
            let path: String = path.chars().take(255).collect();
            let fd = i32::try_from(port_value(fd)).unwrap_or(0);
            log::notice(
                "fpm_sockets_init_main",
                0,
                &format!("using inherited socket fd={fd}, \"{path}\""),
            );
            SOCKETS.with(|l| {
                l.borrow_mut().push(Ls {
                    unix: is_unix_address(&path),
                    key: path,
                    fd,
                    refcount: 0,
                });
            });
        }
    }
}

/// `fpm_sockets_get_listening_socket()`: the socket already listening on
/// `key` (inherited, or another pool's), else a new one.
fn get_listening(key: &str, unix: bool, create: impl FnOnce() -> Option<RawFd>) -> Option<RawFd> {
    let found = SOCKETS.with(|l| {
        l.borrow_mut().iter_mut().find(|s| s.key == key).map(|s| {
            s.refcount += 1;
            s.fd
        })
    });
    if found.is_some() {
        return found;
    }
    let fd = create()?;
    SOCKETS.with(|l| {
        l.borrow_mut().push(Ls {
            key: key.to_string(),
            fd,
            unix,
            refcount: 1,
        });
    });
    Some(fd)
}

/// `fpm_sockets_init_main()`'s end: inherited sockets no pool uses are
/// closed (a Unix socket's file removed).
pub fn close_unused() {
    SOCKETS.with(|l| {
        l.borrow_mut().retain(|s| {
            if s.refcount > 0 {
                return true;
            }
            sys::close(s.fd);
            if s.unix {
                let _ = std::fs::remove_file(&s.key);
            }
            false
        });
    });
}

/// `fpm_sockets_cleanup(FPM_CLEANUP_PARENT_EXEC)`: the sockets stay open
/// across `execvp()` and their addresses go into `FPM_SOCKETS`.
pub fn export_for_exec() {
    let list = SOCKETS.with(|l| l.borrow().clone());
    let mut sets: Vec<Vec<String>> = Vec::new();
    for (i, s) in list.iter().enumerate() {
        if i % SOCKET_SET_SIZE == 0 {
            sets.push(Vec::new());
        }
        sys::clear_cloexec(s.fd);
        if let Some(set) = sets.last_mut() {
            set.push(format!("{}={}", s.key, s.fd));
        }
    }
    for (i, set) in sets.iter().enumerate() {
        // SAFETY: the master is about to exec; no other thread reads the
        // environment.
        unsafe { std::env::set_var(env_name(i), set.join(",")) };
    }
    // SAFETY: as above.
    unsafe { std::env::remove_var(env_name(sets.len())) };
}

/// `fpm_sockets_cleanup(FPM_CLEANUP_PARENT_EXIT_MAIN)`: the master ends;
/// its Unix sockets' files go.
pub fn cleanup_exit() {
    SOCKETS.with(|l| {
        for s in l.borrow().iter() {
            sys::close(s.fd);
            if s.unix {
                let _ = std::fs::remove_file(&s.key);
            }
        }
    });
}

/// A pool's listening socket.
#[derive(Debug, Clone, Copy)]
pub struct Listening {
    /// The descriptor (inherited by the workers).
    pub fd: RawFd,
    /// Whether it is a Unix socket.
    pub unix: bool,
}

/// `atoi()` of a port.
fn port_value(s: &str) -> i64 {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().unwrap_or(0)
}

/// `fpm_socket_af_inet_socket_by_addr()`: the first address `addr`
/// resolves to that can be bound.
fn inet_by_addr(pool: &Pool, addr: &str, port: u16) -> Option<RawFd> {
    use std::net::ToSocketAddrs;
    let host = if addr.contains(':') && !addr.starts_with('[') {
        format!("[{addr}]:{port}")
    } else {
        format!("{addr}:{port}")
    };
    let addrs: Vec<std::net::SocketAddr> = match host.to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(e) => {
            log::error(
                "fpm_socket_af_inet_socket_by_addr",
                0,
                &format!("getaddrinfo: {e}"),
            );
            return None;
        }
    };
    let mut sock = None;
    for a in addrs {
        if sock.is_none() {
            // `fpm_sockets_hash_op()`'s key: `inet_ntop():port`.
            let key = format!("{}:{}", a.ip(), a.port());
            sock = get_listening(&key, false, || {
                match sys::inet_listen(&a, pool.listen_backlog) {
                    Ok(fd) => Some(fd),
                    Err(e) => {
                        log::syserror(
                            "fpm_sockets_new_listening_socket",
                            0,
                            &format!(
                                "unable to bind listening socket for address '{}'",
                                pool.listen_address.as_deref().unwrap_or("")
                            ),
                            &e,
                        );
                        None
                    }
                }
            });
            if sock.is_some() {
                log::debug(
                    "fpm_socket_af_inet_socket_by_addr",
                    0,
                    &format!("Found address for {addr}, socket opened on {}", a.ip()),
                );
            }
        } else {
            log::warning(
                "fpm_socket_af_inet_socket_by_addr",
                0,
                &format!("Found multiple addresses for {addr}, {} ignored", a.ip()),
            );
        }
    }
    sock
}

/// `fpm_socket_af_inet_listening_socket()`.
fn inet_listening(pool: &Pool, address: &str) -> Option<RawFd> {
    let (addr, port_str) = match address.rfind(':') {
        Some(i) => {
            let mut a = &address[..i];
            if a.starts_with('[') && a.ends_with(']') && a.len() >= 2 {
                a = &a[1..a.len() - 1];
            }
            (Some(a.to_string()), address[i + 1..].to_string())
        }
        None => (None, address.to_string()),
    };
    let port = port_value(&port_str);
    if !(1..=65535).contains(&port) {
        log::error(
            "fpm_socket_af_inet_listening_socket",
            0,
            &format!("invalid port value '{port_str}'"),
        );
        return None;
    }
    let port = u16::try_from(port).unwrap_or(0);
    if let Some(a) = addr {
        return inet_by_addr(pool, &a, port);
    }
    // ANYADDR: "::" covers both families; 0.0.0.0 when IPv6 is missing.
    let old = log::level();
    log::set_level(log::Level::Alert);
    let sock = inet_by_addr(pool, "::", port);
    log::set_level(old);
    sock.or_else(|| {
        log::notice(
            "fpm_socket_af_inet_listening_socket",
            0,
            "Failed implicitly binding to ::, retrying with 0.0.0.0",
        );
        inet_by_addr(pool, "0.0.0.0", port)
    })
}

/// `fpm_unix_resolve_socket_permissions()` + the socket's mode: the
/// owner and group to give a Unix socket.
fn socket_owner(pool: &Pool) -> Result<(Option<u32>, Option<u32>), ()> {
    let mut uid = None;
    let mut gid = None;
    if let Some(o) = pool.listen_owner.as_deref().filter(|s| !s.is_empty()) {
        let Some(u) = o
            .parse::<u32>()
            .ok()
            .or_else(|| sys::getpwnam(o).map(|p| p.0))
        else {
            log::error(
                "fpm_unix_resolve_socket_permissions",
                0,
                &format!("[pool {}] cannot get uid for user '{o}'", pool.name),
            );
            return Err(());
        };
        uid = Some(u);
    }
    if let Some(g) = pool.listen_group.as_deref().filter(|s| !s.is_empty()) {
        let Some(x) = g.parse::<u32>().ok().or_else(|| sys::getgrnam(g)) else {
            log::error(
                "fpm_unix_resolve_socket_permissions",
                0,
                &format!("[pool {}] cannot get gid for group '{g}'", pool.name),
            );
            return Err(());
        };
        gid = Some(x);
    }
    Ok((uid, gid))
}

/// `fpm_socket_af_unix_listening_socket()`.
fn unix_listening(pool: &Pool, path: &str) -> Option<RawFd> {
    let max = sys::sun_path_len();
    let mut bound = path.to_string();
    if path.len() >= max {
        // `strlcpy(sa_un.sun_path, ...)`: the path is cut to fit.
        let mut n = max - 1;
        while !path.is_char_boundary(n) {
            n -= 1;
        }
        let cut = path[..n].to_string();
        bound.clone_from(&cut);
        log::warning(
            "fpm_socket_af_unix_listening_socket",
            0,
            &format!(
                "[pool {}] cannot bind to UNIX socket '{path}' as path is too long (found length: {}, maximal length: {max}), trying cut socket path instead '{cut}'",
                pool.name,
                path.len()
            ),
        );
    }
    get_listening(&bound, true, || new_unix_listening(pool, &bound))
}

/// `fpm_sockets_new_listening_socket()` for a Unix socket.
fn new_unix_listening(pool: &Pool, path: &str) -> Option<RawFd> {
    if sys::unix_test_connect(path) {
        log::error(
            "fpm_sockets_new_listening_socket",
            0,
            &format!("Another FPM instance seems to already listen on {path}"),
        );
        return None;
    }
    let mode = pool
        .listen_mode
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|m| u32::from_str_radix(m, 8).ok())
        .unwrap_or(0o660);
    let Ok((uid, gid)) = socket_owner(pool) else {
        return None;
    };
    match sys::unix_listen(path, pool.listen_backlog, mode) {
        Ok(fd) => {
            if uid.is_some() || gid.is_some() {
                if let Err(e) = sys::chown(path, uid, gid) {
                    log::syserror(
                        "fpm_unix_set_socket_permissions",
                        0,
                        &format!("[pool {}] failed to chown() the socket '{path}'", pool.name),
                        &e,
                    );
                    sys::close(fd);
                    return None;
                }
            }
            Some(fd)
        }
        Err(e) => {
            log::syserror(
                "fpm_sockets_new_listening_socket",
                0,
                &format!("unable to bind listening socket for address '{path}'"),
                &e,
            );
            None
        }
    }
}

/// `fpm_sockets_init_main()` for one pool: its listening socket, or
/// `None` after logging why.
pub fn listen(pool: &Pool) -> Option<Listening> {
    let addr = pool.listen_address.clone().unwrap_or_default();
    if is_unix_address(&addr) {
        unix_listening(pool, &addr).map(|fd| Listening { fd, unix: true })
    } else {
        inet_listening(pool, &addr).map(|fd| Listening { fd, unix: false })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_domains() {
        assert!(!is_unix_address("9000"));
        assert!(!is_unix_address("127.0.0.1:9000"));
        assert!(!is_unix_address("[::1]:9000"));
        assert!(is_unix_address("/run/php.sock"));
        assert!(is_unix_address("php.sock"));
    }
}

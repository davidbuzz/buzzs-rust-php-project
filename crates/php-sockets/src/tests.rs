// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Tests on real loopback/UNIX sockets, with `.phpt` expectations ported
//! by hand and `/usr/bin/php` comparisons where behaviour is shared.

#![allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::items_after_statements
)]

use super::*;
use crate::value::{Key, OutValue, Value};
use std::process::Command;

const ORACLE: &str = "/usr/bin/php";

fn php(code: &str) -> Option<String> {
    if !std::path::Path::new(ORACLE).exists() {
        return None;
    }
    let out = Command::new(ORACLE)
        .args(["-d", "memory_limit=1G", "-r", code])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn ve(m: &str) -> SocketError {
    SocketError::ValueError(m.into())
}

fn te(m: &str) -> SocketError {
    SocketError::TypeError(m.into())
}

const AF_INET: i64 = libc::AF_INET as i64;
const AF_INET6: i64 = libc::AF_INET6 as i64;
const AF_UNIX: i64 = libc::AF_UNIX as i64;
const STREAM: i64 = libc::SOCK_STREAM as i64;
const DGRAM: i64 = libc::SOCK_DGRAM as i64;
const SOL_SOCKET: i64 = libc::SOL_SOCKET as i64;

fn tmp_path(name: &str) -> Vec<u8> {
    let p = std::env::temp_dir().join(format!("phprust-sock-{}-{name}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p.to_string_lossy().into_owned().into_bytes()
}

fn constant(name: &str) -> i64 {
    CONSTANTS
        .iter()
        .find(|c| c.0 == name)
        .map(|c| c.1)
        .expect(name)
}

/// A listening TCP socket on 127.0.0.1 and its port.
fn listener(st: &mut Sockets) -> (Socket, i64) {
    let mut l = st.create(AF_INET, STREAM, 0).unwrap();
    st.bind(&mut l, b"127.0.0.1", 0).unwrap();
    st.listen(&mut l, 5).unwrap();
    let port = st.getsockname(&mut l).unwrap().port.unwrap();
    (l, port)
}

#[test]
fn constants_table() {
    assert_eq!(CONSTANTS.len(), 262);
    let mut names: Vec<_> = CONSTANTS.iter().map(|c| c.0).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), CONSTANTS.len());
    assert_eq!(constant("SOCK_STREAM"), 1);
    assert_eq!(constant("MSG_WAITALL"), 256);
    assert_eq!(constant("SOCKET_ECONNREFUSED"), 111);
    assert_eq!(constant("PHP_NORMAL_READ"), PHP_NORMAL_READ);
    assert_eq!(constant("SOMAXCONN"), SOMAXCONN);
    let Some(out) = php(
        "foreach (get_defined_constants(true)['sockets'] as $k => $v) echo $k, '=', $v, \"\\n\";",
    ) else {
        return;
    };
    for line in out.lines() {
        let (k, v) = line.split_once('=').unwrap();
        if k == "MSG_EOF" {
            continue; // 8.3-only on Linux; master does not register it.
        }
        assert_eq!(constant(k), v.parse::<i64>().unwrap(), "{k}");
    }
}

#[test]
fn create_errors() {
    let mut st = Sockets::new();
    assert_eq!(
        st.create(4242, STREAM, 0).unwrap_err(),
        ve("socket_create(): Argument #1 ($domain) must be one of AF_UNIX, AF_PACKET, AF_INET6, or AF_INET")
    );
    assert_eq!(
        st.create(AF_INET, 11, 0).unwrap_err(),
        ve("socket_create(): Argument #2 ($type) must be one of SOCK_STREAM, SOCK_DGRAM, SOCK_SEQPACKET, SOCK_RAW, or SOCK_RDM optionally OR'ed with SOCK_CLOEXEC, SOCK_NONBLOCK")
    );
    assert!(st
        .create(AF_INET, STREAM | i64::from(libc::SOCK_NONBLOCK), 0)
        .is_ok());
    assert_eq!(
        st.create(AF_INET, STREAM, 17).unwrap_err(),
        SocketError::Failed
    );
    assert_eq!(
        st.take_warnings(),
        ["Unable to create socket [93]: Protocol not supported"]
    );
    assert_eq!(st.last_error(None).unwrap(), 93);
    st.clear_error(None).unwrap();
    assert_eq!(st.last_error(None).unwrap(), 0);
    assert_eq!(
        st.create_pair(4242, STREAM, 0).unwrap_err(),
        ve("socket_create_pair(): Argument #1 ($domain) must be one of AF_UNIX, AF_INET6, or AF_INET")
    );
    assert_eq!(
        st.create_pair(AF_INET, STREAM, 0).unwrap_err(),
        SocketError::Failed
    );
    assert_eq!(
        st.take_warnings(),
        ["Unable to create socket pair [95]: Operation not supported"]
    );
    assert_eq!(
        st.create_listen(65536, SOMAXCONN).unwrap_err(),
        ve("socket_create_listen(): Argument #1 ($port) must be between 0 and 65535")
    );
}

#[test]
fn strerror_and_codes() {
    let st = Sockets::new();
    assert_eq!(st.strerror(111).unwrap(), "Connection refused");
    assert_eq!(st.strerror(-10001).unwrap(), "Unknown host");
    assert_eq!(st.strerror(100_000).unwrap(), "Unknown error 100000");
    assert_eq!(
        st.strerror(i64::from(i32::MAX) + 1).unwrap_err(),
        ve("socket_strerror(): Argument #1 ($error_code) must be between -2147483648 and 2147483647")
    );
    let codes = [0, 1, 11, 98, 111, 115, -10001, -10002, -10003, -10004, 9999];
    let ours: Vec<String> = codes.iter().map(|&c| st.strerror(c).unwrap()).collect();
    let list = codes.map(|c| c.to_string()).join(",");
    if let Some(out) = php(&format!(
        "foreach ([{list}] as $c) echo socket_strerror($c), \"\\n\";"
    )) {
        assert_eq!(ours, out.lines().collect::<Vec<_>>());
    }
}

#[test]
fn closed_socket_errors() {
    let mut st = Sockets::new();
    let mut s = st.create(AF_INET, STREAM, 0).unwrap();
    st.close(&mut s).unwrap();
    assert!(s.is_closed());
    let closed = |f: &str| {
        SocketError::Error(format!(
            "{f}(): Argument #1 ($socket) has already been closed"
        ))
    };
    assert_eq!(st.close(&mut s).unwrap_err(), closed("socket_close"));
    assert_eq!(
        st.write(&mut s, b"x", None).unwrap_err(),
        closed("socket_write")
    );
    assert_eq!(
        st.last_error(Some(&s)).unwrap_err(),
        closed("socket_last_error")
    );
    assert_eq!(
        st.get_option("socket_getopt", &mut s, 1, 2).unwrap_err(),
        closed("socket_getopt")
    );
    assert_eq!(
        st.select([Some(&[Value::Socket { fd: -1 }]), None, None], Some(0), 0)
            .unwrap_err(),
        te("socket_select(): Argument #1 ($read) contains a closed socket")
    );
}

#[test]
fn tcp_loopback_roundtrip() {
    let mut st = Sockets::new();
    let (l, port) = listener(&mut st);
    let mut c = st.create(AF_INET, STREAM, 0).unwrap();
    assert_eq!(
        st.connect(&mut c, b"127.0.0.1", None).unwrap_err(),
        ve("socket_connect(): Argument #3 ($port) cannot be null when the socket type is AF_INET")
    );
    st.connect(&mut c, b"127.0.0.1", Some(port)).unwrap();
    let mut a = st.accept(&l).unwrap();
    assert_eq!(a.family(), libc::AF_INET);
    let peer = st.getpeername(&mut a).unwrap();
    assert_eq!(peer.address, b"127.0.0.1");
    let me = st.getsockname(&mut c).unwrap();
    assert_eq!(peer.port, me.port);
    assert_eq!(st.getpeername(&mut c).unwrap().port, Some(port));

    assert_eq!(st.write(&mut c, b"hello\nworld\rlast", None).unwrap(), 16);
    assert_eq!(st.write(&mut c, b"xyz", Some(1)).unwrap(), 1);
    assert_eq!(
        st.write(&mut c, b"xyz", Some(-1)).unwrap_err(),
        ve("socket_write(): Argument #3 ($length) must be greater than or equal to 0")
    );
    // PHP_NORMAL_READ stops after "\n" / "\r" (included).
    assert_eq!(st.read(&mut a, 100, PHP_NORMAL_READ).unwrap(), b"hello\n");
    assert_eq!(st.read(&mut a, 3, PHP_NORMAL_READ).unwrap(), b"wor");
    assert_eq!(st.read(&mut a, 100, PHP_NORMAL_READ).unwrap(), b"ld\r");
    assert_eq!(st.read(&mut a, 100, PHP_BINARY_READ).unwrap(), b"lastx");
    assert_eq!(
        st.read(&mut a, 0, PHP_BINARY_READ).unwrap_err(),
        SocketError::Failed
    );
    assert_eq!(
        st.read(&mut a, i64::MAX, PHP_BINARY_READ).unwrap_err(),
        SocketError::Failed
    );

    assert_eq!(st.send(&mut a, b"pong", 10, 0).unwrap(), 4);
    let r = st.recv(&mut c, 2, i64::from(libc::MSG_PEEK)).unwrap();
    assert_eq!(
        r,
        Recv {
            data: Some(b"po".to_vec()),
            result: Some(2)
        }
    );
    let r = st.recv(&mut c, 10, 0).unwrap();
    assert_eq!(r.data.as_deref(), Some(&b"pong"[..]));
    assert_eq!(st.recv(&mut c, 0, 0).unwrap_err(), SocketError::Failed);

    // Non-blocking empty reads are silent and set EAGAIN.
    st.set_nonblock(&mut c).unwrap();
    assert!(!c.is_blocking());
    assert_eq!(
        st.read(&mut c, 10, PHP_BINARY_READ).unwrap_err(),
        SocketError::Failed
    );
    assert!(st.take_warnings().is_empty());
    assert_eq!(st.last_error(Some(&c)).unwrap(), i64::from(libc::EAGAIN));
    // (PHP_NORMAL_READ on an empty non-blocking socket spins until data
    // arrives, exactly like php_read(); not exercised here.)
    st.set_block(&mut c).unwrap();

    st.shutdown(&mut a, 1).unwrap();
    assert_eq!(st.read(&mut c, 10, PHP_BINARY_READ).unwrap(), b"");
    let r = st.recv(&mut c, 10, 0).unwrap();
    assert_eq!(
        r,
        Recv {
            data: None,
            result: Some(0)
        }
    );
    assert_eq!(
        st.shutdown(&mut a, 3).unwrap_err(),
        ve("socket_shutdown(): Argument #2 ($mode) must be one of SHUT_RD, SHUT_WR or SHUT_RDWR")
    );
    assert!(!st.atmark(&mut c).unwrap());
    assert_eq!(c.export_protocol(), Some("tcp://"));
}

#[test]
fn tcp_errors_match_php() {
    let mut st = Sockets::new();
    // socket_read_params.phpt: reading a listening socket.
    let mut l = st.create_listen(0, SOMAXCONN).unwrap();
    assert_eq!(
        st.read(&mut l, 25, PHP_BINARY_READ).unwrap_err(),
        SocketError::Failed
    );
    assert_eq!(
        st.take_warnings(),
        ["unable to read from socket [107]: Transport endpoint is not connected"]
    );
    // socket_connect_params.phpt: connecting to 0.0.0.0:<listening port>
    // of a listening socket fails.
    let port = st.getsockname(&mut l).unwrap().port.unwrap();
    assert_eq!(
        st.connect(&mut l, b"0.0.0.0", Some(port)).unwrap_err(),
        SocketError::Failed
    );
    let w = st.take_warnings();
    assert!(w[0].starts_with("unable to connect ["), "{w:?}");
    // Refused connection.
    let mut c = st.create(AF_INET, STREAM, 0).unwrap();
    let mut tmp = st.create(AF_INET, STREAM, 0).unwrap();
    st.bind(&mut tmp, b"127.0.0.1", 0).unwrap();
    let free_port = st.getsockname(&mut tmp).unwrap().port.unwrap();
    drop(tmp);
    assert_eq!(
        st.connect(&mut c, b"127.0.0.1", Some(free_port))
            .unwrap_err(),
        SocketError::Failed
    );
    assert_eq!(
        st.take_warnings(),
        ["unable to connect [111]: Connection refused"]
    );
    assert_eq!(c.error(), 111);
    // Bind conflicts and invalid ports.
    let mut b = st.create(AF_INET, STREAM, 0).unwrap();
    assert_eq!(
        st.bind(&mut b, b"127.0.0.1", port).unwrap_err(),
        SocketError::Failed
    );
    assert_eq!(
        st.take_warnings(),
        ["Unable to bind address [98]: Address already in use"]
    );
    assert_eq!(
        st.bind(&mut b, b"127.0.0.1", 65536).unwrap_err(),
        ve("socket_bind(): Argument #3 ($port) must be between 0 and 65535")
    );
    // Host lookup failures use h_errno-based codes.
    assert_eq!(
        st.bind(&mut b, b"no-such-host.invalid", 0).unwrap_err(),
        SocketError::Failed
    );
    let w = st.take_warnings();
    assert!(w[0].starts_with("Host lookup failed [-1000"), "{w:?}");
    // socket_listen-wrongparams / accept on a non-listening socket.
    let mut d = st.create(AF_INET, DGRAM, 0).unwrap();
    assert_eq!(st.listen(&mut d, 0).unwrap_err(), SocketError::Failed);
    assert_eq!(
        st.take_warnings(),
        ["unable to listen on socket [95]: Operation not supported"]
    );
    assert_eq!(st.accept(&d).unwrap_err(), SocketError::Failed);
    assert_eq!(
        st.take_warnings(),
        ["unable to accept incoming connection [95]: Operation not supported"]
    );
}

#[test]
fn ipv6_loopback() {
    let mut st = Sockets::new();
    let Ok(mut l) = st.create(AF_INET6, STREAM, 0) else {
        return;
    };
    if st.bind(&mut l, b"::1", 0).is_err() {
        return; // no IPv6 loopback
    }
    st.listen(&mut l, 1).unwrap();
    let name = st.getsockname(&mut l).unwrap();
    assert_eq!(name.address, b"::1");
    let mut c = st.create(AF_INET6, STREAM, 0).unwrap();
    assert_eq!(
        st.connect(&mut c, b"::1", None).unwrap_err(),
        ve("socket_connect(): Argument #3 ($port) cannot be null when the socket type is AF_INET6")
    );
    st.connect(&mut c, b"::1", name.port).unwrap();
    let mut a = st.accept(&l).unwrap();
    assert_eq!(st.getpeername(&mut a).unwrap().address, b"::1");
    // UDP over IPv6 with recvfrom.
    let mut u1 = st.create(AF_INET6, DGRAM, 0).unwrap();
    st.bind(&mut u1, b"::1", 0).unwrap();
    let p1 = st.getsockname(&mut u1).unwrap().port;
    let mut u2 = st.create(AF_INET6, DGRAM, 0).unwrap();
    assert_eq!(st.sendto(&mut u2, b"v6", 2, 0, b"::1", p1).unwrap(), 2);
    assert_eq!(
        st.recvfrom(&mut u1, 10, 0, false).unwrap_err(),
        SocketError::ArgumentCountError("socket_recvfrom() expects exactly 6 arguments when argument #1 ($socket) is of type AF_INET or AF_INET6".into())
    );
    let r = st.recvfrom(&mut u1, 10, 0, true).unwrap();
    assert_eq!(
        (r.data.as_slice(), r.address.as_slice()),
        (&b"v6"[..], &b"::1"[..])
    );
}

#[test]
fn udp_ipv4_sendto_recvfrom() {
    // socket_sentto_recvfrom_ipv4_udp.phpt
    let mut st = Sockets::new();
    let mut s = st
        .create(AF_INET, DGRAM, i64::from(libc::IPPROTO_UDP))
        .unwrap();
    st.set_nonblock(&mut s).unwrap();
    st.bind(&mut s, b"127.0.0.1", 0).unwrap();
    let port = st.getsockname(&mut s).unwrap().port;
    let msg = b"Ping!";
    assert_eq!(
        st.sendto(&mut s, msg, 5, 0, b"127.0.0.1", None)
            .unwrap_err(),
        ve("socket_sendto(): Argument #6 ($port) cannot be null when the socket type is AF_INET")
    );
    assert_eq!(
        st.sendto(&mut s, msg, 5, 0, b"127.0.0.1", Some(65536))
            .unwrap_err(),
        ve("socket_sendto(): Argument #6 ($port) must be between 0 and 65535")
    );
    assert_eq!(
        st.sendto(&mut s, msg, -1, 0, b"127.0.0.1", port)
            .unwrap_err(),
        ve("socket_sendto(): Argument #3 ($length) must be greater than or equal to 0")
    );
    assert_eq!(
        st.sendto(&mut s, msg, 100, 0, b"127.0.0.1", port).unwrap(),
        5
    );
    let r = st.recvfrom(&mut s, 3, 0, true).unwrap();
    assert_eq!(r.data, b"Pin");
    assert_eq!(r.address, b"127.0.0.1");
    assert_eq!(r.port, port);
    assert_eq!(
        st.recvfrom(&mut s, 0, 0, true).unwrap_err(),
        SocketError::Failed
    );
    // Nothing left: EAGAIN is silent.
    assert_eq!(
        st.recvfrom(&mut s, 10, 0, true).unwrap_err(),
        SocketError::Failed
    );
    assert!(st.take_warnings().is_empty());
    assert_eq!(s.export_protocol(), Some("udp://"));
}

#[test]
fn unix_sockets() {
    // socket_sentto_recvfrom_unix.phpt / unixloop.phpt
    let mut st = Sockets::new();
    let path = tmp_path("dgram");
    let mut s = st.create(AF_UNIX, DGRAM, 0).unwrap();
    st.bind(&mut s, &path, 0).unwrap();
    let mut c = st.create(AF_UNIX, DGRAM, 0).unwrap();
    assert_eq!(st.sendto(&mut c, b"Ping!", 5, 0, &path, None).unwrap(), 5);
    let r = st.recvfrom(&mut s, 10, 0, false).unwrap();
    assert_eq!(
        r,
        RecvFrom {
            data: b"Ping!".to_vec(),
            address: Vec::new(),
            port: None,
            result: 5
        }
    );
    assert_eq!(
        st.getsockname(&mut s).unwrap(),
        SocketName {
            address: path.clone(),
            port: None
        }
    );
    let long = vec![b'x'; 108];
    assert_eq!(
        st.sendto(&mut c, b"x", 1, 0, &long, None).unwrap_err(),
        ve("socket_sendto(): Argument #5 ($address) must be less than 108")
    );
    assert_eq!(
        st.bind(&mut c, &long, 0).unwrap_err(),
        ve("socket_bind(): Argument #2 ($address) must be less than 108")
    );
    assert_eq!(
        st.connect(&mut c, &long, None).unwrap_err(),
        ve("socket_connect(): Argument #2 ($address) must be less than 108")
    );
    assert_eq!(s.export_protocol(), Some("udg://"));
    let _ = std::fs::remove_file(String::from_utf8(path).unwrap());

    // Abstract namespace (socket_abstract_path.phpt).
    let abs = b"\0phprust-abstract-test".to_vec();
    let mut a = st.create(AF_UNIX, STREAM, 0).unwrap();
    st.bind(&mut a, &abs, 0).unwrap();
    st.listen(&mut a, 1).unwrap();
    let mut b = st.create(AF_UNIX, STREAM, 0).unwrap();
    st.connect(&mut b, &abs, None).unwrap();
    let mut conn = st.accept(&a).unwrap();
    assert_eq!(st.write(&mut b, b"abs", None).unwrap(), 3);
    assert_eq!(st.read(&mut conn, 10, PHP_BINARY_READ).unwrap(), b"abs");
    // C-string view of the abstract name is empty.
    assert_eq!(st.getsockname(&mut a).unwrap().address, b"");
    assert_eq!(a.export_protocol(), Some("unix://"));

    // socket_create_pair.phpt
    let (mut p1, mut p2) = st.create_pair(AF_UNIX, STREAM, 0).unwrap();
    st.write(&mut p1, b"pair", None).unwrap();
    assert_eq!(st.read(&mut p2, 10, PHP_BINARY_READ).unwrap(), b"pair");
}

#[test]
fn select_semantics() {
    let mut st = Sockets::new();
    assert_eq!(
        st.select([None, None, None], Some(0), 0).unwrap_err(),
        ve("socket_select(): At least one array argument must be passed")
    );
    assert_eq!(
        st.select([Some(&[]), Some(&[]), None], Some(0), 0)
            .unwrap_err(),
        ve("socket_select(): At least one array argument must be passed")
    );
    assert_eq!(
        st.select([None, Some(&[Value::Int(1)]), None], Some(0), 0).unwrap_err(),
        te("socket_select(): Argument #2 ($write) must only have elements of type Socket, int given")
    );
    let (mut a, mut b) = st.create_pair(AF_UNIX, STREAM, 0).unwrap();
    let r = [Value::Socket { fd: a.fd() }, Value::Socket { fd: b.fd() }];
    let (n, sets) = st.select([Some(&r), None, None], Some(0), 0).unwrap();
    assert_eq!(n, 0);
    assert_eq!(sets[0], Some(vec![]));
    assert_eq!(sets[1], None);
    st.write(&mut a, b"x", None).unwrap();
    let w = [Value::Socket { fd: a.fd() }];
    let (n, sets) = st
        .select([Some(&r), Some(&w), Some(&[])], Some(0), 2_000_000)
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(sets[0], Some(vec![1]));
    assert_eq!(sets[1], Some(vec![0]));
    assert_eq!(sets[2], Some(vec![]));
    st.read(&mut b, 1, PHP_BINARY_READ).unwrap();
}

#[test]
fn socket_options() {
    let mut st = Sockets::new();
    let mut s = st
        .create(AF_INET, STREAM, i64::from(libc::IPPROTO_TCP))
        .unwrap();
    const F: &str = "socket_set_option";
    // socket_set_option_seolinger.phpt
    assert_eq!(
        st.set_option(
            F,
            &mut s,
            SOL_SOCKET,
            i64::from(libc::SO_LINGER),
            &Value::Array(vec![])
        )
        .unwrap_err(),
        ve("socket_set_option(): Argument #4 ($value) must have key \"l_onoff\"")
    );
    assert_eq!(
        st.set_option(
            F,
            &mut s,
            SOL_SOCKET,
            i64::from(libc::SO_LINGER),
            &Value::assoc(vec![("l_onoff", Value::Int(1))])
        )
        .unwrap_err(),
        ve("socket_set_option(): Argument #4 ($value) must have key \"l_linger\"")
    );
    assert_eq!(
        st.set_option(
            F,
            &mut s,
            SOL_SOCKET,
            i64::from(libc::SO_LINGER),
            &Value::assoc(vec![
                ("l_onoff", Value::Int(-1)),
                ("l_linger", Value::Int(1))
            ])
        )
        .unwrap_err(),
        ve("socket_set_option(): Argument #4 ($value) \"l_onoff\" must be between 0 and 65535")
    );
    assert_eq!(
        st.set_option(F, &mut s, SOL_SOCKET, i64::from(libc::SO_LINGER), &Value::Int(1)).unwrap_err(),
        te("socket_set_option(): Argument #4 ($value) must be of type array when argument #3 ($option) is SO_LINGER, int given")
    );
    st.set_option(
        F,
        &mut s,
        SOL_SOCKET,
        i64::from(libc::SO_LINGER),
        &Value::assoc(vec![
            ("l_onoff", Value::Int(1)),
            ("l_linger", Value::str("7")),
        ]),
    )
    .unwrap();
    let g = st
        .get_option(
            "socket_get_option",
            &mut s,
            SOL_SOCKET,
            i64::from(libc::SO_LINGER),
        )
        .unwrap();
    assert_eq!(g.get("l_onoff").and_then(OutValue::as_int), Some(1));
    assert_eq!(g.get("l_linger").and_then(OutValue::as_int), Some(7));
    // socket_set_option_rcvtimeo.phpt / sndtimeo
    assert_eq!(
        st.set_option(
            F,
            &mut s,
            SOL_SOCKET,
            i64::from(libc::SO_RCVTIMEO),
            &Value::Array(vec![])
        )
        .unwrap_err(),
        ve("socket_set_option(): Argument #4 ($value) must have key \"sec\"")
    );
    assert_eq!(
        st.set_option(F, &mut s, SOL_SOCKET, i64::from(libc::SO_SNDTIMEO), &Value::Bool(true)).unwrap_err(),
        te("socket_set_option(): Argument #4 ($value) must be of type array when argument #3 ($option) is SO_SNDTIMEO, true given")
    );
    let tv = Value::assoc(vec![("sec", Value::Int(1)), ("usec", Value::Int(0))]);
    st.set_option(F, &mut s, SOL_SOCKET, i64::from(libc::SO_RCVTIMEO), &tv)
        .unwrap();
    let g = st
        .get_option(
            "socket_get_option",
            &mut s,
            SOL_SOCKET,
            i64::from(libc::SO_RCVTIMEO),
        )
        .unwrap();
    assert_eq!(g.get("sec").and_then(OutValue::as_int), Some(1));
    assert_eq!(g.get("usec").and_then(OutValue::as_int), Some(0));
    // socket_set_option_timeo_error.phpt: zval_get_long() of an object.
    let _ = st.take_warnings();
    let obj = Value::Object {
        class: "stdClass".into(),
        props: Vec::new(),
        string: None,
    };
    let tv = Value::assoc(vec![("sec", obj), ("usec", Value::str("1"))]);
    st.set_option(F, &mut s, SOL_SOCKET, i64::from(libc::SO_SNDTIMEO), &tv)
        .unwrap();
    assert_eq!(
        st.take_warnings(),
        [format!(
            "{}Object of class stdClass could not be converted to int",
            crate::PLAIN_WARNING
        )]
    );
    // Plain ints (socket_setopt_basic.phpt, TCP_NODELAY, SO_REUSEPORT).
    for (lvl, opt) in [
        (SOL_SOCKET, libc::SO_REUSEADDR),
        (SOL_SOCKET, libc::SO_REUSEPORT),
        (i64::from(libc::IPPROTO_TCP), libc::TCP_NODELAY),
        (SOL_SOCKET, libc::SO_KEEPALIVE),
    ] {
        st.set_option("socket_setopt", &mut s, lvl, opt.into(), &Value::Bool(true))
            .unwrap();
        assert_eq!(
            st.get_option("socket_getopt", &mut s, lvl, opt.into())
                .unwrap()
                .as_int(),
            Some(1)
        );
    }
    st.set_option(
        F,
        &mut s,
        i64::from(libc::IPPROTO_TCP),
        i64::from(libc::TCP_KEEPIDLE),
        &Value::str("33"),
    )
    .unwrap();
    assert_eq!(
        st.get_option(
            "socket_getopt",
            &mut s,
            i64::from(libc::IPPROTO_TCP),
            i64::from(libc::TCP_KEEPIDLE)
        )
        .unwrap()
        .as_int(),
        Some(33)
    );
    // TCP_CONGESTION (socket_tcp_congestion.phpt)
    let g = st
        .get_option(
            "socket_get_option",
            &mut s,
            i64::from(libc::IPPROTO_TCP),
            i64::from(libc::TCP_CONGESTION),
        )
        .unwrap();
    assert!(g
        .get("name")
        .and_then(OutValue::as_bytes)
        .is_some_and(|n| !n.is_empty()));
    assert_eq!(
        st.set_option(F, &mut s, i64::from(libc::IPPROTO_TCP), i64::from(libc::TCP_CONGESTION), &Value::Int(1)).unwrap_err(),
        te("socket_set_option(): Argument #4 ($value) must be of type string when argument #3 ($option) is TCP_CONGESTION, int given")
    );
    // TCP_USER_TIMEOUT range.
    assert_eq!(
        st.set_option(
            F,
            &mut s,
            i64::from(libc::IPPROTO_TCP),
            i64::from(libc::TCP_USER_TIMEOUT),
            &Value::Int(-1)
        )
        .unwrap_err(),
        ve("socket_set_option(): Argument #4 ($value) must be between 0 and 4294967295")
    );
    st.set_option(
        F,
        &mut s,
        i64::from(libc::IPPROTO_TCP),
        i64::from(libc::TCP_USER_TIMEOUT),
        &Value::Int(1000),
    )
    .unwrap();
    // SO_BINDTODEVICE type (socket_set_option_invalid_value_for_SOL_SOCKET_SO_BINDTODEVICE.phpt).
    assert_eq!(
        st.set_option(F, &mut s, SOL_SOCKET, i64::from(libc::SO_BINDTODEVICE), &Value::Array(vec![])).unwrap_err(),
        te("socket_set_option(): Argument #4 ($value) must be of type string when argument #3 ($option) is SO_BINDTODEVICE, array given")
    );
    // Unknown option: warning text from setsockopt (socket_set_option_error_socket_option.phpt).
    assert_eq!(
        st.set_option(F, &mut s, SOL_SOCKET, 9999, &Value::Int(1))
            .unwrap_err(),
        SocketError::Failed
    );
    assert_eq!(
        st.take_warnings(),
        ["Unable to set socket option [92]: Protocol not available"]
    );
    assert_eq!(
        st.get_option("socket_get_option", &mut s, SOL_SOCKET, 9999)
            .unwrap_err(),
        SocketError::Failed
    );
    assert_eq!(
        st.take_warnings(),
        ["Unable to retrieve socket option [92]: Protocol not available"]
    );
    // SO_TYPE / SO_ERROR reads.
    assert_eq!(
        st.get_option(
            "socket_get_option",
            &mut s,
            SOL_SOCKET,
            i64::from(libc::SO_TYPE)
        )
        .unwrap()
        .as_int(),
        Some(STREAM)
    );
    // SO_MEMINFO shape (socket_meminfo.phpt).
    let m = st
        .get_option(
            "socket_get_option",
            &mut s,
            SOL_SOCKET,
            i64::from(libc::SO_MEMINFO),
        )
        .unwrap();
    let OutValue::Array(t) = m else { panic!() };
    let keys: Vec<_> = t.iter().map(|(k, _)| k.clone()).collect();
    assert_eq!(keys.len(), 9);
    assert_eq!(keys[0], Key::s("rmem_alloc"));
    assert_eq!(keys[8], Key::s("drops"));
    // SO_ATTACH_REUSEPORT_CBPF.
    assert_eq!(
        st.set_option(
            F,
            &mut s,
            i64::from(libc::IPPROTO_TCP),
            i64::from(libc::SO_ATTACH_REUSEPORT_CBPF),
            &Value::Int(1)
        )
        .unwrap_err(),
        SocketError::Failed
    );
    assert_eq!(st.take_warnings(), ["Invalid level"]);
    assert_eq!(
        st.set_option(F, &mut s, SOL_SOCKET, i64::from(libc::SO_ATTACH_REUSEPORT_CBPF), &Value::str("x")).unwrap_err(),
        te("socket_set_option(): Argument #4 ($value) must be of type int when argument #3 ($option) is SO_ATTACH_REUSEPORT_CBPF, string given")
    );
    assert_eq!(
        st.set_option(
            F,
            &mut s,
            SOL_SOCKET,
            i64::from(libc::SO_ATTACH_REUSEPORT_CBPF),
            &Value::Int(5)
        )
        .unwrap_err(),
        SocketError::Failed
    );
    assert_eq!(st.take_warnings(), ["Unsupported CBPF filter"]);
    st.set_option(
        F,
        &mut s,
        SOL_SOCKET,
        i64::from(libc::SO_REUSEPORT),
        &Value::Int(1),
    )
    .unwrap();
    st.set_option(
        F,
        &mut s,
        SOL_SOCKET,
        i64::from(libc::SO_ATTACH_REUSEPORT_CBPF),
        &Value::Int(36),
    )
    .unwrap();
    // socket_reuseport_cbpf_detach.phpt: SKF_AD_QUEUE is 24
    // (<linux/filter.h>), and 0 detaches.
    for k in [24, 0] {
        st.set_option(
            F,
            &mut s,
            SOL_SOCKET,
            i64::from(libc::SO_ATTACH_REUSEPORT_CBPF),
            &Value::Int(k),
        )
        .unwrap();
    }
}

#[test]
fn multicast_options() {
    // socket_getopt.phpt
    let mut st = Sockets::new();
    const F: &str = "socket_set_option";
    let ip = i64::from(libc::IPPROTO_IP);
    let mut s = st
        .create(AF_INET, DGRAM, i64::from(libc::IPPROTO_UDP))
        .unwrap();
    st.set_option(
        F,
        &mut s,
        ip,
        i64::from(libc::IP_MULTICAST_TTL),
        &Value::Int(9),
    )
    .unwrap();
    assert_eq!(
        st.get_option(
            "socket_getopt",
            &mut s,
            ip,
            i64::from(libc::IP_MULTICAST_TTL)
        )
        .unwrap()
        .as_int(),
        Some(9)
    );
    st.set_option(
        F,
        &mut s,
        ip,
        i64::from(libc::IP_MULTICAST_LOOP),
        &Value::Int(0),
    )
    .unwrap();
    assert_eq!(
        st.get_option(
            "socket_getopt",
            &mut s,
            ip,
            i64::from(libc::IP_MULTICAST_LOOP)
        )
        .unwrap()
        .as_int(),
        Some(0)
    );
    st.set_option(
        F,
        &mut s,
        ip,
        i64::from(libc::IP_MULTICAST_LOOP),
        &Value::Int(1),
    )
    .unwrap();
    assert_eq!(
        st.get_option(
            "socket_getopt",
            &mut s,
            ip,
            i64::from(libc::IP_MULTICAST_LOOP)
        )
        .unwrap()
        .as_int(),
        Some(1)
    );
    st.set_option(
        F,
        &mut s,
        ip,
        i64::from(libc::IP_MULTICAST_IF),
        &Value::Int(0),
    )
    .unwrap();
    assert_eq!(
        st.get_option(
            "socket_getopt",
            &mut s,
            ip,
            i64::from(libc::IP_MULTICAST_IF)
        )
        .unwrap()
        .as_int(),
        Some(0)
    );
    let lo = i64::from(sys::if_nametoindex(&sys::cstr(b"lo")));
    if lo > 0 {
        st.set_option(
            F,
            &mut s,
            ip,
            i64::from(libc::IP_MULTICAST_IF),
            &Value::Int(lo),
        )
        .unwrap();
        assert_eq!(
            st.get_option(
                "socket_getopt",
                &mut s,
                ip,
                i64::from(libc::IP_MULTICAST_IF)
            )
            .unwrap()
            .as_int(),
            Some(lo)
        );
        st.set_option(
            F,
            &mut s,
            ip,
            i64::from(libc::IP_MULTICAST_IF),
            &Value::str("lo"),
        )
        .unwrap();
    }
    assert_eq!(
        st.set_option(
            F,
            &mut s,
            ip,
            i64::from(libc::IP_MULTICAST_TTL),
            &Value::Int(256)
        )
        .unwrap_err(),
        ve("socket_set_option(): Argument #4 ($value) must be between 0 and 255")
    );
    assert_eq!(
        st.set_option(
            F,
            &mut s,
            ip,
            i64::from(libc::IP_MULTICAST_IF),
            &Value::Int(-1)
        )
        .unwrap_err(),
        ve("Index must be between 0 and 4294967295")
    );
    assert_eq!(
        st.set_option(
            F,
            &mut s,
            ip,
            i64::from(libc::IP_MULTICAST_IF),
            &Value::str("nosuchif0")
        )
        .unwrap_err(),
        SocketError::Failed
    );
    assert_eq!(
        st.take_warnings(),
        ["No interface with name \"nosuchif0\" could be found"]
    );
    // socket_set_option_mcast_error.phpt
    let join = i64::from(libc::MCAST_JOIN_GROUP);
    assert_eq!(
        st.set_option(F, &mut s, ip, i64::from(libc::MCAST_LEAVE_GROUP), &Value::Bool(true)).unwrap_err(),
        te("socket_set_option(): Argument #4 ($value) must be of type array when argument #3 ($option) is MCAST_LEAVE_GROUP, true given")
    );
    assert_eq!(
        st.set_option(F, &mut s, ip, i64::from(libc::MCAST_LEAVE_SOURCE_GROUP), &Value::Bool(true)).unwrap_err(),
        te("socket_set_option(): Argument #4 ($value) must be of type array when argument #3 ($option) is MCAST_LEAVE_SOURCE_GROUP, true given")
    );
    assert_eq!(
        st.set_option(F, &mut s, ip, join, &Value::Array(vec![]))
            .unwrap_err(),
        ve("No key \"group\" passed in optval")
    );
    // Joining and leaving a group on loopback (mcast_ipv4_recv.phpt).
    let req = Value::assoc(vec![
        ("group", Value::str("224.0.0.23")),
        ("interface", Value::Int(lo)),
    ]);
    st.set_option(F, &mut s, ip, join, &req).unwrap();
    st.set_option(F, &mut s, ip, i64::from(libc::MCAST_LEAVE_GROUP), &req)
        .unwrap();
    assert_eq!(
        st.set_option(F, &mut s, ip, i64::from(libc::MCAST_LEAVE_GROUP), &req)
            .unwrap_err(),
        SocketError::Failed
    );
    assert_eq!(
        st.take_warnings(),
        ["Unable to set socket option [99]: Cannot assign requested address"]
    );
    let src = Value::assoc(vec![
        ("group", Value::str("224.0.0.23")),
        ("source", Value::str("127.0.0.1")),
        ("interface", Value::Int(lo)),
    ]);
    st.set_option(
        F,
        &mut s,
        ip,
        i64::from(libc::MCAST_JOIN_SOURCE_GROUP),
        &src,
    )
    .unwrap();
    st.set_option(
        F,
        &mut s,
        ip,
        i64::from(libc::MCAST_LEAVE_SOURCE_GROUP),
        &src,
    )
    .unwrap();
    // mcast_sockettype_error.phpt
    let mut u = st.create(AF_UNIX, DGRAM, 0).unwrap();
    assert_eq!(
        st.set_option(
            F,
            &mut u,
            ip,
            join,
            &Value::assoc(vec![
                ("group", Value::str("127.0.0.1")),
                ("interface", Value::str("lo"))
            ])
        )
        .unwrap_err(),
        ve("IP address used in the context of an unexpected type of socket")
    );
    // IPv6 hop limits.
    if let Ok(mut s6) = st.create(AF_INET6, DGRAM, 0) {
        let v6 = i64::from(libc::IPPROTO_IPV6);
        st.set_option(
            F,
            &mut s6,
            v6,
            i64::from(libc::IPV6_MULTICAST_HOPS),
            &Value::Int(-1),
        )
        .unwrap();
        assert_eq!(
            st.set_option(
                F,
                &mut s6,
                v6,
                i64::from(libc::IPV6_MULTICAST_HOPS),
                &Value::Int(256)
            )
            .unwrap_err(),
            ve("socket_set_option(): Argument #4 ($value) must be between -1 and 255")
        );
        st.set_option(
            F,
            &mut s6,
            v6,
            i64::from(libc::IPV6_MULTICAST_LOOP),
            &Value::Bool(false),
        )
        .unwrap();
        assert_eq!(
            st.get_option(
                "socket_get_option",
                &mut s6,
                v6,
                i64::from(libc::IPV6_MULTICAST_LOOP)
            )
            .unwrap()
            .as_int(),
            Some(0)
        );
        // socket_set_option_in6_pktinfo.phpt
        let pk = i64::from(libc::IPV6_PKTINFO);
        assert_eq!(
            st.set_option(F, &mut s6, v6, pk, &Value::Array(vec![]))
                .unwrap_err(),
            SocketError::Failed
        );
        assert_eq!(
            st.take_warnings(),
            ["error converting user data (path: in6_pktinfo): The key 'addr' is required"]
        );
        st.set_option(
            F,
            &mut s6,
            v6,
            pk,
            &Value::assoc(vec![
                ("addr", Value::str("::1")),
                ("ifindex", Value::Int(0)),
            ]),
        )
        .unwrap();
        assert_eq!(
            st.set_option(
                F,
                &mut s6,
                v6,
                pk,
                &Value::assoc(vec![("addr", Value::str("")), ("ifindex", Value::Int(0))])
            )
            .unwrap_err(),
            SocketError::Failed
        );
        let w = st.take_warnings();
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].starts_with("Host lookup failed ["), "{w:?}");
        assert_eq!(w[1], "error converting user data (path: in6_pktinfo > addr): could not resolve address '' to get an AF_INET6 address");
    }
}

#[test]
fn sendmsg_recvmsg_rights_and_iov() {
    // socket_cmsg_rights.phpt / socket_sendrecvmsg_multi_msg-unix.phpt
    let mut st = Sockets::new();
    let path = tmp_path("cmsg");
    let mut sender = st.create(AF_UNIX, DGRAM, 0).unwrap();
    let mut recv = st.create(AF_UNIX, DGRAM, 0).unwrap();
    st.bind(&mut recv, &path, 0).unwrap();
    let rights = i64::from(libc::SCM_RIGHTS);
    let msg = Value::assoc(vec![
        (
            "name",
            Value::assoc(vec![("path", Value::Str(path.clone()))]),
        ),
        (
            "iov",
            Value::list(vec![
                Value::str("test "),
                Value::str("thing"),
                Value::str("\n"),
            ]),
        ),
        (
            "control",
            Value::list(vec![Value::assoc(vec![
                ("level", Value::Int(SOL_SOCKET)),
                ("type", Value::Int(rights)),
                (
                    "data",
                    Value::list(vec![
                        Value::Socket { fd: sender.fd() },
                        Value::Stream { fd: Some(0) },
                        Value::Stream { fd: Some(1) },
                    ]),
                ),
            ])]),
        ),
    ]);
    assert_eq!(st.sendmsg(&mut sender, &msg, 0).unwrap(), 11);
    let space = st.cmsg_space(SOL_SOCKET, rights, 3).unwrap();
    assert_eq!(space, 32);
    let spec = Value::assoc(vec![
        ("name", Value::Array(vec![])),
        ("buffer_size", Value::Int(2000)),
        ("controllen", Value::Int(space)),
    ]);
    let (n, out) = st.recvmsg(&mut recv, &spec, 0).unwrap();
    assert_eq!(n, 11);
    let OutValue::Array(top) = &out else {
        panic!("{out:?}")
    };
    let keys: Vec<_> = top.iter().map(|(k, _)| k.clone()).collect();
    assert_eq!(
        keys,
        [
            Key::s("name"),
            Key::s("control"),
            Key::s("iov"),
            Key::s("flags")
        ]
    );
    // Unnamed (unbound) sender: no address.
    assert!(matches!(out.get("name"), Some(OutValue::Null)));
    let Some(OutValue::Array(iov)) = out.get("iov") else {
        panic!()
    };
    assert_eq!(iov.len(), 1);
    assert!(matches!(&iov[0].1, OutValue::Str(s) if s == b"test thing\n"));
    assert_eq!(out.get("flags").and_then(OutValue::as_int), Some(0));
    let Some(OutValue::Array(ctl)) = out.get("control") else {
        panic!()
    };
    let c0 = &ctl[0].1;
    assert_eq!(c0.get("level").and_then(OutValue::as_int), Some(SOL_SOCKET));
    assert_eq!(c0.get("type").and_then(OutValue::as_int), Some(rights));
    let Some(OutValue::Array(fds)) = c0.get("data") else {
        panic!()
    };
    assert_eq!(fds.len(), 3);
    assert!(
        matches!(&fds[0].1, OutValue::Socket(s) if s.family() == libc::AF_UNIX && !s.is_stream_owned())
    );
    for (_, v) in &fds[1..] {
        match v {
            OutValue::StreamFd(fd) => sys::close(*fd),
            OutValue::Socket(_) => {} // stdio may itself be a socket
            other => panic!("{other:?}"),
        }
    }
    let _ = std::fs::remove_file(String::from_utf8(path).unwrap());
}

#[test]
fn sendmsg_recvmsg_credentials() {
    // socket_cmsg_credentials.phpt
    let mut st = Sockets::new();
    let (mut a, mut b) = st.create_pair(AF_UNIX, DGRAM, 0).unwrap();
    st.set_option(
        "socket_set_option",
        &mut b,
        SOL_SOCKET,
        i64::from(libc::SO_PASSCRED),
        &Value::Int(1),
    )
    .unwrap();
    assert_eq!(st.send(&mut a, b"dread", 5, 0).unwrap(), 5);
    let cred = i64::from(libc::SCM_CREDENTIALS);
    let spec = Value::assoc(vec![
        ("name", Value::Array(vec![])),
        ("buffer_size", Value::Int(2000)),
        (
            "controllen",
            Value::Int(st.cmsg_space(SOL_SOCKET, cred, 0).unwrap()),
        ),
    ]);
    let (n, out) = st.recvmsg(&mut b, &spec, 0).unwrap();
    assert_eq!(n, 5);
    let Some(OutValue::Array(ctl)) = out.get("control") else {
        panic!("{out:?}")
    };
    let data = ctl[0].1.get("data").unwrap();
    let (uid, gid, pid) = sys::ids();
    assert_eq!(data.get("pid").and_then(OutValue::as_int), Some(pid.into()));
    assert_eq!(data.get("uid").and_then(OutValue::as_int), Some(uid.into()));
    assert_eq!(data.get("gid").and_then(OutValue::as_int), Some(gid.into()));
    // Explicit credentials on send.
    let msg = Value::assoc(vec![
        ("iov", Value::list(vec![Value::str("x")])),
        (
            "control",
            Value::list(vec![Value::assoc(vec![
                ("level", Value::Int(SOL_SOCKET)),
                ("type", Value::Int(cred)),
                (
                    "data",
                    Value::assoc(vec![
                        ("pid", Value::Int(pid.into())),
                        ("uid", Value::Int(uid.into())),
                        ("gid", Value::Int(gid.into())),
                    ]),
                ),
            ])]),
        ),
    ]);
    assert_eq!(st.sendmsg(&mut a, &msg, 0).unwrap(), 1);
    let (_, out) = st.recvmsg(&mut b, &spec, 0).unwrap();
    assert!(out.get("control").is_some());
}

#[test]
fn sendmsg_conversion_errors() {
    let mut st = Sockets::new();
    let (mut a, _b) = st.create_pair(AF_UNIX, DGRAM, 0).unwrap();
    let check = |st: &mut Sockets, a: &mut Socket, msg: Value, want: &str| {
        assert_eq!(st.sendmsg(a, &msg, 0).unwrap_err(), SocketError::Failed);
        assert_eq!(st.take_warnings(), [want]);
    };
    check(
        &mut st,
        &mut a,
        Value::assoc(vec![("iov", Value::Int(1))]),
        "error converting user data (path: msghdr > iov): expected an array here",
    );
    check(
        &mut st,
        &mut a,
        Value::assoc(vec![("control", Value::list(vec![Value::assoc(vec![("level", Value::Int(1)), ("type", Value::Int(99))])]))]),
        "error converting user data (path: msghdr > control > element #0): cmsghdr with level 1 and type 99 not supported",
    );
    check(
        &mut st,
        &mut a,
        Value::assoc(vec![("control", Value::list(vec![Value::assoc(vec![("level", Value::Int(1)), ("type", Value::Int(1)), ("data", Value::Array(vec![]))])]))]),
        "error converting user data (path: msghdr > control > element #0): expected at least one element in this array",
    );
    check(
        &mut st,
        &mut a,
        Value::assoc(vec![("control", Value::list(vec![Value::assoc(vec![("level", Value::Int(1)), ("type", Value::Int(1)), ("data", Value::list(vec![Value::Int(3)]))])]))]),
        "error converting user data (path: msghdr > control > element #0 > data > element #1): expected a Socket object or a stream resource",
    );
    check(
        &mut st,
        &mut a,
        Value::assoc(vec![("control", Value::list(vec![Value::assoc(vec![("level", Value::Int(1)), ("type", Value::Int(1)), ("data", Value::list(vec![Value::Socket { fd: -1 }]))])]))]),
        "error converting user data (path: msghdr > control > element #0 > data > element #1): socket is already closed",
    );
    check(
        &mut st,
        &mut a,
        Value::assoc(vec![("control", Value::list(vec![Value::assoc(vec![("level", Value::str("x")), ("type", Value::Int(1))])]))]),
        "error converting user data (path: msghdr > control > element #0 > level): expected an integer, but got a non numeric string (possibly from a converted object): 'x'",
    );
    check(
        &mut st,
        &mut a,
        Value::assoc(vec![("name", Value::assoc(vec![("path", Value::str(""))]))]),
        "error converting user data (path: msghdr > name > path): the path is must not be empty",
    );
    check(
        &mut st,
        &mut a,
        Value::assoc(vec![("name", Value::assoc(vec![("family", Value::Int(AF_INET))]))]),
        "error converting user data (path: msghdr > name): the specified family (number 2) is not supported on this socket",
    );
    check(
        &mut st,
        &mut a,
        Value::assoc(vec![("name", Value::assoc(vec![("family", Value::Int(99))]))]),
        "error converting user data (path: msghdr > name): the only families currently supported are AF_INET, AF_INET6 and AF_UNIX",
    );
    // recvmsg spec errors.
    let r = |st: &mut Sockets, a: &mut Socket, spec: Value, want: &str| {
        assert_eq!(st.recvmsg(a, &spec, 0).unwrap_err(), SocketError::Failed);
        assert_eq!(st.take_warnings(), [want]);
    };
    r(
        &mut st,
        &mut a,
        Value::Array(vec![]),
        "error converting user data (path: msghdr): The key 'controllen' is required",
    );
    r(
        &mut st,
        &mut a,
        Value::assoc(vec![("controllen", Value::Int(0))]),
        "error converting user data (path: msghdr > controllen): controllen cannot be 0",
    );
    r(
        &mut st,
        &mut a,
        Value::assoc(vec![("buffer_size", Value::Int(-1)), ("controllen", Value::Int(1))]),
        "error converting user data (path: msghdr > buffer_size): the buffer size must be between 1 and 104857600; given -1",
    );
    // Flags range and kernel errors (socket_sendrecvmsg_error.phpt).
    assert_eq!(
        st.sendmsg(&mut a, &Value::Array(vec![]), 1 << 40)
            .unwrap_err(),
        ve("socket_sendmsg(): Argument #3 ($flags) must be between -2147483648 and 2147483647")
    );
    let mut u = st
        .create(AF_INET, DGRAM, i64::from(libc::IPPROTO_UDP))
        .unwrap();
    assert_eq!(
        st.sendmsg(&mut u, &Value::Array(vec![]), -1).unwrap_err(),
        SocketError::Failed
    );
    assert!(st.take_warnings()[0].starts_with("Error in sendmsg ["));
    assert_eq!(
        st.recvmsg(
            &mut u,
            &Value::assoc(vec![("controllen", Value::Int(1))]),
            -1
        )
        .unwrap_err(),
        SocketError::Failed
    );
    assert!(st.take_warnings()[0].starts_with("Error in recvmsg ["));
}

#[test]
fn udp_recvmsg_names() {
    // socket_recvmsg.phpt-style IPv4 name decoding.
    let mut st = Sockets::new();
    let mut r = st.create(AF_INET, DGRAM, 0).unwrap();
    st.bind(&mut r, b"127.0.0.1", 0).unwrap();
    let port = st.getsockname(&mut r).unwrap().port.unwrap();
    let mut s = st.create(AF_INET, DGRAM, 0).unwrap();
    st.bind(&mut s, b"127.0.0.1", 0).unwrap();
    let sport = st.getsockname(&mut s).unwrap().port.unwrap();
    let msg = Value::assoc(vec![
        (
            "name",
            Value::assoc(vec![
                ("addr", Value::str("127.0.0.1")),
                ("port", Value::Int(port)),
            ]),
        ),
        (
            "iov",
            Value::list(vec![Value::str("abc"), Value::str("def")]),
        ),
    ]);
    assert_eq!(st.sendmsg(&mut s, &msg, 0).unwrap(), 6);
    let spec = Value::assoc(vec![
        ("name", Value::Array(vec![])),
        ("buffer_size", Value::Int(4)),
        ("controllen", Value::Int(100)),
    ]);
    let (n, out) = st.recvmsg(&mut r, &spec, 0).unwrap();
    assert_eq!(n, 4);
    let name = out.get("name").unwrap();
    assert_eq!(name.get("family").and_then(OutValue::as_int), Some(AF_INET));
    assert_eq!(
        name.get("addr").and_then(OutValue::as_bytes),
        Some(&b"127.0.0.1"[..])
    );
    assert_eq!(name.get("port").and_then(OutValue::as_int), Some(sport));
    assert_eq!(
        out.get("flags").and_then(OutValue::as_int),
        Some(i64::from(libc::MSG_TRUNC))
    );
    let Some(OutValue::Array(ctl)) = out.get("control") else {
        panic!()
    };
    assert!(ctl.is_empty());
}

#[test]
fn cmsg_space_values() {
    let st = Sockets::new();
    let rights = i64::from(libc::SCM_RIGHTS);
    assert_eq!(st.cmsg_space(SOL_SOCKET, rights, 0).unwrap(), 16);
    assert_eq!(st.cmsg_space(SOL_SOCKET, rights, 4).unwrap(), 32);
    assert_eq!(
        st.cmsg_space(SOL_SOCKET, i64::from(libc::SCM_CREDENTIALS), 0)
            .unwrap(),
        32
    );
    assert_eq!(
        st.cmsg_space(
            i64::from(libc::IPPROTO_IPV6),
            i64::from(libc::IPV6_PKTINFO),
            0
        )
        .unwrap(),
        40
    );
    assert_eq!(
        st.cmsg_space(
            i64::from(libc::IPPROTO_IPV6),
            i64::from(libc::IPV6_HOPLIMIT),
            0
        )
        .unwrap(),
        24
    );
    assert_eq!(
        st.cmsg_space(SOL_SOCKET, 0, i64::MAX).unwrap_err(),
        ve("socket_cmsg_space(): Argument #3 ($num) must be between -2147483648 and 2147483647")
    );
    assert_eq!(
        st.cmsg_space(SOL_SOCKET, rights, -1).unwrap_err(),
        ve("socket_cmsg_space(): Argument #3 ($num) must be greater than or equal to 0")
    );
    assert_eq!(
        st.cmsg_space(1, 99, 0).unwrap_err(),
        ve("Pair level 1 and/or type 99 is not supported")
    );
    if let Some(out) = php("echo socket_cmsg_space(SOL_SOCKET, SCM_RIGHTS, 4), ' ', socket_cmsg_space(SOL_SOCKET, SCM_CREDENTIALS), ' ', socket_cmsg_space(IPPROTO_IPV6, IPV6_PKTINFO);") {
        assert_eq!(out, "32 32 40");
    }
}

#[test]
fn addrinfo() {
    // socket_addrinfo_lookup.phpt / _explain / _bind / _connect
    let mut st = Sockets::new();
    let hints = Value::assoc(vec![
        ("ai_family", Value::Int(AF_INET)),
        ("ai_socktype", Value::Int(DGRAM)),
    ]);
    let list = st
        .addrinfo_lookup(b"127.0.0.1", Some(b"2000"), Some(&hints))
        .unwrap()
        .unwrap();
    assert!(!list.is_empty());
    let e = list[0].explain();
    let OutValue::Array(t) = &e else { panic!() };
    let keys: Vec<_> = t.iter().map(|(k, _)| k.clone()).collect();
    assert_eq!(
        keys,
        [
            Key::s("ai_flags"),
            Key::s("ai_family"),
            Key::s("ai_socktype"),
            Key::s("ai_protocol"),
            Key::s("ai_addr")
        ]
    );
    let addr = e.get("ai_addr").unwrap();
    assert_eq!(addr.get("sin_port").and_then(OutValue::as_int), Some(2000));
    assert_eq!(
        addr.get("sin_addr").and_then(OutValue::as_bytes),
        Some(&b"127.0.0.1"[..])
    );
    let mut b = st.addrinfo_bind(&list[0]).unwrap();
    assert_eq!(st.getsockname(&mut b).unwrap().port, Some(2000));
    let mut c = st.addrinfo_connect(&list[0]).unwrap();
    assert_eq!(st.getpeername(&mut c).unwrap().port, Some(2000));
    drop(b);
    // Errors.
    const F: &str = "socket_addrinfo_lookup";
    let keys_msg = format!("{F}(): Argument #3 ($hints) must only contain array keys \"ai_flags\", \"ai_socktype\", \"ai_protocol\", or \"ai_family\"");
    assert_eq!(
        st.addrinfo_lookup(b"127.0.0.1", None, Some(&Value::list(vec![Value::Int(1)])))
            .unwrap_err(),
        ve(&keys_msg)
    );
    assert_eq!(
        st.addrinfo_lookup(
            b"127.0.0.1",
            None,
            Some(&Value::assoc(vec![("foo", Value::Int(1))]))
        )
        .unwrap_err(),
        ve(&keys_msg)
    );
    assert_eq!(
        st.addrinfo_lookup(b"127.0.0.1", None, Some(&Value::assoc(vec![("ai_flags", Value::Array(vec![]))]))).unwrap_err(),
        te("socket_addrinfo_lookup(): Argument #3 ($hints) \"ai_flags\" key must be of type int, array given")
    );
    assert_eq!(
        st.addrinfo_lookup(b"127.0.0.1", None, Some(&Value::assoc(vec![("ai_socktype", Value::Int(-1))]))).unwrap_err(),
        ve("socket_addrinfo_lookup(): Argument #3 ($hints) \"ai_socktype\" key must be between 0 and 2147483647")
    );
    assert_eq!(
        st.addrinfo_lookup(b"127.0.0.1", None, Some(&Value::assoc(vec![("ai_family", Value::Int(AF_UNIX))]))).unwrap_err(),
        ve("socket_addrinfo_lookup(): Argument #3 ($hints) \"ai_family\" key must be AF_INET or AF_INET6")
    );
    // getaddrinfo failure → false + error code (socket_getaddrinfo_error.phpt).
    let flags = Value::assoc(vec![(
        "ai_flags",
        Value::Int(i64::from(libc::AI_NUMERICHOST)),
    )]);
    let code = st
        .addrinfo_lookup(b"not-numeric", None, Some(&flags))
        .unwrap()
        .unwrap_err();
    assert_eq!(code, i64::from(libc::EAI_NONAME));
}

#[test]
fn import_and_export() {
    let mut st = Sockets::new();
    let (a, _b) = st.create_pair(AF_UNIX, STREAM, 0).unwrap();
    let mut imported = st.import_fd(a.fd()).unwrap();
    assert_eq!(imported.family(), libc::AF_UNIX);
    assert!(imported.is_blocking());
    assert!(imported.is_stream_owned());
    st.set_nonblock(&mut imported).unwrap();
    let again = st.import_fd(a.fd()).unwrap();
    assert!(!again.is_blocking());
    drop(again);
    drop(imported);
    // The stream-owned imports did not close the descriptor.
    assert!(sys::is_socket(a.fd()).unwrap());
    assert_eq!(st.import_fd(-1).unwrap_err(), SocketError::Failed);
    assert_eq!(
        st.take_warnings(),
        ["Unable to obtain socket family [9]: Bad file descriptor"]
    );
}

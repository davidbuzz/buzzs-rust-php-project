// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `stream_socket_recvfrom()` on a blocking stream waits for data, as the
//! `recv()` of `sock_recvfrom()` (`main/streams/xp_socket.c`) does: a server
//! that peeks at a new connection's first byte, to tell a TLS handshake
//! from plain HTTP, gets that byte and not `false`. (The descriptor of a
//! TCP stream is non-blocking here; without the wait every such server
//! took a TLS client for a plain one, and the client's handshake timed
//! out.) Expected output from php8.5.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::Duration;

#[test]
fn a_peek_at_a_new_connection_waits_for_its_first_bytes() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_php"))
        .args(["-n", "-r"])
        .arg(
            r#"
$server = stream_socket_server('tcp://127.0.0.1:0');
echo stream_socket_get_name($server, false), "\n";
$conn = stream_socket_accept($server, 30);
var_dump(stream_socket_recvfrom($conn, 1, STREAM_PEEK));
var_dump(stream_socket_recvfrom($conn, 4, STREAM_PEEK));
var_dump(fread($conn, 4));
stream_set_blocking($conn, false);
fread($conn, 100);
var_dump(stream_socket_recvfrom($conn, 1, STREAM_PEEK));
"#,
        )
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn php");
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut name = String::new();
    out.read_line(&mut name).expect("the server's address");
    let mut client = TcpStream::connect(name.trim()).expect("connect");
    // The server is in its first peek before anything is sent.
    std::thread::sleep(Duration::from_millis(300));
    client.write_all(b"abcdef").unwrap();
    let mut rest = String::new();
    out.read_to_string(&mut rest).expect("the server's output");
    drop(client);
    assert!(child.wait().expect("php exits").success());
    assert_eq!(
        rest,
        "string(1) \"a\"\nstring(4) \"abcd\"\nstring(4) \"abcd\"\nbool(false)\n"
    );
}

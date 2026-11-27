// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! End-to-end tests: PHP scripts run on a VM with the `sockets`
//! extension (plus `streams` for the stream glue), over socket pairs and
//! loopback UDP.  Expected outputs come from `/usr/bin/php8.5`.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use php_vm::ext::Extension;
use php_vm::{Vm, VmOptions};

/// A `Write` sink shared with the test.
#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Runs `code` (without `<?php`) and returns its output.
fn run(code: &str) -> String {
    let sink = Sink::default();
    let (streams, std, sockets) = (
        php_ext_streams::Streams,
        php_ext_stdarr::StdArray,
        php_ext_sockets::SocketsExt,
    );
    let exts: [&dyn Extension; 3] = [&streams, &std, &sockets];
    let opts = VmOptions { ini: vec![] };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn socket_pair_io_select_and_errors() {
    assert_eq!(
        run(r#"socket_create_pair(AF_UNIX, SOCK_STREAM, 0, $p);
var_dump(socket_write($p[0], "hello\nworld"), socket_read($p[1], 100, PHP_NORMAL_READ), socket_read($p[1], 3));
$r = ['a' => $p[1], 'b' => $p[0]]; $w = null; $e = null;
var_dump(socket_select($r, $w, $e, 0), array_keys($r));
var_dump(socket_recv($p[1], $buf, 10, 0), $buf);
socket_close($p[0]);
try { socket_write($p[0], "x"); } catch (Error $ex) { echo $ex->getMessage(), "\n"; }
var_dump(socket_recv($p[1], $buf, 10, 0), $buf);
try { new Socket; } catch (Error $ex) { echo $ex->getMessage(), "\n"; }
try { clone $p[1]; } catch (Error $ex) { echo $ex->getMessage(), "\n"; }
var_dump(socket_strerror(2));
"#),
        r#"int(11)
string(6) "hello
"
string(3) "wor"
int(1)
array(1) {
  [0]=>
  string(1) "a"
}
int(2)
string(2) "ld"
socket_write(): Argument #1 ($socket) has already been closed
int(0)
NULL
Cannot directly construct Socket, use socket_create() instead
Trying to clone an uncloneable object of class Socket
string(25) "No such file or directory"
"#
    );
}

#[test]
fn udp_loopback_options_and_export() {
    assert_eq!(
        run(r#"$s = socket_create(AF_INET, SOCK_DGRAM, SOL_UDP);
var_dump(socket_bind($s, '127.0.0.1', 0));
socket_getsockname($s, $addr, $port);
var_dump($addr, $port > 0);
var_dump(socket_sendto($s, "ping", 4, 0, '127.0.0.1', $port));
var_dump(socket_recvfrom($s, $buf, 10, 0, $from, $fport), $buf, $from, $fport === $port);
var_dump(socket_get_option($s, SOL_SOCKET, SO_TYPE) === SOCK_DGRAM);
var_dump(socket_set_option($s, SOL_SOCKET, SO_RCVTIMEO, ['sec' => 1, 'usec' => 0]), socket_get_option($s, SOL_SOCKET, SO_RCVTIMEO));
$st = socket_export_stream($s);
var_dump(stream_get_meta_data($st)['stream_type'], socket_export_stream($s) === $st);
var_dump(@socket_connect($s, '256.1.1.1', 80));
"#),
        r#"bool(true)
string(9) "127.0.0.1"
bool(true)
int(4)
int(4)
string(4) "ping"
string(9) "127.0.0.1"
bool(true)
bool(true)
bool(true)
array(2) {
  ["sec"]=>
  int(1)
  ["usec"]=>
  int(0)
}
string(10) "udp_socket"
bool(true)
bool(false)
"#
    );
}

/// `socket_export_stream()` asks for a stream of the socket's protocol and
/// gives it nothing but the protocol's name (`php_stream_xport_create("udp://",
/// ...)`, ext/sockets/sockets.c), which is then the stream's `uri` in
/// `stream_get_meta_data()`; the tcp transport is the one OpenSSL registers. A
/// debugging library builds the address it shows from that `uri`. Expected
/// output from php8.5.
#[test]
fn an_exported_stream_names_its_protocol() {
    assert_eq!(
        run(
            r#"foreach ([[AF_INET, SOCK_STREAM, SOL_TCP], [AF_INET6, SOCK_STREAM, SOL_TCP], [AF_INET, SOCK_DGRAM, SOL_UDP],
                         [AF_UNIX, SOCK_STREAM, 0], [AF_UNIX, SOCK_DGRAM, 0]] as [$domain, $type, $protocol]) {
                   $meta = stream_get_meta_data(socket_export_stream(socket_create($domain, $type, $protocol)));
                   echo $meta['stream_type'], ' ', $meta['uri'], ' ', $meta['mode'], "\n";
               }"#
        ),
        "tcp_socket/ssl tcp:// r+\ntcp_socket/ssl tcp:// r+\nudp_socket udp:// r+\nunix_socket unix:// r+\nudg_socket udg:// r+\n"
    );
}

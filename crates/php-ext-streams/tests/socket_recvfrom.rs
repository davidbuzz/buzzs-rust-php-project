// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `stream_socket_recvfrom()`'s `$address` is the sender of the datagram
//! (`sock_recvfrom()` in main/streams/xp_socket.c uses recvfrom()), so an
//! unconnected UDP server can answer it with `stream_socket_sendto()`; a
//! connected TCP socket gives "". Failed sends warn as C's do.

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

/// The VM interrupt flag is process-wide: this binary's VMs run one at a
/// time.
static VM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn run(code: &str) -> String {
    let _serial = VM_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let sink = Sink::default();
    let streams = php_ext_streams::Streams;
    let exts: [&dyn Extension; 1] = [&streams];
    let opts = VmOptions { ini: vec![] };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn recvfrom_reports_the_sender() {
    let out = run(r#"
$s = stream_socket_server('udp://127.0.0.1:0', $errno, $errstr, STREAM_SERVER_BIND);
$name = stream_socket_get_name($s, false);
$c = stream_socket_client("udp://$name");
fwrite($c, "hello");
var_dump(stream_socket_recvfrom($s, 512, 0, $addr));
var_dump($addr === stream_socket_get_name($c, false));
var_dump(stream_socket_sendto($s, 'back', 0, $addr));
var_dump(fread($c, 10));
$t = stream_socket_server('tcp://127.0.0.1:0');
$tc = stream_socket_client("tcp://" . stream_socket_get_name($t, false));
$p = stream_socket_accept($t);
fwrite($tc, "hi");
var_dump(stream_socket_recvfrom($p, 10, 0, $taddr), $taddr);
"#);
    // php8.5's output.
    assert_eq!(
        out,
        "string(5) \"hello\"\nbool(true)\nint(4)\nstring(4) \"back\"\n\
         string(2) \"hi\"\nstring(0) \"\"\n"
    );
}

/// A failed send warns as C does: `stream_socket_sendto()` returns -1
/// with the `strerror()` text (php_stream_warn(NetworkSendFailed) in
/// master's main/streams/xp_socket.c; php8.5 still appends "\n"), and
/// `fwrite()` returns false with "Send of %zu bytes failed with errno=%d
/// %s". A datagram over 64 KiB is EMSGSIZE.
#[test]
fn failed_sends_warn() {
    let out = run(r#"
$c = stream_socket_client('udp://127.0.0.1:12345');
$big = str_repeat('1', 1024 * 1024);
var_dump(@stream_socket_sendto($c, $big));
var_dump(error_get_last()['message']);
var_dump(@stream_socket_sendto($c, $big, 0, '127.0.0.1:9'));
var_dump(error_get_last()['message']);
var_dump(@fwrite($c, $big));
var_dump(error_get_last()['message']);
var_dump(stream_socket_sendto($c, 'ok'));
"#);
    assert_eq!(
        out,
        "int(-1)\nstring(40) \"stream_socket_sendto(): Message too long\"\n\
         int(-1)\nstring(40) \"stream_socket_sendto(): Message too long\"\n\
         bool(false)\n\
         string(69) \"fwrite(): Send of 1048576 bytes failed with errno=90 Message too long\"\n\
         int(2)\n"
    );
}

/// Writing to a socket pair whose other end is closed: EPIPE, reported by
/// `fwrite()`'s warning (php8.5's output).
#[test]
fn write_to_closed_pair_warns() {
    let out = run(r#"
[$a, $b] = stream_socket_pair(STREAM_PF_UNIX, STREAM_SOCK_STREAM, STREAM_IPPROTO_IP);
stream_set_blocking($a, false);
fclose($b);
var_dump(@fwrite($a, "foo"), error_get_last()['message'] ?? null);
"#);
    assert_eq!(
        out,
        "bool(false)\nstring(58) \"fwrite(): Send of 3 bytes failed with errno=32 Broken pipe\"\n"
    );
}

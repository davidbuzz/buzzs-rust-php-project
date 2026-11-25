// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `stream_socket_recvfrom()`'s `$address` is the sender of the datagram
//! (`sock_recvfrom()` in main/streams/xp_socket.c uses recvfrom()), so an
//! unconnected UDP server can answer it with `stream_socket_sendto()`; a
//! connected TCP socket gives "".

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

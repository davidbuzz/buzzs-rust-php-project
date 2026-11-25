// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `stream_socket_client()` with `STREAM_CLIENT_ASYNC_CONNECT` and without
//! `STREAM_CLIENT_CONNECT` still connects, without waiting
//! (`php_stream_xport_create()` in main/streams/transports.c connects for
//! either flag). The event loops of async PHP libraries open their sockets
//! this way and wait for them to become writable.

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
fn async_connect_connects() {
    let out = run(r#"
$srv = stream_socket_server("tcp://127.0.0.1:0", $errno, $errstr);
$addr = stream_socket_get_name($srv, false);
$c = stream_socket_client("tcp://$addr", $errno, $errstr, 5, STREAM_CLIENT_ASYNC_CONNECT);
var_dump($c !== false, $errno, $errstr);
$r = null; $w = [$c]; $e = null;
var_dump(stream_select($r, $w, $e, 5));
var_dump(stream_get_meta_data($c)['blocked']);
var_dump(fwrite($c, "ping"));
$peer = stream_socket_accept($srv, 5);
var_dump(fread($peer, 4));
fwrite($peer, "pong");
var_dump(fread($c, 4));
$u = stream_socket_server("udp://127.0.0.1:0", $errno, $errstr, STREAM_SERVER_BIND);
$uaddr = stream_socket_get_name($u, false);
$uc = stream_socket_client("udp://$uaddr", $errno, $errstr, 5, STREAM_CLIENT_ASYNC_CONNECT);
var_dump(fwrite($uc, "datagram"));
var_dump(stream_socket_recvfrom($u, 100));
"#);
    // php8.5's output.
    assert_eq!(
        out,
        "bool(true)\nint(0)\nstring(0) \"\"\nint(1)\nbool(true)\nint(4)\n\
         string(4) \"ping\"\nstring(4) \"pong\"\nint(8)\nstring(8) \"datagram\"\n"
    );
}

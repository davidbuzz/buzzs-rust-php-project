// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A user error handler that throws for a warning a registered transport
//! raised while creating its stream (`tcp://` with a `bindto` address
//! that cannot be assigned, ext/openssl's transport): the exception
//! surfaces from `stream_socket_client()` (it is pending when
//! `php_stream_xport_create()` returns, main/streams/transports.c), and
//! for a failed connect `$error_code`/`$error_message` are still filled
//! while the "Unable to connect" warning reaches no handler
//! (`zend_call_function()` does nothing with an exception pending).
//! amphp/socket's test suite relies on the first for its bind failures.
//! Expected output from php8.5 with the real `tcp://` transport and
//! `bindto => '128.0.0.1:0'`.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use php_ext_streams::xport::{self, Transport, XportError, XportRequest};
use php_streams::mode::TempMode;
use php_streams::stream::Stream;
use php_vm::ext::{Extension, VmResult};
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

/// A transport that warns as a failed `bindto` does, then connects (any
/// port but 1) or is refused (port 1).
fn create(_vm: &mut Vm, req: &XportRequest) -> VmResult<Result<Stream, XportError>> {
    php_streams::diag::warning(
        "Failed to bind to '128.0.0.1:0', system said: Cannot assign requested address",
    );
    if req.port == 1 {
        return Ok(Err(XportError {
            errno: 111,
            message: "Connection refused".into(),
        }));
    }
    Ok(Ok(php_streams::memory::memory_create(TempMode::Default)))
}

fn run(code: &str) -> String {
    let _serial = VM_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let sink = Sink::default();
    let streams = php_ext_streams::Streams;
    let exts: [&dyn Extension; 1] = [&streams];
    let opts = VmOptions { ini: vec![] };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    xport::register_transport(
        "faketcp",
        Transport {
            create,
            accept: None,
            connect: None,
        },
    );
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

const HANDLER: &str = r#"
set_error_handler(function ($no, $str) { echo "H: $str\n"; throw new Exception("H"); });
"#;

#[test]
fn handler_exception_surfaces_from_a_connected_stream() {
    let out = run(&format!(
        r#"{HANDLER}
try {{
    $s = stream_socket_client("faketcp://127.0.0.1:80", $e1, $e2, 1, STREAM_CLIENT_CONNECT);
    echo "returned\n";
}} catch (Exception $e) {{ echo "caught ", $e->getMessage(), "\n"; }}
var_dump(isset($s));
try {{
    $s = stream_socket_client("faketcp://127.0.0.1:80",
        flags: STREAM_CLIENT_CONNECT | STREAM_CLIENT_ASYNC_CONNECT);
    echo "returned\n";
}} catch (Exception $e) {{ echo "caught ", $e->getMessage(), "\n"; }}
try {{ $x = file_get_contents("data:,ok"); echo "next: $x\n"; }}
catch (Exception $e) {{ echo "stale\n"; }}
"#
    ));
    assert_eq!(
        out,
        "H: stream_socket_client(): Failed to bind to '128.0.0.1:0', system said: Cannot assign requested address\n\
         caught H\n\
         bool(false)\n\
         H: stream_socket_client(): Failed to bind to '128.0.0.1:0', system said: Cannot assign requested address\n\
         caught H\n\
         next: ok\n"
    );
}

#[test]
fn failed_connect_fills_the_error_and_skips_the_handler() {
    let out = run(&format!(
        r#"{HANDLER}
try {{
    $s = stream_socket_client("faketcp://127.0.0.1:1", $e1, $e2, 1, STREAM_CLIENT_CONNECT);
    echo "returned\n";
}} catch (Exception $e) {{ echo "caught ", $e->getMessage(), "\n"; }}
var_dump($e1, $e2, error_get_last());
"#
    ));
    assert_eq!(
        out,
        "H: stream_socket_client(): Failed to bind to '128.0.0.1:0', system said: Cannot assign requested address\n\
         caught H\n\
         int(111)\n\
         string(18) \"Connection refused\"\n\
         NULL\n"
    );
}

#[test]
fn without_a_handler_both_warnings_show() {
    let out = run(r#"
$s = stream_socket_client("faketcp://127.0.0.1:1", $e1, $e2, 1, STREAM_CLIENT_CONNECT);
var_dump($s, $e1);
"#);
    let bind = out
        .find("stream_socket_client(): Failed to bind to '128.0.0.1:0'")
        .expect("bind warning");
    let connect = out
        .find("stream_socket_client(): Unable to connect to faketcp://127.0.0.1:1 (Connection refused)")
        .expect("connect warning");
    assert!(bind < connect, "{out}");
    assert!(out.ends_with("bool(false)\nint(111)\n"), "{out}");
}

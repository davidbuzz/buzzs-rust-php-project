// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A write to a socket whose other end does not read waits for room no
//! longer than the stream's timeout (`php_sockop_write()` in
//! `main/streams/xp_socket.c`): what fitted is reported, `timed_out` is
//! set, and the next write fails with a diagnostic. A logging handler
//! with a write timeout relies on it; a write that waited for ever hung
//! the test suite of a framework.

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

fn run(code: &str) -> String {
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

/// Expected output from php8.5, whose diagnostic is a notice where
/// php-src master's `php_stream_warn(NetworkSendFailed)` is a warning.
#[test]
fn a_write_nobody_reads_times_out() {
    let path = std::env::temp_dir().join(format!("phprust-wto-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let out = run(&r#"
$server = stream_socket_server('unix://{PATH}');
$c = stream_socket_client('unix://{PATH}', $errno, $errstr, 1);
stream_set_timeout($c, 0, 100000);
$d = 'a'; for ($i = 0; $i < 20; $i++) { $d .= $d; }
$n = @fwrite($c, $d);
var_dump($n > 0 && $n < strlen($d), stream_get_meta_data($c)['timed_out']);
$n = fwrite($c, $d);
var_dump($n, stream_get_meta_data($c)['timed_out']);
$r = stream_socket_accept($server, 1);
stream_set_blocking($r, false);
while (fread($r, 65536)) {}
var_dump(fwrite($c, "recovered\n"), stream_get_meta_data($c)['timed_out']);
"#
    .replace("{PATH}", &path.to_string_lossy()));
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        out,
        "bool(true)\nbool(true)\n\
         \nWarning: fwrite(): Send of 1048576 bytes failed with errno=11 Resource temporarily \
         unavailable in test.php on line 8\n\
         bool(false)\nbool(true)\nint(10)\nbool(true)\n"
    );
}

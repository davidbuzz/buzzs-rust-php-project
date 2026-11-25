// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `get_resources()` lists `stream filter` resources while they are
//! attached, whether or not the script kept them: `apply_filter_to_stream()`
//! (ext/standard/streamsfuncs.c) keeps a reference in `EG(regular_list)`.

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

/// The expected output is php8.5's for the same script.
#[test]
fn stream_filters_are_listed_while_attached() {
    let out = run(r"
$f = fopen('php://memory', 'w+');
stream_filter_append($f, 'string.rot13');
$x = stream_filter_append($f, 'string.toupper');
$last = null;
foreach (get_resources() as $id => $r) { $last = $id; }
var_dump($last === (int) $x);
echo count(get_resources('stream filter')), PHP_EOL;
stream_filter_remove($x);
echo count(get_resources('stream filter')), PHP_EOL;
fclose($f);
echo count(get_resources('stream filter')), PHP_EOL;
");
    assert_eq!(out, "bool(true)\n2\n1\n0\n");
}

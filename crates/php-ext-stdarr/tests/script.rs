// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! End-to-end tests: PHP scripts run on a VM with the `standard` array
//! binding. Expected outputs come from PHP itself.

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
    let ext = php_ext_stdarr::StdArray;
    let exts: [&dyn Extension; 1] = [&ext];
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &exts);
    vm.run_script(code.as_bytes(), "t.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

/// bug72369: `array_merge()` unwraps a singleton reference and keeps the
/// interned literal (matches PHP's `debug_zval_dump` output).
#[test]
fn merged_unwrapped_reference_retains_interned_string() {
    let output = run(r"<?php
$x = 'xxx';
$d = ['test' => &$x];
unset($x);
$a = ['test' => 'yyy'];
$a = array_merge($a, $d);
debug_zval_dump($a);
");
    assert_eq!(
        output,
        "array(1) refcount(2){\n  [\"test\"]=>\n  string(3) \"xxx\" interned\n}\n"
    );
}

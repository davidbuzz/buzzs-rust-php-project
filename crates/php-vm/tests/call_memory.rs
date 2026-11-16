// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `memory_get_usage()` stability across call sites and constructor invocation
//! with the real request allocator installed.

use php_vm::{Vm, VmOptions};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

php_memlimit::install_global_allocator!();

#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn call_site_first_execution_memory_stability() {
    let source = br#"<?php
class P {
    public function __construct($x = null) {}
}
function g($x = null) {}

$m0 = memory_get_usage();
new P('x');
$d1 = memory_get_usage() - $m0;

$m0 = memory_get_usage();
new P('x');
$d2 = memory_get_usage() - $m0;

$m0 = memory_get_usage();
g('x');
$d3 = memory_get_usage() - $m0;

$m0 = memory_get_usage();
g('x');
$d4 = memory_get_usage() - $m0;

$m0 = memory_get_usage();
new P();
$d5 = memory_get_usage() - $m0;

$m0 = memory_get_usage();
new P();
$d6 = memory_get_usage() - $m0;

var_dump($d1, $d2, $d3, $d4, $d5, $d6);
"#;
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source, "call_site_mem.php");
    drop(vm);
    assert_eq!(status, 0);
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `memory_get_usage()` does not move when a script first uses a table that
//! php-src preallocates or keeps outside the Zend heap: the global symbol
//! table's entries for the main script's variables, the GC root buffer,
//! run-time caches and a class's default property table. Expected output
//! from `/usr/bin/php8.5 -n` (every delta is 0).
//!
//! One test function on purpose: the counting allocator is process-wide,
//! so scripts running on parallel test threads would disturb each other's
//! figures.

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

fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "lazy_memory.php");
    drop(vm);
    assert_eq!(status, 0);
    let out = sink.0.borrow().clone();
    String::from_utf8(out).expect("UTF-8 output")
}

#[test]
fn first_use_of_preallocated_tables_does_not_move_memory_usage() {
    // `unset()` of main-script variables (ext/openssl/tests/bug79881.phpt):
    // their symbol-table entries exist from the start.
    let globals = run(r#"<?php
$a = 1; $b = 2; $c = 3; $d = 4; $e = 5; $f = 6; $g = 7; $h = 8;
$m1 = memory_get_usage();
unset($a, $b);
$m2 = memory_get_usage();
echo $m2 - $m1, "\n";
"#);
    assert_eq!(globals, "0\n");

    // The first `new` of a class at a site: class cache and default
    // property table.
    let new_site = run(r#"<?php
class A { public $x; }
function f() {
    $m = memory_get_usage();
    for ($i = 0; $i < 3; $i++) { $o = new A; unset($o); }
    $m2 = memory_get_usage();
    echo $m2 - $m, "\n";
}
f();
"#);
    assert_eq!(new_site, "0\n");

    // Constructor promotion: property cache, and the GC root buffer the
    // released object is buffered in.
    let promoted = run(r#"<?php
class P { public function __construct(public string $k = 'd') {} }
$s = memory_get_usage();
for ($i = 0; $i < 2; ++$i) { new P('abe'); }
$s2 = memory_get_usage();
echo $s2 - $s, "\n";
"#);
    assert_eq!(promoted, "0\n");
}

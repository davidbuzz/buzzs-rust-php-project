//! `memory_get_usage()` deltas around `serialize()`/`unserialize()` with the
//! real request allocator installed (one test: the counter is process-wide).

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
fn unserialize_leaves_no_memory_behind() {
    // ext/standard/tests/serialize/bug81142.phpt (the first `(string) 0`,
    // the first `serialize()` call site and the parse cells must not show),
    // then a steady-state loop.
    let source = br#"<?php
$mem0 = memory_get_usage();
$ctr = 0;
unserialize(serialize(["foo_$ctr" => 1]));
$mem1 = memory_get_usage();
var_dump($mem1 - $mem0);
$s = serialize(["k" => [1], 2 => "x"]);
$mem0 = memory_get_usage();
for ($i = 0; $i < 100; $i++) {
    unserialize($s);
}
$mem1 = memory_get_usage();
var_dump($mem1 - $mem0);
"#;
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source, "bug81142.php");
    drop(vm);
    assert_eq!(status, 0);
    assert_eq!(&*sink.0.borrow(), b"int(0)\nint(0)\n");
}

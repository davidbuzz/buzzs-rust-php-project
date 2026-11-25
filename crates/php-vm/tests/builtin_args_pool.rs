// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The pooled argument records of builtin calls (`CallBufs::take_args`)
//! with the request allocator installed: builtin calls nested deeper than
//! the pool keeps (an `array_map()` callback calling `array_map()` again,
//! twelve deep, with `strlen()` and `str_repeat()` at the bottom), run
//! from the tape, leave `memory_get_usage()` where it was once warm, as
//! php8.5's VM stack does (`0 0` there, `/usr/bin/php8.5 -n`).

mod common;

use php_vm::VmOptions;
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
fn nested_builtin_calls_leave_memory_usage_where_it_was() {
    let sink = Sink::default();
    let mut vm = common::vm_with_stdlib(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(
        br#"<?php
function deep($n) {
    if ($n === 0) {
        return strlen(str_repeat('x', 3));
    }
    return array_sum(array_map(fn($v) => deep($n - 1) + $v, [1]));
}
function t() {
    $d = [];
    for ($i = 0; $i < 4; $i++) {
        $before = memory_get_usage();
        $r = deep(12);
        $d[] = memory_get_usage() - $before;
        echo $r, "\n";
    }
    echo $d[2], " ", $d[3], "\n";
}
t();
"#,
        "t.php",
    );
    drop(vm);
    let out = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 0);
    assert_eq!(out, "15\n15\n15\n15\n0 0\n");
}

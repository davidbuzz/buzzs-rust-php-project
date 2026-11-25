// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `intval($string, $base)` hands the base to `strtol()` as a C `int`
//! (`ZEND_STRTOL(Z_STRVAL_P(num), NULL, (int)base)`, ext/standard/type.c):
//! a base it refuses (neither 0 nor 2..=36, also after the cut to 32 bits)
//! gives 0, and `PHP_INT_MIN` is base 0 after the cut (`strtol()` then
//! reads the prefix itself). A base of 1 used to end the process (a digit
//! test with an invalid radix). Expected output from `/usr/bin/php8.5 -n`.
mod common;

use php_vm::VmOptions;
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

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
    let mut vm = common::vm_with_stdlib(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

#[test]
fn a_base_strtol_refuses_gives_zero() {
    assert_eq!(
        run(r#"<?php
foreach ([0, 1, 2, 8, 16, 36, 37, -1, PHP_INT_MAX, PHP_INT_MIN] as $base) {
    echo $base, ': ', intval('12', $base), ' ', intval('0x1A', $base), ' ', intval(' 012z', $base), "\n";
}
"#),
        "0: 12 26 10\n\
         1: 0 0 0\n\
         2: 1 0 1\n\
         8: 10 0 10\n\
         16: 18 26 18\n\
         36: 38 42814 1403\n\
         37: 0 0 0\n\
         -1: 0 0 0\n\
         9223372036854775807: 0 0 0\n\
         -9223372036854775808: 12 26 10\n"
    );
}

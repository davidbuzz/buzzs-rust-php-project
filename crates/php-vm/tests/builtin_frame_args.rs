// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A builtin's arguments as backtraces see them while its handler runs
//! from the tape's internal call (`exec/tape/calls.rs`, `call_native`,
//! whose handler reads pooled copies while the call frame keeps the sent
//! arguments): `debug_backtrace()` in a user error handler run for a
//! warning the builtin raises (`hex2bin()`), in a callback of
//! `array_map()` and in a builtin called from that callback (a nested
//! call taking the next pooled record), and an exception's trace
//! (`strpos()`'s `ValueError`), three rounds so that every call runs from
//! the tape. Expected output from `/usr/bin/php8.5 -n`.

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

fn run(source: &str) -> (i32, String) {
    let sink = Sink::default();
    let mut vm = common::vm_with_stdlib(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    (status, String::from_utf8(output).expect("UTF-8 output"))
}

#[test]
fn backtraces_show_a_builtin_call_frames_arguments() {
    let (status, out) = run(r#"<?php
function show($trace) {
    foreach ($trace as $i => $f) {
        echo "  #", $i, ' ', $f['function'], '(', implode(', ', array_map('json_encode', $f['args'] ?? [])), ")\n";
    }
}
set_error_handler(function ($no, $msg) {
    echo $msg, "\n";
    show(array_slice(debug_backtrace(), 1));
    return true;
});
function cb($v) {
    show(debug_backtrace());
    return strlen(hex2bin(str_repeat('a', $v))) * 2;
}
function t() {
    $s = 'abc';
    for ($i = 0; $i < 3; $i++) {
        var_dump(hex2bin($s));
        echo implode(',', array_map('cb', [$i, 5])), "\n";
        try {
            strpos($s, 'b', 10 + $i);
        } catch (ValueError $e) {
            echo $e->getMessage(), "\n";
            show($e->getTrace());
        }
        echo strtoupper($s), current([$s, 1]), "\n";
    }
}
t();
"#);
    assert_eq!(status, 0);
    assert_eq!(
        out,
        r#"hex2bin(): Hexadecimal input string must have an even length
  #0 hex2bin("abc")
  #1 t()
bool(false)
  #0 cb(0)
  #1 array_map("cb", [0,5])
  #2 t()
  #0 cb(5)
  #1 array_map("cb", [0,5])
  #2 t()
hex2bin(): Hexadecimal input string must have an even length
  #0 hex2bin("aaaaa")
  #1 cb(5)
  #2 array_map("cb", [0,5])
  #3 t()
0,0
strpos(): Argument #3 ($offset) must be contained in argument #1 ($haystack)
  #0 strpos("abc", "b", 10)
  #1 t()
ABCabc
hex2bin(): Hexadecimal input string must have an even length
  #0 hex2bin("abc")
  #1 t()
bool(false)
  #0 cb(1)
  #1 array_map("cb", [1,5])
  #2 t()
hex2bin(): Hexadecimal input string must have an even length
  #0 hex2bin("a")
  #1 cb(1)
  #2 array_map("cb", [1,5])
  #3 t()
  #0 cb(5)
  #1 array_map("cb", [1,5])
  #2 t()
hex2bin(): Hexadecimal input string must have an even length
  #0 hex2bin("aaaaa")
  #1 cb(5)
  #2 array_map("cb", [1,5])
  #3 t()
0,0
strpos(): Argument #3 ($offset) must be contained in argument #1 ($haystack)
  #0 strpos("abc", "b", 11)
  #1 t()
ABCabc
hex2bin(): Hexadecimal input string must have an even length
  #0 hex2bin("abc")
  #1 t()
bool(false)
  #0 cb(2)
  #1 array_map("cb", [2,5])
  #2 t()
  #0 cb(5)
  #1 array_map("cb", [2,5])
  #2 t()
hex2bin(): Hexadecimal input string must have an even length
  #0 hex2bin("aaaaa")
  #1 cb(5)
  #2 array_map("cb", [2,5])
  #3 t()
2,0
strpos(): Argument #3 ($offset) must be contained in argument #1 ($haystack)
  #0 strpos("abc", "b", 12)
  #1 t()
ABCabc
"#
    );
}

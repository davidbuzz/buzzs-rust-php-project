// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The line of the builtins php-src compiles to an opcode
//! (`zend_try_compile_special_func_ex`, `Zend/zend_compile.c`: `ZEND_STRLEN`,
//! `ZEND_COUNT`, `ZEND_CAST`, `ZEND_GET_CLASS`, ...) when the call spans
//! several lines: the opcode is emitted right after its argument is
//! compiled, on the argument's line, while an ordinary call such as
//! `ord()` reports the line of the call. Expected output from
//! `/usr/bin/php8.5 -n`.
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
fn special_function_opcodes_report_the_argument_line() {
    assert_eq!(
        run(r###"<?php
set_error_handler(function ($no, $msg, $f, $l) { echo "$msg @$l\n"; return true; });
function f($n) {
  $a = strlen(
    $n
  );
  try { $b = count(
    $n
  ); } catch (TypeError $e) { echo $e->getMessage(), ' @', $e->getLine(), "\n"; }
  try { $c = get_class(
    $n
  ); } catch (TypeError $e) { echo $e->getMessage(), ' @', $e->getLine(), "\n"; }
  $d = strval(
    [1]
  );
  $g = ord(
    $n
  );
  return [$a, $d, $g];
}
for ($i = 0; $i < 3; $i++) { var_export(f(null)); echo "\n"; }
"###),
        r###"strlen(): Passing null to parameter #1 ($string) of type string is deprecated @5
count(): Argument #1 ($value) must be of type Countable|array, null given @8
get_class(): Argument #1 ($object) must be of type object, null given @11
Array to string conversion @14
ord(): Passing null to parameter #1 ($character) of type string is deprecated @16
ord(): Providing an empty string is deprecated @16
array (
  0 => 0,
  1 => 'Array',
  2 => 0,
)
strlen(): Passing null to parameter #1 ($string) of type string is deprecated @5
count(): Argument #1 ($value) must be of type Countable|array, null given @8
get_class(): Argument #1 ($object) must be of type object, null given @11
Array to string conversion @14
ord(): Passing null to parameter #1 ($character) of type string is deprecated @16
ord(): Providing an empty string is deprecated @16
array (
  0 => 0,
  1 => 'Array',
  2 => 0,
)
strlen(): Passing null to parameter #1 ($string) of type string is deprecated @5
count(): Argument #1 ($value) must be of type Countable|array, null given @8
get_class(): Argument #1 ($object) must be of type object, null given @11
Array to string conversion @14
ord(): Passing null to parameter #1 ($character) of type string is deprecated @16
ord(): Providing an empty string is deprecated @16
array (
  0 => 0,
  1 => 'Array',
  2 => 0,
)
"###
    );
}

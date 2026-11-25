// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Lines of prefix operators written over several lines
//! (`Zend/zend_compile.c`): `zend_ast_create_1()` gives `-`, `!`, `~`, a
//! cast, `yield`, `yield from` and `throw` the line of their operand, and
//! the opcode is emitted after compiling it, so a warning or error it
//! raises names the operand's line. Expected output from
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
fn prefix_operators_report_the_operand_line() {
    assert_eq!(
        run(r###"<?php
set_error_handler(function ($no, $msg, $f, $l) { echo "$msg @$l\n"; return true; });
function gen() { $x = yield
  $u1; yield from
  $u2; }
function f() {
  $a = -
    $u3;
  $b = !
    $u4;
  try { $c = ~
    $u5; } catch (TypeError $ex) { echo $ex->getMessage(), ' @', $ex->getLine(), "\n"; }
  $e = (string)
    [1];
  $g = (int)
    $u7;
  try { throw
    $u9; } catch (Error $ex) { echo $ex->getMessage(), " @", $ex->getLine(), "\n"; }
  return [$a, $b, $e, $g];
}
for ($i = 0; $i < 3; $i++) {
  try { foreach (gen() as $v) {} } catch (Error $ex) { echo $ex->getMessage(), " @", $ex->getLine(), "\n"; }
  var_export(f()); echo "\n";
}
"###),
        r###"Undefined variable $u1 @4
Undefined variable $u2 @5
Can use "yield from" only with arrays and Traversables @5
Undefined variable $u3 @8
Undefined variable $u4 @10
Undefined variable $u5 @12
Cannot perform bitwise not on null @12
Array to string conversion @14
Undefined variable $u7 @16
Undefined variable $u9 @18
Can only throw objects @18
array (
  0 => 0,
  1 => true,
  2 => 'Array',
  3 => 0,
)
Undefined variable $u1 @4
Undefined variable $u2 @5
Can use "yield from" only with arrays and Traversables @5
Undefined variable $u3 @8
Undefined variable $u4 @10
Undefined variable $u5 @12
Cannot perform bitwise not on null @12
Array to string conversion @14
Undefined variable $u7 @16
Undefined variable $u9 @18
Can only throw objects @18
array (
  0 => 0,
  1 => true,
  2 => 'Array',
  3 => 0,
)
Undefined variable $u1 @4
Undefined variable $u2 @5
Can use "yield from" only with arrays and Traversables @5
Undefined variable $u3 @8
Undefined variable $u4 @10
Undefined variable $u5 @12
Cannot perform bitwise not on null @12
Array to string conversion @14
Undefined variable $u7 @16
Undefined variable $u9 @18
Can only throw objects @18
array (
  0 => 0,
  1 => true,
  2 => 'Array',
  3 => 0,
)
"###
    );
}

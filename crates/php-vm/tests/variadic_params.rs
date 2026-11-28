// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Variadic parameters (`ZEND_RECV_VARIADIC`, `Vm::recv_variadic_plain`):
//! untyped, typed (each argument taken as it is, coerced in coercive mode,
//! a `TypeError` in strict mode), nullable, by reference, an interface
//! type, a union, empty, spread from a list and from string keys, a
//! reference argument, `func_get_args()` beside them; twice, so that the
//! calls run from the tape. Expected output from `/usr/bin/php8.5`.

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
fn variadic_parameters_match_php_src() {
    assert_eq!(
        run(r#"<?php
function s(string ...$ids) { var_dump($ids); }
function i(int ...$n) { var_dump($n); }
function f(float ...$x) { var_dump($x); }
function m($a, ?string ...$rest) { var_dump($a, $rest, func_get_args(), func_num_args()); }
function r(&...$refs) { foreach ($refs as &$r) { $r .= '!'; } }
function o(Countable ...$c) { echo count($c), "\n"; }
function u(int|string ...$v) { var_dump($v); }
for ($k = 0; $k < 2; $k++) {
  s('a', 'b'); s(); s(1, 2.5, true);
  i(1, 2); i('3', '4'); f(1, 2.5);
  m(1); m(1, 'x', null);
  $p = 'p'; $q = 'q'; r($p, $q); echo "$p $q\n";
  o(new ArrayObject([1]), new ArrayObject([]));
  u(1, 'two', 3);
  try { i('x'); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
  $arr = ['a', 'b', 'c']; s(...$arr); s(...['k' => 'v']);
  $x = 'val'; $ref = &$x; s($ref, $x);
}
"#)
        .1,
        r#"array(2) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
}
array(0) {
}
array(3) {
  [0]=>
  string(1) "1"
  [1]=>
  string(3) "2.5"
  [2]=>
  string(1) "1"
}
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
array(2) {
  [0]=>
  int(3)
  [1]=>
  int(4)
}
array(2) {
  [0]=>
  float(1)
  [1]=>
  float(2.5)
}
int(1)
array(0) {
}
array(1) {
  [0]=>
  int(1)
}
int(1)
int(1)
array(2) {
  [0]=>
  string(1) "x"
  [1]=>
  NULL
}
array(3) {
  [0]=>
  int(1)
  [1]=>
  string(1) "x"
  [2]=>
  NULL
}
int(3)
p! q!
2
array(3) {
  [0]=>
  int(1)
  [1]=>
  string(3) "two"
  [2]=>
  int(3)
}
i(): Argument #1 must be of type int, string given, called in t.php on line 16
array(3) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
  [2]=>
  string(1) "c"
}
array(1) {
  ["k"]=>
  string(1) "v"
}
array(2) {
  [0]=>
  string(3) "val"
  [1]=>
  string(3) "val"
}
array(2) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
}
array(0) {
}
array(3) {
  [0]=>
  string(1) "1"
  [1]=>
  string(3) "2.5"
  [2]=>
  string(1) "1"
}
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
array(2) {
  [0]=>
  int(3)
  [1]=>
  int(4)
}
array(2) {
  [0]=>
  float(1)
  [1]=>
  float(2.5)
}
int(1)
array(0) {
}
array(1) {
  [0]=>
  int(1)
}
int(1)
int(1)
array(2) {
  [0]=>
  string(1) "x"
  [1]=>
  NULL
}
array(3) {
  [0]=>
  int(1)
  [1]=>
  string(1) "x"
  [2]=>
  NULL
}
int(3)
p! q!
2
array(3) {
  [0]=>
  int(1)
  [1]=>
  string(3) "two"
  [2]=>
  int(3)
}
i(): Argument #1 must be of type int, string given, called in t.php on line 16
array(3) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
  [2]=>
  string(1) "c"
}
array(1) {
  ["k"]=>
  string(1) "v"
}
array(2) {
  [0]=>
  string(3) "val"
  [1]=>
  string(3) "val"
}
"#
    );
}

#[test]
fn strict_variadic_parameters_match_php_src() {
    assert_eq!(
        run(r#"<?php declare(strict_types=1);
function s(string ...$ids) { var_dump($ids); }
function f(float ...$x) { var_dump($x); }
s('a'); f(1, 2.5);
try { s(1); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
"#)
        .1,
        r#"array(1) {
  [0]=>
  string(1) "a"
}
array(2) {
  [0]=>
  float(1)
  [1]=>
  float(2.5)
}
s(): Argument #1 must be of type string, int given, called in t.php on line 5
"#
    );
}

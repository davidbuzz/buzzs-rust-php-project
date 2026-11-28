// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! References to array elements (`ZEND_FETCH_DIM_W` + `ZEND_MAKE_REF` /
//! `ZEND_SEND_REF`, the fast path `exec::hot::ref_cv_dim_in`): an existing
//! and a missing key, int and numeric-string keys, a shared array
//! separated, a list grown, an undefined or null base, a string offset's
//! error, `ArrayAccess`, an array behind a reference, a loop, and the same
//! shapes sent to by-reference parameters; twice, so that the ops run from
//! the tape. Expected output from `/usr/bin/php8.5`.

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
fn element_references_match_php_src() {
    assert_eq!(
        run(r#"<?php
function t() {
  $a = ['k' => 1, 5 => 'five', 'n' => null];
  $r = &$a['k']; $r = 2; var_dump($a);
  $s = &$a['new']; var_dump($a); $s = 's'; var_dump($a['new']);
  $i = &$a[5]; $i .= '!'; $j = &$a['5']; $j .= '?'; var_dump($a[5]);
  $b = $a; $x = &$b['k']; $x = 99; var_dump($a['k'], $b['k']);
  $c = [1, 2, 3]; $e = &$c[1]; $e = 20; $f = &$c[10]; var_dump($c);
  $g = &$c['-1']; $h = &$c['01']; var_dump(array_keys($c));
  $u = &$undef['k']; var_dump($undef);
  $nul = null; $q = &$nul['k']; var_dump($nul);
  $str = 'abc'; try { $w = &$str[0]; } catch (Error $e2) { echo get_class($e2), ': ', $e2->getMessage(), "\n"; }
  $ao = new ArrayObject(['k' => 1]); $z = &$ao['k']; $z = 5; var_dump($ao['k']);
  $ref = ['k' => 1]; $alias = &$ref; $y = &$alias['k']; $y = 7; var_dump($ref);
  for ($n = 0; $n < 3; $n++) { $loop = &$a['k']; $loop++; } var_dump($a['k']);
  $lit = [1, 2]; $copy = $lit; $p = &$lit[0]; $p = 'p'; var_dump($lit, $copy);
}
function inc(&$x) { $x = ($x ?? 0) + 1; return $x; }
function setk(array &$arr, $k, $v) { $arr[$k] = $v; }
function u() {
  $a = ['k' => 1]; inc($a['k']); inc($a['k']); inc($a['missing']); inc($a['7']); var_dump($a);
  $b = $a; inc($b['k']); var_dump($a['k'], $b['k']);
  $n = ['x' => ['y' => 1]]; inc($n['x']['y']); setk($n['x'], 'z', 2); var_dump($n);
  $s = 'str'; try { inc($s[0]); } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
  inc($undef['q']); var_dump($undef);
  $o = new ArrayObject([]); inc($o['k']); var_dump(count($o));
}
t(); t(); u(); u();
"#).1,
        r#"array(3) {
  ["k"]=>
  &int(2)
  [5]=>
  string(4) "five"
  ["n"]=>
  NULL
}
array(4) {
  ["k"]=>
  &int(2)
  [5]=>
  string(4) "five"
  ["n"]=>
  NULL
  ["new"]=>
  &NULL
}
string(1) "s"
string(6) "five!?"
int(99)
int(99)
array(4) {
  [0]=>
  int(1)
  [1]=>
  &int(20)
  [2]=>
  int(3)
  [10]=>
  &NULL
}
array(6) {
  [0]=>
  int(0)
  [1]=>
  int(1)
  [2]=>
  int(2)
  [3]=>
  int(10)
  [4]=>
  int(-1)
  [5]=>
  string(2) "01"
}
array(1) {
  ["k"]=>
  &NULL
}
array(1) {
  ["k"]=>
  &NULL
}
Error: Cannot create references to/from string offsets
int(5)
array(1) {
  ["k"]=>
  &int(7)
}
int(102)
array(2) {
  [0]=>
  &string(1) "p"
  [1]=>
  int(2)
}
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
array(3) {
  ["k"]=>
  &int(2)
  [5]=>
  string(4) "five"
  ["n"]=>
  NULL
}
array(4) {
  ["k"]=>
  &int(2)
  [5]=>
  string(4) "five"
  ["n"]=>
  NULL
  ["new"]=>
  &NULL
}
string(1) "s"
string(6) "five!?"
int(99)
int(99)
array(4) {
  [0]=>
  int(1)
  [1]=>
  &int(20)
  [2]=>
  int(3)
  [10]=>
  &NULL
}
array(6) {
  [0]=>
  int(0)
  [1]=>
  int(1)
  [2]=>
  int(2)
  [3]=>
  int(10)
  [4]=>
  int(-1)
  [5]=>
  string(2) "01"
}
array(1) {
  ["k"]=>
  &NULL
}
array(1) {
  ["k"]=>
  &NULL
}
Error: Cannot create references to/from string offsets
int(5)
array(1) {
  ["k"]=>
  &int(7)
}
int(102)
array(2) {
  [0]=>
  &string(1) "p"
  [1]=>
  int(2)
}
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
array(3) {
  ["k"]=>
  int(3)
  ["missing"]=>
  int(1)
  [7]=>
  int(1)
}
int(3)
int(4)
array(1) {
  ["x"]=>
  array(2) {
    ["y"]=>
    int(2)
    ["z"]=>
    int(2)
  }
}
Error: Cannot create references to/from string offsets
array(1) {
  ["q"]=>
  int(1)
}
int(1)
array(3) {
  ["k"]=>
  int(3)
  ["missing"]=>
  int(1)
  [7]=>
  int(1)
}
int(3)
int(4)
array(1) {
  ["x"]=>
  array(2) {
    ["y"]=>
    int(2)
    ["z"]=>
    int(2)
  }
}
Error: Cannot create references to/from string offsets
array(1) {
  ["q"]=>
  int(1)
}
int(1)
"#
    );
}

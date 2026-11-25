// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Variadic parameters (`ZEND_RECV_VARIADIC`, collected without the
//! generic op where nothing can be checked) and returns without a value.

use php_vm::{Vm, VmOptions};
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
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    let output = String::from_utf8(output).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

/// Untyped, `mixed`, typed (coerced and rejected) and by-reference
/// variadics, none passed, named arguments collected into the list, a
/// spread with string keys, and `return;` / an implicit return from
/// functions without a type, `void` and `?int`. Expected output:
/// php-src's (PHP 8.4 CLI).
#[test]
fn variadics_and_valueless_returns() {
    let src = r#"<?php
namespace App;
function v(...$a) { return $a; } function m(mixed ...$a) { return count($a); } function t(int ...$a) { return $a; } function r(&...$a) { $a[0]++; } function k($x, ...$rest) { return [$x, $rest]; }
function vo(): void { return; } function no() { } function nn(): ?int { return null; }
var_dump(v(), v(1, 'a', [2]), m(1, 2, 3), t(1, '2'), k(1), k(1, 2, 3), v(...['a' => 1, 'b' => 2]), k(1, y: 2), vo(), no(), nn());
$z = 5; r($z); var_dump($z);
try { t(1, 'x'); } catch (\TypeError $e) { echo $e->getMessage(), "\n"; }
"#;
    let expected = r#"array(0) {
}
array(3) {
  [0]=>
  int(1)
  [1]=>
  string(1) "a"
  [2]=>
  array(1) {
    [0]=>
    int(2)
  }
}
int(3)
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
array(2) {
  [0]=>
  int(1)
  [1]=>
  array(0) {
  }
}
array(2) {
  [0]=>
  int(1)
  [1]=>
  array(2) {
    [0]=>
    int(2)
    [1]=>
    int(3)
  }
}
array(2) {
  ["a"]=>
  int(1)
  ["b"]=>
  int(2)
}
array(2) {
  [0]=>
  int(1)
  [1]=>
  array(1) {
    ["y"]=>
    int(2)
  }
}
NULL
NULL
NULL
int(6)
App\t(): Argument #2 must be of type int, string given, called in t.php on line 7
"#;
    assert_eq!(run(src), expected);
}

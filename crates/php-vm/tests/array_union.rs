// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `$cv += $array` (`ZEND_ASSIGN_OP` of two arrays), merged in place
//! without the place walk where nothing else can happen.

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

/// Overlapping string and int keys, copy-on-write of a shared array,
/// shared and unshared references on the right, `$a += $a`, a used
/// result, nested arrays, non-array operands on either side (the
/// `TypeError`), an undefined variable, a reference variable, a static
/// variable, a loop, and the global scope. Expected output: php-src's
/// (PHP 8.5 CLI).
#[test]
#[allow(clippy::too_many_lines)] // the script and its expected output
fn array_union_assignment() {
    let src = r#"<?php
namespace App;
function t() {
  $a = ['a' => 1, 0 => 'zero']; $shared = $a; $a += ['a' => 'no', 'b' => 2, 0 => 'no', 1 => 'one']; var_dump($a, $shared);
  $x = 'X'; $r = [&$x]; $s = [5 => 1]; $s += $r; $x = 'changed'; var_dump($s);
  $only = ['k' => 'v']; $tmp = ['k2' => &$only['k']]; $u = []; $u += $tmp; var_dump($u);
  $self = [1, 2]; $self += $self; var_dump($self);
  $b = ['q' => 1]; $res = ($b += ['w' => 2]); $res['e'] = 3; var_dump($b, $res);
  $n = [[1]]; $n += [[2], [3]]; var_dump($n);
  $i = 5; try { $i += [1]; } catch (\TypeError $e) { echo $e->getMessage(), "\n"; }
  $c = [1]; try { $c += 5; } catch (\TypeError $e) { echo $e->getMessage(), "\n"; }
  try { @$undef += ['z' => 1]; var_dump($undef); } catch (\Throwable $e) { echo get_class($e), "\n"; }
  $orig = ['o' => 1]; $ref = &$orig; $ref += ['p' => 2]; var_dump($orig);
  $f = function () { static $st = []; $st += ['s' => 1]; return $st; }; var_dump($f(), $f());
  $big = []; for ($j = 0; $j < 3; $j++) { $big += [$j => $j * 10, "k$j" => $j]; } var_dump($big);
}
t();
$g = ['g' => 1]; $g += ['h' => 2]; var_dump($g);
"#;
    let expected = r#"array(4) {
  ["a"]=>
  int(1)
  [0]=>
  string(4) "zero"
  ["b"]=>
  int(2)
  [1]=>
  string(3) "one"
}
array(2) {
  ["a"]=>
  int(1)
  [0]=>
  string(4) "zero"
}
array(2) {
  [5]=>
  int(1)
  [0]=>
  &string(7) "changed"
}
array(1) {
  ["k2"]=>
  &string(1) "v"
}
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
array(2) {
  ["q"]=>
  int(1)
  ["w"]=>
  int(2)
}
array(3) {
  ["q"]=>
  int(1)
  ["w"]=>
  int(2)
  ["e"]=>
  int(3)
}
array(2) {
  [0]=>
  array(1) {
    [0]=>
    int(1)
  }
  [1]=>
  array(1) {
    [0]=>
    int(3)
  }
}
Unsupported operand types: int + array
Unsupported operand types: array + int
TypeError
array(2) {
  ["o"]=>
  int(1)
  ["p"]=>
  int(2)
}
array(1) {
  ["s"]=>
  int(1)
}
array(1) {
  ["s"]=>
  int(1)
}
array(6) {
  [0]=>
  int(0)
  ["k0"]=>
  int(0)
  [1]=>
  int(10)
  ["k1"]=>
  int(1)
  [2]=>
  int(20)
  ["k2"]=>
  int(2)
}
array(2) {
  ["g"]=>
  int(1)
  ["h"]=>
  int(2)
}
"#;
    assert_eq!(run(src), expected);
}

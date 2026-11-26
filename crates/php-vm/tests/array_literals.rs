// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Array literals whose elements are no constants, built by the tape's
//! element handlers (one per kind of key: the next index, a string key as
//! it is, any other literal key) and released: element order, a key given
//! twice (the later value in the first one's place), integer keys in
//! digits, the next index after a negative key, nested literals, spread, a
//! by-reference element, the order destructors run in, the "next element
//! is already occupied" error, an undefined variable, literals past the
//! smallest table size, the collector's counts and `memory_get_usage()`.
//! The expected output is `/usr/bin/php8.5 -n` on each script (ref:
//! Zend/zend_compile.c `zend_compile_array`, Zend/zend_vm_def.h
//! `ZEND_INIT_ARRAY`, `ZEND_ADD_ARRAY_ELEMENT`, Zend/zend_hash.c).

#![allow(clippy::needless_raw_string_hashes)]

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

/// The scripts read `memory_get_usage()`, and the counting allocator is
/// the process's: a script running in another test thread at the same
/// time would be counted with them. One script at a time.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn run(source: &str) -> String {
    let _alone = ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let sink = Sink::default();
    let mut vm = common::vm_with_stdlib(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

const LITERALS: &str = r#"<?php
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "d{$this->n} "; } }
function f($i) {
    $x = 'xyz';
    $a = ['id' => $i, 'name' => $x, 'n' => 3];
    $b = ['a' => $i, 'b' => $x, 'a' => $i + 10];
    $c = ['5' => $i, '-3' => $x, '05' => 1, 'k' => $i, $i];
    $d = [-5 => $i, $x];
    $e = [$i, [$i, $x, ['k' => $i]], 'z' => [$x]];
    $g = [...[$i, $x], 'q' => $i, ...['r' => $x]];
    var_dump($a, $b, $c, $d, $e, $g);
    $r = 1;
    $h = ['ref' => &$r, $i];
    $r = 99;
    var_dump($h);
    $o = [new D(1), 'k' => new D(2), new D(3)];
    $o = null;
    echo "\n";
    $p = ['x' => new D(4), 'x' => new D(5)];
    echo "after dup\n";
    $p = null;
    echo "\n";
    $m = [PHP_INT_MAX => $i];
    try { $m = [PHP_INT_MAX => $i, $x]; } catch (Error $err) { echo $err->getMessage(), "\n"; }
    $u = ['k' => @$undef, $i];
    var_dump($u);
    $cnt = [];
    for ($j = 0; $j < 3; $j++) { $cnt[] = ['j' => $j, 's' => "v$j"]; }
    var_dump($cnt);
    $s = str_repeat('a', 3);
    $big = ['a' => $s, 'b' => $s, 'c' => $s, 'd' => $s, 'e' => $s, 'f' => $s, 'g' => $s, 'h' => $s, 'i' => $s];
    var_dump(count($big), array_keys($big));
    $l9 = [$i, $i, $i, $i, $i, $i, $i, $i, $s];
    var_dump($l9);
}
f(7);
echo "done\n";
"#;

const LITERALS_OUT: &str = r#"array(3) {
  ["id"]=>
  int(7)
  ["name"]=>
  string(3) "xyz"
  ["n"]=>
  int(3)
}
array(2) {
  ["a"]=>
  int(17)
  ["b"]=>
  string(3) "xyz"
}
array(5) {
  [5]=>
  int(7)
  [-3]=>
  string(3) "xyz"
  ["05"]=>
  int(1)
  ["k"]=>
  int(7)
  [6]=>
  int(7)
}
array(2) {
  [-5]=>
  int(7)
  [-4]=>
  string(3) "xyz"
}
array(3) {
  [0]=>
  int(7)
  [1]=>
  array(3) {
    [0]=>
    int(7)
    [1]=>
    string(3) "xyz"
    [2]=>
    array(1) {
      ["k"]=>
      int(7)
    }
  }
  ["z"]=>
  array(1) {
    [0]=>
    string(3) "xyz"
  }
}
array(4) {
  [0]=>
  int(7)
  [1]=>
  string(3) "xyz"
  ["q"]=>
  int(7)
  ["r"]=>
  string(3) "xyz"
}
array(2) {
  ["ref"]=>
  &int(99)
  [0]=>
  int(7)
}
d1 d2 d3 
d4 after dup
d5 
Cannot add element to the array as the next element is already occupied
array(2) {
  ["k"]=>
  NULL
  [0]=>
  int(7)
}
array(3) {
  [0]=>
  array(2) {
    ["j"]=>
    int(0)
    ["s"]=>
    string(2) "v0"
  }
  [1]=>
  array(2) {
    ["j"]=>
    int(1)
    ["s"]=>
    string(2) "v1"
  }
  [2]=>
  array(2) {
    ["j"]=>
    int(2)
    ["s"]=>
    string(2) "v2"
  }
}
int(9)
array(9) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
  [2]=>
  string(1) "c"
  [3]=>
  string(1) "d"
  [4]=>
  string(1) "e"
  [5]=>
  string(1) "f"
  [6]=>
  string(1) "g"
  [7]=>
  string(1) "h"
  [8]=>
  string(1) "i"
}
array(9) {
  [0]=>
  int(7)
  [1]=>
  int(7)
  [2]=>
  int(7)
  [3]=>
  int(7)
  [4]=>
  int(7)
  [5]=>
  int(7)
  [6]=>
  int(7)
  [7]=>
  int(7)
  [8]=>
  string(3) "aaa"
}
done
"#;

/// Literals of every kind of key, built and released inside a function
/// (so the tape runs them), against php8.5's output.
#[test]
fn literals_build_and_release_as_php_does() {
    assert_eq!(run(LITERALS), LITERALS_OUT);
}

/// Cycles through literals: an object holding a literal that holds it,
/// and a list that holds a reference to itself, are found by the
/// collector (`gc_collect_cycles()` and `gc_status()` as php8.5 counts).
#[test]
fn literal_cycles_are_collected() {
    let src = r#"<?php
function g($i) {
    $o = new stdClass;
    $cyc = ['o' => $o, $i];
    $o->a = $cyc;
    $o->self = $o;
    unset($o, $cyc);
    $r = [$i];
    $r[] = &$r;
    unset($r);
    return gc_collect_cycles();
}
echo g(1), g(2), "\n";
$s = gc_status();
echo $s['runs'], " ", $s['collected'], "\n";
"#;
    assert_eq!(run(src), "33\n2 6\n");
}

/// A literal costs the same memory each time it is built, and building
/// and releasing many leaves nothing behind (php8.5: `same 0`).
#[test]
fn literal_memory_is_returned() {
    let src = r#"<?php
function lit($i) { return ['id' => $i, 'name' => "n$i", 'n' => 3, $i]; }
function list3($i) { return [$i, $i + 1, "s$i"]; }
for ($j = 0; $j < 3; $j++) { $w = lit($j); $w = list3($j); }
unset($w);
$m0 = memory_get_usage();
$a = lit(1);
$d1 = memory_get_usage() - $m0;
$m0 = memory_get_usage();
$b = lit(2);
$d2 = memory_get_usage() - $m0;
unset($a, $b);
$m0 = memory_get_usage();
for ($j = 0; $j < 50; $j++) { $x = lit($j); $y = list3($j); }
unset($x, $y);
$d3 = memory_get_usage() - $m0;
echo $d1 === $d2 ? "same" : "differ $d1 $d2", " ", $d3, "\n";
"#;
    assert_eq!(run(src), "same 0\n");
}

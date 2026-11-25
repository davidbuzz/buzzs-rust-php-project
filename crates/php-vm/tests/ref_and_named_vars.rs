// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Reference assignments and variables looked up by name in functions whose
//! symbol table is not attached: `$a = &$b` between compiled variables (and
//! from elements, properties, `static`/`global` variables, functions
//! returning by reference), `foreach (... as &$v)`, `compact()` and
//! `extract()` with their flags. These take paths that touch only the
//! frame's slots; the expected output is `/usr/bin/php8.5 -n` on each
//! script (ref: Zend/zend_vm_def.h `ZEND_ASSIGN_REF`, `ZEND_FE_FETCH_RW`;
//! Zend/zend_execute.c `zend_assign_to_variable_reference`;
//! ext/standard/array.c `compact`, `extract`).

#![allow(clippy::needless_raw_string_hashes)]

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
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

/// `$a = &$b` on plain variables, elements, properties, `static` and `global` variables: binding, rebinding, `unset()`, destructors of released values, typed property references, "Only variables should be assigned by reference".
#[test]
fn assign_ref_between_variables() {
    assert_eq!(
        run(r#"<?php
class D { function __construct(public $n) {} function __destruct() { echo "~D{$this->n}\n"; } }
class T { public int $p = 1; public array $arr = [1, 2]; }
function f() {
  $a = 1; $b = &$a; $b = 2; var_dump($a);
  unset($b); var_dump($a); $b = 3; var_dump($a);
  $x = new D(1); $y = 5; $x = &$y; echo "after rebind\n"; var_dump($x);
  $d = new D(2); $dd = &$d; $d = &$y; echo "still held\n"; unset($dd); echo "released\n";
  $c = &$c; var_dump($c);
  $undef = null; unset($undef); $u = &$undef; var_dump($u, $undef);
  $arr = [1, 2, 3]; $copy = $arr; $e = &$arr[1]; $e = 20; var_dump($arr, $copy);
  $o = new T; $r = &$o->p; try { $r = "x"; } catch (TypeError $t) { echo $t->getMessage(), "\n"; } var_dump($o->p);
  $q = &$o->arr; $q[] = 3; var_dump($o->arr);
  $chain = 1; $c1 = &$chain; $c2 = &$c1; $c2 = 9; var_dump($chain);
  var_dump(array_keys(get_defined_vars()));
  $res = ($z = &$chain); $res = 0; var_dump($chain, $z);
  echo "end\n";
}
function g() { return 5; }
function &h() { static $s = 0; $s++; return $s; }
function k() { $v = &h(); var_dump($v); $w = &h(); var_dump($w); $nv = &g(); var_dump($nv); }
function st() { static $s = 0; $l = 7; $s = &$l; $s++; var_dump($l); }
function gl() { global $G; $loc = 'L'; $G = &$loc; $G = 'M'; var_dump($loc, $GLOBALS['G']); }
$G = 'g';
f(); k(); st(); st(); gl(); var_dump($G);
$top = 1; $tr = &$top; $tr = 2; var_dump($top);
"#),
        r#"int(2)
int(2)
int(2)
~D1
after rebind
int(5)
still held
~D2
released
NULL
NULL
NULL
array(3) {
  [0]=>
  int(1)
  [1]=>
  &int(20)
  [2]=>
  int(3)
}
array(3) {
  [0]=>
  int(1)
  [1]=>
  int(2)
  [2]=>
  int(3)
}
Cannot assign string to reference held by property T::$p of type int
int(1)
array(3) {
  [0]=>
  int(1)
  [1]=>
  int(2)
  [2]=>
  int(3)
}
int(9)
array(18) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
  [2]=>
  string(1) "x"
  [3]=>
  string(1) "y"
  [4]=>
  string(1) "d"
  [5]=>
  string(1) "c"
  [6]=>
  string(5) "undef"
  [7]=>
  string(1) "u"
  [8]=>
  string(3) "arr"
  [9]=>
  string(4) "copy"
  [10]=>
  string(1) "e"
  [11]=>
  string(1) "o"
  [12]=>
  string(1) "r"
  [13]=>
  string(1) "t"
  [14]=>
  string(1) "q"
  [15]=>
  string(5) "chain"
  [16]=>
  string(2) "c1"
  [17]=>
  string(2) "c2"
}
int(9)
int(9)
end
int(1)
int(2)

Notice: Only variables should be assigned by reference in t.php on line 21
int(5)
int(8)
int(8)
string(1) "M"
string(1) "g"
string(1) "g"
int(2)
"#
    );
}

/// `foreach (... as &$v)` over arrays (copy-on-write, a temporary, nested, through a reference) and object properties.
#[test]
fn foreach_by_reference() {
    assert_eq!(
        run(r#"<?php
class P { public $a = 1; public $b = 2; }
function fe() {
  $arr = [1, 2, 3]; $copy = $arr;
  foreach ($arr as &$v) { $v *= 10; } unset($v);
  var_dump($arr, $copy);
  $arr2 = ['x' => 1, 'y' => 2];
  foreach ($arr2 as $k => &$v) { $v = "$k$v"; }
  $v = 'last'; var_dump($arr2);
  foreach ([1, 2] as &$t) { $t++; } var_dump($t);
  $o = new P; foreach ($o as $name => &$pv) { $pv = $name; } unset($pv); var_dump($o);
  $nest = [[1], [2]]; foreach ($nest as &$row) { $row[] = 0; } unset($row); var_dump($nest);
  $g = [5]; $alias = &$g; foreach ($g as &$e) { $e = 6; } unset($e); var_dump($alias);
}
fe();
"#),
        r#"array(3) {
  [0]=>
  int(10)
  [1]=>
  int(20)
  [2]=>
  int(30)
}
array(3) {
  [0]=>
  int(1)
  [1]=>
  int(2)
  [2]=>
  int(3)
}
array(2) {
  ["x"]=>
  string(2) "x1"
  ["y"]=>
  &string(4) "last"
}
int(3)
object(P)#1 (2) {
  ["a"]=>
  string(1) "a"
  ["b"]=>
  string(1) "b"
}
array(2) {
  [0]=>
  array(2) {
    [0]=>
    int(1)
    [1]=>
    int(0)
  }
  [1]=>
  array(2) {
    [0]=>
    int(2)
    [1]=>
    int(0)
  }
}
array(1) {
  [0]=>
  int(6)
}
"#
    );
}

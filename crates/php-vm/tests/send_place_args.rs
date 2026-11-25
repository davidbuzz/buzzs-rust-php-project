// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Call arguments that are properties or array elements (`f($this->p)`, `g($a['k'])`,
//! `h($this->p[$k])`, `$list[$i]->name`, `f()['k']`), which the tape, the hot loop and
//! `Vm::op_send` read with the specialised fetch ops' hits when the parameter is by value,
//! next to the by-reference sends and the misses that must keep the generic place walk. The
//! expected output is `/usr/bin/php8.5 -n` on each script (ref: Zend/zend_compile.c
//! `zend_compile_args`, Zend/zend_vm_def.h `ZEND_FETCH_DIM_FUNC_ARG`,
//! `ZEND_FETCH_OBJ_FUNC_ARG`, `ZEND_SEND_VAR`).

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

/// Every shape a call argument takes the hit for, sent by value to builtins and to a user
/// function, the loop running each call again on filled caches.
#[test]
fn hits() {
    assert_eq!(
        run(r##"<?php
class P { public $p = 'AbC'; public $l = ['x' => [1, 2, 3], 'y' => [[4, 5], [6]]]; public $o; public $n = 5;
  function __construct() { $this->o = new Q; }
  function run() {
    $a = ['k' => [1, 2], 's' => 'Xy', 'n' => 5, 'x' => ['y' => 'deep', 'z' => [7, 8]]];
    $k = 'x'; $i = 1; $j = 0; $list = [new Q, new Q]; $q = new Q;
    for ($t = 0; $t < 3; $t++) {
      echo strtolower($this->p), ' ', count($a['k']), ' ', strlen($a['s']), ' ', count($this->l[$k]), ' ',
        max($a['n'], 3), ' ', abs($this->n), "\n";
      echo implode(',', array_keys($this->l)), ' ', $this->l['y'][$i][$j], ' ', count($this->l['y'][$j]), "\n";
      echo strtoupper($a['x']['y']), ' ', count($a['x']['z']), ' ', $a[$k]['z'][$i], ' ', strrev($q->name), "\n";
      echo strrev($list[$i]->name), ' ', strrev($this->o->name), ' ', ucfirst($a[$k]['y']), ' ', count($this->l[$k . ''] ), "\n";
      echo user($this->p, $a['k']), user($a['s'], $this->l['y'][$i]), user($list[$j]->name, $q->name), "\n";
      echo strlen(self::mk()['k']), ' ', strlen(shared()['k']), ' ', count(shared()['l']), "\n";
    }
  }
  static function mk() { return ['k' => str_repeat('z', 4)]; }
}
class Q { public $name = 'qname'; }
function shared() { static $s = ['k' => 'shared', 'l' => [1, 2, 3]]; return $s; }
function user($x, $y) { return '[' . (is_array($x) ? count($x) : $x) . '|' . (is_array($y) ? count($y) : $y) . ']'; }
(new P)->run();
"##),
        r##"abc 2 2 3 5 5
x,y 6 2
DEEP 2 8 emanq
emanq emanq Deep 3
[AbC|2][Xy|1][qname|qname]
4 6 3
abc 2 2 3 5 5
x,y 6 2
DEEP 2 8 emanq
emanq emanq Deep 3
[AbC|2][Xy|1][qname|qname]
4 6 3
abc 2 2 3 5 5
x,y 6 2
DEEP 2 8 emanq
emanq emanq Deep 3
[AbC|2][Xy|1][qname|qname]
4 6 3
"##
    );
}

/// By-reference sends of the same shapes (they write through), and the misses that keep
/// the generic walk: `__get`, `ArrayAccess`, string offsets, null bases, undefined keys and
/// variables and their warnings in order, hooked, private, typed and referenced properties.
#[test]
fn misses_and_by_ref() {
    assert_eq!(
        run(r##"<?php
class M { public $list = [3, 1, 2]; public $m; public $arr = ['k' => [1]]; public $s = 'str'; public $nul = null;
  private $priv = 'pv'; public int $typed; public $ref;
  public $hooked { get => 'hooked:' . strlen('ab'); }
  function __get($n) { echo "__get($n)\n"; return 'magic'; }
  function run() {
    $a = ['k' => [9]]; $ao = new ArrayObject(['k' => 'ao']); $str = 'hello'; $nu = null; $v = 1;
    $this->ref = &$v; $e = ['r' => 0]; $e['r'] = &$v;
    for ($t = 0; $t < 2; $t++) {
      sort($this->list); echo implode(',', $this->list), "\n";
      preg_match('/(b)/', 'abc', $this->m); echo $this->m[1], "\n";
      array_push($a['k'], $t); echo count($a['k']), "\n";
      byref($this->arr['k']); echo count($this->arr['k']), "\n";
      byref($a['new' . $t]); var_dump($a['new' . $t]);
      echo strlen($this->nope), "\n";
      echo strlen($a['missing']), strlen($a['m2']), "\n";
      echo strlen($ao['k']), ' ', strlen($str[1]), ' ', strlen($str[9] ?? 'd'), "\n";
      echo strlen($nu['x']), ' ', strlen($this->nul['x']), ' ', strlen($this->s[0]), "\n";
      echo strlen($this->hooked), ' ', strlen($this->priv), ' ', $this->ref, ' ', $e['r'], "\n";
      try { echo strlen($this->typed); } catch (Error $x) { echo $x->getMessage(), "\n"; }
      echo user2($undef['x'], $a['zz']), "\n";
      $v++;
    }
  }
}
function byref(&$x) { $x[] = 1; }
function user2($x, $y) { return var_export([$x, $y], true); }
(new M)->run();
"##),
        r##"1,2,3
b
2
2
array(1) {
  [0]=>
  int(1)
}
__get(nope)
5

Warning: Undefined array key "missing" in t.php on line 16

Deprecated: strlen(): Passing null to parameter #1 ($string) of type string is deprecated in t.php on line 16
0
Warning: Undefined array key "m2" in t.php on line 16

Deprecated: strlen(): Passing null to parameter #1 ($string) of type string is deprecated in t.php on line 16
0
2 1 1

Warning: Trying to access array offset on null in t.php on line 18

Deprecated: strlen(): Passing null to parameter #1 ($string) of type string is deprecated in t.php on line 18
0 
Warning: Trying to access array offset on null in t.php on line 18

Deprecated: strlen(): Passing null to parameter #1 ($string) of type string is deprecated in t.php on line 18
0 1
8 2 1 1
Typed property M::$typed must not be accessed before initialization

Warning: Undefined variable $undef in t.php on line 21

Warning: Trying to access array offset on null in t.php on line 21

Warning: Undefined array key "zz" in t.php on line 21
array (
  0 => NULL,
  1 => NULL,
)
1,2,3
b
3
3
array(1) {
  [0]=>
  int(1)
}
__get(nope)
5

Warning: Undefined array key "missing" in t.php on line 16

Deprecated: strlen(): Passing null to parameter #1 ($string) of type string is deprecated in t.php on line 16
0
Warning: Undefined array key "m2" in t.php on line 16

Deprecated: strlen(): Passing null to parameter #1 ($string) of type string is deprecated in t.php on line 16
0
2 1 1

Warning: Trying to access array offset on null in t.php on line 18

Deprecated: strlen(): Passing null to parameter #1 ($string) of type string is deprecated in t.php on line 18
0 
Warning: Trying to access array offset on null in t.php on line 18

Deprecated: strlen(): Passing null to parameter #1 ($string) of type string is deprecated in t.php on line 18
0 1
8 2 2 2
Typed property M::$typed must not be accessed before initialization

Warning: Undefined variable $undef in t.php on line 21

Warning: Trying to access array offset on null in t.php on line 21

Warning: Undefined array key "zz" in t.php on line 21
array (
  0 => NULL,
  1 => NULL,
)
"##
    );
}

/// Refcounts seen by `debug_zval_dump()` in the callee, copy-on-write of a sent element,
/// the destructor order when the argument is the last handle (an element, a property, an
/// element of a temporary), a referenced element sent by value, and evaluation order.
#[test]
fn refcounts_cow_and_destructors() {
    assert_eq!(
        run(r##"<?php
class D { public $id; function __construct($id) { $this->id = $id; } function __destruct() { echo "dtor {$this->id}\n"; } }
class R { public $arr; public $o;
  function run() {
    for ($t = 0; $t < 2; $t++) {
      $this->arr = ['k' => [1, 2], 's' => str_repeat('s', 3)];
      $a = ['k' => [1, 2]];
      dz($a['k']); dz($this->arr['k']);
      mod($a['k']); mod($this->arr['k']); echo count($a['k']), count($this->arr['k']), "\n";
      $b = $a['k']; $a['k'][] = 3; echo count($b), count($a['k']), "\n";
      $r = ['o' => new D("a$t")];
      take($r['o'], $r = null);
      echo "after take\n";
      $this->o = new D("p$t");
      take($this->o, $this->o = null);
      echo "after prop\n";
      echo take(mkd($t)['o'], 'x'), "\n";
      echo "after tmp\n";
      $v = 'refd'; $w = ['x' => &$v]; viaref($w['x']); echo $v, "\n";
      order($a[ord1()], $this->arr[ord2()]);
    }
  }
}
function mkd($t) { return ['o' => new D("t$t")]; }
function dz($x) { debug_zval_dump($x); }
function mod($x) { $x[] = 9; return $x; }
function take($x, $y) { echo "take {$x->id}\n"; return 'ret'; }
function viaref($x) { $x = 'changed'; }
function ord1() { echo "ord1\n"; return 'k'; }
function ord2() { echo "ord2\n"; return 'nokey'; }
function order($x, $y) { echo "order ", count($x), var_export($y, true), "\n"; }
(new R)->run();
echo "end\n";
"##),
        r##"array(2) packed refcount(3){
  [0]=>
  int(1)
  [1]=>
  int(2)
}
array(2) packed refcount(4){
  [0]=>
  int(1)
  [1]=>
  int(2)
}
22
23
take a0
dtor a0
after take
take p0
dtor p0
after prop
take t0
dtor t0
ret
after tmp
refd
ord1
ord2

Warning: Undefined array key "nokey" in t.php on line 20
order 3NULL
array(2) packed refcount(4){
  [0]=>
  int(1)
  [1]=>
  int(2)
}
array(2) packed refcount(4){
  [0]=>
  int(1)
  [1]=>
  int(2)
}
22
23
take a1
dtor a1
after take
take p1
dtor p1
after prop
take t1
dtor t1
ret
after tmp
refd
ord1
ord2

Warning: Undefined array key "nokey" in t.php on line 20
order 3NULL
end
"##
    );
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Nested element and property accesses that take the frame-local fast
//! paths (`unset($a[$k][$j])`, `unset($this->p[$k])`, ...) next to the
//! cases that must still take the generic place walk: auto-vivification,
//! warnings and their order, string offsets, scalars used as arrays,
//! references, copy-on-write separation, `ArrayAccess`, magic methods,
//! readonly and typed properties. The expected output is
//! `/usr/bin/php8.5 -n` on each script (ref: Zend/zend_vm_def.h
//! `ZEND_UNSET_DIM`, `ZEND_FETCH_DIM_UNSET`, `ZEND_FETCH_OBJ_UNSET`,
//! `ZEND_FETCH_OBJ_W`, `ZEND_FETCH_DIM_W`, `ZEND_ASSIGN_DIM`, `ZEND_FETCH_OBJ_IS`,
//! `ZEND_ISSET_ISEMPTY_DIM_OBJ`, `ZEND_PRE_INC`).

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

/// Reads, `isset()`/`empty()`, assignments and appends of `$this->p[$k]` (the frame-local hits of the `*ObjPropDim` ops).
#[test]
fn prop_dim1() {
    assert_eq!(
        run(r#"<?php
class AA implements ArrayAccess {
  public function offsetExists($o): bool { echo "offsetExists $o\n"; return true; }
  public function offsetGet($o): mixed { echo "offsetGet $o\n"; return 7; }
  public function offsetSet($o, $v): void { echo "offsetSet ", var_export($o, true), "\n"; }
  public function offsetUnset($o): void {}
}
class P {
  public $a = ['k' => 1, 5 => 'five', 'z' => null, 'e' => '', 'arr' => []];
  public $n = null;
  public $list = [];
  public $full = [PHP_INT_MAX => 1];
  public array $typed = [];
  public ?array $nt = null;
  public int $ti;
  public $obj;
  public $prio = [3, 4];
  public $iter = [];
  public $s = 'str';
  function __construct() { $this->obj = new AA; }
  function __isset($n) { echo "__isset $n\n"; return true; }
  function __get($n) { echo "__get $n\n"; return [1, 2]; }
  function run() {
    $k = 'k'; $five = '5'; $i = 5; $m = 'missing';
    var_dump($this->a[$k], $this->a[$five], $this->a[$i], $this->a['z'], $this->a[$k . '']);
    var_dump(isset($this->a[$k]), isset($this->a['z']), isset($this->a[$m]), empty($this->a['e']), empty($this->a['arr']), empty($this->a[$m]), isset($this->n['x']), empty($this->n['x']));
    var_dump($this->a[$m]);
    var_dump($this->n['x']);
    var_dump($this->s[0], isset($this->s[1]), isset($this->s['x']));
    var_dump(isset($this->ti[0]), $this->obj[3], isset($this->obj[4]));
    $this->obj[5] = 1; $this->obj[] = 2;
    $x = $this->a['new'] = 'v'; var_dump($x);
    $this->a[$k . 'x'] = [1]; $this->a[$five] = 'FIVE';
    for ($j = 0; $j < 3; $j++) { $this->list[] = $j; $this->iter[$j] = $this->prio; }
    $this->prio[] = 9;
    var_dump($this->a, $this->list, $this->iter);
    try { $this->full[] = 2; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    $this->typed['t'] = 1; $this->nt['q'] = 2; var_dump($this->typed, $this->nt);
    try { $this->ti[0] = 1; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    try { $this->s[0] = 'X'; var_dump($this->s); $this->s[] = 'y'; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    $copy = $this->a; $this->a['k'] = 2; var_dump($copy['k'], $this->a['k']);
    $r = &$this->list; $this->list[] = 'viaref'; var_dump(count($r)); unset($r);
    $v = 1; $this->a['refd'] = &$v; $this->a['refd'] = 2; var_dump($v, $this->a['refd'], isset($this->a['refd']));
    $o = new stdClass; $this->a['o'] = $o; $this->a['o'] = 1; var_dump($o);
    $this->a[1.5] = 'f'; $this->a[true] = 't'; $this->a[null] = 'n'; var_dump($this->a[1], $this->a['']);
  }
}
(new P)->run();
$p = new P; $q = &$p; var_dump($q->a['k'], isset($q->a['k'])); $q->a['k'] = 3; $q->list[] = 1; var_dump($p->a['k'], $p->list);
function inner(P $p) { return $p->a['k']; } var_dump(inner($p));
class Q extends P {} $qq = new Q; var_dump($qq->a['k']); $qq->a['k'] = 9; var_dump($qq->a['k'], inner($qq), inner($p));
"#),
        r#"int(1)
string(4) "five"
string(4) "five"
NULL
int(1)
bool(true)
bool(false)
bool(false)
bool(true)
bool(true)
bool(true)
bool(false)
bool(true)

Warning: Undefined array key "missing" in t.php on line 27
NULL

Warning: Trying to access array offset on null in t.php on line 28
NULL
string(1) "s"
bool(true)
bool(false)
offsetGet 3
offsetExists 4
bool(false)
int(7)
bool(true)
offsetSet 5
offsetSet NULL
string(1) "v"
array(7) {
  ["k"]=>
  int(1)
  [5]=>
  string(4) "FIVE"
  ["z"]=>
  NULL
  ["e"]=>
  string(0) ""
  ["arr"]=>
  array(0) {
  }
  ["new"]=>
  string(1) "v"
  ["kx"]=>
  array(1) {
    [0]=>
    int(1)
  }
}
array(3) {
  [0]=>
  int(0)
  [1]=>
  int(1)
  [2]=>
  int(2)
}
array(3) {
  [0]=>
  array(2) {
    [0]=>
    int(3)
    [1]=>
    int(4)
  }
  [1]=>
  array(2) {
    [0]=>
    int(3)
    [1]=>
    int(4)
  }
  [2]=>
  array(2) {
    [0]=>
    int(3)
    [1]=>
    int(4)
  }
}
Cannot add element to the array as the next element is already occupied
array(1) {
  ["t"]=>
  int(1)
}
array(1) {
  ["q"]=>
  int(2)
}
Cannot auto-initialize an array inside property P::$ti of type int
string(3) "Xtr"
[] operator not supported for strings
int(1)
int(2)
int(4)
int(2)
int(2)
bool(true)
object(stdClass)#4 (0) {
}

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 45

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 45
string(1) "t"
string(1) "n"
int(1)
bool(true)
int(3)
array(1) {
  [0]=>
  int(1)
}
int(3)
int(1)
int(9)
int(9)
int(3)
"#
    );
}

/// Reads, `isset()`/`empty()` and assignments of `$this->p[$a][$b]`.
#[test]
fn prop_dims() {
    assert_eq!(
        run(r#"<?php
class AA implements ArrayAccess {
  public $d = [];
  public function offsetExists($o): bool { echo "offsetExists $o\n"; return isset($this->d[$o]); }
  public function offsetGet($o): mixed { echo "offsetGet $o\n"; return $this->d[$o] ?? null; }
  public function offsetSet($o, $v): void { echo "offsetSet $o\n"; $this->d[$o] = $v; }
  public function offsetUnset($o): void { echo "offsetUnset $o\n"; }
}
class H {
  public $cb = [];
  public $cache = ['g' => ['k' => 'v', 'z' => null, 'e' => '', 'zero' => '0']];
  public ?array $na = null;
  public $f = false;
  public $s = 'abc';
  public $i = 5;
  public int $ti = 1;
  public readonly array $ro;
  public $aa;
  public $u;
  private $priv = ['a' => ['b' => 1]];
  function __construct() { $this->ro = ['a' => ['b' => 1]]; $this->aa = ['x' => new AA]; unset($this->u); }
  function __get($n) { echo "__get $n\n"; return ['m' => ['n' => 'magic']]; }
  function add($p, $idx, $fn) { $this->cb[$p][$idx] = ['function' => $fn, 'accepted_args' => 1]; }
  function has($p, $idx) { return isset($this->cb[$p][$idx]); }
  function run() {
    $g = 'g'; $k = 'k';
    var_dump($this->cache[$g][$k], $this->cache['g']['z'], $this->cache[$g]['nope'] ?? 'dflt');
    var_dump(isset($this->cache[$g][$k]), isset($this->cache[$g]['z']), isset($this->cache['x']['y']), empty($this->cache[$g]['e']), empty($this->cache[$g]['zero']), empty($this->cache[$g][$k]), empty($this->cache['q']['r']));
    var_dump($this->cache[$g]['nope']);
    var_dump($this->cache['nope'][$k]);
    var_dump($this->cache[$g][$k][0], isset($this->cache[$g][$k][0]), isset($this->cache[$g][$k]['x']));
    $copy = $this->cache;
    $this->cache[$g][$k] = 'w'; $this->cache['new']['deep'] = 1; $this->cache[$g]['z'] = [1];
    var_dump($copy['g']['k'], $this->cache);
    $this->na['a']['b'] = 1; var_dump($this->na);
    $this->f['a']['b'] = 1; var_dump($this->f);
    try { $this->s['a']['b'] = 1; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
    try { $this->i['a']['b'] = 1; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
    try { $this->ti['a']['b'] = 1; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
    try { $this->ro['a']['b'] = 2; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
    var_dump($this->ro['a']['b'], isset($this->ro['a']['b']));
    try { $this->cache[$g][$k]['x'] = 1; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
    $this->aa['x']['y'] = 5; var_dump($this->aa['x']['y'], isset($this->aa['x']['y']));
    var_dump($this->u['m']['n']); var_dump(isset($this->u['m']['n']));
    $this->dyn = ['a' => ['b' => 2]]; var_dump($this->dyn['a']['b'], isset($this->dyn['a']['c']));
    $this->dyn['a']['c'] = 3; var_dump($this->dyn);
    $r = &$this->cache['g']; $r['k'] = 'viaref'; var_dump($this->cache['g']['k']);
    $this->cache['g']['k'] = 'again'; var_dump($r['k']); unset($r);
    $x = 1; $this->cache['g']['ref'] = &$x; $this->cache['g']['ref'] = 2; var_dump($x, $this->cache['g']['ref']);
    $this->cache['1']['2'] = 'num'; var_dump($this->cache[1][2], isset($this->cache['1']['2']));
    var_dump($this->priv['a']['b']);
    $this->cb[10]['a'] = 1; foreach ($this->cb[10] as $kk => $vv) { $this->cb[10]['b'] = 2; echo "$kk\n"; } var_dump($this->cb[10]);
  }
}
$h = new H;
for ($i = 0; $i < 3; $i++) { $h->add(10, "f$i", "strlen"); $h->add(20, "g", "trim"); }
var_dump($h->has(10, 'f1'), $h->has(10, 'nope'), $h->has(30, 'x'), count($h->cb[10]));
$h->run();
try { var_dump($h->priv['a']['b']); } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
var_dump(isset($h->priv['a']['b']));
$h->cache['g']['outside'] = 1; var_dump($h->cache['g']['outside'], isset($h->cache['g']['outside']));
class G2 extends H { function m() { return $this->cache['g']['k']; } }
var_dump((new G2)->m());
"#),
        r#"bool(true)
bool(false)
bool(false)
int(3)
string(1) "v"
NULL
string(4) "dflt"
bool(true)
bool(false)
bool(false)
bool(true)
bool(true)
bool(false)
bool(true)

Warning: Undefined array key "nope" in t.php on line 29
NULL

Warning: Undefined array key "nope" in t.php on line 30

Warning: Trying to access array offset on null in t.php on line 30
NULL
string(1) "v"
bool(true)
bool(false)
string(1) "v"
array(2) {
  ["g"]=>
  array(4) {
    ["k"]=>
    string(1) "w"
    ["z"]=>
    array(1) {
      [0]=>
      int(1)
    }
    ["e"]=>
    string(0) ""
    ["zero"]=>
    string(1) "0"
  }
  ["new"]=>
  array(1) {
    ["deep"]=>
    int(1)
  }
}
array(1) {
  ["a"]=>
  array(1) {
    ["b"]=>
    int(1)
  }
}

Deprecated: Automatic conversion of false to array is deprecated in t.php on line 36
array(1) {
  ["a"]=>
  array(1) {
    ["b"]=>
    int(1)
  }
}
TypeError: Cannot access offset of type string on string
Error: Cannot use a scalar value as an array
Error: Cannot use a scalar value as an array
Error: Cannot indirectly modify readonly property H::$ro
int(1)
bool(true)
TypeError: Cannot access offset of type string on string
offsetSet y
offsetGet y
offsetExists y
int(5)
bool(true)
__get u
string(5) "magic"
__get u
bool(true)

Deprecated: Creation of dynamic property H::$dyn is deprecated in t.php on line 45
int(2)
bool(false)
array(1) {
  ["a"]=>
  array(2) {
    ["b"]=>
    int(2)
    ["c"]=>
    int(3)
  }
}
string(6) "viaref"
string(5) "again"
int(2)
int(2)
string(3) "num"
bool(true)
int(1)
f0
f1
f2
a
array(5) {
  ["f0"]=>
  array(2) {
    ["function"]=>
    string(6) "strlen"
    ["accepted_args"]=>
    int(1)
  }
  ["f1"]=>
  array(2) {
    ["function"]=>
    string(6) "strlen"
    ["accepted_args"]=>
    int(1)
  }
  ["f2"]=>
  array(2) {
    ["function"]=>
    string(6) "strlen"
    ["accepted_args"]=>
    int(1)
  }
  ["a"]=>
  int(1)
  ["b"]=>
  int(2)
}
__get priv

Warning: Undefined array key "a" in t.php on line 59

Warning: Trying to access array offset on null in t.php on line 59
NULL
__get priv
bool(false)
int(1)
bool(true)
string(1) "v"
"#
    );
}

/// `++`/`--` and compound assignments of elements of arrays held by reference (`global`, `static`, `use (&$x)`, by-reference parameters).
#[test]
fn ref_dims() {
    assert_eq!(
        run(r#"<?php
$counts = ['a' => 0, 'f' => 1.5, 's' => 'x', 'max' => PHP_INT_MAX, 'n' => null];
function bump($k) { global $counts; ++$counts[$k]; $counts[$k]++; return $counts[$k]--; }
var_dump(bump('a'), bump('f'), bump('s'), $counts);
function over() { global $counts; $counts['max']++; var_dump($counts['max']); }
over();
function miss() { global $counts; $counts['zz']++; var_dump($counts['zz']); ++$counts['n']; var_dump($counts['n']); }
miss();
function addto($k, $v) { global $counts; $counts[$k] += $v; $counts[$k] -= 1; $counts[$k] *= 2; return $counts[$k]; }
var_dump(addto('a', 10), addto('f', 0.5));
function divz() { global $counts; try { $counts['a'] %= 0; } catch (DivisionByZeroError $e) { echo $e->getMessage(), "\n"; } }
divz(); var_dump($counts['a']);
function stat_counter($k) { static $c = []; $c[$k] = ($c[$k] ?? 0); ++$c[$k]; $c[$k] += 10; return $c; }
stat_counter('x'); var_dump(stat_counter('x'));
function cow() { global $counts; $copy = $counts; ++$counts['a']; $counts['a'] += 5; var_dump($copy['a'], $counts['a']); }
cow();
$fn = function () use (&$counts) { ++$counts['a']; $counts['a'] += 1; return $counts['a']; };
var_dump($fn());
function byref(array &$a) { ++$a['a']; $a['a'] += 2; } byref($counts); var_dump($counts['a']);
function iter() { global $counts; foreach ($counts as $k => $v) { if ($k === 'a') { ++$counts['a']; } } var_dump($counts['a']); }
iter();
function iterref() { global $list; $list = [1, 2]; foreach ($list as &$v) { ++$list[1]; $list[0] += 1; } unset($v); var_dump($list); }
iterref();
$elref = ['x' => 1]; $rr = &$elref['x'];
function elemref() { global $elref; ++$elref['x']; $elref['x'] += 1; } elemref(); var_dump($rr, $elref);
"#),
        r#"
Deprecated: Increment on non-numeric string is deprecated, use str_increment() instead in t.php on line 3

Deprecated: Increment on non-numeric string is deprecated, use str_increment() instead in t.php on line 3

Deprecated: Decrement on non-numeric string has no effect and is deprecated in t.php on line 3
int(2)
float(3.5)
string(1) "z"
array(5) {
  ["a"]=>
  int(1)
  ["f"]=>
  float(2.5)
  ["s"]=>
  string(1) "z"
  ["max"]=>
  int(9223372036854775807)
  ["n"]=>
  NULL
}
float(9.223372036854776E+18)

Warning: Undefined array key "zz" in t.php on line 7
int(1)
int(1)
int(20)
float(4)
Modulo by zero
int(20)
array(1) {
  ["x"]=>
  int(22)
}
int(20)
int(26)
int(28)
int(31)
int(32)
array(2) {
  [0]=>
  int(3)
  [1]=>
  int(4)
}
int(3)
array(1) {
  ["x"]=>
  &int(3)
}
"#
    );
}

/// unset() of elements of a function variable, a reference and a global.
#[test]
fn unset_cv() {
    assert_eq!(
        run(r#"<?php
class Dtor { public function __destruct() { echo "dtor\n"; } }
function f() {
  $a = ['x' => ['y' => 1, 'z' => 2], 'n' => null, 's' => 'str', 'f' => false, 5 => [1], 'o' => new Dtor];
  $b = $a;
  unset($a['x']['y']); var_dump($a['x'], $b['x']);
  unset($a['missing']['y']); unset($a['n']['y']); unset($a['f']['y']);
  try { unset($a['s']['y']); } catch (Error $e) { echo $e->getMessage(), "\n"; }
  try { unset($a['s'][0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
  unset($b); echo "before\n"; unset($a['o']); echo "after\n";
  unset($a[5][0]); unset($a['5']); var_dump($a);
  unset($u['k']); unset($u['k']['j']);
  $i = 3; unset($a[$i]); $k = 'x'; unset($a[$k]['z']); var_dump($a);
  $r = &$a; unset($r['n']); var_dump(array_keys($a));
  $s = 'abc'; try { unset($s[0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
  $n = 5; try { unset($n[0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
  $l = [1, 2, 3]; foreach ($l as $kk => &$vv) { if ($kk == 0) unset($l[1]); echo $vv, "\n"; } unset($vv);
  $l = [1, 2, 3]; foreach ($l as $kk => $vv) { if ($kk == 0) unset($l[1]); echo $vv, "\n"; } var_dump($l);
  $d = ['a' => ['b' => ['c' => ['d' => 1, 'e' => 2]]]]; unset($d['a']['b']['c']['d']); var_dump($d);
  $q = ['k' => 1]; unset($q[$undef]); var_dump($q);
  $m = [1 => 'a', '01' => 'b']; unset($m['1']); var_dump($m);
}
f();
$g = ['a' => ['b' => 1, 'c' => 2]];
function gl() { global $g; $x = 'b'; unset($g['a'][$x]); }
gl(); var_dump($g);
unset($g['a']['c']); var_dump($g, $GLOBALS['g']);
"#),
        r#"array(1) {
  ["z"]=>
  int(2)
}
array(2) {
  ["y"]=>
  int(1)
  ["z"]=>
  int(2)
}

Deprecated: Automatic conversion of false to array is deprecated in t.php on line 7
Cannot unset string offsets
Cannot unset string offsets
before
dtor
after
array(4) {
  ["x"]=>
  array(1) {
    ["z"]=>
    int(2)
  }
  ["n"]=>
  NULL
  ["s"]=>
  string(3) "str"
  ["f"]=>
  bool(false)
}

Warning: Undefined variable $u in t.php on line 12

Warning: Undefined variable $u in t.php on line 12
array(4) {
  ["x"]=>
  array(0) {
  }
  ["n"]=>
  NULL
  ["s"]=>
  string(3) "str"
  ["f"]=>
  bool(false)
}
array(3) {
  [0]=>
  string(1) "x"
  [1]=>
  string(1) "s"
  [2]=>
  string(1) "f"
}
Cannot unset string offsets
Cannot unset offset in a non-array variable
1
3
1
2
3
array(2) {
  [0]=>
  int(1)
  [2]=>
  int(3)
}
array(1) {
  ["a"]=>
  array(1) {
    ["b"]=>
    array(1) {
      ["c"]=>
      array(1) {
        ["e"]=>
        int(2)
      }
    }
  }
}

Warning: Undefined variable $undef in t.php on line 20
array(1) {
  ["k"]=>
  int(1)
}
array(1) {
  ["01"]=>
  string(1) "b"
}
array(1) {
  ["a"]=>
  array(1) {
    ["c"]=>
    int(2)
  }
}
array(1) {
  ["a"]=>
  array(0) {
  }
}
array(1) {
  ["a"]=>
  array(0) {
  }
}
"#
    );
}

/// unset() of elements of array properties.
#[test]
fn unset_prop() {
    assert_eq!(
        run(r#"<?php
class O { public $c = ['g' => ['k' => 1, 'j' => 2]]; public readonly array $ro; public ?array $t = ['x' => 1]; private $p = ['a' => 1]; public $s = 'str';
  public int $i = 1; public $u;
  function __construct() { $this->ro = ['a' => [1]]; unset($this->u); }
  function m() { $g = 'g'; unset($this->c[$g]['k']); unset($this->c['g']['zz']); unset($this->c['nope']['k']); var_dump($this->c);
    try { unset($this->ro['a'][0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
    unset($this->t['x']); unset($this->p['a']); var_dump($this->t, $this->p);
    try { unset($this->s[0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
    try { unset($this->i[0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }

    $copy = $this->c; unset($this->c[$g]); var_dump($this->c, $copy);
    $r = &$this->c; $r['z'] = [1, 2]; unset($this->c['z'][0]); var_dump($r);
  }
  function __get($n) { echo "__get $n\n"; return null; }
}
(new O)->m();
$o = new O; unset($o->c['g']['j']); var_dump($o->c);
class P { public $list = []; function add($k) { $this->list[$k][] = 1; } function del($k) { unset($this->list[$k][0]); if (!$this->list[$k]) unset($this->list[$k]); } }
$p = new P; for ($i = 0; $i < 3; $i++) { $p->add('a'); $p->add('b'); $p->del('a'); } var_dump($p->list);
$x = new stdClass; $x->d = ['a' => 1]; unset($x->d['a']); var_dump($x);
class AA implements ArrayAccess { public function offsetExists($o): bool { return true; } public function offsetGet($o): mixed { return null; }
  public function offsetSet($o, $v): void {} public function offsetUnset($o): void { echo "offsetUnset $o\n"; } }
class Q { public $aa; function __construct() { $this->aa = ['x' => new AA]; } function m() { unset($this->aa['x']['y']); } }
(new Q)->m();
"#),
        r#"array(1) {
  ["g"]=>
  array(1) {
    ["j"]=>
    int(2)
  }
}
Cannot indirectly modify readonly property O::$ro
array(0) {
}
array(0) {
}
Cannot unset string offsets
Cannot unset offset in a non-array variable
array(0) {
}
array(1) {
  ["g"]=>
  array(1) {
    ["j"]=>
    int(2)
  }
}
array(1) {
  ["z"]=>
  array(1) {
    [1]=>
    int(2)
  }
}
array(1) {
  ["g"]=>
  array(1) {
    ["k"]=>
    int(1)
  }
}
array(1) {
  ["b"]=>
  array(3) {
    [0]=>
    int(1)
    [1]=>
    int(1)
    [2]=>
    int(1)
  }
}
object(stdClass)#3 (1) {
  ["d"]=>
  array(0) {
  }
}
offsetUnset y
"#
    );
}

/// unset() of an offset of a property holding no array.
#[test]
fn unset_prop_scalars() {
    assert_eq!(
        run(r#"<?php
class O { public $s = 'str'; public int $i = 1; public $u; public $n = 5; function __construct() { unset($this->u); } function __get($n) { echo "__get $n\n"; return null; } }
$o = new O;
try { unset($o->s[0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { unset($o->n[0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { unset($o->s[0][1]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
$o->a = ['s' => 'x']; try { unset($o->a['s'][0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
echo "end\n";
class O2 { public $f = false; public $n = null; public $fl = 1.5; }
$o2 = new O2;
unset($o2->f[0]); var_dump($o2->f);
unset($o2->n[0]); var_dump($o2->n);
try { unset($o2->fl[0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
$s = new stdClass; $s->x = 'q'; try { unset($s->x[0]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#),
        r#"Cannot unset string offsets
Cannot unset offset in a non-array variable
Cannot use string offset as an array

Deprecated: Creation of dynamic property O::$a is deprecated in t.php on line 7
Cannot unset string offsets
end

Deprecated: Automatic conversion of false to array is deprecated in t.php on line 11
bool(false)
NULL
Cannot unset offset in a non-array variable
Cannot unset string offsets
"#
    );
}

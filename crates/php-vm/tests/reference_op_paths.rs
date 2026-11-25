// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Operations on a variable that holds a reference (bound by `global`,
//! `static`, a by-reference parameter, `&` or `foreach` by reference):
//! php-src dereferences every operand (`ZVAL_DEREF`), so each op behaves
//! as on the value itself; the expected output is what the C
//! implementation prints (`/usr/bin/php8.5 -n`, PHP 8.5.12)
//! (ref: Zend/zend_vm_def.h `ZEND_PRE_INC`, `ZEND_ASSIGN_OP`,
//! `ZEND_FE_FETCH_RW`, Zend/zend_execute.c `zend_assign_to_typed_ref`).

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

/// `++`, `--`, `op=`, `.=`, arithmetic, concatenation, comparison and truthiness on a `global`, a `static`, a by-reference parameter and an `&` alias: ints overflowing to float, floats, string increment, undefined and null variables.
#[test]
fn scalar_ops_through_references() {
    assert_eq!(
        run(r#"<?php
$gi = 1; $gf = 1.5; $gs = 'az'; $gm = PHP_INT_MAX; $ga = ['k' => 1]; $go = new stdClass; $go->p = 1;
function g_ops() {
    global $gi, $gf, $gs, $gm;
    $gi++; ++$gi; $gi--; $gi += 2; $gi -= 1; $gi *= 3; $gf += 1; $gf++; $gs++; $gm++;
    echo $gi + 1, ' ', $gf . 's', ' ', $gs . 'y', ' ', $gs === 'ba' ? 'eq' : 'ne', "\n";
    if ($gi) { echo "truthy\n"; }
    $gs .= 'x'; $gs .= 1;
    var_dump($gi, $gf, $gs, $gm);
}
g_ops();
var_dump($gi, $gf, $gs, $gm);
function s_ops() {
    static $n = 0, $c = null, $s = 'Zz', $m = PHP_INT_MAX - 1, $f = 0.25;
    $n++; ++$n; $s++; $m++; $m++; $f *= 2;
    if (null === $c) { $c = 5; } else { $c += 1; }
    $r = $n + $c;
    echo $r, ' ', $s, ' ', $m, ' ', $f, ' ', $c . '!', "\n";
    return ++$n;
}
var_dump(s_ops(), s_ops(), s_ops());
function rec($d) { static $calls = 0; $calls++; if ($d > 0) { rec($d - 1); } return $calls; }
var_dump(rec(3), rec(0));
function by_ref(&$x, &$y) { $x++; $x += 10; $x .= ''; $y = $x . '-' . $y; if ($x) { $x = $x * 2; } return $x === '24' ? 'str' : 'num'; }
$a = 1; $b = 'b'; var_dump(by_ref($a, $b), $a, $b);
function alias() { $v = 7; $r = &$v; $r++; $r -= 2; $r .= 'x'; $w = $r . '!'; $t = $v === '6x'; $r = 3; $u = $r + 1; return [$v, $w, $t, $u, $r ? 'y' : 'n']; }
var_dump(alias());
function undef_ref() { $r = &$nope; $r++; var_dump($r); $q = &$nope2; $q .= 'x'; var_dump($q); $z = &$nope3; $z--; var_dump($z); }
undef_ref();
function nul_ref() { $u = null; $r = &$u; $r--; var_dump($r); $r++; var_dump($u); }
nul_ref();
function float_ref() { $f = -1.5; $r = &$f; $r++; $r++; $r--; var_dump($f, $r == 0.5, $r < 1, $r > 0.4); }
float_ref();
function cmp_ref() { static $x = 3; $y = 3; if ($x == $y) echo "==\n"; if ($x < 4) echo "<\n"; if ($x !== 3.0) echo "!==\n"; if (!$x) echo "neg\n"; else echo "pos\n"; }
cmp_ref(); cmp_ref();
"#),
        r#"
Deprecated: Increment on non-numeric string is deprecated, use str_increment() instead in t.php on line 5
10 3.5s bay eq
truthy
int(9)
float(3.5)
string(4) "bax1"
float(9.223372036854776E+18)
int(9)
float(3.5)
string(4) "bax1"
float(9.223372036854776E+18)

Deprecated: Increment on non-numeric string is deprecated, use str_increment() instead in t.php on line 15
7 AAa 9.2233720368548E+18 0.5 5!

Deprecated: Increment on non-numeric string is deprecated, use str_increment() instead in t.php on line 15
11 AAb 9.2233720368548E+18 1 6!

Deprecated: Increment on non-numeric string is deprecated, use str_increment() instead in t.php on line 15
15 AAc 9.2233720368548E+18 2 7!
int(3)
int(6)
int(9)
int(4)
int(5)
string(3) "num"
int(24)
string(4) "12-b"
array(5) {
  [0]=>
  int(3)
  [1]=>
  string(3) "6x!"
  [2]=>
  bool(true)
  [3]=>
  int(4)
  [4]=>
  string(1) "y"
}
int(1)
string(1) "x"

Warning: Decrement on type null has no effect, this will change in the next major version of PHP in t.php on line 28
NULL

Warning: Decrement on type null has no effect, this will change in the next major version of PHP in t.php on line 30
NULL
int(1)
float(-0.5)
bool(false)
bool(true)
bool(false)
==
<
!==
pos
==
<
!==
pos
"#
    );
}

/// Element and property reads, writes and `isset` through `global`, `static`, by-reference parameter and `&` alias references (copy-on-write of the array the reference holds), references to typed properties, and `foreach` by reference over arrays (separation, appends during the loop, unset elements, an element that already is a reference, objects).
#[test]
fn array_object_typed_and_foreach_references() {
    assert_eq!(
        run(r#"<?php
$zz = ['q' => 1, 'k' => ['j' => 1]]; $zo = new stdClass; $zo->p = 1; $zo->list = ['k' => 1];
function g_arr($a) {
    global $zz, $zo;
    $zz[$a] = 1; $zz['n'] = count($zz); $zz[] = 'end';
    $r = [$zz[$a], isset($zz[$a]), isset($zz['nope']), $zz['k']['j'], isset($zz['k']['j']), count($zz)];
    $zo->p = $a; $zo->q = $zo->p . '!'; $zo->list[$a] = 2;
    $r[] = $zo->p; $r[] = $zo->q; $r[] = $zo->list;
    return $r;
}
var_dump(g_arr('k'));
var_dump($zz, $zo);
function cow() { global $zz; $copy = $zz; $zz['q'] = 'changed'; return $copy['q']; }
var_dump(cow(), $zz['q']);
function st_arr($k) { static $c = []; $c[$k] = ($c[$k] ?? 0) + 1; return [count($c), $c[$k], isset($c[$k])]; }
st_arr('a'); st_arr('b'); var_dump(st_arr('a'));
function ref_arr(array &$arr, $obj) { $arr['x'] = 1; $n = count($arr); $o = &$obj; $o->z = $n; return $arr['x'] + $o->z; }
$aa = ['y' => 0]; $ob = new stdClass; var_dump(ref_arr($aa, $ob), $aa, $ob);
function shared_ref() { $a = [1, 2]; $r = &$a; $b = $a; $r[] = 3; return [$a, $b, count($r)]; }
var_dump(shared_ref());
function undef_dim() { global $undefg; $x = $undefg['k'] ?? 'dflt'; var_dump($x); $undefg['k'] = 1; var_dump(count($undefg)); }
undef_dim(); var_dump($undefg);
function str_ref() { $s = 'abc'; $r = &$s; var_dump($r[1], isset($r[5])); $r[0] = 'X'; var_dump($s); }
str_ref();
// Typed references.
class T { public int $i = 1; public ?string $s = 'a'; public float $f = 1.0; }
function typed() {
    $t = new T; $r = &$t->i; $r++; $r += 2; var_dump($t->i);
    try { $r .= 'x'; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
    $r = PHP_INT_MAX;
    try { $r++; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
    var_dump($t->i);
    $f = &$t->f; $f = 3; var_dump($t->f); $f++; var_dump($t->f);
    $s = &$t->s; $s .= 'b'; $s = null; var_dump($t->s);
    try { $s = []; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
    $r = '5'; var_dump($t->i);
}
typed();
function tref_param(int &$x) { $x++; return $x; }
$t = new T; var_dump(tref_param($t->i), $t->i);
// foreach by reference.
function fe() {
    $a = [1, 2, 3]; foreach ($a as &$v) { $v *= 2; } unset($v); var_dump($a);
    $b = ['x' => 1, 'y' => 2]; foreach ($b as $k => &$v) { $v = "$k$v"; } var_dump($b, $v);
    $c = [1, 2]; $d = $c; foreach ($c as &$v) { $v++; } unset($v); var_dump($c, $d);
    $e = [1, 2]; foreach ($e as &$v) { if ($v == 1) { $e[] = 9; } } unset($v); var_dump($e);
    $f = []; foreach ($f as &$v) { echo "never\n"; }
    $g = [[1], [2]]; foreach ($g as &$row) { $row[] = 0; } unset($row); var_dump($g);
    $h = [1, 2, 3]; foreach ($h as $i => &$v) { if ($i == 0) { unset($h[1]); } $v = -$v; } unset($v); var_dump($h);
    $o = new stdClass; $o->a = 1; foreach ($o as &$v) { $v = 5; } unset($v); var_dump($o);
    $p = [1, 2]; $q = &$p[0]; foreach ($p as &$v) { $v += 10; } unset($v); var_dump($p, $q);
    global $zz; foreach ($zz as &$v) { if (is_int($v)) { $v = 0; } } unset($v); var_dump($zz['q']);
}
fe();
function fe_gen() { $a = [3, 4]; $s = 0; foreach ($a as &$v) { $s += $v; } return [$s, $v, $a]; }
var_dump(fe_gen());
"#),
        r#"
Warning: Trying to access array offset on int in t.php on line 6
array(9) {
  [0]=>
  int(1)
  [1]=>
  bool(true)
  [2]=>
  bool(false)
  [3]=>
  NULL
  [4]=>
  bool(false)
  [5]=>
  int(4)
  [6]=>
  string(1) "k"
  [7]=>
  string(2) "k!"
  [8]=>
  array(1) {
    ["k"]=>
    int(2)
  }
}
array(4) {
  ["q"]=>
  int(1)
  ["k"]=>
  int(1)
  ["n"]=>
  int(2)
  [0]=>
  string(3) "end"
}
object(stdClass)#1 (3) {
  ["p"]=>
  string(1) "k"
  ["list"]=>
  array(1) {
    ["k"]=>
    int(2)
  }
  ["q"]=>
  string(2) "k!"
}
int(1)
string(7) "changed"
array(3) {
  [0]=>
  int(2)
  [1]=>
  int(2)
  [2]=>
  bool(true)
}
int(3)
array(2) {
  ["y"]=>
  int(0)
  ["x"]=>
  int(1)
}
object(stdClass)#2 (1) {
  ["z"]=>
  int(2)
}
array(3) {
  [0]=>
  array(3) {
    [0]=>
    int(1)
    [1]=>
    int(2)
    [2]=>
    int(3)
  }
  [1]=>
  array(2) {
    [0]=>
    int(1)
    [1]=>
    int(2)
  }
  [2]=>
  int(3)
}
string(4) "dflt"
int(1)
array(1) {
  ["k"]=>
  int(1)
}
string(1) "b"
bool(false)
string(3) "Xbc"
int(4)
Cannot assign string to reference held by property T::$i of type int
Cannot increment a reference held by property T::$i of type int past its maximal value
int(9223372036854775807)
float(3)
float(4)
NULL
Cannot assign array to reference held by property T::$s of type ?string
int(5)
int(2)
int(2)
array(3) {
  [0]=>
  int(2)
  [1]=>
  int(4)
  [2]=>
  int(6)
}
array(2) {
  ["x"]=>
  string(2) "x1"
  ["y"]=>
  &string(2) "y2"
}
string(2) "y2"
array(2) {
  [0]=>
  int(2)
  [1]=>
  int(3)
}
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
array(3) {
  [0]=>
  int(1)
  [1]=>
  int(2)
  [2]=>
  int(9)
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
array(2) {
  [0]=>
  int(-1)
  [2]=>
  int(-3)
}
object(stdClass)#3 (1) {
  ["a"]=>
  int(5)
}
array(2) {
  [0]=>
  &int(11)
  [1]=>
  int(12)
}
int(11)
string(7) "changed"
array(3) {
  [0]=>
  int(7)
  [1]=>
  int(4)
  [2]=>
  array(2) {
    [0]=>
    int(3)
    [1]=>
    int(4)
  }
}
"#
    );
}

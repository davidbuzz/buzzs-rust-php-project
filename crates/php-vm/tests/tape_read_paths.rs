// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Reads and tests of elements, properties and constants that the tape's
//! specialised instructions answer (the `Is`-mode miss of `$a['k'] ?? d`,
//! `isset` / `empty` of a temporary's element, `array_key_exists()`,
//! class constants, `is_*()`, `instanceof`, `foreach` over an array,
//! casts) next to the cases that must still take the generic path:
//! warnings and their order, string offsets, `ArrayAccess`, magic
//! methods, references, float/bool/null keys. Each loop runs its body more
//! than once so that a later iteration runs on the tape. The expected
//! output is `/usr/bin/php8.5 -n` on each script (ref: Zend/zend_vm_def.h
//! `ZEND_FETCH_DIM_IS`, `ZEND_ISSET_ISEMPTY_DIM_OBJ`, `ZEND_FETCH_LIST_R`,
//! `ZEND_ARRAY_KEY_EXISTS`, `ZEND_FETCH_CLASS_CONSTANT`, `ZEND_TYPE_CHECK`,
//! `ZEND_INSTANCEOF`, `ZEND_FE_RESET_R`, `ZEND_CAST`).

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

/// `$cv[k] ?? d` and `$cv[k][j] ?? d`: hits, misses, undefined and scalar containers, string offsets, odd keys, ArrayAccess.
#[test]
fn coalesce_dim_miss() {
    assert_eq!(
        run(r#"<?php
class AA implements ArrayAccess {
  function offsetExists($o): bool { echo "exists($o) "; return $o === 'y'; }
  function offsetGet($o): mixed { echo "get($o) "; return [$o]; }
  function offsetSet($o, $v): void {}
  function offsetUnset($o): void {}
}
function f($k) {
  $a = ['a' => null, 'b' => [1 => 2, 'c' => ['d' => 'e']], '5' => 7, 'r' => 1]; $r = &$a['r'];
  $i = 5; $b = true; $fl = 1.5; $n = null; $s = "abc"; $o = new AA;
  for ($t = 0; $t < 3; $t++) {
    echo $a['a'] ?? 'd', '|', $a['nope'] ?? 'd', '|', $a[$k] ?? 'd', '|', $a['5'] ?? 'd', '|', $a[5] ?? 'd', '|', $a['r'] ?? 'd', "\n";
    echo $a['b'][1] ?? 'd', '|', $a['b'][2] ?? 'd', '|', $a['b']['c']['d'] ?? 'd', '|', $a['b']['c']['x'] ?? 'd', '|', $a['x']['y']['z'] ?? 'd', '|', $a['a']['x'] ?? 'd', "\n";
    echo $i['a'] ?? 'd', '|', $b[0] ?? 'd', '|', $fl['x']['y'] ?? 'd', '|', $n[1] ?? 'd', '|', $u['a'] ?? 'd', '|', $u['a']['b'] ?? 'd', "\n";
    echo $s[1] ?? 'd', '|', $s['x'] ?? 'd', '|', $s[9] ?? 'd', '|', $s[1][0] ?? 'd', '|', $s[1]['k'] ?? 'd', "\n";
    echo $a[1.5] ?? 'd', '|', $a[true] ?? 'd', '|', $a[null] ?? 'd', '|', $a[$undef] ?? 'd', "\n";
    echo json_encode($o['x'] ?? 'd'), '|', json_encode($o['y'] ?? 'd'), '|', json_encode($o['y'][0] ?? 'd'), "\n";
    $m = $a['nope'] ?? $t; $q = $a['b']['zz'] ?? [$t]; var_dump($m, $q);
  }
}
f('zz');
"#),
        r#"d|d|d|7|7|1
2|d|e|d|d|d
d|d|d|d|d|d
b|d|d|b|d

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 16
d|d|
Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 16
d|
Warning: Undefined variable $undef in t.php on line 16

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 16
d
exists(x) "d"|exists(y) get(y) ["y"]|exists(y) get(y) "y"
int(0)
array(1) {
  [0]=>
  int(0)
}
d|d|d|7|7|1
2|d|e|d|d|d
d|d|d|d|d|d
b|d|d|b|d

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 16
d|d|
Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 16
d|
Warning: Undefined variable $undef in t.php on line 16

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 16
d
exists(x) "d"|exists(y) get(y) ["y"]|exists(y) get(y) "y"
int(1)
array(1) {
  [0]=>
  int(1)
}
d|d|d|7|7|1
2|d|e|d|d|d
d|d|d|d|d|d
b|d|d|b|d

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 16
d|d|
Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 16
d|
Warning: Undefined variable $undef in t.php on line 16

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 16
d
exists(x) "d"|exists(y) get(y) ["y"]|exists(y) get(y) "y"
int(2)
array(1) {
  [0]=>
  int(2)
}
"#
    );
}

/// `array_key_exists()`: int, string and numeric-string keys, null values, references, property and literal arrays, odd keys and non-arrays.
#[test]
fn key_exists() {
    assert_eq!(
        run(r#"<?php
class C {
  public $map = ['a' => null, 7 => 1, '08' => 2];
  function has($k) { return array_key_exists($k, $this->map); }
}
function f() {
  $a = ['x' => null, 3 => 'three', '' => 'empty']; $r = &$a; $c = new C; $o = new ArrayObject(['x' => 1]);
  for ($t = 0; $t < 3; $t++) {
    $out = [];
    foreach (['x', 3, '3', 'nope', '', '03', 4] as $k) {
      $out[] = array_key_exists($k, $a); $out[] = array_key_exists($k, $r); $out[] = $c->has($k);
    }
    echo json_encode($out), "\n";
    echo json_encode([array_key_exists('a', $c->map), array_key_exists(7, $c->map), array_key_exists('7', $c->map), array_key_exists('08', $c->map), array_key_exists(8, $c->map)]), "\n";
    echo json_encode([array_key_exists('k', ['k' => 1]), array_key_exists('k' . $t, ['k0' => 1, 'k2' => 2])]), "\n";
    if (array_key_exists('x', $a)) { echo "branch\n"; }
    echo json_encode([array_key_exists(null, $a), array_key_exists(1.0, $a), array_key_exists(true, [1 => 1]), @array_key_exists($undef, $a)]), "\n";
    try { array_key_exists('x', $o); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
    try { array_key_exists('x', 'str'); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
    try { array_key_exists([], $a); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
    try { array_key_exists('x', $nope); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
  }
}
f();
"#),
        r#"[true,true,false,true,true,false,true,true,false,false,false,false,true,true,false,false,false,false,false,false,false]
[true,true,true,true,false]
[true,true]
branch

Deprecated: Using null as the key parameter for array_key_exists() is deprecated, use an empty string instead in t.php on line 17
[true,false,true,true]
array_key_exists(): Argument #2 ($array) must be of type array, ArrayObject given
array_key_exists(): Argument #2 ($array) must be of type array, string given
Cannot access offset of type array on array

Warning: Undefined variable $nope in t.php on line 21
array_key_exists(): Argument #2 ($array) must be of type array, null given
[true,true,false,true,true,false,true,true,false,false,false,false,true,true,false,false,false,false,false,false,false]
[true,true,true,true,false]
[true,false]
branch

Deprecated: Using null as the key parameter for array_key_exists() is deprecated, use an empty string instead in t.php on line 17
[true,false,true,true]
array_key_exists(): Argument #2 ($array) must be of type array, ArrayObject given
array_key_exists(): Argument #2 ($array) must be of type array, string given
Cannot access offset of type array on array

Warning: Undefined variable $nope in t.php on line 21
array_key_exists(): Argument #2 ($array) must be of type array, null given
[true,true,false,true,true,false,true,true,false,false,false,false,true,true,false,false,false,false,false,false,false]
[true,true,true,true,false]
[true,true]
branch

Deprecated: Using null as the key parameter for array_key_exists() is deprecated, use an empty string instead in t.php on line 17
[true,false,true,true]
array_key_exists(): Argument #2 ($array) must be of type array, ArrayObject given
array_key_exists(): Argument #2 ($array) must be of type array, string given
Cannot access offset of type array on array

Warning: Undefined variable $nope in t.php on line 21
array_key_exists(): Argument #2 ($array) must be of type array, null given
"#
    );
}

/// `isset` / `empty` / `??` of two- to four-offset paths on a variable and on `$this->p`, and of `$this->a->b`: hits, misses, magic and ArrayAccess.
#[test]
fn nested_isset_paths() {
    assert_eq!(
        run(r#"<?php
class Inner { public $b = 'B'; public $n = null; public ?int $typed; public $z = 0; }
class M { private $d = ['q' => 1]; function __get($n) { echo "__get($n) "; return $this->d; } function __isset($n) { echo "__isset($n) "; return $n === 'magic'; } }
class O {
  public $p = ['a' => ['b' => [1 => 'one', 'n' => null]], 'z' => 0];
  public $a; public $m; public $nul = null; public $ao;
  function __construct() { $this->a = new Inner; $this->m = new M; $this->ao = new ArrayObject(['k' => ['j' => 1]]); }
  function t($k, $j) {
    return json_encode([
      isset($this->p[$k][$j]), empty($this->p[$k][$j]), $this->p[$k][$j] ?? 'd', $this->p[$k][$j][1] ?? 'd',
      isset($this->a->b), isset($this->a->n), empty($this->a->z), isset($this->a->typed), isset($this->a->nope),
      isset($this->nul->x), isset($this->m->magic), isset($this->m->other), $this->m->magic['q'] ?? 'd',
      isset($this->ao['k']['j']), $this->ao['k']['j'] ?? 'd',
    ]);
  }
}
function f() {
  $a = ['l' => ['m' => [3 => ['x' => 0, 'y' => 'Y']]], 's' => 'str', 'n' => null]; $o = new O;
  $r = ['x' => ['y' => 1]]; $ref = &$r['x'];
  for ($t = 0; $t < 3; $t++) {
    foreach ([3, 4, '3'] as $k) {
      echo json_encode([isset($a['l']['m'][$k]['x']), empty($a['l']['m'][$k]['x']), isset($a['l']['m'][$k]['y']), empty($a['l']['m'][$k]['y']), isset($a['l']['m'][$k])]), ' ';
    }
    echo "\n", json_encode([isset($a['s'][0]), isset($a['s']['x']), isset($a['s'][0][0]), empty($a['s'][9]), isset($a['n']['x']), isset($a['q']['r']), isset($u['a']['b']), isset($r['x']['y']), empty($r['x']['y'])]), "\n";
    echo json_encode([isset($a[1.5]['x']), isset($a[null]['x'])]), "\n";
    echo $o->t('a', 'b'), "\n", $o->t('a', 'c'), "\n", $o->t('z', 'b'), "\n", $o->t('q', 'r'), "\n";
  }
}
f();
"#),
        r#"[true,true,true,false,true] [false,true,false,true,false] [true,true,true,false,true] 
[true,false,true,true,false,false,false,true,false]

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 25

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 25
[false,false]
__isset(magic) __isset(other) __isset(magic) __get(magic) [true,false,{"1":"one","n":null},"one",true,false,true,false,false,false,true,false,1,true,1]
__isset(magic) __isset(other) __isset(magic) __get(magic) [false,true,"d","d",true,false,true,false,false,false,true,false,1,true,1]
__isset(magic) __isset(other) __isset(magic) __get(magic) [false,true,"d","d",true,false,true,false,false,false,true,false,1,true,1]
__isset(magic) __isset(other) __isset(magic) __get(magic) [false,true,"d","d",true,false,true,false,false,false,true,false,1,true,1]
[true,true,true,false,true] [false,true,false,true,false] [true,true,true,false,true] 
[true,false,true,true,false,false,false,true,false]

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 25

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 25
[false,false]
__isset(magic) __isset(other) __isset(magic) __get(magic) [true,false,{"1":"one","n":null},"one",true,false,true,false,false,false,true,false,1,true,1]
__isset(magic) __isset(other) __isset(magic) __get(magic) [false,true,"d","d",true,false,true,false,false,false,true,false,1,true,1]
__isset(magic) __isset(other) __isset(magic) __get(magic) [false,true,"d","d",true,false,true,false,false,false,true,false,1,true,1]
__isset(magic) __isset(other) __isset(magic) __get(magic) [false,true,"d","d",true,false,true,false,false,false,true,false,1,true,1]
[true,true,true,false,true] [false,true,false,true,false] [true,true,true,false,true] 
[true,false,true,true,false,false,false,true,false]

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 25

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 25
[false,false]
__isset(magic) __isset(other) __isset(magic) __get(magic) [true,false,{"1":"one","n":null},"one",true,false,true,false,false,false,true,false,1,true,1]
__isset(magic) __isset(other) __isset(magic) __get(magic) [false,true,"d","d",true,false,true,false,false,false,true,false,1,true,1]
__isset(magic) __isset(other) __isset(magic) __get(magic) [false,true,"d","d",true,false,true,false,false,false,true,false,1,true,1]
__isset(magic) __isset(other) __isset(magic) __get(magic) [false,true,"d","d",true,false,true,false,false,false,true,false,1,true,1]
"#
    );
}

/// `isset` / `empty` / reads of an element of a temporary (`self::MAP[$k]`, `f()[$k]`) and array destructuring, hits and misses.
#[test]
fn tmp_element_reads() {
    assert_eq!(
        run(r#"<?php
class K {
  const MAP = ['a' => 1, 'z' => 0, 'n' => null, 's' => '0', 'arr' => ['x' => [1]], 5 => 'five'];
  static function has($k) { return isset(self::MAP[$k]); }
  static function none($k) { return empty(static::MAP[$k]); }
}
class AA implements ArrayAccess {
  function offsetExists($o): bool { echo "exists($o) "; return true; }
  function offsetGet($o): mixed { echo "get($o) "; return 0; }
  function offsetSet($o, $v): void {}
  function offsetUnset($o): void {}
}
function arr() { return ['a' => 1, 'b' => [2, 3], 'o' => new AA]; }
function str() { return "abc"; }
function obj() { return new AA; }
function f() {
  $x = [10, 20, 'k' => 30]; $r = [1, [2, 3]]; $rr = &$r[1];
  for ($t = 0; $t < 3; $t++) {
    foreach (['a', 'z', 'n', 's', 'arr', 'nope', 5, '5'] as $k) {
      echo $k, ':', json_encode([K::has($k), K::none($k), isset(K::MAP[$k]['x']), empty(K::MAP[$k]['x'][0])]), ' ';
    }
    echo "\n";
    echo json_encode([isset(arr()['a']), isset(arr()['q']), empty(arr()['b'][1]), empty(arr()['b'][7]), isset(str()[1]), isset(str()['x']), empty(str()[0])]), "\n";
    echo json_encode([isset(arr()['o']['w']), empty(arr()['o']['w']), isset(obj()['v'])]), "\n";
    echo arr()['a'], arr()['b'][1], arr()['q'] ?? 'd', arr()['b'][9] ?? 'd', K::MAP['arr']['x'][0], K::MAP['nope'] ?? 'd', str()[2], "\n";
    echo @arr()['missing'], '|', arr()[1.0] ?? 'd', "\n";
    [$p, $q] = $x; ['k' => $v, 0 => $w] = $x; [, [$m, $n]] = $r;
    echo $p, $q, $v, $w, $m, $n, "\n";
    [$a, $b, $c] = $x; var_dump($c);
    [$d] = "str"; [$e] = 5; [$g] = null; var_dump($d, $e, $g);
    [$h, [$i]] = [1, 2]; var_dump($i);
    [$j] = $undef; var_dump($j);
    ['k' => [$l]] = ['k' => [7]]; echo $l, "\n";
  }
}
f();
"#),
        r#"a:[true,false,false,true] z:[true,true,false,true] n:[false,true,false,true] s:[true,true,false,true] arr:[true,false,true,false] nope:[false,true,false,true] 5:[true,false,false,true] 5:[true,false,false,true] 
[true,false,false,true,true,false,false]
exists(w) exists(w) get(w) exists(v) [true,true,true]
13dd1dc
|d
1020301023

Warning: Undefined array key 2 in t.php on line 29
NULL

Warning: Cannot use string as array in t.php on line 30

Warning: Cannot use int as array in t.php on line 30
NULL
NULL
NULL

Warning: Cannot use int as array in t.php on line 31
NULL

Warning: Undefined variable $undef in t.php on line 32
NULL
7
a:[true,false,false,true] z:[true,true,false,true] n:[false,true,false,true] s:[true,true,false,true] arr:[true,false,true,false] nope:[false,true,false,true] 5:[true,false,false,true] 5:[true,false,false,true] 
[true,false,false,true,true,false,false]
exists(w) exists(w) get(w) exists(v) [true,true,true]
13dd1dc
|d
1020301023

Warning: Undefined array key 2 in t.php on line 29
NULL

Warning: Cannot use string as array in t.php on line 30

Warning: Cannot use int as array in t.php on line 30
NULL
NULL
NULL

Warning: Cannot use int as array in t.php on line 31
NULL

Warning: Undefined variable $undef in t.php on line 32
NULL
7
a:[true,false,false,true] z:[true,true,false,true] n:[false,true,false,true] s:[true,true,false,true] arr:[true,false,true,false] nope:[false,true,false,true] 5:[true,false,false,true] 5:[true,false,false,true] 
[true,false,false,true,true,false,false]
exists(w) exists(w) get(w) exists(v) [true,true,true]
13dd1dc
|d
1020301023

Warning: Undefined array key 2 in t.php on line 29
NULL

Warning: Cannot use string as array in t.php on line 30

Warning: Cannot use int as array in t.php on line 30
NULL
NULL
NULL

Warning: Cannot use int as array in t.php on line 31
NULL

Warning: Undefined variable $undef in t.php on line 32
NULL
7
"#
    );
}

/// `is_*()` and `count()` of values that are not variables, `instanceof`, and `foreach` over arrays, empty arrays, objects and generators, with `break` and nesting.
#[test]
fn type_checks_and_foreach() {
    assert_eq!(
        run(r#"<?php
interface I {} class A implements I {} class B extends A {}
function gen() { yield 1; yield 2; }
function vals() { return [1, 'one', 1.5, null, true, [1], new B]; }
function f($m) {
  $objs = [new A, new B, 'A', null, new ArrayObject([])];
  for ($t = 0; $t < 3; $t++) {
    $row = [];
    foreach (vals() as $i => $v) {
      $w = [$v];
      $row[] = (is_int($w[0]) ? 'i' : '') . (is_string($w[0]) ? 's' : '') . (is_float($w[0]) ? 'f' : '') . (is_null($w[0]) ? 'n' : '')
        . (is_bool($w[0]) ? 'b' : '') . (is_array($w[0]) ? 'a' : '') . (is_object($w[0]) ? 'o' : '') . (is_resource($w[0]) ? 'r' : '')
        . (is_scalar($w[0]) ? 'S' : '') . (is_iterable($w[0]) ? 'I' : '') . (is_countable($w[0]) ? 'C' : '') . (is_callable($w[0]) ? 'X' : '');
    }
    echo implode(',', $row), "\n";
    echo json_encode([is_array($m['k'] ?? null), is_string(implode('', ['a'])), is_int(count($objs))]), "\n";
    $io = [];
    foreach ($objs as $o) { $x = [$o]; $io[] = [$x[0] instanceof A, $x[0] instanceof B, $x[0] instanceof I, $x[0] instanceof Countable, $o instanceof Missing]; }
    echo json_encode($io), "\n";
    echo count($m), count($m['k']), count([1, 2, 3]), count([]), count(vals(), COUNT_RECURSIVE), "\n";
    try { echo count($m['s']); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
    echo count(new ArrayObject([1, 2])), "\n";
    $s = 0;
    foreach ([] as $v) { $s += 100; }
    foreach ($m['k'] as $v) { foreach ($m['k'] as $w) { $s += $v * $w; } }
    foreach ($m['k'] as $k => $v) { if ($k == 1) break; $s += $v; }
    foreach (gen() as $v) { $s += $v; }
    foreach (new ArrayObject([5, 6]) as $v) { $s += $v; }
    $arr = [1, 2]; foreach ($arr as $v) { $arr[] = $v; } $s += count($arr);
    echo $s, "\n";
    foreach ($m['nope'] ?? [] as $v) { echo "never\n"; }
    foreach ($m['s'] as $v) { echo "never\n"; }
  }
}
f(['k' => [1, 2, 3], 's' => 'str']);
"#),
        r#"iS,sS,fS,n,bS,aIC,o
[true,true,true]
[[true,false,true,false,false],[true,true,true,false,false],[false,false,false,false,false],[false,false,false,false,false],[false,false,false,true,false]]
23308
count(): Argument #1 ($value) must be of type Countable|array, string given
2
55

Warning: foreach() argument must be of type array|object, string given in t.php on line 32
iS,sS,fS,n,bS,aIC,o
[true,true,true]
[[true,false,true,false,false],[true,true,true,false,false],[false,false,false,false,false],[false,false,false,false,false],[false,false,false,true,false]]
23308
count(): Argument #1 ($value) must be of type Countable|array, string given
2
55

Warning: foreach() argument must be of type array|object, string given in t.php on line 32
iS,sS,fS,n,bS,aIC,o
[true,true,true]
[[true,false,true,false,false],[true,true,true,false,false],[false,false,false,false,false],[false,false,false,false,false],[false,false,false,true,false]]
23308
count(): Argument #1 ($value) must be of type Countable|array, string given
2
55

Warning: foreach() argument must be of type array|object, string given in t.php on line 32
"#
    );
}

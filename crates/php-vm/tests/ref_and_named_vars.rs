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

/// `compact()` and `extract()` in functions, methods and the main script:
/// every flag, references (`EXTR_REFS`, typed property references),
/// destructors of overwritten values, `$this`, invalid and numeric names,
/// undefined names, nested and recursive name arrays, the variables
/// `get_defined_vars()` sees afterwards.
#[test]
fn compact_and_extract() {
    assert_eq!(
        run(r#"<?php
class D { function __construct(public $n) {} function __destruct() { echo "~D{$this->n}\n"; } }
class T { public int $p = 1; function m() { $a = 1; var_dump(compact('this', 'a')); try { extract(['this' => 1]); } catch (Error $e) { echo $e->getMessage(), "\n"; } } }
function ce() {
  $a = 1; $b = 'two'; $r = [1]; $ref = &$r;
  var_dump(compact('a', ['b', ['ref']], 'nope', 'this'));
  var_dump(compact('a', 5));
  $n = extract(['a' => 10, 'c' => 30, 'b' => 20, 'not valid' => 1, 5 => 'num']); var_dump($n, $a, $b, $c);
  var_dump(extract(['a' => 'skip', 'd' => 'new'], EXTR_SKIP), $a, $d);
  var_dump(extract(['a' => 'p', 'e' => 'q'], EXTR_PREFIX_SAME, 'x'), $a, $x_a, $e);
  var_dump(extract(['a' => 'all', 7 => 'seven'], EXTR_PREFIX_ALL, 'pa'), $pa_a, $pa_7);
  var_dump(extract(['1bad' => 'inv', 'ok' => 'fine'], EXTR_PREFIX_INVALID, 'iv'), $iv_1bad, $ok);
  var_dump(extract(['a' => 'exists', 'zz' => 'no'], EXTR_IF_EXISTS), $a, isset($zz));
  var_dump(extract(['a' => 'pie', 'zz' => 'no'], EXTR_PREFIX_IF_EXISTS, 'pi'), $pi_a, isset($pi_zz));
  $src = ['ra' => 1, 'rb' => 2]; extract($src, EXTR_REFS); $ra = 100; var_dump($src);
  $obj = new D(1); extract(['obj' => 'gone']); echo "after\n";
  $o2 = new D(2); $keep = &$o2; extract(['keep' => 'x']); var_dump($o2); unset($keep, $o2);
  $t = new T; $tr = &$t->p; try { extract(['tr' => 'str']); } catch (TypeError $e) { echo $e->getMessage(), "\n"; } extract(['tr' => '42']); var_dump($t->p);
  var_dump(array_keys(get_defined_vars()));
  $u = 1; unset($u); var_dump(extract(['u' => 'back'], EXTR_PREFIX_SAME, 'w'), $u);
  try { extract(['a' => 1], 99); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
  $cyc = ['a']; $cyc[] = &$cyc; try { compact($cyc); } catch (Error $e) { echo $e->getMessage(), "\n"; }
  extract(['fresh' => 'F']); var_dump(compact('fresh', 'a'));
  $gk = 'GLOBALS'; var_dump(extract([$gk => 1]));
}
function loop() { $a = 1; $b = 2; for ($i = 0; $i < 3; $i++) { $c = compact('a', 'b'); $c['a']++; extract($c); } var_dump($a, $b); }
ce(); (new T)->m(); loop();
$top = 't'; var_dump(compact('top')); extract(['top2' => 'T2']); var_dump($top2);
"#),
        r#"
Warning: compact(): Undefined variable $nope in t.php on line 6
array(3) {
  ["a"]=>
  int(1)
  ["b"]=>
  string(3) "two"
  ["ref"]=>
  array(1) {
    [0]=>
    int(1)
  }
}

Warning: compact(): Argument #2 must be string or array of strings, int given in t.php on line 7
array(1) {
  ["a"]=>
  int(1)
}
int(3)
int(10)
int(20)
int(30)
int(1)
int(10)
string(3) "new"
int(2)
int(10)
string(1) "p"
string(1) "q"
int(2)
string(3) "all"
string(5) "seven"
int(2)
string(3) "inv"
string(4) "fine"
int(1)
string(6) "exists"
bool(false)
int(2)
string(3) "pie"
bool(false)
array(2) {
  ["ra"]=>
  &int(100)
  ["rb"]=>
  &int(2)
}
~D1
after
~D2
string(1) "x"
Cannot assign string to reference held by property T::$p of type int
int(42)
array(21) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
  [2]=>
  string(1) "r"
  [3]=>
  string(3) "ref"
  [4]=>
  string(1) "n"
  [5]=>
  string(1) "c"
  [6]=>
  string(1) "d"
  [7]=>
  string(3) "x_a"
  [8]=>
  string(1) "e"
  [9]=>
  string(4) "pa_a"
  [10]=>
  string(4) "pa_7"
  [11]=>
  string(7) "iv_1bad"
  [12]=>
  string(2) "ok"
  [13]=>
  string(2) "zz"
  [14]=>
  string(4) "pi_a"
  [15]=>
  string(3) "src"
  [16]=>
  string(2) "ra"
  [17]=>
  string(3) "obj"
  [18]=>
  string(1) "t"
  [19]=>
  string(2) "tr"
  [20]=>
  string(2) "rb"
}
int(1)
string(4) "back"
extract(): Argument #2 ($flags) must be a valid extract type
Recursion detected
array(2) {
  ["fresh"]=>
  string(1) "F"
  ["a"]=>
  string(6) "exists"
}
int(1)
array(2) {
  ["this"]=>
  object(T)#1 (1) {
    ["p"]=>
    int(1)
  }
  ["a"]=>
  int(1)
}
Cannot re-assign $this
int(4)
int(2)
array(1) {
  ["top"]=>
  string(1) "t"
}
string(2) "T2"
"#
    );
}

/// `$cv op= v` with operands that call `__toString()`, throw, warn or
/// convert (through plain variables, references and typed property
/// references), `$array += $array`, an undefined variable, and
/// `unset($cv)` with the destructor of the released value.
#[test]
fn assign_op_and_unset_of_variables() {
    assert_eq!(
        run(r#"<?php
class S { function __construct(public $v = 'x') {} function __toString(): string { return $this->v; } }
class Bad { function __toString(): string { throw new Exception("no string"); } }
class D { function __construct(public $n) {} function __destruct() { echo "~D{$this->n}\n"; } }
class T { public string $s = 'a'; public int $i = 1; }
function f() {
  $o = new S('o'); $s = 'pre-'; $s .= $o; var_dump($s);
  $n = 5; $n .= $o; var_dump($n);
  $r = 'r'; $rr = &$r; $rr .= $o; var_dump($r);
  $t = new T; $ts = &$t->s; $ts .= $o; var_dump($t->s);
  $ti = &$t->i; try { $ti .= $o; } catch (TypeError $e) { echo $e->getMessage(), "\n"; } var_dump($t->i);
  $b = 'keep'; try { $b .= new Bad; } catch (Exception $e) { echo $e->getMessage(), "\n"; } var_dump($b);
  $a = [1]; $a += [5, 6]; var_dump($a);
  $x = 'abc'; try { $x -= 1; } catch (TypeError $e) { echo $e->getMessage(), "\n"; } var_dump($x);
  $y = '5 apples'; $y += 1; var_dump($y);
  $z = [1]; try { $z .= 'x'; } catch (Error $e) { echo get_class($e), "\n"; } var_dump($z);
  $d = new D(1); try { $d .= "tail"; } catch (Error $e) { echo $e->getMessage(), "\n"; } echo "after concat\n"; unset($d);
  $u = new D(2); unset($u); echo "after unset\n";
  $w = new D(3); $w2 = $w; unset($w); echo "still held\n"; unset($w2); echo "gone\n";
  $g = 1; $gr = &$g; unset($g); var_dump($gr); unset($nothing);
  $p = 2; $p **= 3; var_dump($p);
  $q = 7; $q %= 0.5 + 2.5; var_dump($q);
  $un .= 'u'; var_dump($un);
  var_dump(array_keys(get_defined_vars()));
}
f();
"#),
        r#"string(5) "pre-o"
string(2) "5o"
string(2) "ro"
string(2) "ao"
Cannot assign string to reference held by property T::$i of type int
int(1)
no string
string(4) "keep"
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(6)
}
Unsupported operand types: string - int
string(3) "abc"

Warning: A non-numeric value encountered in t.php on line 15
int(6)

Warning: Array to string conversion in t.php on line 16
string(6) "Arrayx"
Object of class D could not be converted to string
after concat
~D1
~D2
after unset
still held
~D3
gone
int(1)
int(8)
int(1)

Warning: Undefined variable $un in t.php on line 23
string(1) "u"
array(18) {
  [0]=>
  string(1) "o"
  [1]=>
  string(1) "s"
  [2]=>
  string(1) "n"
  [3]=>
  string(1) "r"
  [4]=>
  string(2) "rr"
  [5]=>
  string(1) "t"
  [6]=>
  string(2) "ts"
  [7]=>
  string(2) "ti"
  [8]=>
  string(1) "e"
  [9]=>
  string(1) "b"
  [10]=>
  string(1) "a"
  [11]=>
  string(1) "x"
  [12]=>
  string(1) "y"
  [13]=>
  string(1) "z"
  [14]=>
  string(2) "gr"
  [15]=>
  string(1) "p"
  [16]=>
  string(1) "q"
  [17]=>
  string(2) "un"
}
"#
    );
}

/// Reference assignments and `foreach` by reference in loops long enough
/// to run on the tape and to be compiled by the trace JIT: the values,
/// the destructors of the values the bindings release, references kept
/// in an array, the result of `$r = ($a = &$b)`.
#[test]
fn reference_bindings_in_hot_loops() {
    assert_eq!(
        run(r#"<?php
class D { static $n = 0; function __destruct() { self::$n++; } }
function loops() {
  $a = 0; $sum = 0;
  for ($i = 0; $i < 3000; $i++) { $b = &$a; $b++; $c = &$b; $c += 2; $sum += $a; unset($b); }
  var_dump($a, $sum);
  $arr = range(1, 2000);
  for ($k = 0; $k < 3; $k++) { foreach ($arr as &$v) { $v = $v + 1; } }
  unset($v); var_dump(array_sum($arr), $arr[0], $arr[1999]);
  $x = null;
  for ($i = 0; $i < 2000; $i++) { $x = new D; $y = 1; $x = &$y; }
  var_dump(D::$n);
  $refs = [];
  for ($i = 0; $i < 1500; $i++) { $t = $i; $refs[] = &$t; unset($t); }
  var_dump(count($refs), $refs[1499]);
  $m = 1; for ($i = 0; $i < 1200; $i++) { $q = &$m; $r = ($q2 = &$q); }
  $r = 5; var_dump($m);
}
loops();
"#),
        r#"int(9000)
int(13504500)
int(2007000)
int(4)
int(2003)
int(2000)
int(1500)
int(1499)
int(1)
"#
    );
}

/// `$cv[...] = v` and `$cv[] = v` on undefined, null and `false` variables: the array made first (kept when the offset throws), offsets that warn or throw through an error handler, nested appends, a variable unset and filled again in a loop.
#[test]
fn assign_dim_to_undefined_variables() {
    assert_eq!(
        run(r#"<?php
function f() {
  try { $a[[]] = 1; } catch (TypeError $e) { echo $e->getMessage(), "\n"; } var_dump($a);
  $b[$undef] = 1; var_dump($b);
  try { $c['x']->p = 1; } catch (Error $e) { echo $e->getMessage(), "\n"; } var_dump($c);
  $n = null; $n[] = 'v'; var_dump($n);
  $fa = false; $fa[] = 1; var_dump($fa);
  $d['x'][] = 2; $d['x']['y']['z'] = 3; var_dump($d);
  set_error_handler(function ($no, $msg) { throw new Exception($msg); });
  try { $ee[$undef2] = 1; } catch (Exception $ex) { echo $ex->getMessage(), "\n"; } var_dump($ee);
  restore_error_handler();
  try { $g[PHP_INT_MAX] = 1; $g[] = 2; } catch (Error $ex) { echo $ex->getMessage(), "\n"; } var_dump(count($g));
  $s = ''; try { $s[] = 'x'; } catch (Error $ex) { echo $ex->getMessage(), "\n"; }
  $h[1.5] = 'f'; var_dump($h);
  $o = new ArrayObject; $o['k'] = 1; var_dump(count($o));
  for ($i = 0; $i < 3; $i++) { unset($l); $l[] = $i; $l[] = $i; } var_dump($l);
  var_dump(array_keys(get_defined_vars()));
}
f();
"#),
        r#"Cannot access offset of type array on array
array(0) {
}

Warning: Undefined variable $undef in t.php on line 4

Deprecated: Using null as an array offset is deprecated, use an empty string instead in t.php on line 4
array(1) {
  [""]=>
  int(1)
}
Attempt to assign property "p" on null
array(1) {
  ["x"]=>
  NULL
}
array(1) {
  [0]=>
  string(1) "v"
}

Deprecated: Automatic conversion of false to array is deprecated in t.php on line 7
array(1) {
  [0]=>
  int(1)
}
array(1) {
  ["x"]=>
  array(2) {
    [0]=>
    int(2)
    ["y"]=>
    array(1) {
      ["z"]=>
      int(3)
    }
  }
}
Undefined variable $undef2
array(0) {
}
Cannot add element to the array as the next element is already occupied
int(1)
[] operator not supported for strings

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 14
array(1) {
  [1]=>
  string(1) "f"
}
int(1)
array(2) {
  [0]=>
  int(2)
  [1]=>
  int(2)
}
array(15) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "e"
  [2]=>
  string(1) "b"
  [3]=>
  string(1) "c"
  [4]=>
  string(1) "n"
  [5]=>
  string(2) "fa"
  [6]=>
  string(1) "d"
  [7]=>
  string(2) "ee"
  [8]=>
  string(2) "ex"
  [9]=>
  string(1) "g"
  [10]=>
  string(1) "s"
  [11]=>
  string(1) "h"
  [12]=>
  string(1) "o"
  [13]=>
  string(1) "i"
  [14]=>
  string(1) "l"
}
"#
    );
}

/// `$a[$a] = 1` with `$a` undefined: `ZEND_ASSIGN_DIM` makes `$a` an array before it reads the offset, which is then that array.
#[test]
fn assign_dim_reads_the_offset_after_the_container() {
    assert_eq!(
        run(r#"<?php
function f() { try { $a[$a] = 1; } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; } var_dump($a); }
f();
"#),
        r#"TypeError: Cannot access offset of type array on array
array(0) {
}
"#
    );
}

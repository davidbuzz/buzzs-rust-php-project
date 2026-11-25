// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The call and return protocol of ordinary user calls, and what binds
//! variables at function entry (`global`, `static`): each path a fast
//! path takes is compared with what the C implementation prints
//! (`/usr/bin/php8.5 -n`, PHP 8.5.12), destructor order included
//! (ref: Zend/zend_vm_def.h `ZEND_RETURN`, `zend_leave_helper`,
//! `ZEND_BIND_GLOBAL`, `ZEND_BIND_STATIC`).

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

/// `global` and `static` bind the variable to the stored reference; the
/// variable's old value is released after the bind, and rebinding a bound
/// variable leaves the stored value alone.
#[test]
fn global_and_static_binds() {
    assert_eq!(
        run(r#"<?php
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "destruct {$this->n}\n"; } }
$g = new D('g1');
function rebind() { global $g; $g = new D('g2'); echo "rebound\n"; }
rebind();
echo "after rebind\n";
function counter() { static $n = 0; static $arr = []; $arr[] = ++$n; return $arr; }
counter(); counter();
var_dump(counter());
function gref() { global $x; $x[] = count($x ?? []); return $x; }
gref(); gref();
var_dump($x);
function twice() { global $y, $y; $y = 'set'; }
twice(); var_dump($y);
$h = [1, 2];
function rebind_ref() { global $h; $local = [9]; $h = &$local; $h[] = 10; return $h; }
var_dump(rebind_ref(), $h);
function st_obj() { static $o; $o ??= new D('static'); return $o->n; }
echo st_obj(), st_obj(), "\n";
class K { static function m() { static $c = 0; return ++$c; } }
class L extends K {}
echo K::m(), L::m(), K::m(), "\n";
$f = function () { static $z = 10; return $z++; };
echo $f(), $f(), "\n";
$f2 = $f; echo $f2(), "\n";
unset($g);
echo "end\n";
"#),
        r#"destruct g1
rebound
after rebind
array(3) {
  [0]=>
  int(1)
  [1]=>
  int(2)
  [2]=>
  int(3)
}
array(2) {
  [0]=>
  int(0)
  [1]=>
  int(1)
}
string(3) "set"
array(2) {
  [0]=>
  int(9)
  [1]=>
  int(10)
}
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
staticstatic
123
1011
12
destruct g2
end
destruct static
"#
    );
}

/// `global $g` in a function while the main script's own `$g` is
/// reassigned, unset, rebound by reference or written through `$GLOBALS`
/// between the calls: each bind sees what the main script last stored.
#[test]
fn global_binds_follow_the_main_scripts_variable() {
    assert_eq!(
        run(r#"<?php
function show() { global $g; var_dump($g); }
function inc() { global $g; $g++; }
$g = 1;
show(); inc(); show();
$g = 5; show();
$g = 'str'; show();
unset($g); show();
$g = [1]; show();
$other = 'other';
$g = &$other; show();
$other = 'changed'; show();
inc(); var_dump($other);
unset($g);
$GLOBALS['g'] = 'via globals'; show();
function only_in_func() { global $nomain; $nomain = ($nomain ?? 0) + 1; return $nomain; }
echo only_in_func(), only_in_func(), "\n";
echo $nomain, "\n";
function nested() { global $g; $g = 'nested'; show(); }
nested(); var_dump($g);
for ($i = 0; $i < 5; $i++) { $g = $i; show(); inc(); }
var_dump($g);
function unset_inside() { global $g; unset($g); var_dump(isset($g)); global $g; var_dump($g); }
unset_inside();
function by_name() { $n = 'g'; global $$n; return $g; }
var_dump(by_name());
"#),
        r#"int(1)
int(2)
int(5)
string(3) "str"
NULL
array(1) {
  [0]=>
  int(1)
}
string(5) "other"
string(7) "changed"

Deprecated: Increment on non-numeric string is deprecated, use str_increment() instead in t.php on line 3
string(7) "changee"
string(11) "via globals"
12
2
string(6) "nested"
string(6) "nested"
int(0)
int(1)
int(2)
int(3)
int(4)
int(5)
bool(false)
int(5)
int(5)
"#
    );
}

/// Functions and methods with `static` variables, called from a loop (the
/// tape prepares such calls): defaults, passed-argument counts, extra
/// arguments, recursion, backtraces, an exception thrown from the body,
/// statics shared by a method and its inheriting class, `self::`,
/// `static::` and `parent::` calls, a missing argument, and a static
/// object released at the end of the request.
#[test]
fn calls_of_functions_with_static_variables() {
    assert_eq!(
        run(r#"<?php
function counter() { static $n = 0; return ++$n; }
function withdef($a, $b = 10) { static $sum = 0; $sum += $a + $b; return $sum; }
function args_seen($x) { static $calls = []; $calls[] = func_num_args() . ':' . implode(',', func_get_args()); return $calls; }
function rec($d) { static $depth = 0; $depth++; $r = $d > 0 ? rec($d - 1) : $depth; $depth--; return $r; }
function trace_it() { static $once; $once ??= (new Exception)->getTraceAsString(); return $once; }
function thrower($v) { static $k = 0; if (++$k === 3) { throw new RuntimeException("third $v"); } return $k; }
class P { function m($a) { static $c = 0; return $a . ++$c; } static function s() { static $c = 100; return ++$c; } function viaSelf() { return self::s() . static::s(); } }
class Q extends P { function viaParent() { return parent::s(); } }
for ($i = 0; $i < 3; $i++) { echo counter(), ' ', withdef($i), ' ', withdef($i, 1), "\n"; }
$x = 'v';
for ($i = 0; $i < 3; $i++) { $r = args_seen($x . $i); }
print_r($r); print_r(args_seen("a", "b"));
echo rec(4), ' ', rec(2), "\n";
echo trace_it(), "\n", trace_it(), "\n";
try { for ($i = 0; $i < 5; $i++) { echo thrower($i), "\n"; } } catch (RuntimeException $e) { echo get_class($e), ': ', $e->getMessage(), ' line ', $e->getLine(), "\n"; }
$p = new P; $q = new Q;
for ($i = 0; $i < 3; $i++) { echo $p->m('p'), $q->m('q'), ' ', P::s(), ' ', Q::s(), ' ', $p->viaSelf(), ' ', $q->viaParent(), "\n"; }
function too_few($a) { static $z = 1; return $a; }
try { too_few(); } catch (ArgumentCountError $e) { echo $e->getMessage(), "\n"; }
class D { function __destruct() { echo "D gone\n"; } }
function keeps() { static $d; $d ??= new D; return 1; }
for ($i = 0; $i < 3; $i++) { echo keeps(); }
echo "\nend\n";
"#),
        r#"1 10 11
2 22 24
3 36 39
Array
(
    [0] => 1:v0
    [1] => 1:v1
    [2] => 1:v2
)
Array
(
    [0] => 1:v0
    [1] => 1:v1
    [2] => 1:v2
    [3] => 2:a,b
)
5 3
#0 t.php(15): trace_it()
#1 {main}
#0 t.php(15): trace_it()
#1 {main}
1
2
RuntimeException: third 2 line 7
p1q2 101 102 103104 105
p3q4 106 107 108109 110
p5q6 111 112 113114 115
Too few arguments to function too_few(), 0 passed in t.php on line 20 and exactly 1 expected
111
end
D gone
"#
    );
}

/// Calls of functions with `static` variables from code whose symbol
/// table is attached (`extract()`, `eval()`, `$$name` at the top level),
/// which makes its calls through `Vm::push_simple_ucall`.
#[test]
fn static_variable_calls_from_attached_frames() {
    assert_eq!(
        run(r#"<?php
function counter() { static $n = 0; return ++$n; }
class M { static function s($a) { static $acc = ''; $acc .= $a; return $acc; } }
function attached() {
    extract(['seed' => 5]);
    for ($i = 0; $i < 4; $i++) { $c = counter(); $s = M::s($i); echo $c, ' ', $s, ' ', $seed, "\n"; }
    return compact('c', 's');
}
print_r(attached());
$code = 'for ($j = 0; $j < 2; $j++) { echo counter(), M::s("e"), "\n"; }';
eval($code);
$vn = 'dyn';
$$vn = 'x';
for ($k = 0; $k < 2; $k++) { echo counter(), ' ', $dyn, "\n"; }
"#),
        r#"1 0 5
2 01 5
3 012 5
4 0123 5
Array
(
    [c] => 4
    [s] => 0123
)
50123e
60123ee
7 x
8 x
"#
    );
}

/// Writes of every kind of value to the variables of frames whose symbol
/// table is attached (a function that used `extract()`, a top-level
/// `eval()`), each mirrored into the table, as `get_defined_vars()`,
/// `compact()` and `$GLOBALS` show.
#[test]
fn writes_in_attached_frames_reach_the_table() {
    assert_eq!(
        run(r#"<?php
function attached() {
    extract(['a' => 1]);
    $b = 2.5; $c = 'str'; $d = null; $e = true;
    for ($i = 0; $i < 3; $i++) { $a += $i; $c .= $i; $b *= 2; }
    $obj = new stdClass; $arr = [1, 2];
    $obj = 5; $arr = 'gone';
    var_dump(get_defined_vars());
    return compact('a', 'c');
}
var_dump(attached());
eval('$x = 1; for ($k = 0; $k < 3; $k++) { $x = $x * 3; $s = "s$k"; }');
var_dump($x, $s, $GLOBALS['x'], $GLOBALS['s']);
"#),
        r#"array(8) {
  ["b"]=>
  float(20)
  ["c"]=>
  string(6) "str012"
  ["d"]=>
  NULL
  ["e"]=>
  bool(true)
  ["i"]=>
  int(3)
  ["a"]=>
  int(4)
  ["obj"]=>
  int(5)
  ["arr"]=>
  string(4) "gone"
}
array(2) {
  ["a"]=>
  int(4)
  ["c"]=>
  string(6) "str012"
}
int(27)
string(2) "s2"
int(27)
string(2) "s2"
"#
    );
}

/// A function called from a top-level `eval()` reads the global scope as
/// the `eval` left it (`$GLOBALS`, `global`), nested `eval`s included, and
/// the main script sees the `eval`'s writes afterwards. The main script's
/// own (older) values must not be copied back over them.
#[test]
fn globals_read_from_inside_a_top_level_eval() {
    assert_eq!(
        run(r#"<?php
$x = 1; $s = 'a';
function reader() { return $GLOBALS['x'] . $GLOBALS['s']; }
function bound() { global $x; return $x; }
eval('$x = 7; $s = "t"; echo reader(), "\n"; $x = 8.5; echo reader(), " ", bound(), "\n";');
echo reader(), " ", bound(), "\n";
eval('$x = 9; eval(\'$x = 10; $s = "inner"; echo reader(), " ", bound(), "\n";\'); echo reader(), "\n"; $x = 11; echo bound(), "\n";');
echo $x, $s, " ", reader(), "\n";
for ($i = 0; $i < 3; $i++) { eval('$x = $i * 2; echo bound(), reader(), "\n";'); }
"#),
        r#"7t
8.5t 8.5
8.5t 8.5
10inner 10
10inner
11
11inner 11inner
00inner
22inner
44inner
"#
    );
}

/// The `$this` and closure handles of prepared method and closure calls
/// released at the return: the last handle of a temporary object (`(new
/// D)->m()`) and of a closure made in the call's expression destroys the
/// object at the return, before the result is used; a handle shared with
/// a variable leaves it alive (`zend_leave_helper`'s `OBJ_RELEASE` of
/// `This` and of the closure).
#[test]
fn this_and_closure_handles_released_at_return() {
    assert_eq!(
        run(r#"<?php
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "destruct {$this->n}\n"; }
  function m($a) { return $a . $this->n; }
  function mk() { return function ($a) { return $a . $this->n; }; } }
function f() {
    for ($i = 0; $i < 3; $i++) {
        echo (new D("t$i"))->m('m'), "\n";
        $o = new D("v$i");
        echo $o->m('k'), "\n";
        $fn = $o->mk();
        unset($o);
        echo $fn('c'), "\n";
        echo "drop fn\n";
        $fn = null;
        $g = function ($a) { return "plain $a"; };
        echo $g($i), "\n";
        echo (new D("w$i"))->mk()('x'), "\n";
    }
}
f();
echo "end\n";
"#),
        r#"destruct t0
mt0
kv0
cv0
drop fn
destruct v0
plain 0
destruct w0
xw0
destruct t1
mt1
kv1
cv1
drop fn
destruct v1
plain 1
destruct w1
xw1
destruct t2
mt2
kv2
cv2
drop fn
destruct v2
plain 2
destruct w2
xw2
end
"#
    );
}

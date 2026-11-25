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

/// `return_simple`'s value shapes into a used and a discarded result:
/// a variable (moved: it trades places with the caller's scalar), a
/// temporary, a literal, strings and scalars, the caller's result slot
/// reused round after round.
#[test]
fn direct_return_value_shapes() {
    assert_eq!(
        run(r#"<?php
function cv($a) { return $a; }
function tmp($a) { return $a . '!'; }
function lit($a) { return 'lit'; }
function num($a) { return $a + 1; }
function f() {
    $out = [];
    $s = str_repeat('s', 3);
    for ($i = 0; $i < 4; $i++) {
        $r = cv($s);
        $out[] = $r;
        $r = cv($i);
        $out[] = $r;
        $r = tmp($s);
        $out[] = $r;
        $r = lit($s);
        $out[] = $r;
        $r = num($i);
        $out[] = $r;
        $r = cv(1.5);
        $out[] = $r;
        cv($s);
        tmp($s);
        lit($i);
        $out[] = cv(null);
        $out[] = cv(true);
    }
    var_dump(count($out), $out[0], $out[2], $out[3], $out[4], $out[5], $out[6], $out[7], $out[8], $s, $r);
}
f();
"#),
        r#"int(32)
string(3) "sss"
string(4) "sss!"
string(3) "lit"
int(1)
float(1.5)
NULL
bool(true)
string(3) "sss"
string(3) "sss"
float(1.5)
"#
    );
}

/// Calls of the same function in a row at one depth (the pooled frame
/// `release_direct` left is readied by its mark alone), alternating with
/// other functions, builtins, methods, closures and recursion at that
/// depth, with defaults, locals, a foreach, `func_get_args()` and a
/// backtrace read in the callee.
#[test]
fn repeated_calls_reuse_the_released_frame() {
    assert_eq!(
        run(r#"<?php
function f($a, $b = 'd') { $l = $a . $b; return $l; }
function g($x) { $t = [$x, $x]; foreach ($t as $k => $v) { $x .= $k; } return $x; }
function n() { return func_num_args() . ':' . implode(',', func_get_args()); }
function bt($a) { $t = debug_backtrace(); return $t[0]['function'] . count($t[0]['args']); }
function rec($n) { return $n <= 0 ? 'r' : rec($n - 1) . $n; }
class M { public $p = 'p'; function m($a) { return $this->p . $a; } static function s($a) { return "s$a"; } }
function run() {
    $o = new M;
    $c = function ($a) { return "c$a"; };
    $out = '';
    for ($i = 0; $i < 5; $i++) {
        $out .= f($i) . f($i, 'x') . f('a');
        $out .= g("g$i") . f($i);
        $out .= strlen(f('abc')) . f($i);
        $out .= n($i, $i + 1) . n() . n($i);
        $out .= bt($i, 7) . bt($i);
        $out .= rec(3) . rec(2);
        $out .= $o->m($i) . f($i) . M::s($i) . $c($i) . f($i);
        $out .= "\n";
    }
    return $out;
}
echo run();
"#),
        r#"0d0xadg0010d40d2:0,10:1:0bt2bt1r123r12p00ds0c00d
1d1xadg1011d41d2:1,20:1:1bt2bt1r123r12p11ds1c11d
2d2xadg2012d42d2:2,30:1:2bt2bt1r123r12p22ds2c22d
3d3xadg3013d43d2:3,40:1:3bt2bt1r123r12p33ds3c33d
4d4xadg4014d44d2:4,50:1:4bt2bt1r123r12p44ds4c44d
"#
    );
}

/// Variables of each kind passed by value to a prepared call (`send`'s
/// copy: scalars inline, strings, arrays and objects by count, a
/// variable bound by reference dereferenced, an undefined one warned
/// about by the generic send).
#[test]
fn variables_sent_by_value() {
    assert_eq!(
        run(r#"<?php
function id($a) { return $a; }
function two($a, $b) { $b[] = 1; return [$a, count($b)]; }
function run() {
    $int = 7; $float = 1.25; $bool = false; $null = null; $str = str_repeat('x', 2);
    $arr = [1, 2]; $obj = new stdClass; $obj->p = 1;
    $ref = 5; $alias = &$ref;
    $out = [];
    for ($i = 0; $i < 3; $i++) {
        $out[] = id($int); $out[] = id($float); $out[] = id($bool); $out[] = id($null);
        $out[] = id($str); $out[] = id($arr); $out[] = id($obj); $out[] = id($ref);
        $out[] = two($str, $arr);
        $alias++;
    }
    $out[] = id($undef);
    var_dump($out[0], $out[1], $out[2], $out[3], $out[4], $out[5], $out[6] === $obj, $out[7], $out[8], $out[15], $out[23], $arr, count($out), end($out));
}
run();
"#),
        r#"
Warning: Undefined variable $undef in t.php on line 15
int(7)
float(1.25)
bool(false)
NULL
string(2) "xx"
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
bool(true)
int(5)
array(2) {
  [0]=>
  string(2) "xx"
  [1]=>
  int(3)
}
object(stdClass)#1 (1) {
  ["p"]=>
  int(1)
}
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
  int(2)
}
int(28)
NULL
"#
    );
}

/// Closures of each kind called through the prepared call: without and
/// with `use`d variables (by value, by reference, an arrow function's
/// captures), fake closures of a function, a static closure, one bound to
/// `$this`, and closures as `call_user_func()` callbacks.
#[test]
fn closure_calls_bind_their_variables() {
    assert_eq!(
        run(r#"<?php
function plain($a) { return "p$a"; }
class K { public $v = 'k'; function get() { return function ($a) { return $this->v . $a; }; } }
function run() {
    $u = 'u'; $r = 0;
    $none = function ($a) { return "n$a"; };
    $uses = function ($a) use ($u) { return "$u$a"; };
    $byref = function ($a) use (&$r) { $r++; return "r$r$a"; };
    $arrow = fn($a) => $u . $a . 'f';
    $fake = plain(...);
    $fake2 = Closure::fromCallable('plain');
    $static = static function ($a) { return "s$a"; };
    $bound = (new K)->get();
    $out = '';
    for ($i = 0; $i < 3; $i++) {
        $out .= $none($i) . $uses($i) . $byref($i) . $arrow($i) . $fake($i) . $fake2($i)
            . $static($i) . $bound($i) . call_user_func($uses, $i) . call_user_func($none, $i) . "\n";
        $u .= '+';
    }
    return $out . $r;
}
echo run(), "\n";
"#),
        r#"n0u0r10u0fp0p0s0k0u0n0
n1u1r21u1fp1p1s1k1u1n1
n2u2r32u2fp2p2s2k2u2n2
3
"#
    );
}

/// `return;` and the implicit return at the end of a body on the direct
/// return: constructors (with a default filled), procedures whose result
/// is used or not, a `void` function, an early `return;`, and a body
/// whose local object is destroyed at the return (before the caller goes
/// on).
#[test]
fn returns_without_a_value() {
    assert_eq!(
        run(r#"<?php
class D { function __destruct() { echo "destruct\n"; } }
class K { public $a; public $b; function __construct($a, $b = 2) { $this->a = $a; $this->b = $b; } }
class E { public $d; function __construct() { $this->d = new D; } }
function proc($x) { global $log; $log .= $x; }
function early($x) { if ($x) { return; } echo "fell\n"; }
function vd($x): void { if ($x > 1) { return; } }
function local() { $d = new D; }
function nt(): ?int { if (false) { return 1; } return null; }
function run() {
    global $log;
    for ($i = 0; $i < 3; $i++) {
        $k = new K($i);
        echo $k->a, $k->b, "\n";
        proc($i);
        $r = proc($i);
        var_dump($r);
        early($i);
        var_dump(vd($i));
        local();
        echo "after local\n";
        $e = new E;
        $e = null;
        echo "after e\n";
        var_dump(nt());
    }
    echo $log, "\n";
}
run();
"#),
        r#"02
NULL
fell
NULL
destruct
after local
destruct
after e
NULL
12
NULL
NULL
destruct
after local
destruct
after e
NULL
22
NULL
NULL
destruct
after local
destruct
after e
NULL
001122
"#
    );
}

/// Different functions taking turns at one stack depth, so that each call
/// readies the pooled frame another function used last: functions of
/// different numbers of variables and temporaries, a generator, a frame
/// left by an exception, `get_defined_vars()` and `compact()` in a reused
/// frame, a function called with ints and with strings, a recursion
/// beside a smaller function. Each reuse must find its variables
/// undefined and its defaults bound, and release what the previous call
/// held at that call's return (php8.5 -n).
#[test]
fn alternating_functions_reuse_the_frame_at_each_depth() {
    let src = r#"<?php
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "~{$this->n} "; } }
function big($a, $b) { $c = $a + 1; $d = $b; $e = $c * 2; $f = [$a]; $g = $e - $c; $h = null; $o = new D("big$a"); if ($g > 1000000) { $h = $f; } return $g; }
function small($x) { return $x . "!"; }
function vars($p, $q = 'dq') { $r = $p; return get_defined_vars(); }
function names($u) { $w = $u * 2; return compact('u', 'w'); }
function gen($n) { for ($i = 0; $i < $n; $i++) { $v = new D("g$i"); yield $i => $v->n; } }
function thrower($x) { $t = new D("t$x"); $y = [$x, $x]; throw new Exception("e$x"); }
function sum3($a, $b, $c) { return $a + $b + $c; }
function mixed_($a, $b) { return is_int($a) ? $a + $b : $a . $b; }
for ($i = 0; $i < 3; $i++) {
  echo big($i, "s"), " ", small($i), " ";
  echo json_encode(vars($i)), " ", json_encode(names($i)), " ";
  foreach (gen(2) as $k => $v) { echo "$k=$v "; }
  echo sum3($i, 1, 2), " ";
  try { thrower($i); } catch (Exception $e) { echo $e->getMessage(), " "; }
  echo json_encode(vars("after$i", [1])), " ", small("z"), "\n";
}
$acc = 0; $s = '';
for ($i = 0; $i < 400; $i++) { $s = mixed_("a", $i % 10); $acc = mixed_($acc, $i); }
echo $s, " ", $acc, "\n";
function depth($n) { return $n ? depth($n - 1) + small2($n) : 0; }
function small2($n) { $a = $n; $b = [$a]; return count($b); }
echo depth(50), " ", depth(3), "\n";
"#;
    assert_eq!(
        run(src),
        "~big0 1 0! {\"p\":0,\"q\":\"dq\",\"r\":0} {\"u\":0,\"w\":0} 0=g0 ~g0 1=g1 ~g1 3 ~t0 e0 {\"p\":\"after0\",\"q\":[1],\"r\":\"after0\"} z!\n\
         ~big1 2 1! {\"p\":1,\"q\":\"dq\",\"r\":1} {\"u\":1,\"w\":2} 0=g0 ~g0 1=g1 ~g1 4 ~t1 e1 {\"p\":\"after1\",\"q\":[1],\"r\":\"after1\"} z!\n\
         ~big2 3 2! {\"p\":2,\"q\":\"dq\",\"r\":2} {\"u\":2,\"w\":4} 0=g0 ~g0 1=g1 ~g1 5 ~t2 e2 {\"p\":\"after2\",\"q\":[1],\"r\":\"after2\"} z!\n\
         a9 79800\n\
         50 3\n"
    );
}

/// Defaults computed by code (constants, class constants, an expression of
/// them, `new` in an initializer, an undefined constant) of parameters a
/// call did not pass, typed ones coerced or rejected, a named-argument gap
/// filled: the `RecvInit` goes on to the default's code and its
/// `RecvDefault` binds the computed value (php8.5 -n).
#[test]
fn computed_parameter_defaults() {
    let src = r#"<?php
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "~{$this->n} "; } }
class K { const C = [1, 2]; const S = 'ks'; }
const MINE = 42;
function c1($x, $y = E_ALL, $z = PHP_EOL, $w = K::C, $v = K::S . '!', $u = MINE * 2) { return [$x, $y, $z, $w, $v, $u]; }
function c2(float $f = MINE, ?string $s = K::S, int|string $m = MINE) { var_dump($f, $s, $m); }
function c3($o = new D('dflt')) { echo "in "; $o = 1; echo "reset "; }
function c4(int $i = K::S) { return $i; }
function c5($a, $b = UNDEFINED_CONST_X) { return $a; }
function c6($a, $b = [MINE, K::S], $c = null) { return func_get_args(); }
for ($i = 0; $i < 3; $i++) {
  echo json_encode(c1($i)), json_encode(c1($i, 5)), json_encode(c1($i, 5, 6, 7, 8, 9)), "\n";
  c2(); c2(1.5, null, 'x');
  c3(); echo "after\n";
  try { c4(); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
  try { c5(1); } catch (Error $e) { echo $e->getMessage(), "\n"; }
  echo json_encode(c6(1)), json_encode(c6(1, 2)), json_encode(c6(1, c: 3)), "\n";
}
"#;
    assert_eq!(run(src), "[0,30719,\"\\n\",[1,2],\"ks!\",84][0,5,\"\\n\",[1,2],\"ks!\",84][0,5,6,7,8,9]\nfloat(42)\nstring(2) \"ks\"\nint(42)\nfloat(1.5)\nNULL\nstring(1) \"x\"\nin ~dflt reset after\nc4(): Argument #1 ($i) must be of type int, string given, called in t.php on line 15\nUndefined constant \"UNDEFINED_CONST_X\"\n[1][1,2][1,[42,\"ks\"],3]\n[1,30719,\"\\n\",[1,2],\"ks!\",84][1,5,\"\\n\",[1,2],\"ks!\",84][1,5,6,7,8,9]\nfloat(42)\nstring(2) \"ks\"\nint(42)\nfloat(1.5)\nNULL\nstring(1) \"x\"\nin ~dflt reset after\nc4(): Argument #1 ($i) must be of type int, string given, called in t.php on line 15\nUndefined constant \"UNDEFINED_CONST_X\"\n[1][1,2][1,[42,\"ks\"],3]\n[2,30719,\"\\n\",[1,2],\"ks!\",84][2,5,\"\\n\",[1,2],\"ks!\",84][2,5,6,7,8,9]\nfloat(42)\nstring(2) \"ks\"\nint(42)\nfloat(1.5)\nNULL\nstring(1) \"x\"\nin ~dflt reset after\nc4(): Argument #1 ($i) must be of type int, string given, called in t.php on line 15\nUndefined constant \"UNDEFINED_CONST_X\"\n[1][1,2][1,[42,\"ks\"],3]\n");
}

/// Calls whose arguments make calls of their own (`f(g($x))`: the outer
/// call is entered by `Vm::push_simple_ucall`, since the inner one used
/// the prepared frame), with what reads the passed arguments
/// (`func_num_args()`, `func_get_args()`, a backtrace's arguments,
/// `get_defined_vars()`), defaults and a named argument, a variadic, a
/// by-reference parameter left to its default, a closure, a method, a
/// constructor, and an exception thrown out of such a frame before the
/// next call reuses it (php8.5 -n).
#[test]
fn calls_with_calls_in_their_arguments() {
    let src = r#"<?php
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "~{$this->n} "; } }
class K { public $a; function __construct($a, $b = 2) { $this->a = $a + $b; } function m($x, $y = 'y') { return func_num_args() . ":$x$y"; } }
function one($x) { return $x + 1; }
function args($a, $b = 'B', $c = null) { return func_num_args() . json_encode(func_get_args()); }
function trace($a, $b) { return json_encode(debug_backtrace()[0]['args']); }
function va($a, ...$rest) { return func_num_args() . json_encode([$a, $rest, func_get_args()]); }
function rf($a, &$r = null) { $r = 'set'; return $a; }
function th($a, $b) { $d = new D("th$a"); if ($b) { throw new Exception("x$a"); } return $a; }
function vars($a, $b) { $c = $a . $b; return json_encode(get_defined_vars()); }
$u = "U"; $cl = function ($a, $b) use ($u) { return "cl$a$b$u"; };
$k = new K(1);
for ($i = 0; $i < 3; $i++) {
  echo args(one($i)), " ", args(one($i), one($i)), " ", args(one($i), c: 5), "\n";
  echo trace(one($i), "t"), " ", va(one($i)), " ", va(one($i), 2, 3), "\n";
  echo rf(one($i)), " ", $cl(one($i), 'q'), " ", $k->m(one($i)), " ", (new K(one($i)))->a, "\n";
  try { echo th(one($i), 0), th(one($i), 1); } catch (Exception $e) { echo $e->getMessage(), " "; }
  echo vars(one($i), "v"), " ", vars("w", one($i)), "\n";
}
"#;
    assert_eq!(run(src), "1[1] 2[1,1] 3[1,\"B\",5]\n[1,\"t\"] 1[1,[],[1]] 3[1,[2,3],[1,2,3]]\n1 cl1qU 1:1y 3\n~th1 1~th1 x1 {\"a\":1,\"b\":\"v\",\"c\":\"1v\"} {\"a\":\"w\",\"b\":1,\"c\":\"w1\"}\n1[2] 2[2,2] 3[2,\"B\",5]\n[2,\"t\"] 1[2,[],[2]] 3[2,[2,3],[2,2,3]]\n2 cl2qU 1:2y 4\n~th2 2~th2 x2 {\"a\":2,\"b\":\"v\",\"c\":\"2v\"} {\"a\":\"w\",\"b\":2,\"c\":\"w2\"}\n1[3] 2[3,3] 3[3,\"B\",5]\n[3,\"t\"] 1[3,[],[3]] 3[3,[2,3],[3,2,3]]\n3 cl3qU 1:3y 5\n~th3 3~th3 x3 {\"a\":3,\"b\":\"v\",\"c\":\"3v\"} {\"a\":\"w\",\"b\":3,\"c\":\"w3\"}\n");
}

/// Arrays, objects and variables bound by reference returned from a
/// variable, a temporary or a literal (`Vm::return_simple`'s direct
/// return): the value returned, the order the frame's objects are
/// destructed in (a returned local survives, the others go at the
/// return, in variable order; an unused result goes first), the roots
/// the release buffers (`gc_status()`), static variables, extra
/// arguments and a return type the value fails.
#[test]
fn counted_values_return_directly() {
    assert_eq!(
        run(r#"<?php
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "destruct {$this->n}\n"; } }
function ra($a, $b) { return $a; }
function rl() { $x = new D('local'); $y = new D('other'); echo "in rl\n"; return $x; }
function rt(): array { $t = [1, 2]; return $t; }
function rtmp() { return [new D('tmp')][0]; }
function rref() { $v = [1]; $r = &$v; $r[] = 2; return $r; }
function robj(D $d): D { return $d; }
function rbad(): int { $a = [1]; return $a; }
$arr = [1, 2, 3];
for ($i = 0; $i < 3; $i++) { $r = ra($arr, 'x'); }
var_dump($r === $arr);
$o = rl();
echo "got {$o->n}\n";
var_dump(rt(), rref());
$t = rtmp();
echo "tmp {$t->n}\n";
rl();
echo "discarded\n";
$d = new D('arg');
var_dump(robj($d) === $d);
unset($o, $t);
echo "unset\n";
gc_collect_cycles();
$s0 = gc_status()['roots'];
for ($i = 0; $i < 5; $i++) { $a2 = [$i]; $x = ra($a2, $i); }
var_dump(gc_status()['roots'] - $s0);
function cyc() { $c = new stdClass; $c->self = $c; return $c; }
$c = cyc();
var_dump(gc_status()['roots'] - $s0);
function stat_arr() { static $st = [1]; $st[] = 2; return $st; }
stat_arr(); var_dump(count(stat_arr()));
function args() { $a = func_get_args(); return $a; }
var_dump(args(1, [2], 'three'));
try { rbad(); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
echo "end\n";
"#),
        r#"bool(true)
in rl
destruct other
got local
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
  int(2)
}
tmp tmp
in rl
destruct local
destruct other
discarded
bool(true)
destruct local
destruct tmp
unset
int(1)
int(2)
int(3)
array(3) {
  [0]=>
  int(1)
  [1]=>
  array(1) {
    [0]=>
    int(2)
  }
  [2]=>
  string(5) "three"
}
rbad(): Return value must be of type int, array returned
end
destruct arg
"#
    );
}

/// The release of a returning frame's variables, one pass over them
/// (`Frame::release_returning` of a directly entered frame): scalars,
/// strings, a shared array, a reference to a global, objects destructed
/// at the return in variable order, the roots buffered (`gc_status()`),
/// garbage cycles left by returning frames, a by-reference parameter,
/// a variadic call's extra arguments and a method's `$this`.
#[test]
fn returning_frames_release_their_variables_in_order() {
    assert_eq!(
        run(r#"<?php
class D { public $n; public $peer; function __construct($n) { $this->n = $n; } function __destruct() { echo "destruct {$this->n}\n"; } }
function mix($keep) {
    $i = 1; $f = 1.5; $s = str_repeat('s', 3); $a = [1, 2]; $shared = $keep;
    $ref = &$GLOBALS['g']; $o = new D('o'); $p = new D('p'); $o->peer = $p;
    $q = new D('q');
    return $s . $i;
}
$g = [1];
$keep = [new D('kept')];
gc_collect_cycles();
$r0 = gc_status()['roots'];
for ($k = 0; $k < 4; $k++) { $x = mix($keep); echo "returned $x\n"; }
var_dump(gc_status()['roots'] - $r0);
function cyc($n) { $a = new stdClass; $b = new stdClass; $a->b = $b; $b->a = $a; $t = $n; return $t; }
for ($k = 0; $k < 3; $k++) cyc($k);
var_dump(gc_status()['roots'] - $r0);
var_dump(gc_collect_cycles());
function byref(&$v) { $v[] = 1; $w = $v; return $w; }
$z = [];
var_dump(count(byref($z)), count($z));
function variadic($a, ...$rest) { $c = count($rest); return $c; }
var_dump(variadic(1, 2, [3], new D('extra')));
class M { function m($a, $b) { $t = $this; return $a; } }
$m = new M; $r0 = gc_status()['roots'];
for ($k = 0; $k < 3; $k++) { $y = $m->m([$k], 's'); }
var_dump(gc_status()['roots'] - $r0);
echo "end\n";
"#),
        r#"destruct o
destruct p
destruct q
returned sss1
destruct o
destruct p
destruct q
returned sss1
destruct o
destruct p
destruct q
returned sss1
destruct o
destruct p
destruct q
returned sss1
int(3)
int(9)
int(6)
int(1)
int(1)
destruct extra
int(3)
int(2)
end
destruct kept
"#
    );
}

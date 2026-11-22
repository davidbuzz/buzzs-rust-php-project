// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Script-level tests of the dispatch loop (`Vm::run`): the local `ip`,
//! jumps, safe points and the loop layout. Expected output from
//! `/usr/bin/php8.5 -n`, paths normalised to `t.php`.

#![allow(clippy::needless_raw_string_hashes)]

use crate::{Vm, VmOptions};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run(src: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(src.as_bytes(), "t.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

/// `for`/`while`/`continue N`/`break` on the local `ip`.
#[test]
fn loops_with_jumps_continue_and_break() {
    let src = r#"<?php
for ($i = 0; $i < 3; $i++) { echo $i; }
echo "\n";
for ($i = 0, $j = 10; $i < 10; $i++, $j--) {
    if ($i == 2) continue;
    if ($i == 6) break;
    echo $i, ":", $j, " ";
}
echo "\n";
$n = 0;
for (;;) { if (++$n > 4) break; echo $n; }
echo "\n";
for ($i = 0; print("c"), $i < 2; $i++) { echo "b"; }
echo "\n";
$k = 3;
while ($k--) { echo $k; }
echo "\n";
$k = 0;
while ($k < 10) { $k++; if ($k % 2) continue; echo $k; if ($k > 6) break; }
echo "\n";
for ($i = 0; $i < 3; $i++) { for ($j = 0; $j < 3; $j++) { if ($j == 1) continue 2; echo "$i$j "; } }
echo "\n";
while (false) { echo "never"; }
for ($i = 0; $i < 0; $i++) { echo "never"; }
$x = 1.5;
for ($f = 0.0; $f < $x; $f += 0.5) { echo $f, " "; }
echo "\n";
$s = "a";
for ($i = 0; $i < "3"; $i++) { $s .= $i; }
echo $s, "\n";
"#;
    assert_eq!(
        run(src),
        r#"012
0:10 1:9 3:7 4:6 5:5 
1234
cbcbc
210
2468
00 10 20 
0 0.5 1 
a012
"#
    );
}

/// Warnings from conditions and throws after `finally` report their own line (the frame `ip` is
/// stored before slow instructions).
#[test]
fn warnings_and_throws_in_loop_conditions_report_their_line() {
    let src = r#"<?php
for ($i = 0;
     $i < $undef1;
     $i++) {
}
while ($undef2) {
}
$i = 0;
while ($i < 2
       && $undef3 === null) {
    $i++;
}
for ($i = 0; $i < 2; $i++) {
    try {
        if ($i) throw new Exception("e$i");
        echo "t$i ";
    } finally {
        echo "f$i ";
    }
}
"#;
    assert_eq!(
        run(src),
        r#"
Warning: Undefined variable $undef1 in t.php on line 3

Warning: Undefined variable $undef2 in t.php on line 6

Warning: Undefined variable $undef3 in t.php on line 10

Warning: Undefined variable $undef3 in t.php on line 10
t0 f0 f1 
Fatal error: Uncaught Exception: e1 in t.php:15
Stack trace:
#0 {main}
  thrown in t.php on line 15
"#
    );
}

/// Destructors run at the safe point after an instruction see the line of the instruction that
/// released the object, also right after jumps and in generators.
#[test]
fn destructors_at_safe_points_see_the_current_line() {
    let src = r#"<?php
class D {
    function __construct(public $n) {}
    function __destruct() {
        $t = debug_backtrace();
        echo "d", $this->n, "@", $t[0]['line'] ?? '-', "\n";
    }
}
for ($i = 0; $i < 3; $i++) {
    $o = new D($i);
}
$o = null;
$a = [new D(10), new D(11)];
$k = 1;
while ($k >= 0) {
    unset($a[$k--]);
}
function g() {
    for ($i = 0; $i < 2; $i++) {
        try {
            $x = new D(20 + $i);
            if ($i) return $x->n;
        } finally {
            echo "fin", $i, "\n";
        }
    }
}
echo g(), "\n";
function gen() {
    $i = 0;
    while (($v = yield $i) !== null) {
        $i += $v;
    }
    return $i;
}
$g = gen();
$g->current();
$g->send(2);
$g->send(3);
$g->send(null);
echo $g->getReturn(), "\n";
"#;
    assert_eq!(
        run(src),
        r#"d0@10
d1@10
d2@12
d11@16
d10@16
fin0
d20@21
fin1
d21@28
21
5
"#
    );
}

/// Instructions that jump from the generic dispatch (catch, static, default arguments, `new`
/// without constructor, `?->`, `??`, `?:`, match, foreach, goto, finally) move the local `ip`.
#[test]
fn handler_jumps_move_the_local_ip() {
    let src = r#"<?php
class A extends Exception {}
class B extends Exception {}
foreach ([new B("b"), new A("a"), new Exception("e")] as $e) {
    try {
        throw $e;
    } catch (A $x) {
        echo "A:", $x->getMessage(), " ";
    } catch (B | LogicException $x) {
        echo "B:", $x->getMessage(), " ";
    } catch (Exception $x) {
        echo "E:", $x->getMessage(), " ";
    }
}
echo "\n";
function cnt() { static $n = 0; return ++$n; }
cnt(); cnt(); echo cnt(), "\n";
function def($a, $b = 5, $c = PHP_INT_SIZE) { return $a + $b + $c; }
echo def(1), " ", def(1, 2), " ", def(1, 2, 3), "\n";
class NoCtor { public $v = 7; }
echo (new NoCtor)->v, "\n";
$o = null;
echo $o?->x?->y ?? "nul", " ", $u ?? "dflt", " ", 0 ?: "elvis", " ", (1 && 0) ? "t" : "f", "\n";
echo match (true) { 1 > 2 => "a", 2 > 1 => "b", default => "c" }, "\n";
function find($arr, $want) {
    foreach ($arr as $k => $v) {
        foreach ([1, 2] as $w) {
            if ($v * $w == $want) return "$k/$w";
        }
    }
    return "none";
}
echo find([3, 4, 5], 8), " ", find([1], 9), "\n";
$i = 0;
top:
$i++;
if ($i < 3) goto top;
echo $i, "\n";
for ($i = 0; $i < 5; $i++) {
    if ($i == 3) goto out;
    echo $i;
}
out:
echo "\n";
$r = 0;
do { $r++; } while ($r < 4);
echo $r, "\n";
switch (3) { case 1: echo "one"; break; case 3: echo "three"; case 4: echo "four"; break; default: echo "d"; }
echo "\n";
function fin() {
    for ($i = 0; $i < 3; $i++) {
        try {
            try {
                if ($i == 1) continue;
                if ($i == 2) return "r$i";
            } finally {
                echo "in$i ";
            }
        } finally {
            echo "out$i ";
        }
    }
}
echo fin(), "\n";
echo @$undefined_var ?? "silenced", "\n";
"#;
    assert_eq!(
        run(src),
        r#"B:b A:a E:e 
3
14 11 6
7
nul dflt elvis f
b
1/2 none
3
012
4
threefour
in0 out0 in1 out1 in2 out2 r2
silenced
"#
    );
}

/// `Op::Bin` falls back to the generic path with the frame `ip` stored: its exceptions carry the
/// line.
#[test]
fn throws_from_the_numeric_fast_path_fallback_report_their_line() {
    let src = r#"<?php
function lim() { throw new RuntimeException("lim"); }
try {
    for ($i = 0; $i < lim(); $i++) {}
} catch (RuntimeException $e) {
    echo $e->getMessage(), "@", $e->getLine(), "\n";
}
$z = 0;
for ($i = 0; $i < 3; $i++) {
    try {
        echo 10 % ($z + $i), " ";
    } catch (DivisionByZeroError $e) {
        echo $e->getMessage(), "@", $e->getLine(), " ";
    }
}
echo "\n";
$a = [];
try {
    $b = 1;
    $c = $a + $b;
} catch (TypeError $e) {
    echo $e->getMessage(), "@", $e->getLine(), "\n";
}
for ($i = 0; $i < 2; $i++) {}
echo 1 % $z;
"#;
    assert_eq!(
        run(src),
        r#"lim@2
Modulo by zero@11 0 0 
Unsupported operand types: array + int@20

Fatal error: Uncaught DivisionByZeroError: Modulo by zero in t.php:25
Stack trace:
#0 {main}
  thrown in t.php on line 25
"#
    );
}

/// `++`/`--` on an int CV runs on the local `ip`; overflow, references,
/// floats, numeric strings, null, undefined variables, statics and
/// dimensions take the generic path (with its warnings and line).
#[test]
fn incdec_fast_path_and_its_fallbacks() {
    let src = r#"<?php
$i = 5;
$a = $i++;
$b = ++$i;
$c = $i--;
$d = --$i;
var_dump($a, $b, $c, $d, $i);
$m = PHP_INT_MAX;
$m++;
var_dump($m);
$n = PHP_INT_MIN;
$p = $n--;
var_dump($p, $n);
$r = 1;
$x = &$r;
$x++;
var_dump($r);
$s = "9";
$s++;
var_dump($s);
$f = 1.5;
$f--;
var_dump($f);
$z = null;
$z--;
var_dump($z);
$u++;
var_dump($u);
function counter() { static $k = 0; return ++$k; }
counter();
var_dump(counter());
$arr = [1];
$arr[0]++;
var_dump($arr);
"#;
    assert_eq!(
        run(src),
        r#"int(5)
int(7)
int(7)
int(5)
int(5)
float(9.223372036854776E+18)
int(-9223372036854775808)
float(-9.223372036854776E+18)
int(2)
int(10)
float(0.5)

Warning: Decrement on type null has no effect, this will change in the next major version of PHP in t.php on line 25
NULL

Warning: Undefined variable $u in t.php on line 27
int(1)
int(2)
array(1) {
  [0]=>
  int(2)
}
"#
    );
}

/// `zend_compile_while`/`zend_compile_for` compile the body before the
/// condition (and `for` the step too): CVs, so `get_defined_vars()`, follow
/// that order.
#[test]
fn loop_bodies_are_compiled_before_conditions() {
    let src = r#"<?php
function f() {
    $i = 0;
    while ($a = $i++ < 1) { $b = 1; }
    foreach (get_defined_vars() as $k => $v) { echo $k, ","; }
    echo "\n";
}
f();
function g() {
    for ($i = 0; $c = $i < 1; $d = $i++) { $e = 1; }
    foreach (get_defined_vars() as $k => $v) { echo $k, ","; }
    echo "\n";
}
g();
"#;
    assert_eq!(
        run(src),
        r#"i,b,a,
i,e,d,c,
"#
    );
}

/// `%`, `/`, `<<` and `>>` on int and float operands run in the sealed
/// numeric form (`num_bin`); zero divisors, `PHP_INT_MIN % -1`,
/// `PHP_INT_MIN / -1`, negative and 64-bit shifts and non-int operands of
/// `%` (with its deprecation) are left to the generic path. String
/// concatenation and comparisons skip the equality probe or take it as
/// they apply. Expected output from `/usr/bin/php8.5 -n`.
#[test]
fn division_modulo_and_shifts_on_the_numeric_fast_path() {
    let src = r#"<?php
function show($v) { echo var_export($v, true), "|"; }
$pairs = [[7, 3], [-7, 3], [7, -3], [-7, -3], [5, -1], [0, 5], [PHP_INT_MIN, -1], [6, 3], [7, 2]];
foreach ($pairs as [$a, $b]) {
    show($a % $b); show($a / $b);
    $m = $a % $b; $d = $a / $b;
    show($m); show($d);
    echo "\n";
}
$x = 7.0; $y = 2; $z = 0.5;
show($x / $y); show($y / $z); show($x / $z); show(-$x / $y);
$i = 0; $f = 0.0; $nf = -0.0;
foreach ([[1, $i], [1, $f], [1.5, $f], [2, $nf], [7, 0]] as [$p, $q]) {
    try { show($p / $q); } catch (DivisionByZeroError $e) { echo get_class($e), ":", $e->getMessage(), "|"; }
}
try { show(7 % $i); } catch (DivisionByZeroError $e) { echo get_class($e), ":", $e->getMessage(), "|"; }
echo "\n";
$inf = INF; $nan = NAN;
show($inf / 2); show(2 / $inf); show(is_nan($nan / 2)); show(0.0 / 5);
echo "\n";
foreach ([[1, 3], [1, 63], [1, 64], [3, 100], [-8, 1], [-8, 64], [8, 64], [-1, 1], [PHP_INT_MAX, 1], [PHP_INT_MIN, 63]] as [$a, $b]) {
    show($a << $b); show($a >> $b);
}
echo "\n";
$neg = -1;
try { show(1 << $neg); } catch (ArithmeticError $e) { echo get_class($e), ":", $e->getMessage(), "|"; }
try { show(1 >> $neg); } catch (ArithmeticError $e) { echo get_class($e), ":", $e->getMessage(), "|"; }
echo "\n";
$s = 0; $t = 1.0;
for ($k = 1; $k <= 20; $k++) {
    $s = $s + ($k % 7) * ($k >> 1) - ($k << 2) % 5;
    $t = $t + $k / 4;
}
show($s); show($t);
echo "\n";
$fs = 7.5; $str = "9";
show($fs % 2); show($str % 4); show($str / 2); show("8" >> 1);
echo "\n";
$c = "a"; for ($k = 0; $k < 3; $k++) { $c = $c . $k; } show($c);
$e1 = "x"; $e2 = "x"; show($e1 == $e2); show($e1 === $e2); show($e1 != "y"); show($c < "b");
echo "\n";
"#;
    assert_eq!(
        run(src),
        r#"1|2.3333333333333335|1|2.3333333333333335|
-1|-2.3333333333333335|-1|-2.3333333333333335|
1|-2.3333333333333335|1|-2.3333333333333335|
-1|2.3333333333333335|-1|2.3333333333333335|
0|-5|0|-5|
0|0|0|0|
0|9.223372036854776E+18|0|9.223372036854776E+18|
0|2|0|2|
1|3.5|1|3.5|
3.5|4.0|14.0|-3.5|DivisionByZeroError:Division by zero|DivisionByZeroError:Division by zero|DivisionByZeroError:Division by zero|DivisionByZeroError:Division by zero|DivisionByZeroError:Division by zero|DivisionByZeroError:Modulo by zero|
INF|0.0|true|0.0|
8|0|-9223372036854775807-1|0|0|0|0|0|-16|-4|0|-1|0|0|-2|-1|-2|4611686018427387903|0|-1|
ArithmeticError:Bit shift by negative number|ArithmeticError:Bit shift by negative number|
302|53.5|

Deprecated: Implicit conversion from float 7.5 to int loses precision in t.php on line 37
1|1|4.5|4|
'a012'|true|true|true|true|
"#
    );
}

/// The frame-local hot loop (`exec/hot.rs`): int overflow of `++`/`--` and
/// `+`, floats, post/pre results, string (in)equality in branches, `??`,
/// `?:`, compound assignment as a loop condition, a reference to the loop
/// variable, variables of an attached symbol table (`global`, `$GLOBALS`,
/// `extract()`), destructors of overwritten objects, comparison results
/// stored into variables.
#[test]
fn hot_loop_fast_paths_match_the_generic_ops() {
    let src = r#"<?php
$i = PHP_INT_MAX - 2;
for ($k = 0; $k < 4; $k++) { $i++; }
var_dump($i);
$j = PHP_INT_MIN + 1;
$j--; $j--;
var_dump($j);
$f = 0.5;
for ($k = 0; $k < 3; $k++) { $a = $f++; $b = ++$f; }
var_dump($f, $a, $b);
$x = 0;
for ($i = 0; $i < 10; $i++) { $x = $x + $i * 2.5; $y = $x - 1; }
var_dump($x, $y);
$m = PHP_INT_MAX;
$m = $m + 1;
var_dump($m);
$s = "abc";
$n = 0;
for ($i = 0; $i < 5; $i++) { if ($s === "abc") { $n += 2; } if ($s == "abd") { $n = -100; } }
var_dump($n);
$t = "1";
for ($i = 0; $i < 3; $i++) { if ($t == 1) { $n -= 1; } if ($t == "01") { $n *= 2; } }
var_dump($n);
$v = null;
$w = $v ?? "def";
$u = $n ?: "zero";
$z = 0 ?: "zero";
var_dump($w, $u, $z);
$q = 10;
while ($q -= 3) { if ($q < 0) break; echo $q, " "; }
echo "\n";
$r = &$i;
for ($i = 0; $i < 3; $i++) { $r += 1; }
var_dump($i, $r);
function g() { global $gi; for ($gi = 0; $gi < 5; $gi++) { $gi += 1; } return $gi; }
var_dump(g(), $gi);
for ($h = 0; $h < 3; $h++) { $GLOBALS['h2'] = $h * 2; }
var_dump($h, $h2);
extract(['e' => 5]);
for ($e2 = 0; $e2 < $e; $e2++) { $e = $e - 1; }
var_dump($e, $e2, compact('e', 'e2'));
class D { function __construct(public $n) {} function __destruct() { echo "d", $this->n, " "; } }
for ($i = 0; $i < 3; $i++) { $o = new D($i); $o = $i; }
echo "\n";
$c = 0;
for ($i = 0; $i < 4; $i++) { $c = $c + ($i < 2); $c = $c + ($i <=> 2); }
var_dump($c);
$fl = 1.0;
for ($i = 0; $i < 3; $i++) { $fl *= 1.5; $fl /= 0.5; }
var_dump($fl);
$u = 5; $u++; $p = $u--; var_dump($u, $p);
$st = "5"; $st++; var_dump($st);
$nn = null; $nn++; var_dump($nn);
for ($i = 0; $i < 3; $i++) { $i = $i + 1; } var_dump($i);
"#;
    assert_eq!(
        run(src),
        r#"float(9.223372036854776E+18)
float(-9.223372036854776E+18)
float(6.5)
float(4.5)
float(6.5)
float(112.5)
float(111.5)
float(9.223372036854776E+18)
int(10)
int(66)
string(3) "def"
int(66)
string(4) "zero"
7 4 1 
int(4)
int(4)
int(6)
int(6)
int(3)
int(4)
int(2)
int(3)
array(2) {
  ["e"]=>
  int(2)
  ["e2"]=>
  int(3)
}
d0 d1 d2 
int(1)
float(27)
int(5)
int(6)
int(6)
int(1)
int(4)
"#
    );
}

/// The hot loop's assignments over arrays and objects (released as
/// possible GC roots, destructors in order, the result stored before the
/// variable), `&&`, `||`, `!` and `(bool)`, and an attached symbol table.
#[test]
fn hot_loop_assignments_and_logic_match_the_generic_ops() {
    let src = r#"<?php
class D { function __construct(public $n) {} function __destruct() { echo "d", $this->n, " "; } }
$val = [1, 2, 3];
$f = false;
for ($i = 0; $i < 3; $i++) { $x = $val ?: null; $y = $f ? null : $val; $z = $f ?: $i + 1; }
var_dump(count($x), count($y), $z);
for ($i = 0; $i < 3; $i++) { $o = new D($i); }
echo "| ";
$p = new D(10);
$q = $p;
$p = new D(11);
echo "| ";
$q = $o = 5;
echo "\n";
$a = 3; $b = 0; $n = 0;
for ($i = 0; $i < 6; $i++) {
    if ($i > 1 && $i < 4) { $n += 1; }
    if ($i == 0 || $i == 5) { $n += 10; }
    if (!($i % 2)) { $n += 100; }
    $t = (bool) $i;
    $u = !$a || $b;
    $w = $a && !$b;
}
var_dump($n, $t, $u, $w);
$arr = [[1], [2]];
for ($i = 0; $i < 2; $i++) { $e = $arr[$i]; $e2 = $e; }
var_dump($e2);
function attached() { $v = [1]; $GLOBALS['zz'] = 1; extract(['k' => [5]]); for ($i = 0; $i < 2; $i++) { $k = $v; $v = [$i]; } return compact('k', 'v'); }
var_dump(attached());
$s = "0"; $r = !$s; var_dump($r);
"#;
    assert_eq!(
        run(src),
        r#"int(3)
int(3)
int(3)
d0 d1 | | d2 d10 
int(322)
bool(true)
bool(false)
bool(true)
array(1) {
  [0]=>
  int(2)
}
array(2) {
  ["k"]=>
  array(1) {
    [0]=>
    int(0)
  }
  ["v"]=>
  array(1) {
    [0]=>
    int(1)
  }
}
bool(true)
d11 "#
    );
}

/// Hot ops whose fast path declines inside a loop of hot ops (an
/// undefined variable, a numeric string, a concatenation, an int overflow)
/// run as their generic op, with the warning on their own line.
#[test]
fn hot_loop_declined_ops_warn_on_their_line() {
    let src = r#"<?php
for ($i = 0; $i < 2; $i++) {
    $a = $i + 1;
    $b = $undef + $a;
    $c = $a * 2;
}
var_dump($b, $c);
function f() {
    $s = "x";
    for ($i = 0; $i < 3; $i++) {
        $j = $i + 1;
        $s = $s . $j;
        $k = $j > 1 ? $s : null;
    }
    $n = "5 apples";
    $m = 0;
    for ($i = 0; $i < 2; $i++) { $m = $m + 1; $m = $m + $n; }
    return [$s, $k, $m];
}
var_dump(f());
$x = 1;
while ($x < 1000) { $x = $x * 7; $y = $x % 3; if ($y == 1) { $x++; } }
var_dump($x);
$z = PHP_INT_MAX - 1;
for ($i = 0; $i < 3; $i++) { $z = $z + 1; }
var_dump($z);
"#;
    assert_eq!(
        run(src),
        r#"
Warning: Undefined variable $undef in t.php on line 4

Warning: Undefined variable $undef in t.php on line 4
int(2)
int(4)

Warning: A non-numeric value encountered in t.php on line 17

Warning: A non-numeric value encountered in t.php on line 17
array(3) {
  [0]=>
  string(4) "x123"
  [1]=>
  string(4) "x123"
  [2]=>
  int(12)
}
int(2744)
float(9.223372036854776E+18)
"#
    );
}

/// User calls and returns on the dispatch loop's fast paths
/// (`Vm::push_simple_ucall`, `Vm::return_simple`) and the generic ones they
/// decline to: defaults, coercion, destructor order of values released by
/// the return, recursion, exceptions, closures with bound variables,
/// methods, by-reference returns, static variables, generators, variadics,
/// named arguments and backtraces.
#[test]
fn user_calls_and_returns() {
    let src = r#"<?php
class D { function __construct(public $n) {} function __destruct() { echo "~", $this->n, " "; } }
function id($x) { return $x; }
function two($a, $b = 5) { return func_num_args() . ":" . ($a + $b); }
function typed(int $i, ?string $s = null): int { return $i + strlen((string)$s); }
function fl(float $f): float { return $f; }
function mk($n) { $d = new D($n); return 7; }
function keep($n) { return new D($n); }
function rec($n) { return $n <= 0 ? 0 : $n + rec($n - 1); }
function thrower($x) { if ($x) throw new Exception("boom $x"); return $x; }
for ($i = 0; $i < 3; $i++) { echo id($i), two($i), two($i, 1), " "; }
echo "\n", typed(3, "ab"), typed(4), " ", var_export(fl(2), true), " ", typed("5"), "\n";
echo mk(1), " "; echo "after\n";
$k = keep(2); echo "kept "; unset($k); echo "\n";
id(new D(3)); echo "dropped\n";
echo rec(50), "\n";
try { thrower(0); thrower(1); } catch (Exception $e) { echo $e->getMessage(), " line ", $e->getLine(), "\n"; }
$y = 10;
$c = function($x) use ($y) { return $x + $y; };
for ($i = 0; $i < 2; $i++) echo $c($i), " ";
$o = new class { public $v = 3; function m($x) { return $x * $this->v; } static function s($x) { return $x - 1; } };
for ($i = 0; $i < 2; $i++) echo $o->m($i), $o::s($i), " ";
echo "\n";
function &r(array &$a) { return $a[0]; }
$arr = [1]; $ref = &r($arr); $ref = 9; echo $arr[0], "\n";
function counter() { static $n = 0; return ++$n; }
counter(); counter(); echo counter(), "\n";
function gen() { yield 1; yield 2; }
foreach (gen() as $g) echo $g; echo "\n";
function arr($a) { $a[] = 1; return count($a); }
$big = [1,2,3]; echo arr($big), count($big), "\n";
function v(...$xs) { return implode(",", $xs); }
echo v(1, 2, 3), " ", two(b: 1, a: 2), "\n";
function strict_ret(): string { return 5; }
var_dump(strict_ret());
function noret() {}
var_dump(noret());
function bt() { return debug_backtrace()[0]['function'] . count(debug_backtrace()); }
function outer() { return bt(); }
echo outer(), "\n";
echo "end\n";
"#;
    let expected = r#"01:52:1 11:62:2 21:72:3 
54 2.0 5
~1 7 after
kept ~2 
~3 dropped
1275
boom 1 line 10
10 11 0-1 30 
9
3
12
43
1,2,3 2:3
string(1) "5"
NULL
bt2
end
"#;
    assert_eq!(run(src), expected);
}

/// Method calls on the method cache's fast path from a variable and from
/// temporaries (`$this`, `new`, call results) and the generic paths it
/// declines to: the destructor of a temporary object released by the call,
/// a static method called through an object, `__call`, overrides, a
/// reference, an undefined variable and null.
#[test]
fn method_calls_on_variables_and_temporaries() {
    let src = r#"<?php
class D {
    function __construct(public $n) {}
    function __destruct() { echo "~", $this->n, " "; }
    function m($x) { return $this->n + $x; }
    function viaThis($x) { return $this->m($x) * 2; }
    static function s($x) { return "s$x"; }
    function __call($name, $args) { return "$name(" . implode(",", $args) . ")"; }
}
class E extends D { function m($x) { return -$x; } }
function make($n) { return new D($n); }
$d = new D(10);
for ($i = 0; $i < 3; $i++) { echo $d->m($i), " ", $d->viaThis($i), " "; }
echo "\n", (new D(1))->m(5), " after\n";
echo make(2)->s(3), " after\n";
echo $d->s(4), " ", $d->nope(1, 2), "\n";
$objs = [new D(20), new E(30), new D(40)];
foreach ($objs as $o) echo $o->m(1), " ";
echo "\n";
$r = &$d; echo $r->m(1), "\n";
try { $u->m(1); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
$n = null;
try { $n->m(1); } catch (Error $e) { echo $e->getMessage(), "\n"; }
echo "end\n";
"#;
    let expected = r#"10 20 11 22 12 24 
~1 6 after
~2 s3 after
s4 nope(1,2)
21 -1 41 
11

Warning: Undefined variable $u in t.php on line 21
Error: Call to a member function m() on null
Call to a member function m() on null
end
~10 ~20 ~30 ~40 "#;
    assert_eq!(run(src), expected);
}

/// Dynamic calls of closures held by variables on the INIT fast path and
/// the generic ones it declines to: recursion through a by-reference
/// `use`, a bound `$this`, static closures, a reference to a closure,
/// invokable objects, strings, arrays, first-class callables, an undefined
/// variable and static variables.
#[test]
fn closure_and_dynamic_calls() {
    let src = r#"<?php
class Inv { public $k = 2; function __invoke($x) { return $x * $this->k; } function get() { return function($x) { return $x + $this->k; }; } }
$f = function($x) use (&$f) { return $x <= 0 ? 0 : $x + $f($x - 1); };
echo $f(4), " ";
$inv = new Inv; echo $inv(5), " ";
$s = 'strtoupper'; echo $s('ab'), " ";
$b = $inv->get(); echo $b(1), " ";
$st = static fn($x) => $x . "!"; echo $st('a'), " ";
$ref = &$st; echo $ref('b'), " ";
$m = [$inv, 'get']; echo $m()(3), " ";
$fc = strlen(...); echo $fc('abcd'), "\n";
try { $undef(1); } catch (Error $e) { echo $e->getMessage(), "\n"; }
$counter = function() { static $n = 0; return ++$n; };
$counter(); echo $counter(), "\n";
"#;
    let expected = r#"10 10 AB 3 a! b! 5 4

Warning: Undefined variable $undef in t.php on line 12
Value of type null is not callable
2
"#;
    assert_eq!(run(src), expected);
}

/// The builtin ops and calls the hot loop and the in-place frameless call
/// take over (`ZEND_STRLEN`, `ZEND_COUNT`, `ZEND_TYPE_CHECK`,
/// `ZEND_ISSET_ISEMPTY_CV`, `ZEND_INIT_FCALL`, `ZEND_SEND_*`,
/// `ZEND_FRAMELESS_ICALL_n`), next to their declined shapes: references,
/// undefined variables, coercions, errors whose traces show the builtin's
/// frame, and builtins that call back into user code.
#[test]
fn hot_builtin_ops_and_frameless_calls_match_the_generic_ops() {
    let src = r#"<?php
function lens($s, $a) {
    $r = [];
    $r[] = strlen($s);
    $r[] = strlen("abc");
    $r[] = strlen($s . "xy");
    $r[] = count($a);
    $r[] = count([1, 2]);
    $r[] = count(func_get_args());
    $ref = &$s;
    $r[] = strlen($s);
    $r[] = strlen(12345);
    $r[] = is_int($a[0]) ? "i" : "n";
    $r[] = is_string($s) ? "s" : "n";
    $r[] = is_array($a . "") ? "a" : "n";
    $r[] = is_null($undef) ? "null" : "set";
    echo implode(",", $r), "\n";
}
lens("hello", [1, 2, 3]);
function issets() {
    $n = null; $z = "0"; $e = ""; $a = []; $f = 0.0; $o = new stdClass; $s = "x";
    $r = &$s;
    var_dump(isset($u), empty($u), isset($n), empty($n), isset($z), empty($z), empty($e),
        empty($a), empty($f), isset($o), empty($o), isset($r), empty($r));
}
issets();
function frameless($s) {
    var_dump(substr($s, 1, 3), substr($s, -2), strpos($s, "l"), strpos($s, "l", -2),
        abs(-5), abs(-2.5), abs(PHP_INT_MIN), ord("A"), chr(66), in_array(2, [1, 2, 3], true),
        in_array("2", [1, 2, 3]), intdiv(7, 2), str_contains($s, "ll"), strtolower("ABC"),
        trim("  x "), round(2.5), floor(3), sqrt(16));
    try { intdiv(1, 0); } catch (DivisionByZeroError $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
    try { strpos($s, "l", 10); } catch (ValueError $e) { echo $e->getMessage(), " @ ", $e->getTrace()[0]['function'], "\n"; }
    try { str_repeat("x", -1); } catch (ValueError $e) { echo $e->getMessage(), " @ ", $e->getTrace()[0]['function'], "\n"; }
}
frameless("hello");
echo strlen(null), "\n";
try { strlen([]); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { count(5); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
class C implements Countable { function count(): int { return 42; } }
echo count(new C), "\n";
echo is_int($nope) ? "y" : "n", "\n";
echo call_user_func(function ($x) { return strlen($x) + count(debug_backtrace()); }, "abc"), "\n";
$t = 0;
for ($i = 0; $i < 1000; $i++) {
    $t += strlen("ab") + count([$i]) + (is_int($i) ? 1 : 0) + abs(-$i) % 3 + ord(chr(65 + $i % 26));
}
echo $t, "\n";
function g($x) { return $x * 2; }
$u = 0;
for ($i = 0; $i < 100; $i++) { $u += g($i) + strlen(str_repeat("x", $i % 4)); }
echo $u, "\n";
"#;
    assert_eq!(
        run(src),
        r#"
Warning: Array to string conversion in t.php on line 15

Warning: Undefined variable $undef in t.php on line 16
5,3,7,3,2,2,5,5,i,s,n,null
bool(false)
bool(true)
bool(false)
bool(true)
bool(true)
bool(true)
bool(true)
bool(true)
bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
string(3) "ell"
string(2) "lo"
int(2)
int(3)
int(5)
float(2.5)
float(9.223372036854776E+18)
int(65)
string(1) "B"
bool(true)
bool(true)
int(3)
bool(true)
string(3) "abc"
string(1) "x"
float(3)
float(3)
float(4)
DivisionByZeroError: Division by zero
strpos(): Argument #3 ($offset) must be contained in argument #1 ($haystack) @ strpos
str_repeat(): Argument #2 ($times) must be greater than or equal to 0 @ str_repeat

Deprecated: strlen(): Passing null to parameter #1 ($string) of type string is deprecated in t.php on line 37
0
strlen(): Argument #1 ($string) must be of type string, array given
count(): Argument #1 ($value) must be of type Countable|array, int given
42

Warning: Undefined variable $nope in t.php on line 42
n
4
82415
10050
"#
    );
}

/// Constructors called by `new` on the user-call fast path
/// (`Vm::push_simple_ucall`): defaults, a constructor that throws (its
/// object is never destructed, `zend_object_store_ctor_failed`), coercion
/// left to the general path, private constructors and nested `new`.
#[test]
fn constructor_calls_from_new() {
    let src = r#"<?php
class D { public $n; function __construct($n, $m = 2) { $this->n = $n * $m; if ($n < 0) throw new Exception("neg $n"); } function __destruct() { echo "~", $this->n, " "; } }
class T { function __construct(public int $i) {} }
class P { private function __construct() {} static function make() { return new P; } }
for ($i = 1; $i <= 3; $i++) { $d = new D($i); echo $d->n, " "; }
echo "\n";
try { $e = new D(-1); } catch (Exception $x) { echo $x->getMessage(), " line ", $x->getLine(), "\n"; }
$t = new T("5"); var_dump($t->i);
var_dump(get_class(P::make()));
try { new P; } catch (Error $x) { echo $x->getMessage(), "\n"; }
$w = new D(new D(4) ? 5 : 0, 1); echo "\n";
unset($d, $w); echo "end\n";
"#;
    assert_eq!(
        run(src),
        r#"2 ~2 4 ~4 6 
neg -1 line 2
int(5)
string(1) "P"
Call to private P::__construct() from global scope
~8 
~6 ~5 end
"#
    );
}

//! Script-level tests of the dispatch loop (`Vm::run`): the local `ip`,
//! jumps, safe points and the loop layout. Expected output from
//! `/usr/bin/php -n` (PHP 8.3), paths normalised to `t.php`.

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

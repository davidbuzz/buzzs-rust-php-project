// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A generator's frame as it moves on and off the frame stack
//! (`perf/REDESIGN-FRAMES.md`, stage 2a: it stays in its own heap block,
//! and resuming or suspending it moves a pointer): `yield` inside
//! `try`/`finally`, exceptions thrown in and out, generators destroyed
//! mid-iteration, `yield from` over generators, arrays and iterators,
//! generators resumed at different stack depths and by other generators,
//! backtraces, `get_defined_vars()`/`compact()` across yields, references
//! to a generator's variables, and generators in fibers. Expected output:
//! PHP 8.5's, as the engine gave it before the change.

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

#[test]
fn finally_exceptions_destruction_and_yield_from() {
    let out = run(r#"<?php
// try/finally across yields, exceptions thrown in and out
function fin() {
    try { echo "a\n"; $x = yield 1; echo "got $x\n"; yield 2; }
    finally { echo "finally\n"; }
    yield 3;
}
$g = fin(); echo $g->current(), "\n"; echo $g->send('s'), "\n"; $g->next(); echo $g->current(), "\n"; $g->next(); var_dump($g->valid());
function thr() {
    try { yield 1; } catch (Exception $e) { echo "caught ", $e->getMessage(), "\n"; yield 99; } finally { echo "fin2\n"; }
}
$g = thr(); $g->current(); echo $g->throw(new Exception("boom")), "\n"; $g->next(); var_dump($g->valid());
function nested() { try { try { yield 1; } finally { echo "inner fin\n"; } } finally { echo "outer fin\n"; } }
$g = nested(); $g->current(); unset($g); echo "after unset\n";
function retval() { $a = yield 1; return $a * 2; }
$g = retval(); $g->current(); $g->send(21); var_dump($g->getReturn());
// destroyed mid-iteration, in a loop and by break
function counted($n) { for ($i = 0; $i < $n; $i++) { yield $i; } }
foreach (counted(10) as $v) { if ($v == 3) break; } echo "broke at $v\n";
$gs = []; for ($i = 0; $i < 5; $i++) { $gs[] = counted(3); } foreach ($gs as $k => $g) { $g->current(); if ($k % 2) { $g->next(); } } $gs = null; echo "dropped\n";
// yield from: generators, arrays, iterators, nested; keys
function inner2() { $r = yield 1; echo "inner got $r\n"; yield 2; return 'ir'; }
function outer2() { $x = yield from inner2(); echo "inner returned $x\n"; yield from [10, 20]; yield from new ArrayIterator(['k' => 30]); return 'or'; }
$g = outer2(); foreach ($g as $k => $v) { echo "$k => $v\n"; } echo $g->getReturn(), "\n";
$g = outer2(); $g->current(); $g->send('S'); echo $g->current(), "\n";
function deep($n) { if ($n == 0) { yield 'leaf'; return 0; } $r = yield from deep($n - 1); yield "lvl$n"; return $r + 1; }
$g = deep(5); foreach ($g as $v) echo $v, " "; echo $g->getReturn(), "\n";
"#);
    assert_eq!(
        out,
        r"a
1
got s
2
finally
3
bool(false)
caught boom
99
fin2
bool(false)
inner fin
outer fin
after unset
int(42)
broke at 3
dropped
0 => 1
inner got 
1 => 2
inner returned ir
0 => 10
1 => 20
k => 30
or
inner got S
2
leaf lvl1 lvl2 lvl3 lvl4 lvl5 5
"
    );
}

#[test]
fn depths_backtraces_variables_and_references() {
    let out = run(r#"<?php
// generators resumed at different depths, and calls made from inside them
function rec($n, $g) { if ($n == 0) { return $g->current() . ':' . $g->key(); } $r = rec($n - 1, $g); return $r; }
function caller() { $acc = 0; for ($i = 0; $i < 3; $i++) { $acc += helper($i); $v = yield $acc; echo "resumed with $v\n"; } }
function helper($i) { return $i * 10 + strlen(str_repeat('x', $i)); }
$g = caller();
echo rec(0, $g), "\n"; $g->send('a'); echo rec(7, $g), "\n"; $g->send('b'); echo rec(3, $g), "\n"; $g->send('c'); var_dump($g->valid());
// generator resumed by another generator
function g1() { yield 1; yield 2; }
function g2($g) { foreach ($g as $v) { yield $v * 100; } }
foreach (g2(g1()) as $v) echo $v, "\n";
// generator throws out of a deep call
function thrower() { yield 1; throw new RuntimeException("from gen"); }
function drive($g) { foreach ($g as $v) { echo "v=$v\n"; } }
try { drive(thrower()); } catch (RuntimeException $e) { echo "caught ", $e->getMessage(), " line ", $e->getLine(), "\n"; foreach ($e->getTrace() as $t) { echo "  ", $t['function'], "\n"; } }
// backtrace inside a generator
function bt() { yield 1; echo implode(',', array_map(fn($f) => $f['function'], debug_backtrace())), "\n"; yield 2; }
function wrap() { $g = bt(); foreach ($g as $v) {} }
wrap();
// get_defined_vars / compact across yields
function vars($a) { $b = 2; yield get_defined_vars(); $c = 3; yield compact('a', 'b', 'c'); extract(['d' => 4]); yield get_defined_vars(); }
foreach (vars(1) as $v) { echo json_encode($v), "\n"; }
// references to generator CVs
function &refgen() { $x = 1; while ($x < 4) { yield $x; } echo "x ended $x\n"; }
foreach (refgen() as &$r) { echo "r=$r\n"; $r++; } unset($r);
function holder() { $v = 'orig'; $GLOBALS['ref'] = &$v; yield 1; echo "v is $v\n"; yield 2; }
$g = holder(); $g->current(); $ref = 'changed'; $g->next(); unset($g); echo "ref still $ref\n";
function closure_ref() { $n = 0; $inc = function () use (&$n) { $n++; }; yield $inc; yield $n; }
$g = closure_ref(); $f = $g->current(); $f(); $f(); $g->next(); echo "n=", $g->current(), "\n";
// static vars and $this in generator methods
class C { public $p = 5; function m() { static $s = 0; $s++; yield $this->p + $s; $this->p++; yield $this->p + $s; } }
$c = new C; foreach ($c->m() as $v) echo $v, " "; foreach ($c->m() as $v) echo $v, " "; echo "\n";
// generator inside a closure bound later
$fn = function () { yield $this->p; }; $b = Closure::bind($fn, $c, C::class); foreach ($b() as $v) echo "bound $v\n";
// unstarted generator released, generator in an array cycle
$g = counted2(); function counted2() { yield 1; } unset($g);
$arr = []; $arr[] = (function () use (&$arr) { yield 1; yield 2; })(); $arr[0]->current(); unset($arr); gc_collect_cycles(); echo "cycle collected\n";
"#);
    assert_eq!(
        out,
        r#"0:0
resumed with a
11:1
resumed with b
33:2
resumed with c
bool(false)
100
200
v=1
caught from gen line 13
  thrower
  drive
bt,wrap
{"a":1,"b":2}
{"a":1,"b":2,"c":3}
{"a":1,"b":2,"c":3,"d":4}
r=1
r=2
r=3
x ended 4
v is changed
ref still changed
n=2
6 7 8 9 
bound 7
cycle collected
"#
    );
}

#[test]
fn generators_in_fibers() {
    let out = run(r#"<?php
// generators and fibers
function gen() { $x = yield 1; echo "gen got $x\n"; Fiber::suspend('from gen'); echo "gen after suspend\n"; yield 2; return 'gr'; }
$g = gen();
$f = new Fiber(function () use ($g) { echo "fiber start ", $g->current(), "\n"; $g->send('fs'); echo "fiber resumed, gen at ", $g->current(), "\n"; $g->next(); return 'fr'; });
echo "suspended: ", $f->start(), "\n";
echo "main sees gen running? "; try { $g->current(); echo "no\n"; } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
$f->resume(); echo "fiber returned ", $f->getReturn(), ", gen returned ", $g->getReturn(), "\n";
// a generator created in a fiber and resumed outside it
$f = new Fiber(function () { $g = (function () { yield 'a'; yield 'b'; })(); $g->current(); Fiber::suspend($g); });
$gg = $f->start(); $gg->next(); echo "outside: ", $gg->current(), "\n"; $f->resume(); $gg->next(); var_dump($gg->valid());
// fiber started from inside a generator
function gf() { $f = new Fiber(function () { $v = Fiber::suspend(1); return $v * 2; }); $x = $f->start(); yield $x; $f->resume(21); yield $f->getReturn(); }
foreach (gf() as $v) echo "gf $v\n";
// destroying a fiber suspended inside a generator
function gd() { try { Fiber::suspend('in gd'); yield 1; } finally { echo "gd finally\n"; } }
$f = new Fiber(function () { $g = gd(); $g->current(); });
echo $f->start(), "\n"; unset($f); gc_collect_cycles(); echo "fiber gone\n";
// generator yielding from inside nested calls in a fiber, many switches
$g = (function () { for ($i = 0; $i < 5; $i++) { $r = yield $i; if ($r) Fiber::suspend($r); } })();
$f = new Fiber(function () use ($g) { foreach ($g as $k => $v) { $g->send($v % 2 ? "s$v" : null); } });
$s = $f->start(); while (!$f->isTerminated()) { echo "suspended with $s\n"; $s = $f->resume(); }
echo "done\n";
"#);
    assert_eq!(
        out,
        r"suspended: fiber start 1
gen got fs
from gen
main sees gen running? no
gen after suspend
fiber resumed, gen at 2
fiber returned fr, gen returned gr
outside: b
bool(false)
gf 1
gf 42
in gd
gd finally
fiber gone
done
"
    );
}

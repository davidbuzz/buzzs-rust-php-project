// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! User calls and returns with the tape's core loop switching frames
//! itself (`BUZZS_INCORE=1`, `php-vm`'s `exec::tape::incore`) and without
//! it, each with the JIT on and off: every run must print what php8.5
//! prints for the same script (the expected outputs below were taken from
//! `/usr/bin/php8.5 -n`, with `-d extension=posix` for the signal case).
use std::process::Command;

/// Runs `script` (written to a file of its own) under every combination of
/// the frame-switch and JIT switches and compares the output with
/// `expected`.
fn check(name: &str, script: &str, expected: &str) {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("incore_calls");
    std::fs::create_dir_all(&dir).expect("temporary directory");
    let path = dir.join(format!("{name}.php"));
    std::fs::write(&path, script).expect("write script");
    for incore in ["0", "1"] {
        for (jit, hot) in [("0", "1000"), ("1", "1000"), ("1", "1")] {
            let out = Command::new(env!("CARGO_BIN_EXE_php"))
                .args(["-n", "-d", "memory_limit=1G"])
                .arg(&path)
                .env("BUZZS_INCORE", incore)
                .env("BUZZS_JIT", jit)
                .env("BUZZS_JIT_HOT", hot)
                .output()
                .expect("spawn php");
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert_eq!(
                stdout,
                expected,
                "{name}: BUZZS_INCORE={incore} BUZZS_JIT={jit} BUZZS_JIT_HOT={hot}; stderr: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(out.status.success(), "{name}: exit status {:?}", out.status);
        }
    }
}

/// Deep and mutual recursion, a three-argument call in a loop, `return;` and the implicit return.
#[test]
fn recursion_matches_php() {
    check(
        "recursion",
        r#"<?php
function d($n) { if ($n == 0) { return 0; } return 1 + d($n - 1); }
function fib($n) { return $n < 2 ? $n : fib($n - 1) + fib($n - 2); }
function ev($n) { return $n == 0 ? true : od($n - 1); }
function od($n) { return $n == 0 ? false : ev($n - 1); }
function sum3($a, $b, $c) { $t = $a + $b; return $t + $c; }
echo d(3000), "\n", fib(20), "\n";
var_dump(ev(1001), od(1001), ev(500));
$s = 0;
for ($i = 0; $i < 3000; $i++) { $s += sum3($i, 1, 2); }
echo $s, "\n";
function nothing() { }
function early($x) { if ($x) { return; } echo "late\n"; }
var_dump(nothing(), early(true), early(false));
"#,
        r#"3000
6765
bool(false)
bool(true)
bool(true)
4507500
late
NULL
NULL
NULL
"#,
    );
}

/// A callee that throws through a caller, `finally` around a return, `debug_backtrace()` lines in a callee entered on the tape.
#[test]
fn throws_matches_php() {
    check(
        "throws",
        r#"<?php
function t($i) { if ($i % 7 == 6) { throw new RuntimeException("x$i"); } return $i; }
function mid($i) { return t($i) * 2; }
$s = 0; $n = 0;
for ($i = 0; $i < 200; $i++) {
    try { $s += mid($i); } catch (RuntimeException $e) { $n++; $last = $e; }
}
echo $s, " ", $n, " ", $last->getMessage(), " ", $last->getLine(), "\n";
echo str_replace(__FILE__, "F", $last->getTraceAsString()), "\n";
function fin($x) { try { return $x + 1; } finally { echo "finally $x\n"; } }
echo fin(1), fin(2), "\n";
function bt($n) { if ($n == 0) { foreach (debug_backtrace() as $f) { echo $f['function'], ':', $f['line'], ' '; } echo "\n"; return 0; } return bt($n - 1); }
for ($i = 0; $i < 3; $i++) { bt(3); }
"#,
        r#"34172 28 x195 2
#0 F(3): t(195)
#1 F(6): mid(195)
#2 {main}
finally 1
2finally 2
3
bt:12 bt:12 bt:12 bt:13 
bt:12 bt:12 bt:12 bt:13 
bt:12 bt:12 bt:12 bt:13 
"#,
    );
}

/// Returns of every kind of value (typed, coerced, by reference, `void`), methods, static calls, closures with and without `use`, destructor order at return.
#[test]
fn kinds_matches_php() {
    check(
        "kinds",
        r#"<?php
class P { public $v; function __construct($v) { $this->v = $v; } function get() { return $this->v; } static function make($v) { return new P($v); } function self() { return $this; } }
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "destruct {$this->n}\n"; } }
function ri() { return 1; } function rf() { return 1.5; } function rs() { return "s"; } function ra() { return [1, 2]; }
function ro() { return new P(3); } function rn() { return null; } function rb() { return false; }
function rcv($x) { $y = $x . "!"; return $y; }
function &rr(array &$a) { return $a[0]; }
function rstr(): string { return 3; }
function rint(): int { return "7"; }
function rvoid(): void { return; }
function withd($n) { $d = new D($n); echo "body $n\n"; return $n; }
for ($i = 0; $i < 300; $i++) {
    $r = [ri(), rf(), rs(), ra(), ro()->v, rn(), rb(), rcv("a"), rstr(), rint(), rvoid()];
}
var_dump($r);
$a = [1, 2]; $x = &rr($a); $x = 9; var_dump($a);
$p = new P(5);
$s = 0;
for ($i = 0; $i < 300; $i++) { $s += $p->get() + P::make($i)->get() + $p->self()->get(); }
echo $s, "\n";
$c = function ($x) { return $x * 2; };
$k = 10; $u = function ($x) use ($k) { return $x + $k; };
$s = 0;
for ($i = 0; $i < 300; $i++) { $s += $c($i) + $u($i); }
echo $s, "\n";
echo withd(1), "\n";
echo withd(2) + withd(3), "\n";
"#,
        r#"array(11) {
  [0]=>
  int(1)
  [1]=>
  float(1.5)
  [2]=>
  string(1) "s"
  [3]=>
  array(2) {
    [0]=>
    int(1)
    [1]=>
    int(2)
  }
  [4]=>
  int(3)
  [5]=>
  NULL
  [6]=>
  bool(false)
  [7]=>
  string(2) "a!"
  [8]=>
  string(1) "3"
  [9]=>
  int(7)
  [10]=>
  NULL
}
array(2) {
  [0]=>
  &int(9)
  [1]=>
  int(2)
}
47850
137550
body 1
destruct 1
1
body 2
destruct 2
body 3
destruct 3
5
"#,
    );
}

/// Calls from builtins back into user code (the frame `Vm::run` started at returns), a generator, a fiber, `call_user_func[_array]()`.
#[test]
fn callbacks_matches_php() {
    check(
        "callbacks",
        r#"<?php
function dbl($x) { return $x * 2; }
function cmp($a, $b) { return $a <=> $b; }
function gen($n) { for ($i = 0; $i < $n; $i++) { yield $i => dbl($i); } return $n; }
$r = array_map('dbl', range(1, 300));
echo array_sum($r), "\n";
$a = [5, 3, 9, 1, 7]; usort($a, 'cmp'); echo implode(',', $a), "\n";
echo array_sum(array_map(fn($x) => dbl($x) + 1, [1, 2, 3])), "\n";
$g = gen(5); $s = 0; foreach ($g as $k => $v) { $s += $k + $v; } echo $s, " ", $g->getReturn(), "\n";
echo implode(',', array_filter(range(1, 20), function ($x) { return $x % 3 == 0; })), "\n";
$f = new Fiber(function ($x) { $y = Fiber::suspend(dbl($x)); return dbl($y); });
$v = $f->start(4); $f->resume($v + 1); echo $v, " ", $f->getReturn(), "\n";
echo call_user_func('dbl', 21), " ", call_user_func_array('dbl', [4]), "\n";
"#,
        r#"90300
1,3,5,7,9
15
30 5
3,6,9,12,15,18
8 18
42 8
"#,
    );
}

/// A signal raised in a callee, seen at the same safe point.
#[test]
fn signal_matches_php() {
    check(
        "signal",
        r#"<?php
pcntl_async_signals(true);
$seen = [];
pcntl_signal(SIGUSR1, function () { global $seen, $i, $where; $seen[] = "$i:$where"; });
function inner($x) { global $where; $where = 'inner'; return $x + 1; }
function outer($x) { global $where; $where = 'outer'; if ($x == 50) { posix_kill(getmypid(), SIGUSR1); } $r = inner($x); $where = 'after'; return $r; }
$s = 0;
for ($i = 0; $i < 100; $i++) { $s += outer($i); $where = 'loop'; }
echo $s, " ", implode(',', $seen), "\n";
"#,
        r#"5050 50:outer
"#,
    );
}

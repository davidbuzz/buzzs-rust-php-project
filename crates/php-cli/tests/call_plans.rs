// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Calls made through a call plan (`exec::tape::calls::call_plain`) and
//! the calls a plan hands back: every case runs in a loop, so that the
//! site's first call works the plan out and the later ones take it or
//! fall back. Functions (defaults, typed parameters and return types and
//! their coercions and errors, undefined and by-reference variables,
//! `func_get_args()`, backtraces, exceptions, generators, unused results
//! with destructors, too few arguments), methods (a site that sees seven
//! classes, private methods from different closure scopes, `__call`,
//! temporaries as `$this`, a `null` receiver), static calls (`self::`,
//! `parent::`, `static::`, late static binding, an instance method
//! through `parent::`, a private static from outside), recursion, and
//! arguments read from properties and array elements (a missing key, a
//! value the parameter's type refuses, a receiver that becomes `null`).
//! Expected output: PHP 8.5.

use std::io::Write;
use std::process::{Command, Stdio};

/// Runs `source` (read from standard input) with no php.ini, every error
/// shown and none logged.
fn run(source: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_php"))
        .args([
            "-n",
            "-d",
            "memory_limit=1G",
            "-d",
            "log_errors=0",
            "-d",
            "display_errors=1",
            "-d",
            "error_reporting=-1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the CLI");
    child
        .stdin
        .take()
        .expect("CLI stdin")
        .write_all(source.as_bytes())
        .expect("write the script");
    let out = child.wait_with_output().expect("run the script");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn planned_calls_and_their_fallbacks_behave_as_php_does() {
    let out = run(r#"<?php
function plain($a, $b = 7) { return $a + $b; }
function typed(int $a, ?string $s = null): string { return $a . ':' . ($s ?? 'null'); }
function fl(float $f): float { return $f * 2; }
function ret_int($x): int { return $x; }
function args() { return func_get_args(); }
function trace($x) { $t = debug_backtrace()[0]; return $t['function'] . '(' . implode(',', $t['args']) . ')@' . $t['line']; }
function thrower($x) { if ($x === 2) { throw new RuntimeException("at $x"); } return $x; }
function byref(&$r) { $r++; return $r; }
function gen($x) { yield $x; }
function arr($a) { $a[] = 1; return $a; }
class D { public static $log = []; public $n; function __construct($n) { $this->n = $n; } function __destruct() { self::$log[] = "d{$this->n}"; } }
function mk($n) { return new D($n); }

echo "-- functions\n";
for ($i = 0; $i < 4; $i++) { echo plain($i), ' ', plain($i, $i), ' ', typed($i), ' ', typed($i, 's'), ' ', fl($i), "\n"; }
for ($i = 0; $i < 4; $i++) { $v = ($i === 2) ? '12' : $i; echo typed($v), ' ', var_export(ret_int($v), true), "\n"; }
for ($i = 0; $i < 3; $i++) { try { echo typed($i === 1 ? 'x' : $i), "\n"; } catch (TypeError $e) { echo 'TypeError: ', $e->getMessage(), "\n"; } }
for ($i = 0; $i < 3; $i++) { try { echo ret_int($i === 1 ? 'abc' : $i), "\n"; } catch (TypeError $e) { echo 'TypeError: ', $e->getMessage(), "\n"; } }
for ($i = 0; $i < 3; $i++) { if ($i === 1) { unset($u); } else { $u = $i; } echo var_export(@plain($u ?? 0), true), ' '; $w = @args($u); echo json_encode($w), "\n"; }
function undef_arg() { for ($i = 0; $i < 3; $i++) { if ($i !== 1) { $z = $i; } else { unset($z); } echo json_encode(args($z)), "\n"; } }
undef_arg();
for ($i = 0; $i < 3; $i++) { $q = $i; $r = &$q; echo plain($q), ' ', byref($q), ' ', $q, "\n"; unset($r); }
for ($i = 0; $i < 3; $i++) { echo trace($i), "\n"; }
for ($i = 0; $i < 4; $i++) { try { echo thrower($i), "\n"; } catch (RuntimeException $e) { echo 'caught ', $e->getMessage(), ' line ', $e->getLine(), ' trace ', $e->getTrace()[0]['function'], "\n"; } }
for ($i = 0; $i < 3; $i++) { foreach (gen($i) as $g) { echo $g; } } echo "\n";
for ($i = 0; $i < 3; $i++) { $a = [$i]; $b = arr($a); echo count($a), count($b); } echo "\n";
for ($i = 0; $i < 3; $i++) { mk($i); D::$log[] = "after$i"; } echo implode(',', D::$log), "\n"; D::$log = [];
for ($i = 0; $i < 3; $i++) { $o = mk($i); D::$log[] = "kept$i"; } unset($o); echo implode(',', D::$log), "\n"; D::$log = [];
for ($i = 0; $i < 3; $i++) { try { echo plain(), "\n"; } catch (ArgumentCountError $e) { echo 'ACE: ', $e->getMessage(), "\n"; } }

echo "-- methods\n";
class A { public $v = 'A'; function who() { return static::class . '/' . $this->v . '/' . self::class; } private function priv() { return 'priv' . static::class; } function callPriv() { return $this->priv(); }
  static function make() { return new static; } function me() { return $this; } function add(int $x, int $y = 1): int { return $x + $y; } function __call($n, $a) { return "magic:$n(" . implode(',', $a) . ')'; } }
class B extends A { public $v = 'B'; }
class C extends A { public $v = 'C'; function who() { return 'C!' . parent::who(); } }
class E extends A { public $v = 'E'; } class F extends A { public $v = 'F'; } class G extends B { public $v = 'G'; }
class X { function who() { return 'X'; } function add($x) { return "X$x"; } }
$objs = [new A, new B, new C, new E, new F, new G, new X, new A, new C, new G];
foreach ($objs as $o) { echo $o->who(), ' ', $o->add(2), ' ', $o->add(2, 3), "\n"; }
foreach ([new A, new B, new A] as $o) { echo $o->callPriv(), ' ', $o->nope(1, 2), ' ', get_class($o->me()), "\n"; }
$f = function () { return $this->priv(); };
foreach ([A::class, null, A::class] as $scope) { try { echo Closure::bind($f, new B, $scope)(), "\n"; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; } }
for ($i = 0; $i < 3; $i++) { echo (new D($i))->n, ' ', (new A)->who(), ' ', A::make()->who(), ' ', B::make()->who(), "\n"; }
echo implode(',', D::$log), "\n"; D::$log = [];
class T { public $d; function __construct() { $this->d = new D(9); } function get() { return $this->d->n; } }
for ($i = 0; $i < 3; $i++) { echo (new T)->get(), count(D::$log), ' '; } echo "\n"; D::$log = [];
for ($i = 0; $i < 3; $i++) { try { $n = ($i === 1) ? null : new A; echo $n->who(), "\n"; } catch (Error $e) { echo $e->getMessage(), "\n"; } }
for ($i = 0; $i < 3; $i++) { try { echo (new A)->add($i === 1 ? 'no' : $i), "\n"; } catch (TypeError $e) { echo 'TypeError: ', $e->getMessage(), "\n"; } }

echo "-- static\n";
class S { static function name() { return static::class; } static function viaSelf() { return self::name(); } static function viaStatic() { return static::name(); } function inst() { return 'S::inst/' . static::class; }
  static function sum(int $a, int $b = 2): int { return $a + $b; } private static function hidden() { return 'hidden'; } static function open() { return self::hidden(); } }
class S2 extends S { static function viaParent() { return parent::name(); } function inst() { return 'S2>' . parent::inst(); } static function name() { return 'S2:' . parent::name(); } }
class S3 extends S2 { }
for ($i = 0; $i < 3; $i++) { echo S::name(), ' ', S2::name(), ' ', S3::name(), ' ', S::viaSelf(), ' ', S2::viaSelf(), ' ', S3::viaStatic(), ' ', S2::viaParent(), ' ', S3::viaParent(), "\n"; }
foreach ([new S, new S2, new S3, new S] as $o) { echo $o->inst(), ' ', $o::name(), ' ', S::sum(1), S::sum(1, 1), ' ', S::open(), "\n"; }
for ($i = 0; $i < 3; $i++) { try { echo S::sum($i === 1 ? [] : $i), "\n"; } catch (TypeError $e) { echo 'TypeError: ', $e->getMessage(), "\n"; } }
for ($i = 0; $i < 3; $i++) { try { echo S::hidden(), "\n"; } catch (Error $e) { echo $e->getMessage(), "\n"; } }
function rec($n) { return $n <= 0 ? 0 : $n + rec($n - 1); } function ping($n) { return $n <= 0 ? 'done' : pong($n - 1); } function pong($n) { return $n <= 0 ? 'done' : ping($n - 1); }
echo rec(50), ' ', ping(7), ' ', ping(8), "\n";

echo "-- places as arguments\n";
class H { public $p = 3; public $arr = ['k' => 'v', 'n' => 5]; public ?H $next = null; private $hidden = 'h';
  function run($list) { $out = [];
    for ($i = 0; $i < 4; $i++) { $out[] = plain($this->p, $i) . typed($this->arr['n'], $this->arr['k']) . $this->show($this, $this->hidden, $list[$i % 2]); }
    return implode('|', $out); }
  function show($o, $h, $e) { return get_class($o) . $h . $e; }
  function miss($list) { $out = [];
    for ($i = 0; $i < 3; $i++) { $k = $i === 1 ? 'zz' : 'k'; $out[] = @var_export(args($this->arr[$k], $list[$i] ?? 'd'), true); }
    return implode('|', $out); }
  function bad() { $out = [];
    for ($i = 0; $i < 3; $i++) { if ($i === 1) { $this->arr['n'] = 'x'; } else { $this->arr['n'] = $i; }
      try { $out[] = typed($this->arr['n']); } catch (TypeError $e) { $out[] = 'TE'; } }
    return implode('|', $out); }
  function chain() { $this->next = new H; $this->next->p = 9; $out = [];
    for ($i = 0; $i < 3; $i++) { $out[] = plain($this->next->p, $this->next->arr['n']); if ($i === 1) { $this->next = null; } }
    return implode('|', $out); } }
$h = new H;
echo $h->run(['a', 'b']), "\n", str_replace("\n", '', $h->miss(['x'])), "\n", $h->bad(), "\n";
try { echo $h->chain(), "\n"; } catch (Throwable $e) { echo get_class($e), "\n"; }
function warn_place($a) { $r = []; for ($i = 0; $i < 3; $i++) { $r[] = plain($a[$i === 2 ? 'none' : 'x']); } return $r; }
echo json_encode(warn_place(['x' => 1])), "\n";
"#);
    assert_eq!(
        out,
        r#"-- functions
7 0 0:null 0:s 0
8 2 1:null 1:s 2
9 4 2:null 2:s 4
10 6 3:null 3:s 6
0:null 0
1:null 1
12:null 12
3:null 3
0:null
TypeError: typed(): Argument #1 ($a) must be of type int, string given, called in Standard input code on line 18
2:null
0
TypeError: ret_int(): Return value must be of type int, string returned
2
7 [0]
7 [null]
9 [2]
[0]

Warning: Undefined variable $z in Standard input code on line 21
[null]
[2]
7 1 1
8 2 2
9 3 3
trace(0)@24
trace(1)@24
trace(2)@24
0
1
caught at 2 line 8 trace thrower
3
012
121212
d0,after0,d1,after1,d2,after2
kept0,d0,kept1,d1,kept2,d2
ACE: Too few arguments to function plain(), 0 passed in Standard input code on line 30 and at least 1 expected
ACE: Too few arguments to function plain(), 0 passed in Standard input code on line 30 and at least 1 expected
ACE: Too few arguments to function plain(), 0 passed in Standard input code on line 30 and at least 1 expected
-- methods
A/A/A 3 5
B/B/A 3 5
C!C/C/A 3 5
E/E/A 3 5
F/F/A 3 5
G/G/A 3 5
X X2 X2
A/A/A 3 5
C!C/C/A 3 5
G/G/A 3 5
privA magic:nope(1,2) A
privB magic:nope(1,2) B
privA magic:nope(1,2) A
privB
magic:priv()
privB
0 A/A/A A/A/A B/B/A
1 A/A/A A/A/A B/B/A
2 A/A/A A/A/A B/B/A
d0,d1,d2
91 92 93 
A/A/A
Call to a member function who() on null
A/A/A
1
TypeError: A::add(): Argument #1 ($x) must be of type int, string given, called in Standard input code on line 49
3
-- static
S S2:S2 S2:S3 S S2 S2:S3 S2 S3
S S2:S2 S2:S3 S S2 S2:S3 S2 S3
S S2:S2 S2:S3 S S2 S2:S3 S2 S3
S::inst/S S 32 hidden
S2>S::inst/S2 S2:S2 32 hidden
S2>S::inst/S3 S2:S3 32 hidden
S::inst/S S 32 hidden
2
TypeError: S::sum(): Argument #1 ($a) must be of type int, array given, called in Standard input code on line 58
4
Call to private method S::hidden() from global scope
Call to private method S::hidden() from global scope
Call to private method S::hidden() from global scope
1275 done done
-- places as arguments
35:vHha|45:vHhb|55:vHha|65:vHhb
array (  0 => 'v',  1 => 'x',)|array (  0 => NULL,  1 => 'd',)|array (  0 => 'v',  1 => 'd',)
0:null|TE|2:null

Warning: Attempt to read property "p" on null in Standard input code on line 77

Warning: Attempt to read property "arr" on null in Standard input code on line 77

Warning: Trying to access array offset on null in Standard input code on line 77
14|14|0

Warning: Undefined array key "none" in Standard input code on line 82
[8,8,7]
"#
    );
}

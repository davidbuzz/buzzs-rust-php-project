// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Ops the tape runs as rows of their own instead of leaving its loop for
//! the second-level dispatch or `Vm::run` (`exec/tape.rs`): `$this` as a
//! value (`ZEND_FETCH_THIS`), closure creation
//! (`ZEND_DECLARE_LAMBDA_FUNCTION`), and the other long-tail ops, each
//! shape run several times so that the rows' caches are warm, against
//! the output of `/usr/bin/php8.5 -n` (php-src master where they differ,
//! as noted).
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

/// Runs `source` as `t.php` with run-tests.php's `fatal_error_backtraces=0`.
fn run(source: &str) -> String {
    let sink = Sink::default();
    let options = VmOptions {
        ini: vec![("fatal_error_backtraces".to_string(), "0".to_string())],
    };
    let mut vm = common::vm_with_stdlib(&options, Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

/// `$this` returned, passed, put in a callback array and copied to a
/// variable; closures and arrow functions binding by value, by reference,
/// an undefined variable (the warning), `$this` and nothing; static
/// closures, which php-src master caches per declaration (8.5 makes a new
/// one each time: there `same()` prints "different"); a captured object
/// released with the closure.
#[test]
fn this_values_and_closures() {
    assert_eq!(
        run(r#"<?php
class K {
    public $n = 0;
    function self_() { return $this; }
    function pass() { return spl_object_id($this) === spl_object_id($this->self_()); }
    function cb() { return [$this, 'm']; }
    function m($x) { return "m$x"; }
    function copy() { $x = $this; $x->n++; return $x->n; }
    function closures($k) {
        $a = function ($x) use ($k) { return "$k$x" . $this->n; };
        $b = fn($x) => "$k$x" . $this->n;
        $c = static function ($x) { return "s$x"; };
        $d = function () use (&$k) { $k .= '!'; return $k; };
        $e = function () use ($undef) { return var_export($undef, true); };
        return [$a(1), $b(2), $c(3), $d(), $k, $e()];
    }
    static function st() { try { return isset($this) ? 'set' : 'unset'; } catch (Error $e) { return $e->getMessage(); } }
}
$k = new K;
for ($i = 0; $i < 3; $i++) {
    echo get_class($k->self_()), ' ', var_export($k->pass(), true), ' ', call_user_func($k->cb(), $i), ' ', $k->copy(), "\n";
    echo implode(',', $k->closures("k$i")), "\n";
    echo K::st(), "\n";
}
function plain($v) { $f = function () use ($v) { return $v * 2; }; $g = fn() => $v + 1; $h = static fn() => 7; return $f() + $g() + $h(); }
for ($i = 0; $i < 3; $i++) echo plain($i), "\n";
function same() { $r = []; for ($i = 0; $i < 3; $i++) { $r[] = static fn() => 1; } return $r[0] === $r[2] ? 'same' : 'different'; }
echo same(), "\n";
function nonstatic() { $r = []; for ($i = 0; $i < 3; $i++) { $r[] = fn() => 1; } return $r[0] === $r[2] ? 'same' : 'different'; }
echo nonstatic(), "\n";
class Dd { function __destruct() { echo "destruct\n"; } }
function captures() { $o = new Dd; $f = function () use ($o) { return 1; }; unset($o); echo "unset\n"; $f = null; echo "after\n"; }
captures();
"#),
        r#"K true m0 1

Warning: Undefined variable $undef in t.php on line 14
k011,k021,s3,k0!,k0!,NULL
unset
K true m1 2

Warning: Undefined variable $undef in t.php on line 14
k112,k122,s3,k1!,k1!,NULL
unset
K true m2 3

Warning: Undefined variable $undef in t.php on line 14
k213,k223,s3,k2!,k2!,NULL
unset
8
11
14
same
different
unset
destruct
after
"#
    );
}

/// `return` inside a `foreach` (`FE_FREE` before the return): arrays by key
/// and value, nested loops, an array whose release runs destructors before
/// the returned value is used, iterators, generators and a loop by
/// reference (the generic op's); constants `define()`d at run time read
/// into a temporary and into a variable (the run-time cache hit), a name
/// not yet defined (the `Error`) that is defined later.
#[test]
fn foreach_returns_and_runtime_constants() {
    assert_eq!(
        run(r#"<?php
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "~D{$this->n} "; } }
function first($a) { foreach ($a as $k => $v) { if ($v > 1) { return "$k=$v"; } } return 'none'; }
function nested($a) { foreach ($a as $row) { foreach ($row as $v) { if ($v === 'x') return 'found'; } } return 'no'; }
function objs() { foreach ([new D(1), new D(2)] as $o) { return $o->n; } }
function objs2($n) { $a = [new D($n)]; foreach ($a as $o) { unset($a); $o = null; return 'r'; } }
function iter() { foreach (new ArrayIterator([1, 2]) as $v) { return $v; } }
function gen() { yield 1; yield 2; }
function fromgen() { foreach (gen() as $v) { return $v; } }
function byref() { $a = [1, 2]; foreach ($a as &$v) { return $v; } }
for ($i = 0; $i < 3; $i++) {
    echo first([0, 1, 5, 7]), ' ', first([]), ' ', nested([['a'], ['b', 'x']]), ' ', nested([[]]), "\n";
    echo objs(), "|\n";
    echo objs2($i), "|\n";
    echo iter(), fromgen(), byref(), "\n";
}
if (!defined('RT')) define('RT', 'rt');
define('RT_ARR', [1, 2]);
define('RT_INT', 42);
function consts() { $x = RT; $y = RT_ARR; return $x . RT . implode(',', $y) . RT_INT . (RT_INT + 1); }
function late() { try { return LATE_C; } catch (Error $e) { return $e->getMessage(); } }
function lateTo() { try { $v = LATE_D; return $v; } catch (Error $e) { return $e->getMessage(); } }
for ($i = 0; $i < 3; $i++) {
    echo consts(), ' ', late(), ' ', lateTo(), "\n";
    if ($i == 1) { define('LATE_C', 'c'); define('LATE_D', 'd'); }
}
function caseins() { return \RT; }
echo caseins(), "\n";
"#),
        r#"2=5 none found no
~D2 ~D1 1|
~D0 r|
111
2=5 none found no
~D2 ~D1 1|
~D1 r|
111
2=5 none found no
~D2 ~D1 1|
~D2 r|
111
rtrt1,24243 Undefined constant "LATE_C" Undefined constant "LATE_D"
rtrt1,24243 Undefined constant "LATE_C" Undefined constant "LATE_D"
rtrt1,24243 c d
rt
"#
    );
}

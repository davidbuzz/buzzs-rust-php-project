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

/// `.=` on a property (`$this->p .= v`, `$o->p .= v`): untyped and typed
/// string properties, a nullable one holding null, an int property (the
/// weak conversion), an array (the warning), a readonly one (the `Error`),
/// a property bound by reference, the result used; a dynamic property and
/// one through `__get`/`__set`.
#[test]
fn property_concat_assign() {
    assert_eq!(
        run(r#"<?php
class P {
    public $s = 'a';
    public string $t = 't';
    public ?string $n = null;
    public int $i = 1;
    public $arr = [];
    public readonly string $ro;
    function __construct() { $this->ro = 'r'; }
    function run($k) {
        $this->s .= 'x';
        $this->s .= $k;
        $this->t .= 5;
        $r = ($this->t .= '-');
        $this->s .= 1.5;
        try { $this->i .= '2'; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
        try { $this->ro .= 'z'; } catch (Error $e) { echo $e->getMessage(), "\n"; }
        $this->n .= 'n';
        $this->arr .= 'q';
        return $r;
    }
}
function outer($o, $v) { $o->s .= $v; $o->t .= $v; return $o->s; }
$p = new P;
for ($i = 0; $i < 3; $i++) {
    echo $p->run($i), ' ', $p->s, ' ', $p->t, ' ', var_export($p->n, true), ' ', var_export($p->i, true), ' ', var_export($p->arr, true), "\n";
    echo outer($p, "o$i"), "\n";
}
$q = new P; $ref = &$q->s; $q->run('R'); echo $ref, "\n";
$d = new stdClass; $d->x = 'd'; for ($i = 0; $i < 3; $i++) { $d->x .= $i; } echo $d->x, "\n";
class M { private $data = []; function __get($n) { echo "get $n\n"; return 'g'; } function __set($n, $v) { echo "set $n=$v\n"; } }
$m = new M; for ($i = 0; $i < 2; $i++) { $m->zz .= 'v'; }
"#),
        r#"Cannot modify readonly property P::$ro

Warning: Array to string conversion in t.php on line 19
t5- ax01.5 t5- 'n' 12 'Arrayq'
ax01.5o0
Cannot modify readonly property P::$ro
t5-o05- ax01.5o0x11.5 t5-o05- 'nn' 122 'Arrayqq'
ax01.5o0x11.5o1
Cannot modify readonly property P::$ro
t5-o05-o15- ax01.5o0x11.5o1x21.5 t5-o05-o15- 'nnn' 1222 'Arrayqqq'
ax01.5o0x11.5o1x21.5o2
Cannot modify readonly property P::$ro

Warning: Array to string conversion in t.php on line 19
axR1.5
d012
get zz
set zz=gv
get zz
set zz=gv
"#
    );
}

/// `$obj->$name()` from one call site with the method's name in any case,
/// objects of several classes in turn (the site's cache entry misses and
/// is refilled), a static method, `__call`, private and protected methods
/// through the calling scope (the private method of the scope taking
/// precedence over a subclass's), and the errors: a private or protected
/// method from outside, an undefined method, null, a name that is not a
/// string, `__invoke` of an object and of a closure.
#[test]
fn dynamic_method_names() {
    assert_eq!(
        run(r#"<?php
class A {
    function m($x) { return "A::m$x"; }
    static function s($x) { return "A::s$x"; }
    private function p($x) { return "A::p$x"; }
    protected function q($x) { return "A::q$x"; }
    function callOwn($n, $x) { return $this->$n($x); }
    function callOther($o, $n, $x) { return $o->$n($x); }
}
class B extends A { function m($x) { return "B::m$x"; } private function p($x) { return "B::p$x"; } }
class C { function __call($n, $a) { return "C::__call $n " . implode(',', $a); } function m($x) { return "C::m$x"; } }
function dyn($o, $n, $x) { return $o->$n($x); }
function dyn2($o, $n) { $m = $n . ''; return $o->$m('t'); }
$a = new A; $b = new B; $c = new C;
for ($i = 0; $i < 3; $i++) {
    echo dyn($a, 'm', $i), ' ', dyn($a, 'M', $i), ' ', dyn($b, 'm', $i), ' ', dyn($a, 's', $i), ' ', dyn($c, 'm', $i), ' ', dyn($c, 'zz', $i), "\n";
    echo $a->callOwn('p', $i), ' ', $b->callOwn('p', $i), ' ', $a->callOther($b, 'p', $i), ' ', $a->callOwn('q', $i), ' ', dyn2($a, 'm'), ' ', dyn2($b, 'm'), "\n";
    foreach ([[$a, 'p'], [$a, 'q'], [$a, 'nope'], [null, 'm'], [$a, 5], [$a, '__invoke']] as [$o, $n]) {
        try { echo dyn($o, $n, $i), "\n"; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
    }
    $f = function ($x) { return "closure$x"; };
    echo dyn($f, '__invoke', $i), ' ', dyn($f, 'bindTo', null) === null ? 'null' : 'bound', "\n";
}
"#),
        r#"A::m0 A::m0 B::m0 A::s0 C::m0 C::__call zz 0
A::p0 A::p0 A::p0 A::q0 A::mt B::mt
Error: Call to private method A::p() from global scope
Error: Call to protected method A::q() from global scope
Error: Call to undefined method A::nope()
Error: Call to a member function m() on null
Error: Method name must be a string
Error: Call to undefined method A::__invoke()
closure0 bound
A::m1 A::m1 B::m1 A::s1 C::m1 C::__call zz 1
A::p1 A::p1 A::p1 A::q1 A::mt B::mt
Error: Call to private method A::p() from global scope
Error: Call to protected method A::q() from global scope
Error: Call to undefined method A::nope()
Error: Call to a member function m() on null
Error: Method name must be a string
Error: Call to undefined method A::__invoke()
closure1 bound
A::m2 A::m2 B::m2 A::s2 C::m2 C::__call zz 2
A::p2 A::p2 A::p2 A::q2 A::mt B::mt
Error: Call to private method A::p() from global scope
Error: Call to protected method A::q() from global scope
Error: Call to undefined method A::nope()
Error: Call to a member function m() on null
Error: Method name must be a string
Error: Call to undefined method A::__invoke()
closure2 bound
"#
    );
}

/// `in_array()` with a literal haystack of strings or ints, strict and
/// loose, of every kind of needle (a numeric string, a float, a bool, null,
/// an empty string, an array, another case) and of a reference; and
/// `func_num_args()` with fewer, as many and more arguments than declared,
/// and of a variadic function.
#[test]
fn in_array_literals_and_func_num_args() {
    assert_eq!(
        run(r#"<?php
function ia($x) {
    return var_export(in_array($x, ['a', 'b', 'c'], true), true) . var_export(in_array($x, ['a', 'b', '1']), true)
        . var_export(in_array($x, [1, 2, 3], true), true) . var_export(in_array($x, [1, 2, 3]), true);
}
foreach (['a', 'z', 1, '1', 1.0, true, null, 0, '', [], 'B'] as $v) {
    for ($i = 0; $i < 3; $i++) { echo ia($v), ' '; }
    echo "\n";
}
$r = 'b'; $ref = &$r; for ($i = 0; $i < 3; $i++) { echo var_export(in_array($ref, ['a', 'b'], true), true), "\n"; }
function na() { return func_num_args(); }
function nb($a, $b = 2) { $n = func_num_args(); return $n . ':' . func_num_args(); }
function nv(...$xs) { return func_num_args(); }
for ($i = 0; $i < 3; $i++) { echo na(), na(1, 2), nb(1), nb(1, 2, 3), nv(), nv(1, 2), "\n"; }
"#),
        r#"truetruefalsefalse truetruefalsefalse truetruefalsefalse 
falsefalsefalsefalse falsefalsefalsefalse falsefalsefalsefalse 
falsetruetruetrue falsetruetruetrue falsetruetruetrue 
falsetruefalsetrue falsetruefalsetrue falsetruefalsetrue 
falsetruefalsetrue falsetruefalsetrue falsetruefalsetrue 
falsetruefalsetrue falsetruefalsetrue falsetruefalsetrue 
falsefalsefalsefalse falsefalsefalsefalse falsefalsefalsefalse 
falsefalsefalsefalse falsefalsefalsefalse falsefalsefalsefalse 
falsefalsefalsefalse falsefalsefalsefalse falsefalsefalsefalse 
falsefalsefalsefalse falsefalsefalsefalse falsefalsefalsefalse 
falsefalsefalsefalse falsefalsefalsefalse falsefalsefalsefalse 
true
true
true
021:13:302
021:13:302
021:13:302
"#
    );
}

/// `$c::name()` with the class named by a variable (any case, a leading
/// `\`), by a temporary and by an object, inherited and overridden static
/// methods (late static binding), a private method (`__callStatic` from
/// outside, the method from its class), a non-static method with and
/// without a compatible `$this`, an abstract method, an undeclared class
/// (the `Error`, then autoloading); `$f()` of a function's name in any
/// case, a builtin, a `Class::name` string, an undefined function and
/// values that are not callable.
#[test]
fn dynamic_static_and_function_names() {
    assert_eq!(
        run(r#"<?php
class S {
    static function s($x) { return "S::s$x"; }
    function i($x) { return "S::i$x" . (isset($this) ? '+this' : ''); }
    private static function p($x) { return "S::p$x"; }
    static function __callStatic($n, $a) { return "S::__callStatic $n"; }
    static function viaSelf($c, $x) { return $c::p($x); }
    function inst($c, $x) { return $c::i($x); }
}
class T extends S { static function s($x) { return "T::s$x " . static::class; } }
abstract class Ab { abstract static function a(); }
function st($c, $x) { return $c::s($x); }
function sp($c, $x) { return $c::p($x); }
function si($c, $x) { return $c::i($x); }
function stt($c, $x) { return ($c . '')::s($x); }
function fn_($f, $x) { return $f($x); }
function lower($x) { return "lower$x"; }
$t = new T;
for ($i = 0; $i < 3; $i++) {
    echo st('S', $i), ' ', st('s', $i), ' ', st('\S', $i), ' ', st('T', $i), ' ', st($t, $i), ' ', stt('T', $i), "\n";
    echo sp('S', $i), ' ', S::viaSelf('S', $i), ' ', $t->inst('S', $i), "\n";
    foreach ([['Nope', 's'], ['Ab', 'a'], ['S', 'i']] as [$c, $m]) {
        try { echo $m === 's' ? st($c, $i) : ($m === 'a' ? $c::a() : si($c, $i)), "\n"; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
    }
    echo fn_('lower', $i), ' ', fn_('LOWER', $i), ' ', fn_('\lower', $i), ' ', fn_('strtoupper', "a$i"), ' ', fn_('S::s', $i), "\n";
    foreach (['nofn', 5, null, 'Nope::x'] as $f) {
        try { echo fn_($f, $i), "\n"; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
    }
}
spl_autoload_register(function ($c) { echo "autoload $c\n"; if ($c === 'Late') { eval('class Late { static function s($x) { return "Late::s$x"; } }'); } });
for ($i = 0; $i < 2; $i++) { echo st('Late', $i), "\n"; }
"#),
        r#"S::s0 S::s0 S::s0 T::s0 T T::s0 T T::s0 T
S::__callStatic p S::p0 S::i0+this
Error: Class "Nope" not found
Error: Cannot call abstract method Ab::a()
Error: Non-static method S::i() cannot be called statically
lower0 lower0 lower0 A0 S::s0
Error: Call to undefined function nofn()
Error: Value of type int is not callable
Error: Value of type null is not callable
Error: Class "Nope" not found
S::s1 S::s1 S::s1 T::s1 T T::s1 T T::s1 T
S::__callStatic p S::p1 S::i1+this
Error: Class "Nope" not found
Error: Cannot call abstract method Ab::a()
Error: Non-static method S::i() cannot be called statically
lower1 lower1 lower1 A1 S::s1
Error: Call to undefined function nofn()
Error: Value of type int is not callable
Error: Value of type null is not callable
Error: Class "Nope" not found
S::s2 S::s2 S::s2 T::s2 T T::s2 T T::s2 T
S::__callStatic p S::p2 S::i2+this
Error: Class "Nope" not found
Error: Cannot call abstract method Ab::a()
Error: Non-static method S::i() cannot be called statically
lower2 lower2 lower2 A2 S::s2
Error: Call to undefined function nofn()
Error: Value of type int is not callable
Error: Value of type null is not callable
Error: Class "Nope" not found
autoload Late
Late::s0
Late::s1
"#
    );
}

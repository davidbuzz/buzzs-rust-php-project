// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Script-level tests of the run-time lookup caches: every script reads
//! the same names repeatedly, so the second and later executions run
//! from the cache. Expected output from `/usr/bin/php8.5 -n` (paths
//! normalised to `t.php`) unless noted; it is also the output of the
//! uncached implementation.

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

/// Global constants: direct and namespace-fallback hits, a refcounted (array) constant, and an undefined constant that is defined later.
#[test]
fn global_constants_and_namespace_fallback() {
    let out = run(r#"<?php
namespace n;
function f() { return FOO; }
function g() { return \n\BAR; }
function h() { return ARR; }
define('FOO', 1);
define('n\BAR', 'bar');
define('ARR', [1, 2]);
for ($i = 0; $i < 3; $i++) {
    echo f(), g(), h()[1], "\n";
    define("X$i", $i);
}
function u() { try { return UNDEF_C; } catch (\Error $e) { return $e->getMessage(); } }
echo u(), "\n", u(), "\n";
define('UNDEF_C', 5);
echo u(), "\n";
"#);
    assert_eq!(
        out,
        r#"1bar2
1bar2
1bar2
Undefined constant "n\UNDEF_C"
Undefined constant "n\UNDEF_C"
5
"#
    );
}

/// Class constants: one name literal naming several classes, `static::`, private access from different scopes of one shared op array (closures, trait methods), lazy evaluation, enum cases, trait constants and dynamic class names.
#[test]
fn class_constants_by_class_and_scope() {
    let out = run(r#"<?php
class A { private const P = 'p'; public const Q = 'qa'; protected const R = 'r';
  static function p() { return self::P . static::Q; } }
class B extends A { public const Q = 'qb'; static function r() { return self::R; } }
class C { public const Q = 'qc'; }
for ($i = 0; $i < 2; $i++) { echo A::p(), B::p(), B::r(), A::Q, C::Q, "\n"; }
function outside() { try { return A::P; } catch (Error $e) { return $e->getMessage(); } }
echo outside(), "\n", outside(), "\n";
$f = function () { try { return A::P; } catch (Error $e) { return $e->getMessage(); } };
$g = Closure::bind($f, null, A::class);
echo $g(), "\n", $f(), "\n", $g(), "\n", $f(), "\n";
trait T { function n() { return self::X; } }
class D { use T; const X = 'D'; }
class E { use T; const X = 'E'; }
$d = new D; $e = new E;
for ($i = 0; $i < 2; $i++) { echo $d->n(), $e->n(); }
echo "\n";
class F { const Y = self::Z * 2; const Z = 3; }
for ($i = 0; $i < 2; $i++) { echo F::Y, F::Z, "\n"; }
enum Suit { case H; const ALIAS = self::H; }
for ($i = 0; $i < 2; $i++) { var_dump(Suit::H === Suit::ALIAS); }
trait TC { public const TX = 1; }
for ($i = 0; $i < 2; $i++) { try { echo TC::TX; } catch (Error $e) { echo $e->getMessage(), "\n"; } }
$cls = 'C';
for ($i = 0; $i < 2; $i++) { echo $cls::Q, "\n"; }
"#);
    assert_eq!(
        out,
        r#"pqapqbrqaqc
pqapqbrqaqc
Cannot access private constant A::P
Cannot access private constant A::P
p
Cannot access private constant A::P
p
Cannot access private constant A::P
DEDE
63
63
bool(true)
bool(true)
Cannot access trait constant TC::TX directly
Cannot access trait constant TC::TX directly
qc
qc
"#
    );
}

/// Static properties: read/write/inc/isset/empty through the cache, inherited and redeclared slots, private access from a bound and an unbound closure, typed properties and references, trait statics (deprecated on every access) and undeclared names.
#[test]
fn static_properties_by_class_and_scope() {
    let out = run(r#"<?php
class A { public static $a = 1; private static $p = 0; protected static $r = self::INIT; const INIT = 'r0';
  static function inc() { return ++self::$p . static::$a; } static function r() { return self::$r; } }
class B extends A { public static $a = 2; }
class C extends A {}
for ($i = 0; $i < 3; $i++) { echo A::$a, B::$a, C::$a, A::inc(), B::inc(), A::r(), "\n"; A::$a++; B::$a += 10; }
function outside() { try { return A::$p; } catch (Error $e) { return $e->getMessage(); } }
echo outside(), "\n", outside(), "\n";
$f = function () { try { return A::$p; } catch (Error $e) { return $e->getMessage(); } };
$g = Closure::bind($f, null, A::class);
echo $g(), "\n", $f(), "\n", $g(), "\n";
for ($i = 0; $i < 2; $i++) { var_dump(isset(A::$a), isset(A::$p), empty(A::$a), isset(A::$nope)); }
class Ty { public static int $n = 0; }
for ($i = 0; $i < 2; $i++) { try { Ty::$n = "x$i"; } catch (TypeError $e) { echo $e->getMessage(), "\n"; } Ty::$n = $i; }
$r = &Ty::$n;
for ($i = 0; $i < 2; $i++) { try { $r = "y"; } catch (TypeError $e) { echo $e->getMessage(), "\n"; } echo Ty::$n, "\n"; }
trait TS { public static $t = 't'; }
class UT { use TS; }
for ($i = 0; $i < 2; $i++) { echo TS::$t, UT::$t, "\n"; }
for ($i = 0; $i < 2; $i++) { try { echo A::$undeclared; } catch (Error $e) { echo $e->getMessage(), "\n"; } }
"#);
    assert_eq!(
        out,
        r#"1211122r0
212232412r0
322353622r0
Cannot access private property A::$p
Cannot access private property A::$p
6
Cannot access private property A::$p
6
bool(true)
bool(false)
bool(false)
bool(false)
bool(true)
bool(false)
bool(false)
bool(false)
Cannot assign string to property Ty::$n of type int
Cannot assign string to property Ty::$n of type int
Cannot assign string to reference held by property Ty::$n of type int
1
Cannot assign string to reference held by property Ty::$n of type int
1

Deprecated: Accessing static trait property TS::$t is deprecated, it should only be accessed on a class using the trait in t.php on line 19
tt

Deprecated: Accessing static trait property TS::$t is deprecated, it should only be accessed on a class using the trait in t.php on line 19
tt
Access to undeclared static property A::$undeclared
Access to undeclared static property A::$undeclared
"#
    );
}

/// Static calls: `self::`/`static::`/`parent::` forwarding, `parent::__construct()`, private methods from different scopes, instance methods with and without a compatible `$this`, `__callStatic`, trait, abstract and undefined methods.
#[test]
fn static_calls_by_class_scope_and_this() {
    let out = run(r#"<?php
class A { static function f() { return 'A' . static::class; } private static function p() { return 'pA'; }
  static function callp() { return self::p(); } function m() { return 'm' . get_class($this); }
  function __construct() { echo "ctorA ", static::class, "\n"; } }
class B extends A { static function g() { return self::f() . static::f() . parent::f(); }
  function __construct() { parent::__construct(); } function t() { return A::m(); } }
for ($i = 0; $i < 2; $i++) { echo A::f(), B::f(), B::g(), A::callp(), "\n"; new B; }
function outside() { try { return A::p(); } catch (Error $e) { return $e->getMessage(); } }
echo outside(), "\n", outside(), "\n";
$f = function () { try { return A::p(); } catch (Error $e) { return $e->getMessage(); } };
$g = Closure::bind($f, null, A::class);
echo $g(), "\n", $f(), "\n", $g(), "\n";
$b = new B;
$m = function () { try { return A::m(); } catch (Error $e) { return $e->getMessage(); } };
$bound = Closure::bind($m, $b, B::class);
echo $bound(), "\n", $m(), "\n", $bound(), "\n", $b->t(), "\n";
class M { static function __callStatic($n, $a) { return "cs:$n"; } }
for ($i = 0; $i < 2; $i++) { echo M::foo(), M::foo(), "\n"; }
trait T { static function ts() { return 'ts'; } }
for ($i = 0; $i < 2; $i++) { echo T::ts(), "\n"; }
abstract class Ab { abstract static function x(); }
for ($i = 0; $i < 2; $i++) { try { Ab::x(); } catch (Error $e) { echo $e->getMessage(), "\n"; } }
for ($i = 0; $i < 2; $i++) { try { A::nope(); } catch (Error $e) { echo $e->getMessage(), "\n"; } }
"#);
    assert_eq!(
        out,
        r#"AAABABABABpA
ctorA B
AAABABABABpA
ctorA B
Call to private method A::p() from global scope
Call to private method A::p() from global scope
pA
Call to private method A::p() from global scope
pA
ctorA B
mB
Non-static method A::m() cannot be called statically
mB
mB
cs:foocs:foo
cs:foocs:foo

Deprecated: Calling static trait method T::ts is deprecated, it should only be called on a class using the trait in t.php on line 20
ts

Deprecated: Calling static trait method T::ts is deprecated, it should only be called on a class using the trait in t.php on line 20
ts
Cannot call abstract method Ab::x()
Cannot call abstract method Ab::x()
Call to undefined method A::nope()
Call to undefined method A::nope()
"#
    );
}

/// `instanceof` caches a class once found; before the class exists it is looked up again.
#[test]
fn instanceof_caches_found_classes_only() {
    let out = run(r#"<?php
function t($o) { return $o instanceof Later; }
var_dump(t(new stdClass));
var_dump(t(new stdClass));
eval('class Later {}');
var_dump(t(new Later), t(new stdClass), t(new Later));
class Sub extends Later {}
var_dump(t(new Sub));
"#);
    assert_eq!(
        out,
        r#"bool(false)
bool(false)
bool(true)
bool(false)
bool(true)
bool(true)
"#
    );
}

/// `#[\Deprecated]` and internal deprecated constants are never cached: every fetch emits the deprecation (expected output from the uncached build, 8.5 syntax).
#[test]
fn deprecated_constants_warn_every_time() {
    let out = run(r#"<?php
#[\Deprecated("use B")]
const DC = 1;
class K { #[\Deprecated] const KC = 2; }
for ($i = 0; $i < 2; $i++) { echo DC, K::KC, E_STRICT, "\n"; }
"#);
    assert_eq!(
        out,
        r#"
Deprecated: Constant DC is deprecated, use B in t.php on line 5
1
Deprecated: Constant K::KC is deprecated in t.php on line 5
2
Deprecated: Constant E_STRICT is deprecated since 8.4, the error level was removed in t.php on line 5
2048

Deprecated: Constant DC is deprecated, use B in t.php on line 5
1
Deprecated: Constant K::KC is deprecated in t.php on line 5
2
Deprecated: Constant E_STRICT is deprecated since 8.4, the error level was removed in t.php on line 5
2048
"#
    );
}

/// `catch` looks the class up without autoloading and caches it once found (`ZEND_CATCH`): names in any case, interfaces, multi-catch, a class declared later and a `class_alias` (the alias catches, as in php-src; the name comparison it replaces did not).
#[test]
fn catch_looks_classes_up_once_found() {
    let out = run(r#"<?php
interface Marker {}
class MyEx extends Exception implements Marker {}
function t($e) {
    try { throw $e; }
    catch (LATER $x) { return 'later'; }
    catch (myex $x) { return 'myex'; }
    catch (MARKER $x) { return 'marker'; }
    catch (Aliased $x) { return 'aliased'; }
    catch (runtimeexception | LogicException $x) { return get_class($x); }
    catch (Throwable $x) { return 'throwable'; }
}
for ($i = 0; $i < 2; $i++) {
    echo t(new MyEx), ' ', t(new RuntimeException), ' ', t(new LogicException), ' ', t(new Error), "\n";
}
class_alias('Error', 'Aliased');
eval('class Later extends Exception {}');
for ($i = 0; $i < 2; $i++) {
    echo t(new Later), ' ', t(new MyEx), ' ', t(new Error), ' ', t(new TypeError), "\n";
}
"#);
    assert_eq!(
        out,
        r#"myex RuntimeException LogicException throwable
myex RuntimeException LogicException throwable
later myex aliased aliased
later myex aliased aliased
"#
    );
}

/// `static::X`, `static::$n` and `static::f()` from a base class method over more subclasses than a slot holds: evicted entries are refilled and every class still sees its own member.
#[test]
fn megamorphic_static_lookups_evict_correctly() {
    let out = run(r#"<?php
abstract class Base {
    const T = 'base';
    public static $n = 0;
    static function t() { return static::T . static::$n . static::make(); }
    static function make() { return '!'; }
}
$classes = [];
for ($i = 0; $i < 11; $i++) {
    eval("class C$i extends Base { const T = 't$i'; public static \$n = $i; static function make() { return '$i'; } }");
    $classes[] = "C$i";
}
for ($round = 0; $round < 3; $round++) {
    $out = '';
    foreach ($classes as $c) { $out .= $c::t() . ' '; }
    echo $out, Base::t(), "\n";
    C3::$n += 100;
}
"#);
    assert_eq!(
        out,
        r#"t000 t111 t222 t333 t444 t555 t666 t777 t888 t999 t101010 base0!
t000 t111 t222 t31033 t444 t555 t666 t777 t888 t999 t101010 base0!
t000 t111 t222 t32033 t444 t555 t666 t777 t888 t999 t101010 base0!
"#
    );
}

/// Class types match in any case, through interfaces, parents, `self`, `parent`, unions and intersections (allocation-free name matching).
#[test]
fn class_types_match_names_in_any_case() {
    let out = run(r#"<?php
interface I {}
class A implements I { function s(self $x) { return 's'; } }
class B extends A { function p(PARENT $x) { return 'p'; } }
class C {}
function f(a $x) { return 'a'; }
function g(i $x) { return 'i'; }
function h(C|A $x) { return 'u'; }
function k(I&A $x) { return 'n'; }
$a = new A; $b = new B; $c = new C;
for ($i = 0; $i < 2; $i++) {
    echo f($a), f($b), g($b), h($c), h($b), k($b), $b->s($a), $b->p($b), "\n";
    foreach ([fn() => f($c), fn() => g($c), fn() => $a->s($c), fn() => $b->p($c), fn() => k($c)] as $t) {
        try { $t(); } catch (TypeError $e) { echo strstr($e->getMessage(), ', called', true), "\n"; }
    }
}
"#);
    assert_eq!(
        out,
        r#"aaiuunsp
f(): Argument #1 ($x) must be of type a, C given
g(): Argument #1 ($x) must be of type i, C given
A::s(): Argument #1 ($x) must be of type A, C given
B::p(): Argument #1 ($x) must be of type A, C given
k(): Argument #1 ($x) must be of type I&A, C given
aaiuunsp
f(): Argument #1 ($x) must be of type a, C given
g(): Argument #1 ($x) must be of type i, C given
A::s(): Argument #1 ($x) must be of type A, C given
B::p(): Argument #1 ($x) must be of type A, C given
k(): Argument #1 ($x) must be of type I&A, C given
"#
    );
}

/// `$obj::X` (a dynamic class operand): cached per class of the object,
/// so a subclass instance, a private constant seen through a subclass,
/// a class-name string, an enum case and a temporary object (freed by the
/// fetch, before the echo) all behave as uncached.
#[test]
fn class_constants_through_objects() {
    let out = run(r#"<?php
class P { const X = 'px'; const ARR = [1, 2]; protected const PROT = 'prot'; }
class C extends P {
    const X = 'cx'; const Y = self::X . '!'; private const PRIV = 'priv';
    function run() {
        for ($i = 0; $i < 3; $i++) {
            echo $this::X, ' ', $this::Y, ' ', $this::PROT, ' ', count($this::ARR), "\n";
            try { echo $this::PRIV, "\n"; } catch (Error $e) { echo $e->getMessage(), "\n"; }
            $o = new P; echo $o::X, "\n";
            $n = 'C'; echo $n::X, "\n";
        }
    }
}
class D extends C { const X = 'dx'; }
(new C)->run();
(new D)->run();
$c = new C;
for ($i = 0; $i < 2; $i++) {
    echo $c::X, "\n";
    try { echo $c::PRIV, "\n"; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    try { echo $c::NOPE, "\n"; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    $c = new D;
}
enum E: string { case A = 'a'; const K = self::A; }
$e = E::A;
for ($i = 0; $i < 2; $i++) { var_dump($e::K, $e::A->value); }
class W { function __destruct() { echo "~W\n"; } const Z = 'z'; }
for ($i = 0; $i < 2; $i++) { echo (new W)::Z, "\n"; echo "after\n"; }
"#);
    let c_run = "cx cx! prot 2\npriv\npx\ncx\n";
    let d_run = "dx cx! prot 2\nUndefined constant D::PRIV\npx\ncx\n";
    let expected = format!(
        "{}{}cx\nCannot access private constant C::PRIV\nUndefined constant C::NOPE\ndx\nUndefined constant D::PRIV\nUndefined constant D::NOPE\nenum(E::A)\nstring(1) \"a\"\nenum(E::A)\nstring(1) \"a\"\n~W\nz\nafter\n~W\nz\nafter\n",
        c_run.repeat(3),
        d_run.repeat(3)
    );
    assert_eq!(out, expected);
}

/// `$x = NAME;` / `$x = A::NAME;` with the `AssignCv` folded into the fetch
/// (`FetchConstTo`, `FetchClassConstTo`): namespaced, global, fallback and
/// array constants, class constants through a name, `self`, `static`,
/// `parent` and `$this`, a target holding an object (destructed by the
/// assignment), a reference and an array; the errors of uncached fetches
/// each time.
#[test]
fn constant_fetches_assigned_to_variables() {
    let out = run(r#"<?php
namespace N;
const LOCAL = 'local';
define('GLOBAL_C', 'global');
define('M_INT', 8);
define('ARR', [1, 2]);
class D { function __destruct() { echo "~D\n"; } }
class A { const X = 'ax'; const ARR = ['k' => 1]; private const P = 'priv'; static function p() { $v = self::P; return $v; } }
class B extends A { const X = 'bx'; function get() { $a = static::X; $b = self::X; $c = parent::X; $d = $this::X; return "$a $b $c $d"; } }
for ($i = 0; $i < 3; $i++) {
    $l = LOCAL; $g = GLOBAL_C; $p = \M_INT; $arr = ARR; $e = \N\LOCAL;
    $x = A::X; $y = B::X; $ca = A::ARR;
    echo $l, ' ', $g, ' ', $p, ' ', $arr[1], ' ', $e, ' ', $x, ' ', $y, ' ', $ca['k'], ' ', A::p(), ' ', (new B)->get(), "\n";
}
$o = new D; echo "made\n"; $o = A::X; echo "replaced ", $o, "\n";
$t = 1; $r = &$t; $t = GLOBAL_C; echo $r, "\n";
$q = [1]; $q = LOCAL; echo $q, "\n";
for ($i = 0; $i < 2; $i++) { try { $u = UNDEF_CONST; } catch (\Error $e) { echo $e->getMessage(), "\n"; } }
for ($i = 0; $i < 2; $i++) { try { $u = A::P; } catch (\Error $e) { echo $e->getMessage(), "\n"; } }
for ($i = 0; $i < 2; $i++) { try { $u = A::NOPE; } catch (\Error $e) { echo $e->getMessage(), "\n"; } }
define('N\LATE', 'late-ns');
function late() { $z = LATE; return $z; }
echo late(), "\n";
"#);
    assert_eq!(
        out,
        r#"local global 8 2 local ax bx 1 priv bx bx ax bx
local global 8 2 local ax bx 1 priv bx bx ax bx
local global 8 2 local ax bx 1 priv bx bx ax bx
made
~D
replaced ax
global
local
Undefined constant "N\UNDEF_CONST"
Undefined constant "N\UNDEF_CONST"
Cannot access private constant N\A::P
Cannot access private constant N\A::P
Undefined constant N\A::NOPE
Undefined constant N\A::NOPE
late-ns
"#
    );
}

/// The hot loop's cache hits (`exec/hot.rs`, `exec/spec/statics.rs`):
/// `static::`/`self::` constants and static properties per called class
/// and scope, a typed property holding null (the singleton test) or
/// uninitialized, `isset`/`empty`, closures bound to other scopes (the
/// same op array, other access), a reference to a static property and
/// `func_num_args()` with an unused result. Expected output from
/// `/usr/bin/php8.5 -n`.
#[test]
fn hot_loop_cache_hits_keep_scope_and_type_semantics() {
    let out = run(r#"<?php
class A {
    public static $s = 'a';
    private static $p = 'pa';
    public static ?A $inst = null;
    public static int $n;
    const C = 'ca';
    private const PC = 'pca';
    static function get() { return static::C . ' ' . self::PC . ' ' . static::$s . ' ' . self::$p; }
    static function inst() { if (self::$inst === null) { self::$inst = new static; echo "new\n"; } return self::$inst; }
    function args() { $r = []; for ($i = 0; $i < 2; $i++) { $r[] = func_num_args(); func_num_args(); } return implode(',', $r); }
}
class B extends A { public static $s = 'b'; const C = 'cb'; }
for ($i = 0; $i < 3; $i++) {
    echo A::get(), ' | ', B::get(), "\n";
    A::inst(); B::inst();
    var_dump(isset(A::$n), empty(A::$n), isset(B::$inst), empty(A::$s));
    try { $x = A::$n; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    echo (new B)->args(1, 2, 3), "\n";
}
$f = function () { $o = []; for ($i = 0; $i < 2; $i++) { $o[] = static::$s . self::C . static::C; } return implode(',', $o); };
foreach (['A', 'B'] as $c) {
    $g = Closure::bind($f, null, $c);
    echo $g(), "\n";
}
$h = function () { $o = []; for ($i = 0; $i < 2; $i++) { try { $o[] = A::$p; } catch (Error $e) { $o[] = $e->getMessage(); } } return implode(',', $o); };
echo Closure::bind($h, null, 'A')(), "\n";
echo Closure::bind($h, null, 'B')(), "\n";
echo $h(), "\n";
$r = &A::$s; for ($i = 0; $i < 2; $i++) { A::$s = "w$i"; $y = A::$s; echo $r, $y, "\n"; }
A::$n = 5; for ($i = 0; $i < 2; $i++) { $z = A::$n; A::$n = $z + 1; echo A::$n, "\n"; }
"#);
    assert_eq!(
        out,
        r#"ca pca a pa | cb pca b pa
new
bool(false)
bool(true)
bool(true)
bool(false)
Typed static property A::$n must not be accessed before initialization
3,3
ca pca a pa | cb pca b pa
bool(false)
bool(true)
bool(true)
bool(false)
Typed static property A::$n must not be accessed before initialization
3,3
ca pca a pa | cb pca b pa
bool(false)
bool(true)
bool(true)
bool(false)
Typed static property A::$n must not be accessed before initialization
3,3
acaca,acaca
bcbcb,bcbcb
pa,pa
Cannot access private property A::$p,Cannot access private property A::$p
Cannot access private property A::$p,Cannot access private property A::$p
w0w0
w1w1
6
7
"#
    );
}

/// Unfilled static property entries share one placeholder cell, so that
/// growing the entry table (outside the counted heap) and overwriting an
/// unfilled entry (inside it) allocate and free nothing: a cell per entry
/// made each first fill of an `A::$x` place lower `memory_get_usage()` by
/// 40 bytes for good (C PHP: 0).
#[test]
fn unfilled_static_prop_entries_share_their_cell() {
    let a = super::StaticPropCacheEntry::default();
    let b = super::StaticPropCacheEntry::default();
    assert!(Rc::ptr_eq(&a.cell, &b.cell));
    assert_eq!(a.class_ptr, 0);
}

/// `clear` empties a cache that was used and leaves an empty one as it
/// is; `is_empty` sees every kind of entry.
#[test]
fn clear_empties_used_caches_only() {
    use super::{put, DefinedSlot, GlobalSlot, RuntimeCache};
    let c = RuntimeCache::default();
    assert!(c.is_empty());
    c.clear();
    assert!(c.is_empty());
    put(&c.defined, 2, DefinedSlot::Found);
    assert!(!c.is_empty());
    c.clear();
    assert!(c.is_empty());
    put(&c.globals, 0, GlobalSlot::GlobalStore(3));
    assert!(!c.is_empty());
    c.clear();
    assert!(c.is_empty());
}

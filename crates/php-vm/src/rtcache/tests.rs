//! Script-level tests of the run-time lookup caches: every script reads
//! the same names repeatedly, so the second and later executions run
//! from the cache. Expected output from `/usr/bin/php -n` (paths
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
    echo f(), g(), count(h()), "\n";
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

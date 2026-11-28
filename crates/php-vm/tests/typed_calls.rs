// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Parameter and return type verification of user calls
//! (`zend_verify_recv_arg_type`, `zend_verify_return_type`): the slow paths
//! that coerce, resolve class types or raise the `TypeError`. Expected
//! outputs are php-src's (`/usr/bin/php8.5 -n`).
use php_vm::{Vm, VmOptions};
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
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    let output = String::from_utf8(output).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

#[test]
fn typed_parameters_and_returns_match_reference() {
    assert_eq!(
        run(r#"<?php
class A { function __toString(): string { return "A!"; } }
class B extends A {}
function obj(A $a) { return get_class($a); }
echo obj(new B), "\n";
try { obj(new stdClass); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
function nul(?A $a = null) { return $a === null ? "null" : "obj"; }
echo nul(), nul(null), nul(new A), "\n";
function un(int|string $x) { var_dump($x); }
un(1.0); un("1"); un(true);
try { un([]); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
function v(): void { return; }
var_dump(v());
function nv(): never { throw new Exception("never"); }
try { nv(); } catch (Exception $e) { echo $e->getMessage(), "\n"; }
function nv2(): never { }
try { nv2(); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
function fl(): float { return 1; }
var_dump(fl());
function st(): string { return new A; }
var_dump(st());
function st2(): string { return []; }
try { st2(); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
function none(): int { }
try { none(); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
class C { static function make(): static { return new static; } function self(): self { return $this; } function bad(): self { return new stdClass; } }
class D extends C {}
echo get_class(D::make()), get_class((new D)->self()), "\n";
try { (new D)->bad(); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
function it(iterable $i) { $n = 0; foreach ($i as $_) { $n++; } return $n; }
echo it([1,2]), it((fn() => yield 1)()), "\n";
function cb(callable $c) { return $c(); }
echo cb(fn() => "cb"), cb("phpversion") === PHP_VERSION ? "ok" : "no", "\n";
function byref(int &$r) { $r++; }
$z = 1; byref($z); echo $z, "\n";
$s = "5"; byref($s); var_dump($s);
function gen(): Generator { yield 1; }
foreach (gen() as $g) echo $g, "\n";
function m(mixed $m): mixed { return $m; }
var_dump(m(null));
class E { function f(self $x): static { return $x; } }
try { (new E)->f(new stdClass); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
function nn(?int $x): ?int { return $x; }
var_dump(nn(null), nn("7"));
function deep(int $x): int { return $x > 0 ? deep($x - 1) : 0; }
echo deep(50), "\n";
"#),
        r#"B
obj(): Argument #1 ($a) must be of type A, stdClass given, called in t.php on line 6
nullnullobj
int(1)
string(1) "1"
int(1)
un(): Argument #1 ($x) must be of type string|int, array given, called in t.php on line 11
NULL
never
nv2(): never-returning function must not implicitly return
float(1)
string(2) "A!"
st2(): Return value must be of type string, array returned
none(): Return value must be of type int, none returned
DD
C::bad(): Return value must be of type C, stdClass returned
21
cbok
2
int(6)
1
NULL
E::f(): Argument #1 ($x) must be of type E, stdClass given, called in t.php on line 42
NULL
int(7)
0
"#
    );
}

#[test]
fn strict_types_calls_match_reference() {
    assert_eq!(
        run(r#"<?php
declare(strict_types=1);
function f(float $x): float { return $x; }
var_dump(f(1));
function g(int $x): int { return $x; }
try { g("1"); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
function h(): int { return 1.5; }
try { h(); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
class A {}
function k(A $a): A { return $a; }
echo get_class(k(new A)), "\n";
function w(): float { return 2; }
var_dump(w());
"#),
        r"float(1)
g(): Argument #1 ($x) must be of type int, string given, called in t.php on line 6
h(): Return value must be of type int, float returned
A
float(2)
"
    );
}

/// The class a parameter or return type took last is remembered by the
/// type and accepted again by one address compare: only a class accepted
/// through a name the type lists. A `self` or `parent` match depends on
/// the call's scope, which a trait's method has once per using class and
/// a rebound closure changes; a `static` one on the called class.
#[test]
fn remembered_classes_of_types_keep_scope_dependent_checks() {
    assert_eq!(
        run(r#"<?php
interface Shape {} class Base implements Shape {} class Leaf extends Base {} class Other {}
trait T { function me($o): self { return $o; } function take(self $o) { return 1; } }
class A { use T; } class B { use T; }
function t($f) { try { $f(); echo "ok\n"; } catch (TypeError $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; } }
for ($i = 0; $i < 3; $i++) {
  t(fn() => (new A)->me(new A));
  t(fn() => (new B)->me(new A));
  t(fn() => (new B)->take(new A));
  t(fn() => (new A)->take(new A));
}
// rebound closure with a self return type
$c = function ($o): self { return $o; };
foreach ([A::class, B::class, A::class, B::class] as $s) { $b = Closure::bind($c, null, $s); t(fn() => $b(new A)); }
// param of a parent type: subclasses, then an unrelated class
function p(Base $o) { return 1; }
foreach ([new Leaf, new Base, new Leaf, new Other, new Leaf] as $o) t(fn() => p($o));
function r($o): Shape { return $o; }
foreach ([new Leaf, new Leaf, new Other] as $o) t(fn() => r($o));
// nullable and union
function n(?Leaf $o) { return 1; }
foreach ([new Leaf, null, new Base, new Leaf] as $o) t(fn() => n($o));
function u(Leaf|int $o) { return 1; }
foreach ([new Leaf, 3, "4", new Base] as $o) t(fn() => u($o));
// class_alias'd name
class_alias('Leaf', 'Alias');
function al(Alias $o) { return 1; }
foreach ([new Leaf, new Leaf, new Base] as $o) t(fn() => al($o));
// static return
class S { function m($o): static { return $o; } } class S2 extends S {}
foreach ([[new S2, new S2], [new S, new S2], [new S2, new S]] as [$a, $o]) t(fn() => $a->m($o));
// parent type
class P1 { } class P2 extends P1 { function m($o): parent { return $o; } }
foreach ([new P1, new P2, new Other] as $o) t(fn() => (new P2)->m($o));
"#),
        r#"ok
TypeError: B::me(): Return value must be of type B, A returned
TypeError: B::take(): Argument #1 ($o) must be of type B, A given, called in t.php on line 9
ok
ok
TypeError: B::me(): Return value must be of type B, A returned
TypeError: B::take(): Argument #1 ($o) must be of type B, A given, called in t.php on line 9
ok
ok
TypeError: B::me(): Return value must be of type B, A returned
TypeError: B::take(): Argument #1 ($o) must be of type B, A given, called in t.php on line 9
ok
ok
TypeError: B::{closure:t.php:13}(): Return value must be of type B, A returned
ok
TypeError: B::{closure:t.php:13}(): Return value must be of type B, A returned
ok
ok
ok
TypeError: p(): Argument #1 ($o) must be of type Base, Other given, called in t.php on line 17
ok
ok
ok
TypeError: r(): Return value must be of type Shape, Other returned
ok
ok
TypeError: n(): Argument #1 ($o) must be of type ?Leaf, Base given, called in t.php on line 22
ok
ok
ok
ok
TypeError: u(): Argument #1 ($o) must be of type Leaf|int, Base given, called in t.php on line 24
ok
ok
TypeError: al(): Argument #1 ($o) must be of type Alias, Base given, called in t.php on line 28
ok
ok
TypeError: S::m(): Return value must be of type S2, S returned
ok
ok
TypeError: P2::m(): Return value must be of type P1, Other returned
"#
    );
}

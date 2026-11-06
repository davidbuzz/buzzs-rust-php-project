//! Parameter and return type verification of user calls
//! (`zend_verify_recv_arg_type`, `zend_verify_return_type`): the slow paths
//! that coerce, resolve class types or raise the `TypeError`. Expected
//! outputs are php-src's (`/usr/bin/php -n`).
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

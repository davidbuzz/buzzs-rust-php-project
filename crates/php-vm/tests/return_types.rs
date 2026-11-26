// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Returns of objects from functions declaring `static`, `self` or
//! `parent` (`ZEND_VERIFY_RETURN_TYPE`), which the fast return checks
//! against the frame's called class and scope before the generic check.

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

/// `static` on the class, subclasses, a final class, an interface's
/// method, a static factory (`new static`), a closure's bound scope and a
/// trait's method; `self` and `parent`; `?static` and `static|int`; and
/// the `TypeError` of an object of the declaring class returned from a
/// subclass (`static` is the called class). Expected output: php-src's
/// (PHP 8.5 CLI).
#[test]
fn static_self_and_parent_returns() {
    let out = run(r#"<?php
interface I { public function me(): static; }
class A implements I {
    public function me(): static { return $this; }
    public function selfy(): self { return $this; }
    public function nullable(): ?static { return null; }
    public function orInt(bool $b): static|int { return $b ? $this : 3; }
    public function wrong(): static { return new A; }
    public static function make(): static { return new static; }
    public function closure() { return function (): static { return $this; }; }
}
class B extends A {
    public function par(): parent { return $this; }
    public function parNew(): parent { return new A; }
}
final class C extends B {}
trait T { public function tme(): static { return $this; } public function tself(): self { return $this; } }
class D { use T; }
class E extends D {}
$a = new A; $b = new B; $c = new C; $e = new E;
var_dump(get_class($a->me()), get_class($b->me()), get_class($c->me()));
var_dump(get_class($b->selfy()), get_class($c->par()), get_class($c->parNew()));
var_dump($a->nullable(), $b->orInt(false), get_class($b->orInt(true)));
var_dump(get_class(A::make()), get_class(C::make()));
var_dump(get_class(($b->closure())()));
var_dump(get_class($e->tme()), get_class($e->tself()));
var_dump(get_class($a->wrong()));
try { $b->wrong(); } catch (TypeError $t) { echo $t->getMessage(), "\n"; }
try { $c->wrong(); } catch (TypeError $t) { echo $t->getMessage(), "\n"; }
$f = Closure::bind(function (): static { return new A; }, new B, B::class);
try { $f(); } catch (TypeError $t) { echo $t->getMessage(), "\n"; }
for ($i = 0; $i < 3; $i++) { $x = $c->me(); } var_dump(get_class($x));
"#);
    assert_eq!(
        out,
        r#"string(1) "A"
string(1) "B"
string(1) "C"
string(1) "B"
string(1) "C"
string(1) "A"
NULL
int(3)
string(1) "B"
string(1) "A"
string(1) "C"
string(1) "B"
string(1) "E"
string(1) "E"
string(1) "A"
A::wrong(): Return value must be of type B, A returned
A::wrong(): Return value must be of type C, A returned
B::{closure:t.php:30}(): Return value must be of type B, A returned
string(1) "C"
"#
    );
}

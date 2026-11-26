// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Methods a class or interface takes from its interfaces
//! (`zend_do_implement_interfaces`, `do_interface_implementation`,
//! `Zend/zend_inheritance.c`): the interfaces a declaration names come
//! before the ones they extend, so a method an interface overrides is the
//! one its sub-interfaces and implementing abstract classes hold. `PHPUnit`
//! builds a mock's methods from `ReflectionClass::getMethods()`, so the
//! wrong one made the mock of an interface a fatal error (seen with
//! Drupal's `ContainerInterface`, which extends Symfony's, which narrows
//! PSR's `get()`). Expected output: PHP 8.5.

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

/// Runs `source` as `t.php` with run-tests.php's `fatal_error_backtraces=0`.
fn run(source: &str) -> String {
    let sink = Sink::default();
    let options = VmOptions {
        ini: vec![("fatal_error_backtraces".to_string(), "0".to_string())],
    };
    let mut vm = Vm::new(&options, Box::new(sink.clone()));
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

#[test]
fn an_interface_holds_the_overriding_method_of_the_interface_it_extends() {
    let out = run(r#"<?php
interface P { public function get(string $id); public function has(string $id): bool; }
interface Sy extends P { public function set(string $id): void; public function get(string $id, int $b = 1): ?object; }
interface D extends Sy {}
interface D2 extends Sy, P {}
abstract class C implements D {}
abstract class C2 extends C {}
interface A { function a(); } interface B extends A { function b(); } interface X { function x(); }
interface E extends B, X {} interface F extends X, B {}
abstract class K implements E, Countable {}
foreach (['D', 'D2', 'C', 'C2', 'E', 'F', 'K'] as $c) {
    $r = new ReflectionClass($c);
    $m = $r->hasMethod('get') ? $r->getMethod('get') : null;
    echo $c, ': ', $m ? $m->class . '/' . $m->getNumberOfParameters() : '-', ' | ',
        implode(',', array_map(fn($m) => $m->class . '::' . $m->name, $r->getMethods())), ' | ',
        implode(',', get_class_methods($c)), "\n";
}
"#);
    assert_eq!(
        out,
        r"D: Sy/2 | Sy::set,Sy::get,P::has | set,get,has
D2: Sy/2 | Sy::set,Sy::get,P::has | set,get,has
C: Sy/2 | Sy::set,Sy::get,P::has | set,get,has
C2: Sy/2 | Sy::set,Sy::get,P::has | set,get,has
E: - | B::b,A::a,X::x | b,a,x
F: - | X::x,B::b,A::a | x,b,a
K: - | B::b,A::a,X::x,Countable::count | b,a,x,count
"
    );
}

/// Named the other way round, the class takes `P::get()` first, which the
/// narrower `Sy::get()` then rejects.
#[test]
fn a_method_taken_from_an_interface_is_checked_against_the_later_ones() {
    let out = run(r#"<?php
interface P { public function get(string $id); }
interface Sy extends P { public function get(string $id, int $b = 1): ?object; }
interface D3 extends P, Sy {}
echo "not reached\n";
"#);
    assert_eq!(
        out,
        r"
Fatal error: Declaration of P::get(string $id) must be compatible with Sy::get(string $id, int $b = 1): ?object in t.php on line 2
"
    );
}

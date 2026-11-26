// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Which interface methods a class's own methods are checked against
//! (`zend_do_implement_interfaces`, `Zend/zend_inheritance.c`): only the
//! interfaces the declaration names get `do_interface_implementation`, and
//! each of those already holds the methods it inherits with its own
//! overrides in place. So where `I extends Iterator` redeclares `current()`
//! with `#[\ReturnTypeWillChange]`, a class implementing `I` is checked
//! against `I::current()` and gets no tentative-return-type deprecation;
//! naming `Iterator` as well brings the deprecation back. `PHPUnit` 9 mocks
//! of `MediaWiki`'s `IResultWrapper` (which does exactly that) failed on the
//! spurious deprecation. Expected output: PHP 8.5.

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
fn own_methods_are_checked_against_the_named_interfaces_only() {
    let out = run(r#"<?php
interface I2 extends Iterator { #[\ReturnTypeWillChange] public function current(); }
interface I3 extends I2 {}
trait T {
    public function key(): int { return 0; }
    public function next(): void {}
    public function rewind(): void {}
    public function valid(): bool { return false; }
}
trait TC { public function current() { return 1; } }
class C0 implements I3 { use T; public function current() { return 1; } }
echo "0\n";
class D0 implements Iterator, I3 { use T; public function current() { return 1; } }
echo "1\n";
class C1 implements I3, Iterator { use T; public function current() { return 1; } }
echo "2\n";
class C2 implements I2, Countable { use T; public function current() { return 1; } public function count() { return 0; } }
echo "3\n";
abstract class P implements I2 {}
class C3 extends P { use T; public function current() { return 1; } }
echo "4\n";
class C4 extends P implements Iterator { use T; public function current() { return 1; } }
echo "5\n";
abstract class Q implements Iterator {}
class C5 extends Q implements I2 { use T; public function current() { return 1; } }
echo "6\n";
interface I4 extends I2, Iterator { public function current(); }
echo "7\n";
interface I5 extends Iterator, I2 { public function current(); }
echo "8\n";
class C6 implements I3 { use T, TC; }
echo "9\n";
enum E implements I2 { use T; case A; public function current() { return 1; } }
echo "10\n";
$o = new class implements I3 { use T; public function current() { return 1; } };
echo "11\n";
abstract class A implements Iterator { #[\ReturnTypeWillChange] public function current() {} }
class B extends A { use T; public function current() { return 1; } }
echo "12\n";
"#);
    assert_eq!(
        out,
        r"0

Deprecated: Return type of D0::current() should either be compatible with Iterator::current(): mixed, or the #[\ReturnTypeWillChange] attribute should be used to temporarily suppress the notice in t.php on line 13
1

Deprecated: Return type of C1::current() should either be compatible with Iterator::current(): mixed, or the #[\ReturnTypeWillChange] attribute should be used to temporarily suppress the notice in t.php on line 15
2

Deprecated: Return type of C2::count() should either be compatible with Countable::count(): int, or the #[\ReturnTypeWillChange] attribute should be used to temporarily suppress the notice in t.php on line 17
3
4
5

Deprecated: Return type of C5::current() should either be compatible with Iterator::current(): mixed, or the #[\ReturnTypeWillChange] attribute should be used to temporarily suppress the notice in t.php on line 25
6

Deprecated: Return type of I4::current() should either be compatible with Iterator::current(): mixed, or the #[\ReturnTypeWillChange] attribute should be used to temporarily suppress the notice in t.php on line 27
7

Deprecated: Return type of I5::current() should either be compatible with Iterator::current(): mixed, or the #[\ReturnTypeWillChange] attribute should be used to temporarily suppress the notice in t.php on line 29
8
9
10
11
12
"
    );
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The line a property fetch, a method call's init and a property
//! assignment report when the expression spans several lines. php-src
//! compiles the member name with `zend_compile_expr()`, which moves
//! `CG(zend_lineno)` to the line of the name (`Zend/zend_compile.c`:
//! `zend_delayed_compile_prop`, `zend_compile_method_call`,
//! `zend_compile_static_call`), so `$o\n->nope()` throws on the line of
//! `nope`, and an assignment is the delayed opline made after the name.
//! Expected output from `/usr/bin/php8.5 -n`.
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

fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = common::vm_with_stdlib(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

#[test]
fn multi_line_members_report_the_name_line() {
    assert_eq!(
        run(r###"<?php
class C { public $p; public ?C $c = null; static $s; function m() { return $this; } }
set_error_handler(function ($no, $msg, $f, $l) { echo "$msg @$l\n"; return true; });
function f($o) {
  try { $o
    ->nope(); } catch (Error $e) { echo $e->getMessage(), ' @', $e->getLine(), "\n"; }
  try { $o
    ->m()
    ->nope2(
    ); } catch (Error $e) { echo $e->getMessage(), ' @', $e->getLine(), "\n"; }
  try { C
    ::nope3(); } catch (Error $e) { echo $e->getMessage(), ' @', $e->getLine(), "\n"; }
  $x = $o
    ->m()
    ->q;
  $o
    ->r
    = 5;
  try { $o
    ->c
    ->z = 1; } catch (Error $e) { echo $e->getMessage(), ' @', $e->getLine(), "\n"; }
  $w = $o->
    p2;
  $v = $o
    ->missing;
  $u = [];
  $u
    ['k']
    = $o
    ->p;
  return $x;
}
for ($i = 0; $i < 3; $i++) { f(new C); }
"###),
        r###"Call to undefined method C::nope() @6
Call to undefined method C::nope2() @9
Call to undefined method C::nope3() @12
Undefined property: C::$q @15
Creation of dynamic property C::$r is deprecated @17
Attempt to assign property "z" on null @21
Undefined property: C::$p2 @23
Undefined property: C::$missing @25
Call to undefined method C::nope() @6
Call to undefined method C::nope2() @9
Call to undefined method C::nope3() @12
Undefined property: C::$q @15
Creation of dynamic property C::$r is deprecated @17
Attempt to assign property "z" on null @21
Undefined property: C::$p2 @23
Undefined property: C::$missing @25
Call to undefined method C::nope() @6
Call to undefined method C::nope2() @9
Call to undefined method C::nope3() @12
Undefined property: C::$q @15
Creation of dynamic property C::$r is deprecated @17
Attempt to assign property "z" on null @21
Undefined property: C::$p2 @23
Undefined property: C::$missing @25
"###
    );
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Calls of builtin methods (`ReflectionMethod::getName()`,
//! `Closure::call()`, `Generator::send()`, `Exception::getLine()`) that the
//! tape prepares and makes like builtin function calls, with `$this` and
//! the scopes on the call frame: results, a call on a temporary object, a
//! static method invoked through reflection, and a `ReflectionException`
//! whose trace shows the builtin method's frame. The expected output is
//! what the generic builtin call printed before (no PHP is available as an
//! oracle where this was written).

#![allow(clippy::needless_raw_string_hashes)]

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
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

#[test]
fn builtin_method_calls_from_the_tape() {
    assert_eq!(
        run(r#"<?php
class P { private $x = 1; public static function st() { return 'st'; } function m($a = 5) { return $a; } }
function t() {
  $rm = new ReflectionMethod('P', 'm');
  var_dump($rm->getName(), $rm->isStatic(), $rm->isPublic(), $rm->getNumberOfParameters(), $rm->getParameters()[0]->getName());
  $rc = new ReflectionClass('P');
  var_dump($rc->getShortName(), $rc->hasMethod('m'), $rc->hasMethod('nope'), count($rc->getMethods()), (new ReflectionClass('P'))->getName());
  var_dump($rc->getMethod('st')->invoke(null), $rc->newInstance()->m(7), $rc->getMethod('m')->invoke(new P, 3));
  $c = function () { return $this->x; };
  var_dump($c->bindTo(new P, P::class)(), $c->call(new P));
  try { $rc->getMethod('nope'); } catch (ReflectionException $e) {
    echo $e->getMessage(), ' / ', $e->getTrace()[0]['class'], '->', $e->getTrace()[0]['function'], ' line ', $e->getLine(), "\n";
  }
  $gen = (function () { $x = yield 1; echo "got $x\n"; yield 2; })();
  var_dump($gen->current()); $gen->send('s'); var_dump($gen->current(), $gen->valid(), $gen->key());
  $err = new ErrorException('msg', 1, E_WARNING, 'f.php', 3);
  var_dump($err->getSeverity(), $err->getLine(), $err->getMessage(), $err->getFile(), $err->getCode(), $err->getPrevious());
  for ($i = 0; $i < 3; $i++) { var_dump($rm->getShortName(), $rm->isFinal()); }
}
t();
"#),
        r#"string(1) "m"
bool(false)
bool(true)
int(1)
string(1) "a"
string(1) "P"
bool(true)
bool(false)
int(2)
string(1) "P"
string(2) "st"
int(7)
int(3)
int(1)
int(1)
Method P::nope() does not exist / ReflectionClass->getMethod line 11
int(1)
got s
int(2)
bool(true)
int(1)
int(2)
int(3)
string(3) "msg"
string(5) "f.php"
int(1)
NULL
string(1) "m"
bool(false)
string(1) "m"
bool(false)
string(1) "m"
bool(false)
"#
    );
}

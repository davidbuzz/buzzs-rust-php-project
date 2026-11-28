// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Method and class names looked up as the caller spells them
//! (`Vm::method_call_on`, `Vm::class_by_name`): any case, names longer
//! than the stack buffer they are lower-cased in, calls `__call()` answers
//! (never cached, so looked up on each call), a private method of the
//! calling scope, `Closure::__invoke` and a protected property's access
//! check. Expected output from `/usr/bin/php8.5`.

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

fn run(source: &str) -> (i32, String) {
    let sink = Sink::default();
    let mut vm = common::vm_with_stdlib(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    (status, String::from_utf8(output).expect("UTF-8 output"))
}

#[test]
fn method_names_in_any_case_match_php_src() {
    assert_eq!(
        run(r#"<?php
class P { private function hid() { return "P::hid"; } function callHid($o) { return $o->HID(); } }
class C extends P {
  function __call($n, $a) { return "__call($n)"; }
  function Mixed() { return "C::Mixed"; }
  function aVeryLongMethodNameThatIsLongerThanSixtyFourBytesForTheStackBufferXYZ() { return "long"; }
  protected function prot() { return "prot"; }
}
$c = new C;
foreach (['mixed', 'MIXED', 'Mixed', 'missingOne', 'MissingOne', 'AVERYLONGMETHODNAMETHATISLONGERTHANSIXTYFOURBYTESFORTHESTACKBUFFERXYZ', 'aVeryLongMissingMethodNameThatIsLongerThanSixtyFourBytesForTheBuffer', 'PROT', 'hid'] as $m) {
  echo $c->$m(), "\n";
}
echo (new P)->callHid($c), "\n";
$f = function ($x) { return "closure $x"; };
echo $f->__INVOKE(1), "\n", call_user_func([$f, '__Invoke'], 2), "\n";
class D { protected $p = 1; } class E extends D { function get($o) { return $o->p; } }
echo (new E)->get(new E), "\n";
try { echo (new D)->p; } catch (Error $e) { echo $e->getMessage(), "\n"; }
class F { function __get($n) { return "__get($n)"; } protected $q = 2; }
echo (new F)->q, "\n";
try { (new P)->nothere(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { (new P)->HID(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#).1,
        r"C::Mixed
C::Mixed
C::Mixed
__call(missingOne)
__call(MissingOne)
long
__call(aVeryLongMissingMethodNameThatIsLongerThanSixtyFourBytesForTheBuffer)
__call(PROT)
__call(hid)
P::hid
closure 1
closure 2
1
Cannot access protected property D::$p
__get(q)
Call to undefined method P::nothere()
Call to private method P::HID() from global scope
"
    );
}

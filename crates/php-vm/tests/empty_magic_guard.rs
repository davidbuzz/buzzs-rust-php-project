// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `empty()` of an inaccessible property on a class with `__isset()` and
//! `__get()` (`zend_std_has_property`, `Zend/zend_object_handlers.c`):
//! the property's `IN_ISSET` guard stays set while `__get()` runs, so an
//! `isset()`, `??` or `empty()` of the same property inside that `__get()`
//! does not call `__isset()` again. Expected output from
//! `/usr/bin/php8.5 -n`.
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
fn empty_calls_get_inside_the_isset_guard() {
    assert_eq!(
        run(r###"<?php
class M { private $data = ['a' => 1, 'z' => 0];
  function __get($n) { echo "get($n) "; $q = isset($this->$n); return $this->$n ?? ($this->data[$n] ?? null); }
  function __isset($n) { echo "isset($n) "; return isset($this->data[$n]); }
}
class W { public $log = [];
  function __get($n) { echo "get($n) "; return empty($this->$n) ? 'e' : 'f'; }
  function __isset($n) { echo "isset($n) "; return $n !== 'no'; }
}
function probe($m, $w) {
  for ($i = 0; $i < 3; $i++) {
    var_dump(empty($m->a), empty($m->z), empty($m->none), isset($m->a), $m->a ?? 'd');
    var_dump(empty($w->x), empty($w->no), $w->x);
  }
}
probe(new M, new W);
"###),
        r###"isset(a) get(a) isset(z) get(z) isset(none) isset(a) isset(a) get(a) isset(a) isset(a) bool(false)
bool(true)
bool(true)
bool(true)
int(1)
isset(x) get(x) isset(no) get(x) isset(x) bool(false)
bool(true)
string(1) "e"
isset(a) get(a) isset(z) get(z) isset(none) isset(a) isset(a) get(a) isset(a) isset(a) bool(false)
bool(true)
bool(true)
bool(true)
int(1)
isset(x) get(x) isset(no) get(x) isset(x) bool(false)
bool(true)
string(1) "e"
isset(a) get(a) isset(z) get(z) isset(none) isset(a) isset(a) get(a) isset(a) isset(a) bool(false)
bool(true)
bool(true)
bool(true)
int(1)
isset(x) get(x) isset(no) get(x) isset(x) bool(false)
bool(true)
string(1) "e"
"###
    );
}

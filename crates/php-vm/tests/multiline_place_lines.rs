// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A property or element chain read over several lines (`$o->a\n->b`,
//! `$a['x']\n['y']`): php-src emits one FETCH opline per segment, on the
//! line the compiler was on after the segment's name or offset
//! (`zend_delayed_compile_prop`, `zend_delayed_compile_dim`,
//! Zend/zend_compile.c), so each segment's warning names its own line.
//! Chains from a call, through a nullsafe operator and with a variable
//! offset too; a chain on one line keeps one line. Expected output from
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
fn each_segment_of_a_read_chain_reports_its_own_line() {
    assert_eq!(
        run(r###"<?php
set_error_handler(function ($no, $msg, $f, $l) { echo "$msg @$l\n"; return true; });
function mk() { return new stdClass; }
function f($o, $arr, $k) {
  $v = $o->missing
      ->deeper;
  $z = $arr['a']
     ['b'];
  $w = $o->p['x']
      ->q
      [$k];
  $c = mk()
      ->none
      ->more;
  $n = $o?->absent
      ->gone;
  $same = $o->one->two;
  return [$v, $z, $w, $c, $n, $same];
}
for ($i = 0; $i < 3; $i++) { f(new stdClass, [], 'key'); }
"###),
        r###"Undefined property: stdClass::$missing @5
Attempt to read property "deeper" on null @6
Undefined array key "a" @7
Trying to access array offset on null @8
Undefined property: stdClass::$p @9
Trying to access array offset on null @9
Attempt to read property "q" on null @10
Trying to access array offset on null @11
Undefined property: stdClass::$none @13
Attempt to read property "more" on null @14
Undefined property: stdClass::$absent @15
Attempt to read property "gone" on null @16
Undefined property: stdClass::$one @17
Attempt to read property "two" on null @17
Undefined property: stdClass::$missing @5
Attempt to read property "deeper" on null @6
Undefined array key "a" @7
Trying to access array offset on null @8
Undefined property: stdClass::$p @9
Trying to access array offset on null @9
Attempt to read property "q" on null @10
Trying to access array offset on null @11
Undefined property: stdClass::$none @13
Attempt to read property "more" on null @14
Undefined property: stdClass::$absent @15
Attempt to read property "gone" on null @16
Undefined property: stdClass::$one @17
Attempt to read property "two" on null @17
Undefined property: stdClass::$missing @5
Attempt to read property "deeper" on null @6
Undefined array key "a" @7
Trying to access array offset on null @8
Undefined property: stdClass::$p @9
Trying to access array offset on null @9
Attempt to read property "q" on null @10
Trying to access array offset on null @11
Undefined property: stdClass::$none @13
Attempt to read property "more" on null @14
Undefined property: stdClass::$absent @15
Attempt to read property "gone" on null @16
Undefined property: stdClass::$one @17
Attempt to read property "two" on null @17
"###
    );
}

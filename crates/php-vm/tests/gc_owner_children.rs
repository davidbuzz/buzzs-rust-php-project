// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Possible roots from a freed object's properties (`zend_object_std_dtor`:
//! `i_zval_ptr_dtor` of every property, `Zend/zend_objects.c`, then
//! `gc_check_possible_root`, `Zend/zend_gc.c`): an object an owner held in
//! a property, in an array property or in a dynamic property, and that
//! stays referenced by a cycle of its own, is buffered when the owner is
//! freed and collected by the next automatic run. Built in place, no
//! variable or call frame ever releases a handle of the inner object, so
//! the owner's release is the only place that can buffer it: such
//! children were never collected before (`MediaWiki`'s test suite kept
//! 3.8 GB where php-src keeps 1.2 GB). Expected output: PHP 8.5.

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
fn children_of_a_freed_owner_become_possible_roots() {
    let out = run(r#"<?php
class N { public $me; public $x; public $arr = []; }
#[AllowDynamicProperties] class D { }
function report(string $what, int $runs0, int $collected0) {
    $s = gc_status();
    echo $what, ': ', $s['runs'] - $runs0, ' runs, ', $s['collected'] - $collected0, " collected\n";
}
foreach (['property', 'two levels', 'array property', 'nested arrays', 'dynamic property'] as $what) {
    $s = gc_status(); $runs = $s['runs']; $collected = $s['collected'];
    for ($i = 0; $i < 25000; $i++) {
        switch ($what) {
            // The cycle is built in place: no variable or frame ever
            // releases a handle of the inner object, only its owner does.
            case 'property': $o = new N; $o->x = new N; $o->x->me = $o->x; unset($o); break;
            case 'two levels': $o = new N; $o->x = new N; $o->x->x = new N; $o->x->x->me = $o->x->x; unset($o); break;
            case 'array property': $o = new N; $o->arr = [new N]; $o->arr[0]->me = $o->arr[0]; unset($o); break;
            case 'nested arrays': $o = new N; $o->arr = ['k' => ['j' => new N]]; $o->arr['k']['j']->me = $o->arr['k']['j']; unset($o); break;
            case 'dynamic property': $o = new D; $o->dyn = new N; $o->dyn->me = $o->dyn; unset($o); break;
        }
    }
    report($what, $runs, $collected);
    gc_collect_cycles();
}
"#);
    assert_eq!(
        out,
        r"property: 2 runs, 19999 collected
two levels: 2 runs, 19998 collected
array property: 2 runs, 19998 collected
nested arrays: 2 runs, 19998 collected
dynamic property: 2 runs, 19998 collected
"
    );
}

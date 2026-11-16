// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `zend_call_function`'s interrupt check after a callee that threw: the
//! interrupt handlers run in the caller's frame with the exception set
//! aside (php-src `pcntl_signal_dispatch_exception_pending_user_frame`).
//!
//! The interrupt flag is process-wide, so this binary holds one test only:
//! another VM polling the flag in parallel could take the interrupt.

use php_runtime::{Value, ZStr};
use php_vm::ext::{Args, VmResult};
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

/// `zend_test_raise_and_throw()`: requests an interrupt, then throws.
fn raise_and_throw(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    php_vm::interrupt::request_interrupt();
    Err(vm.throw("Exception", "Exception after raise()"))
}

/// An interrupt handler calling back into user code, like pcntl's
/// dispatcher.
fn on_interrupt(vm: &mut Vm) -> VmResult<()> {
    vm.call_value(&Value::Str(ZStr::from("report")), Vec::new())?;
    Ok(())
}

#[test]
fn interrupt_runs_in_the_caller_frame_after_a_throwing_tick_function() {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.register_function("raise_and_throw(): void", raise_and_throw);
    vm.register_interrupt_handler(on_interrupt);
    let _ = vm.run_script(
        br#"<?php
function report() {
    echo "interrupt in ", debug_backtrace()[1]['function'] ?? '-', "()\n";
}
function test() {
    try {
        declare(ticks=1) {
            register_tick_function('raise_and_throw');
        }
    } catch (Exception $e) {
        echo "caught: ", $e->getMessage(), "\n";
    }
    unregister_tick_function('raise_and_throw');
}
test();
"#,
        "t.php",
    );
    drop(vm);
    let out = String::from_utf8(sink.0.borrow().clone()).unwrap();
    assert_eq!(
        out,
        "interrupt in test()\ncaught: Exception after raise()\n"
    );
}

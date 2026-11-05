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

fn run(source: &str) -> (i32, String) {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    (status, String::from_utf8(output).expect("UTF-8 output"))
}

#[test]
fn suspend_in_force_closed_fiber_has_internal_closure_caller() {
    let (status, output) = run(r#"<?php
$fiber = new Fiber(function (): void {
    try {
        Fiber::suspend();
    } finally {
        Fiber::suspend();
    }
});
$fiber->start();
unset($fiber);
echo "unreachable\n";
"#);
    assert_ne!(status, 0, "{output}");
    assert!(
        output.contains("Fatal error: Uncaught FiberError: Cannot suspend in a force-closed fiber"),
        "{output}"
    );
    assert!(output.contains("): Fiber::suspend()\n"), "{output}");
    assert!(
        output.contains("#1 [internal function]: {closure:t.php:"),
        "{output}"
    );
    assert!(output.contains("#2 {main}\n"), "{output}");
    assert!(!output.contains("__destruct"), "{output}");
    assert!(!output.contains("unreachable"), "{output}");
}

#[test]
fn throw_during_fiber_destruction_has_internal_closure_caller() {
    let (status, output) = run(r#"<?php
$fiber = new Fiber(function () {
    try {
        Fiber::suspend();
    } finally {
        throw new Exception("Exception 2");
    }
});
$fiber->start();
unset($fiber);
throw new Exception("Exception 1");
"#);
    assert_ne!(status, 0, "{output}");
    assert!(
        output.contains("Fatal error: Uncaught Exception: Exception 2"),
        "{output}"
    );
    assert!(
        output.contains("#0 [internal function]: {closure:t.php:"),
        "{output}"
    );
    assert!(output.contains("#1 {main}\n"), "{output}");
    assert!(!output.contains("__destruct"), "{output}");
    assert!(!output.contains("Exception 1"), "{output}");
}

#[test]
fn ordinary_fiber_start_and_resume_keep_their_trace_callers() {
    let (status, output) = run(r#"<?php
function show_trace($exception) {
    $trace = $exception->getTrace();
    echo isset($trace[0]["file"]) ? "user:" : "internal:";
    echo $trace[1]["class"], "::", $trace[1]["function"], ":";
    echo isset($trace[1]["file"]) ? "user\n" : "internal\n";
}
$fiber = new Fiber(function () { throw new Exception("start"); });
try { $fiber->start(); } catch (Exception $e) { show_trace($e); }
$fiber = new Fiber(function () {
    Fiber::suspend();
    throw new Exception("resume");
});
$fiber->start();
try { $fiber->resume(); } catch (Exception $e) { show_trace($e); }
"#);
    assert_eq!(status, 0, "{output}");
    assert_eq!(
        output,
        "internal:Fiber::start:user\ninternal:Fiber::resume:user\n"
    );
}

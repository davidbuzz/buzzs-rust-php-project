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

/// Like [`run`], with `zend_test`'s `_ZendTestFiber` registered.
fn run_with_test_fiber(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    php_vm::ext::register_zend_test_fiber(&mut vm);
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

#[test]
fn fiber_suspended_from_inside_a_test_fiber_resumes_it() {
    // ext/zend_test/tests/fiber_test_04.phpt
    let output = run_with_test_fiber(
        r#"<?php
$fiber = new Fiber(function (): int {
    $test = new _ZendTestFiber(function (): void {
        $value = Fiber::suspend(1);
        var_dump($value);
        $value = _ZendTestFiber::suspend(3);
        var_dump($value);
        $value = Fiber::suspend(4);
        var_dump($value);
        _ZendTestFiber::suspend(5);
        echo "unreachable\n";
    });
    $value = $test->start();
    var_dump($value);
    var_dump($test->resume(2 * $value));
    return -1;
});
$value = $fiber->start();
var_dump($value);
$value = $fiber->resume(2 * $value);
var_dump($value);
$value = $fiber->resume(2 * $value);
var_dump($value);
var_dump($fiber->getReturn());
"#,
    );
    assert_eq!(
        output,
        "int(1)\nint(2)\nint(3)\nint(6)\nint(4)\nint(8)\nint(5)\nNULL\nint(-1)\n"
    );
}

#[test]
fn test_fiber_suspended_from_inside_a_fiber_and_pipe_to() {
    // ext/zend_test/tests/fiber_test_05.phpt and fiber_test_07.phpt
    let output = run_with_test_fiber(
        r"<?php
$test = new _ZendTestFiber(function (): void {
    $fiber = new Fiber(function (): int {
        $value = Fiber::suspend(1);
        var_dump($value);
        $value = _ZendTestFiber::suspend(3);
        var_dump($value);
        $value = Fiber::suspend(4);
        var_dump($value);
        return 2 * $value;
    });
    $value = $fiber->start();
    var_dump($value);
    $value = $fiber->resume(2 * $value);
    var_dump($value);
    $value = $fiber->resume(2 * $value);
    var_dump($value);
    var_dump($fiber->getReturn());
});
$value = $test->start();
var_dump($value);
$value = $test->resume(2 * $value);
$piped = new _ZendTestFiber(function (): int { return 7; });
$piped->pipeTo(function (int $result): void { var_dump($result); });
var_dump($piped->start());
",
    );
    assert_eq!(
        output,
        "int(1)\nint(2)\nint(3)\nint(6)\nint(4)\nint(8)\nNULL\nint(16)\nint(7)\nNULL\n"
    );
}

#[test]
fn test_fiber_backtraces_end_inside_the_fiber() {
    // ext/zend_test/tests/gh16230.phpt
    let output = run_with_test_fiber(
        r"<?php
function show() { var_dump(debug_backtrace()); }
$test = new _ZendTestFiber(function (): void { show(); });
$test->start();
",
    );
    // show() called from the closure, which has no caller (no file/line).
    assert!(output.starts_with("array(2) {"), "{output}");
    assert!(output.contains("string(4) \"show\""), "{output}");
    assert!(output.contains("{closure:t.php:3}"), "{output}");
    assert!(!output.contains("start"), "{output}");
    assert_eq!(output.matches("[\"file\"]").count(), 1, "{output}");
}

#[test]
fn backtraces_show_only_string_coercions_of_internal_arguments() {
    // zend_parse_arg_str_weak converts in place; zend_parse_arg_long_weak
    // does not (gh17797.phpt shows the deprecated NULL).
    let (_, output) = run(r#"<?php
set_error_handler(function () {
    $t = debug_backtrace();
    var_dump($t[1]["args"]);
    return true;
});
trigger_error(123, "512");
"#);
    assert_eq!(
        output,
        "array(2) {\n  [0]=>\n  string(3) \"123\"\n  [1]=>\n  string(3) \"512\"\n}\n"
    );
}

/// `Fiber::suspend()` clears the bottom frame's `prev_execute_data` and
/// destruction does not set it again: a fiber destroyed from inside nested
/// calls unwinds with a backtrace that ends inside the fiber.
#[test]
fn destroyed_fiber_backtrace_ends_inside_the_fiber() {
    let (status, output) = run(r#"<?php
function g() {
    $fiber = new Fiber(function () {
        try { Fiber::suspend(); } finally { echo (new Exception)->getTraceAsString(), "\n"; }
    });
    $fiber->start();
    unset($fiber);
}
function h() { g(); }
h();
"#);
    assert_eq!(status, 0, "{output}");
    assert_eq!(
        output,
        "#0 [internal function]: {closure:g():3}()\n#1 {main}\n"
    );
}

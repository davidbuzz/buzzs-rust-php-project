//! `zend_observer` semantics: begin/end pairing, unwinds, generators and the
//! internal enter/leave notifications around native handlers.

use php_vm::ext::{ObservedFrame, ObserverHooks};
use php_vm::{Vm, VmOptions};
use std::cell::RefCell;

thread_local! {
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn name(f: &ObservedFrame) -> String {
    f.function
        .as_ref()
        .map_or_else(|| "{main}".to_owned(), |n| n.to_string_lossy())
}

fn log(entry: String) {
    LOG.with(|l| l.borrow_mut().push(entry));
}

fn observe(source: &str, internal: bool) -> Vec<String> {
    LOG.with(|l| l.borrow_mut().clear());
    let mut vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
    let mut hooks = ObserverHooks {
        // Observe user code only; builtins are reported by enter/leave.
        init: Some(|_, f| f.filename.is_some()),
        begin: Some(|_, f| log(format!("begin {}", name(f)))),
        end: Some(|vm, f, ret| {
            let how = match (ret, vm.current_exception()) {
                (Some(_), _) => "return",
                (None, Some(_)) => "throw",
                (None, None) => "unwind",
            };
            log(format!("end {} {how}", name(f)));
        }),
        ..ObserverHooks::default()
    };
    if internal {
        hooks.internal_enter = Some(|_, f| log(format!("enter {}", name(f))));
        hooks.internal_leave = Some(|_, f, ret| {
            log(format!(
                "leave {} {}",
                name(f),
                if ret.is_some() { "ok" } else { "err" }
            ));
        });
    }
    vm.set_observer(hooks);
    vm.run_script(source.as_bytes(), "observer.php");
    drop(vm);
    LOG.with(|l| l.borrow().clone())
}

#[test]
fn begin_and_end_pair_for_returns_and_exceptions() {
    let log = observe(
        r"<?php
function ok() { return 1; }
function boom() { throw new Exception('x'); }
ok();
try { boom(); } catch (Exception $e) {}
",
        false,
    );
    assert_eq!(
        log,
        [
            "begin {main}",
            "begin ok",
            "end ok return",
            "begin boom",
            "end boom throw",
            "end {main} return",
        ]
    );
}

#[test]
fn generators_end_on_yield_and_begin_on_resume() {
    let log = observe(
        r"<?php
function gen() { yield 1; yield 2; }
foreach (gen() as $v) {}
",
        false,
    );
    let gen: Vec<_> = log.iter().filter(|e| e.contains("gen")).cloned().collect();
    assert_eq!(
        gen,
        [
            "begin gen",
            "end gen return",
            "begin gen",
            "end gen return",
            "begin gen",
            "end gen return",
        ]
    );
}

#[test]
fn internal_enter_precedes_argument_errors() {
    let log = observe(
        r#"<?php
strcmp("abc", "abd");
try { strcmp(); } catch (ArgumentCountError $e) {}
"#,
        true,
    );
    let internal: Vec<_> = log
        .iter()
        .filter(|e| e.starts_with("enter") || e.starts_with("leave"))
        .cloned()
        .collect();
    assert_eq!(
        internal,
        [
            "enter strcmp",
            "leave strcmp ok",
            "enter strcmp",
            "leave strcmp err"
        ]
    );
}

#[test]
fn no_observer_means_no_notifications() {
    LOG.with(|l| l.borrow_mut().clear());
    let mut vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
    vm.run_script(b"<?php function f() { return 1; } f();", "plain.php");
    drop(vm);
    assert!(LOG.with(|l| l.borrow().is_empty()));
}

#[test]
fn func_num_args_and_func_get_args_in_functions_are_opcodes() {
    let log = observe(
        r#"<?php
function f() { \func_num_args(); func_get_args(); strcmp("x", "y"); }
f(1);
try { func_num_args(); } catch (Error $e) {}
"#,
        true,
    );
    let internal: Vec<_> = log
        .iter()
        .filter(|e| e.starts_with("enter") || e.starts_with("leave"))
        .cloned()
        .collect();
    // Only the global-scope call is a real internal call.
    assert_eq!(
        internal,
        [
            "enter strcmp",
            "leave strcmp ok",
            "enter func_num_args",
            "leave func_num_args err"
        ]
    );
}

#[test]
fn partial_application_trampolines_observe_only_the_target() {
    let log = observe(
        r"<?php
function f($a, $b) { return $a + $b; }
$f = f(1, ...);
$f(2);
",
        true,
    );
    assert!(log.iter().any(|e| e == "begin f"), "{log:?}");
    assert!(!log.iter().any(|e| e.contains("func_num_args")), "{log:?}");
}

#[test]
fn engine_reads_of_the_sapi_header_state_are_not_observed() {
    LOG.with(|l| l.borrow_mut().clear());
    let mut vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
    vm.set_observer(ObserverHooks {
        init: Some(|_, _| true),
        begin: Some(|_, f| log(format!("begin {}", name(f)))),
        internal_enter: Some(|_, f| log(format!("enter {}", name(f)))),
        ..ObserverHooks::default()
    });
    assert!(!vm.response_headers_sent().expect("headers_sent"));
    drop(vm);
    assert!(LOG.with(|l| l.borrow().is_empty()));
}

#[test]
fn direct_call_user_func_and_strlen_are_opcodes() {
    let log = observe(
        r#"<?php
function cb() { return 1; }
call_user_func('cb');
call_user_func_array('cb', []);
strlen("x");
$f = 'call_user_func';
$f('cb');
"#,
        true,
    );
    let internal: Vec<_> = log
        .iter()
        .filter(|e| e.starts_with("enter") || e.starts_with("begin cb"))
        .cloned()
        .collect();
    // zend_compile_func_cuf()/cufa() call the callback directly; only the
    // dynamic call is a real internal call.
    assert_eq!(
        internal,
        ["begin cb", "begin cb", "enter call_user_func", "begin cb"]
    );
}

#[test]
fn fiber_switches_are_notified_before_each_switch() {
    use php_vm::ext::{FiberContext, FiberState};
    fn describe(c: &FiberContext) -> String {
        format!(
            "{}:{:?}{}",
            if c.is_fiber { "fiber" } else { "main" },
            c.status,
            if c.caller.is_some() { "+caller" } else { "" }
        )
    }
    LOG.with(|l| l.borrow_mut().clear());
    let mut vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
    vm.set_observer(ObserverHooks {
        init: Some(|_, _| false),
        fiber_init: Some(|_, c| log(format!("init {}", describe(c)))),
        fiber_switch: Some(|_, from, to| {
            log(format!("switch {} -> {}", describe(from), describe(to)));
        }),
        fiber_destroy: Some(|_, c| log(format!("destroy {}", describe(c)))),
        ..ObserverHooks::default()
    });
    vm.run_script(
        b"<?php $f = new Fiber(function () { Fiber::suspend(); }); $f->start(); $f->resume();",
        "fiber.php",
    );
    drop(vm);
    assert_eq!(
        LOG.with(|l| l.borrow().clone()),
        [
            "init fiber:Init",
            "switch main:Running -> fiber:Init+caller",
            "switch fiber:Running -> main:Running",
            "switch main:Running -> fiber:Suspended+caller",
            "switch fiber:Dead -> main:Running",
            "destroy fiber:Dead",
        ]
    );
}

#[test]
fn exception_to_string_calls_get_trace_as_string() {
    // ext/zend_test/tests/observer_exception_01.phpt
    let log = observe(
        r"<?php
$s = (string) new Exception('x');
",
        true,
    );
    let internal: Vec<_> = log
        .iter()
        .filter(|e| e.starts_with("enter"))
        .cloned()
        .collect();
    assert_eq!(
        internal,
        [
            "enter __construct",
            "enter __toString",
            "enter getTraceAsString"
        ]
    );
}

#[test]
fn declarations_are_notified_when_bound() {
    // ext/zend_test/tests/observer_declarations_01.phpt
    LOG.with(|l| l.borrow_mut().clear());
    let mut vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
    vm.set_observer(ObserverHooks {
        init: Some(|_, _| true),
        begin: Some(|_, f| log(format!("begin {}", name(f)))),
        function_declared: Some(|_, n| log(format!("function {}", n.to_string_lossy()))),
        class_linked: Some(|_, n| log(format!("class {}", n.to_string_lossy()))),
        ..ObserverHooks::default()
    });
    vm.run_script(
        b"<?php function Foo() {} class A {} class B extends A {}
if (true) { function nested() {} class C extends A {} }",
        "decl.php",
    );
    drop(vm);
    assert_eq!(
        LOG.with(|l| l.borrow().clone()),
        [
            "function foo",
            "class a",
            "class b",
            "begin {main}",
            "function nested",
            "class c",
        ]
    );
}

#[test]
fn delegating_generators_are_not_observed_while_the_leaf_runs() {
    // ext/zend_test/tests/observer_generator_03.phpt
    let log = observe(
        r"<?php
function inner() { yield 1; yield 2; }
function outer() { yield 0; yield from inner(); }
foreach (outer() as $v) {}
",
        false,
    );
    let begins: Vec<_> = log
        .iter()
        .filter(|e| e.starts_with("begin ") && (e.contains("inner") || e.contains("outer")))
        .cloned()
        .collect();
    // `yield 0`; `yield from` (outer ends at the delegation); inner's two
    // yields and its return; outer resumed to finish.
    assert_eq!(
        begins,
        [
            "begin outer",
            "begin outer",
            "begin inner",
            "begin inner",
            "begin inner",
            "begin outer",
        ]
    );
}

#[test]
fn begin_and_end_report_the_current_zend_opcode() {
    // ext/zend_test/tests/observer_opline_01.phpt
    LOG.with(|l| l.borrow_mut().clear());
    let mut vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
    vm.set_observer(ObserverHooks {
        init: Some(|_, f| f.filename.is_some()),
        begin: Some(|_, f| log(format!("begin {} {:?}", name(f), f.opcode))),
        end: Some(|_, f, _| log(format!("end {} {:?}", name(f), f.opcode))),
        ..ObserverHooks::default()
    });
    vm.run_script(b"<?php function foo() { echo 'x'; } foo();", "op.php");
    drop(vm);
    assert_eq!(
        LOG.with(|l| l.borrow().clone()),
        [
            "begin {main} Some(\"ZEND_INIT_FCALL\")",
            "begin foo Some(\"ZEND_ECHO\")",
            "end foo Some(\"ZEND_RETURN\")",
            "end {main} Some(\"ZEND_RETURN\")",
        ]
    );
}

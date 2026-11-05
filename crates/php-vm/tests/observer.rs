//! zend_observer semantics: begin/end pairing, unwinds, generators and the
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
        .map_or_else(|| "{main}".to_owned(), |n| n.to_string_lossy().to_string())
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
        r#"<?php
function ok() { return 1; }
function boom() { throw new Exception('x'); }
ok();
try { boom(); } catch (Exception $e) {}
"#,
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
        r#"<?php
function gen() { yield 1; yield 2; }
foreach (gen() as $v) {}
"#,
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
strlen("abc");
try { strlen(); } catch (ArgumentCountError $e) {}
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
            "enter strlen",
            "leave strlen ok",
            "enter strlen",
            "leave strlen err"
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

//! Built-in iterator interfaces, ArrayAccess and foreach regressions
//! (Zend/tests/{iterators,ArrayAccess,foreach}).
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

/// Runs `source` as `t.php`, returning the exit status and the output.
fn run_status(source: &str) -> (i32, String) {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    (status, String::from_utf8(output).expect("UTF-8 output"))
}

/// Runs `source`, asserting a clean exit, and returns its output.
#[allow(dead_code)]
fn run(source: &str) -> String {
    let (status, output) = run_status(source);
    assert_eq!(status, 0, "{output}");
    output
}

/// `zend_implement_aggregate` / `zend_implement_iterator` (bug48667_1/2).
#[test]
fn class_cannot_be_both_iterator_and_aggregate() {
    for ifaces in ["Iterator, IteratorAggregate", "IteratorAggregate, Iterator"] {
        let src = format!("<?php\n\nabstract class A implements {ifaces} {{ }}\n");
        let (status, out) = run_status(&src);
        assert_ne!(status, 0, "{out}");
        assert!(
            out.contains(
                "Fatal error: Class A cannot implement both Iterator and IteratorAggregate at the same time in t.php on line 3"
            ),
            "{out}"
        );
    }
    // Interfaces may extend both.
    let out = run("<?php\ninterface I extends Iterator, IteratorAggregate {}\necho \"ok\";\n");
    assert_eq!(out, "ok");
}

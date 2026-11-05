//! Regressions from assorted `Zend/tests` subdirectories (concat,
//! dynamic properties, generators, offsets, switch, unset, ...).
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

/// Runs `source` as `t.php` and returns its output.
fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let _ = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

/// `ZEND_ASSIGN_DIM_OP` with no offset appends one element and operates
/// on it in place (`bug32833`), also below another append, inside a
/// property and through a reference.
#[test]
fn compound_assignment_to_append_adds_one_element() {
    let out = run(r#"<?php
$t = [];
$t[] .= "ok";
echo count($t), $t[0], "\n";
$u = [];
$u[][] .= "x";
echo count($u), count($u[0]), $u[0][0], "\n";
$o = new stdClass;
$o->a = [5 => 1];
$o->a[] += 2;
echo count($o->a), $o->a[6], "\n";
$r = [];
$b = &$r;
$b[] .= "y";
echo count($r), $r[0], "\n";
"#);
    assert_eq!(out, "1ok\n11x\n22\n1y\n");
}

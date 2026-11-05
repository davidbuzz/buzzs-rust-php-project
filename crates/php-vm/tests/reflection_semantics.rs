//! End-to-end checks of reflection (and closely related engine builtins)
//! against the outputs of the php-src `ext/reflection` `.phpt` tests.

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

fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

/// bug49719.phpt / property_exists.phpt: a parent's private property is
/// not a property of the subclass.
#[test]
fn property_exists_ignores_inherited_private_properties() {
    assert_eq!(
        run(r"<?php
class A { private $a; private $c; }
class B extends A { private $b; private $c; }
class C extends B {}
var_dump(property_exists('B', 'a'), property_exists(new B, 'a'));
var_dump(property_exists('B', 'b'), property_exists('B', 'c'), property_exists('C', 'c'));
"),
        "bool(false)\nbool(false)\nbool(true)\nbool(true)\nbool(false)\n"
    );
}

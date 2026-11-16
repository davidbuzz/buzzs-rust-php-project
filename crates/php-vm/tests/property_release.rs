// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Objects released by user code during a typed-property assignment
//! (`zend_std_write_property`, GH-10169).

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

fn run(source: &str, ini: &[(&str, &str)]) -> String {
    let sink = Sink::default();
    let options = VmOptions {
        ini: ini
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
    };
    let mut vm = Vm::new(&options, Box::new(sink.clone()));
    let _status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8(out).expect("UTF-8 output")
}

const CLASSES: &str = r#"<?php
class A { public string $prop; }
class B {
    public function __toString() {
        global $a;
        $a = null;
        return 'x';
    }
}
"#;

#[test]
fn release_of_last_holder_throws() {
    let out = run(
        &format!(
            "{CLASSES}$a = new A();\ntry {{ $a->prop = new B(); }} catch (Error $e) {{ echo $e->getMessage(), \"\\n\"; }}\n$a = new A();\n$a->prop = '';\ntry {{ $a->prop = new B(); }} catch (Error $e) {{ echo $e->getMessage(), \"\\n\"; }}\n"
        ),
        &[],
    );
    assert_eq!(
        out,
        "Object was released while assigning to property A::$prop\n\
         Object was released while assigning to property A::$prop\n"
    );
}

#[test]
fn surviving_holder_assigns() {
    let out = run(
        &format!(
            "{CLASSES}$a = new A();\n$c = $a;\n$c->prop = new B();\nvar_dump($a, $c->prop);\n"
        ),
        &[],
    );
    assert_eq!(out, "NULL\nstring(1) \"x\"\n");
}

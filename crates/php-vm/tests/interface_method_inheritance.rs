//! Staticness of methods an interface inherits from several parents
//! (`do_interface_implementation` in Zend/zend_inheritance.c).

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

#[test]
fn static_and_instance_from_two_parents() {
    let out = run(
        "<?php\ninterface d {\n    static function B();\n}\ninterface c {\n    function b();\n}\nclass_alias('c', 'w');\ninterface a extends d, w { }\necho 'ok';\n",
        &[],
    );
    assert!(
        out.contains("Fatal error: Cannot make non static method c::B() static in class d in t.php on line 3"),
        "{out}"
    );
    assert!(!out.contains("ok"), "{out}");
}

#[test]
fn own_static_over_parent_instance() {
    let out = run(
        "<?php\ninterface X { function f(); }\ninterface Y extends X {\n    static function f();\n}\necho 'ok';\n",
        &[],
    );
    assert!(
        out.contains("Fatal error: Cannot make non static method X::f() static in class Y in t.php on line 4"),
        "{out}"
    );
}

#[test]
fn compatible_parents_link() {
    let out = run(
        "<?php\ninterface X { function f(); }\ninterface Z { function f(); }\ninterface Y extends X, Z {}\ninterface P extends X {}\ninterface Q extends X {}\ninterface R extends P, Q {}\nclass C implements R, Y { function f() {} }\necho 'ok';\n",
        &[],
    );
    assert_eq!(out, "ok");
}

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

#[test]
fn instance_visibility_precedes_static_storage_lookup() {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(
        br#"<?php
class A {
    private $pri;
    protected $pro;
    public $pub;
    static function inside() { return self::$pri; }
}
foreach (['pri', 'pro', 'pub', 'missing'] as $name) {
    try { A::$$name; } catch (Error $e) { echo $e->getMessage(), "\n"; }
}
try { A::inside(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#,
        "visibility.php",
    );
    assert_eq!(
        sink.0.borrow().as_slice(),
        b"Cannot access private property A::$pri\nCannot access protected property A::$pro\nAccess to undeclared static property A::$pub\nAccess to undeclared static property A::$missing\nAccess to undeclared static property A::$pri\n"
    );
}

//! Script-level tests of typed references (expected output from
//! `/usr/bin/php -n`).

use crate::{Vm, VmOptions};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run(src: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(src.as_bytes(), "t.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn coercion_possible_rules() {
    use super::{coercion_possible, Assignable};
    use crate::types::mask;
    use php_runtime::Value;
    assert_eq!(coercion_possible(mask::DOUBLE, &Value::Int(1), true), Assignable::Coerce);
    assert_eq!(coercion_possible(mask::LONG, &Value::Float(1.0), true), Assignable::No);
    assert_eq!(coercion_possible(mask::LONG, &Value::Null, false), Assignable::No);
    assert_eq!(coercion_possible(mask::ARRAY, &Value::Int(1), false), Assignable::No);
    assert_eq!(coercion_possible(mask::BOOL, &Value::Int(1), false), Assignable::Coerce);
    assert_eq!(coercion_possible(mask::TRUE, &Value::Int(1), false), Assignable::No);
    assert_eq!(coercion_possible(mask::STRING, &Value::Int(1), false), Assignable::Coerce);
}

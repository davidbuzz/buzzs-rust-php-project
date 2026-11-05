//! `SplFixedArray`'s fallible element allocation (`safe_femalloc(...,
//! fallible)` + `RETURN_OOM()`). One test: the pending violation is
//! process-wide.

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

/// `oom_SplFixedArray_fromArray_001.phpt`: the block is refused before it is
/// allocated and the fatal follows the call.
#[test]
fn from_array_refuses_a_block_over_the_limit() {
    let sink = Sink::default();
    let options = VmOptions {
        ini: vec![
            ("display_errors".into(), "1".into()),
            ("memory_limit".into(), "10M".into()),
        ],
    };
    let mut vm = Vm::with_extensions(&options, Box::new(sink.clone()), &[&php_ext_spl::Spl]);
    let status = vm.run_script(
        b"<?php\necho \"start\\n\";\nvar_dump(SplFixedArray::fromArray([10*1024*1024 => 0], true));\necho \"not reached\\n\";\n",
        "test.php",
    );
    drop(vm);
    let out = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 255, "{out}");
    assert!(
        out.starts_with("start\n\nFatal error: Allowed memory size of 10485760 bytes exhausted (tried to allocate "),
        "{out}"
    );
    assert!(out.ends_with(" bytes) in test.php on line 3\n"), "{out}");
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `SplFixedArray`'s fallible element allocation (`safe_femalloc(...,
//! fallible)` + `RETURN_OOM()`). One test function: the pending violation
//! is process-wide.

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
            // As run-tests.php runs the .phpt tests: no fatal error backtrace.
            ("fatal_error_backtraces".into(), "0".into()),
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

    // `oom_SplFixedArray___wakeup.phpt`: `__wakeup()` allocates the elements
    // for the property table before copying it (`spl_fixedarray_init`).
    let sink = Sink::default();
    let options = VmOptions {
        ini: vec![
            ("display_errors".into(), "1".into()),
            ("memory_limit".into(), "4M".into()),
            ("fatal_error_backtraces".into(), "0".into()),
        ],
    };
    let mut vm = Vm::with_extensions(&options, Box::new(sink.clone()), &[&php_ext_spl::Spl]);
    let status = vm.run_script(
        b"<?php\n$a = new SplFixedArray();\nfor ($i = 0; $i < 200000; $i++) { @$a->{$i} = 0; }\necho \"filled\\n\";\n$a->__wakeup();\necho \"not reached\\n\";\n",
        "test.php",
    );
    drop(vm);
    let out = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 255, "{out}");
    assert!(out.starts_with("filled\n"), "{out}");
    assert!(
        out.contains(
            "\nFatal error: Allowed memory size of 4194304 bytes exhausted (tried to allocate "
        ),
        "{out}"
    );
    assert!(out.ends_with(" bytes) in test.php on line 5\n"), "{out}");
}

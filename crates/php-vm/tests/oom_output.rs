//! A `memory_limit` fatal discards the output buffers without calling
//! user handlers (`php_error_cb`, GH-11189), with the real allocator.
//! One test: the counters and the pending violation are process-wide.

use php_vm::{Vm, VmOptions};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

php_memlimit::install_global_allocator!();

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

fn run(source: &str, memory_limit: &str) -> String {
    let sink = Sink::default();
    let options = VmOptions {
        ini: [
            ("display_errors".to_string(), "1".to_string()),
            ("memory_limit".to_string(), memory_limit.to_string()),
        ]
        .into_iter()
        .collect(),
    };
    let mut vm = Vm::new(&options, Box::new(sink.clone()));
    let _status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8(out).expect("UTF-8 output")
}

#[test]
fn oom_discards_buffers_without_calling_handlers() {
    let out = run(
        r#"<?php
register_shutdown_function(function () {
    echo $GLOBALS['called'] ? "called\n" : "not called\n";
});
$called = false;
ob_start(function ($buf) { $GLOBALS['called'] = true; return $buf; });
echo "buffered\n";
$a = [];
while (1) { $a[] = 1; }
"#,
        "8M",
    );
    assert!(
        out.starts_with(
            "\nFatal error: Allowed memory size of 8388608 bytes exhausted (tried to allocate "
        ),
        "{out}"
    );
    assert!(out.ends_with(" in t.php on line 9\nnot called\n"), "{out}");
    assert!(!out.contains("buffered"), "{out}");
}

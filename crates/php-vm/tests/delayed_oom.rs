//! Delayed `memory_limit` fatals (`zend_mm_delayed_error` /
//! `zend_handle_delayed_oom`) with the real request allocator installed.
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
            // As run-tests.php runs the .phpt tests these port.
            ("fatal_error_backtraces".to_string(), "0".to_string()),
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
fn delayed_oom_is_raised_at_interrupt_points() {
    // oom_after_main_script.phpt: the plain instructions after the
    // allocation that crossed the limit still run; the fatal names the
    // line of the allocation and shutdown functions run afterwards.
    let out = run(
        r#"<?php
register_shutdown_function(function () { echo "Shutdown\n"; });
function test() {
    $a = 'aaaaaaaaaaaaaaaa'; for ($i = 0; $i < 18; $i++) { $a .= $a; }
    $b = $a . $a;
    echo "Done\n";
}
test();
echo "not reached\n";
"#,
        "8M",
    );
    assert!(out.starts_with("Done\n\nFatal error: Allowed memory size of 8388608 bytes exhausted (tried to allocate "), "{out}");
    assert!(
        out.ends_with(" bytes) in t.php on line 5\nShutdown\n"),
        "{out}"
    );

    // oom_unserialize_*.phpt: unserialize() stops at the element that
    // crossed the limit (a complete parse would cross the hard limit) and
    // reports nothing itself; the fatal follows the call.
    let out = run(
        r#"<?php
$s = 'a:150000:{';
for ($j = 0; $j < 150; $j++) {
    $c = '';
    for ($i = 0; $i < 1000; $i++) { $k = $j * 1000 + $i; $c .= "i:$k;i:$k;"; }
    $s .= $c;
}
$s .= '}';
ini_set('memory_limit', '8M');
var_dump(unserialize($s));
echo "not reached\n";
"#,
        "128M",
    );
    assert!(
        out.starts_with(
            "\nFatal error: Allowed memory size of 8388608 bytes exhausted (tried to allocate "
        ),
        "{out}"
    );
    assert!(out.ends_with(" bytes) in t.php on line 10\n"), "{out}");
}

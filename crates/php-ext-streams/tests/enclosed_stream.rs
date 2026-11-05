//! `php://temp` and `data:` streams enclose a memory stream that has a
//! resource of its own (`php_stream_temp_create()`), which `fclose()`
//! refuses (GH-21221).

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use php_vm::ext::Extension;
use php_vm::{Vm, VmOptions};

/// A `Write` sink shared with the test.
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

/// The VM interrupt flag is process-wide: this binary's VMs run one at a
/// time.
static VM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn run(code: &str) -> String {
    let _serial = VM_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let sink = Sink::default();
    let streams = php_ext_streams::Streams;
    let exts: [&dyn Extension; 1] = [&streams];
    let opts = VmOptions { ini: vec![] };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn enclosed_stream_cannot_be_closed() {
    let out = run(r#"
$f = fopen('php://temp', 'r+b');
foreach (get_resources() as $h => $inner) {}
var_dump($h === (int) $f + 1);
var_dump(fclose($inner));
fwrite($f, "abc");
fclose($f);
var_dump(get_resource_type($inner));
"#);
    assert_eq!(
        out,
        "bool(true)\n\nWarning: fclose(): cannot close the provided stream, as it must not be \
         manually closed in test.php on line 5\nbool(false)\nstring(7) \"Unknown\"\n"
    );
}

#[test]
fn closed_temp_streams_leave_no_resources() {
    let out = run(r#"
function n() { $n = 0; foreach (get_resources() as $_) { $n++; } return $n; }
fclose(fopen("php://temp", "w+"));
$count = n();
fclose(fopen("php://temp", "w+"));
fclose(fopen("data://text/plain,abc", "r"));
var_dump(n() == $count);
"#);
    assert_eq!(out, "bool(true)\n");
}

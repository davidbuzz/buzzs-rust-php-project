//! End-to-end tests: PHP scripts run on a VM with the `exif` extension
//! (which also provides ext/standard's image functions).

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

/// Runs `code` (without `<?php`) and returns its output.
fn run(code: &str) -> String {
    let sink = Sink::default();
    let ext = php_ext_exif::Exif;
    let exts: [&dyn Extension; 1] = [&ext];
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

/// `zend_try_array_init()`: the by-reference variable is `null` while the
/// destructor of its old value runs (GH-17162).
#[test]
fn image_info_is_null_during_old_value_destructor() {
    let out = run(r#"
error_reporting(0);
class T { function __destruct() { global $box; echo gettype($box), "\n"; } }
$box = [new T];
getimagesizefromstring("x", $box);
echo gettype($box), " ", $box === [] ? 0 : -1, "\n";
"#);
    assert_eq!(out, "NULL\narray 0\n");
}

#[test]
fn throwing_destructor_aborts_the_call() {
    let out = run(r#"
class T { function __destruct() { global $box; $box->value = null; } }
$box = [new T];
try {
    getimagesizefromstring("x", $box);
} catch (Throwable $e) {
    echo $e::class, ': ', $e->getMessage(), "\n";
}
"#);
    assert_eq!(out, "Error: Attempt to assign property \"value\" on null\n");
}

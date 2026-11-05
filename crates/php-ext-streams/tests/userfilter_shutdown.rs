//! `PHP_RSHUTDOWN(user_filters)` destroys `BG(user_filter_map)` before the
//! resource list is closed: a user filter appended from a `filter()`
//! callback running while the remaining streams close cannot be created
//! (GH-22449, GH-22818) instead of recursing.

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

const FILTER_CLASS: &str = r#"
class rot extends php_user_filter {
    public function filter($in, $out, &$consumed, $closing): int {
        $s = fopen('php://memory', 'w+');
        REREGISTER
        stream_filter_append($s, "rot");
        return PSFS_PASS_ON;
    }
}
stream_filter_register("rot", rot::class);
$s = fopen('php://memory', 'w+');
stream_filter_append($s, "rot");
echo "done\n";
"#;

fn assert_single_failure(out: &str) {
    assert!(out.starts_with("done\n"), "{out}");
    assert_eq!(
        out.matches("stream_filter_append(): Unable to create or locate filter \"rot\"")
            .count(),
        1,
        "{out}"
    );
    assert!(!out.contains("depth exceeded"), "{out}");
}

#[test]
fn user_filter_appended_at_shutdown_is_not_created() {
    assert_single_failure(&run(&FILTER_CLASS.replace("REREGISTER", "")));
}

#[test]
fn reregistering_at_shutdown_does_not_revive_the_filter() {
    assert_single_failure(&run(
        &FILTER_CLASS.replace("REREGISTER", "stream_filter_register(\"rot\", rot::class);")
    ));
}

#[test]
fn user_filters_still_work_during_the_request() {
    let out = run(r#"
class up extends php_user_filter {
    public function filter($in, $out, &$consumed, $closing): int {
        while ($b = stream_bucket_make_writeable($in)) {
            $b->data = "<" . $b->data . ">";
            $consumed += $b->datalen;
            stream_bucket_append($out, $b);
        }
        return PSFS_PASS_ON;
    }
}
stream_filter_register("up", up::class);
$s = fopen('php://memory', 'w+');
stream_filter_append($s, "up", STREAM_FILTER_WRITE);
fwrite($s, "abc");
rewind($s);
echo stream_get_contents($s);
"#);
    assert_eq!(out, "<abc>");
}

//! `error_include_args` origins and master's `require` failure Error
//! (`php_verror`, `php_message_handler_for_zend`).

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
    String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output")
}

const MISSING: &str = "/nonexistent-phprust-dir/missing.php";

#[test]
fn include_origin_without_args() {
    let out = run(&format!("<?php include '{MISSING}'; echo 'done';"), &[]);
    assert!(
        out.contains("Warning: include(): Failed to open stream: No such file or directory"),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "Warning: include(): Failed opening '{MISSING}' for inclusion"
        )),
        "{out}"
    );
    assert!(out.ends_with("done"), "{out}");
}

#[test]
fn include_origin_with_args() {
    let out = run(
        "<?php $f = 'no_such_file'; include $f;",
        &[("error_include_args", "1")],
    );
    assert!(
        out.contains("Warning: include('no_such_file'): Failed to open stream"),
        "{out}"
    );
    assert!(
        out.contains("Warning: include('no_such_file'): Failed opening 'no_such_file'"),
        "{out}"
    );
}

#[test]
fn require_failure_throws_error() {
    let out = run(
        &format!(
            "<?php try {{ require '{MISSING}'; }} catch (Error $e) {{ echo get_class($e), ': ', $e->getMessage(); }}"
        ),
        &[],
    );
    assert!(
        out.contains("Warning: require(): Failed to open stream"),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "Error: Failed opening required '{MISSING}' (include_path="
        )),
        "{out}"
    );
}

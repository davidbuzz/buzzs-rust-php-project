//! `ignore_repeated_errors` / `ignore_repeated_source` (`php_error_cb`
//! in main/main.c).

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
    let out = sink.0.borrow().clone();
    String::from_utf8(out).expect("UTF-8 output")
}

const SCRIPT: &str = "<?php\n$u1 + $u2;\n$u + $u;\n$u + 1;\n";

fn count(out: &str, needle: &str) -> usize {
    out.matches(needle).count()
}

#[test]
fn repeats_shown_by_default() {
    let out = run(SCRIPT, &[]);
    assert_eq!(count(&out, "Undefined variable $u in"), 3, "{out}");
}

#[test]
fn same_line_repeats_ignored() {
    let out = run(SCRIPT, &[("ignore_repeated_errors", "1")]);
    assert_eq!(count(&out, "Undefined variable $u1 in"), 1, "{out}");
    assert_eq!(count(&out, "Undefined variable $u2 in"), 1, "{out}");
    assert_eq!(count(&out, "Undefined variable $u in t.php on line 3"), 1, "{out}");
    assert_eq!(count(&out, "Undefined variable $u in t.php on line 4"), 1, "{out}");
}

#[test]
fn any_source_repeats_ignored() {
    let out = run(
        SCRIPT,
        &[("ignore_repeated_errors", "1"), ("ignore_repeated_source", "1")],
    );
    assert_eq!(count(&out, "Undefined variable $u in"), 1, "{out}");
}

#[test]
fn error_clear_last_resets_repeat_check() {
    let out = run(
        "<?php\nfor ($i = 0; $i < 2; $i++) { $u; error_clear_last(); }\n",
        &[("ignore_repeated_errors", "1")],
    );
    assert_eq!(count(&out, "Undefined variable $u in"), 2, "{out}");
}

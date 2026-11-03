//! End-to-end tests: PHP scripts run on a VM with the `filter` extension.
//! Expected values come from php-src (`/usr/bin/php`).

const EXT: php_ext_filter::Filter = php_ext_filter::Filter;

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
    let ext = EXT;
    let exts: [&dyn Extension; 1] = [&ext];
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn validate_and_sanitize() {
    assert_eq!(
        run(
            r"var_dump(filter_var('123', FILTER_VALIDATE_INT), filter_var('abc', FILTER_VALIDATE_INT, FILTER_NULL_ON_FAILURE),
            filter_var('a@b.co', FILTER_VALIDATE_EMAIL), filter_var('<b>', FILTER_SANITIZE_SPECIAL_CHARS));"
        ),
        "int(123)\nNULL\nstring(6) \"a@b.co\"\nstring(11) \"&#60;b&#62;\"\n"
    );
}

#[test]
fn regexp_and_callback() {
    assert_eq!(
        run(
            r"var_dump(filter_var('abc', FILTER_VALIDATE_REGEXP, ['options' => ['regexp' => '/^a/']]),
            filter_var('abc', FILTER_CALLBACK, ['options' => fn ($s) => strrev($s)]));"
        ),
        "string(3) \"abc\"\nstring(3) \"cba\"\n"
    );
}

#[test]
fn arrays() {
    assert_eq!(
        run(
            r"var_dump(filter_var_array(['a' => '1', 'b' => 'x'], ['a' => FILTER_VALIDATE_INT, 'b' => FILTER_VALIDATE_INT, 'c' => FILTER_DEFAULT]));"
        ),
        "array(3) {\n  [\"a\"]=>\n  int(1)\n  [\"b\"]=>\n  bool(false)\n  [\"c\"]=>\n  NULL\n}\n"
    );
}

#[test]
fn lists_and_inputs() {
    assert_eq!(
        run(
            r"var_dump(filter_id('int'), filter_list()[20], filter_input(INPUT_GET, 'x'), filter_has_var(INPUT_GET, 'x'));"
        ),
        "int(257)\nstring(8) \"callback\"\nNULL\nbool(false)\n"
    );
}

#[test]
fn throw_on_failure() {
    assert_eq!(
        run(
            r"try { filter_var('x', FILTER_VALIDATE_INT, FILTER_THROW_ON_FAILURE); }
            catch (Filter\FilterFailedException $e) { echo get_class($e), ': ', $e->getMessage(); }"
        ),
        "Filter\\FilterFailedException: filter validation failed: filter int not satisfied by 'x'"
    );
}

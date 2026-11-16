// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `assert()` with `assert.bail` after the callback threw
//! (`ext/standard/assert.c`): `zend_exception_error(EG(exception),
//! E_WARNING)` reports the exception through `zend_error_va`, i.e. straight
//! to the error callback, so a user error handler sees neither the
//! "Uncaught ..." warning nor the one about a throwing `__toString()`; an
//! exception that `__toString()` threw stays pending and `assert()` throws
//! it instead of unwinding with `exit`. Expected output derived from
//! php-src master (`Zend/zend_exceptions.c`, `ext/standard/assert.c`;
//! PHP 8.3 handles a throwing callback differently). The settings are
//! startup INI values, which the bare VM takes without ext/standard's
//! "INI setting is deprecated" notices.
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

/// Runs `source` as `t.php` with startup INI values; returns its output.
fn run_with(source: &str, ini: &[(&str, &str)]) -> String {
    let sink = Sink::default();
    let opts = VmOptions {
        ini: ini
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
    };
    let mut vm = Vm::new(&opts, Box::new(sink.clone()));
    let _ = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

#[test]
fn bail_reports_the_callbacks_exception_past_user_error_handlers() {
    let src = r#"<?php
class Shown extends Exception { public function __toString(): string { return "Shown!"; } }
class Boom extends Exception { public function __toString(): string { throw new LogicException("inner"); } }
function cb() { static $n = 0; if (++$n === 1) { throw new Boom("boom"); } throw new Shown("shown"); }
set_error_handler(function ($no, $str) { echo "handler: $no $str\n"; return true; });
register_shutdown_function(function () { echo "shutdown\n"; });
try { assert(false); } catch (LogicException $e) { echo "caught ", $e->getMessage(), "\n"; }
assert(false);
echo "not reached\n";
"#;
    let ini = [
        ("assert.callback", "cb"),
        ("assert.exception", "0"),
        ("assert.warning", "0"),
        ("assert.bail", "1"),
    ];
    assert_eq!(
        run_with(src, &ini),
        "\nWarning: Uncaught LogicException in exception handling during call to \
         Boom::__toString() in t.php on line 3\n\
         \nWarning: Uncaught \n  thrown in t.php on line 4\n\
         caught inner\n\
         \nWarning: Uncaught Shown!\n  thrown in t.php on line 4\n\
         shutdown\n"
    );
}

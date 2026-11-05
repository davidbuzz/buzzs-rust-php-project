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
    let out = sink.0.borrow().clone();
    String::from_utf8(out).expect("UTF-8 output")
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

/// `PHP_FUNCTION(assert)` (`ext/standard/assert.c`): dynamic calls honour
/// `zend.assertions` and `assert.active` (php-src
/// `ext/standard/tests/assert/gh18509.phpt`, `assert_basic1.phpt`).
#[test]
fn assert_dynamic_call_and_active() {
    let src = "<?php $c = 'assert'; var_dump($c(false));";
    assert_eq!(run(src, &[("zend.assertions", "0")]), "bool(true)\n");
    let src = "<?php var_dump(assert(false));";
    assert_eq!(run(src, &[("assert.active", "0")]), "bool(true)\n");
}

/// The `AssertionError` code is `E_ERROR`; the startup `assert.callback`
/// gets file, line, `null` and the description; an invalid callback
/// throws "Invalid callback" (`zend_call_function`).
#[test]
fn assert_callback_and_exception() {
    let src = "<?php try { assert(false, 'm'); } catch (AssertionError $e) { echo $e->getMessage(), $e->getCode(); }";
    assert_eq!(run(src, &[]), "m1");
    let src = "<?php function cb($f, $l, $u, $d) { var_dump($f, $l, $u, $d); }\nvar_dump(assert(false, 'd'));";
    assert_eq!(
        run(
            src,
            &[
                ("assert.callback", "cb"),
                ("assert.exception", "0"),
                ("display_errors", "1"),
            ]
        ),
        "string(5) \"t.php\"\nint(2)\nNULL\nstring(1) \"d\"\n\nWarning: assert(): d failed in t.php on line 2\nbool(false)\n"
    );
    let src = "<?php try { var_dump(assert(false)); } catch (Error $e) { echo $e->getMessage(); }";
    assert_eq!(
        run(
            src,
            &[
                ("assert.callback", "nope"),
                ("assert.exception", "0"),
                ("assert.warning", "0"),
            ]
        ),
        "Invalid callback nope, function \"nope\" not found or invalid function name"
    );
}

/// `ini_set()` takes any scalar even under `strict_types` and rejects
/// other types itself (`Z_TYPE_P(new_value) > IS_STRING`, php-src
/// `ext/standard/tests/general_functions/ini_set_types.phpt`).
#[test]
fn ini_set_value_types() {
    let src = "<?php declare(strict_types=1);\nini_set('precision', 6); echo ini_get('precision'), \"\\n\";\ntry { ini_set('foo', []); } catch (TypeError $e) { echo $e->getMessage(); }";
    assert_eq!(
        run(src, &[]),
        "6\nini_set(): Argument #2 ($value) must be of type string|int|float|bool|null"
    );
}

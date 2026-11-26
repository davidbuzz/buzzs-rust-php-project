// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! INI settings read as integers (`ZEND_ATOL()`: decimal only, PR #8) and
//! printed through `(int) EG(precision)`, against PHP 8.5:
//! `php -n -d precision=V -r 'echo 1/3;'` and
//! `php -n -d serialize_precision=V -r 'var_dump(0.1);'`. The php-src suite
//! does not cover these values.

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

fn run_with(name: &str, value: &str, source: &str) -> String {
    let sink = Sink::default();
    let opts = VmOptions {
        ini: vec![(name.to_string(), value.to_string())],
    };
    let mut vm = Vm::new(&opts, Box::new(sink.clone()));
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8(out).expect("UTF-8 output")
}

#[test]
fn precision_values_print_as_in_php_8_5() {
    for (value, want) in [
        ("0x5", "0.3"),
        ("  7", "0.3333333"),
        ("7abc", "0.3333333"),
        ("+3", "0.333"),
        ("-1", "0.3333333333333333"),
        ("-2", "0.33333333333333"),
        ("", "0.3"),
        ("abc", "0.3"),
        ("0b11", "0.3"),
        ("010", "0.3333333333"),
        ("5.9", "0.33333"),
        ("99999999999999999999", "0.3333333333333333"),
        ("4294967296", "0.3"),
        ("2147483648", "0.3333333333333333"),
    ] {
        assert_eq!(
            run_with("precision", value, "<?php echo 1/3;"),
            want,
            "precision={value:?}"
        );
    }
}

#[test]
fn serialize_precision_values_serialize_as_in_php_8_5() {
    for (value, want) in [
        ("99999999999999999999", "d:0.1;"),
        ("4294967296", "d:0.1;"),
        ("17", "d:0.10000000000000001;"),
    ] {
        assert_eq!(
            run_with("serialize_precision", value, "<?php echo serialize(0.1);"),
            want,
            "serialize_precision={value:?}"
        );
    }
}

#[test]
fn serialize_precision_values_dump_as_in_php_8_5() {
    for (value, want) in [
        ("99999999999999999999", "float(0.1)\n"),
        ("4294967296", "float(0.1)\n"),
        ("17", "float(0.10000000000000001)\n"),
    ] {
        assert_eq!(
            run_with("serialize_precision", value, "<?php var_dump(0.1);"),
            want,
            "serialize_precision={value:?}"
        );
    }
}

/// `assert()` reads `zend.assertions` and `assert.active` once per change
/// of the INI values: `ini_set()` of either, back and forth, takes effect
/// at the next assertion, its check (`ZEND_ASSERT_CHECK`) included.
/// Expected output: php-src's (PHP 8.4 CLI).
#[test]
fn assert_follows_ini_changes() {
    let src = r#"<?php
function t($label) {
    for ($i = 0; $i < 2; $i++) {
        try { assert($i > 5, "custom $label $i"); echo "$label: passed $i\n"; } catch (AssertionError $e) { echo "$label: ", $e->getMessage(), "\n"; }
        try { $r = assert(false); echo "$label r=", var_export($r, true), "\n"; } catch (AssertionError $e) { echo "$label: ", $e->getMessage(), "\n"; }
    }
}
t('on');
ini_set('zend.assertions', 0); t('off');
ini_set('zend.assertions', 1); t('on-again');
@ini_set('assert.active', 0); t('inactive');
@ini_set('assert.active', 1); t('active');
var_dump(ini_get('zend.assertions'), ini_get('assert.active'));
"#;
    let expected = r#"on: custom on 0
on: assert(false)
on: custom on 1
on: assert(false)
off: passed 0
off r=true
off: passed 1
off r=true
on-again: custom on-again 0
on-again: assert(false)
on-again: custom on-again 1
on-again: assert(false)
inactive: passed 0
inactive r=true
inactive: passed 1
inactive r=true
active: custom active 0
active: assert(false)
active: custom active 1
active: assert(false)
string(1) "1"
string(1) "1"
"#;
    assert_eq!(run_with("zend.assertions", "1", src), expected);
}

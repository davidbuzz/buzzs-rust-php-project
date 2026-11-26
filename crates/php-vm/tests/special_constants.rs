// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `TRUE`, `FALSE` and `NULL` as constants looked up by name at run time
//! (`zend_register_standard_constants`, `zend_get_special_const`,
//! `Zend/zend_constants.c`): `defined()` and `constant()` find them in any
//! case, with or without a leading backslash, and
//! `get_defined_constants()` lists them. Doctrine's annotation parser (and
//! Drupal's copy of it) reads `true`/`false` in a docblock with
//! `defined($name)` then `constant($name)`. Also: the "Undefined constant"
//! message drops the leading backslash. Expected output: PHP 8.5.

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

/// Runs `source` as `t.php` with run-tests.php's `fatal_error_backtraces=0`.
fn run(source: &str) -> String {
    let sink = Sink::default();
    let options = VmOptions {
        ini: vec![("fatal_error_backtraces".to_string(), "0".to_string())],
    };
    let mut vm = Vm::new(&options, Box::new(sink.clone()));
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

#[test]
fn true_false_and_null_are_constants_found_by_name() {
    let out = run(r#"<?php
namespace Foo;
function f($n) { return \defined($n); }
foreach (['true', 'FALSE', 'nUlL', '\true', '\NULL', 'Foo\true', '\Foo\true', 'tru', 'true ', 'nope', '\Nope\X'] as $n) {
    echo $n, ': ', \var_export(f($n), true), ' ';
    try { \var_export(\constant($n)); } catch (\Error $e) { echo $e->getMessage(); }
    echo "\n";
}
$all = \get_defined_constants();
$nulls = 0;
foreach ($all as $k => $v) { $nulls += $k === 'NULL' ? 1 : 0; }
\var_dump($all['TRUE'], $all['FALSE'], isset($all['NULL']), $nulls, isset($all['true']));
\var_dump(@\define('true', 1), @\define('Null', 1), \define('Foo\true', 5), \constant('foo\true'), \constant('true'));
"#);
    assert_eq!(
        out,
        r#"true: true true
FALSE: true false
nUlL: true NULL
\true: true true
\NULL: true NULL
Foo\true: false Undefined constant "Foo\true"
\Foo\true: false Undefined constant "Foo\true"
tru: false Undefined constant "tru"
true : false Undefined constant "true "
nope: false Undefined constant "nope"
\Nope\X: false Undefined constant "Nope\X"
bool(true)
bool(false)
bool(false)
int(1)
bool(false)
bool(false)
bool(false)
bool(true)
int(5)
bool(true)
"#
    );
}

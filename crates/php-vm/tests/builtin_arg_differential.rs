// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Builtin argument handling the differential sweep of builtins (calls
//! with one to three arguments from a pool of values, compared with the C
//! implementation) found to differ: the type a `TypeError` names, the
//! argument-count messages, literal against dynamic calls of the
//! functions php-src compiles specially. The expected output is
//! `/usr/bin/php8.5 -n` on each script (ref: Zend/zend_API.c
//! `zend_parse_arg_impl`, `zend_wrong_parameters_count_error`).

#![allow(clippy::needless_raw_string_hashes)]

mod common;

use php_vm::VmOptions;
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

php_memlimit::install_global_allocator!();

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

fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = common::vm_with_stdlib(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

/// An `array|object` builtin parameter is parsed with
/// `Z_PARAM_ARRAY_OR_OBJECT`, whose error names "array"; a user function's
/// `array|object` keeps its declared name.
#[test]
fn array_or_object_parameter_names_array() {
    let out = run(r#"<?php
$x = 1;
foreach (['key', 'current', 'pos', 'end', 'next', 'prev', 'reset'] as $f) {
    try { $f($x); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
}
try { array_walk($x, 'strlen'); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { key("s"); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { key(null); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
function u(array|object $a) {}
try { u(1); } catch (TypeError $e) { echo strstr($e->getMessage(), ', called', true), "\n"; }
"#);
    assert_eq!(
        out,
        "key(): Argument #1 ($array) must be of type array, int given
current(): Argument #1 ($array) must be of type array, int given
pos(): Argument #1 ($array) must be of type array, int given
end(): Argument #1 ($array) must be of type array, int given
next(): Argument #1 ($array) must be of type array, int given
prev(): Argument #1 ($array) must be of type array, int given
reset(): Argument #1 ($array) must be of type array, int given
array_walk(): Argument #1 ($array) must be of type array, int given
key(): Argument #1 ($array) must be of type array, string given
key(): Argument #1 ($array) must be of type array, null given
u(): Argument #1 ($a) must be of type object|array, int given
"
    );
}

/// A parameter whose C function parses another type than its arginfo
/// declares: `number_format()`'s `$num` is `float` in arginfo and
/// `Z_PARAM_NUMBER` in zpp, `strtr()`'s `$to` is `?string` in arginfo
/// and `Z_PARAM_STRING` in zpp. The null deprecation names the arginfo
/// type, the `TypeError` the parsed one.
#[test]
fn parse_type_apart_from_the_arginfo_type() {
    let out = run(r#"<?php
set_error_handler(function ($n, $m) { echo "$n: $m\n"; return true; });
var_dump(number_format(null));
var_dump(number_format(5));
var_dump(number_format(" 2"));
try { number_format("x"); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
var_dump(strtr("abc", "b", null));
try { strtr("abc", "b", []); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
var_dump(strtr("abc", ["b" => "x"]));
"#);
    assert_eq!(
        out,
        r#"8192: number_format(): Passing null to parameter #1 ($num) of type float is deprecated
string(1) "0"
string(1) "5"
string(1) "2"
number_format(): Argument #1 ($num) must be of type int|float, string given
8192: strtr(): Passing null to parameter #3 ($to) of type ?string is deprecated
string(3) "abc"
strtr(): Argument #3 ($to) must be of type string, array given
string(3) "axc"
"#
    );
}

/// `array_key_exists()` by its literal name compiles to
/// `ZEND_ARRAY_KEY_EXISTS`, whose illegal-key error is the array access
/// one; every other call runs the function, whose error names the
/// parameter (`$f()`, `call_user_func()`, an unqualified call in a
/// namespace, unpacked or named arguments, the `key_exists()` alias).
#[test]
fn array_key_exists_literal_and_dynamic_illegal_keys() {
    let out = run(r#"<?php
namespace Foo;
function t($f) { try { $f(); } catch (\Throwable $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; } }
t(fn() => array_key_exists([], []));
t(fn() => \array_key_exists([], []));
t(fn() => array_key_exists(...[[], []]));
t(fn() => \array_key_exists(key: [], array: []));
t(fn() => \call_user_func('array_key_exists', [], []));
t(fn() => \array_map('array_key_exists', [[]], [[]]));
t(fn() => \key_exists([], []));
$ake = 'array_key_exists';
t(fn() => $ake(new \stdClass, []));
t(fn() => \array_key_exists(new \stdClass, []));
"#);
    assert_eq!(
        out,
        "TypeError: array_key_exists(): Argument #1 ($key) must be a valid array offset type
TypeError: Cannot access offset of type array on array
TypeError: array_key_exists(): Argument #1 ($key) must be a valid array offset type
TypeError: array_key_exists(): Argument #1 ($key) must be a valid array offset type
TypeError: array_key_exists(): Argument #1 ($key) must be a valid array offset type
TypeError: array_key_exists(): Argument #1 ($key) must be a valid array offset type
TypeError: key_exists(): Argument #1 ($key) must be a valid array offset type
TypeError: array_key_exists(): Argument #1 ($key) must be a valid array offset type
TypeError: Cannot access offset of type stdClass on array
"
    );
}

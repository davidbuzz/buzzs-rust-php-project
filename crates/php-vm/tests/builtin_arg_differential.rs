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

/// A closure's name, file, line, static variables and parameters are its
/// debug information only (`zend_closure_get_debug_info`): json_encode(),
/// var_export(), get_object_vars(), the internal-pointer functions and
/// foreach see a class without properties.
#[test]
fn closure_debug_properties_are_for_debug_output_only() {
    let out = run(r#"<?php
set_error_handler(function ($n, $m) { echo "$n: $m\n"; return true; });
$y = 5;
$f = function ($a) use ($y) { return 1; };
$g = fn() => 1;
echo json_encode($f), json_encode($g), json_encode([$f, 'k' => $g]), "\n";
var_export($g); echo "\n";
var_dump(get_object_vars($f), count((array) $f));
var_dump(current($f), key($f));
$x = $f; var_dump(end($x), next($x), reset($x), prev($x));
foreach ($f as $k => $v) { echo "never\n"; }
"#);
    assert_eq!(
        out,
        r#"{}{}{"0":{},"k":{}}
\Closure::__set_state(array(
))
array(0) {
}
int(1)
8192: current(): Calling current() on an object is deprecated
8192: key(): Calling key() on an object is deprecated
bool(false)
NULL
8192: end(): Calling end() on an object is deprecated
8192: next(): Calling next() on an object is deprecated
8192: reset(): Calling reset() on an object is deprecated
8192: prev(): Calling prev() on an object is deprecated
bool(false)
bool(false)
bool(false)
bool(false)
"#
    );
}

/// get_resource_id() and get_resource_type() parse with
/// `Z_PARAM_RESOURCE`: anything else is a `TypeError`, in either mode.
#[test]
fn resource_functions_refuse_other_values() {
    let out = run(r#"<?php
declare(strict_types=1);
foreach ([1, "x", null, 1.5, [], new stdClass, false] as $v) {
    try { get_resource_id($v); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
    try { get_resource_type($v); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
}
"#);
    assert_eq!(
        out,
        "get_resource_id(): Argument #1 ($resource) must be of type resource, int given
get_resource_type(): Argument #1 ($resource) must be of type resource, int given
get_resource_id(): Argument #1 ($resource) must be of type resource, string given
get_resource_type(): Argument #1 ($resource) must be of type resource, string given
get_resource_id(): Argument #1 ($resource) must be of type resource, null given
get_resource_type(): Argument #1 ($resource) must be of type resource, null given
get_resource_id(): Argument #1 ($resource) must be of type resource, float given
get_resource_type(): Argument #1 ($resource) must be of type resource, float given
get_resource_id(): Argument #1 ($resource) must be of type resource, array given
get_resource_type(): Argument #1 ($resource) must be of type resource, array given
get_resource_id(): Argument #1 ($resource) must be of type resource, stdClass given
get_resource_type(): Argument #1 ($resource) must be of type resource, stdClass given
get_resource_id(): Argument #1 ($resource) must be of type resource, false given
get_resource_type(): Argument #1 ($resource) must be of type resource, false given
"
    );
}

/// The callback variants of array_diff() and array_intersect() parse an
/// array and one callback (`"+f"`) or two (`"+ff"`): their minimum is 2
/// or 3, not the stub's one required parameter.
#[test]
fn array_udiff_family_minimum_argument_counts() {
    let out = run(r#"<?php
foreach (['array_udiff', 'array_udiff_uassoc', 'array_uintersect_assoc', 'array_uintersect_uassoc', 'array_diff_ukey', 'array_intersect_uassoc', 'array_diff'] as $f) {
    try { $f(); } catch (Throwable $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
    try { $f([]); } catch (Throwable $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
}
try { array_udiff(); } catch (Throwable $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        "ArgumentCountError: array_udiff() expects at least 2 arguments, 0 given
ArgumentCountError: array_udiff() expects at least 2 arguments, 1 given
ArgumentCountError: array_udiff_uassoc() expects at least 3 arguments, 0 given
ArgumentCountError: array_udiff_uassoc() expects at least 3 arguments, 1 given
ArgumentCountError: array_uintersect_assoc() expects at least 2 arguments, 0 given
ArgumentCountError: array_uintersect_assoc() expects at least 2 arguments, 1 given
ArgumentCountError: array_uintersect_uassoc() expects at least 3 arguments, 0 given
ArgumentCountError: array_uintersect_uassoc() expects at least 3 arguments, 1 given
ArgumentCountError: array_diff_ukey() expects at least 2 arguments, 0 given
ArgumentCountError: array_diff_ukey() expects at least 2 arguments, 1 given
ArgumentCountError: array_intersect_uassoc() expects at least 2 arguments, 0 given
ArgumentCountError: array_intersect_uassoc() expects at least 2 arguments, 1 given
ArgumentCountError: array_diff() expects at least 1 argument, 0 given
ArgumentCountError: array_udiff() expects at least 2 arguments, 0 given
"
    );
}

/// strtr() parses exactly two arguments, or exactly three when it got
/// more than two: its count errors say "exactly", by name or dynamically.
#[test]
fn strtr_argument_count_is_exact() {
    let out = run(r#"<?php
$f = 'strtr';
foreach ([[], ['a'], ['a', 'b', 'c', 'd']] as $args) {
    try { strtr(...$args); } catch (ArgumentCountError $e) { echo $e->getMessage(), "\n"; }
    try { $f(...$args); } catch (ArgumentCountError $e) { echo $e->getMessage(), "\n"; }
}
try { strtr('a'); } catch (ArgumentCountError $e) { echo $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        "strtr() expects exactly 2 arguments, 0 given
strtr() expects exactly 2 arguments, 0 given
strtr() expects exactly 2 arguments, 1 given
strtr() expects exactly 2 arguments, 1 given
strtr() expects exactly 3 arguments, 4 given
strtr() expects exactly 3 arguments, 4 given
strtr() expects exactly 2 arguments, 1 given
"
    );
}

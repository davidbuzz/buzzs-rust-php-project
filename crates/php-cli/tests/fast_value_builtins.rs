// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! One-argument builtins answered without their handler or a frame
//! (`php_vm::call::fast_eval`): `is_iterable()`, `is_countable()`,
//! `gettype()`, `get_class()`, `spl_object_id()` and `array_is_list()`,
//! for every kind of value, by reference, through a callback, with the
//! wrong arguments (the handler's errors), and next to a method or a
//! namespaced function of the same name, which are not them. Each round
//! repeats the calls, so the later rounds run on warm call sites.

use std::io::Write;
use std::process::{Command, Stdio};

/// Runs `source` (read from standard input) with no php.ini, every error
/// shown and none logged.
fn run(source: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_php"))
        .args([
            "-n",
            "-d",
            "memory_limit=1G",
            "-d",
            "log_errors=0",
            "-d",
            "display_errors=1",
            "-d",
            "error_reporting=-1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the CLI");
    child
        .stdin
        .take()
        .expect("CLI stdin")
        .write_all(source.as_bytes())
        .expect("write the script");
    let out = child.wait_with_output().expect("run the script");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Expected output: PHP 8.5.
#[test]
fn value_tests_without_a_handler_answer_what_the_handler_does() {
    assert_eq!(run(SOURCE), EXPECTED);
}

/// A builtin method named like one of those functions is the method:
/// `BcMath\Number::sqrt($scale)` and `round($precision)` were answered as
/// `sqrt($scale)` and `round($precision)` from the second call on. The
/// expected values are the first, framed call's (php-src:
/// `ext/bcmath/tests/number/methods/sqrt.phpt`, `round.phpt`).
#[test]
fn a_builtin_method_named_like_a_fast_function_is_called() {
    let out = run(r#"<?php
$n = new BcMath\Number("16");
for ($i = 0; $i < 4; $i++) { echo $n->sqrt(2), " ", $n->round(1), " ", $n->floor(), "\n"; }
"#);
    assert_eq!(out, "4.00 16.0 16\n".repeat(4));
}

const SOURCE: &str = r#"<?php
namespace App;
class It implements \IteratorAggregate, \Countable { function getIterator(): \Iterator { return new \ArrayIterator([]); } function count(): int { return 0; } }
enum E { case A; }
function show($f) { try { var_dump($f()); } catch (\Throwable $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; } }
$fh = fopen('php://memory', 'r'); $closed = fopen('php://memory', 'r'); fclose($closed);
$vals = ['arr' => [1, 2], 'map' => ['a' => 1], 'empty' => [], 'holes' => [1 => 'x'], 'str' => 'abc', 'int' => 5, 'flt' => 1.5, 'null' => null, 'true' => true,
  'obj' => new \stdClass, 'it' => new It, 'gen' => (function () { yield 1; })(), 'clo' => fn() => 1, 'enum' => E::A, 'ao' => new \ArrayObject([1]), 'res' => $fh, 'closed' => $closed];
for ($round = 0; $round < 3; $round++) {
  echo "round $round\n";
  foreach ($vals as $name => $v) {
    echo "-- $name\n";
    show(fn() => is_iterable($v)); show(fn() => \is_iterable($v)); show(fn() => is_countable($v)); show(fn() => gettype($v)); show(fn() => \gettype($v));
    show(fn() => get_class($v)); show(fn() => \get_class($v)); show(fn() => $name === 'obj' || $name === 'it' ? spl_object_id($v) === \spl_object_id($v) : spl_object_id($v));
    show(fn() => array_is_list($v)); show(fn() => \array_is_list($v));
  }
  $r = &$vals['arr']; show(fn() => array_is_list($r)); show(fn() => gettype($r)); show(fn() => is_iterable($r)); unset($r);
  show(fn() => gettype()); show(fn() => get_class()); show(fn() => is_iterable(1, 2)); show(fn() => array_is_list(array: [1])); show(fn() => \call_user_func('gettype', 1.0));
  show(fn() => array_map('gettype', [1, 'a', null])); show(fn() => array_map('array_is_list', [[1], ['a' => 1]]));
}
class K { function gettype($x) { return "method $x"; } static function get_class($x) { return "static $x"; } function is_iterable($x) { return 'mine'; } }
$k = new K; for ($i = 0; $i < 3; $i++) { echo $k->gettype(1), ' ', K::get_class(2), ' ', $k->is_iterable([]), "\n"; }
function gettype($x) { return 'ns gettype'; }
for ($i = 0; $i < 3; $i++) { echo gettype(1), ' ', \gettype(1), "\n"; }
"#;

const EXPECTED: &str = r#"round 0
-- arr
bool(true)
bool(true)
bool(true)
string(10) "ns gettype"
string(5) "array"
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, array given
bool(true)
bool(true)
-- map
bool(true)
bool(true)
bool(true)
string(10) "ns gettype"
string(5) "array"
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, array given
bool(false)
bool(false)
-- empty
bool(true)
bool(true)
bool(true)
string(10) "ns gettype"
string(5) "array"
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, array given
bool(true)
bool(true)
-- holes
bool(true)
bool(true)
bool(true)
string(10) "ns gettype"
string(5) "array"
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, array given
bool(false)
bool(false)
-- str
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(6) "string"
TypeError: get_class(): Argument #1 ($object) must be of type object, string given
TypeError: get_class(): Argument #1 ($object) must be of type object, string given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, string given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, string given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, string given
-- int
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(7) "integer"
TypeError: get_class(): Argument #1 ($object) must be of type object, int given
TypeError: get_class(): Argument #1 ($object) must be of type object, int given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, int given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, int given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, int given
-- flt
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(6) "double"
TypeError: get_class(): Argument #1 ($object) must be of type object, float given
TypeError: get_class(): Argument #1 ($object) must be of type object, float given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, float given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, float given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, float given
-- null
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(4) "NULL"
TypeError: get_class(): Argument #1 ($object) must be of type object, null given
TypeError: get_class(): Argument #1 ($object) must be of type object, null given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, null given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, null given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, null given
-- true
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(7) "boolean"
TypeError: get_class(): Argument #1 ($object) must be of type object, true given
TypeError: get_class(): Argument #1 ($object) must be of type object, true given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, true given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, true given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, true given
-- obj
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(6) "object"
string(8) "stdClass"
string(8) "stdClass"
bool(true)
TypeError: array_is_list(): Argument #1 ($array) must be of type array, stdClass given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, stdClass given
-- it
bool(true)
bool(true)
bool(true)
string(10) "ns gettype"
string(6) "object"
string(6) "App\It"
string(6) "App\It"
bool(true)
TypeError: array_is_list(): Argument #1 ($array) must be of type array, App\It given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, App\It given
-- gen
bool(true)
bool(true)
bool(false)
string(10) "ns gettype"
string(6) "object"
string(9) "Generator"
string(9) "Generator"
int(4)
TypeError: array_is_list(): Argument #1 ($array) must be of type array, Generator given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, Generator given
-- clo
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(6) "object"
string(7) "Closure"
string(7) "Closure"
int(5)
TypeError: array_is_list(): Argument #1 ($array) must be of type array, Closure given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, Closure given
-- enum
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(6) "object"
string(5) "App\E"
string(5) "App\E"
int(6)
TypeError: array_is_list(): Argument #1 ($array) must be of type array, App\E given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, App\E given
-- ao
bool(true)
bool(true)
bool(true)
string(10) "ns gettype"
string(6) "object"
string(11) "ArrayObject"
string(11) "ArrayObject"
int(7)
TypeError: array_is_list(): Argument #1 ($array) must be of type array, ArrayObject given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, ArrayObject given
-- res
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(8) "resource"
TypeError: get_class(): Argument #1 ($object) must be of type object, resource given
TypeError: get_class(): Argument #1 ($object) must be of type object, resource given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, resource given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, resource given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, resource given
-- closed
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(17) "resource (closed)"
TypeError: get_class(): Argument #1 ($object) must be of type object, resource given
TypeError: get_class(): Argument #1 ($object) must be of type object, resource given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, resource given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, resource given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, resource given
bool(true)
string(10) "ns gettype"
bool(true)
ArgumentCountError: Too few arguments to function App\gettype(), 0 passed in Standard input code on line 18 and exactly 1 expected
Error: get_class() without arguments must be called from within a class
ArgumentCountError: is_iterable() expects exactly 1 argument, 2 given
bool(true)
string(6) "double"
array(3) {
  [0]=>
  string(7) "integer"
  [1]=>
  string(6) "string"
  [2]=>
  string(4) "NULL"
}
array(2) {
  [0]=>
  bool(true)
  [1]=>
  bool(false)
}
round 1
-- arr
bool(true)
bool(true)
bool(true)
string(10) "ns gettype"
string(5) "array"
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, array given
bool(true)
bool(true)
-- map
bool(true)
bool(true)
bool(true)
string(10) "ns gettype"
string(5) "array"
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, array given
bool(false)
bool(false)
-- empty
bool(true)
bool(true)
bool(true)
string(10) "ns gettype"
string(5) "array"
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, array given
bool(true)
bool(true)
-- holes
bool(true)
bool(true)
bool(true)
string(10) "ns gettype"
string(5) "array"
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, array given
bool(false)
bool(false)
-- str
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(6) "string"
TypeError: get_class(): Argument #1 ($object) must be of type object, string given
TypeError: get_class(): Argument #1 ($object) must be of type object, string given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, string given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, string given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, string given
-- int
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(7) "integer"
TypeError: get_class(): Argument #1 ($object) must be of type object, int given
TypeError: get_class(): Argument #1 ($object) must be of type object, int given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, int given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, int given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, int given
-- flt
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(6) "double"
TypeError: get_class(): Argument #1 ($object) must be of type object, float given
TypeError: get_class(): Argument #1 ($object) must be of type object, float given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, float given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, float given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, float given
-- null
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(4) "NULL"
TypeError: get_class(): Argument #1 ($object) must be of type object, null given
TypeError: get_class(): Argument #1 ($object) must be of type object, null given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, null given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, null given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, null given
-- true
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(7) "boolean"
TypeError: get_class(): Argument #1 ($object) must be of type object, true given
TypeError: get_class(): Argument #1 ($object) must be of type object, true given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, true given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, true given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, true given
-- obj
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(6) "object"
string(8) "stdClass"
string(8) "stdClass"
bool(true)
TypeError: array_is_list(): Argument #1 ($array) must be of type array, stdClass given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, stdClass given
-- it
bool(true)
bool(true)
bool(true)
string(10) "ns gettype"
string(6) "object"
string(6) "App\It"
string(6) "App\It"
bool(true)
TypeError: array_is_list(): Argument #1 ($array) must be of type array, App\It given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, App\It given
-- gen
bool(true)
bool(true)
bool(false)
string(10) "ns gettype"
string(6) "object"
string(9) "Generator"
string(9) "Generator"
int(4)
TypeError: array_is_list(): Argument #1 ($array) must be of type array, Generator given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, Generator given
-- clo
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(6) "object"
string(7) "Closure"
string(7) "Closure"
int(5)
TypeError: array_is_list(): Argument #1 ($array) must be of type array, Closure given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, Closure given
-- enum
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(6) "object"
string(5) "App\E"
string(5) "App\E"
int(6)
TypeError: array_is_list(): Argument #1 ($array) must be of type array, App\E given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, App\E given
-- ao
bool(true)
bool(true)
bool(true)
string(10) "ns gettype"
string(6) "object"
string(11) "ArrayObject"
string(11) "ArrayObject"
int(7)
TypeError: array_is_list(): Argument #1 ($array) must be of type array, ArrayObject given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, ArrayObject given
-- res
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(8) "resource"
TypeError: get_class(): Argument #1 ($object) must be of type object, resource given
TypeError: get_class(): Argument #1 ($object) must be of type object, resource given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, resource given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, resource given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, resource given
-- closed
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(17) "resource (closed)"
TypeError: get_class(): Argument #1 ($object) must be of type object, resource given
TypeError: get_class(): Argument #1 ($object) must be of type object, resource given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, resource given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, resource given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, resource given
bool(true)
string(10) "ns gettype"
bool(true)
ArgumentCountError: Too few arguments to function App\gettype(), 0 passed in Standard input code on line 18 and exactly 1 expected
Error: get_class() without arguments must be called from within a class
ArgumentCountError: is_iterable() expects exactly 1 argument, 2 given
bool(true)
string(6) "double"
array(3) {
  [0]=>
  string(7) "integer"
  [1]=>
  string(6) "string"
  [2]=>
  string(4) "NULL"
}
array(2) {
  [0]=>
  bool(true)
  [1]=>
  bool(false)
}
round 2
-- arr
bool(true)
bool(true)
bool(true)
string(10) "ns gettype"
string(5) "array"
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, array given
bool(true)
bool(true)
-- map
bool(true)
bool(true)
bool(true)
string(10) "ns gettype"
string(5) "array"
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, array given
bool(false)
bool(false)
-- empty
bool(true)
bool(true)
bool(true)
string(10) "ns gettype"
string(5) "array"
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, array given
bool(true)
bool(true)
-- holes
bool(true)
bool(true)
bool(true)
string(10) "ns gettype"
string(5) "array"
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: get_class(): Argument #1 ($object) must be of type object, array given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, array given
bool(false)
bool(false)
-- str
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(6) "string"
TypeError: get_class(): Argument #1 ($object) must be of type object, string given
TypeError: get_class(): Argument #1 ($object) must be of type object, string given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, string given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, string given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, string given
-- int
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(7) "integer"
TypeError: get_class(): Argument #1 ($object) must be of type object, int given
TypeError: get_class(): Argument #1 ($object) must be of type object, int given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, int given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, int given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, int given
-- flt
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(6) "double"
TypeError: get_class(): Argument #1 ($object) must be of type object, float given
TypeError: get_class(): Argument #1 ($object) must be of type object, float given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, float given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, float given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, float given
-- null
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(4) "NULL"
TypeError: get_class(): Argument #1 ($object) must be of type object, null given
TypeError: get_class(): Argument #1 ($object) must be of type object, null given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, null given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, null given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, null given
-- true
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(7) "boolean"
TypeError: get_class(): Argument #1 ($object) must be of type object, true given
TypeError: get_class(): Argument #1 ($object) must be of type object, true given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, true given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, true given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, true given
-- obj
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(6) "object"
string(8) "stdClass"
string(8) "stdClass"
bool(true)
TypeError: array_is_list(): Argument #1 ($array) must be of type array, stdClass given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, stdClass given
-- it
bool(true)
bool(true)
bool(true)
string(10) "ns gettype"
string(6) "object"
string(6) "App\It"
string(6) "App\It"
bool(true)
TypeError: array_is_list(): Argument #1 ($array) must be of type array, App\It given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, App\It given
-- gen
bool(true)
bool(true)
bool(false)
string(10) "ns gettype"
string(6) "object"
string(9) "Generator"
string(9) "Generator"
int(4)
TypeError: array_is_list(): Argument #1 ($array) must be of type array, Generator given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, Generator given
-- clo
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(6) "object"
string(7) "Closure"
string(7) "Closure"
int(5)
TypeError: array_is_list(): Argument #1 ($array) must be of type array, Closure given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, Closure given
-- enum
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(6) "object"
string(5) "App\E"
string(5) "App\E"
int(6)
TypeError: array_is_list(): Argument #1 ($array) must be of type array, App\E given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, App\E given
-- ao
bool(true)
bool(true)
bool(true)
string(10) "ns gettype"
string(6) "object"
string(11) "ArrayObject"
string(11) "ArrayObject"
int(7)
TypeError: array_is_list(): Argument #1 ($array) must be of type array, ArrayObject given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, ArrayObject given
-- res
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(8) "resource"
TypeError: get_class(): Argument #1 ($object) must be of type object, resource given
TypeError: get_class(): Argument #1 ($object) must be of type object, resource given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, resource given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, resource given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, resource given
-- closed
bool(false)
bool(false)
bool(false)
string(10) "ns gettype"
string(17) "resource (closed)"
TypeError: get_class(): Argument #1 ($object) must be of type object, resource given
TypeError: get_class(): Argument #1 ($object) must be of type object, resource given
TypeError: spl_object_id(): Argument #1 ($object) must be of type object, resource given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, resource given
TypeError: array_is_list(): Argument #1 ($array) must be of type array, resource given
bool(true)
string(10) "ns gettype"
bool(true)
ArgumentCountError: Too few arguments to function App\gettype(), 0 passed in Standard input code on line 18 and exactly 1 expected
Error: get_class() without arguments must be called from within a class
ArgumentCountError: is_iterable() expects exactly 1 argument, 2 given
bool(true)
string(6) "double"
array(3) {
  [0]=>
  string(7) "integer"
  [1]=>
  string(6) "string"
  [2]=>
  string(4) "NULL"
}
array(2) {
  [0]=>
  bool(true)
  [1]=>
  bool(false)
}
method 1 static 2 mine
method 1 static 2 mine
method 1 static 2 mine
ns gettype integer
ns gettype integer
ns gettype integer
"#;

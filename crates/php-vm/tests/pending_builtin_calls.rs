// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Builtin calls the tape does not prepare (an argument that is another
//! call's result, an array literal), which reach the pending call's
//! `ZEND_DO_ICALL` (`Vm::op_do_call`): made in the pooled builtin frame
//! when every argument is by value and of its declared type, through the
//! general call otherwise. Backtraces, exceptions, deprecations, coercion
//! and defaults are php-src's either way. Expected output from
//! `/usr/bin/php8.5 -n` (its full script path shortened to `t.php`).

mod common;

use php_vm::VmOptions;
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
fn run(source: &str) -> (i32, String) {
    let sink = Sink::default();
    let options = VmOptions {
        ini: vec![("fatal_error_backtraces".to_string(), "0".to_string())],
    };
    let mut vm = common::vm_with_stdlib(&options, Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    (status, String::from_utf8(output).expect("UTF-8 output"))
}

#[test]
fn builtins_given_call_results_behave_as_in_php_src() {
    let (status, out) = run(r#"<?php
function id($v) { return $v; }
class J implements JsonSerializable {
    function jsonSerialize(): mixed { echo (new Exception)->getTraceAsString(), "\n"; return 1; }
}
$a = ['x' => 1, 'y' => 2];
echo implode(',', array_keys($a)), "\n";
echo implode(',', array_merge([1], id([2, 3]))), "\n";
echo json_encode(id(new J)), "\n";
var_dump(explode(',', id('a,b')));
try { echo str_repeat('x', id(-1)); } catch (ValueError $e) { echo $e->getMessage(), "\n", $e->getTraceAsString(), "\n"; }
echo strlen(id(12345)), "\n";
echo strlen(id(null)), "\n";
try { strlen(id([])); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
echo str_pad(id('a'), 3, '*'), "|", substr_count(id('aXbXc'), 'X'), "\n";
echo max(id([1, 5, 3])), " ", count(array_unique(id([1, 1, 2]))), "\n";
"#);
    assert_eq!(status, 0, "{out}");
    assert_eq!(
        out,
        r#"x,y
1,2,3
#0 [internal function]: J->jsonSerialize()
#1 t.php(9): json_encode(Object(J))
#2 {main}
1
array(2) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
}
str_repeat(): Argument #2 ($times) must be greater than or equal to 0
#0 t.php(11): str_repeat('x', -1)
#1 {main}
5

Deprecated: strlen(): Passing null to parameter #1 ($string) of type string is deprecated in t.php on line 13
0
strlen(): Argument #1 ($string) must be of type string, array given
a**|2
5 2
"#
    );
}

/// The same call made again and again from one site reuses the pooled
/// frame, and a builtin that calls back into user code
/// (`json_encode` of a `JsonSerializable`) can make the same kind of call
/// itself.
#[test]
fn repeated_pending_builtin_calls_release_their_arguments() {
    let (status, out) = run(r#"<?php
function id($v) { return $v; }
class N implements JsonSerializable {
    function __construct(public int $d) {}
    function jsonSerialize(): mixed { return $this->d ? implode('-', array_keys(id([$this->d => 1, 'k' => 2]))) : 0; }
}
$s = str_repeat('ab', 3);
$total = 0;
for ($i = 0; $i < 50; $i++) { $total += strlen(implode(',', array_merge(id([$s]), [$s]))); }
echo $total, "\n";
echo json_encode(id([new N(3), new N(0)])), "\n";
$t = str_repeat('t', 2);
for ($i = 0; $i < 100; $i++) { $r = implode(',', array_keys(id([$t => 1, 'b' => 2]))); }
echo $r, "\n";
"#);
    assert_eq!(status, 0, "{out}");
    assert_eq!(out, "650\n[\"3-k\",0]\ntt,b\n");
}

/// Values sent to `ZEND_ARG_SEND_PREFER_REF` parameters (`current()`,
/// `key()`, `array_multisort()`'s) are passed by value, with no notice; a
/// function's result returned by reference is sent as that reference, so
/// `array_multisort(g())` sorts what `g()` returns a reference to.
#[test]
fn prefer_ref_parameters_take_values_and_returned_references() {
    let (status, out) = run(r#"<?php
function f() { return [3, 1, 2]; }
function &g() { static $a = [3, 1, 2]; return $a; }
function t() {
    echo current(f()), key(f()), current([5, 6]), "\n";
    $a = [3, 1, 2];
    var_dump(array_multisort($a, SORT_DESC));
    echo implode(',', $a), "\n";
    var_dump(array_multisort(f()));
    $r = &g();
    var_dump(array_multisort(g()));
    echo implode(',', $r), "\n";
}
t();
echo current(f()), key(f()), "\n";
"#);
    assert_eq!(status, 0, "{out}");
    assert_eq!(
        out,
        "305\nbool(true)\n3,2,1\nbool(true)\nbool(true)\n1,2,3\n30\n"
    );
}

/// A builtin method with a by-reference parameter (`SplFileObject::fscanf()`'s
/// variadic `&...$vars`) called with variables fills them, a second call
/// over variables that hold values again too.
#[test]
fn builtin_methods_fill_by_reference_arguments() {
    let (status, out) = run(r#"<?php
function t() {
    $f = new SplTempFileObject();
    $f->fwrite("12 ab\n34 cd\n");
    $f->rewind();
    $n = $f->fscanf("%d %s", $i, $s);
    var_dump($n, $i, $s);
    $i .= 'x';
    $n = $f->fscanf("%d %s", $i, $s);
    var_dump($n, $i, $s);
}
t();
"#);
    assert_eq!(status, 0, "{out}");
    assert_eq!(
        out,
        "int(2)\nint(12)\nstring(2) \"ab\"\nint(2)\nint(34)\nstring(2) \"cd\"\n"
    );
}

/// Spread lists into a builtin (`ZEND_SEND_UNPACK`): the call's argument
/// vector is the arguments, references in the list are sent as their
/// values, more than the inline buffer's six arguments are taken too, and
/// a wrong type is still the `TypeError` of the general call.
#[test]
fn spread_lists_into_builtins() {
    let (status, out) = run(r#"<?php
function t() {
    $lists = [[1, 2], [3], [], [4, 5]];
    echo implode(',', array_merge(...$lists)), "\n";
    $x = [9];
    $refs = [&$x, [8]];
    echo implode(',', array_merge(...$refs)), "\n";
    $parts = ['a', 'b'];
    echo sprintf('%s-%s', ...$parts), "\n";
    $many = array_fill(0, 9, [1]);
    echo count(array_merge(...$many)), "\n";
    try { array_merge(...[[1], 2]); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
}
t();
"#);
    assert_eq!(status, 0, "{out}");
    assert_eq!(
        out,
        "1,2,3,4,5\n9,8\na-b\n9\narray_merge(): Argument #2 must be of type array, int given\n"
    );
}

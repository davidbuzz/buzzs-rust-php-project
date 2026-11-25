// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `call_user_func()` and `call_user_func_array()` written by name, which
//! php-src compiles to opcodes (`zend_compile_func_cuf` /
//! `zend_compile_func_cufa` in `Zend/zend_compile.c`: `ZEND_INIT_USER_CALL`,
//! `ZEND_SEND_USER` / `ZEND_SEND_ARRAY`, `ZEND_DO_FCALL`), against the same
//! calls made through the builtins. Expected output from php 8.5 (`-n`).

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

/// A call written as `call_user_func(...)` / `call_user_func_array(...)` is
/// compiled to `ZEND_INIT_USER_CALL` (`zend_compile_func_cuf[a]`): the
/// callback's frame is the only one; through a variable function name the
/// builtin runs and shows in the backtrace.
#[test]
fn backtraces_show_no_call_user_func_frame_when_written_by_name() {
    let (status, out) = run(r#"<?php
function f($a, $b = 2) { echo json_encode(array_map(fn($f) => $f['function'], debug_backtrace())), "\n"; return $a + $b; }
var_dump(call_user_func('f', 1));
var_dump(call_user_func_array('f', [1, 'b' => 5]));
$g = 'call_user_func'; var_dump($g('f', 3));
$h = 'call_user_func_array'; var_dump($h('f', [3, 4]));
function d() { debug_print_backtrace(); }
call_user_func('d');
call_user_func_array('d', []);
$g('d');
class K { function m() { debug_print_backtrace(); } static function s() { debug_print_backtrace(); } }
call_user_func([new K, 'm']);
call_user_func('K::s');
call_user_func(function () { debug_print_backtrace(); });
"#);
    assert_eq!(
        out,
        r#"["f"]
int(3)
["f"]
int(6)
["f","call_user_func"]
int(5)
["f","call_user_func_array"]
int(7)
#0 t.php(8): d()
#0 t.php(9): d()
#0 [internal function]: d()
#1 t.php(10): call_user_func('d')
#0 t.php(12): K->m()
#0 t.php(13): K::s()
#0 t.php(14): {closure:t.php:14}()
"#
    );
    assert_eq!(status, 0);
}

/// Named arguments, unpacking and wrong argument counts keep the builtin
/// call (`zend_args_contain_unpack_or_named`, `args->children`).
#[test]
fn calls_the_builtin_where_the_compiler_refuses_to_lower() {
    let (status, out) = run(r#"<?php
function f($a = 0, $b = 0) { echo json_encode(array_map(fn($f) => $f['function'], debug_backtrace())), "\n"; }
call_user_func('f', b: 3);
call_user_func(...['f', 1]);
call_user_func_array('f', [1], ...[]);
$a = ['f', [1]];
call_user_func_array(...$a);
try { call_user_func_array('f'); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
try { call_user_func(); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
try { call_user_func_array('f', [], 3); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        r#"["f","call_user_func"]
["f","call_user_func"]
["f","call_user_func_array"]
["f","call_user_func_array"]
ArgumentCountError: call_user_func_array() expects exactly 2 arguments, 1 given
ArgumentCountError: call_user_func() expects at least 1 argument, 0 given
ArgumentCountError: call_user_func_array() expects exactly 2 arguments, 3 given
"#
    );
    assert_eq!(status, 0);
}

/// An unqualified call in a namespace is resolved at run time
/// (`zend_compile_ns_call`) and keeps the builtin; a fully qualified one is
/// lowered.
#[test]
fn namespaced_unqualified_calls_keep_the_builtin() {
    let (status, out) = run(r#"<?php
namespace N;
function f($a) { return array_map(fn($f) => $f['function'], debug_backtrace()); }
echo json_encode(call_user_func('N\f', 1)), "\n";
echo json_encode(\call_user_func('N\f', 1)), "\n";
echo json_encode(call_user_func_array('N\f', [1])), "\n";
echo json_encode(\call_user_func_array(__NAMESPACE__ . '\f', [1])), "\n";
"#);
    assert_eq!(
        out,
        r#"["N\\f","call_user_func"]
["N\\f"]
["N\\f","call_user_func_array"]
["N\\f"]
"#
    );
    assert_eq!(status, 0);
}

/// The callback is called from the file that wrote the call, so its
/// `strict_types` applies (and "called in" names that line); through the
/// builtin it does not.
#[test]
fn strict_types_of_the_calling_file_apply_to_the_callback() {
    let (status, out) = run(r#"<?php
declare(strict_types=1);
try { var_dump(call_user_func('strlen', 5)); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$g = 'call_user_func';
try { var_dump($g('strlen', 5)); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { var_dump(call_user_func_array('strlen', [5])); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
function u(int $x) { return $x; }
try { var_dump(call_user_func('u', "5")); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        r#"strlen(): Argument #1 ($string) must be of type string, int given
int(1)
strlen(): Argument #1 ($string) must be of type string, int given
u(): Argument #1 ($x) must be of type int, string given, called in t.php on line 8
"#
    );
    assert_eq!(status, 0);
}

/// `ZEND_SEND_ARRAY`: string keys are named arguments, skipped optional
/// parameters get their defaults (`ZEND_CHECK_UNDEF_ARGS`), and the
/// `array_slice(func_get_args(), n)` form.
#[test]
fn named_arguments_and_func_get_args_forms() {
    let (status, out) = run(r#"<?php
function h($a = 1, $b = 2, ...$rest) { var_dump(func_num_args(), func_get_args(), $rest); }
call_user_func_array('h', ['b' => 5]);
call_user_func_array('h', [7, 8, 9, 'x' => 10]);
call_user_func('h', 1, 2, 3);
function g() { return call_user_func_array('h', array_slice(func_get_args(), 1)); }
g(1, 2, 3);
function g2() { return call_user_func_array('h', func_get_args()); }
g2(4, 5);
call_user_func('h', b: 3);




echo "done\n";
"#);
    assert_eq!(
        out,
        r#"int(2)
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(5)
}
array(0) {
}
int(3)
array(3) {
  [0]=>
  int(7)
  [1]=>
  int(8)
  [2]=>
  int(9)
}
array(2) {
  [0]=>
  int(9)
  ["x"]=>
  int(10)
}
int(3)
array(3) {
  [0]=>
  int(1)
  [1]=>
  int(2)
  [2]=>
  int(3)
}
array(1) {
  [0]=>
  int(3)
}
int(2)
array(2) {
  [0]=>
  int(2)
  [1]=>
  int(3)
}
array(0) {
}
int(2)
array(2) {
  [0]=>
  int(4)
  [1]=>
  int(5)
}
array(0) {
}
int(2)
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(3)
}
array(0) {
}
done
"#
    );
    assert_eq!(status, 0);
}

/// Private methods from inside their class, `self::`/`static` scopes,
/// bound closures, `__invoke`, `__call`, `__callStatic`, and the
/// `TypeError`s of `ZEND_INIT_USER_CALL`.
#[test]
fn scopes_visibility_and_callable_kinds() {
    let (status, out) = run(r#"<?php
class C {
    private $v = 'priv';
    private function p() { return $this->v; }
    protected static function ps() { return static::class; }
    function run() {
        echo call_user_func([$this, 'p']), "\n";
        echo call_user_func('self::ps'), "\n";
        echo call_user_func([self::class, 'ps']), "\n";
        echo call_user_func_array([$this, 'p'], []), "\n";
        echo call_user_func(function () { return $this->v; }), "\n";
    }
}
class D extends C {}
(new D)->run();
try { call_user_func([new C, 'p']); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
$c = Closure::bind(function () { return $this->v; }, new C, C::class);
echo call_user_func($c), "\n";
class Inv { function __invoke($a) { return "inv$a"; } function __call($n, $a) { return "call $n"; } static function __callStatic($n, $a) { return "cs $n"; } }
echo call_user_func(new Inv, 1), "\n";
echo call_user_func([new Inv, 'zz']), "\n";
echo call_user_func('Inv::yy'), "\n";
echo call_user_func_array([new Inv, 'zz'], ['k' => 1]), "\n";
try { call_user_func(5); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
try { call_user_func([1, 2, 3]); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
try { call_user_func('Nope::x'); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
try { call_user_func(['C', 'run']); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        r#"priv

Deprecated: Use of "self" in callables is deprecated in t.php on line 8
D
D
priv
priv
TypeError: call_user_func(): Argument #1 ($callback) must be a valid callback, cannot access private method C::p()
priv
inv1
call zz
cs yy
call zz
TypeError: call_user_func(): Argument #1 ($callback) must be a valid callback, no array or string given
TypeError: call_user_func(): Argument #1 ($callback) must be a valid callback, array callback must have exactly two members
TypeError: call_user_func(): Argument #1 ($callback) must be a valid callback, class "Nope" not found
TypeError: call_user_func(): Argument #1 ($callback) must be a valid callback, non-static method C::run() cannot be called statically
"#
    );
    assert_eq!(status, 0);
}

/// Exceptions through the callback (traces without a builtin frame, an
/// uncaught one included), generators, a by-reference return, builtin
/// callbacks.
#[test]
fn exceptions_generators_references_and_builtins() {
    let (status, out) = run(r#"<?php
function thrower($m) { throw new RuntimeException($m); }
function w() {
    try { return call_user_func('thrower', 'a'); } finally { echo "finally\n"; }
}
try { w(); } catch (Exception $e) { echo $e->getMessage(), "\n", $e->getTraceAsString(), "\n"; }
try { call_user_func_array('thrower', ['b']); } catch (Exception $e) { echo $e->getTraceAsString(), "\n"; }
function gen($n) { for ($i = 0; $i < $n; $i++) yield $i; }
foreach (call_user_func('gen', 3) as $v) echo $v;
foreach (call_user_func_array('gen', ['n' => 2]) as $v) echo $v;
echo "\n";
function &refret(array &$a) { return $a[0]; }
$arr = [1];
$r = &call_user_func_array('refret', [&$arr]);
$r = 5;
var_dump($arr);
echo strtoupper(call_user_func('str_repeat', 'ab', 2)), "\n";
var_dump(call_user_func_array('max', [1, 7, 3]));
var_dump(call_user_func_array('sprintf', ['%s-%s', 'a', 'b']));
call_user_func_array('thrower', ['uncaught']);
"#);
    assert_eq!(
        out,
        r#"finally
a
#0 t.php(4): thrower('a')
#1 t.php(6): w()
#2 {main}
#0 t.php(7): thrower('b')
#1 {main}
01201
array(1) {
  [0]=>
  &int(5)
}
ABAB
int(7)
string(3) "a-b"

Fatal error: Uncaught RuntimeException: uncaught in t.php:2
Stack trace:
#0 t.php(20): thrower('uncaught')
#1 {main}
  thrown in t.php on line 2
"#
    );
    assert_eq!(status, 255);
}

/// `#[\NoDiscard]` of the callback warns when the result of the call is
/// unused (`ZEND_DO_FCALL` with an unused result).
#[test]
fn nodiscard_of_the_callback() {
    let (status, out) = run(r#"<?php
#[\NoDiscard] function nd() { return 1; }
call_user_func('nd');
call_user_func_array('nd', []);
$x = call_user_func('nd');
(void) call_user_func('nd');
echo "done\n";
"#);
    assert_eq!(
        out,
        r#"
Warning: The return value of function nd() should either be used or intentionally ignored by casting it as (void) in t.php on line 3

Warning: The return value of function nd() should either be used or intentionally ignored by casting it as (void) in t.php on line 4
done
"#
    );
    assert_eq!(status, 0);
}

/// `ZEND_SEND_USER` / `ZEND_SEND_ARRAY` of a by-reference parameter: a
/// value is passed with the "must be passed by reference" warning, a
/// reference in the array is passed as it is.
#[test]
fn by_reference_parameters() {
    let (status, out) = run(r#"<?php
function r(&$x) { $x++; return $x; }
$v = 1;
var_dump(call_user_func('r', $v), $v);
var_dump(call_user_func_array('r', [$v]), $v);
$arr = [&$v];
var_dump(call_user_func_array('r', $arr), $v);
$arr2 = ['x' => &$v];
var_dump(call_user_func_array('r', $arr2), $v);
function byval($x) { $x = 9; }
$w = 1; $a = [&$w];
call_user_func_array('byval', $a);
var_dump($w);
function vr(&...$xs) { foreach ($xs as &$x) $x *= 2; }
$p = 1; $q = 2;
call_user_func_array('vr', [&$p, &$q]);
var_dump($p, $q);
call_user_func('vr', $p);
$sorted = [3, 1, 2];
call_user_func_array('sort', [&$sorted]);
var_dump($sorted);
call_user_func('sort', $sorted);
var_dump($sorted);
"#);
    assert_eq!(
        out,
        r#"
Warning: r(): Argument #1 ($x) must be passed by reference, value given in t.php on line 4
int(2)
int(1)

Warning: r(): Argument #1 ($x) must be passed by reference, value given in t.php on line 5
int(2)
int(1)
int(2)
int(2)
int(3)
int(3)
int(1)
int(2)
int(4)

Warning: vr(): Argument #1 must be passed by reference, value given in t.php on line 18
array(3) {
  [0]=>
  int(1)
  [1]=>
  int(2)
  [2]=>
  int(3)
}

Warning: sort(): Argument #1 ($array) must be passed by reference, value given in t.php on line 22
array(3) {
  [0]=>
  int(1)
  [1]=>
  int(2)
  [2]=>
  int(3)
}
"#
    );
    assert_eq!(status, 0);
}

/// The callback is resolved before the arguments are evaluated; a literal
/// name of an internal function is bound at compile time
/// (`zend_try_compile_ct_bound_init_user_func`: `ZEND_INIT_FCALL`, not a
/// dynamic call); a call site called repeatedly.
#[test]
fn resolution_errors_and_order() {
    let (status, out) = run(r#"<?php
try { call_user_func('nope'); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
try { call_user_func_array('nope', []); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
function f($a, $b = 2) { return $a + $b; }
try { call_user_func_array('f', ['a' => 1, 2]); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
try { call_user_func_array('f', ['zz' => 1]); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
try { call_user_func_array('f', [1, 'a' => 1]); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
try { call_user_func_array('f', []); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
try { call_user_func_array('f', 5); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
try { call_user_func_array('f', null); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
try { call_user_func_array('nope', 5); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
function side() { echo "side\n"; return 1; }
try { call_user_func('nope', side()); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
try { call_user_func_array('func_get_args', []); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
try { call_user_func('compact', 'x'); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
$cb = 'f';
for ($i = 0; $i < 3; $i++) echo call_user_func($cb, $i), call_user_func_array($cb, [$i, $i]), "\n";
"#);
    assert_eq!(
        out,
        r#"TypeError: call_user_func(): Argument #1 ($callback) must be a valid callback, function "nope" not found or invalid function name
TypeError: call_user_func_array(): Argument #1 ($callback) must be a valid callback, function "nope" not found or invalid function name
Error: Cannot use positional argument after named argument
Error: Unknown named parameter $zz
Error: Named parameter $a overwrites previous argument
ArgumentCountError: Too few arguments to function f(), 0 passed in t.php on line 8 and at least 1 expected
TypeError: call_user_func_array(): Argument #2 ($args) must be of type array, int given
TypeError: call_user_func_array(): Argument #2 ($args) must be of type array, null given
TypeError: call_user_func_array(): Argument #1 ($callback) must be a valid callback, function "nope" not found or invalid function name
TypeError: call_user_func(): Argument #1 ($callback) must be a valid callback, function "nope" not found or invalid function name
Error: func_get_args() cannot be called from the global scope

Warning: compact(): Undefined variable $x in t.php on line 15
20
32
44
"#
    );
    assert_eq!(status, 0);
}

/// Through the builtin, `zend_call_function` checks no `#[\NoDiscard]`
/// and the callback's caller is internal (no "called in"); one call site
/// called with callables of different kinds, the cached resolution of an
/// `[$object, "name"]` array judged again for another scope.
#[test]
fn dynamic_builtin_calls_and_one_site_with_many_callables() {
    let (status, out) = run(r#"<?php
#[\NoDiscard] function nd() { return 1; }
$g = 'call_user_func';
$g('nd');
call_user_func(...['nd']);
function u(int $x) { return $x; }
try { $g('u', "x"); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { call_user_func('u', "x"); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$cbs = ['strtoupper', 'nd', fn($x) => "c$x", 'u'];
foreach ($cbs as $cb) { var_dump(call_user_func($cb, "5")); }
class P { function m($x) { return "m$x"; } static function s($x) { return "s$x"; } private function q() { return "q"; } function viaq() { return call_user_func([$this, 'q']); } }
$p = new P;
foreach ([[$p, 'm'], [$p, 's'], [$p, 'M'], 'P::s', [new P, 'm']] as $cb) { var_dump(call_user_func_array($cb, [1])); }
var_dump($p->viaq(), $p->viaq());
try { call_user_func([$p, 'q']); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        r#"u(): Argument #1 ($x) must be of type int, string given
u(): Argument #1 ($x) must be of type int, string given, called in t.php on line 8
string(1) "5"
int(1)
string(2) "c5"
int(5)
string(2) "m1"
string(2) "s1"
string(2) "m1"
string(2) "s1"
string(2) "m1"
string(1) "q"
string(1) "q"
call_user_func(): Argument #1 ($callback) must be a valid callback, cannot access private method P::q()
"#
    );
    assert_eq!(status, 0);
}

/// One call site called again and again (its cache hit, and missed by
/// another name, another case, a leading `\`), argument counts below the
/// required ones and past the declared parameters, packed and unpacked
/// arrays, references in the array, string keys, an empty array, variadic
/// and typed callees, the sliced form, a callback call as an argument: the
/// paths the tape's prepared call takes and those it leaves to the
/// generic ops.
#[test]
fn callback_functions_by_name_on_the_prepared_path() {
    let (status, out) = run(r#"<?php
function f($a, $b = 2) { return "f(" . implode(',', func_get_args()) . ")"; }
function g($a) { return "g$a"; }
foreach (['f', 'g', 'f', 'F', '\f', 'f'] as $cb) {
    echo call_user_func($cb, 1), ' ', call_user_func_array($cb, [1, 3]), "\n";
}
foreach ([[], [1], [1, 2], [1, 2, 3], ['b' => 5, 'a' => 1], [1, 'b' => 4]] as $args) {
    try { echo call_user_func_array('f', $args), "\n"; } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
}
for ($i = 0; $i < 3; $i++) {
    try { echo call_user_func('f'), "\n"; } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
    echo call_user_func('f', 1, 2, 3, 4), "\n";
}
function none() { return func_num_args(); }
for ($i = 0; $i < 2; $i++) { var_dump(call_user_func_array('none', []), call_user_func('none'), call_user_func_array('none', [1, 2])); }
$a = [1, 2, 3]; unset($a[0]);
for ($i = 0; $i < 2; $i++) echo call_user_func_array('f', $a), call_user_func_array('f', [1 => 'x', 0 => 'y']), "\n";
function byval($x) { $x = 9; return $x; }
function byref(&$x) { $x .= '!'; return $x; }
$w = 'w'; $refs = [&$w, 2];
for ($i = 0; $i < 2; $i++) { echo call_user_func_array('byval', $refs), call_user_func_array('f', $refs), call_user_func_array('byref', $refs), " $w\n"; }
for ($i = 0; $i < 2; $i++) { echo call_user_func('byref', $w), " $w\n"; }
function v(...$xs) { return count($xs) . ':' . implode(',', $xs); }
function v2($a, ...$xs) { return "$a/" . implode(',', $xs); }
for ($i = 0; $i < 2; $i++) { echo call_user_func_array('v', [1, 2, 3]), ' ', call_user_func_array('v2', [1, 2, 3]), ' ', call_user_func('v2', 1, 2), ' ', call_user_func_array('v', ['k' => 1]), "\n"; }
function t(int $x, string ...$r) { return var_export($x, true) . implode('', $r); }
for ($i = 0; $i < 2; $i++) { echo call_user_func('t', "5"), call_user_func_array('t', ["6", 7]), "\n"; }
try { call_user_func_array('t', ["x"]); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
function w() { return call_user_func_array('f', array_slice(func_get_args(), 1)); }
function w2() { return call_user_func_array('f', array_slice(func_get_args(), 1, 1)); }
for ($i = 0; $i < 2; $i++) { echo w(0, 1, 2), w2(0, 1, 2), call_user_func("f", call_user_func("g", $i), call_user_func_array("g", [$i])), "\n"; }
try { w(0); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        r#"f(1) f(1,3)
g1 g1
f(1) f(1,3)
f(1) f(1,3)
f(1) f(1,3)
f(1) f(1,3)
ArgumentCountError: Too few arguments to function f(), 0 passed in t.php on line 8 and at least 1 expected
f(1)
f(1,2)
f(1,2,3)
f(1,5)
f(1,4)
ArgumentCountError: Too few arguments to function f(), 0 passed in t.php on line 11 and at least 1 expected
f(1,2,3,4)
ArgumentCountError: Too few arguments to function f(), 0 passed in t.php on line 11 and at least 1 expected
f(1,2,3,4)
ArgumentCountError: Too few arguments to function f(), 0 passed in t.php on line 11 and at least 1 expected
f(1,2,3,4)
int(0)
int(0)
int(2)
int(0)
int(0)
int(2)
f(2,3)f(x,y)
f(2,3)f(x,y)
9f(w,2)w! w!
9f(w!,2)w!! w!!

Warning: byref(): Argument #1 ($x) must be passed by reference, value given in t.php on line 22
w!!! w!!

Warning: byref(): Argument #1 ($x) must be passed by reference, value given in t.php on line 22
w!!! w!!
3:1,2,3 1/2,3 1/2 1:1
3:1,2,3 1/2,3 1/2 1:1
567
567
t(): Argument #1 ($x) must be of type int, string given, called in t.php on line 28
f(1,2)f(1)f(g0,g0)
f(1,2)f(1)f(g1,g1)
Too few arguments to function f(), 0 passed in t.php on line 29 and at least 1 expected
"#
    );
    assert_eq!(status, 0);
}

/// Methods by `[$object, 'name']`, `['Class', 'name']`, `'Class::name'`
/// and `'static::name'` (a private one from inside its class), closures
/// (with bound variables, static, bound to an object and scope),
/// invokable objects, generators, builtins by a variable name (coerced,
/// rejected, miscounted, forbidden as dynamic calls), destructor order of
/// a temporary argument, backtraces and exceptions, each called
/// repeatedly from one call site.
#[test]
fn callback_methods_closures_and_builtins_on_the_prepared_path() {
    let (status, out) = run(r#"<?php
class C {
    private $v = 'priv';
    function m($a = 'd') { return static::class . "->m$a"; }
    static function s($a = 'd') { return static::class . "::s$a"; }
    private function p($a) { return "$this->v$a"; }
    function run() {
        $r = '';
        for ($i = 0; $i < 3; $i++) {
            $r .= call_user_func([$this, 'p'], $i) . call_user_func_array([$this, 'p'], [$i]);
            $r .= call_user_func([$this, 'm'], $i) . call_user_func('static::s', $i);
        }
        return $r;
    }
}
class D extends C { function __destruct() { echo "~D\n"; } }
$c = new C; $d = new D;
for ($i = 0; $i < 3; $i++) {
    $o = $i == 1 ? $d : $c;
    echo call_user_func([$o, 'm'], $i), ' ', call_user_func_array([$o, 'm'], [$i]), ' ', call_user_func([$o, 's'], $i), ' ',
        call_user_func(['C', 's']), ' ', call_user_func_array('D::s', [$i]), ' ', call_user_func(['D', 'S'], $i), "\n";
}
echo $c->run(), "\n", $d->run(), "\n";
try { call_user_func([$c, 'p'], 1); } catch (Error $e) { echo $e->getMessage(), "\n"; }
$k = 'ka';
$cl = function ($x, $y = 'y') use ($k) { return "$k$x$y"; };
$st = static fn($x) => "st$x";
$bound = Closure::bind(function ($x) { return $this->v . $x . static::class; }, new D, C::class);
for ($i = 0; $i < 3; $i++) {
    echo call_user_func($cl, $i), call_user_func_array($cl, [$i, 'z']), call_user_func($st, $i), call_user_func_array($bound, [$i]), "\n";
}
class Inv { function __invoke($a) { return "inv$a"; } }
$inv = new Inv;
for ($i = 0; $i < 2; $i++) echo call_user_func($inv, $i), call_user_func_array($inv, [$i]), call_user_func([$inv, '__invoke'], $i), "\n";
function gen($n) { for ($i = 0; $i < $n; $i++) yield $i; }
for ($i = 1; $i < 3; $i++) { foreach (call_user_func('gen', $i) as $v) echo $v; foreach (call_user_func_array('gen', [$i]) as $v) echo $v; echo "\n"; }
$f = 'strlen'; $am = 'array_merge'; $sr = 'str_repeat'; $mx = 'max';
for ($i = 0; $i < 3; $i++) {
    echo call_user_func($f, 'abc'), call_user_func_array($f, ['ab']), json_encode(call_user_func_array($am, [[1], [2, $i]])),
        call_user_func($sr, 5, 2), call_user_func_array($mx, [1, $i, 2]), "\n";
}
try { call_user_func($f, []); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { call_user_func($f); } catch (ArgumentCountError $e) { echo $e->getMessage(), "\n"; }
try { call_user_func_array($f, ['a', 'b']); } catch (ArgumentCountError $e) { echo $e->getMessage(), "\n"; }
function inner() { $x = 1; foreach (['func_get_args', 'compact', 'get_defined_vars'] as $n) { try { call_user_func($n, 'x'); } catch (Error $e) { echo $e->getMessage(), "\n"; } } }
inner();
class Del { function __construct(public $n) {} function __destruct() { echo "~{$this->n}\n"; } }
function eat($o) { echo "eat {$o->n}\n"; }
for ($i = 0; $i < 2; $i++) { call_user_func('eat', new Del("a$i")); echo "after\n"; call_user_func_array('eat', [new Del("b$i")]); echo "after\n"; }
function bt() { return implode(',', array_map(fn($f) => ($f['class'] ?? '') . $f['function'] . '@' . ($f['line'] ?? '-'), debug_backtrace())); }
class B { function m() { return bt(); } static function s() { return bt(); } }
for ($i = 0; $i < 2; $i++) {
    echo call_user_func('bt'), ' ', call_user_func_array('bt', []), ' ', call_user_func([new B, 'm']), ' ', call_user_func('B::s'), ' ', call_user_func(fn() => bt()), "\n";
}
function thrower($m) { throw new RuntimeException($m); }
for ($i = 0; $i < 2; $i++) {
    try { call_user_func('thrower', "t$i"); } catch (Exception $e) { echo $e->getMessage(), ' ', $e->getLine(), "\n", $e->getTraceAsString(), "\n"; }
    try { call_user_func_array([new B, 'nope'], []); } catch (Error $e) { echo $e->getMessage(), "\n"; }
}
echo "end\n";
"#);
    assert_eq!(
        out,
        r#"C->m0 C->m0 C::s0 C::sd D::s0 D::s0
D->m1 D->m1 D::s1 C::sd D::s1 D::s1
C->m2 C->m2 C::s2 C::sd D::s2 D::s2

Deprecated: Use of "static" in callables is deprecated in t.php on line 11

Deprecated: Use of "static" in callables is deprecated in t.php on line 11

Deprecated: Use of "static" in callables is deprecated in t.php on line 11
priv0priv0C->m0C::s0priv1priv1C->m1C::s1priv2priv2C->m2C::s2

Deprecated: Use of "static" in callables is deprecated in t.php on line 11

Deprecated: Use of "static" in callables is deprecated in t.php on line 11

Deprecated: Use of "static" in callables is deprecated in t.php on line 11
priv0priv0D->m0D::s0priv1priv1D->m1D::s1priv2priv2D->m2D::s2
call_user_func(): Argument #1 ($callback) must be a valid callback, cannot access private method C::p()
ka0yka0zst0priv0D
ka1yka1zst1priv1D
ka2yka2zst2priv2D
inv0inv0inv0
inv1inv1inv1
00
0101
32[1,2,0]552
32[1,2,1]552
32[1,2,2]552
strlen(): Argument #1 ($string) must be of type string, array given
strlen() expects exactly 1 argument, 0 given
strlen() expects exactly 1 argument, 2 given
func_get_args() expects exactly 0 arguments, 1 given
Cannot call compact() dynamically
get_defined_vars() expects exactly 0 arguments, 1 given
eat a0
~a0
after
eat b0
~b0
after
eat a1
~a1
after
eat b1
~b1
after
bt@53 bt@53 bt@51,Bm@53 bt@51,Bs@53 bt@53,{closure:t.php:53}@53
bt@53 bt@53 bt@51,Bm@53 bt@51,Bs@53 bt@53,{closure:t.php:53}@53
t0 55
#0 t.php(57): thrower('t0')
#1 {main}
call_user_func_array(): Argument #1 ($callback) must be a valid callback, class B does not have a method "nope"
t1 55
#0 t.php(57): thrower('t1')
#1 {main}
call_user_func_array(): Argument #1 ($callback) must be a valid callback, class B does not have a method "nope"
end
~D
~D
"#
    );
    assert_eq!(status, 0);
}

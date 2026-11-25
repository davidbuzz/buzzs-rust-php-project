// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `$x |> f(?)`: php-src master's `zend_compile_pipe()` compiles a pipe
//! into a partial application with one placeholder as the plain call with
//! the piped value in the placeholder's place (`zend_partial_apply()` in
//! `Zend/zend_compile.c`). What a script can observe: the evaluation order
//! (the operand first, a variable read before the other arguments run),
//! by-reference parameters, backtraces without a Closure frame, and the
//! forms that still build the Closure.
//!
//! php 8.5 has the pipe operator and `f(...)`, but not partial application
//! (`?`): the first test's expectations are php 8.5's output (`-n`); the
//! others come from Zend/tests/partial_application/pipe_optimization_*.phpt
//! and the C source of php-src master.

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
fn run(source: &str) -> String {
    let sink = Sink::default();
    let options = VmOptions {
        ini: vec![("fatal_error_backtraces".to_string(), "0".to_string())],
    };
    let mut vm = common::vm_with_stdlib(&options, Box::new(sink.clone()));
    let _ = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

/// First-class-callable syntax on the right: php 8.5's output.
#[test]
fn pipe_into_first_class_callables() {
    let out = run(r#"<?php
function lhs() { echo "lhs\n"; return 5; }
function bt($x) { debug_print_backtrace(); return $x * 2; }
function byref(&$x) { $x++; }
class K {
    function m($x) { echo "m($x)\n"; return $this; }
    static function s($x) { echo "s($x)\n"; return 1; }
}
function obj() { echo "obj\n"; return new K; }
var_dump(lhs() |> bt(...));
lhs() |> obj()->m(...);
lhs() |> K::s(...);
$a = 1;
try { $a |> byref(...); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
var_dump($a);
$f = 'bt';
var_dump(3 |> $f(...));
echo 'abc' |> strlen(...), "\n";
echo 'abc' |> \strtoupper(...), "\n";
"#);
    assert_eq!(
        out,
        "lhs
#0 t.php(10): bt(5)
int(10)
lhs
obj
m(5)
lhs
s(5)
Error: byref(): Argument #1 ($x) could not be passed by reference
int(1)
#0 t.php(17): bt(3)
int(6)
3
ABC
"
    );
}

/// pipe_optimization_009 and _011: the operand, then the other arguments
/// in order; a variable operand is the value it had before they ran.
#[test]
fn operand_is_evaluated_before_the_other_arguments() {
    let out = run(r#"<?php
function lhs() { echo "lhs\n"; return 0; }
function arg1() { global $v; $v = 2; echo "arg1\n"; return 1; }
function arg2() { global $v; $v = 3; echo "arg2\n"; return 2; }
function h($a, $b, $c) { var_dump($a, $b, $c); }
lhs() |> h(arg1(), ?, arg2());
$v = 0;
$v |> h(arg1(), ?, arg2());
"#);
    assert_eq!(
        out,
        "lhs
arg1
arg2
int(1)
int(0)
int(2)
arg1
arg2
int(1)
int(0)
int(2)
"
    );
}

/// The direct call has no Closure frame in backtraces; named placeholders
/// become named arguments (pipe_optimization_006, _012, _014, _015);
/// pipe_optimization_010's by-reference argument; the operand itself sent
/// to a by-reference parameter.
#[test]
fn direct_calls_positional_named_and_by_reference() {
    let out = run(r#"<?php
function bt($a, $b) { debug_print_backtrace(); return $a - $b; }
var_dump(10 |> bt(?, 3));
var_dump(10 |> bt(3, ?));
var_dump(10 |> bt(b: ?, a: 1));
function g($a, $b = null, $c = null) { var_dump($a, $b, $c); }
3 |> g(1, c: ?);
try { 2 |> g(1, unknown: ?); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
function r(&$a, $b) { var_dump($a, $b); $a = 2; }
1 |> r($z, ?);
var_dump($z);
try { 5 |> r(?, 1); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
class C { function m($a, $b) { return "$a-$b"; } static function s($a, $b) { return "$a+$b"; } }
$o = new C;
echo 7 |> $o->m(?, 1), "\n";
echo 7 |> C::s(1, ?), "\n";
echo 'abc' |> str_pad(?, 5, '*'), "\n";
"#);
    assert_eq!(
        out,
        "#0 t.php(3): bt(10, 3)
int(7)
#0 t.php(4): bt(3, 10)
int(-7)
#0 t.php(5): bt(1, 10)
int(-9)
int(1)
NULL
int(3)
Error: Unknown named parameter $unknown
NULL
int(1)
int(2)
Error: r(): Argument #1 ($a) could not be passed by reference
7-1
1+7
abc**
"
    );
}

/// pipe_optimization_004 and _008: two placeholders, or `...` after a
/// named argument, still build the Closure and call it with the operand.
#[test]
fn other_partial_applications_are_called_as_closures() {
    let out = run(r#"<?php
function foo($a, $b) { var_dump($a, $b); }
try { 2 |> foo(?, ?); } catch (Throwable $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
try { 2 |> foo(a: 1, ...); } catch (Throwable $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        "ArgumentCountError: Too few arguments to function {closure:pfa:t.php:3}(), 1 passed in t.php on line 3 and exactly 2 expected
int(1)
int(2)
"
    );
}

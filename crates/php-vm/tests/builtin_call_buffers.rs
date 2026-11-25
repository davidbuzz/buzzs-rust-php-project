// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The argument vectors of builtin calls (`CallBufs::take_values`, lent
//! for a call and given back after it): builtins called from callbacks of
//! other builtins while those hold theirs (array_map, usort,
//! preg_replace_callback, array_filter), handlers that throw with their
//! arguments held (traces show them), more arguments than declared
//! parameters, none, frameless calls with temporaries in accepted and
//! declined shapes, an argument whose last handle the call frame holds,
//! and by-reference outputs, three rounds each so that every call runs
//! from the tape. Expected output from `/usr/bin/php8.5 -n` (its full
//! script path shortened to `t.php`).

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
fn builtin_calls_lend_and_return_their_argument_vectors() {
    let (status, out) = run(r#"<?php
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "~D{$this->n} in ", debug_backtrace()[1]['function'] ?? '-', "\n"; } }
function inner($s) { return strtoupper($s) . strlen($s) . implode('+', array_keys(['a' => 1, 'b' => 2])); }
function t() {
    $arr = ['x' => 'one', 'y' => 'two', 'z' => 'three'];
    $s = 'hello world';
    for ($i = 0; $i < 3; $i++) {
        // A builtin calling back into code that calls builtins.
        echo implode(',', array_map(fn($v) => inner($v), $arr)), "|", count($arr), "\n";
        $sorted = $arr;
        usort($sorted, fn($a, $b) => strcmp(strrev($a), strrev($b)) <=> 0);
        echo implode(',', $sorted), "\n";
        echo preg_replace_callback('/o/', fn($m) => strtoupper($m[0]) . strlen($s), $s), "\n";
        echo array_sum(array_map('strlen', array_filter($arr, fn($v) => strlen($v) > 3))), "\n";
        // Handlers that throw with their arguments held.
        try { str_repeat($s, -1); } catch (ValueError $e) { echo $e->getMessage(), "\n", $e->getTraceAsString(), "\n"; }
        try { array_combine(['a'], $arr); } catch (ValueError $e) { echo $e->getMessage(), "\n", $e->getTraceAsString(), "\n"; }
        try { array_map(function ($v) { throw new Exception("in $v"); }, $arr); } catch (Exception $e) { echo $e->getMessage(), "\n", $e->getTraceAsString(), "\n"; }
        // More arguments than declared parameters (variadics), and none.
        echo max(1, 5, 3, 9, 2), " ", sprintf('%s-%s-%s', 'a', $s, 3), " ", min($i, 7, -2), "\n";
        $stack = [];
        echo array_push($stack, 1, $s, $arr), " ", count($stack), "\n";
        echo pi(), " ", mt_getrandmax(), " ", count([]), " ", count(func_get_args()), "\n";
        // Frameless calls with temporaries, accepted and declined shapes.
        echo str_replace('l', 'L', $s . $i), " ", str_replace('l', 'LL', $s), " ", substr($s . 'abc', 1, 4), " ", substr($s, '2'), "\n";
        echo strpos($s . 'o', 'o', 5), " ", in_array('two', $arr), " ", in_array(1, [1, 2], true) ? 'y' : 'n', " ", trim(" $s "), " ", dirname('/a/b/c', 2), "\n";
        // An argument whose last handle the call frame holds.
        echo gettype(new D($i)), "\n";
        echo strlen(str_repeat('ab', $i + 1)), " ", strtolower(strtoupper($s)), "\n";
        // By-reference outputs, nested in a callback and at the top.
        echo implode(';', array_map(function ($v) { preg_match('/(\w)(\w)/', $v, $m); return implode('.', $m); }, $arr)), "\n";
        preg_match('/(\w+) (\w+)/', $s, $m);
        $m[] = count($m);
        echo implode('/', $m), " ", end($arr), " ", key($arr), "\n";
    }
}
t();
"#);
    assert_eq!(status, 0, "{out}");
    assert_eq!(
        out,
        r#"ONE3a+b,TWO3a+b,THREE5a+b|3
three,one,two
hellO11 wO11rld
5
str_repeat(): Argument #2 ($times) must be greater than or equal to 0
#0 t.php(16): str_repeat('hello world', -1)
#1 t.php(37): t()
#2 {main}
array_combine(): Argument #1 ($keys) and argument #2 ($values) must have the same number of elements
#0 t.php(17): array_combine(Array, Array)
#1 t.php(37): t()
#2 {main}
in one
#0 [internal function]: {closure:t():18}('one')
#1 t.php(18): array_map(Object(Closure), Array)
#2 t.php(37): t()
#3 {main}
9 a-hello world-3 -2
3 3
3.1415926535898 2147483647 0 0
heLLo worLd0 heLLLLo worLLd ello llo world
7 1 y hello world /a
~D0 in t
object
2 hello world
on.o.n;tw.t.w;th.t.h
hello world/hello/world/3 three z
ONE3a+b,TWO3a+b,THREE5a+b|3
three,one,two
hellO11 wO11rld
5
str_repeat(): Argument #2 ($times) must be greater than or equal to 0
#0 t.php(16): str_repeat('hello world', -1)
#1 t.php(37): t()
#2 {main}
array_combine(): Argument #1 ($keys) and argument #2 ($values) must have the same number of elements
#0 t.php(17): array_combine(Array, Array)
#1 t.php(37): t()
#2 {main}
in one
#0 [internal function]: {closure:t():18}('one')
#1 t.php(18): array_map(Object(Closure), Array)
#2 t.php(37): t()
#3 {main}
9 a-hello world-3 -2
3 3
3.1415926535898 2147483647 0 0
heLLo worLd1 heLLLLo worLLd ello llo world
7 1 y hello world /a
~D1 in t
object
4 hello world
on.o.n;tw.t.w;th.t.h
hello world/hello/world/3 three z
ONE3a+b,TWO3a+b,THREE5a+b|3
three,one,two
hellO11 wO11rld
5
str_repeat(): Argument #2 ($times) must be greater than or equal to 0
#0 t.php(16): str_repeat('hello world', -1)
#1 t.php(37): t()
#2 {main}
array_combine(): Argument #1 ($keys) and argument #2 ($values) must have the same number of elements
#0 t.php(17): array_combine(Array, Array)
#1 t.php(37): t()
#2 {main}
in one
#0 [internal function]: {closure:t():18}('one')
#1 t.php(18): array_map(Object(Closure), Array)
#2 t.php(37): t()
#3 {main}
9 a-hello world-3 -2
3 3
3.1415926535898 2147483647 0 0
heLLo worLd2 heLLLLo worLLd ello llo world
7 1 y hello world /a
~D2 in t
object
6 hello world
on.o.n;tw.t.w;th.t.h
hello world/hello/world/3 three z
"#
    );
}

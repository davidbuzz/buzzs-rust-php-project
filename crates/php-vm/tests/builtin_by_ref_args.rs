// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Builtin calls with by-reference parameters: called without the
//! by-reference argument, with it as a bare variable (undefined, static,
//! global), a property, an element, a value or a call's result; what the
//! variable holds afterwards, copy-on-write of an array held elsewhere,
//! references inside the array, the errors and the notice. Expected
//! output from the C implementation (`/usr/bin/php8.5 -n`, PHP 8.5.12)
//! (ref: Zend/zend_vm_def.h `ZEND_SEND_REF`, `ZEND_SEND_VAR_EX`,
//! `ZEND_SEND_FUNC_ARG`, `ZEND_DO_ICALL`).

#![allow(clippy::needless_raw_string_hashes)]

use php_runtime::{Value, ZStr};
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

/// `array_pop()` and `array_unshift()` as ext/standard registers them
/// over php-stdarr (the bare VM has neither), and a probe of a
/// `ZEND_ARG_SEND_PREFER_REF` parameter: "R" when it got a reference.
fn register(vm: &mut Vm) {
    vm.register_function("array_pop(array &$array): mixed", |vm, a| {
        let r = a
            .reference(0)
            .unwrap_or_else(|| php_runtime::PhpRef::new(a.at(0)));
        php_stdarr::stack::array_pop(&r).map_err(|e| vm.throw_runtime_error(e))
    });
    vm.register_function(
        "array_unshift(array &$array, mixed ...$values): int",
        |vm, a| {
            let r = a
                .reference(0)
                .unwrap_or_else(|| php_runtime::PhpRef::new(a.at(0)));
            let values: Vec<Value> = a.values.iter().skip(1).map(Value::deref).collect();
            php_stdarr::stack::array_unshift(&r, &values)
                .map(Value::Int)
                .map_err(|e| vm.throw_runtime_error(e))
        },
    );
    vm.register_function("prefer_probe(@prefer-ref array &$array): string", |_, a| {
        let mark = if a.reference(0).is_some() { "R" } else { "V" };
        Ok(Value::Str(ZStr::from(mark)))
    });
}

fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    register(&mut vm);
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

/// Each by-reference shape, in functions (on the tape) and at the top
/// level, against the C implementation's output.
#[test]
fn by_reference_arguments_of_builtins() {
    assert_eq!(run(SCRIPT), EXPECTED);
}

const SCRIPT: &str = r#"<?php
function plain() {
    $s = "a-b-a";
    echo str_replace("a", "x", $s), "\n";
    echo str_replace("a", "x", $s, $n), " $n\n";
    echo similar_text("World", "Word"), "\n";
    echo similar_text("World", "Word", $p), " ", round($p, 3), "\n";
    $stack = [1, 2, 3];
    var_dump(array_pop($stack), $stack);
    var_dump(array_unshift($stack, "z", "y"), $stack);
    $v = "12"; var_dump(settype($v, "integer"), $v);
}
plain();
function undef() {
    echo str_replace("a", "b", "aa", $c), " "; var_dump($c);
    try { array_pop($u); } catch (\TypeError $e) { echo $e->getMessage(), "\n"; }
    var_dump($u);
    var_dump(similar_text("ab", "ab", $q), $q);
}
undef();
function cow() {
    $a = [1, 2, 3]; $b = $a;
    array_pop($a);
    var_dump($a, $b);
    $x = [1]; $r = &$x; array_unshift($x, 0); var_dump($r);
    $inner = 5; $arr = [&$inner, 6]; $copy = $arr; array_pop($arr); $inner = 7; var_dump($copy);
}
cow();
function statics() {
    static $st = [1, 2];
    array_pop($st);
    return count($st);
}
echo statics(), statics(), "\n";
$g = [1, 2, 3];
function globals() { global $g; array_pop($g); return count($g); }
echo globals(), globals(), " ", count($g), "\n";
class C {
    public $p = [1, 2, 3];
    public $e = ['k' => [4, 5]];
    function m() {
        array_pop($this->p);
        array_pop($this->e['k']);
        echo str_replace("a", "b", "aaa", $this->n), "\n";
        var_dump($this->p, $this->e, $this->n);
    }
}
#[AllowDynamicProperties] class D extends C {}
(new D)->m();
function elems() {
    $list = [[1, 2], [3]];
    array_pop($list[0]);
    array_unshift($list[1], 9);
    $list['new'][] = 1;
    array_pop($list['none']);
    var_dump($list);
}
try { elems(); } catch (\Throwable $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
function errors() {
    try { array_pop([1, 2]); } catch (\Error $e) { echo $e->getMessage(), "\n"; }
    try { str_replace("a", "b", "c", 5); } catch (\Error $e) { echo $e->getMessage(), "\n"; }
    $x = 5;
    try { array_pop($x); } catch (\TypeError $e) { echo $e->getMessage(), "\n"; }
    var_dump($x);
    try { array_pop($nope); } catch (\TypeError $e) { echo $e->getMessage(), "\n"; }
    var_dump($nope);
    var_dump(array_pop(f()));
    try { echo str_replace([], "b", new stdClass, $cnt); } catch (\TypeError $e) { echo $e->getMessage(), "\n"; }
    var_dump($cnt);
}
function f() { return [1, 2]; }
errors();
function after() {
    $m = null;
    similar_text("abc", "abd", $m);
    echo round($m, 2), "\n";
    $st = [[1, 2]];
    $last = array_pop($st);
    echo $last[1], isset($last[2]) ? "y" : "n", count($st), "\n";
    $s = [1, 2, 3];
    array_pop($s);
    foreach ($s as $k => $v) echo "$k=$v ";
    $s[] = 9; $s[0]++; echo implode(",", $s), "\n";
    $t = $s; $t[] = 1; echo count($s), count($t), "\n";
    $s = null; var_dump($s);
}
after();
function refobs() {
    $a = [1, 2];
    array_pop($a);
    debug_zval_dump($a);
    var_dump(ReflectionReference::fromArrayElement(['x' => &$a], 'x') !== null);
    $b = &$a; $b[] = 5; var_dump($a);
    $f = function () use (&$a) { $a[] = 6; }; $f(); var_dump($a);
}
refobs();
"#;

const EXPECTED: &str = r#"x-b-x
x-b-x 2
4
4 88.889
int(3)
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
int(4)
array(4) {
  [0]=>
  string(1) "z"
  [1]=>
  string(1) "y"
  [2]=>
  int(1)
  [3]=>
  int(2)
}
bool(true)
int(12)
bb int(2)
array_pop(): Argument #1 ($array) must be of type array, null given
NULL
int(2)
float(100)
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
array(3) {
  [0]=>
  int(1)
  [1]=>
  int(2)
  [2]=>
  int(3)
}
array(2) {
  [0]=>
  int(0)
  [1]=>
  int(1)
}
array(2) {
  [0]=>
  &int(7)
  [1]=>
  int(6)
}
10
21 1
bbb
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
array(1) {
  ["k"]=>
  array(1) {
    [0]=>
    int(4)
  }
}
int(3)
TypeError: array_pop(): Argument #1 ($array) must be of type array, null given
array_pop(): Argument #1 ($array) could not be passed by reference
str_replace(): Argument #4 ($count) could not be passed by reference
array_pop(): Argument #1 ($array) must be of type array, int given
int(5)
array_pop(): Argument #1 ($array) must be of type array, null given
NULL

Notice: Only variables should be passed by reference in t.php on line 67
int(2)
str_replace(): Argument #3 ($subject) must be of type array|string, stdClass given
NULL
66.67
2n0
0=1 1=2 2,2,9
34
NULL
array(1) packed refcount(2){
  [0]=>
  int(1)
}
bool(true)
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(5)
}
array(3) {
  [0]=>
  int(1)
  [1]=>
  int(5)
  [2]=>
  int(6)
}
"#;

/// `ZEND_ARG_SEND_PREFER_REF`: a variable (a bare one, undefined, an
/// element, a property) by reference, a value or a call's result by value
/// without the error or the notice (`array_multisort([3, 1])` works).
#[test]
fn prefer_ref_parameters_take_variables_by_reference_only() {
    assert_eq!(
        run(r#"<?php
function g() { return [1]; }
class P { public $p = [1]; function m() { return prefer_probe($this->p); } }
function f() {
    $a = [1];
    echo prefer_probe($a), prefer_probe($undef), prefer_probe([1]), prefer_probe(g());
    $l = [[1]];
    echo prefer_probe($l[0]), (new P)->m(), "\n";
    var_dump($undef);
}
f();
"#),
        "RRVVRR\nNULL\n"
    );
}

/// Repeated calls with and without the by-reference argument in a loop
/// (the call site's prepared path, warm), on a variable that is then read,
/// written and iterated.
#[test]
fn by_reference_calls_in_a_loop() {
    assert_eq!(
        run(r#"<?php
function f() {
    $stack = [];
    $n = 0;
    $total = 0;
    for ($i = 0; $i < 3000; $i++) {
        array_unshift($stack, $i, $i + 1);
        $total += array_pop($stack);
        $s = str_replace("a", "b", "aXa");
        str_replace("a", "b", "aXa", $n);
        $total += $n + strlen($s);
    }
    echo count($stack), " ", $total, " ", $stack[0], " ", $stack[2999], "\n";
}
f();
"#),
        "3000 2265000 2999 1501\n"
    );
}

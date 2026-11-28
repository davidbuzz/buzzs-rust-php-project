// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A builtin's arguments as backtraces see them while its handler runs
//! from the tape's internal call (`exec/tape/calls.rs`, `call_native`,
//! whose handler reads pooled copies while the call frame keeps the sent
//! arguments): `debug_backtrace()` in a user error handler run for a
//! warning the builtin raises (`hex2bin()`), in a callback of
//! `array_map()` and in a builtin called from that callback (a nested
//! call taking the next pooled record), and an exception's trace
//! (`strpos()`'s `ValueError`), three rounds so that every call runs from
//! the tape. Expected output from `/usr/bin/php8.5 -n`.

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

fn run(source: &str) -> (i32, String) {
    let sink = Sink::default();
    let mut vm = common::vm_with_stdlib(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    (status, String::from_utf8(output).expect("UTF-8 output"))
}

#[test]
fn backtraces_show_a_builtin_call_frames_arguments() {
    let (status, out) = run(r#"<?php
function show($trace) {
    foreach ($trace as $i => $f) {
        echo "  #", $i, ' ', $f['function'], '(', implode(', ', array_map('json_encode', $f['args'] ?? [])), ")\n";
    }
}
set_error_handler(function ($no, $msg) {
    echo $msg, "\n";
    show(array_slice(debug_backtrace(), 1));
    return true;
});
function cb($v) {
    show(debug_backtrace());
    return strlen(hex2bin(str_repeat('a', $v))) * 2;
}
function t() {
    $s = 'abc';
    for ($i = 0; $i < 3; $i++) {
        var_dump(hex2bin($s));
        echo implode(',', array_map('cb', [$i, 5])), "\n";
        try {
            strpos($s, 'b', 10 + $i);
        } catch (ValueError $e) {
            echo $e->getMessage(), "\n";
            show($e->getTrace());
        }
        echo strtoupper($s), current([$s, 1]), "\n";
    }
}
t();
"#);
    assert_eq!(status, 0);
    assert_eq!(
        out,
        r#"hex2bin(): Hexadecimal input string must have an even length
  #0 hex2bin("abc")
  #1 t()
bool(false)
  #0 cb(0)
  #1 array_map("cb", [0,5])
  #2 t()
  #0 cb(5)
  #1 array_map("cb", [0,5])
  #2 t()
hex2bin(): Hexadecimal input string must have an even length
  #0 hex2bin("aaaaa")
  #1 cb(5)
  #2 array_map("cb", [0,5])
  #3 t()
0,0
strpos(): Argument #3 ($offset) must be contained in argument #1 ($haystack)
  #0 strpos("abc", "b", 10)
  #1 t()
ABCabc
hex2bin(): Hexadecimal input string must have an even length
  #0 hex2bin("abc")
  #1 t()
bool(false)
  #0 cb(1)
  #1 array_map("cb", [1,5])
  #2 t()
hex2bin(): Hexadecimal input string must have an even length
  #0 hex2bin("a")
  #1 cb(1)
  #2 array_map("cb", [1,5])
  #3 t()
  #0 cb(5)
  #1 array_map("cb", [1,5])
  #2 t()
hex2bin(): Hexadecimal input string must have an even length
  #0 hex2bin("aaaaa")
  #1 cb(5)
  #2 array_map("cb", [1,5])
  #3 t()
0,0
strpos(): Argument #3 ($offset) must be contained in argument #1 ($haystack)
  #0 strpos("abc", "b", 11)
  #1 t()
ABCabc
hex2bin(): Hexadecimal input string must have an even length
  #0 hex2bin("abc")
  #1 t()
bool(false)
  #0 cb(2)
  #1 array_map("cb", [2,5])
  #2 t()
  #0 cb(5)
  #1 array_map("cb", [2,5])
  #2 t()
hex2bin(): Hexadecimal input string must have an even length
  #0 hex2bin("aaaaa")
  #1 cb(5)
  #2 array_map("cb", [2,5])
  #3 t()
2,0
strpos(): Argument #3 ($offset) must be contained in argument #1 ($haystack)
  #0 strpos("abc", "b", 12)
  #1 t()
ABCabc
"#
    );
}

/// Unqualified builtin calls from namespaced code (`ZEND_INIT_NS_FCALL_BY_NAME`
/// resolved to the global function) take the frameless and fast-evaluated
/// forms as a qualified call does (php-src's `ZEND_JMP_FRAMELESS`); the
/// calls that can report keep their frame (traces name the builtin), and
/// a namespaced function defined later replaces the global one where the
/// site had not resolved it yet. Expected output: php-src's (PHP 8.5).
#[test]
fn namespaced_builtin_calls_match_php_src() {
    assert_eq!(
        run(r#"<?php
namespace App;
function run() {
  $a = ['k' => 1, 'z' => null]; $s = "hello world"; $r = [];
  for ($i = 0; $i < 3; $i++) {
    $r[] = array_key_exists('k', $a); $r[] = array_key_exists('z', $a); $r[] = array_key_exists('q', $a);
    $r[] = in_array(1, $a); $r[] = in_array('1', $a, true); $r[] = str_contains($s, 'wor'); $r[] = str_starts_with($s, 'he');
    $r[] = strlen($s); $r[] = is_string($s); $r[] = is_array($a); $r[] = count($a); $r[] = get_class(new \stdClass);
    $r[] = strpos($s, 'o'); $r[] = substr($s, 2, 3); $r[] = str_replace('l', 'L', $s); $r[] = implode(',', [1, 2]);
    $r[] = trim("  x "); $r[] = min(3, 1); $r[] = max([4, 9]); $r[] = dirname('/a/b/c'); $r[] = strtolower('AbC');
  }
  echo json_encode($r), "\n";
}
run();
function errs() {
  foreach ([fn() => strlen([]), fn() => array_key_exists([], []), fn() => in_array(1, 'x'), fn() => str_contains([], 'a'), fn() => substr('abc', 'x')] as $f) {
    try { $f(); } catch (\TypeError $e) { echo get_class($e), ": ", $e->getMessage(), " @", $e->getTrace()[0]['function'] ?? '-', "\n"; }
  }

  try { strpos('abc', 'b', 10); } catch (\ValueError $e) { echo $e->getMessage(), " @", $e->getTrace()[0]['function'], "\n"; }
  set_error_handler(function ($no, $msg, $f, $l) { echo "handler: $msg line $l\n"; return true; });
  $u = [1]; $x = array_key_exists(1.5, $u); var_dump($x);
  restore_error_handler();
}
errs();
function shadow() { $r = []; for ($i = 0; $i < 3; $i++) { $r[] = strlen('abcd'); if ($i == 0) { eval('namespace App; function strlen($s) { return "mine"; }'); } } echo json_encode($r), "\n"; }
shadow();
function strict_ints() { $r = []; for ($i = 0; $i < 2; $i++) { $r[] = strlen(12345); $r[] = str_repeat('a', '3'); } echo json_encode($r), "\n"; }
strict_ints();
"#).1,
        r#"[true,true,false,true,false,true,true,11,true,true,2,"stdClass",4,"llo","heLLo worLd","1,2","x",1,9,"\/a\/b","abc",true,true,false,true,false,true,true,11,true,true,2,"stdClass",4,"llo","heLLo worLd","1,2","x",1,9,"\/a\/b","abc",true,true,false,true,false,true,true,11,true,true,2,"stdClass",4,"llo","heLLo worLd","1,2","x",1,9,"\/a\/b","abc"]
TypeError: strlen(): Argument #1 ($string) must be of type string, array given @strlen
TypeError: array_key_exists(): Argument #1 ($key) must be a valid array offset type @array_key_exists
TypeError: in_array(): Argument #2 ($haystack) must be of type array, string given @in_array
TypeError: str_contains(): Argument #1 ($haystack) must be of type string, array given @str_contains
TypeError: substr(): Argument #2 ($offset) must be of type int, string given @substr
strpos(): Argument #3 ($offset) must be contained in argument #1 ($haystack) @strpos
handler: Implicit conversion from float 1.5 to int loses precision line 22
bool(false)
[4,4,4]
["mine","aaa","mine","aaa"]
"#
    );
}

/// Leaf builtins (`assert()`, `method_exists()`, `class_exists()`,
/// `defined()`, `explode()`, ...) whose arguments are computed after the
/// call starts run without a frame when nothing can see that; failing
/// assertions, errors and autoloading as before. Expected output:
/// php-src's (PHP 8.5), with master's `explode()` message.
#[test]
fn leaf_builtins_with_computed_arguments_match_php_src() {
    assert_eq!(
        run(r#"<?php
namespace App;
interface I {} class A implements I { function m() {} use T; } trait T { function t() {} } class B extends A {}
function run() {
  $o = new B; $r = [];
  for ($i = 0; $i < 3; $i++) {
    assert($o instanceof A); assert($o instanceof I, 'with message');
    $r[] = method_exists($o, 'm'); $r[] = method_exists($o, strtolower('T')); $r[] = method_exists(get_class($o), 'nope');
    $r[] = method_exists('App\B', 'm'); $r[] = class_exists('App\\' . 'A'); $r[] = class_exists('App\Nope' . $i, false);
    $r[] = defined('PHP_' . 'EOL'); $r[] = implode(',', array_keys(class_implements($o)));
    $r[] = explode(',', 'a,b' . $i);
    $r[] = array_values(array_filter([1, 0, 2])); $r[] = ucfirst(strtolower('XY')); $r[] = array_merge([1], [$i]);
  }
  echo json_encode($r), "\n";
}
run();
function failing() {
  ini_set('zend.assertions', 1);
  try { assert(1 > 2, 'custom'); } catch (\AssertionError $e) { echo "AE: ", $e->getMessage(), " @", $e->getTrace()[0]['function'] ?? '-', "\n"; }
  try { assert(strlen('x') > 5); } catch (\AssertionError $e) { echo "AE: ", $e->getMessage(), "\n"; }
  try { explode('' . '', 'abc'); } catch (\ValueError $e) { echo $e->getMessage(), " @", $e->getTrace()[0]['function'], "\n"; }
  try { method_exists(123 + 0, 'x'); } catch (\TypeError $e) { echo $e->getMessage(), "\n"; }
  spl_autoload_register(function ($c) { echo "autoload($c) "; });
  var_dump(class_exists('App\Auto' . 'X')); var_dump(method_exists('App\Auto' . 'Y', 'm'));
}
try { failing(); } catch (\ValueError $e) { echo "VE: ", $e->getMessage(), "\n"; }
"#).1,
        r#"[true,true,false,true,true,false,true,"App\\I",["a","b0"],[1,2],"Xy",[1,0],true,true,false,true,true,false,true,"App\\I",["a","b1"],[1,2],"Xy",[1,1],true,true,false,true,true,false,true,"App\\I",["a","b2"],[1,2],"Xy",[1,2]]
AE: custom @assert
AE: assert(strlen('x') > 5)
explode(): Argument #1 ($separator) must not be empty, use str_split() to split a string into characters @explode
method_exists(): Argument #1 ($object_or_class) must be of type object|string, int given
autoload(App\AutoX) bool(false)
autoload(App\AutoY) bool(false)
"#
    );
}

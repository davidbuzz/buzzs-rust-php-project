// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Frameless calls (`ZEND_FRAMELESS_ICALL_n`) of functions from the
//! extensions, by their global names and from namespaced code
//! (`ZEND_JMP_FRAMELESS`), run by the CLI with every extension loaded.

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

/// A namespaced call of `in_array()` or `str_contains()` runs the global
/// function in place unless the namespace declares one of that name when
/// the call site first runs; a declaration after that leaves the site as
/// it was, while a site that has not run yet calls the new function (both
/// as php-src's `ZEND_JMP_FRAMELESS` decides and caches). Loose and strict
/// `in_array()` over numeric strings, `null`, floats and `__toString()`
/// objects, arguments computed by calls and with side effects, the
/// `str_contains()` family with empty strings, other frameless functions,
/// a type error and a first-class callable. Expected output: php-src's
/// (PHP 8.4 CLI).
#[test]
#[allow(clippy::too_many_lines)] // the script and its expected output
fn namespaced_frameless_calls() {
    let src = r#"<?php
namespace A {
    function in_array($n, $h) { return "user A in_array"; }
    function probe() { return [in_array('x', ['x']), str_contains('abc', 'b')]; }
}
namespace B {
    function site1() { return in_array('x', ['x']); }
    function site2() { return in_array('x', ['x']); }
    function strs($h, $n) { return [str_contains($h, $n), str_starts_with($h, $n), str_ends_with($h, $n)]; }
}
namespace {
    var_dump(\A\probe());
    var_dump(\B\site1());
    eval('namespace B; function in_array($n, $h) { return "user B in_array"; }');
    var_dump(\B\site1(), \B\site2());
}
namespace C {
    class M { function getName() { return 'n' . ($GLOBALS['k'] ??= 0); } }
    $m = new M; $list = ['n0', 'x'];
    var_dump(in_array($m->getName(), $list), in_array($m->getName(), $list, true), in_array('1', [1, 2]), in_array('1', [1, 2], true), in_array(null, [0]), in_array('abc', [0]), in_array(0, ['a']), in_array('1e3', ['1000']), in_array(1.0, [1]));
    $i = 0; $arr = [1, 2, 3]; var_dump(in_array($i++, $arr), $i, in_array($arr[$i++] ?? 9, $arr), $i);
    var_dump(\B\strs('abc', ''), \B\strs('', ''), \B\strs('abc', 'abcd'), \B\strs('hello', 'lo'), \B\strs('hello', 'he'));
    class S { function __toString(): string { return 'x'; } }
    var_dump(in_array('x', [new S]), in_array(new S, ['x']));
    var_dump(in_array('a', ['a' => 1, 'b' => 'a']), strpos('abc', 'c'), substr('abcdef', 2, 2), max(1, 2), str_replace('a', 'b', 'aaa'), trim(' x '), strtolower('AB'));
    try { var_dump(in_array('x', 'notarray')); } catch (\TypeError $e) { echo $e->getMessage(), "\n"; }
    var_dump(str_contains(...)('abc', 'c'));
}
"#;
    let expected = r#"array(2) {
  [0]=>
  string(15) "user A in_array"
  [1]=>
  bool(true)
}
bool(true)
bool(true)
string(15) "user B in_array"
bool(true)
bool(true)
bool(true)
bool(false)
bool(true)
bool(false)
bool(false)
bool(true)
bool(true)
bool(false)
int(1)
bool(true)
int(2)
array(3) {
  [0]=>
  bool(true)
  [1]=>
  bool(true)
  [2]=>
  bool(true)
}
array(3) {
  [0]=>
  bool(true)
  [1]=>
  bool(true)
  [2]=>
  bool(true)
}
array(3) {
  [0]=>
  bool(false)
  [1]=>
  bool(false)
  [2]=>
  bool(false)
}
array(3) {
  [0]=>
  bool(true)
  [1]=>
  bool(false)
  [2]=>
  bool(true)
}
array(3) {
  [0]=>
  bool(true)
  [1]=>
  bool(true)
  [2]=>
  bool(false)
}
bool(true)
bool(true)
bool(true)
int(2)
string(2) "cd"
int(2)
string(3) "bbb"
string(1) "x"
string(2) "ab"
in_array(): Argument #2 ($haystack) must be of type array, string given
bool(true)
"#;
    assert_eq!(run(src), expected);
}

/// The type tests, `strlen()` and `count()` from namespaced code, answered
/// in place where nothing can see the call: every type of value, a
/// reference, a closed resource, a `Countable` object and recursive
/// counts (the handler), a non-countable value (the handler's frame in the
/// trace), `strlen()` of numbers, booleans and `null` (coerced and
/// deprecated), temporaries, a namespace's own `strlen()`, and
/// `strict_types`. Expected output: php-src's (PHP 8.4 CLI; the trace's
/// file paths are those of standard input).
#[test]
#[allow(clippy::too_many_lines)] // the scripts and their expected output
fn type_tests_strlen_and_count() {
    let src = r#"<?php
namespace App {
    class C implements \Countable { function count(): int { echo "[count]"; return 7; } }
    function t() {
        $f = fopen('php://memory', 'r'); $closed = fopen('php://memory', 'r'); fclose($closed);
        $vals = [null, true, 0, 1.5, 'a', '12', [], [1], new \stdClass, $f, $closed];
        foreach ($vals as $v) {
            $r = &$v;
            echo json_encode([is_int($v), is_string($v), is_array($v), is_bool($v), is_null($v), is_float($v), is_object($v), is_resource($v), is_scalar($v), is_numeric($v), is_int($r)]), "\n";
            unset($r);
        }
        var_dump(count([1, 2]), count([[1, 2], 3], COUNT_RECURSIVE), sizeof([1]), count(new C), strlen('abc'), strlen(''), strlen(42), strlen(1.5), strlen(true));
        try { count(new \stdClass); } catch (\TypeError $e) { echo $e->getMessage(), ' @', $e->getTrace()[0]['function'], "\n"; }
        try { count('str'); } catch (\TypeError $e) { echo $e->getMessage(), "\n"; }
        echo strlen(null), "\n";
        $t = 'tmp'; var_dump(strlen($t . 'x'), count(array_keys([1, 2, 3])), is_array(array_keys([])));
    }
}
namespace Shadow { function strlen($s) { return "shadow"; } function check() { return strlen('abc'); } }
namespace { App\t(); var_dump(Shadow\check()); }
"#;
    let expected = r#"[false,false,false,false,true,false,false,false,false,false,false]
[false,false,false,true,false,false,false,false,true,false,false]
[true,false,false,false,false,false,false,false,true,true,true]
[false,false,false,false,false,true,false,false,true,true,false]
[false,true,false,false,false,false,false,false,true,false,false]
[false,true,false,false,false,false,false,false,true,true,false]
[false,false,true,false,false,false,false,false,false,false,false]
[false,false,true,false,false,false,false,false,false,false,false]
[false,false,false,false,false,false,true,false,false,false,false]
[false,false,false,false,false,false,false,true,false,false,false]
[false,false,false,false,false,false,false,false,false,false,false]
[count]int(2)
int(4)
int(1)
int(7)
int(3)
int(0)
int(2)
int(3)
int(1)
count(): Argument #1 ($value) must be of type Countable|array, stdClass given @count
count(): Argument #1 ($value) must be of type Countable|array, string given

Deprecated: strlen(): Passing null to parameter #1 ($string) of type string is deprecated in tt.php on line 15
0
int(4)
int(3)
bool(true)
string(6) "shadow"
"#;
    assert_eq!(run(src).replace("Standard input code", "tt.php"), expected);
    let src = r#"<?php
declare(strict_types=1);
namespace S;
try { var_dump(strlen(42)); } catch (\TypeError $e) { echo $e->getMessage(), "\n"; }
var_dump(is_string('x'), count([1]));
"#;
    let expected = r"strlen(): Argument #1 ($string) must be of type string, int given
bool(true)
int(1)
";
    assert_eq!(run(src), expected);
}

/// `array_key_exists()` from namespaced code, answered in place for a
/// string or int key: numeric and non-numeric strings, negative ints,
/// references, the `key_exists()` alias; float and bool keys (the
/// handler), array and object keys and a non-array haystack (its type
/// errors), a namespace's own function. Then the illegal-key error of
/// each kind of call: by its global name and fully qualified (the
/// `ZEND_ARRAY_KEY_EXISTS` opcode's "Cannot access offset"), and named,
/// spread, through `call_user_func()` and dynamic (the function's).
/// Expected output: php-src's (PHP 8.4 CLI).
#[test]
#[allow(clippy::too_many_lines)] // the scripts and their expected output
fn array_key_exists_calls() {
    let src = r#"<?php
namespace App {
    function t() {
        $a = ['a' => 1, 1 => 2, '01' => 3, -5 => 4, '' => 5, 'n' => null];
        foreach (['a', 'b', '1', 1, '01', '-5', -5, '-0', '', 'n', 0] as $k) { echo var_export(array_key_exists($k, $a), true), ' '; }
        echo "\n";
        $r = &$a; $kr = 'a'; $kref = &$kr; var_dump(array_key_exists($kref, $r), key_exists('a', $a), array_key_exists(1, [1 => 0]));
        var_dump(array_key_exists(1.0, $a), array_key_exists(true, $a), array_key_exists(1.5, $a));
        foreach ([[1], new \stdClass] as $bad) { try { array_key_exists($bad, $a); } catch (\TypeError $e) { echo $e->getMessage(), "\n"; } }
        try { array_key_exists('a', new \ArrayObject(['a' => 1])); } catch (\TypeError $e) { echo $e->getMessage(), "\n"; }
    }
}
namespace Shadow { function array_key_exists($k, $a) { return 'shadow'; } function c() { return array_key_exists('x', []); } }
namespace { App\t(); var_dump(Shadow\c()); }
"#;
    let expected = r#"true false true true true true true false true true false 
bool(true)
bool(true)
bool(true)

Deprecated: Implicit conversion from float 1.5 to int loses precision in Standard input code on line 8
bool(true)
bool(true)
bool(true)
array_key_exists(): Argument #1 ($key) must be a valid array offset type
array_key_exists(): Argument #1 ($key) must be a valid array offset type
array_key_exists(): Argument #2 ($array) must be of type array, ArrayObject given
string(6) "shadow"
"#;
    assert_eq!(run(src), expected);
    let src = r#"<?php
$a = ['x' => 1];
foreach ([[1], new stdClass] as $bad) {
  try { array_key_exists($bad, $a); } catch (TypeError $e) { echo "plain: ", $e->getMessage(), "\n"; }
  try { \array_key_exists($bad, $a); } catch (TypeError $e) { echo "fq: ", $e->getMessage(), "\n"; }
  try { array_key_exists(key: $bad, array: $a); } catch (TypeError $e) { echo "named: ", $e->getMessage(), "\n"; }
  try { array_key_exists(...[$bad, $a]); } catch (TypeError $e) { echo "spread: ", $e->getMessage(), "\n"; }
  try { call_user_func('array_key_exists', $bad, $a); } catch (TypeError $e) { echo "cuf: ", $e->getMessage(), "\n"; }
  $f = 'array_key_exists'; try { $f($bad, $a); } catch (TypeError $e) { echo "dyn: ", $e->getMessage(), "\n"; }
}
"#;
    let expected = r"plain: Cannot access offset of type array on array
fq: Cannot access offset of type array on array
named: array_key_exists(): Argument #1 ($key) must be a valid array offset type
spread: array_key_exists(): Argument #1 ($key) must be a valid array offset type
cuf: array_key_exists(): Argument #1 ($key) must be a valid array offset type
dyn: array_key_exists(): Argument #1 ($key) must be a valid array offset type
plain: Cannot access offset of type stdClass on array
fq: Cannot access offset of type stdClass on array
named: array_key_exists(): Argument #1 ($key) must be a valid array offset type
spread: array_key_exists(): Argument #1 ($key) must be a valid array offset type
cuf: array_key_exists(): Argument #1 ($key) must be a valid array offset type
dyn: array_key_exists(): Argument #1 ($key) must be a valid array offset type
";
    assert_eq!(run(src), expected);
}

/// `strtolower()`/`strtoupper()` answered in place for a string:
/// empty, ASCII, mixed and non-ASCII bytes (only ASCII letters change),
/// already-lowercase input, a first-class callable and a fully qualified
/// call; other types (coerced, or a type error) take the handler.
/// Expected output: php-src's (PHP 8.4 CLI).
#[test]
fn case_conversions() {
    let src = r#"<?php
namespace App;
$m0 = memory_get_usage();
foreach (['', 'abc', 'ABC', 'MiXeD 123 ÄÖÜ', "\xff\x00A"] as $s) { $l = strtolower($s); $u = strtoupper($s); echo json_encode([bin2hex($l), bin2hex($u)]), "\n"; }
$x = 'already lower'; $y = strtolower($x); $x2 = 'ALREADY'; $z = strtoupper($x2);
var_dump($y, $z, strtolower(...)('Q'), \strtolower('W'));
try { strtolower([]); } catch (\TypeError $e) { echo $e->getMessage(), "\n"; }
var_dump(strtolower(12), strtoupper(1.5), strtolower(true));
"#;
    let expected = r#"["",""]
["616263","414243"]
["616263","414243"]
["6d697865642031323320c384c396c39c","4d495845442031323320c384c396c39c"]
["ff0061","ff0041"]
string(13) "already lower"
string(7) "ALREADY"
string(1) "q"
string(1) "w"
strtolower(): Argument #1 ($string) must be of type string, array given
string(2) "12"
string(3) "1.5"
string(1) "1"
"#;
    assert_eq!(run(src), expected);
}

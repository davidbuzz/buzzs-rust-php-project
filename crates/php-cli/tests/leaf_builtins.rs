// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Leaf builtins (`php_vm::special::leaf`): functions the tape calls
//! without a frame when their arguments have shapes for which the handler
//! reports nothing, throws nothing and runs no PHP code (`defined()`,
//! `microtime()`, `explode()`, `implode()`, `class_uses()`,
//! `method_exists()`, `class_exists()`, `assert()`, the `mb_*` string
//! functions in UTF-8, the array and string functions listed there).
//! Every other shape is the framed call's: an argument a parameter
//! coerces or refuses, an empty separator, an array or object element to
//! join, a class name that is not loaded (the autoloader runs, and a
//! missing class stays missing), a class constant, an unknown encoding,
//! an out-of-range offset. The error handler prints the backtrace's
//! function names, so a diagnostic raised without the builtin's frame
//! would show; each round repeats the calls on warm sites.

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

/// Expected output: PHP 8.5, except two messages that php-src master
/// words differently and this engine follows (`ext/standard/string.c`:
/// `explode()`'s empty separator, and `implode(array, string)`, which
/// master's parameter parsing refuses at the second argument).
#[test]
fn leaf_builtins_do_what_the_framed_call_does() {
    assert_eq!(run(SOURCE), EXPECTED);
}

const SOURCE: &str = r##"<?php
namespace App;
trait T {} interface I {} class P { use T; private function priv() {} function pub() {} } class C extends P implements I { function __toString() { return 'C!'; } }
enum E { case A; }
\spl_autoload_register(function ($c) { echo "[autoload $c]\n"; if ($c === 'App\Late') { eval('namespace App; class Late { function m() {} }'); } });
\set_error_handler(function ($no, $msg) { $t = \debug_backtrace(); echo "[handler: $msg; frames: ", \implode(',', \array_map(fn($f) => $f['function'], $t)), "]\n"; return true; });
\define('APP_CONST', 1); const NS_CONST = 2;
function show($f) { try { $r = $f(); if (\is_float($r)) { $r = $r > 1e9 ? 'float-now' : 'float'; } \var_dump($r); } catch (\Throwable $e) { echo \get_class($e), ': ', $e->getMessage(), ' @', $e->getTrace()[0]['function'] ?? '?', "\n"; } }
$o = new C; $arr = ['b' => 1, 'a' => 2, 3]; $list = [3, 1, 2]; $s = 'héllo wörld'; $nested = [1, [2], 3]; $objs = ['x', $o];
for ($round = 0; $round < 3; $round++) {
  echo "round $round\n";
  show(fn() => defined('APP_CONST')); show(fn() => \defined('App\NS_CONST')); show(fn() => defined('NOPE')); show(fn() => defined('App\C::X')); show(fn() => defined('App\Nope' . $round . '::X')); show(fn() => defined(5));
  show(fn() => \is_float(microtime(true))); show(fn() => \strlen(microtime()) > 10); show(fn() => \is_int(hrtime(true))); show(fn() => \count(hrtime())); show(fn() => time() > 1e9); show(fn() => microtime('x')); show(fn() => hrtime([]));
  show(fn() => explode(',', 'a,b,,c')); show(fn() => explode(',', 'a,b,c', 2)); show(fn() => explode(',', 'a,b,c', -1)); show(fn() => explode('', 'abc')); show(fn() => explode(',', 5)); show(fn() => explode(',', $o)); show(fn() => \explode('::', 'a::b'));
  show(fn() => implode(',', $list)); show(fn() => implode(', ', [1, 2.5, true, null, 'x'])); show(fn() => implode(',', $nested)); show(fn() => implode(',', $objs)); show(fn() => implode($list)); show(fn() => implode($list, ',')); show(fn() => \join('-', $list)); show(fn() => implode(',', 'x'));
  show(fn() => class_uses($o)); show(fn() => class_uses('App\P')); show(fn() => class_uses('\App\P')); show(fn() => class_uses('App\Missing' . $round)); show(fn() => class_uses('App\Missing', false)); show(fn() => class_parents($o)); show(fn() => class_implements('App\C')); show(fn() => class_uses(5));
  show(fn() => method_exists($o, 'pub')); show(fn() => method_exists($o, 'priv')); show(fn() => method_exists('App\C', 'priv')); show(fn() => method_exists('App\P', 'PRIV')); show(fn() => method_exists('App\Nope' . $round, 'x')); show(fn() => method_exists('App\Late', 'm')); show(fn() => method_exists(5, 'x')); show(fn() => method_exists(fn() => 1, '__invoke'));
  show(fn() => class_exists('App\C')); show(fn() => class_exists('App\I')); show(fn() => interface_exists('App\I')); show(fn() => trait_exists('App\T')); show(fn() => enum_exists('App\E')); show(fn() => class_exists('App\Gone' . $round)); show(fn() => class_exists('App\Gone', false)); show(fn() => class_exists('App\E', true));
  show(fn() => assert(true)); show(fn() => assert(1 === 1, 'desc')); show(fn() => assert(1)); show(fn() => assert($round > 5)); show(fn() => assert(false, 'custom message')); show(fn() => assert(false, new \LogicException('thrown')));
  show(fn() => mb_strlen($s)); show(fn() => mb_strlen($s, 'UTF-8')); show(fn() => mb_strlen($s, 'utf8')); show(fn() => mb_strlen($s, 'ISO-8859-1')); show(fn() => mb_strlen($s, 'nope')); show(fn() => mb_strlen(5)); show(fn() => mb_strlen([]));
  show(fn() => mb_substr($s, 1)); show(fn() => mb_substr($s, 1, 3)); show(fn() => mb_substr($s, -3, null)); show(fn() => mb_substr($s, 0, 2, 'UTF-8')); show(fn() => mb_substr($s, 0, 2, 'bad-enc')); show(fn() => mb_substr($s, '1'));
  show(fn() => mb_strpos($s, 'l')); show(fn() => mb_strrpos($s, 'l')); show(fn() => mb_strrpos($s, 'zz')); show(fn() => mb_strpos($s, 'l', 100)); show(fn() => mb_strrpos($s, 'l', 0, 'bad'));  show(fn() => mb_strpos($s, ''));
  show(fn() => array_values($arr)); show(fn() => array_keys($arr)); show(fn() => array_keys($arr, 2)); show(fn() => array_key_first($arr)); show(fn() => array_key_last($arr)); show(fn() => array_key_last([])); show(fn() => array_values('x')); show(fn() => array_keys(null));
  show(fn() => array_reverse($arr)); show(fn() => array_reverse($arr, true)); show(fn() => array_merge($arr, $list)); show(fn() => array_merge($arr)); show(fn() => array_merge()); show(fn() => array_merge($arr, 'x')); show(fn() => array_merge($list, $list, $arr));
  show(fn() => array_slice($list, 1)); show(fn() => array_slice($arr, 0, 2)); show(fn() => array_slice($arr, 1, null, true)); show(fn() => array_slice($list, 'x'));
  show(fn() => strcspn('hello', 'l')); show(fn() => strspn('42 apples', '0123456789')); show(fn() => strcmp('a', 'b')); show(fn() => strcasecmp('HELLO', 'hello')); show(fn() => strcspn('hello', 'l', 3)); show(fn() => strcmp('a', []));
  show(fn() => preg_quote('a.b*c')); show(fn() => preg_quote('a/b#c', '#')); show(fn() => preg_quote('a.b', null)); show(fn() => preg_quote([]));
  show(fn() => ucfirst('abc')); show(fn() => lcfirst('ABC')); show(fn() => crc32('hello')); show(fn() => function_exists('App\show')); show(fn() => function_exists('strlen')); show(fn() => ucfirst($o)); show(fn() => crc32(null));
  show(fn() => \call_user_func('explode', ',', 'a,b')); show(fn() => \array_map('ucfirst', ['a', 'b']));
}
class K { function explode($a, $b) { return "method"; } static function time() { return "static time"; } }
$k = new K; for ($i = 0; $i < 3; $i++) { echo $k->explode(',', 'a,b'), ' ', K::time(), "\n"; }
function ucfirst($x) { return 'ns ucfirst'; }
for ($i = 0; $i < 3; $i++) { echo ucfirst('a'), ' ', \ucfirst('a'), "\n"; }
"##;

const EXPECTED: &str = r##"round 0
bool(true)
bool(true)
bool(false)
bool(false)
[autoload App\Nope0]
bool(false)
bool(false)
bool(true)
bool(true)
bool(true)
int(2)
bool(true)
string(9) "float-now"
TypeError: hrtime(): Argument #1 ($as_number) must be of type bool, array given @hrtime
array(4) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
  [2]=>
  string(0) ""
  [3]=>
  string(1) "c"
}
array(2) {
  [0]=>
  string(1) "a"
  [1]=>
  string(3) "b,c"
}
array(2) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
}
ValueError: explode(): Argument #1 ($separator) must not be empty, use str_split() to split a string into characters @explode
array(1) {
  [0]=>
  string(1) "5"
}
array(1) {
  [0]=>
  string(2) "C!"
}
array(2) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
}
string(5) "3,1,2"
string(14) "1, 2.5, 1, , x"
[handler: Array to string conversion; frames: {closure:Standard input code:6},implode,{closure:Standard input code:15},App\show]
string(9) "1,Array,3"
string(4) "x,C!"
string(3) "312"
TypeError: implode(): Argument #2 ($array) must be of type ?array, string given @implode
string(5) "3-1-2"
TypeError: implode(): Argument #2 ($array) must be of type ?array, string given @implode
array(0) {
}
array(1) {
  ["App\T"]=>
  string(5) "App\T"
}
array(1) {
  ["App\T"]=>
  string(5) "App\T"
}
[autoload App\Missing0]
[handler: class_uses(): Class App\Missing0 does not exist and could not be loaded; frames: {closure:Standard input code:6},class_uses,{closure:Standard input code:16},App\show]
bool(false)
[handler: class_uses(): Class App\Missing does not exist; frames: {closure:Standard input code:6},class_uses,{closure:Standard input code:16},App\show]
bool(false)
array(1) {
  ["App\P"]=>
  string(5) "App\P"
}
array(2) {
  ["App\I"]=>
  string(5) "App\I"
  ["Stringable"]=>
  string(10) "Stringable"
}
TypeError: class_uses(): Argument #1 ($object_or_class) must be of type object|string, int given @class_uses
bool(true)
bool(true)
bool(false)
bool(true)
[autoload App\Nope0]
bool(false)
[autoload App\Late]
bool(true)
TypeError: method_exists(): Argument #1 ($object_or_class) must be of type object|string, int given @method_exists
bool(true)
bool(true)
bool(false)
bool(true)
bool(true)
bool(true)
[autoload App\Gone0]
bool(false)
bool(false)
bool(true)
bool(true)
bool(true)
bool(true)
AssertionError: assert($round > 5) @assert
AssertionError: custom message @assert
LogicException: thrown @{closure:Standard input code:19}
int(11)
int(11)
int(11)
int(13)
ValueError: mb_strlen(): Argument #2 ($encoding) must be a valid encoding, "nope" given @mb_strlen
int(1)
TypeError: mb_strlen(): Argument #1 ($string) must be of type string, array given @mb_strlen
string(12) "éllo wörld"
string(4) "éll"
string(3) "rld"
string(3) "hé"
ValueError: mb_substr(): Argument #4 ($encoding) must be a valid encoding, "bad-enc" given @mb_substr
string(12) "éllo wörld"
int(2)
int(9)
bool(false)
ValueError: mb_strpos(): Argument #3 ($offset) must be contained in argument #1 ($haystack) @mb_strpos
ValueError: mb_strrpos(): Argument #4 ($encoding) must be a valid encoding, "bad" given @mb_strrpos
int(0)
array(3) {
  [0]=>
  int(1)
  [1]=>
  int(2)
  [2]=>
  int(3)
}
array(3) {
  [0]=>
  string(1) "b"
  [1]=>
  string(1) "a"
  [2]=>
  int(0)
}
array(1) {
  [0]=>
  string(1) "a"
}
string(1) "b"
int(0)
NULL
TypeError: array_values(): Argument #1 ($array) must be of type array, string given @array_values
TypeError: array_keys(): Argument #1 ($array) must be of type array, null given @array_keys
array(3) {
  [0]=>
  int(3)
  ["a"]=>
  int(2)
  ["b"]=>
  int(1)
}
array(3) {
  [0]=>
  int(3)
  ["a"]=>
  int(2)
  ["b"]=>
  int(1)
}
array(6) {
  ["b"]=>
  int(1)
  ["a"]=>
  int(2)
  [0]=>
  int(3)
  [1]=>
  int(3)
  [2]=>
  int(1)
  [3]=>
  int(2)
}
array(3) {
  ["b"]=>
  int(1)
  ["a"]=>
  int(2)
  [0]=>
  int(3)
}
array(0) {
}
TypeError: array_merge(): Argument #2 must be of type array, string given @array_merge
array(9) {
  [0]=>
  int(3)
  [1]=>
  int(1)
  [2]=>
  int(2)
  [3]=>
  int(3)
  [4]=>
  int(1)
  [5]=>
  int(2)
  ["b"]=>
  int(1)
  ["a"]=>
  int(2)
  [6]=>
  int(3)
}
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
array(2) {
  ["b"]=>
  int(1)
  ["a"]=>
  int(2)
}
array(2) {
  ["a"]=>
  int(2)
  [0]=>
  int(3)
}
TypeError: array_slice(): Argument #2 ($offset) must be of type int, string given @array_slice
int(2)
int(2)
int(-1)
int(0)
int(0)
TypeError: strcmp(): Argument #2 ($string2) must be of type string, array given @strcmp
string(7) "a\.b\*c"
string(6) "a/b\#c"
string(4) "a\.b"
TypeError: preg_quote(): Argument #1 ($str) must be of type string, array given @preg_quote
string(10) "ns ucfirst"
string(3) "aBC"
int(907060870)
bool(true)
bool(true)
string(10) "ns ucfirst"
[handler: crc32(): Passing null to parameter #1 ($string) of type string is deprecated; frames: {closure:Standard input code:6},crc32,{closure:Standard input code:28},App\show]
int(0)
array(2) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
}
array(2) {
  [0]=>
  string(1) "A"
  [1]=>
  string(1) "B"
}
round 1
bool(true)
bool(true)
bool(false)
bool(false)
[autoload App\Nope1]
bool(false)
bool(false)
bool(true)
bool(true)
bool(true)
int(2)
bool(true)
string(9) "float-now"
TypeError: hrtime(): Argument #1 ($as_number) must be of type bool, array given @hrtime
array(4) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
  [2]=>
  string(0) ""
  [3]=>
  string(1) "c"
}
array(2) {
  [0]=>
  string(1) "a"
  [1]=>
  string(3) "b,c"
}
array(2) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
}
ValueError: explode(): Argument #1 ($separator) must not be empty, use str_split() to split a string into characters @explode
array(1) {
  [0]=>
  string(1) "5"
}
array(1) {
  [0]=>
  string(2) "C!"
}
array(2) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
}
string(5) "3,1,2"
string(14) "1, 2.5, 1, , x"
[handler: Array to string conversion; frames: {closure:Standard input code:6},implode,{closure:Standard input code:15},App\show]
string(9) "1,Array,3"
string(4) "x,C!"
string(3) "312"
TypeError: implode(): Argument #2 ($array) must be of type ?array, string given @implode
string(5) "3-1-2"
TypeError: implode(): Argument #2 ($array) must be of type ?array, string given @implode
array(0) {
}
array(1) {
  ["App\T"]=>
  string(5) "App\T"
}
array(1) {
  ["App\T"]=>
  string(5) "App\T"
}
[autoload App\Missing1]
[handler: class_uses(): Class App\Missing1 does not exist and could not be loaded; frames: {closure:Standard input code:6},class_uses,{closure:Standard input code:16},App\show]
bool(false)
[handler: class_uses(): Class App\Missing does not exist; frames: {closure:Standard input code:6},class_uses,{closure:Standard input code:16},App\show]
bool(false)
array(1) {
  ["App\P"]=>
  string(5) "App\P"
}
array(2) {
  ["App\I"]=>
  string(5) "App\I"
  ["Stringable"]=>
  string(10) "Stringable"
}
TypeError: class_uses(): Argument #1 ($object_or_class) must be of type object|string, int given @class_uses
bool(true)
bool(true)
bool(false)
bool(true)
[autoload App\Nope1]
bool(false)
bool(true)
TypeError: method_exists(): Argument #1 ($object_or_class) must be of type object|string, int given @method_exists
bool(true)
bool(true)
bool(false)
bool(true)
bool(true)
bool(true)
[autoload App\Gone1]
bool(false)
bool(false)
bool(true)
bool(true)
bool(true)
bool(true)
AssertionError: assert($round > 5) @assert
AssertionError: custom message @assert
LogicException: thrown @{closure:Standard input code:19}
int(11)
int(11)
int(11)
int(13)
ValueError: mb_strlen(): Argument #2 ($encoding) must be a valid encoding, "nope" given @mb_strlen
int(1)
TypeError: mb_strlen(): Argument #1 ($string) must be of type string, array given @mb_strlen
string(12) "éllo wörld"
string(4) "éll"
string(3) "rld"
string(3) "hé"
ValueError: mb_substr(): Argument #4 ($encoding) must be a valid encoding, "bad-enc" given @mb_substr
string(12) "éllo wörld"
int(2)
int(9)
bool(false)
ValueError: mb_strpos(): Argument #3 ($offset) must be contained in argument #1 ($haystack) @mb_strpos
ValueError: mb_strrpos(): Argument #4 ($encoding) must be a valid encoding, "bad" given @mb_strrpos
int(0)
array(3) {
  [0]=>
  int(1)
  [1]=>
  int(2)
  [2]=>
  int(3)
}
array(3) {
  [0]=>
  string(1) "b"
  [1]=>
  string(1) "a"
  [2]=>
  int(0)
}
array(1) {
  [0]=>
  string(1) "a"
}
string(1) "b"
int(0)
NULL
TypeError: array_values(): Argument #1 ($array) must be of type array, string given @array_values
TypeError: array_keys(): Argument #1 ($array) must be of type array, null given @array_keys
array(3) {
  [0]=>
  int(3)
  ["a"]=>
  int(2)
  ["b"]=>
  int(1)
}
array(3) {
  [0]=>
  int(3)
  ["a"]=>
  int(2)
  ["b"]=>
  int(1)
}
array(6) {
  ["b"]=>
  int(1)
  ["a"]=>
  int(2)
  [0]=>
  int(3)
  [1]=>
  int(3)
  [2]=>
  int(1)
  [3]=>
  int(2)
}
array(3) {
  ["b"]=>
  int(1)
  ["a"]=>
  int(2)
  [0]=>
  int(3)
}
array(0) {
}
TypeError: array_merge(): Argument #2 must be of type array, string given @array_merge
array(9) {
  [0]=>
  int(3)
  [1]=>
  int(1)
  [2]=>
  int(2)
  [3]=>
  int(3)
  [4]=>
  int(1)
  [5]=>
  int(2)
  ["b"]=>
  int(1)
  ["a"]=>
  int(2)
  [6]=>
  int(3)
}
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
array(2) {
  ["b"]=>
  int(1)
  ["a"]=>
  int(2)
}
array(2) {
  ["a"]=>
  int(2)
  [0]=>
  int(3)
}
TypeError: array_slice(): Argument #2 ($offset) must be of type int, string given @array_slice
int(2)
int(2)
int(-1)
int(0)
int(0)
TypeError: strcmp(): Argument #2 ($string2) must be of type string, array given @strcmp
string(7) "a\.b\*c"
string(6) "a/b\#c"
string(4) "a\.b"
TypeError: preg_quote(): Argument #1 ($str) must be of type string, array given @preg_quote
string(10) "ns ucfirst"
string(3) "aBC"
int(907060870)
bool(true)
bool(true)
string(10) "ns ucfirst"
[handler: crc32(): Passing null to parameter #1 ($string) of type string is deprecated; frames: {closure:Standard input code:6},crc32,{closure:Standard input code:28},App\show]
int(0)
array(2) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
}
array(2) {
  [0]=>
  string(1) "A"
  [1]=>
  string(1) "B"
}
round 2
bool(true)
bool(true)
bool(false)
bool(false)
[autoload App\Nope2]
bool(false)
bool(false)
bool(true)
bool(true)
bool(true)
int(2)
bool(true)
string(9) "float-now"
TypeError: hrtime(): Argument #1 ($as_number) must be of type bool, array given @hrtime
array(4) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
  [2]=>
  string(0) ""
  [3]=>
  string(1) "c"
}
array(2) {
  [0]=>
  string(1) "a"
  [1]=>
  string(3) "b,c"
}
array(2) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
}
ValueError: explode(): Argument #1 ($separator) must not be empty, use str_split() to split a string into characters @explode
array(1) {
  [0]=>
  string(1) "5"
}
array(1) {
  [0]=>
  string(2) "C!"
}
array(2) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
}
string(5) "3,1,2"
string(14) "1, 2.5, 1, , x"
[handler: Array to string conversion; frames: {closure:Standard input code:6},implode,{closure:Standard input code:15},App\show]
string(9) "1,Array,3"
string(4) "x,C!"
string(3) "312"
TypeError: implode(): Argument #2 ($array) must be of type ?array, string given @implode
string(5) "3-1-2"
TypeError: implode(): Argument #2 ($array) must be of type ?array, string given @implode
array(0) {
}
array(1) {
  ["App\T"]=>
  string(5) "App\T"
}
array(1) {
  ["App\T"]=>
  string(5) "App\T"
}
[autoload App\Missing2]
[handler: class_uses(): Class App\Missing2 does not exist and could not be loaded; frames: {closure:Standard input code:6},class_uses,{closure:Standard input code:16},App\show]
bool(false)
[handler: class_uses(): Class App\Missing does not exist; frames: {closure:Standard input code:6},class_uses,{closure:Standard input code:16},App\show]
bool(false)
array(1) {
  ["App\P"]=>
  string(5) "App\P"
}
array(2) {
  ["App\I"]=>
  string(5) "App\I"
  ["Stringable"]=>
  string(10) "Stringable"
}
TypeError: class_uses(): Argument #1 ($object_or_class) must be of type object|string, int given @class_uses
bool(true)
bool(true)
bool(false)
bool(true)
[autoload App\Nope2]
bool(false)
bool(true)
TypeError: method_exists(): Argument #1 ($object_or_class) must be of type object|string, int given @method_exists
bool(true)
bool(true)
bool(false)
bool(true)
bool(true)
bool(true)
[autoload App\Gone2]
bool(false)
bool(false)
bool(true)
bool(true)
bool(true)
bool(true)
AssertionError: assert($round > 5) @assert
AssertionError: custom message @assert
LogicException: thrown @{closure:Standard input code:19}
int(11)
int(11)
int(11)
int(13)
ValueError: mb_strlen(): Argument #2 ($encoding) must be a valid encoding, "nope" given @mb_strlen
int(1)
TypeError: mb_strlen(): Argument #1 ($string) must be of type string, array given @mb_strlen
string(12) "éllo wörld"
string(4) "éll"
string(3) "rld"
string(3) "hé"
ValueError: mb_substr(): Argument #4 ($encoding) must be a valid encoding, "bad-enc" given @mb_substr
string(12) "éllo wörld"
int(2)
int(9)
bool(false)
ValueError: mb_strpos(): Argument #3 ($offset) must be contained in argument #1 ($haystack) @mb_strpos
ValueError: mb_strrpos(): Argument #4 ($encoding) must be a valid encoding, "bad" given @mb_strrpos
int(0)
array(3) {
  [0]=>
  int(1)
  [1]=>
  int(2)
  [2]=>
  int(3)
}
array(3) {
  [0]=>
  string(1) "b"
  [1]=>
  string(1) "a"
  [2]=>
  int(0)
}
array(1) {
  [0]=>
  string(1) "a"
}
string(1) "b"
int(0)
NULL
TypeError: array_values(): Argument #1 ($array) must be of type array, string given @array_values
TypeError: array_keys(): Argument #1 ($array) must be of type array, null given @array_keys
array(3) {
  [0]=>
  int(3)
  ["a"]=>
  int(2)
  ["b"]=>
  int(1)
}
array(3) {
  [0]=>
  int(3)
  ["a"]=>
  int(2)
  ["b"]=>
  int(1)
}
array(6) {
  ["b"]=>
  int(1)
  ["a"]=>
  int(2)
  [0]=>
  int(3)
  [1]=>
  int(3)
  [2]=>
  int(1)
  [3]=>
  int(2)
}
array(3) {
  ["b"]=>
  int(1)
  ["a"]=>
  int(2)
  [0]=>
  int(3)
}
array(0) {
}
TypeError: array_merge(): Argument #2 must be of type array, string given @array_merge
array(9) {
  [0]=>
  int(3)
  [1]=>
  int(1)
  [2]=>
  int(2)
  [3]=>
  int(3)
  [4]=>
  int(1)
  [5]=>
  int(2)
  ["b"]=>
  int(1)
  ["a"]=>
  int(2)
  [6]=>
  int(3)
}
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
array(2) {
  ["b"]=>
  int(1)
  ["a"]=>
  int(2)
}
array(2) {
  ["a"]=>
  int(2)
  [0]=>
  int(3)
}
TypeError: array_slice(): Argument #2 ($offset) must be of type int, string given @array_slice
int(2)
int(2)
int(-1)
int(0)
int(0)
TypeError: strcmp(): Argument #2 ($string2) must be of type string, array given @strcmp
string(7) "a\.b\*c"
string(6) "a/b\#c"
string(4) "a\.b"
TypeError: preg_quote(): Argument #1 ($str) must be of type string, array given @preg_quote
string(10) "ns ucfirst"
string(3) "aBC"
int(907060870)
bool(true)
bool(true)
string(10) "ns ucfirst"
[handler: crc32(): Passing null to parameter #1 ($string) of type string is deprecated; frames: {closure:Standard input code:6},crc32,{closure:Standard input code:28},App\show]
int(0)
array(2) {
  [0]=>
  string(1) "a"
  [1]=>
  string(1) "b"
}
array(2) {
  [0]=>
  string(1) "A"
  [1]=>
  string(1) "B"
}
method static time
method static time
method static time
ns ucfirst A
ns ucfirst A
ns ucfirst A
"##;

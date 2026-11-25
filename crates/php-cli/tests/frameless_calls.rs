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

/// Two- and three-argument frameless calls from namespaced code answered
/// with their arguments read in place: `substr()` with every kind of
/// offset and length, `strpos()` with offsets inside and outside the
/// subject (the `ValueError` from the handler's frame), `max()`/`min()`
/// of mixed types, `strstr()`, `strtr()`, `str_replace()`, `trim()`,
/// `dirname()`, `intdiv()`, temporaries, coerced arguments and a spread.
/// Expected output: php-src's (PHP 8.4 CLI).
#[test]
#[allow(clippy::too_many_lines)] // the script and its expected output
fn string_and_number_frameless_calls() {
    let src = r#"<?php
namespace App;
$s = 'hello world'; $e = '';
foreach ([[0], [3], [-3], [20], [-20], [3, 2], [3, -2], [3, null], [3, 0], [-5, 3], [0, 100]] as $a) { echo json_encode(substr($s, ...$a)), ' ', json_encode(count($a) == 1 ? substr($s, $a[0]) : substr($s, $a[0], $a[1])), ' '; } echo "\n";
var_dump(substr($e, 0), substr($e, 1, 1), substr('abc', 1, 1));
foreach ([['o'], ['o', 5], ['o', -3], ['z'], ['', 0], ['hello'], ['o', 11]] as $a) { echo json_encode(count($a) == 1 ? strpos($s, $a[0]) : strpos($s, $a[0], $a[1])), ' '; } echo "\n";
try { strpos($s, 'o', 12); } catch (\ValueError $e2) { echo $e2->getMessage(), ' @', $e2->getTrace()[0]['function'], "\n"; }
try { strpos($s, 'o', -12); } catch (\ValueError $e2) { echo $e2->getMessage(), "\n"; }
var_dump(max(1, 2), max('a', 'b'), max(1.5, 1), max(true, 2), min(null, 0), min('10', 9), max('abc', 0), max([1], 2), min(-0.0, 0));
var_dump(strstr('user@example.com', '@'), strstr('user@example.com', '@', true), strtr('abc', 'ab', 'xy'), str_replace('l', 'L', $s), trim(' x ', ' '), dirname('/a/b/c', 2), intdiv(7, 2), str_contains($s, ''), str_ends_with($s, 'world'));
$t1 = 'hel'; var_dump(strpos($t1 . 'lo', 'l'), substr($t1 . 'xyz', 2, 2), max($t1 . 'a', 'help'));
try { intdiv(1, 0); } catch (\DivisionByZeroError $e3) { echo $e3->getMessage(), "\n"; }
var_dump(substr($s, '1'), strpos($s, 111), max(...[3, 4]));
"#;
    let expected = r#""hello world" "hello world" "lo world" "lo world" "rld" "rld" "" "" "hello world" "hello world" "lo" "lo" "lo wor" "lo wor" "lo world" "lo world" "" "" "wor" "wor" "hello world" "hello world" 
string(0) ""
string(0) ""
string(1) "b"
4 7 false false 0 0 false 
strpos(): Argument #3 ($offset) must be contained in argument #1 ($haystack) @strpos
strpos(): Argument #3 ($offset) must be contained in argument #1 ($haystack)
int(2)
string(1) "b"
float(1.5)
bool(true)
int(0)
int(9)
string(3) "abc"
array(1) {
  [0]=>
  int(1)
}
int(0)
string(12) "@example.com"
string(4) "user"
string(3) "xyc"
string(11) "heLLo worLd"
string(1) "x"
string(2) "/a"
int(3)
bool(true)
bool(true)
int(2)
string(2) "lx"
string(4) "help"
Division by zero
string(10) "ello world"
bool(false)
int(4)
"#;
    assert_eq!(run(src), expected);
}

/// `$this->name` as a call's argument, sent from the property cache's read
/// hit when the call is prepared: declared public, protected and private
/// properties (a parent's private one from the parent's method), a
/// dynamic property, an undeclared one (`__get()`), an uninitialized typed
/// one (its error), a reference, a by-reference parameter (the generic
/// send), dimensions of a property, constructor arguments of `new`, and
/// repeated calls once the cache is filled. Expected output: php-src's
/// (PHP 8.4 CLI).
#[test]
#[allow(clippy::too_many_lines)] // the script and its expected output
fn this_property_arguments() {
    let src = r#"<?php
namespace App;
#[\AllowDynamicProperties]
class Base { private $secret = 'base-private'; protected $prot = 'prot'; public $pub = ['k' => 'v']; public int $typed; public $ref;
  function __construct() { $x = 'refd'; $this->ref = &$x; }
  function viaBase() { return [strlen($this->secret), self::id($this->secret)]; }
  static function id($v) { return $v; } static function byRef(&$v) { $v = 'changed'; return 'r'; } }
class Child extends Base {
  private $secret = 'child-private';
  function __get($n) { echo "[__get $n]"; return "magic-$n"; }
  function run() {
    $this->dyn = 'dynamic';
    var_dump(strlen($this->pub['k']), self::id($this->prot), self::id($this->secret), self::id($this->dyn), self::id($this->missing), self::id($this->ref), in_array('v', $this->pub), count($this->pub));
    try { var_dump(self::id($this->typed)); } catch (\Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
    var_dump(self::byRef($this->prot), $this->prot, $this->viaBase());
    $o = new \ArrayObject($this->pub); var_dump(count($o), new Pair($this->pub['k'], $this->prot));
    for ($i = 0; $i < 3; $i++) { echo self::id($this->secret), strlen($this->dyn), ' '; } echo "\n";
  }
}
class Pair { function __construct(public $a, public $b) {} }
(new Child)->run();
"#;
    let expected = r#"[__get missing]int(1)
string(4) "prot"
string(13) "child-private"
string(7) "dynamic"
string(13) "magic-missing"
string(4) "refd"
bool(true)
int(1)
Error: Typed property App\Base::$typed must not be accessed before initialization
string(1) "r"
string(7) "changed"
array(2) {
  [0]=>
  int(12)
  [1]=>
  string(12) "base-private"
}
int(1)
object(App\Pair)#4 (2) {
  ["a"]=>
  string(1) "v"
  ["b"]=>
  string(7) "changed"
}
child-private7 child-private7 child-private7 
"#;
    assert_eq!(run(src), expected);
}

/// Static property elements read and tested in place (`isset(self::$a[$k])`,
/// `static::$a[k]`, `parent::$p[k]`, `A::$a[k]`) and `self::class` /
/// `static::class` / `parent::class` without the generic ops: int,
/// numeric-string and string keys, null and reference elements, missing
/// ones (the read's warning, `??`), private and protected properties from
/// inside and outside (the access errors), a string property's offset, an
/// uninitialized typed property, float and bool keys, subclasses, traits
/// and static closures. Expected output: php-src's (PHP 8.4 CLI).
#[test]
#[allow(clippy::too_many_lines)] // the script and its expected output
fn static_property_elements_and_class_names() {
    let src = r#"<?php
namespace App;
trait T { function tname() { return [self::class, static::class]; } }
class P { protected static $prot = ['p' => 1]; static function pn() { return static::class; } }
class A extends P { use T;
  public static $a = ['k' => 1, 5 => 'five', 'n' => null, '7' => 'seven'];
  private static $priv = ['x' => 'secret'];
  public static string $s = 'str';
  public static array $typed;
  public static $r;
  static function t() {
    $x = 'refd'; self::$a['ref'] = &$x;
    foreach (['k', 5, '5', 'n', 'missing', '7', 7, 'ref'] as $k) { echo json_encode([isset(self::$a[$k]), isset(static::$a[$k]), isset(A::$a[$k])]), ' '; }
    echo "\n";
    var_dump(self::$a['k'], static::$a[5], self::$a['5'], self::$a['n'], self::$a['ref'], isset(self::$priv['x']), self::$priv['x'], isset(parent::$prot['p']), parent::$prot['p'], isset(self::$s[0]), self::$s[1]);
    var_dump(self::$a['nope'] ?? 'dflt');
    echo self::$a['missing'], "|\n";
    try { var_dump(isset(self::$typed['a'])); var_dump(self::$typed['a']); } catch (\Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
    var_dump(isset(self::$a[1.5]), isset(self::$a[true]));
    var_dump(self::class, static::class, parent::class, (new static)->tname(), static::pn(), (static function () { return [self::class, static::class]; })());
  }
}
class B extends A {}
A::t(); B::t();
var_dump(isset(A::$a['k']), A::$a['k']);
try { var_dump(isset(A::$priv['x'])); } catch (\Error $e) { echo $e->getMessage(), "\n"; }
try { echo A::$priv['x']; } catch (\Error $e) { echo $e->getMessage(), "\n"; }
"#;
    let expected = r#"[true,true,true] [true,true,true] [true,true,true] [false,false,false] [false,false,false] [true,true,true] [true,true,true] [true,true,true] 
int(1)
string(4) "five"
string(4) "five"
NULL
string(4) "refd"
bool(true)
string(6) "secret"
bool(true)
int(1)
bool(true)
string(1) "t"
string(4) "dflt"

Warning: Undefined array key "missing" in Standard input code on line 17
|
bool(false)
Error: Typed static property App\A::$typed must not be accessed before initialization

Deprecated: Implicit conversion from float 1.5 to int loses precision in Standard input code on line 19
bool(false)
bool(false)
string(5) "App\A"
string(5) "App\A"
string(5) "App\P"
array(2) {
  [0]=>
  string(5) "App\A"
  [1]=>
  string(5) "App\A"
}
string(5) "App\A"
array(2) {
  [0]=>
  string(5) "App\A"
  [1]=>
  string(5) "App\A"
}
[true,true,true] [true,true,true] [true,true,true] [false,false,false] [false,false,false] [true,true,true] [true,true,true] [true,true,true] 
int(1)
string(4) "five"
string(4) "five"
NULL
string(4) "refd"
bool(true)
string(6) "secret"
bool(true)
int(1)
bool(true)
string(1) "t"
string(4) "dflt"

Warning: Undefined array key "missing" in Standard input code on line 17
|
bool(false)
Error: Typed static property App\A::$typed must not be accessed before initialization

Deprecated: Implicit conversion from float 1.5 to int loses precision in Standard input code on line 19
bool(false)
bool(false)
string(5) "App\A"
string(5) "App\B"
string(5) "App\P"
array(2) {
  [0]=>
  string(5) "App\A"
  [1]=>
  string(5) "App\B"
}
string(5) "App\B"
array(2) {
  [0]=>
  string(5) "App\A"
  [1]=>
  string(5) "App\B"
}
bool(true)
int(1)
bool(false)
Cannot access private property App\A::$priv
"#;
    assert_eq!(run(src), expected);
}

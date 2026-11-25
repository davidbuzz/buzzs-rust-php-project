// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Frameless builtin calls (php-src `ZEND_FRAMELESS_ICALL_n`): which call
//! shapes run without the builtin frame, and that every shape which can
//! warn or throw keeps the frame, so traces, error handlers and
//! `__toString()` callbacks still see `#0 file(line): strpos(...)`.
//! Expected outputs are `/usr/bin/php8.5 -n`'s, with php-src master's
//! closure names and frameless `min()` tie (`min(null, false)` is the
//! second argument, `ZEND_FRAMELESS_FUNCTION(min, 2)`).
use php_runtime::{Value, ZStr};
use php_vm::ext::{Args, Extension, ModuleBuilder, VmResult};
use php_vm::vm::FrameKind;
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

fn output(vm: Vm, sink: &Sink, status: i32) -> String {
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

/// `B` when the running builtin has a frame of its own, `F` (frameless)
/// when the top frame is the caller's.
#[allow(clippy::unnecessary_wraps)] // builtin signature
fn frame_mark(vm: &mut Vm, _args: &mut Args) -> VmResult<Value> {
    let framed = vm
        .frames
        .last()
        .is_some_and(|f| f.kind == FrameKind::Builtin);
    Ok(Value::Str(ZStr::from(if framed { "B" } else { "F" })))
}

/// Stand-ins with the ext/standard signatures that report how they were
/// called.
struct Probe;

impl Extension for Probe {
    fn name(&self) -> &'static str {
        "frameless_probe"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        for sig in [
            "strpos(string $haystack, string $needle, int $offset = 0): int|false",
            "trim(string $string, string $characters = \" \\n\\r\\t\\v\\0\"): string",
            "dirname(string $path, int $levels = 1): string",
            "in_array(mixed $needle, array $haystack, bool $strict = false): bool",
            "str_replace(array|string $search, array|string $replace, string|array $subject, &$count = null): string|array",
            "strtr(string $string, mixed $from, ?string $to = null): string",
            "max(mixed $value, mixed ...$values): mixed",
            "strtolower(string $string): string",
            "is_numeric(mixed $value): bool",
            "chr(int $codepoint): string",
            "ord(string $character): int",
        ] {
            m.function(sig, frame_mark);
        }
    }
}

#[test]
fn only_call_shapes_without_an_error_path_skip_the_frame() {
    let sink = Sink::default();
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &[&Probe]);
    let status = vm.run_script(PROBE.as_bytes(), "probe.php");
    // strpos: offsets outside the haystack throw, "1" is coerced; trim: a
    // `..` range can warn; dirname: levels below 1 throw; in_array: loose
    // comparisons with objects (or over long haystacks) and non-scalar
    // needles; str_replace: growing replacements reserve memory first;
    // dynamic calls and named arguments always push the frame. Calls the VM
    // answers without the handler show the real result instead of a mark:
    // `chr()` of 0..=255 and `ord()` of one byte (`frameless_inline`),
    // `in_array()` of the shapes it accepts (`Vm::frameless_two`: `1` or
    // nothing where it had no frame), `strtolower()` of a string (`a`),
    // `strpos()` once its name is in the run-time cache (`1`, and nothing
    // for `false`: the first call, which fills the cache, has its handler
    // run frameless), and
    // `is_numeric()` of an array, which is not a frameless shape but takes
    // the one-argument builtin fast path (`fast_builtin_eval`: `false`,
    // echoed as ""). Neither pushes a frame.
    assert_eq!(
        output(vm, &sink, status),
        "F1B1BB\nFFFFBB\nFFFBB\n1BB1B1B\nFBBB\nFBB\nFFBB\naBF\nAB65B\nFB\nB\nB\n"
    );
}

const PROBE: &str = r#"<?php
echo strpos("abc", "b"), strpos("abc", "b", 1), strpos("abc", "b", 3), strpos("abc", "b", 4), strpos("abc", "b", -3), strpos("abc", "b", -4), strpos("abc", "b", "1"), "\n";
echo trim("a"), trim("a", "xy"), trim("a", "x"), trim("a", ""), trim("a", "a..z"), trim("a", 1), "\n";
echo dirname("/a"), dirname("/a", 1), dirname("/a", 2), dirname("/a", 0), dirname("/a", "2"), "\n";
$o = new stdClass; $one = [1]; $obj = [$o]; $nested = [[1], "b"]; $long = [];
for ($i = 0; $i < 40; $i++) { $long[] = $i; }
echo in_array(1, $one), in_array(1, $obj), in_array(1, $obj, true), in_array($o, $one), in_array(1, $one, false), in_array("a", $nested), in_array(1, $long), in_array(1, $long, true), in_array(1, $one, 1), "\n";
echo str_replace("a", "b", "xax"), str_replace("a", "bb", "xax"), str_replace(["a"], "b", "x"), str_replace("a", "b", ["x"]), "\n";
echo strtr("abc", "a", "b"), strtr("abc", ["a" => "b"]), strtr("abc", "a", null), "\n";
echo max(1, 2), max(1, "x"), max(1, $one), max(1, 2, 3), "\n";
echo strtolower("A"), strtolower(1), is_numeric("1"), is_numeric($one), "\n";
$c = 65; $s = "A"; $e = "";
echo chr($c), chr(256 + $c), ord($s), ord($e), "\n";
function f($h) { return strpos($h, "b") . strpos($h, "b", 9); }
echo f("abc"), "\n";
$f = 'strpos';
echo $f("abc", "b"), "\n";
echo strpos(haystack: "abc", needle: "b"), "\n";
"#;

/// `in_array()`, `max()` and `min()` as ext/standard registers them over
/// php-stdarr (the bare VM has none of them).
fn register_array_builtins(vm: &mut Vm) {
    vm.register_function(
        "in_array(mixed $needle, array $haystack, bool $strict = false): bool",
        |vm, a| {
            let (needle, haystack) = (a.at(0), a.array(1));
            php_stdarr::basic::in_array(vm, &needle, &haystack, a.bool(2))
                .map(Value::Bool)
                .map_err(|e| vm.throw_runtime_error(e))
        },
    );
    vm.register_function("max(mixed $value, mixed ...$values): mixed", |vm, a| {
        let values: Vec<Value> = a.values.iter().map(Value::deref).collect();
        let r = match values.as_slice() {
            [x, y] => php_stdarr::basic::max2(vm, x, y),
            _ => php_stdarr::basic::max(vm, &values),
        };
        r.map_err(|e| vm.throw_runtime_error(e))
    });
    vm.register_function("min(mixed $value, mixed ...$values): mixed", |vm, a| {
        let values: Vec<Value> = a.values.iter().map(Value::deref).collect();
        let r = match values.as_slice() {
            [x, y] => php_stdarr::basic::min2(vm, x, y),
            _ => php_stdarr::basic::min(vm, &values),
        };
        r.map_err(|e| vm.throw_runtime_error(e))
    });
}

#[test]
fn frameless_results_match_and_error_paths_keep_the_builtin_frame() {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    register_array_builtins(&mut vm);
    let status = vm.run_script(SCRIPT.as_bytes(), "fl.php");
    assert_eq!(output(vm, &sink, status), EXPECTED);
}

const SCRIPT: &str = r#"<?php
function frames() { $n = []; foreach (debug_backtrace() as $f) { $n[] = $f['function']; } return implode(',', $n); }
set_error_handler(function ($no, $msg) { echo "handler: $msg [", frames(), "]\n"; return true; });
function f($s, $o) { return strpos($s, "b", $o); }
foreach ([10, -4] as $o) {
    try { f("abc", $o); } catch (ValueError $e) { echo $e->getMessage(), "\n", $e->getTraceAsString(), "\n"; }
}
var_dump(f("abc", 1), f("abc", -3), f("abc", 3), strpos("", "", 0), strpos("abc", "c"));
function g($s) { return trim($s, "a..") . "|" . ltrim($s, "..a") . "|" . rtrim($s, "z..a"); }
echo g("abc"), "\n";
echo trim(" a "), "|", trim("xabcx", "x"), "|", trim("xyabcyx", "xy"), "|", trim("a.b", "."), "|", rtrim("ab  "), "|", ltrim("  ab"), "\n";
function h($p, $l) { return dirname($p, $l); }
try { h("/a/b/c", 0); } catch (ValueError $e) { echo $e->getMessage(), "\n", $e->getTraceAsString(), "\n"; }
echo h("/a/b/c", 2), "|", dirname("/a/b/c"), "|", dirname("/a/b/c", 1), "\n";
class S { function __toString(): string { echo "[toString ", frames(), "]"; return "1"; } }
function k($n, $h) { return in_array($n, $h); }
$s1 = [new S];
var_dump(k("1", $s1), k(1, $s1), k(2, [1, 2]), k("2", [1, 2]), k(null, [0]), k(2, [1, "2"]));
$h = [1, "2"];
var_dump(in_array(2, $h, true), in_array("2", $h, true), in_array(2, $h, false), in_array(3, $h));
var_dump(str_replace("a", "bb", "xax"), str_replace("ab", "c", "xabx"), str_replace("", "c", "x"), str_replace("a", "b", ""));
var_dump(strtr("abc", "ab", "xy"), strtr("abc", "abc", "x"), strtr("", "a", "b"));
var_dump(max(1, "2"), min("a", 0), max(1.5, 1), min(null, false), max("abc", "abd"));
$c = 66; $s = "A";
var_dump(ord($s), chr($c), is_numeric("1e3"), is_numeric(" 1"), is_numeric("x"), strtolower("AbC"), strtoupper("aBc"), dechex(255), abs(-5), abs(-2.5));
var_dump(substr("abc", 1), substr("abc", 1, null), substr("abc", 0, -1), strstr("abc", "b"), strstr("abc", "b", true), strstr("abc", "z"));
var_dump(str_contains("abc", "b"), str_starts_with("abc", "a"), str_ends_with("abc", "c"), str_contains("", ""));
"#;

const EXPECTED: &str = r#"strpos(): Argument #3 ($offset) must be contained in argument #1 ($haystack)
#0 fl.php(4): strpos('abc', 'b', 10)
#1 fl.php(6): f('abc', 10)
#2 {main}
strpos(): Argument #3 ($offset) must be contained in argument #1 ($haystack)
#0 fl.php(4): strpos('abc', 'b', -4)
#1 fl.php(6): f('abc', -4)
#2 {main}
int(1)
int(1)
bool(false)
int(0)
int(2)
handler: trim(): Invalid '..'-range, no character to the right of '..' [frames,{closure:fl.php:3},trim,g]
handler: ltrim(): Invalid '..'-range, no character to the left of '..' [frames,{closure:fl.php:3},ltrim,g]
handler: rtrim(): Invalid '..'-range, '..'-range needs to be incrementing [frames,{closure:fl.php:3},rtrim,g]
bc|bc|abc
a|abc|abc|a.b|ab|ab
dirname(): Argument #2 ($levels) must be greater than or equal to 1
#0 fl.php(12): dirname('/a/b/c', 0)
#1 fl.php(13): h('/a/b/c', 0)
#2 {main}
/a|/a/b|/a/b
[toString frames,__toString,in_array,k]handler: Object of class S could not be converted to int [frames,{closure:fl.php:3},in_array,k]
bool(true)
bool(true)
bool(true)
bool(true)
bool(true)
bool(true)
bool(false)
bool(true)
bool(true)
bool(false)
string(4) "xbbx"
string(3) "xcx"
string(1) "x"
string(0) ""
string(3) "xyc"
string(3) "xbc"
string(0) ""
string(1) "2"
int(0)
float(1.5)
bool(false)
string(3) "abd"
int(65)
string(1) "B"
bool(true)
bool(true)
bool(false)
string(3) "abc"
string(3) "ABC"
string(2) "ff"
int(5)
float(2.5)
string(2) "bc"
string(2) "bc"
string(2) "ab"
string(2) "bc"
string(1) "a"
bool(false)
bool(true)
bool(true)
bool(true)
bool(true)
"#;

#[test]
fn math_functions_run_frameless_and_keep_the_frame_for_their_errors() {
    // `intdiv()` by zero or of `PHP_INT_MIN` by -1 and `log()` with an
    // invalid base throw from their frame; ints passed for `float`
    // parameters are widened silently, as zpp does.
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(MATH_SCRIPT.as_bytes(), "math.php");
    assert_eq!(output(vm, &sink, status), MATH_EXPECTED);
}

const MATH_SCRIPT: &str = r#"<?php
function m($a, $b) { return intdiv($a, $b); }
try { m(1, 0); } catch (DivisionByZeroError $e) { echo get_class($e), ": ", $e->getMessage(), "\n", $e->getTraceAsString(), "\n"; }
try { m(PHP_INT_MIN, -1); } catch (ArithmeticError $e) { echo get_class($e), ": ", $e->getMessage(), "\n", $e->getTraceAsString(), "\n"; }
function lg($x, $b) { return log($x, $b); }
try { lg(8, 0); } catch (ValueError $e) { echo $e->getMessage(), "\n", $e->getTraceAsString(), "\n"; }
$i = 16; $f = 2.25;
var_dump(m(7, 2), m(-7, 2), sqrt($i), sqrt($f), floor(5), ceil(4.1), round(2.5), round(1.955, 2), round(-5), log(1), lg(8, 2), fmod(7, 3), atan2(1, 1), hypot(3, 4), fdiv(1, 0), pi(), is_nan(sqrt(-1)), is_finite(1), exp(0), sin(0), cos(0));
"#;

const MATH_EXPECTED: &str = r#"DivisionByZeroError: Division by zero
#0 math.php(2): intdiv(1, 0)
#1 math.php(3): m(1, 0)
#2 {main}
ArithmeticError: Division of PHP_INT_MIN by -1 is not an integer
#0 math.php(2): intdiv(-9223372036854775808, -1)
#1 math.php(4): m(-9223372036854775808, -1)
#2 {main}
log(): Argument #2 ($base) must be greater than 0
#0 math.php(5): log(8, 0)
#1 math.php(6): lg(8, 0)
#2 {main}
int(3)
int(-3)
float(4)
float(1.5)
float(5)
float(5)
float(3)
float(1.96)
float(-5)
float(0)
float(3)
float(1)
float(0.7853981633974483)
float(5)
float(INF)
float(3.141592653589793)
bool(true)
bool(true)
float(1)
float(0)
float(1)
"#;

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Builtins compiled to dedicated opcodes (`zend_try_compile_special_func`:
//! `ZEND_STRLEN`, `ZEND_TYPE_CHECK`, `ZEND_CAST`, `ZEND_GET_TYPE`,
//! `ZEND_DEFINED`, `ZEND_IN_ARRAY`, `ZEND_ARRAY_KEY_EXISTS`) or constants
//! (`chr()`, `ord()`): same results as the calls, and no builtin frame
//! (expected outputs from `/usr/bin/php8.5 -n`; the null-key deprecation of
//! `array_key_exists()`, the float-key owner check and the `chr()`/`ord()`
//! range and length deprecations follow php-src master).
use php_runtime::{Value, ZStr};
use php_vm::compiler::{compile, CompileOptions, SPECIAL_BUILTINS};
use php_vm::ext::{Extension, ModuleBuilder};
use php_vm::opcode::Op;
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

fn run_with(source: &str, path: &str, setup: fn(&mut Vm)) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    setup(&mut vm);
    let status = vm.run_script(source.as_bytes(), path);
    drop(vm);
    let output = sink.0.borrow().clone();
    let output = String::from_utf8(output).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

fn run(source: &str, path: &str) -> String {
    run_with(source, path, |_| {})
}

/// `in_array()` and `array_key_exists()` as ext/standard registers them (the
/// bare VM has neither), so that direct calls compile to the opcodes while
/// the remaining calls reach the library.
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
    vm.register_function("array_key_exists($key, array $array): bool", |vm, a| {
        let (key, array) = (a.at(0), a.array(1));
        php_stdarr::basic::array_key_exists(vm, "array_key_exists", &key, &array)
            .map(Value::Bool)
            .map_err(|e| vm.throw_runtime_error(e))
    });
}

#[test]
fn strlen_opcode_coerces_in_place_without_a_frame() {
    assert_eq!(
        run(
            r#"<?php
class S { function __toString(): string { $t = debug_backtrace(); echo isset($t[1]) ? 2 : 1, ":"; return "abc"; } }
function f() { return strlen([]); }
try { f(); } catch (TypeError $e) { $t = $e->getTrace(); echo $e->getMessage(), "|", isset($t[1]) ? 2 : 1, "\n"; }
$s = "hello"; $r = &$s;
echo strlen($s), strlen($r), strlen(12), strlen(1.5), strlen(true), strlen(false), "\n";
echo strlen(new S), "\n";
echo strlen(null), "\n";
echo strlen($undef), "\n";
"#,
            "a.php"
        ),
        "strlen(): Argument #1 ($string) must be of type string, array given|1
552310
1:3

Deprecated: strlen(): Passing null to parameter #1 ($string) of type string is deprecated in a.php on line 8
0

Warning: Undefined variable $undef in a.php on line 9

Deprecated: strlen(): Passing null to parameter #1 ($string) of type string is deprecated in a.php on line 9
0
"
    );
}

#[test]
fn strlen_opcode_is_strict_under_strict_types() {
    assert_eq!(
        run(
            r#"<?php
declare(strict_types=1);
try { strlen(1); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
echo strlen("abc"), "\n";
"#,
            "c.php"
        ),
        "strlen(): Argument #1 ($string) must be of type string, int given\n3\n"
    );
}

#[test]
fn type_checks_casts_and_gettype_match_the_builtins() {
    assert_eq!(
        run(
            r#"<?php
$s = "x"; $r = &$s;
var_dump(is_int(1), is_int("1"), is_string($r), is_null($undef), is_bool(false), is_float(1.0), is_array([]), is_object(new stdClass), is_scalar(1.5), is_scalar(null), is_resource(1));
echo gettype(1), gettype($r), gettype(null), gettype([]), gettype(1.5), "\n";
var_dump(intval("12abc"), strval(3.0), boolval("0"), floatval("1e3"));
function g(&$x) {}
try { g(strlen($s)); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#,
            "b.php"
        ),
        r#"
Warning: Undefined variable $undef in b.php on line 3
bool(true)
bool(false)
bool(true)
bool(true)
bool(true)
bool(true)
bool(true)
bool(true)
bool(true)
bool(false)
bool(false)
integerstringNULLarraydouble
int(12)
string(1) "3"
bool(false)
float(1000)
g(): Argument #1 ($x) could not be passed by reference
"#
    );
}

#[test]
fn get_class_and_get_called_class_use_the_running_scope() {
    assert_eq!(
        run(
            r#"<?php
class A {
    function f() { return [get_class(), get_class($this), get_called_class()]; }
    static function s() { return get_called_class(); }
}
class B extends A {}
var_dump((new B)->f(), B::s());
try { get_class(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { get_called_class(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { get_class(1); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { get_class(null); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$f = function () { return get_called_class(); };
var_dump(Closure::bind($f, null, B::class)());
"#,
            "gc.php"
        ),
        r#"
Deprecated: Calling get_class() without arguments is deprecated in gc.php on line 3
array(3) {
  [0]=>
  string(1) "A"
  [1]=>
  string(1) "B"
  [2]=>
  string(1) "B"
}
string(1) "B"
get_class() without arguments must be called from within a class
get_called_class() must be called from within a class
get_class(): Argument #1 ($object) must be of type object, int given
get_class(): Argument #1 ($object) must be of type object, null given
string(1) "B"
"#
    );
}

#[test]
fn namespaced_and_aliased_calls_stay_calls() {
    // Unqualified names in a namespace resolve at run time
    // (`ZEND_INIT_NS_FCALL_BY_NAME`), so a namespaced function wins.
    assert_eq!(
        run(
            r#"<?php
namespace A {
    function is_int($v) { return "ns"; }
    function gettype($v) { return "ns"; }
    echo is_int(1), gettype(1), \is_int(1) ? "y" : "n", "\n";
}
"#,
            "t.php"
        ),
        "nsnsy\n"
    );
}

fn ops(src: &str, special_builtins: u32) -> Vec<Op> {
    let program =
        php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
    let opts = CompileOptions {
        filename: "t.php".into(),
        special_builtins,
        ..CompileOptions::default()
    };
    compile(&program, Rc::from(src.as_bytes()), opts)
        .expect("compile")
        .main
        .code
        .ops
        .clone()
}

/// The compile error of `src`, if any.
fn compile_error(src: &str, special_builtins: u32) -> Option<String> {
    let program =
        php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
    let opts = CompileOptions {
        filename: "t.php".into(),
        special_builtins,
        ..CompileOptions::default()
    };
    compile(&program, Rc::from(src.as_bytes()), opts)
        .err()
        .map(|e| e.message)
}

fn all_special_builtins() -> u32 {
    u32::MAX >> (32 - SPECIAL_BUILTINS.len())
}

#[test]
fn special_opcodes_are_emitted_only_for_registered_builtins() {
    let all = u32::MAX >> (32 - SPECIAL_BUILTINS.len());
    let src = "<?php $a = strlen($x); $b = count($y); $c = is_int($z); $d = gettype($z); \
               $e = intval($z); is_null($z); $f = count($y, 1); $g = is_integer($z); \
               $h = get_class($z); $i = get_called_class(); $j = get_class($z, 1);";
    let with = ops(src, all);
    assert!(with
        .iter()
        .any(|o| matches!(o, Op::Strlen { dst: Some(_), .. })));
    assert!(with
        .iter()
        .any(|o| matches!(o, Op::Count { sizeof: false, .. })));
    assert!(with
        .iter()
        .any(|o| matches!(o, Op::TypeCheck { dst: None, .. })));
    assert!(with.iter().any(|o| matches!(o, Op::GetType { .. })));
    assert!(with.iter().any(|o| matches!(o, Op::Cast { .. })));
    assert!(with
        .iter()
        .any(|o| matches!(o, Op::GetClass { dst: Some(_), .. })));
    assert!(with
        .iter()
        .any(|o| matches!(o, Op::GetCalledClass { dst: Some(_) })));
    // `count($y, 1)` and two-argument `get_class` remain calls; the
    // `is_integer` alias is a frameless call (its own ops run while it is
    // deprecated).
    assert_eq!(
        with.iter()
            .filter(|o| matches!(o, Op::DoCall { .. }))
            .count(),
        2
    );
    assert!(with.iter().any(|o| matches!(o, Op::FramelessICall(_))));
    let without = ops(src, 0);
    assert!(!without.iter().any(|o| matches!(
        o,
        Op::Strlen { .. }
            | Op::Count { .. }
            | Op::TypeCheck { .. }
            | Op::GetType { .. }
            | Op::GetClass { .. }
            | Op::GetCalledClass { .. }
    )));
    // Every one remains a call (the one-argument type tests, `strlen()` and
    // `count()` as frameless calls, which run their own ops unless the
    // builtin is there).
    assert_eq!(
        without
            .iter()
            .filter(|o| matches!(o, Op::DoCall { .. } | Op::FramelessICall(_)))
            .count(),
        11
    );
}

/// Whether the running builtin has a frame of its own: `B` when the top
/// frame is a builtin frame, `F` (frameless) when it is the caller's.
fn frame_mark(vm: &Vm) -> &'static str {
    if vm
        .frames
        .last()
        .is_some_and(|f| f.kind == FrameKind::Builtin)
    {
        "B"
    } else {
        "F"
    }
}

/// Stand-ins for two frameless builtins that report how they were called.
struct FramelessProbe;

impl Extension for FramelessProbe {
    fn name(&self) -> &'static str {
        "frameless_probe"
    }

    fn register(&self, module: &mut ModuleBuilder<'_>) {
        module.function_static("abs(int|float $num): int|float", |vm, _args| {
            Ok(Value::Str(ZStr::from(frame_mark(vm))))
        });
        module.function_static(
            "substr(string $string, int $offset, ?int $length = null): string",
            |vm, args| {
                Ok(Value::Str(ZStr::from(format!(
                    "{}:{}",
                    frame_mark(vm),
                    args.has(2)
                ))))
            },
        );
    }
}

#[test]
fn frameless_calls_skip_the_frame_only_without_coercion() {
    let sink = Sink::default();
    let mut vm = Vm::with_extensions(
        &VmOptions::default(),
        Box::new(sink.clone()),
        &[&FramelessProbe],
    );
    let status = vm.run_script(
        br#"<?php
echo abs(1), abs(2.5), abs("1"), abs(true), "\n";
function f() { return abs(1) . abs("1"); }
echo f(), "\n";
echo substr("x", 0), substr("x", 0, null), substr("x", "0"), "\n";
$f = 'abs';
echo $f(1), "\n";
"#,
        "t.php",
    );
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    // Coerced arguments and dynamic calls push the builtin frame. `abs()`
    // of an int or a float is answered without the handler (and without a
    // frame: `frameless_inline`), so it shows its real result, not "F".
    assert_eq!(output, "12.5BB\n1B\nF:falseF:trueB:false\nB\n");
}

#[test]
fn defined_in_array_array_key_exists_chr_ord_compile_by_their_literals() {
    let all = all_special_builtins();
    // One line, so the opcode shapes alone are checked.
    let src = "<?php $a = defined('X'); $b = defined('true'); \
               $c = in_array($x, ['a', 'b']); $d = in_array($x, [1, 'a'], true); \
               $e = in_array($x, [], true); $f = in_array($x, ['a'], '1'); \
               $g = array_key_exists($k, $arr); array_key_exists(1e42, $arr); \
               $h = chr(65); $i = ord('A'); \
               defined('A\\B'); defined('A::B'); defined($n); defined(null); \
               in_array($x, ['1']); in_array($x, [1]); in_array($x, ['a'], $s); in_array($x, $arr); \
               in_array($x, [1.5], true); chr(256); chr(-1); ord('ab'); ord('');";
    let with = ops(src, all);
    let count = |pred: fn(&Op) -> bool| with.iter().filter(|o| pred(o)).count();
    // `defined('true')` is the constant `true`: no opcode.
    assert_eq!(count(|o| matches!(o, Op::Defined { .. })), 1);
    assert_eq!(count(|o| matches!(o, Op::InArray { .. })), 4);
    assert_eq!(count(|o| matches!(o, Op::InArray { strict: true, .. })), 3);
    assert_eq!(count(|o| matches!(o, Op::KeyExists { .. })), 2);
    assert_eq!(count(|o| matches!(o, Op::ArrayKeyExists { .. })), 0);
    // The thirteen calls of the last four statements; `chr(65)` and
    // `ord('A')` are constants. The nine of frameless builtins
    // (`in_array()`, `chr()`, `ord()`) are one `FRAMELESS_ICALL` each, as
    // php-src compiles them (a9d373db); the four of `defined()` stay calls.
    assert_eq!(count(|o| matches!(o, Op::DoCall { .. })), 4);
    assert_eq!(count(|o| matches!(o, Op::FramelessICall(_))), 9);

    let without = ops(src, 0);
    assert!(!without.iter().any(|o| matches!(
        o,
        Op::Defined { .. } | Op::InArray { .. } | Op::KeyExists { .. }
    )));
    // Only the float-key lifetime case of `array_key_exists()` is special
    // without the builtin registered; everything else is a call.
    assert_eq!(
        without
            .iter()
            .filter(|o| matches!(o, Op::ArrayKeyExists { .. }))
            .count(),
        1
    );
    // The other calls stay calls. The fifteen of frameless builtins are
    // still one `FramelessICall` each: the fusion goes by the name (php-src
    // compiles the internal function's calls so), and the op runs the call
    // ops it replaced when the function is not that builtin at run time.
    let calls = |pred: fn(&Op) -> bool| without.iter().filter(|o| pred(o)).count();
    assert_eq!(calls(|o| matches!(o, Op::DoCall { .. })), 7);
    assert_eq!(calls(|o| matches!(o, Op::FramelessICall(_))), 15);
}

#[test]
fn opcode_builtins_are_rejected_in_write_context() {
    let all = all_special_builtins();
    for src in [
        "<?php $x = &in_array($y, ['a']);",
        "<?php $x = &defined('X');",
        "<?php $x = &defined('true');",
        "<?php in_array($y, ['a'])[0] = 1;",
    ] {
        assert_eq!(
            compile_error(src, all).as_deref(),
            Some("Cannot use result of built-in function in write context"),
            "{src}"
        );
        // Not registered as builtins, they are ordinary calls (IS_VAR).
        assert_eq!(compile_error(src, 0), None, "{src}");
    }
    // `chr()`/`ord()` are only folded in read context, and calls that keep
    // their arguments dynamic stay calls.
    for src in [
        "<?php $x = &chr(65);",
        "<?php $x = &ord('A');",
        "<?php $x = &in_array($y, $arr);",
        "<?php $x = &in_array($y, ['a'], $s);",
        "<?php $x = &defined($n);",
        "<?php $x = &defined('A\\B');",
    ] {
        assert_eq!(compile_error(src, all), None, "{src}");
    }
}

#[test]
fn defined_opcode_and_compile_time_constants() {
    // `defined(1)` and `defined(1.5)` convert their literal without the
    // call's parameter check; `__COMPILER_HALT_OFFSET__` is only found
    // through the builtin (mangled name).
    assert_eq!(
        run(
            r#"<?php
declare(strict_types=1);
var_dump(defined('true'), defined('TRUE'), defined('Null'), defined('E_ALL'));
var_dump(defined(1), defined(1.5), defined(''), defined('Foo::BAR'), defined('\\E_ALL'));
var_dump(defined('MY'), defined('my'), defined('__COMPILER_HALT_OFFSET__'));
define('MY', 1);
var_dump(defined('MY'), defined('my'));
$n = 'MY';
var_dump(defined($n), defined('M' . 'Y'));
echo "done\n";
__halt_compiler();
"#,
            "df.php"
        ),
        "bool(true)
bool(true)
bool(true)
bool(true)
bool(false)
bool(false)
bool(false)
bool(false)
bool(true)
bool(false)
bool(false)
bool(false)
bool(true)
bool(false)
bool(true)
bool(true)
done
"
    );
}

#[test]
fn in_array_opcode_matches_the_builtin() {
    // Columns: loose ["a","b"], strict ["a","b"], strict [1,"x"], loose [],
    // loose ["","z"]. `S::__toString` runs once per compared key, in order,
    // without an `in_array` frame below it; `T::__toString` counts the
    // backtrace frames with `foreach` (the bare VM has no `count()`).
    assert_eq!(
        run_with(
            r#"<?php
function b($v) { return $v ? "T" : "F"; }
class S { function __toString() { echo "ts\n"; return "b"; } }
class N {}
$ref = "b"; $rr = &$ref;
$cases = ["a", "A", "1", 1, 0, 1.0, true, false, null, [], new S, new N, "", " a"];
foreach ($cases as $i => $n) {
  echo $i, ":", b(in_array($n, ["a", "b"])), b(in_array($n, ["a", "b"], true)),
    b(in_array($n, [1, "x"], true)), b(in_array($n, [])), b(in_array($n, ["", "z"])), "\n";
}
var_dump(in_array($rr, ["a", "b"]), in_array($rr, ["a", "b"], true), in_array($undef1, ["a"]), in_array($undef2, ["a"], true), in_array($undef3, [""]));
var_dump(in_array("1", [1, "x"], true), in_array(1, ["1", "x"], true), in_array("1", ["1", "x"], true), in_array(true, ["a"], PHP_INT_MAX), in_array(true, ["a"], "0"));
class T { function __toString() { $n = 0; foreach (debug_backtrace() as $_) { $n++; } echo $n, "\n"; return "x"; } }
var_dump(in_array(new T, ["a"]));
class U { function __toString() { throw new Exception("boom"); } }
try { in_array(new U, ["a"]); } catch (Exception $e) { echo $e->getMessage(), " ", $e->getTrace()[0]["function"], "\n"; }
$h = ["a"];
var_dump(in_array("a", $h), in_array("a", ["a" => 1]), in_array("a", ["a", "1"]), in_array("1", ["a", "1"]));
function f(&$x) {}
try { f(in_array("a", ["a"])); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#,
            "ia.php",
            register_array_builtins
        ),
        "0:TTFFF
1:FFFFF
2:FFFFF
3:FFTFF
4:FFFFF
5:FFFFF
6:TFFFT
7:FFFFT
8:FFFFT
9:FFFFF
10:ts
ts
TFFFts
ts
F
11:FFFFF
12:FFFFT
13:FFFFF

Warning: Undefined variable $undef1 in ia.php on line 11

Warning: Undefined variable $undef2 in ia.php on line 11

Warning: Undefined variable $undef3 in ia.php on line 11
bool(true)
bool(true)
bool(false)
bool(false)
bool(true)
bool(false)
bool(false)
bool(true)
bool(false)
bool(true)
1
bool(false)
boom __toString
bool(true)
bool(false)
bool(true)
bool(true)
f(): Argument #1 ($x) could not be passed by reference
"
    );
}

#[test]
fn array_key_exists_opcode_follows_php_src_order() {
    // Operands are read when the opcode runs (`$key` after `change()`), an
    // undefined key warns without the null deprecation, a non-array subject
    // warns for both operands before the TypeError (no frame), and a float
    // key whose diagnostics drop the array's last other owner is not found
    // (`zend_array_key_exists_fast`); a constant literal stays owned by the
    // op array, so only a runtime-built array can be dropped that way.
    assert_eq!(
        run_with(
            r#"<?php
set_error_handler(function ($level, $message) { echo $message, "\n"; });
$a = ["a" => 1, "" => 2, 1 => 3, 0 => 4, "5" => 5];
foreach (["a", "", 1, "1", 0, 1.0, 1.5, true, false, null, "5", "05", "b"] as $i => $k) {
  echo $i, ":"; var_dump(array_key_exists($k, $a));
}
class S { function __toString() { return "a"; } }
foreach ([[], new S] as $k) {
  try { var_dump(array_key_exists($k, $a)); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
}
$ref = &$a; var_dump(array_key_exists("a", $ref));
$kr = "a"; $kref = &$kr; var_dump(array_key_exists($kref, $a));
foreach (["str", new S, null, 1.5] as $s) {
  try { var_dump(array_key_exists("a", $s)); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
}
try { var_dump(array_key_exists($u1, $u2)); } catch (TypeError $e) { echo $e->getMessage(), " ", isset($e->getTrace()[0]) ? "frame" : "no frame", "\n"; }
try { var_dump(array_key_exists(null, $u3)); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { var_dump(array_key_exists(1.5, $u4)); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
var_dump(array_key_exists($u5, $a));
$key = "before";
function change() { global $key; $key = "after"; echo "array\n"; return ["before" => 1]; }
var_dump(array_key_exists($key, change()));
function f(&$x) {}
try { f(array_key_exists("a", $a)); } catch (Error $e) { echo $e->getMessage(), "\n"; }
$arr = [1 => 7];
set_error_handler(function ($level, $message) { global $arr; $arr = null; echo $message, "\n"; });
var_dump(array_key_exists(1.5, $arr));
$one = 1; $arr = [$one => 7];
var_dump(array_key_exists(1.5, $arr));
$arr = [1 => 7]; $copy = $arr;
var_dump(array_key_exists(1.5, $arr));
var_dump(array_key_exists(1.5, [1 => 7]));
function fresh() { return [1 => 7]; }
var_dump(array_key_exists(1.5, fresh()));
"#,
            "ak.php",
            register_array_builtins
        ),
        "0:bool(true)
1:bool(true)
2:bool(true)
3:bool(true)
4:bool(true)
5:bool(true)
6:Implicit conversion from float 1.5 to int loses precision
bool(true)
7:bool(true)
8:bool(true)
9:Using null as the key parameter for array_key_exists() is deprecated, use an empty string instead
bool(true)
10:bool(true)
11:bool(false)
12:bool(false)
Cannot access offset of type array on array
Cannot access offset of type S on array
bool(true)
bool(true)
array_key_exists(): Argument #2 ($array) must be of type array, string given
array_key_exists(): Argument #2 ($array) must be of type array, S given
array_key_exists(): Argument #2 ($array) must be of type array, null given
array_key_exists(): Argument #2 ($array) must be of type array, float given
Undefined variable $u1
Undefined variable $u2
array_key_exists(): Argument #2 ($array) must be of type array, null given no frame
Undefined variable $u3
array_key_exists(): Argument #2 ($array) must be of type array, null given
Undefined variable $u4
array_key_exists(): Argument #2 ($array) must be of type array, null given
Undefined variable $u5
bool(true)
array
bool(false)
f(): Argument #1 ($x) could not be passed by reference
Implicit conversion from float 1.5 to int loses precision
bool(true)
Implicit conversion from float 1.5 to int loses precision
bool(false)
Implicit conversion from float 1.5 to int loses precision
bool(true)
Implicit conversion from float 1.5 to int loses precision
bool(true)
Implicit conversion from float 1.5 to int loses precision
bool(true)
"
    );
}

#[test]
fn multi_line_calls_take_the_last_arguments_line() {
    // `zend_emit_op_tmp` uses `CG(zend_lineno)`, left at the last compiled
    // argument: both warnings and the TypeError name the `$a` line, the
    // `in_array` warning the needle's line.
    assert_eq!(
        run_with(
            r#"<?php
function t() {
try {
  var_dump(array_key_exists(
    $k,
    $a
  ));
} catch (TypeError $e) { echo "TE line ", $e->getLine(), "\n"; }
var_dump(in_array(
  $c,
  ["a", "b"]
));
}
t();
"#,
            "ml.php",
            register_array_builtins
        ),
        "
Warning: Undefined variable $k in ml.php on line 6

Warning: Undefined variable $a in ml.php on line 6
TE line 6

Warning: Undefined variable $c in ml.php on line 10
bool(false)
"
    );
}

#[test]
fn chr_and_ord_literals_fold_to_constants() {
    // Only `chr(0..=255)` and one-byte `ord()` literals fold; the rest are
    // calls, with the range and length deprecations (8.5 and master) and
    // their strict-mode TypeErrors. A folded argument is sent by
    // value even after a call argument (`h(g(), chr(65))`), and in write
    // context the constant behaves like the call's temporary.
    assert_eq!(
        run(
            r#"<?php
declare(strict_types=1);
var_dump(chr(65), chr(0x41), ord("A"), ord("\xff"), strlen(chr(0)), ord(chr(256)), ord(chr(-1)), ord("AB"), ord(""));
try { var_dump(chr("65")); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { var_dump(ord(65)); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
function f(&$x) {}
function g() { return 1; }
function h($a, &$b) {}
try { f(chr(65)); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { h(g(), chr(65)); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { h(g(), ord("A")); } catch (Error $e) { echo $e->getMessage(), "\n"; }
$x = &chr(65); var_dump($x);
$y = &ord("A"); var_dump($y);
var_dump(chr(65)[0], ord("A") + 1);
"#,
            "co.php"
        ),
        r#"
Deprecated: chr(): Providing a value not in-between 0 and 255 is deprecated, this is because a byte value must be in the [0, 255] interval. The value used will be constrained using % 256 in co.php on line 3

Deprecated: chr(): Providing a value not in-between 0 and 255 is deprecated, this is because a byte value must be in the [0, 255] interval. The value used will be constrained using % 256 in co.php on line 3

Deprecated: ord(): Providing a string that is not one byte long is deprecated. Use ord($str[0]) instead in co.php on line 3

Deprecated: ord(): Providing an empty string is deprecated in co.php on line 3
string(1) "A"
string(1) "A"
int(65)
int(255)
int(1)
int(0)
int(255)
int(65)
int(0)
chr(): Argument #1 ($codepoint) must be of type int, string given
ord(): Argument #1 ($character) must be of type string, int given
f(): Argument #1 ($x) could not be passed by reference
h(): Argument #2 ($b) could not be passed by reference
h(): Argument #2 ($b) could not be passed by reference

Notice: Only variables should be assigned by reference in co.php on line 12
string(1) "A"

Notice: Only variables should be assigned by reference in co.php on line 13
int(65)
string(1) "A"
int(66)
"#
    );
}

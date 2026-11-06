//! Builtins compiled to dedicated opcodes (`zend_try_compile_special_func`:
//! `ZEND_STRLEN`, `ZEND_TYPE_CHECK`, `ZEND_CAST`, `ZEND_GET_TYPE`): same
//! results as the calls, and no builtin frame (expected outputs from
//! `/usr/bin/php -n`).
use php_vm::compiler::{compile, CompileOptions, SPECIAL_BUILTINS};
use php_vm::opcode::Op;
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

fn run(source: &str, path: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), path);
    drop(vm);
    let output = sink.0.borrow().clone();
    let output = String::from_utf8(output).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
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
    // `count($y, 1)`, the `is_integer` alias and two-argument `get_class`
    // remain calls.
    assert_eq!(
        with.iter()
            .filter(|o| matches!(o, Op::DoCall { .. }))
            .count(),
        3
    );
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
    assert_eq!(
        without
            .iter()
            .filter(|o| matches!(o, Op::DoCall { .. }))
            .count(),
        11
    );
}

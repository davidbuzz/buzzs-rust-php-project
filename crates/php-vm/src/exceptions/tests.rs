//! Script-level tests of the Throwable machinery (expected output from
//! `/usr/bin/php -n`, paths normalised to `t.php`).

#![allow(clippy::needless_raw_string_hashes)]

use crate::{Vm, VmOptions};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run(src: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(src.as_bytes(), "t.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn previous_chain_never_cycles() {
    let src = r#"<?php
try {
    $e = new Exception("Foo");
    try {
        throw new Exception("Bar", 0, $e);
    } finally {
        throw $e;
    }
} catch (Exception $e) {
    echo $e, "\n";
}
"#;
    assert_eq!(
        run(src),
        r#"Exception: Foo in t.php:3
Stack trace:
#0 {main}
"#
    );
}

#[test]
fn magic_call_frames_show_themselves() {
    let src = r#"<?php
class M {
    function __call($n, $a) { echo (new Exception)->getTraceAsString(), "\n"; }
    static function __callStatic($n, $a) { echo (new Exception)->getTraceAsString(), "\n"; }
}
function f() { (new M)->foo(1, "x"); M::bar(2); }
f();
"#;
    assert_eq!(
        run(src),
        r#"#0 t.php(6): M->__call('foo', Array)
#1 t.php(7): f()
#2 {main}
#0 t.php(6): M::__callStatic('bar', Array)
#1 t.php(7): f()
#2 {main}
"#
    );
}

#[test]
fn top_level_handler_is_internal_frame() {
    let src = r#"<?php
function h($e) { echo (new Exception)->getTraceAsString(), "\n"; }
set_exception_handler('h');
throw new Exception("x");
"#;
    assert_eq!(
        run(src),
        r#"#0 [internal function]: h(Object(Exception))
#1 {main}
"#
    );
}

#[test]
fn error_exception_properties() {
    let src = r#"<?php
$e = new ErrorException("m", 3, E_WARNING, "some.php");
var_dump($e->getSeverity(), $e->getFile(), $e->getLine(), $e->getCode());
$e = new ErrorException("m", 0, E_NOTICE, null, 7, new Exception("p"));
var_dump($e->getLine(), $e->getPrevious()->getMessage());
$e = new Exception("msg", 0);
var_dump($e->getCode());
echo get_class(new TypeError), "\n";
"#;
    assert_eq!(
        run(src),
        r#"int(2)
string(8) "some.php"
int(0)
int(3)
int(7)
string(1) "p"
int(0)
TypeError
"#
    );
}

#[test]
fn sensitive_params_wrapped_in_traces() {
    let src = r#"<?php
function s($a, #[\SensitiveParameter] $secret, ...$rest) { throw new Exception("s"); }
try { s(1, "pw", 'aaaaaaaaaaaaaaaaaaaaaa', "x\n"); } catch (Exception $e) { echo $e->getTraceAsString(), "\n"; }
function fga($a, #[\SensitiveParameter] $b) { var_dump(func_get_args()); }
fga(1, 2);
"#;
    assert_eq!(
        run(src),
        r#"#0 t.php(3): s(1, Object(SensitiveParameterValue), 'aaaaaaaaaaaaaaa...', 'x\n')
#1 {main}
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
"#
    );
}

/// `zend_throw_exception_object`: throwing a non-Throwable object.
#[test]
fn throwing_a_non_throwable_object_is_an_error() {
    let src = r#"<?php
class Foo {}
try { throw new Foo; } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { throw 1; } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#;
    assert_eq!(
        run(src),
        "Cannot throw objects that do not implement Throwable\nCan only throw objects\n"
    );
}

/// `ZEND_UNSET_STATIC_PROP` → `zend_std_unset_static_property` throws,
/// whether or not the property is declared (the class is fetched first).
#[test]
fn unsetting_a_static_property_throws() {
    let src = r#"<?php
class C { public static $p = 1; }
try { unset(C::$a); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { unset(C::$p); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { unset(D::$p); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#;
    assert_eq!(
        run(src),
        "Attempt to unset static property C::$a\nAttempt to unset static property C::$p\nClass \"D\" not found\n"
    );
}

/// `zend_fetch_debug_backtrace` `fake_frame`: a handler called by the
/// include op itself is followed by an `include` pseudo frame.
#[test]
fn backtrace_of_a_handler_called_by_include_has_the_include_frame() {
    let src = r#"<?php
set_error_handler(function ($c, $m) {
    foreach (debug_backtrace(DEBUG_BACKTRACE_IGNORE_ARGS) as $f) {
        echo $f['function'], '@', $f['line'], "\n";
    }
    return true;
});
function f() { include 'nonexistent_zz.php'; }
f();
"#;
    let out = run(src);
    assert!(
        out.starts_with("{closure:t.php:2}@8\ninclude@8\nf@9\n"),
        "{out}"
    );
}

/// `zend_user_exception_handler`: a handler that installs another keeps it
/// current, and an exception it throws goes to that handler (GH-10695,
/// GH-13446).
#[test]
fn exception_handler_installed_by_the_handler_stays_and_catches_its_throw() {
    let src = r#"<?php
function h1($e) {
    echo "h1: ", $e->getMessage(), "\n";
    set_exception_handler('h2');
    throw new Exception("from h1");
}
function h2($e) { echo "h2: ", $e->getMessage(), "\n"; }
set_exception_handler('h1');
register_shutdown_function(function () {
    echo set_exception_handler(null), "\n";
});
throw new Exception("main");
"#;
    assert_eq!(run(src), "h1: main\nh2: from h1\nh2\n");
}

/// `ZEND_AST_NEW` has the line of its class name: the constructor call's
/// frame reports it (GH-8810).
#[test]
fn constructor_frame_reports_the_class_name_line_of_new() {
    let src = r#"<?php
class A {
    public function __construct() {
        throw new Exception();
    }
}
try {
    new
        A
        ();
} catch (Exception $e) {
    echo $e->getTrace()[0]['line'], "\n";
}
"#;
    assert_eq!(run(src), "9\n");
}

/// Array literals and constant-expression arrays reject array/object keys
/// with `zend_illegal_array_offset_access`'s message (GH-8821).
#[test]
fn illegal_array_literal_key_names_its_type() {
    let src = r#"<?php
enum E { case A; }
$k = []; try { $a = [$k => 1]; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$o = new stdClass; try { $a = [1, $o => 1]; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
class C { const X = [E::A => 3]; }
try { var_dump(C::X); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
"#;
    assert_eq!(
        run(src),
        "Cannot access offset of type array on array\nCannot access offset of type stdClass on array\nCannot access offset of type E on array\n"
    );
}

/// `SensitiveParameterValue` keeps its value in the private readonly
/// `$value` property (`Zend/zend_attributes.stub.php`), so clones and
/// reflection see it, and it is `@strict-properties`.
#[test]
fn sensitive_parameter_value_is_a_readonly_property() {
    let src = r#"<?php
$v = new SensitiveParameterValue('secret');
$c = clone $v;
echo $c->getValue(), "\n";
echo (new ReflectionProperty($v, 'value'))->getValue($v), "\n";
try { $v->foo = 1; } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { $v->__construct('x'); } catch (Error $e) { echo $e->getMessage(), "\n"; }
function s(#[\SensitiveParameter] $p) { return debug_backtrace()[0]['args'][0]; }
echo s('pw')->getValue(), "\n";
var_dump($v);
"#;
    assert_eq!(
        run(src),
        "secret\nsecret\nCannot create dynamic property SensitiveParameterValue::$foo\nCannot modify readonly property SensitiveParameterValue::$value\npw\nobject(SensitiveParameterValue)#1 (0) {\n}\n"
    );
}

/// `eval()` reports its operand's line (`zend_ast_create_1()` copies the
/// child's lineno), and a heredoc literal carries its body's line
/// (`Zend/tests/function_arguments/sensitive_parameter_eval_call.phpt`).
#[test]
fn eval_line_is_its_operand_line() {
    let src = r#"<?php
eval(

"echo __FILE__, \"\\n\";"
);
eval(<<<EOT
echo __FILE__, "\\n";
EOT);
"#;
    assert_eq!(
        run(src),
        "t.php(4) : eval()'d code\nt.php(7) : eval()'d code\n"
    );
}

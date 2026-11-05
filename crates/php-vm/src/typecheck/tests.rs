//! Script-level tests of type-declaration coercion (expected output from
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
fn weak_mode_coercions() {
    let src = r#"<?php
function a(int|float $x) { var_dump($x); }
a(true); a("1e2"); a(" 5"); a(false);
function b(int|string $x) { var_dump($x); }
b(1.0); b(INF);
function c(bool $x) { var_dump($x); }
c("0"); c(0.0); c("abc");
function d(string|bool $x) { var_dump($x); }
d(1.5); d(7);
function e(float $x) { var_dump($x); }
e(3); e("4");
function f(int $x) { var_dump($x); }
f(1.5); f("2.5");
try { f("abc"); } catch (TypeError $t) { echo $t->getMessage(), "\n"; }
try { f([]); } catch (TypeError $t) { echo $t->getMessage(), "\n"; }
class S { function __toString(): string { return "str"; } }
function g(string $x) { var_dump($x); }
g(new S);
function h(?int $x) { var_dump($x); }
h(null);
"#;
    assert_eq!(
        run(src),
        r#"int(1)
float(100)
int(5)
int(0)
int(1)
string(3) "INF"
bool(false)
bool(false)
bool(true)
string(3) "1.5"
string(1) "7"
float(3)
float(4)

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 12
int(1)

Deprecated: Implicit conversion from float-string "2.5" to int loses precision in t.php on line 12
int(2)
f(): Argument #1 ($x) must be of type int, string given, called in t.php on line 14
f(): Argument #1 ($x) must be of type int, array given, called in t.php on line 15
string(3) "str"
NULL
"#
    );
}

#[test]
fn strict_mode_coercions() {
    let src = r#"<?php
declare(strict_types=1);
function e(float $x) { var_dump($x); }
e(3);
function f(int $x) { var_dump($x); }
try { f(1.0); } catch (TypeError $t) { echo $t->getMessage(), "\n"; }
try { f("1"); } catch (TypeError $t) { echo $t->getMessage(), "\n"; }
"#;
    assert_eq!(
        run(src),
        r#"float(3)
f(): Argument #1 ($x) must be of type int, float given, called in t.php on line 6
f(): Argument #1 ($x) must be of type int, string given, called in t.php on line 7
"#
    );
}

fn compile(src: &str) -> crate::compiler::CResult<crate::compiler::CompiledUnit> {
    let program = php_parser::parse(
        src.as_bytes(),
        &php_parser::ParseOptions {
            syntax_only: true,
            ..Default::default()
        },
    )
    .expect("valid type declaration syntax");
    crate::compiler::compile(
        &program,
        Rc::from(src.as_bytes()),
        crate::compiler::CompileOptions::default(),
    )
}

#[test]
fn qualified_builtin_types_are_rejected() {
    for name in [
        "int", "float", "string", "bool", "array", "callable", "static", "iterable", "object",
        "mixed", "void", "never", "null", "false", "true",
    ] {
        for prefix in ["\\", "namespace\\"] {
            let src = format!("<?php function f(): {prefix}{name} {{}}");
            assert_eq!(
                compile(&src).expect_err("qualified builtin").message,
                format!("Type declaration '{name}' must be unqualified")
            );
        }
    }
}

#[test]
fn declaration_redundancy_and_relative_types() {
    for (ty, message) in [
        ("A&A", "Duplicate type A is redundant"),
        ("A&B&a", "Duplicate type a is redundant"),
        (
            "(A&B)|A",
            "Type A&B is redundant as it is more restrictive than type A",
        ),
        (
            "A|(B&A)",
            "Type B&A is redundant as it is more restrictive than type A",
        ),
        ("(A&B)|(B&A)", "Type B&A is redundant with type A&B"),
        (
            "(A&B&C)|(C&A)",
            "Type A&B&C is redundant as it is more restrictive than type C&A",
        ),
        ("iterable|array", "Duplicate type array is redundant"),
        ("array|iterable", "Duplicate type array is redundant"),
        ("iterable|iterable", "Duplicate type array is redundant"),
        (
            "Traversable|iterable",
            "Duplicate type Traversable is redundant",
        ),
        (
            "iterable|Traversable",
            "Duplicate type Traversable is redundant",
        ),
        (
            "iterable&Iterator",
            "Type Traversable|array cannot be part of an intersection type",
        ),
        (
            "(int&A)|B",
            "Type int cannot be part of an intersection type",
        ),
        (
            "(A&callable)|B",
            "Type callable cannot be part of an intersection type",
        ),
        ("void|int", "Void can only be used as a standalone type"),
        ("never|int", "never can only be used as a standalone type"),
        ("?never", "never can only be used as a standalone type"),
    ] {
        let src = format!("<?php function f(): {ty} {{}}");
        assert_eq!(
            compile(&src).expect_err("invalid type").message,
            message,
            "{ty}"
        );
    }
    for name in ["self", "SELF", "parent", "PARENT"] {
        let src = format!("<?php trait T {{ function f(): {name}&Iterator {{}} }}");
        assert_eq!(
            compile(&src)
                .expect_err("unresolved relative intersection")
                .message,
            format!("Type {name} cannot be part of an intersection type")
        );
    }
    for src in [
        "<?php class C {function f(parent $x) {}}",
        "<?php interface I {function f(): parent;}",
    ] {
        assert_eq!(
            compile(src).expect_err("missing parent").message,
            "Cannot use \"parent\" when current class scope has no parent"
        );
    }
}

#[test]
fn invalid_parameter_types_and_defaults() {
    for (param, message) in [
        ("void $x", "void cannot be used as a parameter type"),
        ("never $x", "never cannot be used as a parameter type"),
        (
            "float $x = true",
            "Cannot use bool as default value for parameter $x of type float",
        ),
        (
            "int|float $x = '0'",
            "Cannot use string as default value for parameter $x of type int|float",
        ),
        (
            "iterable $x = 1",
            "Cannot use int as default value for parameter $x of type Traversable|array",
        ),
    ] {
        let src = format!("<?php function f({param}) {{}}");
        assert_eq!(
            compile(&src).expect_err("invalid parameter").message,
            message
        );
    }
    for param in [
        "float $x = 1",
        "int|float $x = 1.5",
        "iterable $x = []",
        "A&B $x = null",
    ] {
        let src = format!("<?php function f({param}) {{}}");
        compile(&src).expect("valid parameter default");
    }
}

#[test]
fn explicit_return_declaration_errors() {
    for (ty, body, message) in [
        ("void", "return null;", "A void function must not return a value (did you mean \"return;\" instead of \"return null;\"?)"),
        ("void", "return 1;", "A void function must not return a value"),
        ("never", "return;", "A never-returning function must not return"),
        ("int", "return;", "A function with return type must return a value"),
        ("?int", "return;", "A function with return type must return a value (did you mean \"return null;\" instead of \"return;\"?)"),
    ] {
        let src = format!("<?php function f(): {ty} {{{body}}}");
        assert_eq!(compile(&src).expect_err("invalid return").message, message);
        let src = format!("<?php class C {{function f(): {ty} {{{body}}}}}");
        assert_eq!(compile(&src).expect_err("invalid method return").message, message.replace("function", "method"));
    }
}

#[test]
fn dnf_declaration_order_and_implicit_nullability() {
    let unit = compile("<?php function f(A|(B&C)|D|(E&F) $x, B&C $y = null): A|(B&C) {}")
        .expect("valid DNF");
    let functions = unit.decls.functions.borrow();
    let f = &functions[unit.hoisted_functions[0] as usize];
    assert_eq!(
        f.params[0].ty.as_ref().expect("type").to_php_string(),
        "A|(B&C)|D|(E&F)"
    );
    assert_eq!(
        f.params[1].ty.as_ref().expect("type").to_php_string(),
        "(B&C)|null"
    );
    assert_eq!(
        f.return_type.as_ref().expect("type").to_php_string(),
        "A|(B&C)"
    );
}

#[test]
fn settype_nan_reloads_after_error_handler() {
    let src = r#"<?php
set_error_handler(function ($level, $message) {
    global $v;
    $v = null;
    echo $message, "\n";
});
$v = NAN; settype($v, "array"); var_dump($v[0]);
$v = NAN; settype($v, "object"); var_dump($v->scalar);
$v = NAN; settype($v, "null"); var_dump($v);
set_error_handler(function ($level, $message) {
    global $v;
    unset($v);
    echo $message, "\n";
});
$v = NAN; settype($v, "array"); var_dump($v[0]);
$v = NAN; settype($v, "object"); var_dump($v->scalar);
$v = NAN; settype($v, "null"); var_dump($v);
set_error_handler(function ($level, $message) {
    global $v;
    $v = "changed";
    echo $message, "\n";
});
$v = NAN; settype($v, "array"); var_dump($v[0]);
$v = NAN; settype($v, "object"); var_dump($v->scalar);
$v = NAN; settype($v, "null"); var_dump($v);
"#;
    assert_eq!(run(src), "unexpected NAN value was coerced to array\nNULL\nunexpected NAN value was coerced to object\nNULL\nunexpected NAN value was coerced to null\nNULL\nunexpected NAN value was coerced to array\nfloat(NAN)\nunexpected NAN value was coerced to object\nfloat(NAN)\nunexpected NAN value was coerced to null\nNULL\nunexpected NAN value was coerced to array\nstring(7) \"changed\"\nunexpected NAN value was coerced to object\nstring(7) \"changed\"\nunexpected NAN value was coerced to null\nNULL\n");
}

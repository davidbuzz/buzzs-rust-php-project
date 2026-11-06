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

fn run_array_lookup(src: &str) -> String {
    use crate::func::Args;
    use crate::vm::{FrameKind, VmResult};
    use php_runtime::Value;

    fn lookup(vm: &mut Vm, args: &mut Args) -> VmResult<Value> {
        let key = args.at(0);
        let array = args.array(1);
        let name = args.func_name();
        let n = vm.frames.len();
        if matches!(key, Value::Array(_) | Value::Object(_))
            && n >= 2
            && vm.frames[n - 2].kind != FrameKind::Builtin
        {
            return Err(vm.throw(
                "TypeError",
                &format!("Cannot access offset of type {} on array", key.value_name()),
            ));
        }
        php_stdarr::basic::array_key_exists(vm, &name, &key, &array)
            .map(Value::Bool)
            .map_err(|e| vm.rt_throw(e))
    }

    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    // The stand-in simulates ext/standard's binding, and these scripts test
    // the internal-call path: a registered `array_key_exists` would
    // otherwise compile direct calls to `ZEND_ARRAY_KEY_EXISTS`, whose
    // behaviour (no frame, operands read in place) tests/special_ops.rs
    // covers.
    vm.compile_no_builtins = true;
    vm.register_function("array_key_exists($key, array $array): bool", lookup);
    vm.register_function("test_resource()", |_vm, _args| {
        Ok(Value::Resource(php_runtime::Resource::with_handle(
            42, "test", None,
        )))
    });
    vm.run_script(src.as_bytes(), "t.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn native_public_readonly_set_visibility() {
    use crate::builtins::classbuilder::ClassBuilder;
    use crate::func::flags;
    use php_runtime::ZStr;

    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink));
    let ro = flags::PUBLIC | flags::READONLY;
    let cls = ClassBuilder::new("NativeReadonly")
        .prop("implicit", ro, Some("int"), None)
        .prop("publicSet", ro | flags::SET_PUBLIC, Some("int"), None)
        .prop("privateSet", ro | flags::SET_PRIVATE, Some("int"), None)
        .prop("mutable", flags::PUBLIC, Some("int"), None)
        .register(&mut vm);
    for (name, expected) in [
        ("implicit", flags::SET_PROTECTED),
        ("publicSet", flags::SET_PUBLIC),
        ("privateSet", flags::SET_PRIVATE),
        ("mutable", 0),
    ] {
        assert_eq!(
            cls.prop_map[&ZStr::from(name)].flags & flags::SET_MASK,
            expected
        );
    }
}

#[test]
fn anonymous_dynamic_property_diagnostics() {
    let src = "<?php\n\
        set_error_handler(function ($level, $message) { echo $message, \"\\n\"; });\n\
        $a = new class { public $extra; };\n\
        $a->extra = [];\n\
        $a->extra[] = 1;\n\
        $a->dynamic1 = 1;\n\
        $a->dynamic1 = 2;\n\
        $a->dynamic2 = [];\n\
        $a->dynamic2[] = 3;\n\
        $a->dynamic3[] = 4;\n\
        var_dump($a->extra[0], $a->dynamic1, $a->dynamic2[0], $a->dynamic3[0]);\n";
    assert_eq!(
        run(src),
        "Creation of dynamic property class@anonymous\0t.php:3$0::$dynamic1 is deprecated\n\
         Creation of dynamic property class@anonymous\0t.php:3$0::$dynamic2 is deprecated\n\
         Creation of dynamic property class@anonymous\0t.php:3$0::$dynamic3 is deprecated\n\
         int(1)\nint(2)\nint(3)\nint(4)\n"
    );
}

#[test]
fn anonymous_self_union_diagnostics() {
    let src = "<?php\n\
        $a = new class {\n\
            public function testParam(self|string $a) {}\n\
            public function test(): self|string { return new stdClass; }\n\
        };\n\
        try { $a->testParam(null); } catch (Throwable $e) {\n\
            echo $e::class, ': ', $e->getMessage(), \"\\n\";\n\
        }\n\
        try { $a->test(); } catch (Throwable $e) {\n\
            echo $e::class, ': ', $e->getMessage(), \"\\n\";\n\
        }\n";
    assert_eq!(
        run(src),
        "TypeError: class@anonymous\0t.php:2$0::testParam(): Argument #1 ($a) must be of type class@anonymous|string, null given, called in t.php on line 6\n\
         TypeError: class@anonymous::test(): Return value must be of type class@anonymous|string, stdClass returned\n"
    );
}

#[test]
fn anonymous_type_names_preserve_union_and_nullable_suffixes() {
    use crate::types::{mask, TypeDecl};
    use php_runtime::ZStr;

    let name = ZStr::from("class@anonymous\0t.php:2$0");
    for class in [ZStr::from("self"), ZStr::from("parent"), name.clone()] {
        let mut ty = TypeDecl {
            mask: mask::STRING,
            classes: vec![class],
            ..Default::default()
        };
        assert_eq!(
            ty.display(Some(&name), Some(&name), None),
            "class@anonymous|string"
        );
        ty.mask = mask::NULL;
        assert_eq!(
            ty.display(Some(&name), Some(&name), None),
            "?class@anonymous"
        );
    }
    let ty = TypeDecl {
        mask: mask::STRING,
        classes: vec![name.clone()],
        ..Default::default()
    };
    assert_eq!(ty.to_php_string(), "class@anonymous|string");
    // Zend appends the called scope for `static` without class-name resolution.
    assert_eq!(
        TypeDecl::simple(mask::STATIC | mask::STRING).display(Some(&name), None, Some(&name)),
        "class@anonymous\0t.php:2$0|string"
    );
}

#[test]
fn scoped_closure_argument_member_names() {
    let src = "<?php\n\
        class C { function callback() { return function(int $value) {}; } }\n\
        $a = new class { function callback() { return function(int $value) {}; } };\n\
        foreach ([(new C)->callback(), $a->callback()] as $f) {\n\
            try { $f(null); } catch (TypeError $e) { echo $e->getMessage(), \"\\n\"; }\n\
        }\n";
    assert_eq!(
        run(src),
        "C::{closure:C::callback():2}(): Argument #1 ($value) must be of type int, null given, called in t.php on line 5\n\
         class@anonymous\0t.php:3$0::{closure:class@anonymous\0t.php:3$0::callback():3}(): Argument #1 ($value) must be of type int, null given, called in t.php on line 5\n"
    );
}

#[test]
fn anonymous_parent_union_diagnostics() {
    let src = "<?php\n\
        $parent = new class {};\n\
        class_alias($parent::class, 'Base');\n\
        $child = new class extends Base {\n\
            function param(parent|string $value) {}\n\
            function result(): parent|string { return new stdClass; }\n\
        };\n\
        $child->param($parent);\n\
        try { $child->param(null); } catch (TypeError $e) {\n\
            echo $e->getMessage(), \"\\n\";\n\
        }\n\
        try { $child->result(); } catch (TypeError $e) {\n\
            echo $e->getMessage(), \"\\n\";\n\
        }\n";
    assert_eq!(
        run(src),
        "Base@anonymous\0t.php:4$1::param(): Argument #1 ($value) must be of type class@anonymous|string, null given, called in t.php on line 9\n\
         Base@anonymous::result(): Return value must be of type class@anonymous|string, stdClass returned\n"
    );
}

#[test]
fn anonymous_never_and_static_return_diagnostics() {
    let src = "<?php\n\
        $a = new class {\n\
            function neverReturn(): never {}\n\
            function staticReturn(): static|string { return new stdClass; }\n\
        };\n\
        try { $a->neverReturn(); } catch (TypeError $e) {\n\
            echo $e->getMessage(), \"\\n\";\n\
        }\n\
        try { $a->staticReturn(); } catch (TypeError $e) {\n\
            echo $e->getMessage(), \"\\n\";\n\
        }\n";
    assert_eq!(
        run(src),
        "class@anonymous\0t.php:2$0::neverReturn(): never-returning method must not implicitly return\n\
         class@anonymous::staticReturn(): Return value must be of type class@anonymous\0t.php:2$0|string, stdClass returned\n"
    );
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
fn iterable_defaults_warn_before_fatal() {
    let src = r#"<?php
function foo(iterable $iterable = null) {}
function bar(iterable $iterable = []) {}
function baz(iterable $iterable = 1) {}
echo "unreachable\n";
"#;
    let error = compile(src).expect_err("invalid iterable default");
    assert_eq!(
        error.warnings,
        vec![(
            8192,
            "foo(): Implicitly marking parameter $iterable as nullable is deprecated, the explicit nullable type must be used instead".into(),
            2,
        )]
    );
    assert_eq!(error.line, 4);
    assert_eq!(
        run(src),
        "\nDeprecated: foo(): Implicitly marking parameter $iterable as nullable is deprecated, the explicit nullable type must be used instead in t.php on line 2\n\nFatal error: Cannot use int as default value for parameter $iterable of type Traversable|array in t.php on line 4\nStack trace:\n#0 {main}\n"
    );
}

#[test]
fn iterable_null_and_array_defaults_remain_valid() {
    assert_eq!(
        run(r#"<?php
function foo(iterable $iterable = null) { var_dump($iterable); }
function bar(iterable $iterable = []) { var_dump($iterable); }
function baz(?iterable $iterable = null) { var_dump($iterable); }
foo(); bar(); baz();
"#),
        "\nDeprecated: foo(): Implicitly marking parameter $iterable as nullable is deprecated, the explicit nullable type must be used instead in t.php on line 2\nNULL\narray(0) {\n}\nNULL\n"
    );
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
fn never_implicit_return_function() {
    let src = r#"<?php
function f(): never {
    if (false) {
        throw new Exception('bad');
    }
}
try { f(); } catch (Throwable $e) { echo $e::class, ': ', $e->getMessage(), "\n"; }
"#;
    assert_eq!(
        run(src),
        "TypeError: f(): never-returning function must not implicitly return\n"
    );
}

#[test]
fn never_implicit_return_methods() {
    let src = r#"<?php
class Foo {
    public static function bar(): never {
        if (false) {
            throw new Exception('bad');
        }
    }
    public function baz(): never {}
}
class Child extends Foo {}
try { Foo::bar(); } catch (Throwable $e) { echo $e::class, ': ', $e->getMessage(), "\n"; }
try { (new Foo)->baz(); } catch (Throwable $e) { echo $e::class, ': ', $e->getMessage(), "\n"; }
try { Child::bar(); } catch (Throwable $e) { echo $e::class, ': ', $e->getMessage(), "\n"; }
try { (new Child)->baz(); } catch (Throwable $e) { echo $e::class, ': ', $e->getMessage(), "\n"; }
"#;
    assert_eq!(
        run(src),
        "TypeError: Foo::bar(): never-returning method must not implicitly return\n\
TypeError: Foo::baz(): never-returning method must not implicitly return\n\
TypeError: Foo::bar(): never-returning method must not implicitly return\n\
TypeError: Foo::baz(): never-returning method must not implicitly return\n"
    );
}

#[test]
fn never_implicit_return_closure_scope() {
    let src = r#"<?php
$plain = function(): never {};
class Foo {
    public function closure() { return function(): never {}; }
    public static function staticClosure() { return static function(): never {}; }
}
$o = new Foo;
foreach ([$plain, $o->closure(), Foo::staticClosure(), $plain->bindTo($o, Foo::class),
          $o->closure()->bindTo(null, null)] as $call) {
    try { $call(); } catch (Throwable $e) { echo $e::class, ': ', $e->getMessage(), "\n"; }
}
"#;
    assert_eq!(
        run(src),
        "TypeError: {closure:t.php:2}(): never-returning function must not implicitly return\n\
TypeError: Foo::{closure:Foo::closure():4}(): never-returning method must not implicitly return\n\
TypeError: Foo::{closure:Foo::staticClosure():5}(): never-returning method must not implicitly return\n\
TypeError: Foo::{closure:t.php:2}(): never-returning method must not implicitly return\n\
TypeError: {closure:Foo::closure():4}(): never-returning function must not implicitly return\n"
    );
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

#[test]
fn float_key_callback_removes_array_owner() {
    let src = r#"<?php
function fresh() { $a = []; $a[] = 7; return $a; }
set_error_handler(function ($level, $message) {
    global $a, $mode;
    if ($mode === 0) { $a = null; }
    elseif ($mode === 1) { unset($a); }
    else { $a = "changed"; }
    echo $message, "\n";
});
foreach ([0, 1, 2] as $mode) {
    $a = fresh(); var_dump(isset($a[1.0E+42]));
    $a = fresh(); var_dump(empty($a[1.0E+42]));
    $a = fresh(); var_dump(\array_key_exists(1.0E+42, $a));
}
$mode = 0;
$a = fresh(); $alias =& $a; var_dump(isset($a[1.0E+42]));
$a = fresh(); var_dump(\array_key_exists(1.0E+42, $a));
"#;
    let warning = "The float 1.0E+42 is not representable as an int, cast occurred\n";
    let removed = format!("{warning}bool(false)\n{warning}bool(true)\n{warning}bool(false)\n");
    // Unsetting the handler's local global binding leaves the caller's
    // reference and array alive; null/replacement assignments do not.
    let unbound = format!("{warning}bool(true)\n{warning}bool(false)\n{warning}bool(true)\n");
    assert_eq!(
        run_array_lookup(src),
        format!("{removed}{unbound}{removed}{warning}bool(false)\n{warning}bool(false)\n")
    );
}

#[test]
fn float_key_callback_preserves_shared_and_temporary_arrays() {
    let src = r#"<?php
function fresh() { $a = []; $a[] = 7; return $a; }
set_error_handler(function ($level, $message) {
    global $a;
    $a = null;
    echo $message, "\n";
});
$a = fresh(); $b = $a; var_dump(isset($a[1.0E+42]));
$a = fresh(); $b = $a; var_dump(array_key_exists(1.0E+42, $a));
var_dump(isset(fresh()[1.0E+42]));
var_dump(array_key_exists(1.0E+42, fresh()));
var_dump(empty(fresh()[1.0E+42]));
"#;
    let warning = "The float 1.0E+42 is not representable as an int, cast occurred\n";
    let expected = format!("{warning}bool(true)\n").repeat(4);
    assert_eq!(
        run_array_lookup(src),
        format!("{expected}{warning}bool(false)\n")
    );
}

#[test]
fn float_key_callback_exception_aborts_lookup() {
    let src = r#"<?php
function fresh() { $a = []; $a[] = 7; return $a; }
set_error_handler(function () { throw new Exception("stopped"); });
$a = fresh();
try { var_dump(isset($a[1.0E+42])); }
catch (Exception $e) { echo $e->getMessage(), "\n"; }
try { var_dump(empty($a[1.0E+42])); }
catch (Exception $e) { echo $e->getMessage(), "\n"; }
try { var_dump(array_key_exists(1.0E+42, $a)); }
catch (Exception $e) { echo $e->getMessage(), "\n"; }
"#;
    assert_eq!(run_array_lookup(src), "stopped\nstopped\nstopped\n");
}

#[test]
fn array_key_exists_specialization_is_float_literal_only() {
    use crate::opcode::Op;

    for key in [
        "true",
        "false",
        "0",
        "'0'",
        "null",
        "1.5",
        "$key",
        "$resource",
        "[]",
        "new stdClass",
    ] {
        let src = format!("<?php array_key_exists({key}, $array);");
        let unit = compile(&src).expect("valid call");
        assert!(!unit
            .main
            .code
            .ops
            .iter()
            .any(|op| matches!(op, Op::ArrayKeyExists { .. })));
        assert!(unit
            .main
            .code
            .ops
            .iter()
            .any(|op| matches!(op, Op::DoCall { .. })));
    }
    for src in [
        "<?php namespace N; array_key_exists(1e42, $array);",
        "<?php array_key_exists(key: 1e42, array: $array);",
        "<?php $f = 'array_key_exists'; $f(1e42, $array);",
    ] {
        let unit = compile(src).expect("valid fallback call");
        assert!(!unit
            .main
            .code
            .ops
            .iter()
            .any(|op| matches!(op, Op::ArrayKeyExists { .. })));
    }
    let unit = compile("<?php array_key_exists(1e42, $array);").expect("float call");
    assert!(unit
        .main
        .code
        .ops
        .iter()
        .any(|op| matches!(op, Op::InitFCall { .. })));
    assert!(unit
        .main
        .code
        .ops
        .iter()
        .any(|op| matches!(op, Op::Send { .. })));
    assert!(unit
        .main
        .code
        .ops
        .iter()
        .any(|op| matches!(op, Op::ArrayKeyExists { dst: None, .. })));
}

#[test]
fn array_key_exists_native_parity_and_side_effects() {
    let src = r#"<?php
set_error_handler(function ($level, $message) { echo $message, "\n"; });
$a = [0 => 7, 1 => null, "" => 9];
var_dump(array_key_exists(false, $a), array_key_exists(true, $a));
var_dump(array_key_exists("0", $a), array_key_exists(null, $a));
try { array_key_exists([], $a); }
catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { array_key_exists(new stdClass, $a); }
catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$key = "before";
function change() { global $key; $key = "after"; echo "array\n"; return ["before" => 1]; }
var_dump(array_key_exists($key, change()));
function key_expr() { echo "key\n"; return "before"; }
var_dump(array_key_exists(key_expr(), change()));
var_dump(array_key_exists($missing_key, change()));
try { array_key_exists(1e42, $missing_array); }
catch (TypeError $e) {
    echo $e->getMessage(), "\n";
    echo $e->getTrace()[0]["function"], "\n";
}
"#;
    // Both array_key_exists calls finish before var_dump prints either
    // result, so the null-key diagnostic precedes both boolean values.
    assert_eq!(
        run_array_lookup(src),
        "bool(true)\nbool(true)\nUsing null as the key parameter for array_key_exists() is deprecated, use an empty string instead\nbool(true)\nbool(true)\nCannot access offset of type array on array\nCannot access offset of type stdClass on array\narray\nbool(true)\nkey\narray\nbool(true)\nUndefined variable $missing_key\narray\nUsing null as the key parameter for array_key_exists() is deprecated, use an empty string instead\nbool(false)\nUndefined variable $missing_array\narray_key_exists(): Argument #2 ($array) must be of type array, null given\narray_key_exists\n"
    );
}

#[test]
fn array_key_exists_namespace_shadow_and_float_fallback() {
    let src = r#"<?php
namespace N;
function array_key_exists($key, $array) { echo "shadow\n"; return "shadow"; }
$a = [0 => 7];
var_dump(array_key_exists(1e42, $a));
set_error_handler(function ($level, $message) { echo $message, "\n"; });
function array_expr() { echo "array\n"; return [0 => 7]; }
var_dump(\array_key_exists(1e42, array_expr()));
try { \array_key_exists(1e42, null); }
catch (\TypeError $e) {
    echo $e->getMessage(), "\n";
    echo $e->getTrace()[0]["function"], "\n";
}
"#;
    assert_eq!(
        run_array_lookup(src),
        "shadow\nstring(6) \"shadow\"\narray\nThe float 1.0E+42 is not representable as an int, cast occurred\nbool(true)\narray_key_exists(): Argument #2 ($array) must be of type array, null given\narray_key_exists\n"
    );
}

#[test]
fn array_key_exists_specialized_call_can_resolve_to_user_function() {
    let src = r#"<?php
function array_key_exists($key, &$array) { $array = "changed"; return "user"; }
$a = [0 => 7];
var_dump(array_key_exists(1e42, $a), $a);
"#;
    assert_eq!(run(src), "string(4) \"user\"\nstring(7) \"changed\"\n");
}

#[test]
fn array_key_exists_resource_key_uses_native_diagnostic() {
    let src = r#"<?php
set_error_handler(function ($level, $message) { echo $message, "\n"; });
$resource = test_resource();
$a = [42 => null];
var_dump(array_key_exists($resource, $a));
"#;
    assert_eq!(
        run_array_lookup(src),
        "Resource ID#42 used as offset, casting to integer (42)\nbool(true)\n"
    );
}

/// `zend_check_type_slow` resolves string/array callables for user
/// parameter and return types without `IS_CALLABLE_SUPPRESS_DEPRECATIONS`:
/// the deprecations are raised once per check, at the callee's declaration
/// (RECV) or the `return` line; a `string` member of the type short-circuits.
#[test]
fn user_callable_type_emits_callable_deprecations() {
    let src = r#"<?php
set_error_handler(function ($level, $message, $file, $line) { echo "$line: $message\n"; });
class A { public static function foo() {} }
class B extends A {
    function p(callable $c) { echo "p\n"; }
    function u(callable|string $c) { echo "u\n"; }
    function n(?callable $c) { echo "n\n"; }
    function r(): callable { return ["B", "parent::foo"]; }
    function t() {
        $this->p("self::foo");
        $this->p(["static", "foo"]);
        $this->u("parent::foo");
        $this->n(null);
        $this->r();
        $this->p("parent::foo");
        try { $this->p("self::nope"); } catch (TypeError $e) { echo $e::class, "\n"; }
    }
}
(new B)->t();
"#;
    assert_eq!(
        run(src),
        "5: Use of \"self\" in callables is deprecated\n\
         p\n\
         5: Use of \"static\" in callables is deprecated\n\
         p\n\
         u\n\
         n\n\
         8: Callables of the form [\"B\", \"parent::foo\"] are deprecated\n\
         5: Use of \"parent\" in callables is deprecated\n\
         p\n\
         5: Use of \"self\" in callables is deprecated\n\
         TypeError\n"
    );
}

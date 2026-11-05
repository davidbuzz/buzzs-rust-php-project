//! End-to-end checks of reflection (and closely related engine builtins)
//! against the outputs of the php-src `ext/reflection` `.phpt` tests.

use php_runtime::Value;
use php_vm::ext::{Extension, ModuleBuilder};
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

fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

/// bug49719.phpt / property_exists.phpt: a parent's private property is
/// not a property of the subclass.
#[test]
fn property_exists_ignores_inherited_private_properties() {
    assert_eq!(
        run(r"<?php
class A { private $a; private $c; }
class B extends A { private $b; private $c; }
class C extends B {}
var_dump(property_exists('B', 'a'), property_exists(new B, 'a'));
var_dump(property_exists('B', 'b'), property_exists('B', 'c'), property_exists('C', 'c'));
"),
        "bool(false)\nbool(false)\nbool(true)\nbool(true)\nbool(false)\n"
    );
}

/// ReflectionClass_setStaticPropertyValue_003.phpt /
/// ReflectionProperty_typed_static.phpt: writes through a static property
/// that holds a typed reference honour the reference's type sources.
#[test]
fn static_setters_check_typed_reference_sources() {
    assert_eq!(
        run(r#"<?php
class Test { public static $x; public static int $y = 2; }
Test::$x =& Test::$y;
$rc = new ReflectionClass('Test');
try { $rc->setStaticPropertyValue("x", "foo"); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$rc->setStaticPropertyValue("x", "42");
var_dump(Test::$y);
$rp = new ReflectionProperty('Test', 'x');
try { $rp->setValue(null, "bar"); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$rp->setValue(null, "7");
var_dump(Test::$y);
"#),
        "Cannot assign string to reference held by property Test::$y of type int\nint(42)\n\
Cannot assign string to reference held by property Test::$y of type int\nint(7)\n"
    );
}

/// ReflectionProperty_isWritable_readonly.phpt: an initialized readonly
/// property is writable once inside `__clone()`.
#[test]
fn is_writable_sees_reinitable_readonly_properties_while_cloning() {
    assert_eq!(
        run(r"<?php
class A {
    public readonly int $a;
    public function __construct() { $this->a = 42; }
    public function __clone() {
        $rp = new ReflectionProperty($this, 'a');
        var_dump($rp->isWritable(A::class, $this));
        $this->a = 43;
        var_dump($rp->isWritable(A::class, $this));
    }
}
var_dump((new ReflectionProperty(A::class, 'a'))->isWritable(A::class, new A));
clone new A;
"),
        "bool(false)\nbool(true)\nbool(false)\n"
    );
}

/// ReflectionReference_errors.phpt: the private constructor is what
/// rejects `new ReflectionReference()`.
#[test]
fn reflection_reference_constructor_is_private() {
    assert_eq!(
        run(r#"<?php
try { new ReflectionReference(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
$a = [1]; $r = &$a[0];
var_dump(ReflectionReference::fromArrayElement($a, 0) instanceof ReflectionReference);
"#),
        "Call to private ReflectionReference::__construct() from global scope\nbool(true)\n"
    );
}

/// bug74949.phpt: `Closure::__invoke` of a closure without parameters or
/// return type has no arginfo, so `__toString()` prints no parameter list.
#[test]
fn closure_invoke_without_arginfo_prints_no_parameters() {
    assert_eq!(
        run(r#"<?php
$f = function () {};
echo new ReflectionMethod($f, "__invoke");
$g = function (): int {};
echo new ReflectionMethod($g, "__invoke");
"#),
        "Method [ <internal> public method __invoke ] {\n}\n\
Method [ <internal> public method __invoke ] {\n\n  - Parameters [0] {\n  }\n  - Return [ int ]\n}\n"
    );
}

/// gh9447.phpt: `new parent()` in a parameter default is a constant
/// expression, checked only when evaluated (Zend/tests/constexpr/
/// new_self_parent.phpt, invalid_parent_const_ref_leak.phpt).
#[test]
fn relative_class_names_in_constant_expressions_resolve_lazily() {
    assert_eq!(
        run(r#"<?php
namespace App;
class Foo {
    public function bar($c = new parent(), $d = new self(), $e = parent::X) {}
    const B = parent::C;
}
foreach ((new \ReflectionMethod(Foo::class, 'bar'))->getParameters() as $p) {
    echo $p, "\n";
}
function invalid($x = new self) {}
try { invalid(); } catch (\Error $e) { echo $e->getMessage(), "\n"; }
try { (new Foo)->bar(); } catch (\Error $e) { echo $e->getMessage(), "\n"; }
try { Foo::B; } catch (\Error $e) { echo $e->getMessage(), "\n"; }
"#),
        "Parameter #0 [ <optional> $c = new parent() ]\n\
Parameter #1 [ <optional> $d = new self() ]\n\
Parameter #2 [ <optional> $e = parent::X ]\n\
Cannot access \"self\" when no class scope is active\n\
Cannot access \"parent\" when current class scope has no parent\n\
Cannot access \"parent\" when current class scope has no parent\n"
    );
}

/// Zend/tests/bug70918.phpt, class_name_as_scalar_error_00[5-7].phpt: file
/// scope does not know its class scope at compile time, so relative class
/// names fail when executed.
#[test]
fn relative_class_names_in_file_scope_fail_at_runtime() {
    assert_eq!(
        run(r#"<?php
try { static::x; } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { parent::x; } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { new self; } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { var_dump(self::class); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { var_dump(parent::class); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#),
        "Cannot access \"static\" when no class scope is active\n\
Cannot access \"parent\" when no class scope is active\n\
Cannot access \"self\" when no class scope is active\n\
Cannot use \"self\" in the global scope\n\
Cannot use \"parent\" in the global scope\n"
    );
}

/// gh22681/ReflectionEnum_case_doc_comment.phpt: enum case doc comments are
/// kept by the compiler (also for `eval()`'d code, which has no file to
/// re-read), NUL bytes included.
#[test]
fn enum_case_doc_comments_survive_eval() {
    assert_eq!(
        run("<?php eval(\"enum Demo {\\n /** F\\0oo */\\n case C;\\n case D;\\n}\");
var_dump((new ReflectionEnumUnitCase(Demo::class, 'C'))->getDocComment());
var_dump((new ReflectionEnumUnitCase(Demo::class, 'D'))->getDocComment());
"),
        "string(11) \"/** F\0oo */\"\nbool(false)\n"
    );
}

/// attributes/ReflectionAttribute_newInstance_exception.phpt: the attribute
/// constructor is called from the attribute's location.
#[test]
fn attribute_constructor_is_called_from_the_attribute_location() {
    assert_eq!(
        run(r#"<?php
#[Attribute]
class A {
    public function __construct() { throw new Exception('Test'); }
}
class Foo {
    #[A]
    public function bar() {}
}
try {
    (new ReflectionMethod(Foo::class, 'bar'))->getAttributes()[0]->newInstance();
} catch (Exception $e) {
    echo $e->getTraceAsString(), "\n";
}
"#),
        "#0 t.php(7): A->__construct()\n#1 t.php(11): ReflectionAttribute->newInstance()\n#2 {main}\n"
    );
}

/// ReflectionFiber_bug_gh11121_*.phpt: a multi-line method call is made
/// from the line of the method name.
#[test]
fn method_call_site_is_the_method_name_line() {
    assert_eq!(
        run(r#"<?php
class X {
    function start() { echo (new Exception)->getTrace()[0]['line'], "\n"; }
    static function s() { echo (new Exception)->getTrace()[0]['line'], "\n"; }
}
(new X(
))
    ->start();
X::
    s(
    );
"#),
        "8\n10\n"
    );
}

/// gh21768.phpt: an internal class lists its inherited properties first
/// (DOMDocument after DOMNode's), a user class its own first.
#[test]
fn internal_classes_list_inherited_properties_first() {
    assert_eq!(
        run(r#"<?php
class P { public $a; } class Q extends P { public $b; }
foreach ([Q::class, ErrorException::class] as $c) {
    foreach ((new ReflectionClass($c))->getProperties() as $rp) echo $rp->getName(), " ";
    echo "\n";
}
"#),
        "b a \nmessage code file line severity \n"
    );
}

/// An extension with internal defaults that name (deprecated) constants.
struct Defaults;

impl Extension for Defaults {
    fn name(&self) -> &'static str {
        "reflection_defaults_test"
    }

    fn register(&self, module: &mut ModuleBuilder<'_>) {
        module.constant("RD_A", Value::Int(1));
        module.constant("RD_B", Value::Int(4));
        module.constant_deprecated("RD_OLD", Value::Int(2), "8.4", "use RD_A");
        module.function(
            "rd_flags(int $flags = RD_A|RD_B, int $old = RD_OLD): int",
            |_vm, _args| Ok(Value::Int(0)),
        );
    }
}

/// internal_parameter_default_value/check_all.phpt: flag combinations
/// evaluate to their bitwise or, and deprecated constants warn.
#[test]
fn internal_defaults_evaluate_flags_and_deprecated_constants() {
    let sink = Sink::default();
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &[&Defaults]);
    vm.run_script(
        br"<?php
foreach ((new ReflectionFunction('rd_flags'))->getParameters() as $p) {
    var_dump($p->getDefaultValue());
}
",
        "t.php",
    );
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(
        output,
        "int(5)\n\nDeprecated: Constant RD_OLD is deprecated since 8.4, use RD_A in t.php on line 3\nint(2)\n"
    );
}

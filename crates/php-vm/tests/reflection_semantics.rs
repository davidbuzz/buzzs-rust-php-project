// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! End-to-end checks of reflection (and closely related engine builtins)
//! against the outputs of the php-src `ext/reflection` `.phpt` tests.

mod common;

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

/// [`run`] with the standard library ([`common::vm_with_stdlib`]).
fn run_std(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = common::vm_with_stdlib(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

/// The properties an interface declares are the properties of what
/// implements or extends it (`do_interface_implementation()`), listed
/// after its own; an interface with a hooked property is abstract; and a
/// backed property that implements an abstract one comes in the class's
/// own declaration order, after the inherited backed ones. Expected
/// output from php8.5.
#[test]
fn interface_properties_are_inherited() {
    assert_eq!(
        run(r"<?php
interface HookedInterface { public string $foo { get; } }
interface Wider extends HookedInterface { public int $n { get; set; } }
abstract class AbstractHooked implements HookedInterface {
    abstract public string $bar { get; }
    public int $backed = 1;
}
abstract class Both extends AbstractHooked implements Wider {}
class Concrete extends AbstractHooked { public string $foo = 'Foo'; public string $bar = 'Bar'; }
foreach (['HookedInterface', 'Wider', 'AbstractHooked', 'Both', 'Concrete'] as $c) {
    $r = new ReflectionClass($c);
    echo $c, ':';
    foreach ($r->getProperties() as $p) {
        echo ' ', $p->class, '::$', $p->getName(), $p->isAbstract() ? ' abstract' : '', $p->isVirtual() ? ' virtual' : '', ',';
    }
    echo PHP_EOL, '  ', $r->isAbstract() ? 'abstract' : 'concrete', ', foo ', property_exists($c, 'foo') ? 'of ' . $r->getProperty('foo')->getDeclaringClass()->getName() : 'missing', PHP_EOL;
}
foreach (get_object_vars(new Concrete) as $k => $v) { echo $k, '=', $v, ' '; }
echo PHP_EOL;
class Missing implements HookedInterface {}
"),
        "HookedInterface: HookedInterface::$foo abstract virtual,\n  abstract, foo of HookedInterface\n\
         Wider: Wider::$n abstract virtual, HookedInterface::$foo abstract virtual,\n  abstract, foo of HookedInterface\n\
         AbstractHooked: AbstractHooked::$bar abstract virtual, AbstractHooked::$backed, HookedInterface::$foo abstract virtual,\n  abstract, foo of HookedInterface\n\
         Both: AbstractHooked::$bar abstract virtual, AbstractHooked::$backed, HookedInterface::$foo abstract virtual, Wider::$n abstract virtual,\n  abstract, foo of HookedInterface\n\
         Concrete: Concrete::$foo, Concrete::$bar, AbstractHooked::$backed,\n  concrete, foo of Concrete\n\
         backed=1 foo=Foo bar=Bar \n\
         \nFatal error: Class Missing contains 1 abstract method and must therefore be declared abstract or implement the remaining method (HookedInterface::$foo::get) in t.php on line 20\nStack trace:\n#0 {main}\n"
    );
}

/// `self` and `parent` in a closure's signature are not replaced by the
/// class it is written in (`zend_is_scope_known()`: a closure can be bound
/// to another scope): reflection shows them as written, and a call checks
/// them against the scope the closure has then. Expected output from
/// php8.5.
#[test]
fn self_and_parent_types_of_a_closure_follow_its_scope() {
    assert_eq!(
        run(r"<?php
namespace N;
class A {
    static function s() { return fn (): self => new self; }
    static function bad() { return fn (): self => new \stdClass; }
    static function u() { return function (self $x): self { return $x; }; }
    function m(self $x): self { return $x; }
}
class B extends A { static function v() { return fn (parent $x): parent => $x; } }
class C {}
var_dump(get_class((A::s())()));
try { (A::bad())(); } catch (\TypeError $e) { echo $e->getMessage(), PHP_EOL; }
var_dump(get_class((A::u())(new B)));
$rebound = \Closure::bind(A::u(), null, C::class);
var_dump(get_class($rebound(new C)));
try { $rebound(new A); } catch (\TypeError $e) { echo get_class($e), PHP_EOL; }
var_dump(get_class((B::v())(new A)));
try { (B::v())(new C); } catch (\TypeError $e) { echo get_class($e), PHP_EOL; }
foreach ([A::s(), A::u(), B::v(), $rebound, (new A)->m(...)] as $f) {
    $r = new \ReflectionFunction($f);
    echo $r->getReturnType()->getName();
    foreach ($r->getParameters() as $p) { echo ' / ', $p->getType()->getName(); }
    echo PHP_EOL;
}
"),
        "string(3) \"N\\A\"\n\
         N\\A::{closure:N\\A::bad():5}(): Return value must be of type N\\A, stdClass returned\n\
         string(3) \"N\\B\"\nstring(3) \"N\\C\"\nTypeError\nstring(3) \"N\\A\"\nTypeError\n\
         self\nself / self\nparent / parent\nself / self\nN\\A / N\\A\n"
    );
}

/// A float default is printed with its fraction (`1.0`, `0.0`, `-0.0`:
/// `smart_str_append_scalar()`), in a parameter's and a property's string
/// and inside an array, so that the text evaluates to the same float; a
/// constant's string has the plain conversion. A proxy generator that
/// re-reads the printed default turned `-0.0` into the integer 0.
/// Expected output from php8.5.
#[test]
fn float_defaults_are_printed_with_their_fraction() {
    assert_eq!(
        run(r"<?php
function f($a = -0.0, $b = 1.0, $c = 0.0, $d = [1.5, -0.0, 2.0], $e = 1e100, $g = .5, $h = -1.5) {}
foreach ((new ReflectionFunction('f'))->getParameters() as $p) { echo $p, PHP_EOL; }
class P { public $x = 1.0; public $y = [-0.0]; const C = 1.0; }
echo new ReflectionProperty('P', 'x'), new ReflectionProperty('P', 'y'), new ReflectionClassConstant('P', 'C');
"),
        "Parameter #0 [ <optional> $a = -0.0 ]\n\
         Parameter #1 [ <optional> $b = 1.0 ]\n\
         Parameter #2 [ <optional> $c = 0.0 ]\n\
         Parameter #3 [ <optional> $d = [1.5, -0.0, 2.0] ]\n\
         Parameter #4 [ <optional> $e = 1.0E+100 ]\n\
         Parameter #5 [ <optional> $g = 0.5 ]\n\
         Parameter #6 [ <optional> $h = -1.5 ]\n\
         Property [ public $x = 1.0 ]\n\
         Property [ public $y = [-0.0] ]\n\
         Constant [ public float C ] { 1 }\n"
    );
}

/// bug49719.phpt / `property_exists.phpt`: a parent's private property is
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

/// `ReflectionClass_setStaticPropertyValue_003.phpt` /
/// `ReflectionProperty_typed_static.phpt`: writes through a static property
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

/// `ReflectionProperty_isWritable_readonly.phpt`: an initialized readonly
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

/// `ReflectionReference_errors.phpt`: the private constructor is what
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
/// `new_self_parent.phpt`, `invalid_parent_const_ref_leak.phpt`).
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

/// Zend/tests/bug70918.phpt, `class_name_as_scalar_error_00`[5-7].phpt: file
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

/// `Zend/tests/class_name/self_class_const_in_unknown_scope.phpt`: `self::class`
/// and `parent::class` in an evaluated constant expression (a global `const`
/// value, a closure parameter default) report `ZEND_AST_CLASS_NAME`'s error,
/// always naming "self"; eval code inherits the calling method's scope.
#[test]
fn relative_class_names_in_const_exprs_without_scope() {
    assert_eq!(
        run(r#"<?php
class T { function m() { eval('const FOO = self::class;'); return FOO; } }
var_dump((new T)->m());
$f = function ($a = self::class) { return $a; };
try { var_dump($f()); } catch (Error $e) { echo $e->getMessage(), "\n"; }
$g = function ($a = parent::class) { return $a; };
try { var_dump($g()); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { eval('const Q = parent::class;'); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { var_dump((function () { return self::class; })()); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#),
        "string(1) \"T\"\n\
Cannot use \"self\" when no class scope is active\n\
Cannot use \"self\" when no class scope is active\n\
Cannot use \"self\" when no class scope is active\n\
Cannot use \"self\" in the global scope\n"
    );
}

/// `gh22681/ReflectionEnum_case_doc_comment.phpt`: enum case doc comments are
/// kept by the compiler (also for `eval()`'d code, which has no file to
/// re-read), NUL bytes included.
#[test]
fn enum_case_doc_comments_survive_eval() {
    assert_eq!(
        run(
            "<?php eval(\"enum Demo {\\n /** F\\0oo */\\n case C;\\n case D;\\n}\");
var_dump((new ReflectionEnumUnitCase(Demo::class, 'C'))->getDocComment());
var_dump((new ReflectionEnumUnitCase(Demo::class, 'D'))->getDocComment());
"
        ),
        "string(11) \"/** F\0oo */\"\nbool(false)\n"
    );
}

/// `attributes/ReflectionAttribute_newInstance_exception.phpt`: the attribute
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

/// `ReflectionFiber_bug_gh11121`_*.phpt: a multi-line method call is made
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
/// (`DOMDocument` after `DOMNode`'s), a user class its own first.
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
        module.function_static(
            "rd_flags(int $flags = RD_A|RD_B, int $old = RD_OLD): int",
            |_vm, _args| Ok(Value::Int(0)),
        );
    }
}

/// `internal_parameter_default_value/check_all.phpt`: flag combinations
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

/// GH-9447: parameter defaults are exported with the names `use` imports
/// resolve to (class names and constants).
#[test]
fn parameter_default_export_resolves_imports() {
    let out = run(r#"<?php
namespace App;
use SomewhereElse\Qux;
use SomewhereElse\Sub;
use const Other\LIMIT;
class Foo {
    public function bar(
        $b = new Bar(),
        $c = new parent(),
        $f = new Qux(),
        $g = new namespace\Qux(),
        $h = [Sub\Deep::X, LIMIT],
        $i = new \Qux(),
    ) {}
}
foreach ((new \ReflectionMethod(Foo::class, 'bar'))->getParameters() as $p) {
    echo $p, "\n";
}
"#);
    assert_eq!(
        out,
        "Parameter #0 [ <optional> $b = new \\App\\Bar() ]\n\
         Parameter #1 [ <optional> $c = new parent() ]\n\
         Parameter #2 [ <optional> $f = new \\SomewhereElse\\Qux() ]\n\
         Parameter #3 [ <optional> $g = new \\App\\Qux() ]\n\
         Parameter #4 [ <optional> $h = [\\SomewhereElse\\Sub\\Deep::X, Other\\LIMIT] ]\n\
         Parameter #5 [ <optional> $i = new \\Qux() ]\n"
    );
}

#[test]
fn call_trampoline_closure_reflects_variadic_arguments() {
    // trampoline_closure_named_arguments: `zend_call_trampoline_arginfo`.
    let out = run(r#"<?php
class Test {
    public static function __callStatic($name, $args) {}
}
$rf = new ReflectionFunction(Test::fail(...));
$params = $rf->getParameters();
echo isset($params[1]) ? "many" : "one", " ", $rf->getName(), " ", $params[0]->getName(), "\n";
var_dump($params[0]->isVariadic(), $params[0]->getType()->getName());
"#);
    assert_eq!(out, "one fail arguments\nbool(true)\nstring(5) \"mixed\"\n");
}

/// A stand-in `array_map` that marks the calls that were not inlined.
struct ArrayMapStub;

impl Extension for ArrayMapStub {
    fn name(&self) -> &'static str {
        "array_map_stub_test"
    }

    fn register(&self, module: &mut ModuleBuilder<'_>) {
        module.function_static(
            "array_map(?callable $callback, $array, ...$arrays): array",
            |_vm, _args| Ok(Value::Int(42)),
        );
    }
}

/// `zend_compile_func_array_map()`: `array_map(f(...), $array)` compiles into
/// a foreach loop calling `f` directly (keys kept, no Closure, no
/// `array_map()` frame, `ZEND_TYPE_ASSERT` on the array); other callbacks
/// and literal non-arrays keep the real call
/// (`Zend/tests/functions/zend_call_function_deprecated_frame.phpt`, gh14003).
#[test]
fn array_map_with_first_class_callable_is_inlined() {
    let sink = Sink::default();
    let mut vm = Vm::with_extensions(
        &VmOptions::default(),
        Box::new(sink.clone()),
        &[&ArrayMapStub],
    );
    vm.run_script(
        br"<?php
function foo($v) { return $v . '!'; }
function boom($v) { throw new Exception('x'); }
var_dump(array_map(foo(...), ['a' => 'x', 3 => 'y']));
try { array_map(boom(...), [1]); } catch (Exception $e) { echo $e->getTraceAsString(), PHP_EOL; }
$s = 'str';
try { array_map(foo(...), $s); } catch (TypeError $e) { echo $e->getMessage(), PHP_EOL; }
var_dump(array_map('foo', [1]), array_map(foo(...), 'lit'));
",
        "t.php",
    );
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(
        output,
        "array(2) {\n  [\"a\"]=>\n  string(2) \"x!\"\n  [3]=>\n  string(2) \"y!\"\n}\n#0 t.php(5): boom(1)\n#1 {main}\narray_map(): Argument #2 ($array) must be of type array, string given\nint(42)\nint(42)\n"
    );
}

/// Attribute arguments print like `zend_ast_export` after
/// `zend_compile_const_expr`_*(): closures as `Closure(name)` (named after
/// the function whose attributes they are in), first-class-callable
/// function names resolved (`closure_const_expr` and `first_class_callable`
/// `attributes_ast_printing_runtime.phpt`). The printer re-reads the
/// declaring file, so the scripts are written to disk.
#[test]
fn attribute_arguments_print_compiled_closures_and_resolved_callables() {
    let run_file = |name: &str, src: &str| {
        let file = format!("phprust-{}-{name}.php", std::process::id());
        let path = std::env::temp_dir().join(file);
        std::fs::write(&path, src).expect("write script");
        let sink = Sink::default();
        let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
        vm.run_script(src.as_bytes(), &path.to_string_lossy());
        drop(vm);
        let _ = std::fs::remove_file(&path);
        let output = sink.0.borrow().clone();
        String::from_utf8(output).expect("UTF-8 output")
    };
    assert_eq!(
        run_file(
            "closure",
            r"<?php

#[Attr(static function ($foo) {
    echo $foo;
})]
function foo() { }

foreach ((new ReflectionFunction('foo'))->getAttributes() as $attribute) {
    echo $attribute;
}
"
        ),
        "Attribute [ Attr ] {\n  - Arguments [1] {\n    Argument #0 [ Closure({closure:foo():3}) ]\n  }\n}\n"
    );
    assert_eq!(
        run_file(
            "fcc",
            r"<?php

namespace Test;

class Clazz {
    #[Attr(strrev(...), \strrev(...), Clazz::foo(...), self::foo(...))]
    function foo() { }
}

foreach ((new \ReflectionMethod(Clazz::class, 'foo'))->getAttributes() as $attribute) {
    echo $attribute;
}
"
        ),
        "Attribute [ Test\\Attr ] {\n  - Arguments [4] {\n    Argument #0 [ Test\\strrev(...) ]\n    Argument #1 [ \\strrev(...) ]\n    Argument #2 [ \\Test\\Clazz::foo(...) ]\n    Argument #3 [ self::foo(...) ]\n  }\n}\n"
    );
}

/// Pure reflection getters (`flags::PURE`: `getName()`, `isStatic()`,
/// `getShortName()`, `getAttributes()` with at most a name) are called
/// without a frame; one that fails (an uninitialized `ReflectionMethod`,
/// an invalid flag, `IS_INSTANCEOF` of a missing class) is made again
/// with its frame, so the error and its trace are unchanged, and a user
/// override is a user call. Expected output: php-src's (PHP 8.4 CLI).
#[test]
fn pure_reflection_getters() {
    let src = r#"<?php
namespace App;
#[\Attribute] class Tag { function __construct(public $v = 1) {} }
#[\Attribute] class Other {}
class Base { #[Tag(2)] public static function sm() {} #[Tag] #[Other] public function im(int $a, ?string $b = null) {} private $p; public int $typed = 1; }
class MyRM extends \ReflectionMethod { public function getName(): string { return "custom:" . parent::getName(); } }
function run() {
    $c = new \ReflectionClass(Base::class);
    $uninit = (new \ReflectionClass(\ReflectionMethod::class))->newInstanceWithoutConstructor();
    $tagName = Tag::class; $flagsI = \ReflectionAttribute::IS_INSTANCEOF;
    for ($i = 0; $i < 2; $i++) {
        foreach ($c->getMethods() as $m) {
            echo $m->getName(), ' ', var_export($m->isStatic(), true), ' ', $m->getShortName(), ' ', count($m->getAttributes()), ' ', count($m->getAttributes(Tag::class)), ' ', count($m->getAttributes($tagName)), ' ', count($m->getAttributes(Tag::class, $flagsI)), "\n";
            foreach ($m->getParameters() as $p) { echo '  ', $p->getName(), ' ', $p->getType()?->getName(), "\n"; }
        }
        foreach ($c->getProperties() as $p) { echo $p->getName(), ' ', var_export($p->isStatic(), true), "\n"; }
        echo $c->getName(), ' ', $c->getShortName(), ' ', count($c->getAttributes()), "\n";
        echo (new MyRM(Base::class, 'im'))->getName(), "\n";
        try { $uninit->getName(); } catch (\Error $e) { echo get_class($e), ': ', $e->getMessage(), ' @', $e->getTrace()[0]['function'] ?? '-', "\n"; }
        try { $m->getAttributes(Tag::class, 99); } catch (\ValueError $e) { echo $e->getMessage(), "\n"; }
        try { $m->getAttributes('NoSuchClass', $flagsI); } catch (\Error $e) { echo $e->getMessage(), "\n"; }
        $x = $c->getMethod('im'); echo $x->getName(), "\n";
        echo (new \ReflectionMethod(Base::class, 'sm'))->getName(), "\n";
    }
}
run();
"#;
    let expected = r#"sm true sm 1 1 1 1
im false im 2 1 1 1
  a int
  b string
p false
typed false
App\Base Base 0
custom:im
Error: Internal error: Failed to retrieve the reflection object @getName
ReflectionFunctionAbstract::getAttributes(): Argument #2 ($flags) must be a valid attribute filter flag
Class "NoSuchClass" not found
im
sm
sm true sm 1 1 1 1
im false im 2 1 1 1
  a int
  b string
p false
typed false
App\Base Base 0
custom:im
Error: Internal error: Failed to retrieve the reflection object @getName
ReflectionFunctionAbstract::getAttributes(): Argument #2 ($flags) must be a valid attribute filter flag
Class "NoSuchClass" not found
im
sm
"#;
    assert_eq!(run(src), expected);
}

/// The other pure reflection getters (class, function and method
/// modifiers and kinds, parameters, return and parameter types) on user,
/// internal, abstract, final, interface, trait and enum declarations,
/// closures and variadics; every one of them on an uninitialized object,
/// whose error comes from the call made again with its frame. Expected
/// output: php-src's (PHP 8.4 CLI).
#[test]
#[allow(clippy::too_many_lines)] // a script and its expected output
fn pure_reflection_getters_all() {
    let src = r#"<?php
namespace App;
interface I {} trait T { public function t() {} } enum E: string { case A = 'a'; }
abstract class Base implements I { use T; abstract protected function ab(); final public static function fs(int ...$v): ?int { return null; } }
final class C extends Base { public function __construct(public readonly int $x = 1, &$r = null, ?array $q = [], int|string $u = 0) {} protected function ab() {} private function pv(): static { return $this; } }
function f(\Countable&\Traversable $a, $b = PHP_EOL, ?C ...$c): void {}
function dump($v) { return var_export($v, true); }
function run() {
    $targets = [new \ReflectionClass(C::class), new \ReflectionClass(Base::class), new \ReflectionClass(I::class), new \ReflectionClass(T::class), new \ReflectionClass(E::class), new \ReflectionClass(\Exception::class)];
    foreach ($targets as $c) {
        echo $c->getName(), ' ', dump([$c->isInternal(), $c->isUserDefined(), $c->isInterface(), $c->isTrait(), $c->isEnum(), $c->isAbstract(), $c->isFinal(), $c->getModifiers(), $c->getNamespaceName(), $c->getParentClass() ? $c->getParentClass()->getName() : false, count($c->getTraits())]), "\n";
    }
    $fns = [new \ReflectionFunction('App\f'), new \ReflectionFunction('strlen'), new \ReflectionFunction(fn(int $z = 3) => $z), new \ReflectionMethod(C::class, '__construct'), new \ReflectionMethod(Base::class, 'fs'), new \ReflectionMethod(C::class, 'pv'), new \ReflectionMethod(C::class, 'ab')];
    foreach ($fns as $f) {
        echo $f->getName(), ' ', dump([$f->isInternal(), $f->isUserDefined(), $f->isVariadic(), $f->getNamespaceName(), $f->getNumberOfParameters(), $f->getNumberOfRequiredParameters(), $f->hasReturnType(), (string)$f->getReturnType()]), "\n";
        if ($f instanceof \ReflectionMethod) { echo '  ', dump([$f->isPublic(), $f->isPrivate(), $f->isProtected(), $f->isAbstract(), $f->isFinal(), $f->isConstructor(), $f->getModifiers()]), "\n"; }
        foreach ($f->getParameters() as $p) {
            $t = $p->getType();
            echo '  $', $p->getName(), ' ', dump([$p->isPassedByReference(), $p->hasType(), (string)$t, $p->allowsNull(), $p->getPosition(), $p->isOptional(), $p->isDefaultValueAvailable(), $p->isVariadic(), $t instanceof \ReflectionNamedType ? [$t->isBuiltin(), $t->allowsNull(), $t->getName()] : get_debug_type($t)]), "\n";
        }
    }
    foreach ([\ReflectionClass::class, \ReflectionMethod::class, \ReflectionFunction::class, \ReflectionParameter::class, \ReflectionNamedType::class, \ReflectionProperty::class] as $rc) {
        $u = (new \ReflectionClass($rc))->newInstanceWithoutConstructor();
        foreach (['getName', 'isInternal', 'getModifiers', 'getPosition', 'isBuiltin', 'getParentClass', 'getTraits', 'getParameters', 'isStatic', 'allowsNull'] as $m) {
            if (!method_exists($u, $m)) continue;
            try { echo $rc, '::', $m, ' ', dump($u->$m()), "\n"; } catch (\Throwable $e) { echo $rc, '::', $m, ' ', get_class($e), ': ', $e->getMessage(), ' #0 ', $e->getTrace()[0]['class'] ?? '', '->', $e->getTrace()[0]['function'] ?? '', "\n"; }
            try { echo $rc, '::', $m, ' direct ', dump(match ($m) { 'getName' => $u->getName(), 'isInternal' => $u->isInternal(), default => null }), "\n"; } catch (\Throwable $e) { echo '  direct ', get_class($e), ': ', $e->getMessage(), ' #0 ', $e->getTrace()[0]['function'] ?? '', "\n"; }
        }
    }
}
run();
"#;
    let expected = r#"App\C array (
  0 => false,
  1 => true,
  2 => false,
  3 => false,
  4 => false,
  5 => false,
  6 => true,
  7 => 32,
  8 => 'App',
  9 => 'App\\Base',
  10 => 0,
)
App\Base array (
  0 => false,
  1 => true,
  2 => false,
  3 => false,
  4 => false,
  5 => true,
  6 => false,
  7 => 64,
  8 => 'App',
  9 => false,
  10 => 1,
)
App\I array (
  0 => false,
  1 => true,
  2 => true,
  3 => false,
  4 => false,
  5 => false,
  6 => false,
  7 => 0,
  8 => 'App',
  9 => false,
  10 => 0,
)
App\T array (
  0 => false,
  1 => true,
  2 => false,
  3 => true,
  4 => false,
  5 => false,
  6 => false,
  7 => 0,
  8 => 'App',
  9 => false,
  10 => 0,
)
App\E array (
  0 => false,
  1 => true,
  2 => false,
  3 => false,
  4 => true,
  5 => false,
  6 => true,
  7 => 32,
  8 => 'App',
  9 => false,
  10 => 0,
)
Exception array (
  0 => true,
  1 => false,
  2 => false,
  3 => false,
  4 => false,
  5 => false,
  6 => false,
  7 => 0,
  8 => '',
  9 => false,
  10 => 0,
)
App\f array (
  0 => false,
  1 => true,
  2 => true,
  3 => 'App',
  4 => 3,
  5 => 1,
  6 => true,
  7 => 'void',
)
  $a array (
  0 => false,
  1 => true,
  2 => 'Countable&Traversable',
  3 => false,
  4 => 0,
  5 => false,
  6 => false,
  7 => false,
  8 => 'ReflectionIntersectionType',
)
  $b array (
  0 => false,
  1 => false,
  2 => '',
  3 => true,
  4 => 1,
  5 => true,
  6 => true,
  7 => false,
  8 => 'null',
)
  $c array (
  0 => false,
  1 => true,
  2 => '?App\\C',
  3 => true,
  4 => 2,
  5 => true,
  6 => false,
  7 => true,
  8 => 
  array (
    0 => false,
    1 => true,
    2 => 'App\\C',
  ),
)
strlen array (
  0 => true,
  1 => false,
  2 => false,
  3 => '',
  4 => 1,
  5 => 1,
  6 => true,
  7 => 'int',
)
  $string array (
  0 => false,
  1 => true,
  2 => 'string',
  3 => false,
  4 => 0,
  5 => false,
  6 => false,
  7 => false,
  8 => 
  array (
    0 => true,
    1 => false,
    2 => 'string',
  ),
)
{closure:App\run():13} array (
  0 => false,
  1 => true,
  2 => false,
  3 => '',
  4 => 1,
  5 => 0,
  6 => false,
  7 => '',
)
  $z array (
  0 => false,
  1 => true,
  2 => 'int',
  3 => false,
  4 => 0,
  5 => true,
  6 => true,
  7 => false,
  8 => 
  array (
    0 => true,
    1 => false,
    2 => 'int',
  ),
)
__construct array (
  0 => false,
  1 => true,
  2 => false,
  3 => '',
  4 => 4,
  5 => 0,
  6 => false,
  7 => '',
)
  array (
  0 => true,
  1 => false,
  2 => false,
  3 => false,
  4 => false,
  5 => true,
  6 => 1,
)
  $x array (
  0 => false,
  1 => true,
  2 => 'int',
  3 => false,
  4 => 0,
  5 => true,
  6 => true,
  7 => false,
  8 => 
  array (
    0 => true,
    1 => false,
    2 => 'int',
  ),
)
  $r array (
  0 => true,
  1 => false,
  2 => '',
  3 => true,
  4 => 1,
  5 => true,
  6 => true,
  7 => false,
  8 => 'null',
)
  $q array (
  0 => false,
  1 => true,
  2 => '?array',
  3 => true,
  4 => 2,
  5 => true,
  6 => true,
  7 => false,
  8 => 
  array (
    0 => true,
    1 => true,
    2 => 'array',
  ),
)
  $u array (
  0 => false,
  1 => true,
  2 => 'string|int',
  3 => false,
  4 => 3,
  5 => true,
  6 => true,
  7 => false,
  8 => 'ReflectionUnionType',
)
fs array (
  0 => false,
  1 => true,
  2 => true,
  3 => '',
  4 => 1,
  5 => 0,
  6 => true,
  7 => '?int',
)
  array (
  0 => true,
  1 => false,
  2 => false,
  3 => false,
  4 => true,
  5 => false,
  6 => 49,
)
  $v array (
  0 => false,
  1 => true,
  2 => 'int',
  3 => false,
  4 => 0,
  5 => true,
  6 => false,
  7 => true,
  8 => 
  array (
    0 => true,
    1 => false,
    2 => 'int',
  ),
)
pv array (
  0 => false,
  1 => true,
  2 => false,
  3 => '',
  4 => 0,
  5 => 0,
  6 => true,
  7 => 'static',
)
  array (
  0 => false,
  1 => true,
  2 => false,
  3 => false,
  4 => false,
  5 => false,
  6 => 4,
)
ab array (
  0 => false,
  1 => true,
  2 => false,
  3 => '',
  4 => 0,
  5 => 0,
  6 => false,
  7 => '',
)
  array (
  0 => false,
  1 => false,
  2 => true,
  3 => false,
  4 => false,
  5 => false,
  6 => 2,
)
ReflectionClass::getName ReflectionClass::getName Error: Internal error: Failed to retrieve the reflection object #0 ReflectionClass->getName
ReflectionClass::getName direct   direct Error: Internal error: Failed to retrieve the reflection object #0 getName
ReflectionClass::isInternal ReflectionClass::isInternal Error: Internal error: Failed to retrieve the reflection object #0 ReflectionClass->isInternal
ReflectionClass::isInternal direct   direct Error: Internal error: Failed to retrieve the reflection object #0 isInternal
ReflectionClass::getModifiers ReflectionClass::getModifiers Error: Internal error: Failed to retrieve the reflection object #0 ReflectionClass->getModifiers
ReflectionClass::getModifiers direct NULL
ReflectionClass::getParentClass ReflectionClass::getParentClass Error: Internal error: Failed to retrieve the reflection object #0 ReflectionClass->getParentClass
ReflectionClass::getParentClass direct NULL
ReflectionClass::getTraits ReflectionClass::getTraits Error: Internal error: Failed to retrieve the reflection object #0 ReflectionClass->getTraits
ReflectionClass::getTraits direct NULL
ReflectionMethod::getName ReflectionMethod::getName Error: Internal error: Failed to retrieve the reflection object #0 ReflectionFunctionAbstract->getName
ReflectionMethod::getName direct   direct Error: Internal error: Failed to retrieve the reflection object #0 getName
ReflectionMethod::isInternal ReflectionMethod::isInternal Error: Internal error: Failed to retrieve the reflection object #0 ReflectionFunctionAbstract->isInternal
ReflectionMethod::isInternal direct   direct Error: Internal error: Failed to retrieve the reflection object #0 isInternal
ReflectionMethod::getModifiers ReflectionMethod::getModifiers Error: Internal error: Failed to retrieve the reflection object #0 ReflectionMethod->getModifiers
ReflectionMethod::getModifiers direct NULL
ReflectionMethod::getParameters ReflectionMethod::getParameters Error: Internal error: Failed to retrieve the reflection object #0 ReflectionFunctionAbstract->getParameters
ReflectionMethod::getParameters direct NULL
ReflectionMethod::isStatic ReflectionMethod::isStatic Error: Internal error: Failed to retrieve the reflection object #0 ReflectionFunctionAbstract->isStatic
ReflectionMethod::isStatic direct NULL
ReflectionFunction::getName ReflectionFunction::getName Error: Internal error: Failed to retrieve the reflection object #0 ReflectionFunctionAbstract->getName
ReflectionFunction::getName direct   direct Error: Internal error: Failed to retrieve the reflection object #0 getName
ReflectionFunction::isInternal ReflectionFunction::isInternal Error: Internal error: Failed to retrieve the reflection object #0 ReflectionFunctionAbstract->isInternal
ReflectionFunction::isInternal direct   direct Error: Internal error: Failed to retrieve the reflection object #0 isInternal
ReflectionFunction::getParameters ReflectionFunction::getParameters Error: Internal error: Failed to retrieve the reflection object #0 ReflectionFunctionAbstract->getParameters
ReflectionFunction::getParameters direct NULL
ReflectionFunction::isStatic ReflectionFunction::isStatic Error: Internal error: Failed to retrieve the reflection object #0 ReflectionFunctionAbstract->isStatic
ReflectionFunction::isStatic direct NULL
ReflectionParameter::getName ReflectionParameter::getName Error: Internal error: Failed to retrieve the reflection object #0 ReflectionParameter->getName
ReflectionParameter::getName direct   direct Error: Internal error: Failed to retrieve the reflection object #0 getName
ReflectionParameter::getPosition ReflectionParameter::getPosition Error: Internal error: Failed to retrieve the reflection object #0 ReflectionParameter->getPosition
ReflectionParameter::getPosition direct NULL
ReflectionParameter::allowsNull ReflectionParameter::allowsNull Error: Internal error: Failed to retrieve the reflection object #0 ReflectionParameter->allowsNull
ReflectionParameter::allowsNull direct NULL
ReflectionNamedType::getName ReflectionNamedType::getName Error: Internal error: Failed to retrieve the reflection object #0 ReflectionNamedType->getName
ReflectionNamedType::getName direct   direct Error: Internal error: Failed to retrieve the reflection object #0 getName
ReflectionNamedType::isBuiltin ReflectionNamedType::isBuiltin Error: Internal error: Failed to retrieve the reflection object #0 ReflectionNamedType->isBuiltin
ReflectionNamedType::isBuiltin direct NULL
ReflectionNamedType::allowsNull ReflectionNamedType::allowsNull Error: Internal error: Failed to retrieve the reflection object #0 ReflectionType->allowsNull
ReflectionNamedType::allowsNull direct NULL
ReflectionProperty::getName ReflectionProperty::getName Error: Internal error: Failed to retrieve the reflection object #0 ReflectionProperty->getName
ReflectionProperty::getName direct   direct Error: Internal error: Failed to retrieve the reflection object #0 getName
ReflectionProperty::getModifiers ReflectionProperty::getModifiers Error: Internal error: Failed to retrieve the reflection object #0 ReflectionProperty->getModifiers
ReflectionProperty::getModifiers direct NULL
ReflectionProperty::isStatic ReflectionProperty::isStatic Error: Internal error: Failed to retrieve the reflection object #0 ReflectionProperty->isStatic
ReflectionProperty::isStatic direct NULL
"#;
    assert_eq!(run(src), expected);
}

/// `getAttributes($class, IS_INSTANCEOF)` finds the filter's class and
/// each attribute's class once a request (`AttrClassCache`,
/// `Vm::attr_filter_class`): subclasses and interfaces of the filter,
/// repeated calls, an attribute class an autoloader declares later (a miss
/// is not kept), an alias, a missing filter class; `method_exists()` and
/// `getMethod()` with names in other case (`Class::find_method`).
/// Expected output: php-src's (PHP 8.5).
#[test]
fn attribute_filters_find_classes_once_a_request() {
    assert_eq!(
        run_std(
            r#"<?php
interface Marker {}
#[Attribute(Attribute::TARGET_ALL | Attribute::IS_REPEATABLE)] class Base {}
#[Attribute(Attribute::TARGET_ALL | Attribute::IS_REPEATABLE)] class Child extends Base implements Marker {}
#[Attribute] class Other {}
class Target {
  #[Child] #[Other] #[Base] #[Later] #[Child]
  function m(#[Child] $p, #[Other] $q) {}
}
$m = new ReflectionMethod('Target', 'm');
for ($i = 0; $i < 2; $i++) {
  echo implode(",", array_map(fn($a) => $a->getName(), $m->getAttributes(Base::class, ReflectionAttribute::IS_INSTANCEOF))), " | ";
  echo implode(",", array_map(fn($a) => $a->getName(), $m->getAttributes(Marker::class, ReflectionAttribute::IS_INSTANCEOF))), " | ";
  echo count($m->getAttributes(Other::class, ReflectionAttribute::IS_INSTANCEOF)), " | ";
  foreach ($m->getParameters() as $p) echo $p->getName(), "=", count($p->getAttributes(Base::class, ReflectionAttribute::IS_INSTANCEOF)), " ";
  echo "\n";
}
spl_autoload_register(function ($c) { echo "[autoload $c] "; if ($c === 'Later') { eval('#[Attribute] class Later extends Base {}'); } });
echo implode(",", array_map(fn($a) => $a->getName(), $m->getAttributes(Base::class, ReflectionAttribute::IS_INSTANCEOF))), "\n";
echo implode(",", array_map(fn($a) => $a->getName(), $m->getAttributes(Base::class, ReflectionAttribute::IS_INSTANCEOF))), "\n";
class_alias('Child', 'ChildAlias');
echo count($m->getAttributes('ChildAlias', ReflectionAttribute::IS_INSTANCEOF)), " ", count($m->getAttributes('child')), "\n";
try { $m->getAttributes('Missing', ReflectionAttribute::IS_INSTANCEOF); } catch (Error $e) { echo $e->getMessage(), "\n"; }
echo method_exists('Target', 'M') ? "y" : "n", method_exists('Target', 'mX') ? "y" : "n", " ", (new ReflectionClass('Target'))->getMethod('M')->getName(), "\n";
"#
        ),
        r#"Child,Base,Child | Child,Child | 1 | p=1 q=0 
Child,Base,Child | Child,Child | 1 | p=1 q=0 
[autoload Later] Child,Base,Later,Child
Child,Base,Later,Child
2 2
[autoload Missing] Class "Missing" not found
yn m
"#
    );
}

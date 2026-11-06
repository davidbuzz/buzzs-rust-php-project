//! Script-level tests of enums and callable resolution.

use crate::builtins::classbuilder::ClassBuilder;
use crate::{Vm, VmOptions};
use php_runtime::Value;
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

const CATCH: &str = "catch (Throwable $e) { echo $e::class, ': ', $e->getMessage(), \"\\n\"; }";

#[test]
fn from_and_try_from() {
    let out = run(&format!(
        "<?php enum Foo: int {{ case A = 1; case B = 2; }} enum S: string {{ case A = 'a'; }}\n\
         var_dump(Foo::from(2), Foo::tryFrom(3), Foo::from('1'), S::tryFrom('b'));\n\
         try {{ Foo::from(3); }} {CATCH}\n\
         try {{ S::from('x'); }} {CATCH}\n\
         try {{ Foo::from('x'); }} {CATCH}"
    ));
    assert_eq!(
        out,
        "enum(Foo::B)\nNULL\nenum(Foo::A)\nNULL\n\
         ValueError: 3 is not a valid backing value for enum Foo\n\
         ValueError: \"x\" is not a valid backing value for enum S\n\
         TypeError: Foo::from(): Argument #1 ($value) must be of type int, string given\n"
    );
}

#[test]
fn duplicate_and_mismatched_backing_values() {
    let out = run(&format!(
        "<?php enum D: int {{ case A = 0; case B = 0; }} enum M: int {{ case A = 'a'; }}\n\
         try {{ var_dump(D::A); }} {CATCH}\n\
         try {{ var_dump(D::tryFrom(1)); }} {CATCH}\n\
         try {{ var_dump(M::A); }} {CATCH}\n\
         try {{ var_dump(M::A); }} {CATCH}"
    ));
    assert_eq!(
        out,
        "Error: Duplicate value in enum D for cases A and B\n\
         Error: Duplicate value in enum D for cases A and B\n\
         TypeError: Enum case type string does not match enum backing type int\n\
         TypeError: Enum case type string does not match enum backing type int\n"
    );
}

#[test]
fn internal_backed_enum_cases_are_materialized_individually() {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    ClassBuilder::new("InternalBackedEnum")
        .backed_enum("string")
        .case("First", Some(Value::Str("same".into())))
        .case("Second", Some(Value::Str("same".into())))
        .register(&mut vm);
    let script = concat!(
        "<?php echo InternalBackedEnum::First->name, '|', ",
        "InternalBackedEnum::First->value, '|', InternalBackedEnum::Second->name, '|', ",
        "InternalBackedEnum::Second->value;"
    );
    vm.run_script(script.as_bytes(), "t.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    assert_eq!(String::from_utf8_lossy(&out), "First|same|Second|same");
}

#[test]
fn cases_and_identity() {
    let out = run("<?php enum E { case X; case Y; }\n\
         var_dump(E::cases(), E::X === E::cases()[0], E::X == E::Y, E::X instanceof UnitEnum);");
    assert_eq!(
        out,
        "array(2) {\n  [0]=>\n  enum(E::X)\n  [1]=>\n  enum(E::Y)\n}\nbool(true)\nbool(false)\nbool(true)\n"
    );
}

#[test]
fn callable_resolution_errors() {
    let out = run(
        "<?php abstract class A { abstract static function a(); protected function p() {}\n\
         static function t() { var_dump(is_callable('self::p'), is_callable('parent::x')); } }\n\
         class B extends A { static function a() {} }\n\
         foreach (['A::a', [new B, 'p'], 'B::x', 'Nope::x', 'self::x', [1], 42] as $c) {\n\
         try { Closure::fromCallable($c); } catch (TypeError $e) { echo $e->getMessage(), \"\\n\"; } }\n\
         A::t();",
    );
    assert_eq!(
        out,
        "Failed to create closure from callable: cannot call abstract method A::a()\n\
         Failed to create closure from callable: cannot access protected method B::p()\n\
         Failed to create closure from callable: class B does not have a method \"x\"\n\
         Failed to create closure from callable: class \"Nope\" not found\n\
         Failed to create closure from callable: cannot access \"self\" when no class scope is active\n\
         Failed to create closure from callable: array callback must have exactly two members\n\
         Failed to create closure from callable: no array or string given\n\
         \nDeprecated: Use of \"self\" in callables is deprecated in t.php on line 2\n\
         bool(false)\nbool(false)\n"
    );
}

#[test]
fn from_parses_its_argument_like_zpp() {
    // `zend_enum_from_base`: Z_PARAM_LONG / Z_PARAM_STR under strict_types.
    let out = run(&format!(
        "<?php declare(strict_types=1); enum I: int {{ case A = 42; }} enum S: string {{ case A = '42'; }}\n\
         try {{ S::from(42); }} {CATCH}\n\
         try {{ S::tryFrom(1.5); }} {CATCH}\n\
         try {{ I::from('42'); }} {CATCH}\n\
         try {{ I::from(42.0); }} {CATCH}\n\
         var_dump(I::from(42), S::from('42'));"
    ));
    assert_eq!(
        out,
        "TypeError: S::from(): Argument #1 ($value) must be of type string, int given\n\
         TypeError: S::tryFrom(): Argument #1 ($value) must be of type string, float given\n\
         TypeError: I::from(): Argument #1 ($value) must be of type int, string given\n\
         TypeError: I::from(): Argument #1 ($value) must be of type int, float given\n\
         enum(I::A)\nenum(S::A)\n"
    );
    // Weak mode: Z_PARAM_LONG / Z_PARAM_STR_OR_LONG.
    let out = run(&format!(
        "<?php enum I: int {{ case A = 42; }} enum S: string {{ case A = '42'; case B = '1.0E+30'; }}\n\
         var_dump(S::from(42), I::from('42'), I::from(42.0), S::from(1e30));\n\
         try {{ I::from([]); }} {CATCH}\n\
         try {{ S::from([]); }} {CATCH}\n\
         try {{ I::from(1e30); }} {CATCH}"
    ));
    assert_eq!(
        out,
        "enum(S::A)\nenum(I::A)\nenum(I::A)\nenum(S::B)\n\
         TypeError: I::from(): Argument #1 ($value) must be of type int, array given\n\
         TypeError: S::from(): Argument #1 ($value) must be of type string|int, array given\n\
         TypeError: I::from(): Argument #1 ($value) must be of type int, float given\n"
    );
}

#[test]
fn link_time_enum_errors() {
    let cases = [
        (
            "<?php enum Foo { case Bar; public static function cases(): array { return []; } }",
            "Fatal error: Cannot redeclare Foo::cases()",
        ),
        (
            "<?php enum Foo: int { case Bar = 0; public static function tryFrom(int|string $v): ?self { return null; } }",
            "Fatal error: Cannot redeclare Foo::tryfrom()",
        ),
        (
            "<?php class Foo implements BackedEnum {}",
            "Fatal error: Non-enum class Foo cannot implement interface UnitEnum",
        ),
        (
            "<?php enum Foo implements BackedEnum {}",
            "Fatal error: Non-backed enum Foo cannot implement interface BackedEnum",
        ),
        (
            "<?php enum Foo implements Traversable {}",
            "Fatal error: Enum Foo must implement interface Traversable as part of either Iterator or IteratorAggregate in Unknown on line 0",
        ),
    ];
    for (src, want) in cases {
        let out = run(src);
        assert!(out.contains(want), "{src}: {out}");
    }
    // The Serializable deprecation comes from the interface hook, before
    // `zend_verify_enum` rejects the enum.
    let out = run("<?php enum Foo implements Serializable { case Bar;\n\
         public function serialize() {} public function unserialize($d) {} }");
    let dep = out.find("Deprecated: Foo implements the Serializable interface");
    let fatal = out.find("Fatal error: Enum Foo cannot implement the Serializable interface");
    assert!(matches!((dep, fatal), (Some(d), Some(f)) if d < f), "{out}");
}

#[test]
fn dim_on_object_in_constant_expression() {
    let out = run(&format!(
        "<?php enum Foo implements ArrayAccess {{ case Bar;\n\
         public function offsetGet($k): mixed {{ return 42; }}\n\
         public function offsetExists($k): bool {{ return true; }}\n\
         public function offsetSet($k, $v): void {{}}\n\
         public function offsetUnset($k): void {{}} }}\n\
         class X {{ const A = [1, [2, 3]]; const B = X::A[1][0]; const C = Foo::Bar[0]; }}\n\
         var_dump(X::B);\n\
         try {{ var_dump(X::C); }} {CATCH}"
    ));
    assert_eq!(
        out,
        "int(2)\nError: Cannot use [] on objects in constant expression\n"
    );
}

#[test]
fn case_objects_are_owned_by_their_constant() {
    // Zend/tests/enum/cases-refcount.phpt and unserialize-refcount.phpt.
    let out = run("<?php enum Foo { case Bar; }\n\
         function callCases() { Foo::cases(); }\n\
         callCases(); debug_zval_dump(Foo::Bar);\n\
         $foo = Foo::Bar; $bar = unserialize('E:7:\"Foo:Bar\";'); debug_zval_dump($foo);");
    assert_eq!(
        out,
        "object(Foo)#1 (1) refcount(2){\n  [\"name\"]=>\n  string(3) \"Bar\" interned\n}\n\
         object(Foo)#1 (1) refcount(4){\n  [\"name\"]=>\n  string(3) \"Bar\" interned\n}\n"
    );
}

/// A test extension with `cv($callable, ...$args)`: [`Vm::call_value`],
/// the `zend_call_function()` an extension makes for a callable it
/// received (and validated) through `Z_PARAM_FUNC`.
struct CvExt;

impl crate::ext::Extension for CvExt {
    fn name(&self) -> &'static str {
        "cvtest"
    }
    fn register(&self, m: &mut crate::ext::ModuleBuilder<'_>) {
        m.function("cv(mixed $callable, mixed ...$args): mixed", cv);
    }
}

fn cv(vm: &mut Vm, a: &mut crate::ext::Args) -> crate::vm::VmResult<Value> {
    let callable = a.at(0);
    let args: Vec<Value> = a.values.iter().skip(1).map(Value::deref).collect();
    vm.call_value(&callable, args)
}

fn run_cv(src: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &[&CvExt]);
    vm.run_script(src.as_bytes(), "t.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

/// `zend_call_function()` resolves the callable in the calling user
/// frame's scope (`zend_is_callable_ex`): `parent::`/`self::`/`static::`,
/// `"C::m"` binding that frame's `$this`, private methods of that scope;
/// `ext/pdo/tests/pdo_fetch_func_001.phpt` with `'parent::method'`.
#[test]
fn call_value_resolves_in_the_calling_user_frame() {
    let out = run_cv(
        r#"<?php
function f($x) { return "f($x)"; }
class A {
    public function m($x) { return __METHOD__ . "($x)"; }
    public static function s($x) { return __METHOD__ . "($x)"; }
}
class B extends A {
    private function priv($x) { return "priv($x)"; }
    public function __invoke($x) { return "invoke($x)"; }
    function run() {
        echo cv('parent::m', 1), "\n";
        echo cv('self::m', 2), "\n";
        echo cv('static::s', 3), "\n";
        echo cv([$this, 'parent::m'], 4), "\n";
        echo cv(['B', 'parent::s'], 5), "\n";
        echo cv('A::m', 6), "\n";
        echo cv([$this, 'priv'], 7), "\n";
        echo cv($this, 8), "\n";
        echo cv(fn($x) => "closure($x)", 9), "\n";
        echo cv('f', 'ten'), cv('\f', 11), "\n";
    }
}
(new B)->run();
echo cv([new B, 'A::m'], 12), "\n";
"#,
    );
    assert_eq!(
        out,
        "A::m(1)\nA::m(2)\nA::s(3)\nA::m(4)\nA::s(5)\nA::m(6)\npriv(7)\ninvoke(8)\nclosure(9)\n\
         f(ten)f(11)\nA::m(12)\n"
    );
}

/// The callable deprecations belong to the validation (`Z_PARAM_FUNC`,
/// here `is_callable()`), not to the call of the cached `fcc`.
#[test]
fn call_value_leaves_the_callable_deprecations_to_the_validator() {
    let out = run_cv(
        r#"<?php
class A { public function m($x) { return __METHOD__ . "($x)"; } }
class B extends A {
    function run() {
        $c = [$this, 'parent::m'];
        var_dump(is_callable($c));
        echo cv($c, 1), cv($c, 2), "\n";
    }
}
(new B)->run();
"#,
    );
    assert_eq!(
        out,
        "\nDeprecated: Callables of the form [\"B\", \"parent::m\"] are deprecated in t.php on line 6\n\
         bool(true)\nA::m(1)A::m(2)\n"
    );
}

/// A value `zend_is_callable_ex` rejects keeps the dynamic-call errors.
#[test]
fn call_value_keeps_the_dynamic_call_errors_for_invalid_callables() {
    let out = run_cv(
        r#"<?php
class A { public function m($x) { return 1; } }
function t($c) { try { cv($c); } catch (Error $e) { echo $e->getMessage(), "\n"; } }
t('nope');
t([new A, 'zz']);
t('parent::m');
t([new A, 'parent::m']);
t(42);
"#,
    );
    assert_eq!(
        out,
        "Call to undefined function nope()\n\
         Call to undefined method A::zz()\n\
         Class \"parent\" not found\n\
         Cannot use \"parent\" when current class scope has no parent\n\
         Value of type int is not callable\n"
    );
}

/// `$f()` (`zend_init_dynamic_call_array`) hands `[$obj, "parent::m"]`'s
/// whole method name to `get_method`: an undefined method or `__call`;
/// `"parent::m"` strings fetch a class named `parent`. Only
/// `zend_is_callable_ex` (`call_user_func()`) splits the name.
#[test]
fn dynamic_call_does_not_split_a_method_name_with_a_class_prefix() {
    let out = run_cv(
        r#"<?php
class A { public function m($x) { return __METHOD__ . "($x)"; } }
class B extends A {
    function run() {
        foreach ([[$this, 'parent::m'], 'parent::m'] as $f) {
            try { echo $f(1), "\n"; } catch (Error $e) { echo $e->getMessage(), "\n"; }
        }
    }
}
class C extends A {
    function __call($n, $a) { return "__call($n)"; }
    function run() { $f = [$this, 'parent::m']; echo $f(1), "\n"; }
}
(new B)->run();
(new C)->run();
echo call_user_func([new B, 'parent::m'], 2), "\n";
"#,
    );
    assert_eq!(
        out,
        "Call to undefined method B::parent::m()\n\
         Class \"parent\" not found\n\
         __call(parent::m)\n\
         \nDeprecated: Callables of the form [\"B\", \"parent::m\"] are deprecated in t.php on line 16\n\
         A::m(2)\n"
    );
}

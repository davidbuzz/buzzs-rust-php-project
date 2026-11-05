//! Inheritance, trait and late-static-binding regressions
//! (Zend/tests/{inheritance,interfaces,traits,lsb}).
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

/// Runs `source` as `t.php`, returning the exit status and the output.
fn run_status(source: &str) -> (i32, String) {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    (status, String::from_utf8(output).expect("UTF-8 output"))
}

fn run(source: &str) -> String {
    let (status, output) = run_status(source);
    assert_eq!(status, 0, "{output}");
    output
}

#[test]
fn class_references_reject_fetch_keywords() {
    let cases = [
        ("<?php\nclass Foo extends static {}\n", "\"static\" as class name"),
        ("<?php\nclass Foo implements self {}\n", "\"self\" as interface name"),
        ("<?php\ninterface Foo extends static {}\n", "\"static\" as interface name"),
        ("<?php\nclass Foo {\n    use parent;\n}\n", "\"parent\" as trait name"),
        (
            "<?php\ntrait T { function f() {} }\nclass C {\n    use T { static::f insteadof T; }\n}\n",
            "\"static\" as trait name",
        ),
        (
            "<?php\ntrait T { function f() {} }\nclass C {\n    use T { T::f insteadof static; }\n}\n",
            "\"static\" as trait name",
        ),
    ];
    for (src, what) in cases {
        let (status, out) = run_status(src);
        assert_ne!(status, 0, "{src}");
        assert!(
            out.contains(&format!(
                "Fatal error: Cannot use {what}, as it is reserved in t.php"
            )),
            "{src}: {out}"
        );
    }
    // `zend_resolve_class_name()` rejects the fully qualified spelling.
    let (_, out) = run_status("<?php\nclass Foo extends \\static {}\n");
    assert!(
        out.contains("Fatal error: '\\static' is an invalid class name in t.php"),
        "{out}"
    );
}

#[test]
fn constructor_access_uses_its_own_scope_without_abstract_prototype() {
    let out = run(r#"<?php
class A { protected function __construct() {} }
class B extends A {
    static function make() {
        try { new C; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    }
}
class C extends A { protected function __construct() { echo "C\n"; } }
B::make();
abstract class P { abstract protected function __construct(); }
class Q extends P { protected function __construct() { echo "Q\n"; } }
class R extends P {
    protected function __construct() {}
    static function make() { new Q; }
}
R::make();
class X { private function __construct() {} }
class Y extends X { static function make() { return new Y; } }
try { Y::make(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        "Call to protected C::__construct() from scope B\nQ\nCall to private X::__construct() from scope Y\n"
    );
}

#[test]
fn ancestor_private_static_is_a_visibility_error() {
    let out = run(r#"<?php
class A {
    private static $x = 1;
    static function viaStatic() { return static::$x; }
}
class B extends A {
    static function viaSelf() { return self::$x; }
}
var_dump(B::viaStatic());
try { B::viaSelf(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { B::$x; } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { B::$nope; } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        "int(1)\nCannot access private property B::$x\nCannot access private property B::$x\nAccess to undeclared static property B::$nope\n"
    );
}

#[test]
fn implicit_nullable_default_is_exempt_from_optional_before_required() {
    let out = run("<?php\nclass A {}\nfunction f(A $a = null, $n) {}\nfunction g(?A $a = null, $b = 1, $n, $m) {}\n");
    assert_eq!(
        out,
        "\nDeprecated: f(): Implicitly marking parameter $a as nullable is deprecated, the explicit nullable type must be used instead in t.php on line 3\n\
         \nDeprecated: g(): Optional parameter $a declared before required parameter $m is implicitly treated as a required parameter in t.php on line 4\n\
         \nDeprecated: g(): Optional parameter $b declared before required parameter $m is implicitly treated as a required parameter in t.php on line 4\n"
    );
}

#[test]
fn private_final_method_warns_once() {
    let out = run("<?php\nclass C {\n    private final function f() {}\n}\n");
    assert_eq!(
        out.matches("Private methods cannot be final").count(),
        1,
        "{out}"
    );
}

#[test]
fn direct_static_trait_calls_are_deprecated() {
    let out = run(r#"<?php
trait T {
    public static function foo() { echo "foo\n"; }
    public static function __callStatic($n, $a) { echo "magic $n\n"; }
}
class C { use T; }
T::foo();
T::bar();
C::foo();
C::bar();
set_error_handler(function ($no, $msg) { throw new ErrorException($msg); });
try { T::baz(); } catch (ErrorException $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        "\nDeprecated: Calling static trait method T::foo is deprecated, it should only be called on a class using the trait in t.php on line 7\nfoo\n\
         \nDeprecated: Calling static trait method T::bar is deprecated, it should only be called on a class using the trait in t.php on line 8\nmagic bar\n\
         foo\nmagic bar\n\
         ErrorException: Calling static trait method T::baz is deprecated, it should only be called on a class using the trait\n"
    );
}

#[test]
fn defined_and_constant_follow_scope_and_hide_trait_constants() {
    let out = run(r#"<?php
trait T { public const K = 42; }
class P { private const PRIV = 1; }
class A {
    static function test() {
        var_dump(defined("static::ok"));
        if (defined("static::ok")) { echo constant("static::ok"), "\n"; }
    }
}
class B extends A { const ok = "ok"; }
A::test();
B::test();
var_dump(defined('T::K'), defined('P::PRIV'));
try { constant('T::K'); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { constant('static::K'); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        "bool(false)\nbool(true)\nok\nbool(false)\nbool(false)\nCannot access trait constant T::K directly\nCannot access \"static\" when no class scope is active\n"
    );
}

#[test]
fn is_callable_emits_callable_deprecations() {
    let out = run(r#"<?php
class A { static function t() { var_dump(is_callable("static::ok")); } }
class B extends A { static function ok() {} }
A::t();
B::t();
"#);
    let dep = "\nDeprecated: Use of \"static\" in callables is deprecated in t.php on line 2\n";
    assert_eq!(out, format!("{dep}bool(false)\n{dep}bool(true)\n"));
}

#[test]
fn call_static_trampoline_forwards_called_scope() {
    let out = run(r#"<?php
class A {
    static function who() { echo static::class, "\n"; }
    static function __callStatic($n, $a) { static::who(); }
}
class B extends A {
    static function __callStatic($n, $a) { parent::missing(); }
}
B::missing();
A::missing();
"#);
    assert_eq!(out, "B\nA\n");
}

#[test]
fn redeclaration_rules_for_interfaces_constants_and_static_properties() {
    let cases = [
        (
            "<?php\ninterface I {}\nclass C implements I, I {}\n",
            "Fatal error: Class C cannot implement previously implemented interface I in t.php on line 3",
        ),
        (
            "<?php\ninterface I {}\ninterface J extends I, I {}\n",
            "Fatal error: Interface J cannot implement previously implemented interface I in t.php on line 3",
        ),
        (
            "<?php\ninterface I {\n    protected const X = 1;\n}\n",
            "Fatal error: Access type for interface constant I::X must be public in t.php on line 3",
        ),
        (
            "<?php\nclass A { protected const X = 1; }\nclass B extends A { private const X = 2; }\n",
            "Fatal error: Access level to B::X must be protected (as in class A) or weaker in t.php on line 3",
        ),
        (
            "<?php\ninterface I { const X = 1; }\nclass B implements I { protected const X = 2; }\n",
            "Fatal error: Access level to B::X must be public (as in interface I) in t.php on line 3",
        ),
        (
            "<?php\nclass A { public $p; }\nclass B extends A { public static $p; }\n",
            "Fatal error: Cannot redeclare non static A::$p as static B::$p in t.php on line 3",
        ),
        (
            "<?php\nclass A { protected static $p; }\nclass B extends A { private static $p; }\n",
            "Fatal error: Access level to B::$p must be protected (as in class A) or weaker in t.php on line 3",
        ),
    ];
    for (src, expected) in cases {
        let (status, out) = run_status(src);
        assert_ne!(status, 0, "{src}");
        assert!(out.contains(expected), "{src}: {out}");
    }
    assert_eq!(
        run("<?php\nclass A { private $p; }\nclass B extends A { public static $p; }\nclass C { public static $q; }\nclass D extends C { public static $q; }\necho \"ok\\n\";\n"),
        "ok\n"
    );
}

#[test]
fn method_visibility_errors_name_the_declaring_class_and_scope_call_wins() {
    assert_eq!(
        run(r#"<?php
class A { function __call($n, $a) { echo "A::__call($n)\n"; } }
class B extends A {
    function __call($n, $a) { echo "B::__call($n)\n"; }
    function t() { A::x(); parent::y(); }
}
(new B)->t();
class P { private function m() {} private static function s() {} }
class C extends P { function t() { $this->M(); } static function u() { C::s(); } }
try { (new C)->t(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { C::u(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
class K { private function __construct() {} static function f() { return new D; } }
class D extends K { function __construct() { parent::__construct(); } }
try { K::f(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#),
        "B::__call(x)\nB::__call(y)\nCall to private method P::M() from scope C\nCall to private method P::s() from scope C\nCannot call private K::__construct()\n"
    );
}

/// `do_inherit_constant_check`: ambiguous constants inherited from two
/// sources and weakened constant visibility are compile errors, while a
/// redeclaring class or an interface reached twice is fine.
#[test]
fn inherited_constant_conflicts_and_visibility() {
    let cases = [
        (
            "interface I1 { const C = 1; } interface I2 { const C = 1; } class C implements I1, I2 {}",
            "Class C inherits both I1::C and I2::C, which is ambiguous",
        ),
        (
            "class C { const C = 1; } interface I { const C = 1; } class C2 extends C implements I {}",
            "Class C2 inherits both C::C and I::C, which is ambiguous",
        ),
        (
            "interface I1 { const C = 1; } interface I2 { const C = 2; } interface I3 extends I1, I2 {}",
            "Interface I3 inherits both I1::C and I2::C, which is ambiguous",
        ),
        (
            "interface I { public const FOO = 1; } class C implements I { protected const FOO = 1; }",
            "Access level to C::FOO must be public (as in interface I)",
        ),
        (
            "class P { protected const X = 1; } class C extends P { private const X = 2; }",
            "Access level to C::X must be protected (as in class P) or weaker",
        ),
        (
            "trait T { protected const X = 1; } interface I { const X = 1; } class C implements I { use T; }",
            "Access level to C::X must be public (as in interface I)",
        ),
        (
            "interface I { final public const X = 1; } class C implements I { const X = 2; }",
            "C::X cannot override final constant I::X",
        ),
    ];
    for (src, msg) in cases {
        let (status, out) = run_status(&format!("<?php\n{src}\n"));
        assert_ne!(status, 0, "{src}");
        assert!(
            out.contains(&format!("Fatal error: {msg} in t.php on line 2")),
            "{src}: {out}"
        );
    }
    let ok = "<?php\ninterface I1 { const C = 1; } interface I2 { const C = 2; }\n\
              class C implements I1, I2 { const C = 3; }\n\
              interface E { final public const T = 1; } interface K extends E {} interface S extends E {}\n\
              class T implements K, S {} class P implements E {} class Q extends P implements E {}\n\
              echo C::C, Q::T, \"\\n\";\n";
    assert_eq!(run(ok), "31\n");
}

/// `zend_class_redeclaration_error` names the kind and name of the class
/// already declared, with its declaration site for user classes.
#[test]
fn class_redeclaration_names_the_existing_class() {
    let (status, out) = run_status("<?php\nclass stdclass {}\n");
    assert_ne!(status, 0);
    assert!(
        out.contains("Fatal error: Cannot redeclare class stdClass in t.php on line 2"),
        "{out}"
    );
    let (status, out) = run_status("<?php\ninterface A {}\ntrait A {}\n");
    assert_ne!(status, 0);
    assert!(
        out.contains("Fatal error: Cannot redeclare interface A (previously declared in t.php:2) in t.php on line 3"),
        "{out}"
    );
}

/// `E_STRICT` is `#[\Deprecated]` in `Zend/zend_constants.stub.php`.
#[test]
fn e_strict_constant_is_deprecated() {
    let out = run("<?php\necho E_ALL, \"\\n\";\necho E_STRICT, \"\\n\";\n");
    assert_eq!(
        out,
        "30719\n\nDeprecated: Constant E_STRICT is deprecated since 8.4, the error level was removed in t.php on line 3\n2048\n"
    );
}

/// `zend_update_class_constants` (instantiation) evaluates constants
/// without the recursion mark of `zend_get_class_constant_ex`, so an
/// autoloader may read the constant being updated (GH-10709).
#[test]
fn class_constant_update_does_not_mark_recursion() {
    let out = run(r#"<?php
class B { const C = A::C . "B"; }
spl_autoload_register(function ($class) {
    class A { const C = "A"; }
    echo B::C, "\n";
});
new B();
echo B::C, "\n";
class D { const X = self::Y; const Y = self::X; }
try { new D; } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        "AB\nAB\nCannot declare self-referencing constant self::Y\n"
    );
}

#[test]
fn constructor_visibility_is_checked_against_an_abstract_grand_prototype() {
    // bug61970_2: `do_inheritance_check_on_method` checks a constructor
    // against its abstract prototype further up.
    let (status, out) = run_status(
        "<?php\nabstract class Foo { abstract public function __construct(); }\nclass Bar extends Foo { public function __construct() {} }\nclass Baz extends Bar { protected function __construct() {} }\n",
    );
    assert_eq!(status, 255);
    assert!(
        out.contains("Fatal error: Access level to Baz::__construct() must be public (as in class Foo) in t.php on line 4"),
        "{out}"
    );
}

#[test]
fn trait_to_string_implements_stringable() {
    // stringable_trait (bug #81582).
    let out = run(r#"<?php
trait T { public function __toString(): string { return "ok"; } }
trait T2 { use T; }
class C { use T; }
class C2 { use T2; }
var_dump(new C instanceof Stringable, new C2 instanceof Stringable);
"#);
    assert_eq!(out, "bool(true)\nbool(true)\n");
}

#[test]
fn method_call_errors_name_the_declaring_class_and_called_name() {
    // access_modifiers_010, call_static_006, gh_17718_001, bug46238.
    let out = run(r#"<?php
class d { private function test2() {} }
class a extends d { public function test() { $this->TEST2(); } }
try { (new a)->test(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
interface I { public static function __callStatic($m, $a); }
try { I::bar(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
class S { static function __callStatic($m, $a) { var_dump($m); } }
try { S::__construct(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
$n = "a\0b";
S::$n();
"#);
    assert_eq!(
        out,
        "Call to private method d::TEST2() from scope a\nCannot call abstract method I::bar()\nCannot call constructor\nstring(1) \"a\"\n"
    );
}

#[test]
fn call_trampoline_keeps_references_from_call_user_func_array() {
    // bug50394: `zend_call_function` does not unwrap references for a
    // trampoline; the VM's own sends pass values.
    let out = run(r"<?php
function inc(&$x) { $x++; }
class Proxy {
    function __call($name, $args) { call_user_func_array('inc', $args); }
}
$arg = 1;
call_user_func_array([new Proxy, 'bar'], [&$arg]);
(new Proxy)->bar($arg);
var_dump($arg);
");
    assert_eq!(
        out,
        "\nWarning: inc(): Argument #1 ($x) must be passed by reference, value given in t.php on line 4\nint(2)\n"
    );
}

/// `load_delayed_classes` → `zend_exception_uncaught_error`: an exception
/// from autoloading a class a pending variance check needs is fatal, with
/// the class being linked and the autoloaded name as prefix.
#[test]
fn exception_while_autoloading_for_variance_is_a_prefixed_fatal() {
    let (status, out) = run_status(
        r#"<?php
spl_autoload_register(function ($c) {
    class Y {}
    throw new Exception("boom");
});
class A { function m(): object {} }
class B extends A { function m(): Y {} }
"#,
    );
    assert_ne!(status, 0, "{out}");
    assert!(
        out.contains(
            "Fatal error: During inheritance of B, while autoloading Y: Uncaught Exception: boom in t.php:4\nStack trace:\n"
        ),
        "{out}"
    );
    assert!(out.contains("#1 {main} in t.php on line "), "{out}");
}

#[test]
fn core_introspection_follows_php_src_master() {
    // method_exists() skips inherited private methods for class names;
    // is_a()/is_subclass_of() deprecate strings without $allow_string;
    // get_exception_handler(); instanceof on a constant folds to false.
    assert_eq!(
        run(r#"<?php
class P { private function hidden() {} private static function sp() {} }
class C extends P { private function own() {} }
var_dump(method_exists('C', 'hidden'), method_exists('C', 'sp'), method_exists('C', 'own'), method_exists(new C, 'hidden'));
var_dump(is_a('C', 'P'));
var_dump(is_a('C', 'P', true));
var_dump(is_subclass_of('C', 'P', false));
var_dump(get_exception_handler());
set_exception_handler('strlen');
var_dump(get_exception_handler());
set_exception_handler(null);
var_dump(get_exception_handler());
var_dump("abc" instanceof stdClass, 1 instanceof C);
"#),
        "bool(false)\nbool(false)\nbool(true)\nbool(true)\n\
\nDeprecated: Calling is_a() with a string when $allow_string is false in t.php on line 5\nbool(false)\n\
bool(true)\n\
\nDeprecated: Calling is_subclass_of() with a string when $allow_string is false in t.php on line 7\nbool(false)\n\
NULL\nstring(6) \"strlen\"\nNULL\nbool(false)\nbool(false)\n"
    );
}

#[test]
fn constant_class_and_method_names_are_compile_errors() {
    let cases = [
        ("<?php\nclass p { function c() { (0)::t; } }\n", "Illegal class name in t.php on line 2"),
        ("<?php\n(-0)::$prop;\n", "Illegal class name in t.php on line 2"),
        ("<?php\n[]::X::X;\n", "Illegal class name in t.php on line 2"),
        ("<?php\n$o = new stdClass;\n$o->{0}();\n", "Method name must be a string in t.php on line 3"),
        ("<?php\nstdClass::{null}();\n", "Method name must be a string in t.php on line 2"),
        (
            "<?php\nclass T { function m() { try {} catch (static $e) {} } }\n",
            "Bad class name in the catch statement in t.php on line 2",
        ),
        (
            "<?php\nnamespace A;\nfunction assert() {}\n",
            "Defining a custom assert() function is not allowed, as the function has special semantics in t.php on line 3",
        ),
    ];
    for (src, msg) in cases {
        let (status, out) = run_status(src);
        assert_ne!(status, 0, "{src}");
        assert!(out.contains(&format!("Fatal error: {msg}")), "{src}: {out}");
    }
}

/// `forward_static_call[_array]()` (`ext/standard/basic_functions.c`) keep
/// the caller's called scope when it derives from the callback's class;
/// `forward_static_call()` needs an active class scope.
#[test]
fn forward_static_call_forwards_called_scope() {
    let out = run(r#"<?php
class A { static function t() { echo static::class, "\n"; } }
class B extends A { static function go() { forward_static_call(['A', 't']); forward_static_call_array('A::t', []); call_user_func(['A', 't']); } }
function f() { echo "f\n"; }
class C { static function go() { forward_static_call('f'); } }
B::go();
C::go();
function g() { try { forward_static_call(['A', 't']); } catch (Error $e) { echo $e->getMessage(), "\n"; } }
g();
"#);
    assert_eq!(
        out,
        "B\nB\nA\nf\nCannot call forward_static_call() when no class scope is active\n"
    );
}

/// `is_a_impl` (`Zend/zend_builtin_functions.c`): a string first argument
/// without `$allow_string` is deprecated and yields false.
#[test]
fn is_a_with_string_without_allow_string_is_deprecated() {
    let out = run(r"<?php
class A {}
var_dump(is_a('A', 'A'), is_a('A', 'A', true), is_subclass_of('A', 'A', false), is_a(1, 'A'));
");
    assert_eq!(
        out,
        "\nDeprecated: Calling is_a() with a string when $allow_string is false in t.php on line 3\n\
         \nDeprecated: Calling is_subclass_of() with a string when $allow_string is false in t.php on line 3\n\
         bool(false)\nbool(true)\nbool(false)\nbool(false)\n"
    );
}

/// `ZEND_FUNCTION(method_exists)` hides private methods inherited from a
/// parent when given a class name; `get_class_methods()` quotes an unknown
/// class name (`zend_wrong_parameter_type_error`).
#[test]
fn method_exists_hides_inherited_private_for_class_names() {
    let out = run(r#"<?php
class P { private function p() {} private static function s() {} }
class Q extends P { private function q() {} }
var_dump(method_exists('Q', 'p'), method_exists('Q', 's'), method_exists('Q', 'q'), method_exists(new Q, 'p'), method_exists('P', 'p'));
try { get_class_methods("Nope"); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { get_class_methods(1); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        "bool(false)\nbool(false)\nbool(true)\nbool(true)\nbool(true)\n\
         get_class_methods(): Argument #1 ($object_or_class) must be an object or a valid class name, \"Nope\" given\n\
         get_class_methods(): Argument #1 ($object_or_class) must be an object or a valid class name, int given\n"
    );
}

/// `zend_implement_throwable`: user classes and enums cannot implement
/// `Throwable` directly; it is checked before the abstract methods.
#[test]
fn throwable_is_only_implemented_through_exception_or_error() {
    let (status, output) = run_status("<?php\nclass Failure implements Throwable {}\n");
    assert_eq!(status, 255, "{output}");
    assert!(
        output.contains(
            "Fatal error: Class Failure cannot implement interface Throwable, extend Exception or Error instead in t.php"
        ),
        "{output}"
    );
    let (status, output) = run_status("<?php\nenum Foo implements Throwable {}\n");
    assert_eq!(status, 255, "{output}");
    assert!(
        output.contains("Fatal error: Enum Foo cannot implement interface Throwable in t.php"),
        "{output}"
    );
    let output = run(
        "<?php\ninterface MyThrowable extends Throwable {}\nclass E extends Exception implements MyThrowable {}\necho get_class(new E), \"\\n\";\n",
    );
    assert_eq!(output, "E\n");
}

/// `zend_error_impl`: the deprecations recorded while a class links are
/// emitted before a fatal error of the same link (tentative_type_early_binding).
#[test]
fn recorded_link_deprecations_precede_a_link_fatal() {
    let (status, output) = run_status(
        "<?php\ninterface I { function f(): X; }\nclass A implements Serializable, I {\n    function serialize() {}\n    function unserialize($s) {}\n    function f(): Unknown {}\n}\n",
    );
    assert_eq!(status, 255, "{output}");
    assert_eq!(
        output,
        "\nDeprecated: A implements the Serializable interface, which is deprecated. Implement __serialize() and __unserialize() instead (or in addition, if support for old PHP versions is necessary) in t.php on line 3\n\nFatal error: Could not check compatibility between A::f(): Unknown and I::f(): X, because class Unknown is not available in t.php on line 6\n"
    );
}

/// `zend_lookup_class_ex`: a class still being linked is not found and
/// not autoloaded (Zend/tests/use_unlinked_class).
#[test]
fn class_being_linked_is_not_found_or_autoloaded() {
    let output = run(r#"<?php
spl_autoload_register(function ($class) {
    echo "load $class\n";
    var_dump(class_exists('A'));
    if ($class === 'I') {
        interface I {}
    }
});
class A implements I {}
var_dump(class_exists('A', false));
"#);
    assert_eq!(output, "load I\nbool(false)\nbool(true)\n");
}

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

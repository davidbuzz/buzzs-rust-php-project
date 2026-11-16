// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Script-level tests of closures (expected output from php 8.3+/master).

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
fn debug_info_of_fake_closure() {
    let out = run("<?php var_dump(strlen(...));");
    assert_eq!(
        out,
        "object(Closure)#1 (2) {\n  [\"function\"]=>\n  string(6) \"strlen\"\n  [\"parameter\"]=>\n  array(1) {\n    [\"$string\"]=>\n    string(10) \"<required>\"\n  }\n}\n"
    );
}

#[test]
fn debug_info_statics_and_uses() {
    let out =
        run("<?php $a = 1; $f = function ($x, $y = 2) use ($a) { static $n = 5; };\nvar_dump($f);");
    assert_eq!(
        out,
        "object(Closure)#1 (5) {\n  [\"name\"]=>\n  string(17) \"{closure:t.php:1}\"\n  [\"file\"]=>\n  string(5) \"t.php\"\n  [\"line\"]=>\n  int(1)\n  [\"static\"]=>\n  array(2) {\n    [\"a\"]=>\n    int(1)\n    [\"n\"]=>\n    int(5)\n  }\n  [\"parameter\"]=>\n  array(2) {\n    [\"$x\"]=>\n    string(10) \"<required>\"\n    [\"$y\"]=>\n    string(10) \"<optional>\"\n  }\n}\n"
    );
}

#[test]
fn named_method_closure_return_error_name_and_trace() {
    let out = run("<?php\n\
         class foo {\n\
             public function bar(): callable {\n\
                 $test = 'one';\n\
                 return function() use ($test): array {\n\
                     return null;\n\
                 };\n\
             }\n\
         }\n\
         $baz = new foo;\n\
         $func = $baz->bar();\n\
         try { $func(); } catch (TypeError $e) {\n\
             echo $e->getMessage(), \"\\n\", $e->getTraceAsString(), \"\\n\";\n\
         }\n");
    assert_eq!(
        out,
        "foo::{closure:foo::bar():5}(): Return value must be of type array, null returned\n\
         #0 t.php(12): foo->{closure:foo::bar():5}()\n\
         #1 {main}\n"
    );
}

#[test]
fn anonymous_method_closure_return_error_name_and_trace() {
    let out = run("<?php\n\
         $obj = new class {\n\
             public function bar(): callable {\n\
                 return function(): array { return null; };\n\
             }\n\
         };\n\
         $func = $obj->bar();\n\
         try { $func(); } catch (TypeError $e) {\n\
             echo $e->getMessage(), \"\\n\", $e->getTraceAsString(), \"\\n\";\n\
         }\n");
    assert_eq!(
        out,
        "class@anonymous::{closure:class@anonymous(): Return value must be of type array, null returned\n\
         #0 t.php(8): class@anonymous->{closure:class@anonymous\0t.php:2$0::bar():4}()\n\
         #1 {main}\n"
    );
}

#[test]
fn bind_warnings() {
    let out = run(
        "<?php class A { function m() {} static function s() {} } class B {}\n\
         $f = static function () {};\n\
         var_dump($f->bindTo(new A));\n\
         var_dump(Closure::fromCallable([new A, 'm'])->bindTo(new B));\n\
         var_dump(Closure::fromCallable([new A, 'm'])->bindTo(null));\n\
         var_dump(Closure::fromCallable('strlen')->bindTo(null, 'A'));\n\
         var_dump(Closure::fromCallable('A::s')->bindTo(null, 'B'));\n",
    );
    let w = |m: &str| format!("\nWarning: {m}, this will be an error in PHP 9 in t.php on line ");
    assert_eq!(
        out,
        format!(
            "{}3\nNULL\n{}4\nNULL\n{}5\nNULL\n{}6\nNULL\n{}7\nNULL\n",
            w("Cannot bind an instance to a static closure"),
            w("Cannot bind method A::m() to object of class B"),
            w("Cannot unbind $this of method"),
            w("Cannot rebind scope of closure created from function"),
            w("Cannot rebind scope of closure created from method"),
        )
    );
}

#[test]
fn unbind_this_of_closure_using_this() {
    let out = run(
        "<?php class A { function g() { return function () { return $this; }; } }\n\
         var_dump((new A)->g()->bindTo(null));",
    );
    assert_eq!(
        out,
        "\nWarning: Cannot unbind $this of closure using $this, this will be an error in PHP 9 in t.php on line 2\nNULL\n"
    );
}

#[test]
fn dummy_scope_and_call() {
    let out = run("<?php class A { private $x = 7; }\n\
         $f = function () { return $this->x; };\n\
         var_dump($f->call(new A));\n\
         $g = function () { return static::class; };\n\
         var_dump($g->bindTo(new stdClass)());");
    assert_eq!(out, "int(7)\nstring(8) \"stdClass\"\n");
}

#[test]
fn from_callable_errors() {
    let out = run("<?php class A { function m() {} private static function p() {} }\n\
         foreach (['A::m', 'A::p', 'A::nope', 'nofn', [1, 2]] as $c) {\n\
         try { Closure::fromCallable($c); } catch (TypeError $e) { echo $e->getMessage(), \"\\n\"; } }");
    assert_eq!(
        out,
        "Failed to create closure from callable: non-static method A::m() cannot be called statically\n\
         Failed to create closure from callable: cannot access private method A::p()\n\
         Failed to create closure from callable: class A does not have a method \"nope\"\n\
         Failed to create closure from callable: function \"nofn\" not found or invalid function name\n\
         Failed to create closure from callable: first array member is not a valid class name or object\n"
    );
}

#[test]
fn get_current_and_invoke_trampoline() {
    let out = run(
        "<?php $f = function ($n) { return $n ? Closure::getCurrent()($n - 1) + 1 : 0; };\n\
         var_dump($f(3));\n\
         try { Closure::getCurrent(); } catch (Error $e) { echo $e->getMessage(), \"\\n\"; }\n\
         $inc = function (&$n) { $n++; }; $n = 1; $inc->__invoke($n); var_dump($n);",
    );
    assert_eq!(out, "int(3)\nCurrent function is not a closure\nint(2)\n");
}

#[test]
fn fcc_cache_and_comparison() {
    let out = run(
        "<?php class A { static function s() {} } class B extends A {}\n\
         var_dump(strlen(...) === strlen(...));\n\
         var_dump(Closure::fromCallable('A::s') == Closure::fromCallable('A::s'));\n\
         var_dump(Closure::fromCallable('A::s') == Closure::fromCallable('B::s'));\n\
         var_dump(function () {} == function () {});",
    );
    assert_eq!(out, "bool(true)\nbool(true)\nbool(false)\nbool(false)\n");
}

#[test]
fn callable_names() {
    let out = run("<?php class Foo { function __invoke() {} function bar() {} }\n\
         $f = new Foo; is_callable($f(...), false, $a); is_callable($f->bar(...), false, $b);\n\
         is_callable(strlen(...), false, $c); is_callable([function () {}, '__invoke'], false, $d);\n\
         echo \"$a $b $c $d\\n\";");
    assert_eq!(out, "Foo::__invoke Foo::bar strlen Closure::__invoke\n");
}

/// A recursive call made by a static initializer sets the slot first; the
/// outer `ZEND_BIND_STATIC` then binds that reference and drops its own
/// value (`Zend/tests/static_variables/static_variables_recursive.phpt`).
#[test]
fn recursive_static_initializer_keeps_inner_value() {
    let out = run(
        "<?php function foo($i) { static $a = $i <= 2 ? foo($i + 1) : \"Done $i\"; echo $a, \"\\n\"; return $i; }\nfoo(0); foo(5);",
    );
    assert_eq!(out, "Done 3\nDone 3\nDone 3\nDone 3\nDone 3\n");
}

/// A callable conversion inside a nullsafe chain is a compile error
/// (`zend_compile_method_call()`; `first_class_callable_012/013.phpt`).
#[test]
fn nullsafe_closure_creation_is_compile_error() {
    for src in [
        "<?php\n$foo?->bar(...);\n",
        "<?php\n$foo?->foo->bar(...);\n",
    ] {
        assert_eq!(
            run(src),
            "\nFatal error: Cannot combine nullsafe operator with Closure creation in t.php on line 2\nStack trace:\n#0 {main}\n"
        );
    }
    // A nullsafe access applied to the conversion's result is fine.
    assert_eq!(run("<?php\necho strlen(...)?->__invoke('abc');"), "3");
}

/// Attribute argument-list errors are reported on the declaration's line
/// (`CG(zend_lineno)` in `zend_compile_attributes()`; `first_class_callable_011`).
#[test]
fn attribute_closure_error_uses_declaration_line() {
    let msg = "\nFatal error: Cannot create Closure as attribute argument in t.php on line ";
    assert_eq!(
        run("<?php\n#[Attribute(...)]\nclass Foo {\n\n}\n"),
        format!("{msg}3\nStack trace:\n#0 {{main}}\n")
    );
    assert_eq!(
        run("<?php\n#[Attribute(...)]\n\nfunction f() {\n}\n"),
        format!("{msg}4\nStack trace:\n#0 {{main}}\n")
    );
    assert_eq!(
        run("<?php\nfunction g(\n#[A(...)]\n\n$x) {}\n"),
        format!("{msg}2\nStack trace:\n#0 {{main}}\n")
    );
}

/// `ZEND_ADD_ARRAY_ELEMENT` with an object key raises
/// `zend_illegal_array_offset_access()` (`closure_array_key_error.phpt`).
#[test]
fn closure_as_array_literal_key() {
    let out = run(
        "<?php\ntry { var_dump([function () {} => 1]); } catch (TypeError $e) { echo $e->getMessage(); }",
    );
    assert_eq!(out, "Cannot access offset of type Closure on array");
}

/// A first-class callable reflects as `Closure [ ... ]`: the fake closure's
/// function carries `ZEND_ACC_CLOSURE` (`first_class_callable_signature.phpt`).
#[test]
fn fake_closure_reflection_string() {
    let out = run("<?php function test(int $a) {}\necho new ReflectionFunction(test(...));");
    assert_eq!(
        out,
        "Closure [ <user> function test ] {\n  @@ t.php 1 - 1\n\n  - Parameters [1] {\n    Parameter #0 [ <required> int $a ]\n  }\n}\n"
    );
}

/// `ZEND_INIT_NS_FCALL_BY_NAME` caches the global fallback in the call
/// site's run-time cache, so sites that already ran keep calling it after
/// `ns\strrev` is declared (`first_class_callable/constexpr/namespace_004`).
#[test]
fn namespaced_fallback_is_cached_per_call_site() {
    let out = run(r"<?php
namespace Foo;
function f(\Closure $c = strrev(...)) {
    echo strrev('ab'), $c('cd'), strrev(...)('ef'), PHP_EOL;
}
f();
function g() { echo strrev('gh'), PHP_EOL; }
if (true) {
    function strrev($v) { return 'ns'; }
}
f();
g();
echo strrev('x'), PHP_EOL;
");
    assert_eq!(out, "badcfe\nbadcfe\nns\nns\n");
}

/// Constant-expression first-class callables: compile-time name checks
/// and the `__callStatic` runtime error (`zend_compile_const_expr_fcc()`,
/// `zend_ast_evaluate_inner()`; `first_class_callable/constexpr/error`_*).
#[test]
fn const_expr_callable_errors() {
    assert_eq!(
        run("<?php\nclass Foo {\n    public const C = static::m(...);\n}\n"),
        "\nFatal error: \"static\" is not allowed in compile-time constants in t.php on line 3\nStack trace:\n#0 {main}\n"
    );
    assert_eq!(
        run("<?php\n\nconst C = (0)(...);\n"),
        "\nFatal error: Illegal function name in t.php on line 3\nStack trace:\n#0 {main}\n"
    );
    assert_eq!(
        run("<?php\nclass Foo { public static function __callStatic($n, $a) {} }\nconst C = Foo::m(...);\n"),
        "\nFatal error: Uncaught Error: Creating a callable for the magic __callStatic() method is not supported in constant expressions in t.php:3\nStack trace:\n#0 {main}\n  thrown in t.php on line 3\n"
    );
}

/// Argument errors, by-reference warnings and deprecations name a closure
/// after its bound scope like a method (`get_function_or_method_name`):
/// `K::{closure:K::mk():5}`, `Closure::` for an object bound without a
/// scope, the bare name only when unscoped (expected output from
/// `/usr/bin/php8.5 -n`).
#[test]
fn scoped_closure_names_in_argument_errors() {
    let out = run("<?php\n\
         set_error_handler(function ($no, $str) { echo $str, \"\\n\"; return true; });\n\
         function show($e) { echo get_class($e), ': ', $e->getMessage(), \"\\n\"; }\n\
         class K {\n\
             function mk() { return function (int $a, &$r = null) {}; }\n\
             static function smk() { return static fn(int $a) => $a; }\n\
             function dep() { return #[\\Deprecated] function () {}; }\n\
         }\n\
         $c = (new K)->mk();\n\
         try { $c(); } catch (Error $e) { show($e); }\n\
         try { $c(1, 2); } catch (Error $e) { show($e); }\n\
         try { K::smk()(); } catch (Error $e) { show($e); }\n\
         call_user_func($c, 1, 2);\n\
         (new K)->dep()();\n\
         $g = function (int $a) {};\n\
         try { $g(); } catch (Error $e) { show($e); }\n\
         try { $g->bindTo(new K)(); } catch (Error $e) { show($e); }\n\
         try { Closure::bind($g, null, K::class)(); } catch (Error $e) { show($e); }\n");
    assert_eq!(
        out,
        "ArgumentCountError: Too few arguments to function K::{closure:K::mk():5}(), 0 passed in t.php on line 10 and at least 1 expected\n\
         Error: K::{closure:K::mk():5}(): Argument #2 ($r) could not be passed by reference\n\
         ArgumentCountError: Too few arguments to function K::{closure:K::smk():6}(), 0 passed in t.php on line 12 and exactly 1 expected\n\
         K::{closure:K::mk():5}(): Argument #2 ($r) must be passed by reference, value given\n\
         Method K::{closure:K::dep():7}() is deprecated\n\
         ArgumentCountError: Too few arguments to function {closure:t.php:15}(), 0 passed in t.php on line 16 and exactly 1 expected\n\
         ArgumentCountError: Too few arguments to function Closure::{closure:t.php:15}(), 0 passed in t.php on line 17 and exactly 1 expected\n\
         ArgumentCountError: Too few arguments to function K::{closure:t.php:15}(), 0 passed in t.php on line 18 and exactly 1 expected\n"
    );
}

/// Closures without static variables share one empty store and leave it
/// off their frames; closures with them keep their own, bound copies get
/// a snapshot, fake closures use their function's, generators and loops
/// keep per-object state (expected output from `/usr/bin/php8.5 -n`).
#[test]
fn closure_static_variable_stores() {
    let out = run(r#"<?php
$a = function () { static $n = 0; return ++$n; };
$a(); $a();
$b = $a->bindTo(null);
echo $a(), $b(), $b(), $a(), "\n";
function g() { static $c = 10; return ++$c; }
$f = Closure::fromCallable('g'); $h = g(...);
echo $f(), $h(), g(), "\n";
$k = $f->bindTo(null); echo $k(), "\n";

$x = 1;
$m = function () use ($x) { return $x; };
$m2 = $m->bindTo(null);
echo $m(), $m2(), "\n";
var_dump((new ReflectionFunction($m2))->getStaticVariables());
$gen = function () { static $s = 0; yield ++$s; yield ++$s; };
foreach ($gen() as $v) echo $v; foreach ($gen() as $v) echo $v; echo "\n";
class C { function mk() { return function () { static $q = []; $q[] = count($q); return $q; }; } }
$c1 = (new C)->mk(); $c1(); $c2 = $c1->bindTo(new C); $c1();
echo implode(',', $c1()), '|', implode(',', $c2()), "\n";
for ($i = 0; $i < 3; $i++) { $t = function () { static $z = 0; return ++$z; }; echo $t(), $t(); }
echo "\n";
var_dump($a);
"#);
    assert_eq!(
        out,
        r#"3344
111213
14
11
array(1) {
  ["x"]=>
  int(1)
}
1234
0,1,2|0,1
121212
object(Closure)#1 (4) {
  ["name"]=>
  string(17) "{closure:t.php:2}"
  ["file"]=>
  string(5) "t.php"
  ["line"]=>
  int(2)
  ["static"]=>
  array(1) {
    ["n"]=>
    int(4)
  }
}
"#
    );
}

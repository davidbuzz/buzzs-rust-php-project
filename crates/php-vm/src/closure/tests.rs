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

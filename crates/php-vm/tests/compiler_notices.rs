//! Compile-time decisions with observable notices and errors: by-reference
//! returns and yields (`zend_compile_return`, `zend_compile_yield`), the
//! argument fetch mode of `zend_compile_args` (`Cannot use [] for reading`)
//! and the declaration order of enum members (ref: Zend/zend_compile.c).
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

/// Runs `source`, asserting a clean exit, and returns its output.
fn run(source: &str) -> String {
    let (status, output) = run_status(source);
    assert_eq!(status, 0, "{output}");
    output
}

fn notice(message: &str, line: u32) -> String {
    format!("\nNotice: {message} in t.php on line {line}\n")
}

const RETURNED: &str = "Only variable references should be returned by reference";
const YIELDED: &str = "Only variable references should be yielded by reference";

/// `return $a =& $b;` in a by-reference function: php-src master compiles
/// the assignment for reading, a TMP holding the dereferenced value
/// (GH-20628), so ZEND_RETURN_BY_REF notices and returns a new reference
/// that is not bound to `$b`.
#[test]
fn by_ref_return_of_assign_ref_notices() {
    let out = run("<?php\n\
         function &c() {\n\
         \x20   static $b = 1;\n\
         \x20   return $a =& $b;\n\
         }\n\
         $x =& c();\n\
         $x++;\n\
         var_dump(c());\n");
    assert_eq!(out, format!("{0}{0}int(1)\n", notice(RETURNED, 4)));
}

/// `fn&() => expr` is `return expr;`: `@f()` is compiled for reading (a
/// TMP, dereferenced), a call is fetched for writing and keeps the
/// reference a by-reference function returned.
#[test]
fn by_ref_arrow_function_returns_like_return() {
    let out = run("<?php\n\
         function &r() { static $v = 1; return $v; }\n\
         $f = fn&() => @r();\n\
         $x =& $f();\n\
         $x++;\n\
         var_dump(r());\n\
         $g = fn&() => r();\n\
         $y =& $g();\n\
         $y++;\n\
         var_dump(r());\n");
    assert_eq!(out, format!("{}int(1)\nint(2)\n", notice(RETURNED, 3)));
}

/// A by-reference call result returned from inside a `foreach` or a `try`
/// with `finally` keeps its reference through the loop free and the
/// finally block (no copy, no notice).
#[test]
fn by_ref_call_result_survives_return_unwinding() {
    let out = run("<?php\n\
         function &r() { static $v = 1; return $v; }\n\
         function &a() { foreach ([1] as $_) { return r(); } }\n\
         function &b() { try { return r(); } finally { echo \"fin\\n\"; } }\n\
         $x =& a();\n\
         $x++;\n\
         $y =& b();\n\
         $y++;\n\
         var_dump(r());\n");
    assert_eq!(out, "fin\nint(3)\n");
}

/// A by-value result returned from a by-reference function inside a loop
/// still notices once.
#[test]
fn by_value_call_result_in_loop_notices() {
    let out = run("<?php\n\
         function v() { return 5; }\n\
         function &a() {\n\
         \x20   foreach ([1] as $_) {\n\
         \x20       return v();\n\
         \x20   }\n\
         }\n\
         $x =& a();\n\
         var_dump($x);\n");
    assert_eq!(out, format!("{}int(5)\n", notice(RETURNED, 5)));
}

/// `foreach` by reference over a by-reference generator binds a yielded
/// constant or a bare `yield` silently (`ZEND_FE_FETCH_RW` wraps the
/// generator's value in a reference): only `ZEND_YIELD` notices.
#[test]
fn by_ref_foreach_over_yielded_values_is_silent() {
    let out = run("<?php\n\
         function &gen() {\n\
         \x20   $b = 1;\n\
         \x20   yield 5;\n\
         \x20   yield;\n\
         \x20   yield $b;\n\
         }\n\
         foreach (gen() as &$v) {\n\
         \x20   var_dump($v);\n\
         }\n");
    assert_eq!(out, format!("{}int(5)\nNULL\nint(1)\n", notice(YIELDED, 4)));
}

/// The loop variable is bound to the generator's current value, as
/// `ZEND_FE_FETCH_RW` makes it a reference in place.
#[test]
fn by_ref_foreach_binds_the_generator_value() {
    let out = run("<?php\n\
         function &gen() {\n\
         \x20   yield 1;\n\
         }\n\
         $g = gen();\n\
         foreach ($g as &$v) {\n\
         \x20   $v = 7;\n\
         \x20   var_dump($g->current());\n\
         }\n");
    assert_eq!(out, format!("{}int(7)\n", notice(YIELDED, 3)));
}

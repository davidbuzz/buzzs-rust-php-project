//! Destructors run while an exception unwinds the stack
//! (`cleanup_live_vars`, `zend_leave_helper`), chained to the exception
//! in flight, and exceptions thrown by output handlers at shutdown.

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

fn run(source: &str) -> (i32, String) {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "test.php");
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    (status, output)
}

#[test]
fn local_destructor_exception_is_chained_to_the_unwinding_one() {
    let (status, output) = run(r#"<?php
class D {
    public function __construct(public $n) {}
    public function __destruct() { throw new Exception("dtor {$this->n}"); }
}
function f() {
    $d = new D(1);
    throw new Exception("orig");
}
try {
    f();
} catch (Exception $e) {
    echo $e->getMessage(), " <- ", $e->getPrevious()?->getMessage(), "\n";
}
"#);
    assert_eq!(status, 0);
    assert_eq!(output, "dtor 1 <- orig\n");
}

#[test]
fn foreach_iterator_is_released_in_the_frame_being_left() {
    let (status, output) = run(r#"<?php
class It implements Iterator {
    private $i = 0;
    public function current(): mixed { return $this->i; }
    public function key(): mixed { return $this->i; }
    public function next(): void { $this->i++; }
    public function rewind(): void { $this->i = 0; }
    public function valid(): bool { return $this->i < 3; }
    public function __destruct() { echo "It dtor\n", (new Exception)->getTraceAsString(), "\n"; }
}
function g() {
    foreach (new It as $v) {
        throw new Exception("in loop");
    }
}
try {
    g();
} catch (Exception $e) {
    echo "caught\n";
}
"#);
    assert_eq!(status, 0);
    assert_eq!(
        output,
        "It dtor\n#0 test.php(13): It->__destruct()\n#1 test.php(17): g()\n#2 {main}\ncaught\n"
    );
}

#[test]
fn uncaught_destructor_exception_reports_the_chain() {
    let (status, output) = run(r#"<?php
class D {
    public function __destruct() { throw new Exception("second"); }
}
function f() {
    $d = new D;
    throw new Exception("first");
}
f();
"#);
    assert_eq!(status, 255);
    assert_eq!(
        output,
        "\nFatal error: Uncaught Exception: first in test.php:7\nStack trace:\n#0 test.php(9): f()\n#1 {main}\n\nNext Exception: second in test.php:3\nStack trace:\n#0 test.php(9): D->__destruct()\n#1 {main}\n  thrown in test.php on line 3\n"
    );
}

#[test]
fn output_handler_exception_at_shutdown_is_fatal_and_discards_the_buffer() {
    let (status, output) = run(r#"<?php
echo "No crash.\n";
ob_start(function ($buffer) {
    throw new Error("boom");
});
echo "hmm\n";
"#);
    assert_eq!(status, 255);
    assert_eq!(output, "No crash.\n");
}

/// Unwinding releases a frame's unfinished calls (arguments first) before
/// its live temporaries, as `cleanup_unfinished_calls` runs before
/// `cleanup_live_vars`: that decides which object handles are reused first.
#[test]
fn unwinding_releases_call_arguments_before_live_temporaries() {
    let (_, out) = run(r#"<?php
class Test {
    public function __construct(public $a, public $b) {}
}
$x = new stdClass; $y = new stdClass; $z = new stdClass;
unset($x, $y, $z);
function f() { return new Test(new stdClass, [] + 0); }
try { f(); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$a = new stdClass; $b = new stdClass; $c = new stdClass;
echo spl_object_id($a), spl_object_id($b), spl_object_id($c), "\n";
"#);
    assert_eq!(out, "Unsupported operand types: array + int\n324\n");
}

#[test]
fn catch_releases_the_try_body_temporaries_first() {
    // bug29368_3: `cleanup_live_vars` frees the left operand before the
    // catch block runs.
    let (status, output) = run(r#"<?php
class Foo { function __destruct() { echo "Foo dtor\n"; } }
class Bar { function __construct() { throw new Exception("ctor"); } }
try {
    new Foo() + new Bar();
} catch (Exception $e) {
    echo "caught\n";
}
"#);
    assert_eq!(status, 0);
    assert_eq!(output, "Foo dtor\ncaught\n");
}

#[test]
fn uncaught_exception_keeps_shutdown_destructors() {
    // bug36268: `zend_exception_error` reports with `E_DONT_BAIL`, so the
    // objects are still destructed at shutdown.
    let (status, output) = run(r#"<?php
class Foo { function __destruct() { echo "Ha!\n"; } }
$x = new Foo();
throw new Exception("boom");
"#);
    assert_eq!(status, 255);
    assert_eq!(
        output,
        "\nFatal error: Uncaught Exception: boom in test.php:4\nStack trace:\n#0 {main}\n  thrown in test.php on line 4\nHa!\n"
    );
}

#[test]
fn uncaught_parse_error_keeps_shutdown_destructors() {
    // `zend_exception_error`: E_PARSE | E_DONT_BAIL.
    let (status, output) = run("<?php\nclass Foo { function __destruct() { echo \"Ha!\\n\"; } }\n$x = new Foo();\neval(\"1 +\");\n");
    assert_eq!(status, 255);
    assert_eq!(
        output,
        "\nParse error: syntax error, unexpected end of file in test.php(4) : eval()'d code on line 1\nHa!\n"
    );
}

#[test]
fn shutdown_destructors_reach_objects_their_destructors_create() {
    // bug51822 / bug74053: `zend_objects_store_call_destructors` walks the
    // store up to its current top, without reusing handles meanwhile.
    let (status, output) = run(r#"<?php
class B { function __destruct() { echo "B dtor\n"; } }
class A {
    static $keep;
    static $left = 3;
    function __destruct() {
        echo "A dtor\n";
        if (self::$left-- > 0) { self::$keep[] = new A; }
        self::$keep[] = new B;
    }
}
A::$keep = [new A];
echo "end\n";
"#);
    assert_eq!(status, 0);
    assert_eq!(
        output,
        "end\nA dtor\nA dtor\nB dtor\nA dtor\nB dtor\nA dtor\nB dtor\nB dtor\n"
    );
}

/// `zend_compile_goto` + `zend_resolve_goto_label`: a goto frees the loops
/// and calls the finally blocks it leaves, and only those.
#[test]
fn goto_frees_left_loops_and_calls_left_finally_blocks() {
    let (status, output) = run(r#"<?php
class A {
    function __construct(public $n) {}
    function __destruct() { echo "destruct{$this->n}\n"; }
}
function f() {
    foreach ([new A(1)] as $a) {
        $a = null;
        try {
            foreach ([new A(2)] as $a) {
                $a = null;
                goto out;
            }
        } finally {
            echo "finally\n";
        }
out:
        echo "out\n";
    }
    try {
        goto end;
    } finally {
        echo "finally2\n";
    }
end:
    echo "end\n";
}
f();
"#);
    assert_eq!(status, 0);
    assert_eq!(
        output,
        "destruct2\nfinally\nout\ndestruct1\nfinally2\nend\n"
    );
}

/// `zend_handle_loops_and_finally_ex`: `break 2` calls the finally block
/// before freeing the loops, and a destructor exception from the freed
/// iterator is thrown from the loop (`ZEND_FREE_ON_RETURN`), outside the
/// try the `break` is in.
#[test]
fn break_calls_finally_before_freeing_loops_outside_the_try() {
    let (status, output) = run(r#"<?php
class T {
    function __destruct() { throw new Exception("dtor"); }
}
function f() {
    foreach ([0] as $_) {
        foreach ([new T] as $v) {
            $v = null;
            try {
                break 2;
            } catch (Exception $e) {
                echo "inner catch\n";
            } finally {
                echo "finally\n";
            }
        }
    }
}
try {
    f();
} catch (Exception $e) {
    echo "outer catch: ", $e->getMessage(), "\n";
}
"#);
    assert_eq!(status, 0);
    assert_eq!(output, "finally\nouter catch: dtor\n");
}

/// `zend_check_finally_breakout`: a `break` in a finally block may not
/// leave it.
#[test]
fn break_out_of_a_finally_block_is_a_compile_error() {
    let (_, output) = run(r"<?php
function f() {
    while (true) {
        try {
        } finally {
            break;
        }
    }
}
");
    assert!(
        output.contains(
            "Fatal error: jump out of a finally block is disallowed in test.php on line 6"
        ),
        "{output}"
    );
}

/// `cleanup_live_vars(throw_op, catch_op)`: an exception caught in the
/// frame it was thrown in releases the iterators of the loops it leaves,
/// outermost first, before the catch block runs.
#[test]
fn caught_exception_releases_the_iterators_of_the_loops_it_leaves() {
    let (status, output) = run(r#"<?php
class A {
    function __construct(public $n) {}
    function __destruct() { echo "destruct{$this->n}\n"; }
}
function f() {
    try {
        foreach ([new A(1)] as $a) {
            $a = null;
            foreach ([new A(2)] as $a) {
                $a = null;
                foreach ([new A(3)] as $a) {
                    $a = null;
                    throw new Exception("x");
                }
            }
        }
    } catch (Exception $e) {
        echo "catch\n";
    }
    echo "after\n";
}
f();
"#);
    assert_eq!(status, 0);
    assert_eq!(output, "destruct1\ndestruct2\ndestruct3\ncatch\nafter\n");
}

/// `ZEND_LIVE_NEW` + `zend_object_store_ctor_failed`: an object whose
/// constructor call is abandoned while its arguments are evaluated is
/// never destructed.
#[test]
fn object_of_an_abandoned_constructor_call_is_not_destructed() {
    let (status, output) = run(r#"<?php
class D {
    function __construct() { echo "ctor\n"; }
    function __destruct() { echo "dtor\n"; }
}
function fail() { throw new Exception("arg"); }
try {
    $d = new D(fail());
} catch (Exception $e) {
    echo "caught ", $e->getMessage(), "\n";
}
echo "end\n";
"#);
    assert_eq!(status, 0);
    assert_eq!(output, "caught arg\nend\n");
}

/// Operands freed by a throwing op are destructed before the handler in
/// the same frame runs; their exceptions chain to the one in flight.
#[test]
fn destructor_exceptions_of_freed_operands_chain_before_the_catch() {
    let (status, output) = run(r#"<?php
class T { function __destruct() { throw new Exception("dtor"); } }
try {
    new T + new T;
} catch (Exception $e) {
    for ($x = $e; $x; $x = $x->getPrevious()) {
        echo $x::class, ": ", $x->getMessage(), "\n";
    }
}
echo "end\n";
"#);
    assert_eq!(status, 0);
    assert_eq!(
        output,
        "Exception: dtor\nException: dtor\nTypeError: Unsupported operand types: T + T\nend\n"
    );
}

/// `zend_generator_dtor_storage`: destroying a generator suspended in a
/// finally block whose exception is pending runs the inner finally and
/// rethrows the pending exception (`zend_dispatch_try_catch_finally_helper`
/// adopts the backed-up exception when none is in flight).
#[test]
fn destroyed_generator_rethrows_the_exception_pending_in_its_finally() {
    let (status, output) = run(r#"<?php
function gen() {
    try {
        throw new Exception("1");
    } finally {
        try {
            throw new Exception("2");
        } finally {
            try {
                yield;
            } finally {
                echo "inner finally\n";
            }
        }
    }
}
try {
    gen()->rewind();
} catch (Exception $e) {
    echo $e->getMessage(), " <- ", $e->getPrevious()?->getMessage(), "\n";
}
echo "end\n";
"#);
    assert_eq!(status, 0);
    assert_eq!(output, "inner finally\n2 <- 1\nend\n");
}

#[test]
fn objects_whose_constructor_never_ran_are_not_destructed() {
    // zend_object_init_with_constructor_classes_without_constructor.phpt
    let (_, output) = run(r#"<?php
class NoCtor { public function __destruct() { echo "destructed NoCtor\n"; } }
class WithCtor {
    public function __construct($a) {}
    public function __destruct() { echo "destructed WithCtor\n"; }
}
function boom() { throw new Exception("arg"); }
try { $o = new NoCtor(unknown: 1); } catch (Error $e) { echo $e->getMessage(), "\n"; }
try { $o = new WithCtor(boom()); } catch (Exception $e) { echo $e->getMessage(), "\n"; }
echo "end\n";
"#);
    assert_eq!(output, "Unknown named parameter $unknown\narg\nend\n");
}

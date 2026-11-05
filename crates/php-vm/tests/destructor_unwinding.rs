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

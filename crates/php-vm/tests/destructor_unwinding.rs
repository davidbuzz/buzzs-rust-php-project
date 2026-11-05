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

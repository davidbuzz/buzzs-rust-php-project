//! Script-level tests of the Throwable machinery (expected output from
//! `/usr/bin/php -n`, paths normalised to `t.php`).

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
fn previous_chain_never_cycles() {
    let src = r#"<?php
try {
    $e = new Exception("Foo");
    try {
        throw new Exception("Bar", 0, $e);
    } finally {
        throw $e;
    }
} catch (Exception $e) {
    echo $e, "\n";
}
"#;
    assert_eq!(
        run(src),
        r#"Exception: Foo in t.php:3
Stack trace:
#0 {main}
"#
    );
}

#[test]
fn magic_call_frames_show_themselves() {
    let src = r#"<?php
class M {
    function __call($n, $a) { echo (new Exception)->getTraceAsString(), "\n"; }
    static function __callStatic($n, $a) { echo (new Exception)->getTraceAsString(), "\n"; }
}
function f() { (new M)->foo(1, "x"); M::bar(2); }
f();
"#;
    assert_eq!(
        run(src),
        r#"#0 t.php(6): M->__call('foo', Array)
#1 t.php(7): f()
#2 {main}
#0 t.php(6): M::__callStatic('bar', Array)
#1 t.php(7): f()
#2 {main}
"#
    );
}

#[test]
fn top_level_handler_is_internal_frame() {
    let src = r#"<?php
function h($e) { echo (new Exception)->getTraceAsString(), "\n"; }
set_exception_handler('h');
throw new Exception("x");
"#;
    assert_eq!(
        run(src),
        r#"#0 [internal function]: h(Object(Exception))
#1 {main}
"#
    );
}

#[test]
fn error_exception_properties() {
    let src = r#"<?php
$e = new ErrorException("m", 3, E_WARNING, "some.php");
var_dump($e->getSeverity(), $e->getFile(), $e->getLine(), $e->getCode());
$e = new ErrorException("m", 0, E_NOTICE, null, 7, new Exception("p"));
var_dump($e->getLine(), $e->getPrevious()->getMessage());
$e = new Exception("msg", 0);
var_dump($e->getCode());
echo get_class(new TypeError), "\n";
"#;
    assert_eq!(
        run(src),
        r#"int(2)
string(8) "some.php"
int(0)
int(3)
int(7)
string(1) "p"
int(0)
TypeError
"#
    );
}

#[test]
fn sensitive_params_wrapped_in_traces() {
    let src = r#"<?php
function s($a, #[\SensitiveParameter] $secret, ...$rest) { throw new Exception("s"); }
try { s(1, "pw", 'aaaaaaaaaaaaaaaaaaaaaa', "x\n"); } catch (Exception $e) { echo $e->getTraceAsString(), "\n"; }
function fga($a, #[\SensitiveParameter] $b) { var_dump(func_get_args()); }
fga(1, 2);
"#;
    assert_eq!(
        run(src),
        r#"#0 t.php(3): s(1, Object(SensitiveParameterValue), 'aaaaaaaaaaaaaaa...', 'x\n')
#1 {main}
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
"#
    );
}

//! Callables kept or called by the engine (`Fiber`, user error and
//! exception handlers, output handlers): where php-src resolves them
//! (`zend_fcall_info_init` at registration, or `zend_call_function`
//! without a cache at each call), with the callable deprecations raised
//! there and "Invalid callback" for what no longer resolves.

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

/// Runs `source` as `t.php` with run-tests.php's `fatal_error_backtraces=0`.
fn run(source: &str) -> (i32, String) {
    let sink = Sink::default();
    let options = VmOptions {
        ini: vec![("fatal_error_backtraces".to_string(), "0".to_string())],
    };
    let mut vm = Vm::new(&options, Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    (status, String::from_utf8(output).expect("UTF-8 output"))
}

/// `Fiber::__construct()` takes its callable with `Z_PARAM_FUNC`: resolved
/// once in the constructing scope (deprecations there, before the "twice"
/// check), called from anywhere as a `ZEND_CALL_DYNAMIC` call.
#[test]
fn fiber_resolves_its_callable_at_construction() {
    let (status, out) = run(r#"<?php
class foo {
    public function fb() { echo __METHOD__, "\n"; }
}
class bar extends foo {
    function make() { return [new Fiber('parent::fb'), new Fiber([$this, 'parent::fb'])]; }
    function again(Fiber $f) { $f->__construct('parent::fb'); }
}
class baz {
    private function p() { echo __METHOD__, "\n"; }
    function make() { return new Fiber([$this, 'p']); }
}
$b = new bar;
[$f1, $f2] = $b->make();
$f1->start();
$f2->start();
(new baz)->make()->start();
try { $b->again($f1); } catch (FiberError $e) { echo $e->getMessage(), "\n"; }
try { new Fiber('parent::fb'); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$f = new Fiber('func_num_args');
try { $f->start(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#);
    assert_eq!(status, 0, "{out}");
    assert_eq!(
        out,
        "\nDeprecated: Use of \"parent\" in callables is deprecated in t.php on line 6\n\
         \nDeprecated: Callables of the form [\"bar\", \"parent::fb\"] are deprecated in t.php on line 6\n\
         foo::fb\nfoo::fb\nbaz::p\n\
         \nDeprecated: Use of \"parent\" in callables is deprecated in t.php on line 7\n\
         Cannot call constructor twice\n\
         Fiber::__construct(): Argument #1 ($callback) must be a valid callback, cannot access \"parent\" when no class scope is active\n\
         Cannot call func_num_args() dynamically\n"
    );
}

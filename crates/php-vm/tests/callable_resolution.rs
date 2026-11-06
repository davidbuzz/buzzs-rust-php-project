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

/// Drops the deprecations reported at `lines` (registrations: whether
/// `set_error_handler()`/`set_exception_handler()` raise the callable
/// deprecations when they validate is not what these tests check).
fn without_deprecations_at(out: &str, lines: &[u32]) -> String {
    let parts: Vec<&str> = out.split_inclusive('\n').collect();
    let mut kept = String::new();
    let mut i = 0;
    while i < parts.len() {
        if parts[i] == "\n" {
            if let Some(next) = parts.get(i + 1) {
                let at_line = |l: &u32| next.ends_with(&format!(" in t.php on line {l}\n"));
                if next.starts_with("Deprecated: ") && lines.iter().any(at_line) {
                    i += 2;
                    continue;
                }
            }
        }
        kept.push_str(parts[i]);
        i += 1;
    }
    kept
}

/// `zend_error_impl` calls the user error handler with
/// `call_user_function()`: resolved at each call in the scope of the code
/// that raised the error, its deprecations raised there each time (with no
/// user handler current, so they are displayed), and "Invalid callback"
/// thrown where it no longer resolves.
#[test]
fn error_handler_is_resolved_where_each_error_is_raised() {
    let (status, out) = run(r#"<?php
class foo {
    public function eh($no, $str) { echo "eh: $str\n"; return true; }
}
class bar extends foo {
    function reg() { set_error_handler('parent::eh'); }
    function regp() { set_error_handler([$this, 'parent::eh']); }
    function trig() { trigger_error("in scope", E_USER_WARNING); }
}
class baz {
    private function peh($no, $str) { echo "peh: $str\n"; return true; }
    function reg() { set_error_handler([$this, 'peh']); }
    function trig() { trigger_error("in scope", E_USER_WARNING); }
}
function t($f) { try { $f(); echo "after\n"; } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; } }
$b = new bar;
$b->reg();
$b->trig();
t(fn() => trigger_error("outside", E_USER_WARNING));
restore_error_handler();
$b->regp();
trigger_error("arr outside", E_USER_WARNING);
$b->trig();
restore_error_handler();
$z = new baz;
$z->reg();
$z->trig();
t(fn() => $undef);
"#);
    assert_eq!(status, 0, "{out}");
    assert_eq!(
        without_deprecations_at(&out, &[6, 7]),
        "\nDeprecated: Use of \"parent\" in callables is deprecated in t.php on line 8\n\
         eh: in scope\n\
         Error: Invalid callback parent::eh, cannot access \"parent\" when no class scope is active\n\
         \nDeprecated: Callables of the form [\"bar\", \"parent::eh\"] are deprecated in t.php on line 22\n\
         eh: arr outside\n\
         \nDeprecated: Callables of the form [\"bar\", \"parent::eh\"] are deprecated in t.php on line 8\n\
         eh: in scope\n\
         peh: in scope\n\
         Error: Invalid callback baz::peh, cannot access private method baz::peh()\n"
    );
}

/// `zend_user_exception_handler` runs with no frame: `'parent::xh'` and a
/// private method no longer resolve, which `zend_throw_error()` turns into
/// an "Invalid callback" fatal error "in Unknown on line 0" (the shutdown
/// functions still run); an array callable resolves and deprecates there.
#[test]
fn exception_handler_is_resolved_without_a_frame() {
    let (status, out) = run(r#"<?php
class foo {
    public function xh($e) { echo "xh: ", $e->getMessage(), "\n"; }
    public function fb() { echo __METHOD__, "\n"; }
}
class bar extends foo {
    function reg() {
        set_exception_handler('parent::xh');
        register_shutdown_function([$this, 'parent::fb']);
    }
}
(new bar)->reg();
echo "main\n";
throw new Exception("final");
"#);
    assert_eq!(status, 255, "{out}");
    assert_eq!(
        without_deprecations_at(&out, &[8]),
        "\nDeprecated: Callables of the form [\"bar\", \"parent::fb\"] are deprecated in t.php on line 9\n\
         main\n\
         \nFatal error: Invalid callback parent::xh, cannot access \"parent\" when no class scope is active in Unknown on line 0\n\
         foo::fb\n"
    );

    let (status, out) = run(r#"<?php
class foo {
    public function xh($e) { echo "xh: ", $e->getMessage(), "\n"; }
}
class bar extends foo {
    function reg() { set_exception_handler([$this, 'parent::xh']); }
}
(new bar)->reg();
echo "main\n";
throw new Exception("final");
"#);
    assert_eq!(status, 0, "{out}");
    assert_eq!(
        without_deprecations_at(&out, &[6]),
        "main\n\
         \nDeprecated: Callables of the form [\"bar\", \"parent::xh\"] are deprecated in Unknown on line 0\n\
         xh: final\n"
    );

    let (status, out) = run(r#"<?php
class foo {
    private function xh($e) { echo "xh: ", $e->getMessage(), "\n"; }
    function reg() { set_exception_handler([$this, 'xh']); }
}
(new foo)->reg();
register_shutdown_function(function () { echo "shutdown\n"; });
echo "main\n";
throw new Exception("final");
"#);
    assert_eq!(status, 255, "{out}");
    assert_eq!(
        out,
        "main\n\
         \nFatal error: Invalid callback foo::xh, cannot access private method foo::xh() in Unknown on line 0\n\
         shutdown\n"
    );
}

#[test]
fn deprecations_at_given_lines_are_dropped() {
    let out = "a\n\nDeprecated: x in t.php on line 6\nb\n\nDeprecated: y in t.php on line 7\n";
    assert_eq!(
        without_deprecations_at(out, &[6]),
        "a\nb\n\nDeprecated: y in t.php on line 7\n"
    );
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

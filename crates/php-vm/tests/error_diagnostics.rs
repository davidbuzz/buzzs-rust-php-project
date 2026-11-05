//! The error/warning system: `@` and `error_reporting` across exceptions,
//! error handler stacking, and diagnostics whose order or form an error
//! handler can observe (`Zend/zend.c` `zend_error_impl`,
//! `Zend/zend_vm_def.h`, `Zend/zend_builtin_functions.c`).

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

fn run(source: &str) -> String {
    let sink = Sink::default();
    let options = VmOptions {
        ini: [("display_errors".to_string(), "1".to_string())]
            .into_iter()
            .collect(),
    };
    let mut vm = Vm::new(&options, Box::new(sink.clone()));
    let _status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8(out).expect("UTF-8 output")
}

/// `ZEND_END_SILENCE` and the `ZEND_LIVE_SILENCE` cleanup on unwinding
/// restore the saved mask only while the current one is still silenced
/// (bug #33771, `error_reporting02/03/08`).
#[test]
fn silence_restore_keeps_masks_set_inside() {
    let out = run(r#"<?php
error_reporting(E_ALL & ~E_DEPRECATED);
function bar() { error_reporting(E_ALL); throw new Exception("x"); }
function baz() { error_reporting(E_ALL & ~E_NOTICE); throw new Exception("x"); }
function qux() { throw new Exception("x"); }
try { @qux(); } catch (Exception $e) {}
echo error_reporting(), "\n";
try { @bar(); } catch (Exception $e) {}
echo error_reporting(), "\n";
try { @baz(); } catch (Exception $e) {}
echo error_reporting(), "\n";
error_reporting(E_ALL);
echo @$undef, error_reporting(), "\n";
error_reporting(0);
echo @$undef, error_reporting(), "\n";
"#);
    assert_eq!(out, "22527\n30719\n30711\n30719\n0\n");
}

/// `ZEND_ASSIGN_DIM` reads the offset before `OP_DATA`; `ZEND_ASSIGN_OBJ`
/// reads `OP_DATA` first (bug #79599).
#[test]
fn assign_dim_reads_offset_before_undefined_value() {
    let out = run(r#"<?php
set_error_handler(function ($no, $msg) { throw new Exception($msg); });
function t1() { $a[] = $b; }
function t2() { $a[$c] = $b; }
function t3() { $a[1][$d] = $b; }
function t4() { $o = new stdClass; $o->{$p} = $b; }
foreach (['t1', 't2', 't3', 't4'] as $f) {
    try { $f(); } catch (Exception $e) { echo $e->getMessage(), "\n"; }
}
"#);
    assert_eq!(
        out,
        "Undefined variable $b\nUndefined variable $c\nUndefined variable $d\nUndefined variable $b\n"
    );
}

/// The running handler is unset while it runs, so a handler it installs
/// receives its errors; `get_error_handler()` (bug #63206).
#[test]
fn error_handler_stacking_inside_handler() {
    let out = run(r#"<?php
set_error_handler(function () { echo "first\n"; });
set_error_handler(function () {
    echo "second\n";
    set_error_handler(function () { echo "inner\n"; });
    $u1++;
    restore_error_handler();
});
$u2++;
$u3++;
var_dump(get_error_handler() !== null);
restore_error_handler();
restore_error_handler();
var_dump(get_error_handler());
set_error_handler(function () { var_dump(get_error_handler()); });
$u4++;
"#);
    assert_eq!(
        out,
        "second\ninner\nsecond\ninner\nbool(true)\nNULL\nNULL\n"
    );
}

/// A direct `call_user_func('self::m')` whose deprecation the error handler
/// turns into an exception throws the callback `TypeError` chained to it
/// (GH-21699).
#[test]
fn call_user_func_keyword_deprecation_throw_chains_type_error() {
    let out = run(r#"<?php
set_error_handler(function () { throw new Exception("from handler"); });
class Bar {
    public static function __callStatic($n, $a) {}
    public function test() { call_user_func('static::y'); }
}
try {
    (new Bar)->test();
} catch (TypeError $e) {
    echo $e->getMessage(), "\n", get_class($e->getPrevious()), ": ", $e->getPrevious()->getMessage(), "\n";
}
"#);
    assert_eq!(
        out,
        "call_user_func(): Argument #1 ($callback) must be a valid callback, cannot use \"self\"/\"parent\"/\"static\" in callables\nException: from handler\n"
    );
}

/// `get_object_vars()` copies a property reference nothing else holds
/// (bug #71336).
#[test]
fn get_object_vars_unwraps_singly_held_references() {
    let out = run(r"<?php
class C {
    public $bar = [1];
    function t() { f($this->bar); $v = get_object_vars($this); $this->bar[] = 2; var_dump($v['bar']); }
    function u() { f($this->bar); $r = &$this->bar; $v = get_object_vars($this); $this->bar[] = 2; var_dump($v['bar']); }
}
function f(&$x) {}
(new C)->t();
(new C)->u();
");
    assert_eq!(
        out,
        "array(1) {\n  [0]=>\n  int(1)\n}\narray(2) {\n  [0]=>\n  int(1)\n  [1]=>\n  int(2)\n}\n"
    );
}

/// A compound assignment works on a property the "Undefined property"
/// handler defined (OSS-Fuzz #61712).
#[test]
fn assign_op_sees_property_defined_by_error_handler() {
    let out = run(r#"<?php
#[AllowDynamicProperties]
class C {
    function error($no, $msg) { echo $msg, "\n"; $this->a = 12345; }
}
$c = new C;
set_error_handler([$c, 'error']);
$c->a %= 10;
var_dump($c->a);
"#);
    assert_eq!(out, "Undefined property: C::$a\nint(5)\n");
}

/// `zend_check_string_offset` warns before the string-offset reference
/// error, and an exception from the handler replaces it (bug #76534).
#[test]
fn string_offset_reference_checks_offset_first() {
    let out = run(r#"<?php
$x = "foo";
set_error_handler(function ($no, $msg) { throw new Exception($msg); });
try { $y = &$x["2bar"]; } catch (Exception $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
set_error_handler(function ($no, $msg) { echo $msg, "\n"; });
try { $y = &$x["1x"]; } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        "Exception: Illegal string offset \"2bar\"\nIllegal string offset \"1x\"\nError: Cannot create references to/from string offsets\n"
    );
}

/// Builtins compiled to an opcode cannot be written to or referenced
/// (`zend_separate_if_call_and_write`); namespaced unqualified calls are
/// resolved at runtime and only notice.
#[test]
fn builtin_opcode_result_in_write_context_is_a_compile_error() {
    for src in [
        "<?php echo 1; strlen(\"foo\")[0] = 1;",
        "<?php echo 1; $r = &strlen(\"foo\");",
        "<?php echo 1; \\is_int(1)->p = 1;",
    ] {
        let out = run(src);
        assert!(
            out.contains("Fatal error: Cannot use result of built-in function in write context in t.php on line 1"),
            "{src}: {out}"
        );
        assert!(!out.starts_with('1'), "{src}: compile-time, got {out}");
    }
    let out = run("<?php namespace A; $r = &strlen(\"foo\"); echo \"ok\";");
    assert!(
        out.contains("Notice: Only variables should be assigned by reference"),
        "{out}"
    );
    assert!(out.ends_with("ok"), "{out}");
}

/// `zend_try_compile_cv` / `zend_compile_simple_var_no_cv`
/// (`Zend/zend_compile.c`): reading `$http_response_header` is deprecated at
/// compile time unless the op array assigned it first or took it as a
/// parameter; a constant variable-variable name always warns.
#[test]
fn http_response_header_read_is_deprecated_per_op_array() {
    let out = run(r#"<?php
function a() { echo $http_response_header; }
function b($http_response_header) { echo $http_response_header; }
function c() { $http_response_header = 1; echo $http_response_header; function d() { echo $http_response_header; } }
function e() { ${'http_response_header'} = 1; var_dump(${'http_response_header'}); }
echo "done\n";
"#);
    let msg = "Deprecated: The predefined locally scoped $http_response_header variable is deprecated, call http_get_last_response_headers() instead in t.php on line";
    assert_eq!(out, format!("\n{msg} 2\n\n{msg} 4\n\n{msg} 5\ndone\n"));
}

/// `SPEC(COMMUTATIVE)` opcodes (`ZEND_MUL`, `ZEND_BW_OR`/`AND`/`XOR`) get
/// their operands swapped when op1's type sorts before op2's (`IS_CONST` <
/// `IS_TMP_VAR` < `IS_CV`), so the error names them in swapped order
/// (`operator_unsupported_types.phpt`; expectations from php-src master).
#[test]
fn commutative_operands_are_reported_in_canonical_order() {
    let out = run(r#"<?php
function t($f) { try { $f(); } catch (TypeError $e) { echo $e->getMessage(), "\n"; } }
t(fn() => [] * new stdClass);
t(fn() => 1 & new stdClass);
t(function () { $o = new stdClass; return [] ^ $o; });
t(function () { $o = new stdClass; return $o * []; });
t(function () { $a = []; return new stdClass * $a; });
t(fn() => [] + new stdClass);
"#);
    assert_eq!(
        out,
        "Unsupported operand types: stdClass * array\n\
         Unsupported operand types: stdClass & int\n\
         Unsupported operand types: stdClass ^ array\n\
         Unsupported operand types: stdClass * array\n\
         Unsupported operand types: array * stdClass\n\
         Unsupported operand types: array + stdClass\n"
    );
}

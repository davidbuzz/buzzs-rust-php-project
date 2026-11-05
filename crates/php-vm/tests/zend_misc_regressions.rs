//! Regressions from the top-level `Zend/tests` files (`bug*.phpt` and
//! friends) that do not belong to a feature directory.
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

/// Runs `source` and returns its output.
fn run(source: &str) -> String {
    run_status(source).1
}

/// `ZEND_FE_FETCH_RW` warns when the loop body replaced the iterated
/// variable with a non-array (`bug79792`).
#[test]
fn by_ref_foreach_over_variable_overwritten_with_null_warns() {
    let out = run("<?php\n$a = [42];\nforeach ($a as &$c) {\n    unset($a[0]);\n    $a = null;\n}\necho \"done\";\n");
    assert_eq!(
        out,
        "\nWarning: foreach() argument must be of type array|object, null given in t.php on line 3\ndone"
    );
}

/// `register_shutdown_function()` resolves `"self"` in the registering
/// scope (`bug41026`).
#[test]
fn shutdown_function_self_callable_is_resolved_at_registration() {
    let out = run(r#"<?php
class C {
    static function main() { register_shutdown_function(["self", "down"]); }
    private static function down() { echo "down\n"; }
}
C::main();
echo "Done\n";
"#);
    assert_eq!(
        out,
        "\nDeprecated: Use of \"self\" in callables is deprecated in t.php on line 3\nDone\ndown\n"
    );
}

/// Shutdown functions run through `zend_call_function()`, a dynamic call
/// (`bug71221`).
#[test]
fn shutdown_function_get_defined_vars_is_a_dynamic_call() {
    let out = run("<?php\nregister_shutdown_function(\"get_defined_vars\");\n");
    assert!(
        out.contains("Uncaught Error: Cannot call get_defined_vars() dynamically"),
        "{out}"
    );
}

/// The `Closure::__invoke` trampoline of an internal function's closure
/// reports a required parameter skipped by named arguments under its own
/// name (`call_function_trampoline_leak`).
#[test]
fn closure_invoke_trampoline_reports_skipped_required_parameter() {
    let out = run(r#"<?php
$call = 'call_user_func_array';
try {
    $call([Closure::fromCallable('strcmp'), '__invoke'], ['string2' => 'b']);
} catch (Throwable $e) {
    echo $e::class, ': ', $e->getMessage(), "\n";
}
var_dump($call([Closure::fromCallable('strcmp'), '__invoke'], ['string2' => 'b', 'string1' => 'b']));
"#);
    assert_eq!(
        out,
        "ArgumentCountError: Closure::__invoke(): Argument #1 ($string1) not passed\nint(0)\n"
    );
}

/// A call compiled into a builtin instruction is a temporary: passing it
/// by reference is an error, not the "Only variables" notice of a real
/// call result (`bug79783`).
#[test]
fn special_compiled_call_result_cannot_be_passed_by_reference() {
    let out = run(r#"<?php
function f() { return 1; }
function g(&$r) {}
g(f());
echo "next\n";
try { g(strlen("d")); } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        "\nNotice: Only variables should be passed by reference in t.php on line 4\nnext\ng(): Argument #1 ($r) could not be passed by reference\n"
    );
}

/// `ZEND_ASSIGN_DIM_OP` / `ZEND_FETCH_DIM_RW` fetch an undefined container
/// before reading the offset (`assign_dim_op_undef`).
#[test]
fn compound_dim_assignment_warns_for_undefined_container_first() {
    let out = run("<?php\n$a[$b] += 1;\n$c[$d]++;\n");
    let order: Vec<&str> = out
        .lines()
        .filter_map(|l| l.strip_prefix("Warning: Undefined variable "))
        .map(|l| l.split(' ').next().unwrap_or(""))
        .collect();
    assert_eq!(order, ["$a", "$b", "$c", "$d"], "{out}");
}

/// `FETCH_DIM_W` of the target runs before `ZEND_ASSIGN_REF` raises "Only
/// variables should be assigned by reference" (`assign_ref_error_var_handling`).
#[test]
fn by_ref_assignment_target_error_precedes_only_variables_notice() {
    let out = run(r#"<?php
function val() { return 42; }
$arr = [PHP_INT_MAX => "foo"];
try {
    var_dump($arr[] =& val());
} catch (Throwable $e) {
    echo $e::class, ': ', $e->getMessage(), "\n";
}
$b = [];
var_dump($b['k'] =& val());
var_dump($b);
"#);
    assert_eq!(
        out,
        "Error: Cannot add element to the array as the next element is already occupied\n\
         \nNotice: Only variables should be assigned by reference in t.php on line 10\n\
         int(42)\narray(1) {\n  [\"k\"]=>\n  int(42)\n}\n"
    );
}

/// A call to a function declared earlier in the file compiles a by-value
/// `$a[]` argument for reading: a compile error (`array_append_reading_error`).
/// By-reference parameters and functions not yet declared are unaffected.
#[test]
fn append_passed_by_value_to_known_function_is_a_compile_error() {
    let (status, out) = run_status("<?php\n\nfunction test($var) { }\ntest($arr[]);\n");
    assert_ne!(status, 0, "{out}");
    assert!(
        out.contains("Fatal error: Cannot use [] for reading in t.php on line 4"),
        "{out}"
    );
    let out = run("<?php\nfunction r(&$x) {}\nr($a[]);\nvar_dump(count($a));\ntry { g($b[]); } catch (Error $e) { echo $e->getMessage(), \"\\n\"; }\nfunction g($v) {}\n");
    assert_eq!(out, "int(1)\nCannot use [] for reading\n");
}

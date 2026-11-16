// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Compile-time substitution of internal constants
//! (`zend_try_ct_eval_const`, `Zend/zend_compile.c`).
//!
//! php-src replaces a persistent internal constant such as `PHP_INT_MAX`
//! by its value while compiling, so it is an `IS_CONST` operand: the
//! commutative-operand swap of `zend_vm_set_opcode_handler` does not move
//! it in front of another constant
//! (`Zend/tests/runtime_compile_time_binary_operands.phpt`).
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
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

#[test]
fn substituted_constants_keep_their_operand_position() {
    let out = run(r#"<?php
function m($f) { try { $f(); } catch (Error $e) { echo $e->getMessage(), "\n"; } }
m(fn() => [] * PHP_INT_MAX);
m(fn() => [] | INF);
m(fn() => [] ^ \PHP_INT_MIN);
m(fn() => [] & NAN);
"#);
    assert_eq!(
        out,
        "Unsupported operand types: array * int\n\
         Unsupported operand types: array | float\n\
         Unsupported operand types: array ^ int\n\
         Unsupported operand types: array & float\n"
    );
}

/// An unqualified name in a namespace resolves to the namespaced name,
/// which is not an internal constant: it is fetched at runtime (with the
/// global fallback), so the fetched operand is swapped as before.
#[test]
fn unqualified_constant_in_namespace_is_not_substituted() {
    let out = run(r#"<?php
namespace N;
function m($f) { try { $f(); } catch (\Error $e) { echo $e->getMessage(), "\n"; } }
m(fn() => [] * PHP_INT_MAX);
m(fn() => [] * \PHP_INT_MAX);
"#);
    assert_eq!(
        out,
        "Unsupported operand types: int * array\n\
         Unsupported operand types: array * int\n"
    );
}

/// `zend_compile_params` compiles defaults without substitution, so
/// reflection still sees the constant and a mismatching type is no
/// compile error.
#[test]
fn parameter_defaults_are_not_substituted() {
    let out = run(r#"<?php
function f($a = PHP_INT_MAX, string $s = PHP_INT_MAX) { return $s; }
$p = (new ReflectionFunction('f'))->getParameters();
var_dump($p[0]->isDefaultValueConstant(), $p[0]->getDefaultValueConstantName(), $p[1]->getDefaultValueConstantName());
var_dump(f());
"#);
    assert_eq!(
        out,
        "bool(true)\nstring(11) \"PHP_INT_MAX\"\nstring(11) \"PHP_INT_MAX\"\nstring(19) \"9223372036854775807\"\n"
    );
}

/// Deprecated constants are not substituted (`CONST_DEPRECATED`): reading
/// one still raises the deprecation at runtime.
#[test]
fn deprecated_constants_are_not_substituted() {
    let out = run("<?php\necho E_STRICT, \"\\n\";\n");
    assert!(
        out.contains("Constant E_STRICT is deprecated") && out.ends_with("2048\n"),
        "{out}"
    );
}

/// Substituted constants fold with literals (`PHP_INT_MAX + 1` is a float
/// literal) and user constants are still fetched at runtime.
#[test]
fn substituted_constants_fold_and_user_constants_still_resolve() {
    let out = run(r#"<?php
define('MY_C', 5);
var_dump(PHP_INT_MAX + 1, MY_C * 2, strlen(PHP_EOL));
"#);
    assert_eq!(out, "float(9.223372036854776E+18)\nint(10)\nint(1)\n");
}

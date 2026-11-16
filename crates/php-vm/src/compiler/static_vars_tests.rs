// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The static variable table (`op_array->static_variables`) and the
//! optimizer's removal of unused constant-initialized statics
//! (`Zend/Optimizer/dce.c`, opcache `optimize_static_002`).

use super::optimizer::DEFAULT_OPTIMIZATION_LEVEL;
use super::{compile, CompileOptions, CompiledUnit};
use crate::opcode::{Op, OpArray};
use php_runtime::Value;
use std::rc::Rc;

fn compile_src(src: &str, level: u32) -> CompiledUnit {
    let program =
        php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
    let opts = CompileOptions {
        filename: "t.php".into(),
        optimization_level: level,
        ..CompileOptions::default()
    };
    compile(&program, Rc::from(src.as_bytes()), opts).expect("compile")
}

fn function_code(unit: &CompiledUnit, name: &[u8]) -> Rc<OpArray> {
    unit.decls
        .functions
        .borrow()
        .iter()
        .find(|f| f.name.as_bytes() == name)
        .map(|f| f.code.clone())
        .expect("function")
}

/// (name, constant initial value) of each declared static, in slot order.
fn table(code: &OpArray) -> Vec<(String, Option<Value>)> {
    code.static_vars
        .iter()
        .map(|d| {
            (
                String::from_utf8_lossy(d.name.as_bytes()).into_owned(),
                d.init.clone(),
            )
        })
        .collect()
}

fn binds(code: &OpArray) -> usize {
    code.ops
        .iter()
        .filter(|op| matches!(op, Op::BindStatic { .. } | Op::InitStatic { .. }))
        .count()
}

fn is_int(v: Option<&Value>, n: i64) -> bool {
    matches!(v, Some(Value::Int(i)) if *i == n)
}

#[test]
fn table_records_statics_in_declaration_order() {
    let unit = compile_src(
        "<?php function foo() { static $a = 42, $b; static $c = bar(); return [$a, $b, $c]; }",
        0,
    );
    let code = function_code(&unit, b"foo");
    let t = table(&code);
    assert_eq!(
        t.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    assert!(is_int(t[0].1.as_ref(), 42));
    assert!(matches!(t[1].1, Some(Value::Null)));
    assert!(t[2].1.is_none(), "a call is not a constant initializer");
    assert_eq!(code.num_statics, 3);
    assert_eq!(binds(&code), 6);
}

#[test]
fn unused_constant_static_loses_its_bind_but_keeps_its_table_entry() {
    let unit = compile_src(
        "<?php function foo() { static $a = 42; var_dump((new ReflectionFunction(__FUNCTION__))->getStaticVariables()); }",
        DEFAULT_OPTIMIZATION_LEVEL,
    );
    let code = function_code(&unit, b"foo");
    assert_eq!(binds(&code), 0, "{:?}", code.ops);
    let t = table(&code);
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].0, "a");
    assert!(is_int(t[0].1.as_ref(), 42));
}

#[test]
fn used_or_computed_statics_keep_their_binds() {
    let unit = compile_src(
        "<?php function used() { static $n = 0; return ++$n; }
        function computed() { static $a = bar(); }
        function dynamic() { static $a = 1; return get_defined_vars(); }
        function captured() { static $a = 1; return function () use ($a) { return $a; }; }
        function guarded() { try { static $a = 1; } finally { echo 1; } }",
        DEFAULT_OPTIMIZATION_LEVEL,
    );
    for name in [
        &b"used"[..],
        b"computed",
        b"dynamic",
        b"captured",
        b"guarded",
    ] {
        let code = function_code(&unit, name);
        assert_eq!(
            binds(&code),
            2,
            "{}: {:?}",
            String::from_utf8_lossy(name),
            code.ops
        );
        assert_eq!(code.static_vars.len(), 1);
    }
}

#[test]
fn without_optimizer_every_bind_stays() {
    let unit = compile_src("<?php function foo() { static $a = 42; }", 0);
    let code = function_code(&unit, b"foo");
    assert_eq!(binds(&code), 2);
}

#[test]
fn top_level_statics_are_never_removed() {
    let unit = compile_src("<?php static $a = 42;", DEFAULT_OPTIMIZATION_LEVEL);
    assert_eq!(binds(&unit.main.code), 2);
    assert_eq!(unit.main.code.static_vars.len(), 1);
}

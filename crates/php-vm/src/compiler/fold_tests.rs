//! Compile-time evaluation in expression compilation
//! (`zend_try_ct_eval_binary_op()`, `zend_try_ct_eval_const()`) and
//! `zend_set_user_opcode_handler()` hooks.

use super::{compile, CompileOptions, CompiledUnit, CtConstants};
use crate::opcode::{BinKind, Op};
use php_runtime::{BinaryOp, Value, ZStr};
use std::collections::HashMap;
use std::rc::Rc;

fn compile_src(src: &str, user_opcodes: Vec<&'static str>) -> CompiledUnit {
    let program =
        php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
    let opts = CompileOptions {
        filename: "t.php".into(),
        user_opcodes,
        // The VM's persistent internal constants (`Vm::ct_constants`);
        // `PHP_EOL` is the one these tests use.
        ct_constants: Some(CtConstants::new(HashMap::from([(
            ZStr::from("PHP_EOL"),
            Value::Str(ZStr::from("\n")),
        )]))),
        ..CompileOptions::default()
    };
    compile(&program, Rc::from(src.as_bytes()), opts).expect("compile")
}

/// The ops of the unit's first declared function.
fn first_function_ops(unit: &CompiledUnit) -> Vec<Op> {
    unit.decls.functions.borrow()[0].code.ops.clone()
}

#[test]
fn magic_constant_concat_folds_to_one_echo() {
    let unit = compile_src(
        "<?php function foo() { echo __FUNCTION__ . PHP_EOL; }",
        Vec::new(),
    );
    let ops = first_function_ops(&unit);
    let Some(Op::Echo(crate::opcode::Operand::Lit(i))) = ops.first() else {
        panic!("first op is not an echo of a literal: {ops:?}");
    };
    let f = &unit.decls.functions.borrow()[0];
    assert!(matches!(
        f.code.literals.get(*i as usize),
        Some(Value::Str(s)) if s.as_bytes() == b"foo\n"
    ));
}

#[test]
fn namespaced_php_eol_is_not_substituted() {
    let unit = compile_src(
        "<?php namespace N; function foo() { echo 'a' . PHP_EOL; }",
        Vec::new(),
    );
    let ops = first_function_ops(&unit);
    assert!(
        ops.iter().any(|op| matches!(op, Op::FetchConst { .. })),
        "{ops:?}"
    );
}

#[test]
fn diagnosing_literal_operands_do_not_fold() {
    let unit = compile_src("<?php function foo() { return __LINE__ % 0; }", Vec::new());
    let ops = first_function_ops(&unit);
    assert!(ops.iter().any(|op| matches!(op, Op::Bin { .. })), "{ops:?}");
}

#[test]
fn user_opcode_hook_precedes_each_add() {
    let src = "<?php function f($a, $b) { $r = $a + $b; return $r - 1 + $a; }";
    let ops = first_function_ops(&compile_src(src, vec!["ZEND_ADD"]));
    let adds: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, op)| {
            matches!(
                op,
                Op::Bin {
                    op: BinKind::Arith(BinaryOp::Add),
                    ..
                }
            )
        })
        .map(|(i, _)| i)
        .collect();
    assert_eq!(adds.len(), 2, "{ops:?}");
    for i in adds {
        assert!(
            i > 0 && matches!(ops[i - 1], Op::UserOpcode { .. }),
            "{ops:?}"
        );
    }
    let hooks = ops
        .iter()
        .filter(|op| matches!(op, Op::UserOpcode { .. }))
        .count();
    assert_eq!(hooks, 2);
    let plain = first_function_ops(&compile_src(src, Vec::new()));
    assert!(!plain.iter().any(|op| matches!(op, Op::UserOpcode { .. })));
}

#[test]
fn user_opcode_hooks_bracket_silence() {
    let src = "<?php function f() { return @g(); }";
    let ops = first_function_ops(&compile_src(
        src,
        vec!["ZEND_BEGIN_SILENCE", "ZEND_END_SILENCE"],
    ));
    for (i, op) in ops.iter().enumerate() {
        if matches!(op, Op::BeginSilence { .. } | Op::EndSilence { .. }) {
            assert!(
                i > 0 && matches!(ops[i - 1], Op::UserOpcode { .. }),
                "{ops:?}"
            );
        }
    }
    let hooks = ops
        .iter()
        .filter(|op| matches!(op, Op::UserOpcode { .. }))
        .count();
    assert_eq!(hooks, 2, "{ops:?}");
}

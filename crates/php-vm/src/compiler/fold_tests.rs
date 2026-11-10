//! Compile-time evaluation in expression compilation
//! (`zend_try_ct_eval_binary_op()`, `zend_try_ct_eval_const()`) and
//! `zend_set_user_opcode_handler()` hooks.

use super::{compile, CompileOptions, CompiledUnit, CtConstants};
use crate::opcode::{BinKind, Op};
use php_runtime::{BinaryOp, Value, ZStr};
use std::rc::Rc;

fn compile_src(src: &str, user_opcodes: Vec<&'static str>) -> CompiledUnit {
    let program =
        php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
    let opts = CompileOptions {
        filename: "t.php".into(),
        user_opcodes,
        // The VM's persistent internal constants (`Vm::ct_constants`);
        // `PHP_EOL` is the one these tests use.
        ct_constants: Some(CtConstants::new(
            [(ZStr::from("PHP_EOL"), Value::Str(ZStr::from("\n")))]
                .into_iter()
                .collect(),
        )),
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

#[test]
fn switch_compares_a_variable_subject_in_place() {
    // `zend_compile_switch`: `ZEND_IS_EQUAL` on the CV for every case
    // (php-src: an undefined `$y` warns on each case line).
    let ops = first_function_ops(&compile_src(
        "<?php function f() {\nswitch ($y) {\ncase 'x':\ncase 'y':\n}\n}",
        Vec::new(),
    ));
    let compares: Vec<&Op> = ops
        .iter()
        .filter(|op| {
            matches!(
                op,
                Op::Bin {
                    op: BinKind::Equal,
                    a: crate::opcode::Operand::Cv(_),
                    ..
                } | Op::CmpJmp {
                    op: BinKind::Equal,
                    a: crate::opcode::Operand::Cv(_),
                    ..
                }
            )
        })
        .collect();
    assert_eq!(compares.len(), 2, "{ops:?}");
    assert!(
        !ops.iter().any(|op| matches!(op, Op::Move { .. })),
        "{ops:?}"
    );
}

#[test]
fn array_map_with_a_dynamic_callable_compiles_to_a_loop() {
    // `zend_compile_func_array_map`: `$f(...)` is evaluated once, before
    // the array, and called per element; no closure, no array_map() call.
    let src = "<?php function f($f, $a) { return array_map($f(...), $a); }";
    let program =
        php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
    let opts = CompileOptions {
        filename: "t.php".into(),
        array_map_builtin: true,
        ..CompileOptions::default()
    };
    let unit = compile(&program, Rc::from(src.as_bytes()), opts).expect("compile");
    let ops = first_function_ops(&unit);
    assert!(
        ops.iter().any(|op| matches!(op, Op::InitDynCall { .. })),
        "{ops:?}"
    );
    assert!(
        !ops.iter().any(|op| matches!(op, Op::InitFCall { .. })),
        "{ops:?}"
    );
    let first_move = ops.iter().position(|op| matches!(op, Op::Move { .. }));
    let reset = ops.iter().position(|op| matches!(op, Op::FeReset { .. }));
    assert!(first_move < reset, "{ops:?}");
}

/// `zend_compile_expr_inner` leaves `CG(zend_lineno)` at the last compiled
/// operand, so a binary operator spanning lines takes the line of its right
/// operand (C PHP: `$r = $a +\n $a;` warns on the second line).
#[test]
fn multiline_binary_op_takes_the_line_of_its_last_operand() {
    let unit = compile_src(
        "<?php function foo($a) {\n$r = $a +\n  $a;\n}",
        // A hooked `ZEND_ADD` keeps the generic op (no specialisation).
        vec!["ZEND_ADD"],
    );
    let f = unit.decls.functions.borrow()[0].clone();
    let lines: Vec<(BinKind, u32)> = f
        .code
        .ops
        .iter()
        .zip(&f.code.lines)
        .filter_map(|(op, &line)| match op {
            Op::Bin { op, .. } => Some((*op, line)),
            _ => None,
        })
        .collect();
    assert_eq!(lines, vec![(BinKind::Arith(BinaryOp::Add), 3)]);
}

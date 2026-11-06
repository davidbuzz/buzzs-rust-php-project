//! Loop layout of `zend_compile_while()` / `zend_compile_for()`: the
//! condition follows the body and ends in one `JMPNZ` back to it, and the
//! body is compiled (CVs numbered) before the condition.

use super::{compile, CompileOptions, CompiledUnit};
use crate::opcode::Op;
use std::rc::Rc;

fn compile_src(src: &str) -> CompiledUnit {
    let program =
        php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
    let opts = CompileOptions {
        filename: "t.php".into(),
        ..CompileOptions::default()
    };
    compile(&program, Rc::from(src.as_bytes()), opts).expect("compile")
}

/// The ops and CV names of the unit's first declared function.
fn first_function(src: &str) -> (Vec<Op>, Vec<String>) {
    let unit = compile_src(src);
    let f = unit.decls.functions.borrow()[0].clone();
    let cvs = f
        .code
        .cv_names
        .iter()
        .map(|n| String::from_utf8_lossy(n.as_bytes()).into_owned())
        .collect();
    (f.code.ops.clone(), cvs)
}

/// Jump sites: (index of the jump op, its target).
type Jumps = Vec<(usize, u32)>;

/// The backward conditional jumps (`JMPNZ` to an earlier op) and all
/// unconditional jumps of `ops`.
fn jumps(ops: &[Op]) -> (Jumps, Jumps) {
    let mut back = Vec::new();
    let mut plain = Vec::new();
    for (at, op) in ops.iter().enumerate() {
        match op {
            Op::JmpNz(_, t) if (*t as usize) <= at => back.push((at, *t)),
            Op::Jmp(t) => plain.push((at, *t)),
            _ => {}
        }
    }
    (back, plain)
}

#[test]
fn while_tests_its_condition_at_the_bottom() {
    let (ops, cvs) =
        first_function("<?php function f() { $i = 0; while ($a = $i < 3) { $b = $i++; } }");
    let (back, plain) = jumps(&ops);
    assert_eq!(back.len(), 1, "{ops:?}");
    assert_eq!(plain.len(), 1, "{ops:?}");
    let (jmpnz_at, start) = back[0];
    let (jmp_at, cond) = plain[0];
    // JMP cond; start: body; cond: ...; JMPNZ start
    assert_eq!(start as usize, jmp_at + 1, "{ops:?}");
    assert!(
        (cond as usize) > jmp_at && (cond as usize) < jmpnz_at,
        "{ops:?}"
    );
    assert!(!ops.iter().any(|op| matches!(op, Op::JmpZ(..))), "{ops:?}");
    // `zend_compile_while` compiles the body first.
    assert_eq!(cvs, ["i", "b", "a"]);
}

#[test]
fn for_tests_its_condition_after_the_step() {
    let (ops, cvs) =
        first_function("<?php function f() { for ($i = 0; $c = $i < 1; $d = $i++) { $e = 1; } }");
    let (back, plain) = jumps(&ops);
    assert_eq!(back.len(), 1, "{ops:?}");
    assert_eq!(plain.len(), 1, "{ops:?}");
    assert_eq!(back[0].1 as usize, plain[0].0 + 1, "{ops:?}");
    assert!(!ops.iter().any(|op| matches!(op, Op::JmpZ(..))), "{ops:?}");
    // Init, body, step, condition (`zend_compile_for`).
    assert_eq!(cvs, ["i", "e", "d", "c"]);
}

#[test]
fn for_without_condition_jumps_back_from_the_step() {
    let (ops, _) =
        first_function("<?php function f() { for ($i = 0;; $i++) { if ($i > 3) break; } }");
    let (back, plain) = jumps(&ops);
    assert!(back.is_empty(), "{ops:?}");
    // The `break` and the jump back after the step.
    let last = plain.iter().max_by_key(|(at, _)| *at).copied();
    let Some((at, target)) = last else {
        panic!("no jump back: {ops:?}");
    };
    assert!((target as usize) < at, "{ops:?}");
}

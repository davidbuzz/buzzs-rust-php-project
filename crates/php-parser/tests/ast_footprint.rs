//! Memory footprint of the AST.
//!
//! A file's whole AST is alive while it compiles, so node sizes bound the
//! peak memory of `include` on large generated files
//! (`Zend/tests/runtime_compile_time_binary_operands.phpt` includes 11 MB of
//! code under `memory_limit=256M`; php-src's arena AST is a fraction of it).

use php_parser::ast::{Expr, ExprKind, Program, Stmt, StmtKind};
use php_parser::ParseOptions;

fn parse(src: &str) -> Program {
    match php_parser::parse(src.as_bytes(), &ParseOptions::default()) {
        Ok(p) => p,
        Err(e) => panic!("parse error: {e}"),
    }
}

/// `foreach` used to hold three inline expressions, which made every
/// statement node about 450 bytes; inline `while`/`switch` expressions kept
/// it at 192. No variant is now larger than an expression statement.
#[test]
fn statement_nodes_stay_small() {
    let size = std::mem::size_of::<Stmt>();
    assert!(
        size <= std::mem::size_of::<Expr>() + 40,
        "size_of::<Stmt>() = {size}"
    );
    assert!(size < 256, "size_of::<Stmt>() = {size}");
}

/// The generated file has about 900k expression nodes; inline argument
/// lists made each one 136 bytes.
#[test]
fn expression_nodes_stay_small() {
    let size = std::mem::size_of::<Expr>();
    assert!(size <= 104, "size_of::<Expr>() = {size}");
}

#[test]
fn call_arguments_are_boxed() {
    let p = parse("<?php f(1); $o->m(2, 3); A::s(); new B(4);");
    let args_len = |i: usize| {
        let StmtKind::Expr(e) = &p.statements[i].kind else {
            panic!("not an expression statement: {:?}", p.statements[i].kind);
        };
        match &e.kind {
            ExprKind::Call { args, .. }
            | ExprKind::MethodCall { args, .. }
            | ExprKind::StaticCall { args, .. }
            | ExprKind::New { args, .. } => args.args.len(),
            other => panic!("not a call: {other:?}"),
        }
    };
    assert_eq!(
        (args_len(0), args_len(1), args_len(2), args_len(3)),
        (1, 2, 0, 1)
    );
}

#[test]
fn foreach_parts_are_boxed() {
    let p = parse("<?php foreach ($a as $k => $v) {}");
    let StmtKind::Foreach {
        subject,
        key,
        value,
        by_ref,
        ..
    } = &p.statements[0].kind
    else {
        panic!("not a foreach: {:?}", p.statements[0].kind);
    };
    assert!(matches!(subject.kind, ExprKind::Var(_)));
    assert!(matches!(
        key.as_ref().map(|k| &k.kind),
        Some(ExprKind::Var(_))
    ));
    assert!(matches!(value.kind, ExprKind::Var(_)));
    assert!(!by_ref);
}

/// Statement, argument, array-item and echo lists carry no spare capacity
/// (a `Vec`'s first allocation holds four elements).
#[test]
fn lists_are_shrunk_to_their_length() {
    let p = parse("<?php if (1) { $a; } f($b); echo $c; $d = [1];");
    assert_eq!(p.statements.capacity(), p.statements.len());
    let StmtKind::If { branches, .. } = &p.statements[0].kind else {
        panic!("not an if");
    };
    let StmtKind::Block(block) = &branches[0].body.kind else {
        panic!("not a block");
    };
    assert_eq!(block.capacity(), 1);
    let StmtKind::Expr(call) = &p.statements[1].kind else {
        panic!("not an expression statement");
    };
    let ExprKind::Call { args, .. } = &call.kind else {
        panic!("not a call");
    };
    assert_eq!(args.args.capacity(), 1);
    let StmtKind::Echo(exprs) = &p.statements[2].kind else {
        panic!("not an echo");
    };
    assert_eq!(exprs.capacity(), 1);
    let StmtKind::Expr(assign) = &p.statements[3].kind else {
        panic!("not an expression statement");
    };
    let ExprKind::Assign { value, .. } = &assign.kind else {
        panic!("not an assignment");
    };
    let ExprKind::Array { items, .. } = &value.kind else {
        panic!("not an array");
    };
    assert_eq!(items.capacity(), 1);
}

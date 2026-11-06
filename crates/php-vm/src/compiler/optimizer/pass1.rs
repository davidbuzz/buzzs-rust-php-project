//! Pass 1: Simple local optimizations (ZEND_OPTIMIZER_PASS_1)
//!
//! - Constant expression evaluation: Op::Bin, Op::Unary, Op::Cast, Op::Strlen, Op::TypeCheck
//!   on literal operands.
//! - Constant conditional branches: Op::JmpZ, Op::JmpNz with literal conditions.
//! - Echo folding: eliminate empty echo, pre-convert literal scalar bool/int/null to string.
//! - String concat identity: "" . $x -> (string)$x, $x . "" -> (string)$x.
//! - Pass 2 boolean conversions (ZEND_OPTIMIZER_PASS_2):
//!   === null -> TypeCheck, (bool)$x -> Unary::Bool.

use super::{add_or_find_literal, PASS_2};
use crate::opcode::{type_mask, BinKind, CastKind, Op, OpArray, Operand, UnKind};
use php_runtime::error::CollectingContext;
use php_runtime::{BinaryOp, Value, ZStr};

/// Runs Pass 1 (and Pass 2) on `op_array`. Returns `true` if any instruction was changed.
pub(crate) fn run_pass1(op_array: &mut OpArray, level: u32) -> bool {
    let mut changed = false;
    let len = op_array.ops.len();

    for i in 0..len {
        let op = &op_array.ops[i];
        let mut replacement = None;

        match op {
            Op::Bin {
                op: bin_op,
                a,
                b,
                dst,
            } => {
                if let (Operand::Lit(la), Operand::Lit(lb)) = (a, b) {
                    let va = &op_array.literals[*la as usize];
                    let vb = &op_array.literals[*lb as usize];
                    if let Some(val) = fold_bin_op(*bin_op, va, vb) {
                        let lit = add_or_find_literal(&mut op_array.literals, val);
                        replacement = Some(Op::Move {
                            src: Operand::Lit(lit),
                            dst: *dst,
                        });
                    }
                } else if *bin_op == BinKind::Arith(BinaryOp::Concat) {
                    // String concat identity: "" . $b -> (string)$b, $a . "" -> (string)$a
                    if let Operand::Lit(la) = a {
                        if let Some(Value::Str(s)) = op_array.literals.get(*la as usize) {
                            if s.is_empty() {
                                replacement = Some(Op::Cast {
                                    kind: CastKind::String,
                                    a: *b,
                                    dst: *dst,
                                });
                            }
                        }
                    } else if let Operand::Lit(lb) = b {
                        if let Some(Value::Str(s)) = op_array.literals.get(*lb as usize) {
                            if s.is_empty() {
                                replacement = Some(Op::Cast {
                                    kind: CastKind::String,
                                    a: *a,
                                    dst: *dst,
                                });
                            }
                        }
                    }
                } else if (level & PASS_2) != 0 {
                    // Strength reduction: $x === null -> TypeCheck(NULL)
                    if *bin_op == BinKind::Identical {
                        if let Operand::Lit(lb) = b {
                            if matches!(op_array.literals.get(*lb as usize), Some(Value::Null)) {
                                replacement = Some(Op::TypeCheck {
                                    a: *a,
                                    mask: type_mask::NULL,
                                    dst: Some(*dst),
                                });
                            }
                        } else if let Operand::Lit(la) = a {
                            if matches!(op_array.literals.get(*la as usize), Some(Value::Null)) {
                                replacement = Some(Op::TypeCheck {
                                    a: *b,
                                    mask: type_mask::NULL,
                                    dst: Some(*dst),
                                });
                            }
                        }
                    }
                }
            }

            Op::Unary { op: un_op, a, dst } => {
                if let Operand::Lit(la) = a {
                    let va = &op_array.literals[*la as usize];
                    if let Some(val) = fold_unary_op(*un_op, va) {
                        let lit = add_or_find_literal(&mut op_array.literals, val);
                        replacement = Some(Op::Move {
                            src: Operand::Lit(lit),
                            dst: *dst,
                        });
                    }
                }
            }

            Op::Cast { kind, a, dst } => {
                if (level & PASS_2) != 0 && *kind == CastKind::Bool {
                    replacement = Some(Op::Unary {
                        op: UnKind::Bool,
                        a: *a,
                        dst: *dst,
                    });
                } else if let Operand::Lit(la) = a {
                    let va = &op_array.literals[*la as usize];
                    if let Some(val) = fold_cast(*kind, va) {
                        let lit = add_or_find_literal(&mut op_array.literals, val);
                        replacement = Some(Op::Move {
                            src: Operand::Lit(lit),
                            dst: *dst,
                        });
                    }
                }
            }

            Op::Strlen { a, dst } => {
                if let Operand::Lit(la) = a {
                    let len = if let Some(Value::Str(s)) = op_array.literals.get(*la as usize) {
                        Some(s.len() as i64)
                    } else {
                        None
                    };
                    if let Some(len) = len {
                        let lit =
                            add_or_find_literal(&mut op_array.literals, Value::Int(len));
                        replacement = Some(match dst {
                            Some(d) => Op::Move {
                                src: Operand::Lit(lit),
                                dst: *d,
                            },
                            None => Op::Nop,
                        });
                    }
                }
            }

            Op::TypeCheck { a, mask, dst } => {
                if let Operand::Lit(la) = a {
                    if let Some(va) = op_array.literals.get(*la as usize) {
                        let res = type_mask::matches(*mask, va);
                        let lit = add_or_find_literal(&mut op_array.literals, Value::Bool(res));
                        replacement = Some(match dst {
                            Some(d) => Op::Move {
                                src: Operand::Lit(lit),
                                dst: *d,
                            },
                            None => Op::Nop,
                        });
                    }
                }
            }

            Op::JmpZ(cond, target) => {
                if let Operand::Lit(lc) = cond {
                    if let Some(vc) = op_array.literals.get(*lc as usize) {
                        if let Some(truthy) = crate::compiler::expr::const_truthiness(vc) {
                            if !truthy {
                                // Falsy condition: branch is unconditionally taken
                                replacement = Some(Op::Jmp(*target));
                            } else {
                                // Truthy condition: branch is never taken
                                replacement = Some(Op::Nop);
                            }
                        }
                    }
                }
            }

            Op::JmpNz(cond, target) => {
                if let Operand::Lit(lc) = cond {
                    if let Some(vc) = op_array.literals.get(*lc as usize) {
                        if let Some(truthy) = crate::compiler::expr::const_truthiness(vc) {
                            if truthy {
                                // Truthy condition: branch is unconditionally taken
                                replacement = Some(Op::Jmp(*target));
                            } else {
                                // Falsy condition: branch is never taken
                                replacement = Some(Op::Nop);
                            }
                        }
                    }
                }
            }

            Op::JmpZEx {
                a,
                dst: _,
                target: _,
            } => {
                if let Operand::Lit(la) = a {
                    if let Some(va) = op_array.literals.get(*la as usize) {
                        if crate::compiler::expr::const_truthiness(va) == Some(true) {
                            // Truthy: never taken and dst is not modified; falls through
                            replacement = Some(Op::Nop);
                        }
                    }
                }
            }

            Op::JmpNzEx {
                a,
                dst: _,
                target: _,
            } => {
                if let Operand::Lit(la) = a {
                    if let Some(va) = op_array.literals.get(*la as usize) {
                        if crate::compiler::expr::const_truthiness(va) == Some(false) {
                            // Falsy: never taken and dst is not modified; falls through
                            replacement = Some(Op::Nop);
                        }
                    }
                }
            }

            Op::Echo(op) => {
                if let Operand::Lit(l) = op {
                    enum EchoAction {
                        Nop,
                        Preconvert(Value),
                    }
                    let action = match op_array.literals.get(*l as usize) {
                        Some(Value::Str(s)) if s.is_empty() => Some(EchoAction::Nop),
                        Some(Value::Null) => Some(EchoAction::Nop),
                        Some(Value::Bool(b)) => {
                            Some(EchoAction::Preconvert(Value::Str(ZStr::from(if *b { "1" } else { "" }))))
                        }
                        Some(Value::Int(n)) => {
                            Some(EchoAction::Preconvert(Value::Str(ZStr::from(n.to_string()))))
                        }
                        _ => None,
                    };
                    match action {
                        Some(EchoAction::Nop) => replacement = Some(Op::Nop),
                        Some(EchoAction::Preconvert(v)) => {
                            let lit = add_or_find_literal(&mut op_array.literals, v);
                            replacement = Some(Op::Echo(Operand::Lit(lit)));
                        }
                        None => {}
                    }
                }
            }

            _ => {}
        }

        if let Some(new_op) = replacement {
            op_array.ops[i] = new_op;
            changed = true;
        }
    }

    changed
}

fn fold_bin_op(op: BinKind, a: &Value, b: &Value) -> Option<Value> {
    // Never fold compound types, references, or resources
    if matches!(
        (a, b),
        (Value::Array(_), _)
            | (_, Value::Array(_))
            | (Value::Object(_), _)
            | (_, Value::Object(_))
            | (Value::Reference(_), _)
            | (_, Value::Reference(_))
            | (Value::Resource(_), _)
            | (_, Value::Resource(_))
    ) {
        return None;
    }

    match op {
        BinKind::Arith(bop) => {
            let mut ctx = CollectingContext::new();
            if let Ok(res) = php_runtime::ops::binary_op(bop, a, b, &mut ctx) {
                if ctx.diagnostics.is_empty() {
                    return Some(res);
                }
            }
            None
        }
        BinKind::Equal => {
            let mut ctx = CollectingContext::new();
            if let Ok(res) = php_runtime::ops::is_equal(a, b, &mut ctx) {
                if ctx.diagnostics.is_empty() {
                    return Some(Value::Bool(res));
                }
            }
            None
        }
        BinKind::NotEqual => {
            let mut ctx = CollectingContext::new();
            if let Ok(res) = php_runtime::ops::is_not_equal(a, b, &mut ctx) {
                if ctx.diagnostics.is_empty() {
                    return Some(Value::Bool(res));
                }
            }
            None
        }
        BinKind::Identical => {
            if let Ok(res) = php_runtime::ops::is_identical(a, b) {
                Some(Value::Bool(res))
            } else {
                None
            }
        }
        BinKind::NotIdentical => {
            if let Ok(res) = php_runtime::ops::is_identical(a, b) {
                Some(Value::Bool(!res))
            } else {
                None
            }
        }
        BinKind::Less => {
            let mut ctx = CollectingContext::new();
            if let Ok(res) = php_runtime::ops::is_smaller(a, b, &mut ctx) {
                if ctx.diagnostics.is_empty() {
                    return Some(Value::Bool(res));
                }
            }
            None
        }
        BinKind::LessEqual => {
            let mut ctx = CollectingContext::new();
            if let Ok(res) = php_runtime::ops::is_smaller_or_equal(a, b, &mut ctx) {
                if ctx.diagnostics.is_empty() {
                    return Some(Value::Bool(res));
                }
            }
            None
        }
        BinKind::Spaceship => {
            let mut ctx = CollectingContext::new();
            if let Ok(res) = php_runtime::ops::spaceship(a, b, &mut ctx) {
                if ctx.diagnostics.is_empty() {
                    return Some(res);
                }
            }
            None
        }
    }
}

fn fold_unary_op(op: UnKind, a: &Value) -> Option<Value> {
    match op {
        UnKind::Not => crate::compiler::expr::const_truthiness(a).map(|t| Value::Bool(!t)),
        UnKind::Bool => crate::compiler::expr::const_truthiness(a).map(Value::Bool),
        UnKind::BitNot => {
            let mut ctx = CollectingContext::new();
            if let Ok(res) = php_runtime::ops::bit_not(a, &mut ctx) {
                if ctx.diagnostics.is_empty() {
                    return Some(res);
                }
            }
            None
        }
        UnKind::Neg => {
            let mut ctx = CollectingContext::new();
            if let Ok(res) = php_runtime::ops::sub(&Value::Int(0), a, &mut ctx) {
                if ctx.diagnostics.is_empty() {
                    return Some(res);
                }
            }
            None
        }
        UnKind::Plus => {
            let mut ctx = CollectingContext::new();
            if let Ok(res) = php_runtime::ops::add(&Value::Int(0), a, &mut ctx) {
                if ctx.diagnostics.is_empty() {
                    return Some(res);
                }
            }
            None
        }
    }
}

fn fold_cast(kind: CastKind, a: &Value) -> Option<Value> {
    match kind {
        CastKind::Bool => crate::compiler::expr::const_truthiness(a).map(Value::Bool),
        CastKind::Int => {
            let mut ctx = CollectingContext::new();
            if let Ok(res) = a.to_long(&mut ctx) {
                if ctx.diagnostics.is_empty() {
                    return Some(Value::Int(res));
                }
            }
            None
        }
        CastKind::Float => {
            let mut ctx = CollectingContext::new();
            if let Ok(res) = a.to_double(&mut ctx) {
                if ctx.diagnostics.is_empty() {
                    return Some(Value::Float(res));
                }
            }
            None
        }
        CastKind::String => {
            // Avoid folding float to string to avoid precision / locale differences
            if matches!(a, Value::Float(_)) {
                return None;
            }
            let mut ctx = CollectingContext::new();
            if let Ok(res) = a.to_zstr(&mut ctx) {
                if ctx.diagnostics.is_empty() {
                    return Some(Value::Str(res));
                }
            }
            None
        }
        CastKind::Null => Some(Value::Null),
        CastKind::Array | CastKind::Object => None,
    }
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Pass 1, 2, 4: Local optimizations, strength reduction, and function call optimization:
//! - Pass 1 (ZEND_OPTIMIZER_PASS_1): Local constant folding, constant branches, echo folding.
//! - Pass 2 (ZEND_OPTIMIZER_PASS_2): Constant conversion and typecheck strength reduction.
//! - Pass 4 (ZEND_OPTIMIZER_PASS_4): Function call optimization on pure built-ins with constant arguments.

use super::{add_or_find_literal, PASS_2, PASS_4};
use crate::opcode::{type_mask, ArgSrc, BinKind, CastKind, Op, OpArray, Operand, UnKind};
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
                        let lit = add_or_find_literal(&mut op_array.literals, Value::Int(len));
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

            Op::Count { a, dst, .. } => {
                if let Operand::Lit(la) = a {
                    let count_opt = match op_array.literals.get(*la as usize) {
                        Some(Value::Array(arr)) => Some(arr.len() as i64),
                        _ => None,
                    };
                    if let Some(count) = count_opt {
                        let lit = add_or_find_literal(&mut op_array.literals, Value::Int(count));
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
                            Some(EchoAction::Preconvert(Value::Str(ZStr::from(if *b {
                                "1"
                            } else {
                                ""
                            }))))
                        }
                        Some(Value::Int(n)) => Some(EchoAction::Preconvert(Value::Str(
                            ZStr::from(n.to_string()),
                        ))),
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

    if (level & PASS_2) != 0 {
        changed |= run_pass2(op_array);
    }
    if (level & PASS_4) != 0 {
        changed |= run_pass4(op_array);
    }

    changed
}

/// Runs Pass 2: Constant conversion and typecheck strength reduction (ZEND_OPTIMIZER_PASS_2).
///
/// - Fold type checks on constants (Op::TypeCheck on literals, and is_null/is_int/etc. built-in calls on constants).
/// - Strength reduction: $x === null -> TypeCheck(NULL), null === $x -> TypeCheck(NULL).
/// - Constant conversion: (bool)$x -> Unary::Bool, literal casts folded.
pub(crate) fn run_pass2(op_array: &mut OpArray) -> bool {
    let mut changed = false;
    let len = op_array.ops.len();

    for i in 0..len {
        let op = &op_array.ops[i];
        let mut replacement = None;

        match op {
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

            Op::Bin {
                op: bin_op,
                a,
                b,
                dst,
            } => {
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

            Op::Cast { kind, a, dst } => {
                if let Operand::Lit(la) = a {
                    let va = &op_array.literals[*la as usize];
                    if let Some(val) = fold_cast(*kind, va) {
                        let lit = add_or_find_literal(&mut op_array.literals, val);
                        replacement = Some(Op::Move {
                            src: Operand::Lit(lit),
                            dst: *dst,
                        });
                    }
                } else if *kind == CastKind::Bool {
                    replacement = Some(Op::Unary {
                        op: UnKind::Bool,
                        a: *a,
                        dst: *dst,
                    });
                }
            }

            _ => {}
        }

        if let Some(new_op) = replacement {
            op_array.ops[i] = new_op;
            changed = true;
        }
    }

    // Fold type check function calls (e.g. is_null, is_int, is_float, is_string, is_bool, is_array) on constants
    changed |= fold_builtin_calls(op_array, is_typecheck_fn);

    changed
}

/// Runs Pass 4: Function call optimization (ZEND_OPTIMIZER_PASS_4).
///
/// - Evaluate pure built-in calls at compile-time when all arguments are known constants
///   (e.g. strlen, chr, ord, abs, min, max, is_null, is_string, etc.).
pub(crate) fn run_pass4(op_array: &mut OpArray) -> bool {
    let mut changed = false;

    // 1. Fold specialized Abs on literal operands if present
    for i in 0..op_array.ops.len() {
        if let Op::Abs { src, dst, to } = &op_array.ops[i] {
            if let Operand::Lit(la) = src {
                if let Some(val) = op_array.literals.get(*la as usize) {
                    if let Some(res) = evaluate_pure_builtin(b"abs", &[val]) {
                        let res_lit = add_or_find_literal(&mut op_array.literals, res);
                        if let Some(cv) = to {
                            op_array.ops[i] = Op::AssignCv {
                                cv: *cv,
                                val: Operand::Lit(res_lit),
                                dst: *dst,
                            };
                        } else {
                            op_array.ops[i] = match dst {
                                Some(d) => Op::Move {
                                    src: Operand::Lit(res_lit),
                                    dst: *d,
                                },
                                None => Op::Nop,
                            };
                        }
                        changed = true;
                    }
                }
            }
        }
    }

    // 2. Fold pure built-in function calls
    changed |= fold_builtin_calls(op_array, |_| true);

    changed
}

fn fold_builtin_calls(op_array: &mut OpArray, filter: impl Fn(&[u8]) -> bool) -> bool {
    let mut changed = false;
    let len = op_array.ops.len();

    for i in 0..len {
        // Only a call that resolves to the global function at compile time
        // is evaluated: an unqualified call in a namespace (one with a
        // fallback name) may reach a namespaced function of the same name
        // declared later (`zend_compile_call` compiles it as
        // `INIT_NS_FCALL_BY_NAME`, which php-src never evaluates).
        let fn_name_bytes = match &op_array.ops[i] {
            Op::InitFCall {
                name,
                fallback: None,
                ..
            } => match op_array.literals.get(*name as usize) {
                Some(Value::Str(s)) if !s.as_bytes().contains(&b'\\') => s.clone(),
                _ => continue,
            },
            _ => continue,
        };
        let name_bytes = fn_name_bytes.as_bytes().to_ascii_lowercase();

        if !filter(&name_bytes) {
            continue;
        }

        // Collect argument literals and find Op::DoCall
        let mut arg_lits = Vec::new();
        let mut send_indices = Vec::new();
        let mut docall_info = None;
        let mut valid = true;

        for j in (i + 1)..len {
            match &op_array.ops[j] {
                Op::Nop => {}
                Op::Send { src } => match src {
                    ArgSrc::Val(Operand::Lit(l)) | ArgSrc::CallResult(Operand::Lit(l)) => {
                        arg_lits.push(*l);
                        send_indices.push(j);
                    }
                    _ => {
                        valid = false;
                        break;
                    }
                },
                Op::DoCall { dst } => {
                    docall_info = Some((j, *dst));
                    break;
                }
                _ => {
                    valid = false;
                    break;
                }
            }
        }

        if !valid {
            continue;
        }

        let Some((call_idx, dst)) = docall_info else {
            continue;
        };

        // Get literal values for arguments
        let mut arg_values = Vec::with_capacity(arg_lits.len());
        let mut all_found = true;
        for &lit_idx in &arg_lits {
            if let Some(val) = op_array.literals.get(lit_idx as usize) {
                arg_values.push(val);
            } else {
                all_found = false;
                break;
            }
        }

        if !all_found {
            continue;
        }

        if let Some(result) = evaluate_pure_builtin(&name_bytes, &arg_values) {
            let res_lit = add_or_find_literal(&mut op_array.literals, result);
            op_array.ops[i] = Op::Nop;
            for s_idx in send_indices {
                op_array.ops[s_idx] = Op::Nop;
            }
            op_array.ops[call_idx] = match dst {
                Some(d) => Op::Move {
                    src: Operand::Lit(res_lit),
                    dst: d,
                },
                None => Op::Nop,
            };
            changed = true;
        }
    }

    changed
}

fn is_typecheck_fn(name: &[u8]) -> bool {
    matches!(
        name,
        b"is_null"
            | b"is_bool"
            | b"is_int"
            | b"is_float"
            | b"is_string"
            | b"is_array"
            | b"is_object"
            | b"is_resource"
            | b"is_scalar"
            | b"is_numeric"
    )
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn hex2bin(s: &[u8]) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for chunk in s.chunks_exact(2) {
        let hi = hex_digit(chunk[0])?;
        let lo = hex_digit(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn bin2hex(s: &[u8]) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = Vec::with_capacity(s.len() * 2);
    for &b in s {
        out.push(HEX[(b >> 4) as usize]);
        out.push(HEX[(b & 0x0f) as usize]);
    }
    out
}

/// The result of the call of the builtin `name` (lower case) with the
/// literal arguments `args`, when it is exactly what the run-time call
/// returns and the call would report nothing (sccp.c `ct_eval_func_call`
/// discards an evaluation that raised a warning, notice or deprecation or
/// threw, and never evaluates a deprecated function). Only exact argument
/// types fold, so neither coercion nor `strict_types` is involved; the
/// one-argument calls the handlers answer without a frame use the same
/// code as those handlers ([`crate::special::quick_eval`]: `ord()` of one
/// byte, `chr()` of 0..=255, `trim()` with php-src master's default list).
/// Deprecated aliases (`is_double()`, `is_integer()`, `is_long()`,
/// `doubleval()`, deprecated since 8.6) are never evaluated.
#[allow(clippy::too_many_lines)]
fn evaluate_pure_builtin(name: &[u8], args: &[&Value]) -> Option<Value> {
    if super::is_disabled_function(name) {
        return None;
    }
    if let ([arg], Some((id, _))) = (args, crate::special::quick_id(name)) {
        if let Some(v) = crate::special::quick_eval(id, arg) {
            return Some(v);
        }
    }
    match name {
        b"strlen" => match args {
            [Value::Str(s)] => Some(Value::Int(i64::try_from(s.len()).ok()?)),
            _ => None,
        },
        // `@compile-time-eval` in `zend_builtin_functions.stub.php`.
        b"zend_version" => args
            .is_empty()
            .then(|| Value::Str(ZStr::from(crate::ZEND_VERSION.as_bytes()))),
        // sccp.c `can_ct_eval_func_call` evaluates `dirname` on POSIX; a
        // level below 1 throws, which discards the evaluation.
        b"dirname" => match args {
            [Value::Str(p)] => Some(Value::Str(ZStr::from(
                php_stdstr::transform::dirname(p.as_bytes(), 1).ok()?,
            ))),
            [Value::Str(p), Value::Int(levels)] if *levels >= 1 => Some(Value::Str(ZStr::from(
                php_stdstr::transform::dirname(p.as_bytes(), *levels).ok()?,
            ))),
            _ => None,
        },
        // `abs(PHP_INT_MIN)` is the float `-(float) PHP_INT_MIN`, silently
        // (the other ints and floats are answered by `quick_eval`).
        #[allow(clippy::cast_precision_loss)]
        b"abs" => match args {
            [Value::Int(i64::MIN)] => Some(Value::Float(-(i64::MIN as f64))),
            _ => None,
        },
        // Of ints only: `min()` keeps the second of two equal values and
        // `max()` the first, which for ints cannot be told apart, while a
        // mix of types (`min(1, "1")`, `min(1.0, 1)`) or two zero floats
        // of different signs can.
        b"min" | b"max" => {
            let mut best = match args.first() {
                Some(Value::Int(i)) if args.len() >= 2 => *i,
                _ => return None,
            };
            for arg in &args[1..] {
                let Value::Int(i) = arg else {
                    return None;
                };
                best = if name == b"min" {
                    best.min(*i)
                } else {
                    best.max(*i)
                };
            }
            Some(Value::Int(best))
        }
        b"is_null" => match args {
            [v] => Some(Value::Bool(matches!(v, Value::Null))),
            _ => None,
        },
        b"is_bool" => match args {
            [v] => Some(Value::Bool(matches!(v, Value::Bool(_)))),
            _ => None,
        },
        b"is_int" => match args {
            [v] => Some(Value::Bool(matches!(v, Value::Int(_)))),
            _ => None,
        },
        b"is_float" => match args {
            [v] => Some(Value::Bool(matches!(v, Value::Float(_)))),
            _ => None,
        },
        b"is_string" => match args {
            [v] => Some(Value::Bool(matches!(v, Value::Str(_)))),
            _ => None,
        },
        b"is_array" => match args {
            [v] => Some(Value::Bool(matches!(v, Value::Array(_)))),
            _ => None,
        },
        b"is_object" => match args {
            [v] => Some(Value::Bool(matches!(v, Value::Object(_)))),
            _ => None,
        },
        b"is_resource" => match args {
            [v] => Some(Value::Bool(matches!(v, Value::Resource(_)))),
            _ => None,
        },
        b"is_scalar" => match args {
            [v] => Some(Value::Bool(matches!(
                v,
                Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_)
            ))),
            _ => None,
        },
        b"is_numeric" => match args {
            [v] => Some(Value::Bool(match v {
                Value::Int(_) | Value::Float(_) => true,
                Value::Str(s) => php_runtime::numeric::is_numeric(s.as_bytes()),
                _ => false,
            })),
            _ => None,
        },
        b"boolval" => match args {
            [v] => fold_cast(CastKind::Bool, v),
            _ => None,
        },
        b"intval" => match args {
            [v] => fold_cast(CastKind::Int, v),
            _ => None,
        },
        b"floatval" => match args {
            [v] => fold_cast(CastKind::Float, v),
            _ => None,
        },
        b"strval" => match args {
            [v] => fold_cast(CastKind::String, v),
            _ => None,
        },
        // The alias of `rtrim()` (`quick_eval` answers `rtrim` itself),
        // with the same default list.
        b"chop" => match args {
            [Value::Str(s)] => {
                let mut notices = php_stdstr::error::Notices::new();
                let r = php_stdstr::transform::trim(
                    s.as_bytes(),
                    None,
                    php_stdstr::transform::TrimMode::Right,
                    &mut notices,
                );
                notices.is_empty().then(|| Value::Str(ZStr::from(r)))
            }
            _ => None,
        },
        b"bin2hex" => match args {
            [Value::Str(s)] => Some(Value::Str(ZStr::from(bin2hex(s.as_bytes())))),
            _ => None,
        },
        // An odd length or a non-hex digit warns and gives false: left to
        // the call.
        b"hex2bin" => match args {
            [Value::Str(s)] => Some(Value::Str(ZStr::from(hex2bin(s.as_bytes())?))),
            _ => None,
        },
        b"substr" => match args {
            [Value::Str(st), Value::Int(offset)]
            | [Value::Str(st), Value::Int(offset), Value::Null] => Some(Value::Str(ZStr::from(
                php_stdstr::search::substr(st.as_bytes(), *offset, None),
            ))),
            [Value::Str(st), Value::Int(offset), Value::Int(len)] => Some(Value::Str(ZStr::from(
                php_stdstr::search::substr(st.as_bytes(), *offset, Some(*len)),
            ))),
            _ => None,
        },
        // sccp.c `can_ct_eval_func_call`: a result under 64 KiB (a negative
        // count throws).
        b"str_repeat" => match args {
            [Value::Str(s), Value::Int(times)] => {
                let times = usize::try_from(*times).ok()?;
                let total = s.len().checked_mul(times)?;
                (total < 64 * 1024).then(|| Value::Str(ZStr::from(s.as_bytes().repeat(times))))
            }
            _ => None,
        },
        b"intdiv" => match args {
            [Value::Int(a), Value::Int(b)] if *b != 0 && !(*a == i64::MIN && *b == -1) => {
                Some(Value::Int(*a / *b))
            }
            _ => None,
        },
        b"pi" => args
            .is_empty()
            .then_some(Value::Float(std::f64::consts::PI)),
        _ => None,
    }
}

pub(crate) fn fold_bin_op(op: BinKind, a: &Value, b: &Value) -> Option<Value> {
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

pub(crate) fn fold_unary_op(op: UnKind, a: &Value) -> Option<Value> {
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

pub(crate) fn fold_cast(kind: CastKind, a: &Value) -> Option<Value> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// GH-17733: a function removed by `disable_functions` is not in the
    /// function table, so php-src never evaluates a call to it at compile
    /// time (`str_repeat("ab", 3)` then throws "Call to undefined function").
    #[test]
    fn disabled_function_is_not_evaluated() {
        let s = Value::Str(ZStr::from(&b"ab"[..]));
        let n = Value::Int(3);
        assert!(evaluate_pure_builtin(b"str_repeat", &[&s, &n]).is_some());
        super::super::set_disabled_functions([b"str_repeat".to_vec()]);
        assert!(evaluate_pure_builtin(b"str_repeat", &[&s, &n]).is_none());
        assert!(evaluate_pure_builtin(b"strlen", &[&s]).is_some());
        super::super::set_disabled_functions(std::iter::empty());
        assert!(evaluate_pure_builtin(b"str_repeat", &[&s, &n]).is_some());
    }

    /// `zend_version()` and `dirname()` are evaluated at compile time
    /// (ext/opcache/tests/zend_version.phpt, ct_eval_frameless_001.phpt);
    /// expectations from `/usr/bin/php8.5`.
    #[test]
    fn zend_version_and_dirname_are_evaluated() {
        assert!(matches!(
            evaluate_pure_builtin(b"zend_version", &[]),
            Some(Value::Str(s)) if s.as_bytes() == crate::ZEND_VERSION.as_bytes()
        ));
        let p = Value::Str(ZStr::from(&b"/a/b/c/"[..]));
        let dir = |args: &[&Value]| match evaluate_pure_builtin(b"dirname", args) {
            Some(Value::Str(s)) => Some(s.as_bytes().to_vec()),
            _ => None,
        };
        assert_eq!(dir(&[&p]), Some(b"/a/b".to_vec()));
        assert_eq!(dir(&[&p, &Value::Int(2)]), Some(b"/a".to_vec()));
        assert_eq!(dir(&[&p, &Value::Int(9)]), Some(b"/".to_vec()));
        assert_eq!(
            dir(&[&Value::Str(ZStr::from(&b"abc"[..]))]),
            Some(b".".to_vec())
        );
        // `dirname($p, 0)` throws a ValueError at run time.
        assert_eq!(dir(&[&p, &Value::Int(0)]), None);
        assert_eq!(dir(&[&Value::Int(1)]), None);
    }

    fn bstr(b: &[u8]) -> Value {
        Value::Str(ZStr::from(b))
    }

    fn eval_bytes(name: &[u8], args: &[&Value]) -> Option<Vec<u8>> {
        match evaluate_pure_builtin(name, args) {
            Some(Value::Str(s)) => Some(s.as_bytes().to_vec()),
            _ => None,
        }
    }

    /// [`evaluate_pure_builtin`] as its `Debug` text (`Value` has no
    /// `PartialEq`).
    fn ev(name: &[u8], args: &[&Value]) -> Option<String> {
        evaluate_pure_builtin(name, args).map(|v| format!("{v:?}"))
    }

    fn dbg(v: Value) -> Option<String> {
        Some(format!("{v:?}"))
    }

    /// A call that reports something at run time is left to the call:
    /// `chr(256)`, `chr(-1)`, `ord('')` and `ord('Hello')` deprecate
    /// (chr_out_of_range.phpt, ord_not_1_byte.phpt), and the deprecated
    /// aliases deprecate themselves (is_float_alias.phpt,
    /// is_int_aliases.phpt). Expectations from `/usr/bin/php8.5`.
    #[test]
    fn calls_with_diagnostics_are_not_evaluated() {
        assert_eq!(ev(b"chr", &[&Value::Int(256)]), None);
        assert_eq!(ev(b"chr", &[&Value::Int(-1)]), None);
        assert_eq!(eval_bytes(b"chr", &[&Value::Int(65)]), Some(b"A".to_vec()));
        assert_eq!(eval_bytes(b"chr", &[&Value::Int(255)]), Some(vec![255]));
        assert_eq!(ev(b"ord", &[&bstr(b"")]), None);
        assert_eq!(ev(b"ord", &[&bstr(b"Hello")]), None);
        assert_eq!(ev(b"ord", &[&bstr(b"A")]), dbg(Value::Int(65)));
        let aliases: [&[u8]; 4] = [b"is_double", b"is_integer", b"is_long", b"doubleval"];
        for alias in aliases {
            assert_eq!(ev(alias, &[&Value::Int(1)]), None);
            assert!(!is_typecheck_fn(alias));
        }
        assert_eq!(
            ev(b"is_float", &[&Value::Float(1.0)]),
            dbg(Value::Bool(true))
        );
        assert_eq!(ev(b"is_int", &[&Value::Int(1)]), dbg(Value::Bool(true)));
        // `strlen(null)` deprecates (TypeError under strict_types).
        assert_eq!(ev(b"strlen", &[&Value::Null]), None);
        assert_eq!(ev(b"strlen", &[&Value::Int(12)]), None);
        assert_eq!(ev(b"strlen", &[&bstr(b"abc")]), dbg(Value::Int(3)));
        // An invalid hex string warns.
        assert_eq!(ev(b"hex2bin", &[&bstr(b"abc")]), None);
        assert_eq!(ev(b"hex2bin", &[&bstr(b"zz")]), None);
        assert_eq!(
            eval_bytes(b"hex2bin", &[&bstr(b"4142")]),
            Some(b"AB".to_vec())
        );
    }

    /// php-src master's default trim list is " \f\t\n\r\v\0" (form feed
    /// since 8.4); trim.phpt.
    #[test]
    fn trim_strips_the_run_time_list() {
        let s = bstr(b"\x0c\t\n\r\x0b\0 x \x0c\0");
        assert_eq!(eval_bytes(b"trim", &[&s]), Some(b"x".to_vec()));
        assert_eq!(eval_bytes(b"ltrim", &[&s]), Some(b"x \x0c\0".to_vec()));
        assert_eq!(
            eval_bytes(b"rtrim", &[&s]),
            Some(b"\x0c\t\n\r\x0b\0 x".to_vec())
        );
        assert_eq!(
            eval_bytes(b"chop", &[&s]),
            Some(b"\x0c\t\n\r\x0b\0 x".to_vec())
        );
        assert_eq!(ev(b"trim", &[&Value::Int(1)]), None);
    }

    /// `min()`/`max()` of equal values of different types return one of
    /// them by position (`min(1, "1")` is "1", `min(1.0, 1)` is 1 in
    /// php8.5): only ints are evaluated.
    #[test]
    fn min_max_fold_ints_only() {
        let (one, s1, f1) = (Value::Int(1), bstr(b"1"), Value::Float(1.0));
        assert_eq!(ev(b"min", &[&one, &s1]), None);
        assert_eq!(ev(b"min", &[&f1, &one]), None);
        assert_eq!(ev(b"max", &[&one, &f1]), None);
        assert_eq!(
            ev(b"min", &[&Value::Int(3), &one, &Value::Int(2)]),
            dbg(Value::Int(1))
        );
        assert_eq!(
            ev(b"max", &[&Value::Int(3), &one, &Value::Int(5)]),
            dbg(Value::Int(5))
        );
        assert_eq!(ev(b"max", &[&one]), None);
    }

    /// sccp.c `can_ct_eval_func_call`: `str_repeat()` folds only below
    /// 64 KiB, and a negative count throws.
    #[test]
    fn str_repeat_is_bounded() {
        let ab = bstr(b"ab");
        assert_eq!(
            eval_bytes(b"str_repeat", &[&ab, &Value::Int(3)]),
            Some(b"ababab".to_vec())
        );
        assert_eq!(ev(b"str_repeat", &[&ab, &Value::Int(40_000)]), None);
        assert_eq!(ev(b"str_repeat", &[&ab, &Value::Int(-1)]), None);
    }
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Pass 3: Jump optimization (ZEND_OPTIMIZER_PASS_3)
//!
//! - Jump-to-jump threading: Jmp(L1) where L1 resolves to Jmp(L2) -> thread to L2 (cycle-safe).
//! - Eliminate redundant jumps: Jmp(next) -> Nop.
//! - Conditional jump threading: JmpZ/JmpNz target threading through Jmp(L2) and NOPs.
//! - Conditional jump to next: JmpZ(cond, next) -> Free(cond) if Tmp or Cv, else Nop.
//! - Same-condition jump threading: JmpZ(cond, L1) where L1 is JmpZ(cond, L2) -> jump to L2.
//! - Target threading for JmpZEx, JmpNzEx, JmpSet, Coalesce, JmpNull.

use crate::opcode::{Label, Op, OpArray, Operand};

/// Skips any `Op::Nop` instructions starting from `target`.
pub(crate) fn skip_nops(ops: &[Op], mut target: usize) -> usize {
    while target < ops.len() && matches!(ops[target], Op::Nop) {
        target += 1;
    }
    target
}

/// Follows a jump target through any chained unconditional jumps and NOPs.
/// Returns the final instruction index (cycle-safe, max 32 hops).
pub(crate) fn follow_jumps(ops: &[Op], mut target: usize) -> usize {
    let mut hops = 0;
    while hops < 32 {
        let next = skip_nops(ops, target);
        if next < ops.len() {
            if let Op::Jmp(t) = ops[next] {
                if t as usize == next {
                    break;
                }
                target = t as usize;
                hops += 1;
                continue;
            }
        }
        target = next;
        break;
    }
    target
}

/// What replaces a conditional jump to the next instruction: the condition
/// is still evaluated for its side effects as php-src's block pass does
/// (`FREE` of a temporary; `CHECK_VAR` of a CV, which warns "Undefined
/// variable", here `Free` of the CV, which reads it).
pub(crate) fn drop_condition(cond: Operand) -> Op {
    match cond {
        Operand::Tmp(_) | Operand::Cv(_) => Op::Free(cond),
        _ => Op::Nop,
    }
}

/// Runs Pass 3 on `op_array`. Returns `true` if any jump was modified.
pub(crate) fn run_pass3(op_array: &mut OpArray) -> bool {
    let mut changed = false;
    let len = op_array.ops.len();

    for i in 0..len {
        let op = &op_array.ops[i];
        let mut replacement = None;

        match op {
            Op::Jmp(target) => {
                let resolved = follow_jumps(&op_array.ops, *target as usize);
                let next_ip = skip_nops(&op_array.ops, i + 1);
                if resolved == next_ip {
                    // Jump to the immediately following instruction is a NOP
                    replacement = Some(Op::Nop);
                } else if resolved != *target as usize {
                    replacement = Some(Op::Jmp(resolved as Label));
                }
            }

            Op::JmpZ(cond, target) => {
                let resolved = follow_jumps(&op_array.ops, *target as usize);
                let next_ip = skip_nops(&op_array.ops, i + 1);
                if resolved == next_ip {
                    // Branch to next instruction: free a temporary, check a CV
                    replacement = Some(drop_condition(*cond));
                } else {
                    // Check same-condition threading at target
                    let at_target = skip_nops(&op_array.ops, resolved);
                    let mut threaded = resolved;
                    if at_target < op_array.ops.len() {
                        match &op_array.ops[at_target] {
                            Op::JmpZ(other_cond, next_target) if other_cond == cond => {
                                threaded = follow_jumps(&op_array.ops, *next_target as usize);
                            }
                            Op::JmpNz(other_cond, _) if other_cond == cond => {
                                // Condition is known to be false, so JmpNz is not taken
                                threaded = follow_jumps(&op_array.ops, at_target + 1);
                            }
                            _ => {}
                        }
                    }
                    if threaded != *target as usize {
                        replacement = Some(Op::JmpZ(*cond, threaded as Label));
                    }
                }
            }

            Op::JmpNz(cond, target) => {
                let resolved = follow_jumps(&op_array.ops, *target as usize);
                let next_ip = skip_nops(&op_array.ops, i + 1);
                if resolved == next_ip {
                    replacement = Some(drop_condition(*cond));
                } else {
                    let at_target = skip_nops(&op_array.ops, resolved);
                    let mut threaded = resolved;
                    if at_target < op_array.ops.len() {
                        match &op_array.ops[at_target] {
                            Op::JmpNz(other_cond, next_target) if other_cond == cond => {
                                threaded = follow_jumps(&op_array.ops, *next_target as usize);
                            }
                            Op::JmpZ(other_cond, _) if other_cond == cond => {
                                // Condition is known to be true, so JmpZ is not taken
                                threaded = follow_jumps(&op_array.ops, at_target + 1);
                            }
                            _ => {}
                        }
                    }
                    if threaded != *target as usize {
                        replacement = Some(Op::JmpNz(*cond, threaded as Label));
                    }
                }
            }

            Op::JmpZEx { a, dst, target } => {
                let resolved = follow_jumps(&op_array.ops, *target as usize);
                if resolved != *target as usize {
                    replacement = Some(Op::JmpZEx {
                        a: *a,
                        dst: *dst,
                        target: resolved as Label,
                    });
                }
            }

            Op::JmpNzEx { a, dst, target } => {
                let resolved = follow_jumps(&op_array.ops, *target as usize);
                if resolved != *target as usize {
                    replacement = Some(Op::JmpNzEx {
                        a: *a,
                        dst: *dst,
                        target: resolved as Label,
                    });
                }
            }

            Op::JmpSet { a, dst, target } => {
                let resolved = follow_jumps(&op_array.ops, *target as usize);
                if resolved != *target as usize {
                    replacement = Some(Op::JmpSet {
                        a: *a,
                        dst: *dst,
                        target: resolved as Label,
                    });
                }
            }

            Op::Coalesce { a, dst, target } => {
                let resolved = follow_jumps(&op_array.ops, *target as usize);
                if resolved != *target as usize {
                    replacement = Some(Op::Coalesce {
                        a: *a,
                        dst: *dst,
                        target: resolved as Label,
                    });
                }
            }

            Op::JmpNull { a, dst, target } => {
                let resolved = follow_jumps(&op_array.ops, *target as usize);
                if resolved != *target as usize {
                    replacement = Some(Op::JmpNull {
                        a: *a,
                        dst: *dst,
                        target: resolved as Label,
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

    changed
}

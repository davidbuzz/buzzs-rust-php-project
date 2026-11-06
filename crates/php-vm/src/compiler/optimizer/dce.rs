//! Pass 14: DCE (Dead Code Elimination) (ZEND_OPTIMIZER_PASS_14)
//!
//! - Elimination of side-effect-free instructions whose result temporary is never used.
//! - Clearing unused `dst` on side-effecting instructions (AssignCv, AssignOp, DoCall...).
//! - Elimination of unreachable instructions following unconditional terminators.

use super::{clear_def_tmp, def_tmp};
use crate::opcode::{Op, OpArray, Operand, UnKind};

/// Runs DCE on `op_array`. Returns `true` if any instruction was removed or simplified.
pub(crate) fn run_dce(op_array: &mut OpArray) -> bool {
    let mut changed = false;
    let num_tmps = op_array.num_tmps as usize;

    // 1. Unreachable basic block instructions following unconditional terminators
    let leaders = super::block_pass::find_leaders(op_array);
    let len = op_array.ops.len();
    let mut in_unreachable = false;

    for i in 0..len {
        if leaders[i] {
            in_unreachable = false;
        }

        if in_unreachable {
            // Keep declaration ops (functions, classes, consts are hoisted)
            let is_decl = matches!(
                op_array.ops[i],
                Op::DeclareFunction(_) | Op::DeclareClass(_) | Op::DeclareConst { .. }
            );
            if !is_decl && !matches!(op_array.ops[i], Op::Nop) {
                op_array.ops[i] = Op::Nop;
                changed = true;
            }
        } else {
            let is_terminator = matches!(
                op_array.ops[i],
                Op::Jmp(_)
                    | Op::Return { .. }
                    | Op::ReturnRef { .. }
                    | Op::ReturnByRefValue { .. }
                    | Op::Throw(_)
                    | Op::Rethrow
                    | Op::Exit(_)
                    | Op::FastRet { .. }
            );
            if is_terminator {
                in_unreachable = true;
            }
        }
    }

    // 2. Count uses of each temporary across all ops, places, and live_tmps
    let mut use_counts = vec![0u32; num_tmps + 1];
    super::count_tmp_uses(op_array, &mut use_counts);

    // 3. Remove side-effect-free dead instructions and clear unused dst
    for op in &mut op_array.ops {
        if let Some(t) = def_tmp(op) {
            let count = use_counts.get(t as usize).copied().unwrap_or(0);
            if count == 0 {
                // Check if this op is side-effect-free
                let is_pure = match op {
                    Op::Move { src, .. } => {
                        matches!(src, Operand::Lit(_) | Operand::Tmp(_))
                    }
                    Op::Unary { op, a, .. } => {
                        matches!(op, UnKind::Bool | UnKind::Not)
                            && matches!(a, Operand::Lit(_) | Operand::Tmp(_))
                    }
                    Op::TypeCheck { a, .. } => {
                        matches!(a, Operand::Lit(_) | Operand::Tmp(_))
                    }
                    _ => false,
                };

                if is_pure {
                    *op = Op::Nop;
                    changed = true;
                } else if clear_def_tmp(op) {
                    changed = true;
                }
            }
        } else if let Op::Free(Operand::Tmp(t)) = op {
            let count = use_counts.get(*t as usize).copied().unwrap_or(0);
            if count == 0 {
                *op = Op::Nop;
                changed = true;
            }
        }
    }

    changed
}

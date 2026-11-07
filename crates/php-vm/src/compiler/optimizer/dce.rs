//! Pass 14: DCE (Dead Code Elimination) (ZEND_OPTIMIZER_PASS_14)
//!
//! - Basic block CFG reachability: eliminates dead blocks following unconditional
//!   terminators or unexecutable branches.
//! - Elimination of side-effect-free instructions whose result temporary is never used.
//! - Clearing unused `dst` on side-effecting instructions (AssignCv, AssignOp, DoCall...).
//! - Redundant `Free(Tmp)` removal for dead temporaries.
//! - Dead jump-to-next elimination.

use super::{clear_def_tmp, def_tmp, for_each_operand_mut, for_each_place_operand_mut};
use crate::opcode::{BinKind, CastKind, ClassSpec, Op, OpArray, Operand, UnKind};
use php_runtime::BinaryOp;

/// Runs DCE on `op_array`. Returns `true` if any instruction was removed or simplified.
pub(crate) fn run_dce(op_array: &mut OpArray) -> bool {
    let mut changed = false;
    let len = op_array.ops.len();
    if len == 0 {
        return false;
    }
    let num_tmps = op_array.num_tmps as usize;

    // 1. CFG basic block reachability
    let leaders = super::block_pass::find_leaders(op_array);
    let mut block_starts = Vec::new();
    for i in 0..len {
        if leaders[i] {
            block_starts.push(i);
        }
    }
    block_starts.push(len);
    let num_blocks = block_starts.len() - 1;

    let mut inst_to_block = vec![0usize; len + 1];
    for b in 0..num_blocks {
        for i in block_starts[b]..block_starts[b + 1] {
            inst_to_block[i] = b;
        }
    }
    inst_to_block[len] = num_blocks;

    let mut reachable = vec![false; num_blocks];
    let mut worklist = Vec::new();
    reachable[0] = true;
    worklist.push(0);

    while let Some(b) = worklist.pop() {
        let b_start = block_starts[b];
        let b_end = block_starts[b + 1];

        // Find last non-Nop instruction in block b
        let last_idx = (b_start..b_end)
            .rev()
            .find(|&i| !matches!(op_array.ops[i], Op::Nop));
        let mut successors = Vec::new();

        if let Some(idx) = last_idx {
            let op = &op_array.ops[idx];
            match op {
                Op::Jmp(t) => {
                    if (*t as usize) < len {
                        successors.push(inst_to_block[*t as usize]);
                    }
                }
                Op::Return { .. }
                | Op::ReturnRef { .. }
                | Op::ReturnByRefValue { .. }
                | Op::Throw(_)
                | Op::Rethrow
                | Op::Exit(_)
                | Op::FastRet { .. } => {
                    // Unconditional terminators: no successors
                }
                Op::JmpZ(_, t)
                | Op::JmpNz(_, t)
                | Op::CmpJmp { target: t, .. }
                | Op::JmpZEx { target: t, .. }
                | Op::JmpNzEx { target: t, .. }
                | Op::JmpSet { target: t, .. }
                | Op::Coalesce { target: t, .. }
                | Op::JmpNull { target: t, .. } => {
                    if (*t as usize) < len {
                        successors.push(inst_to_block[*t as usize]);
                    }
                    if b + 1 < num_blocks {
                        successors.push(b + 1);
                    }
                }
                Op::FastCall { target: t, .. }
                | Op::Catch { next: t, .. }
                | Op::FeReset { end: t, .. }
                | Op::FeFetch { end: t, .. }
                | Op::FeFreeOnReturn { loop_at: t, .. }
                | Op::AssertCheck { skip: t, .. }
                | Op::BindStatic { skip: t, .. }
                | Op::New { skip: t, .. }
                | Op::RecvInit { skip: t, .. } => {
                    if (*t as usize) < len {
                        successors.push(inst_to_block[*t as usize]);
                    }
                    if b + 1 < num_blocks {
                        successors.push(b + 1);
                    }
                }
                _ => {
                    if b + 1 < num_blocks {
                        successors.push(b + 1);
                    }
                }
            }
        } else if b + 1 < num_blocks {
            successors.push(b + 1);
        }

        // Check try_table entries overlapping block b
        for entry in &op_array.try_table {
            let try_start = entry.try_start as usize;
            let try_end = entry.try_end as usize;
            if b_start < try_end && b_end > try_start {
                if let Some(cs) = entry.catch_start {
                    if (cs as usize) < len {
                        successors.push(inst_to_block[cs as usize]);
                    }
                }
                if let Some((_, f_start, _)) = entry.finally {
                    if (f_start as usize) < len {
                        successors.push(inst_to_block[f_start as usize]);
                    }
                }
            }
        }

        for succ in successors {
            if succ < num_blocks && !reachable[succ] {
                reachable[succ] = true;
                worklist.push(succ);
            }
        }
    }

    // Eliminate unreachable blocks and intra-block code following unconditional terminators
    for b in 0..num_blocks {
        let b_start = block_starts[b];
        let b_end = block_starts[b + 1];

        if !reachable[b] {
            for i in b_start..b_end {
                let is_decl = matches!(
                    op_array.ops[i],
                    Op::DeclareFunction(_) | Op::DeclareClass(_) | Op::DeclareConst { .. }
                );
                if !is_decl && !matches!(op_array.ops[i], Op::Nop) {
                    op_array.ops[i] = Op::Nop;
                    changed = true;
                }
            }
        } else {
            let mut in_unreachable = false;
            for i in b_start..b_end {
                if in_unreachable {
                    let is_decl = matches!(
                        op_array.ops[i],
                        Op::DeclareFunction(_) | Op::DeclareClass(_) | Op::DeclareConst { .. }
                    );
                    if !is_decl && !matches!(op_array.ops[i], Op::Nop) {
                        op_array.ops[i] = Op::Nop;
                        changed = true;
                    }
                } else if matches!(
                    op_array.ops[i],
                    Op::Jmp(_)
                        | Op::Return { .. }
                        | Op::ReturnRef { .. }
                        | Op::ReturnByRefValue { .. }
                        | Op::Throw(_)
                        | Op::Rethrow
                        | Op::Exit(_)
                        | Op::FastRet { .. }
                ) {
                    in_unreachable = true;
                }
            }
        }
    }

    // 2. Dead jump-to-next elimination
    for i in 0..len {
        if let Op::Jmp(target) = op_array.ops[i] {
            let mut next = i + 1;
            while next < len && matches!(op_array.ops[next], Op::Nop) {
                next += 1;
            }
            if target as usize == next {
                op_array.ops[i] = Op::Nop;
                changed = true;
            }
        }
    }

    // 3. Iterative elimination of side-effect-free dead computations and dead Frees
    for _ in 0..8 {
        let mut inner_changed = false;
        let mut non_free_uses = vec![0u32; num_tmps + 1];
        count_non_free_uses(op_array, &mut non_free_uses);

        for op in &mut op_array.ops {
            if let Some(t) = def_tmp(op) {
                let uses = non_free_uses.get(t as usize).copied().unwrap_or(0);
                if uses == 0 {
                    if is_pure_op(op) {
                        *op = Op::Nop;
                        inner_changed = true;
                    } else if clear_def_tmp(op) {
                        inner_changed = true;
                    }
                }
            } else if is_pure_dead_op(op) {
                *op = Op::Nop;
                inner_changed = true;
            } else if let Op::Free(Operand::Tmp(t)) = op {
                let uses = non_free_uses.get(*t as usize).copied().unwrap_or(0);
                if uses == 0 {
                    *op = Op::Nop;
                    inner_changed = true;
                }
            }
        }

        if !inner_changed {
            break;
        }
        changed = true;
    }

    changed
}

fn count_non_free_uses(op_array: &OpArray, counts: &mut [u32]) {
    for op in &op_array.ops {
        if matches!(op, Op::Free(_)) {
            continue;
        }
        let mut op_clone = op.clone();
        for_each_operand_mut(&mut op_clone, |operand| match operand {
            Operand::Tmp(t) | Operand::TmpKeep(t) => {
                if let Some(c) = counts.get_mut(*t as usize) {
                    *c += 1;
                }
            }
            _ => {}
        });
    }

    for place in &op_array.places {
        let mut place_clone = place.clone();
        for_each_place_operand_mut(&mut place_clone, |operand| match operand {
            Operand::Tmp(t) | Operand::TmpKeep(t) => {
                if let Some(c) = counts.get_mut(*t as usize) {
                    *c += 1;
                }
            }
            _ => {}
        });
    }
}

fn is_pure_op(op: &Op) -> bool {
    match op {
        Op::Move { src, .. } => {
            matches!(src, Operand::Lit(_) | Operand::Tmp(_) | Operand::Cv(_))
        }
        Op::Unary { op, a, .. } => match op {
            UnKind::Bool | UnKind::Not => {
                matches!(a, Operand::Lit(_) | Operand::Tmp(_) | Operand::Cv(_))
            }
            UnKind::BitNot | UnKind::Neg | UnKind::Plus => {
                matches!(a, Operand::Lit(_) | Operand::Tmp(_))
            }
        },
        Op::Cast { kind, a, .. } => match kind {
            CastKind::Bool | CastKind::Null => {
                matches!(a, Operand::Lit(_) | Operand::Tmp(_) | Operand::Cv(_))
            }
            CastKind::Int | CastKind::Float | CastKind::String => {
                matches!(a, Operand::Lit(_) | Operand::Tmp(_))
            }
            CastKind::Array | CastKind::Object => false,
        },
        Op::Bin { op, a, b, .. } => {
            let operands_safe = matches!(a, Operand::Lit(_) | Operand::Tmp(_))
                && matches!(b, Operand::Lit(_) | Operand::Tmp(_));
            if !operands_safe {
                return false;
            }
            match op {
                BinKind::Equal
                | BinKind::NotEqual
                | BinKind::Identical
                | BinKind::NotIdentical
                | BinKind::Less
                | BinKind::LessEqual
                | BinKind::Spaceship => true,
                BinKind::Arith(bop) => !matches!(bop, BinaryOp::Div | BinaryOp::Mod),
            }
        }
        Op::TypeCheck { a, .. } => {
            matches!(a, Operand::Lit(_) | Operand::Tmp(_) | Operand::Cv(_))
        }
        Op::Strlen { a, .. } => {
            matches!(a, Operand::Lit(_) | Operand::Tmp(_) | Operand::Cv(_))
        }
        Op::Count { a, .. } => {
            matches!(a, Operand::Lit(_) | Operand::Tmp(_))
        }
        Op::InitArray { .. } => true,
        Op::Rope { parts, .. } => parts
            .iter()
            .all(|p| matches!(p, Operand::Lit(_) | Operand::Tmp(_))),
        Op::FetchClassName { class, .. } => {
            matches!(
                class,
                ClassSpec::Named(_) | ClassSpec::SelfClass | ClassSpec::Parent | ClassSpec::Static
            )
        }
        Op::Abs { src, to: None, .. } => {
            matches!(src, Operand::Lit(_) | Operand::Tmp(_) | Operand::Cv(_))
        }
        _ => false,
    }
}

fn is_pure_dead_op(op: &Op) -> bool {
    match op {
        Op::Strlen { dst: None, a } => {
            matches!(a, Operand::Lit(_) | Operand::Tmp(_) | Operand::Cv(_))
        }
        Op::TypeCheck { dst: None, a, .. } => {
            matches!(a, Operand::Lit(_) | Operand::Tmp(_) | Operand::Cv(_))
        }
        Op::Count { dst: None, a, .. } => {
            matches!(a, Operand::Lit(_) | Operand::Tmp(_))
        }
        Op::Abs {
            dst: None,
            to: None,
            src,
        } => matches!(src, Operand::Lit(_) | Operand::Tmp(_) | Operand::Cv(_)),
        Op::Free(Operand::Lit(_) | Operand::Unused) => true,
        _ => false,
    }
}

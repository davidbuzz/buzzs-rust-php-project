//! Pass 5: CFG basic block pass (ZEND_OPTIMIZER_PASS_5)
//!
//! - Basic block discovery based on branches, targets, try regions, and live ranges.
//! - Intra-block unused expression result + Free(Tmp(t)) elision.
//! - Intra-block boolean negation & branch condition simplification (Not + JmpZ -> JmpNz).
//! - Consecutive constant Echo merging (echo "a"; echo "b"; -> echo "ab";).
//! - Redundant string cast before Echo elision.

use super::{add_or_find_literal, clear_def_tmp, def_tmp, for_each_label_mut};
use crate::compiler::expr::const_truthiness;
use crate::opcode::{CastKind, Label, Op, OpArray, Operand, UnKind};
use php_runtime::{Value, ZStr};

/// Finds all basic block leaders in `op_array`.
pub(crate) fn find_leaders(op_array: &OpArray) -> Vec<bool> {
    let len = op_array.ops.len();
    let mut leaders = vec![false; len + 1];
    if len == 0 {
        return leaders;
    }
    leaders[0] = true;

    for (i, op) in op_array.ops.iter().enumerate() {
        let mut op_clone = op.clone();
        for_each_label_mut(&mut op_clone, |lbl| {
            if (*lbl as usize) <= len {
                leaders[*lbl as usize] = true;
            }
        });

        // Instructions following terminators and conditional branches are leaders
        match op {
            Op::Jmp(_)
            | Op::JmpZ(..)
            | Op::JmpNz(..)
            | Op::JmpZEx { .. }
            | Op::JmpNzEx { .. }
            | Op::JmpSet { .. }
            | Op::Coalesce { .. }
            | Op::JmpNull { .. }
            | Op::Return { .. }
            | Op::ReturnRef { .. }
            | Op::ReturnByRefValue { .. }
            | Op::Throw(_)
            | Op::Rethrow
            | Op::Exit(_)
            | Op::FastCall { .. }
            | Op::FastRet { .. }
            | Op::CmpJmp { .. } => {
                if i + 1 <= len {
                    leaders[i + 1] = true;
                }
            }
            _ => {}
        }
    }

    for entry in &op_array.try_table {
        if (entry.try_start as usize) <= len {
            leaders[entry.try_start as usize] = true;
        }
        if (entry.try_end as usize) <= len {
            leaders[entry.try_end as usize] = true;
        }
        if let Some(cs) = entry.catch_start {
            if (cs as usize) <= len {
                leaders[cs as usize] = true;
            }
        }
        if (entry.catch_end as usize) <= len {
            leaders[entry.catch_end as usize] = true;
        }
        if let Some((_, f_start, f_end)) = entry.finally {
            if (f_start as usize) <= len {
                leaders[f_start as usize] = true;
            }
            if (f_end as usize) <= len {
                leaders[f_end as usize] = true;
            }
        }
    }

    for live in &op_array.live_tmps {
        if (live.start as usize) <= len {
            leaders[live.start as usize] = true;
        }
        if (live.end as usize) <= len {
            leaders[live.end as usize] = true;
        }
    }

    leaders
}

/// Runs Pass 5 on `op_array`. Returns `true` if any instruction was modified.
pub(crate) fn run_block_pass(op_array: &mut OpArray) -> bool {
    let mut changed = false;
    let leaders = find_leaders(op_array);
    let len = op_array.ops.len();
    if len == 0 {
        return false;
    }

    // Partition instructions into basic blocks [start, end)
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

    // Jump threading past empty blocks
    let mut empty_block_dest = vec![None; num_blocks];
    for b in 0..num_blocks {
        let b_start = block_starts[b];
        let b_end = block_starts[b + 1];
        let non_nops: Vec<usize> = (b_start..b_end)
            .filter(|&i| !matches!(op_array.ops[i], Op::Nop))
            .collect();
        match non_nops.len() {
            0 => {
                if b + 1 < num_blocks {
                    empty_block_dest[b] = Some(block_starts[b + 1]);
                }
            }
            1 => {
                if let Op::Jmp(t) = op_array.ops[non_nops[0]] {
                    empty_block_dest[b] = Some(t as usize);
                }
            }
            _ => {}
        }
    }

    let follow_empty = |mut target: usize| -> usize {
        let mut hops = 0;
        while hops < 32 && target < len {
            let b = inst_to_block[target];
            if b < num_blocks {
                if let Some(next_dest) = empty_block_dest[b] {
                    if next_dest == target {
                        break;
                    }
                    target = next_dest;
                    hops += 1;
                    continue;
                }
            }
            break;
        }
        target
    };

    for i in 0..len {
        let mut op_clone = op_array.ops[i].clone();
        let mut op_modified = false;
        for_each_label_mut(&mut op_clone, |lbl| {
            let orig = *lbl as usize;
            let resolved = follow_empty(orig);
            if resolved != orig && resolved <= len {
                *lbl = resolved as Label;
                op_modified = true;
            }
        });
        if op_modified {
            op_array.ops[i] = op_clone;
            changed = true;
        }
    }

    // Eliminate jumps to next instruction
    for i in 0..len {
        let mut next_i = i + 1;
        while next_i < len && matches!(op_array.ops[next_i], Op::Nop) {
            next_i += 1;
        }
        match &op_array.ops[i] {
            Op::Jmp(t) if *t as usize == next_i => {
                op_array.ops[i] = Op::Nop;
                changed = true;
            }
            Op::JmpZ(cond, t) if *t as usize == next_i => {
                op_array.ops[i] = match cond {
                    Operand::Tmp(ft) => Op::Free(Operand::Tmp(*ft)),
                    _ => Op::Nop,
                };
                changed = true;
            }
            Op::JmpNz(cond, t) if *t as usize == next_i => {
                op_array.ops[i] = match cond {
                    Operand::Tmp(ft) => Op::Free(Operand::Tmp(*ft)),
                    _ => Op::Nop,
                };
                changed = true;
            }
            _ => {}
        }
    }

    // Compute temporary use counts across the entire op array
    let mut use_counts = vec![0u32; op_array.num_tmps as usize + 1];
    super::count_tmp_uses(op_array, &mut use_counts);

    for b in 0..num_blocks {
        let b_start = block_starts[b];
        let b_end = block_starts[b + 1];

        // 1. Unused expression result + Free(Tmp(t)) elision
        for i in b_start..b_end {
            if let Some(t) = def_tmp(&op_array.ops[i]) {
                // Find next non-Nop instruction in this basic block
                let mut next_i = i + 1;
                while next_i < b_end && matches!(op_array.ops[next_i], Op::Nop) {
                    next_i += 1;
                }
                if next_i < b_end {
                    if let Op::Free(Operand::Tmp(ft)) = op_array.ops[next_i] {
                        if ft == t && use_counts.get(t as usize).copied().unwrap_or(0) <= 1 {
                            if clear_def_tmp(&mut op_array.ops[i]) {
                                op_array.ops[next_i] = Op::Nop;
                                changed = true;
                            }
                        }
                    }
                }
            }
        }

        // 2. Known condition branch simplification
        for i in b_start..b_end {
            match op_array.ops[i] {
                Op::JmpZ(Operand::Lit(l), target) => {
                    if let Some(va) = op_array.literals.get(l as usize) {
                        if let Some(truthy) = const_truthiness(va) {
                            op_array.ops[i] = if !truthy { Op::Jmp(target) } else { Op::Nop };
                            changed = true;
                        }
                    }
                }
                Op::JmpNz(Operand::Lit(l), target) => {
                    if let Some(va) = op_array.literals.get(l as usize) {
                        if let Some(truthy) = const_truthiness(va) {
                            op_array.ops[i] = if truthy { Op::Jmp(target) } else { Op::Nop };
                            changed = true;
                        }
                    }
                }
                _ => {}
            }
        }

        // 3. Consecutive conditional jumps on the same condition
        for i in b_start..b_end {
            let (cond, is_jmpz) = match &op_array.ops[i] {
                Op::JmpZ(c, _) => (*c, true),
                Op::JmpNz(c, _) => (*c, false),
                _ => continue,
            };
            let mut next_i = i + 1;
            while next_i < b_end && matches!(op_array.ops[next_i], Op::Nop) {
                next_i += 1;
            }
            if next_i >= b_end {
                continue;
            }
            match &op_array.ops[next_i] {
                Op::JmpZ(c2, target) if *c2 == cond => {
                    if is_jmpz {
                        op_array.ops[next_i] = Op::Nop;
                    } else {
                        op_array.ops[next_i] = Op::Jmp(*target);
                    }
                    changed = true;
                }
                Op::JmpNz(c2, target) if *c2 == cond => {
                    if is_jmpz {
                        op_array.ops[next_i] = Op::Jmp(*target);
                    } else {
                        op_array.ops[next_i] = Op::Nop;
                    }
                    changed = true;
                }
                _ => {}
            }
        }

        // 4. Boolean negation and inverted branch simplification
        for i in b_start..b_end {
            let mut next_i = i + 1;
            while next_i < b_end && matches!(op_array.ops[next_i], Op::Nop) {
                next_i += 1;
            }
            if next_i >= b_end {
                continue;
            }

            match (&op_array.ops[i], &op_array.ops[next_i]) {
                (
                    Op::Unary {
                        op: UnKind::Not,
                        a,
                        dst: t1,
                    },
                    Op::JmpZ(Operand::Tmp(t2), target),
                ) if t1 == t2 && use_counts.get(*t1 as usize).copied().unwrap_or(0) <= 1 => {
                    let new_jmp = Op::JmpNz(*a, *target);
                    op_array.ops[next_i] = new_jmp;
                    op_array.ops[i] = Op::Nop;
                    changed = true;
                }

                (
                    Op::Unary {
                        op: UnKind::Not,
                        a,
                        dst: t1,
                    },
                    Op::JmpNz(Operand::Tmp(t2), target),
                ) if t1 == t2 && use_counts.get(*t1 as usize).copied().unwrap_or(0) <= 1 => {
                    let new_jmp = Op::JmpZ(*a, *target);
                    op_array.ops[next_i] = new_jmp;
                    op_array.ops[i] = Op::Nop;
                    changed = true;
                }

                (
                    Op::Unary {
                        op: UnKind::Bool,
                        a,
                        dst: t1,
                    },
                    Op::JmpZ(Operand::Tmp(t2), target),
                ) if t1 == t2 && use_counts.get(*t1 as usize).copied().unwrap_or(0) <= 1 => {
                    let new_jmp = Op::JmpZ(*a, *target);
                    op_array.ops[next_i] = new_jmp;
                    op_array.ops[i] = Op::Nop;
                    changed = true;
                }

                (
                    Op::Unary {
                        op: UnKind::Bool,
                        a,
                        dst: t1,
                    },
                    Op::JmpNz(Operand::Tmp(t2), target),
                ) if t1 == t2 && use_counts.get(*t1 as usize).copied().unwrap_or(0) <= 1 => {
                    let new_jmp = Op::JmpNz(*a, *target);
                    op_array.ops[next_i] = new_jmp;
                    op_array.ops[i] = Op::Nop;
                    changed = true;
                }

                (
                    Op::Unary {
                        op: UnKind::Not,
                        a,
                        dst: t1,
                    },
                    Op::JmpZEx {
                        a: Operand::Tmp(t2),
                        dst: d,
                        target,
                    },
                ) if t1 == t2 && use_counts.get(*t1 as usize).copied().unwrap_or(0) <= 1 => {
                    let new_jmp = Op::JmpNzEx {
                        a: *a,
                        dst: *d,
                        target: *target,
                    };
                    op_array.ops[next_i] = new_jmp;
                    op_array.ops[i] = Op::Nop;
                    changed = true;
                }

                (
                    Op::Unary {
                        op: UnKind::Not,
                        a,
                        dst: t1,
                    },
                    Op::JmpNzEx {
                        a: Operand::Tmp(t2),
                        dst: d,
                        target,
                    },
                ) if t1 == t2 && use_counts.get(*t1 as usize).copied().unwrap_or(0) <= 1 => {
                    let new_jmp = Op::JmpZEx {
                        a: *a,
                        dst: *d,
                        target: *target,
                    };
                    op_array.ops[next_i] = new_jmp;
                    op_array.ops[i] = Op::Nop;
                    changed = true;
                }

                (
                    Op::Move {
                        src: Operand::Lit(l),
                        dst: t1,
                    },
                    Op::JmpZ(Operand::Tmp(t2), target),
                ) if t1 == t2 && use_counts.get(*t1 as usize).copied().unwrap_or(0) <= 1 => {
                    if let Some(va) = op_array.literals.get(*l as usize) {
                        if let Some(truthy) = const_truthiness(va) {
                            op_array.ops[next_i] = if !truthy { Op::Jmp(*target) } else { Op::Nop };
                            op_array.ops[i] = Op::Nop;
                            changed = true;
                        }
                    }
                }

                (
                    Op::Move {
                        src: Operand::Lit(l),
                        dst: t1,
                    },
                    Op::JmpNz(Operand::Tmp(t2), target),
                ) if t1 == t2 && use_counts.get(*t1 as usize).copied().unwrap_or(0) <= 1 => {
                    if let Some(va) = op_array.literals.get(*l as usize) {
                        if let Some(truthy) = const_truthiness(va) {
                            op_array.ops[next_i] = if truthy { Op::Jmp(*target) } else { Op::Nop };
                            op_array.ops[i] = Op::Nop;
                            changed = true;
                        }
                    }
                }

                (
                    Op::Unary {
                        op: UnKind::Not,
                        a,
                        dst: t1,
                    },
                    Op::Unary {
                        op: UnKind::Not,
                        a: Operand::Tmp(t2),
                        dst: t3,
                    },
                ) if t1 == t2 && use_counts.get(*t1 as usize).copied().unwrap_or(0) <= 1 => {
                    let new_unary = Op::Unary {
                        op: UnKind::Bool,
                        a: *a,
                        dst: *t3,
                    };
                    op_array.ops[next_i] = new_unary;
                    op_array.ops[i] = Op::Nop;
                    changed = true;
                }

                (
                    Op::Cast {
                        kind: CastKind::String,
                        a,
                        dst: t1,
                    },
                    Op::Echo(Operand::Tmp(t2)),
                ) if t1 == t2 && use_counts.get(*t1 as usize).copied().unwrap_or(0) <= 1 => {
                    let new_echo = Op::Echo(*a);
                    op_array.ops[next_i] = new_echo;
                    op_array.ops[i] = Op::Nop;
                    changed = true;
                }

                _ => {}
            }
        }

        // 5. Consecutive string literal Echo merging
        for i in b_start..b_end {
            if let Op::Echo(Operand::Lit(l1)) = op_array.ops[i] {
                let mut next_i = i + 1;
                while next_i < b_end && matches!(op_array.ops[next_i], Op::Nop) {
                    next_i += 1;
                }
                if next_i < b_end {
                    if let Op::Echo(Operand::Lit(l2)) = op_array.ops[next_i] {
                        let s1_opt = match op_array.literals.get(l1 as usize) {
                            Some(Value::Str(s)) => Some(s.clone()),
                            _ => None,
                        };
                        let s2_opt = match op_array.literals.get(l2 as usize) {
                            Some(Value::Str(s)) => Some(s.clone()),
                            _ => None,
                        };
                        if let (Some(s1), Some(s2)) = (s1_opt, s2_opt) {
                            let mut merged = s1.as_bytes().to_vec();
                            merged.extend_from_slice(s2.as_bytes());
                            let lit = add_or_find_literal(
                                &mut op_array.literals,
                                Value::Str(ZStr::from(merged)),
                            );
                            op_array.ops[i] = Op::Echo(Operand::Lit(lit));
                            op_array.ops[next_i] = Op::Nop;
                            changed = true;
                        }
                    }
                }
            }
        }
    }

    changed
}

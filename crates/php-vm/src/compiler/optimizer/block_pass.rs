// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Pass 5: CFG basic block pass (ZEND_OPTIMIZER_PASS_5)
//!
//! - Basic block discovery based on branches, targets, try regions, and live ranges.
//! - Intra-block unused expression result + Free(Tmp(t)) elision.
//! - Intra-block boolean negation & branch condition simplification (Not + JmpZ -> JmpNz).
//! - Consecutive constant Echo merging (echo "a"; echo "b"; -> echo "ab";).
//! - Redundant string cast before Echo elision.
//! - Unused `static` variables with a constant initializer: their
//!   `BindStatic`/`InitStatic` go (php-src's DCE drops such a `BIND_STATIC`).

use super::{add_or_find_literal, clear_def_tmp, def_tmp, for_each_label_mut};
use crate::compiler::expr::const_truthiness;
use crate::opcode::{BinKind, CastKind, Label, Op, OpArray, Operand, UnKind};
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
        let mut branches = false;
        for_each_label_mut(&mut op_clone, |lbl| {
            branches = true;
            if (*lbl as usize) <= len {
                leaders[*lbl as usize] = true;
            }
        });
        // Every op with a jump target ends its block (zend_build_cfg ends a
        // block after CATCH, FE_RESET, FE_FETCH, JMP_SET, ...): the passes
        // read a block's successors from its last op only, so a `Catch`
        // followed by its body's `Jmp` in one block would hide the edge to
        // the next catch clause and its `Rethrow` (jit/assign_055).
        if branches {
            leaders[i + 1] = true;
        }

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
            | Op::CmpJmp { .. }
            | Op::BinAssignCvJmp { .. }
            | Op::AssignOpCvJmp { .. } => {
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
/// `merge_echoes`: also merge consecutive literal `ECHO`s (only on the
/// pass that stands for php-src's single block pass).
pub(crate) fn run_block_pass(op_array: &mut OpArray, merge_echoes: bool) -> bool {
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
            Op::JmpZ(cond, t) | Op::JmpNz(cond, t) if *t as usize == next_i => {
                op_array.ops[i] = super::pass3::drop_condition(*cond);
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
                // A jump that provably falls through without writing its
                // result (SCCP's infeasible edge, `sccp.c`): `JMP_SET` of a
                // falsy literal, `COALESCE` of null, `JMP_NULL` of non-null.
                Op::JmpSet {
                    a: Operand::Lit(l), ..
                } => {
                    if op_array.literals.get(l as usize).and_then(const_truthiness) == Some(false) {
                        op_array.ops[i] = Op::Nop;
                        changed = true;
                    }
                }
                Op::Coalesce {
                    a: Operand::Lit(l), ..
                } => {
                    if matches!(op_array.literals.get(l as usize), Some(Value::Null)) {
                        op_array.ops[i] = Op::Nop;
                        changed = true;
                    }
                }
                Op::JmpNull {
                    a: Operand::Lit(l), ..
                } => {
                    if op_array
                        .literals
                        .get(l as usize)
                        .is_some_and(|v| !v.is_null())
                    {
                        op_array.ops[i] = Op::Nop;
                        changed = true;
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

                // (`!` in front of `JMPZ_EX`/`JMPNZ_EX` stays: the jump keeps
                // its condition as its result, which would be `(bool) X`
                // where every reader expects `!X`. php-src folds `BOOL_NOT`
                // only into the jumps without a result.)
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

        // 4b. `T = BOOL/BOOL_NOT(X)` + `IS_IDENTICAL(T, true/false)`
        changed |= fold_bool_identical(op_array, b_start, b_end, &use_counts);

        // 4c. `T1 = OP ...; T2 = QM_ASSIGN T1` -> `T2 = OP ...`
        changed |= fold_tmp_copy(op_array, b_start, b_end, &use_counts);

        // 5. Consecutive string literal Echo merging
        // A whole run in one go (`echo 'a'; echo 'b'; echo 'c';`): each
        // merge leaves the next echo of the run after `i`.
        for i in (b_start..b_end).filter(|_| merge_echoes) {
            while let Op::Echo(Operand::Lit(l1)) = op_array.ops[i] {
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
                            continue;
                        }
                    }
                }
                break;
            }
        }
    }

    changed
}

/// Folds `T = BOOL(X)`/`T = BOOL_NOT(X)` followed by `IS_IDENTICAL` of `T`
/// with a `true`/`false` literal (either side) into one `BOOL`/`BOOL_NOT`
/// of `X`, like php-src's block pass (`block_pass.c`: `IS_IDENTICAL(T,
/// TRUE)` becomes `TYPE_CHECK(T, TRUE)`, which absorbs a `BOOL`/`BOOL_NOT`
/// source). `T` is a bool, so `T === true` is `T` and `T === false` is
/// `!T`: the fold is exact. `T` must have no other use.
fn fold_bool_identical(
    op_array: &mut OpArray,
    b_start: usize,
    b_end: usize,
    use_counts: &[u32],
) -> bool {
    let mut changed = false;
    for i in b_start..b_end {
        let Op::Unary {
            op: src_op @ (UnKind::Bool | UnKind::Not),
            a: x,
            dst: t,
        } = op_array.ops[i]
        else {
            continue;
        };
        if use_counts.get(t as usize).copied().unwrap_or(0) > 1 {
            continue;
        }
        let mut next_i = i + 1;
        while next_i < b_end && matches!(op_array.ops[next_i], Op::Nop) {
            next_i += 1;
        }
        if next_i >= b_end {
            continue;
        }
        let Op::Bin {
            op: BinKind::Identical,
            a,
            b,
            dst,
        } = op_array.ops[next_i]
        else {
            continue;
        };
        let lit = match (a, b) {
            (Operand::Tmp(u), Operand::Lit(l)) | (Operand::Lit(l), Operand::Tmp(u)) if u == t => l,
            _ => continue,
        };
        let want = match op_array.literals.get(lit as usize) {
            Some(Value::Bool(v)) => *v,
            _ => continue,
        };
        let op = if (src_op == UnKind::Bool) == want {
            UnKind::Bool
        } else {
            UnKind::Not
        };
        op_array.ops[next_i] = Op::Unary { op, a: x, dst };
        op_array.ops[i] = Op::Nop;
        changed = true;
    }
    changed
}

/// The result slot of an op that defines its temporary exactly once and
/// only by computing it (no jump, rope, array build, `NEW` or silence
/// token), so the op may write another temporary instead.
fn plain_def_dst_mut(op: &mut Op) -> Option<&mut u32> {
    match op {
        Op::Bin { dst, .. }
        | Op::Unary { dst, .. }
        | Op::Cast { dst, .. }
        | Op::Isset { dst, .. }
        | Op::Empty { dst, .. } => Some(dst),
        _ => None,
    }
}

/// Whether op `at` reads temporary `t` (an operand or one of its place's).
fn reads_tmp(op_array: &OpArray, at: usize, t: u32) -> bool {
    let mut hit = false;
    let mut check = |o: &mut Operand| {
        if matches!(o, Operand::Tmp(u) | Operand::TmpKeep(u) if *u == t) {
            hit = true;
        }
    };
    let mut op = op_array.ops[at].clone();
    super::for_each_operand_mut(&mut op, &mut check);
    if let Op::Isset { place, .. } | Op::Empty { place, .. } = op {
        if let Some(p) = op_array.places.get(place as usize) {
            super::for_each_place_operand_mut(&mut p.clone(), &mut check);
        }
    }
    hit
}

/// `T1 = OP ...` directly followed by `T2 = QM_ASSIGN T1`, with `T1`
/// defined and used nowhere else, becomes `T2 = OP ...` (php-src
/// `block_pass.c`, `ZEND_QM_ASSIGN`: "T1 = ..., T2 = QM_ASSIGN(T1) to
/// T2 = ..., NOP").
fn fold_tmp_copy(op_array: &mut OpArray, b_start: usize, b_end: usize, use_counts: &[u32]) -> bool {
    let mut changed = false;
    for i in b_start..b_end {
        let Some(t1) = plain_def_dst_mut(&mut op_array.ops[i]).map(|d| *d) else {
            continue;
        };
        if use_counts.get(t1 as usize).copied().unwrap_or(0) != 1 {
            continue;
        }
        let mut next_i = i + 1;
        while next_i < b_end && matches!(op_array.ops[next_i], Op::Nop) {
            next_i += 1;
        }
        if next_i >= b_end {
            continue;
        }
        let Op::Move {
            src: Operand::Tmp(src),
            dst: t2,
        } = op_array.ops[next_i]
        else {
            continue;
        };
        if src != t1 || t2 == t1 {
            continue;
        }
        let defs = op_array
            .ops
            .iter()
            .filter(|op| super::def_tmp(op) == Some(t1))
            .count();
        if defs != 1 || reads_tmp(op_array, i, t2) {
            continue;
        }
        if let Some(d) = plain_def_dst_mut(&mut op_array.ops[i]) {
            *d = t2;
        }
        op_array.ops[next_i] = Op::Nop;
        changed = true;
    }
    changed
}

/// Deletes the `BindStatic`/`InitStatic` pair of a `static $v = <const>;`
/// whose variable nothing else in the function touches, like php-src's DCE
/// (`dce.c`, `may_have_side_effects()` on `ZEND_BIND_STATIC`): with the
/// value in `op_array->static_variables` (op2 unused) the bind only defines
/// the CV, so it dies with an unused CV. The declaration stays in
/// [`OpArray::static_vars`], which reflection reads, so the variable shows
/// its initial value, not a reference (opcache `optimize_static_002`).
///
/// Like php-src, which builds no SSA for such functions
/// (`zend_dfa_analyze_op_array`), nothing is removed from code with
/// try/catch/finally or indirect variable access (`$$`, `compact()`,
/// `extract()`, `get_defined_vars()`, include/eval, `$GLOBALS`), nor from a
/// variable a parameter, closure `use` binding or nested closure captures.
pub(crate) fn remove_unused_const_statics(op_array: &mut OpArray, bound_cv_prefix: u32) -> bool {
    if op_array.num_statics == 0
        || !op_array.try_table.is_empty()
        || super::compact::has_dynamic_var_access(op_array)
    {
        return false;
    }
    // (bind op, init op, cv, literal) per static slot; `None` once a slot
    // is seen in an unexpected shape.
    let mut slots: Vec<Option<(Option<usize>, Option<usize>, u32, Option<u32>)>> =
        vec![Some((None, None, u32::MAX, None)); op_array.num_statics as usize];
    for (at, op) in op_array.ops.iter().enumerate() {
        let (cv, idx, init) = match op {
            Op::BindStatic { cv, idx, .. } => (*cv, *idx, None),
            Op::InitStatic { cv, idx, val } => (*cv, *idx, Some(*val)),
            _ => continue,
        };
        let Some(slot) = slots.get_mut(idx as usize) else {
            continue;
        };
        let ok = match slot.as_mut() {
            None => continue,
            Some((_, _, scv, _)) if *scv != u32::MAX && *scv != cv => false,
            Some((bind, init_at, scv, lit)) => {
                *scv = cv;
                match init {
                    None if bind.is_none() => {
                        *bind = Some(at);
                        true
                    }
                    Some(Operand::Lit(l)) if init_at.is_none() => {
                        *init_at = Some(at);
                        *lit = Some(l);
                        true
                    }
                    _ => false,
                }
            }
        };
        if !ok {
            *slot = None;
        }
    }
    let candidates: Vec<(u32, usize, usize, u32, u32)> = slots
        .iter()
        .zip(0u32..)
        .filter_map(|(s, idx)| match s {
            Some((Some(b), Some(i), cv, Some(l))) if *cv >= bound_cv_prefix => {
                Some((idx, *b, *i, *cv, *l))
            }
            _ => None,
        })
        .collect();
    if candidates.is_empty() {
        return false;
    }
    // Every CV reference outside the candidates' own ops.
    let own: std::collections::HashSet<usize> =
        candidates.iter().flat_map(|c| [c.1, c.2]).collect();
    let mut used = vec![false; op_array.cv_names.len()];
    let mut mark = |cv: &mut u32| {
        if let Some(u) = used.get_mut(*cv as usize) {
            *u = true;
        }
    };
    for (at, op) in op_array.ops.iter().enumerate() {
        if !own.contains(&at) {
            super::compact::for_each_cv_mut(&mut op.clone(), &mut mark);
        }
    }
    for place in &op_array.places {
        super::compact::for_each_place_cv_mut(&mut place.clone(), &mut mark);
    }
    for f in op_array.decls.functions.borrow().iter() {
        for u in &f.uses {
            mark(&mut u.parent_cv.clone());
        }
    }
    let mut changed = false;
    for (idx, bind, init, cv, lit) in candidates {
        if used.get(cv as usize).copied().unwrap_or(true) {
            continue;
        }
        let Some(v) = op_array.literals.get(lit as usize).cloned() else {
            continue;
        };
        let Some(d) = op_array.static_vars.get_mut(idx as usize) else {
            continue;
        };
        d.init = Some(v);
        op_array.ops[bind] = Op::Nop;
        op_array.ops[init] = Op::Nop;
        changed = true;
    }
    changed
}

/// `zend_jmp_optimization`'s "JMP L, L: RETURN to immediate RETURN": a
/// `Jmp` to a block that holds only a `Return` becomes a copy of that
/// return; then a `Move` of a literal into the temporary a `Return`
/// returns right before it in its block is folded into the return (the
/// block pass's `QM_ASSIGN` of a constant replaced in its use). Not run in
/// a function with a `finally` (its returns run it) nor in one whose
/// returns check a declared type (php-src's `VERIFY_RETURN_TYPE` before the
/// `RETURN` makes that block two ops long; the VM folds it into `Return`).
pub(crate) fn jmp_to_return(op_array: &mut OpArray) -> bool {
    if op_array.try_table.iter().any(|e| e.finally.is_some()) {
        return false;
    }
    let len = op_array.ops.len();
    let leaders = find_leaders(op_array);
    let mut block_start = vec![0usize; len + 1];
    let mut block_end = vec![len; len + 1];
    let mut start = 0;
    for i in 0..len {
        if leaders[i] {
            start = i;
        }
        block_start[i] = start;
    }
    let mut end = len;
    for i in (0..len).rev() {
        block_end[i] = end;
        if leaders[i] {
            end = i;
        }
    }
    let mut changed = false;
    for i in 0..len {
        let Op::Jmp(t) = op_array.ops[i] else {
            continue;
        };
        let t = t as usize;
        if t >= len || block_start[t] != t {
            continue;
        }
        let mut live = (t..block_end[t]).filter(|&k| !matches!(op_array.ops[k], Op::Nop));
        if let (Some(k), None) = (live.next(), live.next()) {
            if matches!(op_array.ops[k], Op::Return { .. }) {
                op_array.ops[i] = op_array.ops[k].clone();
                changed = true;
            }
        }
    }
    for i in 0..len {
        let Op::Return {
            val: Operand::Tmp(t),
        } = op_array.ops[i]
        else {
            continue;
        };
        // The op before it in its block, skipping NOPs.
        let Some(p) = (block_start[i]..i)
            .rev()
            .find(|&k| !matches!(op_array.ops[k], Op::Nop))
        else {
            continue;
        };
        if let Op::Move {
            src: src @ Operand::Lit(_),
            dst,
        } = op_array.ops[p]
        {
            if dst == t {
                op_array.ops[p] = Op::Nop;
                op_array.ops[i] = Op::Return { val: src };
                changed = true;
            }
        }
    }
    changed
}

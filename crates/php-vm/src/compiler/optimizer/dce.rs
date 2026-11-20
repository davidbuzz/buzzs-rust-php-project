// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Pass 14: DCE (Dead Code Elimination) (ZEND_OPTIMIZER_PASS_14)
//!
//! - Basic block CFG reachability: eliminates dead blocks following unconditional
//!   terminators or unexecutable branches.
//! - Elimination of side-effect-free instructions whose result temporary is never used.
//! - Clearing unused `dst` on side-effecting instructions (AssignCv, AssignOp, DoCall...).
//! - Redundant `Free(Tmp)` removal for dead temporaries.
//! - Dead jump-to-next elimination.

use super::{clear_def_tmp, def_tmp, for_each_operand_mut, for_each_place_operand_mut};
use crate::opcode::{BinKind, CastKind, ClassSpec, ForeachKind, Op, OpArray, Operand, PlaceBase};
use php_runtime::{BinaryOp, Value};

/// Runs DCE on `op_array`. Returns `true` if any instruction was removed or simplified.
pub(crate) fn run_dce(op_array: &mut OpArray, ctx: &super::OptContext<'_>) -> bool {
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
                | Op::BinAssignCvJmp { target: t, .. }
                | Op::AssignOpCvJmp { target: t, .. }
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
                // A foreach over an empty literal array goes straight to
                // its end: sccp.c marks only the `FE_RESET` target
                // feasible (opt/dce_009).
                // By reference the literal is the place's base value.
                Op::FeReset {
                    subject,
                    place,
                    end: t,
                    ..
                } if foreach_over_empty_lit(op_array, *subject, *place) => {
                    if (*t as usize) < len {
                        successors.push(inst_to_block[*t as usize]);
                    }
                }
                Op::FastCall { target: t, .. }
                | Op::Catch { next: t, .. }
                | Op::FeReset { end: t, .. }
                | Op::FeFetch { end: t, .. }
                | Op::FeFreeOnReturn { loop_at: t, .. }
                | Op::AssertCheck { skip: t, .. }
                | Op::BindStatic { skip: t, .. }
                | Op::RecvInit { skip: t, .. } => {
                    if (*t as usize) < len {
                        successors.push(inst_to_block[*t as usize]);
                    }
                    if b + 1 < num_blocks {
                        successors.push(b + 1);
                    }
                }
                Op::Match { table, .. } => {
                    // No fall-through: every table target and the default.
                    for t in table.labels() {
                        if (t as usize) < len {
                            successors.push(inst_to_block[t as usize]);
                        }
                    }
                }
                Op::New { skip: t, .. } => {
                    if *t != 0 && (*t as usize) < len {
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

    changed |= drop_empty_foreach(op_array);
    changed |= drop_param_check_vars(op_array);

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
    let scalar_cvs = if super::compact::has_dynamic_var_access(op_array) {
        vec![false; op_array.cv_names.len()]
    } else {
        super::compact::stable_scalar_params(op_array, ctx)
    };
    let assigned = Assigned::of(op_array, ctx);
    for _ in 0..8 {
        let mut inner_changed = false;
        let mut non_free_uses = vec![0u32; num_tmps + 1];
        count_non_free_uses(op_array, &mut non_free_uses);
        let mut defs = vec![0u32; num_tmps + 1];
        count_defs(op_array, &mut defs);
        let mut frees = vec![0u32; num_tmps + 1];
        for op in &op_array.ops {
            if let Op::Free(Operand::Tmp(t)) = op {
                if let Some(c) = frees.get_mut(*t as usize) {
                    *c += 1;
                }
            }
        }
        let known = Scalars::of(op_array, &scalar_cvs, &defs);
        if drop_dead_array_builds(op_array, &assigned, &non_free_uses, &defs)
            || drop_dead_pair(op_array, &known, &non_free_uses, &defs, &frees)
        {
            // The counts are stale: the next round takes it from here.
            changed = true;
            continue;
        }

        let literals = &op_array.literals;
        for op in &mut op_array.ops {
            if let Some(t) = def_tmp(op) {
                let uses = non_free_uses.get(t as usize).copied().unwrap_or(0);
                // A side-effecting op's result that several `FREE`s release
                // (a `switch` subject freed on each way out) stays, FREEs
                // included, unless it is never refcounted: php-src drops a
                // result only with its single `FREE` (block_pass.c), and
                // dce.c keeps the FREE of a live refcounted value
                // (opt/gh11245_2: `switch (++X::$prop)`).
                if uses == 0
                    && !known.pure(literals, op)
                    && frees.get(t as usize).copied().unwrap_or(0) > 1
                    && !result_never_refcounted(op)
                {
                    continue;
                }
                if uses == 0 {
                    let rest = if known.pure(literals, op) {
                        leftover_of(op)
                    } else {
                        None
                    };
                    if let Some(rest) = rest {
                        *op = rest;
                        inner_changed = true;
                    } else if clear_def_tmp(op) {
                        inner_changed = true;
                    }
                }
                continue;
            }
            let rest = if is_pure_dead_op(op) {
                leftover_of(op)
            } else {
                None
            };
            if let Some(rest) = rest {
                *op = rest;
                inner_changed = true;
            } else if let Op::Free(Operand::Tmp(t)) = op {
                // The `FREE` of a value an op still produces (`clone $a;`,
                // `new C;`, `include`) is what releases it: without it the
                // temporary keeps an object alive and its destructor runs
                // late. It goes only once no op defines the temporary.
                let uses = non_free_uses.get(*t as usize).copied().unwrap_or(0);
                let defined = defs.get(*t as usize).copied().unwrap_or(0);
                if uses == 0 && defined == 0 {
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

/// Whether a foreach's subject is an empty literal array: by value the
/// operand, by reference (`FE_RESET_RW`) the base value of its place, with
/// no dimension or property after it.
fn foreach_over_empty_lit(op_array: &OpArray, subject: Operand, place: Option<u32>) -> bool {
    let l = match (subject, place) {
        (Operand::Lit(l), None) => l,
        (Operand::Unused, Some(p)) => match op_array.places.get(p as usize) {
            Some(pl) if pl.segs.is_empty() => match pl.base {
                PlaceBase::Value(Operand::Lit(l)) => l,
                _ => return false,
            },
            _ => return false,
        },
        _ => return false,
    };
    is_empty_array_lit(&op_array.literals, l)
}

/// Whether literal `l` is an empty array.
fn is_empty_array_lit(literals: &[Value], l: u32) -> bool {
    matches!(literals.get(l as usize), Some(Value::Array(a)) if a.is_empty())
}

/// Drops a by-value foreach over an empty literal array once its loop body
/// is gone: the `FE_RESET_R` of an immutable array and its `FE_FREE` do
/// nothing (opt/dce_009). The iterator must be named by no other op (no
/// `FE_FETCH` left, no `FE_FREE` on a `return`). A by-reference
/// `FE_RESET_RW` stays, as in php-src.
fn drop_empty_foreach(op_array: &mut OpArray) -> bool {
    let mut changed = false;
    for i in 0..op_array.ops.len() {
        let Op::FeReset {
            subject: Operand::Lit(l),
            place: None,
            kind: ForeachKind::Value,
            iter,
            ..
        } = op_array.ops[i]
        else {
            continue;
        };
        if !is_empty_array_lit(&op_array.literals, l) {
            continue;
        }
        let only_freed = op_array.ops.iter().enumerate().all(|(j, op)| {
            j == i
                || !matches!(
                    op,
                    Op::FeReset { iter: x, .. }
                    | Op::FeFetch { iter: x, .. }
                    | Op::FeFreeOnReturn { iter: x, .. } if *x == iter
                )
        });
        if !only_freed {
            continue;
        }
        op_array.ops[i] = Op::Nop;
        for op in &mut op_array.ops {
            if matches!(op, Op::FeFree { iter: x } if *x == iter) {
                *op = Op::Nop;
            }
        }
        changed = true;
    }
    changed
}

/// Drops the `CHECK_VAR` (here `Free` of a CV) of a parameter after its
/// `RECV`: it cannot be undefined, so the check warns about nothing
/// (dce.c drops a `CHECK_VAR` whose operand is not `MAY_BE_UNDEF`;
/// opt/sccp_007). Not when the function unsets the parameter, nor with
/// dynamic variable access (`$$`, `extract()`, `include`, ...).
fn drop_param_check_vars(op_array: &mut OpArray) -> bool {
    if !op_array
        .ops
        .iter()
        .any(|op| matches!(op, Op::Free(Operand::Cv(_))))
        || super::compact::has_dynamic_var_access(op_array)
    {
        return false;
    }
    // The op after which each parameter is bound.
    let mut bound_at = vec![None; op_array.cv_names.len()];
    for (i, op) in op_array.ops.iter().enumerate() {
        if let Op::Recv { cv, .. } | Op::RecvInit { cv, .. } | Op::RecvVariadic { cv, .. } = op {
            if let Some(slot) = bound_at.get_mut(*cv as usize) {
                slot.get_or_insert(i);
            }
        }
    }
    for op in &op_array.ops {
        if let Op::Unset { place } = op {
            if let Some(p) = op_array.places.get(*place as usize) {
                if let crate::opcode::PlaceBase::Cv(c) = p.base {
                    if p.segs.is_empty() {
                        if let Some(slot) = bound_at.get_mut(c as usize) {
                            *slot = None;
                        }
                    }
                }
            }
        }
    }
    let mut changed = false;
    for (i, op) in op_array.ops.iter_mut().enumerate() {
        if let Op::Free(Operand::Cv(c)) = *op {
            if bound_at
                .get(c as usize)
                .copied()
                .flatten()
                .is_some_and(|at| at < i)
            {
                *op = Op::Nop;
                changed = true;
            }
        }
    }
    changed
}

/// Removes the store of a literal into a variable that holds nothing yet
/// and is overwritten before anything reads it (`$v = null; $v = f();`,
/// opt/jmp_001): php-src's dce.c drops an `ASSIGN` whose defined SSA
/// variable is dead. Kept narrow, as this IR has no SSA: the store must be
/// the first op of the function to mention the variable, with no jump
/// target up to it (so the old value is undefined: no destructor is
/// delayed), and the next mention of the variable, in the same straight
/// line of code, must be a whole assignment that does not read it. Not in
/// top-level code, nor with references, dynamic variable access
/// (`compact()`, `extract()`, `get_defined_vars()`, `include`), a
/// try/catch/finally (a handler sees the variable), or for a variable that
/// is bound (parameter, closure `use`), captured by a closure or named by
/// any place.
pub(crate) fn remove_dead_entry_stores(
    op_array: &mut OpArray,
    ctx: &super::OptContext<'_>,
    refs_possible: bool,
) -> bool {
    let num_cvs = op_array.cv_names.len();
    if ctx.is_main
        || refs_possible
        || num_cvs == 0
        || !op_array.try_table.is_empty()
        || super::compact::has_dynamic_var_access(op_array)
    {
        return false;
    }
    let pinned = pinned_cvs(op_array, ctx);
    let len = op_array.ops.len();
    let mut is_target = vec![false; len + 1];
    for op in &op_array.ops {
        let mut op = op.clone();
        super::for_each_label_mut(&mut op, |l| {
            if let Some(slot) = is_target.get_mut(*l as usize) {
                *slot = true;
            }
        });
    }
    let mentions = |op: &Op| {
        let mut cvs = Vec::new();
        let mut op = op.clone();
        super::compact::for_each_cv_mut(&mut op, |c| cvs.push(*c));
        cvs
    };

    let mut changed = false;
    let mut seen = vec![false; num_cvs];
    let mut straight = true;
    for s in 0..len {
        straight &= !is_target[s];
        let cvs = mentions(&op_array.ops[s]);
        if straight {
            if let Op::AssignCv {
                cv,
                val: Operand::Lit(_),
                dst: None,
            } = op_array.ops[s]
            {
                let c = cv as usize;
                if c < num_cvs
                    && !seen[c]
                    && !pinned[c]
                    && overwritten_before_read(op_array, s + 1, cv, &is_target, &mentions)
                {
                    op_array.ops[s] = Op::Nop;
                    changed = true;
                    continue;
                }
            }
        }
        for c in cvs {
            if let Some(slot) = seen.get_mut(c as usize) {
                *slot = true;
            }
        }
    }
    changed
}

/// The variables that may not lose a store: the bound ones (parameters,
/// a closure's `use`s), the ones a closure declared here captures and the
/// ones a place names.
fn pinned_cvs(op_array: &OpArray, ctx: &super::OptContext<'_>) -> Vec<bool> {
    let mut pinned = vec![false; op_array.cv_names.len()];
    for slot in pinned.iter_mut().take(ctx.bound_cv_prefix() as usize) {
        *slot = true;
    }
    for f in op_array.decls.functions.borrow().iter() {
        for u in &f.uses {
            if let Some(slot) = pinned.get_mut(u.parent_cv as usize) {
                *slot = true;
            }
        }
    }
    for place in &op_array.places {
        let mut place = place.clone();
        super::compact::for_each_place_cv_mut(&mut place, |c| {
            if let Some(slot) = pinned.get_mut(*c as usize) {
                *slot = true;
            }
        });
    }
    pinned
}

/// Removes each store of a literal `at` (op index, variable) that no op
/// running after it names: php-src's dce.c drops an `ASSIGN` whose defined
/// SSA variable is dead (opt/coalesce: `$test = $test ?? true; return
/// $test;` keeps no store once the `return` has its literal). The caller
/// (SCCP's variable analysis: a function body without references,
/// try/catch/finally or dynamic variable access) vouches that the
/// variable holds nothing at the store, so no destructor is delayed.
pub(crate) fn drop_unread_first_stores(
    op_array: &mut OpArray,
    ctx: &super::OptContext<'_>,
    at: &[(usize, u32)],
) -> bool {
    if at.is_empty() {
        return false;
    }
    let pinned = pinned_cvs(op_array, ctx);
    let mut changed = false;
    for &(i, cv) in at {
        let is_store = matches!(
            op_array.ops.get(i),
            Some(Op::AssignCv {
                cv: c,
                val: Operand::Lit(_),
                dst: None,
            }) if *c == cv
        );
        if is_store && pinned.get(cv as usize) == Some(&false) && !named_after(op_array, i, cv) {
            op_array.ops[i] = Op::Nop;
            changed = true;
        }
    }
    changed
}

/// Whether an op that can run after the op at `from` (which goes on to
/// the next op) names `cv`: every jump target and, unless the op leaves
/// for good, the op after it are followed.
fn named_after(op_array: &OpArray, from: usize, cv: u32) -> bool {
    let mut seen = vec![false; op_array.ops.len()];
    let mut work = vec![from + 1];
    while let Some(i) = work.pop() {
        let Some(op) = op_array.ops.get(i) else {
            continue;
        };
        if std::mem::replace(&mut seen[i], true) {
            continue;
        }
        let mut copy = op.clone();
        let mut names = false;
        super::compact::for_each_cv_mut(&mut copy, |c| names |= *c == cv);
        if names {
            return true;
        }
        super::for_each_label_mut(&mut copy, |l| work.push(*l as usize));
        if !matches!(
            op,
            Op::Jmp(_)
                | Op::Return { .. }
                | Op::ReturnRef { .. }
                | Op::ReturnByRefValue { .. }
                | Op::Throw(_)
                | Op::Rethrow
                | Op::Exit(_)
        ) {
            work.push(i + 1);
        }
    }
    false
}

/// Whether, from op `from` on, the next op mentioning `cv` assigns it a
/// value that does not read it, with no jump target, branch or exit
/// before (see [`remove_dead_entry_stores`]).
fn overwritten_before_read(
    op_array: &OpArray,
    from: usize,
    cv: u32,
    is_target: &[bool],
    mentions: &impl Fn(&Op) -> Vec<u32>,
) -> bool {
    for (j, op) in op_array.ops.iter().enumerate().skip(from) {
        if is_target.get(j).copied().unwrap_or(true) {
            return false;
        }
        if mentions(op).contains(&cv) {
            return matches!(op, Op::AssignCv { cv: c, val, .. }
                if *c == cv && *val != Operand::Cv(cv));
        }
        let mut has_label = false;
        let mut op = op.clone();
        super::for_each_label_mut(&mut op, |_| has_label = true);
        if has_label
            || matches!(
                op,
                Op::Return { .. }
                    | Op::ReturnRef { .. }
                    | Op::ReturnByRefValue { .. }
                    | Op::Throw(_)
                    | Op::Rethrow
                    | Op::Exit(_)
                    | Op::FastRet { .. }
            )
        {
            return false;
        }
    }
    false
}

/// What replaces a removed side-effect-free op: the `FREE` of the temporary
/// it consumed, since reading a temporary releases it and an object in it
/// must still be destroyed there (php-src's dce.c leaves a FREE of a
/// refcounted operand), or a NOP. `None` when it consumes two temporaries.
fn leftover_of(op: &Op) -> Option<Op> {
    let mut consumed = None;
    let mut extra = false;
    let mut op_clone = op.clone();
    for_each_operand_mut(&mut op_clone, |operand| {
        if let Operand::Tmp(t) = operand {
            if consumed.replace(*t).is_some() {
                extra = true;
            }
        }
    });
    match (consumed, extra) {
        (_, true) => None,
        (Some(t), false) => Some(Op::Free(Operand::Tmp(t))),
        (None, false) => Some(Op::Nop),
    }
}

/// Counts the ops that write each temporary (including `FE_FETCH`'s value
/// and key, which [`def_tmp`] does not report).
pub(crate) fn count_defs(op_array: &OpArray, counts: &mut [u32]) {
    let mut bump = |t: u32| {
        if let Some(c) = counts.get_mut(t as usize) {
            *c += 1;
        }
    };
    for op in &op_array.ops {
        if let Some(t) = def_tmp(op) {
            bump(t);
        }
        if let Op::FeFetch { val, key, .. } = op {
            bump(*val);
            if let Some(k) = key {
                bump(*k);
            }
        }
    }
}

fn count_non_free_uses(op_array: &OpArray, counts: &mut [u32]) {
    for op in &op_array.ops {
        if matches!(op, Op::Free(_)) {
            continue;
        }
        let mut op_clone = op.clone();
        // Adding an element can throw or warn, so it keeps its array alive.
        if let Some(t) = super::array_build_tmp_mut(&mut op_clone) {
            if let Some(c) = counts.get_mut(*t as usize) {
                *c += 1;
            }
        }
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

/// Whether `op` can be removed when its result is unused: it must not be able
/// to warn, throw or call user code (php-src's `zend_may_throw()` gate in
/// `dce.c`). Reading a CV can warn "Undefined variable"; a temporary of
/// unknown type can be a NaN (bool coercion warns), an array (string
/// conversion warns; `strlen()`/`count()`/`abs()` throw), a non-numeric
/// string (arithmetic warns or throws) or an object (`__toString()`).
fn is_pure_op(op: &Op) -> bool {
    match op {
        Op::Move { src, .. } => matches!(src, Operand::Lit(_) | Operand::Tmp(_)),
        Op::Cast {
            kind: CastKind::Null,
            a,
            ..
        } => matches!(a, Operand::Lit(_) | Operand::Tmp(_)),
        Op::Bin { op, a, b, .. } => {
            matches!(a, Operand::Lit(_) | Operand::Tmp(_))
                && matches!(b, Operand::Lit(_) | Operand::Tmp(_))
                && matches!(op, BinKind::Identical | BinKind::NotIdentical)
        }
        Op::TypeCheck { a, .. } => matches!(a, Operand::Lit(_) | Operand::Tmp(_)),
        Op::InitArray { .. } => true,
        // `static::class` (and `self::`/`parent::` in a closure or trait)
        // resolve at run time and throw without a class scope.
        Op::FetchClassName { class, .. } => matches!(class, ClassSpec::Named(_)),
        _ => false,
    }
}

/// What the removal of unused results knows beyond the op at hand: the
/// compiled variables that hold a scalar for the whole call
/// (`compact::stable_scalar_params`) and the temporaries that hold a
/// string (the one definition of each is a concatenation).
struct Scalars {
    cv: Vec<bool>,
    tmp: Vec<bool>,
}

impl Scalars {
    fn of(op_array: &OpArray, scalar_cvs: &[bool], defs: &[u32]) -> Self {
        let mut tmp = vec![false; defs.len()];
        for op in &op_array.ops {
            if let Op::Bin {
                op: BinKind::Arith(BinaryOp::Concat),
                dst,
                ..
            } = op
            {
                if let (Some(slot), Some(1)) = (tmp.get_mut(*dst as usize), defs.get(*dst as usize))
                {
                    *slot = true;
                }
            }
        }
        Scalars {
            cv: scalar_cvs.to_vec(),
            tmp,
        }
    }

    /// Whether `o` reads a null, a bool, a number or a string, and raises
    /// nothing doing so.
    fn scalar(&self, literals: &[Value], o: Operand) -> bool {
        match o {
            Operand::Lit(l) => matches!(
                literals.get(l as usize),
                Some(
                    Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_)
                )
            ),
            Operand::Cv(c) => self.cv.get(c as usize) == Some(&true),
            Operand::Tmp(t) | Operand::TmpKeep(t) => self.tmp.get(t as usize) == Some(&true),
            Operand::Unused => false,
        }
    }

    /// [`is_pure_op`], or a concatenation of scalars: php-src's
    /// `zend_may_throw` lets a `CONCAT` throw (or warn, or call
    /// `__toString()`) only for an array or an object operand.
    fn pure(&self, literals: &[Value], op: &Op) -> bool {
        is_pure_op(op)
            || matches!(
                op,
                Op::Bin {
                    op: BinKind::Arith(BinaryOp::Concat),
                    a,
                    b,
                    ..
                } if self.scalar(literals, *a) && self.scalar(literals, *b)
            )
    }
}

/// The compiled variables that certainly hold a value where an op reads
/// them (php-src's type inference: no `MAY_BE_UNDEF`), as far as a look at
/// the function's opening straight line tells.
struct Assigned {
    /// From which op on each variable is assigned (`usize::MAX`: unknown).
    from: Vec<usize>,
}

impl Assigned {
    fn of(op_array: &OpArray, ctx: &super::OptContext<'_>) -> Self {
        let mut from = vec![usize::MAX; op_array.cv_names.len()];
        // An exception leaves a try block at any op, the assignment that
        // threw not made; `extract()` and the like unset by name.
        if !op_array.try_table.is_empty() || super::compact::has_dynamic_var_access(op_array) {
            return Assigned { from };
        }
        // Bound before the first op: by-value parameters and `use`
        // variables (a by-reference one may be unset through the other
        // name).
        for &(cv, by_ref) in ctx.bound_cvs {
            if let (false, Some(slot)) = (by_ref, from.get_mut(cv as usize)) {
                *slot = 0;
            }
        }
        // The opening straight line: every later op runs after it.
        let mut is_target = vec![false; op_array.ops.len() + 1];
        for op in &op_array.ops {
            super::for_each_label_mut(&mut op.clone(), |l| {
                if let Some(slot) = is_target.get_mut(*l as usize) {
                    *slot = true;
                }
            });
        }
        for (i, op) in op_array.ops.iter().enumerate() {
            if is_target[i] {
                break;
            }
            let mut jumps = false;
            super::for_each_label_mut(&mut op.clone(), |_| jumps = true);
            if jumps
                || matches!(
                    op,
                    Op::Return { .. }
                        | Op::ReturnRef { .. }
                        | Op::ReturnByRefValue { .. }
                        | Op::Throw(_)
                        | Op::Rethrow
                        | Op::Exit(_)
                )
            {
                break;
            }
            if let Op::AssignCv { cv, .. } = op {
                if let Some(slot) = from.get_mut(*cv as usize) {
                    *slot = (*slot).min(i + 1);
                }
            }
        }
        // `unset($v)` anywhere: not known any more.
        for op in &op_array.ops {
            if let Op::Unset { place } = op {
                if let Some(PlaceBase::Cv(cv)) =
                    op_array.places.get(*place as usize).map(|p| &p.base)
                {
                    if let Some(slot) = from.get_mut(*cv as usize) {
                        *slot = usize::MAX;
                    }
                }
            }
        }
        Assigned { from }
    }

    /// Whether `cv` certainly holds a value when the op at `at` reads it.
    fn at(&self, at: usize, cv: u32) -> bool {
        self.from.get(cv as usize).is_some_and(|&from| from <= at)
    }
}

/// Removes the array builds whose array nothing reads: `InitArray` and its
/// element ops, when no other op names the temporary (its `FREE` goes in
/// the next round) and no element op can raise anything
/// ([`silent_element`]). php-src's SCCP evaluates such a literal as a
/// partial array and removes the instructions of one that does not escape
/// (`opt/dce_007`: `$b = [$a];` with `$b` never read). `true` when a build
/// was removed.
fn drop_dead_array_builds(
    op_array: &mut OpArray,
    assigned: &Assigned,
    non_free_uses: &[u32],
    defs: &[u32],
) -> bool {
    let mut changed = false;
    for i in 0..op_array.ops.len() {
        let Op::InitArray { dst } = op_array.ops[i] else {
            continue;
        };
        if defs.get(dst as usize) != Some(&1) {
            continue;
        }
        let adds: Vec<usize> = (0..op_array.ops.len())
            .filter(|&j| {
                super::array_build_tmp_mut(&mut op_array.ops[j].clone()).is_some_and(|t| *t == dst)
            })
            .collect();
        // Another reader (an assignment, an argument, ...) keeps it.
        if non_free_uses.get(dst as usize).copied() != u32::try_from(adds.len()).ok() {
            continue;
        }
        let silent = adds.iter().all(|&j| silent_element(op_array, assigned, j));
        if silent {
            for j in adds {
                op_array.ops[j] = Op::Nop;
            }
            op_array.ops[i] = Op::Nop;
            changed = true;
        }
    }
    changed
}

/// Whether the op at `at` adds an element to an array without being able
/// to raise anything: by value, appended or under an int or string
/// literal key other than the largest int (an append after that key finds
/// the next index taken), its value a literal or a compiled variable
/// certainly assigned there.
fn silent_element(op_array: &OpArray, assigned: &Assigned, at: usize) -> bool {
    let Some(Op::ArrayAdd { key, val, .. }) = op_array.ops.get(at) else {
        return false;
    };
    let key_ok = match key {
        None => true,
        Some(Operand::Lit(l)) => match op_array.literals.get(*l as usize) {
            Some(Value::Int(i)) => *i != i64::MAX,
            Some(Value::Str(s)) => !matches!(
                php_runtime::ArrayKey::from_zstr(s),
                php_runtime::ArrayKey::Int(i64::MAX)
            ),
            _ => false,
        },
        Some(_) => false,
    };
    let val_ok = match val {
        Operand::Lit(_) => true,
        Operand::Cv(cv) => assigned.at(at, *cv),
        _ => false,
    };
    key_ok && val_ok
}

/// A pure op whose result nothing uses and which reads two temporaries
/// cannot leave a `FREE` for each (one op, one slot). When the op that
/// made the second temporary is pure as well and made it for this op
/// alone, that op goes (leaving the `FREE` of what it read, if anything)
/// and this one becomes the `FREE` of the first; the next round goes on
/// from there. php-src's dce.c never revives the operands of a dead
/// instruction (`opt/dce_001`: a dead concatenation of concatenations).
/// `true` when one such pair was taken apart.
fn drop_dead_pair(
    op_array: &mut OpArray,
    known: &Scalars,
    non_free_uses: &[u32],
    defs: &[u32],
    frees: &[u32],
) -> bool {
    let count = |counts: &[u32], t: u32| counts.get(t as usize).copied().unwrap_or(0);
    for i in 0..op_array.ops.len() {
        let op = &op_array.ops[i];
        let Some(result) = def_tmp(op) else {
            continue;
        };
        if count(non_free_uses, result) != 0 || !known.pure(&op_array.literals, op) {
            continue;
        }
        let mut read = Vec::new();
        for_each_operand_mut(&mut op.clone(), |operand| {
            if let Operand::Tmp(t) = operand {
                read.push(*t);
            }
        });
        let [first, second] = read[..] else {
            continue;
        };
        if first == second
            || count(defs, second) != 1
            || count(non_free_uses, second) != 1
            || count(frees, second) != 0
        {
            continue;
        }
        let Some(made_at) = op_array
            .ops
            .iter()
            .position(|other| def_tmp(other) == Some(second))
        else {
            continue;
        };
        if !known.pure(&op_array.literals, &op_array.ops[made_at]) {
            continue;
        }
        let Some(rest) = leftover_of(&op_array.ops[made_at]) else {
            continue;
        };
        op_array.ops[made_at] = rest;
        op_array.ops[i] = Op::Free(Operand::Tmp(first));
        return true;
    }
    false
}

/// Whether `op`'s result is always an int or a bool, so releasing it does
/// nothing (php-src's `may_be_refcounted()` is false for it).
fn result_never_refcounted(op: &Op) -> bool {
    matches!(
        op,
        Op::Strlen { .. }
            | Op::Count { .. }
            | Op::TypeCheck { .. }
            | Op::Defined { .. }
            | Op::InArray { .. }
            | Op::KeyExists { .. }
            | Op::ArrayKeyExists { .. }
            | Op::FuncNumArgs { .. }
    )
}

/// Ops without a result that can be dropped (see [`is_pure_op`]).
fn is_pure_dead_op(op: &Op) -> bool {
    match op {
        Op::TypeCheck { dst: None, a, .. } => matches!(a, Operand::Lit(_) | Operand::Tmp(_)),
        Op::Free(Operand::Lit(_) | Operand::Unused) => true,
        _ => false,
    }
}

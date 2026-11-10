//! Pass 14: DCE (Dead Code Elimination) (ZEND_OPTIMIZER_PASS_14)
//!
//! - Basic block CFG reachability: eliminates dead blocks following unconditional
//!   terminators or unexecutable branches.
//! - Elimination of side-effect-free instructions whose result temporary is never used.
//! - Clearing unused `dst` on side-effecting instructions (AssignCv, AssignOp, DoCall...).
//! - Redundant `Free(Tmp)` removal for dead temporaries.
//! - Dead jump-to-next elimination.

use super::{clear_def_tmp, def_tmp, for_each_operand_mut, for_each_place_operand_mut};
use crate::opcode::{BinKind, CastKind, ClassSpec, ForeachKind, Op, OpArray, Operand};
use php_runtime::Value;

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
                Op::FeReset {
                    subject: Operand::Lit(l),
                    end: t,
                    ..
                } if is_empty_array_lit(&op_array.literals, *l) => {
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
                    && !is_pure_op(op)
                    && frees.get(t as usize).copied().unwrap_or(0) > 1
                    && !result_never_refcounted(op)
                {
                    continue;
                }
                if uses == 0 {
                    let rest = if is_pure_op(op) {
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
                || match op {
                    Op::FeFree { .. } => true,
                    Op::FeReset { iter: x, .. }
                    | Op::FeFetch { iter: x, .. }
                    | Op::FeFreeOnReturn { iter: x, .. } => *x != iter,
                    _ => true,
                }
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
    // Variables that may not lose a store.
    let mut pinned = vec![false; num_cvs];
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
fn count_defs(op_array: &OpArray, counts: &mut [u32]) {
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

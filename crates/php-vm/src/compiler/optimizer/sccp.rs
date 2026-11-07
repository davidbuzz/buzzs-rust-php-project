//! Pass 8: SCCP (Sparse Conditional Constant Propagation) & DFA (ZEND_OPTIMIZER_PASS_8)
//!
//! - Global SSA temporary constant propagation: for any temporary Tmp(t) defined
//!   by Move { src: Lit(l), dst: t }, AssignCv { val: Lit(l), dst: Some(t) },
//!   copy propagation from aliases, or folded constant operations
//!   (Bin, Unary, Cast, Strlen, TypeCheck), substitute Lit(l) at all read sites across ops and places.
//! - Conditional branch simplification: evaluate branches with constant conditions
//!   (JmpZ, JmpNz, CmpJmp) and simplify to Jmp or Nop.
//! - Inter-block forward DFA CV constant propagation: Kildall-style lattice dataflow
//!   analysis over basic blocks, propagating constants across block boundaries and joins.

use super::pass1::{fold_bin_op, fold_cast, fold_unary_op};
use super::{add_or_find_literal, for_each_operand_mut, for_each_place_operand_mut};
use crate::compiler::expr::const_truthiness;
use crate::opcode::{type_mask, ArgSrc, Op, OpArray, Operand, PlaceBase};
use php_runtime::Value;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CvLattice {
    Top,
    Const(u32),
    Bottom,
}

fn meet_cv(a: CvLattice, b: CvLattice, literals: &[Value]) -> CvLattice {
    match (a, b) {
        (CvLattice::Top, CvLattice::Top) => CvLattice::Top,
        (CvLattice::Const(l1), CvLattice::Const(l2)) => {
            if l1 == l2 || super::literals_equal(&literals[l1 as usize], &literals[l2 as usize]) {
                CvLattice::Const(l1)
            } else {
                CvLattice::Bottom
            }
        }
        _ => CvLattice::Bottom,
    }
}

fn update_cv_state_for_op(
    op: &Op,
    places: &[crate::opcode::Place],
    num_cvs: usize,
    cur_state: &mut [CvLattice],
) {
    match op {
        Op::AssignCv { cv, val, .. } => {
            let cv_idx = *cv as usize;
            if cv_idx < num_cvs {
                match val {
                    Operand::Lit(l) => {
                        cur_state[cv_idx] = CvLattice::Const(*l);
                    }
                    Operand::Cv(other) => {
                        let o_idx = *other as usize;
                        if o_idx < num_cvs {
                            cur_state[cv_idx] = cur_state[o_idx];
                        } else {
                            cur_state[cv_idx] = CvLattice::Bottom;
                        }
                    }
                    _ => {
                        cur_state[cv_idx] = CvLattice::Bottom;
                    }
                }
            }
        }
        Op::IncDec { place, .. }
        | Op::Assign { place, .. }
        | Op::AssignOp { place, .. }
        | Op::AssignRef { place, .. }
        | Op::MakeRef { place, .. }
        | Op::Unset { place, .. } => {
            if let Some(p) = places.get(*place as usize) {
                match p.base {
                    PlaceBase::Cv(cv) => {
                        let cv_idx = cv as usize;
                        if cv_idx < num_cvs {
                            cur_state[cv_idx] = CvLattice::Bottom;
                        }
                    }
                    PlaceBase::VarVar(_) => {
                        cur_state.fill(CvLattice::Bottom);
                    }
                    _ => {}
                }
            }
        }
        Op::IncDecCv { cv, .. }
        | Op::AssignOpCv { cv, .. }
        | Op::IncDecCvCmpJmp { cv, .. }
        | Op::ConcatCv { cv, .. }
        | Op::FetchConstTo { to: cv, .. }
        | Op::FetchClassConstTo { to: cv, .. }
        | Op::BinAssignCv { cv, .. }
        | Op::Global { cv, .. }
        | Op::BindStatic { cv, .. }
        | Op::InitStatic { cv, .. }
        | Op::Recv { cv, .. }
        | Op::RecvInit { cv, .. }
        | Op::RecvVariadic { cv, .. }
        | Op::AssignDimCv { cv, .. }
        | Op::AssignDimsCv { cv, .. }
        | Op::AssignOpDimsCv { cv, .. }
        | Op::AssignOpDimCv { cv, .. }
        | Op::IncDecDimCv { cv, .. } => {
            let cv_idx = *cv as usize;
            if cv_idx < num_cvs {
                cur_state[cv_idx] = CvLattice::Bottom;
            }
        }
        Op::Abs { to: Some(cv), .. } => {
            let cv_idx = *cv as usize;
            if cv_idx < num_cvs {
                cur_state[cv_idx] = CvLattice::Bottom;
            }
        }
        _ => {}
    }
}

/// Runs SCCP / DFA constant propagation on `op_array`. Returns `true` if any substitution was made.
pub(crate) fn run_sccp(op_array: &mut OpArray) -> bool {
    let num_tmps = op_array.num_tmps as usize;
    if num_tmps == 0 && op_array.cv_names.is_empty() {
        return false;
    }

    let mut changed = false;

    // 1. Temporary constant discovery & SSA propagation
    if num_tmps > 0 {
        let mut def_count = vec![0u32; num_tmps + 1];
        let mut lit_def = vec![None; num_tmps + 1];
        let mut tmp_alias = vec![None; num_tmps + 1];

        for op in &op_array.ops {
            if let Some(t) = super::def_tmp(op) {
                let t_idx = t as usize;
                if t_idx <= num_tmps {
                    def_count[t_idx] += 1;
                    match op {
                        Op::Move { src, dst: _ } => match src {
                            Operand::Lit(l) => {
                                lit_def[t_idx] = Some(*l);
                            }
                            Operand::Tmp(s) => {
                                tmp_alias[t_idx] = Some(*s);
                            }
                            _ => {}
                        },
                        Op::AssignCv {
                            val, dst: Some(_), ..
                        } => match val {
                            Operand::Lit(l) => {
                                lit_def[t_idx] = Some(*l);
                            }
                            Operand::Tmp(s) => {
                                tmp_alias[t_idx] = Some(*s);
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                }
            }
        }

        for t in 0..=num_tmps {
            if def_count[t] != 1 {
                lit_def[t] = None;
                tmp_alias[t] = None;
            }
        }

        // Resolve alias chains (e.g. t2 = t1 where t1 is Lit(l))
        for t in 0..=num_tmps {
            if lit_def[t].is_none() && tmp_alias[t].is_some() {
                let mut cur = tmp_alias[t];
                let mut hops = 0;
                while let Some(alias) = cur {
                    if hops >= 16 {
                        break;
                    }
                    hops += 1;
                    let a_idx = alias as usize;
                    if a_idx <= num_tmps {
                        if let Some(lit) = lit_def[a_idx] {
                            lit_def[t] = Some(lit);
                            break;
                        }
                        cur = tmp_alias[a_idx];
                    } else {
                        break;
                    }
                }
            }
        }

        // Fold pure expressions on known temporary constants
        for op in &mut op_array.ops {
            if let Some(t) = super::def_tmp(op) {
                let t_idx = t as usize;
                if t_idx <= num_tmps && def_count[t_idx] == 1 && lit_def[t_idx].is_none() {
                    match op {
                        Op::Bin {
                            op: bkind,
                            a,
                            b,
                            dst,
                        } => {
                            let lit_a = match a {
                                Operand::Lit(l) => Some(*l),
                                Operand::Tmp(s) => lit_def.get(*s as usize).copied().flatten(),
                                _ => None,
                            };
                            let lit_b = match b {
                                Operand::Lit(l) => Some(*l),
                                Operand::Tmp(s) => lit_def.get(*s as usize).copied().flatten(),
                                _ => None,
                            };
                            if let (Some(la), Some(lb)) = (lit_a, lit_b) {
                                if let (Some(va), Some(vb)) = (
                                    op_array.literals.get(la as usize).cloned(),
                                    op_array.literals.get(lb as usize).cloned(),
                                ) {
                                    if let Some(val) = fold_bin_op(*bkind, &va, &vb) {
                                        let lit = add_or_find_literal(&mut op_array.literals, val);
                                        lit_def[*dst as usize] = Some(lit);
                                        *op = Op::Move {
                                            src: Operand::Lit(lit),
                                            dst: *dst,
                                        };
                                        changed = true;
                                    }
                                }
                            }
                        }
                        Op::Unary { op: ukind, a, dst } => {
                            let lit_a = match a {
                                Operand::Lit(l) => Some(*l),
                                Operand::Tmp(s) => lit_def.get(*s as usize).copied().flatten(),
                                _ => None,
                            };
                            if let Some(la) = lit_a {
                                if let Some(va) = op_array.literals.get(la as usize).cloned() {
                                    if let Some(val) = fold_unary_op(*ukind, &va) {
                                        let lit = add_or_find_literal(&mut op_array.literals, val);
                                        lit_def[*dst as usize] = Some(lit);
                                        *op = Op::Move {
                                            src: Operand::Lit(lit),
                                            dst: *dst,
                                        };
                                        changed = true;
                                    }
                                }
                            }
                        }
                        Op::Cast { kind, a, dst } => {
                            let lit_a = match a {
                                Operand::Lit(l) => Some(*l),
                                Operand::Tmp(s) => lit_def.get(*s as usize).copied().flatten(),
                                _ => None,
                            };
                            if let Some(la) = lit_a {
                                if let Some(va) = op_array.literals.get(la as usize).cloned() {
                                    if let Some(val) = fold_cast(*kind, &va) {
                                        let lit = add_or_find_literal(&mut op_array.literals, val);
                                        lit_def[*dst as usize] = Some(lit);
                                        *op = Op::Move {
                                            src: Operand::Lit(lit),
                                            dst: *dst,
                                        };
                                        changed = true;
                                    }
                                }
                            }
                        }
                        Op::Strlen { a, dst } => {
                            let lit_a = match a {
                                Operand::Lit(l) => Some(*l),
                                Operand::Tmp(s) => lit_def.get(*s as usize).copied().flatten(),
                                _ => None,
                            };
                            if let Some(la) = lit_a {
                                let str_len = match op_array.literals.get(la as usize) {
                                    Some(Value::Str(s)) => Some(s.len() as i64),
                                    _ => None,
                                };
                                if let Some(len) = str_len {
                                    let lit = add_or_find_literal(
                                        &mut op_array.literals,
                                        Value::Int(len),
                                    );
                                    if let Some(d) = dst {
                                        lit_def[*d as usize] = Some(lit);
                                        *op = Op::Move {
                                            src: Operand::Lit(lit),
                                            dst: *d,
                                        };
                                        changed = true;
                                    }
                                }
                            }
                        }
                        Op::TypeCheck { a, mask, dst } => {
                            let lit_a = match a {
                                Operand::Lit(l) => Some(*l),
                                Operand::Tmp(s) => lit_def.get(*s as usize).copied().flatten(),
                                _ => None,
                            };
                            if let Some(la) = lit_a {
                                if let Some(va) = op_array.literals.get(la as usize) {
                                    let res = type_mask::matches(*mask, va);
                                    let lit = add_or_find_literal(
                                        &mut op_array.literals,
                                        Value::Bool(res),
                                    );
                                    if let Some(d) = dst {
                                        lit_def[*d as usize] = Some(lit);
                                        *op = Op::Move {
                                            src: Operand::Lit(lit),
                                            dst: *d,
                                        };
                                        changed = true;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // Substitute known temporary constants
        if lit_def.iter().any(|d| d.is_some()) {
            for op in &mut op_array.ops {
                for_each_operand_mut(op, |operand| {
                    if let Operand::Tmp(t) = operand {
                        if let Some(Some(l)) = lit_def.get(*t as usize) {
                            *operand = Operand::Lit(*l);
                            changed = true;
                        }
                    }
                });
            }

            for place in &mut op_array.places {
                for_each_place_operand_mut(place, |operand| {
                    if let Operand::Tmp(t) = operand {
                        if let Some(Some(l)) = lit_def.get(*t as usize) {
                            *operand = Operand::Lit(*l);
                            changed = true;
                        }
                    }
                });
            }

            for op in &mut op_array.ops {
                if let Op::Move {
                    src: Operand::Lit(_),
                    dst,
                } = op
                {
                    if let Some(Some(_)) = lit_def.get(*dst as usize) {
                        *op = Op::Nop;
                        changed = true;
                    }
                }
            }
        }
    }

    // Branch simplification for constant conditions
    for op in &mut op_array.ops {
        match op {
            Op::JmpZ(Operand::Lit(l), target) => {
                if let Some(va) = op_array.literals.get(*l as usize) {
                    if let Some(truthy) = const_truthiness(va) {
                        if !truthy {
                            *op = Op::Jmp(*target);
                        } else {
                            *op = Op::Nop;
                        }
                        changed = true;
                    }
                }
            }
            Op::JmpNz(Operand::Lit(l), target) => {
                if let Some(va) = op_array.literals.get(*l as usize) {
                    if let Some(truthy) = const_truthiness(va) {
                        if truthy {
                            *op = Op::Jmp(*target);
                        } else {
                            *op = Op::Nop;
                        }
                        changed = true;
                    }
                }
            }
            Op::CmpJmp {
                op: bkind,
                a: Operand::Lit(la),
                b: Operand::Lit(lb),
                target,
                jmp_if,
                ..
            } => {
                if let (Some(va), Some(vb)) = (
                    op_array.literals.get(*la as usize).cloned(),
                    op_array.literals.get(*lb as usize).cloned(),
                ) {
                    if let Some(Value::Bool(res)) = fold_bin_op(*bkind, &va, &vb) {
                        if res == *jmp_if {
                            *op = Op::Jmp(*target);
                        } else {
                            *op = Op::Nop;
                        }
                        changed = true;
                    }
                }
            }
            _ => {}
        }
    }

    // 2. Inter-block Forward DFA for CVs
    let mut has_hazard = false;
    for op in &op_array.ops {
        match op {
            Op::MakeRef { .. }
            | Op::AssignRef { .. }
            | Op::ListFetchRef { .. }
            | Op::Include { .. }
            | Op::Global { .. } => {
                has_hazard = true;
                break;
            }
            Op::Send {
                src: ArgSrc::Place(_),
            } => {
                has_hazard = true;
                break;
            }
            _ => {}
        }
    }

    if !has_hazard {
        for place in &op_array.places {
            if matches!(place.base, PlaceBase::VarVar(_) | PlaceBase::Global(_)) {
                has_hazard = true;
                break;
            }
        }
    }

    let num_cvs = op_array.cv_names.len();
    let len = op_array.ops.len();

    if !has_hazard && num_cvs > 0 && len > 0 {
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

        // Mark parameters as Bottom at entry
        let mut is_param = vec![false; num_cvs];
        for op in &op_array.ops {
            match op {
                Op::Recv { cv, .. }
                | Op::RecvInit { cv, .. }
                | Op::RecvDefault { cv, .. }
                | Op::RecvVariadic { cv, .. } => {
                    if (*cv as usize) < num_cvs {
                        is_param[*cv as usize] = true;
                    }
                }
                _ => {}
            }
        }

        let entry_state: Vec<CvLattice> = (0..num_cvs)
            .map(|cv| {
                if is_param[cv] {
                    CvLattice::Bottom
                } else {
                    CvLattice::Top
                }
            })
            .collect();

        let mut in_state = vec![vec![CvLattice::Top; num_cvs]; num_blocks];
        let mut out_state = vec![vec![CvLattice::Top; num_cvs]; num_blocks];
        let mut block_reachable = vec![false; num_blocks];

        in_state[0] = entry_state;
        block_reachable[0] = true;

        let mut worklist: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
        worklist.push_back(0);
        let mut in_worklist = vec![false; num_blocks];
        in_worklist[0] = true;

        while let Some(b) = worklist.pop_front() {
            in_worklist[b] = false;
            let b_start = block_starts[b];
            let b_end = block_starts[b + 1];
            let mut cur_state = in_state[b].clone();

            for i in b_start..b_end {
                let op = &op_array.ops[i];
                let is_call = matches!(
                    op,
                    Op::DoCall { .. }
                        | Op::InitFCall { .. }
                        | Op::InitDynCall { .. }
                        | Op::InitMethodCall { .. }
                        | Op::InitStaticCall { .. }
                );
                if is_call {
                    cur_state.fill(CvLattice::Bottom);
                    continue;
                }

                update_cv_state_for_op(op, &op_array.places, num_cvs, &mut cur_state);
            }

            out_state[b] = cur_state;

            // Determine executable successors of block b
            let last_idx = (b_start..b_end)
                .rev()
                .find(|&i| !matches!(op_array.ops[i], Op::Nop));
            let mut successors = Vec::new();

            if let Some(idx) = last_idx {
                match &op_array.ops[idx] {
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
                        // No successors
                    }
                    Op::JmpZ(cond, t) => {
                        let cond_val = match cond {
                            Operand::Lit(l) => Some(CvLattice::Const(*l)),
                            Operand::Cv(cv) if (*cv as usize) < num_cvs => {
                                Some(out_state[b][*cv as usize])
                            }
                            _ => None,
                        };
                        let target_block = if (*t as usize) < len {
                            Some(inst_to_block[*t as usize])
                        } else {
                            None
                        };
                        let fallthrough = if b + 1 < num_blocks {
                            Some(b + 1)
                        } else {
                            None
                        };

                        match cond_val {
                            Some(CvLattice::Const(l)) => {
                                if let Some(va) = op_array.literals.get(l as usize) {
                                    let truthy = const_truthiness(va);
                                    if truthy == Some(false) {
                                        if let Some(tb) = target_block {
                                            successors.push(tb);
                                        }
                                    } else if truthy == Some(true) {
                                        if let Some(fb) = fallthrough {
                                            successors.push(fb);
                                        }
                                    } else {
                                        if let Some(tb) = target_block {
                                            successors.push(tb);
                                        }
                                        if let Some(fb) = fallthrough {
                                            successors.push(fb);
                                        }
                                    }
                                } else {
                                    if let Some(tb) = target_block {
                                        successors.push(tb);
                                    }
                                    if let Some(fb) = fallthrough {
                                        successors.push(fb);
                                    }
                                }
                            }
                            _ => {
                                if let Some(tb) = target_block {
                                    successors.push(tb);
                                }
                                if let Some(fb) = fallthrough {
                                    successors.push(fb);
                                }
                            }
                        }
                    }
                    Op::JmpNz(cond, t) => {
                        let cond_val = match cond {
                            Operand::Lit(l) => Some(CvLattice::Const(*l)),
                            Operand::Cv(cv) if (*cv as usize) < num_cvs => {
                                Some(out_state[b][*cv as usize])
                            }
                            _ => None,
                        };
                        let target_block = if (*t as usize) < len {
                            Some(inst_to_block[*t as usize])
                        } else {
                            None
                        };
                        let fallthrough = if b + 1 < num_blocks {
                            Some(b + 1)
                        } else {
                            None
                        };

                        match cond_val {
                            Some(CvLattice::Const(l)) => {
                                if let Some(va) = op_array.literals.get(l as usize) {
                                    let truthy = const_truthiness(va);
                                    if truthy == Some(true) {
                                        if let Some(tb) = target_block {
                                            successors.push(tb);
                                        }
                                    } else if truthy == Some(false) {
                                        if let Some(fb) = fallthrough {
                                            successors.push(fb);
                                        }
                                    } else {
                                        if let Some(tb) = target_block {
                                            successors.push(tb);
                                        }
                                        if let Some(fb) = fallthrough {
                                            successors.push(fb);
                                        }
                                    }
                                } else {
                                    if let Some(tb) = target_block {
                                        successors.push(tb);
                                    }
                                    if let Some(fb) = fallthrough {
                                        successors.push(fb);
                                    }
                                }
                            }
                            _ => {
                                if let Some(tb) = target_block {
                                    successors.push(tb);
                                }
                                if let Some(fb) = fallthrough {
                                    successors.push(fb);
                                }
                            }
                        }
                    }
                    Op::CmpJmp { target: t, .. }
                    | Op::JmpZEx { target: t, .. }
                    | Op::JmpNzEx { target: t, .. }
                    | Op::JmpSet { target: t, .. }
                    | Op::Coalesce { target: t, .. }
                    | Op::JmpNull { target: t, .. }
                    | Op::FastCall { target: t, .. }
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
                if succ >= num_blocks {
                    continue;
                }
                let is_first = !block_reachable[succ];
                block_reachable[succ] = true;

                let mut succ_changed = is_first;
                for cv in 0..num_cvs {
                    let out_val = out_state[b][cv];
                    let in_val = in_state[succ][cv];
                    let merged = if is_first {
                        out_val
                    } else {
                        meet_cv(in_val, out_val, &op_array.literals)
                    };
                    if merged != in_val {
                        in_state[succ][cv] = merged;
                        succ_changed = true;
                    }
                }

                if succ_changed && !in_worklist[succ] {
                    worklist.push_back(succ);
                    in_worklist[succ] = true;
                }
            }
        }

        // Apply constant CVs across all reachable blocks
        for b in 0..num_blocks {
            if !block_reachable[b] {
                continue;
            }
            let b_start = block_starts[b];
            let b_end = block_starts[b + 1];
            let mut cur_state = in_state[b].clone();

            for i in b_start..b_end {
                let op = &mut op_array.ops[i];
                let is_call = matches!(
                    op,
                    Op::DoCall { .. }
                        | Op::InitFCall { .. }
                        | Op::InitDynCall { .. }
                        | Op::InitMethodCall { .. }
                        | Op::InitStaticCall { .. }
                );
                if is_call {
                    cur_state.fill(CvLattice::Bottom);
                    continue;
                }

                // Substitute Cv reads with known constants
                for_each_operand_mut(op, |operand| {
                    if let Operand::Cv(cv) = operand {
                        if let Some(CvLattice::Const(l)) = cur_state.get(*cv as usize) {
                            *operand = Operand::Lit(*l);
                            changed = true;
                        }
                    }
                });

                // Update CV state
                update_cv_state_for_op(op, &op_array.places, num_cvs, &mut cur_state);
            }
        }
    }

    changed
}

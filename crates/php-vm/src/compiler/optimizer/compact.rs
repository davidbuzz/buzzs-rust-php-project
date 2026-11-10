//! Compaction passes:
//! - Pass 9: Temporary variable compaction (ZEND_OPTIMIZER_PASS_9)
//! - Pass 10: NOP removal with full label and exception table remapping (ZEND_OPTIMIZER_PASS_10)
//! - Pass 11: Literal table compaction & deduplication (ZEND_OPTIMIZER_PASS_11)
//! - Pass 12: Adjust used stack / compact CV slots (ZEND_OPTIMIZER_PASS_12)
//! - Pass 13: Remove unused variables (ZEND_OPTIMIZER_PASS_13)

use super::{
    def_tmp, for_each_label_mut, for_each_operand_mut, for_each_place_operand_mut, literals_equal,
};
use crate::opcode::{ClassSpec, Op, OpArray, Operand, Place, PlaceBase};
use php_runtime::Value;
use std::collections::HashMap;

/// Pass 10: Removes NOP instructions and updates all jump targets, try regions, and live ranges.
pub(crate) fn remove_nops(op_array: &mut OpArray) -> bool {
    let has_nops = op_array.ops.iter().any(|op| matches!(op, Op::Nop));
    if !has_nops {
        return false;
    }

    let old_len = op_array.ops.len();
    let mut remap = Vec::with_capacity(old_len + 1);
    let mut new_idx = 0u32;

    for op in &op_array.ops {
        remap.push(new_idx);
        if !matches!(op, Op::Nop) {
            new_idx += 1;
        }
    }
    remap.push(new_idx);

    // 1. Remap all instruction jump targets
    for op in &mut op_array.ops {
        for_each_label_mut(op, |lbl| {
            if (*lbl as usize) < remap.len() {
                *lbl = remap[*lbl as usize];
            }
        });
    }

    // 2. Remap try table regions
    for entry in &mut op_array.try_table {
        if (entry.try_start as usize) < remap.len() {
            entry.try_start = remap[entry.try_start as usize];
        }
        if (entry.try_end as usize) < remap.len() {
            entry.try_end = remap[entry.try_end as usize];
        }
        if let Some(cs) = &mut entry.catch_start {
            if (*cs as usize) < remap.len() {
                *cs = remap[*cs as usize];
            }
        }
        if (entry.catch_end as usize) < remap.len() {
            entry.catch_end = remap[entry.catch_end as usize];
        }
        if let Some((_, f_start, f_end)) = &mut entry.finally {
            if (*f_start as usize) < remap.len() {
                *f_start = remap[*f_start as usize];
            }
            if (*f_end as usize) < remap.len() {
                *f_end = remap[*f_end as usize];
            }
        }
    }

    // 3. Remap live temporaries
    for live in &mut op_array.live_tmps {
        if (live.start as usize) < remap.len() {
            live.start = remap[live.start as usize];
        }
        if (live.end as usize) < remap.len() {
            live.end = remap[live.end as usize];
        }
    }
    op_array.live_tmps.retain(|live| live.start < live.end);

    // 4. Compact ops and lines
    let mut new_ops = Vec::with_capacity(new_idx as usize);
    let mut new_lines = Vec::with_capacity(new_idx as usize);
    for (op, line) in op_array.ops.drain(..).zip(op_array.lines.drain(..)) {
        if !matches!(op, Op::Nop) {
            new_ops.push(op);
            new_lines.push(line);
        }
    }
    op_array.ops = new_ops;
    op_array.lines = new_lines;

    true
}

/// Pass 9: Renumbers used temporaries densely from 0, reducing `op_array.num_tmps`.
pub(crate) fn compact_tmps(op_array: &mut OpArray) -> bool {
    let mut used = vec![false; op_array.num_tmps as usize + 1];

    let mut mark_tmp = |t: u32| {
        let t = t as usize;
        if t < used.len() {
            used[t] = true;
        }
    };

    // Mark all read temporaries
    for op in &op_array.ops {
        let mut op_clone = op.clone();
        for_each_operand_mut(&mut op_clone, |operand| match operand {
            Operand::Tmp(t) | Operand::TmpKeep(t) => mark_tmp(*t),
            _ => {}
        });
        if let Some(t) = def_tmp(op) {
            mark_tmp(t);
        }
    }

    for place in &op_array.places {
        let mut place_clone = place.clone();
        for_each_place_operand_mut(&mut place_clone, |operand| match operand {
            Operand::Tmp(t) | Operand::TmpKeep(t) => mark_tmp(*t),
            _ => {}
        });
    }

    for live in &op_array.live_tmps {
        mark_tmp(live.tmp);
    }

    let mut remap = HashMap::new();
    let mut next_id = 0u32;
    for (old_id, is_used) in used.iter().enumerate() {
        if *is_used {
            remap.insert(old_id as u32, next_id);
            next_id += 1;
        }
    }

    if next_id >= op_array.num_tmps {
        return false;
    }

    let remap_tmp = |t: &mut u32| {
        if let Some(&new_t) = remap.get(t) {
            *t = new_t;
        }
    };

    // Remap operands in ops
    for op in &mut op_array.ops {
        for_each_operand_mut(op, |operand| match operand {
            Operand::Tmp(t) | Operand::TmpKeep(t) => remap_tmp(t),
            _ => {}
        });

        match op {
            Op::Move { dst, .. }
            | Op::Bin { dst, .. }
            | Op::Unary { dst, .. }
            | Op::Cast { dst, .. }
            | Op::Rope { dst, .. }
            | Op::InitArray { dst }
            | Op::JmpZEx { dst, .. }
            | Op::JmpNzEx { dst, .. }
            | Op::JmpSet { dst, .. }
            | Op::Coalesce { dst, .. }
            | Op::JmpNull { dst, .. }
            | Op::MakeRef { dst, .. }
            | Op::Fetch { dst, .. }
            | Op::Isset { dst, .. }
            | Op::Empty { dst, .. }
            | Op::FetchThis { dst }
            | Op::New { dst, .. }
            | Op::CallableConvert { dst, .. }
            | Op::CallablePartial { dst, .. }
            | Op::DeclareAnonClass { dst, .. }
            | Op::MakeClosure { dst, .. }
            | Op::FetchConst { dst, .. }
            | Op::FetchClassConst { dst, .. }
            | Op::FetchClassName { dst, .. }
            | Op::InstanceOf { dst, .. }
            | Op::Clone { dst, .. }
            | Op::BeginSilence { dst }
            | Op::Include { dst, .. }
            | Op::AssertCheck { dst, .. }
            | Op::ConstPropCheck { dst, .. }
            | Op::CmpJmp { dst, .. }
            | Op::BinAssignCv { dst, .. }
            | Op::BinAssignCvCmpJmp { dst, .. }
            | Op::BinAssignCvJmp { dst, .. }
            | Op::FetchDimCv { dst, .. }
            | Op::FetchDimsCv { dst, .. }
            | Op::IssetDimCv { dst, .. }
            | Op::FetchStaticProp { dst, .. }
            | Op::FetchGlobal { dst, .. }
            | Op::IssetStaticProp { dst, .. }
            | Op::FetchConstTo { dst, .. }
            | Op::FetchClassConstTo { dst, .. } => remap_tmp(dst),

            Op::AssignCv { dst, .. }
            | Op::Assign { dst, .. }
            | Op::AssignOp { dst, .. }
            | Op::IncDec { dst, .. }
            | Op::AssignRef { dst, .. }
            | Op::ListFetchRef { dst, .. }
            | Op::ArrayKeyExists { dst, .. }
            | Op::DoCall { dst }
            | Op::Yield { dst, .. }
            | Op::YieldRef { dst, .. }
            | Op::YieldByRefValue { dst, .. }
            | Op::YieldFrom { dst, .. }
            | Op::FuncNumArgs { dst }
            | Op::FuncGetArgs { dst }
            | Op::Strlen { dst, .. }
            | Op::Count { dst, .. }
            | Op::TypeCheck { dst, .. }
            | Op::GetType { dst, .. }
            | Op::GetClass { dst, .. }
            | Op::GetCalledClass { dst }
            | Op::Defined { dst, .. }
            | Op::InArray { dst, .. }
            | Op::KeyExists { dst, .. }
            | Op::IncDecCv { dst, .. }
            | Op::AssignOpCv { dst, .. }
            | Op::IncDecDimCv { dst, .. }
            | Op::AssignStaticProp { dst, .. }
            | Op::AssignOpStaticProp { dst, .. }
            | Op::ConcatCv { dst, .. }
            | Op::Abs { dst, .. } => {
                if let Some(d) = dst {
                    remap_tmp(d);
                }
            }
            _ => {}
        }
    }

    // Remap operands in places
    for place in &mut op_array.places {
        for_each_place_operand_mut(place, |operand| match operand {
            Operand::Tmp(t) | Operand::TmpKeep(t) => remap_tmp(t),
            _ => {}
        });
    }

    // Remap live temporaries
    for live in &mut op_array.live_tmps {
        remap_tmp(&mut live.tmp);
    }

    op_array.num_tmps = next_id;
    true
}

/// Pass 11: Deduplicates identical literals in `op_array.literals`.
pub(crate) fn compact_literals(op_array: &mut OpArray) -> bool {
    let old_literals = &op_array.literals;
    let mut unique_literals = Vec::new();
    let mut remap = Vec::with_capacity(old_literals.len());

    for old_val in old_literals {
        let mut found = None;
        for (new_idx, unique_val) in unique_literals.iter().enumerate() {
            if literals_equal(old_val, unique_val) {
                found = Some(new_idx as u32);
                break;
            }
        }
        if let Some(idx) = found {
            remap.push(idx);
        } else {
            let idx = unique_literals.len() as u32;
            unique_literals.push(old_val.clone());
            remap.push(idx);
        }
    }

    if unique_literals.len() >= old_literals.len() {
        return false;
    }

    let remap_lit = |l: &mut u32| {
        if (*l as usize) < remap.len() {
            *l = remap[*l as usize];
        }
    };

    // Remap literals in ops
    for op in &mut op_array.ops {
        for_each_operand_mut(op, |operand| {
            if let Operand::Lit(l) = operand {
                remap_lit(l);
            }
        });

        match op {
            Op::New { class, .. }
            | Op::InitStaticCall { class, .. }
            | Op::FetchClassConst { class, .. }
            | Op::FetchClassName { class, .. }
            | Op::InstanceOf { class, .. } => {
                if let ClassSpec::Named(l) = class {
                    remap_lit(l);
                }
            }
            Op::InitFCall {
                name,
                fallback,
                display,
            } => {
                remap_lit(name);
                if let Some(fb) = fallback {
                    remap_lit(fb);
                }
                remap_lit(display);
            }
            Op::SendNamed { name, .. } => remap_lit(name),
            Op::FetchConst { name, fallback, .. } => {
                remap_lit(name);
                if let Some(fb) = fallback {
                    remap_lit(fb);
                }
            }
            Op::FetchClassConstTo { class, name, .. } => {
                if let ClassSpec::Named(l) = class {
                    remap_lit(l);
                }
                remap_lit(name);
            }
            Op::FetchConstTo { name, fallback, .. } => {
                remap_lit(name);
                if let Some(fb) = fallback {
                    remap_lit(fb);
                }
            }
            Op::Defined { name, .. } => remap_lit(name),
            Op::DeclareConst { name, .. } => remap_lit(name),
            Op::Catch { classes, .. } => {
                for c in classes.iter_mut() {
                    remap_lit(c);
                }
            }
            Op::TypeAssertArray { msg, .. } | Op::Diag { msg, .. } => remap_lit(msg),
            Op::UserOpcode { opcode } => remap_lit(opcode),
            Op::InitParentHook { prop, .. } => remap_lit(prop),
            Op::CallablePartial { name, .. } => remap_lit(name),
            Op::Global { name, .. } => remap_lit(name),
            _ => {}
        }
    }

    // Remap literals in places
    for place in &mut op_array.places {
        for_each_place_operand_mut(place, |operand| {
            if let Operand::Lit(l) = operand {
                remap_lit(l);
            }
        });
        if let PlaceBase::StaticProp { class, .. } = &mut place.base {
            if let ClassSpec::Named(l) = class {
                remap_lit(l);
            }
        }
    }

    op_array.literals = unique_literals;
    true
}

// -----------------------------------------------------------------------------
// Pass 12 & Pass 13: Variable compaction and dead variable removal
// -----------------------------------------------------------------------------

/// Iterates mutably over all CV indices referenced by instruction `op`.
pub(crate) fn for_each_cv_mut(op: &mut Op, mut f: impl FnMut(&mut u32)) {
    // 1. All Operand::Cv in instruction operands
    for_each_operand_mut(op, |operand| {
        if let Operand::Cv(cv) = operand {
            f(cv);
        }
    });

    // 2. Direct CV fields in instructions
    match op {
        Op::AssignCv { cv, .. }
        | Op::Global { cv, .. }
        | Op::BindStatic { cv, .. }
        | Op::InitStatic { cv, .. }
        | Op::Recv { cv, .. }
        | Op::RecvInit { cv, .. }
        | Op::RecvDefault { cv, .. }
        | Op::RecvVariadic { cv, .. }
        | Op::BinAssignCv { cv, .. }
        | Op::BinAssignCvCmpJmp { cv, .. }
        | Op::BinAssignCvJmp { cv, .. }
        | Op::IncDecCv { cv, .. }
        | Op::AssignOpCv { cv, .. }
        | Op::AssignOpCvCmpJmp { cv, .. }
        | Op::AssignOpCvJmp { cv, .. }
        | Op::AssignOpDimCv { cv, .. }
        | Op::IncDecDimCv { cv, .. }
        | Op::IssetDimCv { cv, .. }
        | Op::AssignDimCv { cv, .. }
        | Op::ConcatCv { cv, .. }
        | Op::AssignDimsCv { cv, .. }
        | Op::AssignOpDimsCv { cv, .. }
        | Op::IncDecCvCmpJmp { cv, .. } => f(cv),

        Op::Catch { cv, .. } => {
            if let Some(c) = cv {
                f(c);
            }
        }
        Op::FetchDimCv { cv, to, .. } | Op::FetchDimsCv { cv, to, .. } => {
            f(cv);
            if let Some(t) = to {
                f(t);
            }
        }
        Op::FetchStaticProp { to, .. }
        | Op::FetchGlobal { to, .. }
        | Op::FetchObjProp { to, .. }
        | Op::FetchObjPropDim { to, .. }
        | Op::Abs { to, .. } => {
            if let Some(t) = to {
                f(t);
            }
        }
        Op::FetchConstTo { to, .. } | Op::FetchClassConstTo { to, .. } => {
            f(to);
        }
        _ => {}
    }
}

/// Iterates mutably over all CV indices referenced by place `place`.
pub(crate) fn for_each_place_cv_mut(place: &mut Place, mut f: impl FnMut(&mut u32)) {
    if let PlaceBase::Cv(cv) = &mut place.base {
        f(cv);
    }
    for_each_place_operand_mut(place, |operand| {
        if let Operand::Cv(cv) = operand {
            f(cv);
        }
    });
}

/// Whether the op array contains dynamic variable access constructs like
/// `compact`, `extract`, `get_defined_vars`, variable variables, superglobals, or include/eval.
fn has_dynamic_var_access(op_array: &OpArray) -> bool {
    for place in &op_array.places {
        if matches!(
            place.base,
            PlaceBase::VarVar(_) | PlaceBase::Global(_) | PlaceBase::GlobalsArray
        ) {
            return true;
        }
    }

    for op in &op_array.ops {
        match op {
            Op::Include { .. } => return true,
            Op::InitFCall { name, fallback, .. } => {
                let is_dynamic_fn = |idx: u32| {
                    if let Some(Value::Str(s)) = op_array.literals.get(idx as usize) {
                        let bytes = s.as_bytes();
                        bytes.eq_ignore_ascii_case(b"compact")
                            || bytes.eq_ignore_ascii_case(b"extract")
                            || bytes.eq_ignore_ascii_case(b"get_defined_vars")
                    } else {
                        false
                    }
                };
                if is_dynamic_fn(*name) {
                    return true;
                }
                if let Some(fb) = fallback {
                    if is_dynamic_fn(*fb) {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }

    false
}

/// Pass 13: Removes assignments to variables that are never read and never referenced (ZEND_OPTIMIZER_PASS_13).
pub(crate) fn remove_unused_variables(op_array: &mut OpArray) -> bool {
    let num_cvs = op_array.cv_names.len();
    if num_cvs == 0 || has_dynamic_var_access(op_array) {
        return false;
    }

    // Parameters cannot be removed
    let mut num_param_cvs = 0u32;
    for op in &op_array.ops {
        match op {
            Op::Recv { cv, .. }
            | Op::RecvInit { cv, .. }
            | Op::RecvDefault { cv, .. }
            | Op::RecvVariadic { cv, .. } => {
                num_param_cvs = num_param_cvs.max(*cv + 1);
            }
            _ => {}
        }
    }

    let mut read_counts = vec![0u32; num_cvs];

    // Mark parameter CVs as read
    for i in 0..(num_param_cvs as usize).min(num_cvs) {
        read_counts[i] += 1;
    }

    // Mark closure captures as read
    for f in op_array.decls.functions.borrow().iter() {
        for u in &f.uses {
            if (u.parent_cv as usize) < num_cvs {
                read_counts[u.parent_cv as usize] += 1;
            }
        }
    }

    // Count reads from ops
    for op in &op_array.ops {
        let mut op_clone = op.clone();
        for_each_operand_mut(&mut op_clone, |operand| {
            if let Operand::Cv(cv) = operand {
                if let Some(c) = read_counts.get_mut(*cv as usize) {
                    *c += 1;
                }
            }
        });

        // Instructions that read from CVs (compound assignments, containers, etc.)
        match op {
            Op::IncDecCv { cv, .. }
            | Op::AssignOpCv { cv, .. }
            | Op::AssignOpCvCmpJmp { cv, .. }
            | Op::AssignOpCvJmp { cv, .. }
            | Op::ConcatCv { cv, .. }
            | Op::AssignOpDimCv { cv, .. }
            | Op::IncDecDimCv { cv, .. }
            | Op::FetchDimCv { cv, .. }
            | Op::FetchDimsCv { cv, .. }
            | Op::IssetDimCv { cv, .. }
            | Op::AssignDimCv { cv, .. }
            | Op::AssignDimsCv { cv, .. }
            | Op::AssignOpDimsCv { cv, .. }
            | Op::IncDecCvCmpJmp { cv, .. }
            | Op::Global { cv, .. }
            | Op::BindStatic { cv, .. }
            | Op::InitStatic { cv, .. } => {
                if let Some(c) = read_counts.get_mut(*cv as usize) {
                    *c += 1;
                }
            }
            Op::Catch { cv: Some(cv), .. } => {
                if let Some(c) = read_counts.get_mut(*cv as usize) {
                    *c += 1;
                }
            }
            _ => {}
        }
    }

    // The places that plain assignments (`$cv = v`) write. The compiler makes
    // one place per use site, so a bare-CV place that no plain assignment owns
    // is used some other way (passed to a call, returned, echoed, iterated...)
    // and counts as a read: only writes are listed, so an opcode missing from
    // the list below can never make a live variable look unused.
    let plain_assign_places: std::collections::HashSet<u32> = op_array
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Assign { place, .. } => Some(*place),
            _ => None,
        })
        .collect();

    // Count reads from places
    for (pid, place) in op_array.places.iter().enumerate() {
        let pid_u32 = pid as u32;
        let mut used_in_non_plain_assign = !plain_assign_places.contains(&pid_u32);

        if let PlaceBase::Cv(cv) = place.base {
            if !place.segs.is_empty() {
                // If place has segments (e.g. $a[0]), $a is read as container
                if let Some(c) = read_counts.get_mut(cv as usize) {
                    *c += 1;
                }
            } else {
                for op in &op_array.ops {
                    match op {
                        Op::Assign { place: p, .. } if *p == pid_u32 => {
                            // Plain write
                        }
                        Op::AssignOp { place: p, .. }
                        | Op::IncDec { place: p, .. }
                        | Op::AssignRef { place: p, .. }
                        | Op::MakeRef { place: p, .. }
                        | Op::ListFetchRef { place: p, .. }
                        | Op::Fetch { place: p, .. }
                        | Op::Isset { place: p, .. }
                        | Op::Empty { place: p, .. }
                        | Op::Unset { place: p, .. }
                        | Op::ReturnRef { place: p }
                        | Op::YieldRef { place: p, .. }
                        | Op::ArrayAddRef { place: p, .. }
                        | Op::FeReset { place: Some(p), .. }
                        | Op::IncDecCv { place: p, .. }
                        | Op::AssignOpCv { place: p, .. }
                        | Op::AssignOpDimCv { place: p, .. }
                        | Op::IncDecDimCv { place: p, .. }
                        | Op::FetchDimCv { place: p, .. }
                        | Op::FetchDimsCv { place: p, .. }
                        | Op::IssetDimCv { place: p, .. }
                        | Op::AssignDimCv { place: p, .. }
                        | Op::FetchStaticProp { place: p, .. }
                        | Op::FetchGlobal { place: p, .. }
                        | Op::AssignStaticProp { place: p, .. }
                        | Op::AssignOpStaticProp { place: p, .. }
                        | Op::IssetStaticProp { place: p, .. }
                        | Op::ConcatCv { place: p, .. }
                        | Op::AssignDimsCv { place: p, .. }
                        | Op::AssignOpDimsCv { place: p, .. }
                        | Op::IncDecCvCmpJmp { place: p, .. }
                        | Op::AssignOpCvCmpJmp { place: p, .. }
                        | Op::AssignOpCvJmp { place: p, .. }
                        | Op::FetchObjProp { place: p, .. }
                        | Op::FetchObjPropDim { place: p, .. }
                        | Op::AssignObjProp { place: p, .. }
                        | Op::AssignObjPropDim { place: p, .. }
                        | Op::AssignOpObjProp { place: p, .. }
                        | Op::AssignOpObjPropDim { place: p, .. }
                        | Op::IncDecObjProp { place: p, .. }
                        | Op::IncDecObjPropDim { place: p, .. }
                        | Op::IssetObjProp { place: p, .. }
                        | Op::IssetObjPropDim { place: p, .. }
                            if *p == pid_u32 =>
                        {
                            used_in_non_plain_assign = true;
                            break;
                        }
                        _ => {}
                    }
                }
                if used_in_non_plain_assign {
                    if let Some(c) = read_counts.get_mut(cv as usize) {
                        *c += 1;
                    }
                }
            }
        }

        let mut place_clone = place.clone();
        for_each_place_operand_mut(&mut place_clone, |operand| {
            if let Operand::Cv(cv) = operand {
                if let Some(c) = read_counts.get_mut(*cv as usize) {
                    *c += 1;
                }
            }
        });
    }

    let mut changed = false;

    // Eliminate assignments to unread variables
    for op in &mut op_array.ops {
        match op {
            Op::AssignCv { cv, val, dst } => {
                if (*cv as usize) >= num_param_cvs as usize
                    && read_counts.get(*cv as usize).copied().unwrap_or(0) == 0
                {
                    *op = match dst {
                        Some(d) => Op::Move { src: *val, dst: *d },
                        None => match val {
                            Operand::Tmp(t) => Op::Free(Operand::Tmp(*t)),
                            _ => Op::Nop,
                        },
                    };
                    changed = true;
                }
            }
            Op::Assign { place, val, dst } => {
                let pid = *place as usize;
                if let Some(p) = op_array.places.get(pid) {
                    if let PlaceBase::Cv(cv) = p.base {
                        if p.segs.is_empty()
                            && (cv as usize) >= num_param_cvs as usize
                            && read_counts.get(cv as usize).copied().unwrap_or(0) == 0
                        {
                            *op = match dst {
                                Some(d) => Op::Move { src: *val, dst: *d },
                                None => match val {
                                    Operand::Tmp(t) => Op::Free(Operand::Tmp(*t)),
                                    _ => Op::Nop,
                                },
                            };
                            changed = true;
                        }
                    }
                }
            }
            Op::BinAssignCv {
                op: bin_op,
                a,
                b,
                dst,
                cv,
            } => {
                if (*cv as usize) >= num_param_cvs as usize
                    && read_counts.get(*cv as usize).copied().unwrap_or(0) == 0
                {
                    *op = Op::Bin {
                        op: *bin_op,
                        a: *a,
                        b: *b,
                        dst: *dst,
                    };
                    changed = true;
                }
            }
            Op::FetchConstTo {
                name,
                fallback,
                dst,
                to,
            } => {
                if (*to as usize) >= num_param_cvs as usize
                    && read_counts.get(*to as usize).copied().unwrap_or(0) == 0
                {
                    *op = Op::FetchConst {
                        name: *name,
                        fallback: *fallback,
                        dst: *dst,
                    };
                    changed = true;
                }
            }
            Op::FetchClassConstTo {
                class,
                name,
                dst,
                to,
            } => {
                if (*to as usize) >= num_param_cvs as usize
                    && read_counts.get(*to as usize).copied().unwrap_or(0) == 0
                {
                    *op = Op::FetchClassConst {
                        class: class.clone(),
                        name: Operand::Lit(*name),
                        dst: *dst,
                    };
                    changed = true;
                }
            }
            _ => {}
        }
    }

    changed
}

/// Pass 12: Adjusts used stack by compacting unused CV slots (ZEND_OPTIMIZER_PASS_12).
pub(crate) fn compact_cvs(op_array: &mut OpArray) -> bool {
    let num_cvs = op_array.cv_names.len();
    if num_cvs == 0 || has_dynamic_var_access(op_array) {
        return false;
    }

    let mut is_referenced = vec![false; num_cvs];

    // Parameters cannot be moved/removed
    let mut num_param_cvs = 0u32;
    for op in &op_array.ops {
        match op {
            Op::Recv { cv, .. }
            | Op::RecvInit { cv, .. }
            | Op::RecvDefault { cv, .. }
            | Op::RecvVariadic { cv, .. } => {
                num_param_cvs = num_param_cvs.max(*cv + 1);
            }
            _ => {}
        }
    }
    for i in 0..(num_param_cvs as usize).min(num_cvs) {
        is_referenced[i] = true;
    }

    // Closures
    for f in op_array.decls.functions.borrow().iter() {
        for u in &f.uses {
            if (u.parent_cv as usize) < num_cvs {
                is_referenced[u.parent_cv as usize] = true;
            }
        }
    }

    // Ops
    for op in &op_array.ops {
        let mut op_clone = op.clone();
        for_each_cv_mut(&mut op_clone, |c| {
            if let Some(r) = is_referenced.get_mut(*c as usize) {
                *r = true;
            }
        });
    }

    // Places
    for place in &op_array.places {
        let mut place_clone = place.clone();
        for_each_place_cv_mut(&mut place_clone, |c| {
            if let Some(r) = is_referenced.get_mut(*c as usize) {
                *r = true;
            }
        });
    }

    if is_referenced.iter().all(|&r| r) {
        return false;
    }

    // Build remap and new_cv_names
    let mut remap = vec![None; num_cvs];
    let mut new_cv_names = Vec::new();
    let mut next_id = 0u32;

    for i in 0..num_param_cvs {
        if (i as usize) < num_cvs {
            remap[i as usize] = Some(i);
            new_cv_names.push(op_array.cv_names[i as usize].clone());
            next_id = i + 1;
        }
    }

    for (old_id, &refed) in is_referenced.iter().enumerate() {
        let old_u32 = old_id as u32;
        if old_u32 >= num_param_cvs && refed {
            remap[old_id] = Some(next_id);
            new_cv_names.push(op_array.cv_names[old_id].clone());
            next_id += 1;
        }
    }

    // Remap CVs in ops
    for op in &mut op_array.ops {
        for_each_cv_mut(op, |c| {
            if let Some(Some(new_c)) = remap.get(*c as usize) {
                *c = *new_c;
            }
        });
    }

    // Remap CVs in places
    for place in &mut op_array.places {
        for_each_place_cv_mut(place, |c| {
            if let Some(Some(new_c)) = remap.get(*c as usize) {
                *c = *new_c;
            }
        });
    }

    op_array.cv_names = new_cv_names;
    true
}

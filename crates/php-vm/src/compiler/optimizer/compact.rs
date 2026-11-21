// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Compaction passes:
//! - Pass 9: Temporary variable compaction (ZEND_OPTIMIZER_PASS_9)
//! - Pass 10: NOP removal with full label and exception table remapping (ZEND_OPTIMIZER_PASS_10)
//! - Pass 11: Literal table compaction & deduplication (ZEND_OPTIMIZER_PASS_11)
//! - Pass 12: Adjust used stack / compact CV slots (ZEND_OPTIMIZER_PASS_12)
//! - Pass 13: Remove unused variables (ZEND_OPTIMIZER_PASS_13)

use super::{
    def_tmp, for_each_label_mut, for_each_operand_mut, for_each_place_operand_mut, literals_equal,
    OptContext,
};
use crate::opcode::{ArgSrc, ClassSpec, Op, OpArray, Operand, Place, PlaceBase, Seg};
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
    for at in &mut op_array.dropped_tmps {
        if (*at as usize) < remap.len() {
            *at = remap[*at as usize];
        }
    }
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
        if let Some(t) = super::array_build_tmp_mut(&mut op_clone) {
            mark_tmp(*t);
        }
        // `FE_FETCH` writes its value (and key) temporaries through raw
        // fields that neither `def_tmp` nor `for_each_operand_mut` visit.
        if let Op::FeFetch { val, key, .. } = op {
            mark_tmp(*val);
            if let Some(k) = key {
                mark_tmp(*k);
            }
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
        if let Some(t) = super::array_build_tmp_mut(op) {
            remap_tmp(t);
        }

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

            // A foreach whose value temporary kept its old number read
            // another slot (`null`) once lower temporaries were compacted
            // away (gh24577).
            Op::FeFetch { val, key, .. } => {
                remap_tmp(val);
                if let Some(k) = key {
                    remap_tmp(k);
                }
            }

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
            | Op::FuncGetArgs { dst, .. }
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

        for_each_literal_index_mut(op, remap_lit);
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

/// Visits every raw literal index an instruction holds outside its
/// [`Operand::Lit`] operands (which [`for_each_operand_mut`] visits): class
/// names, function/constant names, named-argument names, diagnostics, ...
///
/// The match is exhaustive on purpose: a new op with a literal field must be
/// listed here, or literal compaction (pass 11) would leave it pointing at a
/// stale index (a `name: ?` placeholder read the wrong literal and failed
/// with "Unknown named parameter $Int(8)").
pub(crate) fn for_each_literal_index_mut(op: &mut Op, mut f: impl FnMut(&mut u32)) {
    match op {
        Op::New { class, .. }
        | Op::InitStaticCall { class, .. }
        | Op::FetchClassConst { class, .. }
        | Op::FetchClassName { class, .. }
        | Op::InstanceOf { class, .. } => {
            if let ClassSpec::Named(l) = class {
                f(l);
            }
        }
        Op::FetchClassConstTo { class, name, .. } => {
            if let ClassSpec::Named(l) = class {
                f(l);
            }
            f(name);
        }
        Op::InitFCall {
            name,
            fallback,
            display,
        } => {
            f(name);
            if let Some(fb) = fallback {
                f(fb);
            }
            f(display);
        }
        // Formed after the optimizer (`specialise::fuse_frameless_calls`).
        Op::FramelessICall(fc) => {
            f(&mut fc.name);
            f(&mut fc.display);
        }
        Op::FetchConst { name, fallback, .. } | Op::FetchConstTo { name, fallback, .. } => {
            f(name);
            if let Some(fb) = fallback {
                f(fb);
            }
        }
        Op::SendPlaceholder { name } => {
            if let Some(n) = name {
                f(n);
            }
        }
        Op::SendNamed { name, .. }
        | Op::CallablePartial { name, .. }
        | Op::Defined { name, .. }
        | Op::DeclareConst { name, .. }
        | Op::Global { name, .. } => f(name),
        Op::InArray { set, .. } => f(set),
        Op::Catch { classes, .. } => {
            for c in classes.iter_mut() {
                f(c);
            }
        }
        Op::TypeAssertArray { msg, .. } | Op::Diag { msg, .. } => f(msg),
        Op::UserOpcode { opcode } => f(opcode),
        Op::InitParentHook { prop, .. } => f(prop),
        Op::Abs { .. }
        | Op::ArrayAdd { .. }
        | Op::ArrayAddRef { .. }
        | Op::ArrayKeyExists { .. }
        | Op::ArrayUnpack { .. }
        | Op::AssertCheck { .. }
        | Op::Assign { .. }
        | Op::AssignCv { .. }
        | Op::AssignDimCv { .. }
        | Op::AssignDimsCv { .. }
        | Op::AssignObjProp { .. }
        | Op::AssignObjPropDim { .. }
        | Op::AssignOp { .. }
        | Op::AssignOpCv { .. }
        | Op::AssignOpCvCmpJmp { .. }
        | Op::AssignOpCvJmp { .. }
        | Op::AssignOpDimCv { .. }
        | Op::AssignOpDimsCv { .. }
        | Op::AssignOpObjProp { .. }
        | Op::AssignOpObjPropDim { .. }
        | Op::AssignOpStaticProp { .. }
        | Op::AssignRef { .. }
        | Op::AssignStaticProp { .. }
        | Op::BeginSilence { .. }
        | Op::Bin { .. }
        | Op::BinAssignCv { .. }
        | Op::BinAssignCvCmpJmp { .. }
        | Op::BinAssignCvJmp { .. }
        | Op::BindStatic { .. }
        | Op::CallableConvert { .. }
        | Op::Cast { .. }
        | Op::Clone { .. }
        | Op::CmpJmp { .. }
        | Op::Coalesce { .. }
        | Op::ConcatCv { .. }
        | Op::ConstArrayUnpack { .. }
        | Op::ConstDimCheck { .. }
        | Op::ConstPropCheck { .. }
        | Op::Count { .. }
        | Op::DeclareAnonClass { .. }
        | Op::DeclareClass(..)
        | Op::DeclareFunction(..)
        | Op::DiscardFinally { .. }
        | Op::DoCall { .. }
        | Op::Echo(..)
        | Op::Empty { .. }
        | Op::EndSilence { .. }
        | Op::Exit(..)
        | Op::FastCall { .. }
        | Op::FastRet { .. }
        | Op::FeFetch { .. }
        | Op::FeFree { .. }
        | Op::FeFreeOnReturn { .. }
        | Op::FeReset { .. }
        | Op::Fetch { .. }
        | Op::FetchDimCv { .. }
        | Op::FetchDimsCv { .. }
        | Op::FetchGlobal { .. }
        | Op::FetchObjProp { .. }
        | Op::FetchObjPropDim { .. }
        | Op::FetchStaticProp { .. }
        | Op::FetchThis { .. }
        | Op::Free(..)
        | Op::FuncGetArgs { .. }
        | Op::FuncNumArgs { .. }
        | Op::GeneratorCreate
        | Op::GetCalledClass { .. }
        | Op::GetClass { .. }
        | Op::GetType { .. }
        | Op::IncDec { .. }
        | Op::IncDecCv { .. }
        | Op::IncDecCvCmpJmp { .. }
        | Op::IncDecDimCv { .. }
        | Op::IncDecObjProp { .. }
        | Op::IncDecObjPropDim { .. }
        | Op::Include { .. }
        | Op::InitArray { .. }
        | Op::InitDynCall { .. }
        | Op::InitMethodCall { .. }
        | Op::InitStatic { .. }
        | Op::Isset { .. }
        | Op::IssetDimCv { .. }
        | Op::IssetObjProp { .. }
        | Op::IssetObjPropDim { .. }
        | Op::IssetStaticProp { .. }
        | Op::Jmp(..)
        | Op::JmpNull { .. }
        | Op::JmpNz(..)
        | Op::JmpNzEx { .. }
        | Op::JmpSet { .. }
        | Op::JmpZ(..)
        | Op::JmpZEx { .. }
        | Op::KeyExists { .. }
        | Op::ListFetchRef { .. }
        | Op::MakeClosure { .. }
        | Op::MakeRef { .. }
        | Op::Match { .. }
        | Op::MatchError { .. }
        | Op::Move { .. }
        | Op::Nop
        | Op::Recv { .. }
        | Op::RecvDefault { .. }
        | Op::RecvInit { .. }
        | Op::RecvVariadic { .. }
        | Op::Rethrow
        | Op::Return { .. }
        | Op::ReturnByRefValue { .. }
        | Op::ReturnRef { .. }
        | Op::Rope { .. }
        | Op::Send { .. }
        | Op::SendUnpack { .. }
        | Op::Strlen { .. }
        | Op::Throw(..)
        | Op::Ticks(..)
        | Op::TypeCheck { .. }
        | Op::Unary { .. }
        | Op::Unset { .. }
        | Op::VerifyReturn { .. }
        | Op::Yield { .. }
        | Op::YieldByRefValue { .. }
        | Op::YieldFrom { .. }
        | Op::YieldRef { .. } => {}
    }
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
pub(crate) fn has_dynamic_var_access(op_array: &OpArray) -> bool {
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

/// The CV slots bound before the first op: one past the highest parameter
/// (from its `RECV`) or closure `use` binding.
fn bound_cv_prefix(op_array: &OpArray, ctx: &OptContext<'_>) -> u32 {
    let mut prefix = ctx.bound_cv_prefix();
    for op in &op_array.ops {
        match op {
            Op::Recv { cv, .. }
            | Op::RecvInit { cv, .. }
            | Op::RecvDefault { cv, .. }
            | Op::RecvVariadic { cv, .. } => {
                prefix = prefix.max(cv.saturating_add(1));
            }
            _ => {}
        }
    }
    prefix
}

/// Calls `f` with each place `op` names: its `place` (or a foreach's by
/// reference), a by-reference argument source (`Send`, `SendNamed`,
/// `array_key_exists()`'s array, a frameless call's argument). Every
/// `PlaceId` of an op is one of these.
pub(crate) fn for_each_op_place(op: &Op, mut f: impl FnMut(u32)) {
    let mut src = |s: &ArgSrc| {
        if let ArgSrc::Place(p) | ArgSrc::TempPlace(p) = s {
            f(*p);
        }
    };
    match op {
        Op::ArrayAddRef { place: p, .. }
        | Op::Assign { place: p, .. }
        | Op::AssignOp { place: p, .. }
        | Op::IncDec { place: p, .. }
        | Op::AssignRef { place: p, .. }
        | Op::MakeRef { place: p, .. }
        | Op::ListFetchRef { place: p, .. }
        | Op::Fetch { place: p, .. }
        | Op::Isset { place: p, .. }
        | Op::Empty { place: p, .. }
        | Op::Unset { place: p, .. }
        | Op::ReturnRef { place: p, .. }
        | Op::YieldRef { place: p, .. }
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
        | Op::FeReset { place: Some(p), .. } => src(&ArgSrc::Place(*p)),
        Op::Send { src: s }
        | Op::SendNamed { src: s, .. }
        | Op::ArrayKeyExists { array: s, .. } => {
            src(s);
        }
        Op::FramelessICall(fc) => {
            for s in &fc.args {
                src(s);
            }
        }
        _ => {}
    }
}

/// True when an instruction's result can never need a user-visible
/// destructor at an arbitrary time: numbers, bools, strings and plain
/// arrays.  Objects, closures, `NEW`, calls and dynamic fetches are all
/// left out; they may carry a `__destruct` or wrap a resource.
fn result_is_inert(op: &Op) -> bool {
    match op {
        Op::Bin { .. }
        | Op::Unary { .. }
        | Op::Rope { .. }
        | Op::Isset { .. }
        | Op::Empty { .. }
        | Op::InstanceOf { .. }
        | Op::InArray { .. }
        | Op::KeyExists { .. }
        | Op::ArrayKeyExists { .. }
        | Op::Defined { .. }
        | Op::TypeCheck { .. }
        | Op::Count { .. }
        | Op::Strlen { .. }
        | Op::FuncNumArgs { .. }
        | Op::GetClass { .. }
        | Op::GetType { .. }
        | Op::IncDec { .. }
        | Op::IncDecCv { .. }
        | Op::AssignOp { .. }
        | Op::AssignOpCv { .. }
        | Op::BinAssignCv { .. }
        | Op::JmpZEx { .. }
        | Op::JmpNzEx { .. }
        | Op::JmpNull { .. } => true,
        Op::Cast { kind, .. } => *kind != crate::opcode::CastKind::Object,
        _ => false,
    }
}

/// True when an operand's value provably needs no destructor at an
/// arbitrary time: a literal, or a temporary whose single defining
/// instruction is inert.  `Move` chains are followed; anything ambiguous
/// counts as possibly an object. An array literal is as inert as what it
/// is built from ([`build_is_inert`]); `kept` says whether a variable
/// read by the op at an index keeps what it holds until the function
/// ends.
fn operand_is_inert(
    op_array: &OpArray,
    tmp_def: &[usize],
    kept: &dyn Fn(usize, u32) -> bool,
    mut val: Operand,
) -> bool {
    for _ in 0..32 {
        match val {
            Operand::Lit(_) => return true,
            Operand::Tmp(t) | Operand::TmpKeep(t) => {
                let d = match tmp_def.get(t as usize) {
                    Some(&d) if d >= usize::MAX - 1 => return false,
                    Some(&d) => d,
                    None => return false,
                };
                let op = &op_array.ops[d];
                if let Op::InitArray { dst } = op {
                    return build_is_inert(op_array, tmp_def, kept, *dst);
                }
                if result_is_inert(op) {
                    return true;
                }
                match op {
                    Op::Move { src, .. } => val = *src,
                    _ => return false,
                }
            }
            _ => return false,
        }
    }
    false
}

/// Whether the array literal built in temporary `t` is never the last
/// holder of something with a destructor: every element is a literal, a
/// value that is inert itself, or a variable that keeps what it holds
/// until the function ends (`kept`). Dropping the array's only store
/// frees it at once, and with it an object nothing else holds: PHP keeps
/// `$a = [new D];` to the end of the function. An element added by
/// reference or unpacked from a value is not looked into.
fn build_is_inert(
    op_array: &OpArray,
    tmp_def: &[usize],
    kept: &dyn Fn(usize, u32) -> bool,
    t: u32,
) -> bool {
    op_array.ops.iter().enumerate().all(|(at, op)| match op {
        Op::ArrayAdd { arr, val, .. } if *arr == t => match val {
            Operand::Cv(c) => kept(at, *c),
            other => operand_is_inert(op_array, tmp_def, kept, *other),
        },
        Op::ArrayAddRef { arr, .. } | Op::ArrayUnpack { arr, .. } => *arr != t,
        _ => true,
    })
}

/// The variables among `start` that the function never writes: every op
/// naming one reads it as a plain operand, and no place has it as its
/// base. (Not meaningful with dynamic variable access, which the callers
/// rule out.)
fn never_written(op_array: &OpArray, mut stable: Vec<bool>) -> Vec<bool> {
    for op in &op_array.ops {
        if matches!(
            op,
            Op::Recv { .. } | Op::RecvInit { .. } | Op::RecvDefault { .. }
        ) {
            continue;
        }
        // Every mention that is not a plain operand read may write.
        let mut mentioned = Vec::new();
        for_each_cv_mut(&mut op.clone(), |c| mentioned.push(*c));
        let mut read = Vec::new();
        for_each_operand_mut(&mut op.clone(), |operand| {
            if let Operand::Cv(c) = operand {
                read.push(*c);
            }
        });
        for c in mentioned {
            if let Some(i) = read.iter().position(|&r| r == c) {
                read.swap_remove(i);
            } else if let Some(slot) = stable.get_mut(c as usize) {
                *slot = false;
            }
        }
    }
    for place in &op_array.places {
        if let PlaceBase::Cv(cv) = place.base {
            if let Some(slot) = stable.get_mut(cv as usize) {
                *slot = false;
            }
        }
    }
    stable
}

/// The locals with a single assignment (`$v = x`) that nothing else writes:
/// every other op naming one reads it as a plain operand, and no place has
/// it as its base. Such a variable holds that value from its assignment to
/// the end of the function.
fn single_assignments(op_array: &OpArray, ctx: &OptContext<'_>) -> Vec<bool> {
    let num_cvs = op_array.cv_names.len();
    let mut assigns = vec![0u32; num_cvs];
    let mut other = vec![false; num_cvs];
    for slot in other
        .iter_mut()
        .take(bound_cv_prefix(op_array, ctx) as usize)
    {
        *slot = true;
    }
    for op in &op_array.ops {
        let mut mentioned = Vec::new();
        for_each_cv_mut(&mut op.clone(), |c| mentioned.push(*c));
        let mut read = Vec::new();
        for_each_operand_mut(&mut op.clone(), |operand| {
            if let Operand::Cv(c) = operand {
                read.push(*c);
            }
        });
        for c in mentioned {
            if let Some(i) = read.iter().position(|&r| r == c) {
                read.swap_remove(i);
            } else if matches!(op, Op::AssignCv { cv, .. } if *cv == c) {
                if let Some(n) = assigns.get_mut(c as usize) {
                    *n += 1;
                }
            } else if let Some(slot) = other.get_mut(c as usize) {
                *slot = true;
            }
        }
    }
    for place in &op_array.places {
        if let PlaceBase::Cv(cv) = place.base {
            if let Some(slot) = other.get_mut(cv as usize) {
                *slot = true;
            }
        }
    }
    assigns
        .iter()
        .zip(&other)
        .map(|(&n, &written)| n == 1 && !written)
        .collect()
}

/// The scalar-typed parameters the function never writes (only the `RECV`
/// binds them), by compiled variable: each holds a null, a bool, a number
/// or a string for the whole call. (Not meaningful with dynamic variable
/// access, which the callers rule out.)
pub(crate) fn stable_scalar_params(op_array: &OpArray, ctx: &OptContext<'_>) -> Vec<bool> {
    let mut stable_scalar = vec![false; op_array.cv_names.len()];
    for &cv in ctx.scalar_params {
        if let Some(slot) = stable_scalar.get_mut(cv as usize) {
            *slot = true;
        }
    }
    never_written(op_array, stable_scalar)
}

/// The variables that only ever hold an array of values without a
/// destructor, built in place: every op naming one reads it as a plain
/// operand, or stores a literal or a scalar-typed parameter the function
/// never writes (`stable_scalar`) under an int or string literal key or by
/// appending (`$v[k] = x`, `$v[] = x`). Not the bound variables, nor one a
/// closure declared here captures. (A place of the variable that no such
/// store names counts against it, whatever names it.)
fn scalar_array_vars(
    op_array: &OpArray,
    ctx: &OptContext<'_>,
    stable_scalar: &[bool],
) -> Vec<bool> {
    let num_cvs = op_array.cv_names.len();
    let mut only = vec![true; num_cvs];
    let mut rule_out = |cv: u32| {
        if let Some(slot) = only.get_mut(cv as usize) {
            *slot = false;
        }
    };
    for cv in 0..bound_cv_prefix(op_array, ctx) {
        rule_out(cv);
    }
    for f in op_array.decls.functions.borrow().iter() {
        for u in &f.uses {
            rule_out(u.parent_cv);
        }
    }
    // Per place: the ops naming it, and how many of them are element
    // stores of scalars.
    let mut named = vec![(0u32, 0u32); op_array.places.len()];
    for op in &op_array.ops {
        let scalar_store = match op {
            Op::Assign {
                place,
                val,
                dst: None,
            } => op_array.places.get(*place as usize).is_some_and(|p| {
                let key_ok = match &p.segs[..] {
                    [Seg::Append] => true,
                    [Seg::Dim(Operand::Lit(l))] => matches!(
                        op_array.literals.get(*l as usize),
                        Some(Value::Int(_) | Value::Str(_))
                    ),
                    _ => false,
                };
                let val_ok = match val {
                    Operand::Lit(_) => true,
                    Operand::Cv(c) => stable_scalar.get(*c as usize).copied().unwrap_or(false),
                    _ => false,
                };
                matches!(p.base, PlaceBase::Cv(_)) && key_ok && val_ok
            }),
            _ => false,
        };
        for_each_op_place(op, |p| {
            if let Some((ops, stores)) = named.get_mut(p as usize) {
                *ops += 1;
                *stores += u32::from(scalar_store);
            }
        });
        // Every mention that is not a plain operand read may write.
        let mut mentioned = Vec::new();
        for_each_cv_mut(&mut op.clone(), |c| mentioned.push(*c));
        let mut read = Vec::new();
        for_each_operand_mut(&mut op.clone(), |operand| {
            if let Operand::Cv(c) = operand {
                read.push(*c);
            }
        });
        for c in mentioned {
            if let Some(i) = read.iter().position(|&r| r == c) {
                read.swap_remove(i);
            } else {
                rule_out(c);
            }
        }
    }
    // A place of the variable is one such store's own, or counts against
    // it; one no op names any more (left behind by a read a pass folded
    // away) reads nothing.
    for (place, &(ops, stores)) in op_array.places.iter().zip(&named) {
        if let PlaceBase::Cv(cv) = place.base {
            if ops != 0 && (ops, stores) != (1, 1) {
                rule_out(cv);
            }
        }
    }
    only
}

/// Pass 13: Removes assignments to variables that are never read and never referenced (ZEND_OPTIMIZER_PASS_13).
pub(crate) fn remove_unused_variables(op_array: &mut OpArray, ctx: &OptContext<'_>) -> bool {
    let num_cvs = op_array.cv_names.len();
    if num_cvs == 0 || has_dynamic_var_access(op_array) {
        return false;
    }

    // Parameters and closure `use` bindings cannot be removed: a write to a
    // by-reference one is seen by the caller or the enclosing scope
    // (`use (&$v) { $v = 0; }`, jit/assign_dim_op_006).
    let num_param_cvs = bound_cv_prefix(op_array, ctx);

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

    // Their value has no destructor.
    let stable_scalar = stable_scalar_params(op_array, ctx);
    // `$cv[k] = v` with an offset and a value that have no destructor (a
    // literal, a stable scalar parameter): a write of the variable, not a
    // read, which goes with it when nothing else reads the variable
    // (php-src's dce.c drops an ASSIGN_DIM of a variable that does not
    // escape and an OP_DATA without MAY_HAVE_DTOR).
    // The write must also do just that: the variable holds an array or
    // nothing (every assignment of it is a literal array, or a variable
    // that only ever holds an array of scalars: a scalar or a string in
    // it makes the write throw or warn), and the offset is an int or
    // string literal or absent (a float or null one warns).
    let scalar_arrays = scalar_array_vars(op_array, ctx, &stable_scalar);
    let assigned = super::dce::Assigned::of(op_array, ctx);
    // The temporaries an array literal is built in (`InitArray` their only
    // definition).
    let mut tmp_defs = vec![0u32; op_array.num_tmps as usize];
    super::dce::count_defs(op_array, &mut tmp_defs);
    let array_tmp = |t: u32| {
        tmp_defs.get(t as usize) == Some(&1)
            && op_array
                .ops
                .iter()
                .any(|op| matches!(op, Op::InitArray { dst } if *dst == t))
    };
    // A key that is, or may be, the largest int: an append after it
    // throws ("the next element is already occupied").
    let may_be_last_key = |k: &Operand| match k {
        Operand::Lit(l) => match op_array.literals.get(*l as usize) {
            Some(Value::Int(i)) => *i == i64::MAX,
            Some(Value::Str(s)) => matches!(
                php_runtime::ArrayKey::from_zstr(s),
                php_runtime::ArrayKey::Int(i64::MAX)
            ),
            _ => true,
        },
        _ => true,
    };
    // Whether the array built in `t` has every key appended or fixed
    // below the largest int.
    let build_keeps_room = |t: u32| {
        op_array.ops.iter().all(|op| match op {
            Op::ArrayAdd { arr, key, .. } if *arr == t => {
                key.as_ref().is_none_or(|k| !may_be_last_key(k))
            }
            other => super::array_build_tmp_mut(&mut other.clone()).is_none_or(|a| *a != t),
        })
    };
    // The variables whose array always has room for an append: every
    // assignment is a literal array without the largest int key or an
    // array built in place with such keys, and every store names such a
    // key (a copy of another variable's array is not looked into).
    let mut has_room = vec![true; num_cvs];
    let mut array_only = vec![true; num_cvs];
    for op in &op_array.ops {
        let target = match op {
            Op::AssignCv { cv, val, .. } => Some((*cv, *val)),
            Op::Assign { place, val, .. } => {
                op_array
                    .places
                    .get(*place as usize)
                    .and_then(|p| match p.base {
                        PlaceBase::Cv(cv) if p.segs.is_empty() => Some((cv, *val)),
                        _ => None,
                    })
            }
            _ => None,
        };
        if let Some((cv, val)) = target {
            let lit_array = matches!(val, Operand::Lit(l)
                if matches!(op_array.literals.get(l as usize), Some(Value::Array(_))));
            let array_var = matches!(val, Operand::Cv(c)
                if scalar_arrays.get(c as usize).copied().unwrap_or(false));
            // (An array literal built in place is an array as well.)
            let built = matches!(val, Operand::Tmp(t) if array_tmp(t));
            let room = match val {
                Operand::Lit(l) => matches!(op_array.literals.get(l as usize),
                    Some(Value::Array(a)) if a.get_int(i64::MAX).is_none()),
                Operand::Tmp(t) => built && build_keeps_room(t),
                _ => false,
            };
            if !room {
                if let Some(slot) = has_room.get_mut(cv as usize) {
                    *slot = false;
                }
            }
            if !lit_array && !array_var && !built {
                if let Some(slot) = array_only.get_mut(cv as usize) {
                    *slot = false;
                }
            }
        }
    }
    for op in &op_array.ops {
        let Op::Assign { place, .. } = op else {
            continue;
        };
        if let Some(p) = op_array.places.get(*place as usize) {
            if let (PlaceBase::Cv(cv), [Seg::Dim(k)]) = (&p.base, &p.segs[..]) {
                if may_be_last_key(k) {
                    if let Some(slot) = has_room.get_mut(*cv as usize) {
                        *slot = false;
                    }
                }
            }
        }
    }
    let dim_ok = |seg: &Seg| match seg {
        Seg::Append => true,
        Seg::Dim(Operand::Lit(l)) => matches!(
            op_array.literals.get(*l as usize),
            Some(Value::Int(_) | Value::Str(_))
        ),
        _ => false,
    };
    // A variable that only ever holds an array of such values has no
    // destructor either; the op at `at` must find it assigned (reading
    // another warns).
    let no_dtor = |at: usize, o: Operand| match o {
        Operand::Lit(_) => true,
        Operand::Cv(c) => {
            stable_scalar.get(c as usize).copied().unwrap_or(false)
                || (scalar_arrays.get(c as usize).copied().unwrap_or(false) && assigned.at(at, c))
        }
        Operand::Unused | Operand::Tmp(_) | Operand::TmpKeep(_) => false,
    };
    // (Before specialisation: `Assign` to a one-offset place of the
    // variable; `AssignDimCv` comes later.)
    let pure_dim_write = |at: usize, op: &Op| {
        let Op::Assign {
            place,
            val,
            dst: None,
        } = op
        else {
            return false;
        };
        match op_array.places.get(*place as usize) {
            Some(p) => match (&p.base, &p.segs[..]) {
                (PlaceBase::Cv(cv), [seg]) => {
                    // An append goes only with a constant value (sccp.c
                    // keeps `ASSIGN_DIM ... NEXT` of a value it does not
                    // know) and when it cannot throw.
                    let append_ok = !matches!(seg, Seg::Append)
                        || (matches!(val, Operand::Lit(_))
                            && has_room.get(*cv as usize).copied().unwrap_or(false));
                    array_only.get(*cv as usize).copied().unwrap_or(false)
                        && dim_ok(seg)
                        && append_ok
                        && no_dtor(at, *val)
                }
                _ => false,
            },
            None => false,
        }
    };

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

    // The places some op names: a place no op names any more (left behind
    // by an op a pass folded away, as `$a += $b` once SCCP computed it)
    // reads nothing.
    let mut named = vec![false; op_array.places.len()];
    let impure = op_array
        .ops
        .iter()
        .enumerate()
        .filter(|(at, op)| !pure_dim_write(*at, op));
    for (_, op) in impure {
        for_each_op_place(op, |p| {
            if let Some(slot) = named.get_mut(p as usize) {
                *slot = true;
            }
        });
    }

    // Count reads from places
    for (pid, place) in op_array.places.iter().enumerate() {
        let pid_u32 = pid as u32;
        if !named.get(pid).copied().unwrap_or(true) {
            continue;
        }
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

    // Dropping an assignment to an unread variable changes when the value
    // it produced is destroyed.  That is unobservable unless a destructor
    // can run: an object (or a resource, whose shutdown hooks users can
    // reach).  So a variable that ever receives a value which may be an
    // object keeps its assignments; numbers, bools, strings and plain
    // arrays lose them, exactly like php-src's dce.c, whose SCCP type
    // flags single out IS_OBJECT for the same reason.
    //
    // A temporary's type comes from its defining instruction: `CONCAT`
    // yields a string, `NEW` an object.  `Move`s are followed to their
    // source.  Anything unprovable counts as possibly an object.
    const NO_DEF: usize = usize::MAX;
    const MANY_DEFS: usize = usize::MAX - 1;
    let mut tmp_def: Vec<usize> = vec![NO_DEF; op_array.num_tmps as usize];
    for (i, op) in op_array.ops.iter().enumerate() {
        if let Some(t) = def_tmp(op) {
            if let Some(slot) = tmp_def.get_mut(t as usize) {
                *slot = if *slot == NO_DEF {
                    i
                } else if *slot == MANY_DEFS || *slot != i {
                    // A temporary defined in more than one place may hold
                    // anything any of those places produced.
                    MANY_DEFS
                } else {
                    *slot
                };
            }
        }
    }
    // A variable keeps what it holds until the function ends when nothing
    // writes it after the read at `at`: a bound variable the function
    // never writes, one that only ever holds scalars or arrays of scalars
    // (nothing to keep), or a local with one assignment, made before `at`.
    let mut bound = vec![false; num_cvs];
    for &(cv, by_ref) in ctx.bound_cvs {
        if let (false, Some(slot)) = (by_ref, bound.get_mut(cv as usize)) {
            *slot = true;
        }
    }
    let unwritten = never_written(op_array, bound);
    let assigned_once = single_assignments(op_array, ctx);
    let kept = |at: usize, c: u32| {
        let is = |set: &[bool]| set.get(c as usize).copied().unwrap_or(false);
        is(&stable_scalar)
            || is(&scalar_arrays)
            || is(&unwritten)
            || (is(&assigned_once) && assigned.at(at, c))
    };
    let mut may_hold_object = vec![false; num_cvs];
    for op in &op_array.ops {
        let target = match op {
            Op::AssignCv { cv, val, .. } => Some((*cv, *val)),
            Op::Assign { place, val, .. } => {
                op_array
                    .places
                    .get(*place as usize)
                    .and_then(|p| match p.base {
                        PlaceBase::Cv(cv) if p.segs.is_empty() => Some((cv, *val)),
                        _ => None,
                    })
            }
            _ => None,
        };
        if let Some((cv, val)) = target {
            // (A scalar parameter, or an array of scalars built in place.)
            let inert_var = matches!(val, Operand::Cv(c)
                if stable_scalar.get(c as usize).copied().unwrap_or(false)
                    || scalar_arrays.get(c as usize).copied().unwrap_or(false));
            if !inert_var && !operand_is_inert(op_array, &tmp_def, &kept, val) {
                if let Some(m) = may_hold_object.get_mut(cv as usize) {
                    *m = true;
                }
            }
        }
    }
    let removable = |cv: u32| {
        cv >= num_param_cvs
            && read_counts.get(cv as usize).copied().unwrap_or(0) == 0
            && !may_hold_object.get(cv as usize).copied().unwrap_or(true)
    };

    let mut changed = false;

    // Eliminate assignments to unread variables
    let pure_dim_writes: Vec<bool> = op_array
        .ops
        .iter()
        .enumerate()
        .map(|(at, op)| pure_dim_write(at, op))
        .collect();
    for (at, (op, pure)) in op_array.ops.iter_mut().zip(pure_dim_writes).enumerate() {
        match op {
            Op::Assign { place, .. } if pure => {
                let cv = match op_array.places.get(*place as usize).map(|p| &p.base) {
                    Some(PlaceBase::Cv(cv)) => *cv,
                    _ => continue,
                };
                if removable(cv) {
                    *op = Op::Nop;
                    changed = true;
                }
            }
            // Reading a CV can raise "Undefined variable" (ZEND_ASSIGN with a CV
            // operand), so an assignment of one is kept even when its target is
            // never read, unless the variable is certainly assigned there.
            Op::AssignCv {
                cv,
                val: Operand::Cv(src),
                dst: None,
            } if assigned.at(at, *src) && removable(*cv) => {
                *op = Op::Nop;
                changed = true;
            }
            Op::AssignCv { cv, val, dst } => {
                if !matches!(val, Operand::Cv(_)) && removable(*cv) {
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
                        if p.segs.is_empty() && !matches!(val, Operand::Cv(_)) && removable(cv) {
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
                if removable(*cv) {
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
                if removable(*to) {
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
                if removable(*to) {
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
pub(crate) fn compact_cvs(op_array: &mut OpArray, ctx: &OptContext<'_>) -> bool {
    let num_cvs = op_array.cv_names.len();
    if num_cvs == 0 || has_dynamic_var_access(op_array) {
        return false;
    }

    let mut is_referenced = vec![false; num_cvs];

    // Parameters and closure `use` bindings cannot be moved or removed: the
    // caller and the closure binding address their slots by number, so
    // renumbering `use ($a, $b)` with `$a` unread bound `$b` to `$a`'s slot.
    let num_param_cvs = bound_cv_prefix(op_array, ctx);
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

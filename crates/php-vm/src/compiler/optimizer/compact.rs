//! Compaction passes:
//! - Pass 9: Temporary variable compaction (ZEND_OPTIMIZER_PASS_9)
//! - Pass 10: NOP removal with full label and exception table remapping (ZEND_OPTIMIZER_PASS_10)
//! - Pass 11: Literal table compaction & deduplication (ZEND_OPTIMIZER_PASS_11)

use super::{
    def_tmp, for_each_label_mut, for_each_operand_mut, for_each_place_operand_mut, literals_equal,
};
use crate::opcode::{ClassSpec, Op, OpArray, Operand, PlaceBase};
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

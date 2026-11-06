//! Pass 8: SCCP (Sparse Conditional Constant Propagation) & DFA (ZEND_OPTIMIZER_PASS_8)
//!
//! - Global SSA temporary constant propagation: for any temporary Tmp(t) defined
//!   by Move { src: Lit(l), dst: t }, substitute Lit(l) at all read sites across ops and places.
//! - Safe basic-block Cv constant propagation: for simple non-aliased locals assigned literals.

use super::{for_each_operand_mut, for_each_place_operand_mut};
use crate::opcode::{ArgSrc, Op, OpArray, Operand, PlaceBase};

/// Runs SCCP / DFA constant propagation on `op_array`. Returns `true` if any substitution was made.
pub(crate) fn run_sccp(op_array: &mut OpArray) -> bool {
    let mut changed = false;
    let num_tmps = op_array.num_tmps as usize;
    if num_tmps == 0 {
        return false;
    }

    // 1. Collect single-assignment definitions of temporaries from literals
    // defs[t] = Some(literal_index) if defined exactly once by Move { src: Lit(l), dst: t }
    let mut defs: Vec<Option<u32>> = vec![None; num_tmps + 1];
    let mut multi_def = vec![false; num_tmps + 1];

    for op in &op_array.ops {
        if let Op::Move {
            src: Operand::Lit(l),
            dst,
        } = op
        {
            let t = *dst as usize;
            if t <= num_tmps {
                if defs[t].is_some() {
                    multi_def[t] = true;
                    defs[t] = None;
                } else if !multi_def[t] {
                    defs[t] = Some(*l);
                }
            }
        } else if let Some(t) = super::def_tmp(op) {
            let t = t as usize;
            if t <= num_tmps {
                if defs[t].is_some() {
                    multi_def[t] = true;
                    defs[t] = None;
                }
            }
        }
    }

    // Check if any single-def temp can be propagated
    let has_any_def = defs.iter().any(|d| d.is_some());
    if has_any_def {
        // Substitute in all ops
        for op in &mut op_array.ops {
            for_each_operand_mut(op, |operand| {
                if let Operand::Tmp(t) = operand {
                    if let Some(Some(l)) = defs.get(*t as usize) {
                        *operand = Operand::Lit(*l);
                        changed = true;
                    }
                }
            });
        }

        // Substitute in all places
        for place in &mut op_array.places {
            for_each_place_operand_mut(place, |operand| {
                if let Operand::Tmp(t) = operand {
                    if let Some(Some(l)) = defs.get(*t as usize) {
                        *operand = Operand::Lit(*l);
                        changed = true;
                    }
                }
            });
        }

        // Now turn the now-redundant Move definitions into NOPs
        for op in &mut op_array.ops {
            if let Op::Move {
                src: Operand::Lit(_),
                dst,
            } = op
            {
                if let Some(Some(_)) = defs.get(*dst as usize) {
                    *op = Op::Nop;
                    changed = true;
                }
            }
        }
    }

    // 2. Intra-block CV constant propagation
    // Check if function uses references or indirect variables
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
                // Potential by-ref pass
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

    if !has_hazard {
        let leaders = super::block_pass::find_leaders(op_array);
        let len = op_array.ops.len();
        let num_cvs = op_array.cv_names.len();

        let mut block_starts = Vec::new();
        for i in 0..len {
            if leaders[i] {
                block_starts.push(i);
            }
        }
        block_starts.push(len);

        for b in 0..block_starts.len() - 1 {
            let b_start = block_starts[b];
            let b_end = block_starts[b + 1];

            // Local constant table for CVs in this block
            let mut cv_const: Vec<Option<u32>> = vec![None; num_cvs];

            for i in b_start..b_end {
                let op = &mut op_array.ops[i];

                // Any call could run user code that modifies globals or calls compact/extract/etc.
                let is_call = matches!(
                    op,
                    Op::DoCall { .. }
                        | Op::InitFCall { .. }
                        | Op::InitDynCall { .. }
                        | Op::InitMethodCall { .. }
                        | Op::InitStaticCall { .. }
                );
                if is_call {
                    cv_const.fill(None);
                    continue;
                }

                // First, replace any read of Cv(cv) with Lit(l) if known constant
                for_each_operand_mut(op, |operand| {
                    if let Operand::Cv(cv) = operand {
                        if let Some(Some(l)) = cv_const.get(*cv as usize) {
                            *operand = Operand::Lit(*l);
                            changed = true;
                        }
                    }
                });

                // Update CV constant table if this op writes to a CV
                match op {
                    Op::AssignCv {
                        cv,
                        val: Operand::Lit(l),
                        ..
                    } => {
                        if (*cv as usize) < num_cvs {
                            cv_const[*cv as usize] = Some(*l);
                        }
                    }
                    Op::AssignCv { cv, .. } => {
                        if (*cv as usize) < num_cvs {
                            cv_const[*cv as usize] = None;
                        }
                    }
                    Op::IncDecCv { cv, .. }
                    | Op::AssignOpCv { cv, .. }
                    | Op::IncDecCvCmpJmp { cv, .. }
                    | Op::ConcatCv { cv, .. }
                    | Op::FetchConstTo { to: cv, .. }
                    | Op::FetchClassConstTo { to: cv, .. }
                    | Op::BinAssignCv { cv, .. } => {
                        if (*cv as usize) < num_cvs {
                            cv_const[*cv as usize] = None;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    changed
}

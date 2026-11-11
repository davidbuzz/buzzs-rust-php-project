//! Tape instructions for the array elements of a variable.
//!
//! See the "Families" section of [`super`] for the contract of a handler
//! and of [`lower`].
//!
//! The fast paths in [`crate::exec::spec::dims`] work on raw operand
//! indices (a compiled variable, a temporary, a literal, a place), not on
//! the byte offsets the numeric family uses, so an instruction here carries
//! each operand's raw index and its kind, and a handler rebuilds the
//! [`Operand`] before calling the spec function. The kind is packed in `k`
//! (low 3 bits: the offset operand, next 3: the value operand), the
//! operator / fetch mode / increment kind / `empty` flag in `e`, the
//! compiled variable in `a`, the offset operand's index in `b`, the result
//! temporary's byte offset in `c` (sentinel `u32::MAX` when there is no
//! used result), the place in `d`, and the value operand's index in `p`.
//!
//! A fetch op passes `to: None`: the result goes to `dst`, and the
//! `AssignCv` right after it runs as the numeric family's `AssignT`, so
//! every dims instruction covers exactly one op.

use super::{tmp_slot, Inst, St, TOp, SLOT_SHIFT};
use crate::exec::spec::{
    assign_dim_cv_in, assign_dims_cv_in, assign_op_dim_cv_in, assign_op_dims_cv_in, concat_cv_in,
    fetch_dim_cv_in, fetch_dims_cv_in, incdec_dim_cv_in, isset_dim_cv_in, put_tmp,
};
use crate::opcode::{IncDecKind, Op, OpArray, Operand};
use php_runtime::BinaryOp;

/// The kind of an operand, as stored in `k`: a compiled variable, a
/// consumed temporary, a kept temporary, a literal, or none.
const KIND_CV: u8 = 0;
const KIND_TMP: u8 = 1;
const KIND_TMPKEEP: u8 = 2;
const KIND_LIT: u8 = 3;
const KIND_UNUSED: u8 = 4;

/// The raw index and kind of an offset operand (`dim`), `None` when out of
/// range. `Unused` (`$cv[] = val`) is accepted.
fn dim_operand(code: &OpArray, o: Operand) -> Option<(u8, u32)> {
    match o {
        Operand::Cv(i) => ((i as usize) < code.cv_names.len()).then_some((KIND_CV, i)),
        Operand::Tmp(t) => (t < code.num_tmps).then_some((KIND_TMP, t)),
        Operand::TmpKeep(t) => (t < code.num_tmps).then_some((KIND_TMPKEEP, t)),
        Operand::Lit(l) => ((l as usize) < code.literals.len()).then_some((KIND_LIT, l)),
        Operand::Unused => Some((KIND_UNUSED, 0)),
    }
}

/// The raw index and kind of a value operand (`val`), `None` when out of
/// range or absent.
fn val_operand(code: &OpArray, o: Operand) -> Option<(u8, u32)> {
    match o {
        Operand::Cv(i) => ((i as usize) < code.cv_names.len()).then_some((KIND_CV, i)),
        Operand::Tmp(t) => (t < code.num_tmps).then_some((KIND_TMP, t)),
        Operand::TmpKeep(t) => (t < code.num_tmps).then_some((KIND_TMPKEEP, t)),
        Operand::Lit(l) => ((l as usize) < code.literals.len()).then_some((KIND_LIT, l)),
        Operand::Unused => None,
    }
}

/// The byte offset of a result temporary; `None` when out of range.
fn dst_off(code: &OpArray, t: u32) -> Option<u32> {
    (t < code.num_tmps).then_some(t << SLOT_SHIFT)
}

/// The raw index of a place; `None` when out of range.
fn place_idx(code: &OpArray, p: u32) -> Option<u32> {
    ((p as usize) < code.places.len()).then_some(p)
}

/// Rebuilds an [`Operand`] from its kind (in `k`) and raw index.
fn operand(kind: u8, idx: u32) -> Operand {
    match kind {
        KIND_CV => Operand::Cv(idx),
        KIND_TMP => Operand::Tmp(idx),
        KIND_TMPKEEP => Operand::TmpKeep(idx),
        KIND_LIT => Operand::Lit(idx),
        _ => Operand::Unused,
    }
}

/// The offset operand's kind from `k`.
fn dim_kind(inst: &Inst) -> u8 {
    inst.k & 7
}

/// The value operand's kind from `k`.
fn val_kind(inst: &Inst) -> u8 {
    (inst.k >> 3) & 7
}

/// The binary operator stored in `e`.
fn op_code(op: BinaryOp) -> u8 {
    match op {
        BinaryOp::Add => 0,
        BinaryOp::Sub => 1,
        BinaryOp::Mul => 2,
        BinaryOp::Div => 3,
        BinaryOp::Mod => 4,
        BinaryOp::Pow => 5,
        BinaryOp::ShiftLeft => 6,
        BinaryOp::ShiftRight => 7,
        BinaryOp::BitOr => 8,
        BinaryOp::BitAnd => 9,
        BinaryOp::BitXor => 10,
        BinaryOp::Concat => 11,
        BinaryOp::BoolXor => 12,
        BinaryOp::BitNot => 13,
        BinaryOp::BoolNot => 14,
    }
}

/// The binary operator in `e`.
fn op_from_code(e: u8) -> BinaryOp {
    match e {
        0 => BinaryOp::Add,
        1 => BinaryOp::Sub,
        2 => BinaryOp::Mul,
        3 => BinaryOp::Div,
        4 => BinaryOp::Mod,
        5 => BinaryOp::Pow,
        6 => BinaryOp::ShiftLeft,
        7 => BinaryOp::ShiftRight,
        8 => BinaryOp::BitOr,
        9 => BinaryOp::BitAnd,
        10 => BinaryOp::BitXor,
        11 => BinaryOp::Concat,
        12 => BinaryOp::BoolXor,
        13 => BinaryOp::BitNot,
        _ => BinaryOp::BoolNot,
    }
}

/// The increment kind stored in `e`.
fn kind_code(k: IncDecKind) -> u8 {
    match k {
        IncDecKind::PreInc => 0,
        IncDecKind::PreDec => 1,
        IncDecKind::PostInc => 2,
        IncDecKind::PostDec => 3,
    }
}

/// The increment kind in `e`.
fn kind_from_code(e: u8) -> IncDecKind {
    match e {
        0 => IncDecKind::PreInc,
        1 => IncDecKind::PreDec,
        2 => IncDecKind::PostInc,
        _ => IncDecKind::PostDec,
    }
}

/// The specialised instruction of the op at `i` when this family has one.
#[allow(clippy::too_many_lines, clippy::cast_possible_truncation)]
pub(super) fn lower(code: &OpArray, i: usize) -> Option<Inst> {
    let plain = Inst::plain;
    Some(match *code.ops.get(i)? {
        Op::FetchDimCv { cv, dim, dst, .. } => {
            let (dk, di) = dim_operand(code, dim)?;
            Inst {
                a: ((cv as usize) < code.cv_names.len()).then_some(cv)?,
                b: di,
                c: dst_off(code, dst)?,
                k: dk,
                ..plain(TOp::FetchDimC)
            }
        }
        Op::FetchDimsCv { cv, place, dst, .. } => Inst {
            a: ((cv as usize) < code.cv_names.len()).then_some(cv)?,
            c: dst_off(code, dst)?,
            d: place_idx(code, place)?,
            ..plain(TOp::FetchDimsC)
        },
        Op::IssetDimCv {
            cv,
            dim,
            dst,
            empty,
            ..
        } => {
            let (dk, di) = dim_operand(code, dim)?;
            Inst {
                a: ((cv as usize) < code.cv_names.len()).then_some(cv)?,
                b: di,
                c: dst_off(code, dst)?,
                k: dk,
                e: u8::from(empty),
                ..plain(TOp::IssetDimC)
            }
        }
        Op::AssignDimCv { cv, dim, val, .. } => {
            let (dk, di) = dim_operand(code, dim)?;
            let (vk, vi) = val_operand(code, val)?;
            Inst {
                a: ((cv as usize) < code.cv_names.len()).then_some(cv)?,
                b: di,
                p: vi as usize,
                k: dk | (vk << 3),
                ..plain(TOp::AssignDimC)
            }
        }
        Op::AssignDimsCv { cv, val, place } => {
            let (vk, vi) = val_operand(code, val)?;
            Inst {
                a: ((cv as usize) < code.cv_names.len()).then_some(cv)?,
                d: place_idx(code, place)?,
                p: vi as usize,
                k: vk << 3,
                ..plain(TOp::AssignDimsC)
            }
        }
        Op::AssignOpDimCv {
            cv, dim, op, val, ..
        } => {
            let (dk, di) = dim_operand(code, dim)?;
            let (vk, vi) = val_operand(code, val)?;
            Inst {
                a: ((cv as usize) < code.cv_names.len()).then_some(cv)?,
                b: di,
                p: vi as usize,
                k: dk | (vk << 3),
                e: op_code(op),
                ..plain(TOp::AssignOpDimC)
            }
        }
        Op::AssignOpDimsCv { cv, op, val, place } => {
            let (vk, vi) = val_operand(code, val)?;
            Inst {
                a: ((cv as usize) < code.cv_names.len()).then_some(cv)?,
                d: place_idx(code, place)?,
                p: vi as usize,
                k: vk << 3,
                e: op_code(op),
                ..plain(TOp::AssignOpDimsC)
            }
        }
        Op::IncDecDimCv {
            cv, dim, kind, dst, ..
        } => {
            let (dk, di) = dim_operand(code, dim)?;
            Inst {
                a: ((cv as usize) < code.cv_names.len()).then_some(cv)?,
                b: di,
                c: dst.and_then(|d| dst_off(code, d)).unwrap_or(u32::MAX),
                k: dk,
                e: kind_code(kind),
                ..plain(TOp::IncDecDimC)
            }
        }
        Op::ConcatCv { cv, val, dst, .. } => {
            let (vk, vi) = val_operand(code, val)?;
            Inst {
                a: ((cv as usize) < code.cv_names.len()).then_some(cv)?,
                c: dst.and_then(|d| dst_off(code, d)).unwrap_or(u32::MAX),
                p: vi as usize,
                k: vk << 3,
                ..plain(TOp::ConcatCv)
            }
        }
        _ => return None,
    })
}

/// `Op::FetchDimCv`: `$cv[dim]` read (`exec/spec/dims.rs`
/// `fetch_dim_cv_in`). The result goes to `dst`; the `AssignCv` after it
/// runs as the numeric family's `AssignT`.
///
/// # Safety
/// `inst` is an instruction [`lower`] built for the frame of `st`.
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn fetch_dim(st: St, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `st.f` is the current frame.
    let f = unsafe { &mut *st.f };
    let dim = operand(dim_kind(inst), inst.b);
    fetch_dim_cv_in(f, inst.a, dim, inst.c >> SLOT_SHIFT, None) == Some(false)
}

/// `Op::FetchDimsCv`: `$cv[d1][d2]...` read (`fetch_dims_cv_in`), as
/// [`fetch_dim`].
///
/// # Safety
/// As [`fetch_dim`].
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn fetch_dims(st: St, inst: &Inst) -> bool {
    let f = unsafe { &mut *st.f };
    fetch_dims_cv_in(f, inst.a, inst.d, inst.c >> SLOT_SHIFT, None) == Some(false)
}

/// `Op::IssetDimCv`: `isset` / `empty` of `$cv[dim]` (`isset_dim_cv_in`).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn isset_dim(st: St, inst: &Inst) -> bool {
    let f = unsafe { &mut *st.f };
    let dim = operand(dim_kind(inst), inst.b);
    isset_dim_cv_in(f, inst.a, dim, inst.c >> SLOT_SHIFT, inst.e != 0)
}

/// `Op::AssignDimCv`: `$cv[dim] = val` (`assign_dim_cv_in`). Releases a
/// string offset and a consumed value temporary, so it polls.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(always)]
#[allow(clippy::inline_always, clippy::cast_possible_truncation)]
pub(super) unsafe fn assign_dim(st: St, inst: &Inst) -> bool {
    let f = unsafe { &mut *st.f };
    let dim = operand(dim_kind(inst), inst.b);
    let val = operand(val_kind(inst), inst.p as u32);
    assign_dim_cv_in(f, inst.a, dim, val)
}

/// `Op::AssignDimsCv`: `$cv[d1][d2]... = val` (`assign_dims_cv_in`).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(always)]
#[allow(clippy::inline_always, clippy::cast_possible_truncation)]
pub(super) unsafe fn assign_dims(st: St, inst: &Inst) -> bool {
    let f = unsafe { &mut *st.f };
    let val = operand(val_kind(inst), inst.p as u32);
    assign_dims_cv_in(f, inst.a, inst.d, val)
}

/// `Op::AssignOpDimCv`: `$cv[dim] op= val` (`assign_op_dim_cv_in`).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(always)]
#[allow(clippy::inline_always, clippy::cast_possible_truncation)]
pub(super) unsafe fn assign_op_dim(st: St, inst: &Inst) -> bool {
    let f = unsafe { &mut *st.f };
    let dim = operand(dim_kind(inst), inst.b);
    let val = operand(val_kind(inst), inst.p as u32);
    assign_op_dim_cv_in(f, inst.a, dim, op_from_code(inst.e), val)
}

/// `Op::AssignOpDimsCv`: `$cv[d1][d2]... op= val` (`assign_op_dims_cv_in`).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(always)]
#[allow(clippy::inline_always, clippy::cast_possible_truncation)]
pub(super) unsafe fn assign_op_dims(st: St, inst: &Inst) -> bool {
    let f = unsafe { &mut *st.f };
    let val = operand(val_kind(inst), inst.p as u32);
    assign_op_dims_cv_in(f, inst.a, op_from_code(inst.e), inst.d, val)
}

/// `Op::IncDecDimCv`: `$cv[dim]++` & co. (`incdec_dim_cv_in`). The result
/// is stored into `dst` when used.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(always)]
#[allow(clippy::inline_always, clippy::cast_ptr_alignment)]
pub(super) unsafe fn incdec_dim(st: St, inst: &Inst) -> bool {
    let f = unsafe { &mut *st.f };
    let dim = operand(dim_kind(inst), inst.b);
    let Some(r) = incdec_dim_cv_in(f, inst.a, dim, kind_from_code(inst.e)) else {
        return false;
    };
    if inst.c != u32::MAX {
        // SAFETY: `c` is a validated temporary offset.
        put_tmp(unsafe { tmp_slot(st, inst.c) }, r);
    }
    true
}

/// `Op::ConcatCv`: `$cv .= val` (`concat_cv_in`). Extends the string in
/// place and releases a consumed value temporary, so it polls.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(always)]
#[allow(clippy::inline_always, clippy::cast_possible_truncation)]
pub(super) unsafe fn concat_cv(st: St, inst: &Inst) -> bool {
    let f = unsafe { &mut *st.f };
    let val = operand(val_kind(inst), inst.p as u32);
    let dst = (inst.c != u32::MAX).then_some(inst.c >> SLOT_SHIFT);
    concat_cv_in(f, inst.a, val, dst)
}

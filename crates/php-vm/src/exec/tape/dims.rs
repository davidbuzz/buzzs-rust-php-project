// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
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
//! A fetch folded with the `AssignCv` after it (`to`) stores into that
//! variable when it holds a plain value and jumps past the `AssignCv`, as
//! `hot::step_ext` skips it; otherwise the value goes to `dst` and the
//! `AssignCv` runs next. Every other dims instruction covers one op.

use super::{rel, Br, Inst, TOp, SLOT_SHIFT};
use crate::exec::spec::{
    assign_dim_cv_in, assign_dims_cv_in, assign_op_dim_cv_in, assign_op_dims_cv_in, concat_cv_in,
    fetch_dim_cv_in, fetch_dims_cv_in, incdec_dim_cv_in, isset_dim_cv_in, put_tmp,
};
use crate::opcode::{IncDecKind, Op, OpArray, Operand};
use crate::vm::{CvSlot, Frame};
use php_runtime::{ArrayKey, BinaryOp, Value};

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

/// The variable of the `AssignCv` folded into a fetch at `i` (`to`;
/// [`NO_TO`] when there is none) and the jump past that `AssignCv` (`i + 2`)
/// for when the fetch stored into the variable itself. `None` when either
/// is out of range.
fn folded_assign(code: &OpArray, i: usize, to: Option<u32>) -> Option<(u32, u32)> {
    match to {
        None => Some((NO_TO, 0)),
        Some(cv) => {
            if cv as usize >= code.cv_names.len() || cv == NO_TO {
                return None;
            }
            Some((cv, rel(code, i, u32::try_from(i + 2).ok()?)?))
        }
    }
}

/// `to` of a fetch without a folded `AssignCv`.
const NO_TO: u32 = u32::MAX;

/// The instruction of an `Op::FetchDimCv`, chosen from what the offset
/// operand can hold when the tape is built: [`TOp::FetchDimIC`] (an int
/// offset into a packed array: [`fetch_dim_int`]) for an offset in a
/// temporary or a variable (an int is tested for when it runs) or an int
/// literal; [`TOp::FetchDimSC`] ([`fetch_dim_str`]) for a string literal
/// that is no integer key (`"1"` is key 1: the generic lookup converts
/// it); else [`TOp::FetchDimC`], the general read. Both fast paths store
/// into the folded `AssignCv`'s variable: a read without one is general.
fn fetch_op(code: &OpArray, dk: u8, di: u32, to: u32) -> TOp {
    if to == NO_TO {
        return TOp::FetchDimC;
    }
    match dk {
        KIND_TMP | KIND_TMPKEEP | KIND_CV => TOp::FetchDimIC,
        KIND_LIT => match code.literals.get(di as usize) {
            Some(Value::Int(_)) => TOp::FetchDimIC,
            Some(Value::Str(s)) if matches!(ArrayKey::from_zstr(s), ArrayKey::Str(_)) => {
                TOp::FetchDimSC
            }
            _ => TOp::FetchDimC,
        },
        _ => TOp::FetchDimC,
    }
}

/// The instruction of an `Op::AssignDimCv`: [`TOp::AssignDimIC`]
/// ([`assign_dim_int`]) when the offset can be an int or is absent (an
/// append; not a literal of another type) and the value can be a scalar or
/// a string (not a literal of another type); else [`TOp::AssignDimC`].
fn assign_op(code: &OpArray, (dk, di): (u8, u32), (vk, vi): (u8, u32)) -> TOp {
    let lit = |i: u32| code.literals.get(i as usize);
    let dim_ok = match dk {
        KIND_LIT => matches!(lit(di), Some(Value::Int(_))),
        _ => true,
    };
    let val_ok = match vk {
        KIND_LIT => lit(vi).is_some_and(Value::is_plain),
        _ => true,
    };
    if dim_ok && val_ok {
        TOp::AssignDimIC
    } else {
        TOp::AssignDimC
    }
}

/// What a fetch handler did: stored into the folded `AssignCv`'s variable
/// (the instruction jumps past that op, as `hot::step_ext` skips it), into
/// the temporary (the `AssignCv` runs next), or nothing.
fn fetched(r: Option<bool>) -> Br {
    match r {
        Some(true) => Br::Jump,
        Some(false) => Br::Fall,
        None => Br::Decline,
    }
}

/// The specialised instruction of the op at `i` when this family has one.
#[allow(clippy::too_many_lines, clippy::cast_possible_truncation)]
pub(super) fn lower(code: &OpArray, i: usize) -> Option<Inst> {
    let plain = Inst::plain;
    Some(match *code.ops.get(i)? {
        Op::FetchDimCv {
            cv, dim, dst, to, ..
        } => {
            let (dk, di) = dim_operand(code, dim)?;
            dst_off(code, dst)?;
            let (to, c) = folded_assign(code, i, to)?;
            Inst {
                a: ((cv as usize) < code.cv_names.len()).then_some(cv)?,
                b: di,
                c,
                d: to,
                k: dk,
                p: dst as usize,
                ..plain(fetch_op(code, dk, di, to))
            }
        }
        Op::FetchDimsCv {
            cv, place, dst, to, ..
        } => {
            dst_off(code, dst)?;
            let (to, c) = folded_assign(code, i, to)?;
            Inst {
                a: ((cv as usize) < code.cv_names.len()).then_some(cv)?,
                b: to,
                c,
                d: place_idx(code, place)?,
                p: dst as usize,
                ..plain(TOp::FetchDimsC)
            }
        }
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
                ..plain(assign_op(code, (dk, di), (vk, vi)))
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
/// `fetch_dim_cv_in`). The result goes to the folded `AssignCv`'s variable
/// (jump past that op) or to `dst` (fall through to it).
///
/// # Safety
/// `inst` is an instruction [`lower`] built for `frame`, the current frame.
#[inline(never)]
#[allow(clippy::cast_possible_truncation)]
pub(super) unsafe fn fetch_dim(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract; `frame` is the current frame.
    let f = unsafe { &mut *frame };
    let dim = operand(dim_kind(inst), inst.b);
    let to = (inst.d != NO_TO).then_some(inst.d);
    fetched(fetch_dim_cv_in(f, inst.a, dim, inst.p as u32, to))
}

/// `TOp::FetchDimIC`: [`fetch_packed_into_cv`], else the general read
/// ([`fetch_dim`]). A handler of its own, so that the fast path's few
/// registers are all its call saves.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn fetch_dim_int(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract; `frame` is the current frame.
    if let Some(br) = fetch_packed_into_cv(unsafe { &mut *frame }, inst) {
        return br;
    }
    // SAFETY: as above; the fast path's borrow of the frame has ended.
    unsafe { fetch_dim(frame, inst) }
}

/// `TOp::FetchDimSC`: [`fetch_str_into_cv`], else the general read
/// ([`fetch_dim`]), as [`fetch_dim_int`].
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn fetch_dim_str(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract; `frame` is the current frame.
    if let Some(br) = fetch_str_into_cv(unsafe { &mut *frame }, inst) {
        return br;
    }
    // SAFETY: as above; the fast path's borrow of the frame has ended.
    unsafe { fetch_dim(frame, inst) }
}

/// The commonest `$v = $cv[$i]`, as `fetch_dim_cv_in` and
/// `store_fetched_in` make it, without their general steps: an int offset
/// (in a temporary, a variable or a literal) into a packed array the
/// variable holds directly, an element that is a scalar or a string, and a
/// folded `AssignCv` variable that holds no array, object or reference
/// (no possible garbage collection root is released). The element is
/// copied into the variable, an old string released, and the instruction
/// jumps past the `AssignCv` (`ZEND_FETCH_DIM_R` + `ZEND_ASSIGN` on these
/// operands). `None`, nothing changed, for anything else: the general path
/// runs.
#[inline(always)]
#[allow(clippy::inline_always)] // the head of `fetch_dim`
fn fetch_packed_into_cv(f: &mut Frame, inst: &Inst) -> Option<Br> {
    if f.symtab_attached {
        return None;
    }
    let i = int_dim(f, inst)?;
    let Value::Array(arr) = f.cvs.get(inst.a as usize)?.get()? else {
        return None;
    };
    let v = plain_copy(arr.get_packed(i)?)?;
    store_plain_into(f, inst.d, v)
}

/// `$v = $cv["key"]` for a literal key that is no integer key
/// ([`TOp::FetchDimSC`]), as [`fetch_packed_into_cv`]: an array the variable holds
/// directly that has the key (`zend_hash_find_known_hash`, as
/// `ZEND_FETCH_DIM_R` with a constant offset looks it up), an element that
/// is a scalar or a string, a destination that holds no array, object or
/// reference.
#[inline(always)]
#[allow(clippy::inline_always)] // the head of `fetch_dim`
fn fetch_str_into_cv(f: &mut Frame, inst: &Inst) -> Option<Br> {
    if f.symtab_attached {
        return None;
    }
    let Value::Str(k) = f.code.literals.get(inst.b as usize)? else {
        return None;
    };
    let Value::Array(arr) = f.cvs.get(inst.a as usize)?.get()? else {
        return None;
    };
    let v = plain_copy(arr.get_zstr(k)?)?;
    store_plain_into(f, inst.d, v)
}

/// The store of the fast paths of `fetch_dim`: the copied element `v` into
/// the folded `AssignCv`'s variable `to` when that holds nothing a release
/// could make a garbage collection root of (`zend_assign_to_variable` of a
/// variable that is no reference: an old string is released), and the jump
/// past the `AssignCv`; `None`, `v` dropped and nothing changed, otherwise.
#[inline(always)]
#[allow(clippy::inline_always)] // the tail of `fetch_dim`'s fast paths
fn store_plain_into(f: &Frame, to: u32, v: Value) -> Option<Br> {
    let slot = f.cvs.get(to as usize)?;
    if matches!(
        slot.get(),
        Some(Value::Array(_) | Value::Object(_) | Value::Reference(_) | Value::Resource(_))
    ) {
        return None;
    }
    // An old scalar owns nothing; an old string is released.
    drop(slot.set(Some(v)));
    Some(Br::Jump)
}

/// The int offset of a fast path: the offset operand (a temporary, a
/// variable or a literal) when it holds an int directly. A temporary keeps
/// it (an int owns nothing), as `dim_int_in` leaves it.
#[inline(always)]
#[allow(clippy::inline_always)] // part of the fast paths' heads
fn int_dim(f: &Frame, inst: &Inst) -> Option<i64> {
    let v = match dim_kind(inst) {
        KIND_TMP | KIND_TMPKEEP => f.tmps.get(inst.b as usize)?,
        KIND_CV => f.cvs.get(inst.b as usize)?.get()?,
        KIND_LIT => f.code.literals.get(inst.b as usize)?,
        _ => return None,
    };
    match v {
        Value::Int(i) => Some(*i),
        _ => None,
    }
}

/// A copy of a scalar or a string (a reference count up); `None` for any
/// other value (an array, an object, a reference, a resource).
#[inline(always)]
#[allow(clippy::inline_always)] // part of the fast paths' heads
fn plain_copy(v: &Value) -> Option<Value> {
    Some(match v {
        Value::Int(x) => Value::Int(*x),
        Value::Float(x) => Value::Float(*x),
        Value::Bool(b) => Value::Bool(*b),
        Value::Null => Value::Null,
        Value::Str(s) => Value::Str(s.clone()),
        _ => return None,
    })
}

/// The commonest `$cv[$i] = $v` and `$cv[] = $v` ([`TOp::AssignDimIC`]), as
/// `assign_dim_cv_in` makes them, without its general steps: an array the
/// variable holds directly and alone (no separation; no symbol table), an
/// int offset (as [`fetch_packed_into_cv`]) of an existing element of a
/// packed table that is a scalar or a string, or the next one
/// (`HashTable::assign_packed_int`), or an append to any table
/// (`HashTable::push`), and a value that is a scalar or a string
/// (`ZEND_ASSIGN_DIM` + `OP_DATA` on these operands: copied from a
/// variable or a literal, moved out of a consumed temporary). An old
/// string element is released; no possible garbage collection root is.
/// `None`, nothing changed (a moved value is put back), for anything else:
/// the general path runs.
#[inline(always)]
#[allow(clippy::inline_always)] // the head of `assign_dim`
fn assign_packed(f: &mut Frame, inst: &Inst) -> Option<()> {
    if f.symtab_attached {
        return None;
    }
    let i = if dim_kind(inst) == KIND_UNUSED {
        None
    } else {
        Some(int_dim(f, inst)?)
    };
    let vk = val_kind(inst);
    let v = match vk {
        KIND_CV => plain_copy(f.cvs.get(inst.p)?.get()?)?,
        KIND_LIT => plain_copy(f.code.literals.get(inst.p)?)?,
        KIND_TMPKEEP => plain_copy(f.tmps.get(inst.p)?)?,
        KIND_TMP => {
            let t = f.tmps.get_mut(inst.p)?;
            if !t.is_plain() {
                return None;
            }
            std::mem::take(t)
        }
        _ => return None,
    };
    match write_plain(f, inst.a, i, v) {
        Ok(()) => Some(()),
        Err(v) => {
            if vk == KIND_TMP {
                if let Some(t) = f.tmps.get_mut(inst.p) {
                    // The slot holds the `Null` `take` left (nothing to
                    // release).
                    std::mem::forget(std::mem::replace(t, v));
                }
            }
            None
        }
    }
}

/// The write of [`assign_packed`]: `v` into the array variable `cv` holds
/// alone, at int offset `i` of a packed table or appended (`i` absent).
/// `Err` gives the value back, nothing changed.
#[inline(always)]
#[allow(clippy::inline_always)] // part of `assign_packed`
fn write_plain(f: &mut Frame, cv: u32, i: Option<i64>, v: Value) -> Result<(), Value> {
    let Some(Some(Value::Array(arr))) = f.cvs.get_mut(cv as usize).map(CvSlot::as_mut) else {
        return Err(v);
    };
    let Some(tbl) = arr.get_mut_unshared() else {
        return Err(v);
    };
    let Some(i) = i else {
        return tbl.push(v).map(|_| ());
    };
    if let Some(slot) = tbl.get_packed_mut(i) {
        if !slot.is_plain() {
            return Err(v);
        }
        discard_plain(std::mem::replace(slot, v));
        return Ok(());
    }
    // The next element of a packed table (or a hole, a hash table: given
    // back).
    if let Some(old) = tbl.assign_packed_int(i, v)? {
        discard_plain(old);
    }
    Ok(())
}

/// Releases a replaced scalar or string element: a scalar owns nothing (no
/// drop glue), a string is released.
#[inline(always)]
#[allow(clippy::inline_always)] // part of `write_plain`
fn discard_plain(old: Value) {
    if matches!(old, Value::Str(_)) {
        drop(old);
    } else {
        std::mem::forget(old);
    }
}

/// `Op::FetchDimsCv`: `$cv[d1][d2]...` read (`fetch_dims_cv_in`), as
/// [`fetch_dim`].
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
#[allow(clippy::cast_possible_truncation)]
pub(super) unsafe fn fetch_dims(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract; `frame` is the current frame.
    let f = unsafe { &mut *frame };
    let to = (inst.b != NO_TO).then_some(inst.b);
    fetched(fetch_dims_cv_in(f, inst.a, inst.d, inst.p as u32, to))
}

/// `Op::IssetDimCv`: `isset` / `empty` of `$cv[dim]` (`isset_dim_cv_in`).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn isset_dim(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    let dim = operand(dim_kind(inst), inst.b);
    isset_dim_cv_in(f, inst.a, dim, inst.c >> SLOT_SHIFT, inst.e != 0)
}

/// `Op::AssignDimCv`: `$cv[dim] = val` (`assign_dim_cv_in`). Releases a
/// string offset and a consumed value temporary, so it polls.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
#[allow(clippy::cast_possible_truncation)]
pub(super) unsafe fn assign_dim(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    let dim = operand(dim_kind(inst), inst.b);
    let val = operand(val_kind(inst), inst.p as u32);
    assign_dim_cv_in(f, inst.a, dim, val)
}

/// `TOp::AssignDimIC`: [`assign_packed`], else the general write
/// ([`assign_dim`]), as [`fetch_dim_int`].
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn assign_dim_int(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `frame` is the current frame.
    if assign_packed(unsafe { &mut *frame }, inst).is_some() {
        return true;
    }
    // SAFETY: as above; the fast path's borrow of the frame has ended.
    unsafe { assign_dim(frame, inst) }
}

/// `Op::AssignDimsCv`: `$cv[d1][d2]... = val` (`assign_dims_cv_in`).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
#[allow(clippy::cast_possible_truncation)]
pub(super) unsafe fn assign_dims(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    let val = operand(val_kind(inst), inst.p as u32);
    assign_dims_cv_in(f, inst.a, inst.d, val)
}

/// `Op::AssignOpDimCv`: `$cv[dim] op= val` (`assign_op_dim_cv_in`).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
#[allow(clippy::cast_possible_truncation)]
pub(super) unsafe fn assign_op_dim(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    let dim = operand(dim_kind(inst), inst.b);
    let val = operand(val_kind(inst), inst.p as u32);
    assign_op_dim_cv_in(f, inst.a, dim, op_from_code(inst.e), val)
}

/// `Op::AssignOpDimsCv`: `$cv[d1][d2]... op= val` (`assign_op_dims_cv_in`).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
#[allow(clippy::cast_possible_truncation)]
pub(super) unsafe fn assign_op_dims(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    let val = operand(val_kind(inst), inst.p as u32);
    assign_op_dims_cv_in(f, inst.a, op_from_code(inst.e), inst.d, val)
}

/// `Op::IncDecDimCv`: `$cv[dim]++` & co. (`incdec_dim_cv_in`). The result
/// is stored into `dst` when used.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn incdec_dim(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    let dim = operand(dim_kind(inst), inst.b);
    let Some(r) = incdec_dim_cv_in(f, inst.a, dim, kind_from_code(inst.e)) else {
        return false;
    };
    if inst.c != u32::MAX {
        if let Some(slot) = f.tmps.get_mut((inst.c >> SLOT_SHIFT) as usize) {
            put_tmp(slot, r);
        }
    }
    true
}

/// `Op::ConcatCv`: `$cv .= val` (`concat_cv_in`). Extends the string in
/// place and releases a consumed value temporary, so it polls.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
#[allow(clippy::cast_possible_truncation)]
pub(super) unsafe fn concat_cv(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    let val = operand(val_kind(inst), inst.p as u32);
    let dst = (inst.c != u32::MAX).then_some(inst.c >> SLOT_SHIFT);
    concat_cv_in(f, inst.a, val, dst)
}

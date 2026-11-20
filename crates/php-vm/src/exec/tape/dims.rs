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
    assign_dim_cv_in, assign_dims_cv_in, assign_op_dim_cv_in, assign_op_dims_cv_in, cast_string_in,
    concat_cv_in, concat_in, fe_fetch_array_in, fetch_dim_cv_in, fetch_dims_cv_in,
    incdec_dim_cv_in, isset_dim_cv_in, put_tmp, rope_in,
};
use crate::opcode::{BinKind, CastKind, IncDecKind, Op, OpArray, Operand, Seg};
use crate::vm::{CvSlot, ForeachIter, Frame};
use php_runtime::{ArrayKey, BinaryOp, HashTable, Value, ZArray, ZStr};

/// The kind of an operand, as stored in `k`: a compiled variable, a
/// consumed temporary, a kept temporary, a literal, or none.
const KIND_CV: u8 = 0;
pub(super) const KIND_TMP: u8 = 1;
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
pub(super) fn val_kind(inst: &Inst) -> u8 {
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
pub(super) const NO_TO: u32 = u32::MAX;

/// The instruction of an `Op::FetchDimCv`, chosen from what the offset
/// operand can hold when the tape is built: [`TOp::FetchDimIC`] (an int
/// offset into a packed array: [`fetch_dim_int`]) for an offset in a
/// temporary or a variable (an int is tested for when it runs) or an int
/// literal; [`TOp::FetchDimSC`] ([`fetch_dim_str`]) for a string literal
/// that is no integer key (`"1"` is key 1: the generic lookup converts
/// it); else [`TOp::FetchDimC`], the general read. Both store into the
/// folded `AssignCv`'s variable; a read into its temporary with an offset
/// that can be an int is [`TOp::FetchDimIT`] ([`fetch_dim_tmp`]), and any
/// other read without a folded variable general.
fn fetch_op(code: &OpArray, dk: u8, di: u32, to: u32) -> TOp {
    if to == NO_TO {
        let int_offset = match dk {
            KIND_TMP | KIND_TMPKEEP | KIND_CV => true,
            KIND_LIT => matches!(code.literals.get(di as usize), Some(Value::Int(_))),
            _ => false,
        };
        return if int_offset {
            TOp::FetchDimIT
        } else {
            TOp::FetchDimC
        };
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
        // `a . b` into a temporary: the operands' kinds in `k` (the left
        // one's lowest), their indices in `a` and `b`, the result
        // temporary's in `p`. A literal that is neither a string nor an
        // int is the generic op's (it converts it).
        Op::Bin {
            op: BinKind::Arith(BinaryOp::Concat),
            a,
            b,
            dst,
        } => {
            let part = |o: Operand| -> Option<(u8, u32)> {
                if let Operand::Lit(l) = o {
                    if !matches!(
                        code.literals.get(l as usize)?,
                        Value::Str(_) | Value::Int(_)
                    ) {
                        return None;
                    }
                }
                val_operand(code, o)
            };
            let (ak, ai) = part(a)?;
            let (bk, bi) = part(b)?;
            dst_off(code, dst)?;
            Inst {
                a: ai,
                b: bi,
                k: ak | (bk << 3),
                p: dst as usize,
                ..plain(TOp::ConcatT)
            }
        }
        // A string built from parts (`ZEND_ROPE_END`): the op's index in
        // `a` (the handler reads the parts from the op), the result
        // temporary's in `p`.
        Op::Rope { dst, .. } => {
            dst_off(code, dst)?;
            Inst {
                a: u32::try_from(i).ok()?,
                p: dst as usize,
                ..plain(TOp::RopeT)
            }
        }
        // `(string) a`: the operand's kind in `k`, its index in `a`, the
        // result temporary's in `p`.
        Op::Cast {
            kind: CastKind::String,
            a,
            dst,
        } => {
            let (ak, ai) = val_operand(code, a)?;
            dst_off(code, dst)?;
            Inst {
                a: ai,
                k: ak,
                p: dst as usize,
                ..plain(TOp::CastStrT)
            }
        }
        // The step of a `foreach`: the iterator in `a`, the value's
        // temporary in `b`, the key's in `d` ([`NO_TO`]: none), the jump
        // out of the loop in `c`. With the plain assignments of the value
        // (and of the key) to the loop's variables right behind it, one
        // instruction covers them: the variables in `b` and `d`, and the
        // temporaries (the value's low, the key's high) in `p`.
        Op::FeFetch {
            iter,
            end,
            val,
            key,
        } => {
            dst_off(code, val)?;
            let key = match key {
                Some(k) => {
                    dst_off(code, k)?;
                    if k == NO_TO {
                        return None;
                    }
                    k
                }
                None => NO_TO,
            };
            let assigned = |at: usize, t: u32| match code.ops.get(at) {
                Some(&Op::AssignCv {
                    cv,
                    val: Operand::Tmp(from),
                    dst: None,
                }) if from == t && (cv as usize) < code.cv_names.len() && cv != NO_TO => Some(cv),
                _ => None,
            };
            let c = rel(code, i, end)?;
            let unfused = Inst {
                a: iter,
                b: val,
                c,
                d: key,
                ..plain(TOp::FeFetchA)
            };
            match (assigned(i + 1, val), key) {
                (Some(to), NO_TO) => Inst {
                    a: iter,
                    b: to,
                    c,
                    d: NO_TO,
                    p: val as usize,
                    ..plain(TOp::FeFetchV)
                },
                (Some(to), k) => match assigned(i + 2, k) {
                    Some(key_to) => Inst {
                        a: iter,
                        b: to,
                        c,
                        d: key_to,
                        p: (val as usize) | ((k as usize) << 32),
                        ..plain(TOp::FeFetchKV)
                    },
                    None => unfused,
                },
                (None, _) => unfused,
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

/// `TOp::FetchDimIC`: the offset, then [`fetch_packed_into_cv`], else
/// the wider read ([`fetch_int_any`]). An offset that is no int goes to
/// the string-key step at once ([`fetch_dim_key`]: a string most likely;
/// anything else falls through it to the general read), so no step looks
/// at the offset to find out which one it is for. A handler of its own,
/// so that the fast path's few registers are all its call saves: what the
/// fast path leaves out lies behind a call for that reason, not inside
/// it.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn fetch_dim_int(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract; `frame` is the current frame.
    let f = unsafe { &mut *frame };
    let Some(i) = int_dim(f, inst) else {
        // SAFETY: as above; the borrow of the frame has ended.
        return unsafe { fetch_dim_key(frame, inst) };
    };
    if let Some(br) = fetch_packed_into_cv(f, inst, i) {
        return br;
    }
    // SAFETY: as above; the fast path's borrow of the frame has ended.
    unsafe { fetch_int_any(frame, inst) }
}

/// `TOp::FetchDimIT`: the offset, then [`fetch_packed_into_tmp`], else
/// the wider read ([`fetch_int_any`]), as [`fetch_dim_int`].
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn fetch_dim_tmp(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract; `frame` is the current frame.
    let f = unsafe { &mut *frame };
    let Some(i) = int_dim(f, inst) else {
        // SAFETY: as above; the borrow of the frame has ended.
        return unsafe { fetch_dim_key(frame, inst) };
    };
    if let Some(br) = fetch_packed_into_tmp(f, inst, i) {
        return br;
    }
    // SAFETY: as above; the fast path's borrow of the frame has ended.
    unsafe { fetch_int_any(frame, inst) }
}

/// The second step of a read with an int offset ([`fetch_int_wide`]), else
/// the read with a string offset ([`fetch_dim_key`]).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn fetch_int_any(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract; `frame` is the current frame.
    if let Some(br) = fetch_int_wide(unsafe { &mut *frame }, inst) {
        return br;
    }
    // SAFETY: as above; the path's borrow of the frame has ended.
    unsafe { fetch_dim_key(frame, inst) }
}

/// `TOp::FetchDimC`, and the int paths' way on: a read with a string
/// offset ([`fetch_key`]), else the general read ([`fetch_dim`]).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn fetch_dim_key(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract; `frame` is the current frame.
    if let Some(br) = fetch_key(unsafe { &mut *frame }, inst) {
        return br;
    }
    // SAFETY: as above; the path's borrow of the frame has ended.
    unsafe { fetch_dim(frame, inst) }
}

/// `$cv[$key]` read with a string offset, as `fetch_dim_cv_in` makes it,
/// without its general steps: the key is a string a variable, a temporary
/// or a literal holds (`zend_symtable_find`: an integer string is the int
/// key), the array is held directly or through a reference and has it. The
/// element goes to the folded `AssignCv`'s variable or to the result
/// temporary, as in [`fetch_int_wide`], and a consumed key temporary is
/// released (`FREE_OP2`). `None`, nothing changed, for anything else.
#[inline(always)]
#[allow(clippy::inline_always)] // the body of `fetch_dim_key`
fn fetch_key(f: &mut Frame, inst: &Inst) -> Option<Br> {
    let kind = dim_kind(inst);
    let v = {
        let key = key_str(f, kind, inst.b)?;
        match f.cvs.get(inst.a as usize)?.get()? {
            Value::Array(arr) => elem_copy(arr.sym_zstr_probe(key)?)?,
            Value::Reference(r) => match &*r.try_borrow()? {
                Value::Array(arr) => elem_copy(arr.get_sym_zstr(key)?)?,
                _ => return None,
            },
            _ => return None,
        }
    };
    let br = if inst.d == NO_TO {
        store_into_tmp(f, inst.p, v)?
    } else if f.symtab_attached {
        return None;
    } else {
        store_plain_into(f, inst.d, v)?
    };
    release_key(f, kind, inst.b);
    Some(br)
}

/// The string an offset operand holds directly: in a variable, a temporary
/// or a literal. `None` for any other value (an int is the int paths', a
/// reference the general path's).
#[inline(always)]
#[allow(clippy::inline_always)] // part of the string-key paths
fn key_str(f: &Frame, kind: u8, idx: u32) -> Option<&ZStr> {
    let v = match kind {
        KIND_CV => f.cvs.get(idx as usize)?.get()?,
        KIND_TMP | KIND_TMPKEEP => f.tmps.get(idx as usize)?,
        KIND_LIT => f.code.literals.get(idx as usize)?,
        _ => return None,
    };
    match v {
        Value::Str(s) => Some(s),
        _ => None,
    }
}

/// Releases the string a consumed offset temporary holds (the `FREE_OP2`
/// of an element op, as `release_str_tmp_in`).
#[inline(always)]
#[allow(clippy::inline_always)] // the tail of the string-key paths
fn release_key(f: &mut Frame, kind: u8, idx: u32) {
    if kind == KIND_TMP {
        if let Some(slot) = f.tmps.get_mut(idx as usize) {
            if matches!(slot, Value::Str(_)) {
                *slot = Value::Null;
            }
        }
    }
}

/// `$cv[$i]` read with an int offset, wider than [`fetch_packed_into_cv`]
/// and [`fetch_packed_into_tmp`] and still without the general steps of
/// `fetch_dim_cv_in`: the array may be held through a reference (a
/// by-reference parameter, a `global`), its table may be a hash, and the
/// element anything but a reference ([`int_elem_copy`]). The element goes
/// to the folded `AssignCv`'s variable or to the result temporary, as
/// there. `None`, nothing changed, for anything else.
#[inline(always)]
#[allow(clippy::inline_always)] // the body of `fetch_int_any`
fn fetch_int_wide(f: &mut Frame, inst: &Inst) -> Option<Br> {
    let i = int_dim(f, inst)?;
    let v = int_elem_copy(f, inst.a, i)?;
    if inst.d == NO_TO {
        store_into_tmp(f, inst.p, v)
    } else if f.symtab_attached {
        None
    } else {
        store_plain_into(f, inst.d, v)
    }
}

/// `$t = $cv[$i]` ([`TOp::FetchDimIT`]), as [`fetch_packed_into_cv`] with
/// the result temporary for the variable: it holds a scalar (nothing to
/// release), and the `Op::FetchDimCv` is done (`ZEND_FETCH_DIM_R` on these
/// operands; an int offset in a temporary stays there, as the general read
/// leaves it).
#[inline(always)]
#[allow(clippy::inline_always)] // the head of `fetch_dim_tmp`
fn fetch_packed_into_tmp(f: &mut Frame, inst: &Inst, i: i64) -> Option<Br> {
    let v = match f.cvs.get(inst.a as usize)?.get()? {
        Value::Array(arr) => plain_copy(arr.get_packed(i)?)?,
        // As in `fetch_packed_into_cv`.
        Value::Str(s) => {
            let c = *s.as_bytes().get(usize::try_from(i).ok()?)?;
            Value::Str(php_runtime::ZStr::from_byte(c))
        }
        _ => return None,
    };
    store_into_tmp(f, inst.p, v)
}

/// `$cv[$i][$k]` ([`TOp::FetchDimsC`]) of two int offsets, as
/// `fetch_dims_cv_in` makes it, without its general walk: the variable
/// holds a list directly that has a list at `$i`, which has a scalar or a
/// string at `$k`. The element goes to the folded `AssignCv`'s variable or
/// to the result temporary, as in [`fetch_packed_into_cv`] and
/// [`fetch_packed_into_tmp`]. `None`, nothing changed, for anything else
/// (more or other offsets, a missing key, a hash, a reference on the way):
/// the general path runs.
#[inline(always)]
#[allow(clippy::inline_always)] // the head of `fetch_dims` and of the trace's helper
pub(super) fn fetch_packed_two(f: &mut Frame, inst: &Inst) -> Option<Br> {
    let [Seg::Dim(first), Seg::Dim(second)] = f.code.places.get(inst.d as usize)?.segs.as_slice()
    else {
        return None;
    };
    let (i, k) = (operand_int(f, *first)?, operand_int(f, *second)?);
    let Value::Array(arr) = f.cvs.get(inst.a as usize)?.get()? else {
        return None;
    };
    let Value::Array(row) = arr.get_packed(i)? else {
        return None;
    };
    let v = plain_copy(row.get_packed(k)?)?;
    if inst.b == NO_TO {
        store_into_tmp(f, inst.p, v)
    } else if f.symtab_attached {
        None
    } else {
        store_plain_into(f, inst.b, v)
    }
}

/// The int an offset operand of a place holds directly (a temporary keeps
/// it, as in [`int_dim`]).
#[inline(always)]
#[allow(clippy::inline_always)] // part of the fast paths' heads
fn operand_int(f: &Frame, o: Operand) -> Option<i64> {
    let v = match o {
        Operand::Cv(cv) => f.cvs.get(cv as usize)?.get()?,
        Operand::Tmp(t) | Operand::TmpKeep(t) => f.tmps.get(t as usize)?,
        Operand::Lit(l) => f.code.literals.get(l as usize)?,
        Operand::Unused => return None,
    };
    match v {
        Value::Int(i) => Some(*i),
        _ => None,
    }
}

/// A copy of the element at int offset `i` of what the compiled variable
/// `cv` holds: of its array's element (`zend_hash_index_find`: by position
/// in a list, by key in a hash; the array held directly or through a
/// reference, the `ZVAL_DEREF` of a container), or the byte there of the
/// string it holds. `None` for a missing key and an element held by
/// reference (the general path warns, or reads through it), for a
/// reference that is being written, and for any other container.
#[inline(always)]
#[allow(clippy::inline_always)] // part of the second step's body
pub(super) fn int_elem_copy(f: &Frame, cv: u32, i: i64) -> Option<Value> {
    match f.cvs.get(cv as usize)?.get()? {
        Value::Array(arr) => elem_copy(int_elem(arr, i)?),
        Value::Reference(r) => match &*r.try_borrow()? {
            Value::Array(arr) => elem_copy(int_elem(arr, i)?),
            _ => None,
        },
        // `$str[$i]` inside the string, counted from its start: the shared
        // one-byte string (`zend_fetch_dimension_str`'s `ZSTR_CHAR`). An
        // offset from the end or outside the string is the general path's.
        Value::Str(s) => {
            let c = *s.as_bytes().get(usize::try_from(i).ok()?)?;
            Some(Value::Str(php_runtime::ZStr::from_byte(c)))
        }
        _ => None,
    }
}

/// The element of `arr` at int offset `i`: by position in a list (looked
/// at here), else by its key (`HashTable::get_int`, out of line).
#[inline(always)]
#[allow(clippy::inline_always)] // part of the second step's body
fn int_elem(arr: &ZArray, i: i64) -> Option<&Value> {
    match arr.get_packed(i) {
        Some(v) => Some(v),
        None => arr.get_int(i),
    }
}

/// A copy of an array element (`ZVAL_COPY`): a scalar's value, one more
/// handle of anything else. `None` for a reference.
#[inline(always)]
#[allow(clippy::inline_always)] // part of the second step's body
fn elem_copy(v: &Value) -> Option<Value> {
    Some(match v {
        Value::Int(x) => Value::Int(*x),
        Value::Float(x) => Value::Float(*x),
        Value::Bool(b) => Value::Bool(*b),
        Value::Null => Value::Null,
        Value::Reference(_) => return None,
        v => v.clone(),
    })
}

/// The store of the paths that read into a temporary: `v` into the
/// temporary `t` when that holds a scalar (nothing to release), and the
/// fall through to the next op; `None`, `v` dropped and nothing changed,
/// otherwise.
#[inline(always)]
#[allow(clippy::inline_always)] // the tail of the fast paths
fn store_into_tmp(f: &mut Frame, t: usize, v: Value) -> Option<Br> {
    let slot = f.tmps.get_mut(t)?;
    if !matches!(
        slot,
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
    ) {
        return None;
    }
    std::mem::forget(std::mem::replace(slot, v));
    Some(Br::Fall)
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
    unsafe { fetch_dim_key(frame, inst) }
}

/// The commonest `$v = $cv[$i]`, as `fetch_dim_cv_in` and
/// `store_fetched_in` make it, without their general steps: an int offset
/// (in a temporary, a variable or a literal) into a packed array the
/// variable holds directly (or into a string it holds: the byte there, as
/// a one-byte string), an element that is a scalar or a string, and a
/// folded `AssignCv` variable that holds no array, object or reference
/// (no possible garbage collection root is released). The element is
/// copied into the variable, an old string released, and the instruction
/// jumps past the `AssignCv` (`ZEND_FETCH_DIM_R` + `ZEND_ASSIGN` on these
/// operands). `None`, nothing changed, for anything else: the general path
/// runs.
#[inline(always)]
#[allow(clippy::inline_always)] // the head of `fetch_dim`
fn fetch_packed_into_cv(f: &mut Frame, inst: &Inst, i: i64) -> Option<Br> {
    if f.symtab_attached {
        return None;
    }
    let v = match f.cvs.get(inst.a as usize)?.get()? {
        Value::Array(arr) => plain_copy(arr.get_packed(i)?)?,
        // `$str[$i]` inside the string, counted from its start: the shared
        // one-byte string (`zend_fetch_dimension_str`'s `ZSTR_CHAR`). An
        // offset from the end or outside the string is the general path's.
        Value::Str(s) => {
            let c = *s.as_bytes().get(usize::try_from(i).ok()?)?;
            Value::Str(php_runtime::ZStr::from_byte(c))
        }
        _ => return None,
    };
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
fn assign_packed(f: &mut Frame, inst: &Inst, i: Option<i64>) -> Option<()> {
    if f.symtab_attached {
        return None;
    }
    // A table that is no list, and an array behind a reference, are the
    // second step's: left here, before the value is taken.
    if !matches!(
        f.cvs.get(inst.a as usize).and_then(CvSlot::get),
        Some(Value::Array(arr)) if arr.is_packed()
    ) {
        return None;
    }
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

/// `Op::FetchDimsCv`: `$cv[d1][d2]...` read: [`fetch_packed_two`], else
/// the general read ([`fetch_dims_general`]), as [`fetch_dim_int`].
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn fetch_dims(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract; `frame` is the current frame.
    if let Some(br) = fetch_packed_two(unsafe { &mut *frame }, inst) {
        return br;
    }
    // SAFETY: as above; the fast path's borrow of the frame has ended.
    unsafe { fetch_dims_general(frame, inst) }
}

/// The general `$cv[d1][d2]...` read (`fetch_dims_cv_in`), as
/// [`fetch_dim`].
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
#[allow(clippy::cast_possible_truncation)]
pub(super) unsafe fn fetch_dims_general(frame: *mut Frame, inst: &Inst) -> Br {
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
    let f = unsafe { &mut *frame };
    // The offset, as a read's ([`fetch_dim_int`]): none for an append, an
    // int for the lean path and the wider int step, anything else the
    // string-key step's.
    let i = if dim_kind(inst) == KIND_UNUSED {
        None
    } else {
        match int_dim(f, inst) {
            Some(i) => Some(i),
            // SAFETY: as above; the borrow of the frame has ended.
            None => return unsafe { assign_dim_key(frame, inst) },
        }
    };
    if assign_packed(f, inst, i).is_some() {
        return true;
    }
    // SAFETY: as above; the fast path's borrow of the frame has ended.
    unsafe { assign_int_any(frame, inst) }
}

/// The second step of a write with an int offset ([`assign_int_wide`]),
/// else the write with a string offset ([`assign_dim_key`]).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn assign_int_any(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `frame` is the current frame.
    if assign_int_wide(unsafe { &mut *frame }, inst).is_some() {
        return true;
    }
    // SAFETY: as above; the path's borrow of the frame has ended.
    unsafe { assign_dim_key(frame, inst) }
}

/// `TOp::AssignDimC`, and the int paths' way on: a write with a string
/// offset ([`assign_key`]), else the general write ([`assign_dim`]).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn assign_dim_key(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `frame` is the current frame.
    if assign_key(unsafe { &mut *frame }, inst).is_some() {
        return true;
    }
    // SAFETY: as above; the path's borrow of the frame has ended.
    unsafe { assign_dim(frame, inst) }
}

/// `$cv[$key] = $v` with a string offset, as [`assign_int_wide`]: the key
/// is a string a variable, a temporary or a literal holds, and the element
/// it names is a scalar or a string, or absent
/// (`HashTable::set_sym_plain`: one probe). A consumed key temporary is
/// released (`FREE_OP2`). `None`, nothing changed, for anything else.
#[inline(always)]
#[allow(clippy::inline_always)] // the body of `assign_dim_key`
fn assign_key(f: &mut Frame, inst: &Inst) -> Option<()> {
    if f.symtab_attached {
        return None;
    }
    let kind = dim_kind(inst);
    // One more handle of the key: the array's variable is written while it
    // is used.
    let key = key_str(f, kind, inst.b)?.clone();
    let v = plain_value(f, inst)?;
    let written = match f.cvs.get_mut(inst.a as usize).map(CvSlot::as_mut) {
        Some(Some(Value::Array(arr))) => write_key(arr, &key, v),
        Some(Some(Value::Reference(r))) => match r.try_borrow_mut() {
            Some(mut held) => match &mut *held {
                Value::Array(arr) => write_key(arr, &key, v),
                _ => Err(v),
            },
            None => Err(v),
        },
        _ => Err(v),
    };
    match written {
        Ok(()) => {
            release_key(f, kind, inst.b);
            Some(())
        }
        Err(v) => {
            give_back(f, inst, v);
            None
        }
    }
}

/// [`assign_key`]'s store into the array `arr`, when no other handle
/// shares its table (a shared one is separated by the general path).
#[inline(always)]
#[allow(clippy::inline_always)] // part of `assign_key`
fn write_key(arr: &mut ZArray, key: &ZStr, v: Value) -> Result<(), Value> {
    match arr.get_mut_unshared() {
        Some(tbl) => tbl.set_sym_plain(key, v),
        None => Err(v),
    }
}

/// `$cv[$i] = $v` and `$cv[] = $v`, wider than [`assign_packed`] and still
/// without the general steps of `assign_dim_cv_in`: the array may be held
/// through a reference, and its table any kind ([`store_int`]). `None`,
/// nothing changed (a moved value is put back), for anything else.
#[inline(always)]
#[allow(clippy::inline_always)] // the body of `assign_int_any`
fn assign_int_wide(f: &mut Frame, inst: &Inst) -> Option<()> {
    if f.symtab_attached {
        return None;
    }
    let i = if dim_kind(inst) == KIND_UNUSED {
        None
    } else {
        Some(int_dim(f, inst)?)
    };
    let v = plain_value(f, inst)?;
    match write_wide(f, inst.a, i, v) {
        Ok(()) => Some(()),
        Err(v) => {
            give_back(f, inst, v);
            None
        }
    }
}

/// The value operand of a write when it is a scalar or a string
/// (`ZEND_ASSIGN_DIM`'s `OP_DATA`): copied from a variable, a literal or a
/// kept temporary, moved out of a consumed one (which [`give_back`]
/// restores when the write does not happen).
#[inline(always)]
#[allow(clippy::inline_always)] // part of the second step's body
fn plain_value(f: &mut Frame, inst: &Inst) -> Option<Value> {
    match val_kind(inst) {
        KIND_CV => plain_copy(f.cvs.get(inst.p)?.get()?),
        KIND_LIT => plain_copy(f.code.literals.get(inst.p)?),
        KIND_TMPKEEP => plain_copy(f.tmps.get(inst.p)?),
        KIND_TMP => {
            let t = f.tmps.get_mut(inst.p)?;
            if !t.is_plain() {
                return None;
            }
            Some(std::mem::take(t))
        }
        _ => None,
    }
}

/// Puts the value [`plain_value`] moved out of a consumed temporary back:
/// the write it was for did not happen.
#[inline(always)]
#[allow(clippy::inline_always)] // part of the second step's body
fn give_back(f: &mut Frame, inst: &Inst, v: Value) {
    if val_kind(inst) == KIND_TMP {
        if let Some(t) = f.tmps.get_mut(inst.p) {
            // The slot holds the `Null` `take` left (nothing to release).
            std::mem::forget(std::mem::replace(t, v));
        }
    }
}

/// The write of [`assign_int_wide`]: `v` into the array variable `cv` holds
/// alone (directly, or through a reference that nothing else is reading or
/// writing), at int offset `i` or appended (`i` absent). `Err` gives the
/// value back, nothing changed.
#[inline(always)]
#[allow(clippy::inline_always)] // part of `assign_int_wide`
pub(super) fn write_wide(f: &mut Frame, cv: u32, i: Option<i64>, v: Value) -> Result<(), Value> {
    match f.cvs.get_mut(cv as usize).map(CvSlot::as_mut) {
        Some(Some(Value::Array(arr))) => write_into(arr, i, v),
        Some(Some(Value::Reference(r))) => {
            let Some(mut held) = r.try_borrow_mut() else {
                return Err(v);
            };
            match &mut *held {
                Value::Array(arr) => write_into(arr, i, v),
                _ => Err(v),
            }
        }
        _ => Err(v),
    }
}

/// [`write_wide`]'s store into the array `arr`, when no other handle
/// shares its table (a shared one is separated by the general path).
#[inline(always)]
#[allow(clippy::inline_always)] // part of `assign_int_wide`
fn write_into(arr: &mut ZArray, i: Option<i64>, v: Value) -> Result<(), Value> {
    let Some(tbl) = arr.get_mut_unshared() else {
        return Err(v);
    };
    match i {
        None => tbl.push(v).map(|_| ()),
        Some(i) => store_int(tbl, i, v),
    }
}

/// `tbl[i] = v` of a scalar or a string (`zend_hash_index_update`): over
/// an existing element that is a scalar or a string (one probe; anything
/// else is given back, nothing changed), or as a new element (a list's
/// next one, a hash's, with whatever growth or conversion that takes).
#[inline(always)]
#[allow(clippy::inline_always)] // part of `assign_int_wide`
fn store_int(tbl: &mut HashTable, i: i64, v: Value) -> Result<(), Value> {
    if let Some(slot) = tbl.get_int_mut(i) {
        if !slot.is_plain() {
            return Err(v);
        }
        discard_plain(std::mem::replace(slot, v));
        return Ok(());
    }
    // A new key: added as null, then stored over (nothing to release).
    let (slot, _) = tbl.lookup_ex(ArrayKey::Int(i));
    std::mem::forget(std::mem::replace(slot, v));
    Ok(())
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

/// `Op::AssignOpDimCv`: `$cv[dim] op= val`: [`assign_op_int`], else the
/// general compound write ([`assign_op_dim_general`]), as
/// [`fetch_dim_int`].
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn assign_op_dim(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `frame` is the current frame.
    if assign_op_int(unsafe { &mut *frame }, inst).is_some() {
        return true;
    }
    // SAFETY: as above; the fast path's borrow of the frame has ended.
    unsafe { assign_op_dim_general(frame, inst) }
}

/// The commonest `$cv[$k] op= $v` ([`TOp::AssignOpDimC`]), as
/// `assign_op_dim_cv_in` makes it, without its general steps: an int value
/// (in a variable, a temporary or a literal), an int or string key read
/// where it lies, the table the variable holds directly and alone (no
/// separation; no symbol table), an int element there, and `+`, `-`, `*`,
/// `|`, `&` or `^` without overflow (`ZEND_ASSIGN_DIM_OP` on these
/// operands: the element is updated where it is). A consumed string key
/// temporary is released. `None`, nothing changed, for anything else: the
/// general path runs.
#[inline(always)]
#[allow(clippy::inline_always)] // the head of `assign_op_dim`
fn assign_op_int(f: &mut Frame, inst: &Inst) -> Option<()> {
    /// The table `slot` holds directly, when no other handle shares it.
    fn table(slot: &mut Option<Value>) -> Option<&mut HashTable> {
        match slot {
            Some(Value::Array(arr)) => arr.get_mut_unshared(),
            _ => None,
        }
    }
    /// The element `key` (an int, or a string with symbol-table
    /// semantics) names in `tbl`.
    fn element<'t>(tbl: &'t mut HashTable, key: &Value) -> Option<&'t mut Value> {
        match key {
            Value::Int(i) => tbl.get_int_mut(*i),
            Value::Str(s) => str_element(tbl, s),
            _ => None,
        }
    }
    /// The element a string key names, the probe inlined
    /// (`HashTable::sym_zstr_probe_mut`). A function of its own: inside
    /// `element` the probe's registers were saved for an int key too (15
    /// instructions an element, zend:ary3 without the JIT).
    #[inline(never)]
    fn str_element<'t>(tbl: &'t mut HashTable, s: &ZStr) -> Option<&'t mut Value> {
        tbl.sym_zstr_probe_mut(s)
    }
    /// The variables `a` (to write) and `b` (to read) of `cvs`, when they
    /// are two: what `<[T]>::get_disjoint_mut` gives (Rust 1.86; the
    /// workspace states 1.85), and like it three tests in line.
    #[inline(always)]
    #[allow(clippy::inline_always)] // see above: as a call it cost 17 instructions
    fn two_slots(cvs: &mut [CvSlot], a: usize, b: usize) -> Option<(&mut CvSlot, &CvSlot)> {
        if a < b {
            let (lo, hi) = cvs.split_at_mut_checked(b)?;
            Some((lo.get_mut(a)?, hi.first()?))
        } else {
            let (lo, hi) = cvs.split_at_mut_checked(a)?;
            Some((hi.first_mut()?, lo.get(b)?))
        }
    }
    if f.symtab_attached {
        return None;
    }
    let y = match val_kind(inst) {
        KIND_CV => f.cvs.get(inst.p)?.get()?,
        KIND_TMP | KIND_TMPKEEP => f.tmps.get(inst.p)?,
        KIND_LIT => f.code.literals.get(inst.p)?,
        _ => return None,
    };
    let Value::Int(y) = *y else {
        return None;
    };
    let kind = dim_kind(inst);
    // The key is read where it lies while the array's variable is written:
    // two variables of the frame, or its variables and another of its
    // tables.
    let el = match kind {
        KIND_CV => {
            let (arr, key) = two_slots(&mut f.cvs, inst.a as usize, inst.b as usize)?;
            element(table(arr.as_mut())?, key.get()?)?
        }
        KIND_TMP | KIND_TMPKEEP => {
            let key = f.tmps.get(inst.b as usize)?;
            element(table(f.cvs.get_mut(inst.a as usize)?.as_mut())?, key)?
        }
        KIND_LIT => {
            let key = f.code.literals.get(inst.b as usize)?;
            element(table(f.cvs.get_mut(inst.a as usize)?.as_mut())?, key)?
        }
        _ => return None,
    };
    let Value::Int(x) = el else {
        return None;
    };
    *x = match inst.e {
        0 => x.checked_add(y)?,
        1 => x.checked_sub(y)?,
        2 => x.checked_mul(y)?,
        8 => *x | y,
        9 => *x & y,
        10 => *x ^ y,
        _ => return None,
    };
    release_key(f, kind, inst.b);
    Some(())
}

/// The general `$cv[dim] op= val` (`assign_op_dim_cv_in`).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
#[allow(clippy::cast_possible_truncation)]
unsafe fn assign_op_dim_general(frame: *mut Frame, inst: &Inst) -> bool {
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

/// `Op::Bin` of `.` into a temporary (`ZEND_CONCAT`), as `hot::step` runs
/// it (`concat_in`: strings and ints; a left temporary holding a string
/// nothing else holds is extended in place). The consumed temporaries are
/// released and the result replaces what its temporary held, so it polls.
/// Declines anything else, nothing changed: the generic op converts.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn concat_tmp(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `frame` is the current frame.
    let code = unsafe { &(*frame).code };
    // SAFETY: the literal table belongs to the op array the frame runs,
    // which stays alive and unchanged while the op does; it is no part of
    // the frame's own memory.
    let lits: &[Value] = unsafe { &*std::ptr::from_ref::<[Value]>(&code.literals) };
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    if f.tmps.len() <= inst.p {
        return false;
    }
    // A string that stays and an int: one allocation.
    if let Some(s) = concat_str_int(f, lits, inst) {
        put_tmp(&mut f.tmps[inst.p], Value::Str(s));
        return true;
    }
    let (a, b) = (
        operand(dim_kind(inst), inst.a),
        operand(val_kind(inst), inst.b),
    );
    let Some(s) = concat_in(f, lits, a, b) else {
        return false;
    };
    put_tmp(&mut f.tmps[inst.p], Value::Str(s));
    true
}

/// `a . b` of a string that stays where it is (a literal, a variable, a
/// kept temporary; a consumed temporary is the general path's, which
/// extends it in place) and an int (which a temporary keeps, as
/// `concat_in` leaves it): `ZStr::concat_long`. `None` for any other
/// operands.
#[inline(always)]
#[allow(clippy::inline_always)] // the head of `concat_tmp`
fn concat_str_int(f: &Frame, lits: &[Value], inst: &Inst) -> Option<ZStr> {
    let left = match dim_kind(inst) {
        KIND_CV => f.cvs.get(inst.a as usize)?.get()?,
        KIND_TMPKEEP => f.tmps.get(inst.a as usize)?,
        KIND_LIT => lits.get(inst.a as usize)?,
        _ => return None,
    };
    let Value::Str(s) = left else {
        return None;
    };
    let right = match val_kind(inst) {
        KIND_CV => f.cvs.get(inst.b as usize)?.get()?,
        KIND_TMP | KIND_TMPKEEP => f.tmps.get(inst.b as usize)?,
        KIND_LIT => lits.get(inst.b as usize)?,
        _ => return None,
    };
    let Value::Int(n) = right else {
        return None;
    };
    Some(s.concat_long(*n))
}

/// `Op::Rope` (`ZEND_ROPE_END`), as `hot::step_ext` runs it (`rope_in`:
/// parts that are strings and ints). The consumed temporaries are released
/// and the result replaces what its temporary held, so it polls. Declines
/// anything else, nothing changed: the generic op converts.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn rope_tmp(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `frame` is the current frame.
    let code = unsafe { &(*frame).code };
    // SAFETY: the ops and the literal table belong to the op array the
    // frame runs, which stays alive and unchanged while the op does; they
    // are no part of the frame's own memory.
    let (ops, lits): (&[Op], &[Value]) = unsafe {
        (
            &*std::ptr::from_ref::<[Op]>(&code.ops),
            &*std::ptr::from_ref::<[Value]>(&code.literals),
        )
    };
    let Some(Op::Rope { parts, .. }) = ops.get(inst.a as usize) else {
        return false;
    };
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    if f.tmps.len() <= inst.p {
        return false;
    }
    let Some(s) = rope_in(f, lits, parts) else {
        return false;
    };
    put_tmp(&mut f.tmps[inst.p], Value::Str(s));
    true
}

/// `Op::Cast` to a string of a string or an int (`ZEND_CAST` with
/// `IS_STRING`), as `hot::step_ext` runs it (`cast_string_in`); as
/// [`rope_tmp`].
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn cast_str_tmp(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `frame` is the current frame.
    let code = unsafe { &(*frame).code };
    // SAFETY: as in `rope_tmp`.
    let lits: &[Value] = unsafe { &*std::ptr::from_ref::<[Value]>(&code.literals) };
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    if f.tmps.len() <= inst.p {
        return false;
    }
    let Some(s) = cast_string_in(f, lits, operand(dim_kind(inst), inst.a)) else {
        return false;
    };
    put_tmp(&mut f.tmps[inst.p], Value::Str(s));
    true
}

/// `Op::FeFetch` of a by-value `foreach` over an array (`ZEND_FE_FETCH_R`
/// on `IS_ARRAY`: `fe_fetch_array_in`): the next element's value and key
/// in their temporaries and the fall through to the assignments of the
/// loop's variables, or the jump out of the loop at the end. Declines the
/// other iterators (by reference, objects, generators): `hot::step_ext`
/// and the generic op run them.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn fe_fetch(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    let key = (inst.d != NO_TO).then_some(inst.d);
    match fe_fetch_array_in(f, inst.a, inst.b, key) {
        Some(true) => Br::Fall,
        Some(false) => Br::Jump,
        None => Br::Decline,
    }
}

/// `TOp::FeFetchV`: a `foreach`'s step with the assignment of the value to
/// its variable ([`fe_fetch_into`]).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn fe_fetch_v(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: as in `fetch_dim`.
    fe_fetch_into(unsafe { &mut *frame }, inst, false)
}

/// `TOp::FeFetchKV`: the same with the key's assignment.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn fe_fetch_kv(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: as in `fetch_dim`.
    fe_fetch_into(unsafe { &mut *frame }, inst, true)
}

/// The step of a by-value `foreach` over an array together with the
/// assignments of the element to the loop's variables (`ZEND_FE_FETCH_R`
/// with a `CV` for the value, and the `ZEND_ASSIGN` of the key): the next
/// element's value (dereferenced) into the value's variable and its key
/// into the key's, and the fall through past those assignments; the jump
/// out of the loop at the end. Declines, nothing changed, another kind of
/// iterator (by reference, objects, generators) and a loop variable that
/// is a reference: `hot::step_ext` and the generic ops run those.
#[inline(always)]
#[allow(clippy::inline_always, clippy::cast_possible_truncation)] // the two handlers' body
fn fe_fetch_into(f: &mut Frame, inst: &Inst, with_key: bool) -> Br {
    let is_ref = |f: &Frame, cv: u32| {
        matches!(
            f.cvs.get(cv as usize).and_then(CvSlot::get),
            Some(Value::Reference(_))
        )
    };
    if f.symtab_attached || is_ref(f, inst.b) || (with_key && is_ref(f, inst.d)) {
        return Br::Decline;
    }
    let Some(Some(ForeachIter::Array { arr, pos })) = f.iters.get_mut(inst.a as usize) else {
        return Br::Decline;
    };
    let (v, k) = if with_key {
        let Some((p, v, k)) = arr.fe_fetch_with_key(*pos) else {
            return Br::Jump;
        };
        let v = copy_deref(v);
        *pos = p + 1;
        (v, Some(k))
    } else {
        let Some((p, v)) = arr.fe_fetch(*pos) else {
            return Br::Jump;
        };
        let v = copy_deref(v);
        *pos = p + 1;
        (v, None)
    };
    fe_assign(f, inst.b, inst.p as u32, v);
    if let Some(k) = k {
        fe_assign(f, inst.d, (inst.p >> 32) as u32, k);
    }
    Br::Fall
}

/// A copy of an element for a loop variable (`ZVAL_COPY_DEREF`).
#[inline(always)]
#[allow(clippy::inline_always)] // part of `fe_fetch_into`
fn copy_deref(v: &Value) -> Value {
    match v {
        Value::Int(x) => Value::Int(*x),
        Value::Float(x) => Value::Float(*x),
        Value::Bool(b) => Value::Bool(*b),
        Value::Null => Value::Null,
        Value::Reference(r) => r.get(),
        v => v.clone(),
    }
}

/// The assignment of a fetched value to a loop variable that is no
/// reference (`zend_assign_to_variable`): over nothing, a scalar or a
/// string the value is stored and an old string released; an array or an
/// object is released by the assignment itself (`hot::assign_cv_in`, from
/// the temporary `tmp` the unfused ops use: a possible root, a destructor).
#[inline(always)]
#[allow(clippy::inline_always)] // part of `fe_fetch_into`
fn fe_assign(f: &mut Frame, cv: u32, tmp: u32, v: Value) {
    let rooted = matches!(
        f.cvs.get(cv as usize).and_then(CvSlot::get),
        Some(Value::Array(_) | Value::Object(_))
    );
    if rooted {
        if let Some(slot) = f.tmps.get_mut(tmp as usize) {
            put_tmp(slot, v);
            // Never refused here: the variable holds an array or an object
            // and no symbol table is attached, the value is no reference.
            let assigned = crate::exec::hot::assign_cv_in(f, cv, Operand::Tmp(tmp), None);
            debug_assert!(assigned);
        }
    } else if let Some(slot) = f.cvs.get(cv as usize) {
        match slot.set(Some(v)) {
            // An old string by its own drop (one fewer handle, or freed).
            Some(Value::Str(s)) => drop(s),
            old @ (None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_))) => {
                std::mem::forget(old);
            }
            other => drop(other),
        }
    }
}

/// `Op::ConcatCv`: `$cv .= val` (`concat_cv_in`). Extends the string in
/// place and releases a consumed value temporary, so it polls.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
#[allow(clippy::cast_possible_truncation)]
pub(super) unsafe extern "C" fn concat_cv(frame: *mut Frame, inst: *const Inst) -> bool {
    // SAFETY: the caller's contract: a row of the frame's tape.
    let inst = unsafe { &*inst };
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    let val = operand(val_kind(inst), inst.p as u32);
    let dst = (inst.c != u32::MAX).then_some(inst.c >> SLOT_SHIFT);
    concat_cv_in(f, inst.a, val, dst)
}

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
    concat_cv_in, concat_in, fe_fetch_array_in, fetch_dim_cv_in, fetch_dim_is_miss_in,
    fetch_dims_cv_in, fetch_dims_is_miss_in, in_array_in, incdec_dim_cv_in, isset_cv_dims_in,
    isset_dim_cv_in, key_exists_in, put_tmp, rope_in,
};
use crate::opcode::{
    BinKind, CastKind, FetchMode, IncDecKind, Op, OpArray, Operand, PlaceBase, Seg,
};
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
pub(super) fn dim_kind(inst: &Inst) -> u8 {
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
fn fetch_op(code: &OpArray, dk: u8, di: u32, to: u32, made_str: bool) -> TOp {
    if made_str {
        return TOp::FetchDimC;
    }
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
fn assign_op(code: &OpArray, (dk, di): (u8, u32), (vk, vi): (u8, u32), made_str: bool) -> TOp {
    let lit = |i: u32| code.literals.get(i as usize);
    let dim_ok = match dk {
        KIND_LIT => matches!(lit(di), Some(Value::Int(_))),
        _ => !made_str,
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

/// Whether the key operand `dim` of the element access at op `i` is a
/// temporary that the op right before it (`Nop`s aside) makes and that
/// always holds a string then: a concatenation, an interpolation, a string
/// cast, or a frameless call of a function that returns a string. A hint
/// for the choice of the row (an int key in a string-key row takes its
/// general path, so a wrong guess costs time, never a result).
fn made_str(code: &OpArray, i: usize, dim: Operand) -> bool {
    /// Builtins whose frameless call returns a string (or throws).
    const STRING_BUILTINS: &[&[u8]] = &[
        b"dechex",
        b"decbin",
        b"decoct",
        b"chr",
        b"strtolower",
        b"strtoupper",
        b"trim",
        b"ltrim",
        b"rtrim",
        b"str_repeat",
        b"implode",
        b"substr",
        b"ucfirst",
        b"lcfirst",
    ];
    let (Operand::Tmp(t) | Operand::TmpKeep(t)) = dim else {
        return false;
    };
    let before = code.ops.get(..i).unwrap_or_default();
    let Some(op) = before.iter().rev().find(|op| !matches!(op, Op::Nop)) else {
        return false;
    };
    match op {
        Op::Rope { dst, .. }
        | Op::Bin {
            op: BinKind::Arith(BinaryOp::Concat),
            dst,
            ..
        }
        | Op::Cast {
            kind: CastKind::String,
            dst,
            ..
        } => *dst == t,
        // (A namespaced call may run a user function of that name.)
        Op::FramelessICall(fc) => {
            fc.dst == Some(t)
                && fc.ns.is_none()
                && matches!(
                    code.literals.get(fc.name as usize),
                    Some(Value::Str(name)) if STRING_BUILTINS.contains(&name.as_bytes())
                )
        }
        _ => false,
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
        // `in_array($x, ['a', 'b'])` with a literal haystack
        // (`ZEND_IN_ARRAY`): one key lookup (`spec::in_array_in`).
        Op::InArray { set, dst, .. } => {
            if !matches!(code.literals.get(set as usize), Some(Value::Array(_)))
                || dst.is_some_and(|d| d >= code.num_tmps)
            {
                return None;
            }
            Inst {
                d: u32::try_from(i).ok()?,
                ..plain(TOp::InArrayX)
            }
        }
        Op::FetchDimCv {
            cv,
            dim,
            dst,
            to,
            mode,
            ..
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
                e: u8::from(mode == FetchMode::Is),
                p: dst as usize,
                ..plain(fetch_op(code, dk, di, to, made_str(code, i, dim)))
            }
        }
        Op::FetchDimsCv {
            cv,
            place,
            dst,
            to,
            mode,
        } => {
            dst_off(code, dst)?;
            let (to, c) = folded_assign(code, i, to)?;
            Inst {
                a: ((cv as usize) < code.cv_names.len()).then_some(cv)?,
                b: to,
                c,
                d: place_idx(code, place)?,
                e: u8::from(mode == FetchMode::Is),
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
            // An offset in a variable or a temporary, an int literal and
            // a string literal that is no integer in digits are looked up
            // by the lean row ([`isset_dim_lean`]); a literal of another
            // type or in digits takes the general one.
            let op = match dk {
                KIND_CV | KIND_TMP | KIND_TMPKEEP => TOp::IssetDimL,
                KIND_LIT => match code.literals.get(di as usize) {
                    Some(Value::Int(_)) => TOp::IssetDimL,
                    Some(Value::Str(s)) if matches!(ArrayKey::from_zstr(s), ArrayKey::Str(_)) => {
                        TOp::IssetDimL
                    }
                    _ => TOp::IssetDimC,
                },
                _ => TOp::IssetDimC,
            };
            Inst {
                a: ((cv as usize) < code.cv_names.len()).then_some(cv)?,
                b: di,
                c: dst_off(code, dst)?,
                k: dk,
                e: u8::from(empty),
                ..plain(op)
            }
        }
        // `isset($cv[d1]...[dn])` / `empty(...)` with two or more offsets
        // (`spec::isset_cv_dims_in`): the place in `b`, the variable in
        // `a`, the result's slot in `c`.
        Op::Isset { place, dst } | Op::Empty { place, dst } => {
            let p = code.places.get(place as usize)?;
            let PlaceBase::Cv(cv) = p.base else {
                return None;
            };
            if p.segs.len() < 2 || !p.segs.iter().all(|s| matches!(s, Seg::Dim(_))) {
                return None;
            }
            Inst {
                a: ((cv as usize) < code.cv_names.len()).then_some(cv)?,
                b: place_idx(code, place)?,
                c: dst_off(code, dst)?,
                e: u8::from(matches!(code.ops[i], Op::Empty { .. })),
                ..plain(TOp::IssetDimsC)
            }
        }
        Op::KeyExists { key, array, dst } => {
            let (kk, ki) = val_operand(code, key)?;
            let (ak, ai) = val_operand(code, array)?;
            Inst {
                a: ai,
                b: ki,
                c: match dst {
                    Some(d) => dst_off(code, d)?,
                    None => NO_TO,
                },
                k: kk | (ak << 3),
                ..plain(TOp::KeyExistsC)
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
                ..plain(assign_op(code, (dk, di), (vk, vi), made_str(code, i, dim)))
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
        // An array literal with an element that is no constant: the
        // result temporary's index in `p`.
        Op::InitArray { dst } => {
            dst_off(code, dst)?;
            Inst {
                p: dst as usize,
                ..plain(TOp::InitArrayT)
            }
        }
        // One element of an array literal (`ZEND_ADD_ARRAY_ELEMENT`, and
        // `ZEND_INIT_ARRAY` for the first): the literal's temporary in
        // `a`, the value's kind in `k` and its index in `p`, and in the
        // low half of `e` whether it has a key, a string or int literal
        // (its index in `b`); any other key is the generic op's. For the
        // element that makes the table: the number of the literal's
        // elements in `d` and, in the high half of `e`, what their keys
        // are.
        Op::ArrayAdd { arr, key, val } => {
            dst_off(code, arr)?;
            let (vk, vi) = val_operand(code, val)?;
            let (e, b) = match key {
                None => (ADD_NEXT, 0),
                Some(Operand::Lit(l)) => match code.literals.get(l as usize)? {
                    // A key as it is: no integer in digits.
                    Value::Str(s) if matches!(ArrayKey::from_zstr(s), ArrayKey::Str(_)) => {
                        (ADD_STR, l)
                    }
                    Value::Str(_) | Value::Int(_) => (ADD_KEYED, l),
                    _ => return None,
                },
                Some(_) => return None,
            };
            // (The literal's shape goes to its first element afterwards:
            // `shape_literals`.)
            Inst {
                a: arr,
                b,
                d: 0,
                e: e | (KEYS_OTHER << KEYS_SHIFT),
                k: vk << 3,
                p: vi as usize,
                ..plain(TOp::ArrayAddT)
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
    match fetch_dim_cv_in(f, inst.a, dim, inst.p as u32, to) {
        Some(r) => fetched(Some(r)),
        // `$a['k'] ?? d` whose key is not there: `ZEND_FETCH_DIM_IS`'s
        // silent null, out of line.
        None if inst.e != 0 => fetched(fetch_dim_is_miss_in(f, inst.a, dim, inst.p as u32, to)),
        None => Br::Decline,
    }
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
    if fetch_str_lean(unsafe { &mut *frame }, inst) {
        return Br::Fall;
    }
    // SAFETY: the caller's contract, passed on; the lean path's borrow of
    // the frame has ended.
    unsafe { fetch_dim_key_any(frame, inst) }
}

/// The commonest `$cv[$key]` read with a string key, without a call until
/// the key is released: the key in a consumed temporary or a variable,
/// surely no integer in digits (`ArrayKey::surely_str`) and hashed here
/// when it was just built, the array held directly, its table a hash that
/// has the key, the element a plain value (a scalar or a string) copied
/// into the result temporary, which holds a scalar. The string of a
/// consumed key temporary is released (`FREE_OP2`). `false`, nothing
/// changed, for anything else.
#[inline(always)]
#[allow(clippy::inline_always)] // the head of `fetch_dim_key`
fn fetch_str_lean(f: &mut Frame, inst: &Inst) -> bool {
    if inst.d != NO_TO
        || !matches!(
            f.tmps.get(inst.p),
            Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_))
        )
    {
        return false;
    }
    let kind = dim_kind(inst);
    let key = match kind {
        KIND_TMP => f.tmps.get(inst.b as usize),
        KIND_CV => f.cvs.get(inst.b as usize).and_then(CvSlot::get),
        _ => return false,
    };
    let Some(Value::Str(key)) = key else {
        return false;
    };
    let Some(Value::Array(arr)) = f.cvs.get(inst.a as usize).and_then(CvSlot::get) else {
        return false;
    };
    let found = if ArrayKey::surely_str(key.as_bytes()) {
        arr.lean_find(key, key.hash_now())
    } else {
        // An integer in digits is the int key (`ZEND_HANDLE_NUMERIC_STR`).
        match ArrayKey::small_int(key.as_bytes()) {
            Some(i) => arr.lean_get_int(i),
            None => return false,
        }
    };
    let Some(found) = found else {
        return false;
    };
    if !found.is_plain() {
        return false;
    }
    let Some(slot) = f.tmps.get_mut(inst.p) else {
        return false;
    };
    // The slot holds a scalar (seen above): nothing to release.
    if let Value::Str(s) = found {
        std::mem::forget(std::mem::replace(slot, Value::Str(s.clone())));
    } else {
        // SAFETY: `found` is a scalar (plain and no string) in the array's
        // table, `slot` a temporary of the frame: both valid for 16 bytes
        // and distinct. A scalar owns nothing, so its bytes are its copy,
        // and the slot holds nothing to release. The safe form was
        // measured first: the value rebuilt through a jump on its tag left
        // zend:hash1 at 1.26 times the C count.
        unsafe {
            std::ptr::copy_nonoverlapping(
                std::ptr::from_ref(found).cast::<u8>(),
                std::ptr::from_mut(slot).cast::<u8>(),
                std::mem::size_of::<Value>(),
            );
        }
    }
    if kind == KIND_TMP {
        if let Some(slot) = f.tmps.get_mut(inst.b as usize) {
            if let Value::Str(key) = std::mem::take(slot) {
                drop(key);
            }
        }
    }
    true
}

/// [`fetch_dim_key`] past its lean path: a read with a string offset
/// ([`fetch_key`]), else the general read ([`fetch_dim`]).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn fetch_dim_key_any(frame: *mut Frame, inst: &Inst) -> Br {
    // The operand of `??` into its temporary and the read into a folded
    // variable have steps of their own (the row's fields alone are looked
    // at, before anything is touched).
    if inst.d != NO_TO {
        // SAFETY: the caller's contract, passed on.
        return unsafe { fetch_dim_to(frame, inst) };
    }
    if inst.e != 0 {
        // SAFETY: the caller's contract, passed on.
        return unsafe { fetch_dim_is(frame, inst) };
    }
    // SAFETY: the caller's contract, passed on.
    unsafe { fetch_dim_key_general(frame, inst) }
}

/// `$v = $cv[$key]` ([`fetch_to_lean`]), else [`fetch_dim_key_general`].
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn fetch_dim_to(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract; `frame` is the current frame.
    if let Some(br) = fetch_to_lean(unsafe { &mut *frame }, inst) {
        return br;
    }
    // SAFETY: the caller's contract, passed on; the lean path's borrow of
    // the frame has ended.
    unsafe { fetch_dim_key_general(frame, inst) }
}

/// `ZEND_FETCH_DIM_R` of a `CV` array with a string offset and the
/// `ZEND_ASSIGN` to a variable folded behind it (`$v = $a[$k]`): one
/// lookup with the key's stored hash (`zend_hash_find_known_hash`), or
/// the int key a string of digits is (`ZEND_HANDLE_NUMERIC_STR`), and the
/// element's copy into the variable. Taken when the key is a string in a
/// variable, a temporary or a literal, the array the one the variable
/// holds directly and has the key, the element no reference, and the
/// variable holds nothing a release could make a root of
/// ([`store_plain_into`]); a consumed key temporary is released
/// (`FREE_OP2`). The general path ([`fetch_key`]) looked the key's bytes
/// up for an integer and probed through a call. `None`, nothing changed,
/// for anything else (a missing key warns there).
#[inline(always)]
#[allow(clippy::inline_always)] // the body of `fetch_dim_to`
fn fetch_to_lean(f: &mut Frame, inst: &Inst) -> Option<Br> {
    if f.symtab_attached {
        return None;
    }
    let kind = dim_kind(inst);
    let v = {
        let key = key_str(f, kind, inst.b)?;
        let Value::Array(arr) = f.cvs.get(inst.a as usize)?.get()? else {
            return None;
        };
        let el = if ArrayKey::surely_str(key.as_bytes()) {
            arr.lean_find(key, key.hash_now())
        } else {
            arr.lean_get_int(ArrayKey::small_int(key.as_bytes())?)
        };
        elem_copy(el?)?
    };
    let br = store_plain_into(f, inst.d, v)?;
    release_key(f, kind, inst.b);
    Some(br)
}

/// [`fetch_dim_key_any`]'s way on: a read with a string offset
/// ([`fetch_key`]), else the general read ([`fetch_dim`]).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn fetch_dim_key_general(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract; `frame` is the current frame.
    if let Some(br) = fetch_key(unsafe { &mut *frame }, inst) {
        return br;
    }
    // SAFETY: as above; the path's borrow of the frame has ended.
    unsafe { fetch_dim(frame, inst) }
}

/// `$cv[$key] ?? d`'s read ([`fetch_is_lean`]), else
/// [`fetch_dim_key_general`].
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn fetch_dim_is(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract; `frame` is the current frame.
    if let Some(br) = fetch_is_lean(unsafe { &mut *frame }, inst) {
        return br;
    }
    // SAFETY: the caller's contract, passed on; the lean path's borrow of
    // the frame has ended.
    unsafe { fetch_dim_key_general(frame, inst) }
}

/// `ZEND_FETCH_DIM_IS` of a `CV` array and a string offset (the operand
/// of `??`) as php-src runs it: one lookup with the key's stored hash
/// (`zend_hash_find_known_hash`), the element copied into the result
/// temporary, or null when the key is not there (no diagnostic). Taken
/// when the key is a string in a variable, a temporary or a literal that
/// is surely no integer in digits, the array is the one the variable
/// holds directly (a list or an empty table has no such key) and the
/// element no reference; a consumed key temporary is released
/// (`FREE_OP2`). The general path looked the key up for the read, again
/// through the general read, and a third time for the miss
/// (`fetch_dim_is_miss_in`). `None`, nothing changed, for anything else.
#[inline(always)]
#[allow(clippy::inline_always)] // the body of `fetch_dim_is`
fn fetch_is_lean(f: &mut Frame, inst: &Inst) -> Option<Br> {
    let kind = dim_kind(inst);
    let v = {
        let key = key_str(f, kind, inst.b)?;
        if !ArrayKey::surely_str(key.as_bytes()) {
            return None;
        }
        let Value::Array(arr) = f.cvs.get(inst.a as usize)?.get()? else {
            return None;
        };
        match arr.lean_find(key, key.hash_now()) {
            Some(el) => elem_copy(el)?,
            None => Value::Null,
        }
    };
    let br = store_into_tmp(f, inst.p, v)?;
    release_key(f, kind, inst.b);
    Some(br)
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

/// A read of `$cv[key]` into the row's temporary that raises nothing when
/// the key is missing (`ZEND_FETCH_DIM_IS`, the operand of `??`; the
/// caller knows the mode, the row does not): null, when the variable
/// holds an array itself and that has no element under the string key
/// (`zend_symtable_find`: a canonical integer string is the integer key),
/// and the key temporary released as [`fetch_key`] releases it. `None`,
/// nothing changed, for anything else.
pub(super) fn fetch_key_missing(f: &mut Frame, inst: &Inst) -> Option<Br> {
    if inst.d != NO_TO {
        return None;
    }
    let kind = dim_kind(inst);
    {
        let key = key_str(f, kind, inst.b)?;
        let Value::Array(arr) = f.cvs.get(inst.a as usize)?.get()? else {
            return None;
        };
        if arr.get_sym_zstr(key).is_some() {
            return None;
        }
    }
    let br = store_into_tmp(f, inst.p, Value::Null)?;
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
pub(super) fn int_elem(arr: &ZArray, i: i64) -> Option<&Value> {
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
fn store_plain_into(f: &mut Frame, to: u32, v: Value) -> Option<Br> {
    let slot = f.cvs.get_mut(to as usize)?;
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
    match fetch_dims_cv_in(f, inst.a, inst.d, inst.p as u32, to) {
        Some(r) => fetched(Some(r)),
        // `$a['k']['j'] ?? d` with a key missing on the way (`Is` mode).
        None if inst.e != 0 => fetched(fetch_dims_is_miss_in(f, inst.a, inst.d, inst.p as u32, to)),
        None => Br::Decline,
    }
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

/// `TOp::IssetDimL`: `isset($cv[$key])` / `empty($cv[$key])` with an
/// int or a string offset ([`isset_lean`]), else [`isset_dim`].
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn isset_dim_lean(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    if let Some(r) = isset_lean(f, inst) {
        if let Some(slot) = f.tmps.get_mut((inst.c >> SLOT_SHIFT) as usize) {
            put_tmp(slot, Value::Bool(r));
            release_key(f, dim_kind(inst), inst.b);
            return true;
        }
    }
    // SAFETY: the caller's contract, passed on; the lean path's borrow of
    // the frame has ended.
    unsafe { isset_dim(frame, inst) }
}

/// `ZEND_ISSET_ISEMPTY_DIM_OBJ` of a `CV` array as php-src runs it: one
/// lookup (`zend_hash_find_known_hash` with a string's stored hash, or
/// `zend_hash_index_find`) and a type test, here without a call (a
/// literal's own bucket key matches by pointer). The offset is an int or
/// a string that is surely no integer in digits, in a variable, a
/// temporary or a literal; the array the one the variable holds directly
/// (a list or an empty table has no string key). `None` for anything
/// else ([`isset_dim`]: an offset of another type or in digits, a
/// reference, another container, an element whose truth needs a
/// conversion). A consumed key temporary is the caller's to release.
#[inline(always)]
#[allow(clippy::inline_always)] // the body of `isset_dim_lean`
fn isset_lean(f: &Frame, inst: &Inst) -> Option<bool> {
    let Some(Value::Array(arr)) = f.cvs.get(inst.a as usize).and_then(CvSlot::get) else {
        return None;
    };
    let key = match dim_kind(inst) {
        KIND_LIT => f.code.literals.get(inst.b as usize),
        KIND_CV => f.cvs.get(inst.b as usize).and_then(CvSlot::get),
        KIND_TMP | KIND_TMPKEEP => f.tmps.get(inst.b as usize),
        _ => None,
    };
    let found = match key? {
        Value::Str(s) if ArrayKey::surely_str(s.as_bytes()) => arr.lean_find(s, s.hash_now()),
        Value::Int(i) => arr.lean_get_int(*i),
        _ => return None,
    };
    let empty = inst.e != 0;
    let Some(v) = found else {
        return Some(empty);
    };
    if empty {
        return crate::exec::spec::plain_truth(v).map(|t| !t);
    }
    Some(match v {
        Value::Null => false,
        Value::Reference(r) => !r.try_borrow()?.is_null(),
        _ => true,
    })
}

/// `TOp::IssetDimsC`: `isset($cv[d1]...[dn])` / `empty(...)` of the
/// place in `b` (`ZEND_FETCH_DIM_IS` along the chain and
/// `ZEND_ISSET_ISEMPTY_DIM_OBJ` on the last container:
/// `spec::isset_cv_dims_in`), the result into the slot in `c`. Before,
/// the op had no row and left the tape for `hot::step_ext`, which runs
/// the same function: 27,600 times a request on a large framework
/// application's page. Declines, nothing changed, what that declines
/// (a string offset, `ArrayAccess`, a symbol table).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn isset_dims(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    let Some(r) = isset_cv_dims_in(f, inst.a, inst.b, inst.e != 0) else {
        return false;
    };
    let Some(slot) = f.tmps.get_mut((inst.c >> SLOT_SHIFT) as usize) else {
        return false;
    };
    put_tmp(slot, Value::Bool(r));
    true
}

/// `Op::KeyExists`: `array_key_exists($key, $array)` (`key_exists_in`):
/// the key's kind in the low bits of `k`, the array's above them, the
/// result's slot in `c` ([`NO_TO`]: none). Releases a consumed key and
/// array, so it polls.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn key_exists(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    let key = operand(dim_kind(inst), inst.b);
    let array = operand(val_kind(inst), inst.a);
    let dst = (inst.c != NO_TO).then_some(inst.c >> SLOT_SHIFT);
    key_exists_in(f, key, array, dst)
}

/// Whether variable `cv` of `f` holds an object: an `ArrayAccess`
/// container, whose write none of the array paths takes. Tested where the
/// first of them declines, so that the write goes on to the object's
/// dimension handler (`Vm::assign_dim_array_access`, by way of
/// `declined_vm_step`) without trying the others (~300 instructions a
/// write of a user `offsetSet()`).
#[inline(always)]
#[allow(clippy::inline_always)] // one test on a declined path
fn holds_object(f: &Frame, cv: usize) -> bool {
    matches!(f.cvs.get(cv).and_then(CvSlot::get), Some(Value::Object(_)))
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
    if holds_object(f, inst.a as usize) {
        return false;
    }
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
    if holds_object(f, inst.a as usize) {
        return false;
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
    if assign_str_lean(unsafe { &mut *frame }, inst) {
        return true;
    }
    // SAFETY: as above; the lean path's borrow of the frame has ended.
    if holds_object(unsafe { &*frame }, inst.a as usize) {
        return false;
    }
    // SAFETY: the caller's contract, passed on; the lean path's borrow of
    // the frame has ended.
    unsafe { assign_dim_key_any(frame, inst) }
}

/// The commonest `$cv[$key] = value` with a string key, without a call
/// until a key is released: the key in a consumed temporary, surely no
/// integer in digits and hashed here when it was just built, the value a
/// scalar in a variable or a literal, the table the variable holds
/// directly and alone, a hash. The element is a scalar written over, or a
/// new bucket in a table with room, which takes the temporary's string
/// itself (`zend_hash_update` adds a handle and `FREE_OP2` releases the
/// temporary's: one string either way). `false`, nothing changed, for
/// anything else.
#[inline(always)]
#[allow(clippy::inline_always)] // the head of `assign_dim_key`
fn assign_str_lean(f: &mut Frame, inst: &Inst) -> bool {
    if dim_kind(inst) != KIND_TMP || f.symtab_attached {
        return false;
    }
    let Some(Value::Str(key)) = f.tmps.get(inst.b as usize) else {
        return false;
    };
    // An integer in digits is the int key (`ZEND_HANDLE_NUMERIC_STR`).
    let int_key = if ArrayKey::surely_str(key.as_bytes()) {
        None
    } else {
        match ArrayKey::small_int(key.as_bytes()) {
            Some(i) => Some(i),
            None => return false,
        }
    };
    let v = match val_kind(inst) {
        KIND_CV => f.cvs.get(inst.p).and_then(CvSlot::get),
        KIND_LIT => f.code.literals.get(inst.p),
        _ => return false,
    };
    let v = match v {
        Some(Value::Int(i)) => Value::Int(*i),
        Some(Value::Float(x)) => Value::Float(*x),
        Some(Value::Bool(b)) => Value::Bool(*b),
        Some(Value::Null) => Value::Null,
        _ => return false,
    };
    let Some(Some(Value::Array(arr))) = f.cvs.get_mut(inst.a as usize).map(CvSlot::as_mut) else {
        return false;
    };
    let Some(table) = arr.get_mut_unshared() else {
        return false;
    };
    if let Some(i) = int_key {
        match table.lean_get_int_mut(i) {
            Some(el) => {
                if !matches!(
                    el,
                    Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
                ) {
                    return false;
                }
                // A scalar over a scalar: nothing to release.
                std::mem::forget(std::mem::replace(el, v));
            }
            None => {
                if table.lean_add_int(i, v).is_err() {
                    // (A scalar given back: nothing to release.)
                    return false;
                }
            }
        }
        // The key's string is released (`FREE_OP2`).
        if let Some(slot) = f.tmps.get_mut(inst.b as usize) {
            if let Value::Str(key) = std::mem::take(slot) {
                drop(key);
            }
        }
        return true;
    }
    let h = key.hash_now();
    if let Some(el) = table.lean_find_mut(key, h) {
        if !matches!(
            el,
            Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
        ) {
            return false;
        }
        // A scalar over a scalar: nothing to release.
        std::mem::forget(std::mem::replace(el, v));
        if let Some(slot) = f.tmps.get_mut(inst.b as usize) {
            if let Value::Str(key) = std::mem::take(slot) {
                drop(key);
            }
        }
        return true;
    }
    // A new key: the temporary's string moves into the bucket.
    let Some(slot) = f.tmps.get_mut(inst.b as usize) else {
        return false;
    };
    let Value::Str(key) = std::mem::take(slot) else {
        return false;
    };
    match table.lean_append(key, h, v) {
        Ok(()) => true,
        Err((key, _scalar)) => {
            // The slot holds the `Null` `take` left (nothing to release).
            std::mem::forget(std::mem::replace(slot, Value::Str(key)));
            false
        }
    }
}

/// [`assign_dim_key`] past its lean path: a write with a string offset
/// ([`assign_key`]), else the general write ([`assign_dim`]).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn assign_dim_key_any(frame: *mut Frame, inst: &Inst) -> bool {
    // A literal key has a step of its own (the operand's kind alone is
    // looked at, before anything is touched).
    if dim_kind(inst) == KIND_LIT {
        // SAFETY: the caller's contract, passed on.
        return unsafe { assign_dim_lit(frame, inst) };
    }
    // SAFETY: the caller's contract, passed on.
    unsafe { assign_dim_key_general(frame, inst) }
}

/// [`assign_dim_key_any`]'s way on: a write with a string offset
/// ([`assign_key`]), else the general write ([`assign_dim`]).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn assign_dim_key_general(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `frame` is the current frame.
    if assign_key(unsafe { &mut *frame }, inst).is_some() {
        return true;
    }
    // SAFETY: as above; the path's borrow of the frame has ended.
    unsafe { assign_dim(frame, inst) }
}

/// `$cv['key'] = value` with a string literal for the key
/// ([`assign_lit_lean`]), else [`assign_dim_key_general`].
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn assign_dim_lit(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `frame` is the current frame.
    if assign_lit_lean(unsafe { &mut *frame }, inst) {
        return true;
    }
    // SAFETY: as above; the lean path's borrow of the frame has ended.
    if holds_object(unsafe { &*frame }, inst.a as usize) {
        return false;
    }
    // SAFETY: the caller's contract, passed on; the lean path's borrow of
    // the frame has ended.
    unsafe { assign_dim_key_general(frame, inst) }
}

/// `$cv['key'] = value` as `ZEND_ASSIGN_DIM` with a `CONST` string offset
/// runs it: one lookup with the key's stored hash (`zend_hash_lookup`)
/// and a copy of the value. Taken when the key is a string literal that
/// is surely no integer in digits, the value a scalar or a string in a
/// variable or a literal, the table the variable holds directly and
/// alone, a hash, and the element a scalar or a string written over (an
/// old string released: no destructor can run) or a new bucket in a table
/// with room (it gets a handle of the literal, as `zend_hash_add_new`
/// does). The general path ([`assign_key`]) took a handle of the key
/// around the write, looked at its bytes for an integer every time, and
/// probed through `HashTable::set_sym_plain`'s call. `false`, nothing
/// changed, for anything else.
#[inline(always)]
#[allow(clippy::inline_always)] // the body of `assign_dim_lit`
fn assign_lit_lean(f: &mut Frame, inst: &Inst) -> bool {
    if f.symtab_attached {
        return false;
    }
    let Some(Value::Str(key)) = f.code.literals.get(inst.b as usize) else {
        return false;
    };
    if !ArrayKey::surely_str(key.as_bytes()) {
        return false;
    }
    let v = match val_kind(inst) {
        KIND_CV => f.cvs.get(inst.p).and_then(CvSlot::get),
        KIND_LIT => f.code.literals.get(inst.p),
        _ => return false,
    };
    let v = match v {
        Some(Value::Int(i)) => Value::Int(*i),
        Some(Value::Float(x)) => Value::Float(*x),
        Some(Value::Bool(b)) => Value::Bool(*b),
        Some(Value::Null) => Value::Null,
        // (On a miss below its handle goes with `v`: no destructor.)
        Some(Value::Str(s)) => Value::Str(s.clone()),
        _ => return false,
    };
    let Some(Some(Value::Array(arr))) = f.cvs.get_mut(inst.a as usize).map(CvSlot::as_mut) else {
        return false;
    };
    let Some(table) = arr.get_mut_unshared() else {
        return false;
    };
    let h = key.hash_now();
    if let Some(el) = table.lean_find_mut(key, h) {
        if !el.is_plain() {
            return false;
        }
        match std::mem::replace(el, v) {
            // An old string by its own drop (one handle fewer, or freed).
            Value::Str(s) => drop(s),
            // A scalar owns nothing.
            old => std::mem::forget(old),
        }
        return true;
    }
    // A new key: the bucket gets a handle of the literal.
    table.lean_append(key.clone(), h, v).is_ok()
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

/// `Op::AssignOpDimCv`: `$cv[dim] op= val`. A dispatcher with no stack
/// frame: `+=` with a key in a variable that holds a string goes to
/// [`assign_op_dim_str`], anything else to [`assign_op_dim_any`], each by a
/// tail call (an int key pays the tag test alone).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn assign_op_dim(frame: *mut Frame, inst: &Inst) -> bool {
    let str_key = inst.e == 0 && dim_kind(inst) == KIND_CV && {
        // SAFETY: the caller's contract; `frame` is the current frame.
        let f = unsafe { &*frame };
        matches!(
            f.cvs.get(inst.b as usize).and_then(CvSlot::get),
            Some(Value::Str(_))
        )
    };
    if str_key {
        // SAFETY: the caller's contract, passed on.
        unsafe { assign_op_dim_str(frame, inst) }
    } else {
        // SAFETY: the caller's contract, passed on.
        unsafe { assign_op_dim_any(frame, inst) }
    }
}

/// [`assign_op_dim`] for `+=` with a string key in a variable (what the
/// dispatcher saw; anything else would be taken for that):
/// [`assign_op_str`], else [`assign_op_dim_any`].
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn assign_op_dim_str(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `frame` is the current frame.
    if assign_op_str(unsafe { &mut *frame }, inst) {
        return true;
    }
    // SAFETY: the caller's contract, passed on; the lean path's borrow of
    // the frame has ended.
    unsafe { assign_op_dim_any(frame, inst) }
}

/// `$cv[$key] += value` (the instruction's operator and key kind, as
/// [`assign_op_dim`] found them) where the variable `$key` holds a string: the
/// value an int in a variable or a literal, the table the variable `$cv`
/// holds directly and alone, the element an int found by a probe that
/// makes no call (`HashTable::lean_zstr_probe_mut`: not a key that may be
/// an integer in digits), and no overflow. Nothing here calls, so a miss
/// costs its tests alone (an int key leaves at the test of the key's
/// tag). `false`, nothing changed, for anything else.
#[inline(always)]
#[allow(clippy::inline_always)] // the head of `assign_op_dim`
fn assign_op_str(f: &mut Frame, inst: &Inst) -> bool {
    // `+=` with the key in a variable: the dispatcher's tests. (The key's
    // tag is tested below, where the key is read.)
    if f.symtab_attached {
        return false;
    }
    let y = match val_kind(inst) {
        KIND_CV => f.cvs.get(inst.p).and_then(CvSlot::get),
        KIND_LIT => f.code.literals.get(inst.p),
        _ => return false,
    };
    let Some(&Value::Int(y)) = y else {
        return false;
    };
    let Some((arr, key)) = two_slots_mut(&mut f.cvs, inst.a as usize, inst.b as usize) else {
        return false;
    };
    let (Some(Value::Array(arr)), Some(Value::Str(key))) = (arr.as_mut(), key.get()) else {
        return false;
    };
    let Some(table) = arr.get_mut_unshared() else {
        return false;
    };
    let Some(Value::Int(x)) = table.lean_zstr_probe_mut(key) else {
        return false;
    };
    match x.checked_add(y) {
        Some(sum) => {
            *x = sum;
            true
        }
        None => false,
    }
}

/// Two variables of `cvs` at once, when they are two: what
/// `<[T]>::get_disjoint_mut` gives (Rust 1.86; the workspace states 1.85).
#[inline(always)]
#[allow(clippy::inline_always)] // as a call such a helper cost 17 instructions
fn two_slots_mut(cvs: &mut [CvSlot], a: usize, b: usize) -> Option<(&mut CvSlot, &mut CvSlot)> {
    if a < b {
        let (lo, hi) = cvs.split_at_mut_checked(b)?;
        Some((lo.get_mut(a)?, hi.first_mut()?))
    } else {
        let (lo, hi) = cvs.split_at_mut_checked(a)?;
        Some((hi.first_mut()?, lo.get_mut(b)?))
    }
}

/// [`assign_op_dim`] past its lean path: [`assign_op_int`], else the
/// general compound write ([`assign_op_dim_general`]), as
/// [`fetch_dim_int`].
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn assign_op_dim_any(frame: *mut Frame, inst: &Inst) -> bool {
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
    #[inline(always)]
    #[allow(clippy::inline_always)] // nested helpers are not inlined by themselves
    fn table(slot: &mut Option<Value>) -> Option<&mut HashTable> {
        match slot {
            Some(Value::Array(arr)) => arr.get_mut_unshared(),
            _ => None,
        }
    }
    /// The element `key` (an int, or a string with symbol-table
    /// semantics) names in `tbl`.
    #[inline(always)]
    #[allow(clippy::inline_always)] // as a call it cost 28 instructions an element
    fn element<'t>(tbl: &'t mut HashTable, key: &Value) -> Option<&'t mut Value> {
        match key {
            // A list through the always-inlined accessor (`get_int_mut`
            // had become a call here: 22 instructions an element).
            Value::Int(i) if tbl.is_packed() => tbl.get_packed_mut(*i),
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
/// it. The operand kinds, fixed at lowering, pick the path: a consumed
/// temporary on the left ([`concat_onto_tmp`], grown in place), a left
/// operand that stays ([`concat_new`], one new string), and anything the
/// two do not take ([`concat_general`]: `concat_in`). The consumed
/// temporaries are released and the result replaces what its temporary
/// held, so it polls. Declines anything else, nothing changed: the generic
/// op converts.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn concat_tmp(frame: *mut Frame, inst: &Inst) -> bool {
    if dim_kind(inst) == KIND_TMP {
        // SAFETY: the caller's contract, passed on.
        unsafe { concat_onto_tmp(frame, inst) }
    } else {
        // SAFETY: the caller's contract, passed on.
        unsafe { concat_new(frame, inst) }
    }
}

/// The value operand `b` of a concatenation row when it is a string, an
/// int or null (the empty string, converted silently) that stays where it
/// is (a variable, a temporary, a literal), read by the kind the row
/// records; `None` for any other value or kind.
#[inline(always)]
#[allow(clippy::inline_always)] // one kind test per operand in the lean paths
fn concat_rhs<'a>(f: &'a Frame, lits: &'a [Value], inst: &Inst) -> Option<&'a Value> {
    let v = match val_kind(inst) {
        KIND_CV => f.cvs.get(inst.b as usize)?.get()?,
        KIND_TMP | KIND_TMPKEEP => f.tmps.get(inst.b as usize)?,
        KIND_LIT => lits.get(inst.b as usize)?,
        _ => return None,
    };
    matches!(v, Value::Str(_) | Value::Int(_) | Value::Null).then_some(v)
}

/// Releases a consumed value temporary `b` of a concatenation row once
/// its string has been copied (an int needs nothing).
#[inline(always)]
#[allow(clippy::inline_always)]
fn concat_release_rhs(f: &mut Frame, inst: &Inst) {
    if val_kind(inst) == KIND_TMP {
        if let Some(slot) = f.tmps.get_mut(inst.b as usize) {
            if !matches!(slot, Value::Int(_)) {
                drop(std::mem::take(slot));
            }
        }
    }
}

/// `T . b` with a consumed temporary `T` holding a string and `b` a
/// string or an int: `ZEND_CONCAT`'s `zend_string_extend` of an operand
/// nobody else holds, in place while its block has room
/// (`ZStr::concat_owned_str_spare`), so a chain `a . b . c . d` grows one
/// string; an empty `T` yields `b` itself. Anything else goes to
/// [`concat_general`], nothing changed.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn concat_onto_tmp(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `frame` is the current frame.
    let code = unsafe { &(*frame).code };
    // SAFETY: as in `rope_tmp`.
    let lits: &[Value] = unsafe { &*std::ptr::from_ref::<[Value]>(&code.literals) };
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    let ta = inst.a as usize;
    let fits = f.tmps.len() > inst.p
        && matches!(f.tmps.get(ta), Some(Value::Str(_)))
        && !(matches!(val_kind(inst), KIND_TMP | KIND_TMPKEEP) && inst.b as usize == ta)
        && concat_rhs(f, lits, inst).is_some();
    if !fits {
        // SAFETY: the caller's contract, passed on.
        return unsafe { concat_general(frame, inst) };
    }
    let Some(Value::Str(s)) = f.tmps.get_mut(ta).map(std::mem::take) else {
        return false;
    };
    let r = match concat_rhs(f, lits, inst) {
        Some(Value::Str(t)) if s.is_empty() => t.clone(),
        // An empty LITERAL on the right is not `ZEND_CONCAT`'s shortcut
        // (that one needs `OP2_TYPE != IS_CONST`): a left string somebody
        // else holds, or an interned one, is copied into a new string
        // (`zend_string_alloc` + `memcpy`), which is what
        // `$row['k'] . ''` is written for; one nobody else holds is
        // extended by nothing and stays itself.
        Some(Value::Str(t))
            if t.is_empty()
                && val_kind(inst) == KIND_LIT
                && (s.refcount() != 1 || s.is_interned()) =>
        {
            ZStr::from(s.as_bytes())
        }
        Some(Value::Str(t)) => s.concat_owned_str_spare(t),
        Some(Value::Int(n)) => s.concat_owned_long_spare(*n),
        // `$s . null`: the string itself (`concat_function`'s empty
        // operand); also unreachable cases (checked above, and `b` is not
        // the temporary just taken).
        _ => s,
    };
    concat_release_rhs(f, inst);
    if let Some(slot) = f.tmps.get_mut(inst.p) {
        put_tmp(slot, Value::Str(r));
    }
    true
}

/// `a . b` with `a` a string that stays where it is (a variable, a kept
/// temporary, a literal) and `b` a string or an int: `ZEND_CONCAT`'s new
/// string, one allocation of the result's size class and two copies
/// (`ZStr::concat_str_spare`, `ZStr::concat_long_spare`); an empty
/// non-literal operand yields the other string itself. Anything else goes
/// to [`concat_general`], nothing changed.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn concat_new(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `frame` is the current frame.
    let code = unsafe { &(*frame).code };
    // SAFETY: as in `rope_tmp`.
    let lits: &[Value] = unsafe { &*std::ptr::from_ref::<[Value]>(&code.literals) };
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    if f.tmps.len() <= inst.p {
        return false;
    }
    let ak = dim_kind(inst);
    let left = match ak {
        KIND_CV => f.cvs.get(inst.a as usize).and_then(CvSlot::get),
        KIND_TMPKEEP => f.tmps.get(inst.a as usize),
        KIND_LIT => lits.get(inst.a as usize),
        _ => None,
    };
    let r = match (left, concat_rhs(f, lits, inst)) {
        (Some(Value::Str(s)), Some(Value::Str(t))) => {
            if s.is_empty() && ak != KIND_LIT {
                t.clone()
            } else if t.is_empty() && val_kind(inst) != KIND_LIT {
                s.clone()
            } else {
                ZStr::concat_str_spare(s, t)
            }
        }
        (Some(Value::Str(s)), Some(Value::Int(n))) => s.concat_long_spare(*n),
        // `$s . null`: the string itself, as with an empty string.
        (Some(Value::Str(s)), Some(Value::Null)) => s.clone(),
        _ => {
            // SAFETY: the caller's contract, passed on.
            return unsafe { concat_general(frame, inst) };
        }
    };
    concat_release_rhs(f, inst);
    if let Some(slot) = f.tmps.get_mut(inst.p) {
        put_tmp(slot, Value::Str(r));
    }
    true
}

/// The concatenations the lean paths do not take (an int on the left, a
/// float, anything that converts): `concat_in`, which declines, nothing
/// changed, for all but strings and ints.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn concat_general(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `frame` is the current frame.
    let code = unsafe { &(*frame).code };
    // SAFETY: as in `rope_tmp`.
    let lits: &[Value] = unsafe { &*std::ptr::from_ref::<[Value]>(&code.literals) };
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    if f.tmps.len() <= inst.p {
        return false;
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

/// The high half of [`Inst::e`] of a [`TOp::ArrayAddT`] says what the keys
/// of its literal are.
const KEYS_SHIFT: u8 = 4;
/// No element of the literal has a key (a list: its storage is made with
/// the table).
const KEYS_NONE: u8 = 0;
/// A key is a string that is no integer in digits
/// (`ZEND_ARRAY_NOT_PACKED`: a hash from the start).
const KEYS_STRING: u8 = 1;
/// Keys, none of them such a string: the first element decides the
/// storage, as in an uninitialised table of php-src.
const KEYS_OTHER: u8 = 2;

/// The low half of [`Inst::e`] of a [`TOp::ArrayAddT`]: the element takes
/// the next index.
const ADD_NEXT: u8 = 0;
/// The element has an int literal key or a string literal key that is an
/// integer in digits (the key it stands for is made per add).
const ADD_KEYED: u8 = 1;
/// The element has a string literal key that is no integer in digits: the
/// key as it is, hashed once and kept on the literal (php-src's literal
/// keys are interned, their hash stored on them).
const ADD_STR: u8 = 2;

/// How far [`literal_shape`] looks for the elements of a literal.
const LITERAL_SCAN: usize = 4096;

/// The number of elements of each array literal of `code` and what their
/// keys are ([`KEYS_NONE`], [`KEYS_STRING`], [`KEYS_OTHER`]): the size and
/// the `ZEND_ARRAY_NOT_PACKED` flag `zend_compile_array` gives
/// `ZEND_INIT_ARRAY`, written into the instruction of the literal's first
/// element (the one that makes its table: [`array_add`]; the others carry
/// none). The elements are the adds to the literal's temporary after the
/// `InitArray` that started it, up to the next literal started in that
/// temporary and within [`LITERAL_SCAN`] ops of the start; a literal whose
/// first add is not within that distance, or that an unpack starts, has no
/// shape (its table takes the shape of its first element). One pass over
/// the ops: each element working the shape out for itself scanned the
/// literal once per element (Composer's 18k-entry class map took a second
/// to decode).
pub(super) fn shape_literals(code: &OpArray, tape: &mut [Inst]) {
    /// What came first among a literal's elements.
    enum First {
        /// Nothing yet.
        Nothing,
        /// The add at this index.
        Add(usize),
        /// An unpack (which makes the table).
        Unpack,
    }
    /// An open literal: its `InitArray`, its first element, its element
    /// count and key kind.
    struct Open {
        start: usize,
        first: First,
        count: u32,
        keys: u8,
    }
    let finish = |o: Open, tape: &mut [Inst]| {
        if let First::Add(f) = o.first {
            if let Some(inst) = tape.get_mut(f) {
                if inst.op == TOp::ArrayAddT {
                    inst.d = o.count;
                    inst.e = (inst.e & ((1 << KEYS_SHIFT) - 1)) | (o.keys << KEYS_SHIFT);
                }
            }
        }
    };
    let mut open: std::collections::HashMap<u32, Open> = std::collections::HashMap::new();
    for (i, op) in code.ops.iter().enumerate() {
        match op {
            Op::InitArray { dst } => {
                let fresh = Open {
                    start: i,
                    first: First::Nothing,
                    count: 0,
                    keys: KEYS_NONE,
                };
                if let Some(o) = open.insert(*dst, fresh) {
                    finish(o, tape);
                }
            }
            Op::ArrayAdd { arr, key, .. } | Op::ArrayAddRef { arr, key, .. } => {
                let Some(o) = open.get_mut(arr) else {
                    continue;
                };
                if i - o.start > LITERAL_SCAN {
                    continue;
                }
                if matches!(o.first, First::Nothing) {
                    o.first = First::Add(i);
                }
                o.count = o.count.saturating_add(1);
                let string = matches!(
                    key,
                    Some(Operand::Lit(l)) if matches!(
                        code.literals.get(*l as usize),
                        Some(Value::Str(s)) if matches!(ArrayKey::from_zstr(s), ArrayKey::Str(_))
                    )
                );
                if string {
                    o.keys = KEYS_STRING;
                } else if key.is_some() && o.keys == KEYS_NONE {
                    o.keys = KEYS_OTHER;
                }
            }
            Op::ArrayUnpack { arr, .. } | Op::ConstArrayUnpack { arr, .. } => {
                if let Some(o) = open.get_mut(arr) {
                    if matches!(o.first, First::Nothing) {
                        o.first = First::Unpack;
                    }
                }
            }
            _ => {}
        }
    }
    for (_, o) in open.drain() {
        finish(o, tape);
    }
}

/// `Op::InitArray`: the shared empty table in the result temporary, as
/// the generic op puts it there (the first element makes the literal's own
/// table: [`array_add`]). Replaces what the temporary held, so it polls.
/// Declines only a temporary out of range.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn init_array(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    let Some(slot) = f.tmps.get_mut(inst.p) else {
        return false;
    };
    put_tmp(slot, Value::Array(ZArray::new()));
    true
}

/// `Op::ArrayAdd` (`ZEND_ADD_ARRAY_ELEMENT`; `ZEND_INIT_ARRAY` for the
/// first element): the value, read where it lies (a variable's or a
/// literal's copied, a temporary's moved), added to the literal's table
/// under the next index or its string/int literal key (a string with
/// symbol-table semantics), as `Vm::array_literal_add` does. The first
/// element finds the untouched shared table `InitArray` left and makes the
/// literal's own in its place, sized for all the elements
/// (`zend_new_array(size)`) with the storage that element brings in
/// php-src: a list's when no element has a key, a hash's when a key is a
/// string, else what the element itself needs. A value replaced under a
/// key given twice is released, so it polls. Declines, nothing changed: a
/// table that is neither that one nor the temporary's alone, an undefined
/// variable (the generic op warns), a reference, and a next index that is
/// taken (the generic op throws).
///
/// Each kind of key that lowering told apart (the low half of `e`) has a
/// handler of its own, reached from here by a tail call: the next index
/// ([`array_add_next`]), a string key as it is ([`array_add_str`]), and
/// any other literal key ([`array_add_keyed`]).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn array_add(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract, passed on.
    unsafe {
        match inst.e & ((1 << KEYS_SHIFT) - 1) {
            ADD_NEXT => array_add_next(frame, inst),
            ADD_STR => array_add_str(frame, inst),
            _ => array_add_keyed(frame, inst),
        }
    }
}

/// Whether the literal's temporary `arr` holds the untouched shared table
/// `InitArray` left (`Some(true)`: this element makes the literal's own) or
/// a table of its own (`Some(false)`); `None` for anything else, which the
/// generic op takes.
#[inline(always)]
#[allow(clippy::inline_always)] // a few tests in each element's handler
fn literal_first(f: &mut Frame, arr: u32) -> Option<bool> {
    match f.tmps.get_mut(arr as usize) {
        Some(Value::Array(a)) => {
            if a.get_mut_unshared().is_some() {
                Some(false)
            } else if a.is_untouched() {
                Some(true)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// A copy of `v` without a call into `Value::clone` for the commonest
/// elements: a scalar's bits, a string's or an array's handle (one count
/// up); objects, resources and references take the general clone.
#[inline(always)]
#[allow(clippy::inline_always)] // a tag test in each element's handler
fn copied(v: &Value) -> Value {
    match v {
        Value::Null => Value::Null,
        Value::Bool(b) => Value::Bool(*b),
        Value::Int(i) => Value::Int(*i),
        Value::Float(x) => Value::Float(*x),
        Value::Str(s) => Value::Str(s.clone()),
        Value::Array(a) => Value::Array(a.clone()),
        _ => v.clone(),
    }
}

/// The element's value (operand kind in `k`, index in `p`): a variable's
/// or a literal's copied, a temporary's moved out of it. `None`, nothing
/// changed, for an undefined variable, a reference or an operand out of
/// range: the generic op's.
#[inline(always)]
#[allow(clippy::inline_always)] // the operand read of each element's handler
fn literal_value(f: &mut Frame, lits: &[Value], inst: &Inst) -> Option<Value> {
    match val_kind(inst) {
        KIND_CV => match f.cvs.get(inst.p).and_then(CvSlot::get) {
            None | Some(Value::Reference(_)) => None,
            Some(v) => Some(copied(v)),
        },
        KIND_TMP => match f.tmps.get_mut(inst.p) {
            None | Some(Value::Reference(_)) => None,
            Some(slot) => Some(std::mem::take(slot)),
        },
        KIND_TMPKEEP => match f.tmps.get(inst.p) {
            None | Some(Value::Reference(_)) => None,
            Some(v) => Some(copied(v)),
        },
        KIND_LIT => lits.get(inst.p).map(copied),
        _ => None,
    }
}

/// The literal's table in the temporary `arr`, made first when this is its
/// first element ([`literal_first`]: `first`). `None` only for a temporary
/// that no longer holds what [`literal_first`] found, which nothing
/// between the two changes.
#[inline(always)]
#[allow(clippy::inline_always)] // see above
fn literal_table<'f>(f: &'f mut Frame, inst: &Inst, first: bool) -> Option<&'f mut HashTable> {
    let Some(Value::Array(arr)) = f.tmps.get_mut(inst.a as usize) else {
        return None;
    };
    if first {
        new_literal_table(arr, inst);
    }
    arr.get_mut_unshared()
}

/// The table of an array literal in place of the shared empty one,
/// sized for all its elements (`d`) with the storage its keys (the high
/// half of `e`) give it in php-src's `zend_compile_array`: a list's when no
/// element has a key, a hash's when a key is a string that is no integer
/// in digits (`ZEND_ARRAY_NOT_PACKED`), else none yet (the first element
/// decides). Once per literal.
#[inline(never)]
fn new_literal_table(arr: &mut ZArray, inst: &Inst) {
    let size = inst.d as usize;
    *arr = match inst.e >> KEYS_SHIFT {
        KEYS_NONE => ZArray::packed_sized(size),
        KEYS_STRING => ZArray::mixed_sized(size),
        _ => ZArray::with_capacity(size),
    };
}

/// An element that was not added: a value taken from its temporary goes
/// back there for the generic op (a copy is released).
#[cold]
#[inline(never)]
fn literal_decline(f: &mut Frame, inst: &Inst, v: Value) -> bool {
    if val_kind(inst) == KIND_TMP {
        if let Some(slot) = f.tmps.get_mut(inst.p) {
            *slot = v;
        }
    }
    false
}

/// [`array_add`] of an element with no key: `zend_hash_next_index_insert`,
/// the list's append without a call while it has room
/// ([`HashTable::lean_push`]), else the general push. Declines a next
/// index that is taken (the generic op throws).
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn array_add_next(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `frame` is the current frame.
    let code = unsafe { &(*frame).code };
    // SAFETY: as in `rope_tmp`.
    let lits: &[Value] = unsafe { &*std::ptr::from_ref::<[Value]>(&code.literals) };
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    let Some(first) = literal_first(f, inst.a) else {
        return false;
    };
    let Some(v) = literal_value(f, lits, inst) else {
        return false;
    };
    let Some(table) = literal_table(f, inst, first) else {
        return literal_decline(f, inst, v);
    };
    let Err(v) = table.lean_push(v) else {
        return true;
    };
    match table.push(v) {
        Ok(_) => true,
        Err(v) => literal_decline(f, inst, v),
    }
}

/// [`array_add`] of an element whose key is a string literal that is no
/// integer in digits (`zend_hash_update` with the interned key): its hash
/// read from the literal, the key probed and appended without a call in a
/// hash with room ([`HashTable::lean_find_mut`],
/// [`HashTable::lean_append`]), else the general insert. A key given twice
/// keeps its first position and takes the later value; the replaced value
/// is released.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn array_add_str(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `frame` is the current frame.
    let code = unsafe { &(*frame).code };
    // SAFETY: as in `rope_tmp`.
    let lits: &[Value] = unsafe { &*std::ptr::from_ref::<[Value]>(&code.literals) };
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    let Some(Value::Str(key)) = lits.get(inst.b as usize) else {
        return false;
    };
    let Some(first) = literal_first(f, inst.a) else {
        return false;
    };
    let Some(v) = literal_value(f, lits, inst) else {
        return false;
    };
    let Some(table) = literal_table(f, inst, first) else {
        return literal_decline(f, inst, v);
    };
    let h = key.hash_value();
    if let Some(slot) = table.lean_find_mut(key, h) {
        drop(std::mem::replace(slot, v));
        return true;
    }
    if let Err((key, v)) = table.lean_append(key.clone(), h, v) {
        drop(table.insert(ArrayKey::Str(key), v));
    }
    true
}

/// [`array_add`] of an element with an int literal key or a string
/// literal key that is an integer in digits (symbol-table semantics: the
/// int key it stands for): the general insert.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn array_add_keyed(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `frame` is the current frame.
    let code = unsafe { &(*frame).code };
    // SAFETY: as in `rope_tmp`.
    let lits: &[Value] = unsafe { &*std::ptr::from_ref::<[Value]>(&code.literals) };
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    let key = match lits.get(inst.b as usize) {
        Some(Value::Str(s)) => ArrayKey::from_zstr(s),
        Some(Value::Int(i)) => ArrayKey::Int(*i),
        _ => return false,
    };
    let Some(first) = literal_first(f, inst.a) else {
        return false;
    };
    let Some(v) = literal_value(f, lits, inst) else {
        return false;
    };
    let Some(table) = literal_table(f, inst, first) else {
        return literal_decline(f, inst, v);
    };
    drop(table.insert(key, v));
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
    if fe_step_plain(unsafe { &mut *frame }, inst, false) {
        return Br::Fall;
    }
    // SAFETY: the caller's contract, passed on; the lean path's borrow of
    // the frame has ended.
    unsafe { fe_fetch_v_any(frame, inst) }
}

/// [`fe_fetch_v`] past its lean path.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn fe_fetch_v_any(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    if fe_at_end(f, inst.a) {
        return Br::Jump;
    }
    fe_fetch_into(f, inst, false)
}

/// `TOp::FeFetchKV`: the same with the key's assignment.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe fn fe_fetch_kv(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: as in `fetch_dim`.
    if fe_step_plain(unsafe { &mut *frame }, inst, true) {
        return Br::Fall;
    }
    // SAFETY: the caller's contract, passed on; the lean path's borrow of
    // the frame has ended.
    unsafe { fe_fetch_kv_any(frame, inst) }
}

/// [`fe_fetch_kv`] past its lean path.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn fe_fetch_kv_any(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    if fe_at_end(f, inst.a) {
        return Br::Jump;
    }
    fe_fetch_into(f, inst, true)
}

/// Whether by-value array iterator `iter` stands past its table's last
/// used slot: the step every loop ends with, which the lean path declines
/// (no element at the position). The loop jumps out with nothing changed,
/// as [`fe_fetch_into`] and the generic op do there (`ZEND_FE_FETCH_R`'s
/// `fe_fetch_r_exit`: no variable written, the position kept), whatever
/// the loop variables hold, so no other test comes first.
#[inline(always)]
#[allow(clippy::inline_always)] // two loads and a compare in the miss handlers
fn fe_at_end(f: &Frame, iter: u32) -> bool {
    matches!(
        f.iters.get(iter as usize),
        Some(Some(ForeachIter::Array { arr, pos })) if *pos >= arr.used()
    )
}

/// The commonest step of a `foreach` over an array, as [`fe_fetch_into`]
/// makes it but without its general steps and without a call: the element
/// at exactly the iterator's position (no hole before it, not the end), a
/// plain value (a scalar or a string), and loop variables that hold
/// nothing or a plain value. The value and the key are stored, the strings
/// the variables held released, the iterator moved on. `false`, nothing
/// changed, for anything else.
#[inline(always)]
#[allow(clippy::inline_always)] // the head of the two handlers
fn fe_step_plain(f: &mut Frame, inst: &Inst, with_key: bool) -> bool {
    /// Whether a loop variable holds nothing or a plain value: what the
    /// step replaces with no more than the release of a string.
    #[inline(always)]
    #[allow(clippy::inline_always)] // two tag tests
    fn plain_cell(cell: Option<&Value>) -> bool {
        matches!(
            cell,
            None | Some(
                Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_)
            )
        )
    }
    /// Releases what such a variable held: a string by its own drop (one
    /// handle fewer, or freed), nothing for anything else.
    #[inline(always)]
    #[allow(clippy::inline_always)] // a tag test
    fn release_plain(old: Option<Value>) {
        match old {
            Some(Value::Str(s)) => drop(s),
            // Nothing, or a scalar: it owns nothing.
            other => std::mem::forget(other),
        }
    }
    if f.symtab_attached {
        return false;
    }
    let (to, key_to) = if with_key {
        let Some((to, key_to)) = two_slots_mut(&mut f.cvs, inst.b as usize, inst.d as usize) else {
            return false;
        };
        (to.as_mut(), Some(key_to.as_mut()))
    } else {
        let Some(to) = f.cvs.get_mut(inst.b as usize) else {
            return false;
        };
        (to.as_mut(), None)
    };
    if !plain_cell(to.as_ref()) || key_to.as_ref().is_some_and(|k| !plain_cell(k.as_ref())) {
        return false;
    }
    let Some(Some(ForeachIter::Array { arr, pos })) = f.iters.get_mut(inst.a as usize) else {
        return false;
    };
    let at = *pos;
    let Some((v, key)) = arr.entry_at(at) else {
        return false;
    };
    if !v.is_plain() {
        return false;
    }
    *pos = at + 1;
    // (A `match`: as an `Option::map` closure it was a call, 19 an element.)
    let old_key = match key_to {
        Some(key_to) => key_to.replace(match key {
            Some(ArrayKey::Str(s)) => Value::Str(s.clone()),
            Some(ArrayKey::Int(i)) => Value::Int(*i),
            None => Value::Int(i64::from(at)),
        }),
        None => None,
    };
    let old = if let Value::Str(s) = v {
        to.replace(Value::Str(s.clone()))
    } else {
        // SAFETY: `v` is a scalar (plain and no string: seen above) in the
        // iterator's table, `to` a variable of the frame: both valid for
        // 16 bytes and distinct. The old value is moved out by its bytes
        // (`old` owns it from here) and the slot is written over at once,
        // before anything reads it, so nothing is released twice. A scalar
        // owns nothing, so its bytes are its copy, and a `Some` has its
        // value's layout (the tag is the value's own: the trace JIT's
        // `value_layout` test pins it). The safe form was measured first:
        // the value rebuilt through a jump on its tag and stored tag and
        // payload apart left zend:hash2 at 1.23 times the C count.
        unsafe {
            let old = std::ptr::read(to);
            std::ptr::copy_nonoverlapping(
                std::ptr::from_ref(v).cast::<u8>(),
                std::ptr::from_mut(to).cast::<u8>(),
                std::mem::size_of::<Value>(),
            );
            old
        }
    };
    release_plain(old);
    release_plain(old_key);
    true
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
    fe_assign(f, inst.b, v);
    if let Some(k) = k {
        fe_assign(f, inst.d, k);
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
/// string the value is stored and an old string released; an old array or
/// object is released after the store (`release_rooted`: a possible root,
/// a destructor).
#[inline(always)]
#[allow(clippy::inline_always)] // part of `fe_fetch_into`
fn fe_assign(f: &mut Frame, cv: u32, v: Value) {
    let rooted = matches!(
        f.cvs.get(cv as usize).and_then(CvSlot::get),
        Some(Value::Array(_) | Value::Object(_))
    );
    if rooted {
        // `hot::assign_cv_in`'s store, without the temporary's round trip:
        // the variable holds an array or an object, no symbol table is
        // attached (`fe_fetch_into` tested both), the value is no
        // reference. The new value is stored first, then the old one
        // released (a possible root, or a destructor that may run).
        if let Some(slot) = f.cvs.get_mut(cv as usize) {
            let old = slot.set(Some(v));
            crate::exec::release_rooted(old);
        }
    } else if let Some(slot) = f.cvs.get_mut(cv as usize) {
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

/// `Op::ConcatCv`: `$cv .= val`. Without a used result the lean path
/// ([`concat_cv_spare`]) takes it; otherwise, or when that declines,
/// `concat_cv_in` ([`concat_cv_general`]). Extends the string in place and
/// releases a consumed value temporary, so it polls.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
pub(super) unsafe extern "C" fn concat_cv(frame: *mut Frame, inst: *const Inst) -> bool {
    // SAFETY: the caller's contract: a row of the frame's tape.
    let inst = unsafe { &*inst };
    if inst.c == u32::MAX {
        // SAFETY: the caller's contract, passed on.
        unsafe { concat_cv_spare(frame, inst) }
    } else {
        // SAFETY: the caller's contract, passed on.
        unsafe { concat_cv_general(frame, inst) }
    }
}

/// `$cv .= val` with no used result, a frame without a symbol table, the
/// variable holding a string directly and `val` a string or an int (a
/// literal, another defined variable holding one directly, a temporary):
/// `concat_function` with `result == op1`. A string nobody else holds
/// grows in place, with no call to the allocator while its block has room
/// (`ZStr::append_str_spare`: `zend_string_extend` within a size class); a
/// shared one is copied once into a string of the final size. Anything
/// else goes to [`concat_cv_general`], nothing changed.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
unsafe fn concat_cv_spare(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `frame` is the current frame.
    let code = unsafe { &(*frame).code };
    // SAFETY: as in `rope_tmp`.
    let lits: &[Value] = unsafe { &*std::ptr::from_ref::<[Value]>(&code.literals) };
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    if concat_cv_plain(f, lits, inst).is_some() {
        return true;
    }
    // SAFETY: the caller's contract, passed on.
    unsafe { concat_cv_general(frame, inst) }
}

/// The body of [`concat_cv_spare`]: `None`, nothing changed, when it
/// declines.
#[inline(always)]
#[allow(clippy::inline_always)] // the body of `concat_cv_spare`
fn concat_cv_plain(f: &mut Frame, lits: &[Value], inst: &Inst) -> Option<()> {
    let cv = inst.a as usize;
    let vi = inst.p;
    let kind = val_kind(inst);
    if f.symtab_attached || (kind == KIND_CV && vi == cv) {
        return None;
    }
    if !matches!(f.cvs.get(cv).and_then(CvSlot::get), Some(Value::Str(_))) {
        return None;
    }
    // The right-hand side, checked before anything changes; a consumed
    // temporary is taken out of its slot and released after the append
    // (`FREE_OP2`).
    let taken;
    let rhs: &Value = match kind {
        KIND_CV => f.cvs.get(vi)?.get().filter(|v| str_or_int(v))?,
        KIND_LIT => lits.get(vi).filter(|v| str_or_int(v))?,
        KIND_TMPKEEP => f.tmps.get(vi).filter(|v| str_or_int(v))?,
        KIND_TMP => {
            let slot = f.tmps.get_mut(vi).filter(|v| str_or_int(v))?;
            taken = std::mem::take(slot);
            &taken
        }
        _ => return None,
    };
    let slot = f.cvs.get(cv)?;
    // SAFETY: `rhs` never points into this slot (another variable's, a
    // literal, a kept temporary or the taken one), so the string is the
    // only borrow of the slot.
    let Some(Value::Str(s)) = (unsafe { slot.get_mut_unchecked() }) else {
        return None;
    };
    match rhs {
        Value::Str(t) if ZStr::ptr_eq(s, t) => s.append_self(),
        Value::Str(t) => s.append_str_spare(t),
        Value::Int(n) => s.append_long_spare(*n),
        // Only strings and ints were let through above.
        _ => {}
    }
    Some(())
}

/// Whether `v` is a string or an int (the values `.=` appends without a
/// conversion).
#[inline(always)]
#[allow(clippy::inline_always)]
fn str_or_int(v: &Value) -> bool {
    matches!(v, Value::Str(_) | Value::Int(_))
}

/// `Op::ConcatCv` by `concat_cv_in`: a used result, a symbol table, or
/// what [`concat_cv_spare`] declines.
///
/// # Safety
/// As [`fetch_dim`].
#[inline(never)]
#[allow(clippy::cast_possible_truncation)]
unsafe fn concat_cv_general(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: as in `fetch_dim`.
    let f = unsafe { &mut *frame };
    let val = operand(val_kind(inst), inst.p as u32);
    let dst = (inst.c != u32::MAX).then_some(inst.c >> SLOT_SHIFT);
    concat_cv_in(f, inst.a, val, dst)
}

/// `Op::InArray` ([`TOp::InArrayX`], the op at `inst.d`) settled by one
/// key lookup in its literal haystack (`spec::in_array_in`, the path
/// `hot::step_ext` takes). Declines anything else, having changed
/// nothing: `hot::step_ext`, then the generic op.
///
/// # Safety
/// `frame` is the current frame and `inst` a row [`lower`] built for its
/// tape.
#[inline(never)]
pub(super) unsafe fn in_array(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    let Some(&Op::InArray {
        a,
        set,
        strict,
        dst,
    }) = f.code.ops.get(inst.d as usize)
    else {
        return false;
    };
    in_array_in(f, a, set, strict, dst)
}

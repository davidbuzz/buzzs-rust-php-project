// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Tape instructions for the frame-local scalar ops the numeric families of
//! [`super`] leave out.
//!
//! See the "Families" section of [`super`] for the contract of a handler
//! and of [`lower`].

use super::{
    cv_off, cv_slot, frame_st, is_scalar, lit_off, num, rel, src, tmp_off, tmp_slot, Br, Inst, St,
    TOp, KC, KI, KT, SLOT_SHIFT,
};
use super::{Num, KL};
use crate::exec::spec::plain_truth;
use crate::opcode::{BinKind, Op, OpArray, Operand, UnKind};
use php_runtime::{BinaryOp, Value};

/// The operators of [`num_bin_tmp`], by their code in an instruction's `e`:
/// those of `Op::Bin` the numeric families of [`super`] leave out, for
/// which `exec::num_bin` decides the plain number cases.
const NUM_OPS: [BinKind; 14] = [
    BinKind::Arith(BinaryOp::Div),
    BinKind::Arith(BinaryOp::Mod),
    BinKind::Arith(BinaryOp::ShiftLeft),
    BinKind::Arith(BinaryOp::ShiftRight),
    BinKind::Arith(BinaryOp::BitOr),
    BinKind::Arith(BinaryOp::BitAnd),
    BinKind::Arith(BinaryOp::BitXor),
    BinKind::Less,
    BinKind::LessEqual,
    BinKind::Equal,
    BinKind::NotEqual,
    BinKind::Identical,
    BinKind::NotIdentical,
    BinKind::Spaceship,
];

/// The specialised instruction of the op at `i` when this family has one.
#[allow(clippy::too_many_lines)] // one arm per op, like the other families' `lower`
pub(super) fn lower(code: &OpArray, i: usize) -> Option<Inst> {
    let plain = Inst::plain;
    Some(match *code.ops.get(i)? {
        Op::Nop => plain(TOp::Nop),
        Op::JmpZ(Operand::Cv(cv), target) => Inst {
            a: cv_off(code, cv)?,
            c: rel(code, i, target)?,
            ..plain(TOp::JmpZC)
        },
        Op::JmpNz(Operand::Cv(cv), target) => Inst {
            a: cv_off(code, cv)?,
            c: rel(code, i, target)?,
            ..plain(TOp::JmpNzC)
        },
        Op::Unary { .. } | Op::JmpZEx { .. } | Op::JmpNzEx { .. } => return lower_bool(code, i),
        // A jump on a literal (`while (1)`, `while (true)` whose test stands
        // at the loop's end): taken always, a plain jump, or never, nothing.
        Op::JmpZ(Operand::Lit(l), target) | Op::JmpNz(Operand::Lit(l), target) => {
            let truth = plain_truth(code.literals.get(l as usize)?)?;
            if truth == matches!(code.ops.get(i)?, Op::JmpNz(..)) {
                Inst {
                    c: rel(code, i, target)?,
                    ..plain(TOp::Jmp)
                }
            } else {
                plain(TOp::Nop)
            }
        }
        // `$cv = val` whose value is used (`$j += ($i = $j)`, `$a = $b =
        // 0`): the source's kind in `k` (as `TOp::MoveT`'s), the result
        // temporary in `c`.
        Op::AssignCv {
            cv,
            val,
            dst: Some(dst),
        } => {
            let (k, b) = match val {
                Operand::Cv(s) => (MOVE_CV, cv_off(code, s)?),
                Operand::Tmp(t) => (MOVE_TMP, tmp_off(code, t)?),
                Operand::Lit(l) => {
                    if !is_scalar(code.literals.get(l as usize)?) {
                        return None;
                    }
                    (MOVE_LIT, lit_off(l)?)
                }
                _ => return None,
            };
            Inst {
                k,
                a: cv_off(code, cv)?,
                b,
                c: tmp_off(code, dst)?,
                ..plain(TOp::AssignR)
            }
        }
        // `$cv = a op b` (the `Bin` and the `AssignCv` after it): the
        // variable is in `d`.
        // (and its form fused with the `CmpJmp` after it: see `lower_numeric`)
        Op::BinAssignCv { op, a, b, cv, .. } | Op::BinAssignCvCmpJmp { op, a, b, cv, .. } => {
            let e = NUM_OPS.iter().position(|&o| o == op)?;
            let (ka, fa) = src(code, a)?;
            let (kb, fb) = src(code, b)?;
            Inst {
                k: (ka << 4) | kb,
                e: u8::try_from(e).ok()?,
                a: fa,
                b: fb,
                d: cv_off(code, cv)?,
                ..plain(TOp::NumBinC)
            }
        }
        // `$cv op= val`, result unused.
        Op::AssignOpCv {
            cv,
            op,
            val,
            dst: None,
            ..
        }
        | Op::AssignOpCvCmpJmp { cv, op, val, .. } => {
            let e = NUM_OPS.iter().position(|&o| o == BinKind::Arith(op))?;
            let (kb, fb) = src(code, val)?;
            Inst {
                k: (KC << 4) | kb,
                e: u8::try_from(e).ok()?,
                a: cv_off(code, cv)?,
                b: fb,
                ..plain(TOp::NumAssignOpC)
            }
        }
        // `abs($x)`, which the compiler folded from its `InitFCall`, `Send`
        // and `DoCall` into one op covering the op slot after it. The
        // result goes to its temporary (or nowhere, when it is unused);
        // an `AssignCv` folded behind it (`to`) then runs as its own
        // instruction and moves it, which is what the generic op does
        // when it cannot store into the variable directly. The form with
        // a folded variable and no temporary stays the generic op's.
        Op::Abs { src: from, dst, to } => {
            if dst.is_none() && to.is_some() {
                return None;
            }
            let (ka, fa) = src(code, from)?;
            Inst {
                k: ka,
                a: fa,
                c: match dst {
                    Some(d) => tmp_off(code, d)?,
                    None => NO_TMP,
                },
                ..plain(TOp::AbsN)
            }
        }
        Op::Bin { op, a, b, dst } => {
            let e = NUM_OPS.iter().position(|&o| o == op)?;
            let (ka, fa) = src(code, a)?;
            let (kb, fb) = src(code, b)?;
            Inst {
                k: (ka << 4) | kb,
                e: u8::try_from(e).ok()?,
                a: fa,
                b: fb,
                c: tmp_off(code, dst)?,
                ..plain(TOp::NumBinT)
            }
        }
        // `ZEND_QM_ASSIGN`: a value into a temporary (a ternary's arms, the
        // right side of `?:` and `??`). The source's kind is in `k`.
        Op::Move { src: from, dst } => {
            let (k, a) = match from {
                Operand::Cv(cv) => (MOVE_CV, cv_off(code, cv)?),
                Operand::Tmp(t) => (MOVE_TMP, tmp_off(code, t)?),
                Operand::TmpKeep(t) => (MOVE_KEEP, tmp_off(code, t)?),
                Operand::Lit(l) => {
                    if matches!(code.literals.get(l as usize)?, Value::Reference(_)) {
                        return None;
                    }
                    (MOVE_LIT, lit_off(l)?)
                }
                Operand::Unused => (MOVE_NULL, 0),
            };
            Inst {
                k,
                a,
                c: tmp_off(code, dst)?,
                ..plain(TOp::MoveT)
            }
        }
        // `$cv ?: ...` and `$cv ?? ...`: the result temporary is in `b`.
        Op::JmpSet {
            a: Operand::Cv(cv),
            dst,
            target,
        } => Inst {
            a: cv_off(code, cv)?,
            b: tmp_off(code, dst)?,
            c: rel(code, i, target)?,
            ..plain(TOp::JmpSetC)
        },
        Op::Coalesce {
            a: Operand::Cv(cv),
            dst,
            target,
        } => Inst {
            a: cv_off(code, cv)?,
            b: tmp_off(code, dst)?,
            c: rel(code, i, target)?,
            ..plain(TOp::CoalesceC)
        },
        // `TMP ?? ...` (an element or a property read for `??`, a call's
        // result): the temporary in `a`.
        Op::Coalesce {
            a: Operand::Tmp(t),
            dst,
            target,
        } => Inst {
            a: tmp_off(code, t)?,
            b: tmp_off(code, dst)?,
            c: rel(code, i, target)?,
            ..plain(TOp::CoalesceT)
        },
        _ => return None,
    })
}

/// The boolean ops of [`lower`]: `&&`/`||` as values, `!` and `(bool)`.
fn lower_bool(code: &OpArray, i: usize) -> Option<Inst> {
    let plain = Inst::plain;
    Some(match *code.ops.get(i)? {
        // `&&` / `||` as a value (`ZEND_JMPZ_EX`, `ZEND_JMPNZ_EX`): the
        // result temporary's offset in `d`.
        Op::JmpZEx { a, dst, target } | Op::JmpNzEx { a, dst, target } => {
            let z = matches!(code.ops[i], Op::JmpZEx { .. });
            let (a, op) = match a {
                Operand::Cv(cv) => (
                    cv_off(code, cv)?,
                    if z { TOp::JmpZExC } else { TOp::JmpNzExC },
                ),
                Operand::Tmp(t) => (
                    tmp_off(code, t)?,
                    if z { TOp::JmpZExT } else { TOp::JmpNzExT },
                ),
                _ => return None,
            };
            Inst {
                a,
                c: rel(code, i, target)?,
                d: tmp_off(code, dst)?,
                ..plain(op)
            }
        }
        // `if (!$cv)`: the negation of a variable and the jump on it
        // after it (`ZEND_BOOL_NOT` + `ZEND_JMPZ`/`ZEND_JMPNZ` of its
        // result, which nothing else reads), as one jump on the variable;
        // a jump to the second op still runs that op alone.
        Op::Unary {
            op: UnKind::Not,
            a: Operand::Cv(cv),
            dst,
        } => match *code.ops.get(i + 1)? {
            Op::JmpZ(Operand::Tmp(t), target) if t == dst => Inst {
                a: cv_off(code, cv)?,
                c: rel(code, i, target)?,
                ..plain(TOp::NotJmpZC)
            },
            Op::JmpNz(Operand::Tmp(t), target) if t == dst => Inst {
                a: cv_off(code, cv)?,
                c: rel(code, i, target)?,
                ..plain(TOp::NotJmpNzC)
            },
            _ => Inst {
                a: cv_off(code, cv)?,
                d: tmp_off(code, dst)?,
                e: 1,
                ..plain(TOp::UnaryC)
            },
        },
        // `!a` and `(bool) a` (`ZEND_BOOL_NOT`, `ZEND_BOOL`) into a
        // temporary: `e` is 1 for the negation.
        Op::Unary {
            op: op @ (UnKind::Not | UnKind::Bool),
            a,
            dst,
        } => {
            let (a, top) = match a {
                Operand::Tmp(t) => (tmp_off(code, t)?, TOp::UnaryT),
                Operand::Cv(cv) => (cv_off(code, cv)?, TOp::UnaryC),
                _ => return None,
            };
            Inst {
                a,
                d: tmp_off(code, dst)?,
                e: u8::from(op == UnKind::Not),
                ..plain(top)
            }
        }
        _ => return None,
    })
}

/// The source of a [`TOp::MoveT`] instruction (its `k`): a compiled
/// variable.
pub(super) const MOVE_CV: u8 = 0;
/// A temporary, consumed.
pub(super) const MOVE_TMP: u8 = 1;
/// A temporary, kept.
pub(super) const MOVE_KEEP: u8 = 2;
/// A literal.
pub(super) const MOVE_LIT: u8 = 3;
/// No operand: null.
pub(super) const MOVE_NULL: u8 = 4;

/// A copy of `v` (`ZVAL_COPY`): a scalar's bits, any other value's
/// reference count raised (out of line).
#[inline(always)]
#[allow(clippy::inline_always)]
fn copy_of(v: &Value) -> Value {
    match v {
        Value::Null => Value::Null,
        Value::Bool(b) => Value::Bool(*b),
        Value::Int(i) => Value::Int(*i),
        Value::Float(x) => Value::Float(*x),
        other => other.clone(),
    }
}

/// `Op::Move` (`ZEND_QM_ASSIGN`), as `hot::move_in` runs it: a copy of a
/// literal, a defined compiled variable or a kept temporary, or the value
/// of a consumed temporary (left null), into the result temporary.
/// Declines an undefined variable (the generic op warns), a reference
/// (dereferenced by `hot::step`) and a result temporary holding a value to
/// release.
///
/// # Safety
/// `inst` is an instruction [`lower`] built for `frame`, the current frame.
#[inline(never)]
#[allow(clippy::cast_ptr_alignment)] // a slot offset from a slot vector's base
pub(super) unsafe extern "C" fn move_tmp(frame: *mut crate::vm::Frame, inst: *const Inst) -> bool {
    // SAFETY: the caller's contract: a row of the frame's tape.
    let inst = unsafe { &*inst };
    // SAFETY: the caller's contract.
    let st = unsafe { frame_st(frame) };
    // By raw pointer: the destination may be the source temporary.
    // SAFETY: `c` is a validated temporary offset.
    let dst = unsafe { st.tmps.add(inst.c as usize).cast::<Value>() };
    // SAFETY: `dst` points at a live temporary.
    if !is_scalar(unsafe { &*dst }) {
        return false;
    }
    let v = match inst.k {
        // SAFETY: `a` is a validated variable offset.
        MOVE_CV => match unsafe { &*cv_slot(st, inst.a) } {
            None | Some(Value::Reference(_)) => return false,
            Some(v) => copy_of(v),
        },
        MOVE_TMP | MOVE_KEEP => {
            // SAFETY: `a` is a validated temporary offset.
            let from = unsafe { st.tmps.add(inst.a as usize).cast::<Value>() };
            // SAFETY: `from` points at a live temporary.
            if matches!(unsafe { &*from }, Value::Reference(_)) {
                return false;
            }
            if inst.k == MOVE_TMP {
                // SAFETY: as above; the temporary is left null (consumed).
                unsafe { std::ptr::replace(from, Value::Null) }
            } else {
                // SAFETY: as above.
                copy_of(unsafe { &*from })
            }
        }
        // SAFETY: `a` is a validated literal offset (not a reference:
        // `lower`).
        MOVE_LIT => copy_of(unsafe { &*st.lits.add(inst.a as usize).cast::<Value>() }),
        _ => Value::Null,
    };
    // SAFETY: the destination holds a scalar (tested above; a consumed
    // source that is the destination itself was replaced by null): nothing
    // to drop.
    unsafe { std::ptr::write(dst, v) };
    true
}

/// `Op::AssignCv` with a used result (`ZEND_ASSIGN` whose result is not
/// `IS_UNUSED`), as `hot::assign_cv_in` runs it, when nothing is released:
/// the value (a compiled variable's, a consumed temporary's or a literal's)
/// is a scalar, and so are what the variable and the result temporary
/// hold (the variable may be undefined). The value's 16 bytes go to both.
/// Declines anything else, nothing changed: `hot::step` then runs the op.
///
/// # Safety
/// As [`move_tmp`].
#[inline(never)]
#[allow(clippy::cast_ptr_alignment)] // a slot offset from a slot vector's base
pub(super) unsafe fn assign_res(frame: *mut crate::vm::Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract.
    let st = unsafe { frame_st(frame) };
    // By raw pointers: the source may be the variable itself (`$a = $a`)
    // or the result temporary.
    // SAFETY: `a` is a validated variable offset, `c` a temporary's.
    let (cv, res) = unsafe {
        (
            st.cvs.add(inst.a as usize).cast::<Option<Value>>(),
            st.tmps.add(inst.c as usize).cast::<Value>(),
        )
    };
    // SAFETY: both point at live slots; these shared references end with
    // the checks, before anything writes through the pointers.
    let (cur, old) = unsafe { (&*cv, &*res) };
    if !matches!(
        cur,
        None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_))
    ) || !is_scalar(old)
    {
        return false;
    }
    let from: *mut Value = match inst.k {
        // SAFETY: `b` is a validated variable offset; a defined variable's
        // slot is its value.
        MOVE_CV => match unsafe { &mut *cv_slot(st, inst.b) } {
            Some(v) => v,
            None => return false,
        },
        // SAFETY: `b` is a validated temporary offset.
        MOVE_TMP => unsafe { st.tmps.add(inst.b as usize).cast::<Value>() },
        // SAFETY: `b` is a validated literal offset (a scalar: `lower`);
        // it is only read.
        _ => unsafe { st.lits.add(inst.b as usize).cast::<Value>().cast_mut() },
    };
    // SAFETY: `from` points at a live value.
    if !is_scalar(unsafe { &*from }) {
        return false;
    }
    // SAFETY: a scalar's bits are its copy, and the three slots hold
    // scalars (or the variable nothing): nothing to drop. A consumed
    // temporary is left null before the result, which may be the same
    // slot, is written.
    unsafe {
        let (v, copy) = (std::ptr::read(from), std::ptr::read(from));
        if inst.k == MOVE_TMP {
            std::ptr::write(from, Value::Null);
        }
        std::ptr::write(res, copy);
        std::ptr::write(cv, Some(v));
    }
    true
}

/// `Op::JmpSet` (`ZEND_JMP_SET_SPEC_CV`, `$cv ?: ...`), as
/// `hot::jmp_set_in` runs it: a truthy value is copied into the result
/// temporary (`b`) and the jump taken; a falsy one falls through. Declines
/// an undefined variable, a value whose truth needs a conversion (a
/// reference, an object, a NaN) and a result temporary holding a value to
/// release.
///
/// # Safety
/// As [`move_tmp`].
#[inline(never)]
pub(super) unsafe fn jmp_set_cv(frame: *mut crate::vm::Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract.
    let st = unsafe { frame_st(frame) };
    // SAFETY: `a` is a validated variable offset.
    let Some(v) = (unsafe { &*cv_slot(st, inst.a) }) else {
        return Br::Decline;
    };
    match plain_truth(v) {
        None => Br::Decline,
        Some(false) => Br::Fall,
        Some(true) => {
            // SAFETY: `b` is a validated temporary offset.
            let dst = unsafe { tmp_slot(st, inst.b) };
            if !is_scalar(dst) {
                return Br::Decline;
            }
            // A scalar has nothing to drop.
            std::mem::forget(std::mem::replace(dst, copy_of(v)));
            Br::Jump
        }
    }
}

/// `Op::Coalesce` (`ZEND_COALESCE_SPEC_CV`, `$cv ?? ...`), as
/// `hot::coalesce_in` runs it: a value that is not null is copied into the
/// result temporary (`b`) and the jump taken; an undefined variable or a
/// null falls through. Declines a reference (dereferenced by `hot::step`)
/// and a result temporary holding a value to release.
///
/// # Safety
/// As [`move_tmp`].
#[inline(never)]
pub(super) unsafe fn coalesce_cv(frame: *mut crate::vm::Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract.
    let st = unsafe { frame_st(frame) };
    // SAFETY: `a` is a validated variable offset.
    match unsafe { &*cv_slot(st, inst.a) } {
        None | Some(Value::Null) => Br::Fall,
        Some(Value::Reference(_)) => Br::Decline,
        Some(v) => {
            // SAFETY: `b` is a validated temporary offset.
            let dst = unsafe { tmp_slot(st, inst.b) };
            if !is_scalar(dst) {
                return Br::Decline;
            }
            // A scalar has nothing to drop.
            std::mem::forget(std::mem::replace(dst, copy_of(v)));
            Br::Jump
        }
    }
}

/// `Op::Coalesce` of a temporary (`ZEND_COALESCE_SPEC_TMPVAR`, `TMP ??
/// ...`), as the generic op runs it: a value that is not null moves into
/// the result temporary (`b`) and the jump is taken; a null (the consumed
/// temporary stays null) falls through. Declines a reference (the generic
/// read looks through it) and a result temporary holding a value to
/// release.
///
/// # Safety
/// As [`move_tmp`].
#[inline(never)]
#[allow(clippy::cast_ptr_alignment)] // a slot offset from a slot vector's base
pub(super) unsafe fn coalesce_tmp(frame: *mut crate::vm::Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract.
    let st = unsafe { frame_st(frame) };
    // By raw pointers: the result may be the tested temporary itself.
    // SAFETY: `a` and `b` are validated temporary offsets.
    let (from, dst) = unsafe {
        (
            st.tmps.add(inst.a as usize).cast::<Value>(),
            st.tmps.add(inst.b as usize).cast::<Value>(),
        )
    };
    // SAFETY: `from` points at a live temporary.
    match unsafe { &*from } {
        Value::Null => Br::Fall,
        Value::Reference(_) => Br::Decline,
        _ => {
            if from != dst {
                // SAFETY: `dst` points at a live temporary, another slot.
                if !is_scalar(unsafe { &*dst }) {
                    return Br::Decline;
                }
                // SAFETY: the value moves from one live slot to another,
                // which holds a scalar (nothing to drop); the source is
                // left null, consumed.
                unsafe { std::ptr::write(dst, std::ptr::replace(from, Value::Null)) };
            }
            Br::Jump
        }
    }
}

/// `$cv = val` (`TOp::AssignT`, `TOp::AssignC`) when the value or the
/// variable's old value is not a scalar. A string or a scalar over a
/// string, a scalar or nothing is stored here (the value moved out of its
/// temporary or copied from its variable, an old string released), and so
/// is a temporary's value over an object (`$o = new C` again: the object
/// goes to `exec::release_object`); any other pair is
/// `hot::assign_cv_in`'s, which copies or moves any value and releases
/// the old one, reached without the way through `hot::step`.
/// `from_tmp` tells a temporary (`b`, consumed) from a compiled variable.
/// The caller polls: a released value may queue a destructor.
///
/// # Safety
/// As [`move_tmp`].
#[inline(never)]
#[allow(clippy::cast_ptr_alignment)] // a slot offset from a slot vector's base
pub(super) unsafe extern "C" fn assign_cv_any(
    frame: *mut crate::vm::Frame,
    inst: *const Inst,
    from_tmp: bool,
) -> bool {
    // SAFETY: the caller's contract: a row of the frame's tape.
    let inst = unsafe { &*inst };
    // SAFETY: the caller's contract.
    let st = unsafe { frame_st(frame) };
    // SAFETY: `a` is a validated variable offset.
    let dst = unsafe { st.cvs.add(inst.a as usize).cast::<Option<Value>>() };
    // SAFETY: `dst` points at a live slot.
    if unsafe { &*dst }.as_ref().is_none_or(Value::is_plain) {
        let v = if from_tmp {
            // SAFETY: `b` is a validated temporary offset.
            let from = unsafe { &mut *st.tmps.add(inst.b as usize).cast::<Value>() };
            // Moved out: the temporary is consumed.
            from.is_plain().then(|| std::mem::take(from))
        } else {
            // SAFETY: `b` is a validated variable offset.
            match unsafe { &*st.cvs.add(inst.b as usize).cast::<Option<Value>>() } {
                Some(Value::Str(s)) => Some(Value::Str(s.clone())),
                Some(v) if is_scalar(v) => Some(copy_of(v)),
                _ => None,
            }
        };
        if let Some(v) = v {
            // SAFETY: `dst` is valid for writes and holds nothing, a scalar
            // or a string (seen above; the source's copy was made first).
            let old = unsafe { std::ptr::replace(dst, Some(v)) };
            match old {
                Some(Value::Str(s)) => drop(s),
                other => std::mem::forget(other),
            }
            return true;
        }
    }
    // A temporary's value over an object: `hot::assign_cv_in`'s case of a
    // variable whose old value may be a root, without its general steps
    // (no symbol table mirrors the variables of a frame the tape runs).
    // SAFETY: `dst` points at a live slot.
    if from_tmp && matches!(unsafe { &*dst }, Some(Value::Object(_))) {
        // SAFETY: `b` is a validated temporary offset.
        let from = unsafe { &mut *st.tmps.add(inst.b as usize).cast::<Value>() };
        if !matches!(from, Value::Reference(_)) {
            // Moved out: the temporary is consumed.
            let v = std::mem::take(from);
            // SAFETY: `dst` is valid for writes; the object it held is
            // released below, the slot already holding the new value.
            if let Some(Value::Object(old)) = unsafe { std::ptr::replace(dst, Some(v)) } {
                crate::exec::release_object(old);
            }
            return true;
        }
    }
    let from = inst.b >> SLOT_SHIFT;
    let val = if from_tmp {
        Operand::Tmp(from)
    } else {
        Operand::Cv(from)
    };
    // SAFETY: the caller's contract: `frame` is the current frame.
    crate::exec::hot::assign_cv_in(unsafe { &mut *frame }, inst.a >> SLOT_SHIFT, val, None)
}

/// The number operand `x` of kind `kind` (the kinds of [`super::src`]);
/// `None` when it holds anything else.
///
/// # Safety
/// As [`num`]: `x` is a validated offset of the kind `kind` names.
#[inline(always)]
#[allow(clippy::inline_always)]
unsafe fn num_of_kind(st: St, kind: u8, x: u32) -> Option<Num> {
    // SAFETY: the caller's contract.
    unsafe {
        match kind {
            KC => num::<KC>(st, x),
            KT => num::<KT>(st, x),
            KI => num::<KI>(st, x),
            _ => num::<KL>(st, x),
        }
    }
}

/// `Op::Bin` into a temporary of two plain numbers for the operators the
/// numeric families leave out ([`NUM_OPS`]: `/`, `%`, `<<`, `>>`, `|`,
/// `&`, `^` and the comparisons; `ZEND_DIV` ... `ZEND_SPACESHIP` on longs
/// and doubles), decided by `exec::num_bin` as `hot::step` decides them:
/// the operator in `e`, the operand kinds in `k` (`a`'s high nibble, `b`'s
/// low one). Declines anything else (other types, a zero divisor, a
/// negative or too wide shift: the generic op converts, warns or throws)
/// and a destination holding a value to release.
///
/// Called with the frame alone (`fam_arm!`'s thin modes), as the other
/// families' handlers are: it reads the slot bases from the frame
/// ([`super::frame_st`]).
///
/// # Safety
/// `inst` is an instruction [`lower`] built for `frame`, the current frame.
#[inline(never)]
#[allow(clippy::cast_ptr_alignment)] // a slot offset from a slot vector's base
pub(super) unsafe extern "C" fn num_bin_tmp(
    frame: *mut crate::vm::Frame,
    inst: *const Inst,
) -> bool {
    // SAFETY: the caller's contract: a row of the frame's tape.
    let inst = unsafe { &*inst };
    // SAFETY: the caller's contract.
    let st = unsafe { frame_st(frame) };
    // By raw pointer: the destination may be one of the operands.
    // SAFETY: `c` is a validated temporary offset.
    let dst = unsafe { st.tmps.add(inst.c as usize).cast::<php_runtime::Value>() };
    // SAFETY: `dst` points at a live temporary.
    if !is_scalar(unsafe { &*dst }) {
        return false;
    }
    // SAFETY: the caller's contract.
    let Some(result) = (unsafe { num_result(st, inst) }) else {
        return false;
    };
    // SAFETY: a scalar has nothing to drop; `dst` is valid for writes.
    unsafe { std::ptr::write(dst, result) };
    true
}

/// The result of [`NUM_OPS`]`[e]` on the operands of `inst` (kinds in
/// `k`), as `exec::num_bin` decides it; `None` for anything else.
///
/// # Safety
/// As [`num_bin_tmp`].
#[inline(always)]
#[allow(clippy::inline_always)]
unsafe fn num_result(st: St, inst: &Inst) -> Option<php_runtime::Value> {
    // SAFETY: the caller's contract.
    let x = unsafe { num_of_kind(st, inst.k >> 4, inst.a) }?;
    // SAFETY: the caller's contract.
    let y = unsafe { num_of_kind(st, inst.k & 0xf, inst.b) }?;
    if let (Num::Int(x), Num::Int(y)) = (x, y) {
        return int_result(inst.e, x, y);
    }
    let op = *NUM_OPS.get(usize::from(inst.e))?;
    crate::exec::num_bin(op, x, y)
}

/// [`NUM_OPS`]`[e]` of two ints, as `exec::num_bin` computes it, by the
/// operator's index alone (one dispatch, where `num_bin` tests for two
/// floats first and then matches the operator's nested enum). `None` for
/// what `num_bin` declines: a zero divisor, `PHP_INT_MIN / -1`, a negative
/// or too wide shift.
#[inline(always)]
#[allow(clippy::inline_always)] // the int case of `num_result`
fn int_result(e: u8, x: i64, y: i64) -> Option<php_runtime::Value> {
    use php_runtime::Value;
    Some(match e {
        // `div_function`: an exact quotient stays an int, any other is
        // `(double) x / y`.
        0 => {
            if x.checked_rem(y)? == 0 {
                Value::Int(x / y)
            } else {
                Value::Float(Num::Int(x).as_f64() / Num::Int(y).as_f64())
            }
        }
        1 => Value::Int(x.checked_rem(y)?),
        2 => Value::Int(x.checked_shl(u32::try_from(y).ok()?)?),
        3 => Value::Int(x.checked_shr(u32::try_from(y).ok()?)?),
        4 => Value::Int(x | y),
        5 => Value::Int(x & y),
        6 => Value::Int(x ^ y),
        7 => Value::Bool(x < y),
        8 => Value::Bool(x <= y),
        9 | 11 => Value::Bool(x == y),
        10 | 12 => Value::Bool(x != y),
        13 => Value::Int(match x.cmp(&y) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }),
        _ => return None,
    })
}

/// Whether a compiled variable holds nothing to release: undefined or a
/// scalar (a store over it is a plain write, as `hot::is_plain_slot`).
#[inline(always)]
#[allow(clippy::inline_always)]
fn plain_cv(v: Option<&php_runtime::Value>) -> bool {
    v.is_none_or(is_scalar)
}

/// `Op::BinAssignCv` (`$cv = a op b`) for the operators of [`NUM_OPS`] on
/// two plain numbers, when the variable (`d`) holds nothing to release:
/// the result goes straight into it, and the `AssignCv` after the op is
/// skipped (the instruction covers both ops), as `hot::bin_assign_cv`
/// stores it. Declines a variable holding anything else (the generic op
/// stores through the `AssignCv`) and as [`num_bin_tmp`] does.
///
/// # Safety
/// As [`num_bin_tmp`].
#[inline(never)]
pub(super) unsafe fn num_bin_cv(frame: *mut crate::vm::Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract.
    let st = unsafe { frame_st(frame) };
    // SAFETY: `d` is a validated variable offset.
    if !plain_cv(unsafe { &*cv_slot(st, inst.d) }.as_ref()) {
        return false;
    }
    // SAFETY: the caller's contract.
    let Some(r) = (unsafe { num_result(st, inst) }) else {
        return false;
    };
    // SAFETY: as above; undefined or a scalar has nothing to drop.
    std::mem::forget(unsafe { cv_slot(st, inst.d) }.replace(r));
    true
}

/// `Op::AssignOpCv` (`$cv op= val`, result unused) for the operators of
/// [`NUM_OPS`] on a variable and a value holding plain numbers.
/// Declines as [`num_bin_tmp`] does (an undefined variable or a reference
/// included: the generic op warns or writes through it).
///
/// # Safety
/// As [`num_bin_tmp`].
#[inline(never)]
pub(super) unsafe fn num_assign_op_cv(frame: *mut crate::vm::Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract.
    let st = unsafe { frame_st(frame) };
    // SAFETY: the caller's contract (the variable is `a`, of kind `KC`).
    let Some(r) = (unsafe { num_result(st, inst) }) else {
        return false;
    };
    // SAFETY: `a` is a validated variable offset; it held a number, which
    // owns nothing.
    std::mem::forget(unsafe { cv_slot(st, inst.a) }.replace(r));
    true
}

/// "No result temporary" in `c` of an [`TOp::AbsN`] instruction.
pub(super) const NO_TMP: u32 = u32::MAX;

/// `Op::Abs` of a plain number (`Vm::abs_fast`): the absolute value of an
/// int (a float for `PHP_INT_MIN`, whose magnitude no int holds) or of a
/// float, into the result temporary when there is one. Declines anything
/// else (the generic op makes the call) and a temporary holding a value
/// to release.
///
/// # Safety
/// As [`num_bin_tmp`].
#[inline(never)]
#[allow(clippy::cast_ptr_alignment)] // a slot offset from a slot vector's base
pub(super) unsafe fn abs_num(frame: *mut crate::vm::Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract.
    let st = unsafe { frame_st(frame) };
    // SAFETY: the caller's contract (`a` is a validated operand of kind `k`).
    let Some(x) = (unsafe { num_of_kind(st, inst.k, inst.a) }) else {
        return false;
    };
    if inst.c == NO_TMP {
        return true;
    }
    let result = match x {
        Num::Int(i64::MIN) => php_runtime::Value::Float(-Num::Int(i64::MIN).as_f64()),
        Num::Int(i) => php_runtime::Value::Int(i.abs()),
        Num::Float(f) => php_runtime::Value::Float(f.abs()),
    };
    // SAFETY: `c` is a validated temporary offset.
    let dst = unsafe { st.tmps.add(inst.c as usize).cast::<php_runtime::Value>() };
    // SAFETY: `dst` points at a live temporary.
    if !is_scalar(unsafe { &*dst }) {
        return false;
    }
    // SAFETY: a scalar has nothing to drop; `dst` is valid for writes.
    unsafe { std::ptr::write(dst, result) };
    true
}

/// `Op::Nop`.
///
/// # Safety
/// None needed; `unsafe` for the handler signature.
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn nop(_st: St, _inst: &Inst) -> bool {
    true
}

/// `Op::JmpZ` of a compiled variable whose truth needs no conversion
/// (`ZEND_JMPZ_SPEC_CV`; `hot::take_bool`): jumps when it is false.
/// Declines an undefined variable, a reference, an object and a NaN.
///
/// # Safety
/// `inst` is an instruction [`lower`] built for the frame of `st`.
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn jmpz_cv(st: St, inst: &Inst) -> Br {
    // SAFETY: the caller's contract: `a` is a validated variable offset.
    match unsafe { cv_slot(st, inst.a) }
        .as_ref()
        .and_then(plain_truth)
    {
        Some(true) => Br::Fall,
        Some(false) => Br::Jump,
        None => Br::Decline,
    }
}

/// `Op::JmpNz` of a compiled variable (`ZEND_JMPNZ_SPEC_CV`): jumps when
/// it is true; otherwise as [`jmpz_cv`].
///
/// # Safety
/// As [`jmpz_cv`].
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn jmpnz_cv(st: St, inst: &Inst) -> Br {
    // SAFETY: as in `jmpz_cv`.
    match unsafe { cv_slot(st, inst.a) }
        .as_ref()
        .and_then(plain_truth)
    {
        Some(true) => Br::Jump,
        Some(false) => Br::Fall,
        None => Br::Decline,
    }
}

/// `Op::Unary { op: Not, a: Cv }` and the `Op::JmpZ` of its result
/// (`if (!$cv)`): jumps when the variable is true (its negation false);
/// otherwise as [`jmpz_cv`].
///
/// # Safety
/// As [`jmpz_cv`].
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn not_jmpz_cv(st: St, inst: &Inst) -> Br {
    // SAFETY: as in `jmpz_cv`.
    unsafe { jmpnz_cv(st, inst) }
}

/// `Op::Unary { op: Not, a: Cv }` and the `Op::JmpNz` of its result:
/// jumps when the variable is false; otherwise as [`jmpz_cv`].
///
/// # Safety
/// As [`jmpz_cv`].
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn not_jmpnz_cv(st: St, inst: &Inst) -> Br {
    // SAFETY: as in `jmpz_cv`.
    unsafe { jmpz_cv(st, inst) }
}

/// `Op::Unary { op: Not | Bool, a: Tmp }` (`ZEND_BOOL_NOT`, `ZEND_BOOL`
/// of a temporary): a temporary holding null, a bool, an int or a non-NaN
/// float (a comparison's or a call's result) is consumed and its truth,
/// negated when `e` is 1, goes into the temporary at `d`, which may be the
/// operand. Declines any other value and a destination holding a value to
/// release, before changing anything (the generic step converts, warns or
/// releases).
///
/// # Safety
/// As [`jmpz_cv`]; `a` and `d` are validated temporary offsets.
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn unary_bool_tmp(st: St, inst: &Inst) -> bool {
    // SAFETY: the caller's contract.
    let src = unsafe { tmp_slot(st, inst.a) };
    let b = match *src {
        Value::Null => false,
        Value::Bool(b) => b,
        Value::Int(i) => i != 0,
        Value::Float(x) if !x.is_nan() => x != 0.0,
        _ => return false,
    };
    // Tested before the operand is consumed: a decline changes nothing.
    // SAFETY: the caller's contract.
    if !is_scalar(unsafe { tmp_slot(st, inst.d) }) {
        return false;
    }
    // A scalar has nothing to drop: consumed, as the generic read does
    // (the destination may be this slot: written below).
    std::mem::forget(std::mem::take(src));
    // SAFETY: the caller's contract.
    unsafe { store_bool(st, inst, b) }
}

/// `Op::Unary { op: Not | Bool, a: Cv }` of a variable whose truth
/// [`plain_truth`] decides (as [`unary_bool_tmp`], the variable read in
/// place).
///
/// # Safety
/// As [`jmpz_cv`]; `d` is a validated temporary offset.
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn unary_bool_cv(st: St, inst: &Inst) -> bool {
    // SAFETY: the caller's contract.
    match unsafe { cv_slot(st, inst.a) }
        .as_ref()
        .and_then(plain_truth)
    {
        // SAFETY: the caller's contract.
        Some(b) => unsafe { store_bool(st, inst, b) },
        None => false,
    }
}

/// The result of [`unary_bool_tmp`] and [`unary_bool_cv`]: `b`, negated
/// when `e` is 1, into the temporary at `d` when it holds a scalar.
///
/// # Safety
/// `d` is a validated temporary offset of the current frame.
#[inline(always)]
#[allow(clippy::inline_always)]
unsafe fn store_bool(st: St, inst: &Inst, b: bool) -> bool {
    // SAFETY: the caller's contract.
    let dst = unsafe { tmp_slot(st, inst.d) };
    if !is_scalar(dst) {
        return false;
    }
    // A scalar has nothing to drop.
    std::mem::forget(std::mem::replace(dst, Value::Bool(b != (inst.e == 1))));
    true
}

/// `Op::JmpZEx` / `Op::JmpNzEx` (`ZEND_JMPZ_EX`, `ZEND_JMPNZ_EX`: `&&`
/// and `||` as a value) once the operand's truth `b` is known: when it is
/// `jump_on`, `b` goes into the result temporary at `inst.d` and the
/// branch is taken; otherwise it falls through. A result temporary holding
/// a value to release declines (before anything changed).
///
/// # Safety
/// As [`jmpz_cv`]; `d` is a validated temporary offset.
#[inline(always)]
#[allow(clippy::inline_always)]
unsafe fn jmp_ex(st: St, inst: &Inst, b: bool, jump_on: bool) -> Br {
    if b != jump_on {
        return Br::Fall;
    }
    // SAFETY: the caller's contract.
    let dst = unsafe { tmp_slot(st, inst.d) };
    if !is_scalar(dst) {
        return Br::Decline;
    }
    // A scalar has nothing to drop.
    std::mem::forget(std::mem::replace(dst, Value::Bool(b)));
    Br::Jump
}

/// `Op::JmpZEx` of a compiled variable whose truth [`plain_truth`]
/// decides; declines anything else (the generic op converts or warns).
///
/// # Safety
/// As [`jmp_ex`].
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn jmpz_ex_cv(st: St, inst: &Inst) -> Br {
    // SAFETY: the caller's contract.
    match unsafe { cv_slot(st, inst.a) }
        .as_ref()
        .and_then(plain_truth)
    {
        // SAFETY: the caller's contract.
        Some(b) => unsafe { jmp_ex(st, inst, b, false) },
        None => Br::Decline,
    }
}

/// `Op::JmpNzEx` of a compiled variable (as [`jmpz_ex_cv`]).
///
/// # Safety
/// As [`jmp_ex`].
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn jmpnz_ex_cv(st: St, inst: &Inst) -> Br {
    // SAFETY: the caller's contract.
    match unsafe { cv_slot(st, inst.a) }
        .as_ref()
        .and_then(plain_truth)
    {
        // SAFETY: the caller's contract.
        Some(b) => unsafe { jmp_ex(st, inst, b, true) },
        None => Br::Decline,
    }
}

/// `Op::JmpZEx` of a temporary holding a bool (a comparison's result),
/// which it consumes; declines any other value. The result temporary may
/// be the operand itself: the operand is emptied first.
///
/// # Safety
/// As [`jmp_ex`]; `a` is a validated temporary offset.
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn jmpz_ex_tmp(st: St, inst: &Inst) -> Br {
    // SAFETY: the caller's contract.
    unsafe { jmp_ex_tmp(st, inst, false) }
}

/// `Op::JmpNzEx` of a temporary (as [`jmpz_ex_tmp`]).
///
/// # Safety
/// As [`jmpz_ex_tmp`].
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn jmpnz_ex_tmp(st: St, inst: &Inst) -> Br {
    // SAFETY: the caller's contract.
    unsafe { jmp_ex_tmp(st, inst, true) }
}

/// [`jmpz_ex_tmp`] and [`jmpnz_ex_tmp`].
///
/// # Safety
/// As [`jmpz_ex_tmp`].
#[inline(always)]
#[allow(clippy::inline_always)]
unsafe fn jmp_ex_tmp(st: St, inst: &Inst, jump_on: bool) -> Br {
    // SAFETY: the caller's contract.
    let src = unsafe { tmp_slot(st, inst.a) };
    let Value::Bool(b) = *src else {
        return Br::Decline;
    };
    if b == jump_on {
        // SAFETY: the caller's contract.
        if !is_scalar(unsafe { tmp_slot(st, inst.d) }) {
            return Br::Decline;
        }
    }
    // Consumed, as the generic read does; a bool has nothing to drop.
    std::mem::forget(std::mem::take(src));
    // SAFETY: the caller's contract.
    unsafe { jmp_ex(st, inst, b, jump_on) }
}

#[cfg(test)]
mod tests {
    use super::{int_result, Num, NUM_OPS};

    /// `int_result` is `exec::num_bin` of two ints for every operator of
    /// [`NUM_OPS`], declines included (by the operator's index).
    #[test]
    fn int_result_is_num_bin_of_two_ints() {
        let vals = [0, 1, -1, 2, 3, -4, -7, 12, 63, 64, 65, i64::MAX, i64::MIN];
        for (e, &op) in NUM_OPS.iter().enumerate() {
            let e8 = u8::try_from(e).expect("operator index");
            for &x in &vals {
                for &y in &vals {
                    let ours = int_result(e8, x, y);
                    let theirs = crate::exec::num_bin(op, Num::Int(x), Num::Int(y));
                    assert_eq!(format!("{ours:?}"), format!("{theirs:?}"), "{op:?} {x} {y}");
                }
            }
        }
        assert!(int_result(14, 1, 1).is_none());
    }
}

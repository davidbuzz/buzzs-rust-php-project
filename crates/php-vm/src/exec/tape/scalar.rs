// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Tape instructions for the frame-local scalar ops the numeric families of
//! [`super`] leave out.
//!
//! See the "Families" section of [`super`] for the contract of a handler
//! and of [`lower`].

use super::{
    cv_off, cv_slot, frame_st, is_scalar, num, rel, src, tmp_off, Br, Inst, St, TOp, KC, KI, KT,
};
use super::{Num, KL};
use crate::exec::spec::plain_truth;
use crate::opcode::{BinKind, Op, OpArray, Operand};
use php_runtime::BinaryOp;

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
        _ => return None,
    })
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
pub(super) unsafe fn num_bin_tmp(frame: *mut crate::vm::Frame, inst: &Inst) -> bool {
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

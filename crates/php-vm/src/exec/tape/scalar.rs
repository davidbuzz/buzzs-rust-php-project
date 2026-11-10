//! Tape instructions for the frame-local scalar ops the numeric families of
//! [`super`] leave out.
//!
//! See the "Families" section of [`super`] for the contract of a handler
//! and of [`lower`].

use super::{cv_off, cv_slot, rel, Br, Inst, St, TOp};
use crate::exec::spec::plain_truth;
use crate::opcode::{Op, OpArray, Operand};

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
        _ => return None,
    })
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

//! Tape instructions for strings and the builtin ops.
//!
//! `strlen()`, `count()`/`sizeof()`, the type tests (`is_int()` & co.) and
//! `isset()`/`empty()` of a compiled variable, which `zend_compile.c`
//! compiles to `ZEND_STRLEN`, `ZEND_COUNT`, `ZEND_TYPE_CHECK` and
//! `ZEND_ISSET_ISEMPTY_CV`. The last two are smart branches in php-src: a
//! `JMPZ`/`JMPNZ` of the result right after them is taken by the test
//! itself (`ZEND_VM_SMART_BRANCH`), which the fused instructions here do.
//!
//! See the "Families" section of [`super`] for the contract of a handler
//! and of [`lower`].

use super::{cv_off, cv_slot, is_scalar, rel, tmp_off, tmp_slot, Br, Inst, St, TOp};
use crate::exec::spec::plain_truth;
use crate::opcode::{type_mask, Op, OpArray, Operand, PlaceBase};
use php_runtime::Value;

/// The result field of an instruction whose result is unused.
const NO_DST: u32 = u32::MAX;

/// The specialised instruction of the op at `i` when this family has one.
pub(super) fn lower(code: &OpArray, i: usize) -> Option<Inst> {
    let plain = Inst::plain;
    let dst = |d: Option<u32>| match d {
        Some(t) => tmp_off(code, t),
        None => Some(NO_DST),
    };
    Some(match *code.ops.get(i)? {
        Op::Strlen {
            a: Operand::Cv(cv),
            dst: d,
        } => Inst {
            a: cv_off(code, cv)?,
            c: dst(d)?,
            ..plain(TOp::StrlenC)
        },
        Op::Count {
            a: Operand::Cv(cv),
            dst: d,
            ..
        } => Inst {
            a: cv_off(code, cv)?,
            c: dst(d)?,
            ..plain(TOp::CountC)
        },
        Op::TypeCheck {
            a: Operand::Cv(cv),
            mask,
            dst: d,
        } => match smart_branch(code, i, d) {
            Some((jump_if, target)) => Inst {
                k: mask,
                e: u8::from(jump_if),
                a: cv_off(code, cv)?,
                c: rel(code, i, target)?,
                ..plain(TOp::TypeCheckJmpC)
            },
            None => Inst {
                k: mask,
                a: cv_off(code, cv)?,
                c: dst(d)?,
                ..plain(TOp::TypeCheckC)
            },
        },
        Op::Isset { place, dst: t } | Op::Empty { place, dst: t } => {
            let empty = u32::from(matches!(code.ops[i], Op::Empty { .. }));
            let p = code.places.get(place as usize)?;
            let &PlaceBase::Cv(cv) = &p.base else {
                return None;
            };
            if !p.segs.is_empty() {
                return None;
            }
            match smart_branch(code, i, Some(t)) {
                Some((jump_if, target)) => Inst {
                    e: u8::from(jump_if),
                    a: cv_off(code, cv)?,
                    b: empty,
                    c: rel(code, i, target)?,
                    ..plain(TOp::IssetJmpC)
                },
                None => Inst {
                    a: cv_off(code, cv)?,
                    b: empty,
                    c: tmp_off(code, t)?,
                    ..plain(TOp::IssetC)
                },
            }
        }
        _ => return None,
    })
}

/// The jump that consumes the result `dst` of the op at `i` right after
/// it, as `(jumps when the result is true, target)`: `JMPZ`/`JMPNZ` of
/// that temporary, which a smart branch takes itself.
fn smart_branch(code: &OpArray, i: usize, dst: Option<u32>) -> Option<(bool, u32)> {
    let t = dst?;
    match *code.ops.get(i + 1)? {
        Op::JmpZ(Operand::Tmp(u), target) if u == t => Some((false, target)),
        Op::JmpNz(Operand::Tmp(u), target) if u == t => Some((true, target)),
        _ => None,
    }
}

/// Stores integer `n` into the result temporary at offset `c`, unless the
/// result is unused ([`NO_DST`]). Declines (`false`, nothing stored) when
/// the slot holds something to release, which the generic op releases.
///
/// # Safety
/// `c` is [`NO_DST`] or a temporary offset [`lower`] validated for the
/// frame of `st`.
#[inline(always)]
#[allow(clippy::inline_always)]
unsafe fn put_result(st: St, c: u32, v: Value) -> bool {
    if c == NO_DST {
        return true;
    }
    // SAFETY: the caller's contract.
    let slot = unsafe { tmp_slot(st, c) };
    if !is_scalar(slot) {
        return false;
    }
    // A scalar has nothing to drop.
    std::mem::forget(std::mem::replace(slot, v));
    true
}

/// `Op::Strlen` of a compiled variable holding a string
/// (`ZEND_STRLEN_SPEC_CV`). Declines anything else (undefined, a
/// reference, another type: the generic op coerces, warns or throws).
///
/// # Safety
/// `inst` is an instruction [`lower`] built for the frame of `st`.
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn strlen_cv(st: St, inst: &Inst) -> bool {
    // SAFETY: the caller's contract: `a` is a validated variable offset.
    let Some(Value::Str(s)) = (unsafe { &*cv_slot(st, inst.a) }) else {
        return false;
    };
    let n = i64::try_from(s.len()).unwrap_or(i64::MAX);
    // SAFETY: `c` is the validated result field.
    unsafe { put_result(st, inst.c, Value::Int(n)) }
}

/// `Op::Count` of a compiled variable holding an array
/// (`ZEND_COUNT_SPEC_CV`). Declines anything else (`Countable` objects,
/// the `TypeError`, references: the generic op).
///
/// # Safety
/// As [`strlen_cv`].
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn count_cv(st: St, inst: &Inst) -> bool {
    // SAFETY: as in `strlen_cv`.
    let Some(Value::Array(a)) = (unsafe { &*cv_slot(st, inst.a) }) else {
        return false;
    };
    let n = i64::try_from(a.len()).unwrap_or(i64::MAX);
    // SAFETY: as in `strlen_cv`.
    unsafe { put_result(st, inst.c, Value::Int(n)) }
}

/// Whether the defined compiled variable at offset `a` has one of the
/// types of `mask`; `None` when it is undefined (the generic op warns).
///
/// # Safety
/// As [`cv_slot`].
#[inline(always)]
#[allow(clippy::inline_always)]
unsafe fn type_test(st: St, a: u32, mask: u8) -> Option<bool> {
    // SAFETY: the caller's contract.
    unsafe { &*cv_slot(st, a) }
        .as_ref()
        .map(|v| type_mask::matches(mask, v))
}

/// `Op::TypeCheck` of a defined compiled variable (`ZEND_TYPE_CHECK_SPEC_CV`).
///
/// # Safety
/// As [`strlen_cv`].
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn type_check_cv(st: St, inst: &Inst) -> bool {
    // SAFETY: as in `strlen_cv`.
    let Some(r) = (unsafe { type_test(st, inst.a, inst.k) }) else {
        return false;
    };
    // SAFETY: as in `strlen_cv`.
    unsafe { put_result(st, inst.c, Value::Bool(r)) }
}

/// `Op::TypeCheck` of a defined compiled variable and the `JmpZ`/`JmpNz`
/// of its result (the smart branch): jumps when the result is `e`.
///
/// # Safety
/// As [`strlen_cv`].
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn type_check_jmp_cv(st: St, inst: &Inst) -> Br {
    // SAFETY: as in `strlen_cv`.
    match unsafe { type_test(st, inst.a, inst.k) } {
        Some(r) if r == (inst.e != 0) => Br::Jump,
        Some(_) => Br::Fall,
        None => Br::Decline,
    }
}

/// `isset($cv)` (`b` = 0) or `empty($cv)` (`b` = 1) as
/// `ZEND_ISSET_ISEMPTY_CV` decides it without a warning: undefined is not
/// set and empty; a value (behind a reference: the referenced one) is set
/// unless null, and empty when [`plain_truth`] says false. `None` for a
/// value whose truth needs a conversion (objects: the generic op) and a
/// reference being written.
///
/// # Safety
/// As [`cv_slot`].
#[inline(always)]
#[allow(clippy::inline_always)]
unsafe fn isset_test(st: St, a: u32, empty: bool) -> Option<bool> {
    let decide = |v: &Value| {
        if empty {
            plain_truth(v).map(|b| !b)
        } else {
            Some(!matches!(v, Value::Null))
        }
    };
    // SAFETY: the caller's contract.
    match unsafe { &*cv_slot(st, a) } {
        None => Some(empty),
        Some(Value::Reference(r)) => decide(&*r.try_borrow()?),
        Some(v) => decide(v),
    }
}

/// `Op::Isset` / `Op::Empty` of a compiled variable.
///
/// # Safety
/// As [`strlen_cv`].
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn isset_cv(st: St, inst: &Inst) -> bool {
    // SAFETY: as in `strlen_cv`.
    let Some(r) = (unsafe { isset_test(st, inst.a, inst.b != 0) }) else {
        return false;
    };
    // SAFETY: as in `strlen_cv`.
    unsafe { put_result(st, inst.c, Value::Bool(r)) }
}

/// `Op::Isset` / `Op::Empty` of a compiled variable and the `JmpZ`/`JmpNz`
/// of its result (the smart branch): jumps when the result is `e`.
///
/// # Safety
/// As [`strlen_cv`].
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn isset_jmp_cv(st: St, inst: &Inst) -> Br {
    // SAFETY: as in `strlen_cv`.
    match unsafe { isset_test(st, inst.a, inst.b != 0) } {
        Some(r) if r == (inst.e != 0) => Br::Jump,
        Some(_) => Br::Fall,
        None => Br::Decline,
    }
}

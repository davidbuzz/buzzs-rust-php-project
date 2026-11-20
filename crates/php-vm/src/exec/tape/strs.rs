// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Tape instructions for strings and the builtin ops.
//!
//! `strlen()`, `count()`/`sizeof()`, the type tests (`is_int()` & co.) and
//! `isset()`/`empty()` of a compiled variable, which `zend_compile.c`
//! compiles to `ZEND_STRLEN`, `ZEND_COUNT`, `ZEND_TYPE_CHECK` and
//! `ZEND_ISSET_ISEMPTY_CV`. The last two are smart branches in php-src: a
//! `JMPZ`/`JMPNZ` of the result right after them is taken by the test
//! itself (`ZEND_VM_SMART_BRANCH`), which the fused instructions here do.
//!
//! Also the one-argument frameless calls whose result needs no handler
//! (`ZEND_FRAMELESS_ICALL_1` of `abs()`, `ord()`, `chr()` and the math
//! functions: `special::quick_eval`).
//!
//! The handlers are out of line, as the `dims` family's: inlined into
//! [`super::run_core`] they changed its register allocation and cost every
//! user call ~10 instructions.
//!
//! See the "Families" section of [`super`] for the contract of a handler
//! and of [`lower`].

use super::{
    cv_off, cv_slot, frame_st, is_scalar, lit_off, rel, tmp_off, tmp_slot, Br, Inst, St, TOp, KC,
    KL,
};
use crate::exec::spec::plain_truth;
use crate::opcode::{type_mask, CachedFuncKind, Op, OpArray, Operand, PlaceBase};
use crate::vm::Frame;
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
        // `f($cv)` or `f(literal)` of a builtin whose one-argument call
        // needs no handler: the function's id in `e`, its frameless class
        // in `d`, the call site's cache slot in `b`. The instruction covers
        // the `Nop`s of the `Send` and the `DoCall` the op replaced.
        Op::FramelessICall(ref fc) => {
            if fc.argc != 1 {
                return None;
            }
            let Some(Value::Str(lc)) = code.literals.get(fc.name as usize) else {
                return None;
            };
            let (id, class) = crate::special::quick_id(lc.as_bytes())?;
            let (k, a) = match fc.vals[0] {
                Operand::Cv(cv) => (KC, cv_off(code, cv)?),
                Operand::Lit(l) => {
                    code.literals.get(l as usize)?;
                    (KL, lit_off(l)?)
                }
                _ => return None,
            };
            Inst {
                k,
                e: id,
                a,
                b: fc.name,
                c: dst(fc.dst)?,
                d: class,
                ..plain(TOp::FramelessQ)
            }
        }
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
/// `inst` is an instruction [`lower`] built for `frame`, the current frame.
#[inline(never)]
pub(super) unsafe fn strlen_cv(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract: `frame` is the current frame.
    let st = unsafe { frame_st(frame) };
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
#[inline(never)]
pub(super) unsafe fn count_cv(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract: `frame` is the current frame.
    let st = unsafe { frame_st(frame) };
    // SAFETY: as in `strlen_cv`.
    let Some(Value::Array(a)) = (unsafe { &*cv_slot(st, inst.a) }) else {
        return false;
    };
    let n = i64::try_from(a.len()).unwrap_or(i64::MAX);
    // SAFETY: as in `strlen_cv`.
    unsafe { put_result(st, inst.c, Value::Int(n)) }
}

/// `Op::FramelessICall` of one argument whose result needs no handler
/// (`Vm::frameless_icall_op`'s inline result): the call site's cache holds
/// the builtin (of the frameless class the instruction was decoded for,
/// neither deprecated nor `#[\NoDiscard]`), no observer is installed, and
/// the argument (a defined variable, not a reference, or a literal) has a
/// shape the function accepts as it is (`special::quick_eval`). The
/// interrupt check after the call is the instruction's poll. Declines
/// anything else, having changed nothing: the op then runs in `Vm::run`
/// (which calls the handler, or makes the call with its frame).
///
/// # Safety
/// As [`strlen_cv`].
#[inline(never)]
#[allow(clippy::cast_ptr_alignment)] // a slot offset from the literal table's base
pub(super) unsafe fn frameless_quick(frame: *mut Frame, inst: &Inst) -> bool {
    if super::jit::OBSERVER_ON.load(std::sync::atomic::Ordering::Relaxed) {
        return false;
    }
    // SAFETY: the caller's contract: `frame` is the current frame.
    let code = unsafe { &(*frame).code };
    {
        let Ok(cache) = code.fcall_cache.try_borrow() else {
            return false;
        };
        let Some(Some(entry)) = cache.get(inst.b as usize) else {
            return false;
        };
        let CachedFuncKind::Builtin(b) = &entry.func else {
            return false;
        };
        if !crate::special::frameless_class_is(b.flags, inst.d) {
            return false;
        }
    }
    // SAFETY: the caller's contract: `frame` is the current frame.
    let st = unsafe { frame_st(frame) };
    let v: &Value = if inst.k == KC {
        // SAFETY: `a` is a validated variable offset.
        match unsafe { &*cv_slot(st, inst.a) } {
            Some(v) => v,
            None => return false,
        }
    } else {
        // SAFETY: `a` is a validated literal offset.
        unsafe { &*st.lits.add(inst.a as usize).cast::<Value>() }
    };
    // A result slot holding a value to release is the generic op's.
    // SAFETY: `c` is the validated result field.
    if inst.c != NO_DST && !is_scalar(unsafe { tmp_slot(st, inst.c) }) {
        return false;
    }
    let Some(r) = crate::special::quick_eval(inst.e, v) else {
        return false;
    };
    // SAFETY: as above; the slot was just seen to hold a scalar.
    unsafe { put_result(st, inst.c, r) }
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
#[inline(never)]
pub(super) unsafe fn type_check_cv(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract: `frame` is the current frame.
    let st = unsafe { frame_st(frame) };
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
#[inline(never)]
pub(super) unsafe fn type_check_jmp_cv(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract: `frame` is the current frame.
    let st = unsafe { frame_st(frame) };
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
#[inline(never)]
pub(super) unsafe fn isset_cv(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract: `frame` is the current frame.
    let st = unsafe { frame_st(frame) };
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
#[inline(never)]
pub(super) unsafe fn isset_jmp_cv(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract: `frame` is the current frame.
    let st = unsafe { frame_st(frame) };
    // SAFETY: as in `strlen_cv`.
    match unsafe { isset_test(st, inst.a, inst.b != 0) } {
        Some(r) if r == (inst.e != 0) => Br::Jump,
        Some(_) => Br::Fall,
        None => Br::Decline,
    }
}

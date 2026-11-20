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
//! Also the frameless calls whose result needs no handler
//! (`ZEND_FRAMELESS_ICALL_n` of `abs()`, `ord()`, `chr()`, the math
//! functions, `strpos()`, `substr()`, `max()`, `min()`, a strict
//! `in_array()` and the `str_contains()` family: `special::quick_eval_n`).
//!
//! The handlers are out of line, as the `dims` family's: inlined into
//! [`super::run_core`] they changed its register allocation and cost every
//! user call ~10 instructions.
//!
//! See the "Families" section of [`super`] for the contract of a handler
//! and of [`lower`].

use super::{
    cv_off, cv_slot, frame_st, is_scalar, lit_off, rel, tmp_off, tmp_slot, Br, Inst, St, TOp,
};
use crate::exec::spec::plain_truth;
use crate::opcode::{type_mask, CachedFuncKind, Op, OpArray, Operand, PlaceBase};
use crate::vm::Frame;
use php_runtime::Value;

/// The result field of an instruction whose result is unused.
const NO_DST: u32 = u32::MAX;

/// The kinds of a frameless call instruction's operands, two bits each in
/// its `k` (the first operand lowest; the count in the top two bits): a
/// compiled variable, a temporary (consumed), a literal.
const Q_CV: u8 = 0;
const Q_TMP: u8 = 1;
const Q_LIT: u8 = 2;
/// `k` of a call of one compiled variable, of one temporary and of one
/// literal.
const Q_ONE_CV: u8 = (1 << 6) | Q_CV;
const Q_ONE_TMP: u8 = (1 << 6) | Q_TMP;
const Q_ONE_LIT: u8 = (1 << 6) | Q_LIT;
/// Where a frameless call instruction's `p` keeps the function's frameless
/// class (five bits; the result's offset is below it, the call site's cache
/// slot above).
const Q_CLASS_SHIFT: u32 = 32;
const Q_CACHE_SHIFT: u32 = 37;

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
        // The call of a builtin whose result needs no handler for the
        // arguments it is usually given (`special::quick_eval_n`): the
        // function's id in `e`, the kinds of its 1..=3 operands and their
        // count in `k`, the operands in `a`, `b` and `d`, and in `p` the
        // result's offset, the function's frameless class and the call
        // site's cache slot. The instruction ends in a jump (`c`) to the op
        // after the call: past the `Nop`s of the `Send`s and the `DoCall`
        // behind an op that stands at its call's `InitFCall`, to the next
        // op for one that stands at the `DoCall` (arguments computed
        // first).
        Op::FramelessICall(ref fc) => {
            let argc = usize::from(fc.argc);
            let Some(Value::Str(lc)) = code.literals.get(fc.name as usize) else {
                return None;
            };
            let (id, class) = crate::special::quick_id_n(lc.as_bytes(), argc)?;
            if class >= 1 << (Q_CACHE_SHIFT - Q_CLASS_SHIFT) {
                return None;
            }
            let mut k = fc.argc << 6;
            let mut offs = [0u32; 3];
            for (i, val) in fc.vals.get(..argc)?.iter().enumerate() {
                let (kind, off) = match *val {
                    Operand::Cv(cv) => (Q_CV, cv_off(code, cv)?),
                    Operand::Tmp(t) => (Q_TMP, tmp_off(code, t)?),
                    Operand::Lit(l) => {
                        code.literals.get(l as usize)?;
                        (Q_LIT, lit_off(l)?)
                    }
                    _ => return None,
                };
                k |= kind << (2 * i);
                offs[i] = off;
            }
            let after = u32::try_from(i + 1 + fc.tail()).ok()?;
            // (A 64-bit word: no instruction on a target whose `usize` is
            // narrower.)
            let p = (u64::from(fc.name) << Q_CACHE_SHIFT)
                | (u64::from(class) << Q_CLASS_SHIFT)
                | u64::from(dst(fc.dst)?);
            Inst {
                k,
                e: id,
                a: offs[0],
                b: offs[1],
                c: rel(code, i, after)?,
                d: offs[2],
                p: usize::try_from(p).ok()?,
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
pub(super) unsafe extern "C" fn strlen_cv(frame: *mut Frame, inst: *const Inst) -> bool {
    // SAFETY: the caller's contract: a row of the frame's tape.
    let inst = unsafe { &*inst };
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
pub(super) unsafe extern "C" fn count_cv(frame: *mut Frame, inst: *const Inst) -> bool {
    // SAFETY: the caller's contract: a row of the frame's tape.
    let inst = unsafe { &*inst };
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

/// `Op::FramelessICall` whose result needs no handler
/// (`Vm::frameless_icall_op`'s inline result, and the functions of
/// `special::quick_eval_n`): the call site's cache holds the builtin (of
/// the frameless class the instruction was decoded for, neither deprecated
/// nor `#[\NoDiscard]`), no observer is installed, and the arguments
/// (defined variables and temporaries, none a reference, and literals) have
/// shapes the function accepts as they are. The temporaries among them are
/// consumed, as their `Send`s moved them. Done, the instruction jumps to
/// the op after the call ([`Br::Jump`]: the jump's poll is the interrupt
/// check after the call). Declines anything else, having changed nothing:
/// the op then runs in `Vm::run` (which calls the handler, or makes the
/// call with its frame).
///
/// A call of one argument (the math functions, `ord()`, `chr()`,
/// `dechex()`: of a variable, a literal, or a temporary as in
/// `chr(65 + $i % 26)`) is decided here; a call of several out of line
/// ([`frameless_quick_n`], which decodes three operands and costs a call
/// ~165 instructions around the function).
///
/// # Safety
/// As [`strlen_cv`].
#[inline(never)]
#[allow(clippy::cast_ptr_alignment)] // a slot offset from the literal table's base
pub(super) unsafe extern "C" fn frameless_quick(frame: *mut Frame, inst: *const Inst) -> Br {
    // SAFETY: the caller's contract: a row of the frame's tape.
    let inst = unsafe { &*inst };
    if super::jit::OBSERVER_ON.load(std::sync::atomic::Ordering::Relaxed) {
        return Br::Decline;
    }
    let p = inst.p as u64;
    // SAFETY: the caller's contract: `frame` is the current frame.
    let code = unsafe { &(*frame).code };
    {
        let Ok(cache) = code.fcall_cache.try_borrow() else {
            return Br::Decline;
        };
        let slot = usize::try_from(p >> Q_CACHE_SHIFT).unwrap_or(usize::MAX);
        let Some(Some(entry)) = cache.get(slot) else {
            return Br::Decline;
        };
        let CachedFuncKind::Builtin(b) = &entry.func else {
            return Br::Decline;
        };
        let class = u32::try_from((p >> Q_CLASS_SHIFT) & 31).unwrap_or(0);
        if !crate::special::frameless_class_is(b.flags, class) {
            return Br::Decline;
        }
    }
    if inst.k != Q_ONE_CV && inst.k != Q_ONE_LIT && inst.k != Q_ONE_TMP {
        // SAFETY: the caller's contract.
        return unsafe { frameless_quick_n(frame, inst) };
    }
    let dst = u32::try_from(p & 0xffff_ffff).unwrap_or(NO_DST);
    // SAFETY: the caller's contract: `frame` is the current frame.
    let st = unsafe { frame_st(frame) };
    if inst.k == Q_ONE_TMP {
        // SAFETY: `a` is a validated temporary offset.
        let arg = unsafe { tmp_slot(st, inst.a) };
        // A reference is the generic op's (it reads through it), and so is
        // a result slot holding a value to release, unless that slot is
        // the argument's own.
        // SAFETY: `dst` is the validated result field.
        if matches!(arg, Value::Reference(_))
            || (dst != NO_DST && dst != inst.a && !is_scalar(unsafe { tmp_slot(st, dst) }))
        {
            return Br::Decline;
        }
        let Some(r) = crate::special::quick_eval(inst.e, arg) else {
            return Br::Decline;
        };
        // The argument is consumed, as its `Send` moved it: nothing refers
        // to it any more, and the result slot then holds a scalar (seen
        // above, or the null the argument left).
        // SAFETY: validated offsets, as above.
        return unsafe {
            q_consume(st, Q_TMP, inst.a);
            if put_result(st, dst, r) {
                Br::Jump
            } else {
                Br::Decline
            }
        };
    }
    let v: &Value = if inst.k == Q_ONE_CV {
        // SAFETY: `a` is a validated variable offset.
        match unsafe { &*cv_slot(st, inst.a) } {
            Some(v) => v,
            None => return Br::Decline,
        }
    } else {
        // SAFETY: `a` is a validated literal offset.
        unsafe { &*st.lits.add(inst.a as usize).cast::<Value>() }
    };
    // A result slot holding a value to release is the generic op's.
    // SAFETY: `dst` is the validated result field.
    if dst != NO_DST && !is_scalar(unsafe { tmp_slot(st, dst) }) {
        return Br::Decline;
    }
    let Some(r) = crate::special::quick_eval(inst.e, v) else {
        return Br::Decline;
    };
    // SAFETY: as above; the slot was just seen to hold a scalar.
    if unsafe { put_result(st, dst, r) } {
        Br::Jump
    } else {
        Br::Decline
    }
}

/// The operand of kind `kind` at `off` of a frameless call instruction,
/// where it lies: `None` for an undefined variable and for a reference
/// (the generic op warns, or reads through it).
///
/// # Safety
/// `off` is an offset of that kind [`lower`] validated for the frame of
/// `st`; the reference is not used past the next write to the slot.
#[inline(always)]
#[allow(clippy::inline_always, clippy::cast_ptr_alignment)] // a few loads; slot offsets
unsafe fn q_arg<'a>(st: St, kind: u8, off: u32) -> Option<&'a Value> {
    let v: &Value = match kind {
        // SAFETY: a validated variable offset (a `CvSlot` is a transparent
        // `Option<Value>`).
        Q_CV => unsafe { &*st.cvs.add(off as usize).cast::<Option<Value>>() }.as_ref()?,
        // SAFETY: a validated temporary offset.
        Q_TMP => unsafe { &*st.tmps.add(off as usize).cast::<Value>() },
        // SAFETY: a validated literal offset.
        _ => unsafe { &*st.lits.add(off as usize).cast::<Value>() },
    };
    (!matches!(v, Value::Reference(_))).then_some(v)
}

/// Leaves a consumed temporary null, as its `Send` left it: what it held is
/// released (a scalar owns nothing: no call for it).
///
/// # Safety
/// As [`q_arg`]; nothing refers to the slot's value any more.
#[inline(always)]
#[allow(clippy::inline_always)] // a tag test and a store
unsafe fn q_consume(st: St, kind: u8, off: u32) {
    if kind == Q_TMP {
        // SAFETY: a validated temporary offset.
        let v = std::mem::take(unsafe { tmp_slot(st, off) });
        if is_scalar(&v) {
            std::mem::forget(v);
        } else {
            drop(v);
        }
    }
}

/// [`frameless_quick`] for a call of several arguments (it takes one too).
/// The call site's cache was checked. Straight-line code per number of
/// arguments: each operand is read where it lies, the result is
/// computed, and only then are the consumed temporaries released and the
/// result stored.
///
/// # Safety
/// As [`strlen_cv`].
#[inline(never)]
unsafe fn frameless_quick_n(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract: `frame` is the current frame.
    let st = unsafe { frame_st(frame) };
    let dst = u32::try_from(inst.p as u64 & 0xffff_ffff).unwrap_or(NO_DST);
    let count = inst.k >> 6;
    let (ka, kb, kc) = (inst.k & 3, (inst.k >> 2) & 3, (inst.k >> 4) & 3);
    // A result slot holding a value to release is the generic op's, unless
    // it is where an argument the call consumes lies.
    let over_arg = (ka == Q_TMP && inst.a == dst)
        || (count >= 2 && kb == Q_TMP && inst.b == dst)
        || (count == 3 && kc == Q_TMP && inst.d == dst);
    // SAFETY: `dst` is the validated result field.
    if dst != NO_DST && !over_arg && !is_scalar(unsafe { tmp_slot(st, dst) }) {
        return Br::Decline;
    }
    // SAFETY (the operands below): validated offsets of their kinds; the
    // references are dropped before the temporaries are consumed.
    let Some(a) = (unsafe { q_arg(st, ka, inst.a) }) else {
        return Br::Decline;
    };
    let result = match count {
        1 => crate::special::quick_eval(inst.e, a),
        2 => {
            let Some(b) = (unsafe { q_arg(st, kb, inst.b) }) else {
                return Br::Decline;
            };
            crate::special::quick_eval_2(inst.e, a, b)
        }
        _ => {
            let (Some(b), Some(c)) = (unsafe { (q_arg(st, kb, inst.b), q_arg(st, kc, inst.d)) })
            else {
                return Br::Decline;
            };
            crate::special::quick_eval_3(inst.e, a, b, c)
        }
    };
    let Some(r) = result else {
        return Br::Decline;
    };
    // The arguments are not read past this point (the jump polls for what
    // a release may queue).
    // SAFETY: as above.
    unsafe {
        q_consume(st, ka, inst.a);
        if count >= 2 {
            q_consume(st, kb, inst.b);
        }
        if count == 3 {
            q_consume(st, kc, inst.d);
        }
    }
    // SAFETY: the slot holds a scalar (seen above, or the null a consumed
    // argument left).
    if unsafe { put_result(st, dst, r) } {
        Br::Jump
    } else {
        Br::Decline
    }
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
pub(super) unsafe extern "C" fn type_check_cv(frame: *mut Frame, inst: *const Inst) -> bool {
    // SAFETY: the caller's contract: a row of the frame's tape.
    let inst = unsafe { &*inst };
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
pub(super) unsafe extern "C" fn type_check_jmp_cv(frame: *mut Frame, inst: *const Inst) -> Br {
    // SAFETY: the caller's contract: a row of the frame's tape.
    let inst = unsafe { &*inst };
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
pub(super) unsafe extern "C" fn isset_cv(frame: *mut Frame, inst: *const Inst) -> bool {
    // SAFETY: the caller's contract: a row of the frame's tape.
    let inst = unsafe { &*inst };
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
pub(super) unsafe extern "C" fn isset_jmp_cv(frame: *mut Frame, inst: *const Inst) -> Br {
    // SAFETY: the caller's contract: a row of the frame's tape.
    let inst = unsafe { &*inst };
    // SAFETY: the caller's contract: `frame` is the current frame.
    let st = unsafe { frame_st(frame) };
    // SAFETY: as in `strlen_cv`.
    match unsafe { isset_test(st, inst.a, inst.b != 0) } {
        Some(r) if r == (inst.e != 0) => Br::Jump,
        Some(_) => Br::Fall,
        None => Br::Decline,
    }
}

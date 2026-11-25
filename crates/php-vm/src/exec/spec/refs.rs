// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The frame-local ops on a compiled variable that holds a reference (bound
//! by `global`, `static`, a by-reference parameter, `&`, `foreach` by
//! reference), beside the lean paths of `exec/hot.rs` and the tape.
//!
//! php-src's handlers dereference every operand (`ZVAL_DEREF`, one tag
//! test), so `++$n`, `$n + 1`, `$s . 'x'`, `$n += 2`, `if ($flag)` and
//! `count($list)` cost the same whether the variable holds the value or a
//! reference to it (ref: Zend/zend_vm_def.h `ZEND_PRE_INC`,
//! `ZEND_ASSIGN_OP`, `ZEND_ASSIGN`, `ZEND_ADD`, `ZEND_CONCAT`,
//! `ZEND_JMPZ`, `ZEND_IS_IDENTICAL`, `ZEND_COUNT`). The lean paths only
//! take a variable that holds the value: a reference made them decline,
//! and the op ran through the generic loop (`Vm::run`), at several hundred
//! to a few thousand instructions.
//!
//! [`step_ref`] is where such a decline goes before the generic loop: one
//! out-of-line function that looks at the op once and hands it to the
//! small function of its path. Each one has the contract of a hot op's fast
//! path: it completes the op without diagnostics, user code or frame
//! changes, or it changes nothing and returns [`DECLINED`]. What it takes:
//!
//! * a reference with no type sources (`ZEND_REF_HAS_TYPE_SOURCES`: a
//!   reference bound to a typed property checks every write; any reference
//!   with VM metadata is left to the generic path), not borrowed elsewhere;
//! * a frame without an attached symbol table (the generic op keeps the
//!   table's copy);
//! * values whose replacement runs no destructor and buffers no GC root
//!   (null, bool, int, float, string), read without a diagnostic.
//!
//! A write goes into the reference's value, which every holder of the
//! reference sees (the global table and the main script's variable after
//! `global`, the static slot after `static`, the caller's variable after a
//! by-reference call): the variable slot itself keeps the reference.

use super::super::hot::{jump_of, DECLINED};
use super::super::{num_bin, Num};
use super::{plain_truth, put_tmp};
use crate::opcode::{cmp_outcome, BinKind, IncDecKind, Op, Operand};
use crate::vm::{CvSlot, Frame};
use php_runtime::{BinaryOp, PhpRef, Value, ZStr};

/// The op at `at` of frame `f`, after its lean path declined, when an
/// operand is a compiled variable holding a reference: the `ip` to
/// continue at, or [`DECLINED`] (nothing changed) for the generic loop.
/// Never inlined: it is reached only by a decline, and its callers' code
/// stays as it was.
#[inline(never)]
pub(in crate::exec) fn step_ref(f: &mut Frame, ops: &[Op], lits: &[Value], at: usize) -> usize {
    if f.symtab_attached {
        return DECLINED;
    }
    let Some(op) = ops.get(at) else {
        return DECLINED;
    };
    let next = at + 1;
    match *op {
        // The fused forms leave the comparison or jump after them to run
        // as its own op, as their lean paths do when it is not on plain
        // numbers.
        Op::IncDecCv { cv, kind, dst, .. } => done(incdec_ref(f, cv, kind, dst), next),
        Op::IncDecCvCmpJmp { cv, kind, .. } => done(incdec_ref(f, cv, kind, None), next),
        Op::AssignOpCv {
            cv,
            op: kind,
            val,
            dst,
            ..
        } => done(assign_op_ref(f, lits, cv, kind, val, dst), next),
        Op::AssignOpCvCmpJmp {
            cv, op: kind, val, ..
        }
        | Op::AssignOpCvJmp {
            cv, op: kind, val, ..
        } => done(assign_op_ref(f, lits, cv, kind, val, None), next),
        Op::AssignCv { cv, val, dst } => done(assign_ref(f, cv, val, dst), next),
        Op::ConcatCv { cv, val, dst, .. } => done(concat_ref(f, lits, cv, val, dst), next),
        Op::Bin {
            op: kind,
            a,
            b,
            dst,
        } => done(bin_ref(f, lits, kind, a, b, dst), next),
        // The result goes to `dst`; the `AssignCv` after the op stores it
        // (as the lean paths do for a result they do not store
        // themselves).
        Op::BinAssignCv {
            op: kind,
            a,
            b,
            dst,
            ..
        }
        | Op::BinAssignCvCmpJmp {
            op: kind,
            a,
            b,
            dst,
            ..
        }
        | Op::BinAssignCvJmp {
            op: kind,
            a,
            b,
            dst,
            ..
        } => done(bin_ref(f, lits, kind, a, b, dst), next),
        Op::JmpZ(Operand::Cv(cv), t) => match truth_ref(f, cv) {
            Some(false) => t as usize,
            Some(true) => next,
            None => DECLINED,
        },
        Op::JmpNz(Operand::Cv(cv), t) => match truth_ref(f, cv) {
            Some(true) => t as usize,
            Some(false) => next,
            None => DECLINED,
        },
        Op::CmpJmp {
            op: kind,
            a,
            b,
            target,
            jmp_if,
            mask,
            ..
        } => match cmp_jmp_ref(f, lits, kind, a, b, jmp_if, mask) {
            Some(true) => target as usize,
            // Skips the jump op after the smart branch.
            Some(false) => next + 1,
            None => DECLINED,
        },
        Op::Count {
            a: Operand::Cv(cv),
            dst,
            ..
        } => done(count_ref(f, cv, dst), next),
        _ => DECLINED,
    }
}

/// `next` when the path completed the op, [`DECLINED`] otherwise.
#[inline(always)]
#[allow(clippy::inline_always)]
fn done(ok: bool, next: usize) -> usize {
    if ok {
        next
    } else {
        DECLINED
    }
}

/// The reference compiled variable `cv` holds, when it is one whose
/// writes need no type check (no VM metadata: `ZEND_REF_HAS_TYPE_SOURCES`
/// is clear).
#[inline(always)]
#[allow(clippy::inline_always)]
fn untyped_ref(f: &Frame, cv: u32) -> Option<&PhpRef> {
    match f.cvs.get(cv as usize).and_then(CvSlot::get)? {
        Value::Reference(r) if r.meta().is_none() => Some(r),
        _ => None,
    }
}

/// Whether operand `op` is a compiled variable holding a reference: the
/// test that sends a read-only op here rather than to the generic loop.
#[inline(always)]
#[allow(clippy::inline_always)]
fn is_ref_cv(f: &Frame, op: Operand) -> bool {
    matches!(op, Operand::Cv(cv) if matches!(
        f.cvs.get(cv as usize).and_then(CvSlot::get),
        Some(Value::Reference(_))
    ))
}

/// Whether the temporary slot `t` may be overwritten without a release
/// that could run a destructor (what a consuming read left, or a scalar).
#[inline(always)]
#[allow(clippy::inline_always)]
fn tmp_free(f: &Frame, t: u32) -> bool {
    matches!(
        f.tmps.get(t as usize),
        Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_))
    )
}

/// A copy of a plain value (null, bool, int, float, string) the operand
/// holds, looking through a compiled variable's reference
/// (`ZVAL_COPY_DEREF`); `None` for anything else, an undefined variable
/// (it warns) included. A consumed temporary is copied, not taken: the
/// caller clears it ([`consume_str_tmp`]) once the op completes.
#[inline]
fn plain_operand(f: &Frame, lits: &[Value], op: Operand) -> Option<Value> {
    let v = match op {
        Operand::Cv(cv) => match f.cvs.get(cv as usize).and_then(CvSlot::get)? {
            Value::Reference(r) => return plain_copy(&*r.try_borrow()?),
            v => v,
        },
        Operand::Lit(i) => lits.get(i as usize)?,
        Operand::Tmp(t) | Operand::TmpKeep(t) => f.tmps.get(t as usize)?,
        Operand::Unused => return None,
    };
    plain_copy(v)
}

/// A copy of `v` when it is null, a bool, an int, a float or a string.
#[inline]
fn plain_copy(v: &Value) -> Option<Value> {
    Some(match v {
        Value::Null => Value::Null,
        Value::Bool(b) => Value::Bool(*b),
        Value::Int(i) => Value::Int(*i),
        Value::Float(x) => Value::Float(*x),
        Value::Str(s) => Value::Str(s.clone()),
        _ => return None,
    })
}

/// Releases the string a consumed temporary operand held (`FREE_OP`), after
/// the op read a copy of it ([`plain_operand`]).
#[inline]
fn consume_str_tmp(f: &mut Frame, op: Operand) {
    if let Operand::Tmp(t) = op {
        if let Some(slot) = f.tmps.get_mut(t as usize) {
            if matches!(slot, Value::Str(_)) {
                *slot = Value::Null;
            }
        }
    }
}

/// The number a plain value is, without conversion.
#[inline]
fn num(v: &Value) -> Option<Num> {
    match *v {
        Value::Int(i) => Some(Num::Int(i)),
        Value::Float(x) => Some(Num::Float(x)),
        _ => None,
    }
}

/// `++`/`--` of a number or null held by reference, in place
/// (`increment_function` / `decrement_function`): an int that overflows
/// becomes the float one past it (`ZEND_LONG_MAX + 1.0`), null incremented
/// becomes 1. Null decremented (a warning), strings (increment rules,
/// deprecations), bools and the rest go generic.
#[allow(clippy::cast_precision_loss)] // php-src's (double) of the overflowing long
fn incdec_value(v: &Value, inc: bool) -> Option<Value> {
    Some(match *v {
        Value::Int(i) => {
            if inc {
                i.checked_add(1)
                    .map_or(Value::Float(i as f64 + 1.0), Value::Int)
            } else {
                i.checked_sub(1)
                    .map_or(Value::Float(i as f64 - 1.0), Value::Int)
            }
        }
        Value::Float(x) => Value::Float(if inc { x + 1.0 } else { x - 1.0 }),
        Value::Null if inc => Value::Int(1),
        _ => return None,
    })
}

/// [`Op::IncDecCv`] of a variable holding an untyped reference to a
/// number (or null, incremented): `ZEND_PRE_INC_SPEC_CV` & co. after
/// `ZVAL_DEREF`, the result (old value for the post forms) written to
/// `dst` when used.
#[inline(never)]
fn incdec_ref(f: &mut Frame, cv: u32, kind: IncDecKind, dst: Option<u32>) -> bool {
    if let Some(t) = dst {
        if !tmp_free(f, t) {
            return false;
        }
    }
    let (old, new) = {
        let Some(r) = untyped_ref(f, cv) else {
            return false;
        };
        let Some(mut v) = r.try_borrow_mut() else {
            return false;
        };
        let Some(new) = incdec_value(&v, kind.is_inc()) else {
            return false;
        };
        // The old value is a number or null: nothing to release.
        let old = std::mem::replace(&mut *v, scalar(&new));
        (old, new)
    };
    if let Some(t) = dst {
        if let Some(slot) = f.tmps.get_mut(t as usize) {
            put_tmp(slot, if kind.is_post() { old } else { new });
        }
    }
    true
}

/// A copy of a scalar.
#[inline]
fn scalar(v: &Value) -> Value {
    match *v {
        Value::Int(i) => Value::Int(i),
        Value::Float(x) => Value::Float(x),
        Value::Bool(b) => Value::Bool(b),
        _ => Value::Null,
    }
}

/// `x op y` of two numbers for a compound assignment
/// (`zend_binary_op`'s `IS_LONG`/`IS_DOUBLE` paths): [`num_bin`], and an
/// int `+`, `-`, `*` that overflows as php-src computes it in doubles
/// (`ZEND_SIGNED_MULTIPLY_LONG`, `fast_long_add_function`).
#[allow(clippy::cast_precision_loss)] // php-src's (double) widening on overflow
fn compound_num(op: BinaryOp, x: Num, y: Num) -> Option<Value> {
    if let Some(r) = num_bin(BinKind::Arith(op), x, y) {
        return Some(r);
    }
    let (Num::Int(p), Num::Int(q)) = (x, y) else {
        return None;
    };
    let (p, q) = (p as f64, q as f64);
    Some(Value::Float(match op {
        BinaryOp::Add => p + q,
        BinaryOp::Sub => p - q,
        BinaryOp::Mul => p * q,
        _ => return None,
    }))
}

/// [`Op::AssignOpCv`] (`$cv op= val`) of a variable holding an untyped
/// reference to a number, with a number as `val` (itself possibly held by
/// reference): `ZEND_ASSIGN_OP_SPEC_CV_*` after `ZVAL_DEREF`, the number
/// replaced in place and written to `dst` when used. `.=` and every
/// operand that needs a conversion go generic.
#[inline(never)]
fn assign_op_ref(
    f: &mut Frame,
    lits: &[Value],
    cv: u32,
    op: BinaryOp,
    val: Operand,
    dst: Option<u32>,
) -> bool {
    if let Some(t) = dst {
        if !tmp_free(f, t) {
            return false;
        }
    }
    // The operand first: it may be this very reference (`$r += $r`).
    let Some(y) = plain_operand(f, lits, val).as_ref().and_then(num) else {
        return false;
    };
    let r = {
        let Some(r) = untyped_ref(f, cv) else {
            return false;
        };
        let Some(mut v) = r.try_borrow_mut() else {
            return false;
        };
        let Some(x) = num(&v) else {
            return false;
        };
        let Some(n) = compound_num(op, x, y) else {
            return false;
        };
        // A number replaces a number: nothing to release.
        *v = scalar(&n);
        n
    };
    if let Some(t) = dst {
        if let Some(slot) = f.tmps.get_mut(t as usize) {
            put_tmp(slot, r);
        }
    }
    true
}

/// [`Op::AssignCv`] (`$cv = val`) of a variable holding an untyped
/// reference whose value is null, a bool, a number or a string (released
/// without a destructor): `ZEND_ASSIGN_SPEC_CV_*` through
/// `zend_assign_to_variable`'s `Z_ISREF_P` branch, the value stored into
/// the reference. `val` is a literal, a temporary (moved) or a defined
/// variable (copied, dereferenced: `ZVAL_COPY_DEREF`), the result written
/// to `dst` when used.
#[inline(never)]
fn assign_ref(f: &mut Frame, cv: u32, val: Operand, dst: Option<u32>) -> bool {
    if let Some(t) = dst {
        if !tmp_free(f, t) {
            return false;
        }
    }
    // The variables, the temporaries and the literals are borrowed apart:
    // the reference stays at hand while a temporary operand is moved out.
    let cvs = &f.cvs;
    let tmps = &mut f.tmps;
    let lits = &f.code.literals;
    let Some(Value::Reference(r)) = cvs.get(cv as usize).and_then(CvSlot::get) else {
        return false;
    };
    if r.meta().is_some() {
        return false;
    }
    match r.try_borrow().as_deref() {
        Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_)) => {}
        _ => return false,
    }
    let v = match val {
        Operand::Tmp(t) => match tmps.get_mut(t as usize) {
            Some(s) if !matches!(s, Value::Reference(_)) => std::mem::take(s),
            _ => return false,
        },
        Operand::TmpKeep(t) => match tmps.get(t as usize) {
            Some(s) if !matches!(s, Value::Reference(_)) => s.clone(),
            _ => return false,
        },
        Operand::Lit(i) => match lits.get(i as usize) {
            Some(l) if !matches!(l, Value::Reference(_)) => l.clone(),
            _ => return false,
        },
        Operand::Cv(src) => match cvs.get(src as usize).and_then(CvSlot::get) {
            Some(Value::Reference(s)) => match s.try_borrow() {
                Some(v) => Value::clone(&v),
                None => return false,
            },
            Some(s) => s.clone(),
            None => return false,
        },
        Operand::Unused => return false,
    };
    // The result first, as the generic assignment stores it.
    if let Some(t) = dst {
        if let Some(slot) = tmps.get_mut(t as usize) {
            put_tmp(slot, v.clone());
        }
    }
    // Not borrowed: nothing that ran since the test above could borrow it.
    let old = r.set(v);
    // Null, a bool, a number or a string: no destructor, no GC root.
    drop(old);
    true
}

/// [`Op::ConcatCv`] (`$cv .= val`) of a variable holding an untyped
/// reference to a string, with a string or an int as `val` (itself
/// possibly held by reference): `concat_function` with `result == op1`
/// after `ZVAL_DEREF`, the string extended in place when nothing else
/// holds it (`zend_string_extend`), the result written to `dst` when used.
#[inline(never)]
fn concat_ref(f: &mut Frame, lits: &[Value], cv: u32, val: Operand, dst: Option<u32>) -> bool {
    if let Some(t) = dst {
        if !tmp_free(f, t) {
            return false;
        }
    }
    let Some(rhs) = plain_operand(f, lits, val) else {
        return false;
    };
    if !matches!(rhs, Value::Str(_) | Value::Int(_)) {
        return false;
    }
    let r = {
        let Some(r) = untyped_ref(f, cv) else {
            return false;
        };
        let Some(mut v) = r.try_borrow_mut() else {
            return false;
        };
        let Value::Str(s) = &mut *v else {
            return false;
        };
        if matches!(&rhs, Value::Str(t) if ZStr::ptr_eq(s, t)) {
            // `$s .= $s` (the copy shares the string): the copy goes
            // first, so that a string nothing else holds grows in place.
            drop(rhs);
            s.append_self();
        } else {
            match &rhs {
                Value::Str(t) => s.append_str(t),
                Value::Int(n) => s.append_long(*n),
                _ => {}
            }
        }
        dst.map(|_| Value::Str(s.clone()))
    };
    consume_str_tmp(f, val);
    if let (Some(t), Some(r)) = (dst, r) {
        if let Some(slot) = f.tmps.get_mut(t as usize) {
            put_tmp(slot, r);
        }
    }
    true
}

/// `x . y` of two strings or ints (`concat_function` without conversions
/// that may warn or call `__toString()`).
fn concat_values(x: &Value, y: &Value) -> Option<ZStr> {
    Some(match (x, y) {
        (Value::Str(s), Value::Str(t)) => {
            if s.is_empty() {
                t.clone()
            } else if t.is_empty() {
                s.clone()
            } else {
                ZStr::concat_str(s, t)
            }
        }
        (Value::Str(s), Value::Int(n)) => s.clone().concat_owned_long(*n),
        (Value::Int(n), Value::Str(_) | Value::Int(_)) => {
            let mut s = ZStr::from(n.to_string().as_str());
            match y {
                Value::Str(t) => s.append_str(t),
                Value::Int(m) => s.append_long(*m),
                _ => {}
            }
            s
        }
        _ => return None,
    })
}

/// `==`, `!=`, `===`, `!==` of two plain values without conversions
/// (`zend_is_identical`, `zend_compare` of same-type pairs): numbers,
/// null, bools and strings (`zendi_smart_streq` for `==`); `===` of two
/// different types is false. `None` for loose comparisons that convert.
fn equal_values(op: BinKind, x: &Value, y: &Value) -> Option<bool> {
    let identity = match op {
        BinKind::Identical | BinKind::NotIdentical => true,
        BinKind::Equal | BinKind::NotEqual => false,
        _ => return None,
    };
    let negate = matches!(op, BinKind::NotIdentical | BinKind::NotEqual);
    let same = match (x, y) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(p), Value::Bool(q)) => p == q,
        (Value::Str(s), Value::Str(t)) => {
            ZStr::ptr_eq(s, t)
                || if identity {
                    s.as_bytes() == t.as_bytes()
                } else {
                    php_runtime::ops::smart_streq(s.as_bytes(), t.as_bytes())
                }
        }
        (lhs, rhs) => {
            if let (Some(p), Some(q)) = (num(lhs), num(rhs)) {
                let bit = super::num_outcome(p, q, identity);
                bit == cmp_outcome::EQUAL
            } else if identity && std::mem::discriminant(lhs) != std::mem::discriminant(rhs) {
                false
            } else {
                return None;
            }
        }
    };
    Some(same != negate)
}

/// [`Op::Bin`] with an operand that is a variable holding a reference:
/// the operation on the dereferenced values (`ZVAL_DEREF` of each
/// operand), for numbers (`+ - * /`, bit operations, comparisons: the
/// sealed numeric results), `.` of strings and ints, and the equality
/// operators of [`equal_values`]. A consumed string temporary operand is
/// released.
#[inline(never)]
fn bin_ref(f: &mut Frame, lits: &[Value], op: BinKind, a: Operand, b: Operand, dst: u32) -> bool {
    if !(is_ref_cv(f, a) || is_ref_cv(f, b)) || !tmp_free(f, dst) {
        return false;
    }
    let Some(x) = plain_operand(f, lits, a) else {
        return false;
    };
    let Some(y) = plain_operand(f, lits, b) else {
        return false;
    };
    let r = if op == BinKind::Arith(BinaryOp::Concat) {
        concat_values(&x, &y).map(Value::Str)
    } else if let (Some(p), Some(q)) = (num(&x), num(&y)) {
        match op {
            BinKind::Arith(o) => compound_num(o, p, q),
            _ => num_bin(op, p, q),
        }
    } else {
        equal_values(op, &x, &y).map(Value::Bool)
    };
    let Some(r) = r else {
        return false;
    };
    drop((x, y));
    consume_str_tmp(f, a);
    consume_str_tmp(f, b);
    if let Some(slot) = f.tmps.get_mut(dst as usize) {
        put_tmp(slot, r);
    }
    true
}

/// The truth of the value a variable's reference holds (`ZEND_JMPZ_SPEC_CV`
/// after `ZVAL_DEREF`, `zend_is_true` without a conversion:
/// [`plain_truth`]).
#[inline(never)]
fn truth_ref(f: &Frame, cv: u32) -> Option<bool> {
    let Some(Value::Reference(r)) = f.cvs.get(cv as usize).and_then(CvSlot::get) else {
        return None;
    };
    plain_truth(&*r.try_borrow()?)
}

/// The jump decision of a smart branch ([`Op::CmpJmp`]) with an operand
/// that is a variable holding a reference: two numbers by the op's
/// comparison mask ([`jump_of`]), other plain values by [`equal_values`]
/// against `jmp_if`. A consumed string temporary operand is released.
#[inline(never)]
fn cmp_jmp_ref(
    f: &mut Frame,
    lits: &[Value],
    op: BinKind,
    a: Operand,
    b: Operand,
    jmp_if: bool,
    mask: u8,
) -> Option<bool> {
    if !(is_ref_cv(f, a) || is_ref_cv(f, b)) {
        return None;
    }
    let x = plain_operand(f, lits, a)?;
    let y = plain_operand(f, lits, b)?;
    let jump = if let (Some(p), Some(q)) = (num(&x), num(&y)) {
        jump_of(p, q, mask)
    } else {
        equal_values(op, &x, &y)? == jmp_if
    };
    drop((x, y));
    consume_str_tmp(f, a);
    consume_str_tmp(f, b);
    Some(jump)
}

/// `count($cv)` (`ZEND_COUNT`) of a variable holding a reference to an
/// array, the result written to `dst` when used.
#[inline(never)]
fn count_ref(f: &mut Frame, cv: u32, dst: Option<u32>) -> bool {
    let n = {
        let Some(Value::Reference(r)) = f.cvs.get(cv as usize).and_then(CvSlot::get) else {
            return false;
        };
        match r.try_borrow().as_deref() {
            Some(Value::Array(a)) => i64::try_from(a.len()).unwrap_or(i64::MAX),
            _ => return false,
        }
    };
    if let Some(t) = dst {
        if let Some(slot) = f.tmps.get_mut(t as usize) {
            put_tmp(slot, Value::Int(n));
        }
    }
    true
}

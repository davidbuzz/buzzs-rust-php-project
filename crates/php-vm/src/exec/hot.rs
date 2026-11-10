//! The frame-local inner dispatch loop: jumps, smart branches, numeric
//! arithmetic, `++`/`--` and plain assignments of compiled variables and
//! temporaries, run on the current frame and the local `ip` alone.
//!
//! php-src's `execute_ex` keeps `execute_data` and `opline` in machine
//! registers (`ZEND_VM_FP_GLOBAL_REG`, `ZEND_VM_IP_GLOBAL_REG`) and its
//! specialised handlers (`ZEND_PRE_INC_LONG_SPEC_CV`,
//! `ZEND_IS_SMALLER_LONG_SPEC_TMPVARCV_CONST_JMPNZ`,
//! `ZEND_ADD_DOUBLE_SPEC_CV_CONST`, `ZEND_ASSIGN_SPEC_CV_CONST_RETVAL_UNUSED`,
//! `ZEND_JMPZ_SPEC_CV`, ...) touch nothing else. `Vm::run` is one large
//! function whose `ip` and frame live on the stack, and every fast path
//! there looks the current frame up again (the frame stack's bounds, the
//! frame's address). [`run_hot`] is entered by `Vm::run` at the first such
//! op and stays here, with the frame, its slot vectors and `ip` in
//! registers, until an op it does not handle, an op whose fast path does
//! not apply (nothing changed: `Vm::run` then runs the generic op), or a
//! due safe point (`EG(vm_interrupt)`: polled after every op, as in
//! `Vm::run`).
//!
//! Every fast path here completes its op without diagnostics, user code or
//! frame changes, exactly as the `Vm::run` pre-match did before: it reads
//! only plain values (no references, no undefined variables), and it
//! overwrites only variable slots that hold no refcounted value other than
//! a string (`is_plain_slot`), so releasing the old value has no side
//! effect. A frame whose symbol table is attached (`Frame::symtab_attached`)
//! has each written variable copied into its `VarStore` slot, out of line
//! ([`sync_cv_cold`]).

use super::spec::{cmp_simple_in, is_plain_slot, num_outcome, plain_truth, put_tmp, slot_truth};
use super::{discard_unrooted, num_bin, Num};
use crate::opcode::{cmp_outcome, BinKind, IncDecKind, Op, Operand};
use crate::vm::{CvSlot, Frame, Vm};
use php_runtime::{BinaryOp, Value};

/// The second half of [`run_hot`]'s result when no safe point is due: the
/// op at the returned `ip` stopped the loop.
pub(super) const NO_SAFE_POINT: usize = usize::MAX;

/// Whether [`run_hot`] handles `op` (its fast path may still decline).
#[inline(always)]
#[allow(clippy::inline_always)] // a discriminant test at the top of the dispatch loop
pub(super) fn is_hot(op: &Op) -> bool {
    matches!(
        op,
        Op::Nop
            | Op::Jmp(_)
            | Op::JmpZ(..)
            | Op::JmpNz(..)
            | Op::AssignCv { .. }
            | Op::Move { .. }
            | Op::Coalesce { .. }
            | Op::JmpSet { .. }
            | Op::JmpNull { .. }
            | Op::CmpJmp { .. }
            | Op::Bin { .. }
            | Op::BinAssignCv { .. }
            | Op::IncDecCv { .. }
            | Op::IncDecCvCmpJmp { .. }
            | Op::BinAssignCvCmpJmp { .. }
            | Op::AssignOpCvCmpJmp { .. }
            | Op::BinAssignCvJmp { .. }
            | Op::AssignOpCvJmp { .. }
            | Op::AssignOpCv { .. }
    )
}

/// Runs the ops of frame `f` from `ip` while they are hot ops whose fast
/// path applies. Returns `(ip, NO_SAFE_POINT)` when the op at `ip` must run
/// in `Vm::run` (not a hot op, or its fast path declined, having changed
/// nothing), or `(ip, at)` when the safe point after the op at `at` is
/// due, `ip` being where execution continues.
///
/// Never inlined: its own register allocation keeps `f`, `ops` and `ip`
/// out of `Vm::run`'s stack frame.
#[inline(never)]
#[allow(clippy::too_many_lines)] // one arm per hot op, like `Vm::run`
pub(super) fn run_hot(f: &mut Frame, ops: &[Op], mut ip: usize) -> (usize, usize) {
    loop {
        let at = ip;
        let Some(op) = ops.get(at) else {
            return (at, NO_SAFE_POINT);
        };
        ip = at + 1;
        match *op {
            Op::Nop => {}
            Op::Jmp(t) => ip = t as usize,
            Op::JmpZ(a, t) => match take_bool(f, a) {
                Some(false) => ip = t as usize,
                Some(true) => {}
                None => return (at, NO_SAFE_POINT),
            },
            Op::JmpNz(a, t) => match take_bool(f, a) {
                Some(true) => ip = t as usize,
                Some(false) => {}
                None => return (at, NO_SAFE_POINT),
            },
            Op::AssignCv { cv, val, dst } => {
                if !assign_cv_in(f, cv, val, dst) {
                    return (at, NO_SAFE_POINT);
                }
            }
            Op::Move { src, dst } => {
                if !move_in(f, src, dst) {
                    return (at, NO_SAFE_POINT);
                }
            }
            Op::Coalesce { a, dst, target } => match coalesce_in(f, a, dst) {
                Some(true) => ip = target as usize,
                Some(false) => {}
                None => return (at, NO_SAFE_POINT),
            },
            Op::JmpSet { a, dst, target } => match jmp_set_in(f, a, dst) {
                Some(true) => ip = target as usize,
                Some(false) => {}
                None => return (at, NO_SAFE_POINT),
            },
            Op::JmpNull { a, dst, target } => match jmp_null_in(f, a, dst) {
                Some(true) => ip = target as usize,
                Some(false) => {}
                None => return (at, NO_SAFE_POINT),
            },
            // Smart branch (`ZEND_IS_SMALLER_LONG_SPEC` +
            // `ZEND_VM_SMART_BRANCH`): jumps, or skips the jump op after
            // it.
            Op::CmpJmp {
                op: kind,
                a,
                b,
                target,
                jmp_if,
                mask,
                ..
            } => {
                let jump = match cmp_num(f, a, b, mask) {
                    Some(j) => j,
                    None => match equality(f, kind, a, b) {
                        Some(r) => r == jmp_if,
                        None => return (at, NO_SAFE_POINT),
                    },
                };
                if jump {
                    ip = target as usize;
                } else {
                    ip += 1;
                }
            }
            Op::Bin {
                op: kind,
                a,
                b,
                dst,
            } => {
                if !bin_tmp(f, kind, a, b, dst) {
                    let Some(r) = equality(f, kind, a, b) else {
                        return (at, NO_SAFE_POINT);
                    };
                    let Some(slot) = f.tmps.get_mut(dst as usize) else {
                        return (at, NO_SAFE_POINT);
                    };
                    put_tmp(slot, Value::Bool(r));
                }
            }
            // Stored into the variable: the `AssignCv` after the op is
            // done; otherwise it runs on `dst`.
            Op::BinAssignCv {
                op: kind,
                a,
                b,
                dst,
                cv,
            } => match bin_assign_cv(f, kind, a, b, cv, dst) {
                Some(true) => ip += 1,
                Some(false) => {}
                None => {
                    let Some(r) = equality(f, kind, a, b) else {
                        return (at, NO_SAFE_POINT);
                    };
                    if store_plain(f, cv, Value::Bool(r)) {
                        ip += 1;
                    } else if let Some(slot) = f.tmps.get_mut(dst as usize) {
                        put_tmp(slot, Value::Bool(r));
                    }
                }
            },
            Op::IncDecCv { cv, kind, dst, .. } => {
                if !incdec_cv(f, cv, kind, dst) {
                    return (at, NO_SAFE_POINT);
                }
            }
            // A loop's step and its condition: after the `++`/`--`, the
            // `CmpJmp` at `ip` on plain numbers jumps or skips itself and
            // its jump op; otherwise it runs as the next op.
            Op::IncDecCvCmpJmp { cv, kind, .. } => {
                if !incdec_cv(f, cv, kind, None) {
                    return (at, NO_SAFE_POINT);
                }
                if let Some(&Op::CmpJmp {
                    a, b, target, mask, ..
                }) = ops.get(ip)
                {
                    match cmp_num(f, a, b, mask) {
                        Some(true) => ip = target as usize,
                        Some(false) => ip += 2,
                        None => {}
                    }
                }
            }
            Op::BinAssignCvCmpJmp {
                op: kind,
                a,
                b,
                dst,
                cv,
            } => match bin_assign_cv(f, kind, a, b, cv, dst) {
                Some(true) => {
                    if let Some(&Op::CmpJmp {
                        a: ca,
                        b: cb,
                        target,
                        mask,
                        ..
                    }) = ops.get(ip + 1)
                    {
                        match cmp_num(f, ca, cb, mask) {
                            Some(true) => ip = target as usize,
                            Some(false) => ip += 3,
                            None => ip += 1,
                        }
                    } else {
                        ip += 1;
                    }
                }
                Some(false) => {}
                None => return (at, NO_SAFE_POINT),
            },
            Op::AssignOpCvCmpJmp {
                cv, op: kind, val, ..
            } => {
                if !assign_op_cv(f, cv, kind, val, None) {
                    return (at, NO_SAFE_POINT);
                }
                if let Some(&Op::CmpJmp {
                    a, b, target, mask, ..
                }) = ops.get(ip)
                {
                    match cmp_num(f, a, b, mask) {
                        Some(true) => ip = target as usize,
                        Some(false) => ip += 2,
                        None => {}
                    }
                }
            }
            Op::BinAssignCvJmp {
                op: kind,
                a,
                b,
                dst,
                cv,
                target,
                jmp_if,
            } => match bin_assign_cv(f, kind, a, b, cv, dst) {
                Some(true) => match f.peek_cv(cv, slot_truth) {
                    Some(cond) if cond == jmp_if => ip = target as usize,
                    Some(_) => ip += 2,
                    None => ip += 1,
                },
                Some(false) => {}
                None => return (at, NO_SAFE_POINT),
            },
            Op::AssignOpCvJmp {
                cv,
                op: kind,
                val,
                target,
                jmp_if,
                ..
            } => {
                if !assign_op_cv(f, cv, kind, val, None) {
                    return (at, NO_SAFE_POINT);
                }
                match f.peek_cv(cv, slot_truth) {
                    Some(cond) if cond == jmp_if => ip = target as usize,
                    Some(_) => ip += 1,
                    None => {}
                }
            }
            Op::AssignOpCv {
                cv,
                op: kind,
                val,
                dst,
                ..
            } => {
                if !assign_op_cv(f, cv, kind, val, dst) {
                    return (at, NO_SAFE_POINT);
                }
            }
            _ => return (at, NO_SAFE_POINT),
        }
        if crate::interrupt::safe_point_due() {
            return (ip, at);
        }
    }
}

/// Copies compiled variable `cv`'s new value into the attached symbol
/// table (`Frame::sync_cv_to_symtab`), out of line: callers test
/// `f.symtab_attached` first, so the hot path builds no `Value` for it.
#[cold]
#[inline(never)]
fn sync_cv_cold(f: &Frame, cv: u32) {
    if let Some(v) = f.cvs.get(cv as usize).and_then(CvSlot::get_clone) {
        f.sync_cv_to_symtab(cv, &v);
    }
}

/// A copy of a plain scalar result (`num_bin` gives ints, floats and
/// bools), without the out-of-line `Value::clone`.
#[inline(always)]
#[allow(clippy::inline_always)]
fn scalar_copy(v: &Value) -> Value {
    match v {
        Value::Int(i) => Value::Int(*i),
        Value::Float(x) => Value::Float(*x),
        Value::Bool(b) => Value::Bool(*b),
        Value::Null => Value::Null,
        other => other.clone(),
    }
}

/// Stores `v` into temporary `t` of `f` ([`put_tmp`]); an out-of-range
/// slot is ignored, as the pre-match did.
#[inline(always)]
#[allow(clippy::inline_always)]
fn set_tmp_in(f: &mut Frame, t: u32, v: Value) {
    if let Some(slot) = f.tmps.get_mut(t as usize) {
        put_tmp(slot, v);
    }
}

/// The boolean of a jump operand without conversion (`Vm::take_tmp_bool`):
/// a temporary holding a bool (consumed), or a compiled variable holding a
/// plain value (tested in place). `None` for the generic `jump_cond`.
#[inline(always)]
#[allow(clippy::inline_always)]
fn take_bool(f: &mut Frame, op: Operand) -> Option<bool> {
    match op {
        Operand::Tmp(t) => {
            let slot = f.tmps.get_mut(t as usize)?;
            if let Value::Bool(b) = *slot {
                // A bool has nothing to drop: overwrite it without the
                // out-of-line `drop_in_place`.
                std::mem::forget(std::mem::take(slot));
                Some(b)
            } else {
                None
            }
        }
        Operand::Cv(cv) => f.peek_cv(cv, slot_truth),
        _ => None,
    }
}

/// The jump decision of a smart branch on two plain numbers
/// (`Vm::cmp_jmp_num`): the comparison's [`cmp_outcome`] bit against the
/// op's mask.
#[inline(always)]
#[allow(clippy::inline_always)]
fn cmp_num(f: &Frame, a: Operand, b: Operand, mask: u8) -> Option<bool> {
    let x = Vm::peek_num(f, a)?;
    let y = Vm::peek_num(f, b)?;
    let identity = (mask & cmp_outcome::IDENTITY) != 0;
    Some((mask & num_outcome(x, y, identity)) != 0)
}

/// `Vm::equality_fast` on frame `f`: `==`, `!=`, `===`, `!==` decided
/// without conversions (out of line); `None` at once for other operators.
#[inline(always)]
#[allow(clippy::inline_always)]
fn equality(f: &mut Frame, op: BinKind, a: Operand, b: Operand) -> Option<bool> {
    if matches!(
        op,
        BinKind::Equal | BinKind::NotEqual | BinKind::Identical | BinKind::NotIdentical
    ) {
        cmp_simple_in(f, op, a, b)
    } else {
        None
    }
}

/// [`Op::Bin`] on two plain numbers, the result written into temporary
/// `dst` (`ZEND_ADD_LONG_SPEC` & co.). `false`, nothing changed, when an
/// operand is not a plain number or the operator has no numeric form.
#[inline(always)]
#[allow(clippy::inline_always)]
fn bin_tmp(f: &mut Frame, op: BinKind, a: Operand, b: Operand, dst: u32) -> bool {
    let Some(x) = Vm::peek_num(f, a) else {
        return false;
    };
    let Some(y) = Vm::peek_num(f, b) else {
        return false;
    };
    let Some(r) = num_bin(op, x, y) else {
        return false;
    };
    let Some(slot) = f.tmps.get_mut(dst as usize) else {
        return false;
    };
    put_tmp(slot, r);
    true
}

/// Stores the scalar `v` into compiled variable `cv` when that is
/// undefined or holds a plain value ([`is_plain_slot`]): `true`. `false`,
/// nothing changed (`v` is a scalar: nothing to give back), otherwise.
#[inline(always)]
#[allow(clippy::inline_always)]
fn store_plain(f: &mut Frame, cv: u32, v: Value) -> bool {
    let Some(slot) = f.cvs.get_mut(cv as usize).map(CvSlot::as_mut) else {
        return false;
    };
    if !is_plain_slot(slot.as_ref()) {
        return false;
    }
    discard_unrooted(slot.replace(v));
    if f.symtab_attached {
        sync_cv_cold(f, cv);
    }
    true
}

/// [`Op::BinAssignCv`] on two plain numbers (`Vm::bin_assign_cv_fast`):
/// the result goes straight into compiled variable `cv` when that holds no
/// refcounted value (`Some(true)`), else into `dst` for the `AssignCv`
/// after the op (`Some(false)`). `None`, nothing changed, as for
/// [`bin_tmp`].
#[inline(always)]
#[allow(clippy::inline_always)]
fn bin_assign_cv(
    f: &mut Frame,
    op: BinKind,
    a: Operand,
    b: Operand,
    cv: u32,
    dst: u32,
) -> Option<bool> {
    let x = Vm::peek_num(f, a)?;
    let y = Vm::peek_num(f, b)?;
    let r = num_bin(op, x, y)?;
    if let Some(slot) = f.cvs.get_mut(cv as usize).map(CvSlot::as_mut) {
        if is_plain_slot(slot.as_ref()) {
            discard_unrooted(slot.replace(r));
            if f.symtab_attached {
                sync_cv_cold(f, cv);
            }
            return Some(true);
        }
    }
    let slot = f.tmps.get_mut(dst as usize)?;
    put_tmp(slot, r);
    Some(false)
}

/// `++`/`--` of compiled variable `cv` holding an int that does not
/// overflow, or a float (`ZEND_PRE_INC_LONG_SPEC_CV` & co.), in place; the
/// expression's value goes to `dst` when used. `false`, nothing changed,
/// for the generic path.
#[inline(always)]
#[allow(clippy::inline_always)]
fn incdec_cv(f: &mut Frame, cv: u32, kind: IncDecKind, dst: Option<u32>) -> bool {
    let Some(slot) = f.cvs.get_mut(cv as usize).map(CvSlot::as_mut) else {
        return false;
    };
    let r = match slot {
        Some(Value::Int(i)) => {
            let old = *i;
            let next = if kind.is_inc() {
                old.checked_add(1)
            } else {
                old.checked_sub(1)
            };
            let Some(new) = next else {
                return false;
            };
            *i = new;
            Value::Int(if kind.is_post() { old } else { new })
        }
        Some(Value::Float(x)) => {
            let old = *x;
            let new = if kind.is_inc() { old + 1.0 } else { old - 1.0 };
            *x = new;
            Value::Float(if kind.is_post() { old } else { new })
        }
        _ => return false,
    };
    if f.symtab_attached {
        sync_cv_cold(f, cv);
    }
    if let Some(t) = dst {
        set_tmp_in(f, t, r);
    } else {
        // An int or a float: nothing to drop.
        std::mem::forget(r);
    }
    true
}

/// `$cv op= val` in place when both hold plain numbers and the result has
/// a numeric form (`ZEND_ASSIGN_OP_SPEC_CV_*` with the `IS_LONG` /
/// `IS_DOUBLE` paths of `zend_binary_op`; `Vm::assign_op_cv_fast`), the
/// result also written to `dst` when used. `false`, nothing changed, for
/// the generic `AssignOp`.
#[inline(always)]
#[allow(clippy::inline_always)]
fn assign_op_cv(f: &mut Frame, cv: u32, op: BinaryOp, val: Operand, dst: Option<u32>) -> bool {
    // The operand first: it may be this very variable (`$x += $x`).
    let Some(y) = Vm::peek_num(f, val) else {
        return false;
    };
    let Some(slot) = f.cvs.get_mut(cv as usize).map(CvSlot::as_mut) else {
        return false;
    };
    let x = match slot {
        Some(Value::Int(i)) => Num::Int(*i),
        Some(Value::Float(v)) => Num::Float(*v),
        _ => return false,
    };
    let Some(r) = num_bin(BinKind::Arith(op), x, y) else {
        return false;
    };
    let keep = dst.map(|_| scalar_copy(&r));
    // The old value is an int or a float: nothing to release.
    discard_unrooted(slot.replace(r));
    if f.symtab_attached {
        sync_cv_cold(f, cv);
    }
    if let (Some(t), Some(k)) = (dst, keep) {
        set_tmp_in(f, t, k);
    }
    true
}

/// `$cv = val` in place (`Vm::assign_cv_fast`): `val` is a literal, a
/// temporary or a defined compiled variable, not a reference (a
/// variable's reference is dereferenced and copied, `ZVAL_COPY_DEREF`),
/// and the variable is undefined or holds no reference, array or object.
/// `false`, nothing changed, for the generic assignment.
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) fn assign_cv_in(f: &mut Frame, cv: u32, val: Operand, dst: Option<u32>) -> bool {
    match f.cvs.get(cv as usize).map(CvSlot::get) {
        Some(Some(Value::Reference(_) | Value::Array(_) | Value::Object(_))) | None => {
            return false;
        }
        Some(_) => {}
    }
    let v = match val {
        // Consumed, as `read` does; a reference held by the temporary is
        // left to the generic path (it derefs and releases it).
        Operand::Tmp(t) => match f.tmps.get_mut(t as usize) {
            Some(s) if !matches!(s, Value::Reference(_)) => std::mem::take(s),
            _ => return false,
        },
        Operand::TmpKeep(t) => match f.tmps.get(t as usize) {
            Some(s) if !matches!(s, Value::Reference(_)) => s.clone(),
            _ => return false,
        },
        Operand::Lit(i) => match f.code.literals.get(i as usize) {
            Some(l) if !matches!(l, Value::Reference(_)) => l.clone(),
            _ => return false,
        },
        Operand::Cv(src) => match f.cvs.get(src as usize).and_then(CvSlot::get) {
            Some(Value::Reference(r)) => r.get(),
            Some(s) => s.clone(),
            None => return false,
        },
        Operand::Unused => return false,
    };
    let keep = dst.map(|_| v.clone());
    let Some(slot) = f.cvs.get_mut(cv as usize).map(CvSlot::as_mut) else {
        return false;
    };
    discard_unrooted(slot.replace(v));
    if f.symtab_attached {
        sync_cv_cold(f, cv);
    }
    if let (Some(d), Some(k)) = (dst, keep) {
        set_tmp_in(f, d, k);
    }
    true
}

/// [`Op::Move`] when its read needs no diagnostic (`Vm::move_fast`): a
/// literal, a defined compiled variable (dereferenced and cloned), a
/// temporary not holding a reference (moved, or cloned when kept).
#[inline(always)]
#[allow(clippy::inline_always)]
fn move_in(f: &mut Frame, src: Operand, dst: u32) -> bool {
    let v = match src {
        Operand::Lit(i) => match f.code.literals.get(i as usize) {
            Some(v) if !matches!(v, Value::Reference(_)) => v.clone(),
            _ => return false,
        },
        Operand::Cv(cv) => {
            let v = f.peek_cv(cv, |slot| match slot {
                Some(Value::Reference(r)) => Some(r.get()),
                Some(v) => Some(v.clone()),
                None => None,
            });
            let Some(v) = v else {
                return false;
            };
            v
        }
        Operand::Tmp(t) => match f.tmps.get_mut(t as usize) {
            Some(slot) if !matches!(slot, Value::Reference(_)) => std::mem::take(slot),
            _ => return false,
        },
        Operand::TmpKeep(t) => match f.tmps.get(t as usize) {
            Some(slot) if !matches!(slot, Value::Reference(_)) => slot.clone(),
            _ => return false,
        },
        Operand::Unused => Value::Null,
    };
    put_tmp(&mut f.tmps[dst as usize], v);
    true
}

/// `??` on a compiled variable (`ZEND_COALESCE_SPEC_CV`), which never
/// warns: `Some(true)` (jump) with the dereferenced value cloned into
/// `dst` when it is not null, `Some(false)` when undefined or null. `None`
/// for any other operand.
#[inline(always)]
#[allow(clippy::inline_always)]
fn coalesce_in(f: &mut Frame, a: Operand, dst: u32) -> Option<bool> {
    let Operand::Cv(cv) = a else {
        return None;
    };
    let v = f.peek_cv(cv, |slot| match slot {
        None | Some(Value::Null) => None,
        Some(Value::Reference(r)) => {
            let inner = r.get();
            (!matches!(inner, Value::Null)).then_some(inner)
        }
        Some(v) => Some(v.clone()),
    });
    let Some(v) = v else {
        return Some(false);
    };
    put_tmp(&mut f.tmps[dst as usize], v);
    Some(true)
}

/// `?:` on a defined compiled variable whose truth needs no conversion
/// (`ZEND_JMP_SET_SPEC_CV`): `Some(true)` (jump) with the value cloned
/// into `dst` when truthy, `Some(false)` when falsy. `None` otherwise.
#[inline(always)]
#[allow(clippy::inline_always)]
fn jmp_set_in(f: &mut Frame, a: Operand, dst: u32) -> Option<bool> {
    let Operand::Cv(cv) = a else {
        return None;
    };
    let found = f.peek_cv(cv, |slot| {
        let v = slot?;
        Some(plain_truth(v)?.then(|| v.clone()))
    })?;
    let Some(v) = found else {
        return Some(false);
    };
    put_tmp(&mut f.tmps[dst as usize], v);
    Some(true)
}

/// `?->`'s null test on a defined compiled variable
/// (`ZEND_JMP_NULL_SPEC_CV`): `Some(true)` (jump, `dst` null) when it holds
/// null, directly or through a reference, `Some(false)` otherwise. `None`
/// for an undefined variable and any other operand.
#[inline(always)]
#[allow(clippy::inline_always)]
fn jmp_null_in(f: &mut Frame, a: Operand, dst: u32) -> Option<bool> {
    let Operand::Cv(cv) = a else {
        return None;
    };
    let null = f.peek_cv(cv, |slot| match slot? {
        Value::Null => Some(true),
        Value::Reference(r) => Some(matches!(&*r.try_borrow()?, Value::Null)),
        _ => Some(false),
    })?;
    if null {
        put_tmp(&mut f.tmps[dst as usize], Value::Null);
    }
    Some(null)
}

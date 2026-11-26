// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Tape instructions for object properties, `$this::NAME` and `++`/`--`
//! of a static property.
//!
//! See the "Families" section of [`super`] for the contract of a handler
//! and of [`lower`].
//!
//! ref: `Zend/zend_vm_execute.h` property handlers, through
//! [`crate::exec::spec::prop_op_in`].
//!
//! A read carries its place, temporary and folded variable, and runs the
//! cache hit `prop_op_in` runs for it (`read_hit`, then
//! `store_fetched_in`). Any other property instruction carries the op
//! index in `d`; its handler borrows the op from its own handle on the
//! frame's `Rc<OpArray>` and runs `prop_op_in` on it — the exact entry
//! point `hot::step` uses, so the family cannot diverge from the generic
//! path. `prop_op_in` covers the prop cache, typed-property checks and
//! `__get`/`__set` (it declines when object handlers must run). Every
//! handler may release a value (an old property value, a consumed source)
//! or allocate, so all rows poll; the read into a folded `AssignCv`'s
//! variable also branches past that op.
//! `new` is a call instruction ([`super::calls`]): it prepares the
//! constructor's frame.

use super::{Br, Inst, TOp};
use crate::exec::spec::{
    assign_prop_dim_in, fetch_prop_dim_lean, fetch_prop_dim_rest, fetch_prop_lean,
    incdec_static_prop_in, isset_prop_dim_in, prop_op_in, read_hit, store_fetched_in,
};
use crate::opcode::{ClassSpec, FetchMode, Op, OpArray, Operand, PlaceBase};
use crate::vm::Frame;
use std::rc::Rc;

/// Whether `place` is a slot of `code`'s place table (the prop cache is
/// keyed by place; an out-of-range place is a safe miss in `prop_op_in`,
/// but the family only lowers validated slots).
fn place_ok(code: &OpArray, place: u32) -> bool {
    (place as usize) < code.places.len()
}

/// Whether a value operand is in range of `code`'s operand tables.
fn val_ok(code: &OpArray, o: Operand) -> bool {
    match o {
        Operand::Cv(i) => (i as usize) < code.cv_names.len(),
        Operand::Tmp(t) | Operand::TmpKeep(t) => t < code.num_tmps,
        Operand::Lit(l) => (l as usize) < code.literals.len(),
        Operand::Unused => true,
    }
}

/// The specialised instruction of the op at `i` when this family has one.
/// Only forms `prop_op_in` can complete on a cache hit are lowered:
/// single-op reads (`to` absent), validated places and operands, and
/// `new` of a statically named class.
#[allow(clippy::too_many_lines, clippy::cast_possible_truncation)]
pub(super) fn lower(code: &OpArray, i: usize) -> Option<Inst> {
    let plain = Inst::plain;
    Some(match *code.ops.get(i)? {
        // 1-op read: the result goes to `dst` directly.
        // `store_fetched_in` indexes `f.tmps[dst]` without a bounds check,
        // so validate `dst` (frames are sized to `num_tmps` by `usable`).
        Op::FetchObjProp {
            place,
            dst,
            to,
            mode,
        } => {
            if mode == FetchMode::List || !place_ok(code, place) || dst >= code.num_tmps {
                return None;
            }
            match to {
                None => Inst {
                    a: place,
                    b: dst,
                    ..plain(TOp::FetchObjProp)
                },
                // `$cv = $obj->prop`: the folded `AssignCv` after the read
                // (`c` jumps past it when the value went into the
                // variable `d`).
                Some(cv) => {
                    if (cv as usize) >= code.cv_names.len() {
                        return None;
                    }
                    Inst {
                        a: place,
                        b: dst,
                        c: super::rel(code, i, u32::try_from(i + 2).ok()?)?,
                        d: cv,
                        ..plain(TOp::FetchObjPropTo)
                    }
                }
            }
        }
        // `$obj->prop[dim]` read, pre-decoded: `a` the place, `b` the
        // temporary (low half) and the folded `AssignCv`'s variable plus
        // one (high half; 0: none), `d` and `e` the offset operand
        // ([`dim_operand`]), `k` whether the read is silent (`??`), `c`
        // the jump past a folded `AssignCv` whose variable took the value.
        // `d` of the cold path's op is recovered from the tape position.
        Op::FetchObjPropDim {
            place,
            dim,
            dst,
            to,
            mode,
        } => {
            if mode == FetchMode::List
                || !place_ok(code, place)
                || !val_ok(code, dim)
                || dst >= code.num_tmps
                || dst > 0xffff
                || to.is_some_and(|cv| (cv as usize) >= code.cv_names.len() || cv >= 0xffff)
            {
                return None;
            }
            let (e, d) = match dim {
                Operand::Cv(i) => (0, i),
                Operand::Tmp(t) => (1, t),
                Operand::Lit(l) => (2, l),
                Operand::TmpKeep(t) => (3, t),
                Operand::Unused => return None,
            };
            let c = match to {
                Some(_) => super::rel(code, i, u32::try_from(i + 2).ok()?)?,
                None => 0,
            };
            Inst {
                a: place,
                b: dst | (to.map_or(0, |cv| cv + 1) << 16),
                c,
                d,
                e,
                k: u8::from(mode == FetchMode::Is),
                ..plain(TOp::FetchObjPropDim)
            }
        }
        // `$obj->prop[dim] = val` / `$obj->prop[] = val` and
        // `isset($obj->prop[dim])`: the op is read back from the op array
        // (`d`).
        Op::AssignObjPropDim {
            place,
            dim,
            val,
            dst,
        } => {
            if !place_ok(code, place)
                || !val_ok(code, dim)
                || !val_ok(code, val)
                || dst.is_some_and(|d| d >= code.num_tmps)
            {
                return None;
            }
            Inst {
                a: place,
                d: i as u32,
                ..plain(TOp::AssignObjPropDim)
            }
        }
        Op::IssetObjPropDim {
            place, dim, dst, ..
        } => {
            if !place_ok(code, place) || !val_ok(code, dim) || dst >= code.num_tmps {
                return None;
            }
            Inst {
                a: place,
                d: i as u32,
                ..plain(TOp::IssetObjPropDim)
            }
        }
        Op::AssignObjProp { place, val, .. } => {
            if !place_ok(code, place) || !val_ok(code, val) {
                return None;
            }
            Inst {
                a: place,
                d: i as u32,
                ..plain(TOp::AssignObjProp)
            }
        }
        // `.=` (`spec::concat_prop_in`): the number cases' row cannot take
        // it.
        Op::AssignOpObjProp {
            place,
            op: php_runtime::BinaryOp::Concat,
            val,
            dst,
        } => {
            if !place_ok(code, place)
                || !val_ok(code, val)
                || dst.is_some_and(|d| d >= code.num_tmps)
            {
                return None;
            }
            Inst {
                a: place,
                d: i as u32,
                ..plain(TOp::ConcatObjProp)
            }
        }
        Op::AssignOpObjProp { place, val, .. } => {
            if !place_ok(code, place) || !val_ok(code, val) {
                return None;
            }
            Inst {
                a: place,
                d: i as u32,
                ..plain(TOp::AssignOpObjProp)
            }
        }
        Op::IncDecObjProp { place, .. } => {
            if !place_ok(code, place) {
                return None;
            }
            Inst {
                a: place,
                d: i as u32,
                ..plain(TOp::IncDecObjProp)
            }
        }
        Op::IssetObjProp { place, .. } => {
            if !place_ok(code, place) {
                return None;
            }
            Inst {
                a: place,
                d: i as u32,
                ..plain(TOp::IssetObjProp)
            }
        }
        // `Class::$name++` and its kin (`ZEND_PRE_INC_STATIC_PROP` & co.):
        // the generic `IncDec` of a place that is a static property and
        // nothing more.
        Op::IncDec { place, dst, .. } => {
            let p = code.places.get(place as usize)?;
            if !matches!(p.base, PlaceBase::StaticProp { .. }) || !p.segs.is_empty() {
                return None;
            }
            if dst.is_some_and(|d| d >= code.num_tmps) {
                return None;
            }
            Inst {
                a: place,
                d: i as u32,
                ..plain(TOp::IncDecStatic)
            }
        }
        // `$this::NAME`: the `FetchThis` and the class-constant read of
        // its temporary (`p`) right after it, as one instruction that
        // reads the frame's `$this` where it is. The constant's name
        // literal is in `a`, the result temporary in `b`.
        Op::FetchThis { dst: this_tmp } => {
            let (name, dst, to) = match *code.ops.get(i + 1)? {
                Op::FetchClassConst {
                    class: ClassSpec::Dyn(Operand::Tmp(t)),
                    name: Operand::Lit(name),
                    dst,
                } if t == this_tmp => (name, dst, None),
                Op::FetchClassConstTo {
                    class: ClassSpec::Dyn(Operand::Tmp(t)),
                    name,
                    dst,
                    to,
                } if t == this_tmp => (name, dst, Some(to)),
                // `$this` as a value (`return $this;`, `f($this)`,
                // `[$this, 'm']`): the object into its temporary.
                _ if this_tmp < code.num_tmps => {
                    return Some(Inst {
                        b: this_tmp,
                        ..plain(TOp::FetchThisT)
                    })
                }
                _ => return None,
            };
            if dst >= code.num_tmps || this_tmp >= code.num_tmps {
                return None;
            }
            match to {
                None => Inst {
                    a: name,
                    b: dst,
                    p: this_tmp as usize,
                    ..plain(TOp::ThisConst)
                },
                // With the folded `AssignCv` after the two ops (`c` jumps
                // past it when the value went into the variable `d`).
                Some(cv) => {
                    if (cv as usize) >= code.cv_names.len() {
                        return None;
                    }
                    Inst {
                        a: name,
                        b: dst,
                        c: super::rel(code, i, u32::try_from(i + 3).ok()?)?,
                        d: cv,
                        p: this_tmp as usize,
                        ..plain(TOp::ThisConstTo)
                    }
                }
            }
        }
        _ => return None,
    })
}

/// `$o->p .= v` ([`TOp::ConcatObjProp`], the op at `inst.d`) on a property
/// cache hit (`spec::concat_prop_in`). Declines anything else, having
/// changed nothing: `hot::step_ext`, then the generic op.
///
/// # Safety
/// As [`prop_in`].
#[inline(never)]
pub(super) unsafe fn concat_prop(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    // A handle of its own on the op array, as in `prop_in`.
    let code = Rc::clone(&f.code);
    let Some(&Op::AssignOpObjProp {
        place, val, dst, ..
    }) = code.ops.get(inst.d as usize)
    else {
        return false;
    };
    crate::exec::spec::concat_prop_in(f, place, val, dst)
}

/// `ZEND_FETCH_THIS` ([`TOp::FetchThisT`]): the frame's `$this` into
/// temporary `inst.b` (one more reference to the object). Declines without
/// `$this`, having changed nothing: the generic op throws "Using $this
/// when not in object context".
///
/// # Safety
/// As [`prop_in`].
#[inline(never)]
pub(super) unsafe fn fetch_this(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    let Some(this) = &f.this else {
        return false;
    };
    let v = php_runtime::Value::Object(this.clone());
    crate::exec::hot::set_tmp_in(f, inst.b, v);
    true
}

/// `++`/`--` of a static property ([`TOp::IncDecStatic`], the op at
/// `inst.d`) on a cache hit (`spec::incdec_static_prop_in`): an untyped
/// property holding an int or a float. Declines anything else, having
/// changed nothing: the generic `IncDec` walks the place.
///
/// # Safety
/// As [`prop_in`].
#[inline(never)]
pub(super) unsafe fn incdec_static(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    // A handle of its own on the op array, as in `prop_in`.
    let code = Rc::clone(&f.code);
    let Some(&Op::IncDec { place, kind, dst }) = code.ops.get(inst.d as usize) else {
        return false;
    };
    incdec_static_prop_in(f, place, kind, dst)
}

/// The value of `$this::NAME` for a [`TOp::ThisConst`] instruction on a
/// hit of the constant's cache for the class of the frame's `$this`
/// (`rtcache::class_const_hit_obj_in`). `None` (the two ops then run one
/// after the other) without a `$this`, on a miss, and when the temporary
/// the ops pass the object through holds a value the `FetchThis` would
/// release.
fn this_const_value(f: &Frame, inst: &Inst) -> Option<php_runtime::Value> {
    if !matches!(
        f.tmps.get(inst.p)?,
        php_runtime::Value::Null
            | php_runtime::Value::Bool(_)
            | php_runtime::Value::Int(_)
            | php_runtime::Value::Float(_)
    ) {
        return None;
    }
    crate::rtcache::class_const_hit_obj_in(f, f.this.as_ref()?, inst.a)
}

/// `$this::NAME` into its temporary `b` ([`this_const_value`]).
///
/// # Safety
/// As [`prop_in`]; `b` is a temporary of the frame.
#[inline(never)]
pub(super) unsafe fn this_const(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    let Some(v) = this_const_value(f, inst) else {
        return false;
    };
    store_fetched_in(f, inst.b, None, v);
    true
}

/// `$cv = $this::NAME`, as [`this_const`]: into the variable `d` of the
/// folded `AssignCv` when that holds nothing to release (the jump past
/// it), else into the temporary `b` (falling through to the `AssignCv`).
///
/// # Safety
/// As [`this_const`]; `d` is a variable of the frame.
#[inline(never)]
pub(super) unsafe fn this_const_to(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    let Some(v) = this_const_value(f, inst) else {
        return Br::Decline;
    };
    if store_fetched_in(f, inst.b, Some(inst.d), v) {
        Br::Jump
    } else {
        Br::Fall
    }
}

/// The handler of the property rows but the reads and `.=`: a dispatch on
/// the row's op and nothing else, so that it keeps no frame of its own and
/// each arm is a tail call. `$o->p = v` and `$o->p++` run in handlers of
/// their own (their hit paths in a small frame each); the others run
/// `prop_op_in` on their op ([`prop_other`]).
///
/// # Safety
/// `inst` is an instruction [`lower`] built for `frame`, the current frame.
#[inline(never)]
pub(super) unsafe fn prop_in(frame: *mut Frame, inst: &Inst) -> bool {
    match inst.op {
        // SAFETY: the caller's contract, for each arm.
        TOp::AssignObjProp => unsafe { assign_prop(frame, inst) },
        TOp::IncDecObjProp => unsafe { incdec_prop(frame, inst) },
        _ => unsafe { prop_other(frame, inst) },
    }
}

/// Runs `prop_op_in` on the op at `inst.d` (`op=` of a number,
/// `isset()`/`empty()`).
///
/// # Safety
/// As [`prop_in`].
#[inline(never)]
unsafe fn prop_other(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    // A handle of its own on the op array: the op is borrowed from it while
    // the frame is borrowed mutably (no copy of the op).
    let code = Rc::clone(&f.code);
    let i = inst.d as usize;
    let Some(op) = code.ops.get(i) else {
        return false;
    };
    let mut ip = i;
    prop_op_in(f, op, &mut ip)
}

/// `Op::AssignObjProp` (`ZEND_ASSIGN_OBJ` with a plain `OP_DATA` on a
/// cache hit): the op's operands are copied out of the op array (they are
/// `Copy`: no handle on it, no op built or dropped) and the hit is
/// `prop_op_in`'s arm for the op (`Frame::prop_assign_hit`), its plainest
/// case inline. Declines what that arm declines, having changed nothing.
///
/// # Safety
/// As [`prop_in`]; `inst.d` is the index of an `Op::AssignObjProp`.
#[inline(never)]
unsafe fn assign_prop(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    let Some(&Op::AssignObjProp { place, val, dst }) = f.code.ops.get(inst.d as usize) else {
        return false;
    };
    f.prop_assign_hit(place, val, dst)
}

/// `Op::IncDecObjProp` (`ZEND_PRE_INC_OBJ` & co. on a cache hit), as
/// [`assign_prop`]: `prop_op_in`'s arm for the op
/// (`Frame::prop_incdec_hit`) on operands copied out of the op array.
///
/// # Safety
/// As [`prop_in`]; `inst.d` is the index of an `Op::IncDecObjProp`.
#[inline(never)]
unsafe fn incdec_prop(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    let Some(&Op::IncDecObjProp { place, kind, dst }) = f.code.ops.get(inst.d as usize) else {
        return false;
    };
    f.prop_incdec_hit(place, kind, dst)
}

/// `Op::FetchObjProp` (`ZEND_FETCH_OBJ_R` / `_IS` on a cache hit) into
/// its temporary `b`: `fetch_prop_lean`, the hit `prop_op_in` takes, without
/// going through the op. Declines a miss (the generic op: magic, a
/// lazy object, an uninitialized or undeclared property).
///
/// # Safety
/// As [`prop_in`]; `a` is the op's place and `b` its temporary.
#[inline(never)]
pub(super) unsafe fn fetch_prop(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    if fetch_prop_lean(f, inst.a, inst.b, None).is_some() {
        return true;
    }
    fetch_prop_cold(f, inst)
}

/// The hits [`fetch_prop`]'s lean form leaves (a dynamic property, a
/// reference in the slot or the base, a temporary holding a value to
/// release), out of line.
#[inline(never)]
fn fetch_prop_cold(f: &mut Frame, inst: &Inst) -> bool {
    // (The plain read was just tried: the general hit.)
    let Some(v) = read_hit(f, inst.a) else {
        return false;
    };
    store_fetched_in(f, inst.b, None, v);
    true
}

/// `Op::AssignObjPropDim` (`ZEND_FETCH_OBJ_W` + `ZEND_ASSIGN_DIM` on a
/// property cache hit): the store into the array a declared property
/// holds, in the frame alone (`assign_prop_dim_in`), where it ran through
/// the VM (`run_vm_op`) before: a parser's stack writes
/// (`$this->semStack[$pos] = $v`). Declines a miss.
///
/// # Safety
/// As [`prop_in`]; `inst.d` is the index of an `Op::AssignObjPropDim`.
#[inline(never)]
pub(super) unsafe fn assign_prop_dim(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    let Some(&Op::AssignObjPropDim {
        place,
        dim,
        val,
        dst,
    }) = f.code.ops.get(inst.d as usize)
    else {
        return false;
    };
    assign_prop_dim_in(f, place, dim, val, dst)
}

/// `Op::IssetObjPropDim` on a property cache hit, as [`assign_prop_dim`]
/// (`isset_prop_dim_in`).
///
/// # Safety
/// As [`prop_in`]; `inst.d` is the index of an `Op::IssetObjPropDim`.
#[inline(never)]
pub(super) unsafe fn isset_prop_dim(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    let Some(&Op::IssetObjPropDim {
        place,
        dim,
        dst,
        empty,
    }) = f.code.ops.get(inst.d as usize)
    else {
        return false;
    };
    isset_prop_dim_in(f, place, dim, dst, empty)
}

/// The offset operand a `FetchObjPropDim` instruction carries in `d`
/// (index) and `e` (kind), as `lower` wrote them.
#[inline(always)]
#[allow(clippy::inline_always)] // a three-way select in each handler
fn dim_operand(inst: &Inst) -> Operand {
    match inst.e {
        0 => Operand::Cv(inst.d),
        1 => Operand::Tmp(inst.d),
        2 => Operand::Lit(inst.d),
        _ => Operand::TmpKeep(inst.d),
    }
}

/// `Op::FetchObjPropDim` (`ZEND_FETCH_OBJ_R` then `ZEND_FETCH_DIM_R`, or
/// the `_IS` pair, on a property cache hit): the element of the array a
/// declared property holds, into the read's temporary or the variable of
/// the `AssignCv` folded behind it (then the jump past that op), in the
/// frame alone (`fetch_prop_dim_lean`: the element copied straight from
/// the array into its destination). A parser's table reads
/// (`$this->action[$idx]`) ran through the VM (`run_vm_op`) before. The
/// other hits are [`fetch_prop_dim_cold`]'s; a miss declines, nothing
/// changed.
///
/// # Safety
/// As [`prop_in`]; the operands are those `lower` validated.
#[inline(never)]
pub(super) unsafe fn fetch_prop_dim(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    let to = (inst.b >> 16).checked_sub(1);
    match fetch_prop_dim_lean(f, inst.a, dim_operand(inst), inst.b & 0xffff, to) {
        Some(true) => Br::Jump,
        Some(false) => Br::Fall,
        // SAFETY: the caller's contract.
        None => unsafe { fetch_prop_dim_cold(frame, inst) },
    }
}

/// The hits [`fetch_prop_dim`]'s lean form leaves (`fetch_prop_dim_rest`),
/// out of line so that its operands are not kept alive across the lean
/// read.
///
/// # Safety
/// As [`fetch_prop_dim`].
#[cold]
#[inline(never)]
unsafe fn fetch_prop_dim_cold(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    let to = (inst.b >> 16).checked_sub(1);
    let mode = if inst.k == 0 {
        FetchMode::Read
    } else {
        FetchMode::Is
    };
    match fetch_prop_dim_rest(f, inst.a, dim_operand(inst), inst.b & 0xffff, to, mode) {
        Some(true) => Br::Jump,
        Some(false) => Br::Fall,
        None => Br::Decline,
    }
}

/// `Op::FetchObjProp` into its folded `AssignCv`'s variable `d` (`$cv =
/// $obj->prop`), as [`fetch_prop`]: the value goes into the variable when
/// that holds nothing to release (`store_fetched_in`; the jump past the
/// `AssignCv`), else into the read's temporary `b` (falling through to the
/// `AssignCv`, which stores it).
///
/// # Safety
/// As [`fetch_prop`]; `d` is a variable of the frame.
#[inline(never)]
pub(super) unsafe fn fetch_prop_to(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    match fetch_prop_lean(f, inst.a, inst.b, Some(inst.d)) {
        Some(true) => Br::Jump,
        Some(false) => Br::Fall,
        None => fetch_prop_to_cold(f, inst),
    }
}

/// The hits [`fetch_prop_to`]'s lean form leaves, as [`fetch_prop_cold`].
#[inline(never)]
fn fetch_prop_to_cold(f: &mut Frame, inst: &Inst) -> Br {
    let Some(v) = read_hit(f, inst.a) else {
        return Br::Decline;
    };
    if store_fetched_in(f, inst.b, Some(inst.d), v) {
        Br::Jump
    } else {
        Br::Fall
    }
}

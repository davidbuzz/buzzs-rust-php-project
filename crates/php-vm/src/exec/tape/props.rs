//! Tape instructions for object properties and `new`.
//!
//! See the "Families" section of [`super`] for the contract of a handler
//! and of [`lower`].
//!
//! ref: `Zend/zend_vm_execute.h` property handlers, through
//! [`crate::exec::spec::prop_op_in`].
//!
//! A property instruction carries the op index in `d`; the handler clones
//! the op from the frame's `Rc<OpArray>` and runs `prop_op_in` on it — the
//! exact entry point `hot::step` uses, so the family cannot diverge from
//! the generic path. `prop_op_in` covers the prop cache, typed-property
//! checks and `__get`/`__set` (it declines when object handlers must run).
//! Every handler may release a value (an old property value, a consumed
//! source) or allocate, so all rows are `poll`. `new` is a call
//! instruction ([`super::calls`]): it prepares the constructor's frame.

use super::{Inst, St, TOp};
use crate::exec::spec::prop_op_in;
use crate::opcode::{FetchMode, Op, OpArray, Operand};

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
            if to.is_some()
                || mode == FetchMode::List
                || !place_ok(code, place)
                || dst >= code.num_tmps
            {
                return None;
            }
            Inst {
                a: place,
                d: i as u32,
                ..plain(TOp::FetchObjProp)
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
        _ => return None,
    })
}

/// Runs `prop_op_in` on the op at `inst.d`. The op is cloned from the
/// shared `Rc<OpArray>` so the `&Op` borrow does not alias `&mut f`.
///
/// # Safety
/// `inst` is an instruction [`lower`] built for the frame of `st`.
#[inline(never)]
pub(super) unsafe fn prop_in(st: St, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `st.f` is the current frame.
    let f = unsafe { &mut *st.f };
    let code = f.code.clone();
    let i = inst.d as usize;
    let op = code.ops[i].clone();
    let mut ip = i;
    prop_op_in(f, &op, &mut ip)
}

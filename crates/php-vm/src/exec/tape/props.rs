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
//! `New` instead encodes its operands in the instruction (`a` the
//! destination slot, `b` the class literal, `c` the ctor-skip jump, `e`
//! whether a `skip` target exists) and runs the class-cache hit and
//! `props::new_plain` / `props::new_with_ctor` directly, without the
//! per-execution op clone. Every handler may release a value (an old
//! property value, a consumed source) or allocate, so all rows are `poll`
//! (`branch_poll` for `New`).

use super::{rel, tmp_off, tmp_slot, Br, Inst, St, TOp};
use crate::exec::spec::{prop_op_in, put_tmp};
use crate::opcode::{ClassSpec, FetchMode, Op, OpArray, Operand};
use php_runtime::Value;

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
        // `new Named(...)`: the ctor path falls through to the following
        // `InitFCall` (the calls family / numeric path runs it); the
        // no-ctor path jumps over it via `c`. `a` is the destination
        // temporary index, `b` the class literal index (both validated
        // against the frame's tables and the op array's literals), and
        // `e` records whether a `skip` target exists (`skip != 0`): only
        // then can the no-constructor path jump; with arguments
        // (`skip == 0`) the generic op starts the placeholder call that
        // evaluates them, so that path declines.
        Op::New {
            class: ClassSpec::Named(lit),
            dst,
            skip,
        } => {
            let c = rel(code, i, skip)?;
            let a = tmp_off(code, dst)?;
            if (lit as usize) >= code.literals.len() {
                return None;
            }
            Inst {
                a,
                b: lit,
                e: u8::from(skip != 0),
                c,
                d: i as u32,
                ..plain(TOp::New)
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
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn prop_in(st: St, inst: &Inst) -> bool {
    // SAFETY: the caller's contract; `st.f` is the current frame.
    let f = unsafe { &mut *st.f };
    let code = f.code.clone();
    let i = inst.d as usize;
    let op = code.ops[i].clone();
    let mut ip = i;
    prop_op_in(f, &op, &mut ip)
}

/// `New` of a statically named class: the `exec::spec::props` fast arm
/// inlined on the encoded operands — the run-time class cache hit, then
/// [`crate::props::new_plain`] (no constructor: jump over the ctor call)
/// or [`crate::props::new_with_ctor`] (constructor call on it: the
/// following `InitFCall` runs it as the fall-through). Every decline
/// changes nothing; the generic op (through `hot::step`) resolves the
/// class, starts the argument-evaluation placeholder call, or refuses the
/// class as such.
///
/// # Safety
/// As [`prop_in`].
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) unsafe fn new_prop(st: St, inst: &Inst) -> Br {
    // SAFETY: the caller's contract; `st.f` is the current frame.
    let f = unsafe { &mut *st.f };
    // `CACHED_PTR`: the generic op's `Vm::resolve_class_spec` fills the
    // cache; a miss (or a borrowed RefCell) declines to it.
    let Some(cls) = f
        .code
        .class_cache
        .try_borrow()
        .ok()
        .and_then(|cache| cache.get(inst.b as usize).and_then(|w| w.upgrade()))
    else {
        return Br::Decline;
    };
    if cls.magic.construct.is_none() {
        // Without a `skip` target (arguments being evaluated), the
        // generic op starts the placeholder call that evaluates them.
        if inst.e == 0 {
            return Br::Decline;
        }
        let Some(o) = crate::props::new_plain(&cls) else {
            return Br::Decline;
        };
        // SAFETY: `inst.a` is `dst`'s slot, validated against
        // `num_tmps` at lowering; the frame is sized to it (`usable`).
        put_tmp(unsafe { tmp_slot(st, inst.a) }, Value::Object(o));
        Br::Jump
    } else {
        let Some(o) = crate::props::new_with_ctor(cls, &mut f.calls) else {
            return Br::Decline;
        };
        // SAFETY: as above.
        put_tmp(unsafe { tmp_slot(st, inst.a) }, Value::Object(o));
        Br::Fall
    }
}

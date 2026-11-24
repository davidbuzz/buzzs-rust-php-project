// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Tape instructions for calls of user functions: php-src's call protocol.
//!
//! php-src prepares a call's frame when the call starts: `ZEND_INIT_FCALL`
//! reserves the callee's `zend_execute_data` on the VM stack, each
//! `ZEND_SEND_VAL` / `ZEND_SEND_VAR` writes its argument straight into the
//! callee's compiled-variable slot, and `ZEND_DO_UCALL` only links the frame
//! in and switches to it (ref: `Zend/zend_vm_def.h` `ZEND_INIT_FCALL`,
//! `ZEND_SEND_VAL`, `ZEND_DO_UCALL`; `Zend/zend_execute.h`
//! `zend_vm_stack_push_call_frame`).
//!
//! The generic protocol here builds a [`PendingCall`] record at
//! `InitFCall`, stages the arguments in it at `Send`, and at `DoCall`
//! re-initialises a pooled frame and moves the arguments across
//! (`Vm::push_simple_ucall`). The tape does what php-src does for the
//! calls that path accepts: a statically named user function, found in
//! the call site's run-time cache, whose declaration and arguments make a
//! plain call (`UserFunction::call_shape`). [`prepare`] readies the pooled
//! frame above the current one for the function at `InitFCall`, [`send`]
//! writes each argument into that frame's compiled variable, and [`enter`]
//! makes the frame current at `DoCall`.
//!
//! Calls of builtin functions by name follow php-src's `ZEND_DO_ICALL`
//! the same way: [`prepare`] takes the function from the call site's
//! cache, [`send`] writes each argument into the argument buffer of the
//! pooled frame above the current one (the builtin's call frame, which
//! backtraces read), and [`call_native`] pushes that frame, runs the
//! handler and pops it, all in the tape loop; a function with a frameless
//! class runs without the frame when its arguments allow it
//! (`ZEND_FRAMELESS_ICALL_n`, `Vm::frameless_icall_prepared`). Calls the generic builtin
//! call would treat specially (an argument it would coerce or reject, a
//! function with a by-reference parameter, a deprecated or `#[\NoDiscard]`
//! one, an observer) take the generic path.
//!
//! At most one call is prepared at a time, and only by the tape loop
//! (`run_tape`), which holds it in a local ([`Prepared`]). Whenever
//! anything else could see or change the caller's calls (another call
//! starting, an op the tape hands back to the generic code, the tape loop
//! returning to `Vm::run`, a safe point), the loop first turns the
//! prepared call into the [`PendingCall`] the generic protocol would have
//! built ([`materialize`]) and returns the frame to the pool, so the rest
//! of the VM never sees a half-prepared frame.

use super::{rel, Inst, TOp, SLOT_SHIFT};
use crate::call::{accepts_without_coercion, clone_value};
use crate::func::{flags, Args, Builtin, CallShape, Func, PlainCall, UserFunction, FOLD_STEPS};
use crate::opcode::{
    ArgSrc, BinKind, ClassSpec, FetchMode, Op, OpArray, Operand, PlaceBase, PROP_CACHE_THIS,
};
use crate::place::propcache::{accepts, PC_DYN, PC_NO_SET, PC_WRITE};
use crate::vm::{FrameKind, PendingCall, RetTarget, Throw, Vm, VmResult};
use php_runtime::{BinaryOp, Obj, Value};
use std::rc::Rc;

/// The most arguments a prepared call takes: the pending call's inline
/// buffer (what [`materialize`] moves them into) and the precomputed
/// first ops of [`UserFunction::plain_first_op`].
const MAX_ARGS: usize = 6;

/// The specialised instruction of the op at `i` when this family has one.
pub(super) fn lower(code: &OpArray, i: usize) -> Option<Inst> {
    let plain = Inst::plain;
    match code.ops.get(i)? {
        // A builtin whose one-argument call needs no handler, called with
        // one computed argument: the function's id in `e`, its frameless
        // class in `d` (`prepare_quick`).
        Op::InitFCall { .. } if quick_call(code, i).is_some() => {
            let (id, class) = quick_call(code, i)?;
            Some(Inst {
                k: CALL_QUICK,
                e: id,
                d: class,
                ..plain(TOp::CallInit)
            })
        }
        // `new C` with nothing to evaluate before its constructor call
        // (`skip`: the op after that call): the class literal in `a`, the
        // result temporary in `b` and the jump to `skip` in `c`, for the
        // core loop's own attempt (`new_plain`).
        Op::New {
            class: ClassSpec::Named(lit),
            dst,
            skip,
        } if *skip != 0 => Some(Inst {
            k: CALL_NEW,
            a: *lit,
            b: *dst,
            c: rel(code, i, *skip)?,
            ..plain(TOp::CallInit)
        }),
        // `new C(arguments)` whose arguments are values at hand
        // ([`new_args_end`]): as above, the jump going to the op after the
        // constructor call, with `e` set and the op's own index in `d`
        // (the handler reads the arguments from the ops after it).
        Op::New {
            class: ClassSpec::Named(lit),
            dst,
            skip: 0,
        } if new_args_end(code, i).is_some() => Some(Inst {
            k: CALL_NEW,
            e: 1,
            a: *lit,
            b: *dst,
            c: rel(code, i, new_args_end(code, i)?)?,
            d: u32::try_from(i).ok()?,
            ..plain(TOp::CallInit)
        }),
        op if preparable(op) && stays_prepared(code, i) => Some(plain(TOp::CallInit)),
        // `new` is prepared as before whatever its arguments: its prepared
        // start also makes the object (`new_object`).
        op @ Op::New { .. } if preparable(op) => Some(plain(TOp::CallInit)),
        // `ZEND_FRAMELESS_ICALL_n` (the forms `strs::lower` left): run with
        // the VM at hand (`run_vm_op`).
        Op::FramelessICall(_) => Some(plain(TOp::VmOp)),
        // A read of a superglobal (`$_GET`) or of a global variable by
        // name (`$GLOBALS['name']`): resolved through the VM's global
        // scope, so run there as well (`Vm::fetch_place_fast`,
        // `Vm::fetch_global_fast`).
        Op::Fetch {
            place,
            mode: FetchMode::Read,
            ..
        } if auto_global_place(code, *place) => Some(plain(TOp::VmOp)),
        Op::FetchGlobal { .. } => Some(plain(TOp::VmOp)),
        // An element of an object's property (`$o->p[k]` read, written,
        // compounded, incremented, tested): the property cache hits of
        // `exec/spec/props.rs`, which need the VM.
        Op::FetchObjPropDim { .. }
        | Op::AssignObjPropDim { .. }
        | Op::AssignOpObjPropDim { .. }
        | Op::IncDecObjPropDim { .. }
        | Op::IssetObjPropDim { .. } => Some(plain(TOp::VmOp)),
        // Only an argument of a call one of those started can go to a
        // prepared call; the others keep the generic `Send`, without the
        // detour.
        Op::Send { src }
            if direct_src(code, src)
                && call_start_at(code, i).is_some_and(|s| {
                    preparable(&code.ops[s])
                        && (matches!(code.ops[s], Op::New { .. }) || stays_prepared(code, s))
                }) =>
        {
            Some(plain(TOp::CallSend))
        }
        _ => None,
    }
}

/// Whether the call started at `start` can be made from its preparation
/// to its `DoCall` without the tape giving it up: the ops in between are
/// argument sends it makes itself and hot ops, and none starts a call of
/// its own (an argument that is a call, `f(g($x))`, a send of another
/// kind or an op the tape leaves at flushes the prepared call into a
/// `PendingCall`, `materialize`, which cost more than the preparation
/// saved). Such calls start the generic way.
fn stays_prepared(code: &OpArray, start: usize) -> bool {
    /// Ops scanned at most (a call with many computed arguments starts
    /// the generic way).
    const SCAN: usize = 24;
    for op in code.ops.iter().skip(start + 1).take(SCAN) {
        match op {
            Op::DoCall { .. } => return true,
            Op::Send { src } if direct_src(code, src) => {}
            Op::Send { .. }
            | Op::InitFCall { .. }
            | Op::InitDynCall { .. }
            | Op::InitMethodCall { .. }
            | Op::InitStaticCall { .. }
            | Op::InitParentHook { .. }
            | Op::New { .. }
            | Op::CallableConvert { .. }
            | Op::CallablePartial { .. } => return false,
            op if crate::exec::hot::is_hot(op) => {}
            _ => return false,
        }
    }
    false
}

/// Whether `place` is a variable named by a literal starting with an
/// underscore, read as a whole: what `Vm::fetch_place_fast` looks up among
/// the superglobals (any other name is not one, and its read stays the
/// generic op's).
fn auto_global_place(code: &OpArray, place: u32) -> bool {
    let Some(p) = code.places.get(place as usize) else {
        return false;
    };
    if !p.segs.is_empty() {
        return false;
    }
    match &p.base {
        PlaceBase::Global(Operand::Lit(lit)) => matches!(
            code.literals.get(*lit as usize),
            Some(Value::Str(name)) if name.as_bytes().first() == Some(&b'_')
        ),
        _ => false,
    }
}

/// The mark (`k`) of a `CallInit` instruction whose call [`prepare_quick`]
/// may take.
pub(super) const CALL_QUICK: u8 = 1;
/// The mark of a `CallInit` instruction that is a `new C` the core loop
/// tries itself ([`new_plain`]).
pub(super) const CALL_NEW: u8 = 2;

/// The most sums [`new_stored`] computes between the sends of one `new`.
const NEW_SUMS: usize = 3;

/// The most `new` instructions of one tape with a site entry
/// ([`NewSite`]); the ones past it make their checks every time.
const NEW_SITES: usize = 256;

/// What [`new_plain`] answers the core loop and a trace: nothing done;
/// the object in the result temporary; or the object made in the block of
/// the one the assigned variable held ([`new_target`]), which holds it:
/// the assignment the instruction's jump goes to is done as well.
pub(super) const NEW_DECLINED: u8 = 0;
/// See [`NEW_DECLINED`].
pub(super) const NEW_MADE: u8 = 1;
/// See [`NEW_DECLINED`].
pub(super) const NEW_OVER: u8 = 2;

/// Where a [`CALL_NEW`] instruction's `p` holds the variable its result
/// is assigned to, plus one, above its site number.
const NEW_TO_SHIFT: u32 = usize::BITS / 2;

/// Gives every [`CALL_NEW`] instruction of `tape` its site number in `p`
/// (its index in the tape's `new_sites`) and, above it, the variable the
/// instruction its jump goes to assigns the result to ([`new_target`]).
pub(super) fn number_new_sites(tape: &mut [Inst]) {
    let mut next = 0;
    for i in 0..tape.len() {
        if tape[i].op == TOp::CallInit && tape[i].k == CALL_NEW {
            let to = assigned_cv(tape, i).map_or(0, |cv| cv as usize + 1);
            tape[i].p = next | (to << NEW_TO_SHIFT);
            next += 1;
        }
    }
}

/// The variable `$v` of `$v = new C(...)`: the instruction the jump of the
/// [`CALL_NEW`] instruction at `i` goes to (past the constructor call)
/// assigns the result temporary to it, the result unused otherwise.
fn assigned_cv(tape: &[Inst], i: usize) -> Option<u32> {
    let inst = tape.get(i)?;
    #[allow(clippy::cast_possible_wrap)]
    let off = i64::from(inst.c as i32) / i64::try_from(std::mem::size_of::<Inst>()).ok()?;
    let at = usize::try_from(i64::try_from(i).ok()?.checked_add(off)?).ok()?;
    let next = tape.get(at)?;
    (at > i && next.op == TOp::AssignT && next.b >> SLOT_SHIFT == inst.b)
        .then_some(next.a >> SLOT_SHIFT)
}

/// The site number of a [`CALL_NEW`] instruction.
#[inline]
fn site_no(inst: &Inst) -> usize {
    inst.p & ((1 << NEW_TO_SHIFT) - 1)
}

/// The variable a [`CALL_NEW`] instruction's result is assigned to at
/// once, when it is ([`number_new_sites`]).
#[inline]
pub(super) fn new_target(inst: &Inst) -> Option<u32> {
    u32::try_from(inst.p >> NEW_TO_SHIFT).ok()?.checked_sub(1)
}

/// What the full checks of [`new_stored`] found out about a `new`
/// instruction, kept for the next `new` there (`Tape::new_sites`): what
/// depends on the instruction alone (where its arguments come from) and
/// what depends on the class alone. A class's constructor, its slots and
/// what its properties take do not change.
#[derive(Default)]
pub(super) struct NewSite {
    /// The class (none yet: `Weak::new()`). The handle pins the class's
    /// address, so an equal address is this class.
    class: std::rc::Weak<crate::class::Class>,
    /// The arguments passed, which are the constructor's parameters.
    n: u8,
    /// The stores (`steps[..len]`).
    len: u8,
    /// A parameter or a property stored to has a type: the arguments'
    /// types are tested (`params`, the steps' `accept`).
    typed: bool,
    /// Where each argument comes from.
    srcs: [Src; MAX_ARGS],
    /// The builtin type bits each parameter's declared type takes as they
    /// are (`u32::MAX`: no type, or `mixed`).
    params: [u32; MAX_ARGS],
    /// The variable the result is assigned to at once ([`new_target`]),
    /// when the object may be made in the block of the one it holds
    /// ([`site_over`]): no argument is read from that variable (it would
    /// be the old object). `u32::MAX`: none.
    over: u32,
    steps: [SiteStep; FOLD_STEPS],
}

/// Where an argument of a `new` comes from: what the `Send`s after the
/// instruction pass ([`sent_args`] reads the same ops every time).
#[derive(Clone, Copy, Default)]
enum Src {
    #[default]
    None,
    /// A literal of the op array.
    Lit(u32),
    /// What a compiled variable holds.
    Cv(u32),
    /// `a + b`, `a - b` or `a * b` of literals and compiled variables.
    Sum(BinaryOp, Operand, Operand),
}

/// An argument of a `new`, read where it is: a value the frame or the op
/// array holds, or a sum just computed. Nothing is owned.
#[derive(Clone, Copy)]
enum Arg<'a> {
    Held(&'a Value),
    Int(i64),
}

impl Arg<'_> {
    /// Whether a parameter with the type bits `mask` takes it as it is
    /// (`enter`'s test).
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of `new_stored`
    fn passes(self, mask: u32) -> bool {
        match self {
            Arg::Held(v) => accepts_without_coercion(mask, v),
            Arg::Int(_) => mask & crate::types::mask::LONG != 0,
        }
    }

    /// Whether a property accepting the type bits `accept` takes it as it
    /// is (`propcache::accepts`).
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of `new_stored`
    fn fits(self, accept: u32) -> bool {
        match self {
            Arg::Held(v) => accepts(accept, v),
            Arg::Int(_) => accept & crate::types::mask::LONG != 0,
        }
    }
}

/// One store of a [`NewSite`]: the parameter, the slot it goes to and the
/// builtin type bits the property takes as they are (`u32::MAX`: untyped).
#[derive(Clone, Copy, Default)]
struct SiteStep {
    param: u8,
    slot: u32,
    accept: u32,
}

impl NewSite {
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of `new_stored`
    fn steps(&self) -> &[SiteStep] {
        self.steps.get(..usize::from(self.len)).unwrap_or(&[])
    }

    /// Whether every parameter's type takes its argument as it is and
    /// every property the argument stored in it.
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of `new_stored`
    fn takes(&self, args: &[Arg<'_>]) -> bool {
        self.params
            .iter()
            .zip(args)
            .all(|(mask, a)| *mask == u32::MAX || a.passes(*mask))
            && self.steps().iter().all(|s| {
                args.get(usize::from(s.param))
                    .is_some_and(|a| a.fits(s.accept))
            })
    }

    /// The constructor's stores: each argument (`args`, by parameter) into
    /// its slot of `slots`, a new object's.
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of `new_stored`
    fn store(&self, slots: &mut [Option<Value>], args: &[Arg<'_>]) {
        for step in self.steps() {
            if let (Some(cell), Some(arg)) = (
                slots.get_mut(step.slot as usize),
                args.get(usize::from(step.param)),
            ) {
                store_arg(cell, *arg);
            }
        }
    }

    /// The object: its slots copied from the class's defaults, then the
    /// arguments (`args`, by parameter) stored. `None` is not reached (the
    /// object is this VM's and nothing else holds its slots); nothing is
    /// left behind then.
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of `new_stored`
    fn build(
        &self,
        cls: &Rc<crate::class::Class>,
        defaults: &[Option<Value>],
        args: &[Arg<'_>],
    ) -> Option<Obj> {
        let made = crate::object::new_object_from_defaults(Rc::clone(cls), defaults);
        let stored = match made
            .downcast_ref::<crate::object::ObjectData>()
            .map(|data| data.slots.try_borrow_mut())
        {
            Some(Ok(mut held)) => {
                self.store(&mut held, args);
                true
            }
            _ => false,
        };
        if stored {
            Some(made)
        } else {
            crate::object::discard_rejected(made);
            None
        }
    }
}

/// Stores `arg` in `cell`, a slot of an object just made from its class's
/// defaults: a default that owns something (a string, an array the class's
/// table holds too) is released, anything else written over. (No slot of
/// a new object is marked unset: there is no such mark to clear.)
#[inline(always)]
#[allow(clippy::inline_always)] // once per store
fn store_arg(cell: &mut Option<Value>, arg: Arg<'_>) {
    if !matches!(
        cell,
        None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_))
    ) {
        *cell = None;
    }
    let new = match arg {
        Arg::Int(i) => Value::Int(i),
        Arg::Held(v @ (Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_))) => {
            // SAFETY: both are valid and distinct (an argument's place and
            // an object's slot), 16 bytes each, and a `Some` has its
            // value's layout (the tag is the value's own: the trace JIT's
            // `value_layout` test pins it). A scalar owns nothing, so its
            // bytes are its copy, and the slot holds nothing to release
            // (cleared above). The two safe forms were measured first, at
            // 55 and 45 instructions a store: the value rebuilt through a
            // jump on its tag, then written tag, padding and payload apart.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    std::ptr::from_ref(v).cast::<u8>(),
                    std::ptr::from_mut(cell).cast::<u8>(),
                    std::mem::size_of::<Value>(),
                );
            }
            return;
        }
        Arg::Held(v) => clone_value(v),
    };
    // What the slot holds now owns nothing.
    std::mem::forget(cell.replace(new));
}

/// Where the arguments of the `new` instruction `inst` come from, and how
/// many there are: [`sent_args`]'s reading of the ops after it, kept as
/// what to read. `None` for anything [`sent_args`] would not read the
/// same way at every `new` (a sum no send passes, another operator).
fn site_srcs(code: &OpArray, inst: &Inst) -> Option<([Src; MAX_ARGS], usize)> {
    let mut srcs = [Src::None; MAX_ARGS];
    let mut n = 0usize;
    if inst.e == 0 {
        return Some((srcs, n));
    }
    let mut sums = [(u32::MAX, Src::None); NEW_SUMS];
    let mut made = 0usize;
    for op in code.ops.get(inst.d as usize + 1..)? {
        match op {
            Op::Nop => {}
            Op::Send { src } => {
                let from = match src {
                    ArgSrc::Val(Operand::Lit(lit)) => Src::Lit(*lit),
                    ArgSrc::Val(Operand::Cv(cv)) => Src::Cv(*cv),
                    ArgSrc::Place(pid) => Src::Cv(place_cv(code, *pid)?),
                    ArgSrc::Val(Operand::Tmp(tmp)) => {
                        let sum = sums.iter_mut().find(|s| s.0 == *tmp)?;
                        sum.0 = u32::MAX;
                        sum.1
                    }
                    _ => return None,
                };
                *srcs.get_mut(n)? = from;
                n += 1;
            }
            Op::Bin {
                op: BinKind::Arith(kind @ (BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul)),
                a,
                b,
                dst,
            } => {
                *sums.get_mut(made)? = (*dst, Src::Sum(*kind, *a, *b));
                made += 1;
            }
            // The `DoCall`.
            _ => break,
        }
    }
    // Every sum was passed.
    sums.iter().all(|s| s.0 == u32::MAX).then_some((srcs, n))
}

/// The argument `src` names, read in frame `f`. `None` for what
/// [`sent_args`] declines (an undefined variable, a sum of anything but
/// ints, an overflow) and for a variable held by reference, whose value
/// [`sent_args`] copies out.
#[inline(always)]
#[allow(clippy::inline_always)] // once per argument
fn arg_view(f: &crate::vm::Frame, src: Src) -> Option<Arg<'_>> {
    // A compiled variable first: the commonest argument.
    if let Src::Cv(cv) = src {
        return match f.cvs.get(cv as usize).and_then(crate::vm::CvSlot::get)? {
            Value::Reference(_) => None,
            v => Some(Arg::Held(v)),
        };
    }
    Some(match src {
        Src::Lit(lit) => Arg::Held(f.code.literals.get(lit as usize)?),
        Src::Sum(kind, a, b) => {
            let (lhs, rhs) = (int_operand(f, a)?, int_operand(f, b)?);
            Arg::Int(match kind {
                BinaryOp::Add => lhs.checked_add(rhs)?,
                BinaryOp::Sub => lhs.checked_sub(rhs)?,
                BinaryOp::Mul => lhs.checked_mul(rhs)?,
                _ => return None,
            })
        }
        Src::Cv(_) | Src::None => return None,
    })
}

/// The op after the constructor call of the `New` at `i` when the tape's
/// `new` instruction can stand for all of it ([`new_stored`]): every op up
/// to the `DoCall` is a `Send` of a literal, of a compiled variable or of
/// a sum computed here, or such a sum (`+`, `-` or `*` of variables and
/// literals into a temporary that a later `Send` passes). `None` for any
/// other argument list (a call, a ternary, a named or unpacked argument,
/// more than [`MAX_ARGS`] arguments).
fn new_args_end(code: &OpArray, i: usize) -> Option<u32> {
    let plain = |o: &Operand| matches!(o, Operand::Lit(_) | Operand::Cv(_));
    let mut sums = [u32::MAX; NEW_SUMS];
    let mut sends = 0usize;
    for (k, op) in code.ops.get(i + 1..)?.iter().enumerate() {
        match op {
            Op::Nop => {}
            Op::Send { src } => {
                match src {
                    ArgSrc::Val(Operand::Lit(_) | Operand::Cv(_)) => {}
                    ArgSrc::Place(pid) if place_cv(code, *pid).is_some() => {}
                    // A sum made here, passed once.
                    ArgSrc::Val(Operand::Tmp(t)) => {
                        *sums.iter_mut().find(|s| **s == *t)? = u32::MAX;
                    }
                    _ => return None,
                }
                sends += 1;
                if sends > MAX_ARGS {
                    return None;
                }
            }
            Op::Bin {
                op: BinKind::Arith(BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul),
                a,
                b,
                dst,
            } if plain(a) && plain(b) && *dst != u32::MAX => {
                *sums.iter_mut().find(|s| **s == u32::MAX)? = *dst;
            }
            // The call, every sum passed.
            Op::DoCall { dst: None } if sums.iter().all(|s| *s == u32::MAX) => {
                return u32::try_from(i + k + 2).ok();
            }
            _ => return None,
        }
    }
    None
}

/// `ZEND_NEW` on the call site's class cache hit, when nothing of it needs
/// a call: the object into the result temporary, and the jump over the
/// ops of the constructor call (the instruction's jump).
///
/// - A class without a constructor, `new C` without arguments: the object
///   (`props::new_plain`: its slots copied from the class's evaluated
///   defaults).
/// - A class whose constructor only stores its parameters in properties
///   ([`new_stored`]): the object with the arguments stored.
///
/// `false`, nothing changed, for anything else: a cache miss, any other
/// constructor, arguments to a class without one, or a class
/// `props::new_plain` leaves to `Vm::instantiate` (abstract, an enum, an
/// internal class with a creation hook, ...) - `run_call` then decides as
/// before. A trace calls this handler too (`jit::plan_new`).
///
/// The first case is the whole of this function, as it was before the
/// second existed (its call is one of `run_core`'s, and `$x = new Foo()` in
/// a loop is little more than it); the second lies behind a call
/// ([`new_filled`]).
///
/// # Safety
/// `inst` is a [`CALL_NEW`] instruction [`lower`] built for `frame`, the
/// current frame.
#[inline(never)]
pub(super) unsafe fn new_plain(frame: *mut crate::vm::Frame, inst: &Inst) -> u8 {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    if f.tmps.len() <= inst.b as usize {
        return NEW_DECLINED;
    }
    if new_target(inst).is_some_and(|cv| new_over(f, inst, cv)) {
        return NEW_OVER;
    }
    let cls = {
        let Ok(cache) = f.code.class_cache.try_borrow() else {
            return NEW_DECLINED;
        };
        match cache.get(inst.a as usize).and_then(std::rc::Weak::upgrade) {
            Some(cls) => cls,
            None => return NEW_DECLINED,
        }
    };
    if cls.magic.construct.is_some() || inst.e != 0 {
        // SAFETY: the caller's contract; the borrows above have ended.
        return unsafe { new_filled(frame, inst, cls) };
    }
    let Some(o) = crate::props::new_plain(&cls) else {
        return NEW_DECLINED;
    };
    match f.tmps.get_mut(inst.b as usize) {
        Some(slot) => crate::exec::spec::put_tmp(slot, Value::Object(o)),
        // (Not reached: the slot was looked for above.)
        None => crate::object::discard_rejected(o),
    }
    NEW_MADE
}

/// `$v = new C(...)` where `$v` (`cv`) holds the last handle of an object
/// of the class the instruction's cache names: the new object made in
/// that block (`object::reuse_for_new`, which says when nothing can
/// tell), by the class the object itself holds (no handle of the class
/// is taken). A class with a constructor, or arguments: when the
/// constructor only stores them ([`site_over`]). `false`, nothing
/// changed, otherwise ([`new_plain`] goes on as without).
#[inline(never)]
fn new_over(f: &crate::vm::Frame, inst: &Inst, cv: u32) -> bool {
    let Some(Value::Object(held)) = f.cvs.get(cv as usize).and_then(crate::vm::CvSlot::get) else {
        return false;
    };
    let Some(d) = held.downcast_ref::<crate::object::ObjectData>() else {
        return false;
    };
    let cls = &d.class;
    let named = f.code.class_cache.try_borrow().is_ok_and(|cache| {
        cache
            .get(inst.a as usize)
            .is_some_and(|named| std::ptr::eq(std::rc::Weak::as_ptr(named), Rc::as_ptr(cls)))
    });
    // (The commonest reason not to: the old object lives on elsewhere.)
    if !named || held.refcount() != 1 {
        return false;
    }
    let Ok(defaults) = cls.defaults.try_borrow() else {
        return false;
    };
    let Some(defaults) = defaults.as_deref() else {
        return false;
    };
    if cls.magic.construct.is_some() || inst.e != 0 {
        return !super::jit::OBSERVER_ON.load(std::sync::atomic::Ordering::Relaxed)
            && site_over(f, inst, cv, held, d, defaults);
    }
    if !crate::props::plain_new_class(cls) {
        return false;
    }
    match crate::object::reuse_for_new(held, d, defaults, |_| {}) {
        Some(made) => {
            if made.buffered {
                crate::gc::reused(held, d, made.old, true, false);
            }
            true
        }
        None => false,
    }
}

/// [`new_over`] for a class whose constructor only stores its parameters
/// in properties, by the site's entry ([`NewSite`], when it is for this
/// class and allows it: `NewSite::over`): the arguments are read where
/// they are and checked as [`site_made`] checks them, the object is made
/// in the block of `held` with them stored, and the possible roots are
/// recorded in the order the call's return and the assignment would have
/// (each argument, the new object, then the old one's entry gone).
#[inline(always)]
#[allow(clippy::inline_always)] // part of `new_over`
fn site_over(
    f: &crate::vm::Frame,
    inst: &Inst,
    cv: u32,
    held: &Obj,
    d: &crate::object::ObjectData,
    defaults: &[Option<Value>],
) -> bool {
    let Ok(sites) = f.code.tape.new_sites.try_borrow() else {
        return false;
    };
    let Some(site) = sites.get(site_no(inst)) else {
        return false;
    };
    if std::rc::Weak::as_ptr(&site.class) != Rc::as_ptr(&d.class) || site.over != cv {
        return false;
    }
    let mut args = [Arg::Int(0); MAX_ARGS];
    let n = usize::from(site.n);
    for (arg, src) in args.iter_mut().zip(&site.srcs).take(n) {
        let Some(view) = arg_view(f, *src) else {
            return false;
        };
        *arg = view;
    }
    let Some(args) = args.get(..n) else {
        return false;
    };
    if site.typed && !site.takes(args) {
        return false;
    }
    let Some(made) = crate::object::reuse_for_new(held, d, defaults, |slots| {
        site.store(slots, args);
    }) else {
        return false;
    };
    for arg in args {
        if let Arg::Held(v @ (Value::Array(_) | Value::Object(_) | Value::Reference(_))) = arg {
            crate::gc::possible_root(v);
        }
    }
    crate::gc::reused(held, d, made.old, made.buffered, true);
    true
}

/// [`new_plain`] for a class with a constructor, or with arguments: the
/// object [`new_stored`] makes, into the result temporary.
///
/// # Safety
/// As [`new_plain`].
// (Part of `new_plain`: one call less on the way to `new_stored`. The
// handle moves in: the call is the caller's last act.)
#[inline(always)]
#[allow(clippy::inline_always, clippy::needless_pass_by_value)]
unsafe fn new_filled(
    frame: *mut crate::vm::Frame,
    inst: &Inst,
    cls: Rc<crate::class::Class>,
) -> u8 {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    let Some(o) = new_stored(f, inst, &cls) else {
        return NEW_DECLINED;
    };
    match f.tmps.get_mut(inst.b as usize) {
        Some(slot) => crate::exec::spec::put_tmp(slot, Value::Object(o)),
        // (Not reached: `new_plain` found the slot.)
        None => crate::object::discard_rejected(o),
    }
    NEW_MADE
}

/// The int operand `o` of a sum between the sends of a `new` is: a
/// literal, or what a compiled variable holds itself.
fn int_operand(f: &crate::vm::Frame, o: Operand) -> Option<i64> {
    let v = match o {
        Operand::Lit(l) => f.code.literals.get(l as usize)?,
        Operand::Cv(cv) => f.cvs.get(cv as usize)?.get()?,
        _ => return None,
    };
    match v {
        Value::Int(i) => Some(*i),
        _ => None,
    }
}

/// The object of `new C(arguments)` (the [`CALL_NEW`] instruction `inst` of
/// frame `f`, `cls` its class) when the constructor only stores its
/// parameters in properties ([`UserFunction::ctor_fold`]): made and
/// filled without the call, as the call would have left it.
///
/// - The arguments are what the `Send`s after the op would pass
///   ([`sent_args`]). Every declared parameter is passed, and no more; each
///   has a type that takes its argument as it is (`enter`'s test).
/// - Every store is checked before the object exists, as
///   `spec::props::assign_hit` checks it: the constructor's own property
///   cache entry for the place is filled for this class and the
///   constructor's scope, allows a plain write of a declared property and
///   takes the value as it is; the slot it names holds, in a new object,
///   a default that is replaced or nothing that `__set()` could claim.
///   (A first `new` of the class runs the call, which fills the entries.)
///   What of this depends on the class alone, and where the arguments come
///   from, is found out once per `new` instruction and class ([`NewSite`],
///   filled by [`new_checked`]); [`site_made`] then reads the arguments
///   where they are and stores them.
/// - What the call's return would have recorded is recorded
///   (`gc::frame_released`): each argument as a possible root, then the
///   object itself, which `new`'s result still holds.
///
/// `None`, nothing done, for anything else: an observer, another
/// constructor (not public, static, with other statements, with a
/// parameter left to its default), a class `props::ctor_ref` refuses.
#[inline(never)]
fn new_stored(f: &crate::vm::Frame, inst: &Inst, cls: &Rc<crate::class::Class>) -> Option<Obj> {
    if super::jit::OBSERVER_ON.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    let defaults = cls.defaults.try_borrow().ok()?;
    let defaults = defaults.as_deref()?;
    // The site's entry when it is for this class (what depends on the class
    // and on the instruction was found out when it was filled), else the
    // full checks.
    site_made(f, inst, cls, defaults).or_else(|| new_checked(f, inst, cls, defaults))
}

/// The object of a `new` whose site entry ([`NewSite`]) is for `cls`, when
/// the arguments are where the entry says and have types the parameters
/// and the properties take as they are. `None`, nothing done, otherwise
/// ([`new_checked`] then decides).
#[inline(always)]
#[allow(clippy::inline_always)] // part of `new_stored`
fn site_made(
    f: &crate::vm::Frame,
    inst: &Inst,
    cls: &Rc<crate::class::Class>,
    defaults: &[Option<Value>],
) -> Option<Obj> {
    let sites = f.code.tape.new_sites.try_borrow().ok()?;
    let site = sites.get(site_no(inst))?;
    if std::rc::Weak::as_ptr(&site.class) != Rc::as_ptr(cls) {
        return None;
    }
    let mut args = [Arg::Int(0); MAX_ARGS];
    let n = usize::from(site.n);
    for (arg, src) in args.iter_mut().zip(&site.srcs).take(n) {
        *arg = arg_view(f, *src)?;
    }
    let args = args.get(..n)?;
    if site.typed && !site.takes(args) {
        return None;
    }
    let made = site.build(cls, defaults, args)?;
    for arg in args {
        if let Arg::Held(v) = arg {
            crate::gc::possible_root(v);
        }
    }
    crate::gc::root_ctor_result(&made);
    Some(made)
}

/// [`new_stored`]'s full checks, the object when they pass, and the site's
/// entry for the next `new` there (kept only when it answers for every
/// argument its type bits take: not when a value passed for being of the
/// class a class-typed property accepted last).
#[inline(never)]
fn new_checked(
    f: &crate::vm::Frame,
    inst: &Inst,
    cls: &Rc<crate::class::Class>,
    defaults: &[Option<Value>],
) -> Option<Obj> {
    // The constructor first: one with a body of its own is the commonest
    // reason to leave, and its answer is kept (`ctor_fold`).
    let (func, scope) = crate::props::ctor_ref(cls)?;
    let Func::User(uf) = func else {
        return None;
    };
    let fold = uf.ctor_fold()?;
    let mut args = NewArgs::new();
    if inst.e != 0 {
        sent_args(f, inst, &mut args)?;
    }
    let args = args.given();
    if args.len() != uf.params.len() {
        return None;
    }
    let mut site = NewSite {
        over: u32::MAX,
        class: Rc::downgrade(cls),
        n: u8::try_from(args.len()).ok()?,
        len: u8::try_from(fold.len()).ok()?,
        ..NewSite::default()
    };
    site.params.fill(u32::MAX);
    if uf.call_shape() == CallShape::TYPED {
        for (mask, p) in site.params.iter_mut().zip(&uf.params) {
            if let Some(ty) = p.ty.as_ref().filter(|ty| !ty.is_mixed()) {
                *mask = ty.mask;
                site.typed = true;
            }
        }
    }
    let params_take = site
        .params
        .iter()
        .zip(args)
        .all(|(mask, v)| *mask == u32::MAX || accepts_without_coercion(*mask, v));
    if !params_take {
        return None;
    }
    let class_addr = Rc::as_ptr(cls) as usize;
    // A value passed for being of the class a class-typed property
    // accepted last: the entry would not answer for it.
    let mut by_class = false;
    {
        let cache = uf.code.prop_cache.try_borrow().ok()?;
        for (step, at) in fold.iter().zip(&mut site.steps) {
            let entry = cache.get(step.place as usize)?;
            let v = args.get(step.param as usize)?;
            if entry.class_addr != class_addr
                || entry.scope_addr != scope
                || entry.base != PROP_CACHE_THIS
                || entry.flags & (PC_WRITE | PC_DYN) != PC_WRITE
            {
                return None;
            }
            if !accepts(entry.accept, v) {
                let stored_class = matches!(
                    v,
                    Value::Object(held) if entry.ok_class_addr != 0
                        && Rc::as_ptr(crate::object::class_of(held)) as usize
                            == entry.ok_class_addr
                );
                if !stored_class {
                    return None;
                }
                by_class = true;
            }
            match defaults.get(entry.slot as usize)? {
                Some(Value::Reference(_)) => return None,
                Some(_) => {}
                // An undefined slot is filled when `__set()` cannot claim
                // it: the class has none, or the property is typed (and,
                // in a new object, never unset).
                None if entry.flags & PC_NO_SET != 0 || entry.accept != u32::MAX => {}
                None => return None,
            }
            site.typed |= entry.accept != u32::MAX;
            *at = SiteStep {
                param: u8::try_from(step.param).ok()?,
                slot: entry.slot,
                accept: entry.accept,
            };
        }
    }
    let mut views = [Arg::Int(0); MAX_ARGS];
    for (view, v) in views.iter_mut().zip(args) {
        *view = Arg::Held(v);
    }
    let made = site.build(cls, defaults, views.get(..args.len())?)?;
    for v in args {
        crate::gc::possible_root(v);
    }
    crate::gc::root_ctor_result(&made);
    if !by_class && site_no(inst) < NEW_SITES {
        if let Some((srcs, n)) = site_srcs(&f.code, inst) {
            if let Ok(mut sites) = f.code.tape.new_sites.try_borrow_mut() {
                if n == args.len() {
                    site.srcs = srcs;
                    // (An argument read from the assigned variable would be
                    // the old object.)
                    site.over = new_target(inst)
                        .filter(|cv| {
                            !srcs.iter().take(n).any(|src| match src {
                                Src::Cv(c) => c == cv,
                                Src::Sum(_, a, b) => {
                                    *a == Operand::Cv(*cv) || *b == Operand::Cv(*cv)
                                }
                                Src::Lit(_) | Src::None => false,
                            })
                        })
                        .unwrap_or(u32::MAX);
                    crate::rtcache::grow_slots(&mut sites, site_no(inst) + 1, NewSite::default);
                    if let Some(slot) = sites.get_mut(site_no(inst)) {
                        *slot = site;
                    }
                }
            }
        }
    }
    Some(made)
}

/// The arguments the `Send`s after the `new` instruction `inst` of frame
/// `f` would pass, into `args`: a copy of a literal or of a variable's
/// value (dereferenced; an undefined variable declines, its send warns),
/// or a sum computed between them (ints, no overflow: any other operand
/// declines). `None`, nothing changed in the frame, when a send has no
/// such value or there are more than [`MAX_ARGS`].
#[inline(always)]
#[allow(clippy::inline_always)] // part of `new_stored`
fn sent_args(f: &crate::vm::Frame, inst: &Inst, args: &mut NewArgs) -> Option<()> {
    let code = &*f.code;
    let mut sums = [(u32::MAX, 0i64); NEW_SUMS];
    let mut made = 0usize;
    for op in code.ops.get(inst.d as usize + 1..)? {
        match op {
            Op::Nop => {}
            Op::Send { src } => {
                let v = match src {
                    ArgSrc::Val(Operand::Lit(lit)) => {
                        clone_value(code.literals.get(*lit as usize)?)
                    }
                    ArgSrc::Val(Operand::Cv(cv)) => cv_arg(f, *cv)?,
                    ArgSrc::Place(pid) => cv_arg(f, place_cv(code, *pid)?)?,
                    ArgSrc::Val(Operand::Tmp(tmp)) => {
                        Value::Int(sums.iter().find(|s| s.0 == *tmp)?.1)
                    }
                    _ => return None,
                };
                args.push(v)?;
            }
            Op::Bin {
                op: BinKind::Arith(kind),
                a,
                b,
                dst,
            } => {
                let (lhs, rhs) = (int_operand(f, *a)?, int_operand(f, *b)?);
                let sum = match kind {
                    BinaryOp::Add => lhs.checked_add(rhs)?,
                    BinaryOp::Sub => lhs.checked_sub(rhs)?,
                    BinaryOp::Mul => lhs.checked_mul(rhs)?,
                    _ => return None,
                };
                *sums.get_mut(made)? = (*dst, sum);
                made += 1;
            }
            // The `DoCall`.
            _ => break,
        }
    }
    Some(())
}

/// The arguments [`new_stored`] collects: at most [`MAX_ARGS`] values, of
/// which only those given may own something. They are released by a tag
/// test each and nothing else of the array is ever dropped (an array of
/// `Value`s has drop glue for every slot, and a store into one a
/// `drop_in_place` call for what it replaces: about 150 instructions
/// around one int argument).
struct NewArgs {
    vals: std::mem::ManuallyDrop<[Value; MAX_ARGS]>,
    n: usize,
}

impl NewArgs {
    #[inline(always)]
    #[allow(clippy::inline_always)] // the head of `new_stored`
    fn new() -> Self {
        NewArgs {
            vals: std::mem::ManuallyDrop::new(std::array::from_fn(|_| Value::Null)),
            n: 0,
        }
    }

    /// Adds `v`; `None` (and `v` released) when there is no room.
    #[inline(always)]
    #[allow(clippy::inline_always)] // once per argument
    fn push(&mut self, v: Value) -> Option<()> {
        let slot = self.vals.get_mut(self.n)?;
        // The slot holds the `Null` it started with.
        std::mem::forget(std::mem::replace(slot, v));
        self.n += 1;
        Some(())
    }

    /// The arguments given so far.
    #[inline(always)]
    #[allow(clippy::inline_always)] // part of `new_stored`
    fn given(&self) -> &[Value] {
        self.vals.get(..self.n).unwrap_or(&[])
    }
}

impl Drop for NewArgs {
    #[inline(always)]
    #[allow(clippy::inline_always)] // every way out of `new_stored`
    fn drop(&mut self) {
        let n = self.n;
        for v in self.vals.iter_mut().take(n) {
            match std::mem::take(v) {
                // A scalar owns nothing.
                old @ (Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)) => {
                    std::mem::forget(old);
                }
                other => drop(other),
            }
        }
    }
}

/// [`cv_value`] where [`sent_args`] reads an argument: the copy of a
/// scalar made in place.
#[inline(always)]
#[allow(clippy::inline_always)] // once per argument
fn cv_arg(f: &crate::vm::Frame, cv: u32) -> Option<Value> {
    Some(
        match f.cvs.get(cv as usize).and_then(crate::vm::CvSlot::get)? {
            Value::Null => Value::Null,
            Value::Bool(b) => Value::Bool(*b),
            Value::Int(i) => Value::Int(*i),
            Value::Float(x) => Value::Float(*x),
            Value::Reference(r) => r.get(),
            v => v.clone(),
        },
    )
}

/// The id and frameless class (`special::quick_id`) of the builtin the
/// `InitFCall` at `i` names, when its call is `f(<expression>)`: one
/// argument, a temporary, passed right before the `DoCall`, with no other
/// call started while it is computed. `None` for any other call.
fn quick_call(code: &OpArray, i: usize) -> Option<(u8, u32)> {
    let Op::InitFCall {
        name,
        fallback: None,
        ..
    } = code.ops.get(i)?
    else {
        return None;
    };
    let Value::Str(lc) = code.literals.get(*name as usize)? else {
        return None;
    };
    let quick = crate::special::quick_id(lc.as_bytes())?;
    let mut j = i + 1;
    loop {
        match code.ops.get(j)? {
            Op::Send {
                src: ArgSrc::Val(Operand::Tmp(_)) | ArgSrc::CallResult(Operand::Tmp(_)),
            } => {
                return matches!(code.ops.get(j + 1)?, Op::DoCall { .. }).then_some(quick);
            }
            // Another argument, or another call: not this shape.
            Op::Send { .. }
            | Op::SendNamed { .. }
            | Op::SendUnpack { .. }
            | Op::SendPlaceholder { .. }
            | Op::DoCall { .. }
            | Op::CallableConvert { .. }
            | Op::CallablePartial { .. }
            | Op::FramelessICall(_)
            | Op::InitFCall { .. }
            | Op::InitDynCall { .. }
            | Op::InitMethodCall { .. }
            | Op::InitStaticCall { .. }
            | Op::InitParentHook { .. }
            | Op::New { .. } => return None,
            _ => j += 1,
        }
    }
}

/// Whether a call started by `op` may be prepared ([`prepare`],
/// [`prepare_method`]).
fn preparable(op: &Op) -> bool {
    matches!(
        op,
        Op::InitFCall { .. }
            | Op::InitMethodCall {
                name: Operand::Lit(_),
                ..
            }
            | Op::InitStaticCall {
                class: ClassSpec::Named(_)
                    | ClassSpec::SelfClass
                    | ClassSpec::Parent
                    | ClassSpec::Static,
                name: Operand::Lit(_),
            }
            | Op::InitDynCall {
                callee: Operand::Cv(_),
            }
            | Op::New {
                class: ClassSpec::Named(_),
                ..
            }
    )
}

/// The index of the op that started the call the `Send` at `i` passes an
/// argument to:
/// the nearest call-starting op before it whose call is not finished
/// before `i` (calls nest: an argument's own calls start and end in
/// between). `None` when there is none (not expected).
fn call_start_at(code: &OpArray, i: usize) -> Option<usize> {
    let mut finished = 0usize;
    for (at, op) in code.ops.get(..i)?.iter().enumerate().rev() {
        match op {
            Op::DoCall { .. } | Op::CallableConvert { .. } | Op::CallablePartial { .. } => {
                finished += 1;
            }
            Op::InitFCall { .. }
            | Op::InitDynCall { .. }
            | Op::InitMethodCall { .. }
            | Op::InitStaticCall { .. }
            | Op::InitParentHook { .. }
            | Op::New { .. } => {
                if finished == 0 {
                    return Some(at);
                }
                finished -= 1;
            }
            _ => {}
        }
    }
    None
}

/// Whether [`send`] can pass `src` (by value: a literal, a temporary, or a
/// compiled variable).
fn direct_src(code: &OpArray, src: &ArgSrc) -> bool {
    match src {
        ArgSrc::Val(Operand::Lit(_) | Operand::Tmp(_) | Operand::Cv(_))
        | ArgSrc::CallResult(Operand::Tmp(_)) => true,
        ArgSrc::Place(pid) => place_cv(code, *pid).is_some(),
        _ => false,
    }
}

/// The compiled variable a place stands for when it is one without
/// segments (`f($x)` compiles the argument as a place: by reference when
/// the parameter is).
pub(crate) fn place_cv(code: &OpArray, pid: u32) -> Option<u32> {
    let p = code.places.get(pid as usize)?;
    match (&p.base, p.segs.is_empty()) {
        (PlaceBase::Cv(cv), true) => Some(*cv),
        _ => None,
    }
}

/// What a prepared call calls.
enum Callee {
    /// A user function, method or closure: its frame is readied, and the
    /// arguments go into its compiled variables.
    User(Rc<UserFunction>),
    /// A builtin function called by name ([`Builtin::plain_call`]): the
    /// arguments go into the pooled frame's argument buffer.
    Native(Rc<Builtin>, PlainCall),
    /// A builtin whose one-argument call needs no handler
    /// ([`prepare_quick`]): its id (`special::quick_id`) and the index of
    /// its `InitFCall`. Nothing is readied: the `Send` computes the result
    /// ([`quick_send`]), or the call becomes the generic pending call.
    Quick(u8, u32),
}

/// A call prepared by [`prepare`]: the function, how many arguments
/// [`send`] wrote into the frame, and how many pending calls the caller
/// had when it started (the place of this call among them).
pub(super) struct Prepared {
    callee: Callee,
    n: usize,
    /// How many arguments [`send`] takes: the declared parameters (any
    /// number into a builtin's variadic one), at most [`MAX_ARGS`].
    limit: usize,
    calls_len: usize,
    /// Started by `InitFCall` (the pending call's `direct`); otherwise a
    /// method, static method or closure call, whose `$this`, scopes and
    /// closure are in the prepared frame.
    fcall: bool,
    /// A closure call (`$f()`, the pending call's `dynamic`): its bound
    /// variables are copied into the frame at `DoCall`.
    closure: bool,
    /// The constructor call of `new` (the pending call's `is_ctor`).
    ctor: bool,
}

/// `ZEND_INIT_FCALL` of `Op::InitFCall { name, fallback: None, .. }` in the
/// current frame: when the call site's cache holds a user function whose
/// declaration makes a plain call (as `Vm::push_simple_ucall` requires),
/// the pooled frame above the current one is readied for it and the call
/// is prepared. `None`, having changed nothing, otherwise: the generic
/// `InitFCall` runs.
pub(super) fn prepare(vm: &mut Vm, name: u32) -> Option<Prepared> {
    if vm.observer.enabled {
        return None;
    }
    let live = vm.frames.live;
    // `push_user_call`'s stack-depth backstop is the generic path's.
    if live == 0 || live > 50_000 {
        return None;
    }
    vm.frames.ensure_pooled(&vm.empty_code, &vm.empty_vars);
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        // No pooled frame at this depth yet: the generic call pushes one.
        return None;
    };
    let func = {
        let cache = caller.code.fcall_cache.borrow();
        let entry = cache.get(name as usize)?.as_ref()?;
        // A builtin the tape does not call goes the generic way without
        // its handle being copied first.
        if let crate::opcode::CachedFuncKind::Builtin(b) = &entry.func {
            if b.plain_call() == PlainCall::No {
                return None;
            }
        }
        entry.get()?
    };
    let (target, limit) = match func {
        Func::User(uf) => {
            if uf.call_shape() == CallShape::NOT_PLAIN {
                return None;
            }
            ready_frame(frame, &uf);
            let limit = user_limit(&uf);
            (Callee::User(uf), limit)
        }
        Func::Builtin(b) => {
            let kind = b.plain_call();
            // A pooled frame's argument buffer is empty.
            if kind == PlainCall::No || !frame.args.is_empty() {
                return None;
            }
            // Past the declared parameters only into a variadic one (else
            // the generic call reports the count).
            let limit = if b.flags & flags::VARIADIC != 0 {
                MAX_ARGS
            } else {
                b.signature().params.len().min(MAX_ARGS)
            };
            (Callee::Native(b, kind), limit)
        }
    };
    Some(Prepared {
        callee: target,
        limit,
        n: 0,
        calls_len: caller.calls.len(),
        fcall: true,
        closure: false,
        ctor: false,
    })
}

/// `ZEND_INIT_NS_FCALL_BY_NAME` (an unqualified call from namespaced
/// code, `InitFCall` with a fallback) at `at`, when the call site's
/// run-time cache resolves it to a builtin the tape calls by name: looked
/// up as `init_ns_fcall_in` looks it up (the site's own slot `site`, where
/// a resolved global fallback is cached, else the namespaced name's), the
/// call is prepared as [`prepare`] prepares a builtin's, not direct (as
/// the generic op pushes it). `None`, nothing changed, otherwise (a user
/// function's call starts the generic way).
pub(super) fn prepare_ns(vm: &mut Vm, name: u32, site: usize) -> Option<Prepared> {
    if vm.observer.enabled {
        return None;
    }
    let live = vm.frames.live;
    if live == 0 || live > 50_000 {
        return None;
    }
    vm.frames.ensure_pooled(&vm.empty_code, &vm.empty_vars);
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return None;
    };
    // A pooled frame's argument buffer is empty.
    if !frame.args.is_empty() {
        return None;
    }
    let func = {
        let cache = caller.code.fcall_cache.borrow();
        let entry = match cache.get(site).and_then(Option::as_ref) {
            Some(entry) => entry,
            None => cache.get(name as usize)?.as_ref()?,
        };
        if let crate::opcode::CachedFuncKind::Builtin(b) = &entry.func {
            if b.plain_call() != PlainCall::Named {
                return None;
            }
        }
        entry.get()?
    };
    let b = match func {
        Func::Builtin(b) => b,
        Func::User(uf) => {
            if uf.call_shape() == CallShape::NOT_PLAIN {
                return None;
            }
            ready_frame(frame, &uf);
            return Some(Prepared {
                limit: user_limit(&uf),
                callee: Callee::User(uf),
                n: 0,
                calls_len: caller.calls.len(),
                fcall: false,
                closure: false,
                ctor: false,
            });
        }
    };
    // Past the declared parameters only into a variadic one (else the
    // generic call reports the count).
    let limit = if b.flags & flags::VARIADIC != 0 {
        MAX_ARGS
    } else {
        b.signature().params.len().min(MAX_ARGS)
    };
    Some(Prepared {
        callee: Callee::Native(b, PlainCall::Named),
        limit,
        n: 0,
        calls_len: caller.calls.len(),
        fcall: false,
        closure: false,
        ctor: false,
    })
}

/// `ZEND_INIT_FCALL` of a call [`CALL_QUICK`] marks (`f(<expression>)` of
/// a builtin whose one-argument call needs no handler: `id`, of frameless
/// class `class`), `at` its index: when the call site's cache holds that
/// builtin (neither deprecated nor `#[\NoDiscard]`) and no observer is
/// installed, the call is prepared without readying anything. Its `Send`
/// makes it ([`quick_send`]). `None`, nothing changed, otherwise.
pub(super) fn prepare_quick(
    vm: &mut Vm,
    name: u32,
    id: u8,
    class: u32,
    at: usize,
) -> Option<Prepared> {
    if vm.observer.enabled {
        return None;
    }
    let caller = vm.frames.last()?;
    {
        let cache = caller.code.fcall_cache.try_borrow().ok()?;
        let entry = cache.get(name as usize)?.as_ref()?;
        let crate::opcode::CachedFuncKind::Builtin(b) = &entry.func else {
            return None;
        };
        if !crate::special::frameless_class_is(b.flags, class) {
            return None;
        }
    }
    Some(Prepared {
        callee: Callee::Quick(id, u32::try_from(at).ok()?),
        n: 0,
        limit: 1,
        calls_len: caller.calls.len(),
        fcall: true,
        closure: false,
        ctor: false,
    })
}

/// The `Send` of a call [`prepare_quick`] prepared (at `at`, the `DoCall`
/// right after it): when the argument, a temporary, has a shape the
/// function accepts as it is (`special::quick_eval`), the result is
/// computed, the temporary consumed and the result stored in the
/// `DoCall`'s temporary (the caller's resume point saved as for the call:
/// `SAVE_OPLINE()`). Returns the op after the `DoCall`. `None`, nothing
/// changed, for any other argument: the call becomes the pending call and
/// the generic `Send` and `DoCall` run.
pub(super) fn quick_send(
    vm: &mut Vm,
    id: u8,
    src: &ArgSrc,
    ops: &[Op],
    at: usize,
) -> Option<usize> {
    let (ArgSrc::Val(Operand::Tmp(t)) | ArgSrc::CallResult(Operand::Tmp(t))) = src else {
        return None;
    };
    let Some(&Op::DoCall { dst }) = ops.get(at + 1) else {
        return None;
    };
    let caller = vm.frames.last_mut()?;
    if dst.is_some_and(|d| d as usize >= caller.tmps.len()) {
        return None;
    }
    let result = crate::special::quick_eval(id, caller.tmps.get(*t as usize)?)?;
    // The argument is consumed: a number, or a string for `ord()`.
    drop(std::mem::take(caller.tmps.get_mut(*t as usize)?));
    caller.ip = at + 2;
    match dst.and_then(|d| caller.tmps.get_mut(d as usize)) {
        Some(slot) => crate::exec::spec::put_tmp(slot, result),
        None => drop(result),
    }
    Some(at + 2)
}

/// Readies the pooled frame `frame` for a call of `uf`, as
/// `Vm::push_simple_ucall` readies it at `DoCall` (`FrameStack::push_in_place`).
/// How many arguments a prepared call of `uf` (a plain declaration) takes:
/// one per parameter it binds ([`UserFunction::plain_params`]), and for a
/// function with a trailing variadic parameter up to [`MAX_ARGS`] in all,
/// the extra ones going to the frame's argument buffer ([`send`]).
#[inline]
fn user_limit(uf: &UserFunction) -> usize {
    if uf.flags & flags::VARIADIC != 0 {
        MAX_ARGS
    } else {
        uf.plain_params().min(MAX_ARGS)
    }
}

fn ready_frame(frame: &mut crate::vm::Frame, uf: &Rc<UserFunction>) {
    debug_assert!(
        frame.this.is_none()
            && frame.scope.is_none()
            && frame.called.is_none()
            && frame.closure.is_none()
            && frame.calls.is_empty()
    );
    let code = &uf.code;
    // (A released frame's iterators and finally states hold nothing:
    // `Frame::clear_for_reuse`.)
    if frame.cvs.len() == code.cv_names.len()
        && frame.tmps.len() == code.num_tmps as usize
        && frame.iters.len() == code.num_iters as usize
        && frame.fins.len() == code.num_finally as usize
    {
        frame.reinit_same_call(code, FrameKind::Function);
    } else {
        frame.reinit_for_call(Rc::clone(code), FrameKind::Function);
    }
    if !frame.ufunc.as_ref().is_some_and(|f| Rc::ptr_eq(f, uf)) {
        // The frame keeps the function across calls in this slot
        // (`Frame::clear_for_reuse`): only another function replaces it.
        if let Some(old) = frame.ufunc.replace(Rc::clone(uf)) {
            drop(old);
        }
        if let Some(old) = frame.func.replace(Func::User(Rc::clone(uf))) {
            drop(old);
        }
    }
}

/// `ZEND_INIT_METHOD_CALL` of `Op::InitMethodCall { obj, name: Lit(lit) }`
/// at `slot` in the current frame: on the method cache hit
/// `Vm::init_method_call_fast` takes (the object's class and the calling
/// scope as cached), when the method is a user function whose declaration
/// makes a plain call, the pooled frame above the current one is readied
/// for it with its `$this` (moved out of a temporary, else a copy; none
/// for a static method) and scopes, and the call is prepared. `None`,
/// having changed nothing, otherwise.
pub(super) fn prepare_method(vm: &mut Vm, obj: Operand, lit: u32, slot: usize) -> Option<Prepared> {
    if vm.observer.enabled {
        return None;
    }
    let live = vm.frames.live;
    if live == 0 || live > 50_000 {
        return None;
    }
    vm.frames.ensure_pooled(&vm.empty_code, &vm.empty_vars);
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return None;
    };
    let o = match obj {
        Operand::Cv(cv) if !caller.symtab_attached => {
            match caller.cvs.get(cv as usize).and_then(crate::vm::CvSlot::get) {
                Some(Value::Object(o)) => o,
                _ => return None,
            }
        }
        Operand::Tmp(t) => match caller.tmps.get(t as usize) {
            Some(Value::Object(o)) => o,
            _ => return None,
        },
        Operand::Unused => caller.this.as_ref()?,
        _ => return None,
    };
    let cls = crate::object::class_of(o);
    if cls.hooks.as_ref().and_then(|h| h.get_method).is_some() {
        return None;
    }
    let cls_addr = Rc::as_ptr(cls) as usize;
    let scope_addr = caller.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
    let (func, scope, is_static) =
        crate::call::method_entry_hit(&caller.code, slot, cls_addr, scope_addr, lit)?;
    let uf = match func {
        Func::User(uf) => uf,
        Func::Builtin(b) => {
            let called_class = Rc::clone(cls);
            return prepare_native_method(caller, frame, obj, b, scope, is_static, called_class);
        }
    };
    if uf.call_shape() == CallShape::NOT_PLAIN {
        return None;
    }
    let called_class = Rc::clone(cls);
    // The call's `$this`, as `Vm::init_method_call_fast` takes it: the
    // temporary's handle moved out, or a copy of the variable's object;
    // a static method takes none (the temporary's handle is released
    // after the frame is readied).
    let (this, released) = match obj {
        Operand::Tmp(t) => match caller.tmps.get_mut(t as usize).map(std::mem::take) {
            Some(Value::Object(held)) if !is_static => (Some(held), None),
            other => (None, other),
        },
        _ if is_static => (None, None),
        _ => (Some(o.clone()), None),
    };
    ready_frame(frame, &uf);
    frame.this = this;
    frame.scope = scope;
    frame.called = Some(called_class);
    let calls_len = caller.calls.len();
    drop(released);
    Some(Prepared {
        limit: user_limit(&uf),
        callee: Callee::User(uf),
        n: 0,
        calls_len,
        fcall: false,
        closure: false,
        ctor: false,
    })
}

/// `ZEND_INIT_STATIC_METHOD_CALL` of `Op::InitStaticCall { class:
/// Named(_), name: Lit(_) }` at `at` in the current frame: on the call
/// site's cache hit (`Vm::op_init_static_call`'s first case: the site's
/// slot past the literals, indexed by the op after it), when the method
/// is a user function whose declaration makes a plain call, the pooled
/// frame above the current one is readied for it with its scopes (no
/// `$this`) and the call is prepared. `None`, having changed nothing,
/// otherwise.
pub(super) fn prepare_static(vm: &mut Vm, at: usize) -> Option<Prepared> {
    if vm.observer.enabled {
        return None;
    }
    let live = vm.frames.live;
    if live == 0 || live > 50_000 {
        return None;
    }
    vm.frames.ensure_pooled(&vm.empty_code, &vm.empty_vars);
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return None;
    };
    let slot = caller.code.literals.len() + at + 1;
    let (func, scope, called_class) = caller
        .code
        .fcall_cache
        .borrow()
        .get(slot)
        .and_then(|c| c.as_ref().and_then(crate::opcode::CachedFunc::get_call))?;
    let Func::User(uf) = func else {
        return None;
    };
    if uf.call_shape() == CallShape::NOT_PLAIN {
        return None;
    }
    ready_frame(frame, &uf);
    frame.scope = scope;
    frame.called = called_class;
    Some(Prepared {
        limit: user_limit(&uf),
        callee: Callee::User(uf),
        n: 0,
        calls_len: caller.calls.len(),
        fcall: false,
        closure: false,
        ctor: false,
    })
}

/// [`prepare_method`] of a builtin method (`ReflectionMethod::getName()`)
/// whose call is plain: called by [`call_native`] like a builtin function,
/// with `$this` (taken as [`prepare_method`] takes it) and the scopes on
/// the pooled frame. Out of line, so that the user method's path keeps
/// its code.
#[inline(never)]
fn prepare_native_method(
    caller: &mut crate::vm::Frame,
    frame: &mut crate::vm::Frame,
    obj: Operand,
    b: Rc<Builtin>,
    scope: Option<Rc<crate::class::Class>>,
    is_static: bool,
    called_class: Rc<crate::class::Class>,
) -> Option<Prepared> {
    if b.plain_call() != PlainCall::Named || !frame.args.is_empty() {
        return None;
    }
    let (this, released) = match obj {
        Operand::Tmp(t) => match caller.tmps.get_mut(t as usize).map(std::mem::take) {
            Some(Value::Object(held)) if !is_static => (Some(held), None),
            other => (None, other),
        },
        _ if is_static => (None, None),
        Operand::Cv(cv) => match caller.cvs.get(cv as usize).and_then(crate::vm::CvSlot::get) {
            Some(Value::Object(o)) => (Some(o.clone()), None),
            _ => return None,
        },
        Operand::Unused => (Some(caller.this.clone()?), None),
        _ => return None,
    };
    // Past the declared parameters only into a variadic one (else the
    // generic call reports the count).
    let limit = if b.flags & flags::VARIADIC != 0 {
        MAX_ARGS
    } else {
        b.signature().params.len().min(MAX_ARGS)
    };
    frame.this = this;
    frame.scope = scope;
    frame.called = Some(called_class);
    let calls_len = caller.calls.len();
    drop(released);
    Some(Prepared {
        limit,
        callee: Callee::Native(b, PlainCall::Named),
        n: 0,
        calls_len,
        fcall: false,
        closure: false,
        ctor: false,
    })
}

/// `ZEND_INIT_STATIC_METHOD_CALL` of `self::name()`, `parent::name()` or
/// `static::name()` (`Op::InitStaticCall` with such a spec and the method
/// name literal `lit`) at `at` in the current frame: on the hit
/// `Vm::init_relative_static_call_cached` takes (the op's own slot of the
/// method cache holds the class the spec resolves to, the calling scope
/// and the method), when the method is a user function whose declaration
/// makes a plain call, the pooled frame above the current one is readied
/// for it as that hit sets the call up: a static method with the frame's
/// called scope when that is a subclass of the resolved class (the three
/// specs forward it), else the class; an instance method with the frame's
/// `$this` when its class is one. `None`, having changed nothing,
/// otherwise (a miss, and an instance method without such a `$this`: the
/// generic op raises the error).
pub(super) fn prepare_relative_static(
    vm: &mut Vm,
    spec: &ClassSpec,
    lit: u32,
    at: usize,
) -> Option<Prepared> {
    if vm.observer.enabled {
        return None;
    }
    let live = vm.frames.live;
    if live == 0 || live > 50_000 {
        return None;
    }
    vm.frames.ensure_pooled(&vm.empty_code, &vm.empty_vars);
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return None;
    };
    if caller.kind == FrameKind::Builtin {
        return None;
    }
    let cls = crate::call::relative_spec_class(caller, spec)?;
    let scope_addr = caller.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
    let (uf, scope, is_static) = {
        let cache = caller.code.method_cache.borrow();
        let cached = cache.get(at)?.as_ref()?;
        if cached.class_addr != Rc::as_ptr(cls) as usize
            || cached.scope_addr != scope_addr
            || cached.name_lit != Some(lit)
        {
            return None;
        }
        let Some(Func::User(uf)) = cached.func.get() else {
            return None;
        };
        let scope = cached
            .method_scope
            .as_ref()
            .and_then(std::rc::Weak::upgrade)?;
        (uf, scope, cached.is_static)
    };
    if uf.call_shape() == CallShape::NOT_PLAIN {
        return None;
    }
    let (this, late_bound) = if is_static {
        let late_bound = match caller.called.as_ref() {
            Some(c) if c.is_subclass_of(cls) => Rc::clone(c),
            _ => Rc::clone(cls),
        };
        (None, late_bound)
    } else {
        let this = caller.this.as_ref()?;
        let late_bound = crate::object::class_of(this);
        if !late_bound.is_subclass_of(cls) {
            return None;
        }
        (Some(this.clone()), Rc::clone(late_bound))
    };
    ready_frame(frame, &uf);
    frame.this = this;
    frame.scope = Some(scope);
    frame.called = Some(late_bound);
    Some(Prepared {
        limit: user_limit(&uf),
        callee: Callee::User(uf),
        n: 0,
        calls_len: caller.calls.len(),
        fcall: false,
        closure: false,
        ctor: false,
    })
}

/// `ZEND_INIT_DYNAMIC_CALL` of `Op::InitDynCall { callee: Cv(cv) }` in the
/// current frame: when the variable holds a closure of a user function
/// whose declaration makes a plain call (`Vm::init_closure_call_fast`,
/// without a `__call` trampoline), the pooled frame above the current one
/// is readied for it with the closure's `$this`, scopes and the closure
/// itself, and the call is prepared. `None`, having changed nothing,
/// otherwise.
pub(super) fn prepare_closure(vm: &mut Vm, cv: u32) -> Option<Prepared> {
    if vm.observer.enabled {
        return None;
    }
    let live = vm.frames.live;
    if live == 0 || live > 50_000 {
        return None;
    }
    vm.frames.ensure_pooled(&vm.empty_code, &vm.empty_vars);
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return None;
    };
    if caller.symtab_attached {
        return None;
    }
    let Some(Value::Object(o)) = caller.cvs.get(cv as usize).and_then(crate::vm::CvSlot::get)
    else {
        return None;
    };
    let data = crate::object::data(o);
    if data.class.lc_name.as_bytes() != b"closure" {
        return None;
    }
    let internal = data.internal.try_borrow().ok()?;
    let d = internal
        .as_ref()
        .and_then(|x| x.downcast_ref::<crate::closure::ClosureData>())?;
    let Func::User(uf) = &d.func else {
        return None;
    };
    if d.magic_name.is_some() || uf.call_shape() == CallShape::NOT_PLAIN {
        return None;
    }
    let uf = Rc::clone(uf);
    ready_frame(frame, &uf);
    frame.this = d.this.clone();
    frame.scope = d.scope.clone();
    frame.called = d.called.clone();
    frame.closure = if d.fake { None } else { Some(o.clone()) };
    drop(internal);
    Some(Prepared {
        limit: user_limit(&uf),
        callee: Callee::User(uf),
        n: 0,
        calls_len: caller.calls.len(),
        fcall: false,
        closure: true,
        ctor: false,
    })
}

/// What [`new_object`] did.
pub(super) enum NewDone {
    /// The object of a class without a constructor is in the result:
    /// continue at the op after the constructor call (`skip`).
    Skip(usize),
    /// The object is in the result and its constructor call is prepared.
    Prepared(Prepared),
    /// The object is in the result and its constructor call is the
    /// generic pending call (`props::new_with_ctor`).
    Pending,
}

/// `ZEND_NEW` of `Op::New { class: Named(lit), dst, skip }` in the current
/// frame, on the class cache hit the props fast path takes: a class
/// without a constructor gets its object (`props::new_plain`, then the
/// jump over the constructor call); a class whose constructor is a user
/// function that makes a plain call gets its object and the constructor's
/// frame prepared on it (`ZEND_NEW` pushing the call frame with the object
/// as `$this`); any other constructor, the generic pending call. `None`,
/// having changed nothing, when none of that applies (the generic `New`).
pub(super) fn new_object(vm: &mut Vm, lit: u32, dst: u32, skip: u32) -> Option<NewDone> {
    let live = vm.frames.live;
    if live == 0 {
        return None;
    }
    let observed = vm.observer.enabled;
    vm.frames.ensure_pooled(&vm.empty_code, &vm.empty_vars);
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let caller = callers.last_mut()?;
    if dst as usize >= caller.tmps.len() {
        return None;
    }
    let cls = caller
        .code
        .class_cache
        .try_borrow()
        .ok()?
        .get(lit as usize)
        .and_then(std::rc::Weak::upgrade)?;
    if cls.magic.construct.is_none() {
        // Without a `skip` target (arguments being evaluated), the generic
        // op starts the placeholder call that evaluates them.
        if skip == 0 {
            return None;
        }
        let o = crate::props::new_plain(&cls)?;
        crate::exec::spec::put_tmp(&mut caller.tmps[dst as usize], Value::Object(o));
        return Some(NewDone::Skip(skip as usize));
    }
    let direct = match (crate::props::ctor_call(&cls), pool.first_mut()) {
        (Some((Func::User(uf), scope)), Some(frame))
            if !observed && live <= 50_000 && uf.call_shape() != CallShape::NOT_PLAIN =>
        {
            Some((uf, scope, frame))
        }
        _ => None,
    };
    let Some((uf, scope, frame)) = direct else {
        let o = crate::props::new_with_ctor(cls, &mut caller.calls)?;
        crate::exec::spec::put_tmp(&mut caller.tmps[dst as usize], Value::Object(o));
        return Some(NewDone::Pending);
    };
    let o = crate::props::new_for_ctor(&cls)?;
    ready_frame(frame, &uf);
    frame.this = Some(o.clone());
    frame.scope = Some(scope);
    frame.called = Some(cls);
    crate::exec::spec::put_tmp(&mut caller.tmps[dst as usize], Value::Object(o));
    Some(NewDone::Prepared(Prepared {
        limit: user_limit(&uf),
        callee: Callee::User(uf),
        n: 0,
        calls_len: caller.calls.len(),
        fcall: false,
        closure: false,
        ctor: true,
    }))
}

/// `ZEND_SEND_VAL` / `ZEND_SEND_VAR` of `src` for the prepared call: the
/// argument is copied (a literal, a variable: dereferenced) or moved (a
/// temporary) straight into the callee's compiled variable. `false`,
/// having changed nothing, when the send is not the prepared call's (a
/// later pending call, started by `new`, is on top), the argument has no
/// declared parameter or is more than [`MAX_ARGS`], or the variable is
/// undefined (the generic send warns): the caller then materializes the
/// call and the generic `Send` runs.
pub(super) fn send(vm: &mut Vm, prep: &mut Prepared, src: &ArgSrc) -> bool {
    let n = prep.n;
    if n >= prep.limit {
        return false;
    }
    let live = vm.frames.live;
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return false;
    };
    if caller.calls.len() != prep.calls_len {
        return false;
    }
    let v = match src {
        ArgSrc::Val(Operand::Lit(i)) => match caller.code.literals.get(*i as usize) {
            Some(v) => clone_value(v),
            None => return false,
        },
        ArgSrc::Val(Operand::Tmp(t)) | ArgSrc::CallResult(Operand::Tmp(t)) => {
            match caller.tmps.get_mut(*t as usize) {
                Some(slot) => match std::mem::take(slot) {
                    Value::Reference(r) => r.get(),
                    v => v,
                },
                None => return false,
            }
        }
        ArgSrc::Val(Operand::Cv(cv)) => match cv_value(caller, *cv) {
            Some(v) => v,
            None => return false,
        },
        ArgSrc::Place(pid) => {
            match place_cv(&caller.code, *pid).and_then(|cv| cv_value(caller, cv)) {
                Some(v) => v,
                None => return false,
            }
        }
        _ => return false,
    };
    match prep.callee {
        Callee::User(ref uf) if n >= uf.plain_params() => {
            // An argument past the bound parameters of a variadic function
            // (`user_limit`): in the argument buffer at its position, as the
            // generic call leaves it, for the callee's `RECV_VARIADIC` (and
            // `func_get_args()`, traces) to find; the positions of the bound
            // ones hold `None` (their values are in compiled variables).
            frame.args.resize(n, None);
            frame.args.push(Some(v));
        }
        Callee::User(_) => {
            let Some(slot) = frame.cvs.get_mut(n) else {
                return false;
            };
            // A prepared frame's compiled variables are empty.
            if let Some(old) = slot.as_mut().replace(v) {
                drop(old);
            }
        }
        // `ZEND_SEND_VAL` / `ZEND_SEND_VAR` into the call frame's argument
        // slot.
        Callee::Native(..) => frame.args.push(Some(v)),
        // (Its `Send` is `quick_send`'s.)
        Callee::Quick(..) => return false,
    }
    prep.n = n + 1;
    true
}

/// The value of compiled variable `cv` of `frame` as an argument: a copy,
/// dereferenced; `None` when it is undefined.
fn cv_value(frame: &crate::vm::Frame, cv: u32) -> Option<Value> {
    match frame
        .cvs
        .get(cv as usize)
        .and_then(crate::vm::CvSlot::get)?
    {
        Value::Reference(r) => Some(r.get()),
        v => Some(clone_value(v)),
    }
}

/// `ZEND_DO_UCALL` of the prepared call (result to `dst`; the caller
/// resumes at `caller_ip`): the frame becomes the current one, as
/// `Vm::push_simple_ucall` leaves it. `Err` gives the call back, having
/// changed nothing, when the arguments do not make a plain call (too few,
/// or one that a typed parameter does not accept as it is) or the call is
/// not the caller's newest: the caller materializes it and the generic
/// `DoCall` runs. A builtin's call is given back as well, for
/// [`call_native`].
pub(super) fn enter(
    vm: &mut Vm,
    prep: Prepared,
    dst: Option<u32>,
    caller_ip: usize,
) -> Result<(), Prepared> {
    let live = vm.frames.live;
    let n = prep.n;
    let Callee::User(uf) = &prep.callee else {
        return Err(prep);
    };
    let bound = uf.plain_params();
    // (More arguments only for a variadic function: `user_limit`.)
    if n < uf.required as usize {
        return Err(prep);
    }
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return Err(prep);
    };
    if caller.calls.len() != prep.calls_len {
        return Err(prep);
    }
    if uf.call_shape() == CallShape::TYPED {
        for (p, slot) in uf.params.iter().zip(&frame.cvs[..n.min(bound)]) {
            if let Some(ty) = &p.ty {
                if !slot
                    .get()
                    .is_some_and(|v| crate::call::param_accepts(ty, v))
                {
                    return Err(prep);
                }
            }
        }
    }
    frame.ret = dst.map_or(RetTarget::Discard, RetTarget::Tmp);
    frame.caller_strict = caller.code.strict_types;
    frame.called_from_internal = false;
    frame.is_ctor = prep.ctor;
    // The passed-argument count (`ZEND_CALL_NUM_ARGS`): each argument is
    // already in its compiled variable, so the buffer stays empty and the
    // count is kept instead (`Frame::num_args`); extra arguments of a
    // variadic function are in the buffer, at their positions (`send`).
    if n <= bound && !frame.args.is_empty() {
        frame.args.clear();
    }
    #[allow(clippy::cast_possible_truncation)] // at most `MAX_ARGS`
    {
        frame.direct_argc = n as u32;
    }
    // The `RECV`s of the passed arguments have nothing left to do.
    frame.ip = uf.plain_first_op(n);
    if prep.closure {
        bind_closure_vars(frame);
    }
    // Released by `Frame::release_direct` at its return.
    frame.direct = true;
    caller.ip = caller_ip;
    vm.frames.live = live + 1;
    Ok(())
}

/// `ZEND_DO_UCALL` of the prepared call when the function's whole body
/// runs as machine code ([`super::jit::native_call`]: a pure int function,
/// its recursion included): a call that passes every declared parameter
/// an int. A call by name, of a method (static or not) or of a closure
/// alike: such a body reads no `$this`, no scope and no bound variable
/// (a function with `use`s is never compiled whole), so nothing it could
/// observe differs between them; a constructor's call is left alone (its
/// `new` has more to do). The result goes to `dst` and the readied frame,
/// which is never entered, back to its empty state: the arguments were
/// ints, and what a method's or a closure's prepare put there (`$this`,
/// the scopes, the closure) is released as the call's return would
/// release it. `false`, nothing changed, when it does
/// not apply or the native code declined (an overflow, a recursion deeper
/// than its stack budget, a pending safe point): [`enter`] then makes the
/// call as usual.
pub(super) fn compiled(vm: &mut Vm, prep: &Prepared, dst: Option<u32>) -> bool {
    let Callee::User(uf) = &prep.callee else {
        return false;
    };
    // One load for every function that is not compiled whole.
    if uf.call_shape.native.get() == super::jit::NATIVE_REFUSED || prep.ctor {
        return false;
    }
    let n = prep.n;
    if n != uf.params.len() || n > super::jit::NATIVE_MAX_ARGS || uf.flags & flags::VARIADIC != 0 {
        return false;
    }
    let live = vm.frames.live;
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return false;
    };
    if caller.calls.len() != prep.calls_len {
        return false;
    }
    let mut args = [0i64; super::jit::NATIVE_MAX_ARGS];
    let Some(slots) = frame.cvs.get(..n) else {
        return false;
    };
    for (a, slot) in args.iter_mut().zip(slots) {
        let Some(Value::Int(v)) = slot.get() else {
            return false;
        };
        *a = *v;
    }
    let Some(r) = super::jit::native_call(uf, &args[..n]) else {
        return false;
    };
    if let Some(slots) = frame.cvs.get_mut(..n) {
        for slot in slots {
            // An int owns nothing.
            std::mem::forget(slot.as_mut().take());
        }
    }
    if !prep.fcall {
        frame.this = None;
        frame.scope = None;
        frame.called = None;
        frame.closure = None;
    }
    if let Some(slot) = dst.and_then(|d| caller.tmps.get_mut(d as usize)) {
        crate::exec::spec::put_tmp(slot, Value::Int(r));
    }
    true
}

/// Whether the prepared call is a builtin's ([`call_native`]).
impl Prepared {
    pub(super) fn is_native(&self) -> bool {
        matches!(self.callee, Callee::Native(..))
    }

    /// The function id of a call [`prepare_quick`] prepared.
    pub(super) fn quick_id(&self) -> Option<u8> {
        match self.callee {
            Callee::Quick(id, _) => Some(id),
            _ => None,
        }
    }
}

/// `ZEND_DO_ICALL` of the prepared builtin call (result to `dst`; the
/// caller resumes at `caller_ip`), as `Vm::call_builtin_impl` makes a
/// plain call: the pooled frame the arguments were sent into becomes the
/// builtin's frame (php-src's call frame, which backtraces see), the
/// handler runs with copies of the arguments and the declared defaults of
/// the omitted trailing parameters, the interrupt check follows
/// (`ZEND_VM_FCALL_INTERRUPT_CHECK`), destructors queued meanwhile run
/// while the frame is still there, and the frame goes back to the pool.
///
/// `Ok` is the call's outcome: the handler's exception or exit, or the
/// result stored in `dst` (released when unused). `Err` gives the call
/// back, having changed nothing, when the generic call would treat it
/// otherwise (an argument count it reports, an argument it coerces or
/// rejects, the call not being the caller's newest): the caller
/// materializes it and the generic `DoCall` runs.
#[inline(never)]
#[allow(clippy::too_many_lines)] // one call's steps, in `call_builtin_impl`'s order
pub(super) fn call_native(
    vm: &mut Vm,
    prep: Prepared,
    dst: Option<u32>,
    caller_ip: usize,
) -> Result<VmResult<()>, Prepared> {
    let Callee::Native(b, kind) = &prep.callee else {
        return Err(prep);
    };
    // A one-argument call `Vm::fast_builtin_call` evaluates without the
    // handler (`is_string()`, `count()`, ...): the same, in place.
    if prep.n == 1 && b.fast_eval_id() != crate::call::fe::NONE {
        if let Some(r) = vm.fast_eval_prepared(b, prep.calls_len, dst, caller_ip) {
            return Ok(r);
        }
    }
    // `ZEND_FRAMELESS_ICALL_n`: a function with a frameless class, called
    // on the caller's frame when its arguments allow it.
    if b.flags & flags::FRAMELESS != 0 {
        if let Some(r) = vm.frameless_icall_prepared(b, prep.n, prep.calls_len, dst, caller_ip) {
            return Ok(r);
        }
    }
    let n = prep.n;
    let live = vm.frames.live;
    {
        let sig = b.signature();
        let nparams = sig.params.len();
        let variadic = b.flags & flags::VARIADIC != 0;
        if n < sig.required as usize || (!variadic && n > nparams) {
            return Err(prep);
        }
        let (callers, pool) = vm.frames.vec.split_at(live);
        let (Some(caller), Some(frame)) = (callers.last(), pool.first()) else {
            return Err(prep);
        };
        if caller.calls.len() != prep.calls_len || frame.args.len() != n {
            return Err(prep);
        }
        // The types zpp accepts as they are (`call_builtin_impl`'s
        // positional fast path); anything else is coerced or rejected
        // there.
        for (i, arg) in frame.args.iter().enumerate() {
            let (Some(v), Some(p)) = (arg, sig.params.get(i).or(sig.params.last())) else {
                return Err(prep);
            };
            if let Some(ty) = &p.ty {
                if p.promote & crate::opcode::UNCHECKED_TYPE == 0
                    && !ty.is_mixed()
                    && !accepts_without_coercion(ty.mask, v)
                {
                    return Err(prep);
                }
            }
        }
    }
    let b = Rc::clone(b);
    let kind = *kind;
    // The handler's copies of the arguments (php-src's handler reads them
    // in the frame), then the declared defaults of the omitted trailing
    // parameters, not marked as passed.
    let mut values = vm.call_bufs.values.pop().unwrap_or_default();
    let mut passed = vm.call_bufs.passed.pop().unwrap_or_default();
    values.reserve(n);
    for arg in &vm.frames.vec[live].args {
        values.push(arg.as_ref().map_or(Value::Null, clone_value));
    }
    passed.resize(n, true);
    let sig = b.signature();
    let nparams = sig.params.len();
    let upto = if b.flags & flags::VARIADIC != 0 {
        nparams.saturating_sub(1)
    } else {
        nparams
    };
    while values.len() < upto {
        let i = values.len();
        match vm.builtin_default(&b, i) {
            Some(d) if i >= sig.required as usize => {
                values.push(d);
                passed.push(false);
            }
            _ => break,
        }
    }
    // `zend_vm_stack_push_call_frame` + `EG(current_execute_data) = call`:
    // the frame as `FrameStack::push_builtin` leaves it.
    {
        let frame = &mut vm.frames.vec[live];
        frame.reinit_for_builtin(&vm.empty_code);
        frame.func = Some(Func::Builtin(Rc::clone(&b)));
        frame.builtin_name = (kind == PlainCall::Named).then(|| b.name.clone());
        frame.observer_hidden = false;
    }
    // `SAVE_OPLINE()`: the handler may report the caller's line.
    vm.frames.vec[live - 1].ip = caller_ip;
    vm.frames.live = live + 1;
    vm.pending_call_dst = dst;
    vm.magic_call_name = None;
    let handler = b.handler;
    // A method's `$this` and called scope, readied on the frame by
    // `prepare_method` (a function's frame has none).
    let (this, called) = {
        let frame = &vm.frames.vec[live];
        (frame.this.clone(), frame.called.clone())
    };
    let mut args = Args {
        values,
        passed,
        named: Vec::new(),
        this,
        called,
        func: Some(b),
    };
    let mut r = handler(vm, &mut args);
    if r.is_ok() {
        if let Err(e) = vm.check_interrupt() {
            r = Err(e);
        }
    }
    let Args { values, passed, .. } = args;
    vm.put_val_vec(values);
    vm.put_bool_vec(passed);
    // Released by the handler, destructed while its frame is current.
    if crate::object::has_pending_destructors() && vm.frames.live == live + 1 {
        if let Err(e) = vm.run_pending_destructors() {
            r = match (r, e) {
                (Ok(_), e) => Err(e),
                (Err(Throw::Exception(old)), Throw::Exception(new)) => {
                    crate::exceptions::chain_previous(&new, &old);
                    Err(Throw::Exception(new))
                }
                (Err(old), _) => Err(old),
            };
        }
    }
    // Popped off the VM stack, frames left above it (none, normally)
    // first. Released in place (`release_native`), its argument buffer
    // stays with the pooled frame for the next call at this depth.
    if vm.frames.live > live + 1 {
        vm.frames.truncate(live + 1);
    }
    if vm.frames.live == live + 1 {
        // A method's `$this` and scopes go first, as `pop_frames_to`
        // releases them with the frame.
        let frame = &mut vm.frames.vec[live];
        if frame.this.is_some() || frame.scope.is_some() || frame.called.is_some() {
            frame.this = None;
            frame.scope = None;
            frame.called = None;
        }
    }
    if vm.frames.live == live + 1 && vm.frames.vec[live].release_native(&vm.empty_vars) {
        vm.frames.pop_cleared_top();
    } else {
        // Anything else as the generic call releases it.
        vm.pop_frames_to(live + 1);
    }
    Ok(r.map(|v| match dst {
        Some(d) => vm.set_tmp(d, v),
        None => {
            if matches!(
                v,
                Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
            ) {
                std::mem::forget(v);
            } else {
                drop(v);
            }
        }
    }))
}

/// The bound variables of the frame's closure (`use`), copied into their
/// compiled variables after the arguments, as `Vm::bind_closure_vars` does
/// once `Vm::push_simple_ucall` pushed the frame (the closure's `$this` and
/// scopes are the frame's already, and a plain call's closure has no
/// static variables).
///
/// Inline: out of line (as it came out once the tape's builtin call was
/// added) it cost every closure call ~20 instructions.
#[inline(always)]
#[allow(clippy::inline_always)]
fn bind_closure_vars(frame: &mut crate::vm::Frame) {
    let Some(c) = frame.closure.as_ref() else {
        return;
    };
    let internal = crate::object::data(c).internal.borrow();
    let Some(d) = internal
        .as_ref()
        .and_then(|x| x.downcast_ref::<crate::closure::ClosureData>())
    else {
        return;
    };
    for (cv, v) in &d.vars {
        if let Some(slot) = frame.cvs.get_mut(*cv as usize) {
            if let Some(old) = slot.as_mut().replace(v.clone()) {
                drop(old);
            }
        }
    }
}

/// Turns the prepared call into the [`PendingCall`] the generic
/// `InitFCall` and `Send`s would have built (the arguments sent so far in
/// its inline buffer), in its place among the caller's pending calls, and
/// returns the frame to the pool.
#[inline(never)]
pub(super) fn materialize(vm: &mut Vm, prep: Prepared) {
    // A quick call readied nothing: the pending call its `InitFCall` would
    // have built, from the call site's cache (resolved again if the cache
    // was cleared while the argument was computed: the builtin is still
    // there). Nothing newer is pending: a call started since would have
    // materialized this one first.
    if let Callee::Quick(_, init_at) = prep.callee {
        let Some(caller) = vm.frames.last_mut() else {
            return;
        };
        let Some(&Op::InitFCall { name, display, .. }) = caller.code.ops.get(init_at as usize)
        else {
            return;
        };
        if !crate::call::init_fcall_in(caller, name) {
            let made = vm.op_init_fcall(name, None, display);
            debug_assert!(made.is_ok(), "a builtin's call site resolves");
        }
        return;
    }
    let live = vm.frames.live;
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return;
    };
    let uf = match prep.callee {
        Callee::User(uf) => uf,
        // (Returned above.)
        Callee::Quick(..) => return,
        Callee::Native(b, _) => {
            // `InitFCall` (or `InitMethodCall`) and the `Send`s of the
            // generic protocol: a direct call (a method's: with `$this`
            // and its scopes) with its arguments inline. The frame was not
            // readied.
            let mut call = PendingCall {
                direct: prep.fcall,
                this: frame.this.take(),
                scope: frame.scope.take(),
                called: frame.called.take(),
                ..PendingCall::new(Func::Builtin(b))
            };
            for (arg, sent) in call.inline_args.iter_mut().zip(frame.args.drain(..)) {
                *arg = sent;
            }
            #[allow(clippy::cast_possible_truncation)] // at most `MAX_ARGS`
            {
                call.inline_count = prep.n as u8;
            }
            let at = prep.calls_len.min(caller.calls.len());
            caller.calls.insert(at, call);
            return;
        }
    };
    // (A variadic function's extra arguments are in the frame's argument
    // buffer, at their positions: `send`.)
    let bound = uf.plain_params().min(prep.n);
    let mut call = PendingCall {
        direct: prep.fcall,
        dynamic: prep.closure,
        is_ctor: prep.ctor,
        this: frame.this.take(),
        scope: frame.scope.take(),
        called: frame.called.take(),
        closure: frame.closure.take(),
        ..PendingCall::new(Func::User(uf))
    };
    for (arg, slot) in call.inline_args.iter_mut().zip(&mut frame.cvs[..bound]) {
        *arg = slot.as_mut().take();
    }
    for (arg, sent) in call.inline_args[bound..prep.n]
        .iter_mut()
        .zip(frame.args.iter_mut().skip(bound))
    {
        *arg = sent.take();
    }
    #[allow(clippy::cast_possible_truncation)] // at most `MAX_ARGS`
    {
        call.inline_count = prep.n as u8;
    }
    let at = prep.calls_len.min(caller.calls.len());
    caller.calls.insert(at, call);
    frame.clear_for_reuse(&vm.empty_code);
}

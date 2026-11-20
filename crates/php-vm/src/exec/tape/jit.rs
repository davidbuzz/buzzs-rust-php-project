// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The trace JIT: whole loops of the tape compiled to machine code.
//!
//! The tape removed the per-op decode of `Op`; its remaining cost is
//! per-op `match` dispatch, and measurements put a taped loop step at
//! about 40 instructions against php-src's 17
//! (`perf/NOTE-jit-2026-11-16.md` has the numbers, and why intercepting
//! the ops one by one is a dead end). The way to the 17 is to stop
//! interpreting: compile the loop.
//!
//! # The trace
//!
//! A trace is the contiguous run of tape instructions of one hot loop:
//! from the target of a hot backward jump (the loop head) through the
//! jump back (the closing instruction). `plan` finds it, checks every
//! instruction against the whitelist (`Jmp`, `++`/`--`, the small-int
//! and int-literal assignments, the fused comparison-and-branch, the
//! fused `++`-compare, and the fused `+ - *` into a variable), and
//! `emit` writes one region of x86-64 code that runs the whole loop,
//! jumping back internally at the close. Nothing else is compiled in
//! this version; a region with anything else in it is not compiled.
//!
//! # The memory model
//!
//! The compiled region keeps the frame's slots (the compiled variables
//! at `st.cvs`, the temporaries at `st.tmps`) fully up to date: every
//! write it makes stores through to the slot, payload first and tag
//! second. So the machine code *is* the interpreter's memory, and a
//! side exit needs no fix-up: it returns the index of the instruction
//! to resume at, and the interpreter picks up from the same slots.
//!
//! Guards - the checks that the slots the loop assumes are ints really
//! are ints - run once at the entry of each compiled call, not per
//! iteration. That is sound because a guard that held at the head
//! cannot be broken by the region itself: every whitelisted write to a
//! guard-checked slot either keeps it an int or is preceded by the
//! same checks the interpreter runs, and any write from outside the
//! region (a call, an op the interpreter ran) happens at a head
//! boundary, where the guards run again. A slot only written (never
//! read as an int) needs only the weak guard: the interpreter's own
//! test that the slot holds nothing reference-counted.
//!
//! # The exit protocol
//!
//! The compiled function has the C signature `fn(cvs, tmps) -> i64`.
//! It returns a tape instruction index to resume at (a guard, poll,
//! iteration-cap or arithmetic-overflow exit), and `back_edge`
//! turns that into the `ip` the interpreter continues at. A poll exits
//! at the head, so the interpreter runs one iteration, reaches the
//! interpreted back edge, and services the safe point there.
//!
//! # The tables
//!
//! Each tape owns a [`Tables`]: per instruction, a back-edge hit count
//! and a trace cell (0 = none, `usize::MAX` = refused, else the
//! compiled function's address as a `usize`). [`back_edge`], run
//! by `take_jump!` at every taken backward jump, counts and compiles.
//! Compiled regions live in fresh `mmap`ed pages and are never freed
//! (a program's loops are few, and unloading machine code a thread may
//! be running is a solved problem this version does not need).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use super::{src, tmp_off, Inst, St, TOp, Tape, KC, KI, KL, KR, KT, SLOT_SHIFT};
use crate::func::{Func, FUNC_TABLE_VERSION};
use crate::opcode::PROP_CACHE_THIS;
use crate::opcode::{ArgSrc, ClassSpec, FetchMode, Op, Operand, PlaceBase};
use crate::place::propcache::{PC_DYN, PC_ISSET, PC_READ, PC_WRITE};
use crate::types::mask;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;

/// Whether a hook observer is installed (set once by `Vm::set_observer`).
/// An observed call runs the function's `pass_two` observers on entry and
/// exit, which an inlined call cannot, so plans with calls refuse one
/// while this is set. (Enabling an observer *during* a trace is a
/// documented limitation: the trace keeps running inlined.)
pub(crate) static OBSERVER_ON: AtomicBool = AtomicBool::new(false);

/// `plan_call`'s marker for a callee `return` of `Operand::Unused`
/// (`return;`): the trace stores a null into the caller's result slot.
const KNULL_ARG: u8 = 5;
/// `plan_call`'s marker for a caller-side `Send` that can be left out:
/// the callee's inlined body never mentions that untyped parameter (or
/// the caller passed more arguments than the callee declares, and the
/// plain call discards the extras), so there is nothing to store,
/// whatever the caller-side value was. A dropped send of a compiled
/// variable keeps the variable's slot in `off` with `lit == 1`: `plan`
/// guards that it is defined (the interpreted send warns when it is
/// not).
const KSKIP: u8 = 6;
/// `plan_call`'s receiver kind for a method call whose receiver is the
/// frame's `$this` (`InitMethodCall` with `Operand::Unused`): not a
/// slot, read through the frame pointer instead (see `THIS_FIELD`).
const KTHIS: u8 = 7;
/// The most arguments an inlined call may pass (the plain-call path's
/// inline argument buffer, `calls.rs`'s `MAX_ARGS`).
const MAX_CALL_ARGS: usize = 6;

/// A `Value`/`Option<Value>` slot's tag byte (offset 0 of the slot): the
/// enum discriminant, pinned by the layout tests' `value_layout` below.
const TAG_NULL: u8 = 0;
const TAG_BOOL: u8 = 1;
const TAG_INT: u8 = 2;
const TAG_STR: u8 = 4;
const TAG_OBJECT: u8 = 6;
/// Byte offset of an `Obj` handle's payload (`ObjectData`) in its
/// allocation, from the handle pointer (the `ObjCell<T>` header):
/// computed by `php-runtime`'s `obj_payload_offset` from the type itself.
const OBJ_PAYLOAD: u32 =
    php_runtime::value::obj_payload_offset::<crate::object::ObjectData>() as u32;
/// Byte offset of `ObjectData::class`, the `Rc<Class>` field.
const CLASS_FIELD: u32 = std::mem::offset_of!(crate::object::ObjectData, class) as u32;
/// Distance from an `Rc`'s allocation base (the raw field word, its
/// `NonNull` handle) to `Rc::as_ptr`, the value: the inner box's two
/// reference-count words. Pinned by `rc_inner_prefix_size` below.
const RC_VALUE_OFFSET: u32 = 2 * std::mem::size_of::<usize>() as u32;
/// Byte offset of `Frame::this`, the `Option<Obj>`: `None` is a null
/// pointer (`Option<NonNull>`'s niche, pinned by the layout tests'
/// `option_pointers_are_null`).
const THIS_FIELD: u32 = std::mem::offset_of!(crate::vm::Frame, this) as u32;
/// Byte offset of `Frame::scope`, the `Option<Rc<Class>>` calling scope:
/// `None` is a null pointer, else the `Rc`'s allocation handle.
const SCOPE_FIELD: u32 = std::mem::offset_of!(crate::vm::Frame, scope) as u32;
/// Byte offset of `Frame::called`, the running function's address: a
/// `static::`-resolved static property or class constant is the *running
/// function's* class, so the entry is only usable when this still names
/// the same function the plan resolved it in.
const CALLED_FIELD: u32 = std::mem::offset_of!(crate::vm::Frame, called) as u32;
/// Byte offset of `ObjectData::slots` (the inline `Slots`, whose field
/// layout mirrors `RefCell<...>`'s).
const OBJ_SLOTS: u32 = OBJ_PAYLOAD + std::mem::offset_of!(crate::object::ObjectData, slots) as u32;
/// Where the `Slots` borrow counter, slot-array pointer and slot count
/// live inside `ObjectData::slots`.
const SLOTS_BORROW: u32 = OBJ_SLOTS + crate::object::SLOTS_BORROW_OFF as u32;
const SLOTS_FIRST: u32 = OBJ_SLOTS + crate::object::SLOTS_FIRST_OFF as u32;
const SLOTS_LEN: u32 = OBJ_SLOTS + crate::object::SLOTS_LEN_OFF as u32;
/// Byte offset of `ObjectData::lazy` (the inline
/// `RefCell<Option<Box<LazyInfo>>>`): `None` is a null pointer, and a
/// borrowed cell has a non-null borrow source.
const OBJ_LAZY: u32 = OBJ_PAYLOAD + std::mem::offset_of!(crate::object::ObjectData, lazy) as u32;
/// Inside an inline `RefCell<T>`: the borrow source at 0, the value at 8.
const REFCELL_BORROW: u32 = 0;
const REFCELL_VALUE: u32 = 8;
/// `Option::None`: a `Value` tag never reaches it, and its niche is the
/// all-ones tag word, so the tag byte reads as this.
const TAG_NONE: u8 = 9;
/// The byte offset of an `Int` payload (the slot's `i64`) in its slot.
const PAYLOAD: u32 = 8;
/// The byte offset of a `Bool`'s value in its slot: the `bool` is the
/// byte right after the tag (the variant's one field, of alignment 1),
/// *not* the low byte of the payload word an `Int` uses. Every row that
/// reads or writes a `Bool` goes through this (the layout tests'
/// `value_layout` pins it).
const BOOL_BYTE: u32 = 1;
/// `size_of::<Inst>()`: a jump operand is a signed byte offset, and the
/// plan works in instruction indexes.
const INST_SIZE: i64 = 32;

/// The JIT's view of the tape a `run_tape` call is running: the tables
/// [`back_edge`] hooks the tape's backward jumps with, kept *out* of the
/// interpreter's state struct (`tape::St`, which stays tiny so its hot
/// loop keeps everything in registers) and reached through one raw
/// pointer there.  The count/trace cells are [`Cell`]s of *const
/// pointers so a dead trace can disarm the current frame through a
/// shared reference.
pub(crate) struct JitCtx {
    /// Per-instruction back-edge counters (null once tracing is dead).
    jcnt: Cell<*const AtomicI32>,
    /// Per-instruction trace cells: 0 = none, `usize::MAX` = refused,
    /// else the entry address (null once tracing is dead).
    jtr: Cell<*const AtomicUsize>,
    /// The tape's instruction array and its length.
    tbase: *const Inst,
    tlen: usize,
    /// The [`Tape`] the tables belong to (null if there are none): used
    /// to kill a tape's tracing once it proves useless.
    tape: *const Tape,
    /// The row a compiled loop's poll exit belongs to (null when its
    /// last exit was no poll): `take_jump!` reports the safe point there.
    poll_row: Cell<*const Inst>,
}

impl JitCtx {
    /// A context for `tape` (already decoded to `tape_slice`), reading
    /// its eagerly-created tables; `None` when the JIT is off (checked
    /// with one load), the tape has no loop the JIT could compile
    /// ([`has_loop_close`]: it got no tables), or [`Tape::jit_kill`] ran.
    #[inline]
    pub(crate) fn maybe_for_tape(tape: &Tape, tape_slice: &[Inst]) -> Option<JitCtx> {
        if !enabled() {
            return None;
        }
        // SAFETY: the loads pair with [`Tape`]'s stores at decode; a
        // non-null table outlives the tape's cell's `Box` until the tape
        // itself is dropped (after every running frame of it).
        let (cnt, tr) = {
            use std::sync::atomic::Ordering::Relaxed;
            (
                tape.jit_cnt.load(Relaxed) as *const AtomicI32,
                tape.jit_tr.load(Relaxed) as *const AtomicUsize,
            )
        };
        if cnt.is_null() || tr.is_null() {
            return None;
        }
        Some(JitCtx {
            jcnt: Cell::new(cnt),
            jtr: Cell::new(tr),
            tbase: tape_slice.as_ptr(),
            tlen: tape_slice.len(),
            tape: std::ptr::from_ref(tape),
            poll_row: Cell::new(std::ptr::null()),
        })
    }

    #[inline(always)]
    pub(crate) fn alive(&self) -> bool {
        !self.jcnt.get().is_null()
    }

    /// The instruction a safe point that came due inside the compiled
    /// loop belongs to: the row the trace named (a row that allocates, a
    /// nested loop's back edge), else `jump`, the back edge that entered
    /// the trace. Read once: the row is forgotten.
    #[inline]
    pub(crate) fn poll_at(&self, jump: *const Inst) -> *const Inst {
        let row = self.poll_row.replace(std::ptr::null());
        if row.is_null() {
            jump
        } else {
            row
        }
    }

    /// Stop hooking this frame's back edges (the tape's tracing is dead).
    fn disarm(&self) {
        self.jcnt.set(std::ptr::null());
        self.jtr.set(std::ptr::null());
    }
}

/// A backward jump onto instruction `close`'s target compiles the loop
/// once `close` has been taken this many times.
const HOT_LIMIT: i32 = 1_000;
/// How many times a loop is planned before it is refused for good, when
/// what refused it was a run-time cache that was not filled yet (a call
/// or a property in a branch no iteration had taken): each refusal makes
/// the next count eight times as long (1,000, then 9,000, then 73,000
/// back edges). A refusal for the loop's shape is final at once.
const MAX_ATTEMPTS: usize = 3;
/// [`attempt`]'s "refused for now": worth planning again later.
const REFUSED_FOR_NOW: usize = usize::MAX - 1;

thread_local! {
    /// Set by a planner that refuses for a cache miss ([`soft`]), read by
    /// [`attempt`] after a refused plan.
    static PLAN_SOFT: Cell<bool> = const { Cell::new(false) };
}

/// A planner's refusal for a run-time cache that holds no entry (yet) for
/// what the row needs: the property, method or function cache of an op
/// that has not run, a constant not fetched so far, an entry filled for
/// another calling scope. Marks the refusal as worth another attempt and
/// gives the `None` to return.
fn soft<T>() -> Option<T> {
    PLAN_SOFT.with(|s| s.set(true));
    None
}

/// [`HOT_LIMIT`], or the count `BUZZS_JIT_HOT` names (read once): a test
/// run sets it low so that even the shortest loops of a test suite are
/// compiled.
fn hot_limit() -> i32 {
    static HOT: OnceLock<i32> = OnceLock::new();
    *HOT.get_or_init(|| {
        std::env::var("BUZZS_JIT_HOT")
            .ok()
            .and_then(|v| v.parse::<i32>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(HOT_LIMIT)
    })
}
/// The largest region compiled (instructions, counting an inlined call as
/// its full span); larger loops stay interpreted in this version.
const MAX_REGION: usize = 128;
/// The largest inlined call body.
const MAX_CALLEE: usize = 48;
/// The largest number of guards one region runs.
const MAX_GUARDS: usize = 32;
/// The most traces compiled by the process (each is a page pair; this
/// bounds the arena).
const MAX_TRACES: usize = 4096;
/// Iterations one compiled call runs before exiting to poll (the
/// interpreter then runs one iteration and re-enters).
const ITER_CAP: u32 = 1 << 24;

/// Whether the JIT is enabled for this process (read once): on by default
/// where the emitted code can run - x86-64 Linux, the platform of the
/// emitter's encodings and of the `mmap` constants below - and switched
/// off with `BUZZS_JIT=0`.
///
/// It used to be opt-in (`BUZZS_JIT=1`), which left every ordinary run,
/// and the acceptance check itself (`perf/bench/acceptance.py` sets no
/// such variable), measuring the interpreter alone.
#[inline]
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        cfg!(all(target_arch = "x86_64", target_os = "linux"))
            && std::env::var_os("BUZZS_JIT").is_none_or(|v| v != "0")
    })
}

/// Whether `tape` has an instruction that could close a compiled loop: a
/// row of a jump class the plan accepts as a region's close (`Jmp`, the
/// fused compare-and-branch, the fused `++`-compare) that jumps backward.
/// `plan` refuses a region closed by anything else, so a tape without one
/// can never trace: it gets no tables, and its frames run the
/// interpreter's instantiation without the back-edge hook at all (a
/// function with no loop, recursive ones included, pays nothing for the
/// JIT being on).
pub(crate) fn has_loop_close(tape: &[Inst]) -> bool {
    tape.iter().any(|inst| {
        // Onto an earlier instruction, or onto itself (an empty loop).
        #[allow(clippy::cast_possible_wrap)]
        let backward = (inst.c as i32) <= 0;
        backward
            && (inst.op == TOp::Jmp
                || inst
                    .op
                    .jit_meta()
                    .is_some_and(|m| m.class == J_CMP || m.class == J_INCCMP))
    })
}

/// The per-tape hot-loop tables, created on the tape's first hot back
/// edge: per instruction, the backward-hit count and the trace cell.
pub(crate) struct Tables {
    cnt: Box<[AtomicI32]>,
    tr: Box<[AtomicUsize]>,
}

impl Tables {
    pub(crate) fn new(n: usize) -> Tables {
        Tables {
            cnt: (0..n).map(|_| AtomicI32::new(0)).collect(),
            tr: (0..n).map(|_| AtomicUsize::new(0)).collect(),
        }
    }

    pub(crate) fn cnt(&self) -> *const AtomicI32 {
        self.cnt.as_ptr()
    }

    pub(crate) fn tr(&self) -> *const AtomicUsize {
        self.tr.as_ptr()
    }
}

/// JIT classes: the fixed whitelist ops, and the fused family rows
/// whose details `TOp::jit_meta` reports in a [`JitMeta`].
pub(crate) const C_JMP: u16 = 1;
pub(crate) const C_INC: u16 = 2;
pub(crate) const C_DEC: u16 = 3;
pub(crate) const C_ASSIGN_I: u16 = 4;
pub(crate) const C_ASSIGN_L: u16 = 5;
pub(crate) const J_CMP: u16 = 10;
pub(crate) const J_INCCMP: u16 = 11;
pub(crate) const J_ARITH_CV: u16 = 12;
pub(crate) const J_ASSIGN_T: u16 = 13;
pub(crate) const J_ARITH_TMP: u16 = 14;
pub(crate) const J_ASSIGN_OP: u16 = 15;
pub(crate) const J_NUMBIN_T: u16 = 16;
pub(crate) const J_NUMBIN_C: u16 = 17;
/// A plain user-function call inlined into the trace (see `CallPlan`).
const C_CALL: u16 = 18;
/// A plain instance-property read: `$obj->name` through the op array's
/// property cache, with the receiver class and the frame's calling scope
/// re-checked at run time (see `plan_prop`).
const C_PROP: u16 = 19;
/// A plain instance-property write: `$obj->name = int` through the op
/// array's property cache, with the receiver class, the frame's calling
/// scope and the slot's current `Int` tag re-checked at run time (see
/// `plan_prop_write`).
const C_PROPW: u16 = 20;
/// A plain instance-property `++`/`--` on an `Int` slot: the property
/// guards, then an `add`/`sub` that exits `jo` *before* any store when
/// the int overflows (the interpreted row then converts or reports as
/// it decides; see `plan_prop_incdec`).
const C_PROPINC: u16 = 21;
/// A plain instance-property `isset()`/`empty()` on a declared slot:
/// the property guards up to the slot array, then a branch on the
/// slot's tag (`Int`, `Null` or `Bool` answered inline, anything else
/// the interpreted row) storing a clean `Bool` into the destination
/// temporary (see `plan_prop_isset`).
const C_PROPISSET: u16 = 22;
/// A plain instance-property compound write (`+=`/`-=`/`*=`/`&=`/`|=`/
/// `^=` on an `Int` slot by a number): the property guards, then the
/// arithmetic into `r9`, exiting `jo` before any store on int overflow
/// (the interpreted row then converts to `Float` where the property's
/// type allows; see `plan_prop_opa`).
const C_PROPOPA: u16 = 23;
/// A plain static-property read (`Foo::$x`, `self::$x`): the run-time
/// cache's entry is found at plan time, its `Rc<RefCell<Value>>` cell
/// pinned, and the row re-checks the frame's calling scope (and the
/// running function for a `static` spec) before reading the cell's
/// payload (see `plan_sprop`).
const C_SPROP: u16 = 24;
/// A plain static-property write (`Foo::$x = int`): the same pinned
/// cell and guards, plus the entry's `fast_plain` flag (an untyped
/// property, so no coercion) and the cell's current `Int` tag, so only
/// the payload word is stored (see `plan_sprop_write`).
const C_SPROPW: u16 = 25;
/// A plain `isset(Foo::$x)`/`empty(Foo::$x)`: the pinned cell, then a
/// branch on its tag (`Int`, `Null` or `Bool` answered inline, anything
/// else the interpreted row) storing a clean `Bool` into the
/// destination temporary (see `plan_sprop_isset`).
const C_SPROPISSET: u16 = 26;
/// A constant or class constant the plan folded to an `Int`, a `null` or
/// a `Bool`: the row stores that value, guarded only when the op resolves
/// the constant through the frame's scope (a class constant) — see
/// `plan_const`.
const C_CONSTI: u16 = 27;
/// An array element read with an int offset (`$t = $cv[$i]`, or
/// `$v = $cv[$i]` folded with the assignment after it): the row calls
/// [`jit_dim_get`], which stores an `Int` element and declines anything
/// else, leaving the row to the interpreter (see `plan_dim`).
const C_DIMR: u16 = 28;
/// An array element write with an int offset, or an append
/// (`$cv[$i] = int`, `$cv[] = int`), through [`jit_dim_set`].
const C_DIMW: u16 = 29;
/// A compound array element write (`$cv[$i] += int` and the other
/// int-int operators of [`jit_dim_op`]).
const C_DIMOP: u16 = 30;
/// `$cv = a op b` for the operators of the generic numeric rows
/// (`NumBinC`: the `Bin` and the `AssignCv` after it, the destination
/// variable in `inst.d`).
pub(crate) const J_NUMBIN_D: u16 = 31;
/// `abs($x)` of an int (`AbsN`): the absolute value into the result
/// temporary, when there is one; `PHP_INT_MIN`, whose magnitude is a
/// float, is the interpreted row's. The row covers the op slot after it.
const C_ABS: u16 = 32;
/// `Nop` (a slot an op folded into its neighbour left behind): no code.
const C_NOP: u16 = 33;
/// `JitMeta.db` for a property write whose result is discarded (no
/// destination temporary).
const NO_DST: u32 = u32::MAX;

/// The number of tape instructions one whitelisted class consumes
/// (fused rows cover the ops they merge).
pub(crate) fn c_step(c: u16) -> usize {
    if c == J_CMP || c == J_ARITH_CV || c == J_NUMBIN_D || c == C_ABS {
        2
    } else if c == J_INCCMP {
        3
    } else {
        1
    }
}

/// The arithmetic of a fused family row: 0 = `+`, 1 = `-`, 2 = `*`.
macro_rules! jit_arith_id {
    (Add) => {
        0u8
    };
    (Sub) => {
        1u8
    };
    (Mul) => {
        2u8
    };
}
pub(crate) use jit_arith_id;

/// What a whitelisted instruction the plan carries for `emit`: its
/// class, the operand kinds of the fused rows (`KC`/`KT`/`KI`/`KL`),
/// the comparison mask, the arithmetic id, the `++`/`--` direction,
/// and the `KL` operands resolved to their int values at plan time.
#[derive(Clone, Copy)]
pub(crate) struct JitMeta {
    pub(crate) class: u16,
    pub(crate) ka: u8,
    pub(crate) kb: u8,
    /// The *destination* kind of an arithmetic row (`J_ARITH_CV` /
    /// `J_ASSIGN_OP` write a compiled variable, `J_ARITH_TMP` /
    /// `J_NUMBIN_T` a temporary); `classify` sets it, and an inlined
    /// call's plan remaps it to `KR` like the source kinds.
    pub(crate) kc: u8,
    pub(crate) mask: u8,
    pub(crate) arith: u8,
    pub(crate) inc: bool,
    pub(crate) lit_a: i64,
    pub(crate) lit_b: i64,
    /// `C_PROP`: byte offset of the base's compiled-variable slot (0 when
    /// the base is `$this`, whose kind is `KTHIS`).
    pub(crate) sb: u32,
    /// `C_PROP`: byte offset of the destination temporary.
    pub(crate) db: u32,
    /// `C_PROP`: the property's slot index in the object.
    pub(crate) slot: u32,
    /// `C_PROPW`: the written value's kind (`KC` a compiled variable,
    /// `KT` a temporary, `KL` an int literal resolved into `lit_c`).
    pub(crate) sc: u8,
    /// `C_PROPW`: byte offset of the written value's slot (unused for
    /// `KL`).
    pub(crate) sw: u32,
    /// `C_PROPW`: the written literal's int value (`sc == KL`).
    pub(crate) lit_c: i64,
    /// `C_PROPW`: the source is an `Operand::Tmp`: null it out after
    /// the write, as the interpreted row does.
    pub(crate) consume: bool,
    /// `C_SPROP*`/`C_CONSTI`: also guard `Frame::called` (the op names
    /// its class with `static::`, whose resolution follows the running
    /// function, not just the frame's scope).
    pub(crate) cg: bool,
    /// `C_SPROP*`/`C_CONSTI`: the pinned `Frame::called` word
    /// (`cg`; 0 = the frame has no calling function).
    pub(crate) wc: i64,
    /// `C_SPROP*`/`C_CONSTI`: whether the emitted code checks the
    /// frame's calling scope against `lit_b` at all.  A public named
    /// entry (`Foo::$x` from any scope) matches every scope, so no
    /// check is needed and none is emitted.
    pub(crate) sg: bool,
}

/// The arithmetic ids `C_PROPOPA` accepts in `arith`: `jit_arith_id!`
/// covers `+`/`-`/`*` (0/1/2) and [`op_arith`] adds `%` (5), so the
/// bitwise ops use the free 6/7/8. Div, Mod, Pow, Concat, the shifts
/// and the boolean operators have no int-int inline form.
/// `JitMeta.mask` flag for a `J_ASSIGN_T` row whose source temporary is
/// one an `isset()`/`empty()` row of the same region stored: the tag
/// written into the compiled variable is `Bool`, not `Int`.
const BOOL_SRC: u8 = 0x80;
/// `JitMeta.mask` flag for a `J_ASSIGN_T` row whose source temporary
/// holds the `Null` of an inlined call that returned nothing (`return;`):
/// the row stores a `Null` into the compiled variable.
const NULL_SRC: u8 = 0x40;

const OPA_BITAND: u8 = 6;
const OPA_BITOR: u8 = 7;
const OPA_BITXOR: u8 = 8;
/// What a folded constant is (`C_CONSTI`'s `JitMeta.arith`): an `Int`
/// (`lit_a` its value), `null`, or a `Bool` (`lit_a` 0 or 1).
const CONST_INT: u8 = 0;
const CONST_NULL: u8 = 1;
const CONST_BOOL: u8 = 2;
/// The shifts of the generic numeric rows: `<<` and `>>` by a count in
/// `0..=63` (any other count exits to the interpreted row, which gives
/// the 0 or -1 of a too wide shift or throws for a negative one).
const ARITH_SHL: u8 = 9;
const ARITH_SAR: u8 = 10;

impl Default for JitMeta {
    fn default() -> JitMeta {
        JitMeta {
            class: 0,
            ka: KC,
            kb: KC,
            kc: KC,
            mask: 0,
            arith: 0,
            inc: false,
            lit_a: 0,
            lit_b: 0,
            sb: 0,
            db: 0,
            slot: 0,
            sc: KC,
            sw: 0,
            lit_c: 0,
            consume: false,
            cg: false,
            wc: 0,
            sg: true,
        }
    }
}

/// One entry guard: slot (`base` 0 = `cvs`, 1 = `tmps`; byte offset) and
/// what it must hold. `exact`: an `Int` (the slot is read as one).
/// `plain`: anything the interpreter's own store test accepts, a scalar
/// or nothing (the slot is only stored into). `defined`: any value at
/// all, but not nothing (the variable is read by a send the trace drops:
/// reading an undefined one warns, which only the interpreter does).
/// `exact` implies the other two; `plain` and `defined` together mean a
/// scalar.
#[derive(Clone, Copy)]
struct Guard {
    base: u8,
    off: u32,
    exact: bool,
    plain: bool,
    defined: bool,
}

/// One operand of an inlined call (an argument or the callee's return
/// value) as the *caller* sees it: an `Inst`-style kind/offset pair, with
/// `KC`/`KT`/`KL` meaning the caller's compiled variable, temporary or
/// literal, and `lit` the resolved value of a `KL` (or the marker value
/// of a `KNULL_ARG` or `KSKIP`).
#[derive(Clone, Copy)]
struct Arg {
    kind: u8,
    off: u32,
    lit: i64,
}

/// A plain user-function call inlined into a trace: the callee's whole
/// straight-line body, its slots addressed off the trace's scratch buffer
/// (`KR`, based at `base`), surrounded by the argument stores (the
/// caller-side `Send` rows, at the scratch slots of the callee's
/// parameters) and the copy of the return value into the caller's `DoCall`
/// result slot. The `Inst` rows were decoded from the callee's tape at
/// plan time, and their slot kinds remapped to `KR` with `set_base`; the
/// `Rc` keeps the callee's tape alive as long as the plan is.
struct CallPlan {
    // Never read: it keeps the callee's `UserFunction` (and its tape and
    // op array, which the decoded rows were taken from) alive as long
    // as the plan.
    #[allow(dead_code)]
    uf: Rc<crate::func::UserFunction>,
    /// The closing instruction to resume at (disarmed) if the callee's
    /// code changes under the trace (the function-table guard).
    callove: usize,
    /// Region instructions the call covers: the `CallInit` row, the
    /// `Send` rows and the `DoCall` row.
    span: usize,
    sends: Vec<Arg>,
    /// The `DoCall`'s result temporary (`None` = discarded).
    dst: Option<u32>,
    classes: Vec<u16>,
    metas: Vec<JitMeta>,
    rows: Vec<Inst>,
    /// The callee's `Return` operand, as the caller sees it.
    ret: Arg,
    /// An instance method call's receiver guard: the receiver slot
    /// (kind, offset) and the class address the plan resolved from the
    /// site's `method_cache` entry (`None` for a plain or static call,
    /// whose callee the function-table version guard covers). The slot
    /// kind is `KC`/`KT` for a compiled-variable or temporary receiver,
    /// or [`KTHIS`] for `$this` (offset unused, read through the frame
    /// pointer).
    recv: Option<(u8, u32, u64)>,
    /// With `recv`: the calling scope the site's `method_cache` entry was
    /// filled for, as the raw `Frame::scope` field word (0 = no scope).
    /// The entry answers only that scope (`prepare_method` compares it on
    /// every call: another scope may see another method of the same
    /// name, or none), so the emitted guard re-checks it.
    recv_scope: i64,
    /// The call's scratch slice base (set once the buffer is mapped).
    base: u32,
    /// The scratch slice size.
    size: u32,
}

impl CallPlan {
    /// Base every `KR` slot offset of the inlined body at `base` (the
    /// call's slice of the trace's scratch buffer).
    fn set_base(&mut self, base: u32) {
        self.base = base;
        let remap = |kind: u8, off: &mut u32| {
            if kind == KR {
                *off = off.wrapping_add(base);
            }
        };
        for (m, inst) in self.metas.iter().zip(&mut self.rows) {
            remap(m.ka, &mut inst.a);
            remap(m.kb, &mut inst.b);
            remap(m.kc, &mut inst.c);
        }
        // The returned slot is one of this call's scratch slots as well:
        // left unbased, every call after the region's first would copy
        // its result out of the *first* call's slice.
        if self.ret.kind == KR {
            self.ret.off = self.ret.off.wrapping_add(base);
        }
    }
}

/// What a temporary the region itself fully stored holds, for the rows
/// after it (see `plan`'s `defs`/`bools`/`nulls`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum TmpKind {
    /// An `Int`: later reads need no entry guard.
    Int,
    /// A `Bool` (an `isset()`/`empty()` result): fresh to copy, never an
    /// int to compute with.
    Bool,
    /// The `Null` of an inlined call that returned nothing.
    Null,
    /// Consumed (nulled by the row that read it) or otherwise unknown.
    Unknown,
}

/// Record that a region row just left `kind` in temporary `off`: the
/// temporary leaves the other lists, so a slot once a `Bool` and then
/// re-stored as an `Int` (or the other way round) is only what its last
/// store made it.
fn set_tmp_kind(
    defs: &mut Vec<u32>,
    bools: &mut Vec<u32>,
    nulls: &mut Vec<u32>,
    off: u32,
    kind: TmpKind,
) {
    defs.retain(|d| *d != off);
    bools.retain(|d| *d != off);
    nulls.retain(|d| *d != off);
    match kind {
        TmpKind::Int => defs.push(off),
        TmpKind::Bool => bools.push(off),
        TmpKind::Null => nulls.push(off),
        TmpKind::Unknown => {}
    }
}

/// What one hot loop compiles to. `plan` builds it or refuses.
struct Plan {
    head: usize,
    close: usize,
    /// Per region instruction: 0 = covered by a fused op before it
    /// (skipped), else its class.
    class: Vec<u16>,
    meta: Vec<JitMeta>,
    guards: Vec<Guard>,
    /// Region instructions a mid-region jump lands on (they get a
    /// label); sorted, and never the head or a covered slot.
    #[allow(dead_code)] // the plan's shape record (every label is emitted)
    entries: Vec<usize>,
    /// Per region instruction: the region instructions its class covers
    /// (1, 2 or 3 for the fused rows; the call's full span for `C_CALL`).
    span: Vec<u32>,
    /// The inlined calls, indexed by region instruction.
    calls: Vec<Option<CallPlan>>,
    has_calls: bool,
    /// Some inlined call guards `$this`: the frame pointer (the third
    /// entry argument, `rdx`) is parked in `r10` by the prologue.
    frame: bool,
    /// The function table's version when the plan was made: the trace
    /// disarms itself when it runs with a different one (the guard on
    /// `FUNC_TABLE_VERSION`, emitted when `has_calls` or some row pins
    /// a runtime-cache entry, `version_guard`).
    version: u64,
    /// Some row pins a runtime-cache entry (a static-property cell or a
    /// folded constant): the prologue checks `FUNC_TABLE_VERSION`, which
    /// every cache clear bumps.
    version_guard: bool,
}
// ----------------------------------------------------------------- plan

/// The class and meta of one instruction, or `None` if the whitelist
/// does not cover it. `KL` operands are resolved to their int values
/// here (a plan only compiles int literals).
fn classify(st: &St, inst: &Inst) -> Option<(u16, JitMeta)> {
    let mut m = match inst.op {
        TOp::Jmp => JitMeta {
            class: C_JMP,
            ..JitMeta::default()
        },
        TOp::IncC => JitMeta {
            class: C_INC,
            ka: KC,
            ..JitMeta::default()
        },
        TOp::DecC => JitMeta {
            class: C_DEC,
            ka: KC,
            ..JitMeta::default()
        },
        TOp::AssignI => JitMeta {
            class: C_ASSIGN_I,
            ka: KC,
            kb: KI,
            ..JitMeta::default()
        },
        TOp::AssignL => JitMeta {
            class: C_ASSIGN_L,
            ka: KC,
            kb: KL,
            ..JitMeta::default()
        },
        // `$cv = TMP` (the int path: the source tmp is guarded exact).
        TOp::AssignT => JitMeta {
            class: J_ASSIGN_T,
            ka: KC,
            kb: KT,
            ..JitMeta::default()
        },
        // The generic numeric rows carry the operator (`e`) and the
        // operand kinds (`k`) inline; the JIT compiles the arithmetic
        // subset only when `jit_meta`'s ids cover `e`.
        TOp::NumBinT => {
            let (ka, kb) = (inst.k >> 4, inst.k & 0xf);
            JitMeta {
                class: J_NUMBIN_T,
                ka,
                kb,
                arith: op_arith(inst.e)?,
                ..JitMeta::default()
            }
        }
        TOp::NumAssignOpC => {
            let (ka, kb) = (inst.k >> 4, inst.k & 0xf);
            if ka != KC {
                return None;
            }
            JitMeta {
                class: J_NUMBIN_C,
                ka,
                kb,
                arith: op_arith(inst.e)?,
                ..JitMeta::default()
            }
        }
        TOp::Nop => JitMeta {
            class: C_NOP,
            kb: KI,
            ..JitMeta::default()
        },
        // `abs(x)`: the operand's kind in `inst.k`, the result temporary
        // in `inst.c` (`u32::MAX`: unused).
        TOp::AbsN => JitMeta {
            class: C_ABS,
            ka: inst.k,
            kb: KI,
            ..JitMeta::default()
        },
        // `$cv = a op b` (`NumBinC`): the operands as for `NumBinT`, the
        // destination variable in `inst.d`; the row covers the `AssignCv`
        // after the `Bin`.
        TOp::NumBinC => {
            let (ka, kb) = (inst.k >> 4, inst.k & 0xf);
            JitMeta {
                class: J_NUMBIN_D,
                ka,
                kb,
                arith: op_arith(inst.e)?,
                ..JitMeta::default()
            }
        }
        op => op.jit_meta()?,
    };
    // The arithmetic destination's kind (the family tables leave it the
    // default): `+`/`*` into a temporary write `inst.c`, everything else
    // writes a compiled variable.
    m.kc = if matches!(m.class, J_ARITH_TMP | J_NUMBIN_T) {
        KT
    } else {
        KC
    };
    if m.ka == KL {
        m.lit_a = resolved_lit(st, inst.a)?;
    }
    if m.kb == KL {
        m.lit_b = resolved_lit(st, inst.b)?;
    }
    Some((m.class, m))
}

/// The int a `KL` operand names, read from the frame's literal table
/// (the tape validated the offset). `None` when it is not an int.
fn resolved_lit(st: &St, off: u32) -> Option<i64> {
    // SAFETY: a validated literal offset into a table of `Value`s
    // (tag word then payload word), alive as long as the op array.
    unsafe {
        let p = st.lits.add(off as usize);
        if *p == TAG_INT {
            Some(std::ptr::read_unaligned(
                p.add(PAYLOAD as usize).cast::<i64>(),
            ))
        } else {
            None
        }
    }
}

/// The JIT's arithmetic id for a `NUM_OPS` index (`inst.e` of the generic
/// numeric rows; `scalar.rs`'s table: 0 `/`, 1 `%`, 2 `<<`, 3 `>>`, 4 `|`,
/// 5 `&`, 6 `^`, then the comparisons). `+`/`-`/`*` never reach this (the
/// specialised `arith_tmp`/`arith_cv`/`assign_op` rows own them, ids
/// 0/1/2 of `jit_arith_id!`). Division's int-or-float result and the
/// comparisons' `Bool` stay interpreted.
fn op_arith(e: u8) -> Option<u8> {
    match e {
        1 => Some(5),
        2 => Some(ARITH_SHL),
        3 => Some(ARITH_SAR),
        4 => Some(OPA_BITOR),
        5 => Some(OPA_BITAND),
        6 => Some(OPA_BITXOR),
        _ => None,
    }
}

/// Add one slot guard (merging with an existing one: exact wins).
fn add_slot(guards: &mut Vec<Guard>, base: u8, off: u32, exact: bool) -> bool {
    if let Some(g) = guards.iter_mut().find(|g| g.base == base && g.off == off) {
        if exact {
            g.exact = true;
        } else {
            g.plain = true;
        }
        true
    } else if guards.len() >= MAX_GUARDS {
        false
    } else {
        guards.push(Guard {
            base,
            off,
            exact,
            plain: !exact,
            defined: false,
        });
        true
    }
}

/// Add the guard that compiled variable `off` holds a value (merging with
/// an existing guard of the slot): the trace drops a read of it that the
/// interpreter makes (a send to a parameter the callee never mentions),
/// and that read warns when the variable is undefined.
fn add_defined(guards: &mut Vec<Guard>, off: u32) -> bool {
    if let Some(g) = guards.iter_mut().find(|g| g.base == KC && g.off == off) {
        g.defined = true;
        true
    } else if guards.len() >= MAX_GUARDS {
        false
    } else {
        guards.push(Guard {
            base: KC,
            off,
            exact: false,
            plain: false,
            defined: true,
        });
        true
    }
}

/// The guard slots one instruction needs. The kinds that name a slot
/// are exactly the guard bases: `KC` = a compiled-variable slot (the
/// `inst.a`/`inst.b`/`inst.c` byte offset among `cvs`), `KT` = a
/// temporary slot among `tmps`; `KI`/`KL` need no guard (their value
/// was checked at plan time).
fn add_class_guards(
    tape: &[Inst],
    i: usize,
    class: u16,
    m: JitMeta,
    defs: &[u32],
    bools: &[u32],
    nulls: &[u32],
    cvbools: &[u32],
    guards: &mut Vec<Guard>,
) -> bool {
    let inst = &tape[i];
    // A temporary fully stored (tag and payload) by an earlier region
    // row is guaranteed fresh at every later read in the trace: the
    // trace is linear, a row whose store is conditional (a guard
    // exit) leaves the trace when it doesn't store.  Such a read needs
    // no entry guard, and its entry value (a consumed `TAG_NULL`, for
    // instance) may be anything.
    let read = |kind: u8, off: u32, guards: &mut Vec<Guard>| -> bool {
        match kind {
            KT if defs.contains(&off) => true,
            // A temporary an earlier row of this iteration left a `Bool`
            // or a `Null` in is not an `Int` at this read, whatever it
            // held at the entry: an entry guard could pass on that stale
            // value, so the region is rejected.
            KT if bools.contains(&off) || nulls.contains(&off) => false,
            // A compiled variable the region itself wrote a `Bool`
            // into (an `isset()`/`empty()` result assigned to it) is
            // `Bool` at the trace's entry, so an exact guard would
            // fail on every entry: the region is rejected instead.
            KC if cvbools.contains(&off) => false,
            KC | KT => add_slot(guards, kind, off, true),
            KI | KL => true,
            _ => false,
        }
    };
    // A destination whose tag the row keeps as `Int` (`++`, `+=`): a
    // `Bool` the region wrote there makes the exact guard dead.
    let keep_int = |off: u32, guards: &mut Vec<Guard>| -> bool {
        !cvbools.contains(&off) && add_slot(guards, KC, off, true)
    };
    // A destination fully stored with tag and payload: the entry value
    // is never read, so a slot an earlier region row already wrote
    // (an int *or* a bool) needs no guard at all.
    let fresh_dst = |off: u32, guards: &mut Vec<Guard>| -> bool {
        defs.contains(&off)
            || bools.contains(&off)
            || nulls.contains(&off)
            || add_slot(guards, KT, off, false)
    };
    match class {
        C_JMP => true,
        C_INC | C_DEC => keep_int(inst.a, guards),
        C_ASSIGN_I | C_ASSIGN_L => add_slot(guards, KC, inst.a, false),
        // The `J_ARITH_CV` destination keeps its tag: it is guarded
        // `Int` (and every whitelisted write keeps it `Int`), so only
        // the payload is stored. `J_ASSIGN_OP`/`J_NUMBIN_C` update
        // `inst.a` in place (`inst.c` is a jump offset or a separate
        // literal slot), so the read of `a` already carries the exact
        // guard.
        J_ARITH_CV => {
            read(m.ka, inst.a, guards) && read(m.kb, inst.b, guards) && keep_int(inst.c, guards)
        }
        J_ASSIGN_OP | J_NUMBIN_C => read(m.ka, inst.a, guards) && read(m.kb, inst.b, guards),
        J_CMP => read(m.ka, inst.a, guards) && read(m.kb, inst.b, guards),
        J_INCCMP => keep_int(inst.a, guards) && read(m.kb, inst.b, guards),
        // `inst.a` = a temp (exact: read as an int), then the cv it is
        // written into (weak: nothing of its own is read).  A temp an
        // earlier `isset()`/`empty()` row of the region stored is fresh
        // too, and the meta's `mask` flag (set in the scan) tells the
        // emitted row to store the `Bool` tag, not `Int`.
        J_ASSIGN_T => {
            (defs.contains(&inst.b)
                || bools.contains(&inst.b)
                || nulls.contains(&inst.b)
                || add_slot(guards, KT, inst.b, true))
                && add_slot(guards, KC, inst.a, false)
        }
        // Arithmetic into a temporary: the destination is fully stored
        // (tag and payload), so it only needs the weak guard (a scalar
        // or nothing, what the interpreter's own store test accepts).
        J_ARITH_TMP | J_NUMBIN_T => {
            read(m.ka, inst.a, guards) && read(m.kb, inst.b, guards) && fresh_dst(inst.c, guards)
        }
        // A property read fully stores its destination temporary (tag
        // and payload); the weak guard only keeps a heap value from
        // being overwritten without release (a fresh temporary, or one
        // an earlier row already stored an int into, is safe). The
        // receiver, scope and class are re-checked by the emitted
        // code, not entry-guarded.
        C_PROP => fresh_dst(m.db, guards),
        // A property write: a `KT` source needs no entry guard (the
        // emitted row re-checks its tag every iteration, so a temporary
        // an earlier row of the region produced, or one whose entry
        // value is the `TAG_NULL` a previous consume left, is fine); a
        // `KL` source is a plan-time constant; a `KC` source is read as
        // an int like an arithmetic operand. The destination temporary
        // is fully stored like a read's. The base, scope, class and the
        // slot's own tag are re-checked by the emitted code, not
        // entry-guarded.
        C_PROPW => {
            (m.sc == KT || m.sc == KL || read(m.sc, m.sw, guards))
                && (m.db == NO_DST || fresh_dst(m.db, guards))
        }
        // `++`/`--`, `isset()`/`empty()` and `op=` re-check the base,
        // scope, class and slot every iteration like a write; only the
        // result temporary needs an entry guard (fully stored, tag and
        // payload, by the emitted row).
        C_PROPINC => m.db == NO_DST || fresh_dst(m.db, guards),
        C_PROPISSET => fresh_dst(m.db, guards),
        C_PROPOPA => {
            (m.sc == KT || m.sc == KL || read(m.sc, m.sw, guards))
                && (m.db == NO_DST || fresh_dst(m.db, guards))
        }
        // A static-property read, `isset()`/`empty()` or a folded
        // constant: everything they touch is the pinned cell (or
        // nothing at all) re-checked by the emitted row; only the
        // result temporary needs an entry guard (fully stored, tag and
        // payload, by the emitted row).
        C_SPROP | C_SPROPISSET | C_CONSTI => fresh_dst(m.db, guards),
        // A static-property write: exactly the instance-write rule —
        // a `KT` source is re-checked per iteration, a `KC` source is
        // read as an int, a `KL` source is plan-time; the result
        // temporary is fully stored.
        C_SPROPW => {
            (m.sc == KT || m.sc == KL || read(m.sc, m.sw, guards))
                && (m.db == NO_DST || fresh_dst(m.db, guards))
        }
        // `$cv = a op b`: as `J_ARITH_CV`, the destination (`inst.d`)
        // keeps its `Int` tag and only its payload is stored.
        J_NUMBIN_D => {
            read(m.ka, inst.a, guards) && read(m.kb, inst.b, guards) && keep_int(inst.d, guards)
        }
        C_NOP => true,
        // `abs(x)`: the operand is read as an int; a result temporary is
        // fully stored.
        C_ABS => read(m.ka, inst.a, guards) && (inst.c == u32::MAX || fresh_dst(inst.c, guards)),
        // The array rows hand their operands to a helper by address, and
        // the helper checks every one of them on every iteration (a
        // wrong type declines, which leaves the row to the interpreter):
        // nothing to guard at the entry.
        C_DIMR | C_DIMW | C_DIMOP => true,
        _ => false,
    }
}

/// The run-time shape of a plain instance-property read (`FetchObjProp`
/// /`FetchObjPropTo`): the op array's property cache entry for the read's
/// place, when it is filled for the current frame's calling scope,
/// allows plain reads of a *declared* (not dynamic) property, and has
/// `$this` or a compiled variable as its base. The returned [`JitMeta`]
/// carries everything `emit_prop` re-checks at run time (the receiver,
/// the scope, the class) and everything it reads (`slot`, `db`); the
/// entry only *proved* those once, so none of them may be assumed. A
/// `None` meta rejects the trace instead: the interpreted row re-runs
/// the full lookup, which is always correct.
fn plan_prop(st: &St, code: &crate::opcode::OpArray, inst: &Inst) -> Option<JitMeta> {
    let e = code
        .prop_cache
        .try_borrow()
        .ok()?
        .get(inst.a as usize)
        .cloned()
        .or_else(soft)?;
    if e.class_addr == 0 {
        return soft();
    }
    if e.class_addr == 0 || e.flags & (PC_READ | PC_DYN) != PC_READ {
        return None;
    }
    // The slot index must fit the `slot << SLOT_SHIFT` disp32s.
    if e.slot >= 1 << 27 {
        return None;
    }
    // The plan ran in this very frame; its scope is what the guard
    // compares against (and 0 for none).
    // SAFETY: the frame outlives the plan (the trace runs in it).
    let scope = unsafe { &*st.f }
        .scope
        .as_ref()
        .map_or(0, |s| Rc::as_ptr(s) as usize);
    if e.scope_addr != scope {
        return soft();
    }
    if inst.b >= code.num_tmps {
        return None;
    }
    let (ka, sb) = if e.base == PROP_CACHE_THIS {
        (KTHIS, 0)
    } else {
        if e.base as usize >= code.cv_names.len() {
            return None;
        }
        (KC, e.base << SLOT_SHIFT)
    };
    Some(JitMeta {
        class: C_PROP,
        ka,
        sb,
        db: inst.b << SLOT_SHIFT,
        slot: e.slot,
        // The raw `Rc<Class>` field word: `emit_prop` reads the field
        // itself, which names `Rc::as_ptr` minus `RC_VALUE_OFFSET`.
        lit_a: e.class_addr as i64 - i64::from(RC_VALUE_OFFSET),
        // The same for the scope field, keeping 0 = "no scope" (what the
        // emitted `test`/`jne` pair checks); an entry's `scope_addr` is
        // an `Rc::as_ptr` value address like `class_addr`.
        lit_b: if e.scope_addr == 0 {
            0
        } else {
            e.scope_addr as i64 - i64::from(RC_VALUE_OFFSET)
        },
        ..JitMeta::default()
    })
}

/// The run-time shape of a plain instance-property write
/// (`AssignObjProp`, `$obj->name = int`): like [`plan_prop`], but for a
/// write the cache entry flags (`PC_WRITE`, no `PC_DYN`) and *untyped*
/// (`accept == u32::MAX`, so no coercion and no `slot_unset` bookkeeping
/// can differ from a plain slot store). The written value must be an
/// `Int` the plan can prove: a compiled variable, a temporary, or an
/// int literal; `Operand::Unused` (an error form) is rejected.
///
/// `emit_prop_write` re-checks (per iteration, like `emit_prop`) the
/// base, the scope, the class, the lazy and slots cells and the slot
/// length, plus one check the read does not need: the *slot's own tag*
/// is `Int`. That single tag check carries the write's whole heap
/// story: the old value is then a scalar (nothing to release, and an
/// `Int` cannot be aliased through a `Reference` or shared, so the
/// payload store cannot split an alias), its tag equals the new value's
/// (so only the payload word is stored and the tag byte never flaps at
/// a safe point), and an unset (`TAG_NONE`) or typed-uninitialised slot
/// takes the interpreted path with its `__set()`/`set_slot_unset`
/// checks. The *source* is guarded `Int` exactly like an arithmetic
/// read (`KT` sources re-checked per row, so a source an earlier row
/// fully stored passes the check on its fresh value).
fn plan_prop_write(st: &St, code: &crate::opcode::OpArray, inst: &Inst) -> Option<JitMeta> {
    let Some(Op::AssignObjProp { place, val, dst }) = code.ops.get(inst.d as usize) else {
        return None;
    };
    if *place != inst.a {
        return None;
    }
    let e = code
        .prop_cache
        .try_borrow()
        .ok()?
        .get(*place as usize)
        .cloned()
        .or_else(soft)?;
    if e.class_addr == 0 {
        return soft();
    }
    if e.class_addr == 0 || e.flags & (PC_WRITE | PC_DYN) != PC_WRITE || e.accept != u32::MAX {
        return None;
    }
    // The slot index must fit the `slot << SLOT_SHIFT` disp32s.
    if e.slot >= 1 << 27 {
        return None;
    }
    // The plan ran in this very frame; its scope is what the guard
    // compares against (and 0 for none).
    // SAFETY: the frame outlives the plan (the trace runs in it).
    let scope = unsafe { &*st.f }
        .scope
        .as_ref()
        .map_or(0, |s| Rc::as_ptr(s) as usize);
    if e.scope_addr != scope {
        return soft();
    }
    let (ka, sb) = if e.base == PROP_CACHE_THIS {
        (KTHIS, 0)
    } else {
        if e.base as usize >= code.cv_names.len() {
            return None;
        }
        (KC, e.base << SLOT_SHIFT)
    };
    let (sc, sw, lit_c, consume) = match val {
        Operand::Cv(cv) if (*cv as usize) < code.cv_names.len() => {
            (KC, *cv << SLOT_SHIFT, 0, false)
        }
        Operand::Tmp(t) | Operand::TmpKeep(t) if *t < code.num_tmps => {
            (KT, *t << SLOT_SHIFT, 0, matches!(val, Operand::Tmp(_)))
        }
        Operand::Lit(l) => match code.literals.get(*l as usize) {
            Some(php_runtime::Value::Int(v)) => (KL, 0, *v, false),
            _ => return None,
        },
        _ => return None,
    };
    // The result temporary (fully stored, as for a read), or `NO_DST`
    // when the expression's value is discarded.
    let db = match dst {
        None => NO_DST,
        Some(t) if *t < code.num_tmps => *t << SLOT_SHIFT,
        Some(_) => return None,
    };
    // `$t = ($o->p = $t)`: the source consume and the result copy meet
    // on one slot; leave that shape to the interpreter.
    if consume && db == sw {
        return None;
    }
    Some(JitMeta {
        class: C_PROPW,
        ka,
        sb,
        db,
        slot: e.slot,
        sc,
        sw,
        lit_c,
        consume,
        lit_a: e.class_addr as i64 - i64::from(RC_VALUE_OFFSET),
        lit_b: if e.scope_addr == 0 {
            0
        } else {
            e.scope_addr as i64 - i64::from(RC_VALUE_OFFSET)
        },
        ..JitMeta::default()
    })
}

/// The base, slot, scope and class checks the three planners below
/// share with [`plan_prop_write`] (their emitted guards are identical);
/// `want` is the entry flag the op's interpreted hit requires.
fn prop_common(st: &St, code: &crate::opcode::OpArray, inst: &Inst, want: u8) -> Option<JitMeta> {
    let e = code
        .prop_cache
        .try_borrow()
        .ok()?
        .get(inst.a as usize)
        .cloned()
        .or_else(soft)?;
    if e.class_addr == 0 {
        return soft();
    }
    if e.class_addr == 0 || e.flags & (want | PC_DYN) != want {
        return None;
    }
    // The slot index must fit the `slot << SLOT_SHIFT` disp32s.
    if e.slot >= 1 << 27 {
        return None;
    }
    // The plan ran in this very frame; its scope is what the guard
    // compares against (and 0 for none).
    // SAFETY: the frame outlives the plan (the trace runs in it).
    let scope = unsafe { &*st.f }
        .scope
        .as_ref()
        .map_or(0, |s| Rc::as_ptr(s) as usize);
    if e.scope_addr != scope {
        return soft();
    }
    let (ka, sb) = if e.base == PROP_CACHE_THIS {
        (KTHIS, 0)
    } else {
        if e.base as usize >= code.cv_names.len() {
            return None;
        }
        (KC, e.base << SLOT_SHIFT)
    };
    let m = JitMeta {
        ka,
        sb,
        slot: e.slot,
        lit_a: e.class_addr as i64 - i64::from(RC_VALUE_OFFSET),
        lit_b: if e.scope_addr == 0 {
            0
        } else {
            e.scope_addr as i64 - i64::from(RC_VALUE_OFFSET)
        },
        ..JitMeta::default()
    };
    Some(m)
}

/// The destination temporary of an `Option<u32>` result slot: `NO_DST`
/// when the expression's value is discarded, the byte offset when it is
/// kept (an out-of-range temporary rejects the plan).
fn prop_dst(code: &crate::opcode::OpArray, dst: Option<u32>) -> Option<u32> {
    match dst {
        None => Some(NO_DST),
        Some(t) if t < code.num_tmps => Some(t << SLOT_SHIFT),
        Some(_) => None,
    }
}

/// The run-time shape of a plain `++$o->p`/`$o->p--` row
/// (`IncDecObjProp`): the cache entry allows plain read-*and*-write
/// hits of a declared slot (what `incdec_hit` requires), and the slot
/// holds an `Int` (re-checked per iteration). Unlike `plan_prop_write`
/// there is no `accept` requirement: an int overflow exits `jo` before
/// any store, and the interpreted row then converts to `Float` or
/// declines exactly as its own `accept` check decides. `inc` carries
/// the direction and `kc` the post-ness of the `IncDecKind`.
fn plan_prop_incdec(st: &St, code: &crate::opcode::OpArray, inst: &Inst) -> Option<JitMeta> {
    let Some(Op::IncDecObjProp { place, kind, dst }) = code.ops.get(inst.d as usize) else {
        return None;
    };
    if *place != inst.a {
        return None;
    }
    let mut m = prop_common(st, code, inst, PC_READ | PC_WRITE)?;
    m.class = C_PROPINC;
    m.db = prop_dst(code, *dst)?;
    m.inc = kind.is_inc();
    m.kc = u8::from(kind.is_post());
    Some(m)
}

/// The run-time shape of a plain `isset($o->p)`/`empty($o->p)` row
/// (`IssetObjProp`): the cache entry allows plain isset hits of a
/// *declared* slot (the interpreted hit also answers dynamic-property
/// hits, which the JIT leaves to it), and the emitted code answers
/// `Int`/`Null`/`Bool` slots inline and exits to the interpreted row
/// for anything else. `inc` carries `empty`.
fn plan_prop_isset(st: &St, code: &crate::opcode::OpArray, inst: &Inst) -> Option<JitMeta> {
    let Some(Op::IssetObjProp { place, dst, empty }) = code.ops.get(inst.d as usize) else {
        return None;
    };
    if *place != inst.a {
        return None;
    }
    let mut m = prop_common(st, code, inst, PC_ISSET)?;
    m.class = C_PROPISSET;
    // `isset`/`empty` always write their result temporary.
    m.db = prop_dst(code, Some(*dst))?;
    m.inc = *empty;
    Some(m)
}

/// The run-time shape of a plain `$o->p += $x` row (`AssignOpObjProp`
/// of an int-typed cache entry): read-and-write hit on a declared slot,
/// an `Int` slot (re-checked per iteration), and a number source: a
/// compiled variable, temporary or int literal, exactly as
/// `plan_prop_write` resolves them. Only int-int operators inline:
/// `+`/`-`/`*` (which exit `jo` on overflow, leaving the interpreted
/// row to decide the `Float` conversion) and the three bitwise ops.
fn plan_prop_opa(st: &St, code: &crate::opcode::OpArray, inst: &Inst) -> Option<JitMeta> {
    let Some(Op::AssignOpObjProp {
        place,
        op,
        val,
        dst,
    }) = code.ops.get(inst.d as usize)
    else {
        return None;
    };
    if *place != inst.a {
        return None;
    }
    let arith = match op {
        php_runtime::BinaryOp::Add => 0,
        php_runtime::BinaryOp::Sub => 1,
        php_runtime::BinaryOp::Mul => 2,
        php_runtime::BinaryOp::BitAnd => OPA_BITAND,
        php_runtime::BinaryOp::BitOr => OPA_BITOR,
        php_runtime::BinaryOp::BitXor => OPA_BITXOR,
        _ => return None,
    };
    let mut m = prop_common(st, code, inst, PC_READ | PC_WRITE)?;
    m.class = C_PROPOPA;
    m.arith = arith;
    let (sc, sw, lit_c, consume) = match val {
        Operand::Cv(cv) if (*cv as usize) < code.cv_names.len() => {
            (KC, *cv << SLOT_SHIFT, 0, false)
        }
        Operand::Tmp(t) | Operand::TmpKeep(t) if *t < code.num_tmps => {
            (KT, *t << SLOT_SHIFT, 0, matches!(val, Operand::Tmp(_)))
        }
        Operand::Lit(l) => match code.literals.get(*l as usize) {
            Some(php_runtime::Value::Int(v)) => (KL, 0, *v, false),
            _ => return None,
        },
        _ => return None,
    };
    m.db = prop_dst(code, *dst)?;
    // `$t = ($o->p += $t)`: the source consume and the result copy meet
    // on one slot; leave that shape to the interpreter (as for a write).
    if consume && m.db == sw {
        return None;
    }
    m.sc = sc;
    m.sw = sw;
    m.lit_c = lit_c;
    m.consume = consume;
    Some(m)
}

thread_local! {
    /// The keep-alive for every static-property cell a plan pinned:
    /// the trace's `movabs` names the cell's address forever (traces
    /// are never freed), so the `Rc` must stay alive even if the
    /// static is later re-declared, `unset` or the class dropped. The
    /// staleness that could otherwise follow is already unreachable
    /// through the prologue's version guard (every runtime-cache clear
    /// bumps it, exactly as the inlined-call rows rely on); this keeps
    /// the pinned *address* valid. Re-planning pushes extra clones: a
    /// handful of words against correctness, and traces are few.
    static PINNED_CELLS: RefCell<Vec<Rc<RefCell<php_runtime::Value>>>> =
        const { RefCell::new(Vec::new()) };
}

/// The `to:` fold check: an op that names a `to` variable stores into
/// its own result temporary and must be followed by the plain
/// `AssignCv` that moves it — the pair `lower` leaves as two tape rows
/// and the scan emits as two rows (the `FetchObjPropTo` convention:
/// the read row stores into its temporary and the `AssignCv` row moves
/// it, mirroring the interpreted fall-through).
fn fold_ok(code: &crate::opcode::OpArray, i: usize, to: u32, tmp: u32) -> Option<()> {
    match code.ops.get(i + 1) {
        Some(Op::AssignCv {
            cv,
            val: Operand::Tmp(v) | Operand::TmpKeep(v),
            dst: None,
        }) if *cv == to && *v == tmp => Some(()),
        _ => None,
    }
}

/// The run-time shape shared by the three static-property planners:
/// the frame's run-time-cache entry for the place (found exactly as
/// the interpreted row finds it, `rtcache::static_prop_entry_in`, which
/// also rejects builtin frames, unfilled entries and `Dyn` places),
/// with the entry's cell pinned for the trace's whole life and the
/// guards it implies. `want_plain` additionally requires the entry's
/// untyped flag: writes need it (no coercion or check to re-run),
/// reads and `isset` do not — any tag but `Int` exits to the
/// interpreted row, which re-checks everything itself.
fn sprop_common(
    st: &St,
    code: &crate::opcode::OpArray,
    place: u32,
    want_plain: bool,
) -> Option<JitMeta> {
    // A plain static-property path: no `[]`/`->` segments after the
    // place, and the class it names is not dynamic.
    let p = code.places.get(place as usize)?;
    let PlaceBase::StaticProp { class, .. } = &p.base else {
        return None;
    };
    if !p.segs.is_empty() || matches!(class, ClassSpec::Dyn(_)) {
        return None;
    }
    // SAFETY: the frame outlives the plan (the trace runs in it).
    let f = unsafe { &*st.f };
    let e = crate::rtcache::static_prop_entry_in(f, place, |e| {
        if want_plain && !e.fast_plain {
            None
        } else {
            Some(e.clone())
        }
    })
    .or_else(soft)?;
    PINNED_CELLS.with(|p| p.borrow_mut().push(e.cell.clone()));
    // `lit_a` is the cell's *absolute* address (its `RefCell` borrow
    // word at `REFCELL_BORROW`, the `Value` at `REFCELL_VALUE`); the
    // guard words are the raw `Rc` field words, `RC_VALUE_OFFSET`
    // below the `Class` value the cache names (0 = no class).
    let mut m = JitMeta {
        lit_a: Rc::as_ptr(&e.cell) as i64,
        lit_b: if e.scope_ptr == 0 {
            0
        } else {
            e.scope_ptr as i64 - i64::from(RC_VALUE_OFFSET)
        },
        wc: if e.called_ptr == 0 {
            0
        } else {
            e.called_ptr as i64 - i64::from(RC_VALUE_OFFSET)
        },
        ..JitMeta::default()
    };
    // What the emitted guards re-check follows the spec the place
    // names, exactly as `static_prop_entry_in`'s own match: a public
    // named entry (filled from no scope) answers every scope, so its
    // row needs no scope guard; `self::`/`parent::` follow the frame's
    // scope; `static::` the running function's class as well.
    match class {
        ClassSpec::Named(_) => m.sg = e.scope_ptr != 0,
        ClassSpec::SelfClass | ClassSpec::Parent => m.sg = true,
        ClassSpec::Static => {
            m.sg = true;
            m.cg = true;
        }
        ClassSpec::Dyn(_) => return None,
    }
    Some(m)
}

/// The run-time shape of a plain static-property read
/// (`FetchStaticProp` in `FetchMode::Read` on a place the cache
/// resolves for this frame): the entry is found and its cell pinned at
/// plan time, and the emitted row re-checks the guards and the cell's
/// `Int` tag per iteration (`static_cell_value` answers an `Int` cell
/// directly; any other tag exits and the interpreted row decides).
fn plan_sprop(st: &St, code: &crate::opcode::OpArray, i: usize) -> Option<JitMeta> {
    let Some(Op::FetchStaticProp {
        place,
        dst,
        to,
        mode,
    }) = code.ops.get(i)
    else {
        return None;
    };
    if *mode != FetchMode::Read {
        return None;
    }
    let mut m = sprop_common(st, code, *place, false)?;
    m.class = C_SPROP;
    // The read always writes its result temporary.
    m.db = prop_dst(code, Some(*dst))?;
    if let Some(to) = to {
        fold_ok(code, i, *to, *dst)?;
    }
    Some(m)
}

/// The run-time shape of a plain static-property write
/// (`AssignStaticProp` into a `fast_plain` entry's cell that currently
/// holds an `Int` — re-checked per iteration): the value source is a
/// compiled variable, temporary or int literal exactly as
/// `plan_prop_write` resolves them. A plain `Int` cell is unshared and
/// uncoerced (`assign_static_prop_in`'s own gate), so the emitted row
/// stores only the payload word.
fn plan_sprop_write(st: &St, code: &crate::opcode::OpArray, i: usize) -> Option<JitMeta> {
    let Some(Op::AssignStaticProp { place, val, dst }) = code.ops.get(i) else {
        return None;
    };
    let mut m = sprop_common(st, code, *place, true)?;
    m.class = C_SPROPW;
    let (sc, sw, lit_c, consume) = match val {
        Operand::Cv(cv) if (*cv as usize) < code.cv_names.len() => {
            (KC, *cv << SLOT_SHIFT, 0, false)
        }
        Operand::Tmp(t) | Operand::TmpKeep(t) if *t < code.num_tmps => {
            (KT, *t << SLOT_SHIFT, 0, matches!(val, Operand::Tmp(_)))
        }
        Operand::Lit(l) => match code.literals.get(*l as usize) {
            Some(php_runtime::Value::Int(v)) => (KL, 0, *v, false),
            _ => return None,
        },
        _ => return None,
    };
    m.db = prop_dst(code, *dst)?;
    // `$t = (Foo::$x = $t)`: the source consume and the result copy
    // meet on one slot; leave that shape to the interpreter.
    if consume && m.db == sw {
        return None;
    }
    m.sc = sc;
    m.sw = sw;
    m.lit_c = lit_c;
    m.consume = consume;
    Some(m)
}

/// The run-time shape of a plain `isset(Foo::$x)`/`empty(Foo::$x)`:
/// the pinned cell, then a branch on its tag (`Int`, `Null` or `Bool`
/// answered inline exactly as `static_cell_isset` decides, anything
/// else the interpreted row). `inc` carries `empty`.
fn plan_sprop_isset(st: &St, code: &crate::opcode::OpArray, i: usize) -> Option<JitMeta> {
    let Some(Op::IssetStaticProp { place, dst, empty }) = code.ops.get(i) else {
        return None;
    };
    let mut m = sprop_common(st, code, *place, false)?;
    m.class = C_SPROPISSET;
    // `isset`/`empty` always write their result temporary.
    m.db = prop_dst(code, Some(*dst))?;
    m.inc = *empty;
    Some(m)
}

/// The run-time shape of a constant read the plan can fold to an
/// `Int`: the row's run-time cache (`rtcache::const_hit_in`,
/// `rtcache::class_const_hit_in` — the same hits the interpreted row
/// uses) already resolves the name to a value, so the emitted row just
/// stores the literal. What it must re-check per iteration follows how
/// the name resolves: a plain named constant is one value for every
/// frame of this op array (the cache slot is the op array's own, and
/// only a redefinition could change it — the version guard), so it
/// needs no guard at all; `self::`/`parent::` (and the access check of
/// any named class constant) follow the frame's calling scope, and
/// `static::` the running function's class as well — a recursive
/// re-entry into the trace arrives with another frame, so those words
/// are pinned and re-checked.
fn plan_const(st: &St, code: &crate::opcode::OpArray, i: usize) -> Option<JitMeta> {
    // SAFETY: the frame outlives the plan (the trace runs in it).
    let f = unsafe { &*st.f };
    let (v, to, dst, class) = match code.ops.get(i) {
        // `fallback` is irrelevant on a `Direct` hit (the cached
        // namespace fallback is left to the interpreted row).
        Some(Op::FetchConst { name, dst, .. }) => (
            crate::rtcache::const_hit_in(f, *name).or_else(soft)?,
            None,
            *dst,
            None,
        ),
        Some(Op::FetchConstTo { name, dst, to, .. }) => (
            crate::rtcache::const_hit_in(f, *name).or_else(soft)?,
            Some(*to),
            *dst,
            None,
        ),
        // A dynamic class operand names the class per iteration; the
        // fold plans none of it.
        Some(Op::FetchClassConst {
            class,
            name: Operand::Lit(n),
            dst,
        }) => (
            crate::rtcache::class_const_hit_in(f, class, *n).or_else(soft)?,
            None,
            *dst,
            Some(class.clone()),
        ),
        Some(Op::FetchClassConstTo {
            class,
            name,
            dst,
            to,
        }) => (
            crate::rtcache::class_const_hit_in(f, class, *name).or_else(soft)?,
            Some(*to),
            *dst,
            Some(class.clone()),
        ),
        _ => return None,
    };
    // The folded value: an int, or the `null`, `true` or `false` a
    // constant is as often defined to (`define('DEBUG', false)`). Anything
    // else (a float, a string, an array) is the interpreted row's.
    let (kind, v) = match v {
        php_runtime::Value::Int(v) => (CONST_INT, v),
        php_runtime::Value::Null => (CONST_NULL, 0),
        php_runtime::Value::Bool(b) => (CONST_BOOL, i64::from(b)),
        _ => return None,
    };
    let mut m = JitMeta {
        class: C_CONSTI,
        // The folded value itself, and which kind of value it is.
        lit_a: v,
        arith: kind,
        db: prop_dst(code, Some(dst))?,
        ..JitMeta::default()
    };
    match class {
        // A plain named constant: its value sits in this op array's own
        // cache slot and is the same for every frame that runs it,
        // whatever the scope; only a redefinition could change it, which
        // the prologue's version guard covers. No guard of its own.
        None => m.sg = false,
        // A class constant named by the op: the frame's calling scope is
        // what resolved the entry (access-checked per lookup, and an op
        // array that needs a non-public one always runs in a frame scoped
        // where it is accessible), so pin it.
        Some(ClassSpec::Named(_)) => {
            m.sg = true;
            m.lit_b = f
                .scope
                .as_ref()
                .map_or(0, |s| Rc::as_ptr(s) as i64 - i64::from(RC_VALUE_OFFSET));
        }
        Some(ClassSpec::SelfClass) | Some(ClassSpec::Parent) => {
            m.sg = true;
            m.lit_b = f
                .scope
                .as_ref()
                .map_or(0, |s| Rc::as_ptr(s) as i64 - i64::from(RC_VALUE_OFFSET));
        }
        Some(ClassSpec::Static) => {
            m.sg = true;
            m.cg = true;
            m.lit_b = f
                .scope
                .as_ref()
                .map_or(0, |s| Rc::as_ptr(s) as i64 - i64::from(RC_VALUE_OFFSET));
            m.wc = f
                .called
                .as_ref()
                .map_or(0, |c| Rc::as_ptr(c) as i64 - i64::from(RC_VALUE_OFFSET));
        }
        Some(ClassSpec::Dyn(_)) => return None,
    }
    if let Some(to) = to {
        fold_ok(code, i, to, dst)?;
    }
    Some(m)
}

/// Plan one lowered `Ext` row: the tape keeps the raw op at its own
/// index (`lower` emits one zeroed `Inst::plain(TOp::Ext)` per op), so
/// the scan arm decodes `code.ops[i]` and dispatches. A tape-core `Ext`
/// row always bails in `run_core`, so every `Ext` row a region scan
/// reaches is one of these hot spec rows.
fn plan_ext(st: &St, code: &crate::opcode::OpArray, i: usize) -> Option<(u16, JitMeta)> {
    let m = match code.ops.get(i) {
        Some(Op::FetchStaticProp { .. }) => plan_sprop(st, code, i)?,
        Some(Op::AssignStaticProp { .. }) => plan_sprop_write(st, code, i)?,
        Some(Op::IssetStaticProp { .. }) => plan_sprop_isset(st, code, i)?,
        Some(
            Op::FetchConst { .. }
            | Op::FetchConstTo { .. }
            | Op::FetchClassConst { .. }
            | Op::FetchClassConstTo { .. },
        ) => plan_const(st, code, i)?,
        _ => return None,
    };
    Some((m.class, m))
}

/// Whether `op` is an array row the plan may inline ([`plan_dim`]).
fn is_dim_row(op: TOp) -> bool {
    matches!(
        op,
        TOp::FetchDimIC
            | TOp::FetchDimSC
            | TOp::FetchDimC
            | TOp::AssignDimIC
            | TOp::AssignDimC
            | TOp::AssignOpDimC
    )
}

/// The operand kinds of the array rows, as `dims.rs` packs them in `k`.
const DIM_CV: u8 = 0;
const DIM_TMP: u8 = 1;
const DIM_TMPKEEP: u8 = 2;
const DIM_LIT: u8 = 3;
const DIM_UNUSED: u8 = 4;
/// The operators [`jit_dim_op`] computes, by `dims.rs`'s `op_code`.
const DIM_OP_ADD: u32 = 0;
const DIM_OP_SUB: u32 = 1;
const DIM_OP_MUL: u32 = 2;
const DIM_OP_OR: u32 = 8;
const DIM_OP_AND: u32 = 9;
const DIM_OP_XOR: u32 = 10;

/// The run-time shape of an array row with an int offset: a read
/// (`FetchDimIC`/`FetchDimC`: `$t = $cv[$i]`, or `$v = $cv[$i]` folded
/// with the `AssignCv` after it), a write (`AssignDimIC`/`AssignDimC`:
/// `$cv[$i] = v`, `$cv[] = v`) or a compound write (`AssignOpDimC`:
/// `$cv[$i] += v`). The row's class, its meta and the tape rows it covers.
///
/// Nothing about the operands is assumed: the emitted row passes their
/// slots' addresses to a helper (`jit_dim_get`/`jit_dim_set`/
/// `jit_dim_op`), which checks on every iteration that the variable
/// holds an array, that the offset and the value are `Int`s and that the
/// element is what it handles, and declines otherwise with nothing
/// changed (the interpreted row then runs). Only what can never be an
/// `Int` is refused here: a literal offset or value of another type.
///
/// Meta fields: `sb` the array variable's slot; `kb`/`slot`/`lit_b` the
/// offset (`KC`/`KT` and a slot offset, `KL` and the literal's address,
/// or `KSKIP` for an append); `kc`/`db` a read's destination (`KC` the
/// folded variable, `KT` the result temporary); `sc`/`sw`/`lit_c` and
/// `consume` a write's value; `arith` a compound write's operator.
fn plan_dim(st: &St, code: &crate::opcode::OpArray, inst: &Inst) -> Option<(u16, JitMeta, usize)> {
    // One operand: its kind, slot offset, literal address, and whether
    // the row consumes it (a `Tmp`, as opposed to a `TmpKeep`). A literal
    // must be an int, or (`key`: the offset of a read) a string.
    let operand = |kind: u8, idx: u32, key: bool| -> Option<(u8, u32, i64, bool)> {
        if idx >= super::SLOT_LIMIT {
            return None;
        }
        match kind {
            DIM_CV if (idx as usize) < code.cv_names.len() => {
                Some((KC, idx << SLOT_SHIFT, 0, false))
            }
            DIM_TMP | DIM_TMPKEEP if idx < code.num_tmps => {
                Some((KT, idx << SLOT_SHIFT, 0, kind == DIM_TMP))
            }
            DIM_LIT => match code.literals.get(idx as usize) {
                Some(php_runtime::Value::Int(_)) => {
                    // SAFETY: a literal of this op array's table, which
                    // `st.lits` is the base of (16 bytes each).
                    let p = unsafe { st.lits.add((idx << SLOT_SHIFT) as usize) };
                    Some((KL, 0, p as i64, false))
                }
                Some(php_runtime::Value::Str(_)) if key => {
                    // SAFETY: as above.
                    let p = unsafe { st.lits.add((idx << SLOT_SHIFT) as usize) };
                    Some((KL, 0, p as i64, false))
                }
                _ => None,
            },
            _ => None,
        }
    };
    if inst.a >= super::SLOT_LIMIT || inst.a as usize >= code.cv_names.len() {
        return None;
    }
    let sb = inst.a << SLOT_SHIFT;
    let dk = inst.k & 7;
    match inst.op {
        TOp::FetchDimIC | TOp::FetchDimSC | TOp::FetchDimC => {
            let (kb, slot, lit_b, _) = operand(dk, inst.b, true)?;
            // The destination: the variable of the folded `AssignCv`
            // (`inst.d`; the row then covers that op as well, as the
            // interpreted row jumps past it), else the result temporary
            // (`inst.p`). `u32::MAX` is `dims.rs`'s "no folded variable".
            let (kc, db, step) = if inst.d == u32::MAX {
                let t = u32::try_from(inst.p).ok()?;
                if t >= code.num_tmps || t >= super::SLOT_LIMIT {
                    return None;
                }
                (KT, t << SLOT_SHIFT, 1)
            } else {
                if inst.d as usize >= code.cv_names.len() || inst.d >= super::SLOT_LIMIT {
                    return None;
                }
                (KC, inst.d << SLOT_SHIFT, 2)
            };
            let m = JitMeta {
                class: C_DIMR,
                sb,
                kb,
                slot,
                lit_b,
                kc,
                db,
                // `FetchDimSC`: a string literal that is no integer key.
                inc: inst.op == TOp::FetchDimSC,
                ..JitMeta::default()
            };
            Some((C_DIMR, m, step))
        }
        TOp::AssignDimIC | TOp::AssignDimC | TOp::AssignOpDimC => {
            let (kb, slot, lit_b) = if dk == DIM_UNUSED {
                (KSKIP, 0, 0)
            } else {
                let (k, o, l, _) = operand(dk, inst.b, false)?;
                (k, o, l)
            };
            let (sc, sw, lit_c, consume) =
                operand((inst.k >> 3) & 7, u32::try_from(inst.p).ok()?, false)?;
            let (class, arith) = if inst.op == TOp::AssignOpDimC {
                // `$cv[] op= v` has no element to read, and only the
                // int-int operators have a helper.
                let known = matches!(
                    u32::from(inst.e),
                    DIM_OP_ADD | DIM_OP_SUB | DIM_OP_MUL | DIM_OP_OR | DIM_OP_AND | DIM_OP_XOR
                );
                if kb == KSKIP || !known {
                    return None;
                }
                (C_DIMOP, inst.e)
            } else {
                (C_DIMW, 0)
            };
            let m = JitMeta {
                class,
                sb,
                kb,
                slot,
                lit_b,
                sc,
                sw,
                lit_c,
                consume,
                arith,
                ..JitMeta::default()
            };
            Some((class, m, 1))
        }
        _ => None,
    }
}

/// [`jit_dim_get`]'s `flags`: the destination is a compiled variable (not
/// a temporary); the offset is a string literal that is no integer key,
/// so it is looked up without the numeric-string test.
const DIM_GET_TO_CV: u32 = 1;
const DIM_GET_EXACT_KEY: u32 = 2;

/// The `Int` in the slot at `p` - a temporary's or a literal's `Value`,
/// or a compiled variable's `Option<Value>`: the same tag byte and
/// payload word (the layout tests' `value_layout`) - or `None` when the
/// slot holds anything else, nothing included.
///
/// # Safety
/// `p` points at a live 16-byte slot.
unsafe fn slot_int(p: *const u8) -> Option<i64> {
    // SAFETY: the caller's contract.
    unsafe {
        (*p == TAG_INT).then(|| std::ptr::read_unaligned(p.add(PAYLOAD as usize).cast::<i64>()))
    }
}

/// `C_DIMR`'s helper: `$dst = $arr[$idx]` when `arr` holds an array
/// directly, `idx` an `Int` or a string, the array has an `Int` element
/// at that key (a packed table or a hash), and the destination holds
/// nothing whose
/// release could do more than free a string - a temporary's plain value,
/// or (`flags` has [`DIM_GET_TO_CV`]) a compiled variable that is undefined or plain, the
/// test the interpreter's own fast path makes (`dims.rs`
/// `store_plain_into`). Returns 0 having stored the element; 1, nothing
/// changed, for anything else (the interpreted row then decides: a
/// missing key warns, another element type is copied by its own rules).
///
/// # Safety
/// The three pointers are slots of the running frame (or of its op
/// array's literal table): `arr` and, with [`DIM_GET_TO_CV`], `dst` compiled
/// variables, `dst` otherwise a temporary.
unsafe extern "C" fn jit_dim_get(
    arr: *const crate::vm::CvSlot,
    idx: *const u8,
    dst: *mut u8,
    flags: u32,
) -> u32 {
    use php_runtime::Value;
    // SAFETY: the caller's contract; the element is copied out before the
    // destination, which may be any slot, is written.
    unsafe {
        let Some(Value::Array(a)) = (*arr).get() else {
            return 1;
        };
        // The offset: an int key, or a string looked up as a key (a
        // numeric string names its int key, as everywhere).
        let found = match *idx {
            TAG_INT => a.get_int(std::ptr::read_unaligned(
                idx.add(PAYLOAD as usize).cast::<i64>(),
            )),
            // A slot holding a string is a `Value::Str` whatever kind of
            // slot it is (a variable's `Some` has its value's layout).
            TAG_STR => match &*idx.cast::<Value>() {
                // A literal key the tape already found to be no integer
                // key (`FetchDimSC`) is looked up as it is.
                Value::Str(s) if flags & DIM_GET_EXACT_KEY != 0 => a.get_zstr(s),
                Value::Str(s) => a.get_sym_zstr(s),
                _ => None,
            },
            _ => None,
        };
        let Some(Value::Int(v)) = found else {
            return 1;
        };
        let v = *v;
        if flags & DIM_GET_TO_CV != 0 {
            let slot = &*dst.cast::<crate::vm::CvSlot>();
            if matches!(
                slot.get(),
                Some(Value::Array(_) | Value::Object(_) | Value::Reference(_) | Value::Resource(_))
            ) {
                return 1;
            }
            // What was there is nothing, a scalar (it owns nothing: no
            // drop glue for it) or a string, which is released.
            match slot.set(Some(Value::Int(v))) {
                old @ Some(Value::Str(_)) => drop(old),
                old => std::mem::forget(old),
            }
        } else {
            let slot = &mut *dst.cast::<Value>();
            if !slot.is_plain() {
                return 1;
            }
            match std::mem::replace(slot, Value::Int(v)) {
                old @ Value::Str(_) => drop(old),
                old => std::mem::forget(old),
            }
        }
        0
    }
}

/// `C_DIMW`'s helper: `$arr[$idx] = $val` (`idx` null: `$arr[] = $val`)
/// when `arr` holds an array directly and alone (no other handle: no
/// separation), `val` an `Int`, and `idx` an `Int` key that is new to the
/// table or whose element is plain (a replaced scalar owns nothing, a
/// replaced string is released; an array, object or reference element
/// is the interpreted row's). Returns 0 having written; 1, nothing
/// changed, for anything else (an append past the largest key is the
/// interpreted row's error).
///
/// # Safety
/// As [`jit_dim_get`]: `arr` a compiled variable's slot, `idx` (when not
/// null) and `val` slots or literals.
unsafe extern "C" fn jit_dim_set(
    arr: *mut crate::vm::CvSlot,
    idx: *const u8,
    val: *const u8,
    _word: u32,
) -> u32 {
    use php_runtime::{ArrayKey, Value};
    // SAFETY: the caller's contract; the operands are read before the
    // array is borrowed mutably (they are other slots, or decline).
    unsafe {
        let Some(v) = slot_int(val) else {
            return 1;
        };
        let key = if idx.is_null() {
            None
        } else {
            match slot_int(idx) {
                Some(i) => Some(i),
                None => return 1,
            }
        };
        let Some(Value::Array(a)) = (*arr).as_mut() else {
            return 1;
        };
        let Some(tbl) = a.get_mut_unshared() else {
            return 1;
        };
        match key {
            None => u32::from(tbl.push(Value::Int(v)).is_err()),
            Some(i) => {
                if let Some(slot) = tbl.get_int_mut(i) {
                    if !slot.is_plain() {
                        return 1;
                    }
                    match std::mem::replace(slot, Value::Int(v)) {
                        old @ Value::Str(_) => drop(old),
                        old => std::mem::forget(old),
                    }
                } else if let Err(v) = tbl.assign_packed_int(i, Value::Int(v)) {
                    // Not the next element of a packed table (which the
                    // call above appends in place): a new key of a hash,
                    // a hole, a key past the end.
                    drop(tbl.insert(ArrayKey::Int(i), v));
                }
                0
            }
        }
    }
}

/// `C_DIMOP`'s helper: `$arr[$idx] op= $val` for an `Int` element, an
/// `Int` value and an int-int operator (`op`: `dims.rs`'s `op_code` of
/// `+`, `-`, `*`, `|`, `&`, `^`), in an array `arr` holds directly and
/// alone. Returns 0 having stored the result; 1, nothing changed, for
/// anything else: a missing key (the interpreted row warns), another
/// element or value type, an overflow (the interpreted row's result is a
/// float).
///
/// # Safety
/// As [`jit_dim_set`].
unsafe extern "C" fn jit_dim_op(
    arr: *mut crate::vm::CvSlot,
    idx: *const u8,
    val: *const u8,
    op: u32,
) -> u32 {
    use php_runtime::Value;
    // SAFETY: the caller's contract; as in `jit_dim_set`.
    unsafe {
        let (Some(i), Some(y)) = (slot_int(idx), slot_int(val)) else {
            return 1;
        };
        let Some(Value::Array(a)) = (*arr).as_mut() else {
            return 1;
        };
        let Some(tbl) = a.get_mut_unshared() else {
            return 1;
        };
        let Some(Value::Int(x)) = tbl.get_int_mut(i) else {
            return 1;
        };
        let r = match op {
            DIM_OP_ADD => x.checked_add(y),
            DIM_OP_SUB => x.checked_sub(y),
            DIM_OP_MUL => x.checked_mul(y),
            DIM_OP_OR => Some(*x | y),
            DIM_OP_AND => Some(*x & y),
            DIM_OP_XOR => Some(*x ^ y),
            _ => None,
        };
        match r {
            Some(r) => {
                *x = r;
                0
            }
            None => 1,
        }
    }
}

/// The target instruction index of the jump of the instruction at
/// index `i`, for the classes that jump.
fn jump_target(tape: &[Inst], i: usize) -> usize {
    #[allow(clippy::cast_possible_wrap)]
    let off = (tape[i].c as i32) as i64;
    (i as i64 + off / INST_SIZE) as usize
}

/// The region scan. `close` is the closing backward jump, `head` its
/// target; both are instruction indexes.
fn plan(
    st: &St,
    code: &crate::opcode::OpArray,
    tape: &[Inst],
    head: usize,
    close: usize,
) -> Option<Plan> {
    macro_rules! rej {
        ($why:expr) => {{
            if debug_on() {
                eprintln!("jit:   plan reject at {head}..={close}: {}", $why);
            }
            return None;
        }};
    }
    // `head == close` is a loop of one instruction: the fused
    // step-compare-jump of an empty `for` loop, jumping onto itself.
    if head > close || close - head + 1 > MAX_REGION {
        rej!("not a loop or too big");
    }
    let len = close - head + 1;
    let mut class = vec![0u16; len];
    let mut meta = vec![JitMeta::default(); len];
    let mut span = vec![0u32; len];
    let mut calls: Vec<Option<CallPlan>> = (0..len).map(|_| None).collect();

    // Class every instruction; a fused instruction's coverage marks
    // the ops it consumes, exactly as `run_core` skips them.
    let mut i = head;
    let mut last = usize::MAX;
    while i <= close {
        let (c, m, step, call) = if tape[i].op == TOp::CallInit {
            match plan_call(st, code, i) {
                Some(cp) => {
                    let span = cp.span;
                    (C_CALL, JitMeta::default(), span, Some(cp))
                }
                None => rej!(format!("[{i}] call not inlinable")),
            }
        } else if matches!(tape[i].op, TOp::FetchObjProp | TOp::FetchObjPropTo) {
            // A property read never spans: the folded `AssignCv` of a
            // `FetchObjPropTo` is scanned and emitted as its own row
            // (the read stores into its temporary and that row moves
            // it, mirroring the interpreted fall-through).
            match plan_prop(st, code, &tape[i]) {
                Some(m) => (C_PROP, m, 1, None),
                None => rej!(format!("[{i}] property read not inlinable")),
            }
        } else if tape[i].op == TOp::AssignObjProp {
            match plan_prop_write(st, code, &tape[i]) {
                Some(m) => (C_PROPW, m, 1, None),
                None => rej!(format!("[{i}] property write not inlinable")),
            }
        } else if tape[i].op == TOp::IncDecObjProp {
            match plan_prop_incdec(st, code, &tape[i]) {
                Some(m) => (C_PROPINC, m, 1, None),
                None => rej!(format!("[{i}] property inc/dec not inlinable")),
            }
        } else if tape[i].op == TOp::IssetObjProp {
            match plan_prop_isset(st, code, &tape[i]) {
                Some(m) => (C_PROPISSET, m, 1, None),
                None => rej!(format!("[{i}] property isset/empty not inlinable")),
            }
        } else if tape[i].op == TOp::AssignOpObjProp {
            match plan_prop_opa(st, code, &tape[i]) {
                Some(m) => (C_PROPOPA, m, 1, None),
                None => rej!(format!("[{i}] property op= not inlinable")),
            }
        } else if is_dim_row(tape[i].op) {
            match plan_dim(st, code, &tape[i]) {
                Some((c, m, step)) => (c, m, step, None),
                None => rej!(format!("[{i}] array row not inlinable")),
            }
        } else if tape[i].op == TOp::Ext {
            // A hot spec row the tape left to the interpreter: the op
            // itself is still at this index, so the planners decode it
            // (static properties and constants inline; anything else
            // stays the interpreted row).
            match plan_ext(st, code, i) {
                Some((c, m)) => (c, m, 1, None),
                None => rej!(format!("[{i}] spec row not inlinable")),
            }
        } else {
            match classify(st, &tape[i]) {
                Some((c, m)) => (c, m, c_step(c), None),
                None => rej!(format!("{:?} not whitelisted", tape[i].op)),
            }
        };
        class[i - head] = c;
        meta[i - head] = m;
        span[i - head] = step as u32;
        if call.is_some() {
            calls[i - head] = call;
        }
        if debug_on() {
            eprintln!(
                "jit:   scan [{i}] {:?} -> class {c}, step {step}",
                tape[i].op
            );
        }
        last = i;
        i += step;
    }
    // The scan must land *on* the close (a fused instruction's coverage
    // may run past it: those slots are outside the loop).
    if last != close {
        rej!("scan stepped over the close");
    }
    // The close must be a jump instruction the scan landed on exactly,
    // and it must jump to the head (that is what a back edge is).
    let cc = class[close - head];
    if cc != C_JMP && cc != J_CMP && cc != J_INCCMP {
        rej!("close is not a jump class");
    }
    if jump_target(tape, close) != head {
        rej!(format!(
            "close jumps to {}, not the head",
            jump_target(tape, close)
        ));
    }

    // Guards: every slot read as an int must be exact; slots only
    // written get the weak guard. A merged exact guard is sound: every
    // whitelisted write to an exact-guarded slot keeps it an int.
    // An inlined call's slots live in the scratch buffer and are tracked
    // by its own known-Int analysis, so they get no entry guard; its
    // *caller* operands (the `Send` sources) are read as ints by the
    // trace, and its result temporary is fully written.
    let has_calls = calls.iter().any(|c| c.is_some());
    // Some inlined method call re-checks the frame's calling scope (and
    // reads a `$this` receiver through the frame), or a property read
    // re-checks the scope: keep the frame pointer (entry argument `rdx`)
    // in `r10` for those guards.
    let frame = class.contains(&C_PROP)
        || class.contains(&C_PROPW)
        || class.contains(&C_PROPINC)
        || class.contains(&C_PROPISSET)
        || class.contains(&C_PROPOPA)
        || class.contains(&C_SPROP)
        || class.contains(&C_SPROPW)
        || class.contains(&C_SPROPISSET)
        || class.contains(&C_CONSTI)
        || calls.iter().flatten().any(|cp| cp.recv.is_some());
    let mut guards: Vec<Guard> = Vec::new();
    // Temporary offsets fully stored by an earlier row of the region (in
    // program order), by what the store left there: an `Int` (`defs`:
    // reads of those at guard time are dead), a `Bool` an
    // `isset()`/`empty()` row stored (`bools`) or the `Null` of an
    // inlined call that returned nothing (`nulls`). The last two are
    // fresh to *copy*, never ints to do arithmetic with, and a temporary
    // is in at most one list (`set_tmp_kind`). `cvbools` are the compiled
    // variables whose current trace-time value is such a `Bool` or
    // `Null` (no later row may read those as ints), and `ever_nonint`
    // every compiled variable the region writes one into anywhere.
    let mut defs: Vec<u32> = Vec::new();
    let mut bools: Vec<u32> = Vec::new();
    let mut nulls: Vec<u32> = Vec::new();
    let mut cvbools: Vec<u32> = Vec::new();
    let mut ever_nonint: Vec<u32> = Vec::new();
    for j in 0..len {
        if class[j] == 0 {
            continue;
        }
        if class[j] == C_CALL {
            let cp = calls[j].as_ref().unwrap();
            // An argument is read as an `Int`: a temporary or variable the
            // region itself left a `Bool` or `Null` in cannot be one (an
            // entry guard would only test what it held at the entry).
            if !cp.sends.iter().all(|arg| match arg.kind {
                KT if defs.contains(&arg.off) => true,
                KT if bools.contains(&arg.off) || nulls.contains(&arg.off) => false,
                KC if cvbools.contains(&arg.off) => false,
                KC | KT => add_slot(&mut guards, arg.kind, arg.off, true),
                // A dropped send of a compiled variable (`plan_call` left
                // its slot in `off`): whatever it holds is unused, but it
                // must hold something - the interpreted send of an
                // undefined variable warns.
                KSKIP if arg.lit == 1 => add_defined(&mut guards, arg.off),
                _ => true,
            }) {
                rej!(format!("guards for the call at {}", head + j));
            }
            if let Some(d) = cp.dst {
                if !add_slot(&mut guards, KT, d, false) {
                    rej!(format!("guard for the call result at {}", head + j));
                }
                // The result temporary is fully stored: with the `Int`
                // the callee returned, or with the `Null` of a bare
                // `return;` - which is no int for the rows after it
                // (`$x = f();` must store a null, not `int(0)`).
                let kind = if cp.ret.kind == KNULL_ARG {
                    TmpKind::Null
                } else {
                    TmpKind::Int
                };
                set_tmp_kind(&mut defs, &mut bools, &mut nulls, d, kind);
            }
            continue;
        }
        if matches!(class[j], C_DIMR | C_DIMW | C_DIMOP) {
            // An offset or a value the region itself left a `Bool` or a
            // `Null` in is no `Int`: the helper would decline on every
            // iteration, so the region is not worth compiling.
            let m = &meta[j];
            let non_int = |kind: u8, off: u32| match kind {
                KT => bools.contains(&off) || nulls.contains(&off),
                KC => cvbools.contains(&off),
                _ => false,
            };
            if non_int(m.kb, m.slot) || (class[j] != C_DIMR && non_int(m.sc, m.sw)) {
                rej!(format!("array row at {} has a non-int operand", head + j));
            }
        }
        if !add_class_guards(
            tape,
            head + j,
            class[j],
            meta[j],
            &defs,
            &bools,
            &nulls,
            &cvbools,
            &mut guards,
        ) {
            rej!(format!("guards at [{}, {:?}]", head + j, tape[head + j].op));
        }
        // A `Bool` or `Null` temporary copied into a compiled variable
        // (`$ok = isset(...)`, `$ok = empty(...)`, `$r = f()` of a
        // function that returns nothing) writes that value there for the
        // trace's whole run: flag the row so the emitted store writes the
        // right tag and value, and record the variable as not an `Int`.
        if class[j] == J_ASSIGN_T {
            let b = tape[head + j].b;
            let flag = if bools.contains(&b) {
                BOOL_SRC
            } else if nulls.contains(&b) {
                NULL_SRC
            } else {
                0
            };
            if flag != 0 {
                meta[j].mask = flag;
                let a = tape[head + j].a;
                if !cvbools.contains(&a) {
                    cvbools.push(a);
                }
                if !ever_nonint.contains(&a) {
                    ever_nonint.push(a);
                }
            }
        }
        // What this row leaves in the temporaries it fully stores, seen
        // in order by the rows after it.
        match class[j] {
            // A property or static-property read and a folded constant:
            // the destination temporary holds an `Int`.
            C_PROP | C_SPROP => {
                set_tmp_kind(&mut defs, &mut bools, &mut nulls, meta[j].db, TmpKind::Int);
            }
            // A folded constant: what the constant is.
            C_CONSTI => {
                let kind = match meta[j].arith {
                    CONST_NULL => TmpKind::Null,
                    CONST_BOOL => TmpKind::Bool,
                    _ => TmpKind::Int,
                };
                set_tmp_kind(&mut defs, &mut bools, &mut nulls, meta[j].db, kind);
            }
            // A write, an `op=` and a `++`/`--`: the result temporary (if
            // kept) is fully stored with the `Int`; a consumed `Tmp`
            // source is left `TAG_NULL` (the interpreted row nulls it),
            // so it is no longer a known `Int`.
            C_PROPW | C_PROPOPA | C_SPROPW | C_PROPINC => {
                if meta[j].db != NO_DST {
                    set_tmp_kind(&mut defs, &mut bools, &mut nulls, meta[j].db, TmpKind::Int);
                }
                if meta[j].consume && meta[j].sc == KT {
                    set_tmp_kind(
                        &mut defs,
                        &mut bools,
                        &mut nulls,
                        meta[j].sw,
                        TmpKind::Unknown,
                    );
                }
            }
            // `isset`/`empty` store a `Bool`: a fresh value, never a
            // known `Int`, replacing whatever the slot held.
            C_PROPISSET | C_SPROPISSET => {
                set_tmp_kind(&mut defs, &mut bools, &mut nulls, meta[j].db, TmpKind::Bool);
            }
            // The source temporary is `TAG_NULL` after this row (the
            // consume), whatever it held.
            J_ASSIGN_T => {
                let b = tape[head + j].b;
                set_tmp_kind(&mut defs, &mut bools, &mut nulls, b, TmpKind::Unknown);
            }
            J_ARITH_TMP | J_NUMBIN_T => {
                let c = tape[head + j].c;
                set_tmp_kind(&mut defs, &mut bools, &mut nulls, c, TmpKind::Int);
            }
            C_ABS => {
                let c = tape[head + j].c;
                if c != u32::MAX {
                    set_tmp_kind(&mut defs, &mut bools, &mut nulls, c, TmpKind::Int);
                }
            }
            // An array read stored an `Int` into its destination (or left
            // the trace): a temporary is a known `Int`, a variable is one
            // again whatever an earlier row left there.
            C_DIMR => {
                let d = meta[j].db;
                if meta[j].kc == KT {
                    set_tmp_kind(&mut defs, &mut bools, &mut nulls, d, TmpKind::Int);
                } else {
                    cvbools.retain(|c| c != &d);
                }
            }
            // An array write moved a consumed value temporary out (the
            // row nulls it, as the interpreted one does).
            C_DIMW | C_DIMOP => {
                if meta[j].consume {
                    let t = meta[j].sw;
                    set_tmp_kind(&mut defs, &mut bools, &mut nulls, t, TmpKind::Unknown);
                }
            }
            _ => {}
        }
        // Any row that fully stores an int into a compiled variable (or
        // an int from a temporary through `AssignT`) ends its `Bool`/
        // `Null` state for the rows after it.
        match class[j] {
            C_INC | C_DEC | C_ASSIGN_I | C_ASSIGN_L | J_INCCMP | J_ASSIGN_OP | J_NUMBIN_C => {
                let a = tape[head + j].a;
                cvbools.retain(|c| c != &a);
            }
            J_ARITH_CV => {
                let a = tape[head + j].c;
                cvbools.retain(|c| c != &a);
            }
            J_NUMBIN_D => {
                let a = tape[head + j].d;
                cvbools.retain(|c| c != &a);
            }
            J_ASSIGN_T if meta[j].mask == 0 => {
                let a = tape[head + j].a;
                cvbools.retain(|c| c != &a);
            }
            _ => {}
        }
    }
    // A compiled variable the region writes a `Bool` or `Null` into is no
    // `Int` from the second iteration on, wherever in the body that write
    // is. An exact entry guard on it (a row *before* the write reads it
    // as an int, or stores only a payload into it trusting its tag) would
    // hold at the entry and be false one iteration later, inside the
    // trace, where nothing re-checks it. Such a region is not compiled.
    if guards
        .iter()
        .any(|g| g.base == KC && g.exact && ever_nonint.contains(&g.off))
    {
        rej!("a variable is read as an int and assigned a bool or null in the same loop");
    }
    // With calls in the region, `rdx` holds the scratch base for its
    // whole run, so no `%` (whose `idiv` clobbers it) may be compiled
    // anywhere in it.
    if has_calls {
        let id5 = |ms: &[JitMeta]| ms.iter().any(|m| m.arith == 5);
        if id5(&meta) || calls.iter().flatten().any(|cp| id5(&cp.metas)) {
            rej!("% with calls (rdx is the scratch base)");
        }
    }

    // Every mid-region jump must land on the head, on a region
    // instruction (which gets a label), or beyond the region (an exit
    // that resumes there). Backward jumps to a non-head (a nested
    // loop) and landings on covered slots are refused in this version.
    let mut entries: Vec<usize> = Vec::new();
    for j in 0..len {
        let c = class[j];
        if c == 0 {
            continue;
        }
        if (c == J_CMP || c == J_INCCMP) && jcc_for(meta[j].mask).is_none() {
            rej!(format!("unknown cmp mask {}", meta[j].mask));
        }
        if c == C_JMP || c == J_CMP || c == J_INCCMP {
            let y = jump_target(tape, head + j);
            if y == head || y == close && j == len - 1 {
                continue;
            }
            if y > head && y <= close {
                if class[y - head] == 0 {
                    rej!(format!("jump lands on covered slot {y}"));
                }
                if !entries.contains(&y) {
                    entries.push(y);
                }
            } else if y < head {
                rej!(format!("nested backward jump to {y}"));
            }
        }
    }
    entries.sort_unstable();
    guards.sort_by_key(|g| (g.base, g.off));
    // A pinned cell or constant fold is only as fresh as the runtime
    // caches that produced it; the prologue's version guard covers the
    // whole trace when any row of this trace depends on them.
    let version_guard = class
        .iter()
        .any(|&c| matches!(c, C_SPROP | C_SPROPW | C_SPROPISSET | C_CONSTI));
    Some(Plan {
        head,
        close,
        class,
        meta,
        guards,
        entries,
        span,
        calls,
        has_calls,
        frame,
        version_guard,
        version: FUNC_TABLE_VERSION.load(Ordering::Relaxed),
    })
}

/// The whole analysis of the plain user-function call started by the
/// `InitFCall` at `init`, the static method call started by a
/// named-class literal-method `InitStaticCall` there, or the instance
/// method call started by a literal-method `InitMethodCall` there:
/// resolve the callee, read the call's shape out of the raw ops (the
/// init row, the `Send`s and the `DoCall` the fused `CallInit` row
/// stands for, at the same indexes), and decode the callee's
/// straight-line body into [`CallPlan`] rows whose slot offsets address
/// the trace's scratch buffer. A static call's callee comes from the
/// site's `fcall_cache` slot past the literals (the `prepare_static`
/// formula); an instance call's from the site's own `method_cache` slot
/// (the `prepare_method` formula), with the receiver's class re-checked
/// at run time by a guard. The scopes a static call resolved are not
/// needed by an inlined straight-line body, which cannot read them (no
/// whitelisted row touches a frame, `$this` or `self::`). Every
/// assumption is plan-time or guarded by the trace's function-table
/// version check; `None` refuses the inlining (and with it the whole
/// region).
fn plan_call(st: &St, code: &crate::opcode::OpArray, init: usize) -> Option<CallPlan> {
    macro_rules! rj {
        ($($a:tt)*) => {{
            if debug_on() {
                eprintln!("jit:   call reject at {init}: {}", format_args!($($a)*));
            }
            return None;
        }};
    }
    // A refusal for a call cache that holds nothing for this site (yet):
    // worth another attempt once the call has run (see `soft`).
    macro_rules! rjs {
        ($($a:tt)*) => {{
            PLAN_SOFT.with(|s| s.set(true));
            rj!($($a)*)
        }};
    }
    // An observed call runs the function's entry/exit hooks, which an
    // inlined call cannot.
    if OBSERVER_ON.load(Ordering::Relaxed) {
        rj!("observer on");
    }
    // The raw shape: a plain named call, value sends, then the `DoCall`.
    // (Any other shape lowers to *raw* `Op`s, not to the fused
    // `CallInit`/`CallSend` rows this scan matched, so it never reaches
    // here; the re-match is belt-and-braces.)
    // `Some(name)` for a plain call (its callee is cached by the name
    // literal); `None` for a static call (cached at a per-site slot);
    // and for an instance call the site's receiver operand and method
    // name literal (the callee is cached at the site's own
    // `method_cache` slot, its class re-checked at run time).
    let (name, mcall) = match code.ops.get(init)? {
        Op::InitFCall {
            name,
            fallback: None,
            ..
        } => (Some(*name), None),
        Op::InitStaticCall {
            class: crate::opcode::ClassSpec::Named(_),
            name: Operand::Lit(_),
        } => (None, None),
        Op::InitMethodCall {
            obj: obj @ (Operand::Cv(_) | Operand::Tmp(_) | Operand::TmpKeep(_) | Operand::Unused),
            name: Operand::Lit(lit),
        } => (None, Some((obj, *lit))),
        _ => rj!(
            "raw row is not a plain InitFCall, a Named/Lit InitStaticCall \
             or a Cv/Tmp/Unused/Lit InitMethodCall"
        ),
    };
    let mut sends: Vec<Arg> = Vec::new();
    let mut ci = init + 1;
    let dst = loop {
        match code.ops.get(ci) {
            Some(Op::Send { src: arg }) => {
                let o = match arg {
                    ArgSrc::Val(o @ (Operand::Lit(_) | Operand::Tmp(_) | Operand::Cv(_)))
                    | ArgSrc::CallResult(o @ Operand::Tmp(_)) => *o,
                    // `f($x)` passes a plain variable as a *place*; the
                    // plain shapes have no by-ref parameter, so it is
                    // just a value read of that compiled variable.
                    ArgSrc::Place(pid) => match super::calls::place_cv(code, *pid) {
                        Some(cv) => Operand::Cv(cv),
                        None => rj!("place arg is not a plain variable"),
                    },
                    _ => rj!("send arg is not a plain value"),
                };
                let (kind, off) = match o {
                    // A literal operand of any kind (even a string, as
                    // in `hallo("hallo")`): a `KL` whose value is only
                    // resolved below if the send turns out to be live;
                    // a dead send is dropped whatever it holds.
                    Operand::Lit(l) if l < super::SLOT_LIMIT => (KL, l << SLOT_SHIFT),
                    _ => match src(code, o) {
                        Some(ko) => ko,
                        None => rj!("send arg {o:?} has no inlinable source"),
                    },
                };
                if kind == KI {
                    // A small-int constant operand: `src` carried its
                    // value in the offset; make it a plain literal.
                    let v = i64::from(off as i32);
                    sends.push(Arg {
                        kind: KL,
                        off: 0,
                        lit: v,
                    });
                    ci += 1;
                    continue;
                }
                // A `KL`'s value is resolved later, only if the send
                // turns out to be live; keep the literal's index until
                // then.
                sends.push(Arg { kind, off, lit: 0 });
                ci += 1;
            }
            Some(Op::DoCall { dst }) => break *dst,
            None => rj!("op array ends inside the call"),
            _ => rj!(
                "row {ci} after InitFCall is neither Send nor DoCall: {:?}",
                code.ops.get(ci)
            ),
        }
    };
    let ns = sends.len();
    if ns > MAX_CALL_ARGS {
        rj!("too many args");
    }

    // The callee: the run-time cache the interpreted call path filled
    // for this exact site, still alive and a plain user function. For
    // an instance call the receiver's class is *not* a plan-time
    // assumption: the emitted guard re-reads it at run time (poll and
    // cap exits resume without disarming), so only the cache entry's
    // shape is checked here, mirroring `prepare_method`.
    let mut recv: Option<(u8, u32, u64)> = None;
    let mut recv_scope = 0i64;
    let uf = if let Some(name) = name {
        let cache = code.fcall_cache.borrow();
        match cache.get(name as usize) {
            None => rjs!("no fcall cache slot for the name"),
            Some(None) => rjs!("fcall cache not filled"),
            Some(Some(cell)) => match cell.get() {
                Some(Func::User(uf)) => uf,
                Some(Func::Builtin(_)) => rj!("callee is a builtin"),
                None => rjs!("cached callee is dead"),
            },
        }
    } else if let Some((obj, lit)) = mcall {
        let (kind, off) = match obj {
            // `$this` is not a slot: the guard reads the frame field.
            Operand::Unused => (KTHIS, 0),
            _ => match src(code, *obj) {
                Some((kind @ (KC | KT), off)) => (kind, off),
                _ => rj!("method-call receiver has no inlinable source"),
            },
        };
        let cache = code.method_cache.borrow();
        let Some(cached) = cache.get(init).and_then(|c| c.as_ref()) else {
            rjs!("method cache slot unfilled");
        };
        if cached.name_lit != Some(lit) {
            rjs!("method cache entry is for another name");
        }
        if cached.class_addr == 0 {
            rjs!("method cache entry has no class");
        }
        // The entry answers one calling scope only: `prepare_method`
        // compares the frame's scope with it on every call, because
        // another scope may see another method of that name, or none (a
        // private method, a closure bound elsewhere over the same op
        // array). The plan runs in the frame that enters the trace, so
        // its scope is the one the entry must be for, and the emitted
        // guard pins it for every later frame.
        // SAFETY: the frame outlives the plan (the trace runs in it).
        let scope = unsafe { &*st.f }
            .scope
            .as_ref()
            .map_or(0, |s| Rc::as_ptr(s) as usize);
        if cached.scope_addr != scope {
            rjs!("method cache entry is for another calling scope");
        }
        recv_scope = if scope == 0 {
            0
        } else {
            scope as i64 - i64::from(RC_VALUE_OFFSET)
        };
        let cls = match cached.class.upgrade() {
            Some(c) => c,
            None => rjs!("cached receiver class is dead"),
        };
        if cls.hooks.as_ref().and_then(|h| h.get_method).is_some() {
            rj!("receiver class has a get_method hook");
        }
        // The guard: this slot must hold an object of exactly this class
        // on every compiled call.
        recv = Some((kind, off, cached.class_addr as u64));
        match cached.func.get() {
            Some(Func::User(uf)) => uf,
            Some(Func::Builtin(_)) => rj!("method is a builtin"),
            None => rjs!("cached method is dead"),
        }
    } else {
        // The static-call site's slot (past the literals, the
        // `prepare_static` formula): a resolved method whose declaring
        // scope and called class are still alive.
        let slot = code.literals.len() + init + 1;
        match code
            .fcall_cache
            .borrow()
            .get(slot)
            .and_then(|c| c.as_ref().and_then(crate::opcode::CachedFunc::get_call))
        {
            None => rjs!("static call cache slot unfilled or stale"),
            Some((Func::User(uf), _, _)) => uf,
            Some((Func::Builtin(_), _, _)) => rj!("callee is a builtin"),
        }
    };
    let shape = uf.call_shape();
    if shape == crate::func::CallShape::NOT_PLAIN || !uf.uses.is_empty() {
        rj!("callee not plain or has `use`s");
    }
    if uf.required as usize > ns {
        rj!("too few args");
    }
    // More sends than declared parameters are allowed: the extras are
    // dead (a plain user function ignores them), and the liveness scan
    // below turns them (and any send to an unused parameter) into
    // `KSKIP`.
    if shape == crate::func::CallShape::TYPED {
        // The trace always passes `Int`s: every parameter must accept
        // one without coercion. (The return type is checked below, once
        // the body's `Return` operand is known, for every shape.)
        if uf
            .params
            .iter()
            .any(|p| p.ty.as_ref().is_some_and(|t| t.mask & mask::LONG == 0))
        {
            rj!("typed callee rejects an int param");
        }
    }

    // The callee body, from the first op after its `RECV`s to its
    // `Return`: straight-line, every row in the arithmetic/assignment
    // whitelist (no jump of any kind, and no nested call: the
    // whitelist has no call class, and no `CallInit` row can classify).
    let cst = St {
        cvs: std::ptr::null_mut(),
        tmps: std::ptr::null_mut(),
        lits: uf.code.literals.as_ptr().cast(),
        f: std::ptr::null_mut(),
    };
    let ncvs = uf.code.cv_names.len();
    let nt = uf.code.num_tmps as usize;
    let mut classes: Vec<u16> = Vec::new();
    let mut metas: Vec<JitMeta> = Vec::new();
    let mut rows: Vec<Inst> = Vec::new();
    let mut bc = uf.plain_first_op(ns);
    let ret = loop {
        match uf.code.ops.get(bc) {
            Some(Op::Return { val }) => break *val,
            Some(_) => {}
            None => rj!("callee body walk ran off the op array at {bc}"),
        }
        let row = uf.code.tape.get(&uf.code)[bc];
        let (c, m) = match classify(&cst, &row) {
            Some(cm) => cm,
            None => rj!("callee row {bc} ({:?}) not classifiable", row.op),
        };
        if !matches!(
            c,
            C_INC
                | C_DEC
                | C_ASSIGN_I
                | C_ASSIGN_L
                | J_ASSIGN_T
                | J_ARITH_TMP
                | J_NUMBIN_T
                | J_ARITH_CV
                | J_ASSIGN_OP
                | J_NUMBIN_C
        ) {
            rj!("callee row {bc} ({:?}) off whitelist", row.op);
        }
        if classes.len() >= MAX_CALLEE {
            rj!("callee too long");
        }
        classes.push(c);
        metas.push(m);
        rows.push(row);
        bc += c_step(c);
    };
    // The declared return type, whatever the parameters are (the call
    // shape only says whether a *parameter* is typed): the interpreted
    // `Return` verifies it for every function that has one
    // (`Vm::verify_return`), and a value the type does not accept as it
    // is gets coerced there (`: float`, `: string` and `: bool` turn an
    // int into something else) or raises a `TypeError`. The inlined body
    // hands back an `Int`, or nothing for a bare `return;`, so only a
    // return the check passes unchanged inlines: an int where the type
    // accepts one, nothing where the type is `void`.
    if let Some(rt) = uf.return_type.as_ref() {
        let passes = match ret {
            Operand::Unused => rt.is_void(),
            _ => !rt.is_void() && !rt.is_never() && (rt.is_mixed() || rt.mask & mask::LONG != 0),
        };
        if !passes {
            rj!("callee's declared return type would coerce or reject the result");
        }
    }

    // The scratch slice: the callee's compiled variables, then its
    // temporaries, 16 bytes each; the `KR` offsets below are relative
    // to it (`set_base` adds the call's place in the buffer).
    let scratch_idx = |kind: u8, off: u32| -> Option<usize> {
        let s = off >> 4;
        match kind {
            KC if (s as usize) < ncvs => Some(s as usize),
            KT if (s as usize) < nt => Some(ncvs + s as usize),
            _ => None,
        }
    };
    // Which parameters the inlined body touches, while the rows are
    // still raw (before the remap): its `KC` operands, and a `Return`
    // of a compiled variable. Over-reading is safe (a send stays that
    // could have been dropped); a parameter never mentioned is *dead*,
    // and its send can go whatever the caller-side value was (even a
    // non-int literal: `hallo("hallo")` to a parameterless body sends
    // a dead argument). So do the sends' `KL` resolutions and `KSKIP`
    // conversions here.
    let mut live = vec![false; ncvs];
    for k in 0..classes.len() {
        let m = &metas[k];
        let row = rows[k];
        for (kind, off) in [(m.ka, row.a), (m.kb, row.b), (m.kc, row.c)] {
            if kind == KC {
                if let Some(i) = scratch_idx(kind, off) {
                    live[i] = true;
                }
            }
        }
    }
    if let Operand::Cv(i) = ret {
        if (i as usize) < ncvs {
            live[i as usize] = true;
        }
    }
    // A typed parameter's argument is checked against the declaration
    // whether or not the body reads it (`calls::enter`: a value the type
    // does not accept as it is leaves the plain call for the generic one,
    // which coerces it or throws). So its send is never dead: it stays a
    // live send, which the trace only makes with an `Int` - a literal
    // resolved below, or a slot guarded as one - and the check above
    // established that every declared type takes an `Int` unchanged.
    for (j, p) in uf.params.iter().enumerate() {
        if j < ncvs && p.ty.as_ref().is_some_and(|t| !t.is_mixed()) {
            live[j] = true;
        }
    }
    for (j, arg) in sends.iter_mut().enumerate() {
        if j >= uf.params.len() || j >= ncvs || !live[j] {
            // Dropped. A compiled variable's slot stays in `off`, marked
            // by `lit == 1`: the interpreted send reads the variable, and
            // reading an undefined one warns, so `plan` guards that it
            // holds a value (anything: the value itself is unused).
            let from_cv = arg.kind == KC;
            arg.kind = KSKIP;
            if from_cv {
                arg.lit = 1;
            } else {
                arg.off = 0;
                arg.lit = 0;
            }
        } else if arg.kind == KL {
            arg.lit = resolved_lit(st, arg.off)?;
        }
    }
    // The parameters the trace actually wrote start as known `Int`s;
    // a slot whose every write is a compiled Int store needs no
    // runtime tag check, so the walk tracks which those are.
    let mut known = vec![false; ncvs + nt];
    for (j, arg) in sends.iter().enumerate() {
        if j < ncvs && arg.kind != KSKIP {
            known[j] = true;
        }
    }
    // Slot kinds become `KR`, and a `KT` offset moves past the
    // compiled-variable part of the slice.
    let remap = |kind: &mut u8, off: &mut u32| -> bool {
        match *kind {
            KC => {
                *kind = KR;
                true
            }
            KT => {
                *kind = KR;
                *off += (ncvs as u32) << 4;
                true
            }
            KI | KL => true,
            _ => false,
        }
    };
    for k in 0..classes.len() {
        let c = classes[k];
        let m = &mut metas[k];
        let row = &mut rows[k];
        let (ia, ib, ic) = (row.a, row.b, row.c);
        // Reads the instruction makes, before any write.
        let read = |kind: u8, off: u32, known: &Vec<bool>| -> bool {
            match kind {
                KI | KL => true,
                KC | KT => scratch_idx(kind, off).is_some_and(|i| known[i]),
                _ => false,
            }
        };
        // `ka` is a pure destination for the plain assignments (`$x =
        // 5`, `$x = TMP`): a fresh local there is written, never read,
        // and the row stores its tag and payload in full. Every other
        // class reads `ka` (C_INC/C_DEC and `op=` re-read what they
        // write), so a not-yet-known slot refuses the region.
        let reads_a = !matches!(c, C_ASSIGN_I | C_ASSIGN_L | J_ASSIGN_T);
        if (reads_a && !read(m.ka, ia, &known)) || !read(m.kb, ib, &known) {
            rj!("callee row reads a slot that is not a known int");
        }
        // The kind/offset remap, per class, and the writes.
        match c {
            C_INC | C_DEC => {
                if !remap(&mut m.ka, &mut row.a) {
                    return None;
                }
                known[scratch_idx(KC, ia)?] = true;
            }
            C_ASSIGN_I | C_ASSIGN_L => {
                if !remap(&mut m.ka, &mut row.a) {
                    return None;
                }
                known[scratch_idx(KC, ia)?] = true;
            }
            J_ASSIGN_T => {
                // The interpreted row also *nulls* the source temp: the
                // slot is no longer a known `Int`.
                if !remap(&mut m.ka, &mut row.a) || !remap(&mut m.kb, &mut row.b) {
                    return None;
                }
                known[scratch_idx(KC, ia)?] = true;
                if let Some(i) = scratch_idx(KT, ib) {
                    known[i] = false;
                }
            }
            J_ARITH_CV | J_ARITH_TMP | J_NUMBIN_T => {
                if !remap(&mut m.ka, &mut row.a)
                    || !remap(&mut m.kb, &mut row.b)
                    || !remap(&mut m.kc, &mut row.c)
                {
                    return None;
                }
                if m.kc != KI && m.kc != KL {
                    m.kc = KR;
                }
                known[scratch_idx(if c == J_ARITH_CV { KC } else { KT }, ic)?] = true;
            }
            _ => {
                // `J_ASSIGN_OP` / `J_NUMBIN_C`: `inst.c` is not a slot
                // (a jump offset or literal slot); the destination is
                // `inst.a`, which the read above already checked.
                if !remap(&mut m.ka, &mut row.a) || !remap(&mut m.kb, &mut row.b) {
                    return None;
                }
                m.kc = KR;
                // Both update a *compiled variable* in place (`classify`
                // only accepts `J_NUMBIN_C` with `ka == KC`): the slot
                // marked is that variable's, not the temporary with the
                // same index.
                known[scratch_idx(KC, ia)?] = true;
            }
        }
    }

    // The `Return` operand, as the caller's `DoCall` result.
    let ret = match ret {
        Operand::Unused => Arg {
            kind: KNULL_ARG,
            off: 0,
            lit: 0,
        },
        o => {
            let (mut kind, mut off) = match src(&uf.code, o) {
                Some(ko) => ko,
                None => rj!("callee return {o:?} has no inlinable source"),
            };
            if kind == KI {
                let v = i64::from(off as i32);
                kind = KL;
                off = 0;
                Arg { kind, off, lit: v }
            } else {
                let lit = if kind == KL {
                    resolved_lit(&cst, off)?
                } else {
                    0
                };
                if kind == KC || kind == KT {
                    if !scratch_idx(kind, off).is_some_and(|i| known[i]) {
                        rj!("callee return slot is not a known int");
                    }
                    // A callee slot: the scratch slice holds the
                    // temporaries after the compiled variables.
                    if kind == KT {
                        off += (ncvs as u32) << 4;
                    }
                    kind = KR;
                }
                Arg { kind, off, lit }
            }
        }
    };
    // The result temporary must be a real slot of the caller.
    let dst = match dst {
        Some(d) => Some(tmp_off(code, d)?),
        None => None,
    };
    Some(CallPlan {
        uf,
        callove: init,
        span: ci - init + 1,
        sends,
        dst,
        classes,
        metas,
        rows,
        ret,
        recv,
        recv_scope,
        base: 0,
        size: (((ncvs + nt) as u32) << 4).max(16),
    })
}

// --------------------------------------------------------------- emitter

/// The jcc opcode (the second byte of `0F cc`, rel32 form) for a
/// comparison mask, or `None` for a mask the emitter does not know.
/// Masks are the tape's `Lt/Eq/Le/Gt/Ne/Ge` numbering.
fn jcc_for(mask: u8) -> Option<u8> {
    Some(match mask {
        1 => 0x8C, // jl
        2 => 0x84, // je
        3 => 0x8E, // jle
        4 => 0x8F, // jg
        5 => 0x85, // jne
        6 => 0x8D, // jge
        _ => return None,
    })
}

/// Where a branch continues: the loop head, a mid-region label, an
/// absolute position in the code written so far (a fused op's
/// fall-through), or one of the exits, which return to `back_edge`:
/// - `Stub`: the loop was left (its exit jump, or the iteration cap):
///   resume at that instruction.
/// - `Side`: a row could not go on (a guard on its operands, an overflow,
///   a helper that declined): resume at that row, which the interpreter
///   runs. The exit reports the iteration it happened in, so that a trace
///   that keeps leaving in its first iteration can be switched off.
/// - `Poll(resume, row)`: the safe point came due at `row`: resume at
///   `resume` once it is serviced.
/// - `Disarm`: an assumption the trace was built on does not hold (an
///   entry guard): resume there and never enter this trace again.
/// - `Replan`: what the trace pinned at plan time is stale (the function
///   table or a run-time cache changed since: a declaration, an include,
///   an autoloaded class): resume there, forget this trace and count the
///   loop toward a new plan made from the caches as they are now.
#[derive(Clone, Copy)]
enum Target {
    Head,
    Label(usize),
    Stub(usize),
    Side(usize),
    Poll(usize, usize),
    Disarm(usize),
    Replan(usize),
    At(usize),
}

/// What sets a [`Target::Replan`] return apart from a `Disarm`'s
/// `-(idx + 1)`: it is lower by this much (an instruction index is far
/// below it).
const REPLAN: i64 = 1 << 40;

/// The return word of a [`Target::Replan`] exit resuming at `idx`.
fn replan_word(idx: usize) -> i64 {
    -(idx as i64 + 1) - REPLAN
}

/// The return word of a [`Target::Poll`] exit: the instruction to resume
/// at in the low half, the row the poll belongs to above it, plus one (so
/// that 0 there means "not a poll exit", what every other exit returns).
fn poll_word(resume: usize, row: usize) -> i64 {
    ((row as i64 + 1) << 32) | (resume as i64 & 0xFFFF_FFFF)
}

/// One argument of a helper call: the address of a frame slot (`KC`/`KT`
/// base and byte offset), an absolute address (a literal's `Value` in the
/// op array's table), or a null pointer.
#[derive(Clone, Copy)]
enum HArg {
    Slot(u8, u32),
    Abs(i64),
    Null,
}

/// The x86-64 emitter. Slots are addressed off `rdi` (`st.cvs`) and
/// `rsi` (`st.tmps`); `rax` carries the int being computed, `r9` the
/// second operand, `r11` the poll address, and `r8d` the iteration
/// counter. Everything that could overflow or meet a non-int exits with
/// `jo`/`jne` before any store, so a side exit leaves the slots exactly
/// as the interpreted step declining this instruction would have.
struct Emitter {
    code: Vec<u8>,
    plan: Plan,
    tape: Vec<Inst>,
    /// The scratch buffer's address (`has_calls`: `rdx` is set to it in
    /// the prologue; every `KR` offset is relative to it).
    scratch: i64,
    labels: HashMap<usize, usize>,
    stubs: HashMap<usize, usize>,
    sides: HashMap<usize, usize>,
    polls: HashMap<(usize, usize), usize>,
    disms: HashMap<usize, usize>,
    replans: HashMap<usize, usize>,
    fixups: Vec<(usize, Target)>,
    head_pos: usize,
    /// Counter for the synthetic forward labels the `isset`/`empty`
    /// tag dispatch registers (see [`Emitter::synth_label`]).
    synth: usize,
}

/// REX.W (0x40) plus the ModRM-reg field's high bit (0x04) plus the
/// ModRM-rm field's high bit (0x01), for the encodings below.
const REX_W: u8 = 0x48;
const REX: u8 = 0x40; // a REX prefix without W (the bits below need it)
const REX_R: u8 = 0x04;
const REX_B: u8 = 0x01;

impl Emitter {
    fn put(&mut self, b: &[u8]) {
        self.code.extend_from_slice(b);
    }

    /// `jmp rel32` to `t`.
    fn jmp(&mut self, t: Target) {
        let at = self.code.len();
        self.put(&[0xE9, 0, 0, 0, 0]);
        self.fixups.push((at + 1, t));
    }

    /// `call rel32` to `t` (a function compiled whole calling its own
    /// body).
    fn call(&mut self, t: Target) {
        let at = self.code.len();
        self.put(&[0xE8, 0, 0, 0, 0]);
        self.fixups.push((at + 1, t));
    }

    /// `jcc cc rel32` to `t`.
    fn jcc(&mut self, cc: u8, t: Target) {
        let at = self.code.len();
        self.put(&[0x0F, cc, 0, 0, 0, 0]);
        self.fixups.push((at + 2, t));
    }

    /// `movabs {r11,r9}, imm64` (the poll address, a literal operand).
    /// `B8+rd` encodes the register in the *opcode*, which `REX.B`
    /// (not `REX.R`) extends: `r9` is `49 B9`, `r11` is `49 BB`.
    fn movabs(&mut self, r9: bool, v: i64) {
        let mut b = vec![REX_W | REX_B, if r9 { 0xB9 } else { 0xBB }];
        b.extend_from_slice(&v.to_le_bytes());
        self.put(&b);
    }

    /// `cmp byte [base + off], imm8` (`80 /7`, disp32): base 0 = `rdi`
    /// (rm 111, modrm 0xBF), 1 = `rsi` (rm 110, modrm 0xBE). `rm=110`
    /// is `rsi` *without* any REX; `REX.B` would name `r14`.
    fn cmpb(&mut self, base: u8, off: u32, imm: u8) {
        self.put(&[0x80, if base == 1 { 0xBE } else { 0xBF }]);
        self.put(&off.to_le_bytes());
        self.put(&[imm]);
    }

    /// Bytes `cmpb` emits for `base` (the guard-skip maths needs this).
    fn cmpb_len(_base: u8) -> usize {
        7
    }

    /// `mov rax, [base + off + 8]`: the payload of a slot already known
    /// to hold an `Int` (`8B` reg=rax: rm 111/110/010, modrm
    /// 0x87/0x86/0x82 for `rdi`/`rsi`/`rdx`).
    fn load_int(&mut self, base: u8, off: u32) {
        self.put(&[REX_W, 0x8B, Self::rm3(base, 0)]);
        self.put(&off.wrapping_add(PAYLOAD).to_le_bytes());
    }

    /// `mov r9, [base + off]`: the raw payload of a slot already known
    /// to hold a pointer value (an `Obj` handle, tag-checked first), not
    /// an `Int` (`8B` reg=r9, rm = the slot base, no payload offset).
    fn load_r9_at(&mut self, base: u8, off: u32) {
        self.put(&[REX_W | REX_R, 0x8B, Self::rm3(base, 1)]);
        self.put(&off.to_le_bytes());
    }

    /// `r9` = the raw `Rc<Class>` inner pointer of the object in slot
    /// `base + off`, whose tag was just checked `Object`: the handle's
    /// payload (`ObjectData`) and its `class` field in one displacement.
    /// Exactly the read the interpreted method-call path performs at
    /// the same site, so no read can fault on a value that passed the
    /// tag check.
    fn load_obj_class(&mut self, base: u8, off: u32) {
        // The slot payload is the `Obj` handle: the `ObjCell` base.
        self.load_r9_at(base, off.wrapping_add(PAYLOAD));
        self.obj_class_from_r9();
    }

    /// `r9` = the raw `Rc<Class>` inner pointer of the object whose
    /// handle is already in `r9`: one deref, `OBJ_PAYLOAD` (the
    /// `ObjCell` header, shared with `load_obj_class`) plus
    /// `CLASS_FIELD` (the `class` field inside `ObjectData`). The field
    /// holds the `Rc` allocation's inner pointer directly (the same
    /// word `Rc::as_ptr` minus `RC_VALUE_OFFSET` names, which is what
    /// the guard's `movabs` constant is), so no further deref belongs
    /// here: a second one would load the strong count. Shared with the
    /// `$this` guard (`load_r9_frame`).
    fn obj_class_from_r9(&mut self) {
        // `mov r9, [r9 + disp32]` (`4D 8B 89 d32`: `REX.R` extends the
        // `reg` field to `r9`, `REX.B` the `rm` field).
        self.put(&[REX_W | REX_R | REX_B, 0x8B, 0x89]);
        self.put(&OBJ_PAYLOAD.wrapping_add(CLASS_FIELD).to_le_bytes());
    }

    /// `mov r9, [r10 + disp32]` (`4D 8B 8A d32`): a frame field, with
    /// the frame pointer parked in `r10` by the prologue (`Plan.frame`).
    /// The modrm is mod `10` (disp32), `reg = 001` widened to `r9` by
    /// `REX.R` (the destination), `rm = 010` widened to `r10` by
    /// `REX.B` (the base). `rm = 001` (`0x89`/`0x91`) would be `r9`,
    /// and dropping `REX.R` moves the destination to `rdx`.
    fn load_r9_frame(&mut self, off: u32) {
        self.put(&[REX_W | REX_R | REX_B, 0x8B, 0x8A]);
        self.put(&off.to_le_bytes());
    }

    /// `mov rax, [r10 + disp32]` (`41 8B 82 d32`): a frame field, with
    /// the frame pointer parked in `r10` (`Plan.frame`). Unlike
    /// `load_r9_frame` the destination is `rax` (no `REX.R`).
    fn load_rax_frame(&mut self, off: u32) {
        self.put(&[REX_W | REX_B, 0x8B, 0x82]);
        self.put(&off.to_le_bytes());
    }

    /// `mov rax, [r9 + disp32]` (`48 8B 81 d32`): a field of the object
    /// data whose handle is in `r9` (its `class` field, or the
    /// `RefCell<Slots>` array words). `REX.B` extends the `rm` field to
    /// `r9`; without `REX.R` the `reg` field is `rax`.
    fn mov_rax_r9(&mut self, off: u32) {
        self.put(&[REX_W | REX_B, 0x8B, 0x81]);
        self.put(&off.to_le_bytes());
    }

    /// `mov rax, [rax + disp32]` (`48 8B 80 d32`): dereference `rax`
    /// (the slot array pointer) at a byte offset.
    fn mov_rax_rax(&mut self, off: u32) {
        self.put(&[REX_W, 0x8B, 0x80]);
        self.put(&off.to_le_bytes());
    }

    /// `movabs rcx, imm64` (`48 B9`): a second compare constant, so the
    /// `movabs rax` idiom keeps working while `r9` holds a pointer.
    fn movabs_rcx(&mut self, v: i64) {
        self.put(&[REX_W, 0xB9]);
        self.put(&v.to_le_bytes());
    }

    /// `cmp rax, rcx` (`3B /r`: `reg` = `rax` (destination), `rm` =
    /// `rcx` (source), modrm `11 000 001` = `0xC1`, no REX bits needed
    /// beyond `REX.W`).
    fn cmp_rax_rcx(&mut self) {
        self.put(&[REX_W, 0x3B, 0xC1]);
    }

    /// `test rax, rax` (`48 85 C0`): set the zero flag from `rax`.
    fn test_rax(&mut self) {
        self.put(&[REX_W, 0x85, 0xC0]);
    }

    /// `cmp qword [r9 + disp32], imm8` (`49 83 B9 d32 imm8`).
    fn cmpq_r9(&mut self, off: u32, imm: i8) {
        self.put(&[REX_W | REX_B, 0x83, 0xB9]);
        self.put(&off.to_le_bytes());
        self.put(&[imm as u8]);
    }

    /// `cmp rax, imm32` (`48 3D id`, sign-extended): `rax` already holds
    /// an unsigned count (a slot array length).
    fn cmp_rax_imm(&mut self, imm: u32) {
        self.put(&[REX_W, 0x3D]);
        self.put(&imm.to_le_bytes());
    }

    /// `cmp byte [rax + disp32], imm8` (`80 B8 d32 imm8`): the tag byte
    /// of the slot array already dereferenced into `rax`. `rm = 000` is
    /// `rax` without any REX.
    fn cmpb_rax(&mut self, off: u32, imm: u8) {
        self.put(&[0x80, 0xB8]);
        self.put(&off.to_le_bytes());
        self.put(&[imm]);
    }

    /// `cmp rax, r9` (`39 /r`: `reg` = `r9` (source), `rm` = `rax`
    /// (destination), so `REX.R` only; modrm `11 001 000` = `0xC8`).
    fn cmp_rax_r9(&mut self) {
        self.put(&[REX_W | REX_R, 0x39, 0xC8]);
    }

    /// The `mod=10, disp32` ModRM byte for memory base `rdi` (KC),
    /// `rsi` (KT) or `rdx` (KR), with `reg` from the high three bits
    /// (`0` = `rax`, 1 = `r9`): the three fields are `mod`, `reg`, `rm`.
    fn rm3(base: u8, reg: u8) -> u8 {
        let rm: u8 = match base {
            KT => 0b110,
            KR => 0b010,
            _ => 0b111,
        };
        (0b10 << 6) | (reg << 3) | rm
    }

    /// The second operand into `r9`: a slot payload, an inline `KI`
    /// constant, or a resolved `KL` literal.
    fn to_r9(&mut self, kind: u8, off: u32, lit: i64) {
        match kind {
            KC | KT | KR => {
                // `8B` reg=r9 (field 001 + REX.R), rm = the slot base.
                self.put(&[REX_W | REX_R, 0x8B, Self::rm3(kind, 1)]);
                self.put(&off.wrapping_add(PAYLOAD).to_le_bytes());
            }
            KI => self.movabs(true, i64::from(off as i32)),
            _ => self.movabs(true, lit),
        }
    }

    /// `mov [rdi/rsi + off + 8], rax` (tag untouched: the slot was
    /// exact-guarded `Int`, or was just written as one below; the base
    /// is `rsi` for a temporary (`KT`)). `89` reg=rax: modrm 0x87/0x86.
    fn store_int(&mut self, base: u8, off: u32) {
        self.put(&[REX_W, 0x89, Self::rm3(base, 0)]);
        self.put(&off.wrapping_add(PAYLOAD).to_le_bytes());
    }

    /// `mov rcx, [base + off]` (`8B` with the `reg` field `001` = `rcx`,
    /// no `REX.R`): a slot word into the second compare/move register,
    /// so `rax` can keep holding the slot array a property row
    /// dereferences.
    fn movrcx_at(&mut self, base: u8, off: u32) {
        self.put(&[REX_W, 0x8B, Self::rm3(base, 1)]);
        self.put(&off.to_le_bytes());
    }

    /// `mov [base + off + 8], rcx` (`89` with `reg` field `001`): a slot
    /// payload store from `rcx`.
    fn store_rcx_base(&mut self, base: u8, off: u32) {
        self.put(&[REX_W, 0x89, Self::rm3(base, 1)]);
        self.put(&off.wrapping_add(PAYLOAD).to_le_bytes());
    }

    /// `mov [rax + off], rcx` (`48 89 88 d32`): a word of the slot array
    /// already dereferenced into `rax` (the property slot's payload).
    fn store_rcx_at_rax(&mut self, off: u32) {
        self.put(&[REX_W, 0x89, 0x88]);
        self.put(&off.to_le_bytes());
    }

    /// `mov qword [base + off + 8], imm32` (`C7 /0`): a literal payload
    /// store with an explicit base.
    fn store_imm_base(&mut self, base: u8, off: u32, v: i32) {
        self.put(&[REX_W, 0xC7, Self::rm3(base, 0)]);
        self.put(&off.wrapping_add(PAYLOAD).to_le_bytes());
        self.put(&v.to_le_bytes());
    }

    /// `mov byte [rdi/rsi/rdx + off], imm8`: the tag store (always
    /// after the payload store: a slot is only ever read as a value at
    /// a safe point, between compiled calls, where the pair is stable)
    /// with an explicit base (`rsi` for a temporary, `rdx` for a scratch
    /// slot) and value (`C6 /0`: modrm 0x87/0x86/0x82).
    fn store_tag_base(&mut self, base: u8, off: u32, tag: u8) {
        self.put(&[0xC6, Self::rm3(base, 0)]);
        self.put(&off.to_le_bytes());
        self.put(&[tag]);
    }

    /// `mov byte [base + off], cl` (`88 /r` with the `reg` field `001` =
    /// `cl`; no REX, so the bases stay `rdi`/`rsi`/`rdx`): one byte of a
    /// slot, used for a `Bool`'s value at `BOOL_BYTE`.
    fn store_cl_base(&mut self, base: u8, off: u32) {
        self.put(&[0x88, Self::rm3(base, 1)]);
        self.put(&off.to_le_bytes());
    }

    /// `movzx ecx, byte [base + off]` (`0F B6 /r` with the `reg` field
    /// `001` = `ecx`): one byte of a slot, zero-extended.
    fn load_cl_base(&mut self, base: u8, off: u32) {
        self.put(&[0x0F, 0xB6, Self::rm3(base, 1)]);
        self.put(&off.to_le_bytes());
    }
}
impl Emitter {
    /// `mov [base + off + 8], r9` (payload store from `r9`: `89 /r` names
    /// `r9` through the *reg* field, which `REX.R` extends). The base is
    /// always explicit: a store that defaulted to the compiled variables
    /// once put an inlined call's literal result into a variable.
    fn store_int_r9_base(&mut self, base: u8, off: u32) {
        self.put(&[REX_W | REX_R, 0x89, Self::rm3(base, 1)]);
        self.put(&off.wrapping_add(PAYLOAD).to_le_bytes());
    }

    /// `mov rcx, [rax + off]` (`48 8B 88 d32`): a word of the slot
    /// array already dereferenced into `rax`.
    fn movrcx_at_rax(&mut self, off: u32) {
        self.put(&[REX_W, 0x8B, 0x88]);
        self.put(&off.to_le_bytes());
    }

    /// `mov r9, [rax + off]` (`4C 8B 88 d32`: `8B /r` moves `r/m` into
    /// `reg`, so the `reg` field `001` widened by `REX.R` names the
    /// destination `r9`; `rm = 000` is `rax`): a word of the slot array
    /// into the scratch register.
    fn mov_r9_rax(&mut self, off: u32) {
        self.put(&[REX_W | REX_R, 0x8B, 0x88]);
        self.put(&off.to_le_bytes());
    }

    /// `mov [rax + off], r9` (`4C 89 88 d32`: `89 /r` names `r9`
    /// through the *reg* field (`001`), `REX.R` extending it; `rm =
    /// 000` is `rax`): a word of the slot array already dereferenced
    /// into `rax`.
    fn store_r9_at_rax(&mut self, off: u32) {
        self.put(&[REX_W | REX_R, 0x89, 0x88]);
        self.put(&off.to_le_bytes());
    }

    /// `mov r9, rcx` (`4C 8B C9`): keep a value across an operation.
    fn mov_r9_rcx(&mut self) {
        self.put(&[REX_W | REX_R, 0x8B, 0xC9]);
    }

    /// `mov ecx, imm32` (`B9 id`, sign-extended through `rcx`). It does
    /// *not* clobber the flags, so a `setcc` can follow it after a test.
    fn mov_ecx(&mut self, v: u32) {
        self.put(&[0xB9]);
        self.put(&v.to_le_bytes());
    }

    /// `movabs rax, imm64`.
    fn movabs_rax(&mut self, v: i64) {
        self.put(&[REX_W, 0xB8]);
        self.put(&v.to_le_bytes());
    }

    /// `movabs rdx, imm64` (the scratch buffer base).
    fn movabs_rdx(&mut self, v: i64) {
        self.put(&[REX_W, 0xBA]);
        self.put(&v.to_le_bytes());
    }

    /// The first operand into `rax`.
    fn to_rax(&mut self, kind: u8, off: u32, lit: i64) {
        match kind {
            KC | KT | KR => self.load_int(kind, off),
            KI => self.movabs_rax(i64::from(off as i32)),
            _ => self.movabs_rax(lit),
        }
    }

    /// The head-continuation label for instruction `idx` (every
    /// region instruction has one; a covered slot never gets a
    /// reference, so an unused entry is fine).
    fn label(&mut self, idx: usize) {
        self.labels.insert(idx, self.code.len());
    }

    /// A forward label not tied to a region instruction (the
    /// `isset`/`empty` tag dispatch's branch targets): its key is above
    /// every region label's, so it cannot collide, and `resolve` patches
    /// it exactly like a region label.
    fn synth_label(&mut self) -> usize {
        self.synth += 1;
        self.plan.close + self.synth
    }

    /// Register synthetic label `key` at the current code position.
    fn set_label(&mut self, key: usize) {
        self.labels.insert(key, self.code.len());
    }

    /// Where the jump of region instruction `j` goes (the plan
    /// validated the landing).
    fn jump_tgt(&self, j: usize) -> Target {
        let y = jump_target(&self.tape, self.plan.head + j);
        if y == self.plan.head {
            Target::Head
        } else if y <= self.plan.close {
            Target::Label(y)
        } else {
            Target::Stub(y)
        }
    }

    /// Emit the one region instruction starting at region index `j`
    /// (the driver advances past the ops a fused row covers).
    fn emit_instr(&mut self, j: usize) {
        let i = self.plan.head + j;
        let inst = self.tape[i];
        let m = self.plan.meta[j];
        match self.plan.class[j] {
            C_JMP => self.jmp(self.jump_tgt(j)),
            C_PROP => self.emit_prop(m, Target::Side(i)),
            C_PROPW => self.emit_propw(m, Target::Side(i)),
            C_PROPINC => self.emit_propinc(m, Target::Side(i)),
            C_PROPISSET => self.emit_propisset(m, Target::Side(i)),
            C_PROPOPA => self.emit_propopa(m, Target::Side(i)),
            C_SPROP => self.emit_sprop(m, Target::Side(i)),
            C_SPROPW => self.emit_spropw(m, Target::Side(i)),
            C_SPROPISSET => self.emit_spropisset(m, Target::Side(i)),
            C_CONSTI => self.emit_consti(m, Target::Side(i)),
            C_DIMR => self.emit_dimr(m, i),
            C_DIMW => self.emit_dimw(m, i, jit_dim_set as *const () as usize, 0),
            C_DIMOP => self.emit_dimw(m, i, jit_dim_op as *const () as usize, u32::from(m.arith)),
            J_CMP => {
                self.to_rax(m.ka, inst.a, m.lit_a);
                self.to_r9(m.kb, inst.b, m.lit_b);
                self.put(&[REX_W | REX_R, 0x39, 0xC8]); // cmp rax, r9
                let cc = jcc_for(m.mask).unwrap_or(0x85);
                self.jcc(cc, self.jump_tgt(j));
            }
            J_INCCMP => {
                // `b` is read *before* the increment (it may be the
                // same slot as `a`).
                self.to_r9(m.kb, inst.b, m.lit_b);
                self.load_int(KC, inst.a);
                self.put(&[REX_W, 0x83, if m.inc { 0xC0 } else { 0xE8 }, 0x01]);
                self.jcc(0x80, Target::Side(i)); // jo: overflow, before any store
                self.store_int(KC, inst.a);
                self.put(&[REX_W | REX_R, 0x39, 0xC8]); // cmp rax, r9
                let cc = jcc_for(m.mask).unwrap_or(0x85);
                self.jcc(cc, self.jump_tgt(j));
            }
            c => {
                // Every other class exits at the instruction itself
                // (the interpreter re-runs it).
                self.emit_op(c, m, &inst, Target::Side(i));
            }
        }
    }

    /// Emit one whitelisted row (a caller row or an inlined callee
    /// row): operands by the meta's kinds (`KR` addresses the scratch
    /// buffer), and every exit goes to `jo` (for a callee row that is
    /// the call's `InitFCall`: the interpreter re-runs the whole call).
    fn emit_op(&mut self, c: u16, m: JitMeta, inst: &Inst, jo: Target) {
        match c {
            C_INC | C_DEC => {
                self.load_int(m.ka, inst.a);
                self.put(&[REX_W, 0x83, if c == C_INC { 0xC0 } else { 0xE8 }, 0x01]);
                self.jcc(0x80, jo); // jo: overflow, before any store
                self.store_int(m.ka, inst.a);
            }
            C_ASSIGN_I => {
                self.store_imm_base(m.ka, inst.a, inst.b as i32);
                self.store_tag_base(m.ka, inst.a, TAG_INT);
            }
            C_ASSIGN_L => {
                let v = m.lit_b;
                if i64::from(i32::try_from(v).unwrap_or(i32::MAX)) == v {
                    self.store_imm_base(m.ka, inst.a, v as i32);
                } else {
                    self.movabs(true, v);
                    self.store_int_r9_base(m.ka, inst.a);
                }
                self.store_tag_base(m.ka, inst.a, TAG_INT);
            }
            C_NOP => {}
            // `abs(x)`: nothing to do for an unused result (the operand is
            // an int, and `abs` of one has no effect); else negate a
            // negative value, leaving `PHP_INT_MIN` (the one `neg`
            // overflows on) to the interpreted row.
            C_ABS => {
                if inst.c != u32::MAX {
                    self.to_rax(m.ka, inst.a, m.lit_a);
                    self.test_rax();
                    // jns over the `neg` (3 bytes) and its `jo` (6).
                    let skip = self.code.len() + 6 + 3 + 6;
                    self.jcc(0x89, Target::At(skip));
                    self.put(&[REX_W, 0xF7, 0xD8]); // neg rax
                    self.jcc(0x80, jo); // jo
                    self.store_int(KT, inst.c);
                    self.store_tag_base(KT, inst.c, TAG_INT);
                }
            }
            // `$cv = TMP`: copy the int, then consume the temporary
            // (null it, as the interpreted row does).
            J_ASSIGN_T => {
                if m.mask == BOOL_SRC {
                    // A `Bool` from the region's own `isset()`/`empty()`
                    // row: its value is the byte after the tag
                    // (`BOOL_BYTE`), not the payload word, so that byte
                    // is what moves; then the tag.
                    self.load_cl_base(m.kb, inst.b.wrapping_add(BOOL_BYTE));
                    self.store_cl_base(m.ka, inst.a.wrapping_add(BOOL_BYTE));
                    self.store_tag_base(m.ka, inst.a, TAG_BOOL);
                } else if m.mask == NULL_SRC {
                    // The `Null` of an inlined call that returned
                    // nothing: a zero payload and the tag.
                    self.store_imm_base(m.ka, inst.a, 0);
                    self.store_tag_base(m.ka, inst.a, TAG_NULL);
                } else {
                    // The int path the plan checked.
                    self.load_int(m.kb, inst.b);
                    self.store_int(m.ka, inst.a);
                    self.store_tag_base(m.ka, inst.a, TAG_INT);
                }
                self.movabs_rax(0);
                self.store_int(m.kb, inst.b);
                self.store_tag_base(m.kb, inst.b, TAG_NULL);
            }
            _ => {
                // `J_ARITH_CV` / `J_ASSIGN_OP` / `J_ARITH_TMP` /
                // `J_NUMBIN_T` / `J_NUMBIN_C` (the plan guarantees):
                // `jo` (or the `idiv` guards) before any store. The
                // destination kind is in the meta; the destination is
                // `inst.c` except for `$cv op= b`, which updates
                // `inst.a`.
                let dst = if c == J_ASSIGN_OP || c == J_NUMBIN_C {
                    inst.a
                } else if c == J_NUMBIN_D {
                    inst.d
                } else {
                    inst.c
                };
                self.to_rax(m.ka, inst.a, m.lit_a);
                self.to_r9(m.kb, inst.b, m.lit_b);
                match m.arith {
                    0 => self.put(&[REX_W | REX_R, 0x01, 0xC8]), // add rax, r9
                    1 => self.put(&[REX_W | REX_R, 0x29, 0xC8]), // sub rax, r9
                    // `imul r, r/m`: reg = `rax`, rm = `r9` (needs REX.B,
                    // not REX.R: the *rm* field names `r9`).
                    2 => self.put(&[REX_W | REX_B, 0x0F, 0xAF, 0xC1]), // imul rax, r9
                    OPA_BITAND => self.put(&[REX_W | REX_R, 0x21, 0xC8]), // and rax, r9
                    OPA_BITOR => self.put(&[REX_W | REX_R, 0x09, 0xC8]), // or rax, r9
                    OPA_BITXOR => self.put(&[REX_W | REX_R, 0x31, 0xC8]), // xor rax, r9
                    ARITH_SHL | ARITH_SAR => {
                        // A count outside `0..=63` is the interpreted
                        // row's: it declines as well, and the generic op
                        // gives the 0 or -1 of a too wide shift or throws
                        // for a negative one. One unsigned compare
                        // catches both (a negative count is huge).
                        self.put(&[REX_W | REX_B, 0x83, 0xF9, 0x3F]); // cmp r9, 63
                        self.jcc(0x87, jo); // ja
                        self.put(&[REX_W | REX_R, 0x89, 0xC9]); // mov rcx, r9
                                                                // `shl rax, cl` / `sar rax, cl` (`D3 /4`, `/7`):
                                                                // bits shifted out are dropped, as `checked_shl`
                                                                // drops them, and `>>` keeps the sign.
                        let modrm = if m.arith == ARITH_SHL { 0xE0 } else { 0xF8 };
                        self.put(&[REX_W, 0xD3, modrm]);
                    }
                    _ => {
                        // `%`: `idiv` faults on a zero divisor and on
                        // `i64::MIN % -1`, so both are guarded exits.
                        // (`rdx` is the `idiv`'s payload half: a region
                        // with scratch calls has no `%`, see `plan`.)
                        self.put(&[REX_W | REX_R | REX_B, 0x85, 0xC9]); // test r9, r9
                        self.jcc(0x84, jo); // je
                        self.put(&[REX_W | REX_B, 0x83, 0xF9, 0xFF]); // cmp r9, -1
                        self.jcc(0x84, jo); // je
                        self.put(&[REX_W, 0x99]); // cqo
                        self.put(&[REX_W | REX_B, 0xF7, 0xF9]); // idiv r9
                                                                // `idiv` leaves the remainder in `rdx`.
                        self.put(&[REX_W, 0x89, 0xD0]); // mov rax, rdx
                    }
                }
                // Only `+`, `-` and `*` can overflow: the bitwise ops clear
                // the flag, a shift leaves it undefined, and `%` was
                // guarded above.
                if m.arith <= 2 {
                    self.jcc(0x80, jo); // jo: overflow
                }
                self.store_int(m.kc, dst);
                if m.kc == KT {
                    self.store_tag_base(KT, dst, TAG_INT);
                }
            }
        }
    }

    /// Emit an inlined call: the arguments into the callee's parameter
    /// slots (scratch offsets `base + 0, 16, ...`), the callee's rows
    /// (already based at `cp.base`), then the result into the caller's
    /// `DoCall` temporary. Callee slots never need a tag check: the
    /// plan proved every read one is a known `Int`.
    fn emit_call(&mut self, cp: &CallPlan) {
        // An instance call first re-checks its receiver: the slot must
        // still hold an object of exactly the class the plan resolved
        // from the site's monomorphic cache. A mismatch (or a
        // non-object) resumes at the call's init row *without*
        // disarming: the receiver class may alternate between
        // iterations, and the interpreted path re-runs the whole raw
        // call either way. The payload derefs mirror that path, so
        // they cannot fault once the tag matches.
        if let Some((rk, roff, cls_addr)) = cp.recv {
            if debug_on() {
                eprintln!("jit:   recv kind {rk} off {roff} class {cls_addr:#x}");
            }
            if rk == KTHIS {
                // `$this`: the frame field is an `Option<Obj>` with no
                // tag byte to check; `None` is a null pointer (the
                // `NonNull` niche).
                self.load_r9_frame(THIS_FIELD);
                self.put(&[REX_W | REX_R | REX_B, 0x85, 0xC9]); // test r9, r9
                self.jcc(0x84, Target::Side(cp.callove)); // je: no `$this`
                self.obj_class_from_r9();
            } else {
                self.cmpb(rk, roff, TAG_OBJECT);
                self.jcc(0x85, Target::Side(cp.callove)); // jne: not an object
                self.load_obj_class(rk, roff);
            }
            // `r9` holds the raw `Rc<Class>` field word: the inner
            // box's base, `RC_VALUE_OFFSET` below the `Class` value the
            // cache names.
            self.movabs_rax(cls_addr.wrapping_sub(u64::from(RC_VALUE_OFFSET)) as i64);
            self.cmp_rax_r9();
            self.jcc(0x85, Target::Side(cp.callove)); // jne: other class
                                                      // The calling scope the cache entry was filled for, as
                                                      // `emit_prop_slots` checks it: the method this site
                                                      // resolved is only this scope's answer.
            self.load_rax_frame(SCOPE_FIELD);
            if cp.recv_scope == 0 {
                self.test_rax();
            } else {
                self.movabs_rcx(cp.recv_scope);
                self.cmp_rax_rcx();
            }
            self.jcc(0x85, Target::Side(cp.callove)); // jne: other scope
        }
        for (n, arg) in cp.sends.iter().enumerate() {
            if arg.kind == KSKIP {
                continue;
            }
            let off = cp.base.wrapping_add((n as u32) << SLOT_SHIFT);
            self.to_r9(arg.kind, arg.off, arg.lit);
            self.store_int_r9_base(KR, off);
            self.store_tag_base(KR, off, TAG_INT);
        }
        for k in 0..cp.classes.len() {
            self.emit_op(
                cp.classes[k],
                cp.metas[k],
                &cp.rows[k],
                Target::Disarm(cp.callove),
            );
        }
        if let Some(d) = cp.dst {
            match cp.ret.kind {
                // A bare `return;`: the interpreted `DoCall` leaves a
                // null result.
                KNULL_ARG => {
                    self.movabs_rax(0);
                    self.store_int(KT, d);
                    self.store_tag_base(KT, d, TAG_NULL);
                }
                // An `int` return of a callee slot the plan proved is
                // an `Int`: copy it.
                KR => {
                    self.to_r9(KR, cp.ret.off, 0);
                    self.store_int_r9_base(KT, d);
                    self.store_tag_base(KT, d, TAG_INT);
                }
                // An `int` literal return: store it directly.
                // (`d` is a temporary: the stores name `KT`. Stores
                // defaulting to the compiled-variable base would put the
                // payload into the variable at the same offset.)
                KL => {
                    let v = cp.ret.lit;
                    match i32::try_from(v) {
                        Ok(v32) => self.store_imm_base(KT, d, v32),
                        Err(_) => {
                            self.movabs(true, v);
                            self.store_int_r9_base(KT, d);
                        }
                    }
                    self.store_tag_base(KT, d, TAG_INT);
                }
                _ => {}
            }
        }
    }

    /// Emit one property read (`C_PROP`): [`emit_prop_slots`] and
    /// [`emit_prop_int_slot`] then copy the slot's payload into the
    /// destination temporary with an `Int` tag.
    fn emit_prop(&mut self, m: JitMeta, jo: Target) {
        self.emit_prop_slots(m, jo);
        self.emit_prop_int_slot(m, jo);
        // `rax` is the slot array and the slot is an `Int`: load its
        // payload.
        let so = m.slot << SLOT_SHIFT;
        self.mov_rax_rax(so.wrapping_add(PAYLOAD));
        // Fully store the destination temporary (tag and payload).
        self.store_int(KT, m.db);
        self.store_tag_base(KT, m.db, TAG_INT);
    }

    /// The checks one property access (read or write) re-runs every
    /// iteration: the base (an object of the pinned class, read through
    /// the frame for `$this`), the frame's calling scope, an unborrowed
    /// and empty `lazy` cell, and an unborrowed `slots` cell long enough
    /// for the slot. On fall-through `rax` is the slot array's base, so
    /// the caller reads or writes `rax + (slot << SLOT_SHIFT) +
    /// PAYLOAD` (see [`emit_prop_int_slot`] for the value check).
    ///
    /// Every mismatch exits to the interpreted row *without* disarming:
    /// classes, scopes and dynamic properties legitimately alternate
    /// between iterations, and the interpreted lookup is always correct.
    /// The derefs mirror the interpreted path, so none can fault once
    /// its checks pass.
    fn emit_prop_slots(&mut self, m: JitMeta, jo: Target) {
        if debug_on() {
            eprintln!(
                "jit:   prop base {} off {:#x} class {:#x} scope {:#x} slot {}",
                m.ka, m.sb, m.lit_a, m.lit_b, m.slot
            );
        }
        // The receiver handle in `r9`.
        if m.ka == KTHIS {
            // `$this`: an `Option<Obj>` frame field, `None` a null
            // pointer (no tag byte to check).
            self.load_r9_frame(THIS_FIELD);
            self.put(&[REX_W | REX_R | REX_B, 0x85, 0xC9]); // test r9, r9
            self.jcc(0x84, jo); // je: no `$this`
        } else {
            self.cmpb(KC, m.sb, TAG_OBJECT);
            self.jcc(0x85, jo); // jne: not an object
                                // The slot payload is the `Obj` handle (the `ObjCell` base).
            self.load_r9_at(KC, m.sb.wrapping_add(PAYLOAD));
        }
        // The frame's calling scope must be the one the cache entry was
        // filled for (`0` = none): a `use`d or different scope changes
        // what a protected property lookup resolves to.
        self.load_rax_frame(SCOPE_FIELD);
        if m.lit_b == 0 {
            self.test_rax();
        } else {
            self.movabs_rcx(m.lit_b);
            self.cmp_rax_rcx();
        }
        self.jcc(0x85, jo); // jne: other scope
                            // The receiver's class field must name the pinned class. The
                            // field word is the raw `Rc` inner pointer, which is what the
                            // plan-time `lit_a` already names (`class_addr` minus
                            // `RC_VALUE_OFFSET`), so no deref beyond the field.
        self.mov_rax_r9(OBJ_PAYLOAD.wrapping_add(CLASS_FIELD));
        self.movabs_rcx(m.lit_a);
        self.cmp_rax_rcx();
        self.jcc(0x85, jo); // jne: other class
                            // The `lazy` cell must be unborrowed and empty: the interpreted
                            // read consults it first, and anything in it (or a live borrow)
                            // takes the slow path. `try_borrow` only fails on a nonzero
                            // borrow state, so comparing against 0 is stricter than needed,
                            // never looser.
        self.cmpq_r9(OBJ_LAZY.wrapping_add(REFCELL_BORROW), 0);
        self.jcc(0x85, jo); // jne: borrowed
        self.cmpq_r9(OBJ_LAZY.wrapping_add(REFCELL_VALUE), 0);
        self.jcc(0x85, jo); // jne: lazy properties present
                            // The `slots` cell must be unborrowed (a live borrow could make
                            // this read alias one that panics) and long enough for the slot
                            // (a slot count shrunk by... never happens, but an entry filled
                            // for another class's layout is the real case this catches).
        self.cmpq_r9(SLOTS_BORROW, 0);
        self.jcc(0x85, jo); // jne: borrowed
        self.mov_rax_r9(SLOTS_LEN);
        self.cmp_rax_imm(m.slot.wrapping_add(1));
        self.jcc(0x82, jo); // jb: `slot` past the array
                            // `rax` is the slot array's base for the caller.
        self.mov_rax_r9(SLOTS_FIRST);
    }

    /// The `Int`-slot check that ends a plain read or write's guards:
    /// [`emit_prop_slots`] must have just left `rax` at the slot array.
    /// The slot itself: `Option<Value>`'s niche gives an unset slot
    /// tag `TAG_NONE` and a real value its own tag, so one `Int`
    /// check rejects unset slots, references and strings together.
    /// For a write the same check carries the whole heap story: the
    /// old value is a scalar (nothing to release, and an `Int` is
    /// neither shared nor aliased through a `Reference`, so the
    /// payload store cannot split an alias), and its tag equals the
    /// new value's, so the tag byte never needs writing.
    fn emit_prop_int_slot(&mut self, m: JitMeta, jo: Target) {
        self.cmpb_rax(m.slot << SLOT_SHIFT, TAG_INT);
        self.jcc(0x85, jo); // jne: not an int (or nothing)
    }

    /// Emit one property write (`C_PROPW`): [`emit_prop_slots`] and
    /// [`emit_prop_int_slot`] then
    /// store the written value's payload into the slot, with the value
    /// also copied to the result temporary when the expression's value
    /// is used (`assign_hit` writes `dst` with the *new* value), and the
    /// source temporary nulled out when the op's operand was a consumed
    /// `Tmp`. Every exit goes to the interpreted row: it re-runs the
    /// whole write, which is what its own checks decide.
    fn emit_propw(&mut self, m: JitMeta, jo: Target) {
        self.emit_prop_slots(m, jo);
        self.emit_prop_int_slot(m, jo);
        let so = m.slot << SLOT_SHIFT;
        // The written value in `rcx`: a `KT` source is re-checked per
        // iteration (its entry value may be anything, including the
        // `TAG_NULL` an earlier row's consume left, and a temporary the
        // region itself produced passes on its fresh value); a `KC`
        // source carries an exact entry guard like an arithmetic
        // operand; a `KL` source is a plan-time constant.
        match m.sc {
            KT => {
                self.cmpb(KT, m.sw, TAG_INT);
                self.jcc(0x85, jo); // jne: not an int
                self.movrcx_at(KT, m.sw.wrapping_add(PAYLOAD));
            }
            KC => self.movrcx_at(KC, m.sw.wrapping_add(PAYLOAD)),
            _ => self.movabs_rcx(m.lit_c),
        }
        // The result temporary: fully stored (tag and payload), as the
        // read's destination is.
        if m.db != NO_DST {
            self.store_rcx_base(KT, m.db);
            self.store_tag_base(KT, m.db, TAG_INT);
        }
        // The slot: tag already `Int` (the guard), so one payload word.
        self.store_rcx_at_rax(so.wrapping_add(PAYLOAD));
        // A consumed source temporary is left null, as the interpreted
        // row does (`put_tmp(slot, Value::Null)`).
        if m.consume {
            self.store_imm_base(KT, m.sw, 0);
            self.store_tag_base(KT, m.sw, TAG_NULL);
        }
    }

    /// Emit one property `++`/`--` (`C_PROPINC`): [`emit_prop_slots`]
    /// and [`emit_prop_int_slot`], then the increment of the slot's
    /// payload in place. `inc` is the direction and `kc` the post-ness
    /// (the `IncDecKind` packed by `plan_prop_incdec`). The `add`/`sub`
    /// exits `jo` *before* any store on int overflow, exactly like the
    /// fused `for` rows: the interpreted row then re-runs the whole op
    /// and decides the `Float` conversion by its own `accept` check.
    /// The slot keeps its `Int` tag, so only the payload word is
    /// written; the result temporary (when the expression's value is
    /// used) gets the *old* value for a post op (parked in `r9`) or the
    /// new one (in `rcx`) for a pre op.
    fn emit_propinc(&mut self, m: JitMeta, jo: Target) {
        self.emit_prop_slots(m, jo);
        self.emit_prop_int_slot(m, jo);
        let so = m.slot << SLOT_SHIFT;
        // The current payload in `rcx`; a post op keeps it in `r9` as
        // the expression's value before changing it.
        self.movrcx_at_rax(so.wrapping_add(PAYLOAD));
        if m.kc != 0 {
            self.mov_r9_rcx();
        }
        // `add rcx, 1` / `sub rcx, 1` (`83 /0` and `/5`).
        self.put(&[REX_W, 0x83, if m.inc { 0xC1 } else { 0xE9 }, 0x01]);
        self.jcc(0x80, jo); // jo: overflow, before any store
                            // The slot takes the new value (tag still `Int`, the guard).
        self.store_rcx_at_rax(so.wrapping_add(PAYLOAD));
        // The result temporary: fully stored (tag and payload).
        if m.db != NO_DST {
            if m.kc != 0 {
                self.store_int_r9_base(KT, m.db);
            } else {
                self.store_rcx_base(KT, m.db);
            }
            self.store_tag_base(KT, m.db, TAG_INT);
        }
    }

    /// Emit one `isset()`/`empty()` on a property (`C_PROPISSET`):
    /// [`emit_prop_slots`] (no `Int` slot check — the slot's *tag* is
    /// what answers), then a branch on the tag byte: `Int`, `Null` and
    /// `Bool` slots are answered inline, anything else (unset, string,
    /// object, reference) exits to the interpreted row, which also
    /// carries the dynamic-property cases the cache entry excludes.
    /// `inc` carries `empty`. The destination temporary gets a `Bool`:
    /// its value byte (`BOOL_BYTE`) and the tag (never an `Int` def: the
    /// plan lets only a copy into a variable read it).
    fn emit_propisset(&mut self, m: JitMeta, jo: Target) {
        self.emit_prop_slots(m, jo);
        let so = m.slot << SLOT_SHIFT;
        // The tag byte of the slot, three forward branches.
        self.cmpb_rax(so, TAG_INT);
        let l_int = self.synth_label();
        self.jcc(0x84, Target::Label(l_int)); // je
        self.cmpb_rax(so, TAG_NULL);
        let l_null = self.synth_label();
        self.jcc(0x84, Target::Label(l_null)); // je
        self.cmpb_rax(so, TAG_BOOL);
        let l_bool = self.synth_label();
        self.jcc(0x84, Target::Label(l_bool)); // je
        self.jmp(jo); // any other tag: the interpreted row decides
                      // `Int`: set, and empty only when the payload is 0.
        self.set_label(l_int);
        if m.inc {
            self.movrcx_at_rax(so.wrapping_add(PAYLOAD));
            self.put(&[REX_W, 0x85, 0xC9]); // test rcx, rcx
            self.mov_ecx(0);
            self.put(&[0x0F, 0x94, 0xC1]); // sete cl
        } else {
            self.mov_ecx(1);
        }
        let l_end = self.synth_label();
        self.jmp(Target::Label(l_end));
        // `Null`: not set, and empty.
        self.set_label(l_null);
        self.mov_ecx(u32::from(m.inc));
        self.jmp(Target::Label(l_end));
        // `Bool`: set; empty only when its value byte is 0.
        self.set_label(l_bool);
        if m.inc {
            // The `Bool`'s value byte (`BOOL_BYTE`, right after the tag;
            // the payload word of a `Bool` slot is leftover bytes).
            self.cmpb_rax(so.wrapping_add(BOOL_BYTE), 0);
            self.mov_ecx(0);
            self.put(&[0x0F, 0x94, 0xC1]); // sete cl
        } else {
            self.mov_ecx(1);
        }
        // Store the clean `Bool` into the result temporary: its value
        // byte, then the tag.
        self.set_label(l_end);
        self.store_cl_base(KT, m.db.wrapping_add(BOOL_BYTE));
        self.store_tag_base(KT, m.db, TAG_BOOL);
    }

    /// Emit one property compound write (`C_PROPOPA`): the guards of a
    /// plain write, then the second operand in `rcx` (re-checked per
    /// iteration for a `KT` source, exact-guarded for `KC`, constant
    /// for `KL` — exactly `emit_propw`'s rules), the old payload in
    /// `r9`, and the arithmetic into `r9`. `+`/`-`/`*` exit `jo` before
    /// any store on int overflow (the interpreted row then converts to
    /// `Float` where the property's `accept` allows, as the JIT's plan
    /// does not pin `accept`); the bitwise ops always succeed on two
    /// ints (`assign_op_in` agrees) and need no check. The slot keeps
    /// its `Int` tag; the result temporary (when used) gets the new
    /// value, and a consumed `Tmp` source is nulled like a write's.
    fn emit_propopa(&mut self, m: JitMeta, jo: Target) {
        self.emit_prop_slots(m, jo);
        self.emit_prop_int_slot(m, jo);
        let so = m.slot << SLOT_SHIFT;
        // The second operand in `rcx`.
        match m.sc {
            KT => {
                self.cmpb(KT, m.sw, TAG_INT);
                self.jcc(0x85, jo); // jne: not an int
                self.movrcx_at(KT, m.sw.wrapping_add(PAYLOAD));
            }
            KC => self.movrcx_at(KC, m.sw.wrapping_add(PAYLOAD)),
            _ => self.movabs_rcx(m.lit_c),
        }
        // The arithmetic into `r9`: `01`/`29` `add`/`sub` r/m64, r64
        // with `rm = 001` widened to `r9` by `REX.B` and `reg = 001`
        // the source `rcx`; `imul r9, rcx` is the `0F AF /r` form with
        // `reg = r9` (`REX.R`). Overflow of `+`/`-`/`*` (the `OF` flag)
        // exits before the store below; `and`/`or`/`xor` clear `OF`.
        self.mov_r9_rax(so.wrapping_add(PAYLOAD));
        let checked = m.arith <= 2;
        match m.arith {
            0 => self.put(&[REX_W | REX_B, 0x01, 0xC9]), // add r9, rcx
            1 => self.put(&[REX_W | REX_B, 0x29, 0xC9]), // sub r9, rcx
            2 => self.put(&[REX_W | REX_R, 0x0F, 0xAF, 0xC9]), // imul r9, rcx
            OPA_BITAND => self.put(&[REX_W | REX_B, 0x21, 0xC9]), // and r9, rcx
            OPA_BITOR => self.put(&[REX_W | REX_B, 0x09, 0xC9]), // or r9, rcx
            _ => self.put(&[REX_W | REX_B, 0x31, 0xC9]), // xor r9, rcx
        }
        if checked {
            self.jcc(0x80, jo); // jo: overflow, before any store
        }
        // The slot takes the new value (tag still `Int`, the guard).
        self.store_r9_at_rax(so.wrapping_add(PAYLOAD));
        // The result temporary: fully stored (tag and payload).
        if m.db != NO_DST {
            self.store_int_r9_base(KT, m.db);
            self.store_tag_base(KT, m.db, TAG_INT);
        }
        // A consumed source temporary is left null, as for a write.
        if m.consume {
            self.store_imm_base(KT, m.sw, 0);
            self.store_tag_base(KT, m.sw, TAG_NULL);
        }
    }

    /// The scope/run checks a static-property or constant row re-runs
    /// every iteration: the frame's calling scope must still be the one
    /// the pinned run-time-cache entry was filled for (`lit_b`, `0` =
    /// no scope, so the guard checks the field is null), and for a
    /// `static::` place the running function's class must still be the
    /// pinned one (`wc`). Both mirror the entry's own re-check in the
    /// interpreted row (`rtcache::static_prop_entry_in`,
    /// `class_const_hit_in`): any other scope re-resolves the name, so
    /// the row exits to the interpreter, which always re-checks.
    fn emit_sprop_guards(&mut self, m: JitMeta, jo: Target) {
        if debug_on() {
            eprintln!(
                "jit:   sprop cell {:#x} scope {:#x} called {:#x}",
                m.lit_a as *const u8 as usize,
                m.lit_b as *const u8 as usize,
                m.wc as *const u8 as usize,
            );
        }
        if m.sg {
            self.load_rax_frame(SCOPE_FIELD);
            if m.lit_b == 0 {
                self.test_rax();
            } else {
                self.movabs_rcx(m.lit_b);
                self.cmp_rax_rcx();
            }
            self.jcc(0x85, jo); // jne: other scope
        }
        if m.cg {
            self.load_rax_frame(CALLED_FIELD);
            if m.wc == 0 {
                self.test_rax();
            } else {
                self.movabs_rcx(m.wc);
                self.cmp_rax_rcx();
            }
            self.jcc(0x85, jo); // jne: other running class
        }
    }

    /// The cell checks shared by the three static-property rows: the
    /// pinned `Rc<RefCell<Value>>` cell (its absolute address baked in
    /// at plan time, kept alive by `PINNED_CELLS`) must be unborrowed —
    /// a live borrow means the interpreted path holds the value and an
    /// inline read could see a torn value or a live `&mut` — then the
    /// caller checks the `Value`'s tag at `REFCELL_VALUE`. `try_borrow`
    /// only fails on a nonzero borrow word, so comparing against 0 is
    /// exact. On fall-through `rax` is the cell base.
    fn emit_sprop_cell(&mut self, m: JitMeta, jo: Target) {
        self.movabs_rax(m.lit_a);
        self.put(&[REX_W, 0x83, 0x38, 0x00]); // cmp qword ptr [rax], 0
        self.jcc(0x85, jo); // jne: borrowed
    }

    /// Emit one static-property read (`C_SPROP`): [`emit_sprop_guards`]
    /// and [`emit_sprop_cell`], then the cell's tag must be `Int`
    /// (`static_cell_value` answers an `Int` cell directly; a `Str`,
    /// array, reference or `Null` cell — including one that was never
    /// initialised — exits to the interpreted row, which decides
    /// warnings, defaults and the typed-property checks the plan does
    /// not pin). Load the payload and fully store the destination
    /// temporary, exactly as [`emit_prop`] ends.
    fn emit_sprop(&mut self, m: JitMeta, jo: Target) {
        self.emit_sprop_guards(m, jo);
        self.emit_sprop_cell(m, jo);
        self.cmpb_rax(REFCELL_VALUE, TAG_INT);
        self.jcc(0x85, jo); // jne: not an int (or not initialised)
        self.mov_rax_rax(REFCELL_VALUE.wrapping_add(PAYLOAD));
        self.store_int(KT, m.db);
        self.store_tag_base(KT, m.db, TAG_INT);
    }

    /// Emit one static-property write (`C_SPROPW`): the guards, the
    /// cell must hold an `Int` (the plan required the entry's
    /// `fast_plain` flag: no type declaration to coerce or reject, and
    /// an `Int` cell is neither shared nor aliased through a
    /// `Reference`, so the payload store cannot split an alias — and
    /// the tag byte never changes), then the value in `rcx` with
    /// `emit_propw`'s source rules, the result temporary's full store,
    /// the cell's payload word, and the consumed temporary's nulling.
    fn emit_spropw(&mut self, m: JitMeta, jo: Target) {
        self.emit_sprop_guards(m, jo);
        self.emit_sprop_cell(m, jo);
        self.cmpb_rax(REFCELL_VALUE, TAG_INT);
        self.jcc(0x85, jo); // jne: not an int
                            // The written value in `rcx` — the same re-check rules as an
                            // instance property write.
        match m.sc {
            KT => {
                self.cmpb(KT, m.sw, TAG_INT);
                self.jcc(0x85, jo); // jne: not an int
                self.movrcx_at(KT, m.sw.wrapping_add(PAYLOAD));
            }
            KC => self.movrcx_at(KC, m.sw.wrapping_add(PAYLOAD)),
            _ => self.movabs_rcx(m.lit_c),
        }
        if m.db != NO_DST {
            self.store_rcx_base(KT, m.db);
            self.store_tag_base(KT, m.db, TAG_INT);
        }
        // The cell: tag already `Int`, so one payload word.
        self.store_rcx_at_rax(REFCELL_VALUE.wrapping_add(PAYLOAD));
        if m.consume {
            self.store_imm_base(KT, m.sw, 0);
            self.store_tag_base(KT, m.sw, TAG_NULL);
        }
    }

    /// Emit one `isset()`/`empty()` on a static property
    /// (`C_SPROPISSET`): the guards and the cell, then a branch on the
    /// cell's tag exactly like [`emit_propisset`] — `Int`, `Null` and
    /// `Bool` answered inline (`static_cell_isset`'s three cases),
    /// anything else the interpreted row. The destination temporary
    /// gets a `Bool`: its value byte (`BOOL_BYTE`) and the tag.
    fn emit_spropisset(&mut self, m: JitMeta, jo: Target) {
        self.emit_sprop_guards(m, jo);
        self.emit_sprop_cell(m, jo);
        // The tag byte of the cell's `Value`, three forward branches.
        self.cmpb_rax(REFCELL_VALUE, TAG_INT);
        let l_int = self.synth_label();
        self.jcc(0x84, Target::Label(l_int)); // je
        self.cmpb_rax(REFCELL_VALUE, TAG_NULL);
        let l_null = self.synth_label();
        self.jcc(0x84, Target::Label(l_null)); // je
        self.cmpb_rax(REFCELL_VALUE, TAG_BOOL);
        let l_bool = self.synth_label();
        self.jcc(0x84, Target::Label(l_bool)); // je
        self.jmp(jo); // any other tag: the interpreted row decides
                      // `Int`: set, and empty only when the payload is 0.
        self.set_label(l_int);
        if m.inc {
            self.movrcx_at_rax(REFCELL_VALUE.wrapping_add(PAYLOAD));
            self.put(&[REX_W, 0x85, 0xC9]); // test rcx, rcx
            self.mov_ecx(0);
            self.put(&[0x0F, 0x94, 0xC1]); // sete cl
        } else {
            self.mov_ecx(1);
        }
        let l_end = self.synth_label();
        self.jmp(Target::Label(l_end));
        // `Null`: not set, and empty.
        self.set_label(l_null);
        self.mov_ecx(u32::from(m.inc));
        self.jmp(Target::Label(l_end));
        // `Bool`: set; empty only when its value byte is 0.
        self.set_label(l_bool);
        if m.inc {
            // The `Bool`'s value byte, as in `emit_propisset`.
            self.cmpb_rax(REFCELL_VALUE.wrapping_add(BOOL_BYTE), 0);
            self.mov_ecx(0);
            self.put(&[0x0F, 0x94, 0xC1]); // sete cl
        } else {
            self.mov_ecx(1);
        }
        // Store the clean `Bool` into the result temporary: its value
        // byte, then the tag.
        self.set_label(l_end);
        self.store_cl_base(KT, m.db.wrapping_add(BOOL_BYTE));
        self.store_tag_base(KT, m.db, TAG_BOOL);
    }

    /// Emit a plan-time-folded `Int` constant (`C_CONSTI`): the
    /// guards the name's resolution needs (a scope word for `self::`,
    /// `parent::`, an access-checked named class constant or a plain
    /// named constant, the run class too for `static::`; the prologue's
    /// version guard covers redefinition for every row of the trace),
    /// then the folded value straight into the destination temporary.
    /// The constant was resolved from this op array's own run-time cache
    /// at plan time, so there is no memory to re-read per iteration.
    fn emit_consti(&mut self, m: JitMeta, jo: Target) {
        self.emit_sprop_guards(m, jo);
        match m.arith {
            // `null`: a zero payload and the tag.
            CONST_NULL => {
                self.store_imm_base(KT, m.db, 0);
                self.store_tag_base(KT, m.db, TAG_NULL);
            }
            // `true`/`false`: the value byte (`BOOL_BYTE`), then the tag.
            CONST_BOOL => {
                self.mov_ecx(u32::from(m.lit_a != 0));
                self.store_cl_base(KT, m.db.wrapping_add(BOOL_BYTE));
                self.store_tag_base(KT, m.db, TAG_BOOL);
            }
            _ => {
                match i32::try_from(m.lit_a) {
                    Ok(v) => self.store_imm_base(KT, m.db, v),
                    Err(_) => {
                        self.movabs(true, m.lit_a);
                        self.store_int_r9_base(KT, m.db);
                    }
                }
                self.store_tag_base(KT, m.db, TAG_INT);
            }
        }
    }
}
impl Emitter {
    /// `lea {rax, rcx, rdx}, [base + off]` (`8D /r` with the `reg` field
    /// 0/1/2): the address of a frame slot, for a helper's argument.
    fn lea_slot(&mut self, reg: u8, base: u8, off: u32) {
        self.put(&[REX_W, 0x8D, Self::rm3(base, reg)]);
        self.put(&off.to_le_bytes());
    }

    /// One helper argument into `rax` (0), `rcx` (1) or `rdx` (2).
    fn helper_arg(&mut self, reg: u8, arg: HArg) {
        match arg {
            HArg::Slot(base, off) => self.lea_slot(reg, base, off),
            HArg::Abs(v) => match reg {
                0 => self.movabs_rax(v),
                1 => self.movabs_rcx(v),
                _ => self.movabs_rdx(v),
            },
            // `xor e?x, e?x` (`31 /r`, the register in both fields).
            HArg::Null => self.put(&[0x31, 0xC0 | (reg << 3) | reg]),
        }
    }

    /// Call the helper at `helper` as `extern "C" fn(args[0], args[1],
    /// args[2], word) -> u32` and leave the row through `fail` when it
    /// returns non-zero (it declined, having changed nothing).
    ///
    /// Every register the trace keeps across rows is caller-saved in the
    /// C ABI, so the call saves them around itself: `rdi`/`rsi` (the slot
    /// bases), `r8` (the iteration count), `r10` (the frame) and `r11`
    /// (the poll address). The five pushes also bring the stack to the
    /// 16-byte boundary a call needs: the trace was entered by a `call`
    /// and pushes nothing else, so it runs 8 bytes off that boundary.
    /// `rdx` (the scratch base of a region with inlined calls) is a
    /// constant and is loaded again. The arguments are computed into
    /// `rax`/`rcx`/`rdx` first, off the slot bases, and only then moved
    /// into the argument registers that hold those bases.
    fn emit_helper(&mut self, helper: usize, args: [HArg; 3], word: u32, fail: Target) {
        // push rdi; push rsi; push r8; push r10; push r11
        self.put(&[0x57, 0x56, 0x41, 0x50, 0x41, 0x52, 0x41, 0x53]);
        self.helper_arg(0, args[0]);
        self.helper_arg(1, args[1]);
        self.helper_arg(2, args[2]);
        self.put(&[REX_W, 0x89, 0xC7]); // mov rdi, rax
        self.put(&[REX_W, 0x89, 0xCE]); // mov rsi, rcx
        self.mov_ecx(word); // the fourth argument
        self.movabs_rax(helper as i64);
        self.put(&[0xFF, 0xD0]); // call rax
                                 // pop r11; pop r10; pop r8; pop rsi; pop rdi
        self.put(&[0x41, 0x5B, 0x41, 0x5A, 0x41, 0x58, 0x5E, 0x5F]);
        if self.plan.has_calls {
            self.movabs_rdx(self.scratch);
        }
        self.put(&[0x85, 0xC0]); // test eax, eax
        self.jcc(0x85, fail); // jnz: the helper declined
    }

    /// The offset operand of an array row as a helper argument: a slot's
    /// address, an int literal's, or null for an append.
    fn dim_arg(m: JitMeta) -> HArg {
        match m.kb {
            KSKIP => HArg::Null,
            KL => HArg::Abs(m.lit_b),
            k => HArg::Slot(k, m.slot),
        }
    }

    /// Emit one array element read (`C_DIMR`): [`jit_dim_get`] with the
    /// array variable's slot, the offset and the destination (a
    /// temporary, or the variable of the folded assignment). It stores an
    /// `Int` element and returns 0; anything else leaves the row to the
    /// interpreter, nothing changed.
    fn emit_dimr(&mut self, m: JitMeta, i: usize) {
        self.emit_helper(
            jit_dim_get as *const () as usize,
            [
                HArg::Slot(KC, m.sb),
                Self::dim_arg(m),
                HArg::Slot(m.kc, m.db),
            ],
            u32::from(m.kc == KC) * DIM_GET_TO_CV + u32::from(m.inc) * DIM_GET_EXACT_KEY,
            Target::Side(i),
        );
    }

    /// Emit one array element write (`C_DIMW`, [`jit_dim_set`]) or
    /// compound write (`C_DIMOP`, [`jit_dim_op`] with the operator in
    /// `word`). A consumed value temporary is left null afterwards, as
    /// the interpreted row leaves it. The write may have allocated (the
    /// table grows), and an allocation past the memory limit is reported
    /// at the next safe point: the row polls right after itself, as the
    /// interpreted row does, so the error names this line.
    fn emit_dimw(&mut self, m: JitMeta, i: usize, helper: usize, word: u32) {
        let val = if m.sc == KL {
            HArg::Abs(m.lit_c)
        } else {
            HArg::Slot(m.sc, m.sw)
        };
        self.emit_helper(
            helper,
            [HArg::Slot(KC, m.sb), Self::dim_arg(m), val],
            word,
            Target::Side(i),
        );
        if m.consume {
            self.store_imm_base(KT, m.sw, 0);
            self.store_tag_base(KT, m.sw, TAG_NULL);
        }
        self.put(&[0x41, 0x80, 0x3B, 0x00]); // cmp byte [r11], 0
        self.jcc(0x85, Target::Poll(i + 1, i)); // jne
    }
}
impl Emitter {
    fn new(plan: Plan, tape: Vec<Inst>, scratch: i64) -> Emitter {
        Emitter {
            code: Vec::new(),
            plan,
            tape,
            scratch,
            labels: HashMap::new(),
            stubs: HashMap::new(),
            sides: HashMap::new(),
            polls: HashMap::new(),
            disms: HashMap::new(),
            replans: HashMap::new(),
            fixups: Vec::new(),
            head_pos: 0,
            synth: 0,
        }
    }

    /// Emit the whole region (branches still need `resolve`).
    fn emit(&mut self) {
        // Prologue, run once per compiled call: the poll address, the
        // iteration counter, and the entry guards (all exit at the
        // head so the interpreted step decides what to do instead).
        self.movabs(false, crate::interrupt::jit_poll_addr() as i64);
        self.put(&[REX | REX_R | REX_B, 0x31, 0xC0]); // xor r8d, r8d
        for k in 0..self.plan.guards.len() {
            let g = self.plan.guards[k];
            if g.exact {
                self.cmpb(g.base, g.off, TAG_INT);
                self.jcc(0x85, Target::Disarm(self.plan.head)); // jne
            } else if g.plain && g.defined {
                // A scalar, and a value: the tags below `Str` (`None`
                // reads as `TAG_NONE`, above it).
                self.cmpb(g.base, g.off, TAG_STR);
                self.jcc(0x83, Target::Disarm(self.plan.head)); // jae
            } else if g.defined {
                // Any value, but not nothing.
                self.cmpb(g.base, g.off, TAG_NONE);
                self.jcc(0x84, Target::Disarm(self.plan.head)); // je
            } else {
                // A scalar (below `Str`) or nothing: what the
                // interpreter's own store test accepts.
                self.cmpb(g.base, g.off, TAG_STR);
                // jb over the next pair: jb (6) + cmpb (`cmpb_len`) +
                // jne (6) (`code.len()` is read before the jb is emitted).
                let skip = self.code.len() + 6 + Self::cmpb_len(g.base) + 6;
                self.jcc(0x82, Target::At(skip));
                self.cmpb(g.base, g.off, TAG_NONE);
                self.jcc(0x85, Target::Disarm(self.plan.head)); // jne
            }
        }
        // A `$this` receiver guard or a property read's scope check
        // reads the frame through `r10`: park the frame pointer (entry
        // argument `rdx`) before the scratch load overwrites `rdx`.
        // The frame cannot move while the trace runs (no real call can
        // happen inside it: every call is inlined or exits).
        if self.plan.frame {
            // `mov r10, rdx` (`4C 8B D2`: the `8B` form moves
            // `r/m` into `reg`, so `REX.R` extends the destination
            // `reg` field to `r10` and plain `rm = 010` is the
            // source `rdx`). `4C 89 D2` would be the `89` form,
            // which moves the *other* way (`mov rdx, r10`).
            self.put(&[REX_W | REX_R, 0x8B, 0xD2]);
        }
        // With inlined calls in the region: the scratch buffer base in
        // `rdx` (every `KR` offset is relative to it) and the function-
        // table version guard: if any function was (re)defined since the
        // plan read the callees, the inlined bodies may be stale, so
        // disarm and let the interpreter run the calls.
        if self.plan.has_calls {
            self.movabs_rdx(self.scratch);
        }
        // The function-table version guard (its own block: a region
        // without inlined calls still needs it once any row pins a
        // runtime-cache entry — a static-property cell or a folded
        // constant). Every path that clears a runtime cache (a class
        // re-declaration, a function (re)definition, `clear_runtime_
        // caches`) bumps the version with it, so a stale inlined body,
        // constant value or pinned cell cannot be reached.
        if self.plan.has_calls || self.plan.version_guard {
            self.movabs_rax(crate::func::FUNC_TABLE_VERSION.as_ptr() as i64);
            // `cmp qword [rax], imm32` (`81 /7`, mod=00 rm=000: 0x38).
            self.put(&[REX_W, 0x81, 0x38]);
            self.put(
                &u32::try_from(self.plan.version)
                    .unwrap_or(u32::MAX)
                    .to_le_bytes(),
            );
            // Stale, not wrong for good: the loop is planned again from
            // the caches as they are now (a declaration, an include or an
            // autoloaded class since the plan must not cost the loop its
            // trace for the rest of the run).
            self.jcc(0x85, Target::Replan(self.plan.head)); // jne
        }
        // The head: per-iteration interrupt poll and iteration cap,
        // both exiting at the head (the interpreter then runs one
        // iteration and services the safe point at its back edge).
        self.head_pos = self.code.len();
        self.put(&[0x41, 0x80, 0x3B, 0x00]); // cmp byte [r11], 0
        self.jcc(0x85, Target::Poll(self.plan.head, self.plan.close)); // jne
        self.put(&[0x41, 0x83, 0xC0, 0x01]); // add r8d, 1
                                             // `cmp r8d, imm32`: `3D` has no ModRM byte (its REX prefix is
                                             // ignored and it would compare a leftover `eax`), so use the
                                             // `81 /7` form.
        self.put(&[REX | REX_B, 0x81, 0xF8]);
        self.put(&ITER_CAP.to_le_bytes());
        self.jcc(0x87, Target::Stub(self.plan.head)); // ja
                                                      // The region itself.
        let head = self.plan.head;
        let len = self.plan.close - head + 1;
        // The landings of backward jumps inside the region (the heads of
        // loops nested in this one), each with one of the rows that jump
        // back to it: an inner loop polls at its own back edge when
        // interpreted, so its compiled head polls too. Without it a long
        // inner loop would run to its end before a timeout or a signal
        // could be serviced.
        let mut inner: HashMap<usize, usize> = HashMap::new();
        for j in 0..len {
            let c = self.plan.class[j];
            if c == C_JMP || c == J_CMP || c == J_INCCMP {
                let y = jump_target(&self.tape, head + j);
                if y > head && y <= head + j {
                    inner.insert(y, head + j);
                }
            }
        }
        let mut j = 0;
        while j < len {
            let c = self.plan.class[j];
            if c != 0 {
                self.label(head + j);
                if let Some(&from) = inner.get(&(head + j)) {
                    self.put(&[0x41, 0x80, 0x3B, 0x00]); // cmp byte [r11], 0
                    self.jcc(0x85, Target::Poll(head + j, from)); // jne
                }
                if c == C_CALL {
                    // Each call plan is consumed once (the driver
                    // visits each region instruction once).
                    let cp = self.plan.calls[j].take().expect("planned call");
                    self.emit_call(&cp);
                } else {
                    self.emit_instr(j);
                }
                j += self.plan.span[j] as usize;
            } else {
                j += 1;
            }
        }
        // A fused closing jump's fall-through leaves the region.
        let cc = self.plan.class[len - 1];
        if cc == J_CMP || cc == J_INCCMP {
            let end = self.plan.close + if cc == J_INCCMP { 3 } else { 2 };
            self.jmp(Target::Stub(end));
        }
    }

    /// Patch every `jmp`/`jcc` rel32 and append the exit stubs. A stub
    /// returns the instruction to resume at in `rax` (see [`Target`] for
    /// the encodings) and, in `rdx`, the iteration a row left the trace in
    /// (`Side`), or 0 for every other exit.
    fn resolve(&mut self) {
        let fixups = std::mem::take(&mut self.fixups);
        for (_, t) in &fixups {
            let at = self.code.len();
            match *t {
                Target::Stub(idx) if !self.stubs.contains_key(&idx) => {
                    self.code.push(0xB8); // mov eax, imm32 (the resume index)
                    self.code
                        .extend_from_slice(&u32::try_from(idx).unwrap_or(u32::MAX).to_le_bytes());
                    self.code.extend_from_slice(&[0x31, 0xD2]); // xor edx, edx
                    self.stubs.insert(idx, at);
                }
                Target::Side(idx) if !self.sides.contains_key(&idx) => {
                    self.code.push(0xB8); // mov eax, imm32 (the resume index)
                    self.code
                        .extend_from_slice(&u32::try_from(idx).unwrap_or(u32::MAX).to_le_bytes());
                    self.code.extend_from_slice(&[0x44, 0x89, 0xC2]); // mov edx, r8d
                    self.sides.insert(idx, at);
                }
                Target::Poll(resume, row) if !self.polls.contains_key(&(resume, row)) => {
                    self.code.extend_from_slice(&[0x48, 0xB8]); // movabs rax, imm64
                    self.code
                        .extend_from_slice(&poll_word(resume, row).to_le_bytes());
                    self.code.extend_from_slice(&[0x31, 0xD2]); // xor edx, edx
                    self.polls.insert((resume, row), at);
                }
                Target::Disarm(idx) if !self.disms.contains_key(&idx) => {
                    // `-(idx + 1)`: negative means "disarm this trace".
                    self.code.extend_from_slice(&[0x48, 0xB8]); // movabs rax, imm64
                    self.code
                        .extend_from_slice(&((idx as i64 + 1).wrapping_neg()).to_le_bytes());
                    self.disms.insert(idx, at);
                }
                Target::Replan(idx) if !self.replans.contains_key(&idx) => {
                    self.code.extend_from_slice(&[0x48, 0xB8]); // movabs rax, imm64
                    self.code.extend_from_slice(&replan_word(idx).to_le_bytes());
                    self.replans.insert(idx, at);
                }
                _ => continue,
            }
            self.code.push(0xC3); // ret
        }
        for (pos, t) in fixups {
            let abs = match t {
                Target::Head => self.head_pos,
                Target::Label(j) => self.labels[&j],
                Target::Stub(idx) => self.stubs[&idx],
                Target::Side(idx) => self.sides[&idx],
                Target::Poll(resume, row) => self.polls[&(resume, row)],
                Target::Disarm(idx) => self.disms[&idx],
                Target::Replan(idx) => self.replans[&idx],
                Target::At(p) => p,
            };
            let rel = abs as i64 - (pos + 4) as i64;
            self.code[pos..pos + 4].copy_from_slice(&i32::try_from(rel).unwrap_or(0).to_le_bytes());
        }
    }
}
// -------------------------------------------------------- whole functions
//
// A loop is not the only thing worth compiling: a function that calls
// itself (`fib`, `Ack`) spends its time in the call protocol, and none of
// it is in a loop. Such a function is compiled *whole* when it is pure
// int code: every instruction it can reach is int arithmetic, a
// comparison and branch, a `return` of an int, or a call by name of
// itself (or of another function already compiled this way) that passes
// every parameter.
//
// Its machine code keeps the function's variables and temporaries in a
// frame on the machine stack and recurses with the machine's own `call`.
// It touches nothing else - no interpreter frame, no heap, no output -
// so it needs no way to hand a half-finished call back: whenever it
// cannot go on (an int overflow, a division by zero, a recursion deeper
// than its stack budget, a safe point that came due) it drops its whole
// stack and reports "declined", and the interpreter makes the same call
// from the start, as if the native code had never run.

/// [`crate::func::CallShape::native`] of a function that is not compiled
/// whole.
pub(crate) const NATIVE_REFUSED: usize = 1;
/// The most arguments a function compiled whole takes: one register each.
pub(crate) const NATIVE_MAX_ARGS: usize = 6;
/// Plain calls of a function before it is compiled whole.
const NATIVE_HOT_CALLS: u32 = 100;
/// Declined calls in a row after which a function's native code is given up.
const NATIVE_MAX_BAILS: u32 = 8;
/// The most ops of a function compiled whole.
const NATIVE_MAX_OPS: usize = 256;
/// The most slots (variables and temporaries) of such a function: the
/// known-int analysis keeps one bit each.
const NATIVE_MAX_SLOTS: usize = 120;
/// The machine stack one native call (with all its recursion) may use; a
/// deeper recursion is the interpreter's, whose frames are on the heap.
const NATIVE_STACK_BUDGET: i32 = 512 * 1024;

/// The entry of a function compiled whole: up to six int arguments, and
/// the result in `r` with `iters` 0; `iters` 1 for "declined", 2 for
/// "declined because a safe point is due".
type NativeFn = unsafe extern "C" fn(i64, i64, i64, i64, i64, i64) -> TraceRet;

/// One op of a function compiled whole.
#[derive(Clone, Copy)]
enum NRow {
    /// An int row [`Emitter::emit_op`] writes: its class and meta.
    Op(u16, JitMeta),
    /// `Jmp` to an op.
    Jmp(usize),
    /// The fused compare-and-branch: to the op when the comparison holds.
    Cmp(JitMeta, usize),
    /// The fused `++`/`--`, compare and branch.
    IncCmp(JitMeta, usize),
    /// `$tmp = value` (an arm of a ternary).
    Move(Arg, u32),
    /// `return value`.
    Ret(Arg),
    /// A call's `InitFCall`: no code (the callee is in its `Call` row).
    Nop,
    /// A `Send`: the argument's value onto the machine stack.
    Push(Arg),
    /// A `DoCall`: the `n` pushed arguments into the argument registers,
    /// the call (`body` 0: the function itself, else the callee's body),
    /// and the result into the temporary `dst`.
    Call {
        n: usize,
        dst: Option<u32>,
        body: usize,
    },
}

/// A function the plan accepted for compiling whole.
struct NPlan {
    /// The reachable ops, by op index (`None`: not reached, or covered by
    /// the fused row before it).
    rows: Vec<Option<NRow>>,
    ncvs: usize,
    ntmps: usize,
    nparams: usize,
    /// The function's tape (the int rows are emitted from it).
    tape: Vec<Inst>,
}

/// The int classes a function compiled whole may hold besides its jumps.
fn native_int_class(c: u16) -> bool {
    matches!(
        c,
        C_INC
            | C_DEC
            | C_ASSIGN_I
            | C_ASSIGN_L
            | J_ASSIGN_T
            | J_ARITH_TMP
            | J_NUMBIN_T
            | J_ARITH_CV
            | J_ASSIGN_OP
            | J_NUMBIN_C
            | J_NUMBIN_D
            | C_ABS
            | C_NOP
    )
}

/// An operand of a function compiled whole: a slot (`KC`/`KT` and its
/// offset) or an int (`KL`, the value in `lit`). `None` for anything else
/// (a literal that is no int, no operand at all).
fn native_arg(code: &crate::opcode::OpArray, cst: &St, o: Operand) -> Option<Arg> {
    let (kind, off) = src(code, o)?;
    Some(match kind {
        KC | KT => Arg { kind, off, lit: 0 },
        #[allow(clippy::cast_possible_wrap)]
        KI => Arg {
            kind: KL,
            off: 0,
            lit: i64::from(off as i32),
        },
        _ => Arg {
            kind: KL,
            off: 0,
            lit: resolved_lit(cst, off)?,
        },
    })
}

/// The callee of an `InitFCall` inside a function being compiled whole:
/// the function itself (body 0) or one already compiled whole (its body's
/// address), with its parameter count. `None` for anything else.
fn native_callee(
    uf: &Rc<crate::func::UserFunction>,
    code: &crate::opcode::OpArray,
    name: u32,
) -> Option<(usize, usize)> {
    let cache = code.fcall_cache.try_borrow().ok()?;
    let Some(Func::User(callee)) = cache.get(name as usize)?.as_ref()?.get() else {
        return None;
    };
    if Rc::ptr_eq(&callee, uf) {
        return Some((0, uf.params.len()));
    }
    let shape = &callee.call_shape;
    (shape.native.get() > NATIVE_REFUSED).then(|| (shape.native_body.get(), callee.params.len()))
}

/// Decode the call that starts at the `InitFCall` at op `start` of a
/// function being compiled whole, up to its `DoCall`: the ops in between
/// must follow each other without a jump (arguments are computed and
/// sent in order; a call inside an argument nests), each one an int row,
/// a `Send` of an int value, or a nested call. Returns the op after the
/// `DoCall`. `inside` marks every op after the first: nothing may jump
/// there.
fn native_call_seq(
    uf: &Rc<crate::func::UserFunction>,
    cst: &St,
    tape: &[Inst],
    start: usize,
    rows: &mut [Option<NRow>],
    inside: &mut [bool],
) -> Option<usize> {
    let code = &uf.code;
    // The calls begun and not made yet: (body, parameters, sent so far).
    let mut pending: Vec<(usize, usize, usize)> = Vec::new();
    let mut j = start;
    loop {
        if rows.get(j)?.is_some() {
            return None;
        }
        if j != start {
            *inside.get_mut(j)? = true;
        }
        match code.ops.get(j)? {
            Op::InitFCall {
                name,
                fallback: None,
                ..
            } => {
                let (body, params) = native_callee(uf, code, *name)?;
                pending.push((body, params, 0));
                rows[j] = Some(NRow::Nop);
                j += 1;
            }
            Op::Send { src: arg } => {
                pending.last_mut()?.2 += 1;
                let o = match arg {
                    ArgSrc::Val(o @ (Operand::Lit(_) | Operand::Tmp(_) | Operand::Cv(_)))
                    | ArgSrc::CallResult(o @ Operand::Tmp(_)) => *o,
                    ArgSrc::Place(pid) => Operand::Cv(super::calls::place_cv(code, *pid)?),
                    _ => return None,
                };
                rows[j] = Some(NRow::Push(native_arg(code, cst, o)?));
                j += 1;
            }
            Op::DoCall { dst } => {
                let (body, params, sent) = pending.pop()?;
                // Every declared parameter, and nothing more.
                if sent != params || sent > NATIVE_MAX_ARGS {
                    return None;
                }
                let dst = match dst {
                    Some(d) => Some(tmp_off(code, *d)?),
                    None => None,
                };
                rows[j] = Some(NRow::Call { n: sent, dst, body });
                j += 1;
                if pending.is_empty() {
                    return Some(j);
                }
            }
            _ => {
                let (c, m) = classify(cst, tape.get(j)?)?;
                if !native_int_class(c) {
                    return None;
                }
                rows[j] = Some(NRow::Op(c, m));
                for k in 1..c_step(c) {
                    *inside.get_mut(j + k)? = true;
                }
                j += c_step(c);
            }
        }
    }
}

/// The ops execution may continue at after the row at op `i`: the next
/// op in line (`None` after a `Jmp` or a `return`) and a jump's target.
fn native_succs(i: usize, row: NRow) -> (Option<usize>, Option<usize>) {
    match row {
        NRow::Op(c, _) => (Some(i + c_step(c)), None),
        NRow::Jmp(t) => (None, Some(t)),
        NRow::Cmp(_, t) => (Some(i + 2), Some(t)),
        NRow::IncCmp(_, t) => (Some(i + 3), Some(t)),
        NRow::Move(..) | NRow::Nop | NRow::Push(_) | NRow::Call { .. } => (Some(i + 1), None),
        NRow::Ret(_) => (None, None),
    }
}

/// The slots (bit numbers: variables first, then temporaries) the row at
/// `inst` reads as ints, writes an int into, and leaves without one.
fn native_effects(row: NRow, inst: &Inst, ncvs: usize) -> (Vec<usize>, Vec<usize>, Vec<usize>) {
    let bit = |kind: u8, off: u32| -> Option<usize> {
        match kind {
            KC => Some((off >> SLOT_SHIFT) as usize),
            KT => Some(ncvs + (off >> SLOT_SHIFT) as usize),
            _ => None,
        }
    };
    let arg = |a: Arg| bit(a.kind, a.off);
    let (mut reads, mut writes, mut kills) = (Vec::new(), Vec::new(), Vec::new());
    match row {
        NRow::Op(c, m) => match c {
            C_NOP => {}
            C_ABS => {
                reads.extend(bit(m.ka, inst.a));
                if inst.c != u32::MAX {
                    writes.extend(bit(KT, inst.c));
                }
            }
            C_INC | C_DEC => reads.extend(bit(KC, inst.a)),
            C_ASSIGN_I | C_ASSIGN_L => writes.extend(bit(KC, inst.a)),
            // The source temporary is consumed (the row nulls it).
            J_ASSIGN_T => {
                reads.extend(bit(KT, inst.b));
                writes.extend(bit(KC, inst.a));
                kills.extend(bit(KT, inst.b));
            }
            J_ARITH_CV | J_ARITH_TMP | J_NUMBIN_T | J_NUMBIN_D => {
                reads.extend(bit(m.ka, inst.a));
                reads.extend(bit(m.kb, inst.b));
                writes.extend(match c {
                    J_ARITH_CV => bit(KC, inst.c),
                    J_NUMBIN_D => bit(KC, inst.d),
                    _ => bit(KT, inst.c),
                });
            }
            // `$cv op= b`: the variable is read and stays an int.
            _ => {
                reads.extend(bit(KC, inst.a));
                reads.extend(bit(m.kb, inst.b));
            }
        },
        NRow::Cmp(m, _) => {
            reads.extend(bit(m.ka, inst.a));
            reads.extend(bit(m.kb, inst.b));
        }
        NRow::IncCmp(m, _) => {
            reads.extend(bit(KC, inst.a));
            reads.extend(bit(m.kb, inst.b));
        }
        NRow::Move(a, dst) => {
            reads.extend(arg(a));
            writes.extend(bit(KT, dst));
        }
        NRow::Ret(a) => reads.extend(arg(a)),
        // A sent temporary is consumed.
        NRow::Push(a) => {
            reads.extend(arg(a));
            if a.kind == KT {
                kills.extend(arg(a));
            }
        }
        NRow::Call { dst, .. } => writes.extend(dst.and_then(|d| bit(KT, d))),
        NRow::Nop | NRow::Jmp(_) => {}
    }
    (reads, writes, kills)
}

/// Plan the user function `uf` for compiling whole, or refuse (`None`).
#[allow(clippy::too_many_lines)] // one function's decode and its one analysis
fn plan_native(uf: &Rc<crate::func::UserFunction>) -> Option<NPlan> {
    macro_rules! no {
        ($($a:tt)*) => {{
            if debug_on() {
                eprintln!("jit: function {} not compiled whole: {}", uf.name, format_args!($($a)*));
            }
            return None;
        }};
    }
    let code = &uf.code;
    let (ncvs, ntmps, nparams) = (code.cv_names.len(), code.num_tmps as usize, uf.params.len());
    if uf.call_shape() == crate::func::CallShape::NOT_PLAIN || !uf.uses.is_empty() {
        no!("its declaration rules out a plain call");
    }
    if nparams > NATIVE_MAX_ARGS || nparams > ncvs || ncvs + ntmps > NATIVE_MAX_SLOTS {
        no!("too many parameters or slots");
    }
    if code.ops.len() > NATIVE_MAX_OPS {
        no!("too long");
    }
    // An int must pass every declared type as it is, as for an inlined
    // call (`plan_call`): the parameters', and the return type's.
    if uf.params.iter().any(|p| {
        p.ty.as_ref()
            .is_some_and(|t| !t.is_mixed() && t.mask & mask::LONG == 0)
    }) {
        no!("a parameter's type does not take an int as it is");
    }
    if uf
        .return_type
        .as_ref()
        .is_some_and(|t| t.is_void() || t.is_never() || !(t.is_mixed() || t.mask & mask::LONG != 0))
    {
        no!("the return type does not take an int as it is");
    }
    let tape = code.tape.get(code);
    let nops = code.ops.len();
    let cst = St {
        cvs: std::ptr::null_mut(),
        tmps: std::ptr::null_mut(),
        lits: code.literals.as_ptr().cast(),
        f: std::ptr::null_mut(),
    };
    // Decode every op the function can reach from its first op after the
    // `RECV`s of its parameters (all of them are passed).
    let start = uf.plain_first_op(nparams);
    let mut rows: Vec<Option<NRow>> = vec![None; nops];
    let mut inside = vec![false; nops + 4];
    let mut work = vec![start];
    while let Some(i) = work.pop() {
        if i >= nops {
            no!("it can run off its end");
        }
        if rows[i].is_some() {
            continue;
        }
        let row = match &code.ops[i] {
            Op::Return { val } => match native_arg(code, &cst, *val) {
                Some(a) => NRow::Ret(a),
                None => no!("op {i} returns something that is no int"),
            },
            Op::Move { src: from, dst } => {
                match (native_arg(code, &cst, *from), tmp_off(code, *dst)) {
                    (Some(a), Some(d)) => NRow::Move(a, d),
                    _ => no!("op {i} moves something that is no int"),
                }
            }
            Op::InitFCall { .. } => {
                match native_call_seq(uf, &cst, tape, i, &mut rows, &mut inside) {
                    Some(next) => work.push(next),
                    None => no!("the call at op {i} is not one of an int function compiled whole"),
                }
                continue;
            }
            _ => {
                let Some((c, m)) = tape.get(i).and_then(|row| classify(&cst, row)) else {
                    no!("op {i} ({:?}) is no int row", tape.get(i).map(|r| r.op));
                };
                match c {
                    C_JMP => NRow::Jmp(jump_target(tape, i)),
                    J_CMP | J_INCCMP if jcc_for(m.mask).is_none() => {
                        no!("op {i} compares in a way the emitter does not know")
                    }
                    J_CMP => NRow::Cmp(m, jump_target(tape, i)),
                    J_INCCMP => NRow::IncCmp(m, jump_target(tape, i)),
                    c if native_int_class(c) => NRow::Op(c, m),
                    _ => no!("op {i} ({:?}) is no int row", tape[i].op),
                }
            }
        };
        let (next, target) = native_succs(i, row);
        work.extend(next);
        work.extend(target);
        rows[i] = Some(row);
    }
    // A jump may not land inside a call's argument sequence.
    for (i, row) in rows.iter().enumerate() {
        if let Some(row) = row {
            let (_, target) = native_succs(i, *row);
            if target.is_some_and(|t| t >= nops || inside[t]) {
                no!("op {i} jumps into a call's arguments");
            }
        }
    }
    // Which slots hold an int when each op starts, on every path to it:
    // the parameters at the start, then what the rows before it wrote.
    // (A set only ever shrinks, so the loop ends.)
    let mut known: Vec<u128> = vec![!0; nops];
    let mut seen = vec![false; nops];
    known[start] = (1u128 << nparams) - 1;
    seen[start] = true;
    let mut work = vec![start];
    while let Some(i) = work.pop() {
        let Some(row) = rows[i] else { continue };
        let (_, writes, kills) = native_effects(row, &tape[i], ncvs);
        let mut out = known[i];
        for b in kills {
            out &= !(1u128 << b);
        }
        for b in writes {
            out |= 1u128 << b;
        }
        let (next, target) = native_succs(i, row);
        for s in next.into_iter().chain(target) {
            let merged = known[s] & out;
            if merged != known[s] || !seen[s] {
                known[s] = merged;
                seen[s] = true;
                work.push(s);
            }
        }
    }
    for (i, row) in rows.iter().enumerate() {
        if let Some(row) = row {
            let (reads, _, _) = native_effects(*row, &tape[i], ncvs);
            if reads.iter().any(|b| known[i] & (1u128 << b) == 0) {
                no!("op {i} reads a slot that is not an int on every path");
            }
        }
    }
    Some(NPlan {
        rows,
        ncvs,
        ntmps,
        nparams,
        tape: tape.to_vec(),
    })
}

/// The machine code of a planned function, and where in it the body
/// starts (what another native function calls).
///
/// The entry (at offset 0) is the C-ABI stub the interpreter calls: it
/// saves `rbx`/`r12`/`r13`, keeps the stack pointer to return to in
/// `rbx`, the stack floor in `r12` and the poll address in `r13`, moves
/// the arguments into the body's registers and calls the body. A body
/// that cannot go on jumps to the stub's bail block, which drops every
/// native frame by loading `rsp` from `rbx` and returns "declined".
///
/// The body keeps its variables and temporaries (16 bytes each, the
/// interpreter's slot layout, so the int rows of the loop emitter are
/// reused as they are) in a frame on the machine stack, addressed off
/// `rdi` (variables) and `rsi` (temporaries), which it saves for its
/// caller. Arguments arrive in `rax`, `rcx`, `r9`, `rdx`, `r8`, `r10`
/// (not the C registers: two of those are the slot bases), and the
/// result leaves in `rax`. A `Send` pushes its value on the machine
/// stack; the call pops them into the registers.
#[allow(clippy::too_many_lines)] // one function's code, top to bottom
fn emit_native(plan: &NPlan) -> (Vec<u8>, usize) {
    let dummy = Plan {
        head: 0,
        close: 0,
        class: Vec::new(),
        meta: Vec::new(),
        guards: Vec::new(),
        entries: Vec::new(),
        span: Vec::new(),
        calls: Vec::new(),
        has_calls: false,
        frame: false,
        version_guard: false,
        version: 0,
    };
    let mut em = Emitter::new(dummy, plan.tape.clone(), 0);
    // Label keys past every op's own.
    let n = plan.tape.len();
    let (l_body, l_bail, l_exit, l_poll) = (n + 1, n + 2, n + 3, n + 4);
    let bail = Target::Label(l_bail);
    // A safe point that came due is no fault of the function's: it leaves
    // with its own status, which is not counted against the native code.
    let poll = Target::Label(l_poll);
    // ---- the entry stub
    em.put(&[0x53, 0x41, 0x54, 0x41, 0x55]); // push rbx; push r12; push r13
    em.put(&[0x48, 0x89, 0xE3]); // mov rbx, rsp
    em.put(&[0x4C, 0x8D, 0xA4, 0x24]); // lea r12, [rsp - budget]
    em.put(&(-NATIVE_STACK_BUDGET).to_le_bytes());
    em.put(&[0x49, 0xBD]); // movabs r13, poll address
    em.put(&(crate::interrupt::jit_poll_addr() as i64).to_le_bytes());
    // The C argument registers into the body's: r10 <- r9, r9 <- rdx,
    // rdx <- rcx, rcx <- rsi, rax <- rdi (`r8` is the fifth in both), in
    // an order that reads each source before it is overwritten.
    em.put(&[0x4D, 0x89, 0xCA]); // mov r10, r9
    em.put(&[0x49, 0x89, 0xD1]); // mov r9, rdx
    em.put(&[0x48, 0x89, 0xCA]); // mov rdx, rcx
    em.put(&[0x48, 0x89, 0xF1]); // mov rcx, rsi
    em.put(&[0x48, 0x89, 0xF8]); // mov rax, rdi
    em.call(Target::Label(l_body));
    em.put(&[0x31, 0xD2]); // xor edx, edx: done
    em.set_label(l_exit);
    em.put(&[0x41, 0x5D, 0x41, 0x5C, 0x5B, 0xC3]); // pop r13; pop r12; pop rbx; ret
    em.set_label(l_bail);
    em.put(&[0x48, 0x89, 0xDC]); // mov rsp, rbx: every native frame is gone
    em.put(&[0xBA, 1, 0, 0, 0]); // mov edx, 1: declined
    em.jmp(Target::Label(l_exit));
    em.set_label(l_poll);
    em.put(&[0x48, 0x89, 0xDC]); // mov rsp, rbx
    em.put(&[0xBA, 2, 0, 0, 0]); // mov edx, 2: declined for a safe point
    em.jmp(Target::Label(l_exit));
    // ---- the body
    em.set_label(l_body);
    let body = em.code.len();
    // The frame: the slots, and 8 bytes that keep the stack 16-aligned
    // (the body is entered by a `call` and pushes two registers).
    #[allow(clippy::cast_possible_truncation)] // at most `NATIVE_MAX_SLOTS`
    let frame = (16 * (plan.ncvs + plan.ntmps) + 8) as u32;
    #[allow(clippy::cast_possible_truncation)]
    let tmps_at = (16 * plan.ncvs) as u32;
    em.put(&[0x57, 0x56]); // push rdi; push rsi (the caller's slot bases)
    em.put(&[0x48, 0x81, 0xEC]); // sub rsp, frame
    em.put(&frame.to_le_bytes());
    em.put(&[0x4C, 0x39, 0xE4]); // cmp rsp, r12
    em.jcc(0x82, bail); // jb: past the stack budget
    em.put(&[0x41, 0x80, 0x7D, 0x00, 0x00]); // cmp byte [r13], 0
    em.jcc(0x85, poll); // jne: a safe point is due
    em.put(&[0x48, 0x89, 0xE7]); // mov rdi, rsp
    em.put(&[0x48, 0x8D, 0xB4, 0x24]); // lea rsi, [rsp + 16 * ncvs]
    em.put(&tmps_at.to_le_bytes());
    // The arguments into the first variables (tags are never read in a
    // native frame: every read was proven an int by the plan).
    for k in 0..plan.nparams {
        #[allow(clippy::cast_possible_truncation)] // at most `NATIVE_MAX_ARGS`
        let off = (k as u32) << SLOT_SHIFT;
        match k {
            0 => em.store_int(KC, off),
            1 => em.store_rcx_base(KC, off),
            2 => em.store_int_r9_base(KC, off),
            _ => {
                // rdx, r8, r10 through `rax` (the first argument is stored).
                em.put(match k {
                    3 => &[0x48, 0x89, 0xD0],
                    4 => &[0x4C, 0x89, 0xC0],
                    _ => &[0x4C, 0x89, 0xD0],
                });
                em.store_int(KC, off);
            }
        }
    }
    // The ops, in order. A loop inside the function polls at its head.
    let order: Vec<usize> = (0..plan.rows.len())
        .filter(|i| plan.rows[*i].is_some())
        .collect();
    let mut heads: Vec<usize> = Vec::new();
    for &i in &order {
        if let Some(row) = plan.rows[i] {
            if let (_, Some(t)) = native_succs(i, row) {
                if t <= i {
                    heads.push(t);
                }
            }
        }
    }
    for (pos, &i) in order.iter().enumerate() {
        let Some(row) = plan.rows[i] else { continue };
        let inst = plan.tape[i];
        em.label(i);
        if heads.contains(&i) {
            em.put(&[0x41, 0x80, 0x7D, 0x00, 0x00]); // cmp byte [r13], 0
            em.jcc(0x85, poll); // jne
        }
        match row {
            NRow::Op(c, m) => em.emit_op(c, m, &inst, bail),
            NRow::Jmp(t) => em.jmp(Target::Label(t)),
            NRow::Cmp(m, t) => {
                em.to_rax(m.ka, inst.a, m.lit_a);
                em.to_r9(m.kb, inst.b, m.lit_b);
                em.put(&[REX_W | REX_R, 0x39, 0xC8]); // cmp rax, r9
                em.jcc(jcc_for(m.mask).unwrap_or(0x85), Target::Label(t));
            }
            NRow::IncCmp(m, t) => {
                // `b` first: it may be the slot that is stepped.
                em.to_r9(m.kb, inst.b, m.lit_b);
                em.load_int(KC, inst.a);
                em.put(&[REX_W, 0x83, if m.inc { 0xC0 } else { 0xE8 }, 0x01]);
                em.jcc(0x80, bail); // jo
                em.store_int(KC, inst.a);
                em.put(&[REX_W | REX_R, 0x39, 0xC8]); // cmp rax, r9
                em.jcc(jcc_for(m.mask).unwrap_or(0x85), Target::Label(t));
            }
            NRow::Move(a, dst) => {
                em.to_rax(a.kind, a.off, a.lit);
                em.store_int(KT, dst);
                em.store_tag_base(KT, dst, TAG_INT);
            }
            NRow::Ret(a) => {
                em.to_rax(a.kind, a.off, a.lit);
                em.put(&[0x48, 0x81, 0xC4]); // add rsp, frame
                em.put(&frame.to_le_bytes());
                em.put(&[0x5E, 0x5F, 0xC3]); // pop rsi; pop rdi; ret
            }
            NRow::Nop => {}
            NRow::Push(a) => {
                em.to_rax(a.kind, a.off, a.lit);
                em.put(&[0x50]); // push rax
            }
            NRow::Call { n, dst, body } => {
                // The pushed arguments into rax, rcx, r9, rdx, r8, r10:
                // the last one sent is on top.
                const POP: [&[u8]; NATIVE_MAX_ARGS] = [
                    &[0x58],
                    &[0x59],
                    &[0x41, 0x59],
                    &[0x5A],
                    &[0x41, 0x58],
                    &[0x41, 0x5A],
                ];
                for k in (0..n.min(NATIVE_MAX_ARGS)).rev() {
                    em.put(POP[k]);
                }
                if body == 0 {
                    em.call(Target::Label(l_body));
                } else {
                    em.movabs(false, body as i64); // movabs r11, body
                    em.put(&[0x41, 0xFF, 0xD3]); // call r11
                }
                // The callee restored `rdi`/`rsi`; the result is in `rax`.
                if let Some(d) = dst {
                    em.store_int(KT, d);
                    em.store_tag_base(KT, d, TAG_INT);
                }
            }
        }
        // Where the row goes on when it does not jump, if that is not
        // the row emitted next (a fused row's covered ops lie between,
        // or an op that is entered on its own, as a loop's condition is).
        if let (Some(next), _) = native_succs(i, row) {
            if order.get(pos + 1) != Some(&next) {
                em.jmp(Target::Label(next));
            }
        }
    }
    em.resolve();
    (em.code, body)
}

/// Compile `uf` whole: its entry's address, or `None` when it is refused.
fn compile_native(uf: &Rc<crate::func::UserFunction>) -> Option<usize> {
    if TRACES.load(Ordering::Relaxed) >= MAX_TRACES {
        return None;
    }
    let _g = COMPILE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let plan = plan_native(uf)?;
    let (code, body) = emit_native(&plan);
    let entry = place(&code)? as usize;
    TRACES.fetch_add(1, Ordering::Relaxed);
    uf.call_shape.native_body.set(entry + body);
    if debug_on() {
        eprintln!(
            "jit: function {} compiled whole to {} bytes",
            uf.name,
            code.len()
        );
    }
    Some(entry)
}

/// A plain call of `uf` with the int arguments `args` (one per declared
/// parameter), made in machine code when the function is compiled whole:
/// its result. `None` when it is not (yet: its calls are counted, and the
/// hundredth compiles it), or when the native code declined, having
/// changed nothing - the interpreter then makes the call.
pub(crate) fn native_call(uf: &Rc<crate::func::UserFunction>, args: &[i64]) -> Option<i64> {
    let shape = &uf.call_shape;
    let mut entry = shape.native.get();
    if entry == NATIVE_REFUSED {
        return None;
    }
    if entry == 0 {
        let n = shape.native_count.get() + 1;
        if n < NATIVE_HOT_CALLS {
            shape.native_count.set(n);
            return None;
        }
        shape.native_count.set(0);
        entry = if enabled() {
            compile_native(uf).unwrap_or(NATIVE_REFUSED)
        } else {
            NATIVE_REFUSED
        };
        shape.native.set(entry);
        if entry == NATIVE_REFUSED {
            return None;
        }
    }
    let mut a = [0i64; NATIVE_MAX_ARGS];
    a.get_mut(..args.len())?.copy_from_slice(args);
    // SAFETY: `entry` is the stub `compile_native` placed for this very
    // function: it takes six ints, touches only its own machine stack and
    // returns through the stub whatever happens inside.
    let r = unsafe {
        let f: NativeFn = std::mem::transmute(entry);
        f(a[0], a[1], a[2], a[3], a[4], a[5])
    };
    if r.iters != 0 {
        // Declined. A function that keeps declining for its own reasons
        // (status 1: an overflow, a recursion past the stack budget) is
        // the interpreter's; a safe point that came due (status 2) is not
        // held against it.
        if r.iters == 1 {
            let bails = shape.native_count.get() + 1;
            shape.native_count.set(bails);
            if bails >= NATIVE_MAX_BAILS {
                shape.native.set(NATIVE_REFUSED);
            }
        }
        return None;
    }
    if shape.native_count.get() != 0 {
        shape.native_count.set(0);
    }
    Some(r.r)
}

// ------------------------------------------------------------- the driver

extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64)
        -> *mut c_void;
    fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32;
}
const PROT_READ: i32 = 1;
const PROT_WRITE: i32 = 2;
const PROT_EXEC: i32 = 4;
const MAP_PRIVATE: i32 = 0x02;
const MAP_ANONYMOUS: i32 = 0x20;
const PAGE: usize = 4096;

/// Copy the code into fresh pages and make them executable. The pages
/// are never freed: unloading machine code a thread may be running is
/// a problem this version does not need (traces are capped).
fn place(code: &[u8]) -> Option<*mut u8> {
    if code.is_empty() {
        return None;
    }
    let len = code.len().next_multiple_of(PAGE);
    // SAFETY: a fresh private anonymous mapping of `len` bytes; the
    // copy fits, and it is only run after the mapping is made RX.
    let p = unsafe {
        mmap(
            std::ptr::null_mut(),
            len,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if p == (-1isize) as *mut c_void {
        return None;
    }
    unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), p.cast::<u8>(), code.len()) };
    if unsafe { mprotect(p, len, PROT_READ | PROT_EXEC) } != 0 {
        return None;
    }
    Some(p.cast::<u8>())
}

/// A trace's scratch buffer for inlined calls: `total` bytes of fresh
/// private anonymous `RW` memory, zeroed by the mapping and never freed
/// (like the trace pages themselves). `None` when the mapping fails.
fn mmap_scratch(total: usize) -> Option<*mut u8> {
    if total == 0 {
        return None;
    }
    let len = total.next_multiple_of(PAGE);
    // SAFETY: a fresh private anonymous mapping of `len` bytes.
    let p = unsafe {
        mmap(
            std::ptr::null_mut(),
            len,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if p == (-1isize) as *mut c_void {
        return None;
    }
    Some(p.cast::<u8>())
}

/// The compiled entry: `fn(cvs, tmps, frame) -> resume index` (the frame
/// pointer is only read by `$this` receiver guards; others ignore it).
type LoopFn = unsafe extern "C" fn(*mut u8, *mut u8, *mut u8) -> TraceRet;

/// What a compiled loop returns (`rax` and `rdx`, the two-word return of
/// the C ABI): `r`, where to resume - negative for "disarm this trace
/// and resume at `-(r + 1)`", else the instruction index in its low half
/// with a poll exit's row above it (see [`poll_word`]) - and `iters`,
/// the iteration a row left the trace in ([`Target::Side`]: 1 is the
/// first one of this entry), 0 for every other exit.
#[repr(C)]
struct TraceRet {
    r: i64,
    iters: u64,
}

/// A trace that left through a row during the first iteration of this
/// many entries in a row is switched off: every such entry costs the
/// trace's entry and exit and gains nothing over interpreting (an array
/// of floats under an int-only row, a receiver of another class each
/// time, a property that is never an `Int`).
const UNPRODUCTIVE_LIMIT: i32 = 32;

/// How many traces the process has compiled (bounded by `MAX_TRACES`).
static TRACES: AtomicUsize = AtomicUsize::new(0);
/// One compile at a time (compile is rare and slow; the lock keeps
/// `mmap` use and any future arena simple).
static COMPILE: Mutex<()> = Mutex::new(());

/// Whether to dump compile decisions to stderr (BUZZS_JIT_DEBUG).
fn debug_on() -> bool {
    static DBG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DBG.get_or_init(|| std::env::var_os("BUZZS_JIT_DEBUG").is_some_and(|v| v != "0"))
}

/// Compile the loop `head ..= close` on `st`'s tape. The result is the
/// entry address as a `usize`, or `usize::MAX` (refused: the region,
/// this version, stays interpreted).
fn attempt(st: &St, ctx: &JitCtx, head: usize, close: usize) -> usize {
    // The cap is on traces compiled (each owns pages for good): a refused
    // region costs nothing lasting, so it does not count toward it.
    if TRACES.load(Ordering::Relaxed) >= MAX_TRACES {
        return usize::MAX;
    }
    let _g = COMPILE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: `tbase`/`tlen` are the tape being run, alive until it is
    // dropped (later than every compiled region through the cell).
    let tape = unsafe { std::slice::from_raw_parts(ctx.tbase, ctx.tlen) };
    // The op array (for the inlined calls' raw shapes and callee codes).
    // SAFETY: `st.f` is the running frame, alive through the compile.
    let code = unsafe { &*std::rc::Rc::as_ptr(&(*st.f).code) };
    if debug_on() {
        eprintln!("jit: region {head}..={close} of tape len {}", ctx.tlen);
        // Small tapes dump whole (the region alone hides where a
        // region's temp is produced); big tapes dump just the region.
        let (d0, d1) = if ctx.tlen <= 64 {
            (0, ctx.tlen)
        } else {
            (head, close + 1)
        };
        for (i, inst) in tape[d0..d1].iter().enumerate() {
            let note = if inst.op == TOp::CallInit
                || is_dim_row(inst.op)
                || classify(st, inst).is_some()
            {
                // (`CallInit`: `plan_call` decides its own whitelist.)
                ""
            } else {
                " <-- not whitelisted"
            };
            eprintln!(
                "jit:   [{}] {:?} a={} b={} c={} e={} k={}{}",
                d0 + i,
                inst.op,
                inst.a,
                inst.b,
                inst.c,
                inst.e,
                inst.k,
                note
            );
        }
    }
    PLAN_SOFT.with(|s| s.set(false));
    let mut p = match plan(st, code, tape, head, close) {
        Some(p) => p,
        None => {
            let again = PLAN_SOFT.with(Cell::get);
            if debug_on() {
                eprintln!(
                    "jit: region {head}..={close} rejected by plan{}",
                    if again {
                        " (a cache miss: may be planned again)"
                    } else {
                        ""
                    }
                );
            }
            return if again { REFUSED_FOR_NOW } else { usize::MAX };
        }
    };
    if debug_on() {
        for g in &p.guards {
            eprintln!(
                "jit:   guard base {} off {} {}",
                g.base,
                g.off,
                match (g.exact, g.plain, g.defined) {
                    (true, _, _) => "exact",
                    (_, true, true) => "scalar",
                    (_, true, false) => "weak",
                    _ => "defined",
                }
            );
        }
    }
    // The inlined calls' scratch buffer: disjoint slices, one per call,
    // each the callee's compiled variables then its temporaries. The
    // `KR` offsets are relative to the buffer start (`rdx` is set to
    // its address in the prologue), so `set_base` lays out byte offsets
    // from 0.
    let mut scratch = 0i64;
    if p.has_calls {
        let total: usize = p.calls.iter().flatten().map(|cp| cp.size as usize).sum();
        if total >= i32::MAX as usize {
            if debug_on() {
                eprintln!("jit: region {head}..={close}: scratch too big");
            }
            return usize::MAX;
        }
        match mmap_scratch(total) {
            Some(ptr) => {
                scratch = ptr as i64;
                let mut base = 0u32;
                for cp in &mut p.calls {
                    if let Some(cp) = cp {
                        cp.set_base(base);
                        base = base.wrapping_add(cp.size);
                    }
                }
            }
            None => {
                if debug_on() {
                    eprintln!("jit: region {head}..={close}: scratch mmap failed");
                }
                return usize::MAX;
            }
        }
    }
    let mut em = Emitter::new(p, tape.to_vec(), scratch);
    em.emit();
    em.resolve();
    if debug_on() {
        eprintln!(
            "jit: region {head}..={close} compiled to {} bytes",
            em.code.len()
        );
        eprintln!(
            "jit: bytes {}",
            em.code
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join("")
        );
    }
    match place(&em.code) {
        Some(p) => {
            TRACES.fetch_add(1, Ordering::Relaxed);
            p as usize
        }
        None => usize::MAX,
    }
}

/// The `take_jump!` hook: `at` took a jump BACK to `target` (the caller
/// tested it; only then is this reached, and only when the tape has
/// tables). Returns the `ip` to continue at: `target` when the jump
/// stays interpreted, else what the compiled loop ran and where it
/// asked to resume.
///
/// The steady-state shapes stay inlined: a disarmed cell (`usize::MAX`,
/// the loop was refused or a guard failed) costs a load and a compare,
/// an armed one a call into the compiled code. Only counting toward
/// `HOT_LIMIT` and compiling go out of line.
///
/// # Safety
/// `at`/`target` point into the running tape described by `ctx`, with
/// `target <= at`, and `ctx.alive()`.
// Not inlined: `take_jump!`'s pointer test already keeps the JIT-off
// back edge at two instructions, and an inlined copy of this body at
// every jump site in `run_core` costs the interpreted path registers
// (measured: +210M instructions on `fib`).
#[inline(never)]
pub(super) unsafe fn back_edge(
    st: St,
    ctx: &JitCtx,
    at: *const Inst,
    target: *const Inst,
) -> *const Inst {
    // A guard failure or refused compile disarms the current frame's
    // context (`disarm`), but its pointer in `St` stays: re-check here.
    if !ctx.alive() {
        return target;
    }
    // SAFETY: guaranteed by the caller.
    let idx = unsafe { at.offset_from(ctx.tbase) } as usize;
    // SAFETY: the cells cover the whole tape (`Tables::new(tape.len())`).
    let cell = unsafe { &*ctx.jtr.get().add(idx) };
    let c = cell.load(Ordering::Relaxed);
    // 0 and the small values (refused that many times, counting toward
    // another attempt) are not code: `edge_count` below.
    if c >= MAX_ATTEMPTS {
        if c == usize::MAX {
            return target;
        }
        // Run the loop in machine code until it exits, and resume where
        // it says.  A negative return means an entry guard failed: the
        // assumption the trace was built on no longer holds, so disarm
        // the cell (this version of the tape stays interpreted) and
        // resume at `-(r + 1)`.  A non-negative return is a poll or the
        // iteration cap: the trace stays armed.
        let f: LoopFn = unsafe { std::mem::transmute(c) };
        if debug_on() {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let k = N.fetch_add(1, Ordering::Relaxed);
            if k < 12 {
                let f = unsafe { &*st.f };
                let d: Vec<(u8, i64)> = (0..f.cvs.len().min(8))
                    .map(|i| unsafe {
                        let p = st.cvs.add(i * 16);
                        (*p, std::ptr::read_unaligned(p.add(8).cast::<i64>()))
                    })
                    .collect();
                eprintln!("jit: ENTER at={idx} cv={d:?}");
            }
        }
        // SAFETY: the region only touches the frame's slots, and exits
        // leave them exactly as an interpreted run would.
        ctx.poll_row.set(std::ptr::null());
        let ret = unsafe { f(st.cvs, st.tmps, st.f.cast::<u8>()) };
        let r = ret.r;
        if debug_on() {
            static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let k = RUNS.fetch_add(1, Ordering::Relaxed);
            if k < 60 || k % 100_000 == 0 {
                eprintln!("jit: trace run #{k} at={idx} r={r}");
            }
            if k < 12 {
                let f = unsafe { &*st.f };
                let d: Vec<(u8, i64)> = (0..f.cvs.len().min(8))
                    .map(|i| unsafe {
                        let p = st.cvs.add(i * 16);
                        (*p, std::ptr::read_unaligned(p.add(8).cast::<i64>()))
                    })
                    .collect();
                eprintln!("jit:   cvs after run: {d:?}");
            }
        }
        if r < -REPLAN {
            // The version guard: what the plan pinned is stale. Forget
            // the trace (its pages stay, as every trace's do, within the
            // process's cap) and count the loop toward a new plan.
            if debug_on() {
                eprintln!(
                    "jit: trace at {idx} is stale (the function table changed): planning again"
                );
            }
            cell.store(0, Ordering::Release);
            // SAFETY: the counters cover the whole tape, and the context
            // is alive (tested above).
            unsafe { (*ctx.jcnt.get().add(idx)).store(0, Ordering::Relaxed) };
            let r = (-(r + REPLAN) - 1).clamp(0, (ctx.tlen - 1) as i64) as usize;
            return unsafe { ctx.tbase.add(r) };
        }
        if r < 0 {
            if debug_on() {
                // Which guard's slot broke the assumption: dump every tag.
                // The tag byte is the first byte of each slot's `Value`.
                let f = unsafe { &*st.f };
                let t: Vec<u8> = (0..f.cvs.len())
                    .map(|i| unsafe { *(st.cvs.add(i * 16)) })
                    .collect();
                eprintln!("jit:   cv tags = {t:?}");
                let t: Vec<u8> = (0..f.tmps.len())
                    .map(|i| unsafe { *(st.tmps.add(i * 16)) })
                    .collect();
                eprintln!("jit:   tmp tags = {t:?}");
            }
            cell.store(usize::MAX, Ordering::Release);
            // This tape's tracing never paid off: stop hooking its back
            // edges, both for frames entered from now on and (by nulling
            // this frame's table pointer) right here.  A guard failure
            // also raises the safe point: the frame leaves `run_core`
            // through the jump's own poll check and re-enters with a
            // fresh (dead) context, so the remaining iterations pay the
            // hook's two instructions and no call at all (the pointer
            // in `St` is register-local and cannot be nulled here).
            if !ctx.tape.is_null() && unsafe { (*ctx.tape).jit_bad() } {
                ctx.disarm();
                crate::interrupt::raise_safe_point();
            }
            let r = (-(r + 1)).clamp(0, (ctx.tlen - 1) as i64) as usize;
            return unsafe { ctx.tbase.add(r) };
        }
        // The instruction to resume at, and the row of a poll exit (plus
        // one) above it: `take_jump!` reports the safe point at that row.
        let resume = ((r & 0xFFFF_FFFF) as usize).min(ctx.tlen - 1);
        let poll_row = (r >> 32) as usize;
        if poll_row != 0 {
            // SAFETY: a row of this tape (clamped to it).
            ctx.poll_row
                .set(unsafe { ctx.tbase.add((poll_row - 1).min(ctx.tlen - 1)) });
        }
        // The cell's counter, free since the loop was compiled: entries
        // in a row that a row left during their first iteration.
        // SAFETY: the counters cover the whole tape, and the context is
        // alive (tested above).
        let exits = unsafe { &*ctx.jcnt.get().add(idx) };
        if ret.iters == 1 {
            if exits.fetch_add(1, Ordering::Relaxed) + 1 >= UNPRODUCTIVE_LIMIT {
                if debug_on() {
                    eprintln!(
                        "jit: trace at {idx} switched off: it keeps leaving in its first iteration"
                    );
                }
                cell.store(usize::MAX, Ordering::Release);
                if !ctx.tape.is_null() && unsafe { (*ctx.tape).jit_bad() } {
                    ctx.disarm();
                    crate::interrupt::raise_safe_point();
                }
            }
        } else {
            exits.store(0, Ordering::Relaxed);
            if !ctx.tape.is_null() {
                unsafe { (*ctx.tape).jit_good() };
            }
        }
        return unsafe { ctx.tbase.add(resume) };
    }
    // SAFETY: as above.
    unsafe { edge_count(st, ctx, at, target, idx) }
}

/// Not hot yet (count toward `HOT_LIMIT`), or hot and not yet compiled
/// (try to compile, then run or mark refused). Out of line so the
/// steady-state hook stays tiny.
#[cold]
unsafe fn edge_count(
    st: St,
    ctx: &JitCtx,
    at: *const Inst,
    target: *const Inst,
    idx: usize,
) -> *const Inst {
    if debug_on() {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let k = N.fetch_add(1, Ordering::Relaxed);
        if k < 40 || k % 1_000_000 == 0 {
            let head = unsafe { target.offset_from(ctx.tbase) };
            eprintln!("jit: count #{k} at={idx} head={head} tlen={}", ctx.tlen);
        }
    }
    let head = unsafe { target.offset_from(ctx.tbase) } as usize;
    if idx >= ctx.tlen || head > idx {
        return target;
    }
    // SAFETY: the counters cover the whole tape.
    let n = unsafe { (*ctx.jcnt.get().add(idx)).fetch_add(1, Ordering::Relaxed) };
    if n + 1 < hot_limit() {
        return target;
    }
    let mut compiled = attempt(&st, ctx, head, idx);
    // SAFETY: as above.
    let cell = unsafe { &*ctx.jtr.get().add(idx) };
    if compiled == REFUSED_FOR_NOW {
        // The cell holds the refusals so far (below `MAX_ATTEMPTS`).
        let tries = cell.load(Ordering::Relaxed) + 1;
        if tries < MAX_ATTEMPTS {
            cell.store(tries, Ordering::Release);
            // Count again, eight times as long after each refusal.
            let hot = hot_limit();
            #[allow(clippy::cast_possible_truncation)] // below `MAX_ATTEMPTS`
            let wait = hot.saturating_mul(8i32.saturating_pow(tries as u32));
            // SAFETY: the counters cover the whole tape.
            unsafe {
                (*ctx.jcnt.get().add(idx)).store(hot.saturating_sub(wait), Ordering::Relaxed)
            };
            return target;
        }
        compiled = usize::MAX;
    }
    cell.store(compiled, Ordering::Release);
    if compiled == usize::MAX {
        // The region can't be traced (and never could): if this tape has
        // no useful trace yet, stop hooking its back edges, for frames
        // entered from now on and (by disarming this frame's context)
        // right here.
        if !ctx.tape.is_null() && unsafe { (*ctx.tape).jit_bad() } {
            ctx.disarm();
            crate::interrupt::raise_safe_point();
        }
        target
    } else {
        // The counter now counts the trace's unproductive entries.
        // SAFETY: as above; now the cell is armed.
        unsafe {
            (*ctx.jcnt.get().add(idx)).store(0, Ordering::Relaxed);
            back_edge(st, ctx, at, target)
        }
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    /// The receiver guard compares the raw `Rc<Class>` field word of a
    /// live object against `class_addr - RC_VALUE_OFFSET`, so the inner
    /// box's prefix (the strong and weak count words before the value)
    /// must be exactly `RC_VALUE_OFFSET` bytes.
    #[test]
    fn rc_inner_prefix_size() {
        struct Holder {
            class: std::rc::Rc<std::cell::Cell<u64>>,
        }
        let h = Holder {
            class: std::rc::Rc::new(std::cell::Cell::new(7)),
        };
        // The raw field word: the `NonNull<RcInner<T>>` `std::rc::Rc`
        // stores, i.e. the allocation base.
        let raw = unsafe { *(std::ptr::addr_of!(h.class) as *const usize) };
        let value = std::rc::Rc::as_ptr(&h.class) as usize;
        assert_eq!(value - raw, usize::try_from(RC_VALUE_OFFSET).unwrap());
    }

    /// The slot layout every emitted row assumes: the tag byte at offset
    /// 0 with these values, an `Int`'s payload word at `PAYLOAD`, a
    /// `Bool`'s value in the byte right after the tag (`BOOL_BYTE`, *not*
    /// the payload word: a row that wrote the payload word left the
    /// previous bool in place), and `None` reading as `TAG_NONE`.
    #[test]
    fn value_layout() {
        use php_runtime::Value;
        assert_eq!(std::mem::size_of::<Value>(), 16);
        assert_eq!(std::mem::size_of::<Option<Value>>(), 16);
        let tag = |v: &Value| unsafe { *std::ptr::from_ref(v).cast::<u8>() };
        assert_eq!(tag(&Value::Null), TAG_NULL);
        assert_eq!(tag(&Value::Bool(true)), TAG_BOOL);
        assert_eq!(tag(&Value::Int(1)), TAG_INT);
        assert_eq!(tag(&Value::Str(php_runtime::ZStr::from("x"))), TAG_STR);
        let i = Value::Int(0x0102_0304_0506_0708);
        let word = unsafe {
            std::ptr::read_unaligned(
                std::ptr::from_ref(&i)
                    .cast::<u8>()
                    .add(PAYLOAD as usize)
                    .cast::<i64>(),
            )
        };
        assert_eq!(word, 0x0102_0304_0506_0708);
        for b in [false, true] {
            let v = Value::Bool(b);
            let byte = unsafe { *std::ptr::from_ref(&v).cast::<u8>().add(BOOL_BYTE as usize) };
            assert_eq!(byte, u8::from(b));
        }
        let none: Option<Value> = None;
        assert_eq!(unsafe { *std::ptr::from_ref(&none).cast::<u8>() }, TAG_NONE);
    }

    /// A temporary is what its last store made it: re-stored as an `Int`
    /// it is no `Bool` any more (a stale entry would tag an int `Bool`
    /// when it is copied into a variable), and a consumed one is in no
    /// list.
    #[test]
    fn tmp_kinds_replace_each_other() {
        let (mut defs, mut bools, mut nulls) = (Vec::new(), Vec::new(), Vec::new());
        set_tmp_kind(&mut defs, &mut bools, &mut nulls, 16, TmpKind::Bool);
        assert_eq!((defs.len(), bools.len(), nulls.len()), (0, 1, 0));
        set_tmp_kind(&mut defs, &mut bools, &mut nulls, 16, TmpKind::Int);
        assert_eq!((defs.len(), bools.len(), nulls.len()), (1, 0, 0));
        set_tmp_kind(&mut defs, &mut bools, &mut nulls, 16, TmpKind::Null);
        assert_eq!((defs.len(), bools.len(), nulls.len()), (0, 0, 1));
        set_tmp_kind(&mut defs, &mut bools, &mut nulls, 32, TmpKind::Int);
        set_tmp_kind(&mut defs, &mut bools, &mut nulls, 16, TmpKind::Unknown);
        assert_eq!((defs.clone(), bools.len(), nulls.len()), (vec![32], 0, 0));
    }

    /// The `Bool` value byte moves through `cl`: `mov byte [base + d32],
    /// cl` (`88 /r`, `reg = 001`) and `movzx ecx, byte [base + d32]`
    /// (`0F B6 /r`), with `rdi` (`rm = 111`) for a compiled variable and
    /// `rsi` (`rm = 110`) for a temporary, and no REX prefix (one would
    /// rename the base or the byte register).
    #[test]
    fn bool_byte_encodings() {
        let mut em = emitter();
        em.store_cl_base(KT, 0x21);
        assert_eq!(em.code, vec![0x88, 0x8E, 0x21, 0x00, 0x00, 0x00]);
        em.code.clear();
        em.store_cl_base(KC, 0x11);
        assert_eq!(em.code, vec![0x88, 0x8F, 0x11, 0x00, 0x00, 0x00]);
        em.code.clear();
        em.load_cl_base(KT, 0x21);
        assert_eq!(em.code, vec![0x0F, 0xB6, 0x8E, 0x21, 0x00, 0x00, 0x00]);
        em.code.clear();
        // `cmp byte [rax + d32], 0`: the `empty()` test of a `Bool` slot.
        em.cmpb_rax(0x31, 0);
        assert_eq!(em.code, vec![0x80, 0xB8, 0x31, 0x00, 0x00, 0x00, 0x00]);
    }

    fn emitter() -> Emitter {
        Emitter::new(
            Plan {
                head: 0,
                close: 0,
                class: Vec::new(),
                meta: Vec::new(),
                guards: Vec::new(),
                entries: Vec::new(),
                span: Vec::new(),
                calls: Vec::new(),
                has_calls: false,
                frame: false,
                version_guard: false,
                version: 0,
            },
            Vec::new(),
            0,
        )
    }

    /// The receiver guards run on machine code we cannot run here, so
    /// pin their exact encodings: a wrong REX bit silently renames a
    /// register (`mov r10, rdx` without `REX.W` is `mov r10d, r10d`;
    /// `4D 8B 91` loads `r10`, not `r9`) and the trace crashes.
    #[test]
    fn receiver_guard_encodings() {
        let mut em = emitter();
        em.load_r9_frame(0x22);
        assert_eq!(em.code, vec![0x4D, 0x8B, 0x8A, 0x22, 0x00, 0x00, 0x00]);
        em.code.clear();
        em.put(&[REX_W | REX_R | REX_B, 0x85, 0xC9]); // test r9, r9
        assert_eq!(em.code, vec![0x4D, 0x85, 0xC9]);
        em.code.clear();
        em.obj_class_from_r9();
        let mut want = vec![0x4D, 0x8B, 0x89];
        want.extend_from_slice(&OBJ_PAYLOAD.wrapping_add(CLASS_FIELD).to_le_bytes());
        assert_eq!(em.code, want);
        em.code.clear();
        em.movabs_rax(-16);
        let mut want = vec![0x48, 0xB8];
        want.extend_from_slice(&(-16i64).to_le_bytes());
        assert_eq!(em.code, want);
        em.code.clear();
        em.cmp_rax_r9();
        // `REX.R` widens the `reg` field to `r9`: without it this would
        // be `cmp rax, rcx`.
        assert_eq!(em.code, vec![REX_W | REX_R, 0x39, 0xC8]);
    }

    /// The prologue parks the frame in `r10` with `mov r10, rdx`
    /// (`4C 8B D2`, the `8B` reg←r/m form). The `89` form moves the
    /// other way: `4C 89 D2` is `mov rdx, r10`, which clobbers the
    /// frame argument and leaves `r10` holding the entry value.
    #[test]
    fn frame_park_encoding() {
        assert_eq!(vec![REX_W | REX_R, 0x8B, 0xD2], vec![0x4C, 0x8B, 0xD2]);
    }

    /// `emit_prop` reads the slot array through fixed displacements, so
    /// the `Slots` fields must sit where `SLOTS_*_OFF` says (and where
    /// an `Option<Value>` slice's words actually live).
    #[test]
    fn slots_field_offsets() {
        assert_eq!(crate::object::SLOTS_BORROW_OFF, 0);
        assert_eq!(crate::object::SLOTS_FIRST_OFF, 8);
        assert_eq!(crate::object::SLOTS_LEN_OFF, 16);
    }

    /// The `lazy` guards compare the two words of an inline
    /// `RefCell<Option<Box<T>>>`: the borrow state at 0 and the option's
    /// pointer at 8 (its null niche is `None`), like `REFCELL_*`.
    #[test]
    fn refcell_words() {
        let cell = std::cell::RefCell::new(Some(Box::new(7u32)));
        let raw = std::ptr::addr_of!(cell) as usize;
        // Unborrowed: the borrow flag word is 0.
        assert_eq!(unsafe { *(raw as *const usize) }, 0);
        // `as_ptr` names the value: 8 bytes past the cell's base.
        assert_eq!(
            cell.as_ptr() as usize - raw,
            usize::try_from(REFCELL_VALUE).unwrap()
        );
        let empty: std::cell::RefCell<Option<Box<u32>>> = std::cell::RefCell::new(None);
        assert_eq!(
            unsafe { *((std::ptr::addr_of!(empty) as usize + 8) as *const usize) },
            0
        );
    }

    /// `emit_prop`'s `$this` guard treats a null field word as "no
    /// `$this`" and its scope guard as "no scope": both fields are
    /// `Option`ed pointers whose niche must make `None` null.
    #[test]
    fn option_pointers_are_null() {
        assert_eq!(
            std::mem::size_of::<Option<std::rc::Rc<std::cell::Cell<u64>>>>(),
            8
        );
        assert_eq!(std::mem::size_of::<Option<php_runtime::Obj>>(), 8);
    }

    /// Pin the property-read guards' encodings, same reasoning as
    /// `receiver_guard_encodings`: a wrong REX bit renames a register,
    /// narrows the access or flips the compared operands, and the trace
    /// crashes or miscompares.
    #[test]
    fn prop_guard_encodings() {
        let mut em = emitter();
        // `mov rax, [r10 + disp32]`: a frame field (`REX.W|REX.B`: the
        // `reg` field stays `rax`, `rm` widened to `r10`).
        em.load_rax_frame(0x22);
        assert_eq!(em.code, vec![0x49, 0x8B, 0x82, 0x22, 0x00, 0x00, 0x00]);
        em.code.clear();
        // `mov rax, [r9 + disp32]`: an `ObjectData` field word.
        em.mov_rax_r9(0x18);
        assert_eq!(em.code, vec![0x49, 0x8B, 0x81, 0x18, 0x00, 0x00, 0x00]);
        em.code.clear();
        // `mov rax, [rax + disp32]`: the slot array, dereferenced.
        em.mov_rax_rax(0x30);
        assert_eq!(em.code, vec![0x48, 0x8B, 0x80, 0x30, 0x00, 0x00, 0x00]);
        em.code.clear();
        // `movabs rcx, imm64`: the second compare constant.
        em.movabs_rcx(-16);
        let mut want = vec![0x48, 0xB9];
        want.extend_from_slice(&(-16i64).to_le_bytes());
        assert_eq!(em.code, want);
        em.code.clear();
        // `cmp rax, rcx` (`3B /r`: `reg = rax` is the *destination*, so
        // this compares exactly like `cmp rax, [field]` vs `movabs`).
        em.cmp_rax_rcx();
        assert_eq!(em.code, vec![REX_W, 0x3B, 0xC1]);
        em.code.clear();
        // `test rax, rax`: the null-scope check.
        em.test_rax();
        assert_eq!(em.code, vec![REX_W, 0x85, 0xC0]);
        em.code.clear();
        // `cmp qword [r9 + disp32], imm8`: borrow and lazy-empty checks.
        em.cmpq_r9(0x40, 0);
        assert_eq!(
            em.code,
            vec![0x49, 0x83, 0xB9, 0x40, 0x00, 0x00, 0x00, 0x00]
        );
        em.code.clear();
        // `cmp rax, imm32`: the slot-array length check.
        em.cmp_rax_imm(0x140);
        assert_eq!(em.code, vec![REX_W, 0x3D, 0x40, 0x01, 0x00, 0x00]);
        em.code.clear();
        // `cmp byte [rax + disp32], imm8`: the slot's tag byte.
        em.cmpb_rax(0x50, TAG_INT);
        assert_eq!(em.code, vec![0x80, 0xB8, 0x50, 0x00, 0x00, 0x00, TAG_INT]);
        em.code.clear();
        // `mov rcx, [rsi + disp32]`: a source temporary's payload while
        // `rax` holds the slot array (`8B` with `reg` field `001`, no
        // `REX.R`: `rm=110` stays `rsi`).
        em.movrcx_at(KT, 0x28);
        assert_eq!(em.code, vec![REX_W, 0x8B, 0x8E, 0x28, 0x00, 0x00, 0x00]);
        em.code.clear();
        // `mov [rsi + off + 8], rcx`: the result temporary's payload.
        em.store_rcx_base(KT, 0x10);
        assert_eq!(em.code, vec![REX_W, 0x89, 0x8E, 0x18, 0x00, 0x00, 0x00]);
        em.code.clear();
        // `mov [rax + disp32], rcx`: the property slot's payload word.
        em.store_rcx_at_rax(0x48);
        assert_eq!(em.code, vec![REX_W, 0x89, 0x88, 0x48, 0x00, 0x00, 0x00]);
        em.code.clear();
        // `mov rcx, [rax + disp32]`: the slot payload while `rax` holds
        // the slot array.
        em.movrcx_at_rax(0x18);
        assert_eq!(em.code, vec![REX_W, 0x8B, 0x88, 0x18, 0x00, 0x00, 0x00]);
        em.code.clear();
        // `mov r9, [rax + disp32]`: the old payload into the scratch
        // register (`8B /r` names destination `r9` via `REX.R`, the
        // modrm `reg` field `001`; `rm = 000` is `rax`).
        em.mov_r9_rax(0x18);
        assert_eq!(em.code, vec![0x4C, 0x8B, 0x88, 0x18, 0x00, 0x00, 0x00]);
        em.code.clear();
        // `mov [rax + disp32], r9`: the slot payload from `r9` (`89 /r`
        // names *source* `r9` via `REX.R`, `rm = 000` is `rax`).
        em.store_r9_at_rax(0x18);
        assert_eq!(em.code, vec![0x4C, 0x89, 0x88, 0x18, 0x00, 0x00, 0x00]);
        em.code.clear();
        // `mov r9, rcx`: keep the old value across the increment.
        em.mov_r9_rcx();
        assert_eq!(em.code, vec![0x4C, 0x8B, 0xC9]);
        em.code.clear();
        // `mov ecx, imm32`: the `isset`/`empty` result (and the zero
        // the `setcc` writes into) without clobbering the flags.
        em.mov_ecx(1);
        assert_eq!(em.code, vec![0xB9, 0x01, 0x00, 0x00, 0x00]);
    }

    /// The `C_PROPINC`/`C_PROPOPA` arithmetic words: `add`/`sub` with
    /// the *destination* `r9` named through `rm` (`REX.B`) and `imul`
    /// with it named through `reg` (`REX.R`) — swapped REX bits silently
    /// compute into `rcx` and leave the slot untouched.
    #[test]
    fn prop_arith_encodings() {
        let mut em = emitter();
        // `add rcx, 1` / `sub rcx, 1` (`83 /0`, `/5`).
        em.put(&[REX_W, 0x83, 0xC1, 0x01]);
        assert_eq!(em.code, vec![0x48, 0x83, 0xC1, 0x01]);
        em.code.clear();
        em.put(&[REX_W, 0x83, 0xE9, 0x01]);
        assert_eq!(em.code, vec![0x48, 0x83, 0xE9, 0x01]);
        em.code.clear();
        // `add r9, rcx`, `sub r9, rcx`, `and`/`or`/`xor r9, rcx`
        // (`01`/`29`/`21`/`09`/`31 /r`: `rm = 001` widened to `r9` is
        // the destination, `reg = 001` (`rcx`) the source).
        for op in [0x01u8, 0x29, 0x21, 0x09, 0x31] {
            em.put(&[REX_W | REX_B, op, 0xC9]);
            assert_eq!(em.code, vec![0x49, op, 0xC9]);
            em.code.clear();
        }
        // `imul r9, rcx` (`0F AF /r`: `reg = r9` is the *destination*).
        em.put(&[REX_W | REX_R, 0x0F, 0xAF, 0xC9]);
        assert_eq!(em.code, vec![0x4C, 0x0F, 0xAF, 0xC9]);
        em.code.clear();
        // `test rcx, rcx` / `test cl, cl` and `sete cl`: the `empty()`
        // zero tests (`mov ecx` between them keeps the flags).
        em.put(&[REX_W, 0x85, 0xC9]);
        em.put(&[0xB9, 0, 0, 0, 0]);
        em.put(&[0x0F, 0x94, 0xC1]);
        assert_eq!(
            em.code,
            vec![0x48, 0x85, 0xC9, 0xB9, 0, 0, 0, 0, 0x0F, 0x94, 0xC1]
        );
    }

    /// A slot's guard requirements merge: an exact guard wins, and the
    /// "defined" of a dropped send joins the "plain" of a store into the
    /// same variable (together: a scalar).
    #[test]
    fn guard_requirements_merge() {
        let mut g: Vec<Guard> = Vec::new();
        assert!(add_defined(&mut g, 32));
        assert_eq!((g[0].exact, g[0].plain, g[0].defined), (false, false, true));
        assert!(add_slot(&mut g, KC, 32, false));
        assert_eq!((g[0].exact, g[0].plain, g[0].defined), (false, true, true));
        assert!(add_slot(&mut g, KC, 32, true));
        assert!(g[0].exact);
        assert_eq!(g.len(), 1);
        // A temporary at the same offset is another slot, and a store
        // into it asks for nothing but "plain".
        assert!(add_slot(&mut g, KT, 32, false));
        assert_eq!(g.len(), 2);
        assert_eq!((g[1].exact, g[1].plain, g[1].defined), (false, true, false));
    }

    /// Only a backward row of a jump class the plan accepts as a region's
    /// close makes a tape worth tables.
    #[test]
    fn loop_close_detection() {
        let forward = Inst {
            c: 64,
            ..Inst::plain(TOp::Jmp)
        };
        let backward = Inst {
            c: (-64i32) as u32,
            ..Inst::plain(TOp::Jmp)
        };
        // A row that is no jump, whose `c` only looks negative.
        let other = Inst {
            c: u32::MAX,
            ..Inst::plain(TOp::AssignI)
        };
        assert!(!has_loop_close(&[]));
        assert!(!has_loop_close(&[forward, other]));
        assert!(has_loop_close(&[forward, other, backward]));
        // An instruction that jumps onto itself: an empty loop.
        let itself = Inst::plain(TOp::Jmp);
        assert!(has_loop_close(&[forward, itself]));
    }

    /// The address of a `Value` as a helper's slot argument.
    fn at(v: &php_runtime::Value) -> *const u8 {
        std::ptr::from_ref(v).cast()
    }

    /// The array a test's variable slot holds.
    fn held(slot: &crate::vm::CvSlot) -> &php_runtime::ZArray {
        match slot.get() {
            Some(php_runtime::Value::Array(a)) => a,
            _ => panic!("the slot holds no array"),
        }
    }

    /// `jit_dim_get`: an `Int` element is stored into a temporary or a
    /// plain variable; everything else declines and changes nothing.
    #[test]
    fn dim_get_helper() {
        use crate::vm::CvSlot;
        use php_runtime::{Value, ZArray};
        let arr = CvSlot::new(Some(Value::Array(ZArray::from_values([
            Value::Int(10),
            Value::Int(20),
            Value::Float(1.5),
        ]))));
        let p = std::ptr::from_ref(&arr).cast_mut();
        // The temporary behind a cell: the helper writes it through the
        // pointer, as the trace's slots are written.
        let tmp = std::cell::UnsafeCell::new(Value::Null);
        let t = tmp.get().cast::<u8>();
        let tmp_is = |want: i64| matches!(unsafe { &*tmp.get() }, Value::Int(v) if *v == want);
        // An `Int` element into a temporary.
        assert_eq!(unsafe { jit_dim_get(p, at(&Value::Int(1)), t, 0) }, 0);
        assert!(tmp_is(20));
        // A float element, a missing key, an offset that is no int.
        for k in [Value::Int(2), Value::Int(7), Value::Null, Value::Float(1.0)] {
            assert_eq!(unsafe { jit_dim_get(p, at(&k), t, 0) }, 1);
            assert!(tmp_is(20));
        }
        // Into a variable: undefined, or holding a plain value.
        let to = CvSlot::new(None);
        let d = std::ptr::from_ref(&to).cast_mut().cast::<u8>();
        assert_eq!(unsafe { jit_dim_get(p, at(&Value::Int(0)), d, 1) }, 0);
        assert!(matches!(to.get(), Some(Value::Int(10))));
        // A variable holding an array is not overwritten here.
        let busy = CvSlot::new(Some(Value::Array(ZArray::from_values([Value::Int(1)]))));
        let b = std::ptr::from_ref(&busy).cast_mut().cast::<u8>();
        assert_eq!(unsafe { jit_dim_get(p, at(&Value::Int(0)), b, 1) }, 1);
        assert!(matches!(busy.get(), Some(Value::Array(_))));
        // A hash table's int keys are found too.
        let hash = CvSlot::new(Some(Value::Array(ZArray::from_pairs([
            (9i64, Value::Int(90)),
            (2i64, Value::Int(22)),
        ]))));
        let h = std::ptr::from_ref(&hash).cast_mut();
        assert_eq!(unsafe { jit_dim_get(h, at(&Value::Int(2)), t, 0) }, 0);
        assert!(tmp_is(22));
        // String keys: a key of the table, a numeric string naming an int
        // key, and one the table does not have.
        let named = CvSlot::new(Some(Value::Array(ZArray::from_pairs([
            (php_runtime::ArrayKey::from("beta"), Value::Int(2)),
            (php_runtime::ArrayKey::Int(7), Value::Int(70)),
        ]))));
        let np = std::ptr::from_ref(&named).cast_mut();
        let key = |s: &str| Value::Str(php_runtime::ZStr::from(s));
        assert_eq!(unsafe { jit_dim_get(np, at(&key("beta")), t, 0) }, 0);
        assert!(tmp_is(2));
        assert_eq!(unsafe { jit_dim_get(np, at(&key("7")), t, 0) }, 0);
        assert!(tmp_is(70));
        assert_eq!(unsafe { jit_dim_get(np, at(&key("gamma")), t, 0) }, 1);
        assert!(tmp_is(70));
        // A key the tape knows to be no integer key is looked up as it is.
        assert_eq!(
            unsafe { jit_dim_get(np, at(&key("beta")), t, DIM_GET_EXACT_KEY) },
            0
        );
        assert!(tmp_is(2));
        // A string the destination holds is released, not leaked.
        let text = CvSlot::new(Some(key("old")));
        let tp = std::ptr::from_ref(&text).cast_mut().cast::<u8>();
        assert_eq!(
            unsafe { jit_dim_get(np, at(&key("beta")), tp, DIM_GET_TO_CV) },
            0
        );
        assert!(matches!(text.get(), Some(Value::Int(2))));
        // No array in the variable at all.
        let none = CvSlot::new(None);
        let n = std::ptr::from_ref(&none).cast_mut();
        assert_eq!(unsafe { jit_dim_get(n, at(&Value::Int(0)), t, 0) }, 1);
    }

    /// `jit_dim_set`: writes an `Int` over a plain element, at a new key
    /// or at the end of an array held alone; declines a shared array (a
    /// write must separate it first), a value that is no int and an
    /// element that owns something.
    #[test]
    fn dim_set_helper() {
        use crate::vm::CvSlot;
        use php_runtime::{Value, ZArray};
        let arr = CvSlot::new(Some(Value::Array(ZArray::from_values([
            Value::Int(1),
            Value::Int(2),
        ]))));
        let p = std::ptr::from_ref(&arr).cast_mut();
        let null = std::ptr::null();
        assert_eq!(
            unsafe { jit_dim_set(p, at(&Value::Int(0)), at(&Value::Int(7)), 0) },
            0
        );
        assert_eq!(unsafe { jit_dim_set(p, null, at(&Value::Int(8)), 0) }, 0);
        assert_eq!(
            unsafe { jit_dim_set(p, at(&Value::Int(40)), at(&Value::Int(9)), 0) },
            0
        );
        let a = held(&arr);
        assert_eq!(a.len(), 4);
        assert!(matches!(a.get_int(0), Some(Value::Int(7))));
        assert!(matches!(a.get_int(1), Some(Value::Int(2))));
        assert!(matches!(a.get_int(2), Some(Value::Int(8))));
        assert!(matches!(a.get_int(40), Some(Value::Int(9))));
        // A value or an offset that is no int.
        assert_eq!(
            unsafe { jit_dim_set(p, at(&Value::Int(0)), at(&Value::Float(1.0)), 0) },
            1
        );
        assert_eq!(
            unsafe { jit_dim_set(p, at(&Value::Null), at(&Value::Int(1)), 0) },
            1
        );
        assert!(matches!(held(&arr).get_int(0), Some(Value::Int(7))));
        // A second handle on the table: the write must separate first.
        let other = held(&arr).clone();
        assert_eq!(
            unsafe { jit_dim_set(p, at(&Value::Int(0)), at(&Value::Int(5)), 0) },
            1
        );
        assert_eq!(unsafe { jit_dim_set(p, null, at(&Value::Int(5)), 0) }, 1);
        assert!(matches!(other.get_int(0), Some(Value::Int(7))));
        assert_eq!(other.len(), 4);
        drop(other);
        // An element that owns something is not replaced here.
        let nested = CvSlot::new(Some(Value::Array(ZArray::from_values([Value::Array(
            ZArray::from_values([Value::Int(1)]),
        )]))));
        let n = std::ptr::from_ref(&nested).cast_mut();
        assert_eq!(
            unsafe { jit_dim_set(n, at(&Value::Int(0)), at(&Value::Int(5)), 0) },
            1
        );
        assert!(matches!(held(&nested).get_int(0), Some(Value::Array(_))));
    }

    /// `jit_dim_op`: the int-int operators on an `Int` element; an
    /// overflow, a missing key and an operator without a helper decline
    /// and leave the element as it was.
    #[test]
    fn dim_op_helper() {
        use crate::vm::CvSlot;
        use php_runtime::{Value, ZArray};
        let arr = CvSlot::new(Some(Value::Array(ZArray::from_values([
            Value::Int(6),
            Value::Int(i64::MAX),
        ]))));
        let p = std::ptr::from_ref(&arr).cast_mut();
        let k0 = Value::Int(0);
        let run = |op: u32, y: i64| unsafe { jit_dim_op(p, at(&k0), at(&Value::Int(y)), op) };
        let elem = |slot: &CvSlot| match held(slot).get_int(0) {
            Some(Value::Int(v)) => *v,
            _ => panic!("no int element"),
        };
        assert_eq!(run(DIM_OP_ADD, 4), 0);
        assert_eq!(elem(&arr), 10);
        assert_eq!(run(DIM_OP_SUB, 3), 0);
        assert_eq!(elem(&arr), 7);
        assert_eq!(run(DIM_OP_MUL, 6), 0);
        assert_eq!(elem(&arr), 42);
        assert_eq!(run(DIM_OP_OR, 5), 0);
        assert_eq!(elem(&arr), 47);
        assert_eq!(run(DIM_OP_AND, 0x2C), 0);
        assert_eq!(elem(&arr), 44);
        assert_eq!(run(DIM_OP_XOR, 4), 0);
        assert_eq!(elem(&arr), 40);
        // An operator without a helper (`dims.rs`'s code of `/`).
        assert_eq!(run(3, 2), 1);
        assert_eq!(elem(&arr), 40);
        // An overflow, and a missing key.
        let k1 = Value::Int(1);
        assert_eq!(
            unsafe { jit_dim_op(p, at(&k1), at(&Value::Int(1)), DIM_OP_ADD) },
            1
        );
        assert!(matches!(held(&arr).get_int(1), Some(Value::Int(i64::MAX))));
        let k9 = Value::Int(9);
        assert_eq!(
            unsafe { jit_dim_op(p, at(&k9), at(&Value::Int(1)), DIM_OP_ADD) },
            1
        );
        assert_eq!(held(&arr).len(), 2);
    }

    /// The helper call: the saves (five pushes, which also align the
    /// stack for the call), the arguments computed off the slot bases
    /// before those registers are overwritten, the call through `rax`,
    /// the restores in reverse order, and the exit when it declined.
    #[test]
    fn helper_call_encoding() {
        let mut em = emitter();
        em.emit_helper(
            0x1122_3344_5566_7788,
            [HArg::Slot(KC, 0x10), HArg::Null, HArg::Slot(KT, 0x20)],
            7,
            Target::Side(3),
        );
        #[rustfmt::skip]
        let want: Vec<u8> = vec![
            0x57, 0x56, 0x41, 0x50, 0x41, 0x52, 0x41, 0x53, // push rdi, rsi, r8, r10, r11
            0x48, 0x8D, 0x87, 0x10, 0, 0, 0,                // lea rax, [rdi + 0x10]
            0x31, 0xC9,                                     // xor ecx, ecx
            0x48, 0x8D, 0x96, 0x20, 0, 0, 0,                // lea rdx, [rsi + 0x20]
            0x48, 0x89, 0xC7,                               // mov rdi, rax
            0x48, 0x89, 0xCE,                               // mov rsi, rcx
            0xB9, 7, 0, 0, 0,                               // mov ecx, 7
            0x48, 0xB8, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, // movabs rax
            0xFF, 0xD0,                                     // call rax
            0x41, 0x5B, 0x41, 0x5A, 0x41, 0x58, 0x5E, 0x5F, // pop r11, r10, r8, rsi, rdi
            0x85, 0xC0,                                     // test eax, eax
            0x0F, 0x85, 0, 0, 0, 0,                         // jnz (patched by `resolve`)
        ];
        assert_eq!(em.code, want);
        // An address argument per register, and a slot off the temporaries.
        em.code.clear();
        em.fixups.clear();
        em.helper_arg(1, HArg::Slot(KT, 0x30)); // lea rcx, [rsi + 0x30]
        em.helper_arg(2, HArg::Null); // xor edx, edx
        em.helper_arg(0, HArg::Null); // xor eax, eax
        assert_eq!(
            em.code,
            vec![0x48, 0x8D, 0x8E, 0x30, 0, 0, 0, 0x31, 0xD2, 0x31, 0xC0]
        );
    }

    /// The exit stubs: what each kind returns in `rax` and `rdx`.
    #[test]
    fn exit_stub_encodings() {
        let mut em = emitter();
        em.jcc(0x85, Target::Side(5));
        em.jcc(0x85, Target::Stub(6));
        em.jcc(0x85, Target::Poll(7, 9));
        em.jcc(0x85, Target::Disarm(2));
        em.jcc(0x85, Target::Side(5)); // a second use shares the stub
        em.resolve();
        // Five `jcc rel32` (6 bytes each), then the stubs in first-use order.
        let c = &em.code;
        // `Side`: the resume index, and the iteration count (`r8d`) in `edx`.
        assert_eq!(c[30..39], [0xB8, 5, 0, 0, 0, 0x44, 0x89, 0xC2, 0xC3]);
        // `Stub`: the resume index, `edx` zero.
        assert_eq!(c[39..47], [0xB8, 6, 0, 0, 0, 0x31, 0xD2, 0xC3]);
        // `Poll`: resume 7 below, row 9 plus one above; `edx` zero.
        assert_eq!(
            c[47..60],
            [0x48, 0xB8, 7, 0, 0, 0, 10, 0, 0, 0, 0x31, 0xD2, 0xC3]
        );
        assert_eq!(poll_word(7, 9), (10 << 32) | 7);
        // A stale trace's return is told from a disarm's `-(idx + 1)`.
        assert!(replan_word(0) < -REPLAN && replan_word(5) < replan_word(0));
        assert_eq!(-(replan_word(5) + REPLAN) - 1, 5);
        // `Disarm`: `-(2 + 1)`.
        assert_eq!(
            c[60..71],
            [0x48, 0xB8, 0xFD, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xC3]
        );
        assert_eq!(c.len(), 71);
        // The first and the last branch both land on the `Side` stub.
        assert_eq!(c[2..6], 24i32.to_le_bytes());
        assert_eq!(c[26..30], 0i32.to_le_bytes());
    }

    /// A function compiled whole: the C-ABI entry stub (the saves, the
    /// stack pointer to return to, the stack floor, the poll address, the
    /// argument shuffle, the call of the body, the two ways out) and the
    /// body's prologue and `return`, for `function f($a) { return $a; }`.
    #[test]
    fn native_function_encoding() {
        let ret = NRow::Ret(Arg {
            kind: KC,
            off: 0,
            lit: 0,
        });
        let plan = NPlan {
            rows: vec![Some(ret)],
            ncvs: 1,
            ntmps: 1,
            nparams: 1,
            tape: vec![Inst::plain(TOp::Exit), Inst::plain(TOp::Exit)],
        };
        let (code, body) = emit_native(&plan);
        let poll = (crate::interrupt::jit_poll_addr() as i64).to_le_bytes();
        #[rustfmt::skip]
        let mut want: Vec<u8> = vec![
            0x53, 0x41, 0x54, 0x41, 0x55,                   // push rbx; push r12; push r13
            0x48, 0x89, 0xE3,                               // mov rbx, rsp
            0x4C, 0x8D, 0xA4, 0x24, 0x00, 0x00, 0xF8, 0xFF, // lea r12, [rsp - 512K]
            0x49, 0xBD,                                     // movabs r13, poll
        ];
        want.extend_from_slice(&poll);
        #[rustfmt::skip]
        want.extend_from_slice(&[
            0x4D, 0x89, 0xCA, 0x49, 0x89, 0xD1, 0x48, 0x89, 0xCA, // r10<-r9, r9<-rdx, rdx<-rcx
            0x48, 0x89, 0xF1, 0x48, 0x89, 0xF8,             // rcx<-rsi, rax<-rdi
            0xE8, 34, 0, 0, 0,                              // call body
            0x31, 0xD2,                                     // xor edx, edx
            0x41, 0x5D, 0x41, 0x5C, 0x5B, 0xC3,             // pop r13; pop r12; pop rbx; ret
            0x48, 0x89, 0xDC, 0xBA, 1, 0, 0, 0,             // bail: mov rsp, rbx; mov edx, 1
            0xE9, 0xED, 0xFF, 0xFF, 0xFF,                   // jmp exit
            0x48, 0x89, 0xDC, 0xBA, 2, 0, 0, 0,             // poll: mov rsp, rbx; mov edx, 2
            0xE9, 0xE0, 0xFF, 0xFF, 0xFF,                   // jmp exit
        ]);
        assert_eq!(body, want.len());
        #[rustfmt::skip]
        want.extend_from_slice(&[
            0x57, 0x56,                                     // push rdi; push rsi
            0x48, 0x81, 0xEC, 40, 0, 0, 0,                  // sub rsp, 2 * 16 + 8
            0x4C, 0x39, 0xE4,                               // cmp rsp, r12
            0x0F, 0x82, 0xD4, 0xFF, 0xFF, 0xFF,             // jb bail
            0x41, 0x80, 0x7D, 0x00, 0x00,                   // cmp byte [r13], 0
            0x0F, 0x85, 0xD6, 0xFF, 0xFF, 0xFF,             // jne poll
            0x48, 0x89, 0xE7,                               // mov rdi, rsp
            0x48, 0x8D, 0xB4, 0x24, 16, 0, 0, 0,            // lea rsi, [rsp + 16]
            0x48, 0x89, 0x87, 8, 0, 0, 0,                   // mov [rdi + 8], rax
            0x48, 0x8B, 0x87, 8, 0, 0, 0,                   // return: mov rax, [rdi + 8]
            0x48, 0x81, 0xC4, 40, 0, 0, 0,                  // add rsp, 40
            0x5E, 0x5F, 0xC3,                               // pop rsi; pop rdi; ret
        ]);
        assert_eq!(code, want);
    }

    /// A shift row: the count is range-checked with one unsigned compare
    /// before `shl rax, cl`, and no overflow exit follows it.
    #[test]
    fn shift_row_encoding() {
        let mut em = emitter();
        let m = JitMeta {
            class: J_NUMBIN_T,
            ka: KC,
            kb: KI,
            kc: KT,
            arith: ARITH_SHL,
            ..JitMeta::default()
        };
        let inst = Inst {
            a: 0x10,
            b: 3,
            c: 0x20,
            ..Inst::plain(TOp::NumBinT)
        };
        em.emit_op(J_NUMBIN_T, m, &inst, Target::Side(0));
        #[rustfmt::skip]
        let want: Vec<u8> = vec![
            0x48, 0x8B, 0x87, 0x18, 0, 0, 0,          // mov rax, [rdi + 0x10 + 8]
            0x49, 0xB9, 3, 0, 0, 0, 0, 0, 0, 0,       // movabs r9, 3
            0x49, 0x83, 0xF9, 0x3F,                   // cmp r9, 63
            0x0F, 0x87, 0, 0, 0, 0,                   // ja (the interpreted row)
            0x4C, 0x89, 0xC9,                         // mov rcx, r9
            0x48, 0xD3, 0xE0,                         // shl rax, cl
            0x48, 0x89, 0x86, 0x28, 0, 0, 0,          // mov [rsi + 0x20 + 8], rax
            0xC6, 0x86, 0x20, 0, 0, 0, 0x02,          // mov byte [rsi + 0x20], Int
        ];
        assert_eq!(em.code, want);
        // `>>` is the arithmetic shift, and `&` one instruction.
        em.code.clear();
        em.fixups.clear();
        em.emit_op(
            J_NUMBIN_T,
            JitMeta {
                arith: ARITH_SAR,
                ..m
            },
            &inst,
            Target::Side(0),
        );
        assert_eq!(em.code[30..33], [0x48, 0xD3, 0xF8]); // sar rax, cl
        em.code.clear();
        em.fixups.clear();
        em.emit_op(
            J_NUMBIN_T,
            JitMeta {
                arith: OPA_BITAND,
                ..m
            },
            &inst,
            Target::Side(0),
        );
        assert_eq!(em.code[17..20], [0x4C, 0x21, 0xC8]); // and rax, r9
        assert_eq!(em.code.len(), 17 + 3 + 7 + 7);
    }
}

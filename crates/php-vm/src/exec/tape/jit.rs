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

use std::collections::HashMap;
use std::cell::Cell;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use super::{Inst, SLOT_SHIFT, St, Tape, TOp, src, tmp_off, KC, KI, KL, KT, KR};
use crate::opcode::PROP_CACHE_THIS;
use crate::place::propcache::{PC_DYN, PC_ISSET, PC_READ, PC_WRITE};
use crate::func::{Func, FUNC_TABLE_VERSION};
use crate::opcode::{ArgSrc, Op, Operand};
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
/// `plan_call`'s marker for a caller-side `Send` that can be left out
/// entirely: the callee's inlined body never mentions that parameter
/// (or the caller passed more arguments than the callee declares, and
/// the plain call discards the extras), so there is nothing to guard
/// and nothing to store, whatever the caller-side value was.
const KSKIP: u8 = 6;
/// `plan_call`'s receiver kind for a method call whose receiver is the
/// frame's `$this` (`InitMethodCall` with `Operand::Unused`): not a
/// slot, read through the frame pointer instead (see `THIS_FIELD`).
const KTHIS: u8 = 7;
/// The most arguments an inlined call may pass (the plain-call path's
/// inline argument buffer, `calls.rs`'s `MAX_ARGS`).
const MAX_CALL_ARGS: usize = 6;

/// A `Value`/`Option<Value>` slot's tag byte (offset 0 of the slot): the
/// enum discriminant, pinned by `php-runtime`'s `jit_value_layout` test.
#[allow(dead_code)] // layout documentation
const TAG_NULL: u8 = 0;
#[allow(dead_code)] // layout documentation
const TAG_BOOL: u8 = 1;
const TAG_INT: u8 = 2;
const TAG_STR: u8 = 4;
const TAG_OBJECT: u8 = 6;
/// Byte offset of an `Obj` handle's payload (`ObjectData`) in its
/// allocation, from the handle pointer (the `ObjCell<T>` header):
/// pinned by `php-runtime`'s `jit_value_layout` test.
const OBJ_PAYLOAD: u32 = php_runtime::value::obj_payload_offset::<crate::object::ObjectData>() as u32;
/// Byte offset of `ObjectData::class`, the `Rc<Class>` field.
const CLASS_FIELD: u32 = std::mem::offset_of!(crate::object::ObjectData, class) as u32;
/// Distance from an `Rc`'s allocation base (the raw field word, its
/// `NonNull` handle) to `Rc::as_ptr`, the value: the inner box's two
/// reference-count words. Pinned by `rc_inner_prefix_size` below.
const RC_VALUE_OFFSET: u32 = 2 * std::mem::size_of::<usize>() as u32;
/// Byte offset of `Frame::this`, the `Option<Obj>`: `None` is a null
/// pointer (`Option<NonNull>`'s niche, pinned by `value_is_16_bytes`).
const THIS_FIELD: u32 = std::mem::offset_of!(crate::vm::Frame, this) as u32;
/// Byte offset of `Frame::scope`, the `Option<Rc<Class>>` calling scope:
/// `None` is a null pointer, else the `Rc`'s allocation handle.
const SCOPE_FIELD: u32 = std::mem::offset_of!(crate::vm::Frame, scope) as u32;
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
}

impl JitCtx {
    /// A context for `tape` (already decoded to `tape_slice`), reading
    /// its eagerly-created tables; `None` when the JIT is off (checked
    /// with one load), the JIT was off while `tape` was decoded, or
    /// [`Tape::jit_kill`] ran.
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
        })
    }

    #[inline(always)]
    pub(crate) fn alive(&self) -> bool {
        !self.jcnt.get().is_null()
    }

    /// Stop hooking this frame's back edges (the tape's tracing is dead).
    fn disarm(&self) {
        self.jcnt.set(std::ptr::null());
        self.jtr.set(std::ptr::null());
    }
}

/// A backward jump onto instruction `close`'s target compiles the loop
/// once `close` has been taken this many times.
const HOT_LIMIT: i32 = 20_000;
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

/// Whether the JIT is enabled for this process (read once).
#[inline]
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("BUZZS_JIT").is_some_and(|v| v != "0"))
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
/// `JitMeta.db` for a property write whose result is discarded (no
/// destination temporary).
const NO_DST: u32 = u32::MAX;

/// The number of tape instructions one whitelisted class consumes
/// (fused rows cover the ops they merge).
pub(crate) fn c_step(c: u16) -> usize {
    if c == J_CMP || c == J_ARITH_CV {
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
}

/// The arithmetic ids `C_PROPOPA` accepts in `arith`: `jit_arith_id!`
/// covers `+`/`-`/`*` (0/1/2) and [`op_arith`] adds `%` (5), so the
/// bitwise ops use the free 6/7/8. Div, Mod, Pow, Concat, the shifts
/// and the boolean operators have no int-int inline form.
/// `JitMeta.mask` flag for a `J_ASSIGN_T` row whose source temporary is
/// one an `isset()`/`empty()` row of the same region stored: the tag
/// written into the compiled variable is `Bool`, not `Int`.
const BOOL_SRC: u8 = 0x80;

const OPA_BITAND: u8 = 6;
const OPA_BITOR: u8 = 7;
const OPA_BITXOR: u8 = 8;

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
        }
    }
}

/// One entry guard: slot (`base` 0 = `cvs`, 1 = `tmps`; byte offset),
/// exact (it must hold an `Int`) or weak (anything the interpreter's
/// own store test accepts: a scalar or nothing).
#[derive(Clone, Copy)]
struct Guard {
    base: u8,
    off: u32,
    exact: bool,
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
    /// `FUNC_TABLE_VERSION`, only emitted `has_calls`).
    version: u64,
}
// ----------------------------------------------------------------- plan

/// The class and meta of one instruction, or `None` if the whitelist
/// does not cover it. `KL` operands are resolved to their int values
/// here (a plan only compiles int literals).
fn classify(st: &St, inst: &Inst) -> Option<(u16, JitMeta)> {    let mut m = match inst.op {
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
            Some(std::ptr::read_unaligned(p.add(PAYLOAD as usize).cast::<i64>()))
        } else {
            None
        }
    }
}

/// The JIT's arithmetic id for a `NUM_OPS` index (`inst.e`): `+`/`-`/`*`
/// never reach this (the specialised `arith_tmp`/`arith_cv`/`assign_op`
/// rows own them), so within the numeric families' reach only `%`
/// (id 1 in `NUM_OPS`, the C `%` on x86) has an emitter id here (5,
/// distinct from `jit_arith_id!`'s 0/1/2); division's int-or-float
/// result and the bit/logic ops stay interpreted.
fn op_arith(e: u8) -> Option<u8> {
    if e == 1 {
        Some(5)
    } else {
        None
    }
}

/// Add one slot guard (merging with an existing one: exact wins).
fn add_slot(guards: &mut Vec<Guard>, base: u8, off: u32, exact: bool) -> bool {    if let Some(g) = guards.iter_mut().find(|g| g.base == base && g.off == off) {
        g.exact |= exact;
        true
    } else if guards.len() >= MAX_GUARDS {
        false
    } else {
        guards.push(Guard { base, off, exact });
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
        defs.contains(&off) || bools.contains(&off) || add_slot(guards, KT, off, false)
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
        J_ARITH_CV => read(m.ka, inst.a, guards)
            && read(m.kb, inst.b, guards)
            && keep_int(inst.c, guards),
        J_ASSIGN_OP | J_NUMBIN_C => read(m.ka, inst.a, guards) && read(m.kb, inst.b, guards),
        J_CMP => read(m.ka, inst.a, guards) && read(m.kb, inst.b, guards),
        J_INCCMP => keep_int(inst.a, guards) && read(m.kb, inst.b, guards),
        // `inst.a` = a temp (exact: read as an int), then the cv it is
        // written into (weak: nothing of its own is read).  A temp an
        // earlier `isset()`/`empty()` row of the region stored is fresh
        // too, and the meta's `mask` flag (set in the scan) tells the
        // emitted row to store the `Bool` tag, not `Int`.
        J_ASSIGN_T => (defs.contains(&inst.b)
            || bools.contains(&inst.b)
            || add_slot(guards, KT, inst.b, true))
            && add_slot(guards, KC, inst.a, false),
        // Arithmetic into a temporary: the destination is fully stored
        // (tag and payload), so it only needs the weak guard (a scalar
        // or nothing, what the interpreter's own store test accepts).
        J_ARITH_TMP | J_NUMBIN_T => read(m.ka, inst.a, guards)
            && read(m.kb, inst.b, guards)
            && fresh_dst(inst.c, guards),
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
        C_PROPW => (m.sc == KT || m.sc == KL || read(m.sc, m.sw, guards))
            && (m.db == NO_DST || fresh_dst(m.db, guards)),
        // `++`/`--`, `isset()`/`empty()` and `op=` re-check the base,
        // scope, class and slot every iteration like a write; only the
        // result temporary needs an entry guard (fully stored, tag and
        // payload, by the emitted row).
        C_PROPINC => m.db == NO_DST || fresh_dst(m.db, guards),
        C_PROPISSET => fresh_dst(m.db, guards),
        C_PROPOPA => (m.sc == KT || m.sc == KL || read(m.sc, m.sw, guards))
            && (m.db == NO_DST || fresh_dst(m.db, guards)),
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
    let e = code.prop_cache.try_borrow().ok()?.get(inst.a as usize)?.clone();
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
    if e.scope_addr != scope || inst.b >= code.num_tmps {
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
    let e = code.prop_cache.try_borrow().ok()?.get(*place as usize)?.clone();
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
    let (sc, sw, lit_c, consume) = match val {
        Operand::Cv(cv) if (*cv as usize) < code.cv_names.len() => (KC, *cv << SLOT_SHIFT, 0, false),
        Operand::Tmp(t) | Operand::TmpKeep(t) if *t < code.num_tmps => (
            KT,
            *t << SLOT_SHIFT,
            0,
            matches!(val, Operand::Tmp(_)),
        ),
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
    let e = code.prop_cache.try_borrow().ok()?.get(inst.a as usize)?.clone();
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
    let Some(Op::AssignOpObjProp { place, op, val, dst }) = code.ops.get(inst.d as usize) else {
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
        Operand::Cv(cv) if (*cv as usize) < code.cv_names.len() => (KC, *cv << SLOT_SHIFT, 0, false),
        Operand::Tmp(t) | Operand::TmpKeep(t) if *t < code.num_tmps => (
            KT,
            *t << SLOT_SHIFT,
            0,
            matches!(val, Operand::Tmp(_)),
        ),
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

/// The target instruction index of the jump of the instruction at
/// index `i`, for the classes that jump.
fn jump_target(tape: &[Inst], i: usize) -> usize {
    #[allow(clippy::cast_possible_wrap)]
    let off = (tape[i].c as i32) as i64;
    (i as i64 + off / INST_SIZE) as usize
}

/// The region scan. `close` is the closing backward jump, `head` its
/// target; both are instruction indexes.
fn plan(st: &St, code: &crate::opcode::OpArray, tape: &[Inst], head: usize, close: usize) -> Option<Plan> {
    macro_rules! rej {
        ($why:expr) => {{
            if debug_on() {
                eprintln!("jit:   plan reject at {head}..={close}: {}", $why);
            }
            return None;
        }};
    }
    if head >= close || close - head + 1 > MAX_REGION {
        rej!("empty or too big");
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
    // Some inlined call reads its `$this` receiver, or a property read
    // re-checks the frame's scope: keep the frame pointer (entry
    // argument `rdx`) in `r10` for those guards.
    let frame = class.contains(&C_PROP)
        || class.contains(&C_PROPW)
        || class.contains(&C_PROPINC)
        || class.contains(&C_PROPISSET)
        || class.contains(&C_PROPOPA)
        || calls.iter().flatten().any(|cp| {
            cp.recv.is_some_and(|(kind, _, _)| kind == KTHIS)
        });
    let mut guards: Vec<Guard> = Vec::new();
    // Temporary offsets fully stored by an earlier row of the region
    // (in program order): reads of those at guard time are dead.  The
    // same for a `Bool` an `isset()`/`empty()` row stored (`bools`:
    // fresh to *copy*, never a known `Int` to do arithmetic with), and
    // for the compiled variables whose whole trace-time value is such
    // a `Bool` (`cvbools`: no row may read those as ints).
    let mut defs: Vec<u32> = Vec::new();
    let mut bools: Vec<u32> = Vec::new();
    let mut cvbools: Vec<u32> = Vec::new();
    for j in 0..len {
        if class[j] == 0 {
            continue;
        }
        if class[j] == C_CALL {
            let cp = calls[j].as_ref().unwrap();
            if !cp.sends.iter().all(|arg| match arg.kind {
                KT if defs.contains(&arg.off) => true,
                KC | KT => add_slot(&mut guards, arg.kind, arg.off, true),
                _ => true,
            }) {
                rej!(format!("guards for the call at {}", head + j));
            }
            if let Some(d) = cp.dst {
                if !add_slot(&mut guards, KT, d, false) {
                    rej!(format!("guard for the call result at {}", head + j));
                }
                defs.push(d);
            }
            continue;
        }
        if !add_class_guards(
            tape,
            head + j,
            class[j],
            meta[j],
            &defs,
            &bools,
            &cvbools,
            &mut guards,
        ) {
            rej!(format!("guards at [{}, {:?}]", head + j, tape[head + j].op));
        }
        // An `isset()`/`empty()` temp copied into a compiled variable
        // (`$ok = isset(...)`, `$ok = empty(...)`) writes a `Bool`
        // there for the trace's whole run: flag the row so the emitted
        // store tags it `Bool` (the isset's clean 0/1 payload makes the
        // constant tag sound), and record the variable as a bool.
        if class[j] == J_ASSIGN_T && bools.contains(&tape[head + j].b) {
            meta[j].mask = BOOL_SRC;
            let a = tape[head + j].a;
            if !cvbools.contains(&a) {
                cvbools.push(a);
            }
        }
        // The temps this row fully stores (int tag and payload), seen
        // in order by the rows after it.
        match class[j] {
            C_PROP => defs.push(meta[j].db),
            // The destination is fully stored too; a consumed `Tmp`
            // source is left `TAG_NULL` (the interpreted row nulls it),
            // so it is no longer a known `Int`.
            C_PROPW => {
                if meta[j].db != NO_DST {
                    defs.push(meta[j].db);
                }
                if meta[j].consume && meta[j].sc == KT {
                    defs.retain(|d| *d != meta[j].sw);
                }
            }
            // `++`/`--`: the slot keeps its `Int` tag and the result
            // temporary (if kept) is fully stored.
            C_PROPINC => {
                if meta[j].db != NO_DST {
                    defs.push(meta[j].db);
                }
            }
            // `isset`/`empty` store a `Bool`: the destination is a
            // fresh value, but never a known `Int`, and it overwrites
            // any earlier `Int` def for the same slot.
            C_PROPISSET => {
                bools.retain(|d| *d != meta[j].db);
                defs.retain(|d| *d != meta[j].db);
                bools.push(meta[j].db);
            }
            // `op=`: like a write; the slot stays `Int`, the result
            // temporary (if kept) is fully stored, and a consumed `Tmp`
            // source becomes `TAG_NULL`.
            C_PROPOPA => {
                if meta[j].db != NO_DST {
                    defs.push(meta[j].db);
                }
                if meta[j].consume && meta[j].sc == KT {
                    defs.retain(|d| *d != meta[j].sw);
                }
            }
            // Same consume hygiene: the source temporary is `TAG_NULL`
            // after this row, whatever tag it had.
            J_ASSIGN_T => {
                let b = tape[head + j].b;
                defs.retain(|d| d != &b);
                bools.retain(|d| d != &b);
            }
            J_ARITH_TMP | J_NUMBIN_T => defs.push(tape[head + j].c),
            _ => {}
        }
        // Any row that writes an int into a compiled variable (or an int
        // from a temporary through `AssignT`) ends its `Bool` state.
        match class[j] {
            C_INC | C_DEC | C_ASSIGN_I | C_ASSIGN_L | J_INCCMP | J_ASSIGN_OP | J_NUMBIN_C => {
                let a = tape[head + j].a;
                cvbools.retain(|c| c != &a);
            }
            J_ARITH_CV => {
                let a = tape[head + j].c;
                cvbools.retain(|c| c != &a);
            }
            J_ASSIGN_T if meta[j].mask != BOOL_SRC => {
                let a = tape[head + j].a;
                cvbools.retain(|c| c != &a);
            }
            _ => {}
        }
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
    let uf = if let Some(name) = name {
        let cache = code.fcall_cache.borrow();
        match cache.get(name as usize) {
            None => rj!("no fcall cache slot for the name"),
            Some(None) => rj!("fcall cache not filled"),
            Some(Some(cell)) => match cell.get() {
                Some(Func::User(uf)) => uf,
                Some(Func::Builtin(_)) => rj!("callee is a builtin"),
                None => rj!("cached callee is dead"),
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
            rj!("method cache slot unfilled");
        };
        if cached.name_lit != Some(lit) {
            rj!("method cache entry is for another name");
        }
        if cached.class_addr == 0 {
            rj!("method cache entry has no class");
        }
        let cls = match cached.class.upgrade() {
            Some(c) => c,
            None => rj!("cached receiver class is dead"),
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
            None => rj!("cached method is dead"),
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
            None => rj!("static call cache slot unfilled or stale"),
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
        // one without coercion, and an `int` return is the only one the
        // body can be trusted to honour.
        if uf.params.iter().any(|p| {
            p.ty
                .as_ref()
                .is_some_and(|t| t.mask & mask::LONG == 0)
        }) || uf
            .return_type
            .as_ref()
            .is_some_and(|t| t.mask & mask::LONG == 0)
        {
            rj!("typed callee rejects an int param or return");
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
            C_INC | C_DEC | C_ASSIGN_I | C_ASSIGN_L | J_ASSIGN_T | J_ARITH_TMP | J_NUMBIN_T
                | J_ARITH_CV | J_ASSIGN_OP | J_NUMBIN_C
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
    for (j, arg) in sends.iter_mut().enumerate() {
        if j >= uf.params.len() || j >= ncvs || !live[j] {
            arg.kind = KSKIP;
            arg.off = 0;
            arg.lit = 0;
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
        if !read(m.ka, ia, &known) || !read(m.kb, ib, &known) {
            // (C_INC/C_DEC re-read what they write, so a not-yet-known
            // slot refuses the region.)
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
                known[scratch_idx(if c == J_ASSIGN_OP { KC } else { KT }, ia)?] = true;
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
                Arg {
                    kind,
                    off,
                    lit: v,
                }
            } else {
                let lit = if kind == KL { resolved_lit(&cst, off)? } else { 0 };
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
/// exit stub resuming at an instruction index, an entry-guard exit
/// (which also disarms the trace: the assumption it was built on is
/// not holding), or an absolute position in the code written so far
/// (a fused op's fall-through).
#[derive(Clone, Copy)]
enum Target {
    Head,
    Label(usize),
    Stub(usize),
    Disarm(usize),
    At(usize),
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
    disms: HashMap<usize, usize>,
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

    /// `mov qword [rdi + off + 8], imm32` (sign-extended): a literal
    /// payload store.
    fn store_imm(&mut self, off: u32, v: i32) {
        self.store_imm_base(KC, off, v);
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
}
impl Emitter {
    /// `mov [rdi + off + 8], r9` (payload store from `r9`: `89 /r` names
    /// `r9` through the *reg* field, which `REX.R` extends).
    fn store_int_r9(&mut self, off: u32) {
        self.store_int_r9_base(KC, off);
    }

    /// `mov [base + off + 8], r9`: the same with an explicit base.
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
            C_PROP => self.emit_prop(m, Target::Stub(i)),
            C_PROPW => self.emit_propw(m, Target::Stub(i)),
            C_PROPINC => self.emit_propinc(m, Target::Stub(i)),
            C_PROPISSET => self.emit_propisset(m, Target::Stub(i)),
            C_PROPOPA => self.emit_propopa(m, Target::Stub(i)),
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
                self.jcc(0x80, Target::Stub(i)); // jo: overflow, before any store
                self.store_int(KC, inst.a);
                self.put(&[REX_W | REX_R, 0x39, 0xC8]); // cmp rax, r9
                let cc = jcc_for(m.mask).unwrap_or(0x85);
                self.jcc(cc, self.jump_tgt(j));
            }
            c => {
                // Every other class exits at the instruction itself
                // (the interpreter re-runs it).
                self.emit_op(c, m, &inst, Target::Stub(i));
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
            // `$cv = TMP`: copy the int, then consume the temporary
            // (null it, as the interpreted row does).
            J_ASSIGN_T => {
                self.load_int(m.kb, inst.b);
                self.store_int(m.ka, inst.a);
                // A `Bool` from the region's own `isset()`/`empty()`
                // row keeps its tag (that row always stores a clean
                // 0/1 payload, so the tag is constant here); otherwise
                // this is the int path the plan checked.
                self.store_tag_base(
                    m.ka,
                    inst.a,
                    if m.mask == BOOL_SRC { TAG_BOOL } else { TAG_INT },
                );
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
                if m.arith != 5 {
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
                eprintln!(
                    "jit:   recv kind {rk} off {roff} class {cls_addr:#x}"
                );
            }
            if rk == KTHIS {
                // `$this`: the frame field is an `Option<Obj>` with no
                // tag byte to check; `None` is a null pointer (the
                // `NonNull` niche).
                self.load_r9_frame(THIS_FIELD);
                self.put(&[REX_W | REX_R | REX_B, 0x85, 0xC9]); // test r9, r9
                self.jcc(0x84, Target::Stub(cp.callove)); // je: no `$this`
                self.obj_class_from_r9();
            } else {
                self.cmpb(rk, roff, TAG_OBJECT);
                self.jcc(0x85, Target::Stub(cp.callove)); // jne: not an object
                self.load_obj_class(rk, roff);
            }
            // `r9` holds the raw `Rc<Class>` field word: the inner
            // box's base, `RC_VALUE_OFFSET` below the `Class` value the
            // cache names.
            self.movabs_rax(cls_addr.wrapping_sub(u64::from(RC_VALUE_OFFSET)) as i64);
            self.cmp_rax_r9();
            self.jcc(0x85, Target::Stub(cp.callove)); // jne: other class
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
            self.emit_op(cp.classes[k], cp.metas[k], &cp.rows[k], Target::Disarm(cp.callove));
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
                KL => {
                    let v = cp.ret.lit;
                    if i64::from(i32::try_from(v).unwrap_or(i32::MAX)) == v {
                        self.store_imm(d, v as i32);
                    } else {
                        self.movabs(true, v);
                        self.store_int_r9(d);
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
    /// `inc` carries `empty`. The destination temporary gets a clean
    /// `0`/`1` payload with a `Bool` tag (never an `Int` def, so later
    /// rows re-guard it as they read it).
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
        // `Bool`: set; empty only when the payload byte is 0 (the
        // payload's upper bits are unspecified, so compare byte-wise).
        self.set_label(l_bool);
        if m.inc {
            self.movrcx_at_rax(so.wrapping_add(PAYLOAD));
            self.put(&[0x84, 0xC9]); // test cl, cl
            self.mov_ecx(0);
            self.put(&[0x0F, 0x94, 0xC1]); // sete cl
        } else {
            self.mov_ecx(1);
        }
        // Store the clean `Bool` into the result temporary.
        self.set_label(l_end);
        self.store_rcx_base(KT, m.db);
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
            disms: HashMap::new(),
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
            self.movabs_rax(crate::func::FUNC_TABLE_VERSION.as_ptr() as i64);
            // `cmp qword [rax], imm32` (`81 /7`, mod=00 rm=000: 0x38).
            self.put(&[REX_W, 0x81, 0x38]);
            self.put(&u32::try_from(self.plan.version).unwrap_or(u32::MAX).to_le_bytes());
            self.jcc(0x85, Target::Disarm(self.plan.head)); // jne
        }
        // The head: per-iteration interrupt poll and iteration cap,
        // both exiting at the head (the interpreter then runs one
        // iteration and services the safe point at its back edge).
        self.head_pos = self.code.len();
        self.put(&[0x41, 0x80, 0x3B, 0x00]); // cmp byte [r11], 0
        self.jcc(0x85, Target::Stub(self.plan.head)); // jne
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
        let mut j = 0;
        while j < len {
            let c = self.plan.class[j];
            if c != 0 {
                self.label(head + j);
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

    /// Patch every `jmp`/`jcc` rel32 and append the exit stubs.
    fn resolve(&mut self) {
        let fixups = std::mem::take(&mut self.fixups);
        let mut missing: Vec<(usize, bool)> = fixups
            .iter()
            .filter_map(|(_, t)| match t {
                Target::Stub(i) if !self.stubs.contains_key(i) => Some((*i, false)),
                Target::Disarm(i) if !self.disms.contains_key(i) => Some((*i, true)),
                _ => None,
            })
            .collect();
        missing.sort_unstable();
        missing.dedup();
        for (idx, disarm) in missing {
            let at = self.code.len();
            if disarm {
                // `-(idx + 1)`: negative means "disarm this trace".
                self.code.push(0x48); // REX.W
                self.code.push(0xB8); // movabs rax, imm64
                self.code.extend_from_slice(&((idx as i64 + 1).wrapping_neg()).to_le_bytes());
            } else {
                self.code.push(0xB8); // mov eax, imm32 (the resume index)
                self.code.extend_from_slice(&u32::try_from(idx).unwrap_or(u32::MAX).to_le_bytes());
            }
            self.code.push(0xC3); // ret
            if disarm {
                self.disms.insert(idx, at);
            } else {
                self.stubs.insert(idx, at);
            }
        }
        for (pos, t) in fixups {
            let abs = match t {
                Target::Head => self.head_pos,
                Target::Label(j) => self.labels[&j],
                Target::Stub(idx) => self.stubs[&idx],
                Target::Disarm(idx) => self.disms[&idx],
                Target::At(p) => p,
            };
            let rel = abs as i64 - (pos + 4) as i64;
            self.code[pos..pos + 4].copy_from_slice(&i32::try_from(rel).unwrap_or(0).to_le_bytes());
        }
    }
}
// ------------------------------------------------------------- the driver

extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut c_void;
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
type LoopFn = unsafe extern "C" fn(*mut u8, *mut u8, *mut u8) -> i64;

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
    if TRACES.fetch_add(1, Ordering::Relaxed) >= MAX_TRACES {
        return usize::MAX;
    }
    let _g = COMPILE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
            let note = if inst.op == TOp::CallInit || classify(st, inst).is_some() {
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
    let mut p = match plan(st, code, tape, head, close) {
        Some(p) => p,
        None => {
            if debug_on() {
                eprintln!("jit: region {head}..={close} rejected by plan");
            }
            return usize::MAX;
        }
    };
    if debug_on() {
        for g in &p.guards {
            eprintln!(
                "jit:   guard base {} off {} {}",
                g.base,
                g.off,
                if g.exact { "exact" } else { "weak" }
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
        let total: usize = p
            .calls
            .iter()
            .flatten()
            .map(|cp| cp.size as usize)
            .sum();
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
        eprintln!("jit: region {head}..={close} compiled to {} bytes", em.code.len());
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
        Some(p) => p as usize,
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
/// `target < at`, and `ctx.alive()`.
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
    if c != 0 {
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
        // SAFETY: the region only touches the frame's slots, and exits
        // leave them exactly as an interpreted run would.
        let r = unsafe { f(st.cvs, st.tmps, st.f.cast::<u8>()) };
        if debug_on() {
            static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let k = RUNS.fetch_add(1, Ordering::Relaxed);
            if k % 100_000 == 0 {
                eprintln!("jit: trace run #{k} r={r}");
            }
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
        if !ctx.tape.is_null() {
            unsafe { (*ctx.tape).jit_good() };
        }
        let r = r.clamp(0, (ctx.tlen - 1) as i64) as usize;
        return unsafe { ctx.tbase.add(r) };
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
        if k % 1_000_000 == 0 {
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
    if n + 1 < HOT_LIMIT {
        return target;
    }
    let compiled = attempt(&st, ctx, head, idx);
    // SAFETY: as above.
    let cell = unsafe { &*ctx.jtr.get().add(idx) };
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
        // SAFETY: as above; now the cell is armed.
        unsafe { back_edge(st, ctx, at, target) }
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
        assert_eq!(unsafe { *((std::ptr::addr_of!(empty) as usize + 8) as *const usize) }, 0);
    }

    /// `emit_prop`'s `$this` guard treats a null field word as "no
    /// `$this`" and its scope guard as "no scope": both fields are
    /// `Option`ed pointers whose niche must make `None` null.
    #[test]
    fn option_pointers_are_null() {
        assert_eq!(std::mem::size_of::<Option<std::rc::Rc<std::cell::Cell<u64>>>>(), 8);
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
        assert_eq!(em.code, vec![0x49, 0x83, 0xB9, 0x40, 0x00, 0x00, 0x00, 0x00]);
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
}

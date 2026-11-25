// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The trace JIT: whole loops of the tape compiled to machine code.
//!
//! The tape removed the per-op decode of `Op`; its remaining cost is
//! per-op `match` dispatch, and measurements put a taped loop step at
//! about 40 instructions against php-src's 17. Running the ops natively
//! one at a time does not close that gap (tried and measured on
//! 16 Nov 2026, never committed): every op still enters and leaves
//! through the interpreter, about 10-12 instructions per transition, so
//! the one arithmetic loop it sped up went from 40 to 28 instructions per
//! iteration and a loop calling `strlen()` doubled to 84. The way to the
//! 17 is to stop interpreting: compile the loop.
//!
//! # The trace
//!
//! A trace is the contiguous run of tape instructions of one hot loop:
//! from the target of a hot backward jump (the loop head) through the
//! jump back (the closing instruction). `plan` finds it, checks every
//! instruction against the whitelist (`Jmp`, `++`/`--`, the small-int
//! and int-literal assignments, the fused comparison-and-branch, the
//! fused `++`-compare, and the fused `+ - *` into a variable), and
//! `emit` writes one region of x86-64 code (on AArch64 Linux, the `a64`
//! backend's A64 code, from the same plan) that runs the whole loop,
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

mod a64;

/// Whether a hook observer is installed (set once by `Vm::set_observer`).
/// An observed call runs the function's `pass_two` observers on entry and
/// exit, which an inlined call cannot, so plans with calls refuse one
/// while this is set. (Enabling an observer *during* a trace is a
/// documented limitation: the trace keeps running inlined.)
pub(crate) static OBSERVER_ON: AtomicBool = AtomicBool::new(false);

/// `plan_call`'s marker for a callee `return` of `Operand::Unused`
/// (`return;`): the trace stores a null into the caller's result slot.
const KNULL_ARG: u8 = 5;
/// The return of an inlined call that ends in a builtin call of one int
/// argument ([`CallPlan::ret`]): `off` is the argument's scratch slot and
/// `lit` the builtin's id (`special::quick_id`); [`jit_quick_tail`] makes
/// the call and stores its result.
const KQUICK: u8 = 8;
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
/// The last of the scalar tags (`Null`, `Bool`, `Int`, `Float`): a tag
/// above it is a reference-counted value, or nothing (`TAG_NONE`).
const TAG_FLOAT: u8 = 3;
const TAG_STR: u8 = 4;
const TAG_ARRAY: u8 = 5;
const TAG_OBJECT: u8 = 6;
const TAG_RESOURCE: u8 = 7;
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
/// A list's table as a trace reads an element of it in place
/// ([`Emitter::emit_list_read`]), by byte offset from the handle word of a
/// `Value::Array`: the word that says how the table holds its elements
/// (`LIST_KIND`: [`LIST_PACKED`] for a list, bare values at their
/// positions), the address of the first element and their count. The
/// compiler lays the store and its vector out: the layout tests'
/// `list_layout` pins all of it.
#[allow(clippy::cast_possible_truncation)] // an offset inside a table
const LIST_KIND: u32 = RC_VALUE_OFFSET + php_runtime::HashTable::STORE_OFFSET as u32;
const LIST_PACKED: i64 = i64::MIN + 1;
const LIST_ELEMS: u32 = LIST_KIND + 16;
const LIST_LEN: u32 = LIST_KIND + 24;
/// What an append to a list in place reads and writes besides: the room of
/// the allocation (the word between the kind and the address), the table's
/// size and its next free int key. Pinned by the layout tests'
/// `list_append_layout`, with [`ARRAY_COUNT`].
const LIST_CAP: u32 = LIST_KIND + 8;
#[allow(clippy::cast_possible_truncation)] // an offset inside a table
const TABLE_SIZE: u32 = RC_VALUE_OFFSET + php_runtime::HashTable::TABLE_SIZE_OFFSET as u32;
#[allow(clippy::cast_possible_truncation)] // an offset inside a table
const TABLE_NEXT: u32 = RC_VALUE_OFFSET + php_runtime::HashTable::NEXT_FREE_OFFSET as u32;
/// A reference as a trace reads through it in place, by byte offset from
/// the payload word of a `Value::Reference`: the cell's borrow word
/// (negative while the value is being written) and the value. Pinned by
/// the layout tests' `reference_layout`.
#[allow(clippy::cast_possible_truncation)] // an offset inside a 32-byte box
const REF_BORROW: u32 = php_runtime::PhpRef::CELL_OFFSET as u32;
const REF_VALUE: u32 = REF_BORROW + 8;
/// The tag of a `Value::Reference`.
const TAG_REF: u8 = 8;
/// Byte offset of an array's element count (a `u32`) from the handle word
/// of a `Value::Array` (the `Rc` allocation: its two count words, then the
/// table). The strong count is the word at the handle itself. Pinned by
/// the layout tests' `array_handle_layout`.
#[allow(clippy::cast_possible_truncation)] // an offset inside a table
const ARRAY_COUNT: u32 = RC_VALUE_OFFSET + php_runtime::HashTable::NUM_ELEMENTS_OFFSET as u32;
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
/// `Option::None` of a slot: a `Value` tag never reaches it. Which byte
/// the compiler picks for the niche is its choice (9, the first value
/// after the last variant, with older compilers; 255 with rustc 1.100), so
/// it is read from the layout itself here, at compile time. The layout
/// tests' `value_layout` pins it above every `Value` tag.
const TAG_NONE: u8 = {
    union Bytes {
        slot: std::mem::ManuallyDrop<Option<php_runtime::Value>>,
        bytes: [std::mem::MaybeUninit<u8>; 16],
    }
    let none = Bytes {
        slot: std::mem::ManuallyDrop::new(None),
    };
    // SAFETY: byte 0 of a `None` slot is its niche tag, initialized (the
    // compile-time evaluator rejects a read of an uninitialized byte).
    unsafe { none.bytes[0].assume_init() }
};
// Every guard that tells "nothing" from a value relies on this: `None`
// reads above every `Value` tag (above `Str`, the first non-scalar).
const _: () = assert!(TAG_NONE > TAG_REF);
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
    /// The VM the frame belongs to ([`JitCtx::set_vm`]), handed to the
    /// rows of a trace that read through its global scope.
    vm: Cell<*mut crate::vm::Vm>,
}

thread_local! {
    /// The VM whose frame the running trace belongs to, for
    /// [`jit_row_vm_read`]: set by [`back_edge_slow`] right before it
    /// enters a trace (a trace has no register to spare for it, and the
    /// rows that want it are few).
    static TRACE_VM: Cell<*mut crate::vm::Vm> = const { Cell::new(std::ptr::null_mut()) };
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
            vm: Cell::new(std::ptr::null_mut()),
        })
    }

    #[inline(always)]
    pub(crate) fn alive(&self) -> bool {
        !self.jcnt.get().is_null()
    }

    /// Names the VM the frame about to run belongs to: `run_tape` calls
    /// this before each entry of its core loop, with the pointer it holds
    /// the VM by. A trace entered from that run may call back into it
    /// ([`jit_row_vm_read`]) while `run_tape` itself is suspended in the
    /// core loop, which reads nothing of the VM but the frame's slots.
    #[inline(always)]
    #[allow(clippy::inline_always)] // one store on the way into the core loop
    pub(crate) fn set_vm(&self, vm: *mut crate::vm::Vm) {
        self.vm.set(vm);
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
/// The length in tape rows up to which a loop waits the whole hot limit
/// ([`head_start`]).
const HOT_ROWS: i64 = 4;

/// What the back-edge counter of row `inst` starts at when `hot` back
/// edges make a loop hot: 0, or for a row a region could close with
/// ([`is_loop_close`]) a head start by the loop's length. The limit counts
/// rounds, and what a loop costs interpreted before its trace exists is
/// its rounds times its length: a loop of up to [`HOT_ROWS`] rows waits the
/// whole limit, one of twenty rows a fifth of it, and none less than a
/// sixteenth (C PHP's tracing JIT takes any loop after 64 rounds).
fn head_start(inst: &Inst, hot: i32) -> i32 {
    if !is_loop_close(inst) {
        return 0;
    }
    #[allow(clippy::cast_possible_wrap)]
    let rows = i64::from(-(inst.c as i32)) / INST_SIZE + 1;
    let share = i32::try_from((rows / HOT_ROWS).clamp(1, 16)).unwrap_or(1);
    hot - (hot / share).max(1)
}
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
        let hot = std::env::var("BUZZS_JIT_HOT")
            .ok()
            .and_then(|v| v.parse::<i32>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(HOT_LIMIT);
        HOT_CACHE.store(hot, Ordering::Relaxed);
        hot
    })
}

/// [`hot_limit`] for [`back_edge`]'s counting path, which loads it on every
/// counted back edge: 0 until `hot_limit` first ran (no count is below
/// it, so that back edge takes the slow path, where `hot_limit` runs).
static HOT_CACHE: AtomicI32 = AtomicI32::new(0);
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
/// where the emitted code can run - x86-64 Linux ([`Emitter`]) and `AArch64`
/// Linux ([`a64`], which compiles the rows it supports and refuses every
/// other plan), the platforms of the `mmap` constants below - and switched
/// off with `BUZZS_JIT=0`.
///
/// It used to be opt-in (`BUZZS_JIT=1`), which left every ordinary run,
/// and the acceptance check itself (`perf/bench/acceptance.py` sets no
/// such variable), measuring the interpreter alone.
#[inline]
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        cfg!(all(
            any(target_arch = "x86_64", target_arch = "aarch64"),
            target_os = "linux"
        )) && std::env::var_os("BUZZS_JIT").is_none_or(|v| v != "0")
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
    tape.iter().any(is_loop_close)
}

/// Whether `inst` is a row a region could close with: of a jump class the
/// plan accepts as a close, jumping backward.
fn is_loop_close(inst: &Inst) -> bool {
    // Onto an earlier instruction, or onto itself (an empty loop).
    #[allow(clippy::cast_possible_wrap)]
    let backward = (inst.c as i32) <= 0;
    backward
        && (inst.op == TOp::Jmp
            || inst
                .op
                .jit_meta()
                .is_some_and(|m| m.class == J_CMP || m.class == J_INCCMP))
}

/// The per-tape hot-loop tables, created on the tape's first hot back
/// edge: per instruction, the backward-hit count and the trace cell.
pub(crate) struct Tables {
    cnt: Box<[AtomicI32]>,
    tr: Box<[AtomicUsize]>,
}

impl Tables {
    /// The tables of `tape`: a cell and a counter for each of its rows.
    /// The counter of a row a region could close with starts ahead by what
    /// the loop's length earns it ([`head_start`]).
    pub(crate) fn new(tape: &[Inst]) -> Tables {
        let hot = hot_limit();
        Tables {
            cnt: tape
                .iter()
                .map(|inst| AtomicI32::new(head_start(inst, hot)))
                .collect(),
            tr: tape.iter().map(|_| AtomicUsize::new(0)).collect(),
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
/// `++`/`--` of a static property whose pinned cell holds an `Int`
/// ([`plan_sprop_incdec`]).
const C_SPROPINC: u16 = 34;
/// `Op::Move` into a temporary (`ZEND_QM_ASSIGN`: a ternary's arms): an
/// int, bool or null literal, a compiled variable or a temporary holding a
/// scalar ([`plan_move`]).
const C_MOVE: u16 = 35;
/// `Op::JmpZ` of a compiled variable holding a bool, an int or null.
const C_JMPZ: u16 = 36;
/// `Op::JmpNz` of such a variable.
const C_JMPNZ: u16 = 37;
/// `$cv ?: ...` (`Op::JmpSet`) of such a variable: a truthy value is
/// copied into the result temporary and the jump taken.
const C_JMPSET: u16 = 38;
/// `$cv ?? ...` (`Op::Coalesce`) of a variable that is undefined or holds
/// a scalar: one that is not null is copied and the jump taken.
const C_COALESCE: u16 = 39;

/// `$cv[d1][d2]...` read into a temporary by the tape's own handler
/// (`dims::fetch_dims`), called from the trace ([`jit_row_fetch_dims`]):
/// the handler checks every operand on every iteration, and the result is
/// of no known kind (a row that reads it as an int checks its tag).
const C_DIMSR: u16 = 40;
/// A read through the VM's global scope (`$GLOBALS['name']`, a superglobal
/// as a whole: the tape's `VmOp` rows of `Op::FetchGlobal` and `Op::Fetch`)
/// on its fast path, called from the trace ([`jit_row_vm_read`]). The value
/// goes into the row's temporary, or straight into the variable of the
/// assignment folded behind a `FetchGlobal` (the row then covers both ops).
const C_VMREAD: u16 = 41;

/// A row run through the tape's own thin handler ([`plan_row`]): the trace
/// has no code of its own for it. The handler checks every operand on
/// every iteration and declines what it does not take, which leaves the
/// row to the interpreter, nothing changed.
const C_ROW: u16 = 42;
/// The same for a row whose handler may jump (a type test or `isset()`
/// fused with the jump on its result): the row covers both ops.
const C_ROWBR: u16 = 43;
/// `Op::JmpZ` of a temporary holding a bool (consumed).
const C_JMPZT: u16 = 44;
/// `Op::JmpNz` of such a temporary.
const C_JMPNZT: u16 = 45;
/// A read through the global scope ([`C_VMREAD`]'s ops) made by the trace
/// itself: the slot's address comes from the trace's table of global
/// slots, filled when the trace is entered ([`jit_resolve_globals`]; the
/// row's index in it in `JitMeta.slot`), and the row copies a scalar or an
/// array out of it.
const C_GREAD: u16 = 46;
/// `$cv = v` with the result kept (`AssignR`), of an int: from a variable,
/// a temporary (consumed) or a literal into the variable and into the
/// result temporary.
const C_ASSIGN_R: u16 = 47;
/// `++$cv`, `$cv++`, `--$cv`, `$cv--` with the result kept (`PreIncT` and
/// its kin), of an int: `JitMeta.inc` the direction, `JitMeta.arr` whether
/// the result is the value before the step.
const C_INCDEC_T: u16 = 48;
/// A comparison and its jump ([`J_CMP`]'s rows) with an operand the plan
/// knows to be no int for sure: decided by the operands' tags where it
/// runs ([`Emitter::emit_cmps`]).
const J_CMPS: u16 = 49;
/// `$cv op= v` ([`J_ASSIGN_OP`]'s rows, `+`, `-` and `*`) with an operand
/// the plan knows to be no int for sure: decided by the operands' tags
/// where it runs ([`Emitter::emit_assign_ops`]).
const J_ASSIGN_OPS: u16 = 50;
/// An array row ([`C_DIMR`], [`C_DIMW`], [`C_DIMOP`]) whose offset the plan
/// knows to be no int (a variable the region left a string in, a
/// temporary holding a value): run by the tape's own handler for the row,
/// which looks a string key up ([`jit_row_dim_keyed`]).
const C_DIMKEY: u16 = 51;

/// Whether rows of class `c` may jump (their target is the row's `c`
/// field, as for every tape instruction that jumps).
fn is_jump_class(c: u16) -> bool {
    matches!(
        c,
        C_JMP
            | J_CMP
            | J_CMPS
            | J_INCCMP
            | C_JMPZ
            | C_JMPNZ
            | C_JMPSET
            | C_COALESCE
            | C_ROWBR
            | C_JMPZT
            | C_JMPNZT
    )
}

/// What a [`C_ROW`] leaves in its result temporary (`JitMeta.arith`).
const ROW_INT: u8 = 1;
const ROW_BOOL: u8 = 2;
/// A value of any type, owned by the temporary ([`TmpKind::Val`]).
const ROW_VAL: u8 = 3;
/// `JitMeta.db` for a property write whose result is discarded (no
/// destination temporary).
const NO_DST: u32 = u32::MAX;

/// The number of tape instructions one whitelisted class consumes
/// (fused rows cover the ops they merge).
pub(crate) fn c_step(c: u16) -> usize {
    if c == J_CMP || c == J_CMPS || c == J_ARITH_CV || c == J_NUMBIN_D || c == C_ABS || c == C_ROWBR
    {
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
    /// A property row's result temporary base: `KT` for a row of the
    /// region, `KR` for a row of an inlined method (whose temporaries are
    /// scratch slots).
    pub(crate) kd: u8,
    /// A property row of an inlined method: every check its access needs
    /// was made once, before the method's first row (`emit_call`), so the
    /// row itself only loads the receiver again and reads or writes the
    /// slot.
    pub(crate) bare: bool,
    /// Operand `a` is a temporary whose content the plan does not know
    /// where the row reads it (a value from before the loop, or one that
    /// differs between the paths that reach the row): the emitted row
    /// checks its tag before it uses it, and leaves when it is not what
    /// the row needs (an `Int`; any scalar for a `C_MOVE`).
    pub(crate) ca: bool,
    /// The same for operand `b`.
    pub(crate) cb: bool,
    /// The compiled variable the row reads (`C_MOVE` from a variable, the
    /// truth jumps, `?:`, `??`) held an array when the plan was made: the
    /// row takes an array as well as a scalar (its truth is its element
    /// count; a copy of it is one more handle on it).
    pub(crate) arr: bool,
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
/// `JitMeta.mask` flag for a `J_ASSIGN_T` row whose source temporary
/// holds a scalar of a kind the plan does not know (the result of a
/// ternary, `?:` or `??`: one kind on one path, another on the other):
/// both words of the slot are copied.
const SCALAR_SRC: u8 = 0x20;
/// `JitMeta.mask` flag for a `J_ASSIGN_T` row whose source temporary owns
/// a value of any type ([`TmpKind::Val`]): both words of the slot move
/// into the variable (a move needs no count), whose old value is released
/// first when it is an array (the row checks what the variable holds; it
/// has no entry guard).
const VAL_SRC: u8 = 0x10;
/// `JitMeta.mask` flag for a `J_ASSIGN_T` row of such a temporary into a
/// variable that held a string, an object or a resource when the plan was
/// made: the tape's own assignment runs (`scalar::assign_cv_any`), which
/// releases whatever the variable holds.
const ANY_SRC: u8 = 0x08;

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
            kd: KT,
            bare: false,
            ca: false,
            cb: false,
            arr: false,
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
    /// A temporary whose content the plan does not know where the call
    /// reads it: its tag is checked (an `Int`) before the argument is
    /// passed.
    chk: bool,
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
    /// A closure call (`$f(...)`): the variable's slot and the handle
    /// word of the closure object the plan inlined. The guard compares
    /// the slot with it on every call (the object is kept alive in
    /// [`PINNED_OBJS`], so the word can name no other closure).
    callee_obj: Option<(u32, u64)>,
    /// With `callee_obj`: a handle on that closure object, pinned when
    /// the trace is placed.
    callee_pin: Option<php_runtime::Obj>,
    /// A `self::name()`, `parent::name()` or `static::name()` call: what
    /// the site's `method_cache` entry was resolved for, re-checked on
    /// every compiled call. The calling scope as the raw `Frame::scope`
    /// field word (the class `self` and `parent` name follows from it);
    /// for `static::` the raw `Frame::called` field word as well; and
    /// whether the method is an instance one, which needs a `$this` (an
    /// object of the scope's class, whenever the frame has one).
    rel: Option<(i64, Option<i64>, bool)>,
    /// The property slots of the receiver the inlined method reads or
    /// writes (its `C_PROP*` rows, all on its `$this`): checked once
    /// before the method's first row.
    prop_slots: Vec<u32>,
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
        for (m, inst) in self.metas.iter_mut().zip(&mut self.rows) {
            remap(m.ka, &mut inst.a);
            remap(m.kb, &mut inst.b);
            remap(m.kc, &mut inst.c);
            // A property row keeps its callee slots in the meta: the
            // value it writes and its result temporary.
            if m.bare {
                if m.sc == KR {
                    m.sw = m.sw.wrapping_add(base);
                }
                if m.kd == KR && m.db != NO_DST {
                    m.db = m.db.wrapping_add(base);
                }
            }
        }
        // The returned slot is one of this call's scratch slots as well:
        // left unbased, every call after the region's first would copy
        // its result out of the *first* call's slice.
        if self.ret.kind == KR || self.ret.kind == KQUICK {
            self.ret.off = self.ret.off.wrapping_add(base);
        }
    }
}

/// What a temporary holds at one point of the region, as far as the rows
/// of the region itself tell (see [`TmpState`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TmpKind {
    /// An `Int`: reads need no check.
    Int,
    /// A `Bool` (an `isset()`/`empty()` result, a folded constant): fresh
    /// to copy, never an int to compute with.
    Bool,
    /// A `Null` (an inlined call that returned nothing, a folded
    /// constant).
    Null,
    /// A scalar (null, a bool, an int or a float), which one unknown: the
    /// result of a ternary, `?:` or `??`, or what two paths that left
    /// different scalars agree on. Fresh to copy whole.
    Scalar,
    /// A value the temporary owns, of a type the plan does not know: a
    /// copy of a variable that held an array when the plan was made (a
    /// scalar or an array, the temporary holding a handle on it), what a
    /// read through the global scope stored (anything), or what a path
    /// that left such a value and one that left a scalar agree on. Moved
    /// whole, never stored over in place.
    Val,
    /// Consumed (nulled by the row that read it), never written by the
    /// region before this point, or written on one path only: a row that
    /// reads it checks its tag in the trace.
    Unknown,
}

/// What the rows of a region left in its temporaries at one point of it:
/// the kind of every temporary one of them fully stored (tag and payload)
/// on every path to that point; any other temporary is
/// [`TmpKind::Unknown`]. `plan` carries one through the region in program
/// order and merges the states of the paths that meet at a label.
#[derive(Clone, Default)]
struct TmpState {
    known: Vec<(u32, TmpKind)>,
}

impl TmpState {
    /// What temporary `off` holds.
    fn kind(&self, off: u32) -> TmpKind {
        self.known
            .iter()
            .find(|(o, _)| *o == off)
            .map_or(TmpKind::Unknown, |(_, k)| *k)
    }

    /// A row just left `kind` in temporary `off`: it is only what its
    /// last store made it.
    fn set(&mut self, off: u32, kind: TmpKind) {
        self.known.retain(|(o, _)| *o != off);
        if kind != TmpKind::Unknown {
            self.known.push((off, kind));
        }
    }

    /// Nothing is known (a point reached by a path the plan did not
    /// follow: a nested loop's back edge).
    fn clear(&mut self) {
        self.known.clear();
    }

    /// The state where two paths meet: a temporary keeps a kind both
    /// agree on, is a scalar of unknown kind when they left different
    /// scalars (a scalar or an array when either may have left an array),
    /// and is unknown when one of them did not store it.
    fn merge(&self, other: &TmpState) -> TmpState {
        let mut out = TmpState::default();
        for &(off, kind) in &self.known {
            let merged = match other.kind(off) {
                TmpKind::Unknown => TmpKind::Unknown,
                k if k == kind => kind,
                k if k == TmpKind::Val || kind == TmpKind::Val => TmpKind::Val,
                _ => TmpKind::Scalar,
            };
            out.set(off, merged);
        }
        out
    }

    /// Whether some temporary may hold an array here.
    fn has_val(&self) -> bool {
        self.known.iter().any(|(_, k)| *k == TmpKind::Val)
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
        // The truth jumps of a compiled variable, `?:` and `??` (the
        // result temporary in `inst.b`), and a value moved into a
        // temporary: no operand of theirs is an `Int` to guard.
        TOp::JmpZC => JitMeta {
            class: C_JMPZ,
            ka: KI,
            kb: KI,
            arr: cv_holds_array(st, inst.a),
            ..JitMeta::default()
        },
        TOp::JmpNzC => JitMeta {
            class: C_JMPNZ,
            ka: KI,
            kb: KI,
            arr: cv_holds_array(st, inst.a),
            ..JitMeta::default()
        },
        TOp::JmpSetC => JitMeta {
            class: C_JMPSET,
            ka: KI,
            kb: KI,
            arr: cv_holds_array(st, inst.a),
            ..JitMeta::default()
        },
        TOp::CoalesceC => JitMeta {
            class: C_COALESCE,
            ka: KI,
            kb: KI,
            arr: cv_holds_array(st, inst.a),
            ..JitMeta::default()
        },
        TOp::MoveT => plan_move(st, inst)?,
        // A jump on a temporary: a bool, consumed (the row checks it when
        // the plan does not know).
        TOp::JmpZT => JitMeta {
            class: C_JMPZT,
            ka: KI,
            kb: KI,
            ..JitMeta::default()
        },
        TOp::JmpNzT => JitMeta {
            class: C_JMPNZT,
            ka: KI,
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
        // `$cv = v` with the result kept: the source's kind by the row's
        // `k` (a literal is resolved below: an int, or the row is the
        // interpreter's).
        TOp::AssignR => {
            use super::scalar::{MOVE_CV, MOVE_LIT, MOVE_TMP};
            let kb = match inst.k {
                MOVE_CV => KC,
                MOVE_TMP => KT,
                MOVE_LIT => KL,
                _ => return None,
            };
            JitMeta {
                class: C_ASSIGN_R,
                ka: KC,
                kb,
                consume: inst.k == MOVE_TMP,
                ..JitMeta::default()
            }
        }
        TOp::PreIncT | TOp::PostIncT | TOp::PreDecT | TOp::PostDecT => JitMeta {
            class: C_INCDEC_T,
            ka: KC,
            kb: KI,
            inc: matches!(inst.op, TOp::PreIncT | TOp::PostIncT),
            arr: matches!(inst.op, TOp::PostIncT | TOp::PostDecT),
            ..JitMeta::default()
        },
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

/// The meta of a `MoveT` row (`Op::Move` into the temporary `inst.c`): the
/// source's kind in `sc` (`KL`: a literal folded into `arith`/`lit_c` as
/// [`plan_const`] folds one - an int, a bool or null; `KC`: the compiled
/// variable `inst.a`; `KT`: the temporary `inst.a`, nulled afterwards when
/// `consume`). `None` for a literal of another type (the interpreted
/// row's: a float, a string, an array).
fn plan_move(st: &St, inst: &Inst) -> Option<JitMeta> {
    use super::scalar::{MOVE_CV, MOVE_KEEP, MOVE_LIT, MOVE_TMP};
    let (sc, arith, lit_c) = match inst.k {
        MOVE_CV => (KC, 0, 0),
        MOVE_TMP | MOVE_KEEP => (KT, 0, 0),
        MOVE_LIT => {
            // SAFETY: a validated literal offset into a table of `Value`s
            // (tag byte, then for a `Bool` its value byte, payload word),
            // alive as long as the op array.
            unsafe {
                let p = st.lits.add(inst.a as usize);
                match *p {
                    TAG_INT => (
                        KL,
                        CONST_INT,
                        std::ptr::read_unaligned(p.add(PAYLOAD as usize).cast::<i64>()),
                    ),
                    TAG_NULL => (KL, CONST_NULL, 0),
                    TAG_BOOL => (KL, CONST_BOOL, i64::from(*p.add(BOOL_BYTE as usize))),
                    _ => return None,
                }
            }
        }
        // No operand: null.
        _ => (KL, CONST_NULL, 0),
    };
    Some(JitMeta {
        class: C_MOVE,
        ka: KI,
        kb: KI,
        sc,
        arith,
        lit_c,
        consume: inst.k == MOVE_TMP,
        arr: inst.k == MOVE_CV && cv_holds_array(st, inst.a),
        ..JitMeta::default()
    })
}

/// Whether the compiled variable at byte offset `off` of the frame a plan
/// is made in holds a string, an object or a resource right now: a value
/// the trace's own stores do not release.
fn cv_holds_other(st: &St, off: u32) -> bool {
    // SAFETY: as in `cv_holds_array`.
    !st.cvs.is_null()
        && matches!(
            unsafe { *st.cvs.add(off as usize) },
            TAG_STR | TAG_OBJECT | TAG_RESOURCE
        )
}

/// `JitMeta.mask` of a handler row whose handler answers with a
/// [`super::Br`] (one that may jump); any other answers with a `bool`
/// (done, or declined with nothing changed). See [`Emitter::emit_row`].
const ROW_BR_RET: u8 = 1;
/// `JitMeta.mask` of the `new` row whose result the next row assigns to a
/// variable: its handler answers with `calls::NEW_DECLINED`, `NEW_MADE`
/// or `NEW_OVER` (the assignment done as well). See
/// [`Emitter::emit_new_row`].
const ROW_NEW_RET: u8 = 2;

/// The tape's `new` instruction as a trace calls it ([`plan_new`]):
/// `calls::new_plain` behind the helper convention. (The handler keeps the
/// tape's own convention: its call is one of `run_core`'s, whose shape is
/// not to change for this.)
///
/// # Safety
/// `frame` is the running frame and `inst` a `calls::CALL_NEW` row of its
/// tape.
unsafe extern "C" fn jit_row_new(frame: *mut crate::vm::Frame, inst: *const Inst) -> u8 {
    // SAFETY: the caller's contract.
    unsafe { super::calls::new_plain(frame, &*inst) }
}

/// The tape's concatenation into a temporary as a trace calls it
/// ([`plan_row`]): `dims::concat_tmp` behind the helper convention, as
/// [`jit_row_new`].
///
/// # Safety
/// `frame` is the running frame and `inst` a `TOp::ConcatT` row of its
/// tape.
unsafe extern "C" fn jit_row_concat_tmp(frame: *mut crate::vm::Frame, inst: *const Inst) -> bool {
    // SAFETY: the caller's contract.
    unsafe { super::dims::concat_tmp(frame, &*inst) }
}

/// The tape's `??` of a temporary as a trace calls it ([`plan_row`]):
/// `scalar::coalesce_tmp` behind the helper convention, as
/// [`jit_row_new`].
///
/// # Safety
/// `frame` is the running frame and `inst` a `TOp::CoalesceT` row of its
/// tape.
unsafe extern "C" fn jit_row_coalesce_tmp(
    frame: *mut crate::vm::Frame,
    inst: *const Inst,
) -> super::Br {
    // SAFETY: the caller's contract.
    unsafe { super::scalar::coalesce_tmp(frame, &*inst) }
}

/// `$cv[key]` read, written or compound-written with a key that is no int,
/// for a trace ([`C_DIMKEY`]): the tape's own handler for the row `inst`
/// (the op at `at`) after its int paths, which looks a string key up and
/// leaves anything else to the general op by declining; a read that
/// raises nothing for a missing key (`FetchMode::Is`) gives null then
/// (`dims::fetch_key_missing`). Returns 0 when the row is done, 1 when
/// it was declined (nothing changed: the interpreter runs the row).
///
/// # Safety
/// `frame` is the running frame and `inst` the array row at `at` of its
/// tape.
unsafe extern "C" fn jit_row_dim_keyed(
    frame: *mut crate::vm::Frame,
    inst: *const Inst,
    _unused: *const u8,
    at: u32,
) -> u32 {
    // SAFETY: the caller's contract.
    let inst = unsafe { &*inst };
    // SAFETY: the caller's contract, for each handler.
    let done = match inst.op {
        TOp::FetchDimIC | TOp::FetchDimIT | TOp::FetchDimSC | TOp::FetchDimC => {
            // SAFETY: the caller's contract: `frame` runs the tape `inst` is a row of.
            let br = unsafe { super::dims::fetch_dim_key(frame, inst) };
            br != super::Br::Decline || {
                // SAFETY: the caller's contract; the handler's borrow of
                // the frame has ended.
                let f = unsafe { &mut *frame };
                let quiet = matches!(
                    f.code.ops.get(at as usize),
                    Some(Op::FetchDimCv {
                        mode: crate::opcode::FetchMode::Is,
                        ..
                    })
                );
                quiet && super::dims::fetch_key_missing(f, inst).is_some()
            }
        }
        // SAFETY: the caller's contract, as for the fetch rows above.
        TOp::AssignDimIC | TOp::AssignDimC => unsafe { super::dims::assign_dim_key(frame, inst) },
        // SAFETY: the caller's contract, as for the fetch rows above.
        TOp::AssignOpDimC => unsafe { super::dims::assign_op_dim(frame, inst) },
        _ => false,
    };
    u32::from(!done)
}

/// [`jit_row_dim_keyed`] for a read (`FetchDim*`): the handler, and on its
/// decline the quiet read of a missing key. The row's op is known when the
/// trace is emitted, so no dispatch on it is made here.
///
/// # Safety
/// As [`jit_row_dim_keyed`].
unsafe extern "C" fn jit_row_fetch_key(
    frame: *mut crate::vm::Frame,
    inst: *const Inst,
    _unused: *const u8,
    at: u32,
) -> u32 {
    // SAFETY: the caller's contract.
    if unsafe { super::dims::fetch_dim_key(frame, &*inst) } != super::Br::Decline {
        return 0;
    }
    // SAFETY: the caller's contract, passed on.
    unsafe { jit_row_dim_keyed(frame, inst, std::ptr::null(), at) }
}

/// [`jit_row_dim_keyed`] for a write (`AssignDimIC`, `AssignDimC`).
///
/// # Safety
/// As [`jit_row_dim_keyed`].
unsafe extern "C" fn jit_row_assign_key(
    frame: *mut crate::vm::Frame,
    inst: *const Inst,
    _unused: *const u8,
    _at: u32,
) -> u32 {
    // SAFETY: the caller's contract.
    u32::from(!unsafe { super::dims::assign_dim_key(frame, &*inst) })
}

/// [`jit_row_dim_keyed`] for a compound write (`AssignOpDimC`).
///
/// # Safety
/// As [`jit_row_dim_keyed`].
unsafe extern "C" fn jit_row_assign_op_key(
    frame: *mut crate::vm::Frame,
    inst: *const Inst,
    _unused: *const u8,
    _at: u32,
) -> u32 {
    // SAFETY: the caller's contract.
    u32::from(!unsafe { super::dims::assign_op_dim(frame, &*inst) })
}

/// The helper of a keyed element row of op `op`: one made for it, else
/// [`jit_row_dim_keyed`].
fn keyed_row_helper(op: TOp) -> usize {
    match op {
        TOp::FetchDimIC | TOp::FetchDimIT | TOp::FetchDimSC | TOp::FetchDimC => {
            jit_row_fetch_key as *const () as usize
        }
        TOp::AssignDimIC | TOp::AssignDimC => jit_row_assign_key as *const () as usize,
        TOp::AssignOpDimC => jit_row_assign_op_key as *const () as usize,
        _ => jit_row_dim_keyed as *const () as usize,
    }
}

/// The handler row of the tape's `new` instruction at `i`
/// (`calls::CALL_NEW`: `calls::new_plain` makes the object, with the
/// arguments its constructor only stores, and declines anything that
/// needs a call) and the instructions it covers: up to the one its jump
/// goes to, past the constructor call. The object is a value the result
/// temporary owns; the row polls afterwards (a possible root is recorded,
/// which may make a collection due). `None` when the row has no such
/// shape.
fn plan_new(code: &crate::opcode::OpArray, tape: &[Inst], i: usize) -> Option<(JitMeta, usize)> {
    let inst = tape.get(i)?;
    if inst.b >= code.num_tmps {
        return None;
    }
    let end = jump_target(tape, i);
    if end <= i || end > tape.len() {
        return None;
    }
    let handler = jit_row_new as *const ();
    let m = JitMeta {
        class: C_ROW,
        ka: KI,
        kb: KI,
        db: inst.b.checked_shl(SLOT_SHIFT)?,
        arith: ROW_VAL,
        sb: NO_DST,
        slot: NO_DST,
        sw: NO_DST,
        lit_c: -1,
        inc: true,
        lit_a: i64::try_from(handler as usize).ok()?,
        // (The row after the jump is the assignment: `end + 1` is a row.)
        mask: if super::calls::new_target(inst).is_some() && end + 1 < tape.len() {
            ROW_NEW_RET
        } else {
            0
        },
        ..JitMeta::default()
    };
    Some((m, end - i))
}

/// The handler row of the instruction at op `i`, for the rows the trace has
/// no code of its own for, with the ops it covers: the handler's entry
/// point in `lit_a` (the tape's own handler, `extern "C"` for this: the
/// frame and the row) and how it answers in `mask`, whether the row polls afterwards in `inc` (it may
/// release a value or allocate), the result temporary in `db` and what it
/// then holds in `arith`, the temporaries the handler consumes in `sb`,
/// `slot` and `sw`, and a variable it writes in `lit_c` (-1: none).
/// `None` for a row without a thin handler taken here.
fn plan_row(code: &crate::opcode::OpArray, i: usize, inst: &Inst) -> Option<(u16, JitMeta, usize)> {
    let mut m = JitMeta {
        ka: KI,
        kb: KI,
        db: NO_DST,
        sb: NO_DST,
        slot: NO_DST,
        sw: NO_DST,
        lit_c: -1,
        ..JitMeta::default()
    };
    let (class, handler, step): (u16, *const (), usize) = match inst.op {
        // (Their result field is `u32::MAX` when unused: `NO_DST`.)
        TOp::StrlenC => {
            (m.db, m.arith) = (inst.c, ROW_INT);
            (C_ROW, super::strs::strlen_cv as *const (), 1)
        }
        TOp::CountC => {
            (m.db, m.arith) = (inst.c, ROW_INT);
            (C_ROW, super::strs::count_cv as *const (), 1)
        }
        TOp::TypeCheckC => {
            (m.db, m.arith) = (inst.c, ROW_BOOL);
            (C_ROW, super::strs::type_check_cv as *const (), 1)
        }
        TOp::IssetC => {
            (m.db, m.arith) = (inst.c, ROW_BOOL);
            (C_ROW, super::strs::isset_cv as *const (), 1)
        }
        TOp::TypeCheckJmpC => {
            m.mask = ROW_BR_RET;
            (C_ROWBR, super::strs::type_check_jmp_cv as *const (), 2)
        }
        TOp::IssetJmpC => {
            m.mask = ROW_BR_RET;
            (C_ROWBR, super::strs::isset_jmp_cv as *const (), 2)
        }
        // The operators of two numbers the trace has no code for: the
        // comparisons (a bool), `<=>` (an int), `/` (an int or a float).
        // The handler reads its operands where they lie.
        TOp::NumBinT => {
            m.db = inst.c;
            m.arith = match inst.e {
                7..=12 => ROW_BOOL,
                13 => ROW_INT,
                _ => ROW_VAL,
            };
            (C_ROW, super::scalar::num_bin_tmp as *const (), 1)
        }
        // A literal that is no int, bool or null (a string, a float, an
        // array): a copy the temporary owns.
        TOp::MoveT => {
            (m.db, m.arith) = (inst.c, ROW_VAL);
            if inst.k == super::scalar::MOVE_TMP {
                m.sb = inst.a;
            }
            (C_ROW, super::scalar::move_tmp as *const (), 1)
        }
        // `a . b` into a temporary (its index in `p`): strings and ints.
        // An operand that is a temporary (its index in `a` or `b`) is
        // consumed.
        TOp::ConcatT => {
            let slot = |index: u32| index.checked_shl(SLOT_SHIFT);
            (m.db, m.arith) = (slot(u32::try_from(inst.p).ok()?)?, ROW_VAL);
            if super::dims::dim_kind(inst) == super::dims::KIND_TMP {
                m.sb = slot(inst.a)?;
            }
            if super::dims::val_kind(inst) == super::dims::KIND_TMP {
                m.slot = slot(inst.b)?;
            }
            m.inc = true;
            (C_ROW, jit_row_concat_tmp as *const (), 1)
        }
        // `??` of a temporary (`a`): what it holds, unless null, moves
        // into the result temporary (`b`) and the row jumps; the tested
        // temporary is left null either way.
        TOp::CoalesceT => {
            m.mask = ROW_BR_RET;
            (m.sb, m.db, m.arith) = (inst.a, inst.b, ROW_VAL);
            (C_ROWBR, jit_row_coalesce_tmp as *const (), 1)
        }
        // `$cv .= value`: the variable (its index in `a`) is written, a
        // value temporary (its index in `p`) consumed.
        TOp::ConcatCv => {
            (m.db, m.arith) = (inst.c, ROW_VAL);
            if super::dims::val_kind(inst) == super::dims::KIND_TMP {
                m.sb = u32::try_from(inst.p).ok()?.checked_shl(SLOT_SHIFT)?;
            }
            m.lit_c = i64::from(inst.a.checked_shl(SLOT_SHIFT)?);
            m.inc = true;
            (C_ROW, super::dims::concat_cv as *const (), 1)
        }
        // The operands in `a`, `b` and `d` (their kinds two bits each in
        // `k`, 1: a temporary, consumed), the result in the low half of
        // `p`; the row covers the `Nop`s behind an op at its `InitFCall`.
        TOp::FramelessQ => {
            let Some(Op::FramelessICall(fc)) = code.ops.get(i) else {
                return None;
            };
            m.db = u32::try_from(inst.p as u64 & 0xffff_ffff).ok()?;
            m.arith = ROW_VAL;
            let offs = [inst.a, inst.b, inst.d];
            let mut consumed = [NO_DST; 3];
            for (k, slot) in consumed
                .iter_mut()
                .enumerate()
                .take(usize::from(inst.k >> 6))
            {
                if (inst.k >> (2 * k)) & 3 == 1 {
                    *slot = offs[k];
                }
            }
            [m.sb, m.slot, m.sw] = consumed;
            m.inc = true;
            // Done, it jumps to the op after the call, which is where its
            // row in a trace ends: only a decline is told apart.
            m.mask = ROW_BR_RET;
            (
                C_ROW,
                super::strs::frameless_quick as *const (),
                1 + fc.tail(),
            )
        }
        _ => return None,
    };
    m.class = class;
    m.lit_a = i64::try_from(handler as usize).ok()?;
    Some((class, m, step))
}

/// Whether the compiled variable at byte offset `off` of the frame a plan
/// is made in holds an array right now: the rows that read it then take an
/// array too (a loop's variables keep their types from one iteration to
/// the next far more often than not; a row meeting anything else leaves the
/// trace). `false` where there is no frame (a callee's body is planned
/// before it runs).
fn cv_holds_array(st: &St, off: u32) -> bool {
    // SAFETY: `off` is a variable offset the tape validated for the frame
    // `st` describes, whose slots are alive while the plan is made; the
    // first byte of a slot is its tag.
    !st.cvs.is_null() && unsafe { *st.cvs.add(off as usize) } == TAG_ARRAY
}

/// What the compiled variable at byte offset `off` of the frame a plan is
/// made in holds right now; `None` where there is no frame (a callee's
/// body is planned before it runs) or the variable is undefined.
#[allow(clippy::cast_ptr_alignment)] // a slot offset from a slot vector's base
fn cv_now(st: &St, off: u32) -> Option<&php_runtime::Value> {
    if st.cvs.is_null() {
        return None;
    }
    // SAFETY: `off` is a variable offset the tape validated for the frame
    // `st` describes, whose slots are alive while the plan is made.
    unsafe { (*st.cvs.add(off as usize).cast::<crate::vm::CvSlot>()).get() }
}

/// Whether operand (`kind`, `off`) of a comparison is a compiled variable
/// that holds a float right now: the row is then decided by its operands'
/// tags ([`J_CMPS`]), not as two ints.
fn cv_holds_float(st: &St, kind: u8, off: u32) -> bool {
    kind == KC && matches!(cv_now(st, off), Some(php_runtime::Value::Float(_)))
}

/// Whether the element rows of the compiled variable at `off` take the
/// wider helpers ([`jit_dim_get_wide`], [`jit_dim_set_wide`]): it holds a
/// reference right now (a by-reference parameter, a `global`: the array
/// is behind it), or an array whose first element is a float, a bool or
/// null (a list of ints keeps the int helpers, whose results the trace
/// computes with unchecked).
fn dim_wide_now(st: &St, off: u32) -> bool {
    use php_runtime::Value;
    match cv_now(st, off) {
        Some(Value::Reference(_)) => true,
        Some(Value::Array(a)) => matches!(
            a.values().next(),
            Some(Value::Float(_) | Value::Bool(_) | Value::Null)
        ),
        _ => false,
    }
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
///
/// A compiled variable read as an int gets an exact entry guard, which
/// holds for the trace's whole run (every row that writes such a variable
/// keeps it an int, or the region is refused). A temporary gets none: `ts`
/// says what the rows before this one left in it on every path here, and
/// one whose content is not known that way is checked by the row itself
/// (`m.ca`/`m.cb`, set here). `false` refuses the region.
fn add_class_guards(
    tape: &[Inst],
    i: usize,
    class: u16,
    m: &mut JitMeta,
    ts: &TmpState,
    cvbools: &[u32],
    guards: &mut Vec<Guard>,
) -> bool {
    let inst = &tape[i];
    // An operand read as an int: `Some(true)` when the emitted row has to
    // check its tag, `None` when it cannot be one.
    let read = |kind: u8, off: u32, guards: &mut Vec<Guard>| -> Option<bool> {
        match kind {
            KT => match ts.kind(off) {
                // Fully stored as an `Int` on every path here.
                TmpKind::Int => Some(false),
                // A `Bool` or a `Null` is no `Int` at this read.
                TmpKind::Bool | TmpKind::Null => None,
                // Whatever it holds: checked where it is read.
                TmpKind::Scalar | TmpKind::Val | TmpKind::Unknown => Some(true),
            },
            // A compiled variable the region itself wrote a `Bool`
            // into (an `isset()`/`empty()` result assigned to it) is
            // `Bool` at the trace's entry, so an exact guard would
            // fail on every entry: the region is rejected instead.
            KC if cvbools.contains(&off) => None,
            KC => add_slot(guards, kind, off, true).then_some(false),
            KI | KL => Some(false),
            _ => None,
        }
    };
    // A destination whose tag the row keeps as `Int` (`++`, `+=`): a
    // `Bool` the region wrote there makes the exact guard dead.
    let keep_int = |off: u32, guards: &mut Vec<Guard>| -> bool {
        !cvbools.contains(&off) && add_slot(guards, KC, off, true)
    };
    // A destination fully stored with tag and payload: the entry value
    // is never read, so a slot an earlier region row already wrote
    // needs no guard at all; any other gets the weak one (a scalar or
    // nothing: no value the store would have to release).
    // (A temporary that may hold an array is never stored over: the
    // region is refused.)
    let fresh_dst = |off: u32, guards: &mut Vec<Guard>| -> bool {
        match ts.kind(off) {
            TmpKind::Unknown => add_slot(guards, KT, off, false),
            TmpKind::Val => false,
            _ => true,
        }
    };
    // The two operands of an arithmetic or comparison row.
    let both = |m: &mut JitMeta, guards: &mut Vec<Guard>| -> bool {
        let (Some(ca), Some(cb)) = (read(m.ka, inst.a, guards), read(m.kb, inst.b, guards)) else {
            return false;
        };
        m.ca = ca;
        m.cb = cb;
        true
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
        J_ARITH_CV => both(m, guards) && keep_int(inst.c, guards),
        J_ASSIGN_OP | J_NUMBIN_C | J_CMP => both(m, guards),
        J_INCCMP => {
            let Some(cb) = read(m.kb, inst.b, guards) else {
                return false;
            };
            m.cb = cb;
            keep_int(inst.a, guards)
        }
        // `$cv = TMP`: the temporary's kind picks the copy (`plan` flags
        // a `Bool`, a `Null` and a scalar of unknown kind in the meta's
        // `mask`); one the plan knows nothing about is taken for an
        // `Int` and checked by the row. The variable is only written
        // (weak: nothing of its own is read).
        // A temporary that may hold an array goes into a variable the row
        // looks at itself (it may hold an array to release): no guard.
        J_ASSIGN_T => {
            m.cb = ts.kind(inst.b) == TmpKind::Unknown;
            ts.kind(inst.b) == TmpKind::Val || add_slot(guards, KC, inst.a, false)
        }
        // Arithmetic into a temporary: the destination is fully stored
        // (tag and payload), so it only needs the weak guard (a scalar
        // or nothing, what the interpreter's own store test accepts).
        J_ARITH_TMP | J_NUMBIN_T => both(m, guards) && fresh_dst(inst.c, guards),
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
        // entry-guarded. A static-property write follows the same rule.
        C_PROPW | C_PROPOPA | C_SPROPW => {
            (m.sc == KT || m.sc == KL || read(m.sc, m.sw, guards).is_some())
                && (m.db == NO_DST || fresh_dst(m.db, guards))
        }
        // `++`/`--` re-check the base, scope, class and slot (or the
        // pinned cell) every iteration like a write; only the result
        // temporary needs an entry guard (fully stored, tag and payload,
        // by the emitted row).
        C_PROPINC | C_SPROPINC => m.db == NO_DST || fresh_dst(m.db, guards),
        // `isset()`/`empty()`, a static-property read and a folded
        // constant: everything they touch is re-checked by the emitted
        // row (or is nothing at all); the result temporary is fully
        // stored.
        C_PROPISSET | C_SPROP | C_SPROPISSET | C_CONSTI => fresh_dst(m.db, guards),
        // `$cv = a op b`: as `J_ARITH_CV`, the destination (`inst.d`)
        // keeps its `Int` tag and only its payload is stored.
        J_NUMBIN_D => both(m, guards) && keep_int(inst.d, guards),
        C_NOP => true,
        // `$cv = v` with the result kept: the source is read as an int,
        // the variable and the result temporary are fully stored.
        C_ASSIGN_R => {
            let Some(cb) = read(m.kb, inst.b, guards) else {
                return false;
            };
            m.cb = cb;
            add_slot(guards, KC, inst.a, false) && fresh_dst(inst.c, guards)
        }
        // `++`/`--` with the result kept: the variable stays an int, the
        // result temporary is fully stored.
        C_INCDEC_T => keep_int(inst.a, guards) && fresh_dst(inst.c, guards),
        // A comparison or an `op=` decided by its operands' tags: nothing
        // to guard. (A temporary the region left a bool, a null or a value
        // to release in is no number: the row would leave on every turn.)
        J_CMPS | J_ASSIGN_OPS => {
            let number = |kind: u8, off: u32| {
                kind != KT || !matches!(ts.kind(off), TmpKind::Bool | TmpKind::Null | TmpKind::Val)
            };
            number(m.ka, inst.a) && number(m.kb, inst.b)
        }
        // `abs(x)`: the operand is read as an int; a result temporary is
        // fully stored.
        C_ABS => {
            let Some(ca) = read(m.ka, inst.a, guards) else {
                return false;
            };
            m.ca = ca;
            inst.c == u32::MAX || fresh_dst(inst.c, guards)
        }
        // The array rows hand their operands to a helper by address, and
        // the helper checks every one of them on every iteration (a
        // wrong type declines, which leaves the row to the interpreter):
        // nothing to guard at the entry.
        C_DIMR | C_DIMW | C_DIMOP => true,
        // A value moved into a temporary: the row checks a compiled
        // variable's tag itself (any scalar), and a temporary's when the
        // plan does not know what it holds; the destination is fully
        // stored.
        // (A kept temporary that may hold an array would need one more
        // handle on it: the interpreted row's.)
        C_MOVE => {
            m.ca = m.sc == KT && ts.kind(inst.a) == TmpKind::Unknown;
            !(m.sc == KT && !m.consume && ts.kind(inst.a) == TmpKind::Val)
                && fresh_dst(inst.c, guards)
        }
        // The truth jumps look at their variable's tag in the trace.
        C_JMPZ | C_JMPNZ => true,
        // A handler checks its operands, and declines a result temporary
        // that holds a value to release.
        C_ROW | C_ROWBR => true,
        // A jump on a temporary takes a bool, or an int where the plan does
        // not know what it holds (the row looks at the tag then).
        C_JMPZT | C_JMPNZT => {
            m.ca = ts.kind(inst.a) != TmpKind::Bool;
            true
        }
        // The handler checks its operands and releases what its temporary
        // held.
        C_DIMSR | C_DIMKEY => true,
        // A read through the global scope fully stores its temporary; the
        // variable of a folded assignment is looked at by the read itself
        // (it releases what the variable holds).
        C_VMREAD | C_GREAD => m.kc == KC || fresh_dst(m.db, guards),
        // `?:` and `??` as well, and fully store their result temporary
        // (`inst.b`) when they jump.
        C_JMPSET | C_COALESCE => fresh_dst(inst.b, guards),
        _ => false,
    }
}

/// The calling scope of the frame a plan is made in, as a class address
/// (0: none).
fn frame_scope(st: &St) -> usize {
    // SAFETY: the frame outlives the plan (the trace runs in it).
    unsafe { &*st.f }
        .scope
        .as_ref()
        .map_or(0, |s| Rc::as_ptr(s) as usize)
}

/// Whether `c` is a class of the instance-property rows an inlined method
/// may contain ([`plan_callee_prop`]).
fn is_callee_prop(c: u16) -> bool {
    matches!(c, C_PROP | C_PROPW | C_PROPINC | C_PROPOPA)
}

/// A property row of a method being inlined (`row` of the callee's tape,
/// `code` its op array): a read, write, `++`/`--` or `op=` of a property
/// of its `$this`, planned as the same row of a region is, from the
/// callee's own property cache. That entry must have been filled for the
/// scope the method runs in (`scope`) and for the class of the call's
/// receiver (`class_addr`, which the call's guard pins): then the
/// receiver is the object the row works on, and the row is `bare`: what
/// it needs checked is checked once before the method's first row. `rk`
/// and `roff` are the receiver as the caller holds it (`KTHIS`, or `KC`
/// and its slot).
fn plan_callee_prop(
    code: &crate::opcode::OpArray,
    row: &Inst,
    scope: usize,
    class_addr: usize,
    rk: u8,
    roff: u32,
) -> Option<(u16, JitMeta)> {
    let (c, mut m) = match row.op {
        TOp::FetchObjProp | TOp::FetchObjPropTo => (C_PROP, plan_prop(scope, code, row)?),
        TOp::AssignObjProp => (C_PROPW, plan_prop_write(scope, code, row)?),
        TOp::IncDecObjProp => (C_PROPINC, plan_prop_incdec(scope, code, row)?),
        TOp::AssignOpObjProp => (C_PROPOPA, plan_prop_opa(scope, code, row)?),
        _ => return None,
    };
    // Only the method's own `$this`, and only a receiver the trace can
    // load again (its `$this` through the frame, or a variable).
    if m.ka != KTHIS || !matches!(rk, KTHIS | KC) {
        return None;
    }
    // The entry answers the class it was filled for: another receiver
    // class (the method is inherited, the site is polymorphic) may have
    // filled it since.
    if m.lit_a != class_addr as i64 - i64::from(RC_VALUE_OFFSET) {
        return soft();
    }
    m.class = c;
    m.ka = rk;
    m.sb = roff;
    m.bare = true;
    Some((c, m))
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
fn plan_prop(scope: usize, code: &crate::opcode::OpArray, inst: &Inst) -> Option<JitMeta> {
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
    // The scope the row runs in (the frame the plan is made in, or the
    // inlined method's): what the entry must have been filled for, and
    // what the guard compares against (0 for none).
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
fn plan_prop_write(scope: usize, code: &crate::opcode::OpArray, inst: &Inst) -> Option<JitMeta> {
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
    // The scope the row runs in (the frame the plan is made in, or the
    // inlined method's): what the entry must have been filled for, and
    // what the guard compares against (0 for none).
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
fn prop_common(
    scope: usize,
    code: &crate::opcode::OpArray,
    inst: &Inst,
    want: u8,
) -> Option<JitMeta> {
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
    // The scope the row runs in (the frame the plan is made in, or the
    // inlined method's): what the entry must have been filled for, and
    // what the guard compares against (0 for none).
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
fn plan_prop_incdec(scope: usize, code: &crate::opcode::OpArray, inst: &Inst) -> Option<JitMeta> {
    let Some(Op::IncDecObjProp { place, kind, dst }) = code.ops.get(inst.d as usize) else {
        return None;
    };
    if *place != inst.a {
        return None;
    }
    let mut m = prop_common(scope, code, inst, PC_READ | PC_WRITE)?;
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
fn plan_prop_isset(scope: usize, code: &crate::opcode::OpArray, inst: &Inst) -> Option<JitMeta> {
    let Some(Op::IssetObjProp { place, dst, empty }) = code.ops.get(inst.d as usize) else {
        return None;
    };
    if *place != inst.a {
        return None;
    }
    let mut m = prop_common(scope, code, inst, PC_ISSET)?;
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
fn plan_prop_opa(scope: usize, code: &crate::opcode::OpArray, inst: &Inst) -> Option<JitMeta> {
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
    let mut m = prop_common(scope, code, inst, PC_READ | PC_WRITE)?;
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

thread_local! {
    /// The keep-alive of every closure object a plan inlined a call of,
    /// by its handle word: a trace's guard names that word for good, and
    /// an object freed and another allocated at its address would pass
    /// the guard with another body. Only closures that bind and capture
    /// nothing are inlined, so nothing but the small object is kept.
    static PINNED_OBJS: RefCell<Vec<(u64, php_runtime::Obj)>> =
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
    // (Outside the counted heap: a plan must not show in
    // `memory_get_usage()`.)
    php_memlimit::persistent(|| PINNED_CELLS.with(|p| p.borrow_mut().push(e.cell.clone())));
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

/// The run-time shape of a plain `Foo::$x++` (the tape's `IncDecStatic`
/// row: an `IncDec` of a static-property place): the pinned cell of a
/// `fast_plain` entry, as for a write, holding an `Int` (re-checked per
/// iteration). An overflow leaves before any store (the interpreted row
/// makes the float). `inc` carries the direction and `kc` the post-ness.
fn plan_sprop_incdec(st: &St, code: &crate::opcode::OpArray, i: usize) -> Option<JitMeta> {
    let Some(Op::IncDec { place, kind, dst }) = code.ops.get(i) else {
        return None;
    };
    let mut m = sprop_common(st, code, *place, true)?;
    m.class = C_SPROPINC;
    m.db = prop_dst(code, *dst)?;
    m.inc = kind.is_inc();
    m.kc = u8::from(kind.is_post());
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

/// The run-time shape of `$this::NAME` (the tape's `ThisConst` /
/// `ThisConstTo` row at `i`: the `FetchThis` there and the class-constant
/// read of its temporary after it) when the constant folds as
/// [`plan_const`] folds one: the value the constant's cache holds for the
/// class of the frame's `$this` (`rtcache::class_const_hit_obj_in`, the
/// hit the interpreted row takes). That value is the class's, as seen
/// from the frame's scope: the emitted row re-checks both on every
/// iteration (`ka == KTHIS` marks the class check, `lit_c` the class).
fn plan_this_const(st: &St, code: &crate::opcode::OpArray, i: usize) -> Option<JitMeta> {
    // SAFETY: the frame outlives the plan (the trace runs in it).
    let f = unsafe { &*st.f };
    let (name, dst, to) = match code.ops.get(i + 1) {
        Some(Op::FetchClassConst {
            class: ClassSpec::Dyn(_),
            name: Operand::Lit(n),
            dst,
        }) => (*n, *dst, None),
        Some(Op::FetchClassConstTo {
            class: ClassSpec::Dyn(_),
            name,
            dst,
            to,
        }) => (*name, *dst, Some(*to)),
        _ => return None,
    };
    let this = f.this.as_ref()?;
    let v = crate::rtcache::class_const_hit_obj_in(f, this, name).or_else(soft)?;
    let (kind, v) = match v {
        php_runtime::Value::Int(v) => (CONST_INT, v),
        php_runtime::Value::Null => (CONST_NULL, 0),
        php_runtime::Value::Bool(b) => (CONST_BOOL, i64::from(b)),
        _ => return None,
    };
    if let Some(to) = to {
        fold_ok(code, i + 1, to, dst)?;
    }
    let word = |addr: usize| addr as i64 - i64::from(RC_VALUE_OFFSET);
    Some(JitMeta {
        class: C_CONSTI,
        lit_a: v,
        arith: kind,
        db: prop_dst(code, Some(dst))?,
        sg: true,
        lit_b: f.scope.as_ref().map_or(0, |s| word(Rc::as_ptr(s) as usize)),
        ka: KTHIS,
        lit_c: word(Rc::as_ptr(crate::object::class_of(this)) as usize),
        ..JitMeta::default()
    })
}

/// Plan one lowered `Ext` row, or one of the tape's `ClassConst`,
/// `ClassConstTo`, `StaticProp` and `StaticPropTo` rows: the tape keeps
/// the raw op at its own index (`lower` emits one instruction per op), so
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
        // `func_num_args();` with its result unused: the op stores
        // nothing and has no other effect, so the row is no code. (With a
        // result it reads the frame's argument count: the interpreted
        // row's, for now.)
        Some(Op::FuncNumArgs { dst: None }) => JitMeta {
            class: C_NOP,
            kb: KI,
            ..JitMeta::default()
        },
        _ => return None,
    };
    Some((m.class, m))
}

/// Whether `op` is an array row the plan may inline ([`plan_dim`]).
fn is_dim_row(op: TOp) -> bool {
    matches!(
        op,
        TOp::FetchDimIC
            | TOp::FetchDimIT
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
        TOp::FetchDimIC | TOp::FetchDimIT | TOp::FetchDimSC | TOp::FetchDimC => {
            // A string's offsets are the interpreter's: the row's helpers
            // decline a variable that holds no array, so a trace would
            // leave at this row in every iteration until it is switched
            // off, its compile wasted (`read_str_offset` in
            // Zend/micro_bench.php).
            if matches!(cv_now(st, sb), Some(php_runtime::Value::Str(_))) {
                return None;
            }
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
            // An offset that may be an int, into what takes the wider
            // helper right now.
            let str_key = dk == DIM_LIT
                && matches!(
                    code.literals.get(inst.b as usize),
                    Some(php_runtime::Value::Str(_))
                );
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
                arr: !str_key && dim_wide_now(st, sb),
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
                // A plain write into what takes the wider helper right
                // now, or of a variable that holds a float.
                arr: class == C_DIMW && (dim_wide_now(st, sb) || cv_holds_float(st, sc, sw)),
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
            // The destination slot, exclusively: the frame's other slots
            // are read through `arr`/`key`, never this one's value.
            let slot = &mut *dst.cast::<crate::vm::CvSlot>();
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

/// The builtin call that ends an inlined body ([`KQUICK`]): the result of
/// builtin `id` (`special::quick_eval`) for the int in the scratch slot
/// `arg` (its payload: the rows of an inlined body store no tag there),
/// into the caller's result temporary `dst` when that holds nothing
/// whose release could do more than free a string. Returns 0 having stored
/// it; 1, nothing changed, when the builtin needs its handler for this
/// argument (the interpreter then makes the whole call).
///
/// # Safety
/// `arg` is a slot of the trace's scratch buffer, `dst` a temporary of the
/// running frame.
#[allow(clippy::cast_ptr_alignment)] // slots: 16-byte values, 8-aligned
unsafe extern "C" fn jit_quick_tail(
    arg: *const u8,
    dst: *mut u8,
    _unused: *const u8,
    id: u32,
) -> u32 {
    use php_runtime::Value;
    // SAFETY: the caller's contract.
    unsafe {
        // An int the plan proved (a body row's result): only its payload
        // is stored in a scratch slot, no tag.
        let i = std::ptr::read_unaligned(arg.add(PAYLOAD as usize).cast::<i64>());
        #[allow(clippy::cast_possible_truncation)] // an id is a byte
        let Some(v) = crate::special::quick_eval(id as u8, &Value::Int(i)) else {
            return 1;
        };
        let slot = &mut *dst.cast::<Value>();
        if !slot.is_plain() || !matches!(v, Value::Int(_) | Value::Float(_)) {
            return 1;
        }
        match std::mem::replace(slot, v) {
            old @ Value::Str(_) => drop(old),
            old => std::mem::forget(old),
        }
        0
    }
}

/// [`jit_dim_get_wide`]'s and [`jit_dim_set_wide`]'s `word`: the array
/// variable's index, and for a read whether the destination is a compiled
/// variable (not a temporary).
const DIM_WIDE_CV: u32 = (1 << 31) - 1;
const DIM_WIDE_TO_CV: u32 = 1 << 31;

/// `C_DIMR`'s helper for a variable the plan found holding a reference, or
/// an array of floats (`JitMeta.arr`): `$dst = $cv[$idx]` for an `Int`
/// offset, the array held directly or through a reference (the element
/// the tape's own second step finds, `dims::int_elem`: a list's by
/// position, a hash's by key), when the element is null, a bool, an int or
/// a float and the destination holds nothing whose release could do more
/// than free a string ([`jit_dim_get`]'s test). Returns 0 having stored
/// the element; 1, nothing changed, for anything else.
///
/// # Safety
/// `frame` is the running frame, `idx` a slot of it or a literal of its op
/// array, `dst` a temporary of it, or (`word` has [`DIM_WIDE_TO_CV`]) a
/// compiled variable; `word`'s low bits name one of its compiled
/// variables.
#[allow(clippy::cast_ptr_alignment)] // slots: 16-byte values, 8-aligned
unsafe extern "C" fn jit_dim_get_wide(
    frame: *mut crate::vm::Frame,
    idx: *const u8,
    dst: *mut u8,
    word: u32,
) -> u32 {
    use php_runtime::Value;
    // SAFETY: the caller's contract. The element is looked at where it
    // lies and its bytes are taken out (a null, a bool, an int or a float
    // owns nothing: its 16 bytes are the value, in a temporary and in a
    // variable's `Some` alike, the layout tests' `value_layout`) before
    // the destination, another slot of the frame, is written; the
    // reference's borrow has ended by then.
    unsafe {
        let Some(i) = slot_int(idx) else {
            return 1;
        };
        let v = {
            let f = &*frame;
            let Some(held) = f
                .cvs
                .get((word & DIM_WIDE_CV) as usize)
                .and_then(crate::vm::CvSlot::get)
            else {
                return 1;
            };
            let through;
            let arr = match held {
                Value::Array(arr) => arr,
                Value::Reference(r) => {
                    let Some(inner) = r.try_borrow() else {
                        return 1;
                    };
                    through = inner;
                    match &*through {
                        Value::Array(arr) => arr,
                        _ => return 1,
                    }
                }
                _ => return 1,
            };
            let Some(elem) = super::dims::int_elem(arr, i) else {
                return 1;
            };
            if !matches!(
                elem,
                Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
            ) {
                return 1;
            }
            std::ptr::read(elem)
        };
        // What the destination holds, by its tag: a scalar (or nothing, in
        // a variable) is written over, a string released first.
        match *dst {
            TAG_NULL | TAG_BOOL | TAG_INT | TAG_FLOAT => {}
            TAG_NONE if word & DIM_WIDE_TO_CV != 0 => {}
            TAG_STR if word & DIM_WIDE_TO_CV != 0 => {
                drop((*dst.cast::<crate::vm::CvSlot>()).set(None));
            }
            TAG_STR => drop(std::mem::replace(&mut *dst.cast::<Value>(), Value::Null)),
            _ => return 1,
        }
        std::ptr::write(dst.cast::<Value>(), v);
        0
    }
}

/// `C_DIMW`'s helper for such a variable, or for a value that is no int:
/// `$cv[$idx] = $val` (`idx` null: `$cv[] = $val`) for an `Int` offset and
/// a value that is null, a bool, an int or a float, the array held alone,
/// directly or through a reference (`dims::write_wide`, the tape's own
/// second step: over a plain element or as a new one). Returns 0 having
/// written; 1, nothing changed, for anything else.
///
/// # Safety
/// As [`jit_dim_get_wide`]: `idx` (when not null) and `val` slots or
/// literals.
#[allow(clippy::cast_ptr_alignment)] // slots: 16-byte values, 8-aligned
unsafe extern "C" fn jit_dim_set_wide(
    frame: *mut crate::vm::Frame,
    idx: *const u8,
    val: *const u8,
    word: u32,
) -> u32 {
    use php_runtime::Value;
    // SAFETY: the caller's contract; the operands are read before the
    // frame is borrowed for the write. A scalar's bytes are its copy (a
    // variable's `Some` has its value's layout).
    unsafe {
        let v = match *val {
            TAG_NULL | TAG_BOOL | TAG_INT | TAG_FLOAT => std::ptr::read(val.cast::<Value>()),
            _ => return 1,
        };
        let key = if idx.is_null() {
            None
        } else {
            match slot_int(idx) {
                Some(i) => Some(i),
                None => return 1,
            }
        };
        // (A value given back is a scalar: nothing to release.)
        u32::from(super::dims::write_wide(&mut *frame, word & DIM_WIDE_CV, key, v).is_err())
    }
}

/// [`jit_dim_get`] for the commonest string-key read, `$v = $arr['key']`:
/// the key a string literal that is no integer key, the destination the
/// variable of the folded assignment. The same answers, without the
/// dispatch on what kind of offset and destination it was given.
///
/// # Safety
/// `arr` and `dst` are compiled variables' slots of the running frame,
/// `key` a literal of its op array.
unsafe extern "C" fn jit_dim_get_key(
    arr: *const crate::vm::CvSlot,
    key: *const php_runtime::Value,
    dst: *mut crate::vm::CvSlot,
    _word: u32,
) -> u32 {
    use php_runtime::Value;
    // SAFETY: the caller's contract; the element is copied out before the
    // destination is written.
    unsafe {
        let (Some(Value::Array(a)), Value::Str(k)) = ((*arr).get(), &*key) else {
            return 1;
        };
        let Some(Value::Int(v)) = a.get_zstr(k) else {
            return 1;
        };
        let v = *v;
        // The destination slot, exclusively (as above).
        let slot = &mut *dst;
        match slot.get() {
            // Nothing or a scalar: it owns nothing.
            None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)) => {
                std::mem::forget(slot.set(Some(Value::Int(v))));
            }
            // A string is released.
            Some(Value::Str(_)) => drop(slot.set(Some(Value::Int(v)))),
            Some(_) => return 1,
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
    use php_runtime::Value;
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
            // One probe of the table: the store over a plain element, or
            // the new key (an element holding anything else declines).
            Some(i) => u32::from(tbl.set_int_plain(i, Value::Int(v)).is_err()),
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

/// `$cv[d1][d2]...` read for a trace ([`C_DIMSR`]): the tape's handler
/// for the row `inst` (`dims::fetch_dims`), with the result always left in
/// the row's temporary (the folded `AssignCv` of the row, if it has one,
/// is its own row in the trace). Returns 0 when the read was made, 1 when
/// the handler declined (nothing changed: the interpreter runs the row).
///
/// # Safety
/// `frame` is the running frame and `inst` a `FetchDimsC` row of its tape.
unsafe extern "C" fn jit_row_fetch_dims(
    frame: *mut crate::vm::Frame,
    inst: *const Inst,
    _unused: *const u8,
    _word: u32,
) -> u32 {
    // SAFETY: the caller's contract.
    let mut row = unsafe { *inst };
    row.b = super::dims::NO_TO;
    // The lean path here, without the call into the handler.
    // SAFETY: the caller's contract; `frame` is the running frame.
    if super::dims::fetch_packed_two(unsafe { &mut *frame }, &row).is_some() {
        return 0;
    }
    // SAFETY: as above; the copy differs from the tape's row only in where
    // the value goes.
    match unsafe { super::dims::fetch_dims_general(frame, &row) } {
        super::Br::Decline => 1,
        _ => 0,
    }
}

/// The meta of a `VmOp` row that is a read through the global scope, and
/// the ops it covers: `Op::FetchGlobal` (two ops with the assignment
/// folded behind it, the variable then in `db` with `kc == KC`) or the read
/// of a superglobal as a whole (`Op::Fetch`), the result temporary in `db`.
/// `None` for any other op.
fn plan_vm_read(code: &crate::opcode::OpArray, i: usize) -> Option<(JitMeta, usize)> {
    let tmp = |t: u32| (t < code.num_tmps).then_some(t << SLOT_SHIFT);
    let (kc, db, step) = match *code.ops.get(i)? {
        Op::FetchGlobal {
            to: Some(cv),
            mode: FetchMode::Read | FetchMode::Is,
            ..
        } => {
            if cv as usize >= code.cv_names.len() {
                return None;
            }
            (KC, cv << SLOT_SHIFT, 2)
        }
        Op::FetchGlobal {
            dst,
            to: None,
            mode: FetchMode::Read | FetchMode::Is,
            ..
        }
        | Op::Fetch {
            dst,
            mode: FetchMode::Read,
            ..
        } => (KT, tmp(dst)?, 1),
        _ => return None,
    };
    let m = JitMeta {
        class: C_VMREAD,
        ka: KI,
        kb: KI,
        kc,
        db,
        ..JitMeta::default()
    };
    Some((m, step))
}

/// What the slot a global read at op `at` would read holds right now (its
/// tag): the plan's look at it. `None` when no VM is named, there is no
/// frame, or the slot is not resolved.
fn global_tag_now(st: &St, at: usize) -> Option<u8> {
    let vm = TRACE_VM.with(Cell::get);
    if vm.is_null() || st.f.is_null() {
        return None;
    }
    // SAFETY: `TRACE_VM` names the VM of the frame the plan is made in
    // ([`back_edge_slow`] set it on the way here), which nothing else uses
    // meanwhile; the address returned is of a live slot, whose first byte
    // is its tag.
    unsafe {
        let addr = super::global_slot_addr(&*vm, &*st.f, at);
        (addr != 0).then(|| *(addr as *const u8))
    }
}

/// The entry helper of a trace with [`C_GREAD`] rows: for each of the `n`
/// rows, the address of the slot its read reads now (0: not resolved) into
/// `table`. `ops` holds the rows' op indexes. Always returns 0.
///
/// # Safety
/// `frame` is the frame the trace is entered in, `table` and `ops` the
/// trace's own table (`n` words, then `n` op indexes), and [`TRACE_VM`]
/// names the frame's VM or is null, as for [`jit_row_vm_read`].
unsafe extern "C" fn jit_resolve_globals(
    frame: *mut crate::vm::Frame,
    table: *mut usize,
    ops: *const u32,
    n: u32,
) -> u32 {
    let vm = TRACE_VM.with(Cell::get);
    for k in 0..n as usize {
        // SAFETY: the caller's contract. The VM is only read.
        unsafe {
            let at = *ops.add(k) as usize;
            let addr = if vm.is_null() {
                0
            } else {
                super::global_slot_addr(&*vm, &*frame, at)
            };
            table.add(k).write(addr);
        }
    }
    0
}

/// [`C_VMREAD`]'s helper: the read at op `at` of the frame's code on its
/// fast path (`tape::vm_read`). Returns 0 when the value is stored (in the
/// row's temporary, or in the variable of the folded assignment), 1 when
/// the row is the interpreter's, nothing changed: a miss of the run-time
/// cache, an undefined variable read with a warning, a variable bound by
/// reference (the assignment writes through it), no VM named.
///
/// # Safety
/// `frame` is the frame the trace runs in, and [`TRACE_VM`] names the VM
/// it belongs to or is null ([`back_edge_slow`] sets it from the context
/// `run_tape` filled before it entered the core loop this trace was
/// entered from). `run_tape` is suspended in that loop meanwhile, and the
/// loop holds nothing of the VM but the frame's slot bases, which the read
/// leaves where they are (it stores one value into a slot).
unsafe extern "C" fn jit_row_vm_read(
    frame: *mut crate::vm::Frame,
    at: usize,
    _unused: *const u8,
    _word: u32,
) -> u32 {
    let vm = TRACE_VM.with(Cell::get);
    if vm.is_null() {
        return 1;
    }
    // The op's operands, copied out of the frame's code before the VM is
    // used (the frame is not borrowed across the read).
    let (place, dst, to, mode) = {
        // SAFETY: the caller's contract: the frame the trace runs in.
        let f = unsafe { &*frame };
        match f.code.ops.get(at) {
            Some(&Op::FetchGlobal {
                place,
                dst,
                to,
                mode,
            }) => {
                // A variable bound by reference takes the value through
                // its assignment, which the row covers: the interpreter's.
                let by_ref = to.is_some_and(|cv| {
                    matches!(
                        f.cvs.get(cv as usize).and_then(crate::vm::CvSlot::get),
                        Some(php_runtime::Value::Reference(_))
                    )
                });
                if by_ref {
                    return 1;
                }
                (place, dst, to, Some(mode))
            }
            Some(&Op::Fetch {
                place,
                mode: FetchMode::Read,
                dst,
            }) => (place, dst, None, None),
            _ => return 1,
        }
    };
    // SAFETY: the caller's contract: the VM the frame belongs to, which
    // nothing else uses while the trace runs.
    let vm = unsafe { &mut *vm };
    u32::from(super::vm_read(vm, place, dst, to, mode).is_none())
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
    let scope = frame_scope(st);
    let mut class = vec![0u16; len];
    let mut meta = vec![JitMeta::default(); len];
    let mut span = vec![0u32; len];
    let mut calls: Vec<Option<CallPlan>> = (0..len).map(|_| None).collect();
    // The `C_GREAD` rows so far: each one's index in the trace's table of
    // global slots is its rank among them.
    let mut greads = 0u32;

    // Class every instruction; a fused instruction's coverage marks
    // the ops it consumes, exactly as `run_core` skips them.
    let mut i = head;
    let mut last = usize::MAX;
    while i <= close {
        let (c, m, step, call) = if tape[i].op == TOp::CallInit {
            if tape[i].k == super::calls::CALL_NEW {
                // The tape's own `new` instruction, through its handler.
                let Some((m, step)) = plan_new(code, tape, i) else {
                    rej!(format!("[{i}] new not a handler row"));
                };
                (C_ROW, m, step, None)
            } else {
                let Some(cp) = plan_call(st, code, i) else {
                    rej!(format!("[{i}] call not inlinable"));
                };
                let span = cp.span;
                (C_CALL, JitMeta::default(), span, Some(cp))
            }
        } else if matches!(tape[i].op, TOp::FetchObjProp | TOp::FetchObjPropTo) {
            // A property read never spans: the folded `AssignCv` of a
            // `FetchObjPropTo` is scanned and emitted as its own row
            // (the read stores into its temporary and that row moves
            // it, mirroring the interpreted fall-through).
            match plan_prop(scope, code, &tape[i]) {
                Some(m) => (C_PROP, m, 1, None),
                None => rej!(format!("[{i}] property read not inlinable")),
            }
        } else if tape[i].op == TOp::AssignObjProp {
            match plan_prop_write(scope, code, &tape[i]) {
                Some(m) => (C_PROPW, m, 1, None),
                None => rej!(format!("[{i}] property write not inlinable")),
            }
        } else if tape[i].op == TOp::IncDecObjProp {
            match plan_prop_incdec(scope, code, &tape[i]) {
                Some(m) => (C_PROPINC, m, 1, None),
                None => rej!(format!("[{i}] property inc/dec not inlinable")),
            }
        } else if tape[i].op == TOp::IssetObjProp {
            match plan_prop_isset(scope, code, &tape[i]) {
                Some(m) => (C_PROPISSET, m, 1, None),
                None => rej!(format!("[{i}] property isset/empty not inlinable")),
            }
        } else if tape[i].op == TOp::AssignOpObjProp {
            match plan_prop_opa(scope, code, &tape[i]) {
                Some(m) => (C_PROPOPA, m, 1, None),
                None => rej!(format!("[{i}] property op= not inlinable")),
            }
        } else if tape[i].op == TOp::IncDecStatic {
            match plan_sprop_incdec(st, code, i) {
                Some(m) => (C_SPROPINC, m, 1, None),
                None => rej!(format!("[{i}] static property ++/-- not inlinable")),
            }
        } else if matches!(tape[i].op, TOp::ThisConst | TOp::ThisConstTo) {
            // The row covers the `FetchThis` and the constant read; the
            // folded `AssignCv` of a `ThisConstTo` is its own row, as after
            // a property read.
            match plan_this_const(st, code, i) {
                Some(m) => (C_CONSTI, m, 2, None),
                None => rej!(format!("[{i}] $this::NAME not foldable")),
            }
        } else if tape[i].op == TOp::VmOp {
            // A read through the global scope; any other `VmOp` (a
            // frameless call with its handler) stays the interpreter's.
            let Some((mut m, step)) = plan_vm_read(code, i) else {
                rej!(format!("[{i}] op with the VM not compiled"));
            };
            // The slot holds a scalar or an array right now: the trace
            // reads it itself. Else the tape's read, through its helper.
            if global_tag_now(st, i).is_some_and(|tag| tag <= TAG_FLOAT || tag == TAG_ARRAY) {
                m.class = C_GREAD;
                m.slot = greads;
                greads += 1;
                (C_GREAD, m, step, None)
            } else {
                (C_VMREAD, m, step, None)
            }
        } else if tape[i].op == TOp::FetchDimsC {
            // Through the tape's handler; the result temporary is `p`.
            let Ok(t) = u32::try_from(tape[i].p) else {
                rej!(format!("[{i}] element read's temporary out of range"));
            };
            if t >= code.num_tmps {
                rej!(format!("[{i}] element read's temporary out of range"));
            }
            let m = JitMeta {
                class: C_DIMSR,
                ka: KI,
                kb: KI,
                db: t << SLOT_SHIFT,
                ..JitMeta::default()
            };
            (C_DIMSR, m, 1, None)
        } else if is_dim_row(tape[i].op) {
            match plan_dim(st, code, &tape[i]) {
                Some((c, m, step)) => (c, m, step, None),
                None => rej!(format!("[{i}] array row not inlinable")),
            }
        } else if matches!(
            tape[i].op,
            TOp::Ext
                | TOp::ClassConst
                | TOp::ClassConstTo
                | TOp::StaticProp
                | TOp::StaticPropTo
                | TOp::ConstT
                | TOp::ConstTo
        ) {
            // A hot spec row the tape left to the interpreter, or one of
            // the tape's class-constant, static-property and run-time
            // constant rows (which run the same cache hits; `plan_const`
            // reads a `FetchConst[To]` whatever row it became): the op
            // itself is still at this index, so the planners
            // decode it (static properties and constants inline; anything
            // else stays the interpreted row).
            match plan_ext(st, code, i) {
                Some((c, m)) => (c, m, 1, None),
                None => rej!(format!("[{i}] spec row not inlinable")),
            }
        } else {
            // No code of the trace's own: the tape's handler, if the row
            // has a thin one.
            if let Some((c, mut m)) = classify(st, &tape[i]) {
                // A comparison of a variable that holds a float right now
                // (not `===`: an int and a float differ there by type;
                // not a loop's closing jump, which `emit` takes for an
                // int row).
                let c = if c == J_CMP
                    && tape[i].k & crate::opcode::cmp_outcome::IDENTITY == 0
                    && jump_target(tape, i) > i
                    && (cv_holds_float(st, m.ka, tape[i].a) || cv_holds_float(st, m.kb, tape[i].b))
                {
                    m.class = J_CMPS;
                    J_CMPS
                } else if c == J_ASSIGN_OP
                    && m.arith <= 2
                    && (cv_holds_float(st, KC, tape[i].a) || cv_holds_float(st, m.kb, tape[i].b))
                {
                    // `$cv op= v` of a float, as the comparison.
                    m.class = J_ASSIGN_OPS;
                    J_ASSIGN_OPS
                } else {
                    c
                };
                (c, m, c_step(c), None)
            } else if let Some((c, m, step)) = plan_row(code, i, &tape[i]) {
                (c, m, step, None)
            } else {
                rej!(format!("{:?} not whitelisted", tape[i].op))
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

    // Every mid-region jump must land on the head, on a region
    // instruction (which gets a label), or beyond the region (an exit
    // that resumes there). Backward jumps to before the head and landings
    // on covered slots are refused. `back_targets` are the labels a
    // backward jump of the region lands on (a nested loop's head).
    let mut entries: Vec<usize> = Vec::new();
    let mut back_targets: Vec<usize> = Vec::new();
    for j in 0..len {
        let c = class[j];
        if c == 0 {
            continue;
        }
        if (c == J_CMP || c == J_CMPS || c == J_INCCMP) && jcc_for(meta[j].mask).is_none() {
            rej!(format!("unknown cmp mask {}", meta[j].mask));
        }
        if is_jump_class(c) {
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
                if y <= head + j && !back_targets.contains(&y) {
                    back_targets.push(y);
                }
            } else if y < head {
                rej!(format!("nested backward jump to {y}"));
            }
        }
    }
    entries.sort_unstable();

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
    let mut frame = class.contains(&C_PROP)
        || class.contains(&C_PROPW)
        || class.contains(&C_PROPINC)
        || class.contains(&C_PROPISSET)
        || class.contains(&C_PROPOPA)
        || class.contains(&C_SPROP)
        || class.contains(&C_SPROPW)
        || class.contains(&C_SPROPISSET)
        || class.contains(&C_SPROPINC)
        || class.contains(&C_CONSTI)
        || class.contains(&C_DIMSR)
        || class.contains(&C_VMREAD)
        || class.contains(&C_GREAD)
        || class.contains(&C_ROW)
        || class.contains(&C_ROWBR)
        || calls
            .iter()
            .flatten()
            .any(|cp| cp.recv.is_some() || cp.rel.is_some());
    let mut guards: Vec<Guard> = Vec::new();
    // What the rows of the region left in its temporaries (`ts`, a
    // [`TmpState`]): carried through the region in program order, with
    // the states of the paths that meet at a label merged there
    // (`incoming`: what each forward jump carries to its target; a label
    // a backward jump lands on is reached by a path this pass has not
    // followed yet, so nothing is known there). `cvbools` are the compiled
    // variables whose current trace-time value is a `Bool`, a `Null` or a
    // scalar of unknown kind the region wrote (no later row may read those
    // as ints), and `ever_nonint` every compiled variable the region
    // writes one into anywhere.
    let mut ts = TmpState::default();
    let mut incoming: Vec<(usize, TmpState)> = Vec::new();
    let mut reachable = true;
    let mut cvbools: Vec<u32> = Vec::new();
    let mut ever_nonint: Vec<u32> = Vec::new();
    // The compiled variables a row moves a value that may be an array
    // into (they get no guard: the row looks at what they hold).
    let mut counted: Vec<u32> = Vec::new();
    for j in 0..len {
        if class[j] == 0 {
            continue;
        }
        if entries.contains(&(head + j)) {
            let mut merged = reachable.then(|| ts.clone());
            for (y, state) in &incoming {
                if *y == head + j {
                    merged = Some(match merged {
                        Some(m) => m.merge(state),
                        None => state.clone(),
                    });
                }
            }
            incoming.retain(|(y, _)| *y != head + j);
            ts = merged.unwrap_or_default();
            if back_targets.contains(&(head + j)) {
                // The state at a nested loop's head is not followed: a
                // temporary holding a value to release there would be
                // taken for free to store over.
                if ts.has_val() {
                    rej!(format!(
                        "a temporary may hold a value to release at the nested loop at {}",
                        head + j
                    ));
                }
                ts.clear();
            }
            reachable = true;
        }
        if class[j] == C_CALL {
            let Some(cp) = calls[j].as_mut() else {
                rej!(format!("no plan for the call at {}", head + j));
            };
            // An argument is read as an `Int`: a temporary or variable the
            // region itself left a `Bool` or `Null` in cannot be one; a
            // temporary of unknown content is checked before it is passed.
            let mut ok = true;
            for arg in &mut cp.sends {
                ok &= match arg.kind {
                    KT => match ts.kind(arg.off) {
                        TmpKind::Int => true,
                        TmpKind::Bool | TmpKind::Null => false,
                        TmpKind::Scalar | TmpKind::Val | TmpKind::Unknown => {
                            arg.chk = true;
                            true
                        }
                    },
                    KC if cvbools.contains(&arg.off) => false,
                    KC => add_slot(&mut guards, arg.kind, arg.off, true),
                    // A dropped send of a compiled variable (`plan_call`
                    // left its slot in `off`): whatever it holds is
                    // unused, but it must hold something - the
                    // interpreted send of an undefined variable warns.
                    KSKIP if arg.lit == 1 => add_defined(&mut guards, arg.off),
                    _ => true,
                };
            }
            if !ok {
                rej!(format!("guards for the call at {}", head + j));
            }
            // A checked temporary is an `Int` from here on (the call
            // copies an int argument; it nulls nothing).
            for arg in &cp.sends {
                if arg.chk {
                    ts.set(arg.off, TmpKind::Int);
                }
            }
            if let Some(d) = cp.dst {
                if !add_slot(&mut guards, KT, d, false) {
                    rej!(format!("guard for the call result at {}", head + j));
                }
                // The result temporary is fully stored: with the `Int`
                // the callee returned, or with the `Null` of a bare
                // `return;` - which is no int for the rows after it
                // (`$x = f();` must store a null, not `int(0)`).
                let kind = match cp.ret.kind {
                    KNULL_ARG => TmpKind::Null,
                    // What the builtin returned: a float or an int.
                    KQUICK => TmpKind::Scalar,
                    _ => TmpKind::Int,
                };
                ts.set(d, kind);
            }
            continue;
        }
        // A comparison with an operand that is no int as far as the plan
        // knows - a temporary an element read left a scalar in, a variable
        // the region stores one in - is decided by its operands' tags (as
        // in the first pass: not `===`, not a backward jump).
        if class[j] == J_CMP {
            let inst = &tape[head + j];
            let scalar = |kind: u8, off: u32| match kind {
                KT => ts.kind(off) == TmpKind::Scalar,
                KC => cvbools.contains(&off),
                _ => false,
            };
            if inst.k & crate::opcode::cmp_outcome::IDENTITY == 0
                && jump_target(tape, head + j) > head + j
                && (scalar(meta[j].ka, inst.a) || scalar(meta[j].kb, inst.b))
            {
                class[j] = J_CMPS;
                meta[j].class = J_CMPS;
            }
        }
        // `$cv op= v` likewise, with such an operand.
        if class[j] == J_ASSIGN_OP && meta[j].arith <= 2 {
            let inst = &tape[head + j];
            let scalar = match meta[j].kb {
                KT => ts.kind(inst.b) == TmpKind::Scalar,
                KC => cvbools.contains(&inst.b),
                _ => false,
            };
            if scalar || cvbools.contains(&inst.a) {
                class[j] = J_ASSIGN_OPS;
                meta[j].class = J_ASSIGN_OPS;
            }
        }
        if matches!(class[j], C_DIMR | C_DIMW | C_DIMOP) {
            let non_int = |kind: u8, off: u32| match kind {
                KT => matches!(ts.kind(off), TmpKind::Bool | TmpKind::Null),
                KC => cvbools.contains(&off),
                _ => false,
            };
            // A write of a value that is a scalar of unknown kind (what
            // an element read of the wider kind left, in a temporary or
            // in a variable) takes the wider helper too.
            if class[j] == C_DIMW
                && !meta[j].arr
                && (non_int(meta[j].sc, meta[j].sw)
                    || (meta[j].sc == KT && ts.kind(meta[j].sw) == TmpKind::Scalar))
            {
                meta[j].arr = true;
            }
            // (The wider helpers take the frame.)
            if meta[j].arr {
                frame = true;
            }
            // An offset or a value the region itself left a `Bool` or a
            // `Null` in is no `Int`: the helper would decline on every
            // iteration, so the region is not worth compiling. (The wider
            // write stores any scalar.)
            // An offset that is no int is a key: the tape's own handler
            // for the row looks it up ([`C_DIMKEY`]; it takes the frame).
            let m = &meta[j];
            if non_int(m.kb, m.slot) || (m.kb == KT && ts.kind(m.slot) == TmpKind::Val) {
                class[j] = C_DIMKEY;
                meta[j].class = C_DIMKEY;
                frame = true;
            } else if class[j] != C_DIMR && !m.arr && non_int(m.sc, m.sw) {
                rej!(format!("array row at {} has a non-int operand", head + j));
            }
        }
        if !add_class_guards(
            tape,
            head + j,
            class[j],
            &mut meta[j],
            &ts,
            &cvbools,
            &mut guards,
        ) {
            rej!(format!("guards at [{}, {:?}]", head + j, tape[head + j].op));
        }
        // An operand the row checks is an `Int` after it (the row left the
        // trace otherwise).
        if !matches!(class[j], C_MOVE | J_ASSIGN_T | C_JMPZT | C_JMPNZT) {
            if meta[j].ca {
                ts.set(tape[head + j].a, TmpKind::Int);
            }
            if meta[j].cb {
                ts.set(tape[head + j].b, TmpKind::Int);
            }
        }
        // A `Bool` or `Null` temporary copied into a compiled variable
        // (`$ok = isset(...)`, `$ok = empty(...)`, `$r = f()` of a
        // function that returns nothing), or a scalar of unknown kind (a
        // ternary's result), writes that value there for the trace's
        // whole run: flag the row so the emitted store writes the right
        // tag and value, and record the variable as not an `Int`.
        if class[j] == J_ASSIGN_T {
            let b = tape[head + j].b;
            let flag = match ts.kind(b) {
                TmpKind::Bool => BOOL_SRC,
                TmpKind::Null => NULL_SRC,
                TmpKind::Scalar => SCALAR_SRC,
                TmpKind::Val => VAL_SRC,
                TmpKind::Int | TmpKind::Unknown => 0,
            };
            if flag == VAL_SRC && !counted.contains(&tape[head + j].a) {
                counted.push(tape[head + j].a);
            }
            // The variable holds a string, an object or a resource right
            // now: the inline store would leave the trace every time.
            let flag = if flag == VAL_SRC && cv_holds_other(st, tape[head + j].a) {
                // (Its helper takes the frame.)
                frame = true;
                ANY_SRC
            } else {
                flag
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
                ts.set(meta[j].db, TmpKind::Int);
            }
            // A folded constant: what the constant is.
            C_CONSTI => {
                let kind = match meta[j].arith {
                    CONST_NULL => TmpKind::Null,
                    CONST_BOOL => TmpKind::Bool,
                    _ => TmpKind::Int,
                };
                ts.set(meta[j].db, kind);
            }
            // A write, an `op=` and a `++`/`--`: the result temporary (if
            // kept) is fully stored with the `Int`; a consumed `Tmp`
            // source is left `TAG_NULL` (the interpreted row nulls it),
            // so it is no longer a known `Int`.
            C_PROPW | C_PROPOPA | C_SPROPW | C_PROPINC | C_SPROPINC => {
                if meta[j].db != NO_DST {
                    ts.set(meta[j].db, TmpKind::Int);
                }
                if meta[j].consume && meta[j].sc == KT {
                    ts.set(meta[j].sw, TmpKind::Unknown);
                }
            }
            // `isset`/`empty` store a `Bool`: a fresh value, never a
            // known `Int`, replacing whatever the slot held.
            C_PROPISSET | C_SPROPISSET => {
                ts.set(meta[j].db, TmpKind::Bool);
            }
            // The source temporary is `TAG_NULL` after this row (the
            // consume), whatever it held.
            J_ASSIGN_T => {
                let b = tape[head + j].b;
                ts.set(b, TmpKind::Unknown);
            }
            // An operand temporary was read as an int (checked by the row
            // where the plan did not know, and left as it is): an int
            // from here on.
            J_ARITH_TMP | J_NUMBIN_T => {
                let inst = &tape[head + j];
                for (kind, off) in [(meta[j].ka, inst.a), (meta[j].kb, inst.b)] {
                    if kind == KT {
                        ts.set(off, TmpKind::Int);
                    }
                }
                ts.set(inst.c, TmpKind::Int);
            }
            C_ABS => {
                let c = tape[head + j].c;
                if c != u32::MAX {
                    ts.set(c, TmpKind::Int);
                }
            }
            // An array read stored an `Int` into its destination (or left
            // the trace): a temporary is a known `Int`, a variable is one
            // again whatever an earlier row left there.
            // (The wider read stores a scalar of any kind: no int for
            // the rows after it, in a temporary or in a variable.)
            C_DIMR if meta[j].arr => {
                let d = meta[j].db;
                if meta[j].kc == KT {
                    ts.set(d, TmpKind::Scalar);
                } else {
                    for list in [&mut cvbools, &mut ever_nonint] {
                        if !list.contains(&d) {
                            list.push(d);
                        }
                    }
                }
            }
            C_DIMR => {
                let d = meta[j].db;
                if meta[j].kc == KT {
                    ts.set(d, TmpKind::Int);
                } else {
                    cvbools.retain(|c| c != &d);
                }
            }
            // `$cv = v` and `++`/`--` with the result kept: the result is
            // an `Int`; a consumed source temporary is left null.
            C_ASSIGN_R => {
                let inst = &tape[head + j];
                if meta[j].consume {
                    ts.set(inst.b, TmpKind::Unknown);
                }
                ts.set(inst.c, TmpKind::Int);
            }
            C_INCDEC_T => {
                let c = tape[head + j].c;
                ts.set(c, TmpKind::Int);
            }
            // An array write moved a consumed value temporary out (the
            // row nulls it, as the interpreted one does).
            C_DIMW | C_DIMOP => {
                if meta[j].consume {
                    let t = meta[j].sw;
                    ts.set(t, TmpKind::Unknown);
                }
            }
            // The handler left a value of no known kind.
            C_DIMSR => ts.set(meta[j].db, TmpKind::Unknown),
            // A handler that may jump left the temporary it tested null,
            // on both ways out.
            C_ROWBR if meta[j].sb != NO_DST => ts.set(meta[j].sb, TmpKind::Unknown),
            // The tape's handler released a key temporary, left a value of
            // no known kind in a read's destination (a variable is then no
            // int the trace knows of and may hold a value to release) and
            // moved a write's consumed value temporary out.
            C_DIMKEY => {
                let m = meta[j];
                if m.kb == KT {
                    ts.set(m.slot, TmpKind::Unknown);
                }
                let read = matches!(
                    tape[head + j].op,
                    TOp::FetchDimIC | TOp::FetchDimIT | TOp::FetchDimSC | TOp::FetchDimC
                );
                if !read {
                    if m.consume {
                        ts.set(m.sw, TmpKind::Unknown);
                    }
                } else if m.kc == KT {
                    ts.set(m.db, TmpKind::Unknown);
                } else {
                    for list in [&mut counted, &mut cvbools, &mut ever_nonint] {
                        if !list.contains(&m.db) {
                            list.push(m.db);
                        }
                    }
                }
            }
            // What the handler stored (an int, a bool, or a value of any
            // type the temporary owns), the temporaries it consumed, and a
            // variable it wrote (no int the trace knows of afterwards).
            C_ROW => {
                let m = meta[j];
                for t in [m.sb, m.slot, m.sw] {
                    if t != NO_DST {
                        ts.set(t, TmpKind::Unknown);
                    }
                }
                if m.db != NO_DST {
                    let kind = match m.arith {
                        ROW_INT => TmpKind::Int,
                        ROW_BOOL => TmpKind::Bool,
                        _ => TmpKind::Val,
                    };
                    ts.set(m.db, kind);
                }
                if let Ok(cv) = u32::try_from(m.lit_c) {
                    for list in [&mut counted, &mut cvbools, &mut ever_nonint] {
                        if !list.contains(&cv) {
                            list.push(cv);
                        }
                    }
                }
            }
            // The temporary is consumed, on both ways out of the row.
            C_JMPZT | C_JMPNZT => {
                let a = tape[head + j].a;
                ts.set(a, TmpKind::Unknown);
            }
            // A value of any type, owned by the temporary, or stored into
            // the variable, which is then no int the trace knows of and
            // may hold a value to release.
            C_VMREAD | C_GREAD => {
                let d = meta[j].db;
                if meta[j].kc == KT {
                    ts.set(d, TmpKind::Val);
                } else {
                    for list in [&mut counted, &mut cvbools, &mut ever_nonint] {
                        if !list.contains(&d) {
                            list.push(d);
                        }
                    }
                }
            }
            // A moved value: the literal's kind, any scalar from a
            // variable, a temporary's own kind (a checked one is some
            // scalar); a consumed source temporary is left null.
            C_MOVE => {
                let inst = &tape[head + j];
                let kind = match meta[j].sc {
                    KL => match meta[j].arith {
                        CONST_NULL => TmpKind::Null,
                        CONST_BOOL => TmpKind::Bool,
                        _ => TmpKind::Int,
                    },
                    KT => match ts.kind(inst.a) {
                        TmpKind::Unknown => TmpKind::Scalar,
                        k => k,
                    },
                    _ if meta[j].arr => TmpKind::Val,
                    _ => TmpKind::Scalar,
                };
                if meta[j].sc == KT && meta[j].consume {
                    ts.set(inst.a, TmpKind::Unknown);
                }
                ts.set(inst.c, kind);
            }
            _ => {}
        }
        // Any row that fully stores an int into a compiled variable (or
        // an int from a temporary through `AssignT`) ends its `Bool`/
        // `Null` state for the rows after it.
        match class[j] {
            C_INC | C_DEC | C_ASSIGN_I | C_ASSIGN_L | J_INCCMP | J_ASSIGN_OP | J_NUMBIN_C
            | C_ASSIGN_R => {
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
        // What a jump carries to a label further down: the state after
        // the row, with the result of a `?:` or `??` (stored only when
        // the jump is taken) a scalar. Nothing falls through a plain jump.
        if is_jump_class(class[j]) {
            let y = jump_target(tape, head + j);
            // A temporary that may hold a value to release is consumed
            // before a loop goes round (this one or one nested in it): the
            // next iteration stores over it.
            if y <= head + j && ts.has_val() {
                rej!(format!(
                    "a temporary may hold a value to release at the back edge at {}",
                    head + j
                ));
            }
            // The rows that jump on a temporary or through a handler close
            // no loop (a nested loop's head polls for its closing row,
            // which `emit` only finds among the plain jumps and compares).
            if y <= head + j && matches!(class[j], C_ROWBR | C_JMPZT | C_JMPNZT | J_CMPS) {
                rej!(format!("a backward jump on a temporary at {}", head + j));
            }
            if y > head + j && y <= close {
                let mut edge = ts.clone();
                if matches!(class[j], C_JMPSET | C_COALESCE) {
                    let kind = if meta[j].arr {
                        TmpKind::Val
                    } else {
                        TmpKind::Scalar
                    };
                    edge.set(tape[head + j].b, kind);
                }
                // A handler row's result, stored only when it jumps: a
                // value of any type.
                if class[j] == C_ROWBR && meta[j].db != NO_DST {
                    edge.set(meta[j].db, TmpKind::Val);
                }
                incoming.push((y, edge));
            }
            if class[j] == C_JMP {
                reachable = false;
                ts.clear();
            }
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
    // A variable that may hold an array is only written by the rows that
    // release what it holds, and only read by the rows that look at its
    // tag: any guard on it comes from a row that would store over it or
    // read it as an int.
    if guards
        .iter()
        .any(|g| g.base == KC && counted.contains(&g.off))
    {
        rej!("a variable that may hold an array is also written or read as a scalar");
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

    guards.sort_by_key(|g| (g.base, g.off));
    // A pinned cell or constant fold is only as fresh as the runtime
    // caches that produced it; the prologue's version guard covers the
    // whole trace when any row of this trace depends on them.
    let version_guard = class
        .iter()
        .any(|&c| matches!(c, C_SPROP | C_SPROPW | C_SPROPISSET | C_SPROPINC | C_CONSTI));
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

/// The builtin call that ends an inlined body: the `FramelessQ` row `row`
/// at op `bc` of `code` when it is a call of one temporary to a builtin
/// whose result for an int is a float or an int and needs no handler
/// (`special::quick_eval`: the math functions, `abs()`), the call site's
/// cache holds that builtin (what `strs::frameless_quick` checks on every
/// call; the trace's function-table guard keeps it so), and the op after
/// the call returns its result. Gives the builtin's id, the argument's
/// temporary and the `Return`'s operand.
fn quick_tail(code: &crate::opcode::OpArray, bc: usize, row: &Inst) -> Option<(u8, u32, Operand)> {
    use super::strs::{Q_CACHE_SHIFT, Q_CLASS_SHIFT, Q_ONE_TMP};
    let Op::FramelessICall(fc) = code.ops.get(bc)? else {
        return None;
    };
    if row.k != Q_ONE_TMP || row.e > crate::special::QUICK_ABS {
        return None;
    }
    let p = row.p as u64;
    {
        let cache = code.fcall_cache.try_borrow().ok()?;
        let slot = usize::try_from(p >> Q_CACHE_SHIFT).ok()?;
        let entry = cache.get(slot)?.as_ref()?;
        let crate::opcode::CachedFuncKind::Builtin(b) = &entry.func else {
            return None;
        };
        let class = u32::try_from((p >> Q_CLASS_SHIFT) & 31).ok()?;
        if !crate::special::frameless_class_is(b.flags, class) {
            return None;
        }
    }
    let dst = u32::try_from(p & 0xffff_ffff).ok()?;
    match code.ops.get(bc + 1 + fc.tail())? {
        Op::Return {
            val: val @ Operand::Tmp(t),
        } if t.checked_shl(SLOT_SHIFT) == Some(dst) => Some((row.e, row.a, *val)),
        _ => None,
    }
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
    let (name, mcall, dyncv, relsel) = match code.ops.get(init)? {
        Op::InitFCall {
            name,
            fallback: None,
            ..
        } => (Some(*name), None, None, None),
        Op::InitStaticCall {
            class: crate::opcode::ClassSpec::Named(_),
            name: Operand::Lit(_),
        } => (None, None, None, None),
        // `self::name()`, `parent::name()`, `static::name()`: resolved by
        // the op's own slot of the method cache.
        Op::InitStaticCall {
            class:
                spec @ (crate::opcode::ClassSpec::SelfClass
                | crate::opcode::ClassSpec::Parent
                | crate::opcode::ClassSpec::Static),
            name: Operand::Lit(lit),
        } => (None, None, None, Some((spec, *lit))),
        Op::InitMethodCall {
            obj: obj @ (Operand::Cv(_) | Operand::Tmp(_) | Operand::TmpKeep(_) | Operand::Unused),
            name: Operand::Lit(lit),
        } => (None, Some((obj, *lit)), None, None),
        // `$f(...)`: a closure held by a compiled variable.
        Op::InitDynCall {
            callee: Operand::Cv(cv),
        } => (None, None, Some(*cv), None),
        _ => rj!("raw row is not a plain InitFCall, a Lit InitStaticCall, \
             a Cv/Tmp/Unused/Lit InitMethodCall or an InitDynCall of a variable"),
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
                        chk: false,
                    });
                    ci += 1;
                    continue;
                }
                // A `KL`'s value is resolved later, only if the send
                // turns out to be live; keep the literal's index until
                // then.
                sends.push(Arg {
                    kind,
                    off,
                    lit: 0,
                    chk: false,
                });
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
    let mut callee_obj: Option<(u32, u64)> = None;
    let mut callee_pin: Option<php_runtime::Obj> = None;
    let mut rel: Option<(i64, Option<i64>, bool)> = None;
    // The scope an inlined method runs in (its declaring class, as the
    // site's `method_cache` entry gives it): what its own property rows'
    // cache entries must have been filled for.
    let mut callee_scope = 0usize;
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
        callee_scope = cached
            .method_scope
            .as_ref()
            .and_then(std::rc::Weak::upgrade)
            .map_or(0, |s| Rc::as_ptr(&s) as usize);
        match cached.func.get() {
            Some(Func::User(uf)) => uf,
            Some(Func::Builtin(_)) => rj!("method is a builtin"),
            None => rjs!("cached method is dead"),
        }
    } else if let Some(cv) = dyncv {
        // `$f(...)`: the closure the variable holds now, as
        // `prepare_closure` takes it - a closure object of a user
        // function, no `__call` trampoline - and besides that unbound
        // and capturing nothing. The trace keeps the object alive for
        // good (so that its address cannot come to name another closure),
        // which must not keep a bound `$this` from its destructor; and a
        // body with captured variables is not inlined anyway. The emitted
        // guard re-checks on every call that the variable still holds
        // this very object.
        if cv as usize >= code.cv_names.len() || cv >= super::SLOT_LIMIT {
            rj!("the called variable is out of range");
        }
        let off = cv << SLOT_SHIFT;
        // SAFETY: the frame outlives the plan (the trace runs in it).
        let f = unsafe { &*st.f };
        let Some(php_runtime::Value::Object(o)) =
            f.cvs.get(cv as usize).and_then(crate::vm::CvSlot::get)
        else {
            rjs!("the called variable holds no object");
        };
        let data = crate::object::data(o);
        if data.class.lc_name.as_bytes() != b"closure" {
            rj!("the called variable holds no closure");
        }
        let Ok(internal) = data.internal.try_borrow() else {
            rj!("the closure's data is borrowed");
        };
        let Some(d) = internal
            .as_ref()
            .and_then(|x| x.downcast_ref::<crate::closure::ClosureData>())
        else {
            rj!("the closure has no data");
        };
        if d.magic_name.is_some()
            || d.this.is_some()
            || !d.vars.is_empty()
            || d.partial_inner.is_some()
        {
            rj!("the closure is bound, captures variables or wraps another call");
        }
        let Func::User(callee) = &d.func else {
            rj!("the closure is of a builtin");
        };
        let callee = Rc::clone(callee);
        // The handle word the slot holds: what the guard compares.
        // SAFETY: a validated variable offset of the running frame, whose
        // slot holds an object (matched above).
        let word =
            unsafe { std::ptr::read_unaligned(st.cvs.add((off + PAYLOAD) as usize).cast::<u64>()) };
        // The handle travels with the plan and is only pinned once the
        // trace exists (`attempt`): a refused plan keeps nothing alive
        // and allocates nothing that outlives it.
        callee_pin = Some(o.clone());
        callee_obj = Some((off, word));
        callee
    } else if let Some((spec, lit)) = relsel {
        // The hit `calls::prepare_relative_static` takes: the op's slot of
        // the method cache holds the class the spec resolves to in this
        // frame, the frame's calling scope and the method. The emitted
        // guard pins what that resolution depends on for every later
        // frame: the scope (`self` and `parent` follow from it), and for
        // `static::` the called scope.
        // SAFETY: the frame outlives the plan (the trace runs in it).
        let f = unsafe { &*st.f };
        let Some(cls) = crate::call::relative_spec_class(f, spec) else {
            rj!("the frame has no class for the relative spec");
        };
        let scope = f.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
        let cache = code.method_cache.borrow();
        let Some(cached) = cache.get(init).and_then(|c| c.as_ref()) else {
            rjs!("method cache slot unfilled");
        };
        if cached.class_addr != Rc::as_ptr(cls) as usize
            || cached.scope_addr != scope
            || cached.name_lit != Some(lit)
        {
            rjs!("method cache entry is for another class, scope or name");
        }
        if cached
            .method_scope
            .as_ref()
            .and_then(std::rc::Weak::upgrade)
            .is_none()
        {
            rjs!("cached method scope is dead");
        }
        let word = |addr: usize| {
            if addr == 0 {
                0
            } else {
                addr as i64 - i64::from(RC_VALUE_OFFSET)
            }
        };
        let is_static_spec = matches!(spec, crate::opcode::ClassSpec::Static);
        let called = is_static_spec.then(|| word(Rc::as_ptr(cls) as usize));
        // An instance method called this way takes the frame's `$this`,
        // which must be an object of the resolved class: with the scope
        // pinned that holds for `self` and `parent` whenever there is a
        // `$this` at all. `static::` of an instance method is left to the
        // interpreter.
        let needs_this = !cached.is_static;
        if needs_this {
            if is_static_spec {
                rj!("static:: call of an instance method");
            }
            let Some(this) = f.this.as_ref() else {
                rj!("instance method called statically without $this");
            };
            if !crate::object::class_of(this).is_subclass_of(cls) {
                rj!("$this is not of the resolved class");
            }
        }
        rel = Some((word(scope), called, needs_this));
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
    // A builtin call that ends the body: its id and its argument's
    // temporary (`quick_tail`).
    let mut quick: Option<(u8, u32)> = None;
    let ret = loop {
        match uf.code.ops.get(bc) {
            Some(Op::Return { val }) => break *val,
            Some(_) => {}
            None => rj!("callee body walk ran off the op array at {bc}"),
        }
        let row = uf.code.tape.get(&uf.code)[bc];
        // What a fused builtin call left of its `InitFCall` and `Send`.
        if row.op == TOp::Nop {
            bc += 1;
            continue;
        }
        if row.op == TOp::FramelessQ {
            let Some((id, arg, ret)) = quick_tail(&uf.code, bc, &row) else {
                rj!("callee row {bc}: a builtin call that does not end the body");
            };
            quick = Some((id, arg));
            break ret;
        }
        let is_prop_row = matches!(
            row.op,
            TOp::FetchObjProp
                | TOp::FetchObjPropTo
                | TOp::AssignObjProp
                | TOp::IncDecObjProp
                | TOp::AssignOpObjProp
        );
        let (c, m) = if is_prop_row {
            // A property of the method's `$this`: of the call's receiver.
            let Some((rk, roff, cls)) = recv else {
                rj!("callee row {bc} ({:?}) without a receiver", row.op);
            };
            match plan_callee_prop(&uf.code, &row, callee_scope, cls as usize, rk, roff) {
                Some(cm) => cm,
                None => rj!("callee property row {bc} ({:?}) not inlinable", row.op),
            }
        } else {
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
            (c, m)
        };
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
        // (A builtin's result is a float or an int: only an undeclared
        // return hands back either as it is.)
        if !passes || quick.is_some() {
            rj!("callee's declared return type would coerce or reject the result");
        }
    }
    // The builtin may decline its argument, which leaves the whole call
    // to the interpreter: nothing of it may have been written by then.
    if quick.is_some()
        && classes
            .iter()
            .any(|c| matches!(*c, C_PROPW | C_PROPINC | C_PROPOPA))
    {
        rj!("a builtin call ends a body that writes a property");
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
        if is_callee_prop(classes[k]) {
            // A property row names a callee slot only as the value it
            // writes (its other operands are the receiver and a place).
            if m.sc == KC {
                if let Some(i) = scratch_idx(KC, m.sw) {
                    live[i] = true;
                }
            }
            continue;
        }
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
    // Whether an earlier row of the body wrote a property: what the
    // interpreter, re-running the call after a later row left the trace,
    // would write a second time. So after a write only rows that cannot
    // leave may follow (and a property row's checks are all made before
    // the first row: `prop_slots`).
    let mut wrote = false;
    let mut prop_slots: Vec<u32> = Vec::new();
    for k in 0..classes.len() {
        let c = classes[k];
        let m = &mut metas[k];
        let row = &mut rows[k];
        let (ia, ib, ic) = (row.a, row.b, row.c);
        if is_callee_prop(c) {
            // The value written: a callee slot the trace knows holds an
            // `Int`, or an int literal.
            if matches!(c, C_PROPW | C_PROPOPA) {
                match m.sc {
                    KC | KT => {
                        let Some(i) = scratch_idx(m.sc, m.sw) else {
                            rj!("callee property row's value is not a callee slot");
                        };
                        if !known[i] {
                            rj!("callee property row writes a slot that is not a known int");
                        }
                        if m.sc == KT {
                            m.sw += (ncvs as u32) << 4;
                        }
                        m.sc = KR;
                        // A consumed temporary is left null.
                        if m.consume {
                            known[i] = false;
                        }
                    }
                    KL => {}
                    _ => rj!("callee property row's value has no inlinable source"),
                }
            }
            // `++`, `--` and `op=` leave on an overflow (before their own
            // store).
            if wrote && matches!(c, C_PROPINC | C_PROPOPA) {
                rj!("a callee row that can overflow follows a property write");
            }
            // The result temporary, fully stored with an `Int`.
            if m.db != NO_DST {
                let Some(i) = scratch_idx(KT, m.db) else {
                    rj!("callee property row's result is not a callee temporary");
                };
                known[i] = true;
                m.db += (ncvs as u32) << 4;
                m.kd = KR;
            }
            if c != C_PROP {
                wrote = true;
            }
            if !prop_slots.contains(&m.slot) {
                prop_slots.push(m.slot);
            }
            continue;
        }
        if wrote && !matches!(c, C_ASSIGN_I | C_ASSIGN_L | J_ASSIGN_T) {
            rj!("a callee row that can leave the trace follows a property write");
        }
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
        // The result of the builtin call that ends the body: its
        // argument, a callee temporary the body left an int in.
        _ if quick.is_some() => {
            let (id, arg) = quick?;
            if !scratch_idx(KT, arg).is_some_and(|i| known[i]) {
                rj!("the builtin's argument is not a known int");
            }
            Arg {
                kind: KQUICK,
                off: arg + (u32::try_from(ncvs).ok()? << 4),
                lit: i64::from(id),
                chk: false,
            }
        }
        Operand::Unused => Arg {
            kind: KNULL_ARG,
            off: 0,
            lit: 0,
            chk: false,
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
                    chk: false,
                }
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
                Arg {
                    kind,
                    off,
                    lit,
                    chk: false,
                }
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
        callee_obj,
        callee_pin,
        rel,
        prop_slots,
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
    /// The frame (`r10`: the plan's `frame` is set).
    Frame,
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
    /// The address of the tape the trace is of (the rows a handler called
    /// from the trace reads; 0 in the unit tests).
    tbase: usize,
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
    /// The closure objects the emitted guards name by handle word: kept
    /// alive ([`PINNED_OBJS`]) once the code is placed.
    pins: Vec<(u64, php_runtime::Obj)>,
    /// The address of the trace's table of global slots (one word per
    /// [`C_GREAD`] row, in row order, then the rows' op indexes as `u32`s:
    /// what [`jit_resolve_globals`] reads); 0 when it has none.
    gtable: i64,
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

    /// `cmp qword [rax + off], imm8` (`48 83 B8 d32 ib`, the immediate
    /// sign-extended).
    fn cmpq_rax(&mut self, off: u32, imm: i8) {
        self.put(&[REX_W, 0x83, 0xB8]);
        self.put(&off.to_le_bytes());
        self.put(&imm.to_le_bytes());
    }

    /// `cmp [rax + off], rcx` (`48 39 88 d32`: `39 /r` with `reg` =
    /// `rcx`, `rm` = `rax`).
    fn cmp_at_rax_rcx(&mut self, off: u32) {
        self.put(&[REX_W, 0x39, 0x88]);
        self.put(&off.to_le_bytes());
    }

    /// `cmp r9, [rax + off]` (`4C 3B 88 d32`: `3B /r` with `reg` = `r9`
    /// through `REX.R`, `rm` = `rax`).
    fn cmp_r9_at_rax(&mut self, off: u32) {
        self.put(&[REX_W | REX_R, 0x3B, 0x88]);
        self.put(&off.to_le_bytes());
    }

    /// `shl r9, imm8` (`49 C1 E1 ib`: `C1 /4` with `rm` = `r9` through
    /// `REX.B`).
    fn shl_r9(&mut self, by: u8) {
        self.put(&[REX_W | REX_B, 0xC1, 0xE1, by]);
    }

    /// `add rax, r9` (`4C 01 C8`: `01 /r` with `reg` = `r9` through
    /// `REX.R`, `rm` = `rax`).
    fn add_rax_r9(&mut self) {
        self.put(&[REX_W | REX_R, 0x01, 0xC8]);
    }

    /// `mov ecx, [rax + off]` (`8B 88 d32`, no REX: a 32-bit word,
    /// zero-extended through `rcx`).
    fn mov_ecx_at_rax(&mut self, off: u32) {
        self.put(&[0x8B, 0x88]);
        self.put(&off.to_le_bytes());
    }

    /// `cmp r9, rcx` (`49 39 C9`: `39 /r` with `reg` = `rcx`, `rm` = `r9`
    /// through `REX.B`).
    fn cmp_r9_rcx(&mut self) {
        self.put(&[REX_W | REX_B, 0x39, 0xC9]);
    }

    /// `lea rcx, [r9 + 1]` (`49 8D 49 01`: `8D /r` with `reg` = `rcx`,
    /// `rm` = `r9` through `REX.B` and an 8-bit displacement).
    fn lea_rcx_r9_next(&mut self) {
        self.put(&[REX_W | REX_B, 0x8D, 0x49, 0x01]);
    }

    /// `inc dword [rax + off]` (`FF 80 d32`: `FF /0`, no REX).
    fn incl_at_rax(&mut self, off: u32) {
        self.put(&[0xFF, 0x80]);
        self.put(&off.to_le_bytes());
    }

    /// `mov [base + off], rcx` (`48 89 /r` with `reg` = `rcx`): the first
    /// word of a slot, its tag byte with the bytes after it
    /// ([`Self::store_rcx_base`] writes the payload word).
    fn store_rcx_first(&mut self, base: u8, off: u32) {
        self.put(&[REX_W, 0x89, Self::rm3(base, 1)]);
        self.put(&off.to_le_bytes());
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
            C_SPROPINC => self.emit_spropinc(m, Target::Side(i)),
            C_CONSTI => self.emit_consti(m, Target::Side(i)),
            C_DIMR => self.emit_dimr(m, i),
            C_DIMW => self.emit_dimw(m, i, jit_dim_set as *const () as usize, 0),
            C_DIMOP => self.emit_dimw(m, i, jit_dim_op as *const () as usize, u32::from(m.arith)),
            C_MOVE => self.emit_move(m, &inst, Target::Side(i)),
            // The tape's handler for the row, then the poll every row that
            // may release or allocate ends with.
            C_DIMSR => {
                let row = self.row_arg(i);
                self.emit_helper(
                    jit_row_fetch_dims as *const () as usize,
                    [HArg::Frame, row, HArg::Null],
                    0,
                    Target::Side(i),
                );
                self.put(&[0x41, 0x80, 0x3B, 0x00]); // cmp byte [r11], 0
                self.jcc(0x85, Target::Poll(i + 1, i)); // jne
            }
            // The tape's own handler for an array row with a key that is
            // no int, then the poll.
            C_DIMKEY => {
                let row = self.row_arg(i);
                let helper = keyed_row_helper(self.tape[i].op);
                self.emit_helper(
                    helper,
                    [HArg::Frame, row, HArg::Null],
                    u32::try_from(i).unwrap_or(u32::MAX),
                    Target::Side(i),
                );
                let next = i + self.plan.span[j] as usize;
                self.put(&[0x41, 0x80, 0x3B, 0x00]); // cmp byte [r11], 0
                self.jcc(0x85, Target::Poll(next, i)); // jne
            }
            C_GREAD => self.emit_gread(m, i),
            // The read's fast path through the helper, then the poll every
            // row that may release a value ends with.
            C_VMREAD => {
                self.emit_helper(
                    jit_row_vm_read as *const () as usize,
                    [
                        HArg::Frame,
                        HArg::Abs(i64::try_from(i).unwrap_or(i64::MAX)),
                        HArg::Null,
                    ],
                    0,
                    Target::Side(i),
                );
                let next = i + self.plan.span[j] as usize;
                self.put(&[0x41, 0x80, 0x3B, 0x00]); // cmp byte [r11], 0
                self.jcc(0x85, Target::Poll(next, i)); // jne
            }
            // The row's handler, then the poll of a row that may release a
            // value or allocate.
            C_ROW if m.mask == ROW_NEW_RET => self.emit_new_row(m, i, j),
            C_ROW => {
                let row = self.row_arg(i);
                self.emit_row(
                    usize::try_from(m.lit_a).unwrap_or(0),
                    m.mask == ROW_BR_RET,
                    [HArg::Frame, row, HArg::Null],
                    Target::Side(i),
                );
                if m.inc {
                    let next = i + self.plan.span[j] as usize;
                    self.put(&[0x41, 0x80, 0x3B, 0x00]); // cmp byte [r11], 0
                    self.jcc(0x85, Target::Poll(next, i)); // jne
                }
            }
            // A handler that may jump (`Br`): 1 declined, 2 jumped, 0 goes
            // on.
            C_ROWBR => {
                let row = self.row_arg(i);
                self.emit_helper_call(
                    usize::try_from(m.lit_a).unwrap_or(0),
                    [HArg::Frame, row, HArg::Null],
                    0,
                );
                self.put(&[0x3C, 0x01]); // cmp al, 1
                self.jcc(0x84, Target::Side(i)); // je
                self.jcc(0x87, self.jump_tgt(j)); // ja
            }
            // A jump on a temporary: a bool, or (where the plan does not
            // know what it holds: an element read's result) a bool or an
            // int by the tag, anything else being the interpreted row's;
            // read and left null as the interpreted row leaves it.
            C_JMPZT | C_JMPNZT => {
                if m.ca {
                    let (is_bool, tested) = (self.synth_label(), self.synth_label());
                    self.cmpb(KT, inst.a, TAG_BOOL);
                    self.jcc(0x84, Target::Label(is_bool)); // je
                    self.cmpb(KT, inst.a, TAG_INT);
                    self.jcc(0x85, Target::Side(i)); // jne
                    self.load_int(KT, inst.a);
                    self.put(&[0x31, 0xC9]); // xor ecx, ecx
                    self.put(&[REX_W, 0x85, 0xC0]); // test rax, rax
                    self.put(&[0x0F, 0x95, 0xC1]); // setne cl
                    self.jmp(Target::Label(tested));
                    self.set_label(is_bool);
                    self.load_cl_base(KT, inst.a.wrapping_add(BOOL_BYTE));
                    self.set_label(tested);
                } else {
                    self.load_cl_base(KT, inst.a.wrapping_add(BOOL_BYTE));
                }
                self.store_imm_base(KT, inst.a, 0);
                self.store_tag_base(KT, inst.a, TAG_NULL);
                self.put(&[0x85, 0xC9]); // test ecx, ecx
                let cc = if self.plan.class[j] == C_JMPZT {
                    0x84 // jz
                } else {
                    0x85 // jnz
                };
                self.jcc(cc, self.jump_tgt(j));
            }
            // `$cv = TMP` of any value over any value: the tape's own
            // assignment, then its poll (the old value is released).
            J_ASSIGN_T if m.mask == ANY_SRC => {
                let row = self.row_arg(i);
                // `scalar::assign_cv_any(frame, row, from_tmp: true)`.
                self.emit_row(
                    super::scalar::assign_cv_any as *const () as usize,
                    false,
                    [HArg::Frame, row, HArg::Abs(1)],
                    Target::Side(i),
                );
                self.put(&[0x41, 0x80, 0x3B, 0x00]); // cmp byte [r11], 0
                self.jcc(0x85, Target::Poll(i + 1, i)); // jne
            }
            C_JMPZ | C_JMPNZ => {
                let end = self.synth_label();
                let (on_true, on_false) = if self.plan.class[j] == C_JMPNZ {
                    (self.jump_tgt(j), Target::Label(end))
                } else {
                    (Target::Label(end), self.jump_tgt(j))
                };
                self.emit_cv_truth(inst.a, m.arr, on_true, on_false, Target::Side(i));
                self.set_label(end);
            }
            // `$cv ?: ...`: a truthy value into the result temporary, then
            // the jump.
            C_JMPSET => {
                let (copy, end) = (self.synth_label(), self.synth_label());
                self.emit_cv_truth(
                    inst.a,
                    m.arr,
                    Target::Label(copy),
                    Target::Label(end),
                    Target::Side(i),
                );
                self.set_label(copy);
                if m.arr {
                    self.copy_cv_value(inst.a, inst.b, true, Target::Side(i));
                } else {
                    self.copy_slot(KC, inst.a, KT, inst.b);
                }
                self.jmp(self.jump_tgt(j));
                self.set_label(end);
            }
            // `$cv ?? ...`: undefined or null falls through; a bool, an
            // int or a float is copied and the jump taken; anything else
            // is the interpreted row's.
            C_COALESCE => {
                let end = self.synth_label();
                self.cmpb(KC, inst.a, TAG_NULL);
                self.jcc(0x84, Target::Label(end)); // je
                self.cmpb(KC, inst.a, TAG_NONE);
                self.jcc(0x84, Target::Label(end)); // je
                self.copy_cv_value(inst.a, inst.b, m.arr, Target::Side(i));
                self.jmp(self.jump_tgt(j));
                self.set_label(end);
            }
            J_CMP => {
                self.chk_int(m.ca, inst.a, Target::Side(i));
                self.chk_int(m.cb, inst.b, Target::Side(i));
                self.to_rax(m.ka, inst.a, m.lit_a);
                self.to_r9(m.kb, inst.b, m.lit_b);
                self.put(&[REX_W | REX_R, 0x39, 0xC8]); // cmp rax, r9
                let cc = jcc_for(m.mask).unwrap_or(0x85);
                self.jcc(cc, self.jump_tgt(j));
            }
            J_CMPS => self.emit_cmps(m, &inst, i, j),
            J_ASSIGN_OPS => self.emit_assign_ops(m, &inst, i),
            J_INCCMP => {
                // `b` is read *before* the increment (it may be the
                // same slot as `a`).
                self.chk_int(m.cb, inst.b, Target::Side(i));
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
            // `$cv = v` with the result kept: the int into the variable
            // and into the result temporary, a consumed source temporary
            // left null first (it may be the result's slot).
            C_ASSIGN_R => {
                self.chk_int(m.cb, inst.b, jo);
                self.to_rax(m.kb, inst.b, m.lit_b);
                if m.consume {
                    self.store_imm_base(KT, inst.b, 0);
                    self.store_tag_base(KT, inst.b, TAG_NULL);
                }
                self.store_int(KC, inst.a);
                self.store_tag_base(KC, inst.a, TAG_INT);
                self.store_int(KT, inst.c);
                self.store_tag_base(KT, inst.c, TAG_INT);
            }
            // `++`/`--` with the result kept: the value after the step, or
            // (`arr`) the one before it (the step undone in the register),
            // into the result temporary; an overflow leaves before any
            // store.
            C_INCDEC_T => {
                self.load_int(KC, inst.a);
                self.put(&[REX_W, 0x83, if m.inc { 0xC0 } else { 0xE8 }, 0x01]);
                self.jcc(0x80, jo); // jo: overflow
                self.store_int(KC, inst.a);
                if m.arr {
                    self.put(&[REX_W, 0x83, if m.inc { 0xE8 } else { 0xC0 }, 0x01]);
                }
                self.store_int(KT, inst.c);
                self.store_tag_base(KT, inst.c, TAG_INT);
            }
            // `abs(x)`: nothing to do for an unused result (the operand is
            // an int, and `abs` of one has no effect); else negate a
            // negative value, leaving `PHP_INT_MIN` (the one `neg`
            // overflows on) to the interpreted row.
            C_ABS => {
                self.chk_int(m.ca, inst.a, jo);
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
                } else if m.mask == SCALAR_SRC {
                    // A scalar of a kind the plan does not know (a
                    // ternary's result): both words of the slot.
                    self.copy_slot(m.kb, inst.b, m.ka, inst.a);
                } else if m.mask == VAL_SRC {
                    // A value the temporary owns, moved as it is. The
                    // variable may hold an array from the iteration
                    // before: one handle less on it,
                    // unless it is the last (freeing it is the interpreted
                    // row's, like a string, an object or a reference
                    // there). Nothing is changed before the last test.
                    let store = self.synth_label();
                    self.cmpb(KC, inst.a, TAG_FLOAT);
                    self.jcc(0x86, Target::Label(store)); // jbe: a scalar
                    self.cmpb(KC, inst.a, TAG_NONE);
                    self.jcc(0x84, Target::Label(store)); // je: undefined
                    self.cmpb(KC, inst.a, TAG_ARRAY);
                    self.jcc(0x85, jo); // jne
                    self.load_int(KC, inst.a);
                    self.count_op(7, 1); // cmp qword [rax], 1
                    self.jcc(0x84, jo); // je: the last handle
                    self.count_op(5, 1); // sub qword [rax], 1
                    self.set_label(store);
                    self.copy_slot(m.kb, inst.b, m.ka, inst.a);
                } else {
                    // The int path: the plan knows the temporary holds
                    // one, or the row checks it.
                    self.chk_int(m.cb, inst.b, jo);
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
                self.chk_int(m.ca, inst.a, jo);
                self.chk_int(m.cb, inst.b, jo);
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

    /// A frame field (`r10` + `field`) must hold the word `want` (0: must
    /// be null), else leave through `fail`: the calling scope or called
    /// scope a plan resolved something for.
    fn emit_frame_word_guard(&mut self, field: u32, want: i64, fail: Target) {
        self.load_rax_frame(field);
        if want == 0 {
            self.test_rax();
        } else {
            self.movabs_rcx(want);
            self.cmp_rax_rcx();
        }
        self.jcc(0x85, fail); // jne
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
        // A `self::`, `parent::` or `static::` call: the frame's calling
        // scope (and called scope, for `static::`) the plan resolved the
        // method for, and a `$this` for an instance method.
        if let Some((scope, called, needs_this)) = cp.rel {
            self.emit_frame_word_guard(SCOPE_FIELD, scope, Target::Side(cp.callove));
            if let Some(called) = called {
                self.emit_frame_word_guard(CALLED_FIELD, called, Target::Side(cp.callove));
            }
            if needs_this {
                self.load_r9_frame(THIS_FIELD);
                self.put(&[REX_W | REX_R | REX_B, 0x85, 0xC9]); // test r9, r9
                self.jcc(0x84, Target::Side(cp.callove)); // je: no `$this`
            }
        }
        // A closure call: the variable must still hold the very closure
        // object whose body the plan inlined (another closure, or none,
        // leaves the call to the interpreter).
        if let Some((off, word)) = cp.callee_obj {
            if let Some(o) = &cp.callee_pin {
                self.pins.push((word, o.clone()));
            }
            self.cmpb(KC, off, TAG_OBJECT);
            self.jcc(0x85, Target::Side(cp.callove)); // jne: no object
            self.load_r9_at(KC, off.wrapping_add(PAYLOAD));
            self.movabs_rax(word as i64);
            self.cmp_rax_r9();
            self.jcc(0x85, Target::Side(cp.callove)); // jne: another object
        }
        // An inlined method that reads or writes properties of its `$this`
        // (the receiver, whose class the guard above pinned): everything
        // those accesses need is checked here, once, before any row of the
        // body runs, so that no row leaves the trace after another one
        // wrote a property. The object's `lazy` cell unborrowed and empty,
        // its `slots` cell unborrowed and long enough
        // (`emit_prop_slots`'s checks), and every slot the body touches an
        // `Int` (`emit_prop_int_slot`'s): the body's own writes keep them
        // ints.
        if let (false, Some((rk, roff, _))) = (cp.prop_slots.is_empty(), cp.recv) {
            let jo = Target::Side(cp.callove);
            self.load_receiver_r9(rk, roff);
            self.cmpq_r9(OBJ_LAZY.wrapping_add(REFCELL_BORROW), 0);
            self.jcc(0x85, jo); // jne: borrowed
            self.cmpq_r9(OBJ_LAZY.wrapping_add(REFCELL_VALUE), 0);
            self.jcc(0x85, jo); // jne: lazy properties present
            self.cmpq_r9(SLOTS_BORROW, 0);
            self.jcc(0x85, jo); // jne: borrowed
            let top = cp.prop_slots.iter().copied().max().unwrap_or(0);
            self.mov_rax_r9(SLOTS_LEN);
            self.cmp_rax_imm(top.wrapping_add(1));
            self.jcc(0x82, jo); // jb: a slot past the array
            self.mov_rax_r9(SLOTS_FIRST);
            for &slot in &cp.prop_slots {
                self.cmpb_rax(slot << SLOT_SHIFT, TAG_INT);
                self.jcc(0x85, jo); // jne: not an int (or nothing)
            }
        }
        for (n, arg) in cp.sends.iter().enumerate() {
            if arg.kind == KSKIP {
                continue;
            }
            let off = cp.base.wrapping_add((n as u32) << SLOT_SHIFT);
            self.chk_int(arg.chk, arg.off, Target::Side(cp.callove));
            self.to_r9(arg.kind, arg.off, arg.lit);
            self.store_int_r9_base(KR, off);
            self.store_tag_base(KR, off, TAG_INT);
        }
        for k in 0..cp.classes.len() {
            let (c, m) = (cp.classes[k], cp.metas[k]);
            // A property row's only exit is its own overflow, before its
            // store (and the plan lets none follow a write): the
            // interpreter re-runs the whole call.
            let side = Target::Side(cp.callove);
            match c {
                C_PROP => self.emit_prop(m, side),
                C_PROPW => self.emit_propw(m, side),
                C_PROPINC => self.emit_propinc(m, side),
                C_PROPOPA => self.emit_propopa(m, side),
                _ => self.emit_op(c, m, &cp.rows[k], Target::Disarm(cp.callove)),
            }
        }
        // A body that ends in a builtin call: made by its helper, which
        // stores the result; a call it declines is the interpreter's
        // whole. (Nothing to do for an unused result: these builtins only
        // compute.)
        if cp.ret.kind == KQUICK {
            if let Some(d) = cp.dst {
                self.emit_helper(
                    jit_quick_tail as *const () as usize,
                    [HArg::Slot(KR, cp.ret.off), HArg::Slot(KT, d), HArg::Null],
                    u32::try_from(cp.ret.lit).unwrap_or(u32::MAX),
                    Target::Side(cp.callove),
                );
            }
            return;
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

    /// The check of a temporary the plan knows nothing about (`flag`: the
    /// meta's `ca`/`cb`), where a row reads it as an int: its tag, or the
    /// row is the interpreter's.
    fn chk_int(&mut self, flag: bool, off: u32, jo: Target) {
        if flag {
            self.cmpb(KT, off, TAG_INT);
            self.jcc(0x85, jo); // jne: not an int
        }
    }

    /// `cmp qword [base + off], imm8` (`REX.W 83 /7`, disp32): a slot
    /// word against a small constant.
    fn cmpq_slot(&mut self, base: u8, off: u32, imm: i8) {
        self.put(&[REX_W, 0x83, Self::rm3(base, 7)]);
        self.put(&off.to_le_bytes());
        self.put(&[imm.to_le_bytes()[0]]);
    }

    /// Copies a whole slot (the tag word and the payload word) through
    /// `rax` and `rcx`: a scalar, whatever its kind.
    fn copy_slot(&mut self, from_base: u8, from: u32, to_base: u8, to: u32) {
        // `mov rax, [from]`, `mov rcx, [from + 8]`.
        self.put(&[REX_W, 0x8B, Self::rm3(from_base, 0)]);
        self.put(&from.to_le_bytes());
        self.movrcx_at(from_base, from.wrapping_add(PAYLOAD));
        // `mov [to], rax`, `mov [to + 8], rcx`.
        self.put(&[REX_W, 0x89, Self::rm3(to_base, 0)]);
        self.put(&to.to_le_bytes());
        self.store_rcx_base(to_base, to);
    }

    /// `cmp dword [rax + disp32], imm8` (`83 /7 ib` with mod `10` and rm
    /// `000`, 32-bit operands: no `REX.W`): a `u32` field of what `rax`
    /// points at.
    fn cmpl_rax(&mut self, off: u32, imm: i8) {
        self.put(&[0x83, 0xB8]);
        self.put(&off.to_le_bytes());
        self.put(&[imm.to_le_bytes()[0]]);
    }

    /// `add` (`ext` 0), `sub` (5) or `cmp` (7) `qword [rax], imm8`
    /// (`REX.W 83 /ext ib`, mod `00`, rm `000`): the strong count of the
    /// array whose handle is in `rax`.
    fn count_op(&mut self, ext: u8, imm: i8) {
        self.put(&[REX_W, 0x83, ext << 3, imm.to_le_bytes()[0]]);
    }

    /// Emit one [`C_GREAD`] row: the global's slot (its address in the
    /// trace's table; 0 when the entry helper did not find it) holds a
    /// scalar, copied as it is, or an array, copied with one more handle on
    /// it; anything else leaves through the row's side exit. The value goes
    /// into the row's temporary, or into the variable of the assignment
    /// folded behind the read, whose old value is released as a
    /// [`VAL_SRC`] assignment releases it. Every test comes before the
    /// first change.
    fn emit_gread(&mut self, m: JitMeta, i: usize) {
        let side = Target::Side(i);
        self.movabs_rax(self.gtable + 8 * i64::from(m.slot));
        self.put(&[REX_W | REX_R, 0x8B, 0x08]); // mov r9, [rax]
        self.put(&[REX_W | REX_R | REX_B, 0x85, 0xC9]); // test r9, r9
        self.jcc(0x84, side); // jz: not resolved
        self.put(&[REX | REX_B, 0x0F, 0xB6, 0x09]); // movzx ecx, byte [r9]: the tag
        let known = self.synth_label();
        self.put(&[0x80, 0xF9, TAG_ARRAY]); // cmp cl, TAG_ARRAY
        self.jcc(0x84, Target::Label(known)); // je
        self.put(&[0x80, 0xF9, TAG_FLOAT]); // cmp cl, TAG_FLOAT
        self.jcc(0x87, side); // ja: not a scalar
        self.set_label(known);
        if m.kc == KC {
            // The variable's old value: an array with other handles loses
            // this one; the last handle, a string, an object, a reference:
            // the interpreted row.
            let store = self.synth_label();
            self.cmpb(KC, m.db, TAG_FLOAT);
            self.jcc(0x86, Target::Label(store)); // jbe: a scalar
            self.cmpb(KC, m.db, TAG_NONE);
            self.jcc(0x84, Target::Label(store)); // je: undefined
            self.cmpb(KC, m.db, TAG_ARRAY);
            self.jcc(0x85, side); // jne
            self.load_int(KC, m.db);
            self.count_op(7, 1); // cmp qword [rax], 1
            self.jcc(0x84, side); // je: the last handle
            self.count_op(5, 1); // sub qword [rax], 1
            self.set_label(store);
        }
        let copy = self.synth_label();
        self.put(&[0x80, 0xF9, TAG_ARRAY]); // cmp cl, TAG_ARRAY
        self.jcc(0x85, Target::Label(copy)); // jne
        self.put(&[REX_W | REX_B, 0x8B, 0x41, 0x08]); // mov rax, [r9 + 8]
        self.count_op(0, 1); // add qword [rax], 1
        self.set_label(copy);
        self.put(&[REX_W | REX_B, 0x8B, 0x01]); // mov rax, [r9]
        self.put(&[REX_W | REX_B, 0x8B, 0x49, 0x08]); // mov rcx, [r9 + 8]
                                                      // `mov [base + off], rax`, then the payload word from `rcx`.
        self.put(&[REX_W, 0x89, Self::rm3(m.kc, 0)]);
        self.put(&m.db.to_le_bytes());
        self.store_rcx_base(m.kc, m.db);
    }

    /// Copies the compiled variable at `from` into the temporary at `to`
    /// when it holds a scalar or, with `arr`, an array, which the
    /// temporary then holds a handle on as well (its count goes up).
    /// Anything else (a string, an object, a reference, an undefined
    /// variable) leaves through `jo`, nothing changed.
    fn copy_cv_value(&mut self, from: u32, to: u32, arr: bool, jo: Target) {
        let copy = self.synth_label();
        if arr {
            let other = self.synth_label();
            self.cmpb(KC, from, TAG_ARRAY);
            self.jcc(0x85, Target::Label(other)); // jne
            self.load_int(KC, from);
            self.count_op(0, 1); // add qword [rax], 1
            self.jmp(Target::Label(copy));
            self.set_label(other);
        }
        self.cmpb(KC, from, TAG_FLOAT);
        self.jcc(0x87, jo); // ja: not a scalar (or undefined)
        self.set_label(copy);
        self.copy_slot(KC, from, KT, to);
    }

    /// Branches on the truth of the compiled variable at `off`, decided
    /// as `plain_truth` decides it for what the trace handles: a bool by
    /// its value byte, an int by its payload, null false, and with `arr`
    /// an array by its element count (tested first: the variable held one
    /// when the plan was made). Anything else (a float, a string, an
    /// object, an undefined variable) leaves through `jo`. Every path ends
    /// in a jump.
    fn emit_cv_truth(
        &mut self,
        off: u32,
        arr: bool,
        on_true: Target,
        on_false: Target,
        jo: Target,
    ) {
        if arr {
            let other = self.synth_label();
            self.cmpb(KC, off, TAG_ARRAY);
            self.jcc(0x85, Target::Label(other)); // jne
            self.load_int(KC, off);
            self.cmpl_rax(ARRAY_COUNT, 0);
            self.jcc(0x85, on_true); // jne: not empty
            self.jmp(on_false);
            self.set_label(other);
        }
        let not_bool = self.synth_label();
        self.cmpb(KC, off, TAG_BOOL);
        self.jcc(0x85, Target::Label(not_bool)); // jne
        self.cmpb(KC, off.wrapping_add(BOOL_BYTE), 0);
        self.jcc(0x85, on_true); // jne: true
        self.jmp(on_false);
        self.set_label(not_bool);
        let not_int = self.synth_label();
        self.cmpb(KC, off, TAG_INT);
        self.jcc(0x85, Target::Label(not_int)); // jne
        self.cmpq_slot(KC, off.wrapping_add(PAYLOAD), 0);
        self.jcc(0x85, on_true); // jne: not 0
        self.jmp(on_false);
        self.set_label(not_int);
        self.cmpb(KC, off, TAG_NULL);
        self.jcc(0x85, jo); // jne: the interpreted row decides
        self.jmp(on_false);
    }

    /// Emit one `C_MOVE` row (`Op::Move` into the temporary `inst.c`): a
    /// folded literal stored as [`emit_consti`] stores one; a compiled
    /// variable copied whole once its tag says it holds a scalar; a
    /// temporary copied whole (checked first when the plan does not know
    /// what it holds) and, when the op consumes it, left null as the
    /// interpreted row leaves it.
    fn emit_move(&mut self, m: JitMeta, inst: &Inst, jo: Target) {
        match m.sc {
            KC => self.copy_cv_value(inst.a, inst.c, m.arr, jo),
            KT => {
                if m.ca {
                    self.cmpb(KT, inst.a, TAG_FLOAT);
                    self.jcc(0x87, jo); // ja: not a scalar
                }
                self.copy_slot(KT, inst.a, KT, inst.c);
                if m.consume && inst.a != inst.c {
                    self.store_imm_base(KT, inst.a, 0);
                    self.store_tag_base(KT, inst.a, TAG_NULL);
                }
            }
            _ => match m.arith {
                CONST_NULL => {
                    self.store_imm_base(KT, inst.c, 0);
                    self.store_tag_base(KT, inst.c, TAG_NULL);
                }
                CONST_BOOL => {
                    self.mov_ecx(u32::from(m.lit_c != 0));
                    self.store_cl_base(KT, inst.c.wrapping_add(BOOL_BYTE));
                    self.store_tag_base(KT, inst.c, TAG_BOOL);
                }
                _ => {
                    if let Ok(v) = i32::try_from(m.lit_c) {
                        self.store_imm_base(KT, inst.c, v);
                    } else {
                        self.movabs(true, m.lit_c);
                        self.store_int_r9_base(KT, inst.c);
                    }
                    self.store_tag_base(KT, inst.c, TAG_INT);
                }
            },
        }
    }

    /// `r9` = the handle of a receiver the trace already checked: the
    /// frame's `$this` (`KTHIS`), or the object a compiled variable holds.
    fn load_receiver_r9(&mut self, kind: u8, off: u32) {
        if kind == KTHIS {
            self.load_r9_frame(THIS_FIELD);
        } else {
            self.load_r9_at(KC, off.wrapping_add(PAYLOAD));
        }
    }

    /// The value a property row writes (or its second operand) into
    /// `rcx`: a temporary of the region is re-checked on every iteration
    /// (its entry value may be anything, including the `TAG_NULL` an
    /// earlier row's consume left, and a temporary the region itself
    /// produced passes on its fresh value); a compiled variable carries an
    /// exact entry guard like an arithmetic operand; a scratch slot of an
    /// inlined method is one the plan proved an `Int`; anything else is a
    /// plan-time constant.
    fn prop_src_rcx(&mut self, m: JitMeta, jo: Target) {
        match m.sc {
            KT => {
                self.cmpb(KT, m.sw, TAG_INT);
                self.jcc(0x85, jo); // jne: not an int
                self.movrcx_at(KT, m.sw.wrapping_add(PAYLOAD));
            }
            KC | KR => self.movrcx_at(m.sc, m.sw.wrapping_add(PAYLOAD)),
            _ => self.movabs_rcx(m.lit_c),
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
        self.store_int(m.kd, m.db);
        self.store_tag_base(m.kd, m.db, TAG_INT);
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
        if m.bare {
            // A row of an inlined method: its receiver and the object's
            // state were checked before the method's first row
            // (`emit_call`); only the handle and the slot array are loaded
            // again (the rows in between use the registers).
            self.load_receiver_r9(m.ka, m.sb);
            self.mov_rax_r9(SLOTS_FIRST);
            return;
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
        // (Checked before the method's first row for a `bare` row.)
        if m.bare {
            return;
        }
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
        self.prop_src_rcx(m, jo);
        // The result temporary: fully stored (tag and payload), as the
        // read's destination is.
        if m.db != NO_DST {
            self.store_rcx_base(m.kd, m.db);
            self.store_tag_base(m.kd, m.db, TAG_INT);
        }
        // The slot: tag already `Int` (the guard), so one payload word.
        self.store_rcx_at_rax(so.wrapping_add(PAYLOAD));
        // A consumed source temporary is left null, as the interpreted
        // row does (`put_tmp(slot, Value::Null)`).
        if m.consume {
            self.store_imm_base(m.sc, m.sw, 0);
            self.store_tag_base(m.sc, m.sw, TAG_NULL);
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
                self.store_int_r9_base(m.kd, m.db);
            } else {
                self.store_rcx_base(m.kd, m.db);
            }
            self.store_tag_base(m.kd, m.db, TAG_INT);
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
        self.prop_src_rcx(m, jo);
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
            self.store_int_r9_base(m.kd, m.db);
            self.store_tag_base(m.kd, m.db, TAG_INT);
        }
        // A consumed source temporary is left null, as for a write.
        if m.consume {
            self.store_imm_base(m.sc, m.sw, 0);
            self.store_tag_base(m.sc, m.sw, TAG_NULL);
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

    /// Emit one static-property `++`/`--` (`C_SPROPINC`): the guards and
    /// the cell of a write, then the increment of the cell's payload in
    /// place, exactly as [`emit_propinc`] increments a slot: the overflow
    /// exit comes before any store, the cell keeps its `Int` tag, and the
    /// result temporary (when the expression's value is used) gets the old
    /// value for a post operator (parked in `r9`) or the new one.
    fn emit_spropinc(&mut self, m: JitMeta, jo: Target) {
        self.emit_sprop_guards(m, jo);
        self.emit_sprop_cell(m, jo);
        self.cmpb_rax(REFCELL_VALUE, TAG_INT);
        self.jcc(0x85, jo); // jne: not an int
        self.movrcx_at_rax(REFCELL_VALUE.wrapping_add(PAYLOAD));
        if m.kc != 0 {
            self.mov_r9_rcx();
        }
        // `add rcx, 1` / `sub rcx, 1` (`83 /0` and `/5`).
        self.put(&[REX_W, 0x83, if m.inc { 0xC1 } else { 0xE9 }, 0x01]);
        self.jcc(0x80, jo); // jo: overflow, before any store
        self.store_rcx_at_rax(REFCELL_VALUE.wrapping_add(PAYLOAD));
        if m.db != NO_DST {
            if m.kc != 0 {
                self.store_int_r9_base(KT, m.db);
            } else {
                self.store_rcx_base(KT, m.db);
            }
            self.store_tag_base(KT, m.db, TAG_INT);
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
        // `$this::NAME`: the value is the constant of one class, which the
        // frame's `$this` must still be an object of.
        if m.ka == KTHIS {
            self.load_r9_frame(THIS_FIELD);
            self.put(&[REX_W | REX_R | REX_B, 0x85, 0xC9]); // test r9, r9
            self.jcc(0x84, jo); // je: no `$this`
            self.obj_class_from_r9();
            self.movabs_rax(m.lit_c);
            self.cmp_rax_r9();
            self.jcc(0x85, jo); // jne: another class
        }
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
            // `mov r?x, r10` (`4C 89 /r`: `REX.R` names `r10` in the reg
            // field, the destination is the rm field).
            HArg::Frame => self.put(&[REX_W | REX_R, 0x89, 0xD0 | reg]),
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
        self.emit_helper_call(helper, args, word);
        self.put(&[0x85, 0xC0]); // test eax, eax
        self.jcc(0x85, fail); // jnz: the helper declined
    }

    /// The call of a tape handler (`extern "C"`: the frame, the row, and a
    /// third argument where it takes one) for a row it either does or
    /// declines: `br` tells a handler that answers with a [`super::Br`]
    /// (1: declined) from one that answers with a `bool` (false: declined).
    /// The answer is a byte: only `al` is read.
    fn emit_row(&mut self, handler: usize, br: bool, args: [HArg; 3], fail: Target) {
        self.emit_helper_call(handler, args, 0);
        if br {
            self.put(&[0x3C, 0x01]); // cmp al, 1
        } else {
            self.put(&[0x84, 0xC0]); // test al, al
        }
        self.jcc(0x84, fail); // je / jz: the handler declined
    }

    /// The `new` row at `i` whose result the row after it assigns to a
    /// variable ([`ROW_NEW_RET`]): its handler answers 0 (declined,
    /// nothing changed), 1 (the object is in the result temporary: on to
    /// the assignment) or 2 (the object was made in the block of the one
    /// the variable held, which holds it: on past the assignment). Either
    /// way on, the row's poll first (a possible root is recorded).
    fn emit_new_row(&mut self, m: JitMeta, i: usize, j: usize) {
        let row = self.row_arg(i);
        self.emit_helper_call(
            usize::try_from(m.lit_a).unwrap_or(0),
            [HArg::Frame, row, HArg::Null],
            0,
        );
        // The assignment's row, and the one after it.
        let next = i + self.plan.span[j] as usize;
        let past = if next < self.plan.close {
            Target::Label(next + 1)
        } else {
            Target::Stub(next + 1)
        };
        let (over, made) = (self.synth_label(), self.synth_label());
        self.put(&[0x3C, 0x01]); // cmp al, 1
        self.jcc(0x82, Target::Side(i)); // jb: declined
        self.jcc(0x87, Target::Label(over)); // ja: made over the variable's object
        self.put(&[0x41, 0x80, 0x3B, 0x00]); // cmp byte [r11], 0
        self.jcc(0x85, Target::Poll(next, i)); // jne
        self.jmp(Target::Label(made));
        self.set_label(over);
        self.put(&[0x41, 0x80, 0x3B, 0x00]); // cmp byte [r11], 0
        self.jcc(0x85, Target::Poll(next + 1, i)); // jne
        self.jmp(past);
        self.set_label(made);
    }

    /// The address of row `i` of the tape the trace is of, as a helper's
    /// argument (the handlers read their operands from the row).
    fn row_arg(&self, i: usize) -> HArg {
        let row = self.tbase.wrapping_add(i * std::mem::size_of::<Inst>());
        HArg::Abs(i64::try_from(row).unwrap_or(0))
    }

    /// [`Emitter::emit_helper`]'s call alone: what the helper returned is
    /// in `eax`.
    fn emit_helper_call(&mut self, helper: usize, args: [HArg; 3], word: u32) {
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
    }

    /// `movsd xmm<x>, [slot payload]` (`F2 0F 10 /r`, disp32): the double
    /// of a slot whose tag was just checked `Float`.
    fn movsd_slot(&mut self, x: u8, kind: u8, off: u32) {
        self.put(&[0xF2, 0x0F, 0x10, Self::rm3(kind, x)]);
        self.put(&off.wrapping_add(PAYLOAD).to_le_bytes());
    }

    /// `cvtsi2sd xmm<x>, qword [slot payload]` (`F2 REX.W 0F 2A /r`,
    /// disp32): the int of a slot whose tag was just checked `Int`, as a
    /// double (`(double) lval`, as `zend_compare` of a long and a double).
    fn cvtsi2sd_slot(&mut self, x: u8, kind: u8, off: u32) {
        self.put(&[0xF2, REX_W, 0x0F, 0x2A, Self::rm3(kind, x)]);
        self.put(&off.wrapping_add(PAYLOAD).to_le_bytes());
    }

    /// `cvtsi2sd xmm<x>, rax`.
    fn cvtsi2sd_rax(&mut self, x: u8) {
        self.put(&[0xF2, REX_W, 0x0F, 0x2A, 0xC0 | (x << 3)]);
    }

    /// Emit `$cv op= v` (`+`, `-`, `*`) for operands the plan does not
    /// know to be ints ([`J_ASSIGN_OPS`]): by their tags, two ints as
    /// [`J_ASSIGN_OP`] computes them (an overflow leaves before the
    /// store); a float variable with a float or an int as doubles, the
    /// result stored as the variable's payload (its tag stays). An int
    /// variable with a float operand, and any other operand, leave the row
    /// to the interpreter.
    fn emit_assign_ops(&mut self, m: JitMeta, inst: &Inst, i: usize) {
        let side = Target::Side(i);
        let slot = |k: u8| k == KC || k == KT;
        let (a_other, done) = (self.synth_label(), self.synth_label());
        // Two ints.
        self.cmpb(KC, inst.a, TAG_INT);
        self.jcc(0x85, Target::Label(a_other)); // jne
        if slot(m.kb) {
            self.cmpb(m.kb, inst.b, TAG_INT);
            self.jcc(0x85, side); // jne: the variable becomes a float
        }
        self.load_int(KC, inst.a);
        self.to_r9(m.kb, inst.b, m.lit_b);
        match m.arith {
            0 => self.put(&[REX_W | REX_R, 0x01, 0xC8]), // add rax, r9
            1 => self.put(&[REX_W | REX_R, 0x29, 0xC8]), // sub rax, r9
            _ => self.put(&[REX_W | REX_B, 0x0F, 0xAF, 0xC1]), // imul rax, r9
        }
        self.jcc(0x80, side); // jo: overflow, before the store
        self.store_int(KC, inst.a);
        self.jmp(Target::Label(done));
        // A float variable with a float or an int.
        self.set_label(a_other);
        self.cmpb(KC, inst.a, TAG_FLOAT);
        self.jcc(0x85, side); // jne
        self.movsd_slot(0, KC, inst.a);
        if slot(m.kb) {
            let (b_int, op) = (self.synth_label(), self.synth_label());
            self.cmpb(m.kb, inst.b, TAG_FLOAT);
            self.jcc(0x85, Target::Label(b_int)); // jne
            self.movsd_slot(1, m.kb, inst.b);
            self.jmp(Target::Label(op));
            self.set_label(b_int);
            self.cmpb(m.kb, inst.b, TAG_INT);
            self.jcc(0x85, side); // jne
            self.cvtsi2sd_slot(1, m.kb, inst.b);
            self.set_label(op);
        } else {
            self.to_rax(m.kb, inst.b, m.lit_b);
            self.cvtsi2sd_rax(1);
        }
        // addsd / subsd / mulsd xmm0, xmm1 (`F2 0F 58 / 5C / 59 /r`).
        self.put(&[
            0xF2,
            0x0F,
            [0x58, 0x5C, 0x59][usize::from(m.arith.min(2))],
            0xC1,
        ]);
        // movsd [variable's payload], xmm0 (`F2 0F 11 /r`).
        self.put(&[0xF2, 0x0F, 0x11, Self::rm3(KC, 0)]);
        self.put(&inst.a.wrapping_add(PAYLOAD).to_le_bytes());
        self.set_label(done);
    }

    /// Emit a comparison and its jump for operands the plan does not know
    /// to be ints ([`J_CMPS`]): by their tags, two ints are compared as
    /// ints (the row's int test, `m.mask`); a float with a float or an int
    /// as doubles, the jump taken when the outcome is in the row's mask
    /// (`inst.k`, as `hot::jump_of` decides it: a NaN operand is
    /// unordered); any other operand leaves the row to the interpreter.
    fn emit_cmps(&mut self, m: JitMeta, inst: &Inst, i: usize, j: usize) {
        use crate::opcode::cmp_outcome::UNORDERED;
        let side = Target::Side(i);
        let tgt = self.jump_tgt(j);
        let slot = |k: u8| k == KC || k == KT;
        let (a_other, b_other) = (self.synth_label(), self.synth_label());
        let (floats, done) = (self.synth_label(), self.synth_label());
        // Two ints.
        if slot(m.ka) {
            self.cmpb(m.ka, inst.a, TAG_INT);
            self.jcc(0x85, Target::Label(a_other)); // jne
        }
        if slot(m.kb) {
            self.cmpb(m.kb, inst.b, TAG_INT);
            self.jcc(0x85, Target::Label(b_other)); // jne
        }
        self.to_rax(m.ka, inst.a, m.lit_a);
        self.to_r9(m.kb, inst.b, m.lit_b);
        self.put(&[REX_W | REX_R, 0x39, 0xC8]); // cmp rax, r9
        if let Some(cc) = jcc_for(m.mask) {
            self.jcc(cc, tgt);
        }
        self.jmp(Target::Label(done));
        // An int and no int: a float, or the interpreter's.
        if slot(m.kb) {
            self.set_label(b_other);
            self.cmpb(m.kb, inst.b, TAG_FLOAT);
            self.jcc(0x85, side); // jne
            if slot(m.ka) {
                self.cvtsi2sd_slot(0, m.ka, inst.a);
            } else {
                self.to_rax(m.ka, inst.a, m.lit_a);
                self.cvtsi2sd_rax(0);
            }
            self.movsd_slot(1, m.kb, inst.b);
            self.jmp(Target::Label(floats));
        }
        // No int and anything: a float with a float or an int.
        if slot(m.ka) {
            self.set_label(a_other);
            self.cmpb(m.ka, inst.a, TAG_FLOAT);
            self.jcc(0x85, side); // jne
            self.movsd_slot(0, m.ka, inst.a);
            if slot(m.kb) {
                let b_int = self.synth_label();
                self.cmpb(m.kb, inst.b, TAG_FLOAT);
                self.jcc(0x85, Target::Label(b_int)); // jne
                self.movsd_slot(1, m.kb, inst.b);
                self.jmp(Target::Label(floats));
                self.set_label(b_int);
                self.cmpb(m.kb, inst.b, TAG_INT);
                self.jcc(0x85, side); // jne
                self.cvtsi2sd_slot(1, m.kb, inst.b);
            } else {
                self.to_rax(m.kb, inst.b, m.lit_b);
                self.cvtsi2sd_rax(1);
            }
        }
        self.set_label(floats);
        self.put(&[0x66, 0x0F, 0x2E, 0xC1]); // ucomisd xmm0, xmm1
                                             // Unordered (a NaN): taken only by a mask that names it.
        if inst.k & UNORDERED != 0 {
            self.jcc(0x8A, tgt); // jp
        } else {
            self.jcc(0x8A, Target::Label(done)); // jp
        }
        match inst.k & 7 {
            1 => self.jcc(0x82, tgt), // jb: less
            2 => self.jcc(0x84, tgt), // je
            3 => self.jcc(0x86, tgt), // jbe
            4 => self.jcc(0x87, tgt), // ja: greater
            5 => self.jcc(0x85, tgt), // jne
            6 => self.jcc(0x83, tgt), // jae
            7 => self.jmp(tgt),
            _ => {}
        }
        self.set_label(done);
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
        // A variable holding a reference or an array of floats: the read
        // in place where the element is a list's scalar, else the wider
        // helper, with the frame and the variable's index.
        if m.arr {
            let (slow, done) = (self.synth_label(), self.synth_label());
            if self.emit_list_read(m, slow, false) {
                self.jmp(Target::Label(done));
            }
            self.set_label(slow);
            self.emit_helper(
                jit_dim_get_wide as *const () as usize,
                [HArg::Frame, Self::dim_arg(m), HArg::Slot(m.kc, m.db)],
                (m.sb >> SLOT_SHIFT) | (u32::from(m.kc == KC) * DIM_WIDE_TO_CV),
                Target::Side(i),
            );
            self.set_label(done);
            return;
        }
        // `$v = $arr['key']` has a helper of its own.
        if m.inc && m.kb == KL && m.kc == KC {
            self.emit_helper(
                jit_dim_get_key as *const () as usize,
                [
                    HArg::Slot(KC, m.sb),
                    HArg::Abs(m.lit_b),
                    HArg::Slot(KC, m.db),
                ],
                0,
                Target::Side(i),
            );
            return;
        }
        // An int element of a list in place, else the helper.
        let (slow, done) = (self.synth_label(), self.synth_label());
        if !m.inc && self.emit_list_read(m, slow, true) {
            self.jmp(Target::Label(done));
        }
        self.set_label(slow);
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
        self.set_label(done);
    }

    /// The element write of a `C_DIMW` row made in place, for the commonest
    /// cases: the offset is an int; the variable holds an array, or a
    /// reference to one that nothing is reading or writing; the table is
    /// this handle's alone (no separation) and a list; the value is a
    /// null, a bool, an int or a float (`int_only`: an int). Then either
    /// the offset is inside the list and the element there such a scalar
    /// (nothing to release; a hole would change the count): the value's
    /// two words are copied over the element's. Or the offset is the
    /// list's length, with room in the allocation and in the table's
    /// size: the words are written behind the last element, and the
    /// length, the next free key and the element count go up
    /// (`HashTable::set_int_plain`'s append). Both end with a jump to the
    /// label `done`. Any other case jumps to the label `slow` with nothing
    /// changed, where the row's helper call decides. Nothing is emitted
    /// for an append without an offset, an offset that is no slot and no
    /// int literal, or a literal value that is no such scalar.
    ///
    /// Uses `rax`, `rcx` and `r9` only, as [`Self::emit_list_read`].
    fn emit_list_write(&mut self, m: JitMeta, slow: usize, done: usize, int_only: bool) {
        use php_runtime::Value;
        let slow = Target::Label(slow);
        // A literal value: its two words, known here.
        let lit_words = if m.sc == KL {
            // SAFETY: a `KL` value of an array row is the address of a
            // literal of the op array the plan was made for (`plan_dim`).
            match unsafe { &*(m.lit_c as *const Value) } {
                Value::Int(v) => Some((i64::from(TAG_INT), *v)),
                Value::Float(x) if !int_only => {
                    Some((i64::from(TAG_FLOAT), i64::from_ne_bytes(x.to_ne_bytes())))
                }
                Value::Null if !int_only => Some((i64::from(TAG_NULL), 0)),
                Value::Bool(b) if !int_only => {
                    Some((i64::from(TAG_BOOL) | (i64::from(*b) << (8 * BOOL_BYTE)), 0))
                }
                _ => return,
            }
        } else if m.sc == KC || m.sc == KT {
            None
        } else {
            return;
        };
        match m.kb {
            KC | KT => {
                self.cmpb(m.kb, m.slot, TAG_INT);
                self.jcc(0x85, slow); // jne
                self.to_r9(m.kb, m.slot, 0);
            }
            KL => {
                // SAFETY: as above, for the offset.
                let Value::Int(at) = (unsafe { &*(m.lit_b as *const Value) }) else {
                    return;
                };
                self.movabs(true, *at);
            }
            _ => return,
        }
        // The value in its slot: a scalar.
        if lit_words.is_none() {
            if int_only {
                self.cmpb(m.sc, m.sw, TAG_INT);
                self.jcc(0x85, slow); // jne
            } else {
                self.cmpb(m.sc, m.sw, TAG_FLOAT);
                self.jcc(0x87, slow); // ja
            }
        }
        let (direct, store, append) = (self.synth_label(), self.synth_label(), self.synth_label());
        // What the variable holds: an array, or a reference to one that
        // nothing has borrowed.
        self.load_int(KC, m.sb);
        self.cmpb(KC, m.sb, TAG_ARRAY);
        self.jcc(0x84, Target::Label(direct)); // je
        self.cmpb(KC, m.sb, TAG_REF);
        self.jcc(0x85, slow); // jne
        self.cmpq_rax(REF_BORROW, 0);
        self.jcc(0x85, slow); // jne
        self.cmpb_rax(REF_VALUE, TAG_ARRAY);
        self.jcc(0x85, slow); // jne
        self.mov_rax_rax(REF_VALUE + PAYLOAD);
        self.set_label(direct);
        // A list.
        self.movabs_rcx(LIST_PACKED);
        self.cmp_at_rax_rcx(LIST_KIND);
        self.jcc(0x85, slow); // jne
                              // The table is this handle's alone: one strong handle, and the
                              // one weak count the strong ones hold together.
        self.cmpq_rax(0, 1);
        self.jcc(0x85, slow); // jne
        self.cmpq_rax(8, 1);
        self.jcc(0x85, slow); // jne
                              // The offset inside the list, or at its end.
        self.cmp_r9_at_rax(LIST_LEN);
        self.jcc(0x83, Target::Label(append)); // jae
        self.mov_rax_rax(LIST_ELEMS);
        self.shl_r9(4);
        self.add_rax_r9();
        // The element: a scalar is written over.
        self.cmpb_rax(0, TAG_FLOAT);
        self.jcc(0x87, slow); // ja
        self.set_label(store);
        if let Some((first, second)) = lit_words {
            self.movabs_rcx(first);
            self.movabs(true, second);
        } else {
            self.movrcx_at(m.sc, m.sw);
            self.to_r9(m.sc, m.sw, 0);
        }
        self.store_rcx_at_rax(0);
        self.store_r9_at_rax(PAYLOAD);
        self.jmp(Target::Label(done));
        // The list's next element (the flags are the length test's: past
        // the end is the helper's): room in the allocation and in the
        // table's size, then the counts, and the store above behind the
        // last element.
        self.set_label(append);
        self.jcc(0x85, slow); // jne
        self.cmp_r9_at_rax(LIST_CAP);
        self.jcc(0x83, slow); // jae
        self.mov_ecx_at_rax(TABLE_SIZE);
        self.cmp_r9_rcx();
        self.jcc(0x83, slow); // jae
        self.lea_rcx_r9_next();
        self.store_rcx_at_rax(LIST_LEN);
        self.store_rcx_at_rax(TABLE_NEXT);
        self.incl_at_rax(ARRAY_COUNT);
        self.mov_rax_rax(LIST_ELEMS);
        self.shl_r9(4);
        self.add_rax_r9();
        self.jmp(Target::Label(store));
    }

    /// The element read of a `C_DIMR` row made in place, for the commonest
    /// case: the offset is an int; the variable holds an array, or a
    /// reference (not being written) to one; the table is a list and the
    /// offset inside it; the element is a null, a bool, an int or a float
    /// (`int_only`: an int, what the rows after an int read rely on); and
    /// the destination holds a scalar, so nothing is released. The
    /// element's two words are then copied. Any other case jumps to the
    /// label `slow` with nothing changed, where the row's helper call
    /// decides. `false`, nothing emitted, for an offset that is no slot
    /// and no int literal.
    ///
    /// Uses `rax`, `rcx` and `r9` only. Nothing runs between the tests and
    /// the copy, so the reference's borrow is not counted.
    fn emit_list_read(&mut self, m: JitMeta, slow: usize, int_only: bool) -> bool {
        let slow = Target::Label(slow);
        match m.kb {
            KC | KT => {
                self.cmpb(m.kb, m.slot, TAG_INT);
                self.jcc(0x85, slow); // jne
                self.to_r9(m.kb, m.slot, 0);
            }
            KL => {
                // SAFETY: a `KL` offset of an array row is the address of a
                // literal of the op array the plan was made for
                // (`plan_dim`), alive as long as the trace.
                let lit = unsafe { &*(m.lit_b as *const php_runtime::Value) };
                let php_runtime::Value::Int(at) = lit else {
                    return false;
                };
                self.movabs(true, *at);
            }
            _ => return false,
        }
        let direct = self.synth_label();
        // What the variable holds: an array, or a reference to one.
        self.load_int(KC, m.sb);
        self.cmpb(KC, m.sb, TAG_ARRAY);
        self.jcc(0x84, Target::Label(direct)); // je
        self.cmpb(KC, m.sb, TAG_REF);
        self.jcc(0x85, slow); // jne
        self.cmpq_rax(REF_BORROW, 0);
        self.jcc(0x8C, slow); // jl: being written
        self.cmpb_rax(REF_VALUE, TAG_ARRAY);
        self.jcc(0x85, slow); // jne
        self.mov_rax_rax(REF_VALUE + PAYLOAD);
        self.set_label(direct);
        // A list, and the offset inside it (unsigned: a negative one is
        // outside as well).
        self.movabs_rcx(LIST_PACKED);
        self.cmp_at_rax_rcx(LIST_KIND);
        self.jcc(0x85, slow); // jne
        self.cmp_r9_at_rax(LIST_LEN);
        self.jcc(0x83, slow); // jae
        self.mov_rax_rax(LIST_ELEMS);
        self.shl_r9(4);
        self.add_rax_r9();
        // The element (a hole's tag is above every value's).
        if int_only {
            self.cmpb_rax(0, TAG_INT);
            self.jcc(0x85, slow); // jne
        } else {
            self.cmpb_rax(0, TAG_FLOAT);
            self.jcc(0x87, slow); // ja
        }
        // The destination: a scalar is written over.
        self.cmpb(m.kc, m.db, TAG_FLOAT);
        self.jcc(0x87, slow); // ja
        self.movrcx_at_rax(0);
        self.mov_r9_rax(PAYLOAD);
        self.store_rcx_first(m.kc, m.db);
        self.store_int_r9_base(m.kc, m.db);
        true
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
        // A plain write of a scalar into a list in place (it ends at
        // `done`), else the helper.
        let (slow, done) = (self.synth_label(), self.synth_label());
        if helper == jit_dim_set as *const () as usize {
            self.emit_list_write(m, slow, done, !m.arr);
        }
        self.set_label(slow);
        if m.arr {
            // (As in `emit_dimr`.)
            self.emit_helper(
                jit_dim_set_wide as *const () as usize,
                [HArg::Frame, Self::dim_arg(m), val],
                m.sb >> SLOT_SHIFT,
                Target::Side(i),
            );
        } else {
            self.emit_helper(
                helper,
                [HArg::Slot(KC, m.sb), Self::dim_arg(m), val],
                word,
                Target::Side(i),
            );
        }
        self.set_label(done);
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
            tbase: 0,
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
            pins: Vec::new(),
            gtable: 0,
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
        // The slots of the globals the trace reads itself, found once per
        // entry (the helper always returns 0).
        let greads = self.plan.class.iter().filter(|c| **c == C_GREAD).count();
        if greads != 0 {
            let n = u32::try_from(greads).unwrap_or(0);
            self.emit_helper(
                jit_resolve_globals as *const () as usize,
                [
                    HArg::Frame,
                    HArg::Abs(self.gtable),
                    HArg::Abs(self.gtable + 8 * i64::from(n)),
                ],
                n,
                Target::Stub(self.plan.head),
            );
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
        KC | KT => Arg {
            kind,
            off,
            lit: 0,
            chk: false,
        },
        #[allow(clippy::cast_possible_wrap)]
        KI => Arg {
            kind: KL,
            off: 0,
            lit: i64::from(off as i32),
            chk: false,
        },
        _ => Arg {
            kind: KL,
            off: 0,
            lit: resolved_lit(cst, off)?,
            chk: false,
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
    let (code, body) = if cfg!(target_arch = "aarch64") {
        a64::emit_native(&plan)?
    } else {
        emit_native(&plan)
    };
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
    // SAFETY: `p` is the fresh writable mapping of `len >= code.len()` bytes, distinct from
    // `code`.
    unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), p.cast::<u8>(), code.len()) };
    // AArch64's instruction fetch does not see the data cache: the new
    // code is cleaned to the point of unification first.
    #[cfg(target_arch = "aarch64")]
    a64::flush_icache(p.cast::<u8>(), code.len());
    // SAFETY: `p .. p + len` is the mapping made above; only its protection changes.
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
            // A row the tape has no instruction for: the op it stands for.
            let raw = match (inst.op, code.ops.get(d0 + i)) {
                (
                    TOp::Exit
                    | TOp::Step
                    | TOp::Ext
                    | TOp::CallInit
                    | TOp::CallSend
                    | TOp::VmOp
                    | TOp::ClassConst
                    | TOp::ClassConstTo
                    | TOp::StaticProp
                    | TOp::StaticPropTo,
                    Some(op),
                ) => {
                    format!(" = {op:?}")
                }
                _ => String::new(),
            };
            eprintln!(
                "jit:   [{}] {:?} a={} b={} c={} e={} k={}{}{}",
                d0 + i,
                inst.op,
                inst.a,
                inst.b,
                inst.c,
                inst.e,
                inst.k,
                note,
                raw
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
    if cfg!(target_arch = "aarch64") && !a64::supports(&p) {
        if debug_on() {
            eprintln!("jit: region {head}..={close} has a row the AArch64 backend does not emit");
        }
        return usize::MAX;
    }
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
    // The table of global slots: a word per `C_GREAD` row, then the rows'
    // op indexes (what the entry helper resolves).
    let greads: Vec<u32> = p
        .class
        .iter()
        .enumerate()
        .filter(|(_, c)| **c == C_GREAD)
        .filter_map(|(j, _)| u32::try_from(head + j).ok())
        .collect();
    let mut gtable = 0i64;
    if !greads.is_empty() {
        let Some(ptr) = mmap_scratch(greads.len() * 12) else {
            if debug_on() {
                eprintln!("jit: region {head}..={close}: global table mmap failed");
            }
            return usize::MAX;
        };
        // SAFETY: the mapping holds `len` words and `len` `u32`s behind
        // them, and is page-aligned (the `u32`s start at a multiple of 8).
        #[allow(clippy::cast_ptr_alignment)]
        unsafe {
            let list = ptr.add(greads.len() * 8).cast::<u32>();
            for (k, at) in greads.iter().enumerate() {
                list.add(k).write(*at);
            }
        }
        gtable = ptr as i64;
    }
    let (code, pins) = if cfg!(target_arch = "aarch64") {
        let mut em = a64::A64::new(p, tape.to_vec(), scratch);
        em.gtable = gtable;
        em.tbase = ctx.tbase as usize;
        em.emit();
        if !em.resolve() {
            return usize::MAX;
        }
        (em.bytes(), std::mem::take(&mut em.pins))
    } else {
        let mut em = Emitter::new(p, tape.to_vec(), scratch);
        em.gtable = gtable;
        em.tbase = ctx.tbase as usize;
        em.emit();
        em.resolve();
        let pins = std::mem::take(&mut em.pins);
        (em.code, pins)
    };
    if debug_on() {
        eprintln!(
            "jit: region {head}..={close} compiled to {} bytes",
            code.len()
        );
        eprintln!(
            "jit: bytes {}",
            code.iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join("")
        );
    }
    match place(&code) {
        Some(p) => {
            TRACES.fetch_add(1, Ordering::Relaxed);
            // The closures the trace names stay alive as long as it does
            // (for good). Kept outside the counted heap, like the tape:
            // compiling a loop must not show in `memory_get_usage()`.
            if !pins.is_empty() {
                php_memlimit::persistent(|| {
                    PINNED_OBJS.with(|all| {
                        let mut all = all.borrow_mut();
                        for (word, o) in pins {
                            if !all.iter().any(|(w, _)| *w == word) {
                                all.push((word, o));
                            }
                        }
                    });
                });
            }
            p as usize
        }
        None => usize::MAX,
    }
}

/// The `take_jump!` hook: `at` took a jump BACK to `target` (the caller
/// tested it; only then is this reached, and only when the tape has
/// tables) in frame `f`. Returns the `ip` to continue at: `target` when
/// the jump stays interpreted, else what the compiled loop ran and where
/// it asked to resume.
///
/// This is the part every interpreted back edge pays, so it is a leaf: a
/// loop that was refused (or whose guard failed) costs two loads and a
/// compare, one that is not hot yet a count on top of them. Running a
/// trace, compiling one and every way a trace leaves are
/// [`back_edge_slow`]'s. (Before the split a counted back edge cost about
/// 88 instructions: the call into the whole hook, then `edge_count`.)
///
/// # Safety
/// `at`/`target` point into the running tape described by `ctx`, with
/// `target <= at`, and `f` is the frame that runs it.
// Not inlined: `take_jump!`'s pointer test already keeps the JIT-off
// back edge at two instructions, and an inlined copy of this body at
// every jump site in `run_core` costs the interpreted path registers
// (measured: +210M instructions on `fib`).
#[inline(never)]
pub(super) unsafe fn back_edge(
    f: *mut crate::vm::Frame,
    ctx: &JitCtx,
    at: *const Inst,
    target: *const Inst,
) -> *const Inst {
    // A guard failure or refused compile disarms the current frame's
    // context (`disarm`), but the caller's pointer to it stays.
    let counts = ctx.jcnt.get();
    if counts.is_null() {
        return target;
    }
    #[allow(clippy::cast_sign_loss)] // `at` is a row of the tape at `tbase`
    // SAFETY: guaranteed by the caller.
    let idx = unsafe { at.offset_from(ctx.tbase) } as usize;
    // SAFETY: the cells cover the whole tape (`Tables::new(tape)`).
    let c = unsafe { &*ctx.jtr.get().add(idx) }.load(Ordering::Relaxed);
    if c == usize::MAX {
        // A refused loop: its back edges are counted on (from 0, where the
        // refusal left the counter), and when it has run `GIVE_UP` more hot
        // limits the tape is given up unless a trace of it has paid off.
        // SAFETY: the counters cover the whole tape.
        let count = unsafe { &*counts.add(idx) };
        let n = count.load(Ordering::Relaxed).wrapping_add(1);
        count.store(n, Ordering::Relaxed);
        if n != HOT_CACHE.load(Ordering::Relaxed).saturating_mul(GIVE_UP) {
            return target;
        }
        return refused_edge(ctx, target);
    }
    if c < MAX_ATTEMPTS {
        // Not compiled: one more back edge toward the limit. The edge that
        // reaches it is counted by `edge_count`, which compiles.
        // SAFETY: the counters cover the whole tape.
        let count = unsafe { &*counts.add(idx) };
        let n = count.load(Ordering::Relaxed).wrapping_add(1);
        if n < HOT_CACHE.load(Ordering::Relaxed) {
            count.store(n, Ordering::Relaxed);
            return target;
        }
    }
    // SAFETY: the caller's contract; the slot bases are the frame's.
    unsafe { back_edge_slow(super::frame_st(f), ctx, at, target) }
}

/// How many hot limits a refused loop runs on before its tape is given up
/// ([`refused_edge`]).
const GIVE_UP: i32 = 8;

/// A refused loop has run [`GIVE_UP`] more hot limits ([`back_edge`]): when
/// no trace of its tape has paid off by now, the tape is given up
/// (`Tape::jit_bad`): its back edges are no longer hooked, for frames
/// entered from now on and, by disarming this frame's context and raising
/// the safe point (the frame leaves `run_core` through the jump's own poll
/// and re-enters without the hook), right here. A function's other loops
/// have had that long to earn a trace; a refused loop on a live tape pays
/// the hook on every back edge.
#[cold]
#[inline(never)]
fn refused_edge(ctx: &JitCtx, target: *const Inst) -> *const Inst {
    // SAFETY: a context's tape pointer is null or the tape it was made for,
    // which outlives the frames that run it.
    if !ctx.tape.is_null() && unsafe { (*ctx.tape).jit_bad() } {
        ctx.disarm();
        crate::interrupt::raise_safe_point();
    }
    target
}

/// [`back_edge`] past its counting path: the cell holds a trace to run, or
/// the loop just became hot.
///
/// # Safety
/// As [`back_edge`]; `st` holds the slot bases of the frame.
#[inline(never)]
#[allow(clippy::too_many_lines)] // every way a trace leaves, in one place
unsafe fn back_edge_slow(
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
    // The VM, for the rows that read through its global scope and for the
    // plan that looks at what those reads would find.
    TRACE_VM.with(|vm| vm.set(ctx.vm.get()));
    // SAFETY: guaranteed by the caller.
    let idx = unsafe { at.offset_from(ctx.tbase) } as usize;
    // SAFETY: the cells cover the whole tape (`Tables::new(tape)`).
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
        // SAFETY: a cell value of at least `MAX_ATTEMPTS` that is not `usize::MAX` is the entry
        // address `attempt` stored: code `place` put in executable pages, compiled for the
        // `LoopFn` ABI.
        let f: LoopFn = unsafe { std::mem::transmute(c) };
        if debug_on() {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let k = N.fetch_add(1, Ordering::Relaxed);
            if k < 12 {
                // SAFETY: `st.f` is the frame that runs the tape (the caller's contract).
                let f = unsafe { &*st.f };
                let d: Vec<(u8, i64)> = (0..f.cvs.len().min(8))
                    // SAFETY: `i` is below the frame's variable count: `p` is that variable's
                    // 16-byte slot behind `st.cvs`, its tag byte and payload word initialized.
                    .map(|i| unsafe {
                        let p = st.cvs.add(i * 16);
                        (*p, std::ptr::read_unaligned(p.add(8).cast::<i64>()))
                    })
                    .collect();
                eprintln!("jit: ENTER at={idx} cv={d:?}");
            }
        }
        ctx.poll_row.set(std::ptr::null());
        // SAFETY: `f` is the trace's code (above), called with the slot
        // bases of the frame it was compiled for; the region only touches
        // the frame's slots, and exits leave them exactly as an
        // interpreted run would.
        let ret = unsafe { f(st.cvs, st.tmps, st.f.cast::<u8>()) };
        let r = ret.r;
        if debug_on() {
            static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let k = RUNS.fetch_add(1, Ordering::Relaxed);
            if k < 60 || k % 100_000 == 0 {
                eprintln!("jit: trace run #{k} at={idx} r={r}");
            }
            if k < 12 {
                // SAFETY: `st.f` is the frame that runs the tape (the caller's contract).
                let f = unsafe { &*st.f };
                let d: Vec<(u8, i64)> = (0..f.cvs.len().min(8))
                    // SAFETY: `i` is below the frame's variable count: `p` is that variable's
                    // 16-byte slot behind `st.cvs`, its tag byte and payload word initialized.
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
            // SAFETY: `r` is clamped below `ctx.tlen`: a row of this tape.
            return unsafe { ctx.tbase.add(r) };
        }
        if r < 0 {
            if debug_on() {
                // Which guard's slot broke the assumption: dump every tag.
                // The tag byte is the first byte of each slot's `Value`.
                // SAFETY: `st.f` is the frame that runs the tape (the caller's contract).
                let f = unsafe { &*st.f };
                let t: Vec<u8> = (0..f.cvs.len())
                    // SAFETY: `i` is below the frame's variable count: the tag byte of that
                    // slot behind `st.cvs`.
                    .map(|i| unsafe { *(st.cvs.add(i * 16)) })
                    .collect();
                eprintln!("jit:   cv tags = {t:?}");
                let t: Vec<u8> = (0..f.tmps.len())
                    // SAFETY: `i` is below the frame's temporary count: the tag byte of that
                    // slot behind `st.tmps`.
                    .map(|i| unsafe { *(st.tmps.add(i * 16)) })
                    .collect();
                eprintln!("jit:   tmp tags = {t:?}");
            }
            cell.store(usize::MAX, Ordering::Release);
            // The loop stays interpreted; its back edges count on from
            // here toward giving the tape up (`refused_edge`).
            // SAFETY: the counters cover the whole tape, and the context
            // is alive (tested above).
            unsafe { (*ctx.jcnt.get().add(idx)).store(0, Ordering::Relaxed) };
            let r = (-(r + 1)).clamp(0, (ctx.tlen - 1) as i64) as usize;
            // SAFETY: `r` is clamped below `ctx.tlen`: a row of this tape.
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
                // As after a failed guard: counted on from here.
                exits.store(0, Ordering::Relaxed);
            }
        } else {
            exits.store(0, Ordering::Relaxed);
            if !ctx.tape.is_null() {
                // SAFETY: a context's tape pointer, not null here, is the tape it was made for,
                // which outlives the frames that run it.
                unsafe { (*ctx.tape).jit_good() };
            }
        }
        // SAFETY: `resume` is clamped below `ctx.tlen`: a row of this tape.
        return unsafe { ctx.tbase.add(resume) };
    }
    // SAFETY: as above.
    unsafe { edge_count(st, ctx, at, target, idx) }
}

/// Not hot yet (count toward `HOT_LIMIT`), or hot and not yet compiled
/// (try to compile, then run or mark refused). Out of line so the
/// steady-state hook stays tiny.
///
/// # Safety
/// As [`back_edge_slow`]: `at`/`target` point into the running tape `ctx`
/// describes, with `target <= at`, `idx` is the row index of `at`, and
/// `st` holds the slot bases of the frame that runs the tape.
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
            // SAFETY: `target` is a row of the tape `ctx.tbase` starts (the caller's contract).
            let head = unsafe { target.offset_from(ctx.tbase) };
            eprintln!("jit: count #{k} at={idx} head={head} tlen={}", ctx.tlen);
        }
    }
    // SAFETY: `target` is a row of the tape `ctx.tbase` starts (the caller's contract).
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
        // The region can't be traced (and never could): the loop stays
        // interpreted, and its back edges count on from here toward
        // giving the tape up (`refused_edge`).
        // SAFETY: as above.
        unsafe { (*ctx.jcnt.get().add(idx)).store(0, Ordering::Relaxed) };
        target
    } else {
        // The counter now counts the trace's unproductive entries.
        // SAFETY: as above; now the cell is armed.
        unsafe {
            (*ctx.jcnt.get().add(idx)).store(0, Ordering::Relaxed);
            back_edge_slow(st, ctx, at, target)
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
        // SAFETY: `h.class` is a live `Rc`, one pointer wide: its word is initialized.
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
        // SAFETY: `v` is a live 16-byte `Value`, whose first byte is its initialized tag.
        let tag = |v: &Value| unsafe { *std::ptr::from_ref(v).cast::<u8>() };
        assert_eq!(tag(&Value::Null), TAG_NULL);
        assert_eq!(tag(&Value::Bool(true)), TAG_BOOL);
        assert_eq!(tag(&Value::Int(1)), TAG_INT);
        assert_eq!(tag(&Value::Str(php_runtime::ZStr::from("x"))), TAG_STR);
        let i = Value::Int(0x0102_0304_0506_0708);
        // SAFETY: the 8 bytes at `PAYLOAD` are inside the live `Int` `i`, its initialized
        // payload.
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
            // SAFETY: `BOOL_BYTE` is inside the live `Bool` `v`, its initialized value byte.
            let byte = unsafe { *std::ptr::from_ref(&v).cast::<u8>().add(BOOL_BYTE as usize) };
            assert_eq!(byte, u8::from(b));
        }
        let none: Option<Value> = None;
        // SAFETY: `none` is a live 16-byte `Option<Value>`, whose first byte is its initialized
        // niche tag.
        assert_eq!(unsafe { *std::ptr::from_ref(&none).cast::<u8>() }, TAG_NONE);
    }

    /// A temporary is what its last store made it: re-stored as an `Int`
    /// it is no `Bool` any more (a stale entry would tag an int `Bool`
    /// when it is copied into a variable), and a consumed one is in no
    /// list.
    #[test]
    fn tmp_kinds_replace_each_other() {
        let mut ts = TmpState::default();
        assert_eq!(ts.kind(16), TmpKind::Unknown);
        ts.set(16, TmpKind::Bool);
        assert_eq!(ts.kind(16), TmpKind::Bool);
        ts.set(16, TmpKind::Int);
        assert_eq!(ts.kind(16), TmpKind::Int);
        ts.set(16, TmpKind::Null);
        ts.set(32, TmpKind::Int);
        assert_eq!((ts.kind(16), ts.kind(32)), (TmpKind::Null, TmpKind::Int));
        ts.set(16, TmpKind::Unknown);
        assert_eq!(ts.known, vec![(32, TmpKind::Int)]);
    }

    /// A jump on a temporary reads the bool's byte, leaves the temporary
    /// null and jumps on the byte; one the plan knows nothing about takes
    /// a bool or an int by the tag (an int is tested against zero). A
    /// handler row is a helper call with the frame and the row's address.
    #[test]
    fn rows_on_temporaries_and_through_handlers() {
        let row = |op: TOp| Inst {
            a: 0x30,
            ..Inst::plain(op)
        };
        let plan = |class: u16, m: JitMeta| Plan {
            head: 0,
            close: 0,
            class: vec![class],
            meta: vec![m],
            guards: Vec::new(),
            entries: Vec::new(),
            span: vec![1],
            calls: Vec::new(),
            has_calls: false,
            frame: true,
            version_guard: false,
            version: 0,
        };
        for (class, op, cc) in [(C_JMPZT, TOp::JmpZT, 0x84u8), (C_JMPNZT, TOp::JmpNzT, 0x85)] {
            for checked in [false, true] {
                let m = JitMeta {
                    class,
                    ca: checked,
                    ..JitMeta::default()
                };
                let mut em = Emitter::new(plan(class, m), vec![row(op)], 0);
                em.emit_instr(0);
                let mut want = emitter();
                if checked {
                    want.cmpb(KT, 0x30, TAG_BOOL);
                    want.put(&[0x0F, 0x84, 0, 0, 0, 0]); // je: a bool
                    want.cmpb(KT, 0x30, TAG_INT);
                    want.put(&[0x0F, 0x85, 0, 0, 0, 0]); // jne out
                    want.put(&[0x48, 0x8B, 0x86, 0x38, 0, 0, 0]); // mov rax, [rsi + 0x38]
                    want.put(&[0x31, 0xC9]); // xor ecx, ecx
                    want.put(&[0x48, 0x85, 0xC0]); // test rax, rax
                    want.put(&[0x0F, 0x95, 0xC1]); // setne cl
                    want.put(&[0xE9, 0, 0, 0, 0]); // jmp past the bool's read
                }
                want.put(&[0x0F, 0xB6, 0x8E, 0x31, 0, 0, 0]); // movzx ecx, byte [rsi + 0x31]
                want.put(&[0x48, 0xC7, 0x86, 0x38, 0, 0, 0, 0, 0, 0, 0]); // mov qword [rsi + 0x38], 0
                want.put(&[0xC6, 0x86, 0x30, 0, 0, 0, TAG_NULL]); // mov byte [rsi + 0x30], 0
                want.put(&[0x85, 0xC9]); // test ecx, ecx
                want.put(&[0x0F, cc, 0, 0, 0, 0]); // jz / jnz to the target
                assert_eq!(em.code, want.code, "{op:?} checked={checked}");
            }
        }
        // A handler row: `emit_helper` with the frame, the row and nothing.
        let m = JitMeta {
            class: C_ROW,
            lit_a: 0x1234,
            inc: true,
            ..JitMeta::default()
        };
        let mut em = Emitter::new(plan(C_ROW, m), vec![row(TOp::StrlenC)], 0);
        em.tbase = 0x7000;
        em.emit_instr(0);
        let mut want = emitter();
        want.emit_row(
            0x1234,
            false,
            [HArg::Frame, HArg::Abs(0x7000), HArg::Null],
            Target::Side(0),
        );
        want.put(&[0x41, 0x80, 0x3B, 0x00]); // cmp byte [r11], 0
        want.put(&[0x0F, 0x85, 0, 0, 0, 0]); // jne to the poll exit
        assert_eq!(em.code, want.code);
        // The rows a handler row is planned for, by what they leave behind.
        let code = crate::opcode::OpArray::default();
        let strlen = Inst {
            c: 0x40,
            ..Inst::plain(TOp::StrlenC)
        };
        let (class, m, step) = plan_row(&code, 0, &strlen).expect("a handler row");
        assert_eq!(
            (class, step, m.db, m.arith, m.inc),
            (C_ROW, 1, 0x40, ROW_INT, false)
        );
        assert_eq!((m.sb, m.slot, m.sw, m.lit_c), (NO_DST, NO_DST, NO_DST, -1));
        let test = Inst::plain(TOp::TypeCheckJmpC);
        let (class, _, step) = plan_row(&code, 0, &test).expect("a handler row");
        assert_eq!((class, step), (C_ROWBR, 2));
        let cmp = Inst {
            e: 7,
            c: 0x50,
            ..Inst::plain(TOp::NumBinT)
        };
        let (_, m, _) = plan_row(&code, 0, &cmp).expect("a handler row");
        assert_eq!((m.db, m.arith), (0x50, ROW_BOOL));
        assert!(plan_row(&code, 0, &Inst::plain(TOp::Jmp)).is_none());
        // (A frameless call's row needs its op.)
        assert!(plan_row(&code, 0, &Inst::plain(TOp::FramelessQ)).is_none());
    }

    /// What the element read in place relies on: from the handle of a
    /// `Value::Array`, the word at `LIST_KIND` is `LIST_PACKED` for a list
    /// and nothing else, the word at `LIST_ELEMS` the address of its first
    /// element and the word at `LIST_LEN` their count; an element is a
    /// 16-byte `Option<Value>` whose hole has a tag above every value's.
    #[test]
    fn list_layout() {
        use php_runtime::{Value, ZArray};
        // SAFETY: (all reads below) a `Value` is a tag byte and a payload
        // word (`value_layout`); the words read are inside the live
        // array's allocation.
        let word = |v: &Value, off: u32| unsafe {
            let handle = std::ptr::read_unaligned(
                std::ptr::from_ref(v)
                    .cast::<u8>()
                    .add(PAYLOAD as usize)
                    .cast::<*const u8>(),
            );
            std::ptr::read_unaligned(handle.add(off as usize).cast::<i64>())
        };
        let address = |v: &Value| i64::try_from(std::ptr::from_ref(v) as usize).expect("fits");
        for n in [1i64, 2, 3, 8, 9, 100] {
            let a = ZArray::from_values((0..n).map(|i| {
                if i % 2 == 0 {
                    Value::Float(0.5)
                } else {
                    Value::Int(i)
                }
            }));
            let first = address(a.get_packed(0).expect("a list"));
            let slot = Value::Array(a.clone());
            assert_eq!(word(&slot, LIST_KIND), LIST_PACKED, "{n} elements");
            assert_eq!(word(&slot, LIST_ELEMS), first, "{n} elements");
            assert_eq!(word(&slot, LIST_LEN), n, "{n} elements");
            // The last element, where the read would find it.
            let last = address(a.get_packed(n - 1).expect("in range"));
            assert_eq!(last, first + 16 * (n - 1));
        }
        // No list: nothing allocated yet, a string key, an int key far
        // from the positions.
        let mut keyed = ZArray::new();
        assert_ne!(word(&Value::Array(keyed.clone()), LIST_KIND), LIST_PACKED);
        keyed.make_mut().insert(
            php_runtime::ArrayKey::from_zstr(&php_runtime::ZStr::from("k")),
            Value::Int(1),
        );
        assert_ne!(word(&Value::Array(keyed.clone()), LIST_KIND), LIST_PACKED);
        let mut sparse = ZArray::new();
        sparse
            .make_mut()
            .insert(php_runtime::ArrayKey::Int(1 << 40), Value::Int(1));
        assert_ne!(word(&Value::Array(sparse), LIST_KIND), LIST_PACKED);
        // A hole reads as `TAG_NONE`, above `TAG_FLOAT`.
        let hole: Option<Value> = None;
        // SAFETY: `hole` is a live 16-byte `Option<Value>`, whose first byte is its initialized
        // niche tag.
        assert!(unsafe { *std::ptr::from_ref(&hole).cast::<u8>() } > TAG_FLOAT);
        assert_eq!(std::mem::size_of::<Option<Value>>(), 16);
    }

    /// What the element write in place relies on: the handle of a
    /// `Value::Array` starts with its strong count and its weak count,
    /// both 1 while this handle is the table's only one (`Rc::get_mut`'s
    /// test, `ZArray::get_mut_unshared`).
    #[test]
    fn unshared_table_counts() {
        use php_runtime::{Value, ZArray};
        let mut a = ZArray::from_values([Value::Int(7), Value::Int(8)]);
        let slot = Value::Array(a.clone());
        // SAFETY: as in `list_layout`.
        let counts = |v: &Value| unsafe {
            let handle = std::ptr::read_unaligned(
                std::ptr::from_ref(v)
                    .cast::<u8>()
                    .add(PAYLOAD as usize)
                    .cast::<*const u64>(),
            );
            (
                std::ptr::read_unaligned(handle),
                std::ptr::read_unaligned(handle.add(1)),
            )
        };
        assert_eq!(counts(&slot), (2, 1));
        assert!(a.get_mut_unshared().is_none());
        drop(slot);
        let slot = Value::Array(a);
        assert_eq!(counts(&slot), (1, 1));
        let Value::Array(mut alone) = slot else {
            unreachable!()
        };
        assert!(alone.get_mut_unshared().is_some());
        let weak = alone.downgrade();
        assert_eq!(counts(&Value::Array(alone.clone())).1, 2);
        drop(weak);
    }

    /// What the append in place relies on: from the handle of a
    /// `Value::Array` holding a list, the word at `LIST_CAP` is the room
    /// of its allocation in elements, the 32-bit words at `TABLE_SIZE` and
    /// `ARRAY_COUNT` the table's size and its element count, the word at
    /// `TABLE_NEXT` the next free int key; and an append made the way the
    /// trace makes it (the element's words behind the last one, the three
    /// counts one up) leaves the table `HashTable::set_int_plain` leaves.
    #[test]
    fn list_append_layout() {
        use php_runtime::{Value, ZArray};
        // SAFETY: (all reads and writes below) as in `list_layout`; the
        // words are inside the live table's allocation, the element
        // written is inside the list's (the room was just tested), and
        // the table has no other handle.
        let handle = |v: &Value| unsafe {
            std::ptr::read_unaligned(
                std::ptr::from_ref(v)
                    .cast::<u8>()
                    .add(PAYLOAD as usize)
                    .cast::<*mut u8>(),
            )
        };
        // SAFETY: see the note above `handle`: the word is inside the live table.
        let word = |v: &Value, off: u32| unsafe {
            std::ptr::read_unaligned(handle(v).add(off as usize).cast::<i64>())
        };
        // SAFETY: see the note above `handle`: the word is inside the live table.
        let half = |v: &Value, off: u32| unsafe {
            std::ptr::read_unaligned(handle(v).add(off as usize).cast::<u32>())
        };
        for n in [1i64, 3, 8, 9, 100] {
            let mut a = ZArray::from_values((0..n).map(Value::Int));
            // Room for one more, the way the interpreter makes it.
            a.make_mut()
                .set_int_plain(n, Value::Int(-1))
                .expect("appended");
            let n = n + 1;
            let (room, size) = {
                let t = a.make_mut();
                (t.packed_capacity().expect("a list"), t.table_size())
            };
            let slot = Value::Array(a);
            assert_eq!(word(&slot, LIST_CAP), i64::try_from(room).expect("fits"));
            assert_eq!(half(&slot, TABLE_SIZE), size);
            assert_eq!(i64::from(half(&slot, ARRAY_COUNT)), n);
            assert_eq!(word(&slot, TABLE_NEXT), n);
            assert_eq!(word(&slot, LIST_LEN), n);
            if n >= i64::try_from(room).expect("fits") || n >= i64::from(size) {
                continue;
            }
            // The trace's append of `2.5` at offset `n`.
            // SAFETY: see the note above `handle`: `n` is below the list's room, so the element
            // written is inside its allocation, and the table has no other handle.
            unsafe {
                let h = handle(&slot);
                let elems = std::ptr::read_unaligned(h.add(LIST_ELEMS as usize).cast::<*mut u8>());
                let at = elems.add(16 * usize::try_from(n).expect("fits"));
                std::ptr::write_unaligned(at.cast::<i64>(), i64::from(TAG_FLOAT));
                std::ptr::write_unaligned(
                    at.add(PAYLOAD as usize).cast::<i64>(),
                    i64::from_ne_bytes(2.5f64.to_ne_bytes()),
                );
                std::ptr::write_unaligned(h.add(LIST_LEN as usize).cast::<i64>(), n + 1);
                std::ptr::write_unaligned(h.add(TABLE_NEXT as usize).cast::<i64>(), n + 1);
                let count = h.add(ARRAY_COUNT as usize);
                std::ptr::write_unaligned(
                    count.cast::<u32>(),
                    std::ptr::read_unaligned(count.cast::<u32>()) + 1,
                );
            }
            let Value::Array(mut made) = slot else {
                unreachable!()
            };
            let mut same = ZArray::from_values((0..n - 1).map(Value::Int));
            same.make_mut()
                .set_int_plain(n - 1, Value::Int(-1))
                .expect("appended");
            same.make_mut()
                .set_int_plain(n, Value::Float(2.5))
                .expect("appended");
            assert_eq!(made.len(), same.len(), "{n} elements");
            assert!(made.is_list() && made.is_without_holes());
            assert_eq!(made.next_free_element(), same.next_free_element());
            assert!(
                matches!(made.get_packed(n), Some(Value::Float(x)) if x.to_bits() == 2.5f64.to_bits())
            );
            assert!(matches!(made.get_packed(n - 1), Some(Value::Int(-1))));
            // And it goes on as a list.
            made.make_mut().push(Value::Int(5)).expect("appended");
            assert!(matches!(made.get_packed(n + 1), Some(Value::Int(5))));
        }
    }

    /// The instructions of the append in place
    /// ([`Emitter::emit_list_write`]).
    #[test]
    fn list_append_encodings() {
        let mut em = emitter();
        em.mov_ecx_at_rax(0x24);
        assert_eq!(em.code, vec![0x8B, 0x88, 0x24, 0x00, 0x00, 0x00]);
        em.code.clear();
        em.cmp_r9_rcx();
        assert_eq!(em.code, vec![0x49, 0x39, 0xC9]);
        em.code.clear();
        em.lea_rcx_r9_next();
        assert_eq!(em.code, vec![0x49, 0x8D, 0x49, 0x01]);
        em.code.clear();
        em.incl_at_rax(0x20);
        assert_eq!(em.code, vec![0xFF, 0x80, 0x20, 0x00, 0x00, 0x00]);
    }

    /// What the read through a reference relies on: from the payload word
    /// of a `Value::Reference`, the word at `REF_BORROW` counts the
    /// readers and is negative while the value is being written, and the
    /// value lies at `REF_VALUE`.
    #[test]
    fn reference_layout() {
        use php_runtime::{PhpRef, Value};
        let r = PhpRef::new(Value::Int(0x0102_0304_0506_0708));
        let slot = Value::Reference(r.clone());
        // SAFETY: as in `list_layout`; the box is alive.
        let (tag, cell) = unsafe {
            let words = std::ptr::from_ref(&slot).cast::<u8>();
            (
                *words,
                std::ptr::read_unaligned(words.add(PAYLOAD as usize).cast::<*const u8>()),
            )
        };
        assert_eq!(tag, TAG_REF);
        // SAFETY: `cell` is the live box of `r` (held by the test); the offset is inside it.
        let borrow =
            || unsafe { std::ptr::read_unaligned(cell.add(REF_BORROW as usize).cast::<i64>()) };
        // SAFETY: `cell` is the live box of `r` (held by the test); the offset is inside it.
        let value_tag = || unsafe { *cell.add(REF_VALUE as usize) };
        // SAFETY: `cell` is the live box of `r` (held by the test); the offset is inside it.
        let payload = || unsafe {
            std::ptr::read_unaligned(cell.add((REF_VALUE + PAYLOAD) as usize).cast::<i64>())
        };
        assert_eq!(borrow(), 0);
        assert_eq!(value_tag(), TAG_INT);
        assert_eq!(payload(), 0x0102_0304_0506_0708);
        {
            let reading = r.try_borrow().expect("free");
            assert_eq!(borrow(), 1);
            drop(reading);
        }
        {
            let writing = r.try_borrow_mut().expect("free");
            assert!(borrow() < 0);
            drop(writing);
        }
        assert_eq!(borrow(), 0);
        r.set(Value::Array(php_runtime::ZArray::from_values([
            Value::Int(1),
        ])));
        assert_eq!(value_tag(), TAG_ARRAY);
    }

    /// What the rows that move an array rely on: the payload word of a
    /// `Value::Array` is the handle of its `Rc` allocation, whose first
    /// word is the strong count, and the table's element count is
    /// `ARRAY_COUNT` bytes from the handle.
    #[test]
    fn array_handle_layout() {
        use php_runtime::{Value, ZArray};
        let a = ZArray::from_values([Value::Int(7), Value::Int(8), Value::Int(9)]);
        let slot = Value::Array(a.clone());
        let words = std::ptr::from_ref(&slot).cast::<u8>();
        // SAFETY: a `Value` is a tag byte and a payload word
        // (`value_layout`); the words read are inside the live array's
        // allocation.
        unsafe {
            assert_eq!(*words, TAG_ARRAY);
            let handle = std::ptr::read_unaligned(words.add(PAYLOAD as usize).cast::<*const u8>());
            let strong = || std::ptr::read_unaligned(handle.cast::<usize>());
            assert_eq!(strong(), a.refcount());
            assert_eq!(strong(), 2);
            let third = slot.clone();
            assert_eq!(strong(), 3);
            drop(third);
            assert_eq!(strong(), 2);
            let count = std::ptr::read_unaligned(handle.add(ARRAY_COUNT as usize).cast::<u32>());
            assert_eq!(count, 3);
        }
        let empty = Value::Array(ZArray::new());
        let words = std::ptr::from_ref(&empty).cast::<u8>();
        // SAFETY: as above.
        unsafe {
            let handle = std::ptr::read_unaligned(words.add(PAYLOAD as usize).cast::<*const u8>());
            let count = std::ptr::read_unaligned(handle.add(ARRAY_COUNT as usize).cast::<u32>());
            assert_eq!(count, 0);
        }
    }

    /// Where two paths meet, a temporary is what both left in it: the
    /// same kind stays, two different scalars are "a scalar", and one a
    /// path did not store is unknown.
    #[test]
    fn tmp_kinds_merge_at_a_join() {
        let mut then_path = TmpState::default();
        then_path.set(0, TmpKind::Int);
        then_path.set(16, TmpKind::Bool);
        then_path.set(32, TmpKind::Int);
        then_path.set(48, TmpKind::Scalar);
        let mut else_path = TmpState::default();
        else_path.set(0, TmpKind::Int);
        else_path.set(16, TmpKind::Int);
        else_path.set(48, TmpKind::Null);
        else_path.set(64, TmpKind::Int);
        for m in [then_path.merge(&else_path), else_path.merge(&then_path)] {
            assert_eq!(m.kind(0), TmpKind::Int);
            assert_eq!(m.kind(16), TmpKind::Scalar);
            assert_eq!(m.kind(32), TmpKind::Unknown);
            assert_eq!(m.kind(48), TmpKind::Scalar);
            assert_eq!(m.kind(64), TmpKind::Unknown);
            assert!(!m.has_val());
        }
        // A path that may have left an array: a scalar or an array, also
        // against a null or an int; still unknown against nothing.
        let mut arr_path = TmpState::default();
        arr_path.set(0, TmpKind::Val);
        arr_path.set(16, TmpKind::Val);
        arr_path.set(32, TmpKind::Val);
        arr_path.set(64, TmpKind::Val);
        for m in [then_path.merge(&arr_path), arr_path.merge(&then_path)] {
            assert_eq!(m.kind(0), TmpKind::Val);
            assert_eq!(m.kind(16), TmpKind::Val);
            assert_eq!(m.kind(32), TmpKind::Val);
            assert_eq!(m.kind(64), TmpKind::Unknown);
            assert!(m.has_val());
        }
        let mut cleared = then_path.clone();
        cleared.clear();
        assert_eq!(cleared.kind(0), TmpKind::Unknown);
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

    /// The loads of a float comparison ([`Emitter::emit_cmps`]): a double
    /// from a slot whose tag was checked, an int converted to one, from a
    /// slot or from `rax`. `F2` comes before `REX.W`; the `ModRM` byte names
    /// the slot base (`rdi` for a variable, `rsi` for a temporary) and the
    /// `xmm` register.
    #[test]
    fn float_compare_encodings() {
        let mut em = emitter();
        // movsd xmm0, [rdi + 0x18]
        em.movsd_slot(0, KC, 0x10);
        assert_eq!(
            em.code,
            vec![0xF2, 0x0F, 0x10, 0x87, 0x18, 0x00, 0x00, 0x00]
        );
        em.code.clear();
        // movsd xmm1, [rsi + 0x28]
        em.movsd_slot(1, KT, 0x20);
        assert_eq!(
            em.code,
            vec![0xF2, 0x0F, 0x10, 0x8E, 0x28, 0x00, 0x00, 0x00]
        );
        em.code.clear();
        // cvtsi2sd xmm0, qword [rdi + 0x18]
        em.cvtsi2sd_slot(0, KC, 0x10);
        assert_eq!(
            em.code,
            vec![0xF2, 0x48, 0x0F, 0x2A, 0x87, 0x18, 0x00, 0x00, 0x00]
        );
        em.code.clear();
        // cvtsi2sd xmm1, qword [rsi + 0x28]
        em.cvtsi2sd_slot(1, KT, 0x20);
        assert_eq!(
            em.code,
            vec![0xF2, 0x48, 0x0F, 0x2A, 0x8E, 0x28, 0x00, 0x00, 0x00]
        );
        em.code.clear();
        // cvtsi2sd xmm1, rax
        em.cvtsi2sd_rax(1);
        assert_eq!(em.code, vec![0xF2, 0x48, 0x0F, 0x2A, 0xC8]);
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
        // SAFETY: `raw` is the address of the live `cell`; its first word is the initialized
        // borrow flag.
        assert_eq!(unsafe { *(raw as *const usize) }, 0);
        // `as_ptr` names the value: 8 bytes past the cell's base.
        assert_eq!(
            cell.as_ptr() as usize - raw,
            usize::try_from(REFCELL_VALUE).unwrap()
        );
        let empty: std::cell::RefCell<Option<Box<u32>>> = std::cell::RefCell::new(None);
        assert_eq!(
            // SAFETY: the word 8 bytes into the live `empty` is its initialized option (null
            // for `None`).
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

    /// A loop's counter starts ahead by its length: nothing for a short
    /// loop or a row that closes none, down to a sixteenth of the limit
    /// left for a long one.
    #[test]
    fn a_long_loop_starts_ahead() {
        let back = |rows: u32| Inst {
            c: (rows * 32).wrapping_neg(),
            ..Inst::plain(TOp::Jmp)
        };
        // The row jumps `rows` rows back: the loop is one row longer.
        assert_eq!(head_start(&back(0), 1000), 0);
        assert_eq!(head_start(&back(3), 1000), 0);
        assert_eq!(head_start(&back(7), 1000), 500);
        assert_eq!(head_start(&back(19), 1000), 800);
        assert_eq!(head_start(&back(100), 1000), 938);
        assert_eq!(head_start(&back(19), 7), 6);
        assert_eq!(head_start(&back(19), 1), 0);
        let forward = Inst {
            c: 64,
            ..Inst::plain(TOp::Jmp)
        };
        assert_eq!(head_start(&forward, 1000), 0);
        assert_eq!(head_start(&Inst::plain(TOp::Nop), 1000), 0);
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
        // SAFETY: used between helper calls only, when nothing else accesses `tmp`.
        let tmp_is = |want: i64| matches!(unsafe { &*tmp.get() }, Value::Int(v) if *v == want);
        // An `Int` element into a temporary.
        // SAFETY: test-owned stand-ins for the slots `jit_dim_get` requires (variable,
        // temporary or variable, a live key `Value`), alive for the call.
        assert_eq!(unsafe { jit_dim_get(p, at(&Value::Int(1)), t, 0) }, 0);
        assert!(tmp_is(20));
        // A float element, a missing key, an offset that is no int.
        for k in [Value::Int(2), Value::Int(7), Value::Null, Value::Float(1.0)] {
            // SAFETY: test-owned stand-ins for the slots `jit_dim_get` requires (variable,
            // temporary or variable, a live key `Value`), alive for the call.
            assert_eq!(unsafe { jit_dim_get(p, at(&k), t, 0) }, 1);
            assert!(tmp_is(20));
        }
        // Into a variable: undefined, or holding a plain value.
        let to = CvSlot::new(None);
        let d = std::ptr::from_ref(&to).cast_mut().cast::<u8>();
        // SAFETY: test-owned stand-ins for the slots `jit_dim_get` requires (variable,
        // temporary or variable, a live key `Value`), alive for the call.
        assert_eq!(unsafe { jit_dim_get(p, at(&Value::Int(0)), d, 1) }, 0);
        assert!(matches!(to.get(), Some(Value::Int(10))));
        // A variable holding an array is not overwritten here.
        let busy = CvSlot::new(Some(Value::Array(ZArray::from_values([Value::Int(1)]))));
        let b = std::ptr::from_ref(&busy).cast_mut().cast::<u8>();
        // SAFETY: test-owned stand-ins for the slots `jit_dim_get` requires (variable,
        // temporary or variable, a live key `Value`), alive for the call.
        assert_eq!(unsafe { jit_dim_get(p, at(&Value::Int(0)), b, 1) }, 1);
        assert!(matches!(busy.get(), Some(Value::Array(_))));
        // A hash table's int keys are found too.
        let hash = CvSlot::new(Some(Value::Array(ZArray::from_pairs([
            (9i64, Value::Int(90)),
            (2i64, Value::Int(22)),
        ]))));
        let h = std::ptr::from_ref(&hash).cast_mut();
        // SAFETY: test-owned stand-ins for the slots `jit_dim_get` requires (variable,
        // temporary or variable, a live key `Value`), alive for the call.
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
        // SAFETY: test-owned stand-ins for the slots `jit_dim_get` requires (variable,
        // temporary or variable, a live key `Value`), alive for the call.
        assert_eq!(unsafe { jit_dim_get(np, at(&key("beta")), t, 0) }, 0);
        assert!(tmp_is(2));
        // SAFETY: test-owned stand-ins for the slots `jit_dim_get` requires (variable,
        // temporary or variable, a live key `Value`), alive for the call.
        assert_eq!(unsafe { jit_dim_get(np, at(&key("7")), t, 0) }, 0);
        assert!(tmp_is(70));
        // SAFETY: test-owned stand-ins for the slots `jit_dim_get` requires (variable,
        // temporary or variable, a live key `Value`), alive for the call.
        assert_eq!(unsafe { jit_dim_get(np, at(&key("gamma")), t, 0) }, 1);
        assert!(tmp_is(70));
        // A key the tape knows to be no integer key is looked up as it is.
        assert_eq!(
            // SAFETY: test-owned stand-ins for the slots `jit_dim_get` requires (variable,
            // temporary or variable, a live key `Value`), alive for the call.
            unsafe { jit_dim_get(np, at(&key("beta")), t, DIM_GET_EXACT_KEY) },
            0
        );
        assert!(tmp_is(2));
        // The helper of `$v = $arr['key']`: the same answers.
        let mut to2 = CvSlot::new(None);
        let d2 = std::ptr::from_mut(&mut to2);
        let (kb, kg) = (key("beta"), key("gamma"));
        // SAFETY: test-owned stand-ins for the slots `jit_dim_get_key` requires (variables and
        // a live key `Value`), alive for the call.
        assert_eq!(unsafe { jit_dim_get_key(np, &raw const kb, d2, 0) }, 0);
        assert!(matches!(to2.get(), Some(Value::Int(2))));
        // SAFETY: test-owned stand-ins for the slots `jit_dim_get_key` requires (variables and
        // a live key `Value`), alive for the call.
        assert_eq!(unsafe { jit_dim_get_key(np, &raw const kg, d2, 0) }, 1);
        assert!(matches!(to2.get(), Some(Value::Int(2))));
        let mut held_text = CvSlot::new(Some(key("old")));
        assert_eq!(
            // SAFETY: test-owned stand-ins for the slots `jit_dim_get_key` requires (variables
            // and a live key `Value`), alive for the call.
            unsafe { jit_dim_get_key(np, &raw const kb, &raw mut held_text, 0) },
            0
        );
        assert!(matches!(held_text.get(), Some(Value::Int(2))));
        let mut held_arr = CvSlot::new(Some(Value::Array(ZArray::from_values([Value::Int(1)]))));
        assert_eq!(
            // SAFETY: test-owned stand-ins for the slots `jit_dim_get_key` requires (variables
            // and a live key `Value`), alive for the call.
            unsafe { jit_dim_get_key(np, &raw const kb, &raw mut held_arr, 0) },
            1
        );
        assert!(matches!(held_arr.get(), Some(Value::Array(_))));
        // A string the destination holds is released, not leaked.
        let text = CvSlot::new(Some(key("old")));
        let tp = std::ptr::from_ref(&text).cast_mut().cast::<u8>();
        assert_eq!(
            // SAFETY: test-owned stand-ins for the slots `jit_dim_get` requires (variable,
            // temporary or variable, a live key `Value`), alive for the call.
            unsafe { jit_dim_get(np, at(&key("beta")), tp, DIM_GET_TO_CV) },
            0
        );
        assert!(matches!(text.get(), Some(Value::Int(2))));
        // No array in the variable at all.
        let none = CvSlot::new(None);
        let n = std::ptr::from_ref(&none).cast_mut();
        // SAFETY: test-owned stand-ins for the slots `jit_dim_get` requires (variable,
        // temporary or variable, a live key `Value`), alive for the call.
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
            // SAFETY: test-owned stand-ins for the slots `jit_dim_set` requires (a variable,
            // live key and value `Value`s), alive for the call.
            unsafe { jit_dim_set(p, at(&Value::Int(0)), at(&Value::Int(7)), 0) },
            0
        );
        // SAFETY: test-owned stand-ins for the slots `jit_dim_set` requires (a variable, live
        // key and value `Value`s), alive for the call.
        assert_eq!(unsafe { jit_dim_set(p, null, at(&Value::Int(8)), 0) }, 0);
        assert_eq!(
            // SAFETY: test-owned stand-ins for the slots `jit_dim_set` requires (a variable,
            // live key and value `Value`s), alive for the call.
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
            // SAFETY: test-owned stand-ins for the slots `jit_dim_set` requires (a variable,
            // live key and value `Value`s), alive for the call.
            unsafe { jit_dim_set(p, at(&Value::Int(0)), at(&Value::Float(1.0)), 0) },
            1
        );
        assert_eq!(
            // SAFETY: test-owned stand-ins for the slots `jit_dim_set` requires (a variable,
            // live key and value `Value`s), alive for the call.
            unsafe { jit_dim_set(p, at(&Value::Null), at(&Value::Int(1)), 0) },
            1
        );
        assert!(matches!(held(&arr).get_int(0), Some(Value::Int(7))));
        // A second handle on the table: the write must separate first.
        let other = held(&arr).clone();
        assert_eq!(
            // SAFETY: test-owned stand-ins for the slots `jit_dim_set` requires (a variable,
            // live key and value `Value`s), alive for the call.
            unsafe { jit_dim_set(p, at(&Value::Int(0)), at(&Value::Int(5)), 0) },
            1
        );
        // SAFETY: test-owned stand-ins for the slots `jit_dim_set` requires (a variable, live
        // key and value `Value`s), alive for the call.
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
            // SAFETY: test-owned stand-ins for the slots `jit_dim_set` requires (a variable,
            // live key and value `Value`s), alive for the call.
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
        // SAFETY: test-owned stand-ins for the slots `jit_dim_op` requires (a variable, live
        // key and operand `Value`s), alive for the call.
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
            // SAFETY: test-owned stand-ins for the slots `jit_dim_op` requires (a variable,
            // live key and operand `Value`s), alive for the call.
            unsafe { jit_dim_op(p, at(&k1), at(&Value::Int(1)), DIM_OP_ADD) },
            1
        );
        assert!(matches!(held(&arr).get_int(1), Some(Value::Int(i64::MAX))));
        let k9 = Value::Int(9);
        assert_eq!(
            // SAFETY: test-owned stand-ins for the slots `jit_dim_op` requires (a variable,
            // live key and operand `Value`s), alive for the call.
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
    /// A property row of an inlined method (`bare`) only loads the
    /// receiver again and reads or writes the slot: its checks were made
    /// before the method's first row, so a read and a plain write have no
    /// exit at all and `+=` only its overflow; a value or a result that is
    /// one of the method's own slots is addressed off the scratch buffer.
    #[test]
    fn inlined_method_property_rows() {
        let slot = 2u32;
        let so = slot << SLOT_SHIFT;
        let read = JitMeta {
            class: C_PROP,
            ka: KC,
            sb: 0x30,
            db: 0x40,
            slot,
            kd: KR,
            bare: true,
            ..JitMeta::default()
        };
        let mut em = emitter();
        em.emit_prop(read, Target::Side(0));
        let mut want = emitter();
        want.load_r9_at(KC, 0x30 + PAYLOAD);
        want.mov_rax_r9(SLOTS_FIRST);
        want.mov_rax_rax(so + PAYLOAD);
        want.store_int(KR, 0x40);
        want.store_tag_base(KR, 0x40, TAG_INT);
        assert_eq!(em.code, want.code);
        assert!(em.fixups.is_empty(), "a bare read has no exit");

        // The same read through the frame's `$this`.
        let mut em = emitter();
        em.emit_prop(
            JitMeta {
                ka: KTHIS,
                sb: 0,
                ..read
            },
            Target::Side(0),
        );
        let mut want = emitter();
        want.load_r9_frame(THIS_FIELD);
        want.mov_rax_r9(SLOTS_FIRST);
        want.mov_rax_rax(so + PAYLOAD);
        want.store_int(KR, 0x40);
        want.store_tag_base(KR, 0x40, TAG_INT);
        assert_eq!(em.code, want.code);

        // A write of a scratch slot the plan proved an `Int`: no tag check
        // of the source, no result.
        let write = JitMeta {
            class: C_PROPW,
            ka: KC,
            sb: 0x30,
            db: NO_DST,
            slot,
            sc: KR,
            sw: 0x50,
            bare: true,
            ..JitMeta::default()
        };
        let mut em = emitter();
        em.emit_propw(write, Target::Side(0));
        let mut want = emitter();
        want.load_r9_at(KC, 0x30 + PAYLOAD);
        want.mov_rax_r9(SLOTS_FIRST);
        want.movrcx_at(KR, 0x50 + PAYLOAD);
        want.store_rcx_at_rax(so + PAYLOAD);
        assert_eq!(em.code, want.code);
        assert!(em.fixups.is_empty(), "a bare write has no exit");

        // `+=`: the overflow is its only exit.
        let mut em = emitter();
        em.emit_propopa(
            JitMeta {
                class: C_PROPOPA,
                arith: 0,
                ..write
            },
            Target::Side(7),
        );
        assert_eq!(em.fixups.len(), 1, "jo only");
        // A row of the region itself keeps every check.
        let mut em = emitter();
        em.emit_prop(
            JitMeta {
                bare: false,
                kd: KT,
                ..read
            },
            Target::Side(0),
        );
        assert!(em.fixups.len() >= 6, "{}", em.fixups.len());
    }

    /// `++`/`--` of a static property works on the pinned cell as the
    /// property row works on a slot: the tag check, the old payload kept
    /// for a post operator, the overflow exit before the store.
    #[test]
    fn static_incdec_encoding() {
        let m = JitMeta {
            class: C_SPROPINC,
            lit_a: 0x1122_3344_5566,
            sg: false,
            db: 0x20,
            inc: true,
            kc: 1,
            ..JitMeta::default()
        };
        let mut em = emitter();
        em.emit_spropinc(m, Target::Side(3));
        let mut want = emitter();
        want.emit_sprop_cell(m, Target::Side(3));
        want.cmpb_rax(REFCELL_VALUE, TAG_INT);
        want.jcc(0x85, Target::Side(3));
        want.movrcx_at_rax(REFCELL_VALUE + PAYLOAD);
        want.mov_r9_rcx();
        want.put(&[REX_W, 0x83, 0xC1, 0x01]); // add rcx, 1
        want.jcc(0x80, Target::Side(3));
        want.store_rcx_at_rax(REFCELL_VALUE + PAYLOAD);
        want.store_int_r9_base(KT, 0x20);
        want.store_tag_base(KT, 0x20, TAG_INT);
        assert_eq!(em.code, want.code);

        // `--`, result unused: `sub rcx, 1` and only the cell is stored.
        let mut em = emitter();
        em.emit_spropinc(
            JitMeta {
                inc: false,
                kc: 0,
                db: NO_DST,
                ..m
            },
            Target::Side(3),
        );
        let mut want = emitter();
        want.emit_sprop_cell(m, Target::Side(3));
        want.cmpb_rax(REFCELL_VALUE, TAG_INT);
        want.jcc(0x85, Target::Side(3));
        want.movrcx_at_rax(REFCELL_VALUE + PAYLOAD);
        want.put(&[REX_W, 0x83, 0xE9, 0x01]); // sub rcx, 1
        want.jcc(0x80, Target::Side(3));
        want.store_rcx_at_rax(REFCELL_VALUE + PAYLOAD);
        assert_eq!(em.code, want.code);
    }

    /// A folded `$this::NAME` checks the class of the frame's `$this`
    /// before it stores the constant; a named constant does not.
    #[test]
    fn this_const_guard_encoding() {
        let m = JitMeta {
            class: C_CONSTI,
            lit_a: 7,
            arith: CONST_INT,
            db: 0x10,
            sg: false,
            ka: KTHIS,
            lit_c: 0x7700_1100,
            ..JitMeta::default()
        };
        let mut em = emitter();
        em.emit_consti(m, Target::Side(1));
        let mut want = emitter();
        want.load_r9_frame(THIS_FIELD);
        want.put(&[REX_W | REX_R | REX_B, 0x85, 0xC9]); // test r9, r9
        want.jcc(0x84, Target::Side(1));
        want.obj_class_from_r9();
        want.movabs_rax(0x7700_1100);
        want.cmp_rax_r9();
        want.jcc(0x85, Target::Side(1));
        want.store_imm_base(KT, 0x10, 7);
        want.store_tag_base(KT, 0x10, TAG_INT);
        assert_eq!(em.code, want.code);
        let mut em = emitter();
        em.emit_consti(JitMeta { ka: KC, ..m }, Target::Side(1));
        assert!(em.fixups.is_empty(), "no guard for a named constant");
    }

    /// The scalar rows of a ternary: a whole-slot copy is two loads and
    /// two stores through `rax`/`rcx` with the bases of both slots, the
    /// word compare is `REX.W 83 /7`, and the truth of a variable is
    /// decided on its tag (a bool by its value byte, an int by its
    /// payload, null false; anything else leaves).
    #[test]
    fn scalar_move_encodings() {
        let mut em = emitter();
        em.copy_slot(KC, 0x10, KT, 0x20);
        assert_eq!(
            em.code,
            [
                0x48, 0x8B, 0x87, 0x10, 0, 0, 0, // mov rax, [rdi + 0x10]
                0x48, 0x8B, 0x8F, 0x18, 0, 0, 0, // mov rcx, [rdi + 0x18]
                0x48, 0x89, 0x86, 0x20, 0, 0, 0, // mov [rsi + 0x20], rax
                0x48, 0x89, 0x8E, 0x28, 0, 0, 0, // mov [rsi + 0x28], rcx
            ]
        );
        let mut em = emitter();
        em.cmpq_slot(KC, 0x28, 0);
        assert_eq!(em.code, [0x48, 0x83, 0xBF, 0x28, 0, 0, 0, 0]);
        let mut em = emitter();
        em.cmpq_slot(KT, 0x8, 0);
        assert_eq!(em.code, [0x48, 0x83, 0xBE, 0x08, 0, 0, 0, 0]);

        // The truth dispatch: three tag tests, each path ending in a jump.
        let mut em = emitter();
        em.emit_cv_truth(
            0x30,
            false,
            Target::Stub(1),
            Target::Stub(2),
            Target::Side(3),
        );
        let mut want = emitter();
        let not_bool = want.synth_label();
        want.cmpb(KC, 0x30, TAG_BOOL);
        want.jcc(0x85, Target::Label(not_bool));
        want.cmpb(KC, 0x30 + BOOL_BYTE, 0);
        want.jcc(0x85, Target::Stub(1));
        want.jmp(Target::Stub(2));
        want.set_label(not_bool);
        let not_int = want.synth_label();
        want.cmpb(KC, 0x30, TAG_INT);
        want.jcc(0x85, Target::Label(not_int));
        want.cmpq_slot(KC, 0x30 + PAYLOAD, 0);
        want.jcc(0x85, Target::Stub(1));
        want.jmp(Target::Stub(2));
        want.set_label(not_int);
        want.cmpb(KC, 0x30, TAG_NULL);
        want.jcc(0x85, Target::Side(3));
        want.jmp(Target::Stub(2));
        assert_eq!(em.code, want.code);

        // A moved literal is stored as a folded constant is; a variable is
        // copied whole after its tag says "scalar"; a consumed temporary
        // of a known kind is copied without a check and left null.
        let mut em = emitter();
        let lit = JitMeta {
            class: C_MOVE,
            sc: KL,
            arith: CONST_BOOL,
            lit_c: 1,
            ..JitMeta::default()
        };
        let inst = Inst {
            c: 0x40,
            ..Inst::plain(TOp::MoveT)
        };
        em.emit_move(lit, &inst, Target::Side(0));
        let mut want = emitter();
        want.mov_ecx(1);
        want.store_cl_base(KT, 0x40 + BOOL_BYTE);
        want.store_tag_base(KT, 0x40, TAG_BOOL);
        assert_eq!(em.code, want.code);

        let mut em = emitter();
        let from_cv = JitMeta {
            class: C_MOVE,
            sc: KC,
            ..JitMeta::default()
        };
        let inst = Inst {
            a: 0x10,
            c: 0x40,
            ..Inst::plain(TOp::MoveT)
        };
        em.emit_move(from_cv, &inst, Target::Side(0));
        let mut want = emitter();
        want.cmpb(KC, 0x10, TAG_FLOAT);
        want.jcc(0x87, Target::Side(0));
        want.copy_slot(KC, 0x10, KT, 0x40);
        assert_eq!(em.code, want.code);

        // A variable that held an array when the plan was made: the array
        // case first (one more handle), then the scalar one.
        let mut em = emitter();
        let from_arr = JitMeta {
            arr: true,
            ..from_cv
        };
        em.emit_move(from_arr, &inst, Target::Side(0));
        let mut want = emitter();
        want.cmpb(KC, 0x10, TAG_ARRAY);
        want.put(&[0x0F, 0x85, 0, 0, 0, 0]); // jne to the scalar test
        want.put(&[0x48, 0x8B, 0x87, 0x18, 0, 0, 0]); // mov rax, [rdi + 0x18]
        want.put(&[0x48, 0x83, 0x00, 0x01]); // add qword [rax], 1
        want.put(&[0xE9, 0, 0, 0, 0]); // jmp to the copy
        want.cmpb(KC, 0x10, TAG_FLOAT);
        want.put(&[0x0F, 0x87, 0, 0, 0, 0]); // ja out
        want.copy_slot(KC, 0x10, KT, 0x40);
        assert_eq!(em.code, want.code);
        let mut em = emitter();
        em.count_op(7, 1);
        em.count_op(5, 1);
        em.cmpl_rax(0x30, 0);
        assert_eq!(
            em.code,
            [
                0x48, 0x83, 0x38, 0x01, // cmp qword [rax], 1
                0x48, 0x83, 0x28, 0x01, // sub qword [rax], 1
                0x83, 0xB8, 0x30, 0, 0, 0, 0, // cmp dword [rax + 0x30], 0
            ]
        );

        let mut em = emitter();
        let from_tmp = JitMeta {
            class: C_MOVE,
            sc: KT,
            consume: true,
            ..JitMeta::default()
        };
        em.emit_move(from_tmp, &inst, Target::Side(0));
        let mut want = emitter();
        want.copy_slot(KT, 0x10, KT, 0x40);
        want.store_imm_base(KT, 0x10, 0);
        want.store_tag_base(KT, 0x10, TAG_NULL);
        assert_eq!(em.code, want.code);
        assert!(em.fixups.is_empty(), "a known scalar is not checked");

        // An operand the plan knows nothing about is checked before use.
        let mut em = emitter();
        em.chk_int(false, 0x20, Target::Side(0));
        assert!(em.code.is_empty());
        em.chk_int(true, 0x20, Target::Side(0));
        assert_eq!(&em.code[..7], [0x80, 0xBE, 0x20, 0, 0, 0, TAG_INT]);
        assert_eq!(em.fixups.len(), 1);
    }

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
            chk: false,
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

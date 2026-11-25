// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The tape: pre-decoded, specialised instructions for the frame-local loop.
//!
//! php-src decides at compile time which handler runs an instruction:
//! `zend_vm_gen.php` generates one handler per opcode and operand-kind
//! combination (`ZEND_ADD_SPEC_TMPVARCV_CONST`, the smart-branch variants
//! `ZEND_IS_SMALLER_SPEC_TMPVARCV_TMPVARCV_JMPNZ` ...), an operand is a byte
//! offset from the frame (`EX_VAR`), and an instruction costs a handful of
//! machine instructions before its real work (ref: `Zend/zend_vm_def.h`,
//! `Zend/zend_vm_execute.h`, `Zend/zend_execute.h`).
//!
//! [`Op`] is the opposite: one generic instruction per operation, whose
//! operand kinds ([`Operand`]), sub-operation (`++` or `--`, which
//! comparison) and fused successor are data that [`super::hot::step`] decodes
//! on every execution, through bounds-checked slot vectors. Measured on
//! `for ($i = 0; $i < $n; $i++) {}`, one fused op took 95 machine
//! instructions against 30 for php-src's two handlers, 80 of them overhead.
//!
//! The tape is the same program decoded once: [`lower`] turns each `Op` of
//! an op array into an [`Inst`] at the same index, whose opcode ([`TOp`])
//! names the operation *and* the kinds of its operands, and whose operand
//! fields are byte offsets into the frame's compiled-variable or temporary
//! slots (or an immediate integer). [`run_tape`] executes it with a `match`
//! on the opcode and unchecked slot access, polling for a safe point only
//! where a jump is taken.
//!
//! Only what cannot fail is specialised: plain numbers in plain slots. An
//! instruction whose operands are anything else (a string, a reference, an
//! undefined variable, an overflow) has changed nothing when it finds out
//! and hands the op to [`super::hot::step`], the bounds-checked fast path
//! that existed before, which in turn hands what it does not handle to the
//! generic op in `Vm::run`. Ops with no specialised form are [`TOp::Step`]
//! (run by `step`) or [`TOp::Exit`] (not hot ops: back to `Vm::run`), so a
//! tape always describes the whole op array and behaviour never depends on
//! what is specialised.
//!
//! # Families
//!
//! The numeric instructions (smart branches, `+ - *`, `++`/`--`, scalar
//! assignments) are generated here by [`tape_ops!`] from tables of operand
//! kinds. Every other specialised instruction belongs to a family module,
//! which decodes its ops (`lower`) and holds their handlers; a row of the
//! `family` table of the [`tape_ops!`] invocation gives each instruction
//! its opcode, its handler, the number of ops it covers and how it ends:
//!
//! * [`scalar`]: the rest of the frame-local scalar ops (bit operations,
//!   `%`, `/`, comparisons into a temporary, `!`, truth jumps, `??`, `?:`,
//!   the fused assign-and-branch forms);
//! * [`dims`]: array elements of a variable (`$a[k]` read, write, `op=`,
//!   `++`, `isset`, `foreach`);
//! * [`props`]: object and static properties, constants;
//! * [`strs`]: strings and the builtin ops (`.`, `.=`, `strlen()`,
//!   `count()`, type tests, `isset`/`empty`, casts);
//! * [`calls`]: calls and returns of user functions.
//!
//! A handler has the contract of a hot op's fast path
//! ([`super::hot`]): it completes its op without diagnostics, user code or
//! frame changes, or it changes nothing and says so, and the op then runs
//! through [`super::hot::step`]. A handler that may release a value or
//! allocate ends with a poll for a safe point (a queued destructor, a
//! delayed `memory_limit` violation), as every hot op does.
//!
//! # Safety
//!
//! Slot access is unchecked. It is sound because [`lower`] only emits a
//! specialised instruction when every slot offset in it is below the op
//! array's slot counts (`cv_names.len()`, `num_tmps`) or literal count and
//! every jump target is inside the tape, and [`run_tape`] only runs the tape
//! on a frame whose slot vectors are at least that long ([`usable`]). The
//! tape ends in an [`TOp::Exit`] sentinel, so an instruction pointer that
//! advances past the last op stops there.

use super::hot::{self, DECLINED, LEFT, NO_SAFE_POINT, SWITCHED};
use super::Num;
use crate::opcode::{BinKind, ClassSpec, FetchMode, Op, OpArray, Operand};
use crate::vm::{CvSlot, Frame, Vm};
use php_runtime::{BinaryOp, Value};
use std::cell::OnceCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, AtomicUsize};
use std::sync::OnceLock;

/// log2 of the size of a slot (a compiled variable, a temporary, a
/// literal): an operand's byte offset is its index shifted by this.
const SLOT_SHIFT: u32 = 4;

/// One more than the largest slot index a tape operand can name (its byte
/// offset then fits the operand field with room to spare).
const SLOT_LIMIT: u32 = 1 << 27;

// The layout the byte offsets rely on.
const _: () = {
    assert!(std::mem::size_of::<Value>() == 1 << SLOT_SHIFT);
    assert!(std::mem::size_of::<Option<Value>>() == 1 << SLOT_SHIFT);
    assert!(std::mem::size_of::<CvSlot>() == 1 << SLOT_SHIFT);
    assert!(std::mem::align_of::<CvSlot>() == std::mem::align_of::<Option<Value>>());
    assert!(std::mem::size_of::<Inst>() == 32);
};

/// An op array's tape ([`OpArray::tape`]), decoded when the op array first
/// runs in [`run_tape`].
///
/// Its buffer is kept out of `memory_get_usage()`, like the run-time cache
/// (php-src's handlers and operand offsets are part of the op array the
/// compiler allocated; decoding on first use must not show as memory the
/// script allocated between two readings).
#[derive(Default)]
pub(crate) struct Tape {
    insts: OnceCell<Box<[Inst]>>,
    /// The trace JIT's per-instruction hot-loop tables (see [`jit`]),
    /// created when the tape is decoded with the JIT enabled. Their raw
    /// pointers sit beside the cell so [`run_tape`] reads them with two
    /// plain loads; `kill` nulls them (freeing the traces for later
    /// frames of this tape) while the cell keeps the arrays alive for
    /// frames already running.
    jit: OnceCell<jit::Tables>,
    jit_cnt: std::sync::atomic::AtomicPtr<AtomicI32>,
    jit_tr: std::sync::atomic::AtomicPtr<AtomicUsize>,
    /// A trace from this tape has proved useful (see [`Tape::jit_good`]).
    jit_good: std::sync::atomic::AtomicBool,
    /// What `calls::new_stored` found out about each `new` instruction that
    /// ran a constructor as stores, by the instruction's site number
    /// (`Inst.p`): see [`calls::NewSite`]. Grown outside the counted heap,
    /// like the op array's other run-time caches.
    new_sites: std::cell::RefCell<Vec<calls::NewSite>>,
}

impl Tape {
    /// The instructions, decoded from `code` on first use.
    #[inline]
    fn get(&self, code: &OpArray) -> &[Inst] {
        self.insts.get_or_init(|| {
            php_memlimit::persistent(|| {
                let tape = lower(code);
                // Tables only for a tape that has a loop the JIT could
                // ever compile: without them `run_tape` picks the core
                // without the back-edge hook for every frame of it.
                if jit::enabled() && jit::has_loop_close(&tape) {
                    self.jit_init(&tape);
                }
                tape
            })
        })
    }

    /// The hot-loop tables, created with the tape.
    fn jit_init(&self, tape: &[Inst]) {
        let jt = self.jit.get_or_init(|| jit::Tables::new(tape));
        self.jit_cnt.store(
            jt.cnt() as *mut AtomicI32,
            std::sync::atomic::Ordering::Relaxed,
        );
        self.jit_tr.store(
            jt.tr() as *mut AtomicUsize,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    /// Stop arming traces for this tape (a trace was refused or its
    /// guards failed): frames entered after this run the tape fully
    /// interpreted, without the per-back-edge hook.
    pub(crate) fn jit_kill(&self) {
        self.jit_cnt
            .store(std::ptr::null_mut(), std::sync::atomic::Ordering::Relaxed);
        self.jit_tr
            .store(std::ptr::null_mut(), std::sync::atomic::Ordering::Relaxed);
    }

    /// This tape produced a trace that paid for itself (compiled, or ran
    /// to a poll/cap): never kill its tracing.
    pub(crate) fn jit_good(&self) {
        self.jit_good
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// A loop of this tape that was refused or whose trace failed its
    /// entry guards has run on for a while (`jit::back_edge` counts its
    /// back edges). If this tape has not produced a useful trace yet, stop
    /// hooking its back edges (frames entered after this run it fully
    /// interpreted); the per-back-edge hook is worth its cost once a trace
    /// has paid off. Returns whether the tape was killed (so callers can
    /// stop hooking their own back edges too).
    pub(crate) fn jit_bad(&self) -> bool {
        if self.jit_good.load(std::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        self.jit_kill();
        true
    }
}

impl Drop for Tape {
    fn drop(&mut self) {
        if let Some(buf) = self.insts.take() {
            php_memlimit::persistent(|| drop(buf));
        }
        let sites = self.new_sites.get_mut();
        if sites.capacity() != 0 {
            // The class handles go as the counted memory they pin, the
            // buffer as it came.
            sites.clear();
            let sites = std::mem::take(sites);
            php_memlimit::persistent(|| drop(sites));
        }
    }
}

impl std::fmt::Debug for Tape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.insts.get() {
            Some(t) => write!(f, "Tape({} instructions)", t.len()),
            None => f.write_str("Tape(not decoded)"),
        }
    }
}

/// Operand kind: a compiled variable (`a`/`b` is its byte offset among the
/// frame's compiled variables).
const KC: u8 = 0;
/// Operand kind: a temporary (byte offset among the frame's temporaries).
const KT: u8 = 1;
/// Operand kind: an integer literal that fits `i32`, stored in the operand
/// field itself.
const KI: u8 = 2;
/// Operand kind: any other numeric literal (byte offset in the literal
/// table).
const KL: u8 = 3;
/// Operand kind: a byte offset from `rdx`, the JIT's per-trace scratch
/// buffer holding an inlined callee's slots (JIT-only; no tape family
/// emits it).
const KR: u8 = 4;

/// One pre-decoded instruction (32 bytes, the size of an [`Op`] and of a
/// `zend_op`).
///
/// What the operand fields hold is the instruction's own business (its
/// family's `lower` writes them and its handler reads them), with one
/// rule: an instruction that jumps has its target in `c`, as a signed byte
/// offset from itself ([`rel`]).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Inst {
    /// What to do, operand kinds included.
    op: TOp,
    /// The comparison mask of a smart branch
    /// ([`crate::opcode::cmp_outcome`]); free for other instructions.
    k: u8,
    /// A small extra operand.
    #[allow(dead_code)] // for the family modules
    e: u8,
    /// First operand.
    a: u32,
    /// Second operand.
    b: u32,
    /// Result slot (byte offset), or the jump target.
    c: u32,
    /// Fourth operand.
    #[allow(dead_code)] // for the family modules
    d: u32,
    /// Set by [`lower`], not by the families: whether `Vm::run` enters the
    /// tape at this instruction ([`worth_entering`]).
    enter: bool,
    /// Set by [`lower`]: whether the op's old fast path is
    /// `hot::step_ext` (not one of `hot::step`'s own arms), where a
    /// declined instruction hands it.
    ext: bool,
    /// A pointer-sized operand (the address of something that lives as
    /// long as the op array, such as a cache entry the tape owns).
    #[allow(dead_code)] // for the family modules
    p: usize,
}

impl Inst {
    const fn plain(op: TOp) -> Inst {
        Inst {
            op,
            k: 0,
            e: 0,
            a: 0,
            b: 0,
            c: 0,
            d: 0,
            enter: false,
            ext: false,
            p: 0,
        }
    }
}

/// The bases the operand offsets of a frame's instructions are relative to.
#[derive(Clone, Copy)]
struct St {
    /// The frame's compiled variables (`[Option<Value>]`).
    cvs: *mut u8,
    /// The frame's temporaries (`[Value]`).
    tmps: *mut u8,
    /// The op array's literals (`[Value]`).
    lits: *const u8,
    /// The frame itself, for what is not a slot (`$this`, the op array).
    #[allow(dead_code)] // for the family modules
    f: *mut Frame,
}

/// The compiled-variable slot at byte offset `off`.
///
/// # Safety
/// `off` is a slot offset [`lower`] validated for the frame `st` is of.
#[inline(always)]
#[allow(clippy::inline_always, clippy::cast_ptr_alignment)] // a slot offset from a slot vector's base
unsafe fn cv_slot<'a>(st: St, off: u32) -> &'a mut Option<Value> {
    // SAFETY: the caller's contract; a `CvSlot` is a transparent
    // `Option<Value>`.
    unsafe { &mut *st.cvs.add(off as usize).cast::<Option<Value>>() }
}

/// The temporary slot at byte offset `off`.
///
/// # Safety
/// As [`cv_slot`].
#[inline(always)]
#[allow(clippy::inline_always, clippy::cast_ptr_alignment)] // a slot offset from a slot vector's base
unsafe fn tmp_slot<'a>(st: St, off: u32) -> &'a mut Value {
    // SAFETY: the caller's contract.
    unsafe { &mut *st.tmps.add(off as usize).cast::<Value>() }
}

/// The slot bases of frame `f`, as [`run_tape`] derives them: what a thin
/// handler (`fam_arm!`'s `_f` modes) reads its operands through.
///
/// # Safety
/// `f` is the current frame, whose slot vectors the tape's offsets fit.
#[inline(always)]
#[allow(clippy::inline_always)] // a few loads at the top of a thin handler
unsafe fn frame_st(f: *mut Frame) -> St {
    // SAFETY: the caller's contract.
    let fr = unsafe { &mut *f };
    St {
        cvs: fr.cvs.as_mut_ptr().cast(),
        tmps: fr.tmps.as_mut_ptr().cast(),
        lits: fr.code.literals.as_ptr().cast(),
        f,
    }
}

/// The number operand `x` of kind `K` holds; `None` when it holds anything
/// else (or is an undefined variable).
///
/// # Safety
/// As [`cv_slot`]; for [`KL`], `x` is a validated literal offset.
#[inline(always)]
#[allow(
    clippy::inline_always,
    clippy::cast_possible_wrap,
    clippy::cast_ptr_alignment // a slot offset from a slot vector's base
)]
unsafe fn num<const K: u8>(st: St, x: u32) -> Option<Num> {
    // SAFETY: the caller's contract: `x` is a validated offset of the kind
    // `K` names.
    let v: &Value = unsafe {
        match K {
            KC => match &*st.cvs.add(x as usize).cast::<Option<Value>>() {
                Some(v) => v,
                None => return None,
            },
            KT => &*st.tmps.add(x as usize).cast::<Value>(),
            KI => return Some(Num::Int(i64::from(x as i32))),
            _ => &*st.lits.add(x as usize).cast::<Value>(),
        }
    };
    match v {
        Value::Int(i) => Some(Num::Int(*i)),
        Value::Float(f) => Some(Num::Float(*f)),
        _ => None,
    }
}

/// Whether a temporary slot holds nothing to release (a store over it is a
/// plain write).
#[inline(always)]
#[allow(clippy::inline_always)]
fn is_scalar(v: &Value) -> bool {
    matches!(
        v,
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
    )
}

/// The scalar `v` holds, copied; `None` for anything reference-counted.
/// (Where the value is known to be a scalar, copying its 16 bytes with
/// `std::ptr::read` is cheaper: this rebuilds it variant by variant.)
#[inline(always)]
#[allow(clippy::inline_always, dead_code)] // for the family modules
fn scalar_of(v: &Value) -> Option<Value> {
    match v {
        Value::Null => Some(Value::Null),
        Value::Bool(b) => Some(Value::Bool(*b)),
        Value::Int(i) => Some(Value::Int(*i)),
        Value::Float(f) => Some(Value::Float(*f)),
        _ => None,
    }
}

/// Whether two ints compare as a smart branch whose mask has the int
/// outcomes `M` (`LESS` = 1, `EQUAL` = 2, `GREATER` = 4) takes its jump:
/// `(M & int_outcome(x, y)) != 0`, as one comparison.
#[inline(always)]
#[allow(clippy::inline_always)]
fn int_test<const M: u8>(x: i64, y: i64) -> bool {
    match M {
        1 => x < y,
        3 => x <= y,
        4 => x > y,
        6 => x >= y,
        2 => x == y,
        _ => x != y,
    }
}

macro_rules! int_arith {
    (Add, $x:expr, $y:expr) => {
        $x.checked_add($y)
    };
    (Sub, $x:expr, $y:expr) => {
        $x.checked_sub($y)
    };
    (Mul, $x:expr, $y:expr) => {
        $x.checked_mul($y)
    };
}

macro_rules! float_arith {
    (Add, $x:expr, $y:expr) => {
        $x + $y
    };
    (Sub, $x:expr, $y:expr) => {
        $x - $y
    };
    (Mul, $x:expr, $y:expr) => {
        $x * $y
    };
}

/// Why [`run_core`] stopped: the op at `ip` is not specialised, or its
/// operands are not what its instruction handles (nothing changed): it runs
/// through [`hot::step`].
const WHY_STEP: u8 = 0;
/// Why [`run_core`] stopped: the op at `ip` is not a hot op.
const WHY_EXIT: u8 = 1;
/// Why [`run_core`] stopped: the op at `ip` is a hot op outside
/// [`hot::step`]'s core arms: it runs through [`hot::step_ext`].
const WHY_EXT: u8 = 3;
/// Why [`run_core`] stopped: the op at `ip` starts a call or passes an
/// argument ([`calls`]), which [`run_tape`] runs with the VM at hand.
const WHY_CALL: u8 = 4;
/// Why [`run_core`] stopped: the jump at `at` was taken (to `ip`) and a safe
/// point is due.
const WHY_POLL: u8 = 2;
/// Why [`run_core`] stopped: the op at `ip` is one [`run_vm_op`] runs with
/// the VM at hand (a read through the global scope, a frameless call).
const WHY_VM: u8 = 5;

/// Where and why [`run_core`] stopped.
struct Out {
    ip: *const Inst,
    at: *const Inst,
    why: u8,
}

/// The truth of the temporary a jump tests, which the jump consumes
/// (`ZEND_JMPZ` / `ZEND_JMPNZ` of a `TMP`: `i_zend_is_true`, then the
/// operand's release): a bool, an int, a null, a float that is no NaN, a
/// string (released out of line), or an array or object whose release
/// frees nothing it holds (`scalar::rc_tmp_truth`). `None`, nothing
/// changed, for anything else: a NaN (the generic op reports it), an array
/// or an object the temporary alone holds (its release may queue a
/// destructor), a reference.
#[inline(always)]
// The test of the tape's jumps on a temporary; a float's truth is its
// comparison with zero.
#[allow(clippy::inline_always, clippy::float_cmp)]
fn tmp_truth(slot: &mut Value) -> Option<bool> {
    let truth = match *slot {
        Value::Bool(b) => b,
        Value::Int(i) => i != 0,
        Value::Null => false,
        Value::Float(x) if !x.is_nan() => x != 0.0,
        Value::Str(_) => return Some(str_truth(slot)),
        Value::Array(_) | Value::Object(_) => return scalar::rc_tmp_truth(slot),
        _ => return None,
    };
    // A scalar owns nothing: overwritten without the drop glue.
    std::mem::forget(std::mem::take(slot));
    Some(truth)
}

/// [`tmp_truth`] of a temporary holding a string, which is released: false
/// for `""` and `"0"` (`zend_is_true` of a string).
#[inline(never)]
fn str_truth(slot: &mut Value) -> bool {
    match std::mem::take(slot) {
        Value::Str(s) => !(s.is_empty() || s.as_bytes() == b"0"),
        // Not reached: the caller saw a string.
        _ => false,
    }
}

#[inline(always)]
#[allow(clippy::inline_always)]
fn step_out(ip: *const Inst) -> Out {
    Out {
        ip,
        at: ip,
        why: WHY_STEP,
    }
}

/// The instruction after a fused loop step that did not jump back (its
/// smart branch and jump op skipped): out of line and cold, so that the
/// compiler lays the loop's jump back out as the straight path (inline, it
/// put the jump out of line: one more instruction per iteration).
#[cold]
#[inline(never)]
fn loop_exit(ip: *const Inst) -> *const Inst {
    ip.wrapping_add(3)
}

/// Takes the jump of the instruction at `$ip` and polls for a safe point,
/// as `ZEND_VM_JMP` does (`ZEND_VM_INTERRUPT_CHECK`): the instructions
/// between two taken jumps are specialised ones, which queue no destructor
/// and allocate nothing, so only an asynchronous request (a signal, a
/// timeout) can be pending, and it is seen here.
macro_rules! take_jump {
    ($ip:ident, $inst:ident, $st:ident, $jit:expr, $ctx:expr) => {{
        let at = $ip;
        #[allow(clippy::cast_possible_wrap)]
        {
            $ip = $ip.byte_offset($inst.c as i32 as isize);
        }
        if crate::interrupt::safe_point_due() {
            return Out {
                ip: $ip,
                at,
                why: WHY_POLL,
            };
        }
        // The trace JIT: a backward jump onto a tape instruction is a loop
        // back edge (a jump of an instruction onto itself included: an
        // empty `for` loop is one fused step-compare-jump instruction),
        // and the hook counts it and may run (or compile and run) the
        // loop as machine code.  It returns the instruction to
        // continue at, which is the target itself when the loop is not
        // hot yet or cannot be compiled.  `$jit` is `run_core`'s const
        // generic: a frame without a live context runs the instantation
        // where the hook is compiled out entirely (its two-instruction
        // test measurably reshapes `run_core`: +114M instructions on
        // `fib`).  `$ctx` is `run_core`'s context parameter: non-null
        // exactly when `$jit` is true.
        if $jit && $ip <= at {
            // SAFETY: guaranteed by `$jit` and the test just made the
            // jump backward. (Reached from `run_core`'s `unsafe` block,
            // hence no `unsafe` here.)  The hook gets the frame, from
            // which it derives the slot bases when it needs them: `st`
            // is never passed by address (that keeps it a memory local
            // and demotes every slot-base access to the stack; measured:
            // +210M instructions on `fib`).
            $ip = jit::back_edge($st.f, &*$ctx, at, $ip);
            // A compiled loop leaves when the safe point comes due inside
            // it (at its head, at a nested loop's back edge, after a row
            // that allocates): it is serviced before the next instruction
            // runs, as the check above does for an interpreted jump, and
            // reported at the row the trace named.
            if crate::interrupt::safe_point_due() {
                return Out {
                    ip: $ip,
                    at: (&*$ctx).poll_at(at),
                    why: WHY_POLL,
                };
            }
        }
    }};
}

/// What a family handler that may jump decided. A trace calls such a
/// handler itself and reads the answer as a byte (`jit::Emitter::emit_row`):
/// the values are part of that contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Br {
    /// Done; execution goes on after the ops the instruction covers.
    Fall = 0,
    /// Its operands are not what it handles; nothing changed.
    Decline = 1,
    /// Done; execution goes on at the instruction's jump target.
    Jump = 2,
}

/// The arm of a `family` row in [`run_core`]: `$h` is the handler, `$n`
/// the number of ops the instruction covers, and the first argument how it
/// ends:
///
/// * `plain`: `unsafe fn(St, &Inst) -> bool` (`false`: declined, nothing
///   changed); for handlers that neither release a value nor allocate;
/// * `poll`: the same, followed by a poll for a safe point (anything that
///   may release a value or allocate);
/// * `branch`: `unsafe fn(St, &Inst) -> Br`; a taken jump polls, as every
///   jump does;
/// * `branch_poll`: the same, polling on the fall-through path as well;
/// * `switch_f`: `unsafe fn(*mut Frame, &Inst) -> isize`, a jump to the
///   byte offset the handler answers (0: declined).
macro_rules! fam_arm {
    // The thin modes: the handler is called with the frame alone (`st.f`)
    // and reads any slot base from it (`frame_st`). Passed all four bases,
    // the calls raise their weight in the loop's register allocation: three
    // more such arms put the variables' base of every op on the stack.
    (plain_f, $h:expr, $n:literal, $st:ident, $inst:ident, $ip:ident, $ctx:ident) => {{
        if !($h)($st.f, $inst) {
            return step_out($ip);
        }
        $ip = $ip.add($n);
    }};
    (poll_f, $h:expr, $n:literal, $st:ident, $inst:ident, $ip:ident, $ctx:ident) => {{
        if !($h)($st.f, $inst) {
            return step_out($ip);
        }
        let at = $ip;
        $ip = $ip.add($n);
        if crate::interrupt::safe_point_due() {
            return Out {
                ip: $ip,
                at,
                why: WHY_POLL,
            };
        }
    }};
    (branch_f, $h:expr, $n:literal, $st:ident, $inst:ident, $ip:ident, $ctx:ident) => {{
        match ($h)($st.f, $inst) {
            Br::Decline => return step_out($ip),
            Br::Fall => $ip = $ip.add($n),
            Br::Jump => take_jump!($ip, $inst, $st, JIT, $ctx),
        }
    }};
    (branch_poll_f, $h:expr, $n:literal, $st:ident, $inst:ident, $ip:ident, $ctx:ident) => {{
        match ($h)($st.f, $inst) {
            Br::Decline => return step_out($ip),
            Br::Fall => {
                let at = $ip;
                $ip = $ip.add($n);
                if crate::interrupt::safe_point_due() {
                    return Out {
                        ip: $ip,
                        at,
                        why: WHY_POLL,
                    };
                }
            }
            Br::Jump => take_jump!($ip, $inst, $st, JIT, $ctx),
        }
    }};
    // A jump to a target the handler picks (`scalar::match_jump`): it
    // answers the target's byte offset from the instruction, or 0 when it
    // declined. The targets are forward (no back edge for the JIT hook),
    // and a taken jump polls, as every jump does.
    (switch_f, $h:expr, $n:literal, $st:ident, $inst:ident, $ip:ident, $ctx:ident) => {{
        let off = ($h)($st.f, $inst);
        if off == 0 {
            return step_out($ip);
        }
        let at = $ip;
        $ip = $ip.byte_offset(off);
        if crate::interrupt::safe_point_due() {
            return Out {
                ip: $ip,
                at,
                why: WHY_POLL,
            };
        }
    }};
    (plain, $h:expr, $n:literal, $st:ident, $inst:ident, $ip:ident, $ctx:ident) => {{
        if !($h)($st, $inst) {
            return step_out($ip);
        }
        $ip = $ip.add($n);
    }};
    (poll, $h:expr, $n:literal, $st:ident, $inst:ident, $ip:ident, $ctx:ident) => {{
        if !($h)($st, $inst) {
            return step_out($ip);
        }
        let at = $ip;
        $ip = $ip.add($n);
        if crate::interrupt::safe_point_due() {
            return Out {
                ip: $ip,
                at,
                why: WHY_POLL,
            };
        }
    }};
    (branch, $h:expr, $n:literal, $st:ident, $inst:ident, $ip:ident, $ctx:ident) => {{
        match ($h)($st, $inst) {
            Br::Decline => return step_out($ip),
            Br::Fall => $ip = $ip.add($n),
            Br::Jump => take_jump!($ip, $inst, $st, JIT, $ctx),
        }
    }};
    (branch_poll, $h:expr, $n:literal, $st:ident, $inst:ident, $ip:ident, $ctx:ident) => {{
        match ($h)($st, $inst) {
            Br::Decline => return step_out($ip),
            Br::Fall => {
                let at = $ip;
                $ip = $ip.add($n);
                if crate::interrupt::safe_point_due() {
                    return Out {
                        ip: $ip,
                        at,
                        why: WHY_POLL,
                    };
                }
            }
            Br::Jump => take_jump!($ip, $inst, $st, JIT, $ctx),
        }
    }};
}

/// Defines [`TOp`], the opcode lookups [`lower`] uses and [`run_core`] from
/// the tables of specialised instruction families.
macro_rules! tape_ops {
    (
        cmp { $( $cn:ident = ($cka:ident, $ckb:ident, $cm:literal), )* }
        inccmp { $( $in:ident = ($iinc:literal, $ikb:ident, $im:literal), )* }
        arith_tmp { $( $an:ident = ($aop:ident, $aka:ident, $akb:ident), )* }
        arith_cv { $( $vn:ident = ($vop:ident, $vka:ident, $vkb:ident), )* }
        assign_op { $( $on:ident = ($oop:ident, $okb:ident), )* }
        family { $( $fname:ident = ($fh:expr, $fskip:literal, $fmode:ident), )* }
    ) => {
        /// A tape opcode: the operation and the kinds of its operands
        /// (`C` compiled variable, `T` temporary, `I` small integer
        /// literal, `L` other numeric literal).
        #[repr(u16)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[allow(clippy::upper_case_acronyms)]
        pub(crate) enum TOp {
            /// Not a hot op (or the end of the tape): back to `Vm::run`.
            Exit,
            /// A core hot op with no specialised form: [`hot::step`] runs
            /// it.
            Step,
            /// Any other hot op with no specialised form:
            /// [`hot::step_ext`] runs it.
            Ext,
            /// `Op::InitFCall` without a namespace fallback ([`calls`]).
            CallInit,
            /// `Op::Send` of a value ([`calls`]).
            CallSend,
            /// An op that is no call but needs the VM ([`run_vm_op`]).
            VmOp,
            /// `Op::Jmp`.
            Jmp,
            /// `Op::JmpZ` of a temporary (a bool, int or null on the tape).
            JmpZT,
            /// `Op::JmpNz` of a temporary (as [`TOp::JmpZT`]).
            JmpNzT,
            /// `++$cv` / `$cv++`, result unused.
            IncC,
            /// `--$cv` / `$cv--`, result unused.
            DecC,
            /// `++$cv` into a temporary.
            PreIncT,
            /// `--$cv` into a temporary.
            PreDecT,
            /// `$cv++` into a temporary.
            PostIncT,
            /// `$cv--` into a temporary.
            PostDecT,
            /// `$cv = $cv2`, result unused.
            AssignC,
            /// `$cv = TMP`, result unused.
            AssignT,
            /// `$cv = <small int literal>`, result unused.
            AssignI,
            /// `$cv = <scalar literal>`, result unused.
            AssignL,
            $(
                /// Smart branch (`Op::CmpJmp`).
                $cn,
            )*
            $(
                /// `++`/`--` of a compiled variable fused with the smart
                /// branch on it (`Op::IncDecCvCmpJmp` + `Op::CmpJmp`).
                $in,
            )*
            $(
                /// `Op::Bin` (`+`, `-`, `*`) into a temporary.
                $an,
            )*
            $(
                /// `Op::BinAssignCv` (`$cv = a op b`).
                $vn,
            )*
            $(
                /// `Op::AssignOpCv` (`$cv op= b`), result unused.
                $on,
            )*
            $(
                /// An instruction of a family module (see the `family`
                /// table).
                $fname,
            )*
        }

        impl TOp {
            fn cmp(ka: u8, kb: u8, m: u8) -> Option<TOp> {
                $( if ka == $cka && kb == $ckb && m == $cm { return Some(TOp::$cn); } )*
                None
            }
            fn inccmp(inc: bool, kb: u8, m: u8) -> Option<TOp> {
                $( if inc == $iinc && kb == $ikb && m == $im { return Some(TOp::$in); } )*
                None
            }
            fn arith_tmp(op: BinaryOp, ka: u8, kb: u8) -> Option<TOp> {
                $( if matches!(op, BinaryOp::$aop) && ka == $aka && kb == $akb { return Some(TOp::$an); } )*
                None
            }
            fn arith_cv(op: BinaryOp, ka: u8, kb: u8) -> Option<TOp> {
                $( if matches!(op, BinaryOp::$vop) && ka == $vka && kb == $vkb { return Some(TOp::$vn); } )*
                None
            }
            fn assign_op(op: BinaryOp, kb: u8) -> Option<TOp> {
                $( if matches!(op, BinaryOp::$oop) && kb == $okb { return Some(TOp::$on); } )*
                None
            }

            /// The trace JIT's [`jit::JitMeta`] for a fused row (see
            /// [`jit`]'s whitelist); `None` for every row the JIT does
            /// not compile in this version.
            pub(crate) fn jit_meta(self) -> Option<jit::JitMeta> {
                match self {
                    $(
                        TOp::$cn => Some(jit::JitMeta {
                            class: jit::J_CMP,
                            ka: $cka,
                            kb: $ckb,
                            mask: $cm,
                            ..jit::JitMeta::default()
                        }),
                    )*
                    $(
                        TOp::$in => Some(jit::JitMeta {
                            class: jit::J_INCCMP,
                            kb: $ikb,
                            mask: $im,
                            inc: $iinc,
                            ..jit::JitMeta::default()
                        }),
                    )*
                    $(
                        TOp::$vn => Some(jit::JitMeta {
                            class: jit::J_ARITH_CV,
                            ka: $vka,
                            kb: $vkb,
                            arith: jit::jit_arith_id!($vop),
                            ..jit::JitMeta::default()
                        }),
                    )*
                    $(
                        TOp::$an => Some(jit::JitMeta {
                            class: jit::J_ARITH_TMP,
                            ka: $aka,
                            kb: $akb,
                            arith: jit::jit_arith_id!($aop),
                            ..jit::JitMeta::default()
                        }),
                    )*
                    $(
                        TOp::$on => Some(jit::JitMeta {
                            class: jit::J_ASSIGN_OP,
                            ka: KC,
                            kb: $okb,
                            arith: jit::jit_arith_id!($oop),
                            ..jit::JitMeta::default()
                        }),
                    )*
                    _ => None,
                }
            }
        }

        /// Runs specialised instructions from `ip` until one stops it
        /// ([`Out`]): the loop of the tape. Inlined into [`run_tape`].
        ///
        /// # Safety
        /// `ip` points into a tape [`lower`] built for the op array the
        /// frame of `st` runs, and that frame is [`usable`].
        #[inline(always)]
        #[allow(
            clippy::inline_always,
            clippy::too_many_lines,
            clippy::float_arithmetic,
            // `x = x op y` comes out of `float_arith!` for every operator.
            clippy::assign_op_pattern,
            // Slot offsets are multiples of the 16-byte slot size from the
            // bases of `Vec<Value>` and `Vec<CvSlot>` buffers.
            clippy::cast_ptr_alignment
        )]
        unsafe fn run_core<const JIT: bool>(
            st: St,
            ctx: *const jit::JitCtx,
            mut ip: *const Inst,
        ) -> Out {
            // SAFETY: (the whole loop) `ip` stays inside the tape (every
            // instruction advances it by what `lower` validated: the next
            // op, the ops a fused instruction covers, or a jump target, and
            // the sentinel stops a run past the last op), and every slot
            // offset an instruction carries was validated by `lower` for
            // the slot vectors `st` points to.
            unsafe { loop {
                let inst = &*ip;
                match inst.op {
                    TOp::Exit => {
                        return Out { ip, at: ip, why: WHY_EXIT };
                    }
                    TOp::Step => return step_out(ip),
                    TOp::Ext => {
                        return Out { ip, at: ip, why: WHY_EXT };
                    }
                    TOp::CallInit | TOp::CallSend => {
                        // `new C` of a class without a constructor is no
                        // call: the object, then the jump over the
                        // constructor call's ops (with the jump's poll).
                        if inst.k != calls::CALL_NEW {
                            return Out { ip, at: ip, why: WHY_CALL };
                        }
                        match calls::new_plain(st.f, inst) {
                            calls::NEW_MADE => take_jump!(ip, inst, st, JIT, ctx),
                            // Made in the block of the object the assigned
                            // variable held: the assignment the jump goes
                            // to is done as well, so on past it, with the
                            // jump's poll.
                            calls::NEW_OVER => {
                                let at = ip;
                                #[allow(clippy::cast_possible_wrap)]
                                {
                                    ip = ip.byte_offset(inst.c as i32 as isize).add(1);
                                }
                                if crate::interrupt::safe_point_due() {
                                    return Out { ip, at, why: WHY_POLL };
                                }
                            }
                            _ => return Out { ip, at: ip, why: WHY_CALL },
                        }
                    }
                    TOp::VmOp => {
                        return Out { ip, at: ip, why: WHY_VM };
                    }
                    TOp::Jmp => take_jump!(ip, inst, st, JIT, ctx),
                    TOp::JmpZT => {
                        // The temporary is consumed ([`tmp_truth`]).
                        let Some(b) = tmp_truth(tmp_slot(st, inst.a)) else {
                            return step_out(ip);
                        };
                        if b {
                            ip = ip.add(1);
                        } else {
                            take_jump!(ip, inst, st, JIT, ctx);
                        }
                    }
                    TOp::JmpNzT => {
                        let Some(b) = tmp_truth(tmp_slot(st, inst.a)) else {
                            return step_out(ip);
                        };
                        if b {
                            take_jump!(ip, inst, st, JIT, ctx);
                        } else {
                            ip = ip.add(1);
                        }
                    }
                    TOp::IncC => {
                        match cv_slot(st, inst.a) {
                            Some(Value::Int(i)) => {
                                let Some(n) = i.checked_add(1) else {
                                    return step_out(ip);
                                };
                                *i = n;
                            }
                            Some(Value::Float(x)) => *x += 1.0,
                            _ => return step_out(ip),
                        }
                        ip = ip.add(1);
                    }
                    TOp::DecC => {
                        match cv_slot(st, inst.a) {
                            Some(Value::Int(i)) => {
                                let Some(n) = i.checked_sub(1) else {
                                    return step_out(ip);
                                };
                                *i = n;
                            }
                            Some(Value::Float(x)) => *x -= 1.0,
                            _ => return step_out(ip),
                        }
                        ip = ip.add(1);
                    }
                    TOp::PreIncT | TOp::PostIncT | TOp::PreDecT | TOp::PostDecT => {
                        let inc = matches!(inst.op, TOp::PreIncT | TOp::PostIncT);
                        let post = matches!(inst.op, TOp::PostIncT | TOp::PostDecT);
                        let d = tmp_slot(st, inst.c);
                        // An old value to release is the slow path's (it
                        // may queue a destructor, which wants its poll).
                        if !is_scalar(d) {
                            return step_out(ip);
                        }
                        match cv_slot(st, inst.a) {
                            Some(Value::Int(i)) => {
                                let old = *i;
                                let n = if inc { old.checked_add(1) } else { old.checked_sub(1) };
                                let Some(n) = n else {
                                    return step_out(ip);
                                };
                                *i = n;
                                std::ptr::write(d, Value::Int(if post { old } else { n }));
                            }
                            Some(Value::Float(x)) => {
                                let old = *x;
                                let n = if inc { old + 1.0 } else { old - 1.0 };
                                *x = n;
                                std::ptr::write(d, Value::Float(if post { old } else { n }));
                            }
                            _ => return step_out(ip),
                        }
                        ip = ip.add(1);
                    }
                    // The scalar assignments copy the 16 bytes of the value
                    // (a scalar owns nothing, so its bits are its copy);
                    // rebuilding it variant by variant made the compiler
                    // shuffle it through the stack in pieces.
                    TOp::AssignC => {
                        let s = st.cvs.add(inst.b as usize).cast::<Option<Value>>();
                        let d = st.cvs.add(inst.a as usize).cast::<Option<Value>>();
                        if !matches!(&*s, Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)))
                            || !matches!(&*d, None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)))
                        {
                            // A string, an array or an object on either
                            // side: copied and released out of line, then
                            // the poll every releasing op ends with.
                            if !scalar::assign_cv_any(st.f, inst, false) {
                                return step_out(ip);
                            }
                            let at = ip;
                            ip = ip.add(1);
                            if crate::interrupt::safe_point_due() {
                                return Out { ip, at, why: WHY_POLL };
                            }
                            continue;
                        }
                        // The destination is undefined or a scalar: nothing
                        // to release. `copy`, not `copy_nonoverlapping`:
                        // `$x = $x` names one slot.
                        std::ptr::copy(s, d, 1);
                        ip = ip.add(1);
                    }
                    TOp::AssignT => {
                        let s = st.tmps.add(inst.b as usize).cast::<Value>();
                        let d = st.cvs.add(inst.a as usize).cast::<Option<Value>>();
                        // Any value but a reference moves out of the
                        // temporary (a string, an array or an object as
                        // well: `hot::assign_cv_in` moves them); the
                        // variable's old value must own nothing.
                        if matches!(&*s, Value::Reference(_))
                            || !matches!(&*d, None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)))
                        {
                            // As in `AssignC`.
                            if !scalar::assign_cv_any(st.f, inst, true) {
                                return step_out(ip);
                            }
                            let at = ip;
                            ip = ip.add(1);
                            if crate::interrupt::safe_point_due() {
                                return Out { ip, at, why: WHY_POLL };
                            }
                            continue;
                        }
                        std::ptr::write(d, Some(std::ptr::read(s)));
                        // The temporary is consumed (`std::mem::take` in
                        // `hot::assign_cv_in`).
                        std::ptr::write(s, Value::Null);
                        ip = ip.add(1);
                    }
                    TOp::AssignI => {
                        let d = cv_slot(st, inst.a);
                        if !matches!(d, None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_))) {
                            return step_out(ip);
                        }
                        #[allow(clippy::cast_possible_wrap)]
                        std::ptr::write(d, Some(Value::Int(i64::from(inst.b as i32))));
                        ip = ip.add(1);
                    }
                    TOp::AssignL => {
                        // A scalar literal (`lower` emits this for nothing
                        // else).
                        let s = st.lits.add(inst.b as usize).cast::<Value>();
                        debug_assert!(is_scalar(&*s));
                        let d = cv_slot(st, inst.a);
                        if !matches!(d, None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_))) {
                            return step_out(ip);
                        }
                        std::ptr::write(d, Some(std::ptr::read(s)));
                        ip = ip.add(1);
                    }
                    $(
                        TOp::$cn => {
                            let jump = match num::<{ $cka }>(st, inst.a) {
                                Some(Num::Int(x)) => match num::<{ $ckb }>(st, inst.b) {
                                    Some(Num::Int(y)) => int_test::<$cm>(x, y),
                                    Some(y) => hot::jump_of(Num::Int(x), y, inst.k),
                                    None => return step_out(ip),
                                },
                                Some(x) => match num::<{ $ckb }>(st, inst.b) {
                                    Some(y) => hot::jump_of(x, y, inst.k),
                                    None => return step_out(ip),
                                },
                                None => return step_out(ip),
                            };
                            if jump {
                                take_jump!(ip, inst, st, JIT, ctx);
                            } else {
                                // Past the jump op the comparison decided.
                                ip = ip.add(2);
                            }
                        }
                    )*
                    $(
                        TOp::$in => {
                            // Every test before the store: a declined
                            // instruction has changed nothing.
                            let Some(Num::Int(y)) = num::<{ $ikb }>(st, inst.b) else {
                                return step_out(ip);
                            };
                            let Some(Value::Int(i)) = cv_slot(st, inst.a) else {
                                return step_out(ip);
                            };
                            let n = if $iinc { i.checked_add(1) } else { i.checked_sub(1) };
                            let Some(n) = n else {
                                return step_out(ip);
                            };
                            *i = n;
                            if int_test::<$im>(n, y) {
                                take_jump!(ip, inst, st, JIT, ctx);
                            } else {
                                // Past the smart branch and its jump op: the
                                // loop's exit, once per loop (the jump back
                                // is the common way; see `loop_exit`).
                                ip = loop_exit(ip);
                            }
                        }
                    )*
                    $(
                        TOp::$an => {
                            // The result is stored where it is computed
                            // (a tag and a payload store): merged into one
                            // `Value` first, it travelled in pieces. The
                            // destination is tested up front for that; it
                            // is addressed by raw pointer because it may be
                            // one of the operands.
                            let d = st.tmps.add(inst.c as usize).cast::<Value>();
                            if !is_scalar(&*d) {
                                return step_out(ip);
                            }
                            // Two ints and two floats each on a path of
                            // their own; an int with a float is widened
                            // (php-src's `(double)`). An int overflow is
                            // `num_bin`'s (a float).
                            #[allow(clippy::cast_precision_loss)]
                            match num::<{ $aka }>(st, inst.a) {
                                Some(Num::Int(x)) => match num::<{ $akb }>(st, inst.b) {
                                    Some(Num::Int(y)) => {
                                        let Some(r) = int_arith!($aop, x, y) else {
                                            return step_out(ip);
                                        };
                                        std::ptr::write(d, Value::Int(r));
                                    }
                                    Some(Num::Float(y)) => {
                                        std::ptr::write(d, Value::Float(float_arith!($aop, x as f64, y)));
                                    }
                                    None => return step_out(ip),
                                },
                                Some(Num::Float(x)) => match num::<{ $akb }>(st, inst.b) {
                                    Some(Num::Float(y)) => {
                                        std::ptr::write(d, Value::Float(float_arith!($aop, x, y)));
                                    }
                                    Some(Num::Int(y)) => {
                                        std::ptr::write(d, Value::Float(float_arith!($aop, x, y as f64)));
                                    }
                                    None => return step_out(ip),
                                },
                                None => return step_out(ip),
                            }
                            ip = ip.add(1);
                        }
                    )*
                    $(
                        TOp::$vn => {
                            // As for the temporary result above.
                            #[allow(clippy::cast_precision_loss)]
                            let n = match num::<{ $vka }>(st, inst.a) {
                                Some(Num::Int(x)) => match num::<{ $vkb }>(st, inst.b) {
                                    Some(Num::Int(y)) => {
                                        let Some(r) = int_arith!($vop, x, y) else {
                                            return step_out(ip);
                                        };
                                        Num::Int(r)
                                    }
                                    Some(Num::Float(y)) => Num::Float(float_arith!($vop, x as f64, y)),
                                    None => return step_out(ip),
                                },
                                Some(Num::Float(x)) => match num::<{ $vkb }>(st, inst.b) {
                                    Some(Num::Float(y)) => Num::Float(float_arith!($vop, x, y)),
                                    Some(Num::Int(y)) => Num::Float(float_arith!($vop, x, y as f64)),
                                    None => return step_out(ip),
                                },
                                None => return step_out(ip),
                            };
                            // Into the variable when it holds no array,
                            // object or reference (the `AssignCv` after the
                            // op is then done); else into the temporary,
                            // for that `AssignCv`.
                            let d = cv_slot(st, inst.c);
                            match d {
                                None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)) => {
                                    std::ptr::write(d, Some(match n {
                                        Num::Int(i) => Value::Int(i),
                                        Num::Float(x) => Value::Float(x),
                                    }));
                                    ip = ip.add(2);
                                }
                                _ => return step_out(ip),
                            }
                        }
                    )*
                    $(
                        TOp::$on => {
                            let Some(y) = num::<{ $okb }>(st, inst.b) else {
                                return step_out(ip);
                            };
                            let slot = cv_slot(st, inst.a);
                            match (&mut *slot, y) {
                                (Some(Value::Int(i)), Num::Int(y)) => {
                                    // An overflow is `num_bin`'s (a float).
                                    let Some(r) = int_arith!($oop, *i, y) else {
                                        return step_out(ip);
                                    };
                                    *i = r;
                                }
                                (Some(Value::Float(x)), Num::Float(y)) => {
                                    *x = float_arith!($oop, *x, y);
                                }
                                (Some(Value::Float(x)), Num::Int(y)) => {
                                    #[allow(clippy::cast_precision_loss)] // php-src's (double) widening
                                    let y = y as f64;
                                    *x = float_arith!($oop, *x, y);
                                }
                                // An int variable and a float: the variable
                                // becomes a float (an int: nothing to
                                // release).
                                (Some(Value::Int(i)), Num::Float(y)) => {
                                    #[allow(clippy::cast_precision_loss)] // php-src's (double) widening
                                    let r = float_arith!($oop, *i as f64, y);
                                    std::ptr::write(slot, Some(Value::Float(r)));
                                }
                                _ => return step_out(ip),
                            }
                            ip = ip.add(1);
                        }
                    )*
                    $(
                        TOp::$fname => fam_arm!($fmode, $fh, $fskip, st, inst, ip, ctx),
                    )*
                }
            } }
        }
    };
}

tape_ops! {
    cmp {
        CmpLtCC = (KC, KC, 1), CmpLeCC = (KC, KC, 3), CmpGtCC = (KC, KC, 4), CmpGeCC = (KC, KC, 6), CmpEqCC = (KC, KC, 2), CmpNeCC = (KC, KC, 5),
        CmpLtCT = (KC, KT, 1), CmpLeCT = (KC, KT, 3), CmpGtCT = (KC, KT, 4), CmpGeCT = (KC, KT, 6), CmpEqCT = (KC, KT, 2), CmpNeCT = (KC, KT, 5),
        CmpLtCI = (KC, KI, 1), CmpLeCI = (KC, KI, 3), CmpGtCI = (KC, KI, 4), CmpGeCI = (KC, KI, 6), CmpEqCI = (KC, KI, 2), CmpNeCI = (KC, KI, 5),
        CmpLtCL = (KC, KL, 1), CmpLeCL = (KC, KL, 3), CmpGtCL = (KC, KL, 4), CmpGeCL = (KC, KL, 6), CmpEqCL = (KC, KL, 2), CmpNeCL = (KC, KL, 5),
        CmpLtTC = (KT, KC, 1), CmpLeTC = (KT, KC, 3), CmpGtTC = (KT, KC, 4), CmpGeTC = (KT, KC, 6), CmpEqTC = (KT, KC, 2), CmpNeTC = (KT, KC, 5),
        CmpLtTT = (KT, KT, 1), CmpLeTT = (KT, KT, 3), CmpGtTT = (KT, KT, 4), CmpGeTT = (KT, KT, 6), CmpEqTT = (KT, KT, 2), CmpNeTT = (KT, KT, 5),
        CmpLtTI = (KT, KI, 1), CmpLeTI = (KT, KI, 3), CmpGtTI = (KT, KI, 4), CmpGeTI = (KT, KI, 6), CmpEqTI = (KT, KI, 2), CmpNeTI = (KT, KI, 5),
        CmpLtTL = (KT, KL, 1), CmpLeTL = (KT, KL, 3), CmpGtTL = (KT, KL, 4), CmpGeTL = (KT, KL, 6), CmpEqTL = (KT, KL, 2), CmpNeTL = (KT, KL, 5),
    }
    inccmp {
        IncLtC = (true, KC, 1), IncLeC = (true, KC, 3), IncGtC = (true, KC, 4), IncGeC = (true, KC, 6), IncEqC = (true, KC, 2), IncNeC = (true, KC, 5),
        IncLtI = (true, KI, 1), IncLeI = (true, KI, 3), IncGtI = (true, KI, 4), IncGeI = (true, KI, 6), IncEqI = (true, KI, 2), IncNeI = (true, KI, 5),
        DecLtC = (false, KC, 1), DecLeC = (false, KC, 3), DecGtC = (false, KC, 4), DecGeC = (false, KC, 6), DecEqC = (false, KC, 2), DecNeC = (false, KC, 5),
        DecLtI = (false, KI, 1), DecLeI = (false, KI, 3), DecGtI = (false, KI, 4), DecGeI = (false, KI, 6), DecEqI = (false, KI, 2), DecNeI = (false, KI, 5),
    }
    arith_tmp {
        AddCC = (Add, KC, KC), AddCT = (Add, KC, KT), AddCI = (Add, KC, KI), AddCL = (Add, KC, KL), AddTC = (Add, KT, KC), AddTT = (Add, KT, KT), AddTI = (Add, KT, KI), AddTL = (Add, KT, KL), AddIC = (Add, KI, KC), AddIT = (Add, KI, KT), AddLC = (Add, KL, KC), AddLT = (Add, KL, KT),
        SubCC = (Sub, KC, KC), SubCT = (Sub, KC, KT), SubCI = (Sub, KC, KI), SubCL = (Sub, KC, KL), SubTC = (Sub, KT, KC), SubTT = (Sub, KT, KT), SubTI = (Sub, KT, KI), SubTL = (Sub, KT, KL), SubIC = (Sub, KI, KC), SubIT = (Sub, KI, KT), SubLC = (Sub, KL, KC), SubLT = (Sub, KL, KT),
        MulCC = (Mul, KC, KC), MulCT = (Mul, KC, KT), MulCI = (Mul, KC, KI), MulCL = (Mul, KC, KL), MulTC = (Mul, KT, KC), MulTT = (Mul, KT, KT), MulTI = (Mul, KT, KI), MulTL = (Mul, KT, KL), MulIC = (Mul, KI, KC), MulIT = (Mul, KI, KT), MulLC = (Mul, KL, KC), MulLT = (Mul, KL, KT),
    }
    arith_cv {
        AddCCV = (Add, KC, KC), AddCTV = (Add, KC, KT), AddCIV = (Add, KC, KI), AddCLV = (Add, KC, KL), AddTCV = (Add, KT, KC), AddTTV = (Add, KT, KT), AddTIV = (Add, KT, KI), AddTLV = (Add, KT, KL), AddICV = (Add, KI, KC), AddITV = (Add, KI, KT), AddLCV = (Add, KL, KC), AddLTV = (Add, KL, KT),
        SubCCV = (Sub, KC, KC), SubCTV = (Sub, KC, KT), SubCIV = (Sub, KC, KI), SubCLV = (Sub, KC, KL), SubTCV = (Sub, KT, KC), SubTTV = (Sub, KT, KT), SubTIV = (Sub, KT, KI), SubTLV = (Sub, KT, KL), SubICV = (Sub, KI, KC), SubITV = (Sub, KI, KT), SubLCV = (Sub, KL, KC), SubLTV = (Sub, KL, KT),
        MulCCV = (Mul, KC, KC), MulCTV = (Mul, KC, KT), MulCIV = (Mul, KC, KI), MulCLV = (Mul, KC, KL), MulTCV = (Mul, KT, KC), MulTTV = (Mul, KT, KT), MulTIV = (Mul, KT, KI), MulTLV = (Mul, KT, KL), MulICV = (Mul, KI, KC), MulITV = (Mul, KI, KT), MulLCV = (Mul, KL, KC), MulLTV = (Mul, KL, KT),
    }
    assign_op {
        AddEqC = (Add, KC), AddEqT = (Add, KT), AddEqI = (Add, KI), AddEqL = (Add, KL),
        SubEqC = (Sub, KC), SubEqT = (Sub, KT), SubEqI = (Sub, KI), SubEqL = (Sub, KL),
        MulEqC = (Mul, KC), MulEqT = (Mul, KT), MulEqI = (Mul, KI), MulEqL = (Mul, KL),
    }
    // One row per instruction: `Opcode = (handler, ops covered, ending),`
    // where the ending is `plain`, `poll`, `branch` or `branch_poll`
    // (see `fam_arm!`). Each family adds its rows under its own heading.
    family {
        // ---- scalar (exec/tape/scalar.rs) ----
        Nop = (scalar::nop, 1, plain),
        JmpZC = (scalar::jmpz_cv, 1, branch),
        JmpNzC = (scalar::jmpnz_cv, 1, branch),
        NotJmpZC = (scalar::not_jmpz_cv, 2, branch),
        NotJmpNzC = (scalar::not_jmpnz_cv, 2, branch),
        UnaryT = (scalar::unary_bool_tmp, 1, plain),
        UnaryC = (scalar::unary_bool_cv, 1, plain),
        JmpZExC = (scalar::jmpz_ex_cv, 1, branch),
        JmpNzExC = (scalar::jmpnz_ex_cv, 1, branch),
        JmpZExT = (scalar::jmpz_ex_tmp, 1, branch),
        JmpNzExT = (scalar::jmpnz_ex_tmp, 1, branch),
        NumBinT = (scalar::num_bin_tmp, 1, plain_f),
        NumBinC = (scalar::num_bin_cv, 2, plain_f),
        NumAssignOpC = (scalar::num_assign_op_cv, 1, plain_f),
        AbsN = (scalar::abs_num, 2, plain_f),
        MoveT = (scalar::move_tmp, 1, plain_f),
        AssignR = (scalar::assign_res, 1, plain_f),
        JmpSetC = (scalar::jmp_set_cv, 1, branch_f),
        CoalesceC = (scalar::coalesce_cv, 1, branch_f),
        CoalesceT = (scalar::coalesce_tmp, 1, branch_f),
        TypeCheckX = (scalar::type_check, 1, poll_f),
        CountX = (scalar::count, 1, poll_f),
        InstanceOfX = (scalar::instance_of, 1, poll_f),
        FeResetA = (scalar::fe_reset, 1, branch_poll_f),
        FeFreeA = (scalar::fe_free, 1, poll_f),
        CastX = (scalar::cast, 1, poll_f),
        AssignLitC = (scalar::assign_lit_cv, 1, poll_f),
        EqLitJmpC = (scalar::eq_lit_jmp::<0>, 2, branch_f),
        EqLitJmpT = (scalar::eq_lit_jmp::<1>, 2, branch_f),
        EqLitJmpK = (scalar::eq_lit_jmp::<2>, 2, branch_f),
        EqLitTmpC = (scalar::eq_lit_tmp::<0>, 1, plain_f),
        EqLitTmpT = (scalar::eq_lit_tmp::<1>, 1, plain_f),
        EqLitTmpK = (scalar::eq_lit_tmp::<2>, 1, plain_f),
        EqLitCvC = (scalar::eq_lit_cv::<0>, 2, plain_f),
        EqLitCvT = (scalar::eq_lit_cv::<1>, 2, plain_f),
        EqLitCvK = (scalar::eq_lit_cv::<2>, 2, plain_f),
        JmpSetT = (scalar::jmp_set_tmp, 1, branch_poll_f),
        FreeT = (scalar::free_tmp, 1, plain_f),
        MatchJ = (scalar::match_jump, 1, switch_f),

        // ---- end of scalar ----
        //
        //
        //
        // ---- dims (exec/tape/dims.rs) ----
        FetchDimC = (dims::fetch_dim_key, 1, branch_poll_f),
        FetchDimIC = (dims::fetch_dim_int, 1, branch_poll_f),
        FetchDimIT = (dims::fetch_dim_tmp, 1, branch_poll_f),
        FetchDimSC = (dims::fetch_dim_str, 1, branch_poll_f),
        FetchDimsC = (dims::fetch_dims, 1, branch_poll_f),
        IssetDimC = (dims::isset_dim, 1, poll_f),
        IssetDimSC = (dims::isset_dim_str, 1, poll_f),
        KeyExistsC = (dims::key_exists, 1, poll_f),
        AssignDimC = (dims::assign_dim_key, 1, poll_f),
        AssignDimIC = (dims::assign_dim_int, 1, poll_f),
        AssignDimsC = (dims::assign_dims, 1, poll_f),
        AssignOpDimC = (dims::assign_op_dim, 1, poll_f),
        AssignOpDimsC = (dims::assign_op_dims, 1, poll_f),
        IncDecDimC = (dims::incdec_dim, 1, poll_f),
        ConcatCv = (dims::concat_cv, 1, poll_f),
        ConcatT = (dims::concat_tmp, 1, poll_f),
        RopeT = (dims::rope_tmp, 1, poll_f),
        CastStrT = (dims::cast_str_tmp, 1, poll_f),
        InitArrayT = (dims::init_array, 1, poll_f),
        ArrayAddT = (dims::array_add, 1, poll_f),
        FeFetchA = (dims::fe_fetch, 1, branch_poll_f),
        FeFetchV = (dims::fe_fetch_v, 2, branch_poll_f),
        FeFetchKV = (dims::fe_fetch_kv, 3, branch_poll_f),

        // ---- end of dims ----
        //
        //
        //
        // ---- props (exec/tape/props.rs) ----
        FetchObjProp = (props::fetch_prop, 1, poll_f),
        FetchObjPropTo = (props::fetch_prop_to, 1, branch_poll_f),
        AssignObjProp = (props::prop_in, 1, poll_f),
        AssignOpObjProp = (props::prop_in, 1, poll_f),
        IncDecObjProp = (props::prop_in, 1, poll_f),
        IssetObjProp = (props::prop_in, 1, poll_f),
        IncDecStatic = (props::incdec_static, 1, poll_f),
        ThisConst = (props::this_const, 2, poll_f),
        ThisConstTo = (props::this_const_to, 2, branch_poll_f),

        // ---- end of props ----
        //
        //
        //
        // ---- statics (this file: `lower_statics`) ----
        ClassConst = (statics_class_const, 1, poll_f),
        ClassConstTo = (statics_class_const_to, 1, branch_poll_f),
        StaticProp = (statics_static_prop, 1, poll_f),
        StaticPropTo = (statics_static_prop_to, 1, branch_poll_f),

        // ---- end of statics ----
        //
        //
        //
        // ---- strs (exec/tape/strs.rs) ----
        StrlenC = (strs::strlen_cv, 1, plain_f),
        CountC = (strs::count_cv, 1, plain_f),
        TypeCheckC = (strs::type_check_cv, 1, plain_f),
        TypeCheckJmpC = (strs::type_check_jmp_cv, 2, branch_f),
        IssetC = (strs::isset_cv, 1, plain_f),
        IssetJmpC = (strs::isset_jmp_cv, 2, branch_f),
        FramelessQ = (strs::frameless_quick, 1, branch_poll_f),

        // ---- end of strs ----
        //
        //
        //
        // ---- calls (exec/tape/calls.rs) ----


        // ---- end of calls ----
    }
}

/// [`hot::step`] out of line: the core loop must not carry its code. An
/// op its lean path declined because a variable holds a reference goes on
/// to [`super::spec::step_ref`] before the generic loop.
#[inline(never)]
fn step_slow(f: &mut Frame, ops: &[Op], lits: &[Value], at: usize) -> usize {
    let next = hot::step(f, ops, lits, at);
    if next != DECLINED {
        return next;
    }
    super::spec::step_ref(f, ops, lits, at)
}

/// The kind and operand field of a numeric operand; `None` when it has no
/// specialised form (not a number literal, or out of range).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn src(code: &OpArray, o: Operand) -> Option<(u8, u32)> {
    match o {
        Operand::Cv(i) => Some((KC, cv_off(code, i)?)),
        Operand::Tmp(t) | Operand::TmpKeep(t) => Some((KT, tmp_off(code, t)?)),
        Operand::Lit(l) => match code.literals.get(l as usize)? {
            Value::Int(v) => match i32::try_from(*v) {
                Ok(small) => Some((KI, small as u32)),
                Err(_) => Some((KL, lit_off(l)?)),
            },
            Value::Float(_) => Some((KL, lit_off(l)?)),
            _ => None,
        },
        Operand::Unused => None,
    }
}

/// The byte offset of a compiled variable; `None` when out of range.
fn cv_off(code: &OpArray, cv: u32) -> Option<u32> {
    ((cv as usize) < code.cv_names.len() && cv < SLOT_LIMIT).then_some(cv << SLOT_SHIFT)
}

/// The byte offset of a temporary; `None` when out of range.
pub(crate) fn tmp_off(code: &OpArray, t: u32) -> Option<u32> {
    (t < code.num_tmps && t < SLOT_LIMIT).then_some(t << SLOT_SHIFT)
}

/// The byte offset of a literal the caller found in the literal table.
fn lit_off(l: u32) -> Option<u32> {
    (l < SLOT_LIMIT).then_some(l << SLOT_SHIFT)
}

/// The jump from instruction `from` to op index `to`, as a byte offset;
/// `None` when the target is outside the tape.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
fn rel(code: &OpArray, from: usize, to: u32) -> Option<u32> {
    if to as usize > code.ops.len() {
        return None;
    }
    let delta = (i64::from(to) - from as i64) * std::mem::size_of::<Inst>() as i64;
    i32::try_from(delta).ok().map(|d| d as u32)
}

/// The mask of a smart branch with its operands exchanged: `LESS` and
/// `GREATER` trade places; equal, unordered and the identity bits are
/// symmetric ([`crate::opcode::cmp_outcome`]).
const fn mirrored(mask: u8) -> u8 {
    use crate::opcode::cmp_outcome::{GREATER, LESS};
    let mut m = mask & !(LESS | GREATER);
    if mask & LESS != 0 {
        m |= GREATER;
    }
    if mask & GREATER != 0 {
        m |= LESS;
    }
    m
}

/// Whether the instructions of family `name` are switched off for this
/// process: the environment variable `PHPRUST_TAPE_OFF` lists families
/// (`numeric`, `scalar`, `dims`, `props`, `strs`, `calls`, or `all`),
/// separated by commas. A debugging aid, read once: it tells which family a
/// wrong result comes from without a rebuild (every op of a family that is
/// off runs through `hot::step`, as it did before the family existed).
fn switched_off(name: &str) -> bool {
    static OFF: OnceLock<Vec<String>> = OnceLock::new();
    OFF.get_or_init(|| {
        std::env::var("PHPRUST_TAPE_OFF")
            .map(|list| list.split(',').map(|n| n.trim().to_owned()).collect())
            .unwrap_or_default()
    })
    .iter()
    .any(|off| off == name || off == "all")
}

/// The specialised instruction of the op at `i`, if it has one: a numeric
/// one of this module, else one of a family's.
fn lower_op(code: &OpArray, i: usize) -> Option<Inst> {
    let family = |name: &str, lower: fn(&OpArray, usize) -> Option<Inst>| {
        if switched_off(name) {
            None
        } else {
            lower(code, i)
        }
    };
    family("numeric", lower_numeric)
        .or_else(|| family("scalar", scalar::lower))
        .or_else(|| family("dims", dims::lower))
        .or_else(|| family("props", props::lower))
        .or_else(|| family("statics", lower_statics))
        .or_else(|| family("strs", strs::lower))
        .or_else(|| family("calls", calls::lower))
        .or_else(|| family("places", lower_place))
        .or_else(|| family("calls", calls::lower_entry))
}

/// Whether `p` is a place one of the frame-local nested-access paths can
/// serve (`Vm::fetch_prop_dims_fast`, `Vm::fetch_global_dims_fast`,
/// `Vm::isset_prop_dims_fast`, `Vm::assign_prop_dims_fast`,
/// `Vm::unset_dims_fast`): `$this->p[$k]...` / `$cv->p[$k]...` (`dims`),
/// a chain ending in a property (`$this->a->b`, `$cv[$k]->b`), or
/// `$GLOBALS['name'][$k]...`; `cv_dims` admits `$cv[$k]...` too. Which
/// ones hit is decided when they run.
fn nested_place(p: &crate::opcode::Place, cv_dims: bool, chains: bool) -> bool {
    use crate::opcode::{PlaceBase, Seg};
    let segs = p.segs.as_slice();
    let prop_dims = matches!(p.base, PlaceBase::This | PlaceBase::Cv(_))
        && matches!(segs, [Seg::Prop(Operand::Lit(_)), Seg::Dim(_), ..]);
    let chain = chains
        && matches!(segs.last(), Some(Seg::Prop(Operand::Lit(_))))
        && segs.len() >= 2
        && matches!(p.base, PlaceBase::This | PlaceBase::Cv(_))
        && matches!(segs.first(), Some(Seg::Prop(Operand::Lit(_)) | Seg::Dim(_)));
    let globals = matches!(
        (&p.base, segs),
        (PlaceBase::Global(Operand::Lit(_)), [Seg::Dim(_), ..])
            | (
                PlaceBase::GlobalsArray,
                [Seg::Dim(Operand::Lit(_)), Seg::Dim(_), ..]
            )
    );
    let cv = cv_dims && matches!((&p.base, segs), (PlaceBase::Cv(_), [Seg::Dim(_), ..]));
    prop_dims || chain || globals || cv
}

/// An element of a temporary, a literal or a variable read as a value,
/// `tmp[d1]...[dn]` (`f()['k']`, `self::MAP[$k]`, the elements
/// `[$a, $b] = $x` reads): `spec::fetch_tmp_dims_in`.
fn tmp_dims_place(p: &crate::opcode::Place) -> bool {
    use crate::opcode::{PlaceBase, Seg};
    matches!(
        p.base,
        PlaceBase::Value(Operand::Tmp(_) | Operand::TmpKeep(_) | Operand::Lit(_) | Operand::Cv(_))
    ) && !p.segs.is_empty()
        && p.segs.iter().all(|s| matches!(s, Seg::Dim(_)))
}

/// An element of a static property, `A::$name[d1]...[dn]` (a literal
/// name, a class that is not dynamic): `spec::fetch_static_dims_in`,
/// `spec::isset_static_dims_in`, `spec::assign_static_dims_in`.
fn static_dims_place(p: &crate::opcode::Place) -> bool {
    use crate::opcode::{PlaceBase, Seg};
    matches!(
        p.base,
        PlaceBase::StaticProp {
            class: ClassSpec::Named(_)
                | ClassSpec::SelfClass
                | ClassSpec::Parent
                | ClassSpec::Static,
            name: Operand::Lit(_),
        }
    ) && !p.segs.is_empty()
        && p.segs.iter().all(|s| matches!(s, Seg::Dim(_)))
}

/// `$this->$name` / `$cv->$name`: a property named by a variable
/// (`Vm::fetch_dyn_prop_fast`, `Vm::assign_dyn_prop_fast`).
fn dyn_prop_place(p: &crate::opcode::Place) -> bool {
    use crate::opcode::{PlaceBase, Seg};
    matches!(p.base, PlaceBase::This | PlaceBase::Cv(_))
        && matches!(p.segs.as_slice(), [Seg::Prop(Operand::Cv(_))])
}

/// The generic place ops whose nested shapes have a fast path that needs
/// the VM ([`nested_place`]): run with it at hand ([`run_vm_op`], which
/// declines to `Vm::run`'s generic op on a miss) instead of leaving the
/// tape for `Vm::run`. A read in `Read` or `Is` mode, `isset()` /
/// `empty()`, `unset()` and an assignment.
fn lower_place(code: &OpArray, i: usize) -> Option<Inst> {
    let place_of = |pid: u32| code.places.get(pid as usize);
    let in_range = |o: Operand| match o {
        Operand::Tmp(t) | Operand::TmpKeep(t) => t < code.num_tmps,
        Operand::Cv(c) => (c as usize) < code.cv_names.len(),
        Operand::Lit(l) => (l as usize) < code.literals.len(),
        Operand::Unused => false,
    };
    let ok = match *code.ops.get(i)? {
        Op::Fetch { place, mode, dst } => {
            let p = place_of(place)?;
            dst < code.num_tmps
                && ((mode != FetchMode::List && nested_place(p, false, true))
                    || (matches!(mode, FetchMode::Read | FetchMode::Is | FetchMode::List)
                        && tmp_dims_place(p))
                    || (mode != FetchMode::List && static_dims_place(p))
                    || (matches!(mode, FetchMode::Read | FetchMode::Is) && dyn_prop_place(p)))
        }
        Op::Isset { place, dst } | Op::Empty { place, dst } => {
            let p = place_of(place)?;
            dst < code.num_tmps && (nested_place(p, false, true) || static_dims_place(p))
        }
        Op::Unset { place } => nested_place(place_of(place)?, true, false),
        // `defined('NAME')`: the op's run-time cache hit (`run_vm_op`).
        Op::Defined { name, dst } => {
            (name as usize) < code.literals.len() && dst.is_none_or(|d| d < code.num_tmps)
        }
        Op::Assign { place, val, dst } => {
            let p = place_of(place)?;
            in_range(val)
                && dst.is_none_or(|d| d < code.num_tmps)
                && (nested_place(p, true, true) || static_dims_place(p) || dyn_prop_place(p))
        }
        _ => false,
    };
    ok.then(|| Inst::plain(TOp::VmOp))
}

/// The numeric instruction ([`tape_ops!`]'s generated families) of the op
/// at `i`, if it has one.
#[allow(clippy::too_many_lines, clippy::many_single_char_names)]
fn lower_numeric(code: &OpArray, i: usize) -> Option<Inst> {
    let plain = Inst::plain;
    Some(match *code.ops.get(i)? {
        Op::Jmp(t) => Inst {
            c: rel(code, i, t)?,
            ..plain(TOp::Jmp)
        },
        Op::JmpZ(Operand::Tmp(t), target) => Inst {
            a: tmp_off(code, t)?,
            c: rel(code, i, target)?,
            ..plain(TOp::JmpZT)
        },
        Op::JmpNz(Operand::Tmp(t), target) => Inst {
            a: tmp_off(code, t)?,
            c: rel(code, i, target)?,
            ..plain(TOp::JmpNzT)
        },
        Op::CmpJmp {
            a, b, target, mask, ..
        } => {
            let (ka, fa) = src(code, a)?;
            let (kb, fb) = src(code, b)?;
            // A literal on the left (`$x > 5` is compiled as `5 < $x`, as
            // php-src does): the mirrored comparison of the slot with it.
            let (ka, fa, kb, fb, mask) = if matches!(ka, KI | KL) {
                (kb, fb, ka, fa, mirrored(mask))
            } else {
                (ka, fa, kb, fb, mask)
            };
            Inst {
                k: mask,
                a: fa,
                b: fb,
                c: rel(code, i, target)?,
                ..plain(TOp::cmp(ka, kb, mask & 7)?)
            }
        }
        Op::IncDecCvCmpJmp { cv, kind, .. } => {
            let Some(&Op::CmpJmp {
                a, b, target, mask, ..
            }) = code.ops.get(i + 1)
            else {
                return None;
            };
            // The variable against something else, which the fused form
            // reads before the store. `$i > 0` and `$n > $i` have the
            // variable on the right (compiled as `0 < $i`, `$i < $n`
            // mirrored): the mirrored comparison.
            let known = Operand::Cv(cv);
            let (other, mask) = match (a == known, b == known) {
                (true, false) => (b, mask),
                (false, true) => (a, mirrored(mask)),
                _ => return None,
            };
            let (kb, fb) = src(code, other)?;
            Inst {
                k: mask,
                a: cv_off(code, cv)?,
                b: fb,
                c: rel(code, i, target)?,
                ..plain(TOp::inccmp(kind.is_inc(), kb, mask & 7)?)
            }
        }
        Op::IncDecCv {
            cv,
            kind,
            dst: None,
            ..
        } => Inst {
            a: cv_off(code, cv)?,
            ..plain(if kind.is_inc() { TOp::IncC } else { TOp::DecC })
        },
        Op::IncDecCv {
            cv,
            kind,
            dst: Some(t),
            ..
        } => Inst {
            a: cv_off(code, cv)?,
            c: tmp_off(code, t)?,
            ..plain(match (kind.is_inc(), kind.is_post()) {
                (true, false) => TOp::PreIncT,
                (true, true) => TOp::PostIncT,
                (false, false) => TOp::PreDecT,
                (false, true) => TOp::PostDecT,
            })
        },
        Op::Bin {
            op: BinKind::Arith(op),
            a,
            b,
            dst,
        } => {
            let (ka, fa) = src(code, a)?;
            let (kb, fb) = src(code, b)?;
            Inst {
                a: fa,
                b: fb,
                c: tmp_off(code, dst)?,
                ..plain(TOp::arith_tmp(op, ka, kb)?)
            }
        }
        // Also the form the compiler fused with the `CmpJmp` after it (a
        // rotated loop's step and condition, `for (...; $i < $n; $i = $i +
        // 1)`): on the tape the step is this instruction, which covers the
        // kept `AssignCv`, and the comparison runs as its own instruction
        // right after. The `CmpJmp` stays in the op array (the loop is
        // entered at it), so the two are exactly what the fused op stands
        // for; left unlowered, the step left the core on every iteration.
        Op::BinAssignCv {
            op: BinKind::Arith(op),
            a,
            b,
            cv,
            ..
        }
        | Op::BinAssignCvCmpJmp {
            op: BinKind::Arith(op),
            a,
            b,
            cv,
            ..
        } => {
            let (ka, fa) = src(code, a)?;
            let (kb, fb) = src(code, b)?;
            Inst {
                a: fa,
                b: fb,
                c: cv_off(code, cv)?,
                ..plain(TOp::arith_cv(op, ka, kb)?)
            }
        }
        // As above for `$cv op= val` (`for (...; $k <= $n; $k += $i)`).
        Op::AssignOpCv {
            cv,
            op,
            val,
            dst: None,
            ..
        }
        | Op::AssignOpCvCmpJmp { cv, op, val, .. } => {
            let (kb, fb) = src(code, val)?;
            Inst {
                a: cv_off(code, cv)?,
                b: fb,
                ..plain(TOp::assign_op(op, kb)?)
            }
        }
        Op::AssignCv { cv, val, dst: None } => {
            let a = cv_off(code, cv)?;
            match val {
                Operand::Cv(s) => Inst {
                    a,
                    b: cv_off(code, s)?,
                    ..plain(TOp::AssignC)
                },
                Operand::Tmp(t) => Inst {
                    a,
                    b: tmp_off(code, t)?,
                    ..plain(TOp::AssignT)
                },
                Operand::Lit(l) => match code.literals.get(l as usize)? {
                    Value::Int(v) => match i32::try_from(*v) {
                        #[allow(clippy::cast_sign_loss)]
                        Ok(small) => Inst {
                            a,
                            b: small as u32,
                            ..plain(TOp::AssignI)
                        },
                        Err(_) => Inst {
                            a,
                            b: lit_off(l)?,
                            ..plain(TOp::AssignL)
                        },
                    },
                    Value::Null | Value::Bool(_) | Value::Float(_) => Inst {
                        a,
                        b: lit_off(l)?,
                        ..plain(TOp::AssignL)
                    },
                    _ => return None,
                },
                _ => return None,
            }
        }
        _ => return None,
    })
}

/// Decodes `code` into its tape: one [`Inst`] per op, at the op's index,
/// and an [`TOp::Exit`] sentinel after the last one.
pub(crate) fn lower(code: &OpArray) -> Box<[Inst]> {
    let mut tape = Vec::with_capacity(code.ops.len() + 1);
    for (i, op) in code.ops.iter().enumerate() {
        tape.push(lower_op(code, i).unwrap_or_else(|| {
            Inst::plain(if !hot::is_hot(op) {
                TOp::Exit
            } else if hot::is_core(op) {
                TOp::Step
            } else {
                TOp::Ext
            })
        }));
    }
    tape.push(Inst::plain(TOp::Exit));
    calls::number_new_sites(&mut tape);
    // Entering the tape costs a call, a look at the frame and the way back
    // (about what one unspecialised hot op costs): worth it at a
    // specialised instruction when the tape also runs what follows it.
    for i in 0..code.ops.len() {
        let follows = likely_next(code, i);
        tape[i].ext = !hot::is_core(&code.ops[i]);
        tape[i].enter = !matches!(tape[i].op, TOp::Exit | TOp::Step | TOp::Ext)
            && tape.get(follows).is_some_and(|next| next.op != TOp::Exit);
    }
    tape.into_boxed_slice()
}

/// The op that most likely runs after the op at `i`: the target of a
/// backward jump (a loop goes round), the fall-through of a forward one,
/// the op after the ops a fused op covers.
fn likely_next(code: &OpArray, i: usize) -> usize {
    let after = |target: u32, fall: usize| {
        if target as usize <= i {
            target as usize
        } else {
            fall
        }
    };
    match code.ops.get(i) {
        Some(&Op::Jmp(t)) => t as usize,
        Some(&(Op::JmpZ(_, t) | Op::JmpNz(_, t))) => after(t, i + 1),
        Some(&Op::CmpJmp { target, .. }) => after(target, i + 2),
        Some(Op::IncDecCvCmpJmp { .. } | Op::AssignOpCvCmpJmp { .. }) => {
            match code.ops.get(i + 1) {
                Some(&Op::CmpJmp { target, .. }) => after(target, i + 3),
                _ => i + 1,
            }
        }
        Some(Op::BinAssignCvCmpJmp { .. }) => match code.ops.get(i + 2) {
            Some(&Op::CmpJmp { target, .. }) => after(target, i + 4),
            _ => i + 1,
        },
        Some(Op::BinAssignCv { .. }) => i + 2,
        _ => i + 1,
    }
}

/// Whether `Vm::run` enters the tape at the op at `at` of `code` at once
/// (the tape is decoded on first use): it has a specialised instruction
/// and so has the op that most likely follows it ([`lower`] works that
/// out). Any other hot op `Vm::run` runs itself first (`hot::step`), and
/// it enters the tape when the op after that one is a hot op too.
#[inline]
pub(super) fn worth_entering(code: &OpArray, at: usize) -> bool {
    code.tape.get(code).get(at).is_some_and(|inst| inst.enter)
}

/// Whether the tape of `code` may run on frame `f`: its slots are where
/// the tape's offsets point (see the module's safety notes), and no symbol
/// table mirrors its compiled variables (a write would have to be copied
/// there: `hot::step` does that).
#[inline(always)]
#[allow(clippy::inline_always)] // three tests on the way into the loop
fn usable(f: &Frame, code: &OpArray) -> bool {
    !f.symtab_attached
        && f.cvs.len() >= code.cv_names.len()
        && f.tmps.len() >= code.num_tmps as usize
}

/// Runs the hot ops of the current frame (the one `code` belongs to) from
/// `ip`, and the plain user calls and returns among them
/// (`ZEND_DO_UCALL`, `ZEND_RETURN`: [`Vm::push_simple_ucall`],
/// [`Vm::return_simple`]), going on in the callee's or the caller's frame
/// in place of returning to `Vm::run` to reload it, and the plain builtin
/// calls by name (`ZEND_DO_ICALL`: `calls::call_native`). Specialised
/// instructions run in [`run_core`]; a hot op without one, or whose
/// instruction declined, runs through [`hot::step`] or
/// [`hot::step_ext`]; a frame the tape cannot run on ([`usable`]) has its
/// hot ops run by [`hot::run_steps`]. Returns:
///
/// * `(ip, NO_SAFE_POINT)` when the op at `ip` must run in `Vm::run` (not
///   a hot op, or its fast path declined, having changed nothing; a
///   `DoCall` or `Return` here was declined by its fast path);
/// * `(ip, at)` when the safe point after the op at `at` is due, `ip`
///   being where execution continues;
/// * either with [`SWITCHED`] set in `ip` when the current frame changed
///   since the call (`ip` and `at` are then the new frame's);
/// * `(_, LEFT)` when a call or return changed the frame and the safe
///   point after it is due ([`LEFT`]);
/// * `(_, THREW)` when a builtin it called (`calls::call_native`) threw or
///   exited ([`hot::THREW`]): the throw is in `Vm::tape_throw`, the
///   current frame's resume point stored as for `Vm::run`'s generic
///   `DoCall` (`SAVE_OPLINE`), so the exception unwinds from the call.
///
/// `base` is `Vm::run`'s: the frame at that index is never returned from
/// here (`Vm::return_simple` declines it). The calls and returns are
/// exactly what `Vm::run`'s fast `DoCall` and `Return` arms do, in the
/// same order: those push or pop only plain user frames (no observer,
/// generator, by-reference return, attached symbol table, named or spread
/// arguments, coercion, deep recursion: anything else is declined and runs
/// as the generic op), and run no user code; the frame's `ip` is stored as
/// there, and a due safe point is run by `Vm::run` right after the call or
/// return, as there. Every frame entered is looked at afresh: a callee
/// runs on the tape whatever its caller ran on.
///
/// Never inlined: the loop's state (instruction pointer, slot bases) lives
/// in its own registers, which is also why it is one function.
#[inline(never)]
#[allow(clippy::too_many_lines)]
pub(super) fn run_tape(vm: &mut Vm, base: usize, code: &OpArray, ip: usize) -> (usize, usize) {
    let mut switched = 0;
    let mut code: *const OpArray = code;
    let mut ip = ip;
    // The call `calls::prepare` readied in the current frame, until its
    // `DoCall` enters it. Turned back into the generic pending call
    // (`flush`) before anything else could see the caller's calls: an op
    // handed to the generic code that passes an argument, and every way
    // out of this function.
    let mut prep: Option<calls::Prepared> = None;
    // Whether the current frame was just pushed by a call made below: its
    // slots are sized for its code and no symbol table is attached
    // (`usable` holds without the test).
    let mut fresh = false;
    // Whether a call or return just changed the current frame: its resume
    // point and code are read below from the frame the loop looks up
    // anyway (`LOAD_OPLINE()`), instead of a lookup of their own at each
    // switch (two lookups of the top frame per switch cost ~13
    // instructions each).
    let mut reload = false;
    'frames: loop {
        let Some(f) = vm.frames.last_mut() else {
            if reload {
                return (0, LEFT);
            }
            flush(vm, &mut prep);
            return (ip | switched, NO_SAFE_POINT);
        };
        if reload {
            reload = false;
            ip = f.ip;
            code = Rc::as_ptr(&f.code);
        }
        // SAFETY: as in `Vm::run`: the op array is owned by `Rc`s, one of
        // them held by the current frame, so its heap block stays where it
        // is while that frame is current. Until the next call or return
        // below (after which it is reloaded from the new current frame),
        // only hot ops run, which neither push nor pop frames nor replace
        // a frame's `code`; and `Vm::run`, which gets this frame's `ip`
        // back with `SWITCHED`, reloads the op array from the frame itself.
        let c: &OpArray = unsafe { &*code };
        let ops: &[Op] = &c.ops;
        let lits: &[Value] = &c.literals;
        let at = if fresh || usable(f, c) {
            fresh = false;
            let tape: &[Inst] = c.tape.get(c);
            debug_assert_eq!(tape.len(), ops.len() + 1);
            let tbase = tape.as_ptr();
            // The trace JIT's tables for this tape (see [`jit`]): `take_jump!`
            // counts hot back edges through them and may run or compile a
            // trace. Built once per frame entry (two relaxed loads off the
            // op array); dead when the JIT is off or this tape's tracing
            // was killed.
            let jctx = jit::JitCtx::maybe_for_tape(&c.tape, tape);
            // SAFETY: `ip` is an op index of this op array (`Vm::run`'s, a
            // frame's stored resume point, or what a hot op continued at),
            // at most `ops.len()`: inside the tape, whose last element is
            // the sentinel.
            let mut p = unsafe { tbase.add(ip.min(ops.len())) };
            // The current frame by raw pointer: the call instructions need
            // the VM (the frame stack), and the frame is looked up again
            // after each of them.
            let mut fp: *mut Frame = f;
            loop {
                // SAFETY: `fp` is the current frame (`vm.frames`' last
                // live one), derived after the last use of the VM.
                let f = unsafe { &mut *fp };
                // The slot vectors are read again after every hot op run
                // out of the core (none of them resizes one, but none is
                // bound not to replace its buffer).
                let st = St {
                    cvs: f.cvs.as_mut_ptr().cast(),
                    tmps: f.tmps.as_mut_ptr().cast(),
                    lits: lits.as_ptr().cast(),
                    f: std::ptr::from_mut(f),
                };
                // SAFETY: the frame is `usable` for this tape (tested
                // above; a hot op that attaches a symbol table sends the
                // frame back through that test below), and `p` points into
                // the tape.  The `JIT` parameter picks the instantation:
                // with a dead (or absent) context the back-edge hook is
                // compiled out of the loop entirely.
                let out = match &jctx {
                    Some(j) => {
                        // For the rows of a compiled loop that read through
                        // the global scope (`jit::JitCtx::set_vm`).
                        j.set_vm(std::ptr::from_mut(vm));
                        // SAFETY: as above; `j` is this tape's live context.
                        unsafe { run_core::<true>(st, std::ptr::from_ref(j), p) }
                    }
                    // SAFETY: as above; no context, so no back-edge hook.
                    None => unsafe { run_core::<false>(st, std::ptr::null(), p) },
                };
                // SAFETY: both point into the tape.
                #[allow(clippy::cast_sign_loss)]
                let at = unsafe { out.ip.offset_from(tbase) } as usize;
                let next = match out.why {
                    WHY_CALL => {
                        // SAFETY: `out.ip` points into the tape (the
                        // instruction that stopped the core).
                        let next = run_call(vm, &mut prep, ops, lits, at, unsafe { &*out.ip });
                        if next == ENTERED {
                            // The call was entered: the tape goes on in the
                            // callee's frame, as after an entered `DoCall`
                            // below (sized for its code, no symbol table).
                            fresh = true;
                            switched = SWITCHED;
                            if crate::interrupt::safe_point_due() {
                                return (0, LEFT);
                            }
                            reload = true;
                            continue 'frames;
                        }
                        let Some(f) = vm.frames.last_mut() else {
                            flush(vm, &mut prep);
                            return (0, NO_SAFE_POINT);
                        };
                        fp = f;
                        if next & (CALLED | CALL_THREW) == 0 || next == DECLINED {
                            next
                        } else if next & CALL_THREW != 0 {
                            // As after the `DoCall` arm's call below.
                            return ((next & !CALL_THREW) | switched, hot::THREW);
                        } else {
                            // The builtin call was made: the safe point
                            // after its `DoCall`, as the arm below; then
                            // the op after it (the loop looks at the frame
                            // afresh: the handler may have attached its
                            // symbol table).
                            let d = next & !CALLED;
                            if crate::interrupt::safe_point_due() {
                                return ((d + 1) | switched, d);
                            }
                            d + 1
                        }
                    }
                    WHY_VM => {
                        // The interrupt check after a frameless call's
                        // handler is a safe point: a call being prepared
                        // becomes the pending call first. (The reads run
                        // no user code and leave the frame stack alone.)
                        if matches!(ops.get(at), Some(Op::FramelessICall(_))) {
                            flush(vm, &mut prep);
                        }
                        let next = run_vm_op(vm, ops, at);
                        let Some(f) = vm.frames.last_mut() else {
                            flush(vm, &mut prep);
                            return (0, NO_SAFE_POINT);
                        };
                        fp = f;
                        if next != DECLINED && next & CALL_THREW != 0 {
                            // As after a builtin's call that threw.
                            return ((next & !CALL_THREW) | switched, hot::THREW);
                        }
                        next
                    }
                    WHY_EXT => {
                        // An argument the generic `Send` passes goes to the
                        // newest pending call: the prepared one becomes it.
                        if prep.is_some() && matches!(ops.get(at), Some(Op::Send { .. })) {
                            flush(vm, &mut prep);
                        }
                        // SAFETY: `fp` is current (`flush` only changed
                        // the frame's pending calls).
                        hot::step_ext(unsafe { &mut *fp }, ops, lits, at)
                    }
                    // A declined instruction: the op's old fast path, by
                    // the shortest route (a family op is not one of
                    // `hot::step`'s own arms; `ext`, worked out by
                    // `lower`).
                    // SAFETY: `out.ip` points into the tape (as above).
                    WHY_STEP if unsafe { (*out.ip).ext } => hot::step_ext(f, ops, lits, at),
                    WHY_STEP => step_slow(f, ops, lits, at),
                    WHY_POLL => {
                        flush(vm, &mut prep);
                        // SAFETY: as above.
                        #[allow(clippy::cast_sign_loss)]
                        let last = unsafe { out.at.offset_from(tbase) } as usize;
                        return (at | switched, last);
                    }
                    _ => break at,
                };
                if next == DECLINED {
                    break at;
                }
                if crate::interrupt::safe_point_due() {
                    flush(vm, &mut prep);
                    return (next | switched, at);
                }
                // SAFETY: as above; `fp` is current.
                if unsafe { (*fp).symtab_attached } {
                    // The op attached a symbol table: the frame leaves the
                    // tape.
                    flush(vm, &mut prep);
                    ip = next;
                    continue 'frames;
                }
                // SAFETY: a hot op continues at an op index of this op
                // array (the next op or a jump target).
                p = unsafe { tbase.add(next.min(ops.len())) };
            }
        } else {
            let (next, last) = hot::run_steps(f, ops, lits, ip);
            if last != NO_SAFE_POINT {
                flush(vm, &mut prep);
                return (next | switched, last);
            }
            next
        };
        // `ZEND_DO_UCALL` / `ZEND_RETURN` of a plain user function: the
        // prepared call entered directly (`calls::enter`), or the fast
        // arms of `Vm::run`, which store `at + 1` as the frame's resume
        // point (`SAVE_OPLINE`).
        let changed = match ops.get(at) {
            Some(&Op::DoCall { dst }) => {
                fresh = true;
                match prep.take() {
                    // `ZEND_DO_ICALL` of the prepared builtin call: the
                    // handler runs here, and the caller goes on after the
                    // call, looked at afresh (the handler may have attached
                    // its symbol table), with the safe point after the call
                    // when it is due, as after `Vm::run`'s generic `DoCall`.
                    // Tested before `enter`, which would only hand the call
                    // back: called for builtins too, it was taken out of
                    // line and cost each builtin call ~75 instructions.
                    Some(call) if call.is_native() => {
                        fresh = false;
                        match calls::call_native(vm, call, dst, at + 1) {
                            Ok(Ok(())) => {
                                if crate::interrupt::safe_point_due() {
                                    return ((at + 1) | switched, at);
                                }
                                ip = at + 1;
                                continue 'frames;
                            }
                            Ok(Err(t)) => {
                                vm.tape_throw = Some(t);
                                return (at | switched, hot::THREW);
                            }
                            Err(call) => {
                                calls::materialize(vm, call);
                                false
                            }
                        }
                    }
                    // A function compiled whole: the call is made in
                    // machine code and is done, as the builtin's above.
                    Some(call) if calls::compiled(vm, &call, dst) => {
                        fresh = false;
                        if crate::interrupt::safe_point_due() {
                            return ((at + 1) | switched, at);
                        }
                        ip = at + 1;
                        continue 'frames;
                    }
                    Some(call) => match calls::enter(vm, call, dst, at + 1) {
                        Ok(()) => true,
                        Err(call) => {
                            calls::materialize(vm, call);
                            vm.push_simple_ucall(dst, at + 1)
                        }
                    },
                    // `ZEND_FRAMELESS_ICALL_n` of a pending call the tape
                    // did not prepare (one started while another was
                    // being prepared): in place, as `Vm::op_do_call` runs
                    // it, and the caller goes on as after a builtin's call
                    // above.
                    None => match frameless_pending(vm, dst, at + 1) {
                        Some(Ok(())) => {
                            fresh = false;
                            if crate::interrupt::safe_point_due() {
                                return ((at + 1) | switched, at);
                            }
                            ip = at + 1;
                            continue 'frames;
                        }
                        Some(Err(t)) => {
                            vm.tape_throw = Some(t);
                            return (at | switched, hot::THREW);
                        }
                        None => {
                            let ok = vm.push_simple_ucall(dst, at + 1);
                            if !ok && pending_builtin(vm) {
                                // `ZEND_DO_ICALL` of a builtin the generic
                                // call path makes (a method of a builtin
                                // class, a function its `InitFCall` left
                                // pending): run here as `Vm::run`'s generic
                                // `DoCall` arm runs it, instead of a way out
                                // to `Vm::run` and back.
                                fresh = false;
                                if let Some(f) = vm.frames.last_mut() {
                                    f.ip = at + 1;
                                }
                                match vm.op_do_call(dst) {
                                    Ok(false) => {
                                        if crate::interrupt::safe_point_due() {
                                            return ((at + 1) | switched, at);
                                        }
                                        ip = at + 1;
                                        continue 'frames;
                                    }
                                    Ok(true) => true,
                                    Err(t) => {
                                        vm.tape_throw = Some(t);
                                        return (at | switched, hot::THREW);
                                    }
                                }
                            } else {
                                ok
                            }
                        }
                    },
                }
            }
            Some(&Op::Return { val }) => {
                flush(vm, &mut prep);
                fresh = false;
                vm.return_simple(val, base, at + 1).is_some()
            }
            _ => false,
        };
        if !changed {
            flush(vm, &mut prep);
            return (at | switched, NO_SAFE_POINT);
        }
        // The frame's op array may be freed with it: `ops`, `lits` and
        // the op are not read again before they are reloaded above.
        switched = SWITCHED;
        if crate::interrupt::safe_point_due() {
            return (0, LEFT);
        }
        // `LOAD_OPLINE()` of the new current frame, as `Vm::run` does: at
        // the top of the loop.
        reload = true;
    }
}

/// Whether the current frame's newest pending call is of a builtin.
#[inline]
fn pending_builtin(vm: &Vm) -> bool {
    vm.frames
        .last()
        .and_then(|f| f.calls.last())
        .is_some_and(|c| matches!(c.func, crate::func::Func::Builtin(_)))
}

/// `ZEND_FRAMELESS_ICALL_n` of the current frame's newest pending call
/// (`Vm::frameless_icall_in_place`), its result stored in `dst`; the
/// frame's resume point is `caller_ip` (`SAVE_OPLINE()`, which
/// `Vm::run` stores before its own generic `DoCall`). `None`, the call
/// untouched, when it is not a frameless one.
#[inline(never)]
fn frameless_pending(
    vm: &mut Vm,
    dst: Option<u32>,
    caller_ip: usize,
) -> Option<crate::vm::VmResult<()>> {
    let frame = vm.frames.last_mut()?;
    if !matches!(
        frame.calls.last().map(|c| &c.func),
        Some(crate::func::Func::Builtin(b)) if b.flags & crate::func::flags::FRAMELESS != 0
    ) {
        return None;
    }
    frame.ip = caller_ip;
    let r = vm.frameless_icall_in_place(dst)?;
    Some(r.map(|v| {
        if let Some(d) = dst {
            vm.set_tmp(d, v);
        }
    }))
}

/// Turns a prepared call back into the generic pending call
/// ([`calls::materialize`]).
#[inline(always)]
#[allow(clippy::inline_always)] // a test on every way out of the loop
fn flush(vm: &mut Vm, prep: &mut Option<calls::Prepared>) {
    // Tested first: `take` would store `None` back on every way out.
    if prep.is_some() {
        flush_cold(vm, prep);
    }
}

/// [`flush`] of a prepared call, out of line.
#[cold]
#[inline(never)]
fn flush_cold(vm: &mut Vm, prep: &mut Option<calls::Prepared>) {
    if let Some(call) = prep.take() {
        calls::materialize(vm, call);
    }
}

/// Runs the call instruction at `at` ([`WHY_CALL`]) with the VM:
/// `InitFCall` prepares the call ([`calls::prepare`]), `Send` writes into
/// the prepared call ([`calls::send`]); when they do not apply (the call
/// is first turned back into a pending call), the op's generic fast path
/// (`hot::step_ext`: `init_fcall_in`, `send_in`). Returns where execution
/// continues, or [`DECLINED`].
#[inline(never)]
fn run_call(
    vm: &mut Vm,
    prep: &mut Option<calls::Prepared>,
    ops: &[Op],
    lits: &[Value],
    at: usize,
    inst: &Inst,
) -> usize {
    match ops.get(at) {
        Some(&Op::InitFCall { name, .. }) => {
            flush(vm, prep);
            // `f(<expression>)` of a builtin that needs no handler: nothing
            // to ready; its `Send` makes the call.
            if inst.k == calls::CALL_QUICK {
                if let Some(call) = calls::prepare_quick(vm, name, inst.e, inst.d, at) {
                    *prep = Some(call);
                    return at + 1;
                }
            }
            if let Some(call) = calls::prepare(vm, name) {
                return send_and_enter(vm, prep, call, ops, at);
            }
        }
        Some(&Op::InitMethodCall {
            obj,
            name: Operand::Lit(lit),
        }) => {
            flush(vm, prep);
            if let Some(call) = calls::prepare_method(vm, obj, lit, at) {
                return send_and_enter(vm, prep, call, ops, at);
            }
            // Not a hot op: its generic fast path (`Vm::run`'s pre-match
            // arm), else the generic op in `Vm::run`.
            return if vm.init_method_call_fast(obj, lit, at) {
                at + 1
            } else {
                DECLINED
            };
        }
        Some(&Op::New {
            class: ClassSpec::Named(lit),
            dst,
            skip,
        }) => {
            flush(vm, prep);
            match calls::new_object(vm, lit, dst, skip) {
                Some(calls::NewDone::Skip(target)) => return target,
                Some(calls::NewDone::Prepared(call)) => {
                    return send_and_enter(vm, prep, call, ops, at);
                }
                Some(calls::NewDone::Pending) => return at + 1,
                // The generic `New` (`hot::step_ext`, then `Vm::run`).
                None => {}
            }
        }
        Some(Op::InitStaticCall { class, name }) => {
            flush(vm, prep);
            // `Class::name()` by the call site's cache; `self::`,
            // `parent::` and `static::` by the op's slot of the method
            // cache. Not a hot op: else the generic op in `Vm::run`.
            let call = match (class, name) {
                (ClassSpec::Named(_), _) => calls::prepare_static(vm, at),
                (_, &Operand::Lit(lit)) => calls::prepare_relative_static(vm, class, lit, at),
                _ => None,
            };
            return match call {
                Some(call) => send_and_enter(vm, prep, call, ops, at),
                None => DECLINED,
            };
        }
        Some(&Op::InitDynCall {
            callee: Operand::Cv(cv),
        }) => {
            flush(vm, prep);
            if let Some(call) = calls::prepare_closure(vm, cv) {
                return send_and_enter(vm, prep, call, ops, at);
            }
            // Not a hot op: its generic fast path (`Vm::run`'s pre-match
            // arm), else the generic op in `Vm::run`.
            return if vm.init_closure_call_fast(cv) {
                at + 1
            } else {
                DECLINED
            };
        }
        // `call_user_func[_array]()` written by name: not hot ops, so a
        // call not prepared here is the generic op's in `Vm::run`.
        Some(&Op::InitUserCall { callee, array }) => {
            flush(vm, prep);
            return init_user_call(vm, prep, callee, array, ops, at);
        }
        Some(&Op::SendUser { src, n }) => {
            if let Some(call) = prep.as_mut() {
                if calls::send_user(vm, call, src, n) {
                    return at + 1;
                }
            }
            flush(vm, prep);
            return DECLINED;
        }
        Some(&Op::SendArray {
            src,
            len: Operand::Unused,
            skip: 0,
        }) => {
            if let Some(call) = prep.as_mut() {
                if calls::send_array(vm, call, src) {
                    return at + 1;
                }
            }
            flush(vm, prep);
            return DECLINED;
        }
        Some(Op::Send { src }) => {
            if let Some(call) = prep.as_mut() {
                if let Some(id) = call.quick_id() {
                    // The argument and the call at once, past the `DoCall`.
                    if let Some(next) = calls::quick_send(vm, id, src, ops, at) {
                        *prep = None;
                        return next;
                    }
                } else if calls::send(vm, call, src) || calls::send_ref(vm, call, src) {
                    return at + 1;
                }
            }
            flush(vm, prep);
        }
        _ => {}
    }
    let Some(f) = vm.frames.last_mut() else {
        return DECLINED;
    };
    hot::step_ext(f, ops, lits, at)
}

/// Runs the op at `at` that needs the VM without being a call's start
/// ([`WHY_VM`]), on the fast path `Vm::run`'s own arm takes for it.
/// Returns where execution continues, [`DECLINED`] (nothing changed: the
/// generic op in `Vm::run`), or the op's index with [`CALL_THREW`] when a
/// frameless call's handler threw (the throw is in `Vm::tape_throw`).
///
/// A function of its own, and small: inside [`run_call`], whose frame the
/// calls' many paths make large, each of these reads paid for all of it.
#[inline(never)]
fn run_vm_op(vm: &mut Vm, ops: &[Op], at: usize) -> usize {
    match ops.get(at) {
        // A global variable by name: its run-time cache hit, the value
        // into the temporary or straight into the variable of the
        // `AssignCv` folded behind the op (then skipped).
        Some(&Op::FetchGlobal {
            place,
            dst,
            to,
            mode,
        }) => match vm.fetch_global_hit(place, mode, dst, to) {
            Some(stored) => at + 1 + usize::from(stored),
            None => fetch_global_slow(vm, place, dst, to, mode, at),
        },
        // A superglobal read as a whole, or a nested read the tape lowered
        // here (`lower_place`): `$this->p[$k][$j]`, `$this->a->b`,
        // `$GLOBALS['name'][$k]`.
        Some(&Op::Fetch { place, mode, dst }) => {
            let fast = match mode {
                FetchMode::Read => vm.fetch_place_fast(place),
                // `[$a, $b] = $x`: only an element of a value (`lower_place`).
                FetchMode::List => {
                    return match super::spec::fetch_tmp_dims_in(vm.frame_mut(), place, false) {
                        Some(v) => {
                            vm.set_tmp(dst, v);
                            at + 1
                        }
                        None => DECLINED,
                    };
                }
                _ => None,
            };
            match fast
                .or_else(|| vm.fetch_prop_dims_fast(place, mode))
                .or_else(|| vm.fetch_global_dims_fast(place, mode))
                .or_else(|| vm.fetch_dyn_prop_fast(place, mode))
                .or_else(|| match mode {
                    FetchMode::Read | FetchMode::Is => {
                        let is = mode == FetchMode::Is;
                        let f = vm.frame_mut();
                        super::spec::fetch_tmp_dims_in(f, place, is)
                            .or_else(|| super::spec::fetch_static_dims_in(f, place, is))
                    }
                    _ => None,
                }) {
                Some(v) => {
                    vm.set_tmp(dst, v);
                    at + 1
                }
                None => DECLINED,
            }
        }
        // `isset()` / `empty()` of `$this->p[$k][$j]` & co. (`lower_place`).
        Some(&Op::Isset { place, dst }) => match vm
            .isset_prop_dims_fast(place, false)
            .or_else(|| super::spec::isset_static_dims_in(vm.frame_mut(), place, false))
        {
            Some(r) => {
                vm.set_tmp(dst, Value::Bool(r));
                at + 1
            }
            None => DECLINED,
        },
        Some(&Op::Empty { place, dst }) => match vm
            .isset_prop_dims_fast(place, true)
            .or_else(|| super::spec::isset_static_dims_in(vm.frame_mut(), place, true))
        {
            Some(r) => {
                vm.set_tmp(dst, Value::Bool(r));
                at + 1
            }
            None => DECLINED,
        },
        // `defined('NAME')` (`ZEND_DEFINED`) on a hit of its run-time
        // cache slot; a miss is the generic op, which fills it.
        Some(&Op::Defined { name, dst }) => match vm.defined_hit(name) {
            Some(r) => {
                vm.set_dst(dst, Value::Bool(r));
                at + 1
            }
            None => DECLINED,
        },
        // `unset($a[$k])`, `unset($this->p[$k][$j])` & co. (`lower_place`).
        Some(&Op::Unset { place }) => {
            if vm.unset_dims_fast(place) {
                at + 1
            } else {
                DECLINED
            }
        }
        // `$this->p[$k][$j] = v`, `$a[$k][] = v`, `$this->a->b = v` & co.
        // (`lower_place`): the value as `Vm::run` reads it for a plain,
        // array or object operand (`read` + `copy_on_assign`), a consumed
        // temporary released once the store is done.
        Some(&Op::Assign { place, val, dst }) => {
            let Some(v) = super::spec::dim_value_in(vm.frame(), val) else {
                return DECLINED;
            };
            match vm
                .assign_prop_dims_fast(place, v)
                .or_else(|v| super::spec::assign_static_dims_in(vm.frame_mut(), place, v))
                .or_else(|v| vm.assign_dyn_prop_fast(place, v))
            {
                Ok(r) => {
                    if let Operand::Tmp(t) = val {
                        vm.set_tmp(t, Value::Null);
                    }
                    vm.set_dst(dst, r);
                    at + 1
                }
                Err(_) => DECLINED,
            }
        }
        // An element of an object's property, on a property cache hit
        // (`Vm::run`'s own arms for these ops): a read into its temporary
        // or the variable of the `AssignCv` folded behind it (then
        // skipped), a write, a compound write, `++`/`--`, `isset`/`empty`.
        Some(&Op::FetchObjPropDim {
            place,
            dim,
            dst,
            to,
            mode,
        }) => match vm.fetch_obj_prop_dim_fast(place, dim, dst, to, mode) {
            Some(stored) => at + 1 + usize::from(stored),
            None => DECLINED,
        },
        Some(&Op::AssignObjPropDim {
            place,
            dim,
            val,
            dst,
        }) => {
            if vm.assign_obj_prop_dim_fast(place, dim, val, dst) {
                at + 1
            } else {
                DECLINED
            }
        }
        Some(&Op::AssignOpObjPropDim {
            place,
            dim,
            op,
            val,
            dst,
        }) => {
            if vm.assign_op_obj_prop_dim_fast(place, dim, op, val, dst) {
                at + 1
            } else {
                DECLINED
            }
        }
        Some(&Op::IncDecObjPropDim {
            place,
            dim,
            kind,
            dst,
        }) => match vm.incdec_obj_prop_dim_fast(place, dim, kind) {
            Some(r) => {
                vm.set_dst(dst, r);
                at + 1
            }
            None => DECLINED,
        },
        Some(&Op::IssetObjPropDim {
            place,
            dim,
            dst,
            empty,
        }) => match vm.isset_obj_prop_dim_fast(place, dim, empty) {
            Some(r) => {
                vm.set_tmp(dst, Value::Bool(r));
                at + 1
            }
            None => DECLINED,
        },
        // `global $name` on a warm run-time cache (`Vm::bind_global_cached`),
        // `static $x` once initialised (`Vm::bind_static_cached`: then on
        // past the initialiser), `...$args` of an untyped variadic
        // parameter (`Vm::recv_variadic_plain`).
        // Each only over a variable whose old value releases nothing that
        // could run a destructor or buffer a cycle root: the tape goes on
        // without the safe point after the op where `Vm::run` would run
        // them (`$o = new D; global $o;` destructs before the next
        // statement).
        Some(&Op::Global { cv, name }) => {
            if bind_target_plain(vm, cv) && vm.bind_global_cached(cv, name) {
                at + 1
            } else {
                DECLINED
            }
        }
        Some(&Op::BindStatic { cv, idx, skip }) => {
            if bind_target_plain(vm, cv) && vm.bind_static_cached(cv, idx) {
                skip as usize
            } else {
                DECLINED
            }
        }
        Some(&Op::RecvVariadic { n, cv }) => {
            if bind_target_plain(vm, cv) && vm.recv_variadic_plain(n, cv) {
                at + 1
            } else {
                DECLINED
            }
        }
        // `ZEND_FRAMELESS_ICALL_n` in place (`Vm::frameless_icall_op`),
        // past the `Nop`s of the call ops it replaced.
        Some(Op::FramelessICall(fc)) => match vm.frameless_icall_op(fc, at + 1) {
            Some(Ok(())) => at + 1 + fc.tail(),
            Some(Err(t)) => {
                vm.tape_throw = Some(t);
                at | CALL_THREW
            }
            None => DECLINED,
        },
        _ => DECLINED,
    }
}

/// Whether compiled variable `cv` of the current frame is undefined or
/// holds a value whose release runs nothing (null, a bool, a number, a
/// string): what [`run_vm_op`]'s binds require of the variable they bind.
#[inline]
fn bind_target_plain(vm: &Vm, cv: u32) -> bool {
    vm.frames
        .last()
        .and_then(|f| f.cvs.get(cv as usize))
        .is_some_and(|slot| {
            matches!(
                slot.get(),
                None | Some(
                    Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_)
                )
            )
        })
}

/// The address of the slot the read through the global scope at op `at`
/// of frame `f` reads now, for a compiled loop that copies the value out
/// itself (`jit::C_GREAD`): the main frame's variable or the global store's
/// slot the run-time cache of an [`Op::FetchGlobal`] names
/// (`Vm::fetch_global_hit`'s two cases), or the slot of a superglobal read
/// as a whole (`Vm::fetch_place_fast`: its cached slot). 0 when the cache
/// holds nothing for the op, the slot does not exist, or the op is no such
/// read. The address holds while nothing adds a global variable or
/// replaces the main frame's variables.
pub(super) fn global_slot_addr(vm: &Vm, f: &Frame, at: usize) -> usize {
    use crate::rtcache::GlobalSlot;
    let store_slot = match f.code.ops.get(at) {
        Some(&Op::FetchGlobal { place, .. }) => {
            let cached = f
                .code
                .rt_cache
                .globals
                .try_borrow()
                .ok()
                .and_then(|c| c.get(place as usize).copied());
            match cached {
                Some(GlobalSlot::GlobalCv(cv)) => {
                    return vm
                        .frames
                        .vec
                        .first()
                        .and_then(|main| main.cvs.get(cv as usize))
                        .map_or(0, |slot| std::ptr::from_ref(slot) as usize);
                }
                Some(GlobalSlot::GlobalStore(idx)) => Some(idx),
                _ => None,
            }
        }
        Some(&Op::Fetch {
            place,
            mode: FetchMode::Read,
            ..
        }) => auto_global_of(&f.code, place)
            .and_then(|i| vm.ext.auto_globals.cached_slots.get(i).copied().flatten()),
        _ => None,
    };
    let Some(idx) = store_slot else {
        return 0;
    };
    let Ok(globals) = vm.globals.try_borrow() else {
        return 0;
    };
    globals
        .slots
        .get(idx as usize)
        .map_or(0, |slot| std::ptr::from_ref(slot) as usize)
}

/// The registration index of the superglobal `place` names as a whole
/// (`$_GET` is 0), as `Vm::fetch_place_fast` finds it; `None` for any
/// other place.
fn auto_global_of(code: &OpArray, place: u32) -> Option<usize> {
    let p = code.places.get(place as usize)?;
    if !p.segs.is_empty() {
        return None;
    }
    let crate::opcode::PlaceBase::Global(Operand::Lit(lit)) = &p.base else {
        return None;
    };
    let Value::Str(name) = code.literals.get(*lit as usize)? else {
        return None;
    };
    let name = name.as_bytes();
    if name == b"_GET" {
        Some(0)
    } else if name.first() == Some(&b'_') {
        crate::auto_globals::index(name)
    } else {
        None
    }
}

/// The reads of [`run_vm_op`] on their fast paths alone, for a compiled
/// loop (`jit::jit_row_vm_read`): the run-time cache hit of
/// [`Op::FetchGlobal`] (`mode` given), or a superglobal read as a whole
/// (`Op::Fetch`, no `mode`), the value into `dst` or the variable `to`.
/// `Some(stored in the variable)`; `None`, nothing changed, when the
/// interpreted row has to decide (a miss, an undefined variable read with a
/// warning).
pub(super) fn vm_read(
    vm: &mut Vm,
    place: u32,
    dst: u32,
    to: Option<u32>,
    mode: Option<FetchMode>,
) -> Option<bool> {
    if let Some(mode) = mode {
        return vm.fetch_global_hit(place, mode, dst, to);
    }
    let v = vm.fetch_place_fast(place)?;
    vm.set_tmp(dst, v);
    Some(false)
}

/// [`Op::FetchGlobal`] on a cache miss: the name is resolved and the slot
/// filled (`Vm::fetch_global_fast`).
#[cold]
#[inline(never)]
fn fetch_global_slow(
    vm: &mut Vm,
    place: u32,
    dst: u32,
    to: Option<u32>,
    mode: FetchMode,
    at: usize,
) -> usize {
    match vm.fetch_global_fast(place, mode) {
        Some(v) => at + 1 + usize::from(vm.store_fetched(dst, to, v)),
        None => DECLINED,
    }
}

/// [`run_call`]'s answer when it entered the call it prepared: the
/// callee's frame is the current one.
const ENTERED: usize = usize::MAX - 1;

/// Set in [`run_call`]'s answer (with the `DoCall`'s index) when it made
/// the builtin call it prepared ([`calls::call_native`]): the caller goes
/// on after the `DoCall`.
const CALLED: usize = 1 << (usize::BITS - 2);

/// Set in [`run_call`]'s answer (with the `DoCall`'s index) when the
/// builtin call it made threw ([`calls::call_native`]; the throw is in
/// `Vm::tape_throw`).
const CALL_THREW: usize = 1 << (usize::BITS - 3);

/// The `Send`s after the op at `at` that started `call`
/// ([`send_following`]), and the call itself when its `DoCall` comes right
/// after them and it is a user one (`ZEND_DO_UCALL`: [`calls::enter`]),
/// without another round trip through the loop: [`ENTERED`]. Otherwise the
/// call is kept in `prep` for its `DoCall` (a builtin's, one `enter`
/// declines, or one whose `Send`s were not all passed here), and the op
/// where the tape goes on is returned.
#[inline(always)]
#[allow(clippy::inline_always)] // one copy per kind of call start
fn send_and_enter(
    vm: &mut Vm,
    prep: &mut Option<calls::Prepared>,
    mut call: calls::Prepared,
    ops: &[Op],
    at: usize,
) -> usize {
    let next = send_following(vm, &mut call, ops, at);
    enter_after_sends(vm, prep, call, ops, next)
}

/// The rest of [`send_and_enter`] once the arguments are passed: the call
/// made when the op at `next` is its `DoCall`, else kept in `prep`.
#[inline(always)]
#[allow(clippy::inline_always)] // one copy per kind of call start
fn enter_after_sends(
    vm: &mut Vm,
    prep: &mut Option<calls::Prepared>,
    mut call: calls::Prepared,
    ops: &[Op],
    next: usize,
) -> usize {
    if let Some(&Op::DoCall { dst }) = ops.get(next) {
        if call.is_native() {
            // `ZEND_DO_ICALL` at once (as the loop's `DoCall` arm runs it).
            match calls::call_native(vm, call, dst, next + 1) {
                Ok(Ok(())) => return next | CALLED,
                Ok(Err(t)) => {
                    vm.tape_throw = Some(t);
                    return next | CALL_THREW;
                }
                Err(back) => call = back,
            }
        } else {
            // A function compiled whole: the call is made in machine code
            // and is done, as a builtin's is.
            if calls::compiled(vm, &call, dst) {
                return next | CALLED;
            }
            match calls::enter(vm, call, dst, next + 1) {
                Ok(()) => return ENTERED,
                Err(back) => call = back,
            }
        }
    }
    *prep = Some(call);
    next
}

/// The `Send`s right after the op at `at`, which started the prepared
/// `call`, passed at once ([`calls::send`]): they are its arguments (an
/// argument's own call would start with its own op), and nothing runs in
/// between, so each loop round trip saved changes nothing else. Returns
/// where the tape goes on: the op after the last one passed (the `DoCall`,
/// or a `Send` `calls::send` declined, which then runs as before).
#[inline]
fn send_following(vm: &mut Vm, call: &mut calls::Prepared, ops: &[Op], at: usize) -> usize {
    let mut next = at + 1;
    while let Some(Op::Send { src }) = ops.get(next) {
        // (A plain variable for a by-reference parameter: `send_ref`.)
        if !calls::send(vm, call, src) && !calls::send_ref(vm, call, src) {
            break;
        }
        next += 1;
    }
    next
}

/// `ZEND_INIT_USER_CALL` of `callee` at `at` ([`run_call`]; `array`: of
/// `call_user_func_array()`): the call prepared on the call site's cache
/// hit ([`calls::prepare_user_call`]); on a miss, the callback resolved
/// as the generic op resolves it (`Vm::user_call_pending`: the
/// deprecations, the `TypeError`, the cache filled) and the call it makes
/// prepared ([`calls::prepare_pending`]), or pushed as the pending call
/// when it is not one the tape makes. Then its `SendUser`s / `SendArray`
/// right after it are passed and the call is made at its `DoCall` as
/// [`send_and_enter`] makes other calls. A throw of the resolution is the
/// op's ([`CALL_THREW`]). Out of line: `run_call`'s other arms keep their
/// frame.
#[inline(never)]
fn init_user_call(
    vm: &mut Vm,
    prep: &mut Option<calls::Prepared>,
    callee: Operand,
    array: bool,
    ops: &[Op],
    at: usize,
) -> usize {
    let mut call = match calls::prepare_user_call(vm, callee, at) {
        Some(call) => call,
        None => match user_call_miss(vm, callee, array, at) {
            Ok(call) => call,
            Err(next) => return next,
        },
    };
    let mut next = at + 1;
    loop {
        let sent = match ops.get(next) {
            Some(&Op::SendUser { src, n }) => calls::send_user(vm, &mut call, src, n),
            Some(&Op::SendArray {
                src,
                len: Operand::Unused,
                skip: 0,
            }) => calls::send_array(vm, &mut call, src),
            _ => false,
        };
        if !sent {
            break;
        }
        next += 1;
    }
    enter_after_sends(vm, prep, call, ops, next)
}

/// [`init_user_call`] on a miss of the call site's cache: the call the
/// generic op would push, prepared; `Err` with where the tape goes on
/// when it is not (pushed as the pending call: the op after this one), or
/// when the resolution threw (the op's index with [`CALL_THREW`], the
/// throw in `Vm::tape_throw`). Cold: the cached path is the common one.
#[cold]
#[inline(never)]
fn user_call_miss(
    vm: &mut Vm,
    callee: Operand,
    array: bool,
    at: usize,
) -> Result<calls::Prepared, usize> {
    let pending = match vm.user_call_pending(callee, array, at) {
        Ok(pending) => pending,
        Err(t) => {
            vm.tape_throw = Some(t);
            return Err(at | CALL_THREW);
        }
    };
    match calls::prepare_pending(vm, pending) {
        Ok(call) => Ok(call),
        Err(pending) => {
            if let Some(f) = vm.frames.last_mut() {
                f.calls.push(pending);
            }
            Err(at + 1)
        }
    }
}

// ---- statics: class constants and static properties ----
//
// `Name::C`, `self::C`, `parent::C`, `static::C` and `A::$x` reads on a
// hit of the op's run-time cache (php-src's `ZEND_FETCH_CLASS_CONSTANT`
// and `ZEND_FETCH_STATIC_PROP_R`/`_IS` with `CACHED_PTR`,
// `Zend/zend_vm_def.h`): the hits `hot::step_ext` takes
// (`spec::fetch_class_const_in`, `spec::fetch_static_prop_in`), as rows of
// their own, so that a hit stays in the tape's loop instead of leaving it
// for the second-level dispatch. A miss declines, having changed nothing,
// and `hot::step_ext` (the row's `ext` flag), then the generic op, run it.

/// The `e` field of a class-constant row: how the class is named.
const SC_NAMED: u8 = 0;
/// `self::`
const SC_SELF: u8 = 1;
/// `parent::`
const SC_PARENT: u8 = 2;
/// `static::`
const SC_STATIC: u8 = 3;

/// The `e` field of a static-property row: its fetch mode.
const SP_READ: u8 = 0;
/// `FetchMode::Is`
const SP_IS: u8 = 1;
/// `FetchMode::List`
const SP_LIST: u8 = 2;

/// The class operand of a class-constant row: `e` with, for a named
/// class, its literal in `p`.
#[inline(always)]
#[allow(clippy::inline_always, clippy::cast_possible_truncation)] // one match in a thin handler
fn statics_class(inst: &Inst) -> ClassSpec {
    match inst.e {
        SC_SELF => ClassSpec::SelfClass,
        SC_PARENT => ClassSpec::Parent,
        SC_STATIC => ClassSpec::Static,
        // `p` came from a `u32` literal index (`lower_statics`).
        _ => ClassSpec::Named(inst.p as u32),
    }
}

/// The fetch mode of a static-property row (`e`).
#[inline(always)]
#[allow(clippy::inline_always)] // one match in a thin handler
fn statics_mode(inst: &Inst) -> FetchMode {
    match inst.e {
        SP_IS => FetchMode::Is,
        SP_LIST => FetchMode::List,
        _ => FetchMode::Read,
    }
}

/// The class-constant and static-property instruction of the op at `i`:
/// a literal constant name and a class that is not dynamic (`$obj::C`
/// stays `hot::step_ext`'s; `$this::C` is `props`' `ThisConst`), a static
/// property read in any mode. Fields: `a` the name literal (constant) or
/// the place (property), `b` the result temporary, `e` the class kind or
/// the fetch mode, `p` a named class's literal; a `To` form jumps (`c`)
/// past its folded `AssignCv` when the value went into that op's variable
/// `d`.
fn lower_statics(code: &OpArray, i: usize) -> Option<Inst> {
    let plain = Inst::plain;
    let to_row = |inst: Inst, cv: u32| -> Option<Inst> {
        if (cv as usize) >= code.cv_names.len() {
            return None;
        }
        Some(Inst {
            c: rel(code, i, u32::try_from(i + 2).ok()?)?,
            d: cv,
            ..inst
        })
    };
    let class_kind = |class: &ClassSpec| -> Option<(u8, usize)> {
        Some(match *class {
            ClassSpec::Named(l) if (l as usize) < code.literals.len() => (SC_NAMED, l as usize),
            ClassSpec::SelfClass => (SC_SELF, 0),
            ClassSpec::Parent => (SC_PARENT, 0),
            ClassSpec::Static => (SC_STATIC, 0),
            _ => return None,
        })
    };
    match *code.ops.get(i)? {
        Op::FetchClassConst {
            ref class,
            name: Operand::Lit(name),
            dst,
        } => {
            let (e, p) = class_kind(class)?;
            if dst >= code.num_tmps || (name as usize) >= code.literals.len() {
                return None;
            }
            Some(Inst {
                a: name,
                b: dst,
                e,
                p,
                ..plain(TOp::ClassConst)
            })
        }
        Op::FetchClassConstTo {
            ref class,
            name,
            dst,
            to,
        } => {
            let (e, p) = class_kind(class)?;
            if dst >= code.num_tmps || (name as usize) >= code.literals.len() {
                return None;
            }
            to_row(
                Inst {
                    a: name,
                    b: dst,
                    e,
                    p,
                    ..plain(TOp::ClassConstTo)
                },
                to,
            )
        }
        Op::FetchStaticProp {
            place,
            dst,
            to,
            mode,
        } => {
            if (place as usize) >= code.places.len() || dst >= code.num_tmps {
                return None;
            }
            let e = match mode {
                FetchMode::Read => SP_READ,
                FetchMode::Is => SP_IS,
                FetchMode::List => SP_LIST,
            };
            let inst = Inst {
                a: place,
                b: dst,
                e,
                ..plain(TOp::StaticProp)
            };
            match to {
                None => Some(inst),
                Some(cv) => to_row(
                    Inst {
                        op: TOp::StaticPropTo,
                        ..inst
                    },
                    cv,
                ),
            }
        }
        _ => None,
    }
}

/// [`TOp::ClassConst`]: the cached value of the class constant into its
/// temporary `b` (`spec::fetch_class_const_in`, the hit `hot::step_ext`
/// takes).
///
/// # Safety
/// `inst` is an instruction [`lower_statics`] built for `frame`, the
/// current frame.
#[inline(never)]
unsafe fn statics_class_const(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    super::spec::fetch_class_const_in(f, &statics_class(inst), inst.a, inst.b, None).is_some()
}

/// [`TOp::ClassConstTo`]: as [`statics_class_const`], into the folded
/// `AssignCv`'s variable `d` when that holds nothing to release (the jump
/// past it), else into the temporary (falling through to the `AssignCv`).
///
/// # Safety
/// As [`statics_class_const`].
#[inline(never)]
unsafe fn statics_class_const_to(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    match super::spec::fetch_class_const_in(f, &statics_class(inst), inst.a, inst.b, Some(inst.d)) {
        Some(true) => Br::Jump,
        Some(false) => Br::Fall,
        None => Br::Decline,
    }
}

/// [`TOp::StaticProp`]: the static property's cached cell read into the
/// temporary `b` (`spec::fetch_static_prop_in`).
///
/// # Safety
/// As [`statics_class_const`].
#[inline(never)]
unsafe fn statics_static_prop(frame: *mut Frame, inst: &Inst) -> bool {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    super::spec::fetch_static_prop_in(f, inst.a, inst.b, None, statics_mode(inst)).is_some()
}

/// [`TOp::StaticPropTo`]: as [`statics_static_prop`], into the folded
/// `AssignCv`'s variable `d` when it can (see [`statics_class_const_to`]).
///
/// # Safety
/// As [`statics_class_const`].
#[inline(never)]
unsafe fn statics_static_prop_to(frame: *mut Frame, inst: &Inst) -> Br {
    // SAFETY: the caller's contract.
    let f = unsafe { &mut *frame };
    match super::spec::fetch_static_prop_in(f, inst.a, inst.b, Some(inst.d), statics_mode(inst)) {
        Some(true) => Br::Jump,
        Some(false) => Br::Fall,
        None => Br::Decline,
    }
}

mod calls;
mod dims;
pub(crate) mod jit;
mod props;
mod scalar;
mod strs;

#[cfg(test)]
mod tests;

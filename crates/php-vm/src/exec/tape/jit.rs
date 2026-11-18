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

use super::{Inst, St, Tape, TOp, KC, KI, KL, KT};

/// A `Value`/`Option<Value>` slot's tag byte (offset 0 of the slot): the
/// enum discriminant, pinned by `php-runtime`'s `jit_value_layout` test.
#[allow(dead_code)] // layout documentation
const TAG_NULL: u8 = 0;
#[allow(dead_code)] // layout documentation
const TAG_BOOL: u8 = 1;
const TAG_INT: u8 = 2;
const TAG_STR: u8 = 4;
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
        let (cnt, tr) = unsafe {
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
/// The largest region compiled (instructions); larger loops stay
/// interpreted in this version.
const MAX_REGION: usize = 64;
/// The largest number of guards one region runs.
const MAX_GUARDS: usize = 16;
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
    pub(crate) mask: u8,
    pub(crate) arith: u8,
    pub(crate) inc: bool,
    pub(crate) lit_a: i64,
    pub(crate) lit_b: i64,
}

impl Default for JitMeta {
    fn default() -> JitMeta {
        JitMeta {
            class: 0,
            ka: KC,
            kb: KC,
            mask: 0,
            arith: 0,
            inc: false,
            lit_a: 0,
            lit_b: 0,
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
fn add_class_guards(tape: &[Inst], i: usize, class: u16, m: JitMeta, guards: &mut Vec<Guard>) -> bool {
    let inst = &tape[i];
    let read = |kind: u8, off: u32, guards: &mut Vec<Guard>| -> bool {
        match kind {
            KC | KT => add_slot(guards, kind, off, true),
            KI | KL => true,
            _ => false,
        }
    };
    match class {
        C_JMP => true,
        C_INC | C_DEC => add_slot(guards, KC, inst.a, true),
        C_ASSIGN_I | C_ASSIGN_L => add_slot(guards, KC, inst.a, false),
        // The `J_ARITH_CV` destination keeps its tag: it is guarded
        // `Int` (and every whitelisted write keeps it `Int`), so only
        // the payload is stored. `J_ASSIGN_OP`/`J_NUMBIN_C` update
        // `inst.a` in place (`inst.c` is a jump offset or a separate
        // literal slot), so the read of `a` already carries the exact
        // guard.
        J_ARITH_CV => read(m.ka, inst.a, guards)
            && read(m.kb, inst.b, guards)
            && add_slot(guards, KC, inst.c, true),
        J_ASSIGN_OP | J_NUMBIN_C => read(m.ka, inst.a, guards) && read(m.kb, inst.b, guards),
        J_CMP => read(m.ka, inst.a, guards) && read(m.kb, inst.b, guards),
        J_INCCMP => add_slot(guards, KC, inst.a, true) && read(m.kb, inst.b, guards),
        // `inst.a` = a temp (exact: read as an int), then the cv it is
        // written into (weak: nothing of its own is read).
        J_ASSIGN_T => add_slot(guards, KT, inst.b, true) && add_slot(guards, KC, inst.a, false),
        // Arithmetic into a temporary: the destination is fully stored
        // (tag and payload), so it only needs the weak guard (a scalar
        // or nothing, what the interpreter's own store test accepts).
        J_ARITH_TMP | J_NUMBIN_T => {
            read(m.ka, inst.a, guards)
                && read(m.kb, inst.b, guards)
                && add_slot(guards, KT, inst.c, false)
        }
        _ => false,
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
fn plan(st: &St, tape: &[Inst], head: usize, close: usize) -> Option<Plan> {
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

    // Class every instruction; a fused instruction's coverage marks
    // the ops it consumes, exactly as `run_core` skips them.
    let mut i = head;
    let mut last = usize::MAX;
    while i <= close {
        let (c, m) = match classify(st, &tape[i]) {
            Some(cm) => cm,
            None => rej!(format!("{:?} not whitelisted", tape[i].op)),
        };
        class[i - head] = c;
        meta[i - head] = m;
        if debug_on() {
            eprintln!(
                "jit:   scan [{i}] {:?} -> class {c}, step {}",
                tape[i].op,
                c_step(c)
            );
        }
        last = i;
        i += c_step(c);
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
    let mut guards: Vec<Guard> = Vec::new();
    for j in 0..len {
        if class[j] == 0 {
            continue;
        }
        if !add_class_guards(tape, head + j, class[j], meta[j], &mut guards) {
            rej!(format!("guards at [{}, {:?}]", head + j, tape[head + j].op));
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
    labels: HashMap<usize, usize>,
    stubs: HashMap<usize, usize>,
    disms: HashMap<usize, usize>,
    fixups: Vec<(usize, Target)>,
    head_pos: usize,
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
    /// to hold an `Int` (`8B` reg=rax: rm 111/110, modrm 0x87/0x86).
    fn load_int(&mut self, base: u8, off: u32) {
        self.put(&[REX_W, 0x8B, if base == 1 { 0x86 } else { 0x87 }]);
        self.put(&off.wrapping_add(PAYLOAD).to_le_bytes());
    }

    /// The second operand into `r9`: a slot payload, an inline `KI`
    /// constant, or a resolved `KL` literal.
    fn to_r9(&mut self, kind: u8, off: u32, lit: i64) {
        match kind {
            KC | KT => {
                // `8B` reg=r9 (field 001 + REX.R), rm = rdi (0xF) /
                // rsi (0x8E: field 110 is `rsi`, no REX.B).
                if kind == KT {
                    self.put(&[REX_W | REX_R, 0x8B, 0x8E]);
                } else {
                    self.put(&[REX_W | REX_R, 0x8B, 0x8F]);
                }
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
        self.put(&[REX_W, 0x89, if base == 1 { 0x86 } else { 0x87 }]);
        self.put(&off.wrapping_add(PAYLOAD).to_le_bytes());
    }

    /// `mov qword [rdi + off + 8], imm32` (sign-extended): a literal
    /// payload store.
    fn store_imm(&mut self, off: u32, v: i32) {
        self.put(&[REX_W, 0xC7, 0x87]);
        self.put(&off.wrapping_add(PAYLOAD).to_le_bytes());
        self.put(&v.to_le_bytes());
    }

    /// `mov byte [rdi + off], TAG_INT`: the tag store, always after the
    /// payload store (a slot is only ever read as a value at a safe
    /// point, between compiled calls, where the pair is stable).
    fn store_tag(&mut self, off: u32) {
        self.put(&[0xC6, 0x87]);
        self.put(&off.to_le_bytes());
        self.put(&[TAG_INT]);
    }

    /// `mov byte [rdi/rsi + off], imm8`: the tag store with an explicit
    /// base (`rsi` for a temporary) and value (`C6 /0`: modrm 0x87/0x86).
    fn store_tag_base(&mut self, base: u8, off: u32, tag: u8) {
        self.put(&[0xC6, if base == 1 { 0x86 } else { 0x87 }]);
        self.put(&off.to_le_bytes());
        self.put(&[tag]);
    }
}
impl Emitter {
    /// `mov [rdi + off + 8], r9` (payload store from `r9`: `89 /r` names
    /// `r9` through the *reg* field, which `REX.R` extends).
    fn store_int_r9(&mut self, off: u32) {
        self.put(&[REX_W | REX_R, 0x89, 0x8F]);
        self.put(&off.wrapping_add(PAYLOAD).to_le_bytes());
    }

    /// `movabs rax, imm64`.
    fn movabs_rax(&mut self, v: i64) {
        self.put(&[REX_W, 0xB8]);
        self.put(&v.to_le_bytes());
    }

    /// The first operand into `rax`.
    fn to_rax(&mut self, kind: u8, off: u32, lit: i64) {
        match kind {
            KC | KT => self.load_int(kind, off),
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
            C_INC | C_DEC => {
                self.load_int(KC, inst.a);
                self.put(&[REX_W, 0x83, if self.plan.class[j] == C_INC { 0xC0 } else { 0xE8 }, 0x01]);
                self.jcc(0x80, Target::Stub(i)); // jo: overflow, before any store
                self.store_int(KC, inst.a);
            }
            C_ASSIGN_I => {
                self.store_imm(inst.a, inst.b as i32);
                self.store_tag(inst.a);
            }
            C_ASSIGN_L => {
                let v = m.lit_b;
                if i64::from(i32::try_from(v).unwrap_or(i32::MAX)) == v {
                    self.store_imm(inst.a, v as i32);
                } else {
                    self.movabs(true, v);
                    self.store_int_r9(inst.a);
                }
                self.store_tag(inst.a);
            }
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
            // `$cv = TMP`: copy the int, then consume the temporary
            // (null it, as the interpreted row does).
            J_ASSIGN_T => {
                self.load_int(KT, inst.b);
                self.store_int(KC, inst.a);
                self.store_tag(inst.a);
                self.movabs_rax(0);
                self.store_int(KT, inst.b);
                self.store_tag_base(KT, inst.b, TAG_NULL);
            }
            _ => {
                // `J_ARITH_CV` / `J_ASSIGN_OP` / `J_ARITH_TMP` /
                // `J_NUMBIN_T` / `J_NUMBIN_C` (the plan guarantees):
                // `jo` (or the `idiv` guards) before any store. The
                // destination is `inst.c` except for `$cv op= b`,
                // which updates `inst.a`.
                let dst_base = if self.plan.class[j] == J_ASSIGN_OP {
                    KC
                } else if self.plan.class[j] == J_ARITH_TMP || self.plan.class[j] == J_NUMBIN_T {
                    KT
                } else {
                    KC
                };
                let dst = if self.plan.class[j] == J_ASSIGN_OP || self.plan.class[j] == J_NUMBIN_C {
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
                        self.put(&[REX_W | REX_R | REX_B, 0x85, 0xC9]); // test r9, r9
                        self.jcc(0x84, Target::Stub(i)); // je
                        self.put(&[REX_W | REX_B, 0x83, 0xF9, 0xFF]); // cmp r9, -1
                        self.jcc(0x84, Target::Stub(i)); // je
                        self.put(&[REX_W, 0x99]); // cqo
                        self.put(&[REX_W | REX_B, 0xF7, 0xF9]); // idiv r9
                        // `idiv` leaves the remainder in `rdx`.
                        self.put(&[REX_W, 0x89, 0xD0]); // mov rax, rdx
                    }
                }
                if m.arith != 5 {
                    self.jcc(0x80, Target::Stub(i)); // jo: overflow
                }
                self.store_int(dst_base, dst);
                if dst_base == KT {
                    self.store_tag_base(KT, dst, TAG_INT);
                }
            }
        }
    }
}
impl Emitter {
    fn new(plan: Plan, tape: Vec<Inst>) -> Emitter {
        Emitter {
            code: Vec::new(),
            plan,
            tape,
            labels: HashMap::new(),
            stubs: HashMap::new(),
            disms: HashMap::new(),
            fixups: Vec::new(),
            head_pos: 0,
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
                self.emit_instr(j);
                j += c_step(c);
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

/// The compiled entry: `fn(cvs, tmps) -> resume index`.
type LoopFn = unsafe extern "C" fn(*mut u8, *mut u8) -> i64;

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
            let note = if classify(st, inst).is_some() {
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
    let p = match plan(st, tape, head, close) {
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
    let mut em = Emitter::new(p, tape.to_vec());
    em.emit();
    em.resolve();
    if debug_on() {
        eprintln!("jit: region {head}..={close} compiled to {} bytes", em.code.len());
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
pub(crate) unsafe fn back_edge(
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
        let r = unsafe { f(st.cvs, st.tmps) };
        if debug_on() {
            static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let k = RUNS.fetch_add(1, Ordering::Relaxed);
            if k % 100_000 == 0 {
                eprintln!("jit: trace run #{k} r={r}");
            }
        }
        if r < 0 {
            if debug_on() {
                // Which guard's slot broke the assumption: dump the tags.
                let tags = unsafe { std::slice::from_raw_parts(st.cvs, 64) };
                eprintln!(
                    "jit:   cvs tags @0,16,32,48 = {} {} {} {}",
                    tags[0], tags[16], tags[32], tags[48]
                );
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

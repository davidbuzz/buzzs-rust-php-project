// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The JIT: a hot op array's tape compiled to machine code
//! (ref: `ext/opcache/jit/zend_jit.c`, the function JIT; `zend_jit_ir.c`
//! for what each handler becomes).
//!
//! The unit of compilation is the tape ([`super`]), and the compiled code
//! is a drop-in for [`super::run_core`]: it runs specialised instructions
//! from a given one until an instruction stops it, and says where and why
//! ([`super::Out`]). Everything around it ([`super::run_tape`]: the ops
//! handed to the old fast paths, calls, returns, safe points) is unchanged
//! and does not know which of the two ran.
//!
//! * The numeric instructions (smart branches, `+ - *`, `++`/`--`, scalar
//!   assignments) are emitted inline: the type tests, the arithmetic with
//!   its overflow test, the store.
//! * A family instruction ([`Shape::Family`]) is a call of its handler,
//!   through a trampoline with a C signature ([`Handler`]).
//! * An instruction that declines leaves the code with `WHY_STEP`, having
//!   changed nothing, exactly as in `run_core`. So does every instruction
//!   `run_core` leaves at (`Exit`, `Step`, `Ext`, the calls). (Running the
//!   declined op from the code instead, through a call of its old fast
//!   path, was tried: it cost the loops that never decline 3 instructions
//!   a round, for the registers kept across the possible call.)
//! * A loop that keeps leaving from its own body (an operand that is a
//!   string on every round, say) pays for entering the code again each
//!   time, 50-80 instructions; after [`BAD_LIMIT`] such leaves its entry
//!   hands the loop back to the interpreter ([`enter`]).
//! * A taken jump polls the safe-point flag, as `take_jump!` does.
//!
//! * The step of a `foreach` (`Op::FeFetch`, which has no tape
//!   instruction) is a call of its fast path ([`fe_fetch`]).
//!
//! The code is entered where a loop worth compiling starts
//! ([`loop_heads`]): [`install`] puts a [`super::TOp::Native`] instruction
//! there, which `run_core` runs like any other ([`enter`]). So the tape
//! loop itself is unchanged, an op array that is not compiled pays
//! nothing, and after the code has left (a declined instruction, a call)
//! the interpreter goes on in the tape until it meets such an instruction
//! again.
//!
//! Compiling is what costs: millions of machine instructions for a
//! function of a few loops, more than a loop of a few rounds takes to run.
//! So nothing is compiled when the tape is decoded. The entry instruction
//! counts how often the interpreter reaches it ([`warm`]), and the tape is
//! compiled when that makes `opcache.jit_hot_loop` times; until then (and
//! for good, if the code generator gives up) the interpreter runs the op
//! that was there.
//!
//! No value lives in a register across two instructions here: what this
//! first tier removes is the dispatch and the operand decoding, not the
//! type tests or the loads and stores. The second tier ([`typed`]) keeps
//! the numbers of a loop in registers. A loop it may take is tried there
//! first, straight from the entry instruction ([`enter`]), and the first
//! tier is only compiled for a loop the second tier gave up on (or cannot
//! take at all): its code would be no more than what the typed code falls
//! back on, at a third of the compile time. The first tier's code calls
//! the typed code too, at the head of a loop inside one it runs
//! ([`Emit::typed_loop`]). A loop the second tier may take but the first
//! cannot run to its end (it has ops without a tape instruction: a cast,
//! a builtin's call) is entered too, for the second tier's sake.
//!
//! The code generator is Cranelift. One `JITModule` per thread holds all
//! code, which is never freed (an op array that dies leaves its code
//! behind; the hot counters keep one-shot code such as `eval` from
//! getting there).
//!
//! # Safety
//!
//! The machine code reads and writes frame slots at the offsets the tape
//! carries, under the tape's own contract (see [`super`]'s safety notes),
//! and relies on the layout of [`Value`]: `repr(u8)`, so the tag is byte 0,
//! numbered in declaration order, a `bool` is byte 1 and other payloads
//! start at byte 8. [`probe`] checks those facts, and finds the tag of an
//! undefined variable (`None::<Value>`), before anything is compiled; if
//! one does not hold, nothing is.

use super::{Br, Inst, Out, KC, KI, KT, WHY_POLL, WHY_STEP};
use crate::opcode::{cmp_outcome, Op, OpArray, Operand};
use crate::vm::Frame;
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{
    types, AbiParam, Block, InstBuilder, MemFlags, SigRef, Signature, Value as V,
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::Module;
use php_runtime::Value;
use std::cell::RefCell;

mod typed;

/// A family handler as machine code calls it: the frame and the
/// instruction; 0 when it declined, 1 to go on after the ops it covers, 2
/// to go on at its jump target ([`br_code`]).
pub(super) type Handler = unsafe extern "C" fn(*mut Frame, *const Inst) -> usize;

/// A function the code calls with the frame and an entry block
/// ([`specialise`], [`not_typed`]).
type BlockFn = unsafe extern "C" fn(*mut Frame, *mut usize) -> usize;

/// [`fe_fetch`]: a function the code calls with the frame and an op index.
type Step = unsafe extern "C" fn(*mut Frame, usize) -> usize;

/// The code of a compiled tape: the slot bases of [`St`], the index of the
/// instruction to start at and the tape's [`Native`] record. Returns the
/// index of the instruction it stopped at, shifted left by [`WHY_BITS`],
/// with the reason (`WHY_*`) below it.
type Code = unsafe extern "C" fn(
    cvs: *mut u8,
    tmps: *mut u8,
    lits: *const u8,
    f: *mut Frame,
    entry: usize,
    record: *mut usize,
) -> usize;

/// A compiled tape: a leaked record of words that lives as long as the
/// thread's code. Its first words are at the `REC_*` indices; then comes
/// one block of [`HB_WORDS`] words per entry point (the `HB_*` indices),
/// and then the addresses of the functions the code calls.
///
/// The code reads the addresses it needs from the record (one load each)
/// in place of carrying them as immediates: a 64-bit immediate is up to
/// four instructions on aarch64, and a family instruction needs three of
/// them (its handler, its `Inst`, the safe-point flag).
type Native = std::ptr::NonNull<usize>;

/// Record word: the [`Code`], or [`CODE_NONE`] / [`CODE_FAILED`].
const REC_CODE: usize = 0;
/// Record word: written by the code before it returns with `WHY_POLL`,
/// the index of the instruction that asked for the poll.
const REC_AT: usize = 1;
/// Record word: the address of the safe-point flag.
const REC_FLAG: usize = 2;
/// Record word: the address of the tape's first [`Inst`].
const REC_TAPE: usize = 3;
/// Record word: the number of instructions of the tape.
const REC_LEN: usize = 4;
/// Record word: while the tape is not compiled, how many more times the
/// interpreter must reach an entry before it is.
const REC_HOT: usize = 5;
/// Record word: the address of the table of the functions the code calls
/// (made when the tape is compiled).
const REC_FUNCS: usize = 6;
/// Record word: the number of entry blocks.
const REC_COUNT: usize = 7;
/// First record word of the entry blocks.
const REC_ENTRIES: usize = 8;

/// [`REC_CODE`] of a tape that is not compiled yet.
const CODE_NONE: usize = 0;
/// [`REC_CODE`] of a tape the code generator gave up on.
const CODE_FAILED: usize = 1;

/// Words of an entry block.
const HB_WORDS: usize = 13;
/// Entry block word: for an instruction a typed loop is entered again at
/// ([`typed::Plan::Out`]), the address of the entry block of that loop's
/// head, whose typed code it calls; 0 otherwise.
const HB_OWNER: usize = 11;
/// Entry block word, of a loop's head: set when compiling the loop put
/// entry instructions for its resume points into a new tape
/// ([`retape`]); the frame at hand must then stop once to get on it.
const HB_RETAPE: usize = 12;
/// [`HB_DEAD`] of an entry whose instruction is not in the tape yet: one
/// made for a point a typed loop may be entered again at, until a loop
/// is compiled that does.
const DORMANT: usize = 2;
/// Entry block word: whether the first tier is to compile and run the
/// loop that starts here (when the second tier does not).
const HB_NATIVE: usize = 9;
/// Entry block word: set once the entry is given up ([`retire`]).
const HB_DEAD: usize = 10;
/// Entry block word: how often the loop's typed code was compiled again
/// for a read short cut that kept missing ([`missed`]).
const HB_MISS: usize = 7;
/// Entry block word: the instruction whose short cut missed last
/// ([`typed::WHY_MISS`]), for [`missed`].
const HB_MISS_AT: usize = 8;
/// Entry block word: how many more times the first tier's code must reach
/// the loop's head before the second tier looks at it.
const HB_WARM: usize = 6;
/// Entry block word: the second tier's state for the loop that starts
/// here. 0: none (not a loop head, the tier is off, or it gave up);
/// [`TYPED_UNTRIED`]: to be compiled when the code first gets here; else
/// the address of the typed loop's code.
const HB_TYPED: usize = 0;
/// Entry block word: how often the typed loop's types did not hold, or
/// the second tier could not compile it for the types it met
/// ([`FAILS_MASK`], [`RESPECIALISED`]).
const HB_FAILS: usize = 1;
/// Entry block word: the index of the entry's instruction.
const HB_HEAD: usize = 2;
/// Entry block word: the address of the record.
const HB_RECORD: usize = 3;
/// Entry block word: how often the code, entered here, left from inside
/// the loop for an op it could not run.
const HB_BAD: usize = 4;
/// Entry block word: one more than the index of the loop's last
/// instruction (0 when the entry is not a loop head).
const HB_END: usize = 5;

/// [`HB_TYPED`] of a loop the second tier has not looked at yet.
const TYPED_UNTRIED: usize = 1;
/// How often a typed loop's types may fail to hold before it is compiled
/// again for the types met then (once; after that the first tier stops
/// calling it), and how often the second tier looks at a loop it cannot
/// compile.
const FAIL_LIMIT: usize = 4;
/// Set in [`HB_FAILS`] when the loop's typed code is to be compiled
/// without short cuts.
const NO_SHORT_CUTS: usize = 0x200;
/// Set in [`HB_FAILS`] once the loop was found to be one that waits
/// longer before it is compiled ([`typed::Loop::wait`]).
const WAITED: usize = 0x400;
/// How often the read short cut of one instruction may miss before the
/// loop is compiled again with that instruction calling its handler.
const MISS_LIMIT: u32 = 8;
/// How often a loop is compiled again for that before it gets no read
/// short cuts at all.
const MISS_RECOMPILES: usize = 6;

thread_local! {
    /// The misses of read short cuts, by entry block and instruction.
    static MISSES: RefCell<std::collections::HashMap<(usize, usize), u32>> =
        RefCell::new(std::collections::HashMap::new());
}
/// The count in [`HB_FAILS`].
const FAILS_MASK: usize = 0xff;
/// Set in [`HB_FAILS`] once the loop's typed code was replaced.
const RESPECIALISED: usize = 0x100;
/// How often the code may leave from inside a loop before the loop's
/// entry runs the interpreter instead ([`enter`]).
const BAD_LIMIT: usize = 64;

/// Where the interpreter enters the code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    /// The instruction.
    at: usize,
    /// One more than the last instruction of the loop that starts here; 0
    /// when none is known to.
    end: usize,
    /// Whether the second tier is to try the loop.
    typed: bool,
    /// Whether the first tier is to run it (when the second does not).
    native: bool,
    /// Whether it is only a point a typed loop may be entered again at:
    /// not in the tape until one is compiled that is ([`DORMANT`]).
    dormant: bool,
}

/// Byte offset of record word `i`, for the emitter.
fn rec_at(i: usize) -> Option<i32> {
    i32::try_from(i.checked_mul(std::mem::size_of::<usize>())?).ok()
}

/// Bits of a compiled tape's result that hold the reason it stopped.
const WHY_BITS: u32 = 3;

/// [`Br`] as a [`Handler`] returns it.
pub(super) fn br_code(b: Br) -> usize {
    match b {
        Br::Decline => 0,
        Br::Fall => 1,
        Br::Jump => 2,
    }
}

/// How a family instruction ends (the modes of `fam_arm!`).
#[derive(Clone, Copy, Debug)]
pub(super) struct Ending {
    /// The handler answers with a [`Br`] and may jump.
    pub(super) branches: bool,
    /// A safe point is polled on the fall-through path as well.
    pub(super) polls: bool,
}

/// The operator of an arithmetic instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Arith {
    Add,
    Sub,
    Mul,
}

/// What a tape instruction is made of ([`super::TOp::shape`]). Operand
/// kinds are [`super::KC`] and its siblings; `m` is the mask of int
/// outcomes that take the jump.
#[derive(Clone, Copy, Debug)]
pub(super) enum Shape {
    /// `run_core` returns at this instruction with this reason.
    Leave(u8),
    Jmp,
    /// `JmpZT` / `JmpNzT`: jumps when the temporary holds this bool.
    JmpBoolT(bool),
    /// `IncC` (`true`) / `DecC`.
    IncDecC(bool),
    IncDecT {
        inc: bool,
        post: bool,
    },
    AssignC,
    AssignT,
    AssignI,
    AssignL,
    Cmp {
        ka: u8,
        kb: u8,
        m: u8,
    },
    IncCmp {
        inc: bool,
        kb: u8,
        m: u8,
    },
    ArithTmp {
        op: Arith,
        ka: u8,
        kb: u8,
    },
    ArithCv {
        op: Arith,
        ka: u8,
        kb: u8,
    },
    AssignOp {
        op: Arith,
        kb: u8,
    },
    Family {
        handler: Handler,
        covers: usize,
        ending: Ending,
    },
}

// ---------------------------------------------------------------------
// When a tape is compiled
// ---------------------------------------------------------------------

/// Compiles the loops of `tape` (the tape of `code`, just decoded) that
/// are worth it, when the JIT is on, and puts a [`super::TOp::Native`]
/// instruction where each starts: from then on `run_core` enters the
/// machine code there, and the rest of the tape is interpreted as before.
/// A tape that is not compiled (nothing worth it, or the code generator
/// gave up) is left as it is.
#[allow(clippy::too_many_lines)] // the entries of each kind, one after the other
pub(super) fn install(code: &OpArray, tape: &mut [Inst]) {
    let options = crate::jit::options();
    if !options.enabled {
        return;
    }
    // The first tier: the loops that stay in its code from one round to
    // the next.
    let heads = if options.first_tier || options.everything {
        loop_heads(tape, &code.ops, &|i| !leaves(tape, &code.ops, i))
    } else {
        Vec::new()
    };
    // The second tier: also the loops whose ops without a tape
    // instruction it may have a form for.
    let typed_heads: Vec<(usize, usize)> = if options.registers {
        loop_heads(tape, &code.ops, &|i| {
            !leaves(tape, &code.ops, i)
                || code
                    .ops
                    .get(i)
                    .is_some_and(|op| typed::plannable_op(op, &code.literals))
        })
        .into_iter()
        .filter(|(head, last)| typed::candidate(tape, code, *head, *last))
        .collect()
    } else {
        Vec::new()
    };
    if options.registers && why().is_some() {
        // The loops the second tier does not look at, and the first op of
        // each it has no form for.
        let takes = |i: usize| {
            !leaves(tape, &code.ops, i)
                || code
                    .ops
                    .get(i)
                    .is_some_and(|op| typed::plannable_op(op, &code.literals))
        };
        for (head, last) in loop_heads(tape, &code.ops, &|_| true) {
            if typed_heads.iter().any(|(h, _)| *h == head) {
                continue;
            }
            match (head..=last).find(|i| !takes(*i)) {
                Some(i) => eprintln!(
                    "jit: loop at {head}: not looked at, for op {i}: {:?}",
                    code.ops.get(i)
                ),
                None => eprintln!("jit: loop at {head}: not looked at"),
            }
        }
    }
    let typed_at = |at: usize| typed_heads.iter().find(|(head, _)| *head == at).copied();
    let entries: Vec<Entry> = if options.everything {
        // Every instruction the code does something for.
        (0..tape.len())
            .filter(|i| !leaves(tape, &code.ops, *i) || typed_at(*i).is_some())
            .map(|at| Entry {
                at,
                end: 0,
                typed: typed_at(at).is_some(),
                native: true,
                dormant: false,
            })
            .collect()
    } else {
        let mut all: std::collections::BTreeMap<usize, Entry> = heads
            .iter()
            .map(|(at, last)| {
                (
                    *at,
                    Entry {
                        at: *at,
                        end: last + 1,
                        typed: false,
                        native: true,
                        dormant: false,
                    },
                )
            })
            .collect();
        for (at, last) in &typed_heads {
            let native = all.contains_key(at);
            all.insert(
                *at,
                Entry {
                    at: *at,
                    end: last + 1,
                    typed: true,
                    native,
                    dormant: false,
                },
            );
        }
        // Where a typed loop may be entered again: after each call and
        // each `echo` in it, and each comparison and `instanceof` (when
        // its step keeps declining) ([`typed::Plan::Out`]).
        for (head, last) in &typed_heads {
            for at in head + 2..=*last {
                if matches!(
                    code.ops.get(at - 1),
                    Some(
                        Op::DoCall { .. }
                            | Op::Echo(_)
                            | Op::CmpJmp { .. }
                            | Op::Bin { .. }
                            | Op::InstanceOf { .. }
                            | Op::Unary { .. }
                            | Op::FramelessICall(_)
                            | Op::IncDec { .. }
                            | Op::FeFree { .. }
                    )
                ) {
                    // (For the first tier, should it be compiled: this
                    // one instruction, not "every instruction", which an
                    // unknown end stands for.)
                    all.entry(at).or_insert(Entry {
                        at,
                        end: at + 1,
                        typed: false,
                        native: false,
                        dormant: true,
                    });
                }
            }
        }
        all.into_values().collect()
    };
    if entries.is_empty() {
        return;
    }
    // Compiled when the loops prove hot (`enter`): compiling costs
    // millions of instructions, more than a loop of a few rounds runs.
    let Some(native) = new_record(tape, &entries, options.hot_loop.max(1) as usize) else {
        return;
    };
    for (k, entry) in entries.iter().enumerate() {
        let (Ok(index), Ok(block)) = (
            u32::try_from(entry.at),
            u32::try_from(REC_ENTRIES + HB_WORDS * k),
        ) else {
            continue;
        };
        if entry.dormant {
            continue;
        }
        let inst = &mut tape[entry.at];
        inst.op = super::TOp::Native;
        inst.a = index;
        inst.b = block;
        inst.p = native.as_ptr() as usize;
        enter_at_jumps_back(tape, &code.ops, entry.typed, entry.at, entry.end);
    }
}

/// Makes `Vm::run` enter the tape at the jumps back to the head `head`
/// of a loop the second tier is to try (`end`: one more than the loop's
/// last instruction; 0 when not known). A loop whose first op has no fast
/// path of the interpreter's own (`abs()`, a constant) is otherwise run op
/// by op without the tape ever being at its head again: the entry
/// instruction would count one arrival and never a second.
fn enter_at_jumps_back(tape: &mut [Inst], ops: &[Op], typed: bool, head: usize, end: usize) {
    if !typed {
        return;
    }
    for i in head + 1..end.min(tape.len()) {
        // (Not where the tape has no instruction to jump with: a jump on
        // a literal, `while (true)`.)
        let jumps = !matches!(
            tape[i].op,
            super::TOp::Native | super::TOp::Exit | super::TOp::Ext | super::TOp::Step
        );
        if jumps && jump_target(tape, ops, i) == Some(head) {
            tape[i].enter = true;
        }
    }
}

/// The record of `tape`, which is to be entered at `entries` and compiled
/// once the interpreter has reached them `hot` times: its copy of the tape
/// (the handlers are passed the instructions as they are now; `install`
/// overwrites some in the tape itself) and its entry blocks. Leaked: the
/// code is never freed either.
fn new_record(tape: &[Inst], entries: &[Entry], hot: usize) -> Option<Native> {
    let pristine: &[Inst] = Box::leak(tape.to_vec().into_boxed_slice());
    let mut record = vec![0_usize; REC_ENTRIES + HB_WORDS * entries.len()];
    record[REC_CODE] = CODE_NONE;
    record[REC_FLAG] = crate::interrupt::safe_point_flag() as usize;
    record[REC_TAPE] = pristine.as_ptr() as usize;
    record[REC_LEN] = pristine.len();
    record[REC_HOT] = hot;
    record[REC_COUNT] = entries.len();
    let record = Box::leak(record.into_boxed_slice());
    let base = record.as_mut_ptr();
    for (k, entry) in entries.iter().enumerate() {
        let block = &mut record[REC_ENTRIES + HB_WORDS * k..][..HB_WORDS];
        block[HB_TYPED] = if entry.typed { TYPED_UNTRIED } else { 0 };
        block[HB_HEAD] = entry.at;
        block[HB_RECORD] = base as usize;
        block[HB_END] = entry.end;
        block[HB_WARM] = hot;
        block[HB_NATIVE] = usize::from(entry.native);
        if entry.dormant {
            block[HB_DEAD] = DORMANT;
        }
    }
    Native::new(base)
}

/// [`enter`] of a tape that is not compiled: counts the entry, and
/// compiles the tape when it was the last one needed. Either way the
/// interpreter runs the op that was at the entry (as after a declined
/// instruction); the code runs from the next time on.
///
/// # Safety
/// As [`enter`]; `record` is the record of the instruction at `ip`.
#[cold]
#[inline(never)]
unsafe fn warm(f: *mut Frame, ip: *const Inst, record: *mut usize) -> Out {
    // SAFETY: the caller's contract; the record's words are as
    // `new_record` made them.
    unsafe {
        if *record.add(REC_CODE) == CODE_NONE {
            let left = *record.add(REC_HOT);
            if left > 1 {
                *record.add(REC_HOT) = left - 1;
            } else {
                *record.add(REC_CODE) = CODE_FAILED;
                let code = &*std::rc::Rc::as_ptr(&(*f).code);
                php_memlimit::persistent(|| compile(code, record));
            }
        }
    }
    super::step_out(ip)
}

/// Runs the machine code entered at the [`super::TOp::Native`] instruction
/// `ip`, in the frame `f`; stops as [`super::run_core`] does.
///
/// Out of line and called with the frame alone, like the thin family
/// handlers: `run_core` pays nothing for it. Returns where it stopped with
/// the reason in the address's low bits (an `Inst` is aligned to 8), and
/// the instruction that asked for a poll: two words, which come back in
/// registers. (Returned as an [`Out`], through memory, it made `run_core`
/// keep its own `Out` in memory on every way out: 4 instructions on each
/// op handed to the old fast paths, with the JIT off too.)
///
/// A loop whose code keeps leaving from inside it for an op it cannot run
/// is better off interpreted (entering the code costs 50-80 instructions
/// more than going on in `run_core`): after [`BAD_LIMIT`] such leaves, the
/// entry stops as a declined instruction does, so that `run_tape` runs
/// the op that was there through its old fast path and `run_core` goes on
/// after it.
///
/// # Safety
/// As [`super::run_core`]: `f` is the current frame, usable for the tape
/// `ip` points into, and `ip` is an instruction [`install`] wrote.
#[inline(never)]
pub(super) unsafe fn enter(f: *mut Frame, ip: *const Inst) -> (usize, usize) {
    // SAFETY: the caller's contract.
    let out = unsafe { enter_out(f, ip) };
    (out.ip as usize | usize::from(out.why), out.at as usize)
}

/// [`enter`], with its result as `run_core` gives its own.
///
/// # Safety
/// As [`enter`].
#[inline]
#[allow(clippy::too_many_lines)] // one way in for each kind of entry
unsafe fn enter_out(f: *mut Frame, ip: *const Inst) -> Out {
    // SAFETY: the caller's contract. The instruction holds its own index,
    // the record of the code compiled from its tape and where its entry
    // block is in it; the record's first word is its `Code`
    // (`Compiler::compile`), and its `REC_AT` word is read after the code
    // wrote it.
    unsafe {
        let st = super::frame_st(f);
        let entry = (*ip).a as usize;
        let tbase = ip.sub(entry);
        let record = (*ip).p as *mut usize;
        let block = record.add((*ip).b as usize);
        if *block.add(HB_BAD) >= BAD_LIMIT {
            return retire(f, ip, record, block);
        }
        // An instruction a typed loop is entered again at, after the
        // interpreter ran what the loop left for: that loop's code, which
        // goes on from here if all it relies on still holds.
        let owner = *block.add(HB_OWNER) as *mut usize;
        if !owner.is_null() {
            let typed = *owner.add(HB_TYPED);
            if typed != 0 && typed != TYPED_UNTRIED {
                let code = std::mem::transmute::<usize, Code>(typed);
                let r = code(st.cvs, st.tmps, st.lits, st.f, entry, record);
                let stopped = tbase.add(r >> WHY_BITS);
                #[allow(clippy::cast_possible_truncation)]
                let why = (r & ((1 << WHY_BITS) - 1)) as u8;
                return match why {
                    WHY_POLL => Out {
                        ip: stopped,
                        at: tbase.add(*record.add(REC_AT)),
                        why,
                    },
                    typed::WHY_MISS => {
                        *owner.add(HB_MISS_AT) = r >> WHY_BITS;
                        missed(f, owner);
                        super::step_out(stopped)
                    }
                    typed::WHY_GENERIC => super::step_out(stopped),
                    // Not a point the loop's code (compiled again since)
                    // is entered at: nothing enters here any more.
                    typed::WHY_NOT_TYPED if stopped == ip => {
                        *block.add(HB_OWNER) = 0;
                        if *block.add(HB_NATIVE) == 0 && *block.add(HB_TYPED) == 0 {
                            retire(f, ip, record, block)
                        } else {
                            super::step_out(ip)
                        }
                    }
                    // The types did not hold at the end of a round: the
                    // next one, from the loop's head, is the interpreter's.
                    _ => {
                        not_typed(f, owner);
                        super::step_out(stopped)
                    }
                };
            }
            if typed == 0 && *block.add(HB_NATIVE) == 0 && *block.add(HB_TYPED) == 0 {
                // Its loop was given up: nothing enters here any more.
                *block.add(HB_OWNER) = 0;
                return retire(f, ip, record, block);
            }
            if *block.add(HB_NATIVE) == 0 && *block.add(HB_TYPED) == 0 {
                // Its loop is being looked at again: the interpreter's op.
                return super::step_out(ip);
            }
        }
        // A loop the second tier may take: its typed code, called from
        // here, with the interpreter going on wherever that leaves. The
        // first tier is then never compiled for the loop's sake (its code
        // would only be what the typed code falls back on).
        let typed = *block.add(HB_TYPED);
        if typed != 0 {
            let code = if typed == TYPED_UNTRIED {
                let code = specialise(f, block);
                if *block.add(HB_RETAPE) != 0 {
                    // The loop's resume points are in a new tape: this
                    // frame stops here once, to go on on it.
                    *block.add(HB_RETAPE) = 0;
                    return Out {
                        ip,
                        at: ip,
                        why: WHY_POLL,
                    };
                }
                code
            } else {
                typed
            };
            if code != 0 {
                let code = std::mem::transmute::<usize, Code>(code);
                let r = code(st.cvs, st.tmps, st.lits, st.f, entry, record);
                let stopped = tbase.add(r >> WHY_BITS);
                #[allow(clippy::cast_possible_truncation)]
                let why = (r & ((1 << WHY_BITS) - 1)) as u8;
                return match why {
                    WHY_POLL => Out {
                        ip: stopped,
                        at: tbase.add(*record.add(REC_AT)),
                        why,
                    },
                    // Left, or an int overflowed: the interpreter goes on
                    // at the op named (as after a declined instruction).
                    typed::WHY_GENERIC => super::step_out(stopped),
                    // A short cut missed there: the same, counted.
                    typed::WHY_MISS => {
                        *block.add(HB_MISS_AT) = r >> WHY_BITS;
                        missed(f, block);
                        super::step_out(stopped)
                    }
                    // The types did not hold: this round is the
                    // interpreter's.
                    _ => {
                        not_typed(f, block);
                        super::step_out(ip)
                    }
                };
            }
            if *block.add(HB_TYPED) == TYPED_UNTRIED {
                // Not hot yet, or not typed with what the frame holds now
                // (looked at again next time): the interpreter's round.
                return super::step_out(ip);
            }
            // The second tier gave the loop up: the first tier's, if it
            // runs loops of this kind at all.
        }
        if *block.add(HB_NATIVE) == 0 {
            return retire(f, ip, record, block);
        }
        if *record.add(REC_CODE) <= CODE_FAILED {
            return warm(f, ip, record);
        }
        let code = std::mem::transmute::<usize, Code>(*record.add(REC_CODE));
        let r = code(st.cvs, st.tmps, st.lits, st.f, entry, record);
        let stopped = r >> WHY_BITS;
        let ip = tbase.add(stopped);
        #[allow(clippy::cast_possible_truncation)]
        let why = (r & ((1 << WHY_BITS) - 1)) as u8;
        if matches!(
            why,
            WHY_STEP | super::WHY_EXT | super::WHY_EXIT | super::WHY_CALL
        ) && stopped >= entry
            && stopped < *block.add(HB_END)
        {
            *block.add(HB_BAD) += 1;
        }
        Out {
            ip,
            at: if why == WHY_POLL {
                tbase.add(*record.add(REC_AT))
            } else {
                ip
            },
            why,
        }
    }
}

/// Gives the entry at `ip` up: neither tier runs its loop (the second gave
/// it up, and the first is off, or its code kept leaving the loop). The op
/// array gets a tape without this entry's instruction (the record's copy
/// of the tape, with the instructions of the entries still in use), so
/// that the loop runs as if it had never been looked at. Stops for a safe
/// point at the entry: `run_tape` then returns to `Vm::run`, which comes
/// back on the new tape.
///
/// # Safety
/// As [`enter`]; `record` and `block` are those of the instruction at
/// `ip`.
#[cold]
#[inline(never)]
unsafe fn retire(f: *mut Frame, ip: *const Inst, record: *mut usize, block: *mut usize) -> Out {
    // SAFETY: the caller's contract; the record's words are as
    // `new_record` made them, and its copy of the tape has `REC_LEN`
    // instructions.
    unsafe {
        if *block.add(HB_DEAD) != 0 {
            // Given up before (by a frame further in): this frame is on
            // the old tape, and gets the new one at this stop.
            return Out {
                ip,
                at: ip,
                why: WHY_POLL,
            };
        }
        *block.add(HB_DEAD) = 1;
        retape(f, record);
    }
    Out {
        ip,
        at: ip,
        why: WHY_POLL,
    }
}

/// Gives the op array of frame `f` a tape with the entry instructions of
/// the entries of `record` that are in use now (not given up, not
/// [`DORMANT`]): the record's copy of the tape with those. Frames in the
/// middle of the old tape go on reading it (it is leaked, as the record
/// is) until they stop.
///
/// # Safety
/// `f` is the current frame, running the code `record` is of.
unsafe fn retape(f: *mut Frame, record: *mut usize) {
    // SAFETY: the caller's contract; the record's words are as
    // `new_record` made them, and its copy of the tape has `REC_LEN`
    // instructions.
    unsafe {
        let pristine =
            std::slice::from_raw_parts(*record.add(REC_TAPE) as *const Inst, *record.add(REC_LEN));
        let mut tape = pristine.to_vec();
        for k in 0..*record.add(REC_COUNT) {
            let at = REC_ENTRIES + HB_WORDS * k;
            let entry = record.add(at);
            if *entry.add(HB_DEAD) != 0 {
                continue;
            }
            let head = *entry.add(HB_HEAD);
            if let (Some(inst), Ok(index), Ok(at)) =
                (tape.get_mut(head), u32::try_from(head), u32::try_from(at))
            {
                inst.op = super::TOp::Native;
                inst.a = index;
                inst.b = at;
                inst.p = record as usize;
            }
            enter_at_jumps_back(
                &mut tape,
                &(&*f).code.ops,
                *entry.add(HB_TYPED) != 0,
                head,
                *entry.add(HB_END),
            );
        }
        let tape: &'static [Inst] = php_memlimit::persistent(|| Box::leak(tape.into_boxed_slice()));
        (&*f).code.tape.replace(tape);
    }
}

/// The entry block of `record` for the instruction `at`, if it has one.
///
/// # Safety
/// `record` is a record as `new_record` made it.
unsafe fn block_at(record: *mut usize, at: usize) -> Option<*mut usize> {
    // SAFETY: the caller's contract.
    unsafe {
        (0..*record.add(REC_COUNT))
            .map(|k| record.add(REC_ENTRIES + HB_WORDS * k))
            .find(|block| *block.add(HB_HEAD) == at)
    }
}

/// Compiles the typed loop of the entry block `block` for the types the
/// frame `f` holds now, when the second tier can ([`typed`]); called by
/// the first tier's code the first time it reaches the loop's head.
/// Stores and returns the block's new [`HB_TYPED`].
///
/// # Safety
/// `f` is the current frame, running the code of the record `block`
/// belongs to.
#[allow(clippy::too_many_lines)] // one closure for each thing the frame is asked
unsafe extern "C" fn specialise(f: *mut Frame, block: *mut usize) -> usize {
    // SAFETY: the caller's contract; the record's tape copy has `REC_LEN`
    // instructions and lives as long as the record.
    unsafe {
        // Only a loop that goes on is worth it.
        let warm = *block.add(HB_WARM);
        if warm > 1 {
            *block.add(HB_WARM) = warm - 1;
            return 0;
        }
        let head = *block.add(HB_HEAD);
        let record = *block.add(HB_RECORD) as *const usize;
        let tape =
            std::slice::from_raw_parts(*record.add(REC_TAPE) as *const Inst, *record.add(REC_LEN));
        let frame = &*f;
        let snapshot = |slot: typed::SlotRef| -> Option<typed::Ty> {
            let index = usize::try_from(slot.at).ok()? / SLOT;
            let value = if slot.tmp {
                frame.tmps.get(index)?
            } else {
                match frame.cvs.get(index)?.get() {
                    Some(value) => value,
                    None => return Some(typed::Ty::Plain),
                }
            };
            match value {
                Value::Int(_) => Some(typed::Ty::Int),
                Value::Float(_) => Some(typed::Ty::Float),
                Value::Bool(_) => Some(typed::Ty::Bool),
                Value::Null => Some(typed::Ty::Plain),
                _ => None,
            }
        };
        let element = |cv: u32, depth: usize| -> Option<typed::Ty> {
            // (Directly or through a reference: `function f(&$a)`.)
            let through;
            let mut array = match frame.cvs.get(cv as usize)?.get()? {
                Value::Array(array) => array,
                Value::Reference(r) => {
                    through = r.try_borrow()?;
                    let Value::Array(array) = &*through else {
                        return None;
                    };
                    array
                }
                _ => return None,
            };
            for _ in 1..depth {
                let Value::Array(inner) = array.values().next()? else {
                    return None;
                };
                array = inner;
            }
            match array.values().next()? {
                Value::Int(_) => Some(typed::Ty::Int),
                Value::Float(_) => Some(typed::Ty::Float),
                _ => None,
            }
        };
        // What a `foreach` iterator yields now: floats or ints (by the
        // first element), and whether its keys are ints.
        let each = |iter: u32, at: usize| -> Option<(Option<bool>, bool)> {
            // The iterator's array; or, of a `foreach` not started yet (one
            // inside the loop), the array its variable holds now (the one
            // of the last round, as a guess).
            let started = match frame.iters.get(iter as usize)?.as_ref() {
                Some(crate::vm::ForeachIter::Array { arr, .. }) => Some(arr),
                Some(_) => return None,
                None => None,
            };
            // (The `FeReset` of this iterator nearest before the step at
            // `at`: an iterator slot may serve several loops.)
            let subject = || {
                let ops = frame.code.ops.get(..at)?;
                let reset = ops
                    .iter()
                    .rev()
                    .find(|op| matches!(**op, Op::FeReset { iter: i, .. } if i == iter))?;
                std::iter::once(reset).find_map(|op| match *op {
                    Op::FeReset {
                        subject: Operand::Cv(c),
                        kind: crate::opcode::ForeachKind::Value,
                        iter: i,
                        ..
                    } if i == iter => match frame.cvs.get(c as usize)?.get()? {
                        Value::Array(a) => Some(a),
                        _ => None,
                    },
                    Op::FeReset {
                        subject: Operand::Lit(l),
                        kind: crate::opcode::ForeachKind::Value,
                        iter: i,
                        ..
                    } if i == iter => match frame.code.literals.get(l as usize)? {
                        Value::Array(a) => Some(a),
                        _ => None,
                    },
                    _ => None,
                })
            };
            let arr = match started {
                Some(arr) => arr,
                None => subject()?,
            };
            let (key, value) = arr.first()?;
            let kind = |value: &Value| match value {
                Value::Int(_) => Some(false),
                Value::Float(_) => Some(true),
                _ => None,
            };
            // Numbers of one type, as far as the first few elements show.
            let float = kind(value).filter(|&float| {
                arr.values()
                    .take(32)
                    .all(|value| kind(value) == Some(float))
            });
            Some((float, matches!(key, php_runtime::ArrayKey::Int(_))))
        };
        // The builtin a call's name literal resolved to when it last ran
        // here (the op array's function cache).
        let builtin = |name: u32| -> Option<Vec<u8>> {
            if crate::jit::calls_are_observed() {
                return None;
            }
            let cache = frame.code.fcall_cache.try_borrow().ok()?;
            match &cache.get(name as usize)?.as_ref()?.func {
                crate::opcode::CachedFuncKind::Builtin(b) => Some(b.name.as_bytes().to_vec()),
                crate::opcode::CachedFuncKind::User(_) => None,
            }
        };
        // The user function a call site calls, when it can run inline.
        let user = |init: usize, argc: usize| -> Option<std::rc::Rc<crate::func::UserFunction>> {
            use crate::func::{flags, CallShape};
            if crate::jit::calls_are_observed() {
                return None;
            }
            let func = callee_at(frame, init)?;
            let plain = matches!(func.call_shape(), CallShape::UNTYPED | CallShape::TYPED)
                && argc <= func.params.len()
                && argc >= func.required as usize
                && func.flags & flags::CLOSURE == 0
                && func.uses.is_empty()
                && func.hook_prop.is_none()
                && func.code.try_table.is_empty()
                && func.code.ops.len() <= typed::INLINE_OPS;
            plain.then_some(func)
        };
        // What a property read of an inlined method holds now.
        let prop_of_this = |init: usize, func: usize, pid: u32| -> Option<(typed::Ty, u32)> {
            // SAFETY: `func` is the address of a function the analysis
            // holds an `Rc` of (`typed::Inlined`).
            match this_prop(frame, init, func, pid)? {
                (Value::Int(_), slot) => Some((typed::Ty::Int, slot)),
                (Value::Float(_), slot) => Some((typed::Ty::Float, slot)),
                _ => None,
            }
        };
        // The slot a property write of an inlined method goes to, and the
        // types the property takes.
        let write_of_this = |init: usize, func: usize, pid: u32, reads: bool| {
            let Op::InitMethodCall { obj, .. } = *frame.code.ops.get(init)? else {
                return None;
            };
            let recv = match obj {
                Operand::Cv(cv) => cv as usize,
                Operand::Unused => usize::MAX,
                _ => return None,
            };
            let object = receiver_in(frame, recv)?;
            let cache = frame.code.method_cache.try_borrow().ok()?;
            let cached = cache.get(init)?.as_ref()?;
            let scope = cached
                .method_scope
                .as_ref()
                .map_or(0, |scope| scope.as_ptr() as usize);
            // SAFETY: `func` is the address of a function the analysis
            // holds an `Rc` of (`typed::Inlined`).
            let code = &(*(func as *const crate::func::UserFunction)).code;
            crate::exec::spec::jit_this_prop_slot(code, scope, object, pid, reads)
        };
        // The receiver of a method's call now: its variable (`usize::MAX`
        // for `$this`), its class and the frame's scope, both pinned.
        let receiver = |init: usize| -> Option<(usize, usize, usize)> {
            let Op::InitMethodCall { obj, .. } = *frame.code.ops.get(init)? else {
                return None;
            };
            let recv = match obj {
                Operand::Cv(cv) => cv as usize,
                Operand::Unused => usize::MAX,
                _ => return None,
            };
            let class = pin(crate::object::class_of(receiver_in(frame, recv)?));
            let scope = frame.scope.as_ref().map_or(0, pin);
            Some((recv, class, scope))
        };
        // What a property holds now, when its read is a cache hit.
        let property = |place: u32| -> Option<typed::Ty> {
            match crate::exec::spec::read_hit(frame, place)? {
                Value::Int(_) => Some(typed::Ty::Int),
                Value::Float(_) => Some(typed::Ty::Float),
                _ => None,
            }
        };
        let keyed = |cv: u32, lit: u32| -> Option<typed::Ty> {
            let Value::Str(key) = frame.code.literals.get(lit as usize)? else {
                return None;
            };
            let Value::Array(array) = frame.cvs.get(cv as usize)?.get()? else {
                return None;
            };
            match array.get_zstr(key)? {
                Value::Int(_) => Some(typed::Ty::Int),
                Value::Float(_) => Some(typed::Ty::Float),
                _ => None,
            }
        };
        let statik = |place: u32| -> Option<typed::Ty> {
            let (cell, _) = crate::rtcache::static_place_cell_in(frame, place)?;
            let value = cell.try_borrow().ok()?;
            match &*value {
                Value::Int(_) => Some(typed::Ty::Int),
                Value::Float(_) => Some(typed::Ty::Float),
                _ => None,
            }
        };
        let slow = |at: usize| {
            MISSES.with(|m| m.borrow().get(&(block as usize, at)).copied()) >= Some(MISS_LIMIT)
        };
        let constant = |at: usize| -> Option<(typed::Ty, u64)> {
            #[allow(clippy::cast_sign_loss)]
            match const_at(frame, at)? {
                Value::Int(i) => Some((typed::Ty::Int, i as u64)),
                Value::Float(x) => Some((typed::Ty::Float, x.to_bits())),
                Value::Bool(b) => Some((typed::Ty::Bool, u64::from(b))),
                _ => None,
            }
        };
        let resumable = |at: usize| block_at(record.cast_mut(), at).is_some();
        let seen = typed::Seen {
            resumable: &resumable,
            constant: &constant,
            fast: *block.add(HB_FAILS) & NO_SHORT_CUTS == 0,
            slow: &slow,
            snapshot: &snapshot,
            element: &element,
            keyed: &keyed,
            statik: &statik,
            each: &each,
            builtin: &builtin,
            user: &user,
            this_prop: &prop_of_this,
            receiver: &receiver,
            this_write: &write_of_this,
            property: &property,
        };
        // (No loop waits under a hot count below 8, which is for tests:
        // `PHPRUST_JIT=all`, `PHPRUST_JIT_HOT`.)
        let options = crate::jit::options();
        let waited =
            *block.add(HB_FAILS) & WAITED != 0 || options.everything || options.hot_loop < 8;
        let code =
            php_memlimit::persistent(|| compile_typed(&frame.code, tape, head, &seen, waited));
        let code = match code {
            Some(Made::Wait(times)) => {
                // Looked at again after that many more rounds, and then
                // compiled.
                *block.add(HB_FAILS) |= WAITED;
                *block.add(HB_WARM) = times.saturating_mul(options.hot_loop as usize);
                return 0;
            }
            Some(Made::Code(code, resumes)) => Some((code, resumes)),
            None => None,
        };
        let Some((code, resumes)) = code else {
            // Not with these types: looked at again the next time the
            // code gets here (a variable that is an int on the first
            // round and a float from then on), a few times.
            *block.add(HB_FAILS) += 1;
            if *block.add(HB_FAILS) & FAILS_MASK >= FAIL_LIMIT {
                *block.add(HB_TYPED) = 0;
            }
            return 0;
        };
        let state = code as usize;
        *block.add(HB_TYPED) = state;
        // The instructions the loop is entered again at: their entries
        // call this loop's code, and are in the tape from now on.
        let mut changed = false;
        for at in resumes {
            if let Some(entry) = block_at(record.cast_mut(), at) {
                if entry != block {
                    *entry.add(HB_OWNER) = block as usize;
                }
                if *entry.add(HB_DEAD) != 0 {
                    *entry.add(HB_DEAD) = 0;
                    changed = true;
                }
            }
        }
        if changed {
            retape(f, record.cast_mut());
            *block.add(HB_RETAPE) = 1;
        }
        state
    }
}

/// Counts a miss of a read short cut of a typed loop
/// ([`typed::WHY_MISS`]; the instruction is in the block's
/// [`HB_MISS_AT`]). An instruction that misses [`MISS_LIMIT`] times reads
/// something its short cut does not take (a hash table, elements that are
/// not numbers), and each miss costs a way out of the loop and back: the
/// loop is compiled again with that instruction calling its handler.
///
/// # Safety
/// `block` is an entry block of a record.
unsafe extern "C" fn missed(f: *mut Frame, block: *mut usize) -> usize {
    // SAFETY: the caller's contract.
    unsafe {
        let site = (block as usize, *block.add(HB_MISS_AT));
        // A call the loop runs inline that turned out to be of another
        // function (an object of another class), or whose method read a
        // property of another type: the loop is compiled again for what
        // the call is now, and the count starts over. (The loops compiled
        // again are counted too: after `MISS_RECOMPILES`, no short cuts
        // and no inlined calls.)
        let call = matches!(
            (&*f).code.ops.get(site.1),
            Some(Op::InitFCall { .. } | Op::InitStaticCall { .. } | Op::InitMethodCall { .. })
        );
        let count = MISSES.with(|m| {
            let mut m = m.borrow_mut();
            let count = m.entry(site).or_insert(0);
            *count += 1;
            *count
        });
        if count == MISS_LIMIT {
            if std::env::var_os("PHPRUST_JIT_STATS").is_some() {
                eprintln!(
                    "jit: the read short cut of op {} (loop at {}) keeps missing",
                    site.1,
                    *block.add(HB_HEAD)
                );
            }
            *block.add(HB_MISS) += 1;
            if *block.add(HB_MISS) > MISS_RECOMPILES {
                *block.add(HB_FAILS) |= NO_SHORT_CUTS;
            }
            if call {
                MISSES.with(|m| m.borrow_mut().remove(&site));
            }
            *block.add(HB_TYPED) = TYPED_UNTRIED;
        }
    }
    0
}

/// Counts a typed loop whose types did not hold; after [`FAIL_LIMIT`] of
/// them it is compiled again, once, and then given up.
///
/// # Safety
/// `block` is an entry block of a record.
unsafe extern "C" fn not_typed(_f: *mut Frame, block: *mut usize) -> usize {
    // SAFETY: the caller's contract.
    unsafe {
        *block.add(HB_FAILS) += 1;
        let fails = *block.add(HB_FAILS);
        if fails & FAILS_MASK >= FAIL_LIMIT {
            // Compiled once more, for the types the loop has now; a
            // second time it is left to the first tier.
            if fails & RESPECIALISED == 0 {
                *block.add(HB_FAILS) = RESPECIALISED | (fails & NO_SHORT_CUTS);
                *block.add(HB_TYPED) = TYPED_UNTRIED;
            } else {
                *block.add(HB_TYPED) = 0;
            }
        }
    }
    0
}

/// The loops of `tape` that run in machine code from one round to the
/// next, each as its first instruction and its last jump back: the target
/// of each backward jump with no instruction
/// between that target and itself that returns to the interpreter
/// ([`Shape::Leave`]: a call, an op without a specialised instruction).
///
/// Only those are worth compiling. Entering the code costs more than
/// going on in `run_core` (a call, the saved registers), so a loop that
/// leaves on every round loses to the interpreter: measured on aarch64,
/// calls and unspecialised ops cost 3-6% more compiled, while loops of
/// specialised instructions cost 1-29% less.
fn loop_heads(tape: &[Inst], ops: &[Op], stays: &dyn Fn(usize) -> bool) -> Vec<(usize, usize)> {
    let mut heads: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
    for i in 0..tape.len() {
        let Some(target) = jump_target(tape, ops, i) else {
            continue;
        };
        if target <= i && (target..=i).all(stays) {
            // The loop's last jump back: indexes only grow here.
            heads.insert(target, i);
        }
    }
    heads.into_iter().collect()
}

/// Where instruction `i` of `tape` jumps to when it jumps (`None`: it
/// does not).
fn jump_target(tape: &[Inst], ops: &[Op], i: usize) -> Option<usize> {
    let inst = tape.get(i)?;
    let jumps = match inst.op.shape() {
        Shape::Jmp | Shape::JmpBoolT(_) | Shape::Cmp { .. } | Shape::IncCmp { .. } => true,
        // A family instruction that can branch but does not here (a
        // fetch without a `??` target) carries no offset.
        Shape::Family { ending, .. } => ending.branches && inst.c != 0,
        _ => false,
    };
    // A jump on a literal has no tape instruction (`while (1)`): its
    // target is the op's.
    match ops.get(i) {
        Some(&(Op::JmpNz(Operand::Lit(_), to) | Op::JmpZ(Operand::Lit(_), to))) => {
            Some(to as usize)
        }
        _ if jumps => {
            #[allow(clippy::cast_possible_wrap)]
            let delta = i64::from(inst.c as i32) / std::mem::size_of::<Inst>() as i64;
            i64::try_from(i)
                .ok()
                .and_then(|i| usize::try_from(i + delta).ok())
        }
        _ => None,
    }
}

/// Whether the code returns to the interpreter at instruction `i`
/// whatever its operands: one `run_core` leaves at ([`Shape::Leave`]),
/// other than a `foreach` step ([`each_end`]), which the code runs through
/// [`fe_fetch`].
fn leaves(tape: &[Inst], ops: &[Op], i: usize) -> bool {
    tape.get(i)
        .is_none_or(|inst| matches!(inst.op.shape(), Shape::Leave(_)))
        && each_end(tape, ops, i).is_none()
}

/// When instruction `i` is the step of a `foreach` (`Op::FeFetch`, which
/// has no tape instruction of its own): where the loop goes when the
/// iteration is over.
fn each_end(tape: &[Inst], ops: &[Op], i: usize) -> Option<usize> {
    if tape.get(i)?.op != super::TOp::Ext {
        return None;
    }
    match ops.get(i)? {
        Op::FeFetch { end, .. } => Some(*end as usize),
        _ => None,
    }
}

/// The op at `at`, one without a tape instruction, as `run_tape` runs it
/// (`hot::step_ext`). For the step of a `foreach`: the next element of an
/// array iterated by value into its temporaries, and the assignments to
/// the loop's variables that follow when they are plain; returns the op
/// to go on at (after the assignments it made, or the loop's end), or
/// `hot::DECLINED`, nothing changed, for any other iterator. For a static
/// property's read or write: the cache hit, or `hot::DECLINED`.
///
/// # Safety
/// `f` is the current frame and `at` an op of its op array that
/// `hot::step_ext` runs (an `Op::FeFetch`, `FetchStaticProp`,
/// `AssignStaticProp`).
pub(super) unsafe extern "C" fn fe_fetch(f: *mut Frame, at: usize) -> usize {
    // SAFETY: the caller's contract. The op array outlives the op: a hot
    // op neither pops the frame nor replaces its code (`run_tape`).
    let (frame, code) = unsafe { (&mut *f, &*std::rc::Rc::as_ptr(&(*f).code)) };
    crate::exec::hot::step_ext(frame, &code.ops, &code.literals, at)
}

/// The op at `at`, one of the interpreter's core ops, as `run_tape` runs
/// it (`hot::step`): for a typed loop, the assignment of a temporary that
/// holds no number to a variable. Returns the op to go on at, or
/// `hot::DECLINED`, nothing changed.
///
/// # Safety
/// `f` is the current frame and `at` an op of its op array.
pub(super) unsafe extern "C" fn core_step(f: *mut Frame, at: usize) -> usize {
    // SAFETY: the caller's contract; the op array outlives the op, as in
    // `fe_fetch`.
    let (frame, code) = unsafe { (&mut *f, &*std::rc::Rc::as_ptr(&(*f).code)) };
    crate::exec::hot::step(frame, &code.ops, &code.literals, at)
}

/// The comparison at `at` (`==`, `!=`, `===`, `!==`, `<`, `<=`: an
/// `Op::Bin`, or the `CmpJmp` of a branch) for a typed loop, of two values
/// that compare without the VM: any two under `===` and `!==`, and null,
/// bools, ints, floats and strings under the others, but a float with a
/// string (whose comparison may print the float, by the `precision`
/// setting). Its bool goes into the op's temporary, the
/// strings in temporaries it read are released, and the op after it is
/// next (the jump op of a `CmpJmp`, which tests the bool). Returns
/// `hot::DECLINED`, nothing changed, for anything else.
///
/// # Safety
/// As [`core_step`].
pub(super) unsafe extern "C" fn compare_step(f: *mut Frame, at: usize) -> usize {
    use crate::exec::spec::{operand_simple, release_str_tmp_in};
    use crate::opcode::BinKind;
    use php_runtime::ops;
    // SAFETY: the caller's contract; the op array outlives the op, as in
    // `fe_fetch`.
    let (frame, code) = unsafe { (&mut *f, &*std::rc::Rc::as_ptr(&(*f).code)) };
    let Some(
        &(Op::Bin {
            op: kind,
            a: lhs,
            b: rhs,
            dst,
        }
        | Op::CmpJmp {
            op: kind,
            a: lhs,
            b: rhs,
            dst,
            ..
        }),
    ) = code.ops.get(at)
    else {
        return crate::exec::hot::DECLINED;
    };
    if frame.tmps.len() <= dst as usize {
        return crate::exec::hot::DECLINED;
    }
    // The interpreter's own short cut first (it releases the strings it
    // read): same-type null, bools and strings, `===` of different types.
    if let Some(r) = crate::exec::spec::cmp_simple_in(frame, kind, lhs, rhs) {
        frame.tmps[dst as usize] = Value::Bool(r);
        return at + 1;
    }
    let result = {
        let (Some(x), Some(y)) = (operand_simple(frame, lhs), operand_simple(frame, rhs)) else {
            return crate::exec::hot::DECLINED;
        };
        let scalar = |v: &Value| {
            matches!(
                v,
                Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_)
            )
        };
        // (`===` and `!==` of anything need no VM.)
        let identity = matches!(kind, BinKind::Identical | BinKind::NotIdentical);
        if !identity && (!scalar(x) || !scalar(y))
            || matches!(
                (x, y),
                (Value::Float(_), Value::Str(_)) | (Value::Str(_), Value::Float(_))
            )
        {
            return crate::exec::hot::DECLINED;
        }
        let mut ctx = php_runtime::CollectingContext::new();
        let r = match kind {
            BinKind::Identical => ops::is_identical(x, y),
            BinKind::NotIdentical => ops::is_identical(x, y).map(|r| !r),
            BinKind::Equal => ops::is_equal(x, y, &mut ctx),
            BinKind::NotEqual => ops::is_not_equal(x, y, &mut ctx),
            BinKind::Less => ops::is_smaller(x, y, &mut ctx),
            BinKind::LessEqual => ops::is_smaller_or_equal(x, y, &mut ctx),
            _ => return crate::exec::hot::DECLINED,
        };
        match r {
            Ok(r) if ctx.take().is_empty() => r,
            _ => return crate::exec::hot::DECLINED,
        }
    };
    release_str_tmp_in(frame, lhs);
    release_str_tmp_in(frame, rhs);
    if let Some(slot) = frame.tmps.get_mut(dst as usize) {
        *slot = Value::Bool(result);
    }
    at + 1
}

/// A truth test for a typed loop (`!a`, `(bool) a`, `JmpZ`, `JmpNz`,
/// `JmpZEx`, `JmpNzEx`) of a temporary holding a string, which the
/// interpreter's own step declines (its temporaries hold scalars there):
/// the string's truth, the string released. Anything else as that step
/// runs it ([`core_step`]). Returns the op to go on at, or
/// `hot::DECLINED`, nothing changed.
///
/// # Safety
/// As [`core_step`].
pub(super) unsafe extern "C" fn truth_step(f: *mut Frame, at: usize) -> usize {
    use crate::opcode::UnKind;
    // SAFETY: the caller's contract; the op array outlives the op, as in
    // `fe_fetch`.
    let (frame, code) = unsafe { (&mut *f, &*std::rc::Rc::as_ptr(&(*f).code)) };
    let op = code.ops.get(at);
    let Some(
        &(Op::Unary {
            op: UnKind::Not | UnKind::Bool,
            a,
            ..
        }
        | Op::JmpZ(a, _)
        | Op::JmpNz(a, _)
        | Op::JmpZEx { a, .. }
        | Op::JmpNzEx { a, .. }),
    ) = op
    else {
        return crate::exec::hot::DECLINED;
    };
    let Operand::Tmp(t) = a else {
        // SAFETY: the caller's contract.
        return unsafe { core_step(f, at) };
    };
    let Some(Value::Str(s)) = frame.tmps.get(t as usize) else {
        // SAFETY: the caller's contract.
        return unsafe { core_step(f, at) };
    };
    let truth = !(s.is_empty() || s.as_bytes() == b"0");
    // The string released, and its truth where the op leaves it.
    match *op.unwrap_or(&Op::Nop) {
        Op::Unary { op: un, dst, .. } => {
            if frame.tmps.len() <= dst as usize {
                return crate::exec::hot::DECLINED;
            }
            frame.tmps[t as usize] = Value::Null;
            frame.tmps[dst as usize] = Value::Bool(truth != (un == UnKind::Not));
            at + 1
        }
        Op::JmpZ(_, target) | Op::JmpNz(_, target) => {
            frame.tmps[t as usize] = Value::Null;
            if truth == matches!(op, Some(Op::JmpNz(..))) {
                target as usize
            } else {
                at + 1
            }
        }
        Op::JmpZEx { dst, target, .. } | Op::JmpNzEx { dst, target, .. } => {
            if frame.tmps.len() <= dst as usize {
                return crate::exec::hot::DECLINED;
            }
            frame.tmps[t as usize] = Value::Null;
            let when = matches!(op, Some(Op::JmpNzEx { .. }));
            if truth == when {
                frame.tmps[dst as usize] = Value::Bool(when);
                target as usize
            } else {
                at + 1
            }
        }
        _ => crate::exec::hot::DECLINED,
    }
}

/// The start and the end of a `foreach` by value over an array, for a
/// typed loop (a `foreach` inside it): `Op::FeReset` of a variable's
/// (copied), a literal's or a temporary's (taken) array into the iterator,
/// going on at the loop's end when it is empty; `Op::FeFree` of an
/// iterator over an array someone else holds too (so that releasing it
/// frees nothing, no destructor among it). Returns the op to go on at, or
/// `hot::DECLINED`, nothing changed (an object, what is no array, an
/// undefined variable: the VM's).
///
/// # Safety
/// As [`core_step`].
pub(super) unsafe extern "C" fn foreach_step(f: *mut Frame, at: usize) -> usize {
    use crate::exec::hot::DECLINED;
    use crate::vm::ForeachIter;
    // SAFETY: the caller's contract; the op array outlives the op, as in
    // `fe_fetch`.
    let (frame, code) = unsafe { (&mut *f, &*std::rc::Rc::as_ptr(&(*f).code)) };
    match code.ops.get(at) {
        Some(&Op::FeReset {
            subject,
            kind: crate::opcode::ForeachKind::Value,
            iter,
            end,
            ..
        }) => {
            if !matches!(frame.iters.get(iter as usize), Some(None)) {
                return DECLINED;
            }
            let arr = match subject {
                Operand::Cv(c) => {
                    match frame.cvs.get(c as usize).and_then(crate::vm::CvSlot::get) {
                        Some(Value::Array(a)) => a.clone(),
                        Some(Value::Reference(r)) => match r.get() {
                            Value::Array(a) => a,
                            _ => return DECLINED,
                        },
                        _ => return DECLINED,
                    }
                }
                Operand::Lit(l) => match code.literals.get(l as usize) {
                    Some(Value::Array(a)) => a.clone(),
                    _ => return DECLINED,
                },
                Operand::Tmp(t) => match frame.tmps.get_mut(t as usize) {
                    Some(slot @ Value::Array(_)) => match std::mem::take(slot) {
                        Value::Array(a) => a,
                        _ => return DECLINED,
                    },
                    _ => return DECLINED,
                },
                _ => return DECLINED,
            };
            if arr.is_empty() {
                return end as usize;
            }
            frame.iters[iter as usize] = Some(ForeachIter::Array { arr, pos: 0 });
            at + 1
        }
        Some(&Op::FeFree { iter }) => match frame.iters.get(iter as usize) {
            Some(None) => at + 1,
            Some(Some(ForeachIter::Array { arr, .. })) if arr.refcount() > 1 => {
                frame.iters[iter as usize] = None;
                at + 1
            }
            _ => DECLINED,
        },
        _ => DECLINED,
    }
}

/// `A::$p++` and its kin (`Op::IncDec` of a static property by name) for
/// a typed loop, on a hit of the place's cache, as the VM's fast path
/// does it: an untyped property holding an int (that does not overflow)
/// or a float is updated in its cell, the old or new value going to the
/// op's temporary when it has one. Returns the op after it, or
/// `hot::DECLINED`, nothing changed (a typed property, a reference, a
/// miss, anything else: the VM's).
///
/// # Safety
/// As [`core_step`].
pub(super) unsafe extern "C" fn incdec_static_step(f: *mut Frame, at: usize) -> usize {
    use crate::exec::hot::DECLINED;
    // SAFETY: the caller's contract; the op array outlives the op, as in
    // `fe_fetch`.
    let (frame, code) = unsafe { (&mut *f, &*std::rc::Rc::as_ptr(&(*f).code)) };
    let Some(&Op::IncDec { place, kind, dst }) = code.ops.get(at) else {
        return DECLINED;
    };
    match code.places.get(place as usize) {
        Some(p)
            if matches!(p.base, crate::opcode::PlaceBase::StaticProp { .. })
                && p.segs.is_empty() => {}
        _ => return DECLINED,
    }
    if dst.is_some_and(|d| frame.tmps.len() <= d as usize) {
        return DECLINED;
    }
    let Some((cell, false)) = crate::rtcache::static_place_cell_in(frame, place) else {
        return DECLINED;
    };
    let Ok(mut slot) = cell.try_borrow_mut() else {
        return DECLINED;
    };
    let result = match &mut *slot {
        Value::Int(cur) => {
            let old = *cur;
            let new = if kind.is_inc() {
                old.checked_add(1)
            } else {
                old.checked_sub(1)
            };
            let Some(new) = new else {
                return DECLINED;
            };
            *cur = new;
            Value::Int(if kind.is_post() { old } else { new })
        }
        Value::Float(cur) => {
            let old = *cur;
            let new = if kind.is_inc() { old + 1.0 } else { old - 1.0 };
            *cur = new;
            Value::Float(if kind.is_post() { old } else { new })
        }
        _ => return DECLINED,
    };
    drop(slot);
    if let Some(d) = dst {
        frame.tmps[d as usize] = result;
    }
    at + 1
}

/// `$x instanceof Name` (`Op::InstanceOf`) for a typed loop, when the
/// class the name resolved to is in the op array's cache and the value
/// is a variable's (defined) or a temporary's that is no object or array
/// (whose release could run a destructor): its bool goes into the op's
/// temporary (which holds neither either), and the op after it is next.
/// Returns `hot::DECLINED`, nothing changed, for anything else.
///
/// # Safety
/// As [`core_step`].
pub(super) unsafe extern "C" fn instanceof_step(f: *mut Frame, at: usize) -> usize {
    use crate::exec::hot::DECLINED;
    // SAFETY: the caller's contract; the op array outlives the op, as in
    // `fe_fetch`.
    let (frame, code) = unsafe { (&mut *f, &*std::rc::Rc::as_ptr(&(*f).code)) };
    let Some(&Op::InstanceOf {
        a,
        class: crate::opcode::ClassSpec::Named(lit),
        dst,
    }) = code.ops.get(at)
    else {
        return DECLINED;
    };
    let Some(class) = code
        .class_cache
        .borrow()
        .get(lit as usize)
        .and_then(std::rc::Weak::upgrade)
    else {
        return DECLINED;
    };
    let owns = |v: &Value| matches!(v.deref(), Value::Object(_) | Value::Array(_));
    let result = match a {
        Operand::Cv(c) => match frame.cvs.get(c as usize).and_then(crate::vm::CvSlot::get) {
            Some(v) => match v.deref() {
                Value::Object(o) => crate::object::class_of(&o).is_subclass_of(&class),
                _ => false,
            },
            None => return DECLINED,
        },
        Operand::Tmp(t) => match frame.tmps.get(t as usize) {
            Some(v) if !owns(v) => false,
            _ => return DECLINED,
        },
        _ => return DECLINED,
    };
    match frame.tmps.get(dst as usize) {
        Some(v) if !owns(v) => {}
        _ => return DECLINED,
    }
    if let Operand::Tmp(t) = a {
        if let Some(slot) = frame.tmps.get_mut(t as usize) {
            *slot = Value::Null;
        }
    }
    if let Some(slot) = frame.tmps.get_mut(dst as usize) {
        *slot = Value::Bool(result);
    }
    at + 1
}

/// The op at `at` of an array literal (`Op::InitArray`, `Op::ArrayAdd`)
/// for a typed loop, as the VM runs it, when it needs nothing of the VM:
/// an element that is a literal, a temporary (taken) or a defined
/// variable (copied), under an int or string key or the next one.
/// Returns the op after it, or `hot::DECLINED`, nothing changed (an
/// undefined variable, a key that warns or is no int or string, an
/// element the array has no room for: the VM's).
///
/// # Safety
/// As [`core_step`].
pub(super) unsafe extern "C" fn array_step(f: *mut Frame, at: usize) -> usize {
    use crate::exec::hot::DECLINED;
    // SAFETY: the caller's contract; the op array outlives the op, as in
    // `fe_fetch`.
    let (frame, code) = unsafe { (&mut *f, &*std::rc::Rc::as_ptr(&(*f).code)) };
    match code.ops.get(at) {
        Some(&Op::InitArray { dst }) => {
            let Some(slot) = frame.tmps.get_mut(dst as usize) else {
                return DECLINED;
            };
            *slot = Value::Array(php_runtime::ZArray::new());
            at + 1
        }
        Some(&Op::ArrayAdd { arr, key, val }) => {
            if !matches!(frame.tmps.get(arr as usize), Some(Value::Array(_))) {
                return DECLINED;
            }
            // The key, as the VM makes it of a literal's.
            let key = match key {
                None => None,
                Some(k) => {
                    let value = match k {
                        Operand::Lit(l) => code.literals.get(l as usize),
                        Operand::Tmp(t) | Operand::TmpKeep(t) => frame.tmps.get(t as usize),
                        Operand::Cv(c) => {
                            frame.cvs.get(c as usize).and_then(crate::vm::CvSlot::get)
                        }
                        Operand::Unused => None,
                    };
                    let key = match value {
                        Some(v @ (Value::Int(_) | Value::Str(_))) => {
                            let mut diags = Vec::new();
                            match crate::place::key_of(v, &mut diags, crate::exec::KeyCtx::Literal)
                            {
                                Ok(key) if diags.is_empty() => key,
                                _ => return DECLINED,
                            }
                        }
                        _ => return DECLINED,
                    };
                    Some((key, k))
                }
            };
            // The element, as the VM reads it.
            let value = match val {
                Operand::Lit(l) => match code.literals.get(l as usize) {
                    Some(v) => v.clone(),
                    None => return DECLINED,
                },
                Operand::Cv(c) => {
                    match frame.cvs.get(c as usize).and_then(crate::vm::CvSlot::get) {
                        Some(v) => v.deref(),
                        None => return DECLINED,
                    }
                }
                Operand::TmpKeep(t) => match frame.tmps.get(t as usize) {
                    Some(v) => v.deref(),
                    None => return DECLINED,
                },
                Operand::Tmp(t) => match frame.tmps.get_mut(t as usize) {
                    Some(slot) => match std::mem::take(slot) {
                        Value::Reference(r) => r.get(),
                        v => v,
                    },
                    None => return DECLINED,
                },
                Operand::Unused => Value::Null,
            };
            let Some(Value::Array(array)) = frame.tmps.get_mut(arr as usize) else {
                return DECLINED;
            };
            match key {
                None => {
                    if let Err(value) = array.make_mut().push(value) {
                        // (No room: the VM's error. The element goes back.)
                        if let Operand::Tmp(t) = val {
                            if let Some(slot) = frame.tmps.get_mut(t as usize) {
                                *slot = value;
                            }
                        }
                        return DECLINED;
                    }
                }
                Some((key, k)) => {
                    array.make_mut().insert(key, value);
                    // (A temporary key is read, and left null, after it.)
                    if let Operand::Tmp(t) = k {
                        if let Some(slot) = frame.tmps.get_mut(t as usize) {
                            *slot = Value::Null;
                        }
                    }
                }
            }
            at + 1
        }
        _ => DECLINED,
    }
}

/// The value the run-time cache of frame `f` holds for the constant the
/// op at `at` reads (a constant by name, or a class constant of a class
/// named, `self`, `parent` or `static`); `None` on a cache miss.
fn const_at(f: &Frame, at: usize) -> Option<Value> {
    use crate::opcode::ClassSpec;
    match f.code.ops.get(at)? {
        Op::FetchConst {
            name,
            fallback: None,
            ..
        }
        | Op::FetchConstTo {
            name,
            fallback: None,
            ..
        } => crate::rtcache::const_hit_in(f, *name),
        Op::FetchClassConst {
            class,
            name: Operand::Lit(name),
            ..
        }
        | Op::FetchClassConstTo { class, name, .. }
            if !matches!(class, ClassSpec::Dyn(_)) =>
        {
            crate::rtcache::class_const_hit_in(f, class, *name)
        }
        _ => None,
    }
}

/// The user function the call started by the op at `init` of frame `f`'s
/// code calls now, by the caches the interpreter's own fast paths read
/// (`calls::prepare_fcall`, `Vm::static_call_hit`, `calls::prepare_method`): a
/// function by a name that cannot be a namespaced one's; a static method
/// of a class named in the code; a method, by name, of the object a
/// variable or `$this` holds, for that object's class and the calling
/// scope. `None` when the cache has no such entry (the call has not run,
/// or runs through `__call`, or the object is of another class than last
/// time).
fn callee_at(f: &Frame, init: usize) -> Option<std::rc::Rc<crate::func::UserFunction>> {
    use crate::func::flags;
    use crate::opcode::CachedFuncKind;
    let user = |cached: &crate::opcode::CachedFunc| match &cached.func {
        CachedFuncKind::User(weak) => weak.upgrade(),
        CachedFuncKind::Builtin(_) => None,
    };
    match *f.code.ops.get(init)? {
        Op::InitFCall {
            name,
            fallback: None,
            ..
        } => {
            let cache = f.code.fcall_cache.try_borrow().ok()?;
            let cached = cache.get(name as usize)?.as_ref()?;
            let func = user(cached)?;
            (cached.scope.is_none()
                && cached.called.is_none()
                && func.class_name.is_none()
                && func.flags & flags::STATIC == 0)
                .then_some(func)
        }
        Op::InitStaticCall {
            ref class,
            name: Operand::Lit(name),
        } => crate::rtcache::static_call_user_in(f, class, name),
        Op::InitMethodCall {
            obj,
            name: Operand::Lit(lit),
        } => {
            let object = match obj {
                Operand::Cv(cv) => match f.cvs.get(cv as usize)?.get()? {
                    Value::Object(object) => object,
                    _ => return None,
                },
                Operand::Unused => f.this.as_ref()?,
                _ => return None,
            };
            let class = crate::object::class_of(object);
            if class.hooks.as_ref().and_then(|h| h.get_method).is_some() {
                return None;
            }
            let scope = f
                .scope
                .as_ref()
                .map_or(0, |s| std::rc::Rc::as_ptr(s) as usize);
            let cache = f.code.method_cache.try_borrow().ok()?;
            let cached = cache.get(init)?.as_ref()?;
            if cached.class_addr != std::rc::Rc::as_ptr(class) as usize
                || cached.scope_addr != scope
                || cached.name_lit != Some(lit)
            {
                return None;
            }
            user(&cached.func)
        }
        _ => None,
    }
}

/// The number the property read at place `pid` of the method at address
/// `func` reads from `$this`, for the method's call started by the op at
/// `init` of frame `f`'s code: the object that call is made on, and the
/// method's scope as the call's cache entry has it.
///
/// # Safety
/// `func` is the address of a `UserFunction` that is alive (one a typed
/// loop runs inline is kept alive for good, `typed::analyse`).
unsafe fn this_prop(f: &Frame, init: usize, func: usize, pid: u32) -> Option<(Value, u32)> {
    let Op::InitMethodCall {
        obj,
        name: Operand::Lit(_),
    } = *f.code.ops.get(init)?
    else {
        return None;
    };
    let object = match obj {
        Operand::Cv(cv) => match f.cvs.get(cv as usize)?.get()? {
            Value::Object(object) => object,
            _ => return None,
        },
        Operand::Unused => f.this.as_ref()?,
        _ => return None,
    };
    let cache = f.code.method_cache.try_borrow().ok()?;
    let cached = cache.get(init)?.as_ref()?;
    if cached.class_addr != std::rc::Rc::as_ptr(crate::object::class_of(object)) as usize {
        return None;
    }
    let scope = cached
        .method_scope
        .as_ref()
        .map_or(0, |scope| scope.as_ptr() as usize);
    // SAFETY: the caller's contract.
    let code = unsafe { &(*(func as *const crate::func::UserFunction)).code };
    crate::exec::spec::jit_this_prop_num(code, scope, object, pid)
}

/// The object the variable with index `recv` of frame `f` holds, or with
/// `usize::MAX` its `$this`: the receiver of a method's call.
#[inline]
fn receiver_in(f: &Frame, recv: usize) -> Option<&php_runtime::Obj> {
    if recv == usize::MAX {
        return f.this.as_ref();
    }
    match f.cvs.get(recv)?.get()? {
        Value::Object(object) => Some(object),
        _ => None,
    }
}

/// For a typed loop, at a method's call it runs inline: whether the
/// receiver ([`receiver_in`]) is an object of the class at address
/// `class`, the one the loop resolved the method for (kept alive for
/// good, [`pin`]: the address names it).
///
/// # Safety
/// `f` is the current frame.
pub(super) unsafe extern "C" fn class_is(f: *mut Frame, recv: usize, class: usize) -> usize {
    // SAFETY: the caller's contract.
    let f = unsafe { &*f };
    usize::from(receiver_in(f, recv).is_some_and(|object| {
        std::rc::Rc::as_ptr(crate::object::class_of(object)) as usize == class
    }))
}

/// For a typed loop's entry: whether frame `f` runs in the class scope at
/// address `scope` (0: none), the one the loop's method calls were
/// resolved in, and calls are not observed.
///
/// # Safety
/// `f` is the current frame.
pub(super) unsafe extern "C" fn scope_is(f: *mut Frame, scope: usize, _unused: usize) -> usize {
    // SAFETY: the caller's contract.
    let f = unsafe { &*f };
    let now = f
        .scope
        .as_ref()
        .map_or(0, |s| std::rc::Rc::as_ptr(s) as usize);
    usize::from(now == scope && !crate::jit::calls_are_observed())
}

/// For a method a typed loop runs inline: the number in the declared slot
/// `slot` of the receiver ([`receiver_in`]), an object of the class at
/// address `class`; its bits into `out`. Returns its tag (2 an int, 3 a
/// float), or 0, nothing stored, for anything else.
///
/// # Safety
/// `f` is the current frame and `out` is writable.
pub(super) unsafe extern "C" fn this_slot(
    f: *mut Frame,
    recv: usize,
    class: usize,
    slot: usize,
    out: *mut u64,
) -> usize {
    // SAFETY: the caller's contract.
    let f = unsafe { &*f };
    let Some(object) = receiver_in(f, recv) else {
        return 0;
    };
    match crate::exec::spec::jit_slot_num(object, class, slot) {
        #[allow(clippy::cast_sign_loss)]
        Some(Value::Int(x)) => {
            // SAFETY: the caller's contract.
            unsafe { *out = x as u64 };
            2
        }
        Some(Value::Float(x)) => {
            // SAFETY: the caller's contract.
            unsafe { *out = x.to_bits() };
            3
        }
        _ => 0,
    }
}

/// For a method a typed loop runs inline: a write to the declared slot
/// `slot` of the receiver ([`receiver_in`]), an object of the class at
/// address `class`, of the number with bits `bits`. `how` packs the
/// number's tag (bits 0-1: 2 an int, 3 a float), the operation (bits 2-4:
/// `spec::jit_slot_put`'s `op`) and, from bit 32, the builtin types the
/// property takes as they are. Returns 1, or 0 with nothing changed.
///
/// # Safety
/// `f` is the current frame.
pub(super) unsafe extern "C" fn this_put(
    f: *mut Frame,
    recv: usize,
    class: usize,
    slot: usize,
    how: usize,
    bits: u64,
) -> usize {
    // SAFETY: the caller's contract.
    let f = unsafe { &*f };
    let Some(object) = receiver_in(f, recv) else {
        return 0;
    };
    #[allow(clippy::cast_possible_wrap)]
    let rhs = if how & 3 == 2 {
        crate::exec::Num::Int(bits as i64)
    } else {
        crate::exec::Num::Float(f64::from_bits(bits))
    };
    #[allow(clippy::cast_possible_truncation)]
    let accept = (how >> 32) as u32;
    usize::from(crate::exec::spec::jit_slot_put(
        object,
        class,
        slot,
        accept,
        (how >> 2) & 7,
        rhs,
    ))
}

thread_local! {
    /// The addresses of the classes compiled code names ([`pin`]).
    static PINNED: RefCell<std::collections::HashSet<usize>> =
        RefCell::new(std::collections::HashSet::new());
}

/// Keeps the allocation of `class` for good, so that its address names
/// this class and no other for as long as compiled code compares with it
/// (the code is never freed either); returns the address.
fn pin(class: &std::rc::Rc<crate::class::Class>) -> usize {
    let addr = std::rc::Rc::as_ptr(class) as usize;
    if PINNED.with(|p| p.borrow_mut().insert(addr)) {
        std::mem::forget(std::rc::Rc::downgrade(class));
    }
    addr
}

/// For a typed loop: whether the call started by the op at `init` of
/// frame `f`'s code still calls the user function at address `func` (the
/// one the loop runs inline, kept alive since), and calls are not
/// observed. For a function's or a static method's call, at the loop's
/// entry; for a method's, at each call.
///
/// # Safety
/// `f` is the current frame.
pub(super) unsafe extern "C" fn user_is(f: *mut Frame, init: usize, func: usize) -> usize {
    if crate::jit::calls_are_observed() {
        return 0;
    }
    // SAFETY: the caller's contract.
    let f = unsafe { &*f };
    usize::from(callee_at(f, init).is_some_and(|now| std::rc::Rc::as_ptr(&now) as usize == func))
}

/// For a typed loop's entry: whether the constant the op at `at` reads
/// still has the value the loop was compiled with (tag 1 a bool, 2 an int,
/// 3 a float; its bits).
///
/// # Safety
/// `f` is the current frame.
pub(super) unsafe extern "C" fn const_is(f: *mut Frame, at: usize, tag: usize, bits: u64) -> usize {
    // SAFETY: the caller's contract.
    let now = const_at(unsafe { &*f }, at);
    #[allow(clippy::cast_sign_loss)]
    let same = match now {
        Some(Value::Int(i)) => tag == usize::from(TAG_INT) && i as u64 == bits,
        Some(Value::Float(x)) => tag == usize::from(TAG_FLOAT) && x.to_bits() == bits,
        Some(Value::Bool(b)) => tag == usize::from(TAG_BOOL) && u64::from(b) == bits,
        _ => false,
    };
    usize::from(same)
}

/// For a typed loop: the step of a `foreach` over an array of values that
/// are not numbers, with the assignment to the loop's variable that
/// follows it, without the general path of [`fe_fetch`] and the
/// interpreter's assignment after it. The next element of iterator `iter`
/// of frame `f` is copied into the variable's slot `value`, whose old
/// value is released as an assignment releases it (an array or object may
/// become a GC root, and an object's destructor may then be waiting: the
/// caller polls); when `key` is not null, the element's int key goes to
/// that slot, which the caller knows to own nothing. Returns 1 at the end
/// of the iteration, 2 when an element was stored, and 0, nothing
/// changed, for anything else (not an array iterated by value, an element
/// or a variable that is a reference, a key that is not an int):
/// [`fe_fetch`] then runs.
///
/// # Safety
/// `f` is the current frame, which has no symbol table attached; `value`
/// points at a variable's slot of it and `key` (when not null) at another
/// that holds a scalar or nothing.
pub(super) unsafe extern "C" fn each_any(
    f: *mut Frame,
    iter: usize,
    value: *mut Option<Value>,
    key: *mut Value,
) -> usize {
    // SAFETY: the caller's contract.
    let frame = unsafe { &mut *f };
    let Some(Some(crate::vm::ForeachIter::Array { arr, pos })) = frame.iters.get_mut(iter) else {
        return 0;
    };
    let (at, v, k) = if key.is_null() {
        match arr.fe_fetch(*pos) {
            Some((at, v)) => (at, v, None),
            None => return 1,
        }
    } else {
        match arr.fe_fetch_with_key(*pos) {
            Some((at, v, Value::Int(k))) => (at, v, Some(k)),
            Some(_) => return 0,
            None => return 1,
        }
    };
    // SAFETY: the caller's contract; the slot is not borrowed (the frame's
    // variables are only touched through the frame, borrowed here alone,
    // and `arr` is the iterator's own handle on the array).
    let slot = unsafe { &mut *value };
    if matches!(v, Value::Reference(_)) || matches!(slot, Some(Value::Reference(_))) {
        return 0;
    }
    let v = v.clone();
    *pos = at + 1;
    let old = slot.replace(v);
    if let Some(k) = k {
        // SAFETY: the caller's contract: nothing to release in the slot.
        unsafe { std::ptr::write(key, Value::Int(k)) };
    }
    crate::exec::release_slot_value(old);
    2
}

/// For a typed loop: the step of a `foreach` over a packed or hash array
/// of numbers, without the general path of [`fe_fetch`]. The next element
/// of iterator `iter` of frame `f` goes to the slot `value` and, when
/// `key` is not null, its int key to that slot (slots the caller knows to
/// own nothing: the loop's variables or the op's temporaries). Returns 1
/// at the end of the iteration, the value's tag (2 an int, 3 a float) when
/// an element was stored, and 0, nothing changed, for anything else (not
/// an array iterated by value, an element that is not a number, a key that
/// is not an int): [`fe_fetch`] then runs.
///
/// # Safety
/// `f` is the current frame; `value` and `key` (when not null) point at
/// slots of it that hold a scalar or nothing.
pub(super) unsafe extern "C" fn each_num(
    f: *mut Frame,
    iter: usize,
    value: *mut Value,
    key: *mut Value,
) -> usize {
    // SAFETY: the caller's contract.
    let frame = unsafe { &mut *f };
    let Some(Some(crate::vm::ForeachIter::Array { arr, pos })) = frame.iters.get_mut(iter) else {
        return 0;
    };
    let (at, v, k) = if key.is_null() {
        match arr.fe_fetch(*pos) {
            Some((at, v)) => (at, v, None),
            None => return 1,
        }
    } else {
        match arr.fe_fetch_with_key(*pos) {
            Some((at, v, Value::Int(k))) => (at, v, Some(k)),
            Some(_) => return 0,
            None => return 1,
        }
    };
    let (v, tag) = match v {
        Value::Int(x) => (Value::Int(*x), 2),
        Value::Float(x) => (Value::Float(*x), 3),
        _ => return 0,
    };
    *pos = at + 1;
    // SAFETY: the caller's contract: nothing to release in the slots, and
    // a variable's slot holding a number has that number's layout.
    unsafe {
        std::ptr::write(value, v);
        if let Some(k) = k {
            std::ptr::write(key, Value::Int(k));
        }
    }
    tag
}

// ---------------------------------------------------------------------
// The layout the code relies on
// ---------------------------------------------------------------------

/// Tag of `Value::Null`.
const TAG_NULL: u8 = 0;
/// Tag of `Value::Bool`.
const TAG_BOOL: u8 = 1;
/// Tag of `Value::Int`.
const TAG_INT: u8 = 2;
/// Tag of `Value::Float`.
const TAG_FLOAT: u8 = 3;
/// The largest tag of a value that owns nothing (null, bool, int, float).
const TAG_LAST_SCALAR: u8 = TAG_FLOAT;
/// Offset of a `bool` payload in a slot.
const BOOL_AT: i32 = 1;
/// Offset of an int or float payload in a slot.
const PAYLOAD_AT: i32 = 8;
/// Size of a slot (`super::SLOT_SHIFT`).
const SLOT: usize = 1 << super::SLOT_SHIFT;

/// Checks the layout facts in the module's safety notes against the
/// compiler that built this binary. Returns the tag byte of an undefined
/// variable (`None::<Value>`), or `None` when a fact does not hold.
fn probe() -> Option<u8> {
    fn bytes<T>(v: &T) -> (u8, u8, [u8; 8]) {
        assert_eq!(std::mem::size_of::<T>(), SLOT);
        let p = std::ptr::from_ref(v).cast::<u8>();
        // SAFETY: `T` is `Value` or `Option<Value>`, 16 bytes; for the
        // values passed below, byte 0 is the initialised tag (`repr(u8)`,
        // and the niche `Option` uses is in it), bytes 8..16 an
        // initialised int or float payload, and byte 1 is only looked at
        // by the caller when it is a `bool` payload.
        unsafe { (*p, *p.add(1), p.add(8).cast::<[u8; 8]>().read()) }
    }
    const I: i64 = 0x0102_0304_0506_0708;
    const F: f64 = 1.5;
    let int = bytes(&Value::Int(I));
    let float = bytes(&Value::Float(F));
    let some_int = bytes(&Some(Value::Int(I)));
    let some_float = bytes(&Some(Value::Float(F)));
    // Only the tag of these is initialised for sure: read nothing else.
    let tag = |v: &Value| std::ptr::from_ref(v).cast::<u8>();
    // SAFETY: byte 0 of a `repr(u8)` enum is its tag.
    let (null, yes, no) = unsafe {
        (
            *tag(&Value::Null),
            (*tag(&Value::Bool(true)), *tag(&Value::Bool(true)).add(1)),
            (*tag(&Value::Bool(false)), *tag(&Value::Bool(false)).add(1)),
        )
    };
    let none: Option<Value> = None;
    // SAFETY: `Option<Value>` is the size of `Value` (asserted in
    // `super`), so `None` is a tag value no variant has, in byte 0.
    let none = unsafe { *std::ptr::from_ref(&none).cast::<u8>() };
    let ok = int.0 == TAG_INT
        && int.2 == I.to_ne_bytes()
        && float.0 == TAG_FLOAT
        && float.2 == F.to_ne_bytes()
        && some_int.0 == TAG_INT
        && some_int.2 == I.to_ne_bytes()
        && some_float.0 == TAG_FLOAT
        && some_float.2 == F.to_ne_bytes()
        && null == TAG_NULL
        && yes == (TAG_BOOL, 1)
        && no == (TAG_BOOL, 0)
        && none > 8;
    let _ = int.1;
    ok.then_some(none)
}

// ---------------------------------------------------------------------
// The compiler
// ---------------------------------------------------------------------

/// This thread's code generator and the code it has produced.
struct Compiler {
    module: JITModule,
    ctx: Context,
    builder_ctx: FunctionBuilderContext,
    /// Tag of an undefined variable ([`probe`]).
    none_tag: u8,
}

impl Compiler {
    fn new() -> Option<Compiler> {
        let none_tag = probe()?;
        let mut flags = settings::builder();
        flags.set("opt_level", "speed").ok()?;
        // What `JITModule` requires of the code.
        flags.set("use_colocated_libcalls", "false").ok()?;
        flags.set("is_pic", "false").ok()?;
        let verify = if cfg!(debug_assertions) {
            "true"
        } else {
            "false"
        };
        flags.set("enable_verifier", verify).ok()?;
        let isa = cranelift_native::builder()
            .ok()?
            .finish(settings::Flags::new(flags))
            .ok()?;
        if isa.pointer_type() != types::I64 {
            return None;
        }
        let module = JITModule::new(JITBuilder::with_isa(
            isa,
            cranelift_module::default_libcall_names(),
        ));
        let ctx = module.make_context();
        Some(Compiler {
            module,
            ctx,
            builder_ctx: FunctionBuilderContext::new(),
            none_tag,
        })
    }

    /// Compiles the tape of the record `record` (the tape of `of`) and
    /// stores its code in it; `None`, the record unchanged, when the tape
    /// holds something the emitter does not take (an offset out of its
    /// range). Returns the code's size.
    ///
    /// # Safety
    /// `record` is a record [`new_record`] made for a tape of `of`.
    unsafe fn compile_into(&mut self, of: &OpArray, record: *mut usize) -> Option<u64> {
        // SAFETY: the caller's contract.
        let (tape, entries) = unsafe {
            let tape = std::slice::from_raw_parts(
                *record.add(REC_TAPE) as *const Inst,
                *record.add(REC_LEN),
            );
            let entries: Vec<Entry> = (0..*record.add(REC_COUNT))
                .map(|k| {
                    let block = record.add(REC_ENTRIES + HB_WORDS * k);
                    Entry {
                        at: *block.add(HB_HEAD),
                        end: *block.add(HB_END),
                        // (Typed code it already has is called too.)
                        typed: *block.add(HB_TYPED) != 0,
                        native: *block.add(HB_NATIVE) != 0,
                        dormant: *block.add(HB_DEAD) == DORMANT,
                    }
                })
                .collect();
            (tape, entries)
        };
        let (code, funcs, size) = self.compile_code(of, tape, &entries)?;
        // Leaked, as the code is.
        let funcs = Box::leak(funcs.into_boxed_slice());
        // SAFETY: the caller's contract.
        unsafe {
            *record.add(REC_FUNCS) = funcs.as_ptr() as usize;
            *record.add(REC_CODE) = code as usize;
        }
        Some(size)
    }

    /// The signature of [`Code`].
    fn code_signature(&self) -> Signature {
        let mut sig = Signature::new(self.module.isa().default_call_conv());
        // cvs, tmps, lits, frame, entry, record.
        sig.params
            .extend(std::iter::repeat_n(AbiParam::new(types::I64), 6));
        sig.returns.push(AbiParam::new(types::I64));
        sig
    }

    /// Turns the function built in the context into machine code; the
    /// context is cleared either way.
    fn finish(&mut self, built: Option<()>) -> Option<(Code, u64)> {
        let done = built.and_then(|()| {
            let id = self
                .module
                .declare_anonymous_function(&self.ctx.func.signature)
                .ok()?;
            // `PHPRUST_JIT_DUMP`: the machine code, for whoever tunes it.
            let dump = std::env::var_os("PHPRUST_JIT_DUMP").is_some();
            if std::env::var_os("PHPRUST_JIT_CLIF").is_some() {
                let f = &self.ctx.func;
                eprintln!(
                    "jit: clif {} blocks, {} insts",
                    f.layout.blocks().count(),
                    f.layout
                        .blocks()
                        .map(|b| f.layout.block_insts(b).count())
                        .sum::<usize>()
                );
            }
            self.ctx.set_disasm(dump);
            if let Err(error) = self.module.define_function(id, &mut self.ctx) {
                // (Said with the counters: `jit::report_if_asked`.)
                if std::env::var_os("PHPRUST_JIT_STATS").is_some() {
                    eprintln!("jit: not compiled: {error:?}");
                }
                return None;
            }
            let size = self
                .ctx
                .compiled_code()
                .map_or(0, |c| u64::from(c.code_info().total_size));
            if dump {
                if let Some(text) = self.ctx.compiled_code().and_then(|c| c.vcode.as_ref()) {
                    eprintln!("jit: {size} bytes\n{text}");
                }
            }
            self.module.finalize_definitions().ok()?;
            let code = self.module.get_finalized_function(id);
            // SAFETY: `code` is the entry of the function just defined
            // with `Code`'s signature in the platform's C convention; the
            // module (and so the code) lives as long as the thread.
            Some((
                unsafe { std::mem::transmute::<*const u8, Code>(code) },
                size,
            ))
        });
        self.module.clear_context(&mut self.ctx);
        done
    }

    /// The code of `tape` and the functions its record must list.
    fn compile_code(
        &mut self,
        of: &OpArray,
        tape: &[Inst],
        entries: &[Entry],
    ) -> Option<(Code, Vec<usize>, u64)> {
        let sig = self.code_signature();
        self.ctx.func.signature = sig.clone();
        let mut handler_sig = Signature::new(sig.call_conv);
        handler_sig
            .params
            .extend(std::iter::repeat_n(AbiParam::new(types::I64), 2));
        handler_sig.returns.push(AbiParam::new(types::I64));
        let handlers = {
            let mut b = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_ctx);
            let sigs = Sigs {
                handler: b.import_signature(handler_sig),
                code: b.import_signature(sig),
            };
            Emit::run(b, sigs, self.none_tag, of, tape, entries)
        };
        let built = handlers.as_ref().map(|_| ());
        if built.is_none() {
            // The builder was dropped in the middle of a function: its
            // context must not leak into the next one.
            self.builder_ctx = FunctionBuilderContext::new();
        }
        let (code, size) = self.finish(built)?;
        Some((code, handlers?, size))
    }

    /// The code of the typed loop `lp`.
    fn compile_typed(&mut self, lp: &typed::Loop) -> Option<(Code, u64)> {
        self.ctx.func.signature = self.code_signature();
        let built = {
            let b = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_ctx);
            typed::emit(b, lp, self.none_tag)
        };
        if built.is_none() {
            // As in `compile_code`: a builder dropped half way.
            self.builder_ctx = FunctionBuilderContext::new();
            if std::env::var_os("PHPRUST_JIT_STATS").is_some() {
                eprintln!("jit: no code for {lp:?}");
            }
        }
        self.finish(built)
    }
}

/// The signatures the first tier's code calls through.
#[derive(Clone, Copy)]
struct Sigs {
    /// [`Handler`], and the functions with its shape.
    handler: SigRef,
    /// [`Code`]: a typed loop.
    code: SigRef,
}

thread_local! {
    /// This thread's compiler, made when the first tape gets hot; `Err`
    /// when it cannot be made (an unsupported host, or [`probe`] failed).
    static COMPILER: RefCell<Option<Result<Compiler, ()>>> = const { RefCell::new(None) };
}

/// Compiles the tape of the record `record`, a tape of `code`; the record
/// gets the code, or is left as it was when the code generator gives up.
/// The caller keeps the compiler's memory out of the script's
/// `memory_limit` accounting, like the tape's.
///
/// # Safety
/// As [`Compiler::compile_into`].
unsafe fn compile(code: &OpArray, record: *mut usize) {
    let compiled = COMPILER.with(|c| {
        let mut c = c.try_borrow_mut().ok()?;
        let compiler = c
            .get_or_insert_with(|| Compiler::new().ok_or(()))
            .as_mut()
            .ok()?;
        // SAFETY: the caller's contract.
        unsafe { compiler.compile_into(code, record) }
    });
    match compiled {
        Some(size) => crate::jit::record(|s| {
            s.compiled += 1;
            s.code_bytes += size;
        }),
        None => crate::jit::record(|s| s.failed += 1),
    }
}

/// Whether `PHPRUST_JIT_WHY` is set: why loops are not compiled is
/// printed. (`2`: with the types at the instruction the analysis has no
/// plan for.) Read once.
pub(super) fn why() -> Option<u8> {
    static WHY: std::sync::OnceLock<Option<u8>> = std::sync::OnceLock::new();
    *WHY.get_or_init(|| std::env::var_os("PHPRUST_JIT_WHY").map(|v| if v == "2" { 2 } else { 1 }))
}

/// What [`compile_typed`] made of a loop.
enum Made {
    /// Its code, and the instructions it may be entered again at.
    Code(Code, Vec<usize>),
    /// Nothing yet: it is to go this many times the hot count more rounds
    /// first ([`typed::Loop::wait`]).
    Wait(usize),
}

/// Compiles the loop of `tape` that starts at `head` for the types
/// `snapshot` reports, when the second tier can (and, unless `waited`,
/// the loop need not go on longer first).
fn compile_typed(
    code: &OpArray,
    tape: &[Inst],
    head: usize,
    seen: &typed::Seen<'_>,
    waited: bool,
) -> Option<Made> {
    let lp = typed::analyse(tape, head, code, seen)?;
    let wait = lp.wait();
    if !waited && wait > 0 {
        return Some(Made::Wait(wait));
    }
    let compiled = COMPILER.with(|c| {
        let mut c = c.try_borrow_mut().ok()?;
        c.get_or_insert_with(|| Compiler::new().ok_or(()))
            .as_mut()
            .ok()?
            .compile_typed(&lp)
    });
    let Some((code, size)) = compiled else {
        crate::jit::record(|s| s.failed += 1);
        return None;
    };
    crate::jit::record(|s| {
        s.typed_loops += 1;
        s.code_bytes += size;
    });
    Some(Made::Code(code, lp.resume_points()))
}

// ---------------------------------------------------------------------
// The emitter
// ---------------------------------------------------------------------

/// A numeric operand as the emitter sees it.
#[derive(Clone, Copy)]
enum Src {
    /// A slot at an offset from a base: its type is tested at run time.
    Slot(V, i32),
    /// An int known now (a literal).
    Int(i64),
    /// A float known now (a literal).
    Float(f64),
    /// A literal that is not a number: the instruction always declines.
    Never,
}

/// A number whose type the emitted code has established.
#[derive(Clone, Copy)]
enum Typed {
    Int(V),
    Float(V),
}

/// Loads and stores of slots: never trapping, aligned.
fn mem() -> MemFlags {
    MemFlags::trusted()
}

/// `x` as an operand offset the emitter can add a payload offset to.
fn off(x: u32) -> Option<i32> {
    i32::try_from(x).ok().filter(|o| *o <= i32::MAX - 16)
}

/// The condition under which two ints take a jump with outcome mask `m`
/// (`super::int_test`).
fn int_cc(m: u8) -> IntCC {
    match m {
        1 => IntCC::SignedLessThan,
        3 => IntCC::SignedLessThanOrEqual,
        4 => IntCC::SignedGreaterThan,
        6 => IntCC::SignedGreaterThanOrEqual,
        2 => IntCC::Equal,
        _ => IntCC::NotEqual,
    }
}

/// How two floats decide a jump with outcome mask `mask` (`hot::jump_of`
/// over `spec::num_outcome`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FloatTest {
    /// The mask decides alone.
    Always(bool),
    /// The jump is taken when the condition holds; with `negate`, when it
    /// does not.
    Cc { cc: FloatCC, negate: bool },
    /// The jump is taken when `x < y || x > y`; with `negate`, when not.
    Apart { negate: bool },
}

/// [`FloatTest`] of a mask. Only the conditions every Cranelift backend
/// lowers are used (aarch64 has no `UnorderedOrLessThan` and its like): a
/// mask with `UNORDERED` and some ordered outcome is the negation of the
/// complementary ordered condition.
fn float_test(mask: u8) -> FloatTest {
    use cmp_outcome::{EQUAL, GREATER, LESS, UNORDERED};
    const ORDER: u8 = LESS | EQUAL | GREATER | UNORDERED;
    const _: () = assert!(ORDER == 15 && LESS == 1 && EQUAL == 2 && GREATER == 4);
    let plain = |cc| FloatTest::Cc { cc, negate: false };
    let not = |cc| FloatTest::Cc { cc, negate: true };
    match mask & ORDER {
        0 => FloatTest::Always(false),
        1 => plain(FloatCC::LessThan),
        2 => plain(FloatCC::Equal),
        3 => plain(FloatCC::LessThanOrEqual),
        4 => plain(FloatCC::GreaterThan),
        5 => FloatTest::Apart { negate: false },
        6 => plain(FloatCC::GreaterThanOrEqual),
        7 => plain(FloatCC::Ordered),
        8 => plain(FloatCC::Unordered),
        9 => not(FloatCC::GreaterThanOrEqual),
        10 => FloatTest::Apart { negate: true },
        11 => not(FloatCC::GreaterThan),
        12 => not(FloatCC::LessThanOrEqual),
        13 => plain(FloatCC::NotEqual),
        14 => not(FloatCC::LessThan),
        _ => FloatTest::Always(true),
    }
}

/// A block an instruction's code branches to, filled once that code is
/// emitted (a block cannot be left half-built for another).
#[derive(Clone, Copy)]
enum Pending {
    /// Returns with `WHY_STEP` at the instruction.
    Decline(usize),
    /// Goes on at `to`, after a poll asked by `at` when `poll`.
    Land { at: usize, to: usize, poll: bool },
}

/// Builds the function of one tape.
struct Emit<'a, 'b> {
    b: FunctionBuilder<'b>,
    tape: &'a [Inst],
    lits: &'a [Value],
    ops: &'a [Op],
    /// The function's parameters.
    cvs: V,
    tmps: V,
    litp: V,
    frame: V,
    record: V,
    /// The addresses of the functions called so far: the code's function
    /// table (`REC_FUNCS`).
    handlers: Vec<usize>,
    /// The block each instruction starts.
    blocks: Vec<Block>,
    sigs: Sigs,
    none_tag: u8,
    /// Blocks the current instruction branches to, not filled yet.
    pending: Vec<(Block, Pending)>,
    /// Whether each instruction is in a loop the code is entered for (all
    /// of them, when it is entered anywhere). The others are not compiled:
    /// the code returns to the interpreter when it gets to one, once per
    /// run of a loop, which is cheaper than compiling the rest of the
    /// function.
    inside: Vec<bool>,
}

impl<'a, 'b> Emit<'a, 'b> {
    /// Emits the whole function into `b`; `None` when an instruction
    /// cannot be emitted (the function is then abandoned).
    fn run(
        mut b: FunctionBuilder<'b>,
        sigs: Sigs,
        none_tag: u8,
        of: &'a OpArray,
        tape: &'a [Inst],
        entries: &[Entry],
    ) -> Option<Vec<usize>> {
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let params = b.block_params(entry).to_vec();
        let &[cvs, tmps, litp, frame, entry_index, record] = params.as_slice() else {
            return None;
        };
        let blocks: Vec<Block> = tape.iter().map(|_| b.create_block()).collect();
        // Where `run_core` hands over (`install`).
        let mut switch = Switch::new();
        for entry in entries {
            switch.set_entry(entry.at as u128, *blocks.get(entry.at)?);
        }
        let stray = b.create_block();
        b.set_cold_block(stray);
        switch.emit(&mut b, entry_index, stray);
        let mut e = Emit {
            b,
            tape,
            lits: &of.literals,
            ops: &of.ops,
            cvs,
            tmps,
            litp,
            frame,
            record,
            handlers: Vec::new(),
            blocks,
            sigs,
            none_tag,
            pending: Vec::new(),
            inside: (0..tape.len())
                .map(|i| {
                    entries
                        .iter()
                        .any(|e| e.end == 0 || (e.at <= i && i < e.end))
                })
                .collect(),
        };
        // An entry index outside the tape (there is none): the sentinel.
        e.b.switch_to_block(stray);
        e.leave(tape.len().checked_sub(1)?, super::WHY_EXIT);
        for i in 0..tape.len() {
            e.b.switch_to_block(e.blocks[i]);
            if !e.inside[i] && !leaves(tape, e.ops, i) {
                // Past the loops: the interpreter's, from this op on.
                e.leave(i, WHY_STEP);
                continue;
            }
            if let Some(k) = entries.iter().position(|e| e.at == i && e.typed) {
                e.typed_loop(i, REC_ENTRIES + HB_WORDS * k)?;
            }
            e.inst(i)?;
            while let Some((block, what)) = e.pending.pop() {
                e.b.switch_to_block(block);
                match what {
                    Pending::Decline(i) => e.leave(i, WHY_STEP),
                    Pending::Land { at, to, poll: true } => e.goto_poll(at, to)?,
                    Pending::Land { to, .. } => e.goto(to)?,
                }
            }
        }
        e.b.seal_all_blocks();
        e.b.finalize();
        Some(e.handlers)
    }

    /// A word of the record that never changes (an address).
    fn record_word(&mut self, i: usize) -> Option<V> {
        let flags = mem().with_readonly();
        Some(
            self.b
                .ins()
                .load(types::I64, flags, self.record, rec_at(i)?),
        )
    }

    // ---- ways out of an instruction ----

    /// Returns to the interpreter: stopped at instruction `i` for `why`.
    fn leave(&mut self, i: usize, why: u8) {
        let packed = (i << WHY_BITS) | usize::from(why);
        #[allow(clippy::cast_possible_wrap)]
        let r = self.b.ins().iconst(types::I64, packed as i64);
        self.b.ins().return_(&[r]);
    }

    /// A cold block that returns with `WHY_STEP` at instruction `i`: where
    /// the instruction's tests go when it declines.
    fn decline_block(&mut self, i: usize) -> Block {
        let d = self.b.create_block();
        self.b.set_cold_block(d);
        self.pending.push((d, Pending::Decline(i)));
        d
    }

    /// The address `addr` of a function, read from the record.
    fn callee(&mut self, addr: usize) -> Option<V> {
        let index = if let Some(k) = self.handlers.iter().position(|h| *h == addr) {
            k
        } else {
            self.handlers.push(addr);
            self.handlers.len() - 1
        };
        let funcs = self.record_word(REC_FUNCS)?;
        Some(
            self.b
                .ins()
                .load(types::I64, mem().with_readonly(), funcs, rec_at(index)?),
        )
    }

    /// Emits, at the head of a loop (instruction `i`, whose entry block is
    /// at record word `block`), the call of the loop's typed code
    /// ([`typed`]) when there is some, compiling it first when the loop was
    /// not looked at yet, and what follows from its result. Leaves the
    /// builder in the block the instruction itself goes into: where the
    /// first tier runs the loop when there is no typed code, or when its
    /// types did not hold.
    fn typed_loop(&mut self, i: usize, block: usize) -> Option<()> {
        let [body, some, untried, call, back, generic, other, polled, rest, miss, failed, stray] =
            std::array::from_fn(|_| self.b.create_block());
        for cold in [untried, other, polled, rest, miss, failed, stray] {
            self.b.set_cold_block(cold);
        }
        self.b.append_block_param(call, types::I64);
        // Not read-only: `specialise` and `not_typed` write it.
        let state = self
            .b
            .ins()
            .load(types::I64, mem(), self.record, rec_at(block + HB_TYPED)?);
        self.b.ins().brif(state, some, &[], body, &[]);
        self.b.switch_to_block(some);
        #[allow(clippy::cast_possible_wrap)]
        let first = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, state, TYPED_UNTRIED as i64);
        self.b
            .ins()
            .brif(first, untried, &[], call, &[state.into()]);
        // The first time here: compiled for the types the frame has now.
        self.b.switch_to_block(untried);
        let compiled = self.call_block_fn(specialise as BlockFn as usize, block)?;
        self.b
            .ins()
            .brif(compiled, call, &[compiled.into()], body, &[]);
        self.b.switch_to_block(call);
        let code = *self.b.block_params(call).first()?;
        // (Entered at the loop's head.)
        let head = self.b.ins().iconst(types::I64, i64::try_from(i).ok()?);
        let args = [
            self.cvs,
            self.tmps,
            self.litp,
            self.frame,
            head,
            self.record,
        ];
        let made = self.b.ins().call_indirect(self.sigs.code, code, &args);
        let r = *self.b.inst_results(made).first()?;
        self.b.ins().jump(back, &[]);
        self.b.switch_to_block(back);
        let why = self.b.ins().band_imm(r, (1 << WHY_BITS) - 1);
        let left = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, why, i64::from(typed::WHY_GENERIC));
        self.b.ins().brif(left, generic, &[], other, &[]);
        // The loop was left (or an int overflowed): on at the instruction
        // it names. Its own head means this round in the first tier.
        self.b.switch_to_block(generic);
        let index = self.b.ins().ushr_imm(r, i64::from(WHY_BITS));
        let mut switch = Switch::new();
        for (to, target) in self.blocks.iter().enumerate() {
            // (Anywhere else the interpreter goes on.)
            if self.inside[to] {
                switch.set_entry(to as u128, if to == i { body } else { *target });
            }
        }
        switch.emit(&mut self.b, index, stray);
        self.b.switch_to_block(stray);
        self.leave_at(index, WHY_STEP);
        self.b.switch_to_block(other);
        let poll = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, why, i64::from(WHY_POLL));
        self.b.ins().brif(poll, polled, &[], rest, &[]);
        // A safe point is due: the typed code's result is the code's.
        self.b.switch_to_block(polled);
        self.b.ins().return_(&[r]);
        // A short cut missed at the instruction named: counted, and on
        // from there as when the loop is left.
        self.b.switch_to_block(rest);
        let missing = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, why, i64::from(typed::WHY_MISS));
        self.b.ins().brif(missing, miss, &[], failed, &[]);
        self.b.switch_to_block(miss);
        let at = self.b.ins().ushr_imm(r, i64::from(WHY_BITS));
        self.b
            .ins()
            .store(mem(), at, self.record, rec_at(block + HB_MISS_AT)?);
        self.call_block_fn(missed as BlockFn as usize, block)?;
        self.b.ins().jump(generic, &[]);
        // The types did not hold: this round in the first tier.
        self.b.switch_to_block(failed);
        self.call_block_fn(not_typed as BlockFn as usize, block)?;
        self.b.ins().jump(body, &[]);
        self.b.switch_to_block(body);
        Some(())
    }

    /// Calls the function at `addr` with the frame and the address of the
    /// entry block at record word `block`; its result.
    fn call_block_fn(&mut self, addr: usize, block: usize) -> Option<V> {
        let callee = self.callee(addr)?;
        let block = self
            .b
            .ins()
            .iadd_imm(self.record, i64::from(rec_at(block)?));
        let call = self
            .b
            .ins()
            .call_indirect(self.sigs.handler, callee, &[self.frame, block]);
        self.b.inst_results(call).first().copied()
    }

    /// Emits the step of a `foreach` (instruction `i`, whose loop ends at
    /// `end`): the call of [`fe_fetch`], the poll `run_tape` makes after
    /// the op, and the jump to where it says. An iterator it does not
    /// take is the interpreter's.
    fn each(&mut self, i: usize, end: usize) -> Option<()> {
        let callee = self.callee(fe_fetch as Step as usize)?;
        let at = self.b.ins().iconst(types::I64, i64::try_from(i).ok()?);
        let call = self
            .b
            .ins()
            .call_indirect(self.sigs.handler, callee, &[self.frame, at]);
        let next = *self.b.inst_results(call).first()?;
        let [declined, done, poll, go, other] = std::array::from_fn(|_| self.b.create_block());
        for cold in [declined, poll, other] {
            self.b.set_cold_block(cold);
        }
        // `hot::DECLINED` is all ones.
        let refused = self.b.ins().icmp_imm(IntCC::Equal, next, -1);
        self.b.ins().brif(refused, declined, &[], done, &[]);
        self.b.switch_to_block(declined);
        self.leave(i, super::WHY_EXT);
        self.b.switch_to_block(done);
        let flag = self.record_word(REC_FLAG)?;
        let due = self
            .b
            .ins()
            .load(types::I8, MemFlags::new().with_notrap(), flag, 0);
        self.b.ins().brif(due, poll, &[], go, &[]);
        self.b.switch_to_block(poll);
        self.b.ins().store(mem(), at, self.record, rec_at(REC_AT)?);
        self.leave_at(next, WHY_POLL);
        // After the assignments it made (up to two), or at the loop's end.
        self.b.switch_to_block(go);
        let mut switch = Switch::new();
        let mut successors = vec![end, i + 1, i + 2, i + 3];
        successors.sort_unstable();
        successors.dedup();
        for to in successors {
            if let Some(block) = self.block(to) {
                switch.set_entry(to as u128, block);
            }
        }
        switch.emit(&mut self.b, next, other);
        self.b.switch_to_block(other);
        self.leave_at(next, WHY_STEP);
        Some(())
    }

    /// Returns to the interpreter: stopped for `why` at the instruction
    /// whose index is the value `index`.
    fn leave_at(&mut self, index: V, why: u8) {
        let shifted = self.b.ins().ishl_imm(index, i64::from(WHY_BITS));
        let packed = self.b.ins().bor_imm(shifted, i64::from(why));
        self.b.ins().return_(&[packed]);
    }

    /// The block of instruction `to`.
    fn block(&self, to: usize) -> Option<Block> {
        self.blocks.get(to).copied()
    }

    /// Goes on at instruction `to`.
    fn goto(&mut self, to: usize) -> Option<()> {
        let block = self.block(to)?;
        self.b.ins().jump(block, &[]);
        Some(())
    }

    /// Goes on at instruction `to` after a poll for a safe point asked by
    /// instruction `at` (`take_jump!`): when one is due, returns with
    /// `WHY_POLL`.
    fn goto_poll(&mut self, at: usize, to: usize) -> Option<()> {
        let block = self.block(to)?;
        let flag = self.record_word(REC_FLAG)?;
        // Set from other threads and signal handlers: a plain load, which
        // the optimiser may not move or merge.
        let due = self
            .b
            .ins()
            .load(types::I8, MemFlags::new().with_notrap(), flag, 0);
        let poll = self.b.create_block();
        self.b.set_cold_block(poll);
        self.b.ins().brif(due, poll, &[], block, &[]);
        self.b.switch_to_block(poll);
        #[allow(clippy::cast_possible_wrap)]
        let at = self.b.ins().iconst(types::I64, at as i64);
        self.b.ins().store(mem(), at, self.record, rec_at(REC_AT)?);
        self.leave(to, WHY_POLL);
        Some(())
    }

    /// A new block that goes on at `to`, polling first when `poll`.
    fn landing(&mut self, at: usize, to: usize, poll: bool) -> Option<Block> {
        self.block(to)?;
        let l = self.b.create_block();
        self.pending.push((l, Pending::Land { at, to, poll }));
        Some(l)
    }

    /// The instruction the jump of instruction `i` goes to.
    fn target(&self, i: usize) -> Option<usize> {
        #[allow(clippy::cast_possible_wrap)]
        let delta = isize::try_from(self.tape[i].c as i32).ok()?;
        let size = isize::try_from(std::mem::size_of::<Inst>()).ok()?;
        if delta % size != 0 {
            return None;
        }
        let to = usize::try_from(isize::try_from(i).ok()? + delta / size).ok()?;
        (to < self.tape.len()).then_some(to)
    }

    // ---- slots ----

    fn tag(&mut self, base: V, at: i32) -> V {
        self.b.ins().load(types::I8, mem(), base, at)
    }

    fn store_tag(&mut self, base: V, at: i32, tag: u8) {
        let t = self.b.ins().iconst(types::I8, i64::from(tag));
        self.b.ins().store(mem(), t, base, at);
    }

    /// Stores a number over a slot that owns nothing.
    fn store_num(&mut self, base: V, at: i32, n: Typed) {
        let (tag, v) = match n {
            Typed::Int(v) => (TAG_INT, v),
            Typed::Float(v) => (TAG_FLOAT, v),
        };
        self.store_tag(base, at, tag);
        self.b.ins().store(mem(), v, base, at + PAYLOAD_AT);
    }

    /// Copies the 16 bytes of a scalar slot.
    fn copy_slot(&mut self, from: (V, i32), to: (V, i32)) {
        let lo = self.b.ins().load(types::I64, mem(), from.0, from.1);
        let hi = self
            .b
            .ins()
            .load(types::I64, mem(), from.0, from.1 + PAYLOAD_AT);
        self.b.ins().store(mem(), lo, to.0, to.1);
        self.b.ins().store(mem(), hi, to.0, to.1 + PAYLOAD_AT);
    }

    /// Goes on in a new block when `cond` holds, to `decline` otherwise.
    fn require(&mut self, cond: V, decline: Block) {
        let next = self.b.create_block();
        self.b.ins().brif(cond, next, &[], decline, &[]);
        self.b.switch_to_block(next);
    }

    /// Declines unless the slot holds a value that owns nothing
    /// (`super::is_scalar`), or, with `or_undef`, is an undefined variable.
    fn require_plain(&mut self, base: V, at: i32, or_undef: bool, decline: Block) {
        let t = self.tag(base, at);
        let mut ok = self.b.ins().icmp_imm(
            IntCC::UnsignedLessThanOrEqual,
            t,
            i64::from(TAG_LAST_SCALAR),
        );
        if or_undef {
            let undef = self
                .b
                .ins()
                .icmp_imm(IntCC::Equal, t, i64::from(self.none_tag));
            ok = self.b.ins().bor(ok, undef);
        }
        self.require(ok, decline);
    }

    // ---- numbers ----

    /// The operand of kind `kind` in operand field `x` (`super::num`).
    fn src(&self, kind: u8, x: u32) -> Option<Src> {
        Some(match kind {
            KC => Src::Slot(self.cvs, off(x)?),
            KT => Src::Slot(self.tmps, off(x)?),
            #[allow(clippy::cast_possible_wrap)]
            KI => Src::Int(i64::from(x as i32)),
            // A literal: its value is known now.
            _ => match self.lits.get(x as usize / SLOT)? {
                Value::Int(i) => Src::Int(*i),
                Value::Float(f) => Src::Float(*f),
                _ => Src::Never,
            },
        })
    }

    /// Emits the type test of `s` and, once for each type it can have, what
    /// `then` emits for a number of that type (which must end its block).
    /// Anything that is not a number goes to `decline`.
    fn with_num(&mut self, s: Src, decline: Block, then: &mut dyn FnMut(&mut Self, Typed)) {
        match s {
            Src::Int(i) => {
                let v = self.b.ins().iconst(types::I64, i);
                then(self, Typed::Int(v));
            }
            Src::Float(f) => {
                let v = self.b.ins().f64const(f);
                then(self, Typed::Float(v));
            }
            Src::Never => {
                self.b.ins().jump(decline, &[]);
            }
            Src::Slot(base, at) => {
                let t = self.tag(base, at);
                let int = self.b.create_block();
                let other = self.b.create_block();
                let float = self.b.create_block();
                let is_int = self.b.ins().icmp_imm(IntCC::Equal, t, i64::from(TAG_INT));
                self.b.ins().brif(is_int, int, &[], other, &[]);
                self.b.switch_to_block(other);
                let is_float = self.b.ins().icmp_imm(IntCC::Equal, t, i64::from(TAG_FLOAT));
                self.b.ins().brif(is_float, float, &[], decline, &[]);
                self.b.switch_to_block(int);
                let v = self.b.ins().load(types::I64, mem(), base, at + PAYLOAD_AT);
                then(self, Typed::Int(v));
                self.b.switch_to_block(float);
                let v = self.b.ins().load(types::F64, mem(), base, at + PAYLOAD_AT);
                then(self, Typed::Float(v));
            }
        }
    }

    /// Emits the type tests of two operands. Returns the block reached with
    /// two ints and the block reached with two floats, each with the two
    /// numbers as its parameters, for the caller to fill. An int with a
    /// float goes to `mixed` when given, else to the float block with the
    /// int widened (php-src's `(double)`).
    fn num2(&mut self, a: Src, b: Src, decline: Block, mixed: Option<Block>) -> (Block, Block) {
        let ints = self.b.create_block();
        self.b.append_block_param(ints, types::I64);
        self.b.append_block_param(ints, types::I64);
        let floats = self.b.create_block();
        self.b.append_block_param(floats, types::F64);
        self.b.append_block_param(floats, types::F64);
        self.with_num(a, decline, &mut |e, x| {
            e.with_num(b, decline, &mut |e, y| match (x, y) {
                (Typed::Int(x), Typed::Int(y)) => {
                    e.b.ins().jump(ints, &[x.into(), y.into()]);
                }
                (Typed::Float(x), Typed::Float(y)) => {
                    e.b.ins().jump(floats, &[x.into(), y.into()]);
                }
                (x, y) => {
                    if let Some(mixed) = mixed {
                        e.b.ins().jump(mixed, &[]);
                    } else {
                        let x = e.widen(x);
                        let y = e.widen(y);
                        e.b.ins().jump(floats, &[x.into(), y.into()]);
                    }
                }
            });
        });
        (ints, floats)
    }

    /// `n` as a float.
    fn widen(&mut self, n: Typed) -> V {
        match n {
            Typed::Int(v) => self.b.ins().fcvt_from_sint(types::F64, v),
            Typed::Float(v) => v,
        }
    }

    /// `x op y` on ints; an overflow goes to `decline` (the generic op
    /// makes the float).
    fn int_arith(&mut self, op: Arith, x: V, y: V, decline: Block) -> V {
        let (r, overflow) = match op {
            Arith::Add => self.b.ins().sadd_overflow(x, y),
            Arith::Sub => self.b.ins().ssub_overflow(x, y),
            Arith::Mul => self.b.ins().smul_overflow(x, y),
        };
        let cont = self.b.create_block();
        self.b.ins().brif(overflow, decline, &[], cont, &[]);
        self.b.switch_to_block(cont);
        r
    }

    /// `x op y` on floats.
    fn float_arith(&mut self, op: Arith, x: V, y: V) -> V {
        match op {
            Arith::Add => self.b.ins().fadd(x, y),
            Arith::Sub => self.b.ins().fsub(x, y),
            Arith::Mul => self.b.ins().fmul(x, y),
        }
    }

    /// The two parameters of a block [`Self::num2`] made, having switched
    /// to it.
    fn enter2(&mut self, block: Block) -> Option<(V, V)> {
        self.b.switch_to_block(block);
        match *self.b.block_params(block) {
            [x, y] => Some((x, y)),
            _ => None,
        }
    }

    // ---- instructions ----

    /// Emits instruction `i` into the current block (its own).
    #[allow(clippy::too_many_lines, clippy::many_single_char_names)]
    fn inst(&mut self, i: usize) -> Option<()> {
        if let Some(end) = each_end(self.tape, self.ops, i) {
            return self.each(i, end);
        }
        let inst = self.tape[i];
        match inst.op.shape() {
            Shape::Leave(why) => self.leave(i, why),
            Shape::Jmp => {
                let to = self.target(i)?;
                self.goto_poll(i, to)?;
            }
            Shape::JmpBoolT(when) => {
                let at = off(inst.a)?;
                let decline = self.decline_block(i);
                let taken = {
                    let to = self.target(i)?;
                    self.landing(i, to, true)?
                };
                let fall = self.block(i + 1)?;
                let t = self.tag(self.tmps, at);
                let is_bool = self.b.ins().icmp_imm(IntCC::Equal, t, i64::from(TAG_BOOL));
                self.require(is_bool, decline);
                let v = self.b.ins().load(types::I8, mem(), self.tmps, at + BOOL_AT);
                // Consumed (`hot::take_bool`).
                self.store_tag(self.tmps, at, TAG_NULL);
                if when {
                    self.b.ins().brif(v, taken, &[], fall, &[]);
                } else {
                    self.b.ins().brif(v, fall, &[], taken, &[]);
                }
            }
            Shape::IncDecC(inc) => {
                let at = off(inst.a)?;
                let decline = self.decline_block(i);
                let next = self.block(i + 1)?;
                let cvs = self.cvs;
                self.with_num(Src::Slot(cvs, at), decline, &mut |e, n| {
                    let r = e.step(n, inc, decline);
                    e.store_payload(cvs, at, r);
                    e.b.ins().jump(next, &[]);
                });
            }
            Shape::IncDecT { inc, post } => {
                let at = off(inst.a)?;
                let dst = off(inst.c)?;
                let decline = self.decline_block(i);
                let next = self.block(i + 1)?;
                let (cvs, tmps) = (self.cvs, self.tmps);
                self.require_plain(tmps, dst, false, decline);
                self.with_num(Src::Slot(cvs, at), decline, &mut |e, n| {
                    let r = e.step(n, inc, decline);
                    e.store_payload(cvs, at, r);
                    e.store_num(tmps, dst, if post { n } else { r });
                    e.b.ins().jump(next, &[]);
                });
            }
            Shape::AssignC => {
                let (dst, src) = (off(inst.a)?, off(inst.b)?);
                let decline = self.decline_block(i);
                self.require_plain(self.cvs, src, false, decline);
                self.require_plain(self.cvs, dst, true, decline);
                self.copy_slot((self.cvs, src), (self.cvs, dst));
                self.goto(i + 1)?;
            }
            Shape::AssignT => {
                let (dst, src) = (off(inst.a)?, off(inst.b)?);
                let decline = self.decline_block(i);
                self.require_plain(self.tmps, src, false, decline);
                self.require_plain(self.cvs, dst, true, decline);
                self.copy_slot((self.tmps, src), (self.cvs, dst));
                // The temporary is consumed.
                self.store_tag(self.tmps, src, TAG_NULL);
                self.goto(i + 1)?;
            }
            Shape::AssignI => {
                let dst = off(inst.a)?;
                let decline = self.decline_block(i);
                self.require_plain(self.cvs, dst, true, decline);
                #[allow(clippy::cast_possible_wrap)]
                let v = self.b.ins().iconst(types::I64, i64::from(inst.b as i32));
                self.store_num(self.cvs, dst, Typed::Int(v));
                self.goto(i + 1)?;
            }
            Shape::AssignL => {
                let (dst, src) = (off(inst.a)?, off(inst.b)?);
                // `lower` emits this for scalar literals only.
                if !super::is_scalar(self.lits.get(inst.b as usize / SLOT)?) {
                    return None;
                }
                let decline = self.decline_block(i);
                self.require_plain(self.cvs, dst, true, decline);
                self.copy_slot((self.litp, src), (self.cvs, dst));
                self.goto(i + 1)?;
            }
            Shape::Cmp { ka, kb, m } => {
                let (a, b) = (self.src(ka, inst.a)?, self.src(kb, inst.b)?);
                let decline = self.decline_block(i);
                let taken = {
                    let to = self.target(i)?;
                    self.landing(i, to, true)?
                };
                // Past the jump op the comparison decided.
                let fall = self.block(i + 2)?;
                // An int and a float under `===` / `!==` are not compared.
                let mixed = if inst.k & cmp_outcome::IDENTITY != 0 {
                    Some(if inst.k & cmp_outcome::MISMATCH != 0 {
                        taken
                    } else {
                        fall
                    })
                } else {
                    None
                };
                let (ints, floats) = self.num2(a, b, decline, mixed);
                let (x, y) = self.enter2(ints)?;
                let c = self.b.ins().icmp(int_cc(m), x, y);
                self.b.ins().brif(c, taken, &[], fall, &[]);
                let (x, y) = self.enter2(floats)?;
                let (c, negate) = match float_test(inst.k) {
                    FloatTest::Always(taken_always) => {
                        let to = if taken_always { taken } else { fall };
                        self.b.ins().jump(to, &[]);
                        return Some(());
                    }
                    FloatTest::Cc { cc, negate } => (self.b.ins().fcmp(cc, x, y), negate),
                    FloatTest::Apart { negate } => {
                        let lt = self.b.ins().fcmp(FloatCC::LessThan, x, y);
                        let gt = self.b.ins().fcmp(FloatCC::GreaterThan, x, y);
                        (self.b.ins().bor(lt, gt), negate)
                    }
                };
                if negate {
                    self.b.ins().brif(c, fall, &[], taken, &[]);
                } else {
                    self.b.ins().brif(c, taken, &[], fall, &[]);
                }
            }
            Shape::IncCmp { inc, kb, m } => {
                let at = off(inst.a)?;
                let bound = self.src(kb, inst.b)?;
                let decline = self.decline_block(i);
                let taken = {
                    let to = self.target(i)?;
                    self.landing(i, to, true)?
                };
                // Past the smart branch and its jump op.
                let fall = self.block(i + 3)?;
                // Every test before the store.
                let y = match bound {
                    Src::Int(y) => self.b.ins().iconst(types::I64, y),
                    Src::Slot(base, o) => {
                        let t = self.tag(base, o);
                        let is_int = self.b.ins().icmp_imm(IntCC::Equal, t, i64::from(TAG_INT));
                        self.require(is_int, decline);
                        self.b.ins().load(types::I64, mem(), base, o + PAYLOAD_AT)
                    }
                    Src::Float(_) | Src::Never => {
                        self.b.ins().jump(decline, &[]);
                        return Some(());
                    }
                };
                let t = self.tag(self.cvs, at);
                let is_int = self.b.ins().icmp_imm(IntCC::Equal, t, i64::from(TAG_INT));
                self.require(is_int, decline);
                let x = self
                    .b
                    .ins()
                    .load(types::I64, mem(), self.cvs, at + PAYLOAD_AT);
                let Typed::Int(n) = self.step(Typed::Int(x), inc, decline) else {
                    return None;
                };
                self.b.ins().store(mem(), n, self.cvs, at + PAYLOAD_AT);
                let c = self.b.ins().icmp(int_cc(m), n, y);
                self.b.ins().brif(c, taken, &[], fall, &[]);
            }
            Shape::ArithTmp { op, ka, kb } => {
                let (a, b) = (self.src(ka, inst.a)?, self.src(kb, inst.b)?);
                let dst = off(inst.c)?;
                let decline = self.decline_block(i);
                let next = self.block(i + 1)?;
                // An old value to release is the slow path's.
                self.require_plain(self.tmps, dst, false, decline);
                let (ints, floats) = self.num2(a, b, decline, None);
                let (x, y) = self.enter2(ints)?;
                let r = self.int_arith(op, x, y, decline);
                self.store_num(self.tmps, dst, Typed::Int(r));
                self.b.ins().jump(next, &[]);
                let (x, y) = self.enter2(floats)?;
                let r = self.float_arith(op, x, y);
                self.store_num(self.tmps, dst, Typed::Float(r));
                self.b.ins().jump(next, &[]);
            }
            Shape::ArithCv { op, ka, kb } => {
                let (a, b) = (self.src(ka, inst.a)?, self.src(kb, inst.b)?);
                let dst = off(inst.c)?;
                let decline = self.decline_block(i);
                // Past the `AssignCv` the store into the variable does.
                let next = self.block(i + 2)?;
                // A variable holding something to release is the slow
                // path's (the result then goes to the temporary).
                self.require_plain(self.cvs, dst, true, decline);
                let (ints, floats) = self.num2(a, b, decline, None);
                let (x, y) = self.enter2(ints)?;
                let r = self.int_arith(op, x, y, decline);
                self.store_num(self.cvs, dst, Typed::Int(r));
                self.b.ins().jump(next, &[]);
                let (x, y) = self.enter2(floats)?;
                let r = self.float_arith(op, x, y);
                self.store_num(self.cvs, dst, Typed::Float(r));
                self.b.ins().jump(next, &[]);
            }
            Shape::AssignOp { op, kb } => {
                let at = off(inst.a)?;
                let b = self.src(kb, inst.b)?;
                let decline = self.decline_block(i);
                let next = self.block(i + 1)?;
                let (ints, floats) = self.num2(Src::Slot(self.cvs, at), b, decline, None);
                let (x, y) = self.enter2(ints)?;
                let r = self.int_arith(op, x, y, decline);
                self.b.ins().store(mem(), r, self.cvs, at + PAYLOAD_AT);
                self.b.ins().jump(next, &[]);
                // A float result: the variable was a float, or an int (which
                // owns nothing) that becomes one.
                let (x, y) = self.enter2(floats)?;
                let r = self.float_arith(op, x, y);
                self.store_num(self.cvs, at, Typed::Float(r));
                self.b.ins().jump(next, &[]);
            }
            Shape::Family {
                handler,
                covers,
                ending,
            } => {
                let decline = self.decline_block(i);
                let fall = self.landing(i, i + covers, ending.polls)?;
                let callee = self.callee(handler as usize)?;
                let tape = self.record_word(REC_TAPE)?;
                let inst_ptr = self
                    .b
                    .ins()
                    .iadd_imm(tape, i64::try_from(i * std::mem::size_of::<Inst>()).ok()?);
                let call =
                    self.b
                        .ins()
                        .call_indirect(self.sigs.handler, callee, &[self.frame, inst_ptr]);
                let r = *self.b.inst_results(call).first()?;
                if ending.branches {
                    let to = self.target(i)?;
                    let taken = self.landing(i, to, true)?;
                    let done = self.b.create_block();
                    self.b.ins().brif(r, done, &[], decline, &[]);
                    self.b.switch_to_block(done);
                    let jumped = self.b.ins().icmp_imm(IntCC::Equal, r, 2);
                    self.b.ins().brif(jumped, taken, &[], fall, &[]);
                } else {
                    self.b.ins().brif(r, fall, &[], decline, &[]);
                }
            }
        }
        Some(())
    }

    /// `n + 1` or `n - 1`; an int overflow goes to `decline`.
    fn step(&mut self, n: Typed, inc: bool, decline: Block) -> Typed {
        match n {
            Typed::Int(x) => {
                let one = self.b.ins().iconst(types::I64, 1);
                let op = if inc { Arith::Add } else { Arith::Sub };
                Typed::Int(self.int_arith(op, x, one, decline))
            }
            Typed::Float(x) => {
                let one = self.b.ins().f64const(1.0);
                Typed::Float(if inc {
                    self.b.ins().fadd(x, one)
                } else {
                    self.b.ins().fsub(x, one)
                })
            }
        }
    }

    /// Stores the payload of a number over a slot that already has its tag.
    fn store_payload(&mut self, base: V, at: i32, n: Typed) {
        let (Typed::Int(v) | Typed::Float(v)) = n;
        self.b.ins().store(mem(), v, base, at + PAYLOAD_AT);
    }
}

#[cfg(test)]
mod tests;

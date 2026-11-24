// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `AArch64` backend of the trace JIT: the same plans, guards, slot
//! memory model, exit protocol and tables as the x86-64 [`Emitter`], with
//! the encoding layer for ARMv8-A (A64) Linux.
//!
//! # Registers (AAPCS64)
//!
//! The entry is `extern "C" fn(cvs, tmps, frame) -> TraceRet` (`x0`, `x1`,
//! `x2` in; `x0`, `x1` out: the two words of [`TraceRet`]). What the trace
//! keeps across rows lives in callee-saved registers, which the prologue
//! saves and the shared epilogue restores, so a helper call (later rows)
//! needs no saves of its own:
//!
//! - `x19` the compiled variables (`KC`), `x20` the temporaries (`KT`),
//!   `x21` the scratch buffer of inlined calls (`KR`), `x22` the frame,
//!   `x23` the poll address, `w24` the iteration counter.
//! - `x9` is the x86 emitter's `rax` (the int being computed), `x10` its
//!   `r9` (the second operand), `x11` its `rcx`; `x16`/`x17` are scratch
//!   for addresses and immediates. No row keeps a value in a register
//!   from one row to the next.
//!
//! The frame record (`x29`, `x30`) and `x19`-`x24` take 64 bytes, so `sp`
//! stays 16-byte aligned. No row uses `d8`-`d15`, so they are not saved.
//!
//! Every exit stub loads the return words and branches to the one
//! epilogue. Branches are patched in [`A64::resolve`]: `b` has a 26-bit
//! word offset, `b.cond`/`cbz`/`cbnz` a 19-bit one (±1 MiB, far beyond the
//! largest region).
//!
//! Only the rows [`supports`] accepts are emitted; a plan with any other
//! row is refused, and the interpreter runs the loop.

use super::{
    jcc_for, jump_target, poll_word, replan_word, Inst, JitMeta, Plan, Target, ARITH_SAR,
    ARITH_SHL, C_ASSIGN_I, C_ASSIGN_L, C_DEC, C_INC, C_JMP, C_NOP, ITER_CAP, J_ARITH_CV,
    J_ARITH_TMP, J_ASSIGN_OP, J_ASSIGN_T, J_CMP, J_INCCMP, J_NUMBIN_C, J_NUMBIN_D, J_NUMBIN_T, KC,
    KI, KR, KT, OPA_BITAND, OPA_BITOR, OPA_BITXOR, PAYLOAD, TAG_INT, TAG_NONE, TAG_NULL, TAG_STR,
};
use std::collections::HashMap;

/// An index or a code position as a signed word (both are far below
/// `i64::MAX`).
fn sword(x: usize) -> i64 {
    i64::try_from(x).unwrap_or(i64::MAX)
}

/// The slot bases and the registers the trace keeps (see the module docs).
const CVS: u32 = 19;
const TMPS: u32 = 20;
const SCR: u32 = 21;
const FRAME: u32 = 22;
const POLL: u32 = 23;
const ITER: u32 = 24;
/// The x86 emitter's `rax`, `r9` and `rcx`.
const RA: u32 = 9;
const RB: u32 = 10;
/// Scratch for an address offset (`T0`) and a stored immediate (`T1`).
const T0: u32 = 16;
const T1: u32 = 17;
/// `xzr`/`wzr` in a register operand, `sp` in a base operand.
const ZR: u32 = 31;
const SP: u32 = 31;

/// Condition codes.
const EQ: u32 = 0;
const NE: u32 = 1;
const HS: u32 = 2;
const LO: u32 = 3;
const VS: u32 = 6;
const HI: u32 = 8;
const GE: u32 = 10;
const LT: u32 = 11;
const GT: u32 = 12;
const LE: u32 = 13;

/// The A64 condition of an x86 `jcc` opcode byte (`jcc_for`'s
/// comparison masks, and the unsigned and overflow tests the rows use).
fn cond_of(jcc: u8) -> u32 {
    match jcc {
        0x80 => VS,
        0x82 => LO,
        0x83 => HS,
        0x84 => EQ,
        0x86 => 9, // ls
        0x87 => HI,
        0x8C => LT,
        0x8D => GE,
        0x8E => LE,
        0x8F => GT,
        _ => NE,
    }
}

/// How a branch's offset is patched in: `b` (imm26) or `b.cond`/`cbz`/
/// `cbnz` (imm19 at bit 5).
#[derive(Clone, Copy)]
enum Fix {
    B26,
    I19,
}

/// The access widths of [`A64::mem`].
#[derive(Clone, Copy)]
enum W {
    /// A 64-bit word.
    X,
    /// A byte (zero-extended into a `w` register on a load).
    B,
}

/// Whether the `AArch64` backend emits every row of `p`: the minimal loop
/// rows (`Jmp`, int `++`/`--`, the int assignments, the fused
/// compare-and-branch and `++`-compare, the int arithmetic into a variable
/// or a temporary, `$cv = TMP` of an int) and no inlined call, frame read
/// or global read. Anything else is refused, so the interpreter runs it.
pub(super) fn supports(p: &Plan) -> bool {
    if p.has_calls || p.frame {
        return false;
    }
    p.class.iter().zip(&p.meta).all(|(&c, m)| match c {
        0 | C_JMP | C_INC | C_DEC | C_ASSIGN_I | C_ASSIGN_L | C_NOP | J_CMP | J_INCCMP => true,
        J_ARITH_CV | J_ARITH_TMP | J_ASSIGN_OP | J_NUMBIN_T | J_NUMBIN_C | J_NUMBIN_D => {
            matches!(
                m.arith,
                0 | 1 | 2 | 5 | OPA_BITAND | OPA_BITOR | OPA_BITXOR | ARITH_SHL | ARITH_SAR
            )
        }
        // The int path only: a bool, null, scalar or owned value source
        // is a later row.
        J_ASSIGN_T => m.mask == 0,
        _ => false,
    })
}

/// The base register of a slot kind (`KC`, `KT`, `KR`).
fn base_reg(kind: u8) -> u32 {
    match kind {
        KT => TMPS,
        KR => SCR,
        _ => CVS,
    }
}

/// The `AArch64` emitter (see the module docs).
pub(super) struct A64 {
    code: Vec<u32>,
    plan: Plan,
    tape: Vec<Inst>,
    /// The scratch buffer's address (`has_calls`).
    scratch: i64,
    labels: HashMap<usize, usize>,
    stubs: HashMap<usize, usize>,
    sides: HashMap<usize, usize>,
    polls: HashMap<(usize, usize), usize>,
    disms: HashMap<usize, usize>,
    replans: HashMap<usize, usize>,
    fixups: Vec<(usize, Fix, Target)>,
    head_pos: usize,
    synth: usize,
}

impl A64 {
    pub(super) fn new(plan: Plan, tape: Vec<Inst>, scratch: i64) -> A64 {
        A64 {
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

    /// The code as bytes (little-endian instruction words).
    pub(super) fn bytes(&self) -> Vec<u8> {
        self.code.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    fn put(&mut self, w: u32) {
        self.code.push(w);
    }

    // ------------------------------------------------------- encodings

    /// `mov xd, #v` in the fewest `movz`/`movn` + `movk` words.
    fn mov_imm(&mut self, rd: u32, v: i64) {
        #[allow(clippy::cast_sign_loss)]
        let u = v as u64;
        let half = |x: u64, k: u32| ((x >> (16 * k)) & 0xFFFF) as u32;
        let zeros = (0..4).filter(|&k| half(u, k) == 0).count();
        let ones = (0..4).filter(|&k| half(u, k) == 0xFFFF).count();
        if ones > zeros {
            // `movn` of the first halfword that is not all ones (or of
            // halfword 0 for -1), then `movk` of the rest.
            let first = (0..4).find(|&k| half(u, k) != 0xFFFF).unwrap_or(0);
            self.put(0x9280_0000 | (first << 21) | ((!half(u, first) & 0xFFFF) << 5) | rd);
            for k in (first + 1)..4 {
                if half(u, k) != 0xFFFF {
                    self.put(0xF280_0000 | (k << 21) | (half(u, k) << 5) | rd);
                }
            }
        } else {
            let first = (0..4).find(|&k| half(u, k) != 0).unwrap_or(0);
            self.put(0xD280_0000 | (first << 21) | (half(u, first) << 5) | rd);
            for k in (first + 1)..4 {
                if half(u, k) != 0 {
                    self.put(0xF280_0000 | (k << 21) | (half(u, k) << 5) | rd);
                }
            }
        }
    }

    /// `mov xd, xm` (`orr xd, xzr, xm`).
    fn mov_reg(&mut self, rd: u32, rm: u32) {
        self.put(0xAA00_03E0 | (rm << 16) | rd);
    }

    /// A load (`load`) or store of `rt` at `[rn + off]`: the scaled
    /// unsigned-offset form when `off` fits it, else the register-offset
    /// form through `T0` (never the stored register: stores of an
    /// immediate go through `T1`).
    fn mem(&mut self, load: bool, w: W, rt: u32, rn: u32, off: u32) {
        let (imm, reg, scale) = match (w, load) {
            (W::X, true) => (0xF940_0000, 0xF860_6800, 8),
            (W::X, false) => (0xF900_0000, 0xF820_6800, 8),
            (W::B, true) => (0x3940_0000, 0x3860_6800, 1),
            (W::B, false) => (0x3900_0000, 0x3820_6800, 1),
        };
        if off % scale == 0 && off / scale < 4096 {
            self.put(imm | ((off / scale) << 10) | (rn << 5) | rt);
        } else {
            self.mov_imm(T0, i64::from(off));
            self.put(reg | (T0 << 16) | (rn << 5) | rt);
        }
    }

    /// `cmp wn, #imm` (`subs wzr, wn, #imm`, `imm < 4096`).
    fn cmp_w_imm(&mut self, rn: u32, imm: u32) {
        self.put(0x7100_001F | ((imm & 0xFFF) << 10) | (rn << 5));
    }

    /// `cmp xn, xm`.
    fn cmp_x(&mut self, rn: u32, rm: u32) {
        self.put(0xEB00_001F | (rm << 16) | (rn << 5));
    }

    /// `T0` = the tag byte of slot `base + off`, compared with `imm`.
    fn cmp_tag(&mut self, kind: u8, off: u32, imm: u8) {
        self.mem(true, W::B, T0, base_reg(kind), off);
        self.cmp_w_imm(T0, u32::from(imm));
    }

    /// `ldr rd, [base + off + 8]`: the payload of an `Int` slot.
    fn load_payload(&mut self, rd: u32, kind: u8, off: u32) {
        self.mem(true, W::X, rd, base_reg(kind), off.wrapping_add(PAYLOAD));
    }

    /// `str rs, [base + off + 8]`.
    fn store_payload(&mut self, rs: u32, kind: u8, off: u32) {
        self.mem(false, W::X, rs, base_reg(kind), off.wrapping_add(PAYLOAD));
    }

    /// An int payload store of the constant `v`.
    fn store_imm(&mut self, kind: u8, off: u32, v: i64) {
        if v == 0 {
            self.store_payload(ZR, kind, off);
        } else {
            self.mov_imm(T1, v);
            self.store_payload(T1, kind, off);
        }
    }

    /// The tag store (after the payload store, as on x86-64).
    fn store_tag(&mut self, kind: u8, off: u32, tag: u8) {
        if tag == 0 {
            self.mem(false, W::B, ZR, base_reg(kind), off);
        } else {
            self.put(0x5280_0000 | (u32::from(tag) << 5) | T1); // movz w17, #tag
            self.mem(false, W::B, T1, base_reg(kind), off);
        }
    }

    /// An operand into `rd`: a slot payload, an inline `KI` constant, or
    /// a resolved `KL` literal.
    fn operand(&mut self, rd: u32, kind: u8, off: u32, lit: i64) {
        match kind {
            KC | KT | KR => self.load_payload(rd, kind, off),
            #[allow(clippy::cast_possible_wrap)]
            KI => self.mov_imm(rd, i64::from(off as i32)),
            _ => self.mov_imm(rd, lit),
        }
    }

    /// `b t`.
    fn b(&mut self, t: Target) {
        self.fixups.push((self.code.len(), Fix::B26, t));
        self.put(0x1400_0000);
    }

    /// `b.cond t`.
    fn bcond(&mut self, cond: u32, t: Target) {
        self.fixups.push((self.code.len(), Fix::I19, t));
        self.put(0x5400_0000 | cond);
    }

    /// `cbnz wt, t`.
    fn cbnz_w(&mut self, rt: u32, t: Target) {
        self.fixups.push((self.code.len(), Fix::I19, t));
        self.put(0x3500_0000 | rt);
    }

    /// `cbz xt, t`.
    fn cbz_x(&mut self, rt: u32, t: Target) {
        self.fixups.push((self.code.len(), Fix::I19, t));
        self.put(0xB400_0000 | rt);
    }

    /// The safe-point poll: `ldrb w16, [x23]; cbnz w16, t`.
    fn poll(&mut self, t: Target) {
        self.put(0x3940_0000 | (POLL << 5) | T0);
        self.cbnz_w(T0, t);
    }

    // ---------------------------------------------------------- labels

    fn label(&mut self, idx: usize) {
        self.labels.insert(idx, self.code.len());
    }

    fn synth_label(&mut self) -> usize {
        self.synth += 1;
        self.plan.close + self.synth
    }

    fn set_label(&mut self, key: usize) {
        self.labels.insert(key, self.code.len());
    }

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

    /// The check of a temporary the plan knows nothing about, where a row
    /// reads it as an int.
    fn chk_int(&mut self, flag: bool, off: u32, jo: Target) {
        if flag {
            self.cmp_tag(KT, off, TAG_INT);
            self.bcond(NE, jo);
        }
    }

    /// `x9 = x9 ± 1` setting the flags (`adds`/`subs x9, x9, #1`).
    fn step_ra(&mut self, inc: bool) {
        let op = if inc { 0xB100_0000 } else { 0xF100_0000 };
        self.put(op | (1 << 10) | (RA << 5) | RA);
    }

    // ------------------------------------------------------------ rows

    fn emit_instr(&mut self, j: usize) {
        let i = self.plan.head + j;
        let inst = self.tape[i];
        let m = self.plan.meta[j];
        match self.plan.class[j] {
            C_JMP => self.b(self.jump_tgt(j)),
            J_CMP => {
                self.chk_int(m.ca, inst.a, Target::Side(i));
                self.chk_int(m.cb, inst.b, Target::Side(i));
                self.operand(RA, m.ka, inst.a, m.lit_a);
                self.operand(RB, m.kb, inst.b, m.lit_b);
                self.cmp_x(RA, RB);
                let cc = cond_of(jcc_for(m.mask).unwrap_or(0x85));
                self.bcond(cc, self.jump_tgt(j));
            }
            J_INCCMP => {
                // `b` is read before the step (it may be the same slot).
                self.chk_int(m.cb, inst.b, Target::Side(i));
                self.operand(RB, m.kb, inst.b, m.lit_b);
                self.load_payload(RA, KC, inst.a);
                self.step_ra(m.inc);
                self.bcond(VS, Target::Side(i)); // overflow, before any store
                self.store_payload(RA, KC, inst.a);
                self.cmp_x(RA, RB);
                let cc = cond_of(jcc_for(m.mask).unwrap_or(0x85));
                self.bcond(cc, self.jump_tgt(j));
            }
            c => self.emit_op(c, m, &inst, Target::Side(i)),
        }
    }

    fn emit_op(&mut self, c: u16, m: JitMeta, inst: &Inst, jo: Target) {
        match c {
            C_INC | C_DEC => {
                self.load_payload(RA, m.ka, inst.a);
                self.step_ra(c == C_INC);
                self.bcond(VS, jo); // overflow, before any store
                self.store_payload(RA, m.ka, inst.a);
            }
            C_ASSIGN_I => {
                #[allow(clippy::cast_possible_wrap)]
                self.store_imm(m.ka, inst.a, i64::from(inst.b as i32));
                self.store_tag(m.ka, inst.a, TAG_INT);
            }
            C_ASSIGN_L => {
                self.store_imm(m.ka, inst.a, m.lit_b);
                self.store_tag(m.ka, inst.a, TAG_INT);
            }
            C_NOP => {}
            // `$cv = TMP` of an int: copy it, then consume the temporary.
            J_ASSIGN_T => {
                self.chk_int(m.cb, inst.b, jo);
                self.load_payload(RA, m.kb, inst.b);
                self.store_payload(RA, m.ka, inst.a);
                self.store_tag(m.ka, inst.a, TAG_INT);
                self.store_imm(m.kb, inst.b, 0);
                self.store_tag(m.kb, inst.b, TAG_NULL);
            }
            J_ARITH_CV | J_ARITH_TMP | J_ASSIGN_OP | J_NUMBIN_T | J_NUMBIN_C | J_NUMBIN_D => {
                let dst = if c == J_ASSIGN_OP || c == J_NUMBIN_C {
                    inst.a
                } else if c == J_NUMBIN_D {
                    inst.d
                } else {
                    inst.c
                };
                self.chk_int(m.ca, inst.a, jo);
                self.chk_int(m.cb, inst.b, jo);
                self.operand(RA, m.ka, inst.a, m.lit_a);
                self.operand(RB, m.kb, inst.b, m.lit_b);
                self.arith(m.arith, jo);
                self.store_payload(RA, m.kc, dst);
                if m.kc == KT {
                    self.store_tag(KT, dst, TAG_INT);
                }
            }
            // `supports` refuses every other row; should one get here, it
            // is the interpreter's (nothing of it has run).
            _ => self.b(jo),
        }
    }

    /// `x9 = x9 op x10`, leaving through `jo` before any store where the
    /// int result does not exist (an overflow, a shift count outside
    /// `0..=63`, a zero divisor or `PHP_INT_MIN % -1`), as on x86-64.
    fn arith(&mut self, arith: u8, jo: Target) {
        let rrr = |op: u32| op | (RB << 16) | (RA << 5) | RA;
        match arith {
            0 => {
                self.put(rrr(0xAB00_0000)); // adds x9, x9, x10
                self.bcond(VS, jo);
            }
            1 => {
                self.put(rrr(0xEB00_0000)); // subs x9, x9, x10
                self.bcond(VS, jo);
            }
            2 => {
                // The low and the high half of the 128-bit product: the
                // product fits when the high half is the low half's sign.
                self.put(0x9B00_7C00 | (RB << 16) | (RA << 5) | T0); // mul x16, x9, x10
                self.put(0x9B40_7C00 | (RB << 16) | (RA << 5) | T1); // smulh x17, x9, x10
                self.put(0xEB80_0000 | (T0 << 16) | (63 << 10) | (T1 << 5) | ZR); // cmp x17, x16, asr #63
                self.bcond(NE, jo);
                self.mov_reg(RA, T0);
            }
            OPA_BITAND => self.put(rrr(0x8A00_0000)), // and
            OPA_BITOR => self.put(rrr(0xAA00_0000)),  // orr
            OPA_BITXOR => self.put(rrr(0xCA00_0000)), // eor
            ARITH_SHL | ARITH_SAR => {
                // A count outside `0..=63` (a negative one is huge
                // unsigned) is the interpreted row's.
                self.put(0xF100_001F | (63 << 10) | (RB << 5)); // cmp x10, #63
                self.bcond(HI, jo);
                let op = if arith == ARITH_SHL {
                    0x9AC0_2000 // lslv
                } else {
                    0x9AC0_2800 // asrv
                };
                self.put(rrr(op));
            }
            _ => {
                // `%`: a zero divisor and `PHP_INT_MIN % -1` are guarded
                // exits (as the x86 `idiv`'s faults are).
                self.cbz_x(RB, jo);
                self.put(0xB100_001F | (1 << 10) | (RB << 5)); // cmn x10, #1
                self.bcond(EQ, jo);
                self.put(0x9AC0_0C00 | (RB << 16) | (RA << 5) | T0); // sdiv x16, x9, x10
                                                                     // msub x9, x16, x10, x9: x9 - x16 * x10, the remainder.
                self.put(0x9B00_8000 | (RB << 16) | (RA << 10) | (T0 << 5) | RA);
            }
        }
    }

    // ---------------------------------------------------------- driver

    /// Emit the whole region (branches still need `resolve`).
    pub(super) fn emit(&mut self) {
        // The frame record and the registers the trace keeps.
        self.put(0xA9BC_7BFD); // stp x29, x30, [sp, #-64]!
        self.put(0x9100_03FD); // mov x29, sp
        for (k, r) in [(2u32, 19u32), (4, 21), (6, 23)] {
            // stp x<r>, x<r+1>, [sp, #8k]
            self.put(0xA900_0000 | (k << 15) | ((r + 1) << 10) | (SP << 5) | r);
        }
        self.mov_reg(CVS, 0);
        self.mov_reg(TMPS, 1);
        self.mov_reg(FRAME, 2);
        self.mov_imm(POLL, crate::interrupt::jit_poll_addr() as i64);
        self.put(0x5280_0000 | ITER); // movz w24, #0
        let disarm = Target::Disarm(self.plan.head);
        for k in 0..self.plan.guards.len() {
            let g = self.plan.guards[k];
            if g.exact {
                self.cmp_tag(g.base, g.off, TAG_INT);
                self.bcond(NE, disarm);
            } else if g.plain && g.defined {
                // A scalar, and a value: the tags below `Str`.
                self.cmp_tag(g.base, g.off, TAG_STR);
                self.bcond(HS, disarm);
            } else if g.defined {
                self.cmp_tag(g.base, g.off, TAG_NONE);
                self.bcond(EQ, disarm);
            } else {
                // A scalar or nothing.
                let ok = self.synth_label();
                self.cmp_tag(g.base, g.off, TAG_STR);
                self.bcond(LO, Target::Label(ok));
                self.cmp_w_imm(T0, u32::from(TAG_NONE));
                self.bcond(NE, disarm);
                self.set_label(ok);
            }
        }
        if self.plan.has_calls {
            self.mov_imm(SCR, self.scratch);
        }
        if self.plan.has_calls || self.plan.version_guard {
            self.mov_imm(T0, crate::func::FUNC_TABLE_VERSION.as_ptr() as i64);
            self.mem(true, W::X, T0, T0, 0);
            #[allow(clippy::cast_possible_wrap)]
            self.mov_imm(T1, self.plan.version as i64);
            self.cmp_x(T0, T1);
            self.bcond(NE, Target::Replan(self.plan.head));
        }
        // The head: the poll and the iteration cap.
        self.head_pos = self.code.len();
        self.poll(Target::Poll(self.plan.head, self.plan.close));
        self.put(0x1100_0000 | (1 << 10) | (ITER << 5) | ITER); // add w24, w24, #1
        self.mov_imm(T0, i64::from(ITER_CAP));
        self.put(0x6B00_001F | (T0 << 16) | (ITER << 5)); // cmp w24, w16
        self.bcond(HI, Target::Stub(self.plan.head));
        let head = self.plan.head;
        let len = self.plan.close - head + 1;
        // The heads of loops nested in this one poll too.
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
            if self.plan.class[j] != 0 {
                self.label(head + j);
                if let Some(&from) = inner.get(&(head + j)) {
                    self.poll(Target::Poll(head + j, from));
                }
                self.emit_instr(j);
                j += self.plan.span[j] as usize;
            } else {
                j += 1;
            }
        }
        // A fused closing jump's fall-through leaves the region.
        let cc = self.plan.class[len - 1];
        if cc == J_CMP || cc == J_INCCMP {
            let end = self.plan.close + if cc == J_INCCMP { 3 } else { 2 };
            self.b(Target::Stub(end));
        }
    }

    /// Append the epilogue and the exit stubs, and patch every branch.
    /// `false` when a branch does not reach (never for a region of the
    /// planner's size; the trace is then refused).
    pub(super) fn resolve(&mut self) -> bool {
        let fixups = std::mem::take(&mut self.fixups);
        let epilogue = self.code.len();
        for (k, r) in [(6u32, 23u32), (4, 21), (2, 19)] {
            // ldp x<r>, x<r+1>, [sp, #8k]
            self.put(0xA940_0000 | (k << 15) | ((r + 1) << 10) | (SP << 5) | r);
        }
        self.put(0xA8C4_7BFD); // ldp x29, x30, [sp], #64
        self.put(0xD65F_03C0); // ret
        for (_, _, t) in &fixups {
            let at = self.code.len();
            // x0: the resume word; x1: the iteration a row left in, or 0.
            let (word, iters) = match *t {
                Target::Stub(idx) if !self.stubs.contains_key(&idx) => {
                    self.stubs.insert(idx, at);
                    (sword(idx), false)
                }
                Target::Side(idx) if !self.sides.contains_key(&idx) => {
                    self.sides.insert(idx, at);
                    (sword(idx), true)
                }
                Target::Poll(resume, row) if !self.polls.contains_key(&(resume, row)) => {
                    self.polls.insert((resume, row), at);
                    (poll_word(resume, row), false)
                }
                Target::Disarm(idx) if !self.disms.contains_key(&idx) => {
                    self.disms.insert(idx, at);
                    ((sword(idx) + 1).wrapping_neg(), false)
                }
                Target::Replan(idx) if !self.replans.contains_key(&idx) => {
                    self.replans.insert(idx, at);
                    (replan_word(idx), false)
                }
                _ => continue,
            };
            self.mov_imm(0, word);
            if iters {
                self.put(0x2A00_03E0 | (ITER << 16) | 1); // mov w1, w24
            } else {
                self.mov_reg(1, ZR);
            }
            let back = sword(epilogue) - sword(self.code.len());
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            self.put(0x1400_0000 | (back as u32 & 0x03FF_FFFF));
        }
        for (pos, fix, t) in fixups {
            let abs = match t {
                Target::Head => self.head_pos,
                Target::Label(j) => match self.labels.get(&j) {
                    Some(&p) => p,
                    None => return false,
                },
                Target::Stub(idx) => self.stubs[&idx],
                Target::Side(idx) => self.sides[&idx],
                Target::Poll(resume, row) => self.polls[&(resume, row)],
                Target::Disarm(idx) => self.disms[&idx],
                Target::Replan(idx) => self.replans[&idx],
                Target::At(p) => p,
            };
            let rel = sword(abs) - sword(pos);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            match fix {
                Fix::B26 if (-(1 << 25)..(1 << 25)).contains(&rel) => {
                    self.code[pos] |= rel as u32 & 0x03FF_FFFF;
                }
                Fix::I19 if (-(1 << 18)..(1 << 18)).contains(&rel) => {
                    self.code[pos] |= (rel as u32 & 0x7_FFFF) << 5;
                }
                _ => return false,
            }
        }
        true
    }
}

/// Make freshly written code at `p .. p + len` visible to instruction
/// fetch: clean the data cache and invalidate the instruction cache to the
/// point of unification, line by line, with the barriers between (what
/// `__clear_cache` does on `AArch64` Linux). Linux lets user space read
/// `CTR_EL0` and run `dc cvau`/`ic ivau`.
#[cfg(target_arch = "aarch64")]
pub(super) fn flush_icache(p: *const u8, len: usize) {
    let ctr: u64;
    // SAFETY: reading `CTR_EL0`, which EL0 may read on Linux.
    unsafe { std::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
    let dline = 4usize << ((ctr >> 16) & 0xF);
    let iline = 4usize << (ctr & 0xF);
    let start = p as usize;
    let end = start + len;
    let mut a = start & !(dline - 1);
    while a < end {
        // SAFETY: `a` is inside the mapping the caller just wrote.
        unsafe { std::arch::asm!("dc cvau, {}", in(reg) a, options(nostack)) };
        a += dline;
    }
    // SAFETY: a barrier.
    unsafe { std::arch::asm!("dsb ish", options(nostack)) };
    let mut a = start & !(iline - 1);
    while a < end {
        // SAFETY: `a` is inside the mapping the caller just wrote.
        unsafe { std::arch::asm!("ic ivau, {}", in(reg) a, options(nostack)) };
        a += iline;
    }
    // SAFETY: barriers.
    unsafe { std::arch::asm!("dsb ish", "isb", options(nostack)) };
}

#[cfg(test)]
// The instruction words are as GNU `objdump` prints them.
#[allow(clippy::unreadable_literal)]
mod tests {
    use super::*;

    fn em() -> A64 {
        let plan = Plan {
            head: 0,
            close: 0,
            class: vec![C_JMP],
            meta: vec![JitMeta::default()],
            guards: Vec::new(),
            entries: Vec::new(),
            span: vec![1],
            calls: vec![None],
            has_calls: false,
            frame: false,
            version: 0,
            version_guard: false,
        };
        A64::new(plan, vec![Inst::plain(super::super::TOp::Jmp)], 0)
    }

    /// Immediates take the fewest words: `movz`, `movn`, then `movk`s
    /// (checked against GNU `as`).
    #[test]
    fn mov_imm_encodings() {
        let mut e = em();
        e.mov_imm(9, 0);
        e.mov_imm(9, 5);
        e.mov_imm(9, -1);
        e.mov_imm(9, -3);
        e.mov_imm(9, 0x1_0000);
        e.mov_imm(9, 0x1234_5678_9ABC_DEF0);
        e.mov_imm(9, i64::MIN);
        assert_eq!(
            e.code,
            [
                0xD280_0009, // mov x9, #0
                0xD280_00A9, // mov x9, #5
                0x9280_0009, // mov x9, #-1
                0x9280_0049, // mov x9, #-3
                0xD2A0_0029, // mov x9, #0x10000
                0xD29B_DE09, // movz x9, #0xdef0
                0xF2B3_5789, // movk x9, #0x9abc, lsl #16
                0xF2CA_CF09, // movk x9, #0x5678, lsl #32
                0xF2E2_4689, // movk x9, #0x1234, lsl #48
                0xD2F0_0009, // mov x9, #i64::MIN
            ]
        );
    }

    /// Slot accesses: the scaled offset form, and the register form past
    /// its reach.
    #[test]
    fn slot_access_encodings() {
        let mut e = em();
        e.load_payload(RA, KC, 0x10); // ldr x9, [x19, #0x18]
        e.store_payload(RA, KT, 0x20); // str x9, [x20, #0x28]
        e.cmp_tag(KC, 0x30, TAG_INT); // ldrb w16, [x19, #0x30]; cmp w16, #2
        e.store_tag(KT, 0x20, TAG_INT); // mov w17, #2; strb w17, [x20, #0x20]
        e.store_tag(KT, 0x20, TAG_NULL); // strb wzr, [x20, #0x20]
        e.load_payload(RA, KC, 0x10_0000); // mov x16, #0x100008; ldr x9, [x19, x16]
        assert_eq!(
            e.code,
            [
                0xF9400E69, 0xF9001689, 0x3940C270, 0x71000A1F, 0x52800051, 0x39008291, 0x3900829F,
                0xD2800110, 0xF2A00210, 0xF8706A69,
            ]
        );
    }

    /// The arithmetic rows' overflow tests and operations.
    #[test]
    fn arith_encodings() {
        let mut e = em();
        e.arith(0, Target::Side(0));
        e.arith(2, Target::Side(0));
        e.arith(ARITH_SHL, Target::Side(0));
        e.arith(5, Target::Side(0));
        assert_eq!(
            e.code,
            [
                0xAB0A0129, // adds x9, x9, x10
                0x54000006, // b.vs
                0x9B0A7D30, // mul x16, x9, x10
                0x9B4A7D31, // smulh x17, x9, x10
                0xEB90FE3F, // cmp x17, x16, asr #63
                0x54000001, // b.ne
                0xAA1003E9, // mov x9, x16
                0xF100FD5F, // cmp x10, #63
                0x54000008, // b.hi
                0x9ACA2129, // lsl x9, x9, x10
                0xB400000A, // cbz x10
                0xB100055F, // cmn x10, #1
                0x54000000, // b.eq
                0x9ACA0D30, // sdiv x16, x9, x10
                0x9B0AA609, // msub x9, x16, x10, x9
            ]
        );
    }

    /// The prologue saves `x29`/`x30` and `x19`-`x24` in a 64-byte frame;
    /// every exit stub loads its words and branches to the epilogue, which
    /// restores them.
    #[test]
    fn frame_and_exit_encodings() {
        let mut e = em();
        e.emit();
        assert_eq!(
            e.code[..5],
            [0xA9BC7BFD, 0x910003FD, 0xA90153F3, 0xA9025BF5, 0xA90363F7]
        );
        let mut e = em();
        e.bcond(NE, Target::Side(5));
        e.bcond(NE, Target::Disarm(2));
        assert!(e.resolve());
        assert_eq!(
            e.code[2..7],
            [0xA94363F7, 0xA9425BF5, 0xA94153F3, 0xA8C47BFD, 0xD65F03C0]
        );
        // `Side(5)`: x0 = 5, w1 = w24, b epilogue.
        assert_eq!(e.code[7..10], [0xD28000A0, 0x2A1803E1, 0x17FFFFF9]);
        // `Disarm(2)`: x0 = -3, x1 = 0, b epilogue.
        assert_eq!(e.code[10..13], [0x92800040, 0xAA1F03E1, 0x17FFFFF6]);
        // The branches land on their stubs.
        assert_eq!(e.code[0], 0x5400_0001 | (7 << 5));
        assert_eq!(e.code[1], 0x5400_0001 | (9 << 5));
    }

    #[derive(Clone, Default)]
    struct Sink(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

    impl std::io::Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// The output of `src`, and whether a trace was compiled meanwhile
    /// (by this test or another one running at the same time: only its
    /// absence means something).
    fn run(src: &str) -> (String, bool) {
        let before = super::super::TRACES.load(std::sync::atomic::Ordering::Relaxed);
        let sink = Sink::default();
        let mut vm = crate::Vm::new(&crate::VmOptions::default(), Box::new(sink.clone()));
        vm.run_script(src.as_bytes(), "t.php");
        drop(vm);
        let out = String::from_utf8_lossy(&sink.0.borrow()).into_owned();
        let after = super::super::TRACES.load(std::sync::atomic::Ordering::Relaxed);
        (out, after > before)
    }

    /// Whether this process runs the `AArch64` traces.
    fn native() -> bool {
        cfg!(all(target_arch = "aarch64", target_os = "linux")) && super::super::enabled()
    }

    /// Hot int loops, compiled, give the interpreter's answers (the
    /// expected output is the interpreter's, `BUZZS_JIT=0`).
    #[test]
    fn int_loops_run_compiled() {
        let (out, traced) = run(r#"<?php
$s = 0;
for ($i = 0; $i < 100000; $i++) { $s += $i; }
echo $s, "\n";
$s = 0; $i = 0;
while ($i < 50000) { $s = $s + $i * 3 - 1; $i = $i + 1; }
echo $s, "\n";
$s = 1;
for ($i = 0; $i < 20000; ++$i) { $s = ($s * 31 + $i) % 1000003; }
echo $s, "\n";
$x = 0;
for ($i = 100000; $i > 0; $i--) { $x ^= $i; $x |= ($i & 7); $x &= 0xFFFFF; }
echo $x, "\n";
$y = 0;
for ($i = 0; $i < 30000; $i++) { $y = $y + ($i << 3) + ($i >> 2); }
echo $y, "\n";
"#);
        assert_eq!(out, "4999950000\n3749875000\n990470\n100007\n3712365000\n");
        if native() {
            assert!(traced);
        }
    }

    /// An int overflow inside a compiled loop leaves it before the store,
    /// and the interpreter goes on in floats.
    #[test]
    fn overflow_leaves_the_trace() {
        let (out, _) = run(r#"<?php
$s = PHP_INT_MAX - 50000;
for ($i = 0; $i < 100000; $i++) { $s += 1; }
var_dump($s);
$s = PHP_INT_MIN + 3000;
for ($i = 0; $i < 10000; $i++) { $s--; }
var_dump($s);
$k = 2; $j = 0;
for ($i = 0; $i < 4000; $i++) { $j = $k * 2; $k = $j; }
var_dump($k);
$c = 0;
for ($i = PHP_INT_MAX - 5000; $i < PHP_INT_MAX; $i++) { $c++; }
var_dump($i, $c);
"#);
        assert_eq!(
            out,
            "float(9.223372036854776E+18)\nfloat(-9.223372036854776E+18)\nfloat(INF)\n\
             int(9223372036854775807)\nint(5000)\n"
        );
    }

    /// A zero divisor, a negative shift count and `PHP_INT_MIN % -1` are
    /// the interpreted row's.
    #[test]
    fn arithmetic_errors_are_the_interpreters() {
        let (out, _) = run(r#"<?php
function f($n) {
  $r = 0;
  for ($i = 0; $i < 5000; $i++) { $r = $r + 100 % ($n - $i); }
  return $r;
}
try { var_dump(f(3000)); } catch (\Throwable $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
function g($n) {
  $r = 0;
  for ($i = 0; $i < 5000; $i++) { $r = $r + (1 << ($n - $i)); }
  return $r;
}
try { var_dump(g(3000)); } catch (\Throwable $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
$r = 0;
for ($i = 0; $i < 5000; $i++) { $r += ($i << 70) + (-8 >> 65); }
var_dump($r);
$r = 0;
for ($i = 1; $i < 5000; $i++) { $r += PHP_INT_MIN % -1; $r += $i % -3; }
var_dump($r);
"#);
        assert_eq!(
            out,
            "DivisionByZeroError: Modulo by zero\nArithmeticError: Bit shift by negative number\n\
             int(-5000)\nint(4999)\n"
        );
    }
}

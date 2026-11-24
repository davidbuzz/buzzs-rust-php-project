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

// The backend speaks the parent's whole vocabulary: its plan types, row
// classes, meta flags, layout constants and helpers.
#[allow(clippy::wildcard_imports)]
use super::*;
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
const RC: u32 = 11;
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
const MI: u32 = 4;
const PL: u32 = 5;
const VS: u32 = 6;
const HI: u32 = 8;
const LS: u32 = 9;
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
        0x86 => LS,
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
enum Width {
    /// A 64-bit word.
    X,
    /// A 32-bit word (zero-extended on a load).
    W,
    /// A byte (zero-extended into a `w` register on a load).
    B,
    /// A double, into or out of a `d` register.
    D,
}

/// Whether the `AArch64` backend emits every row of `p`: every row class
/// the x86-64 emitter has, inlined calls included (their bodies checked
/// row by row). Anything else is refused, so the interpreter runs it.
pub(super) fn supports(p: &Plan) -> bool {
    p.class
        .iter()
        .zip(&p.meta)
        .enumerate()
        .all(|(j, (&c, m))| match c {
            0 | C_JMP | J_CMP | J_INCCMP | C_JMPZ | C_JMPNZ | C_JMPSET | C_COALESCE | C_JMPZT
            | C_JMPNZT | J_CMPS | C_MOVE | C_DIMR | C_DIMW | C_DIMOP | C_DIMSR | C_DIMKEY
            | C_GREAD | C_VMREAD | C_ROW | C_ROWBR | C_PROP | C_PROPW | C_PROPINC | C_PROPISSET
            | C_PROPOPA | C_SPROP | C_SPROPW | C_SPROPISSET | C_SPROPINC | C_CONSTI
            | J_ASSIGN_T => true,
            J_ASSIGN_OPS => m.arith <= 2,
            // An inlined call: its body's rows are property rows or rows
            // `emit_op` writes.
            C_CALL => p.calls.get(j).and_then(Option::as_ref).is_some_and(|cp| {
                cp.classes.iter().zip(&cp.metas).all(|(&c, m)| {
                    matches!(c, C_PROP | C_PROPW | C_PROPINC | C_PROPOPA) || op_supported(c, m)
                })
            }),
            _ => op_supported(c, m),
        })
}

/// Whether [`A64::emit_op`] writes row class `c` (a region row, or a row
/// of an inlined body).
fn op_supported(c: u16, m: &JitMeta) -> bool {
    match c {
        C_INC | C_DEC | C_ASSIGN_I | C_ASSIGN_L | C_NOP | C_ASSIGN_R | C_INCDEC_T | C_ABS => true,
        // `ANY_SRC` is the tape's own assignment, which `emit_instr` calls.
        J_ASSIGN_T => m.mask != ANY_SRC,
        J_ARITH_CV | J_ARITH_TMP | J_ASSIGN_OP | J_NUMBIN_T | J_NUMBIN_C | J_NUMBIN_D => matches!(
            m.arith,
            0 | 1 | 2 | 5 | OPA_BITAND | OPA_BITOR | OPA_BITXOR | ARITH_SHL | ARITH_SAR
        ),
        _ => false,
    }
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
    /// The address of the tape the trace is of (the rows a handler reads).
    pub(super) tbase: usize,
    /// The trace's table of global slots (`C_GREAD`), 0 when none.
    pub(super) gtable: i64,
    /// The closure objects the guards name, kept alive once placed.
    pub(super) pins: Vec<(u64, php_runtime::Obj)>,
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
            tbase: 0,
            gtable: 0,
            pins: Vec::new(),
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
    fn mem(&mut self, load: bool, w: Width, rt: u32, rn: u32, off: u32) {
        let (imm, reg, scale) = match (w, load) {
            (Width::X, true) => (0xF940_0000, 0xF860_6800, 8),
            (Width::X, false) => (0xF900_0000, 0xF820_6800, 8),
            (Width::W, true) => (0xB940_0000, 0xB860_6800, 4),
            (Width::W, false) => (0xB900_0000, 0xB820_6800, 4),
            (Width::D, true) => (0xFD40_0000, 0xFC60_6800, 8),
            (Width::D, false) => (0xFD00_0000, 0xFC20_6800, 8),
            (Width::B, true) => (0x3940_0000, 0x3860_6800, 1),
            (Width::B, false) => (0x3900_0000, 0x3820_6800, 1),
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
        self.mem(true, Width::B, T0, base_reg(kind), off);
        self.cmp_w_imm(T0, u32::from(imm));
    }

    /// `ldr rd, [base + off + 8]`: the payload of an `Int` slot.
    fn load_payload(&mut self, rd: u32, kind: u8, off: u32) {
        self.mem(
            true,
            Width::X,
            rd,
            base_reg(kind),
            off.wrapping_add(PAYLOAD),
        );
    }

    /// `str rs, [base + off + 8]`.
    fn store_payload(&mut self, rs: u32, kind: u8, off: u32) {
        self.mem(
            false,
            Width::X,
            rs,
            base_reg(kind),
            off.wrapping_add(PAYLOAD),
        );
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
            self.mem(false, Width::B, ZR, base_reg(kind), off);
        } else {
            self.put(0x5280_0000 | (u32::from(tag) << 5) | T1); // movz w17, #tag
            self.mem(false, Width::B, T1, base_reg(kind), off);
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

    /// `cbnz xt, t`.
    fn cbnz_x(&mut self, rt: u32, t: Target) {
        self.fixups.push((self.code.len(), Fix::I19, t));
        self.put(0xB500_0000 | rt);
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

    #[allow(clippy::too_many_lines)] // one arm per row class, as the x86 `emit_instr`
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
            C_MOVE => self.emit_move(m, &inst, Target::Side(i)),
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
            C_DIMSR => {
                let row = self.row_arg(i);
                self.emit_helper(
                    jit_row_fetch_dims as *const () as usize,
                    [HArg::Frame, row, HArg::Null],
                    0,
                    Target::Side(i),
                );
                self.poll(Target::Poll(i + 1, i));
            }
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
                self.poll(Target::Poll(next, i));
            }
            C_GREAD => self.emit_gread(m, i),
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
                self.poll(Target::Poll(next, i));
            }
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
                    self.poll(Target::Poll(next, i));
                }
            }
            // A handler that may jump: 1 declined, 2 jumped, 0 goes on.
            C_ROWBR => {
                let row = self.row_arg(i);
                self.emit_helper_call(
                    usize::try_from(m.lit_a).unwrap_or(0),
                    [HArg::Frame, row, HArg::Null],
                    0,
                );
                self.uxtb_w0();
                self.cmp_w_imm(0, 1);
                self.bcond(EQ, Target::Side(i));
                self.bcond(HI, self.jump_tgt(j));
            }
            // `$cv = TMP` of any value over any value: the tape's own
            // assignment, then its poll.
            J_ASSIGN_T if m.mask == ANY_SRC => {
                let row = self.row_arg(i);
                self.emit_row(
                    super::super::scalar::assign_cv_any as *const () as usize,
                    false,
                    [HArg::Frame, row, HArg::Abs(1)],
                    Target::Side(i),
                );
                self.poll(Target::Poll(i + 1, i));
            }
            C_JMPZT | C_JMPNZT => {
                if m.ca {
                    let (is_bool, tested) = (self.synth_label(), self.synth_label());
                    self.cmp_tag(KT, inst.a, TAG_BOOL);
                    self.bcond(EQ, Target::Label(is_bool));
                    self.cmp_w_imm(T0, u32::from(TAG_INT));
                    self.bcond(NE, Target::Side(i));
                    self.load_payload(RA, KT, inst.a);
                    self.cmp_x_imm(RA, 0);
                    self.put(0x1A9F_07E0 | RC); // cset w11, ne
                    self.b(Target::Label(tested));
                    self.set_label(is_bool);
                    self.ldb_rc(KT, inst.a.wrapping_add(BOOL_BYTE));
                    self.set_label(tested);
                } else {
                    self.ldb_rc(KT, inst.a.wrapping_add(BOOL_BYTE));
                }
                self.store_imm(KT, inst.a, 0);
                self.store_tag(KT, inst.a, TAG_NULL);
                let tgt = self.jump_tgt(j);
                self.cb_rc(self.plan.class[j] == C_JMPNZT, tgt);
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
                self.b(self.jump_tgt(j));
                self.set_label(end);
            }
            C_COALESCE => {
                let end = self.synth_label();
                self.cmp_tag(KC, inst.a, TAG_NULL);
                self.bcond(EQ, Target::Label(end));
                self.cmp_w_imm(T0, u32::from(TAG_NONE));
                self.bcond(EQ, Target::Label(end));
                self.copy_cv_value(inst.a, inst.b, m.arr, Target::Side(i));
                self.b(self.jump_tgt(j));
                self.set_label(end);
            }
            J_CMPS => self.emit_cmps(m, &inst, i, j),
            J_ASSIGN_OPS => self.emit_assign_ops(m, &inst, i),
            c => self.emit_op(c, m, &inst, Target::Side(i)),
        }
    }

    #[allow(clippy::too_many_lines)] // one arm per row class, as the x86 `emit_op`
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
            // `$cv = v` with the result kept: into the variable and the
            // result temporary, a consumed source left null first.
            C_ASSIGN_R => {
                self.chk_int(m.cb, inst.b, jo);
                self.operand(RA, m.kb, inst.b, m.lit_b);
                if m.consume {
                    self.store_imm(KT, inst.b, 0);
                    self.store_tag(KT, inst.b, TAG_NULL);
                }
                self.store_payload(RA, KC, inst.a);
                self.store_tag(KC, inst.a, TAG_INT);
                self.store_payload(RA, KT, inst.c);
                self.store_tag(KT, inst.c, TAG_INT);
            }
            // `++`/`--` with the result kept (`arr`: the value before).
            C_INCDEC_T => {
                self.load_payload(RA, KC, inst.a);
                self.step_ra(m.inc);
                self.bcond(VS, jo);
                self.store_payload(RA, KC, inst.a);
                if m.arr {
                    // The step undone (`sub`/`add x9, x9, #1`, no flags).
                    let op = if m.inc { 0xD100_0000 } else { 0x9100_0000 };
                    self.put(op | (1 << 10) | (RA << 5) | RA);
                }
                self.store_payload(RA, KT, inst.c);
                self.store_tag(KT, inst.c, TAG_INT);
            }
            // `abs(x)`: `PHP_INT_MIN`, which `negs` overflows on, is the
            // interpreted row's.
            C_ABS => {
                self.chk_int(m.ca, inst.a, jo);
                if inst.c != u32::MAX {
                    let skip = self.synth_label();
                    self.operand(RA, m.ka, inst.a, m.lit_a);
                    self.cmp_x_imm(RA, 0);
                    self.bcond(PL, Target::Label(skip));
                    self.put(0xEB00_03E0 | (RA << 16) | RA); // negs x9, x9
                    self.bcond(VS, jo);
                    self.set_label(skip);
                    self.store_payload(RA, KT, inst.c);
                    self.store_tag(KT, inst.c, TAG_INT);
                }
            }
            // `$cv = TMP`, by what the temporary holds, then the temporary
            // consumed.
            J_ASSIGN_T => {
                if m.mask == BOOL_SRC {
                    self.ldb_rc(m.kb, inst.b.wrapping_add(BOOL_BYTE));
                    self.stb_rc(m.ka, inst.a.wrapping_add(BOOL_BYTE));
                    self.store_tag(m.ka, inst.a, TAG_BOOL);
                } else if m.mask == NULL_SRC {
                    self.store_imm(m.ka, inst.a, 0);
                    self.store_tag(m.ka, inst.a, TAG_NULL);
                } else if m.mask == SCALAR_SRC {
                    self.copy_slot(m.kb, inst.b, m.ka, inst.a);
                } else if m.mask == VAL_SRC {
                    // The variable's array from the iteration before loses
                    // a handle, unless it is the last; nothing changes
                    // before the last test.
                    let store = self.synth_label();
                    self.cmp_tag(KC, inst.a, TAG_FLOAT);
                    self.bcond(LS, Target::Label(store)); // a scalar
                    self.cmp_w_imm(T0, u32::from(TAG_NONE));
                    self.bcond(EQ, Target::Label(store)); // undefined
                    self.cmp_w_imm(T0, u32::from(TAG_ARRAY));
                    self.bcond(NE, jo);
                    self.load_payload(RA, KC, inst.a);
                    self.count_dec_not_last(jo);
                    self.set_label(store);
                    self.copy_slot(m.kb, inst.b, m.ka, inst.a);
                } else {
                    self.chk_int(m.cb, inst.b, jo);
                    self.load_payload(RA, m.kb, inst.b);
                    self.store_payload(RA, m.ka, inst.a);
                    self.store_tag(m.ka, inst.a, TAG_INT);
                }
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

    // ------------------------------------------------- step 2 primitives

    /// `cmp xn, #imm` (`imm < 4096`).
    fn cmp_x_imm(&mut self, rn: u32, imm: u32) {
        self.put(0xF100_001F | ((imm & 0xFFF) << 10) | (rn << 5));
    }

    /// `ldr xd, [base + off]` / `str` of a slot's first word (the tag
    /// with the bytes after it) or any word.
    fn ld(&mut self, rd: u32, kind: u8, off: u32) {
        self.mem(true, Width::X, rd, base_reg(kind), off);
    }

    fn st(&mut self, rs: u32, kind: u8, off: u32) {
        self.mem(false, Width::X, rs, base_reg(kind), off);
    }

    /// Copies a whole slot (the tag word and the payload word).
    fn copy_slot(&mut self, from_base: u8, from: u32, to_base: u8, to: u32) {
        self.ld(RA, from_base, from);
        self.ld(RC, from_base, from.wrapping_add(PAYLOAD));
        self.st(RA, to_base, to);
        self.st(RC, to_base, to.wrapping_add(PAYLOAD));
    }

    /// The strong count of the array whose handle is in `x9`: `+ 1`.
    fn count_inc(&mut self) {
        self.put(0xF940_0000 | (RA << 5) | T0); // ldr x16, [x9]
        self.put(0x9100_0000 | (1 << 10) | (T0 << 5) | T0); // add x16, x16, #1
        self.put(0xF900_0000 | (RA << 5) | T0); // str x16, [x9]
    }

    /// The strong count of the array whose handle is in `x9`: `- 1`,
    /// leaving through `jo` (nothing changed) when it is the last.
    fn count_dec_not_last(&mut self, jo: Target) {
        self.put(0xF940_0000 | (RA << 5) | T0); // ldr x16, [x9]
        self.put(0xF100_001F | (1 << 10) | (T0 << 5)); // cmp x16, #1
        self.bcond(EQ, jo);
        self.put(0xD100_0000 | (1 << 10) | (T0 << 5) | T0); // sub x16, x16, #1
        self.put(0xF900_0000 | (RA << 5) | T0); // str x16, [x9]
    }

    /// `ldrb w11, [base + off]` / `strb w11, [base + off]`.
    fn ldb_rc(&mut self, kind: u8, off: u32) {
        self.mem(true, Width::B, RC, base_reg(kind), off);
    }

    fn stb_rc(&mut self, kind: u8, off: u32) {
        self.mem(false, Width::B, RC, base_reg(kind), off);
    }

    /// `ldr d<x>, [slot payload]`.
    fn ld_d(&mut self, x: u32, kind: u8, off: u32) {
        self.mem(true, Width::D, x, base_reg(kind), off.wrapping_add(PAYLOAD));
    }

    /// `scvtf d<x>, x<r>`: an int as a double (round to nearest).
    fn scvtf(&mut self, x: u32, r: u32) {
        self.put(0x9E62_0000 | (r << 5) | x);
    }

    /// `d<x>` = the int of slot `kind + off` (tag checked `Int`).
    fn scvtf_slot(&mut self, x: u32, kind: u8, off: u32) {
        self.load_payload(RB, kind, off);
        self.scvtf(x, RB);
    }

    /// `cbz wt, t` / `cbnz wt, t` on `w11`.
    fn cb_rc(&mut self, nz: bool, t: Target) {
        self.fixups.push((self.code.len(), Fix::I19, t));
        self.put(if nz { 0x3500_0000 } else { 0x3400_0000 } | RC);
    }

    // ------------------------------------------------------- step 2 rows

    /// Copies the compiled variable `from` into the temporary `to` when
    /// it holds a scalar or (`arr`) an array, one more handle on it.
    fn copy_cv_value(&mut self, from: u32, to: u32, arr: bool, jo: Target) {
        let copy = self.synth_label();
        if arr {
            let other = self.synth_label();
            self.cmp_tag(KC, from, TAG_ARRAY);
            self.bcond(NE, Target::Label(other));
            self.load_payload(RA, KC, from);
            self.count_inc();
            self.b(Target::Label(copy));
            self.set_label(other);
        }
        self.cmp_tag(KC, from, TAG_FLOAT);
        self.bcond(HI, jo); // not a scalar (or undefined)
        self.set_label(copy);
        self.copy_slot(KC, from, KT, to);
    }

    /// Branches on the truth of the compiled variable at `off` (see the
    /// x86 `emit_cv_truth`): every path ends in a jump.
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
            self.cmp_tag(KC, off, TAG_ARRAY);
            self.bcond(NE, Target::Label(other));
            self.load_payload(RA, KC, off);
            self.mem(true, Width::W, T0, RA, ARRAY_COUNT); // ldr w16, [x9 + count]
            self.cbnz_w(T0, on_true);
            self.b(on_false);
            self.set_label(other);
        }
        let not_bool = self.synth_label();
        self.cmp_tag(KC, off, TAG_BOOL);
        self.bcond(NE, Target::Label(not_bool));
        self.mem(true, Width::B, T0, CVS, off.wrapping_add(BOOL_BYTE));
        self.cbnz_w(T0, on_true);
        self.b(on_false);
        self.set_label(not_bool);
        let not_int = self.synth_label();
        self.cmp_tag(KC, off, TAG_INT);
        self.bcond(NE, Target::Label(not_int));
        self.load_payload(T0, KC, off);
        self.cbnz_x(T0, on_true);
        self.b(on_false);
        self.set_label(not_int);
        self.cmp_tag(KC, off, TAG_NULL);
        self.bcond(NE, jo);
        self.b(on_false);
    }

    /// One `C_MOVE` row (`Op::Move` into the temporary `inst.c`).
    fn emit_move(&mut self, m: JitMeta, inst: &Inst, jo: Target) {
        match m.sc {
            KC => self.copy_cv_value(inst.a, inst.c, m.arr, jo),
            KT => {
                if m.ca {
                    self.cmp_tag(KT, inst.a, TAG_FLOAT);
                    self.bcond(HI, jo);
                }
                self.copy_slot(KT, inst.a, KT, inst.c);
                if m.consume && inst.a != inst.c {
                    self.store_imm(KT, inst.a, 0);
                    self.store_tag(KT, inst.a, TAG_NULL);
                }
            }
            _ => match m.arith {
                CONST_NULL => {
                    self.store_imm(KT, inst.c, 0);
                    self.store_tag(KT, inst.c, TAG_NULL);
                }
                CONST_BOOL => {
                    self.put(0x5280_0000 | (u32::from(m.lit_c != 0) << 5) | RC); // mov w11, #b
                    self.stb_rc(KT, inst.c.wrapping_add(BOOL_BYTE));
                    self.store_tag(KT, inst.c, TAG_BOOL);
                }
                _ => {
                    self.store_imm(KT, inst.c, m.lit_c);
                    self.store_tag(KT, inst.c, TAG_INT);
                }
            },
        }
    }

    /// `$cv op= v` (`+ - *`) by the operands' tags: two ints as ints, a
    /// float variable with a float or an int as doubles; anything else
    /// the interpreter's.
    fn emit_assign_ops(&mut self, m: JitMeta, inst: &Inst, i: usize) {
        let side = Target::Side(i);
        let slot = |k: u8| k == KC || k == KT;
        let (a_other, done) = (self.synth_label(), self.synth_label());
        self.cmp_tag(KC, inst.a, TAG_INT);
        self.bcond(NE, Target::Label(a_other));
        if slot(m.kb) {
            self.cmp_tag(m.kb, inst.b, TAG_INT);
            self.bcond(NE, side); // the variable becomes a float
        }
        self.load_payload(RA, KC, inst.a);
        self.operand(RB, m.kb, inst.b, m.lit_b);
        self.arith(m.arith.min(2), side);
        self.store_payload(RA, KC, inst.a);
        self.b(Target::Label(done));
        self.set_label(a_other);
        self.cmp_tag(KC, inst.a, TAG_FLOAT);
        self.bcond(NE, side);
        self.ld_d(0, KC, inst.a);
        if slot(m.kb) {
            let (b_int, op) = (self.synth_label(), self.synth_label());
            self.cmp_tag(m.kb, inst.b, TAG_FLOAT);
            self.bcond(NE, Target::Label(b_int));
            self.ld_d(1, m.kb, inst.b);
            self.b(Target::Label(op));
            self.set_label(b_int);
            self.cmp_tag(m.kb, inst.b, TAG_INT);
            self.bcond(NE, side);
            self.scvtf_slot(1, m.kb, inst.b);
            self.set_label(op);
        } else {
            self.operand(RA, m.kb, inst.b, m.lit_b);
            self.scvtf(1, RA);
        }
        // fadd / fsub / fmul d0, d0, d1
        self.put([0x1E61_2800, 0x1E61_3800, 0x1E61_0800][usize::from(m.arith.min(2))]);
        self.mem(false, Width::D, 0, CVS, inst.a.wrapping_add(PAYLOAD));
        self.set_label(done);
    }

    /// A comparison and its jump by the operands' tags (see the x86
    /// `emit_cmps`): two ints as ints, a float with a float or an int as
    /// doubles (`fcmp`: unordered sets `V`, tested first), anything else
    /// the interpreter's.
    fn emit_cmps(&mut self, m: JitMeta, inst: &Inst, i: usize, j: usize) {
        use crate::opcode::cmp_outcome::UNORDERED;
        let side = Target::Side(i);
        let tgt = self.jump_tgt(j);
        let slot = |k: u8| k == KC || k == KT;
        let (a_other, b_other) = (self.synth_label(), self.synth_label());
        let (floats, done) = (self.synth_label(), self.synth_label());
        if slot(m.ka) {
            self.cmp_tag(m.ka, inst.a, TAG_INT);
            self.bcond(NE, Target::Label(a_other));
        }
        if slot(m.kb) {
            self.cmp_tag(m.kb, inst.b, TAG_INT);
            self.bcond(NE, Target::Label(b_other));
        }
        self.operand(RA, m.ka, inst.a, m.lit_a);
        self.operand(RB, m.kb, inst.b, m.lit_b);
        self.cmp_x(RA, RB);
        if let Some(cc) = jcc_for(m.mask) {
            self.bcond(cond_of(cc), tgt);
        }
        self.b(Target::Label(done));
        if slot(m.kb) {
            self.set_label(b_other);
            self.cmp_tag(m.kb, inst.b, TAG_FLOAT);
            self.bcond(NE, side);
            if slot(m.ka) {
                self.scvtf_slot(0, m.ka, inst.a);
            } else {
                self.operand(RA, m.ka, inst.a, m.lit_a);
                self.scvtf(0, RA);
            }
            self.ld_d(1, m.kb, inst.b);
            self.b(Target::Label(floats));
        }
        if slot(m.ka) {
            self.set_label(a_other);
            self.cmp_tag(m.ka, inst.a, TAG_FLOAT);
            self.bcond(NE, side);
            self.ld_d(0, m.ka, inst.a);
            if slot(m.kb) {
                let b_int = self.synth_label();
                self.cmp_tag(m.kb, inst.b, TAG_FLOAT);
                self.bcond(NE, Target::Label(b_int));
                self.ld_d(1, m.kb, inst.b);
                self.b(Target::Label(floats));
                self.set_label(b_int);
                self.cmp_tag(m.kb, inst.b, TAG_INT);
                self.bcond(NE, side);
                self.scvtf_slot(1, m.kb, inst.b);
            } else {
                self.operand(RA, m.kb, inst.b, m.lit_b);
                self.scvtf(1, RA);
            }
        }
        self.set_label(floats);
        self.put(0x1E61_2000); // fcmp d0, d1
        if inst.k & UNORDERED != 0 {
            self.bcond(VS, tgt);
        } else {
            self.bcond(VS, Target::Label(done));
        }
        match inst.k & 7 {
            1 => self.bcond(MI, tgt), // less
            2 => self.bcond(EQ, tgt),
            3 => self.bcond(LS, tgt), // less or equal
            4 => self.bcond(HI, tgt), // greater
            5 => self.bcond(NE, tgt),
            6 => self.bcond(HS, tgt), // greater or equal
            7 => self.b(tgt),
            _ => {}
        }
        self.set_label(done);
    }

    // ------------------------------------------------- helper calls

    /// `xd = xn + imm` (`add`, no flags), through `T0` past 12 bits.
    fn add_imm(&mut self, rd: u32, rn: u32, imm: u32) {
        if imm < 4096 {
            self.put(0x9100_0000 | (imm << 10) | (rn << 5) | rd);
        } else {
            self.mov_imm(T0, i64::from(imm));
            self.put(0x8B00_0000 | (T0 << 16) | (rn << 5) | rd);
        }
    }

    /// One helper argument into `x<reg>`.
    fn helper_arg(&mut self, reg: u32, arg: HArg) {
        match arg {
            HArg::Slot(base, off) => self.add_imm(reg, base_reg(base), off),
            HArg::Abs(v) => self.mov_imm(reg, v),
            HArg::Null => self.mov_reg(reg, ZR),
            HArg::Frame => self.mov_reg(reg, FRAME),
        }
    }

    /// The call of `helper` as `extern "C" fn(args[0], args[1], args[2],
    /// word)`: its answer is in `w0`. Everything the trace keeps is in
    /// callee-saved registers, so nothing is saved around the call; `sp`
    /// is 16-byte aligned (the prologue's 64-byte frame).
    fn emit_helper_call(&mut self, helper: usize, args: [HArg; 3], word: u32) {
        for (k, arg) in (0u32..).zip(args) {
            self.helper_arg(k, arg);
        }
        self.mov_imm(3, i64::from(word));
        self.mov_imm(T0, i64::try_from(helper).unwrap_or(0));
        self.put(0xD63F_0000 | (T0 << 5)); // blr x16
    }

    /// A helper that answers 0 (done) or anything else (declined, nothing
    /// changed: leave through `fail`).
    fn emit_helper(&mut self, helper: usize, args: [HArg; 3], word: u32, fail: Target) {
        self.emit_helper_call(helper, args, word);
        self.cbnz_w(0, fail);
    }

    /// `w0` = its low byte (a `bool`/`u8` answer: AAPCS64 leaves the bits
    /// above it unspecified).
    fn uxtb_w0(&mut self) {
        self.put(0x1200_1C00); // and w0, w0, #0xff
    }

    /// A tape handler's call for a row it does or declines (`br`: a
    /// `Br` answer, 1 declined; else a `bool`, false
    /// declined).
    fn emit_row(&mut self, handler: usize, br: bool, args: [HArg; 3], fail: Target) {
        self.emit_helper_call(handler, args, 0);
        self.uxtb_w0();
        if br {
            self.cmp_w_imm(0, 1);
            self.bcond(EQ, fail);
        } else {
            self.fixups.push((self.code.len(), Fix::I19, fail));
            self.put(0x3400_0000); // cbz w0
        }
    }

    /// The address of row `i` of the tape the trace is of.
    fn row_arg(&self, i: usize) -> HArg {
        let row = self.tbase.wrapping_add(i * std::mem::size_of::<Inst>());
        HArg::Abs(i64::try_from(row).unwrap_or(0))
    }

    /// The `new` row whose result the next row assigns (see the x86
    /// `emit_new_row`): 0 declined, 1 on to the assignment, 2 on past it.
    fn emit_new_row(&mut self, m: JitMeta, i: usize, j: usize) {
        let row = self.row_arg(i);
        self.emit_helper_call(
            usize::try_from(m.lit_a).unwrap_or(0),
            [HArg::Frame, row, HArg::Null],
            0,
        );
        self.uxtb_w0();
        let next = i + self.plan.span[j] as usize;
        let past = if next < self.plan.close {
            Target::Label(next + 1)
        } else {
            Target::Stub(next + 1)
        };
        let (over, made) = (self.synth_label(), self.synth_label());
        self.cmp_w_imm(0, 1);
        self.bcond(LO, Target::Side(i));
        self.bcond(HI, Target::Label(over));
        self.poll(Target::Poll(next, i));
        self.b(Target::Label(made));
        self.set_label(over);
        self.poll(Target::Poll(next + 1, i));
        self.b(past);
        self.set_label(made);
    }

    // ---------------------------------------------------------- arrays

    /// `x9 = x9 + (x10 << 4)`: the element at index `x10` of the
    /// 16-byte elements at `x9`.
    fn elem_addr(&mut self) {
        self.mem(true, Width::X, RA, RA, LIST_ELEMS);
        self.put(0x8B00_0000 | (RB << 16) | (SLOT_SHIFT << 10) | (RA << 5) | RA);
    }

    /// The offset of an array row into `x10` (`false`: none to read).
    fn dim_index(&mut self, m: JitMeta, slow: Target) -> bool {
        match m.kb {
            KC | KT => {
                self.cmp_tag(m.kb, m.slot, TAG_INT);
                self.bcond(NE, slow);
                self.load_payload(RB, m.kb, m.slot);
                true
            }
            KL => {
                // SAFETY: a `KL` offset of an array row is the address of a
                // literal of the op array the plan was made for
                // (`plan_dim`), alive as long as the trace.
                let lit = unsafe { &*(m.lit_b as *const php_runtime::Value) };
                let php_runtime::Value::Int(at) = lit else {
                    return false;
                };
                self.mov_imm(RB, *at);
                true
            }
            _ => false,
        }
    }

    /// `x9` = the table of the array the variable `m.sb` holds, directly
    /// or through a reference (`write`: no borrow at all; else none being
    /// written); else to `slow`.
    fn dim_table(&mut self, m: JitMeta, write: bool, slow: Target) {
        let direct = self.synth_label();
        self.load_payload(RA, KC, m.sb);
        self.cmp_tag(KC, m.sb, TAG_ARRAY);
        self.bcond(EQ, Target::Label(direct));
        self.cmp_w_imm(T0, u32::from(TAG_REF));
        self.bcond(NE, slow);
        self.mem(true, Width::X, T0, RA, REF_BORROW);
        if write {
            self.cbnz_x(T0, slow);
        } else {
            self.cmp_x_imm(T0, 0);
            self.bcond(LT, slow); // being written
        }
        self.mem(true, Width::B, T0, RA, REF_VALUE);
        self.cmp_w_imm(T0, u32::from(TAG_ARRAY));
        self.bcond(NE, slow);
        self.mem(true, Width::X, RA, RA, REF_VALUE + PAYLOAD);
        self.set_label(direct);
        // A list.
        self.mov_imm(RC, LIST_PACKED);
        self.mem(true, Width::X, T0, RA, LIST_KIND);
        self.cmp_x(T0, RC);
        self.bcond(NE, slow);
    }

    /// The x86 `emit_list_read`, the same cases and order.
    fn emit_list_read(&mut self, m: JitMeta, slow: usize, int_only: bool) -> bool {
        let slow = Target::Label(slow);
        if !self.dim_index(m, slow) {
            return false;
        }
        self.dim_table(m, false, slow);
        self.mem(true, Width::X, T0, RA, LIST_LEN);
        self.cmp_x(RB, T0);
        self.bcond(HS, slow); // unsigned: a negative offset is outside
        self.elem_addr();
        self.mem(true, Width::B, T0, RA, 0);
        if int_only {
            self.cmp_w_imm(T0, u32::from(TAG_INT));
            self.bcond(NE, slow);
        } else {
            self.cmp_w_imm(T0, u32::from(TAG_FLOAT));
            self.bcond(HI, slow);
        }
        self.cmp_tag(m.kc, m.db, TAG_FLOAT);
        self.bcond(HI, slow);
        self.mem(true, Width::X, RC, RA, 0);
        self.mem(true, Width::X, RB, RA, PAYLOAD);
        self.st(RC, m.kc, m.db);
        self.store_payload(RB, m.kc, m.db);
        true
    }

    /// The x86 `emit_list_write`, the same cases and order.
    fn emit_list_write(&mut self, m: JitMeta, slow: usize, done: usize, int_only: bool) {
        use php_runtime::Value;
        let slow = Target::Label(slow);
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
        if !matches!(m.kb, KC | KT | KL) || !self.dim_index(m, slow) {
            return;
        }
        if lit_words.is_none() {
            if int_only {
                self.cmp_tag(m.sc, m.sw, TAG_INT);
                self.bcond(NE, slow);
            } else {
                self.cmp_tag(m.sc, m.sw, TAG_FLOAT);
                self.bcond(HI, slow);
            }
        }
        let (store, append) = (self.synth_label(), self.synth_label());
        self.dim_table(m, true, slow);
        // The table is this handle's alone.
        self.mem(true, Width::X, T0, RA, 0);
        self.cmp_x_imm(T0, 1);
        self.bcond(NE, slow);
        self.mem(true, Width::X, T0, RA, 8);
        self.cmp_x_imm(T0, 1);
        self.bcond(NE, slow);
        self.mem(true, Width::X, T0, RA, LIST_LEN);
        self.cmp_x(RB, T0);
        self.bcond(HS, Target::Label(append));
        self.elem_addr();
        self.mem(true, Width::B, T0, RA, 0);
        self.cmp_w_imm(T0, u32::from(TAG_FLOAT));
        self.bcond(HI, slow);
        self.set_label(store);
        if let Some((first, second)) = lit_words {
            self.mov_imm(RC, first);
            self.mov_imm(RB, second);
        } else {
            self.ld(RC, m.sc, m.sw);
            self.load_payload(RB, m.sc, m.sw);
        }
        self.mem(false, Width::X, RC, RA, 0);
        self.mem(false, Width::X, RB, RA, PAYLOAD);
        self.b(Target::Label(done));
        // The next element: the flags are the length test's.
        self.set_label(append);
        self.bcond(NE, slow);
        self.mem(true, Width::X, T0, RA, LIST_CAP);
        self.cmp_x(RB, T0);
        self.bcond(HS, slow);
        self.mem(true, Width::W, T0, RA, TABLE_SIZE);
        self.cmp_x(RB, T0);
        self.bcond(HS, slow);
        self.put(0x9100_0000 | (1 << 10) | (RB << 5) | RC); // add x11, x10, #1
        self.mem(false, Width::X, RC, RA, LIST_LEN);
        self.mem(false, Width::X, RC, RA, TABLE_NEXT);
        self.mem(true, Width::W, T0, RA, ARRAY_COUNT);
        self.put(0x1100_0000 | (1 << 10) | (T0 << 5) | T0); // add w16, w16, #1
        self.mem(false, Width::W, T0, RA, ARRAY_COUNT);
        self.elem_addr();
        self.b(Target::Label(store));
    }

    fn dim_arg(m: JitMeta) -> HArg {
        match m.kb {
            KSKIP => HArg::Null,
            KL => HArg::Abs(m.lit_b),
            k => HArg::Slot(k, m.slot),
        }
    }

    /// One array element read (`C_DIMR`), as the x86 `emit_dimr`.
    fn emit_dimr(&mut self, m: JitMeta, i: usize) {
        if m.arr {
            let (slow, done) = (self.synth_label(), self.synth_label());
            if self.emit_list_read(m, slow, false) {
                self.b(Target::Label(done));
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
        let (slow, done) = (self.synth_label(), self.synth_label());
        if !m.inc && self.emit_list_read(m, slow, true) {
            self.b(Target::Label(done));
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

    /// One array element write or compound write (`C_DIMW`/`C_DIMOP`),
    /// as the x86 `emit_dimw`, ending with the row's poll.
    fn emit_dimw(&mut self, m: JitMeta, i: usize, helper: usize, word: u32) {
        let val = if m.sc == KL {
            HArg::Abs(m.lit_c)
        } else {
            HArg::Slot(m.sc, m.sw)
        };
        let (slow, done) = (self.synth_label(), self.synth_label());
        if helper == jit_dim_set as *const () as usize {
            self.emit_list_write(m, slow, done, !m.arr);
        }
        self.set_label(slow);
        if m.arr {
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
            self.store_imm(KT, m.sw, 0);
            self.store_tag(KT, m.sw, TAG_NULL);
        }
        self.poll(Target::Poll(i + 1, i));
    }

    /// One [`C_GREAD`] row, as the x86 `emit_gread`.
    fn emit_gread(&mut self, m: JitMeta, i: usize) {
        let side = Target::Side(i);
        self.mov_imm(RA, self.gtable + 8 * i64::from(m.slot));
        self.mem(true, Width::X, RB, RA, 0);
        self.cbz_x(RB, side); // not resolved
        self.mem(true, Width::B, RC, RB, 0); // the tag
        let known = self.synth_label();
        self.cmp_w_imm(RC, u32::from(TAG_ARRAY));
        self.bcond(EQ, Target::Label(known));
        self.cmp_w_imm(RC, u32::from(TAG_FLOAT));
        self.bcond(HI, side);
        self.set_label(known);
        if m.kc == KC {
            let store = self.synth_label();
            self.cmp_tag(KC, m.db, TAG_FLOAT);
            self.bcond(LS, Target::Label(store));
            self.cmp_w_imm(T0, u32::from(TAG_NONE));
            self.bcond(EQ, Target::Label(store));
            self.cmp_w_imm(T0, u32::from(TAG_ARRAY));
            self.bcond(NE, side);
            self.load_payload(RA, KC, m.db);
            self.count_dec_not_last(side);
            self.set_label(store);
        }
        let copy = self.synth_label();
        self.cmp_w_imm(RC, u32::from(TAG_ARRAY));
        self.bcond(NE, Target::Label(copy));
        self.mem(true, Width::X, RA, RB, PAYLOAD);
        self.count_inc();
        self.set_label(copy);
        self.mem(true, Width::X, RA, RB, 0);
        self.mem(true, Width::X, RC, RB, PAYLOAD);
        self.st(RA, m.kc, m.db);
        self.store_payload(RC, m.kc, m.db);
    }

    // ------------------------------------------------------ properties

    /// `cset wd, eq` (`csinc wd, wzr, wzr, ne`).
    fn cset_eq(&mut self, rd: u32) {
        self.put(0x1A9F_17E0 | rd);
    }

    /// `mov wd, #v` (`v < 65536`).
    fn mov_w(&mut self, rd: u32, v: u32) {
        self.put(0x5280_0000 | ((v & 0xFFFF) << 5) | rd);
    }

    /// A frame field must hold the word `want` (0: be null), else `fail`.
    fn frame_word_guard(&mut self, field: u32, want: i64, fail: Target) {
        self.mem(true, Width::X, RA, FRAME, field);
        if want == 0 {
            self.cbnz_x(RA, fail);
        } else {
            self.mov_imm(RC, want);
            self.cmp_x(RA, RC);
            self.bcond(NE, fail);
        }
    }

    /// `x10` = the frame's `$this` handle, leaving through `fail` when
    /// there is none.
    fn this_rb(&mut self, fail: Target) {
        self.mem(true, Width::X, RB, FRAME, THIS_FIELD);
        self.cbz_x(RB, fail);
    }

    /// `x10` = the raw `Rc<Class>` field word of the object whose handle
    /// is in `x10`.
    fn obj_class_rb(&mut self) {
        self.mem(
            true,
            Width::X,
            RB,
            RB,
            OBJ_PAYLOAD.wrapping_add(CLASS_FIELD),
        );
    }

    /// `x10` = the handle of a receiver the trace already checked.
    fn load_receiver_rb(&mut self, kind: u8, off: u32) {
        if kind == KTHIS {
            self.mem(true, Width::X, RB, FRAME, THIS_FIELD);
        } else {
            self.load_payload(RB, KC, off);
        }
    }

    /// The x86 `emit_prop_slots`: on fall-through `x9` is the slot array.
    fn emit_prop_slots(&mut self, m: JitMeta, jo: Target) {
        if m.bare {
            self.load_receiver_rb(m.ka, m.sb);
            self.mem(true, Width::X, RA, RB, SLOTS_FIRST);
            return;
        }
        if m.ka == KTHIS {
            self.this_rb(jo);
        } else {
            self.cmp_tag(KC, m.sb, TAG_OBJECT);
            self.bcond(NE, jo);
            self.load_payload(RB, KC, m.sb);
        }
        self.frame_word_guard(SCOPE_FIELD, m.lit_b, jo);
        self.mem(
            true,
            Width::X,
            RA,
            RB,
            OBJ_PAYLOAD.wrapping_add(CLASS_FIELD),
        );
        self.mov_imm(RC, m.lit_a);
        self.cmp_x(RA, RC);
        self.bcond(NE, jo); // another class
        self.obj_state_checks(&[], m.slot.wrapping_add(1), jo);
        self.mem(true, Width::X, RA, RB, SLOTS_FIRST);
    }

    /// The receiver in `x10`: its `lazy` cell unborrowed and empty, its
    /// `slots` cell unborrowed and at least `len` long.
    fn obj_state_checks(&mut self, _slots: &[u32], len: u32, jo: Target) {
        for off in [
            OBJ_LAZY.wrapping_add(REFCELL_BORROW),
            OBJ_LAZY.wrapping_add(REFCELL_VALUE),
            SLOTS_BORROW,
        ] {
            self.mem(true, Width::X, T0, RB, off);
            self.cbnz_x(T0, jo);
        }
        self.mem(true, Width::X, RA, RB, SLOTS_LEN);
        self.mov_imm(T0, i64::from(len));
        self.cmp_x(RA, T0);
        self.bcond(LO, jo); // a slot past the array
    }

    /// The `Int` check of the slot (`x9` the slot array).
    fn emit_prop_int_slot(&mut self, m: JitMeta, jo: Target) {
        if m.bare {
            return;
        }
        self.mem(true, Width::B, T0, RA, m.slot << SLOT_SHIFT);
        self.cmp_w_imm(T0, u32::from(TAG_INT));
        self.bcond(NE, jo);
    }

    /// The value a property row writes into `x11` (the x86
    /// `prop_src_rcx`).
    fn prop_src_rc(&mut self, m: JitMeta, jo: Target) {
        match m.sc {
            KT => {
                self.cmp_tag(KT, m.sw, TAG_INT);
                self.bcond(NE, jo);
                self.load_payload(RC, KT, m.sw);
            }
            KC | KR => self.load_payload(RC, m.sc, m.sw),
            _ => self.mov_imm(RC, m.lit_c),
        }
    }

    fn emit_prop(&mut self, m: JitMeta, jo: Target) {
        self.emit_prop_slots(m, jo);
        self.emit_prop_int_slot(m, jo);
        let so = m.slot << SLOT_SHIFT;
        self.mem(true, Width::X, RA, RA, so.wrapping_add(PAYLOAD));
        self.store_payload(RA, m.kd, m.db);
        self.store_tag(m.kd, m.db, TAG_INT);
    }

    fn emit_propw(&mut self, m: JitMeta, jo: Target) {
        self.emit_prop_slots(m, jo);
        self.emit_prop_int_slot(m, jo);
        let so = m.slot << SLOT_SHIFT;
        self.prop_src_rc(m, jo);
        if m.db != NO_DST {
            self.store_payload(RC, m.kd, m.db);
            self.store_tag(m.kd, m.db, TAG_INT);
        }
        self.mem(false, Width::X, RC, RA, so.wrapping_add(PAYLOAD));
        if m.consume {
            self.store_imm(m.sc, m.sw, 0);
            self.store_tag(m.sc, m.sw, TAG_NULL);
        }
    }

    /// `x11 = x11 ± 1` setting the flags.
    fn step_rc(&mut self, inc: bool) {
        let op = if inc { 0xB100_0000 } else { 0xF100_0000 };
        self.put(op | (1 << 10) | (RC << 5) | RC);
    }

    /// `++`/`--` of the `Int` at `[x9 + at]` in place: the old value in
    /// `x10` for a post op (`post`), the new one in `x11`; the result
    /// temporary of `m.db` (base `kd`) gets the expression's value.
    fn incdec_at(&mut self, m: JitMeta, at: u32, post: bool, kd: u8, jo: Target) {
        self.mem(true, Width::X, RC, RA, at);
        if post {
            self.mov_reg(RB, RC);
        }
        self.step_rc(m.inc);
        self.bcond(VS, jo); // overflow, before any store
        self.mem(false, Width::X, RC, RA, at);
        if m.db != NO_DST {
            self.store_payload(if post { RB } else { RC }, kd, m.db);
            self.store_tag(kd, m.db, TAG_INT);
        }
    }

    fn emit_propinc(&mut self, m: JitMeta, jo: Target) {
        self.emit_prop_slots(m, jo);
        self.emit_prop_int_slot(m, jo);
        let so = m.slot << SLOT_SHIFT;
        self.incdec_at(m, so.wrapping_add(PAYLOAD), m.kc != 0, m.kd, jo);
    }

    /// `isset()`/`empty()` (`m.inc`) of the value at `[x9 + at]` by its
    /// tag (`Int`, `Null`, `Bool` inline, anything else `jo`), the clean
    /// `Bool` into the temporary `m.db`.
    fn isset_at(&mut self, m: JitMeta, at: u32, jo: Target) {
        let (l_int, l_null, l_bool, l_end) = (
            self.synth_label(),
            self.synth_label(),
            self.synth_label(),
            self.synth_label(),
        );
        self.mem(true, Width::B, T0, RA, at);
        self.cmp_w_imm(T0, u32::from(TAG_INT));
        self.bcond(EQ, Target::Label(l_int));
        self.cmp_w_imm(T0, u32::from(TAG_NULL));
        self.bcond(EQ, Target::Label(l_null));
        self.cmp_w_imm(T0, u32::from(TAG_BOOL));
        self.bcond(EQ, Target::Label(l_bool));
        self.b(jo);
        self.set_label(l_int);
        if m.inc {
            self.mem(true, Width::X, RC, RA, at.wrapping_add(PAYLOAD));
            self.cmp_x_imm(RC, 0);
            self.cset_eq(RC);
        } else {
            self.mov_w(RC, 1);
        }
        self.b(Target::Label(l_end));
        self.set_label(l_null);
        self.mov_w(RC, u32::from(m.inc));
        self.b(Target::Label(l_end));
        self.set_label(l_bool);
        if m.inc {
            self.mem(true, Width::B, T0, RA, at.wrapping_add(BOOL_BYTE));
            self.cmp_w_imm(T0, 0);
            self.cset_eq(RC);
        } else {
            self.mov_w(RC, 1);
        }
        self.set_label(l_end);
        self.stb_rc(KT, m.db.wrapping_add(BOOL_BYTE));
        self.store_tag(KT, m.db, TAG_BOOL);
    }

    fn emit_propisset(&mut self, m: JitMeta, jo: Target) {
        self.emit_prop_slots(m, jo);
        self.isset_at(m, m.slot << SLOT_SHIFT, jo);
    }

    /// `xd = xd op xm` for the compound writes (`+ - *` exit `jo` on
    /// overflow before any store; `& | ^` cannot fail).
    fn opa(&mut self, arith: u8, rd: u32, rm: u32, jo: Target) {
        let rrr = |op: u32| op | (rm << 16) | (rd << 5) | rd;
        match arith {
            0 => {
                self.put(rrr(0xAB00_0000));
                self.bcond(VS, jo);
            }
            1 => {
                self.put(rrr(0xEB00_0000));
                self.bcond(VS, jo);
            }
            2 => {
                self.put(0x9B00_7C00 | (rm << 16) | (rd << 5) | T0);
                self.put(0x9B40_7C00 | (rm << 16) | (rd << 5) | T1);
                self.put(0xEB80_0000 | (T0 << 16) | (63 << 10) | (T1 << 5) | ZR);
                self.bcond(NE, jo);
                self.mov_reg(rd, T0);
            }
            OPA_BITAND => self.put(rrr(0x8A00_0000)),
            OPA_BITOR => self.put(rrr(0xAA00_0000)),
            _ => self.put(rrr(0xCA00_0000)),
        }
    }

    fn emit_propopa(&mut self, m: JitMeta, jo: Target) {
        self.emit_prop_slots(m, jo);
        self.emit_prop_int_slot(m, jo);
        let so = m.slot << SLOT_SHIFT;
        self.prop_src_rc(m, jo);
        self.mem(true, Width::X, RB, RA, so.wrapping_add(PAYLOAD));
        self.opa(m.arith, RB, RC, jo);
        self.mem(false, Width::X, RB, RA, so.wrapping_add(PAYLOAD));
        if m.db != NO_DST {
            self.store_payload(RB, m.kd, m.db);
            self.store_tag(m.kd, m.db, TAG_INT);
        }
        if m.consume {
            self.store_imm(m.sc, m.sw, 0);
            self.store_tag(m.sc, m.sw, TAG_NULL);
        }
    }

    // ------------------------------------------- static properties

    fn emit_sprop_guards(&mut self, m: JitMeta, jo: Target) {
        if m.sg {
            self.frame_word_guard(SCOPE_FIELD, m.lit_b, jo);
        }
        if m.cg {
            self.frame_word_guard(CALLED_FIELD, m.wc, jo);
        }
    }

    /// `x9` = the pinned cell, which must be unborrowed.
    fn emit_sprop_cell(&mut self, m: JitMeta, jo: Target) {
        self.mov_imm(RA, m.lit_a);
        self.mem(true, Width::X, T0, RA, 0);
        self.cbnz_x(T0, jo);
    }

    /// The cell's value must be an `Int`.
    fn sprop_int(&mut self, jo: Target) {
        self.mem(true, Width::B, T0, RA, REFCELL_VALUE);
        self.cmp_w_imm(T0, u32::from(TAG_INT));
        self.bcond(NE, jo);
    }

    fn emit_sprop(&mut self, m: JitMeta, jo: Target) {
        self.emit_sprop_guards(m, jo);
        self.emit_sprop_cell(m, jo);
        self.sprop_int(jo);
        self.mem(true, Width::X, RA, RA, REFCELL_VALUE.wrapping_add(PAYLOAD));
        self.store_payload(RA, KT, m.db);
        self.store_tag(KT, m.db, TAG_INT);
    }

    fn emit_spropw(&mut self, m: JitMeta, jo: Target) {
        self.emit_sprop_guards(m, jo);
        self.emit_sprop_cell(m, jo);
        self.sprop_int(jo);
        match m.sc {
            KT => {
                self.cmp_tag(KT, m.sw, TAG_INT);
                self.bcond(NE, jo);
                self.load_payload(RC, KT, m.sw);
            }
            KC => self.load_payload(RC, KC, m.sw),
            _ => self.mov_imm(RC, m.lit_c),
        }
        if m.db != NO_DST {
            self.store_payload(RC, KT, m.db);
            self.store_tag(KT, m.db, TAG_INT);
        }
        self.mem(false, Width::X, RC, RA, REFCELL_VALUE.wrapping_add(PAYLOAD));
        if m.consume {
            self.store_imm(KT, m.sw, 0);
            self.store_tag(KT, m.sw, TAG_NULL);
        }
    }

    fn emit_spropinc(&mut self, m: JitMeta, jo: Target) {
        self.emit_sprop_guards(m, jo);
        self.emit_sprop_cell(m, jo);
        self.sprop_int(jo);
        self.incdec_at(m, REFCELL_VALUE.wrapping_add(PAYLOAD), m.kc != 0, KT, jo);
    }

    fn emit_spropisset(&mut self, m: JitMeta, jo: Target) {
        self.emit_sprop_guards(m, jo);
        self.emit_sprop_cell(m, jo);
        self.isset_at(m, REFCELL_VALUE, jo);
    }

    fn emit_consti(&mut self, m: JitMeta, jo: Target) {
        self.emit_sprop_guards(m, jo);
        if m.ka == KTHIS {
            self.this_rb(jo);
            self.obj_class_rb();
            self.mov_imm(RA, m.lit_c);
            self.cmp_x(RA, RB);
            self.bcond(NE, jo); // another class
        }
        match m.arith {
            CONST_NULL => {
                self.store_imm(KT, m.db, 0);
                self.store_tag(KT, m.db, TAG_NULL);
            }
            CONST_BOOL => {
                self.mov_w(RC, u32::from(m.lit_a != 0));
                self.stb_rc(KT, m.db.wrapping_add(BOOL_BYTE));
                self.store_tag(KT, m.db, TAG_BOOL);
            }
            _ => {
                self.store_imm(KT, m.db, m.lit_a);
                self.store_tag(KT, m.db, TAG_INT);
            }
        }
    }

    // ----------------------------------------------------------- calls

    /// An inlined call, as the x86 `emit_call`.
    fn emit_call(&mut self, cp: &CallPlan) {
        let side = Target::Side(cp.callove);
        if let Some((rk, roff, cls_addr)) = cp.recv {
            if rk == KTHIS {
                self.this_rb(side);
            } else {
                self.cmp_tag(rk, roff, TAG_OBJECT);
                self.bcond(NE, side);
                self.load_payload(RB, rk, roff);
            }
            self.obj_class_rb();
            #[allow(clippy::cast_possible_wrap)]
            self.mov_imm(RA, cls_addr.wrapping_sub(u64::from(RC_VALUE_OFFSET)) as i64);
            self.cmp_x(RA, RB);
            self.bcond(NE, side); // another class
            self.frame_word_guard(SCOPE_FIELD, cp.recv_scope, side);
        }
        if let Some((scope, called, needs_this)) = cp.rel {
            self.frame_word_guard(SCOPE_FIELD, scope, side);
            if let Some(called) = called {
                self.frame_word_guard(CALLED_FIELD, called, side);
            }
            if needs_this {
                self.this_rb(side);
            }
        }
        if let Some((off, word)) = cp.callee_obj {
            if let Some(o) = &cp.callee_pin {
                self.pins.push((word, o.clone()));
            }
            self.cmp_tag(KC, off, TAG_OBJECT);
            self.bcond(NE, side);
            self.load_payload(RB, KC, off);
            #[allow(clippy::cast_possible_wrap)]
            self.mov_imm(RA, word as i64);
            self.cmp_x(RA, RB);
            self.bcond(NE, side); // another object
        }
        if let (false, Some((rk, roff, _))) = (cp.prop_slots.is_empty(), cp.recv) {
            self.load_receiver_rb(rk, roff);
            let top = cp.prop_slots.iter().copied().max().unwrap_or(0);
            self.obj_state_checks(&cp.prop_slots, top.wrapping_add(1), side);
            self.mem(true, Width::X, RA, RB, SLOTS_FIRST);
            for &slot in &cp.prop_slots {
                self.mem(true, Width::B, T0, RA, slot << SLOT_SHIFT);
                self.cmp_w_imm(T0, u32::from(TAG_INT));
                self.bcond(NE, side);
            }
        }
        for (n, arg) in cp.sends.iter().enumerate() {
            if arg.kind == KSKIP {
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            let off = cp.base.wrapping_add((n as u32) << SLOT_SHIFT);
            self.chk_int(arg.chk, arg.off, side);
            self.operand(RB, arg.kind, arg.off, arg.lit);
            self.store_payload(RB, KR, off);
            self.store_tag(KR, off, TAG_INT);
        }
        for k in 0..cp.classes.len() {
            let (c, m) = (cp.classes[k], cp.metas[k]);
            match c {
                C_PROP => self.emit_prop(m, side),
                C_PROPW => self.emit_propw(m, side),
                C_PROPINC => self.emit_propinc(m, side),
                C_PROPOPA => self.emit_propopa(m, side),
                _ => self.emit_op(c, m, &cp.rows[k], Target::Disarm(cp.callove)),
            }
        }
        if cp.ret.kind == KQUICK {
            if let Some(d) = cp.dst {
                self.emit_helper(
                    jit_quick_tail as *const () as usize,
                    [HArg::Slot(KR, cp.ret.off), HArg::Slot(KT, d), HArg::Null],
                    u32::try_from(cp.ret.lit).unwrap_or(u32::MAX),
                    side,
                );
            }
            return;
        }
        if let Some(d) = cp.dst {
            match cp.ret.kind {
                KNULL_ARG => {
                    self.store_imm(KT, d, 0);
                    self.store_tag(KT, d, TAG_NULL);
                }
                KR => {
                    self.load_payload(RB, KR, cp.ret.off);
                    self.store_payload(RB, KT, d);
                    self.store_tag(KT, d, TAG_INT);
                }
                KL => {
                    self.store_imm(KT, d, cp.ret.lit);
                    self.store_tag(KT, d, TAG_INT);
                }
                _ => {}
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
            self.mem(true, Width::X, T0, T0, 0);
            #[allow(clippy::cast_possible_wrap)]
            self.mov_imm(T1, self.plan.version as i64);
            self.cmp_x(T0, T1);
            self.bcond(NE, Target::Replan(self.plan.head));
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
                if self.plan.class[j] == C_CALL {
                    // Each call plan is consumed once.
                    match self.plan.calls[j].take() {
                        Some(cp) => self.emit_call(&cp),
                        None => self.b(Target::Side(head + j)),
                    }
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
        let (out, _) = run(r"<?php
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
");
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

    /// Floats, bools, nulls, the truth jumps, `?:`, `??`, `abs()` and the
    /// increments that keep their result, compiled, give the interpreter's
    /// answers (`BUZZS_JIT=0`).
    #[test]
    fn scalar_rows_run_compiled() {
        // One function per loop: a refused loop gives its whole tape up.
        let (out, _) = run(r"<?php
function f0() {
  $f = 0.5;
  for ($i = 0; $i < 5000; $i++) { $f += $i; $f -= 0.25; $f *= 1.0; }
  var_dump($f);
}
f0();
function f1() {
  $g = 1.5; $n = 0;
  for ($i = 0; $i < 5000; $i++) { if ($g < $i) { $n++; } if ($i > 2.5) { $n += 2; } if ($g == 1.5) { $n--; } }
  var_dump($n);
}
f1();
function f2() {
  $nan = NAN; $c = 0;
  for ($i = 0; $i < 3000; $i++) { if ($nan < $i) $c++; if ($nan != $i) $c += 10; if ($nan >= 1.0) $c += 100; }
  var_dump($c);
}
f2();
function f3() {
  $t = 0; $b = true;
  for ($i = 0; $i < 5000; $i++) { $b = !$b; if ($b) { $t++; } $x = $i ?: 7; $t += $x; }
  var_dump($t);
}
f3();
function f4() {
  $u = null; $s = 0;
  for ($i = 0; $i < 5000; $i++) { $v = $u ?? 3; $s += $v; $w = ($i & 1) ? 1 : 2; $s += $w; }
  var_dump($s);
}
f4();
function f5() {
  $a = 0; $r = 0;
  for ($i = -2500; $i < 2500; $i++) { $r += abs($i); $p = $a++; $q = ++$a; $r += $q - $p; $z = $a--; }
  var_dump($r, $a);
}
f5();
function f6() {
  $m = PHP_INT_MIN; $k = 0;
  for ($i = 0; $i < 3000; $i++) { $y = abs($m); }
  var_dump($y);
}
f6();
function f7() {
  $arr = [1,2]; $e = []; $cnt = 0;
  for ($i = 0; $i < 4000; $i++) { if ($arr) $cnt++; if ($e) $cnt += 100; $copy = $arr ?: $e; }
  var_dump($cnt, $copy);
}
f7();
function f8() {
  $x = 0; $y = 5;
  for ($i = 0; $i < 4000; $i++) { $x = $y = $i; }
  var_dump($x, $y);
}
f8();
function f9() {
  $h = 0;
  for ($i = 0; $i < 4000; $i++) { $h = $i % 3 == 0 ? $h + 1 : $h; $j = isset($h) ? 1 : 0; $h += $j; }
  var_dump($h);
}
f9();
function f10() {
  $fl = 1;
  for ($i = 0; $i < 4000; $i++) { $fl = $fl * 1.5; if ($fl > 1e300) $fl = 1; }
  var_dump($fl);
}
f10();
");
        assert_eq!(out, "float(12496250.5)\nint(9992)\nint(30000)\nint(12500007)\nint(22500)\nint(6260000)\nint(5000)\nfloat(9.223372036854776E+18)\nint(4000)\narray(2) {\n  [0]=>\n  int(1)\n  [1]=>\n  int(2)\n}\nint(3999)\nint(3999)\nint(5334)\nfloat(1.76207894137449E+104)\n");
    }

    /// Array element reads and writes (in place and through the helpers),
    /// handler rows and global reads, compiled, give the interpreter's
    /// answers (`BUZZS_JIT=0`).
    #[test]
    fn array_and_helper_rows_run_compiled() {
        let (out, _) = run(r"<?php
function a0() { $a = []; for ($i = 0; $i < 5000; $i++) { $a[] = $i * 2; } $s = 0; for ($i = 0; $i < 5000; $i++) { $s += $a[$i]; } var_dump($s, count($a)); }
a0();
function a1() { $a = []; for ($i = 0; $i < 1000; $i++) { $a[] = 1; } for ($i = 0; $i < 5000; $i++) { $a[$i % 1000] += $i; } $s = 0; foreach ($a as $v) { $s += $v; } var_dump($s); }
a1();
function a2() { $a = [1.5, 2, null, true]; $t = 0; for ($i = 0; $i < 4000; $i++) { $v = $a[$i & 3]; if ($v) $t++; } var_dump($t); }
a2();
function a3() { $a = ['x' => 1, 'y' => 2]; $t = 0; for ($i = 0; $i < 4000; $i++) { $t += $a['x'] + $a['y']; $a['x'] = $i; } var_dump($t, $a); }
a3();
function a4() { $m = [[1,2],[3,4]]; $t = 0; for ($i = 0; $i < 4000; $i++) { $t += $m[$i & 1][1]; } var_dump($t); }
a4();
function a5() { $s = ''; for ($i = 0; $i < 3000; $i++) { $s .= 'ab'; } var_dump(strlen($s)); }
a5();
function a6() { $a = [5]; $r = &$a; $t = 0; for ($i = 0; $i < 4000; $i++) { $t += $a[0]; $a[0] = $i; } var_dump($t, $r); }
a6();
function a7() { $a = []; for ($i = 0; $i < 3000; $i++) { $a[$i] = $i; $b = $a; } var_dump(count($b), $b[2999]); }
a7();
$g = 5; $GLOBALS['h'] = 1;
function a8() { $t = 0; for ($i = 0; $i < 4000; $i++) { $t += $GLOBALS['g']; } var_dump($t); }
a8();
function a9() { $t = 0; for ($i = 0; $i < 4000; $i++) { $o = new stdClass; $o->x = $i; $t += $o->x; } var_dump($t); }
a9();
function a10() { $a = [1,2,3]; $t = 0; for ($i = 0; $i < 4000; $i++) { $t += $a[$i] ?? 7; if (isset($a[$i % 5])) $t++; } var_dump($t); }
a10();
function a11() { $a = []; for ($i = 0; $i < 4000; $i++) { $a[$i] = $i * 0.5; } var_dump($a[3999]); $k = 'k'; $b = []; for ($i = 0; $i < 4000; $i++) { $b[$k . ($i % 10)] = $i; } var_dump(count($b)); }
a11();
function a12() { $a = [0]; for ($i = 0; $i < 4000; $i++) { $a[0] = PHP_INT_MAX; $a[0] += 1; } var_dump($a); }
a12();
");
        assert_eq!(out, "int(24995000)\nint(5000)\nint(12498500)\nint(3000)\nint(8002002)\narray(2) {\n  [\"x\"]=>\n  int(3999)\n  [\"y\"]=>\n  int(2)\n}\nint(12000)\nint(6000)\nint(7994006)\narray(1) {\n  [0]=>\n  int(3999)\n}\nint(3000)\nint(2999)\nint(20000)\nint(7998000)\nint(30385)\nfloat(1999.5)\nint(10)\narray(1) {\n  [0]=>\n  float(9.223372036854776E+18)\n}\n");
    }

    /// Property, static-property and constant rows and inlined calls
    /// (functions, methods, closures), compiled, give the interpreter's
    /// answers (`BUZZS_JIT=0`).
    #[test]
    fn object_rows_and_calls_run_compiled() {
        let (out, _) = run(r"<?php
class P { public $x = 0; public $y = 1; protected $z = 2; public static $s = 0; const K = 7;
  function inc() { $this->x++; return $this->x; }
  function addz($v) { $this->z += $v; return $this->z; }
  function run() { $t = 0; for ($i = 0; $i < 4000; $i++) { $this->x = $i; $t += $this->x + $this->y; $this->y++; $t += self::K; if (isset($this->x)) $t++; } return $t; }
  static function srun() { $t = 0; for ($i = 0; $i < 4000; $i++) { self::$s++; $t += self::$s; static::$s = static::$s + 1; if (isset(self::$s)) $t++; } return $t; }
}
function sq($a) { return $a * $a; }
function add3($a, $b, $c) { return $a + $b + $c; }
function nothing($a) { return; }
function c0() { $p = new P; var_dump($p->run(), $p->x, $p->y); }
c0();
function c1() { var_dump(P::srun(), P::$s); }
c1();
function c2() { $t = 0; for ($i = 0; $i < 4000; $i++) { $t += sq($i) + add3($i, 1, 2); } var_dump($t); }
c2();
function c3() { $p = new P; $t = 0; for ($i = 0; $i < 4000; $i++) { $t += $p->inc(); $t += $p->addz(1); } var_dump($t); }
c3();
function c4() { $f = function ($a) { return $a + 1; }; $t = 0; for ($i = 0; $i < 4000; $i++) { $t += $f($i); } var_dump($t); }
c4();
function c5() { $p = new P; $t = 0; for ($i = 0; $i < 4000; $i++) { $p->x += 3; $p->x -= 1; $p->x *= 1; $p->x |= 0; } var_dump($p->x); }
c5();
function c6() { $p = new P; $p->x = PHP_INT_MAX - 2000; for ($i = 0; $i < 4000; $i++) { $p->x++; } var_dump($p->x); }
c6();
function c7() { $ps = [new P, new P]; $t = 0; for ($i = 0; $i < 4000; $i++) { $q = $ps[$i & 1]; $q->x = $i; $t += $q->x; } var_dump($t); }
c7();
function c8() { $t = 0; for ($i = 0; $i < 4000; $i++) { $r = nothing($i); if ($r === null) $t++; $t += P::K + PHP_INT_SIZE; } var_dump($t); }
c8();
function c9() { $t = 0; for ($i = 0; $i < 4000; $i++) { $t += abs(sq($i % 7) - 20); } var_dump($t); }
c9();
");
        assert_eq!(out, "int(16032000)\nint(3999)\nint(4001)\nint(16004000)\nint(8000)\nint(21333344000)\nint(16012000)\nint(8002000)\nint(8000)\nfloat(9.223372036854776E+18)\nint(7998000)\nint(64000)\nint(52016)\n");
    }
}

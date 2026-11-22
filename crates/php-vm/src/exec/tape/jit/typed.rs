// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The second tier: a numeric loop compiled for the types its variables
//! have when it is first reached, with their values in registers
//! (ref: `ext/opcache/jit/zend_jit_trace.c` for the idea of compiling for
//! observed types behind guards; the unit here is a loop, not a trace).
//!
//! The first tier ([`super`]) tests the type of every operand of every
//! instruction and loads it from its slot, because any instruction may be
//! where the interpreter hands over. A loop is different: it is entered at
//! its head, and what its numeric instructions compute has the same types
//! on every round as long as its inputs do. So, the first time the
//! first-tier code reaches a loop head, the types in the frame are looked
//! at ([`analyse`]'s `snapshot`), and if every instruction of the loop is a
//! numeric one whose operand types follow from them, the loop is compiled
//! again as a function of its own ([`emit`]):
//!
//! * it starts with one guard per slot the loop uses (the tag the snapshot
//!   saw, or just "owns nothing" for a slot that is only written);
//! * past the guards no type is tested: an int `+` is an add and an
//!   overflow test, a comparison is a compare and a branch;
//! * a slot's value is read from an SSA variable (a register), never
//!   loaded again;
//! * every write still goes to the slot as well (the payload, and the tag
//!   when the type changes), so the frame is exact at every instruction
//!   and leaving the loop needs no write-back: an int overflow, the loop's
//!   exit and a due safe point just return, naming the instruction the
//!   first tier (or the interpreter) goes on at.
//!
//! The first-tier code calls the function at the loop head and goes on
//! where it says ([`WHY_GENERIC`], [`WHY_NOT_TYPED`]).
//!
//! What a typed loop may be made of:
//!
//! * the tape's numeric instructions, the scalar family's (`%`, `/`,
//!   shifts, bit operations, comparisons into a bool, truth jumps), and,
//!   planned from their ops, what has no tape instruction: casts to int
//!   and float, moves (a ternary's result), `a ?? b`, bools as values
//!   (`a && b`, `!a`), a jump on a literal (`while (1)`), an assignment
//!   used as an expression, `abs()`, the math builtins of one argument,
//!   `intdiv()`, `min()` and `max()` ([`Analysis::math`]). A builtin's call
//!   with computed arguments makes no call at all: its arguments are kept
//!   in variables until the `DoCall` ([`Plan::Arg`]), and if the loop gives
//!   up in between, the whole call is redone by the interpreter
//!   ([`Loop::resume`]). A constant or class constant that holds a number
//!   or a bool is read as that value, checked again at the loop's entry
//!   ([`Loop::consts`]). A `return` leaves the loop;
//! * array element reads and writes (`isset`, `op=`, `++`, nested ones and
//!   a string literal's key included), object property reads, writes and
//!   `isset`, `count()`, `strlen()`, type tests and `.=`, as calls of their
//!   family handlers on the exact
//!   frame ([`Plan::Call`]). The array's, object's or string's variable is
//!   not tracked; what the handler leaves in a tracked slot is read back
//!   behind a test of its type, which was guessed from what the array or
//!   property holds when the loop is compiled. An int offset into a
//!   packed array of numbers, a number under a string key and a declared
//!   property on a property cache hit take a short cut through a small
//!   helper ([`Fast`]): a write falls back on its handler in place, a read that
//!   misses is run outside the loop and, if it keeps missing, compiled as
//!   the handler's call ([`WHY_MISS`]);
//! * the step of a `foreach` over an array of numbers ([`Plan::Each`]);
//! * static property reads, writes and `isset`, which have no tape
//!   instruction, through the interpreter's own cached fast path
//!   ([`Plan::Ext`]);
//! * a call by name of a small plain user function whose body is made of
//!   the first group's instructions, inline ([`Inlined`], [`Plan::Bind`]):
//!   the function's instructions become instructions of the loop, after
//!   the tape's own, on variables that are in no frame. It has no frame
//!   to stop in: whatever gives up inside it has the whole call redone by
//!   the interpreter, from its `InitFCall`.
//!
//! A loop whose types stop holding is compiled once more for the types
//! met then; a loop the analysis cannot type is looked at a few times
//! (its variables may settle after the first round) and then left to the
//! first tier, or, with that off, to the interpreter (`super::retire`).

// One arm a plan kind, one function a concern, here as in the tape: an
// arm merged with its neighbour, or a table split to fit a line count,
// would be harder to check against the op it stands for.
#![allow(
    clippy::match_same_arms,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::option_option,
    clippy::single_match_else
)]

use super::super::{Inst, TOp, KC, KI, KT};
use super::{
    float_test, int_cc, mem, off, rec_at, Arith, FloatTest, Shape, BOOL_AT, PAYLOAD_AT, REC_AT,
    REC_FLAG, REC_TAPE, SLOT, TAG_BOOL, TAG_FLOAT, TAG_INT, TAG_LAST_SCALAR, TAG_NULL, WHY_BITS,
};
use crate::opcode::{
    cmp_outcome, ArgSrc, CastKind, Op, OpArray, Operand, Place, PlaceBase, Seg, UnKind,
};
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{
    types, AbiParam, Block, InstBuilder, MemFlags, SigRef, Signature, Value as V,
};
use cranelift_frontend::{FunctionBuilder, Variable};
use php_runtime::Value;
use std::collections::BTreeMap;

/// Reason a typed loop returns with: the first-tier code goes on at the
/// instruction named (the loop was left, or an int overflowed there).
pub(super) const WHY_GENERIC: u8 = 5;
/// Reason a typed loop returns with: its types do not hold (a guard at its
/// entry, or a variable that changed type by the end of a round); the
/// first-tier code runs the round itself, from the loop head.
pub(super) const WHY_NOT_TYPED: u8 = 6;

/// Reason a typed loop returns with: a short cut ([`Fast`], the step of a
/// `foreach`) did not take its case at the instruction named, and nothing
/// changed. The first tier (or the interpreter) runs that instruction and
/// goes on; a loop that says this often is compiled again without short
/// cuts (`super::missed`).
pub(super) const WHY_MISS: u8 = 7;

/// The most instructions a typed loop may have.
const MAX_REGION: usize = 256;
/// The most instruction visits the type analysis makes before giving up.
const MAX_VISITS: usize = 16 * MAX_REGION;

/// A frame slot: a compiled variable or a temporary, by byte offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct SlotRef {
    pub(super) tmp: bool,
    pub(super) at: i32,
}

/// What is known of a slot at some point of the loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Ty {
    Int,
    Float,
    Bool,
    /// It owns nothing (null, a bool, a number of either type, or an
    /// undefined variable): it may be written over, not read as a number.
    Plain,
}

/// The types of the slots that differ from the loop's entry types (a slot
/// that is absent has its entry type).
type State = BTreeMap<SlotRef, Ty>;

/// Where a number comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Num {
    Slot(SlotRef),
    Int(i64),
    Float(f64),
}

/// A numeric operand and its type.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Opnd {
    num: Num,
    float: bool,
}

/// What a jump on a value's truth tests: a number (true when not zero), a
/// bool in a slot, or a truth known now.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Truthy {
    Num(Opnd),
    Bool(SlotRef),
    Known(bool),
}

/// How a comparison decides its jump.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Test {
    Int(IntCC),
    Float(FloatTest),
    Always(bool),
}

/// An operator on two ints with an int result (the scalar family's
/// `NUM_OPS`, `exec/tape/scalar.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IntOp {
    /// `intdiv()`.
    Div,
    Mod,
    Shl,
    Shr,
    Or,
    And,
    Xor,
}

impl IntOp {
    /// The operator with code `e` in a scalar-family instruction, when it
    /// is one of these.
    fn of(e: u8) -> Option<IntOp> {
        Some(match e {
            1 => IntOp::Mod,
            2 => IntOp::Shl,
            3 => IntOp::Shr,
            4 => IntOp::Or,
            5 => IntOp::And,
            6 => IntOp::Xor,
            _ => return None,
        })
    }

    /// Whether the int `y` is a right operand the fast path takes
    /// whatever the left one (`scalar::int_result`): not a zero divisor,
    /// not `-1` (`PHP_INT_MIN % -1` is the generic op's), a shift of 0 to
    /// 63 bits.
    fn takes(self, y: i64) -> bool {
        match self {
            IntOp::Div | IntOp::Mod => y != 0 && y != -1,
            IntOp::Shl | IntOp::Shr => (0..64).contains(&y),
            IntOp::Or | IntOp::And | IntOp::Xor => true,
        }
    }
}

/// How the comparison with operator code `e` of a scalar-family
/// instruction decides, for operands that are floats or not
/// (`exec::num_bin`: an int with a float compares as floats, except under
/// `===` / `!==`, where they differ); `None` for the other operators.
fn compare_test(e: u8, a_float: bool, b_float: bool) -> Option<Test> {
    let float = |cc| Test::Float(FloatTest::Cc { cc, negate: false });
    let (ints, mixed) = (!a_float && !b_float, a_float != b_float);
    Some(match e {
        7 if ints => Test::Int(IntCC::SignedLessThan),
        8 if ints => Test::Int(IntCC::SignedLessThanOrEqual),
        9 | 11 if ints => Test::Int(IntCC::Equal),
        10 | 12 if ints => Test::Int(IntCC::NotEqual),
        7 => float(FloatCC::LessThan),
        8 => float(FloatCC::LessThanOrEqual),
        11 if mixed => Test::Always(false),
        12 if mixed => Test::Always(true),
        9 | 11 => float(FloatCC::Equal),
        10 | 12 => float(FloatCC::NotEqual),
        _ => return None,
    })
}

/// The operator code of `/` in a scalar-family instruction.
const E_DIV: u8 = 0;

/// A function of one float with a float result, as
/// `call::float_builtin_eval` computes it: an instruction of the machine,
/// or a call of the function at the address ([`math_fn`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Math {
    /// `abs()` of a float.
    Abs,
    Sqrt,
    Floor,
    Ceil,
    Lib(usize),
}

/// A math function as the code calls it.
type MathFn = extern "C" fn(f64) -> f64;

/// The functions of `call::float_builtin_eval` that are not one
/// instruction, each behind the C signature the code calls: the same Rust
/// function the interpreter's fast path evaluates, so the results are the
/// same to the bit.
fn math_fn(name: &[u8]) -> Option<Math> {
    use php_stdstr::math as m;
    macro_rules! lib {
        ($($n:literal => $f:expr,)*) => {
            match name {
                b"sqrt" => Some(Math::Sqrt),
                b"floor" => Some(Math::Floor),
                b"ceil" => Some(Math::Ceil),
                $( $n => {
                    extern "C" fn call(x: f64) -> f64 {
                        ($f)(x)
                    }
                    Some(Math::Lib(call as MathFn as usize))
                } )*
                _ => None,
            }
        };
    }
    lib! {
        b"sin" => f64::sin,
        b"cos" => f64::cos,
        b"tan" => f64::tan,
        b"atan" => f64::atan,
        b"exp" => f64::exp,
        b"log" => f64::ln,
        b"log10" => f64::log10,
        b"asin" => f64::asin,
        b"acos" => f64::acos,
        b"sinh" => f64::sinh,
        b"cosh" => f64::cosh,
        b"tanh" => f64::tanh,
        b"asinh" => m::asinh,
        b"acosh" => m::acosh,
        b"atanh" => m::atanh,
        b"expm1" => f64::exp_m1,
        b"log1p" => f64::ln_1p,
        b"deg2rad" => m::deg2rad,
        b"rad2deg" => m::rad2deg,
        b"round" => |x| php_support::fmt::php_round(x, 0, php_support::fmt::RoundingMode::HalfUp),
    }
}

/// Whether the op `op`, which has no tape instruction, is of a kind the
/// second tier may have a form for (decided for good by
/// [`Analysis::op_plan`], with the types): a move of a number, a cast to
/// int or float, a frameless call of a math function. A loop with such ops
/// is compiled for the second tier's sake alone (`super::install`).
pub(super) fn plannable_op(op: &Op, lits: &[Value]) -> bool {
    match op {
        Op::Move { src, .. } => !matches!(src, Operand::Unused),
        Op::Cast {
            kind: CastKind::Int | CastKind::Float,
            ..
        } => true,
        Op::FramelessICall(call) => {
            let Some(Value::Str(name)) = lits.get(call.name as usize) else {
                return false;
            };
            call.dst.is_some() && computed(name.as_bytes(), usize::from(call.argc))
        }
        // A call by a name that cannot be a namespaced function, with
        // computed arguments: its `InitFCall`, its `Send`s and its
        // `DoCall`. One of those math functions' is computed; a user
        // function's may run inline ([`Inlined`]), which only shows when
        // the loop is looked at (the function need not exist yet).
        Op::InitFCall { fallback: None, .. }
        | Op::Send {
            src: ArgSrc::Val(_) | ArgSrc::CallResult(_) | ArgSrc::Place(_),
        }
        | Op::DoCall { .. }
        | Op::Abs { .. }
        // Bools as values: the middle of `a && b`, `!a`, `(bool) a`; a
        // jump on a literal (`while (1)`); an assignment used as an
        // expression; and a `return`, where the loop is left.
        | Op::JmpZEx { .. }
        | Op::JmpNzEx { .. }
        | Op::Unary {
            op: UnKind::Not | UnKind::Bool,
            ..
        }
        | Op::JmpZ(Operand::Lit(_), _)
        | Op::JmpNz(Operand::Lit(_), _)
        | Op::AssignCv { dst: Some(_), .. }
        // `a ?? b` of a temporary or a variable.
        | Op::Coalesce { .. }
        // A constant, read as its value.
        | Op::FetchConst { fallback: None, .. }
        | Op::FetchConstTo { fallback: None, .. }
        | Op::FetchClassConst {
            name: Operand::Lit(_),
            ..
        }
        | Op::FetchClassConstTo { .. }
        // A static property, through the interpreter's cached fast path.
        | Op::FetchStaticProp {
            mode: crate::opcode::FetchMode::Read,
            ..
        }
        | Op::AssignStaticProp { dst: None, .. }
        | Op::IssetStaticProp { .. }
        | Op::Return { .. } => true,
        _ => false,
    }
}

/// Whether the second tier computes the builtin `name` called with `argc`
/// arguments itself, given numbers of the right types ([`Analysis::math`]).
fn computed(name: &[u8], argc: usize) -> bool {
    match (name, argc) {
        (b"intdiv" | b"min" | b"max", 2) | (b"abs", 1) => true,
        (name, 1) => math_fn(name).is_some(),
        _ => false,
    }
}

/// The most arguments of a call the second tier computes (a builtin's)
/// or runs inline (a user function's).
const MAX_ARGS: usize = 4;
/// How deep such calls may nest (`f($a, g($b))`): each depth has its own
/// argument variables, [`ARG_VARS`] in all.
const MAX_DEPTH: usize = 3;
const ARG_VARS: usize = MAX_ARGS * MAX_DEPTH;

/// Where the variables of an inlined user function live: below the
/// argument variables ([`arg_slot`]), by the slot's offset in the frame
/// the function would have had. Like those, they are no slots of any
/// frame: nothing is ever loaded from or stored to them.
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
const VIRTUAL_AT: i32 = SLOT as i32 * 16;

/// The variable of an inlined function's slot `slot` ([`VIRTUAL_AT`]).
fn virtual_slot(slot: SlotRef) -> SlotRef {
    SlotRef {
        tmp: slot.tmp,
        at: -(VIRTUAL_AT + slot.at),
    }
}

/// The inlined function's slot a variable stands for, when it is one.
fn real_slot(slot: SlotRef) -> Option<SlotRef> {
    (slot.at <= -VIRTUAL_AT).then(|| SlotRef {
        tmp: slot.tmp,
        at: -slot.at - VIRTUAL_AT,
    })
}

/// The most instructions of a user function the second tier inlines, and
/// the room each inlined call takes in the loop's instruction numbers.
pub(super) const INLINE_OPS: usize = 48;
const INLINE_SPAN: usize = 64;

/// What an inlined function returns ([`Analysis::returned`]).
#[derive(Clone, Copy)]
enum Returned {
    Num(Opnd),
    Bool(Truthy),
}

/// Whether a parameter or return type `ty` takes a value of type `of` as
/// it is: no declaration, or one that names the value's own type (an int
/// given to a `float` is converted, which is not done here).
fn takes(ty: Option<&crate::types::TypeDecl>, of: Ty) -> bool {
    use crate::types::mask;
    let Some(ty) = ty else {
        return true;
    };
    let need = match of {
        Ty::Int => mask::LONG,
        Ty::Float => mask::DOUBLE,
        Ty::Bool => mask::BOOL,
        Ty::Plain => return false,
    };
    ty.mask & need == need
}

/// A call of a user function that a typed loop runs inline: the call's
/// ops in the loop, and the function's code with a tape of its own (the
/// function's may hold entry instructions of the JIT). Its instructions
/// are the loop's instructions `base..`, after the last of the loop's own.
pub(super) struct Inlined {
    /// The call's `InitFCall` and `DoCall`.
    init: usize,
    call: usize,
    /// The literal of the function's name at the call.
    name: u32,
    base: usize,
    /// How many arguments the call passes, and the function's first
    /// instruction for that many.
    argc: usize,
    first: usize,
    func: std::rc::Rc<crate::func::UserFunction>,
    tape: Box<[Inst]>,
}

/// The variable the `k`-th argument of a builtin call is kept in between
/// its `Send` and the `DoCall`: not a slot of the frame (the call has no
/// frame here), so nothing is ever loaded from or stored to it.
fn arg_slot(k: usize) -> SlotRef {
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    SlotRef {
        tmp: true,
        at: -(SLOT as i32) * (k as i32 + 1),
    }
}

/// A builtin call in the ops: its `InitFCall`, the `Send`s of its
/// arguments and its `DoCall`.
#[derive(Clone, Debug)]
struct Site {
    init: usize,
    name: u32,
    sends: Vec<usize>,
    call: usize,
}

/// The calls of `ops` that are an `InitFCall` without a namespace
/// fallback, plain `Send`s and a `DoCall`, nested ones included.
fn call_sites(ops: &[Op]) -> Vec<Site> {
    let mut open: Vec<Site> = Vec::new();
    let mut sites = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        match op {
            Op::InitFCall {
                name,
                fallback: None,
                ..
            } => open.push(Site {
                init: i,
                name: *name,
                sends: Vec::new(),
                call: 0,
            }),
            Op::Send { .. } => {
                if let Some(site) = open.last_mut() {
                    site.sends.push(i);
                }
            }
            Op::DoCall { .. } => {
                if let Some(mut site) = open.pop() {
                    site.call = i;
                    sites.push(site);
                }
            }
            _ => {}
        }
    }
    sites
}

/// An instruction of the loop with its types resolved.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Plan {
    Jmp {
        to: usize,
    },
    Cmp {
        a: Opnd,
        b: Opnd,
        test: Test,
        taken: usize,
        fall: usize,
    },
    /// `++`/`--` of an int variable fused with the branch on it.
    IncCmp {
        slot: SlotRef,
        inc: bool,
        bound: Opnd,
        cc: IntCC,
        taken: usize,
        fall: usize,
    },
    /// `dst = a op b`: floats when either operand is one.
    Arith {
        op: Arith,
        a: Opnd,
        b: Opnd,
        dst: SlotRef,
        next: usize,
    },
    /// `dst = a op b` on two ints, for the operators of [`IntOp`].
    Int2 {
        op: IntOp,
        a: Opnd,
        b: Opnd,
        dst: SlotRef,
        next: usize,
    },
    /// `dst = a / b` with a float among the operands.
    Div {
        a: Opnd,
        b: Opnd,
        dst: SlotRef,
        next: usize,
    },
    /// `dst = (float) a` or `dst = (int) a`; a temporary operand is left
    /// null.
    Conv {
        to_float: bool,
        a: Opnd,
        consume: Option<SlotRef>,
        dst: SlotRef,
        next: usize,
    },
    /// `dst = f(a)` for a math function of one float ([`Math`]).
    Math {
        f: Math,
        a: Opnd,
        dst: SlotRef,
        next: usize,
    },
    /// `dst = abs(a)` of an int (`PHP_INT_MIN` is the generic op's).
    AbsInt {
        a: Opnd,
        dst: SlotRef,
        next: usize,
    },
    /// An argument of a builtin call the loop computes itself: kept in the
    /// call's `k`-th argument variable ([`arg_slot`]) until its `DoCall`.
    Arg {
        k: usize,
        value: Opnd,
        consume: Option<SlotRef>,
        next: usize,
    },
    /// `dst = max(a, b)` or `min(a, b)` of two ints.
    Pick {
        max: bool,
        a: Opnd,
        b: Opnd,
        dst: SlotRef,
        next: usize,
    },
    /// A jump on the truth of a number: taken when it is `when`. With
    /// `consume`, the number is in that temporary, which is left null.
    Truth {
        value: Truthy,
        when: bool,
        consume: Option<SlotRef>,
        taken: usize,
        fall: usize,
    },
    /// `dst = a <op> b` for a comparison of two numbers: a bool.
    Compare {
        a: Opnd,
        b: Opnd,
        test: Test,
        dst: SlotRef,
        next: usize,
    },
    /// `dst = (bool) value`, or with `negate` `!value`; a temporary
    /// operand is left null.
    SetBool {
        value: Truthy,
        negate: bool,
        consume: Option<SlotRef>,
        dst: SlotRef,
        next: usize,
    },
    /// `JmpZEx` / `JmpNzEx` (the middle of `a && b`, `a || b` as values):
    /// when the value's truth is `when`, that bool goes to `dst` and the
    /// jump is taken; else nothing is stored.
    JmpEx {
        value: Truthy,
        when: bool,
        consume: Option<SlotRef>,
        dst: SlotRef,
        target: usize,
        fall: usize,
    },
    /// `cv = value` with the value also going to a temporary (an
    /// assignment used as an expression).
    Assign2 {
        value: Opnd,
        consume: Option<SlotRef>,
        cv: SlotRef,
        tmp: SlotRef,
        next: usize,
    },
    /// The call of a user function the loop runs inline, at its
    /// `DoCall`: the arguments kept since their `Send`s ([`Plan::Arg`])
    /// become the function's parameters, its other variables are
    /// undefined (`cvs` variables and `tmps` temporaries in all), and the
    /// loop goes on at the function's first instruction, `to`.
    Bind {
        /// The first of its argument variables ([`Analysis::arg_base`]).
        args: usize,
        argc: usize,
        cvs: usize,
        tmps: usize,
        to: usize,
    },
    /// The loop is left for good here (a `return`): the interpreter's.
    Leave,
    /// A family instruction run by its own handler, on the frame (which is
    /// exact). What the handler leaves in a slot the loop tracks is read
    /// back from it.
    Call(Call),
    /// The step of a `foreach` over an array of numbers (`super::fe_fetch`):
    /// the loop goes on at `full` when the next element was fetched and
    /// assigned as expected (its value, and its key when the loop has one,
    /// read back into the slots named), at `end` when there is none; any
    /// other answer is the first tier's.
    Each {
        /// Whether the short cut is tried (`super::each_num`).
        slim: bool,
        /// The iterator's index.
        iter: u32,
        end: usize,
        full: usize,
        value: (SlotRef, bool),
        key: Option<SlotRef>,
        /// The temporaries the element went through.
        consumed: [Option<SlotRef>; 2],
    },
    /// An op without a tape instruction that the interpreter's own fast
    /// path runs on the frame alone (`hot::step_ext`: a static property's
    /// read or write on a cache hit). It goes on at `next` when the op
    /// did as expected (a read's number is then in `result`, read back
    /// behind a test of its type); it is the interpreter's op when the
    /// fast path declined, having changed nothing, and the first tier's
    /// from where it got to otherwise (a read that went into its
    /// temporary, not into the variable of the `AssignCv` after it).
    Ext {
        result: Option<(SlotRef, Ty)>,
        /// The temporary the op consumes (left null by it).
        consume: Option<SlotRef>,
        /// The instruction after the op itself.
        after: usize,
        next: usize,
    },
    /// `++`/`--`; with `copy`, the new value (or with `true` the old one)
    /// also goes to a temporary.
    Step {
        slot: SlotRef,
        inc: bool,
        float: bool,
        copy: Option<(SlotRef, bool)>,
        next: usize,
    },
    Const {
        dst: SlotRef,
        value: Opnd,
        next: usize,
    },
    /// `dst = src`; with `consume`, the source (a temporary) is left null.
    Copy {
        src: SlotRef,
        dst: SlotRef,
        float: bool,
        consume: bool,
        next: usize,
    },
}

/// A family instruction in a typed loop ([`Plan::Call`]).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Call {
    /// The address of its handler (`super::Handler`).
    handler: usize,
    /// Whether a safe point is polled after it.
    polls: bool,
    /// Where the loop goes on when the handler answers "done" (1); `None`
    /// when the first tier takes over there.
    fall: Option<usize>,
    /// The instruction after the ones it covers.
    after: usize,
    /// Where the loop goes on when the handler answers "jumped" (2), for
    /// a handler that may.
    jump: Option<usize>,
    /// The slot the handler leaves a number in, and whether a float is
    /// expected there (an int otherwise): read back, behind a test of its
    /// tag, on the way the loop goes on.
    result: Option<(SlotRef, Ty)>,
    /// Temporaries the handler may consume: they own nothing afterwards,
    /// and nothing more is known of them.
    consumed: [Option<SlotRef>; MAX_CONSUMED],
    /// A short cut tried before the handler.
    fast: Option<Fast>,
}

/// The commonest array accesses, done by a small helper in place of the
/// family handler (`dims::jit_get_num`, `dims::jit_set_num`): an int
/// offset the loop has in a register, into a packed array of numbers. The
/// handler runs when the helper does not take the case.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Fast {
    /// A read into the call's result slot.
    Get {
        /// The array variable's offset.
        array: i32,
        index: Opnd,
    },
    /// A write of a number over an existing element, or as the next one.
    Set {
        array: i32,
        index: Opnd,
        value: Opnd,
        /// The temporary the value came from, left null.
        consume: Option<SlotRef>,
    },
    /// `$cv[index] op= value` on an existing number (`dims::jit_op_num`);
    /// `op` is 0 for `+`, 1 for `-`, 2 for `*`.
    Op {
        array: i32,
        index: Opnd,
        op: usize,
        value: Opnd,
        consume: Option<SlotRef>,
    },
    /// A read of the number under a string literal's key (the variable's
    /// and the literal's indexes) of the array a variable holds
    /// (`dims::jit_get_key`).
    GetKey { cv: u32, lit: u32 },
    /// A read of the number in a declared property (the place's index)
    /// on a property cache hit (`spec::jit_prop_get`).
    PropGet { place: u32 },
    /// A write of a number into one, over a scalar (`spec::jit_prop_set`).
    PropSet {
        place: u32,
        value: Opnd,
        consume: Option<SlotRef>,
    },
    /// `op=` of a number on one (`spec::jit_prop_op`; `op` as
    /// [`Fast::Op`]'s, and 3 for `++`, 4 for `--`).
    PropOp {
        place: u32,
        op: usize,
        value: Opnd,
        consume: Option<SlotRef>,
    },
    /// [`Fast::Get`] of `$cv[i][j]`.
    Get2 { array: i32, i: Opnd, j: Opnd },
    /// [`Fast::Set`] of `$cv[i][j]`, over an existing element.
    Set2 {
        array: i32,
        i: Opnd,
        j: Opnd,
        value: Opnd,
        consume: Option<SlotRef>,
    },
}

/// The most temporaries a family instruction of a typed loop may consume
/// (the offsets of a nested access and its value).
const MAX_CONSUMED: usize = 4;

impl Opnd {
    /// The operand of an inlined function's instruction, in the loop.
    fn inlined(self) -> Opnd {
        match self.num {
            Num::Slot(slot) => Opnd {
                num: Num::Slot(virtual_slot(slot)),
                float: self.float,
            },
            _ => self,
        }
    }
}

impl Truthy {
    /// As [`Opnd::inlined`].
    fn inlined(self) -> Truthy {
        match self {
            Truthy::Num(o) => Truthy::Num(o.inlined()),
            Truthy::Bool(slot) => Truthy::Bool(virtual_slot(slot)),
            Truthy::Known(_) => self,
        }
    }
}

impl Plan {
    /// The plan of an inlined function's instruction as an instruction
    /// of the loop: its slots are variables ([`virtual_slot`]), its
    /// instructions follow `base`, and no temporary is nulled (there is
    /// none). `None` for what works on a frame (a handler's call, a
    /// `foreach`, a call).
    fn inlined(self, base: usize) -> Option<Plan> {
        let v = virtual_slot;
        Some(match self {
            Plan::Jmp { to } => Plan::Jmp { to: to + base },
            Plan::Cmp {
                a,
                b,
                test,
                taken,
                fall,
            } => Plan::Cmp {
                a: a.inlined(),
                b: b.inlined(),
                test,
                taken: taken + base,
                fall: fall + base,
            },
            Plan::IncCmp {
                slot,
                inc,
                bound,
                cc,
                taken,
                fall,
            } => Plan::IncCmp {
                slot: v(slot),
                inc,
                bound: bound.inlined(),
                cc,
                taken: taken + base,
                fall: fall + base,
            },
            Plan::Arith {
                op,
                a,
                b,
                dst,
                next,
            } => Plan::Arith {
                op,
                a: a.inlined(),
                b: b.inlined(),
                dst: v(dst),
                next: next + base,
            },
            Plan::Int2 {
                op,
                a,
                b,
                dst,
                next,
            } => Plan::Int2 {
                op,
                a: a.inlined(),
                b: b.inlined(),
                dst: v(dst),
                next: next + base,
            },
            Plan::Div { a, b, dst, next } => Plan::Div {
                a: a.inlined(),
                b: b.inlined(),
                dst: v(dst),
                next: next + base,
            },
            Plan::Conv {
                to_float,
                a,
                dst,
                next,
                ..
            } => Plan::Conv {
                to_float,
                a: a.inlined(),
                consume: None,
                dst: v(dst),
                next: next + base,
            },
            Plan::Math { f, a, dst, next } => Plan::Math {
                f,
                a: a.inlined(),
                dst: v(dst),
                next: next + base,
            },
            Plan::AbsInt { a, dst, next } => Plan::AbsInt {
                a: a.inlined(),
                dst: v(dst),
                next: next + base,
            },
            Plan::Pick {
                max,
                a,
                b,
                dst,
                next,
            } => Plan::Pick {
                max,
                a: a.inlined(),
                b: b.inlined(),
                dst: v(dst),
                next: next + base,
            },
            Plan::Truth {
                value,
                when,
                taken,
                fall,
                ..
            } => Plan::Truth {
                value: value.inlined(),
                when,
                consume: None,
                taken: taken + base,
                fall: fall + base,
            },
            Plan::Compare {
                a,
                b,
                test,
                dst,
                next,
            } => Plan::Compare {
                a: a.inlined(),
                b: b.inlined(),
                test,
                dst: v(dst),
                next: next + base,
            },
            Plan::SetBool {
                value,
                negate,
                dst,
                next,
                ..
            } => Plan::SetBool {
                value: value.inlined(),
                negate,
                consume: None,
                dst: v(dst),
                next: next + base,
            },
            Plan::JmpEx {
                value,
                when,
                dst,
                target,
                fall,
                ..
            } => Plan::JmpEx {
                value: value.inlined(),
                when,
                consume: None,
                dst: v(dst),
                target: target + base,
                fall: fall + base,
            },
            Plan::Assign2 {
                value,
                cv,
                tmp,
                next,
                ..
            } => Plan::Assign2 {
                value: value.inlined(),
                consume: None,
                cv: v(cv),
                tmp: v(tmp),
                next: next + base,
            },
            Plan::Step {
                slot,
                inc,
                float,
                copy,
                next,
            } => Plan::Step {
                slot: v(slot),
                inc,
                float,
                copy: copy.map(|(dst, post)| (v(dst), post)),
                next: next + base,
            },
            Plan::Const { dst, value, next } => Plan::Const {
                dst: v(dst),
                value: value.inlined(),
                next: next + base,
            },
            Plan::Copy {
                src,
                dst,
                float,
                next,
                ..
            } => Plan::Copy {
                src: v(src),
                dst: v(dst),
                float,
                consume: false,
                next: next + base,
            },
            Plan::Arg { .. }
            | Plan::Bind { .. }
            | Plan::Leave
            | Plan::Call(_)
            | Plan::Each { .. }
            | Plan::Ext { .. } => return None,
        })
    }

    /// Whether the instruction may run again after it ran: it writes
    /// nothing but temporaries (which the ops before their use compute
    /// again). What an argument of a builtin call the loop computes may be
    /// made of ([`Loop::resume`]).
    fn redoable(&self) -> bool {
        match *self {
            Plan::Jmp { .. }
            | Plan::Cmp { .. }
            | Plan::Truth { .. }
            | Plan::Arg { .. }
            | Plan::Bind { .. }
            | Plan::Leave => true,
            Plan::Compare { dst, .. } | Plan::SetBool { dst, .. } | Plan::JmpEx { dst, .. } => {
                dst.tmp
            }
            Plan::Assign2 { .. } => false,
            Plan::Arith { dst, .. }
            | Plan::Int2 { dst, .. }
            | Plan::Div { dst, .. }
            | Plan::Conv { dst, .. }
            | Plan::Math { dst, .. }
            | Plan::Pick { dst, .. }
            | Plan::AbsInt { dst, .. }
            | Plan::Const { dst, .. }
            | Plan::Copy { dst, .. } => dst.tmp,
            Plan::IncCmp { .. }
            | Plan::Step { .. }
            | Plan::Call(_)
            | Plan::Each { .. }
            | Plan::Ext { .. } => false,
        }
    }

    /// Where execution may go on.
    fn successors(&self) -> [Option<usize>; 2] {
        match *self {
            Plan::Jmp { to } | Plan::Bind { to, .. } => [Some(to), None],
            Plan::Cmp { taken, fall, .. }
            | Plan::IncCmp { taken, fall, .. }
            | Plan::Truth { taken, fall, .. } => [Some(taken), Some(fall)],
            Plan::JmpEx { target, fall, .. } => [Some(target), Some(fall)],
            Plan::Leave => [None, None],
            Plan::Compare { next, .. }
            | Plan::SetBool { next, .. }
            | Plan::Assign2 { next, .. } => [Some(next), None],
            Plan::Call(call) => [call.fall, call.jump],
            Plan::Each { end, full, .. } => [Some(full), Some(end)],
            Plan::Ext { next, .. } => [Some(next), None],
            Plan::Int2 { next, .. }
            | Plan::Conv { next, .. }
            | Plan::Math { next, .. }
            | Plan::AbsInt { next, .. }
            | Plan::Arg { next, .. }
            | Plan::Pick { next, .. }
            | Plan::Div { next, .. }
            | Plan::Arith { next, .. }
            | Plan::Step { next, .. }
            | Plan::Const { next, .. }
            | Plan::Copy { next, .. } => [Some(next), None],
        }
    }

    /// The types on the way to instruction `to`, given those before the
    /// instruction.
    fn out(&self, st: &State, to: usize) -> State {
        let mut out = st.clone();
        match *self {
            // At the end of a `foreach` nothing was fetched.
            Plan::Each { end, .. } if end == to => {}
            // The bool is stored only when the jump is taken.
            Plan::JmpEx {
                consume,
                dst,
                target,
                ..
            } => {
                if let Some(tmp) = consume {
                    out.insert(tmp, Ty::Plain);
                }
                if to == target {
                    out.insert(dst, Ty::Bool);
                }
            }
            _ => self.apply(&mut out),
        }
        out
    }

    /// The types after the instruction did its work, given those before
    /// it.
    fn apply(&self, st: &mut State) {
        let num = |float| if float { Ty::Float } else { Ty::Int };
        match *self {
            Plan::Jmp { .. } | Plan::Cmp { .. } | Plan::IncCmp { .. } => {}
            Plan::Truth { consume, .. } => {
                if let Some(tmp) = consume {
                    st.insert(tmp, Ty::Plain);
                }
            }
            Plan::Leave => {}
            Plan::Bind {
                args,
                argc,
                cvs,
                tmps,
                ..
            } => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                let slot = |tmp: bool, k: usize| {
                    virtual_slot(SlotRef {
                        tmp,
                        at: (k * SLOT) as i32,
                    })
                };
                for k in 0..cvs {
                    let ty = if k < argc {
                        st.get(&arg_slot(args + k)).copied().unwrap_or(Ty::Plain)
                    } else {
                        Ty::Plain
                    };
                    st.insert(slot(false, k), ty);
                }
                for k in 0..tmps {
                    st.insert(slot(true, k), Ty::Plain);
                }
            }
            Plan::Compare { dst, .. } => {
                st.insert(dst, Ty::Bool);
            }
            Plan::SetBool { consume, dst, .. } | Plan::JmpEx { consume, dst, .. } => {
                if let Some(tmp) = consume {
                    st.insert(tmp, Ty::Plain);
                }
                st.insert(dst, Ty::Bool);
            }
            Plan::Assign2 {
                value,
                consume,
                cv,
                tmp,
                ..
            } => {
                if let Some(consumed) = consume {
                    st.insert(consumed, Ty::Plain);
                }
                st.insert(cv, num(value.float));
                st.insert(tmp, num(value.float));
            }
            Plan::Each {
                value,
                key,
                consumed,
                ..
            } => {
                for tmp in consumed.into_iter().flatten() {
                    st.insert(tmp, Ty::Plain);
                }
                st.insert(value.0, num(value.1));
                if let Some(key) = key {
                    st.insert(key, Ty::Int);
                }
            }
            Plan::Ext {
                result, consume, ..
            } => {
                if let Some(tmp) = consume {
                    st.insert(tmp, Ty::Plain);
                }
                if let Some((slot, ty)) = result {
                    st.insert(slot, ty);
                }
            }
            Plan::Call(call) => {
                for tmp in call.consumed.into_iter().flatten() {
                    st.insert(tmp, Ty::Plain);
                }
                if let Some((slot, ty)) = call.result {
                    st.insert(slot, ty);
                }
            }
            Plan::Int2 { dst, .. } | Plan::Pick { dst, .. } | Plan::AbsInt { dst, .. } => {
                st.insert(dst, Ty::Int);
            }
            Plan::Arg {
                k, value, consume, ..
            } => {
                if let Some(tmp) = consume {
                    st.insert(tmp, Ty::Plain);
                }
                st.insert(arg_slot(k), num(value.float));
            }
            Plan::Math { dst, .. } => {
                st.insert(dst, Ty::Float);
            }
            Plan::Conv {
                to_float,
                consume,
                dst,
                ..
            } => {
                if let Some(tmp) = consume {
                    st.insert(tmp, Ty::Plain);
                }
                st.insert(dst, num(to_float));
            }
            Plan::Div { dst, .. } => {
                st.insert(dst, Ty::Float);
            }
            Plan::Arith { a, b, dst, .. } => {
                st.insert(dst, num(a.float || b.float));
            }
            Plan::Step { float, copy, .. } => {
                if let Some((dst, _)) = copy {
                    st.insert(dst, num(float));
                }
            }
            Plan::Const { dst, value, .. } => {
                st.insert(dst, num(value.float));
            }
            Plan::Copy {
                src,
                dst,
                float,
                consume,
                ..
            } => {
                st.insert(dst, num(float));
                if consume {
                    st.insert(src, Ty::Plain);
                }
            }
        }
    }
}

/// A loop the second tier can compile: its head, the types its slots must
/// have on entry and each of its instructions with the types before it.
#[derive(Debug)]
pub(super) struct Loop {
    head: usize,
    entry: State,
    plans: BTreeMap<usize, (Plan, State)>,
    /// For each instruction between the `InitFCall` and the `DoCall` of a
    /// builtin call the loop computes itself: the `InitFCall`. The
    /// interpreter has no pending call there (the loop made none), so when
    /// the loop gives up in the middle of the arguments, the whole call is
    /// the first tier's, from its start ([`Plan::redoable`]).
    resume: BTreeMap<usize, usize>,
    /// The constants the loop was compiled with, by the op that reads
    /// each, with the type and bits of its value: checked again at the
    /// loop's entry (`super::const_is`; a constant may be defined to
    /// something else in another request).
    consts: Vec<(usize, Ty, u64)>,
    /// How many instructions the loop's own tape has: the instructions
    /// from there on are those of inlined functions ([`Inlined`]).
    real: usize,
    /// The variables of the inlined functions ([`virtual_slot`]).
    virtuals: Vec<SlotRef>,
    /// The functions inlined, each by the literal of its name at the call
    /// and its address: checked again at the loop's entry
    /// (`super::user_is`; the name may be another function's in another
    /// request).
    inlined: Vec<(u32, usize)>,
}

impl Loop {
    /// The type of `slot` in `st`.
    fn ty(&self, st: &State, slot: SlotRef) -> Option<Ty> {
        st.get(&slot).or_else(|| self.entry.get(&slot)).copied()
    }

    /// Whether a round that ends with the types `st` may start the next
    /// one: every slot the entry guards as a number still is that number.
    fn may_repeat(&self, st: &State) -> bool {
        self.entry
            .iter()
            .all(|(slot, ty)| *ty == Ty::Plain || self.ty(st, *slot) == Some(*ty))
    }
}

/// What the frame holds when a loop is looked at: the analysis's view of
/// the run-time state (`super::specialise` answers from the frame).
pub(super) struct Seen<'a> {
    /// The type a slot has now; `None` when it owns something (a string,
    /// an array, an object, a reference).
    pub(super) snapshot: &'a dyn Fn(SlotRef) -> Option<Ty>,
    /// The type of the first element, `depth` arrays down, of the array a
    /// compiled variable (by index) holds, when that is a number.
    pub(super) element: &'a dyn Fn(u32, usize) -> Option<Ty>,
    /// The type of the number under the key a string literal (by index)
    /// is, in the array a compiled variable (by index) holds.
    pub(super) keyed: &'a dyn Fn(u32, u32) -> Option<Ty>,
    /// The type of the number the static property of a place (by index)
    /// holds, on a hit of its run-time cache.
    pub(super) statik: &'a dyn Fn(u32) -> Option<Ty>,
    /// Whether the numbers a `foreach` iterator (by index) yields are
    /// floats, and whether its keys are ints; `None` when it is not over
    /// an array of numbers.
    pub(super) each: &'a dyn Fn(u32) -> Option<(bool, bool)>,
    /// The name of the builtin the function-name literal (by index)
    /// resolved to when its call last ran; `None` when it has not run, or
    /// calls may not be skipped (observers).
    pub(super) builtin: &'a dyn Fn(u32) -> Option<Vec<u8>>,
    /// The user function the function-name literal (by index) resolved to
    /// when its call last ran, when a call of it with that many arguments
    /// can run inline: a plain function (no types, no defaults used, no
    /// references, no statics) of at most [`INLINE_OPS`] ops, and calls
    /// are not observed.
    pub(super) user: &'a dyn Fn(u32, usize) -> Option<std::rc::Rc<crate::func::UserFunction>>,
    /// The type of the number the property at a place (by index) holds,
    /// when its read is a cache hit.
    pub(super) property: &'a dyn Fn(u32) -> Option<Ty>,
    /// The value of the constant the op (by index) reads, when its
    /// run-time cache holds a number or a bool: its type and its bits.
    pub(super) constant: &'a dyn Fn(usize) -> Option<(Ty, u64)>,
    /// Whether the loop is to use the short cuts for reads ([`Fast`]) at
    /// all: not when too many of them turned out not to fit its arrays.
    pub(super) fast: bool,
    /// Whether the read short cut of the instruction (by index) kept
    /// missing (`super::missed`): that instruction then calls its handler.
    pub(super) slow: &'a dyn Fn(usize) -> bool,
}

/// The type analysis of one loop.
struct Analysis<'a> {
    tape: &'a [Inst],
    lits: &'a [Value],
    seen: &'a Seen<'a>,
    /// The op array's places (the paths of the nested accesses).
    places: &'a [Place],
    /// The op array's ops, for what has no tape instruction (`foreach`).
    ops: &'a [Op],
    /// The builtin calls among them ([`call_sites`]).
    sites: Vec<Site>,
    /// The entry types of the slots met so far.
    entry: State,
    /// The constants planned as their values ([`Loop::consts`]).
    consts: Vec<(usize, Ty, u64)>,
    /// The calls of user functions the loop may run inline.
    inl: &'a [Inlined],
    /// How many instructions the loop's own tape has ([`Loop::real`]).
    real: usize,
    /// Set while an inlined function's instruction is planned: the tape,
    /// literals, places and ops are then the function's, and its slots
    /// are in no frame (nothing is known of one but what the state says).
    inline: bool,
    /// Which of `inl` have been entered ([`Plan::Bind`]).
    bound: Vec<bool>,
}

impl<'a> Analysis<'a> {
    /// The type of `slot` in `st`, for an instruction that writes it or
    /// lets a handler consume it: all that is asked of a slot met for the
    /// first time is that it owns nothing now, and that is all its entry
    /// type then says. (The loop may be looked at after it has gone round:
    /// a temporary still holds the last round's number, which the next
    /// round does not read.)
    fn ty(&mut self, st: &State, slot: SlotRef) -> Option<Ty> {
        if let Some(ty) = st.get(&slot).or_else(|| self.entry.get(&slot)) {
            return Some(*ty);
        }
        if slot.at < 0 || self.inline {
            // An argument variable ([`arg_slot`]) nothing was sent to, or
            // a variable of an inlined function.
            return Some(Ty::Plain);
        }
        (self.seen.snapshot)(slot)?;
        self.entry.insert(slot, Ty::Plain);
        Some(Ty::Plain)
    }

    /// The type of `slot` in `st`, for an instruction that reads it. A
    /// slot the loop has not written on the way here is read with what it
    /// held on entry: its entry type becomes the type the frame shows now
    /// (the guard at the loop's entry then tests for it).
    fn read(&mut self, st: &State, slot: SlotRef) -> Option<Ty> {
        Some(match st.get(&slot) {
            Some(ty) => *ty,
            None if slot.at < 0 || self.inline => Ty::Plain,
            None => match self.entry.get(&slot) {
                Some(ty @ (Ty::Int | Ty::Float | Ty::Bool)) => *ty,
                _ => {
                    let ty = (self.seen.snapshot)(slot)?;
                    self.entry.insert(slot, ty);
                    ty
                }
            },
        })
    }

    /// The slot as a number: `None` unless it is known to hold one.
    fn number(&mut self, st: &State, slot: SlotRef) -> Option<Opnd> {
        let float = match self.read(st, slot)? {
            Ty::Int => false,
            Ty::Float => true,
            Ty::Bool | Ty::Plain => return None,
        };
        Some(Opnd {
            num: Num::Slot(slot),
            float,
        })
    }

    /// The slot as something a jump may test: a number or a bool.
    fn truthy(&mut self, st: &State, slot: SlotRef) -> Option<Truthy> {
        match self.read(st, slot)? {
            Ty::Bool => Some(Truthy::Bool(slot)),
            Ty::Plain => None,
            _ => self.number(st, slot).map(Truthy::Num),
        }
    }

    /// The operand `o` of an op as something a jump may test, and the
    /// temporary it is consumed from (`hot::take_bool`).
    fn op_truthy(&mut self, st: &State, o: Operand) -> Option<(Truthy, Option<SlotRef>)> {
        Some(match o {
            Operand::Cv(c) => (self.truthy(st, cv(c.checked_mul(SLOT_U32)?)?)?, None),
            Operand::Tmp(t) => {
                let slot = tmp(t.checked_mul(SLOT_U32)?)?;
                (self.truthy(st, slot)?, Some(slot))
            }
            Operand::TmpKeep(t) => (self.truthy(st, tmp(t.checked_mul(SLOT_U32)?)?)?, None),
            Operand::Lit(l) => {
                let known = match self.lits.get(l as usize)? {
                    Value::Null => false,
                    Value::Bool(b) => *b,
                    Value::Int(i) => *i != 0,
                    Value::Float(f) if !f.is_nan() => *f != 0.0,
                    _ => return None,
                };
                (Truthy::Known(known), None)
            }
            Operand::Unused => return None,
        })
    }

    /// The operand of kind `kind` in operand field `x` (`super::num`).
    fn operand(&mut self, st: &State, kind: u8, x: u32) -> Option<Opnd> {
        match kind {
            KC => self.number(st, cv(x)?),
            KT => self.number(st, tmp(x)?),
            #[allow(clippy::cast_possible_wrap)]
            KI => Some(Opnd {
                num: Num::Int(i64::from(x as i32)),
                float: false,
            }),
            _ => literal(self.lits, x),
        }
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

    /// Instruction `i` with the types `st` before it; `None` when the
    /// second tier has no form for it (not a numeric instruction, or an
    /// operand that is not known to be a number).
    #[allow(clippy::too_many_lines)]
    fn plan(&mut self, st: &State, i: usize) -> Option<Plan> {
        if i >= self.real && !self.inline {
            return self.inlined_plan(st, i);
        }
        self.own_plan(st, i)
    }

    /// The plan of instruction `i` of an inlined function ([`Inlined`]):
    /// planned as the function's own instruction, on the types of its
    /// variables in `st`, and moved into the loop ([`Plan::inlined`]). Its
    /// `return` of a number stores that into the temporary of the call's
    /// `DoCall` and goes on after it. The function has no frame: its
    /// instructions may only go forward (a loop would need a safe point)
    /// and must be of the kinds that touch none.
    fn inlined_plan(&mut self, st: &State, i: usize) -> Option<Plan> {
        let inl: &'a [Inlined] = self.inl;
        let callee = inl
            .iter()
            .find(|c| i >= c.base && i < c.base + c.tape.len())?;
        let at = i - callee.base;
        let code = &callee.func.code;
        let local: State = st
            .iter()
            .filter_map(|(slot, ty)| Some((real_slot(*slot)?, *ty)))
            .collect();
        // The function's view, for one instruction.
        let saved = (
            std::mem::replace(&mut self.tape, &callee.tape),
            std::mem::replace(&mut self.lits, &code.literals),
            std::mem::replace(&mut self.places, &code.places),
            std::mem::replace(&mut self.ops, &code.ops),
            std::mem::take(&mut self.sites),
            std::mem::take(&mut self.entry),
        );
        self.inline = true;
        // What the function returns here, if it does: a number, a bool, or
        // (`None`) something else.
        let mut returned: Option<Option<Returned>> = None;
        let func = &callee.func;
        let plan = match code.ops.get(at) {
            Some(&Op::Return { val }) => {
                returned = Some(self.returned(&local, val));
                None
            }
            // The declared return type takes the value as it is (no
            // coercion, no error): nothing to do.
            Some(&Op::VerifyReturn { val }) => {
                let ty = match self.returned(&local, val) {
                    Some(Returned::Num(value)) => Some(num_ty(value.float)),
                    Some(Returned::Bool(_)) => Some(Ty::Bool),
                    None => None,
                };
                ty.filter(|ty| takes(func.return_type.as_ref(), *ty))
                    .map(|_| Plan::Jmp { to: at + 1 })
            }
            // A parameter with a default: passed (the call starts after
            // these), or its default, a number its type takes as it is.
            Some(&Op::RecvInit { n, skip, .. }) => Some(Plan::Jmp {
                to: if (n as usize) < callee.argc {
                    skip as usize
                } else {
                    at + 1
                },
            }),
            Some(&Op::RecvDefault { n, cv: var, val }) => (|| {
                let (value, _) = self.op_number(&local, val)?;
                let param = func.params.get(n as usize)?;
                if !takes(param.ty.as_ref(), num_ty(value.float)) {
                    return None;
                }
                let dst = cv(var.checked_mul(SLOT_U32)?)?;
                Some(match value.num {
                    Num::Slot(src) => Plan::Copy {
                        src,
                        dst,
                        float: value.float,
                        consume: false,
                        next: at + 1,
                    },
                    _ => Plan::Const {
                        dst,
                        value,
                        next: at + 1,
                    },
                })
            })(),
            _ => self.own_plan(&local, at),
        };
        self.inline = false;
        (
            self.tape,
            self.lits,
            self.places,
            self.ops,
            self.sites,
            self.entry,
        ) = saved;
        if let Some(value) = returned {
            let next = (callee.call + 1 < self.real).then_some(callee.call + 1)?;
            let Some(&Op::DoCall { dst }) = self.ops.get(callee.call) else {
                return None;
            };
            // An unused result is no work, whatever it is (the function
            // has no effects to leave behind).
            let Some(dst) = dst else {
                return Some(Plan::Jmp { to: next });
            };
            let dst = tmp(dst.checked_mul(SLOT_U32)?)?;
            self.ty(st, dst)?;
            return Some(match value? {
                Returned::Num(value) => match value.inlined().num {
                    Num::Slot(src) => Plan::Copy {
                        src,
                        dst,
                        float: value.float,
                        consume: false,
                        next,
                    },
                    _ => Plan::Const { dst, value, next },
                },
                Returned::Bool(value) => Plan::SetBool {
                    value: value.inlined(),
                    negate: false,
                    consume: None,
                    dst,
                    next,
                },
            });
        }
        let plan = plan?;
        // Forward only, and inside the function.
        if plan
            .successors()
            .into_iter()
            .flatten()
            .any(|to| to <= at || to >= callee.tape.len())
        {
            return None;
        }
        plan.inlined(callee.base)
    }

    /// The value the operand `val` of a `return` is, when the loop can
    /// hand it on: a number, or a bool.
    fn returned(&mut self, st: &State, val: Operand) -> Option<Returned> {
        if let Some((value, _)) = self.op_number(st, val) {
            return Some(Returned::Num(value));
        }
        if matches!(val, Operand::Lit(l) if !matches!(self.lits.get(l as usize), Some(Value::Bool(_))))
        {
            return None;
        }
        match self.op_truthy(st, val)? {
            (value @ (Truthy::Bool(_) | Truthy::Known(_)), _) => Some(Returned::Bool(value)),
            (Truthy::Num(_), _) => None,
        }
    }

    /// The first argument variable of the call `site`: those of its depth
    /// among the calls around it (an outer call's arguments are kept
    /// while an inner call runs). `None` beyond [`MAX_DEPTH`].
    fn arg_base(&self, site: &Site) -> Option<usize> {
        let depth = self
            .sites
            .iter()
            .filter(|outer| outer.init < site.init && outer.call > site.call)
            .count();
        (depth < MAX_DEPTH).then_some(depth * MAX_ARGS)
    }

    /// The call site `inl` has for the `InitFCall` at `init`, and its index.
    fn inlined_site(&self, init: usize) -> Option<(usize, &'a Inlined)> {
        if self.inline {
            return None;
        }
        let inl: &'a [Inlined] = self.inl;
        inl.iter().enumerate().find(|(_, c)| c.init == init)
    }

    /// [`Self::plan`] of one of the tape's own instructions.
    #[allow(clippy::too_many_lines)]
    fn own_plan(&mut self, st: &State, i: usize) -> Option<Plan> {
        let inst = *self.tape.get(i)?;
        let after = |n: usize| (i + n < self.tape.len()).then_some(i + n);
        if let Some(call) = self.call(st, i) {
            return Some(Plan::Call(call));
        }
        if inst.op == TOp::Ext {
            // The step of a `foreach`, or a constant's read.
            if let Some(plan) = self.each_step(st, i) {
                return Some(plan);
            }
            return self.op_plan(st, i);
        }
        if matches!(
            inst.op,
            TOp::Exit | TOp::Step | TOp::CallInit | TOp::CallSend
        ) {
            return self.op_plan(st, i);
        }
        // The scalar family's instructions, by opcode.
        match inst.op {
            TOp::Nop => return Some(Plan::Jmp { to: after(1)? }),
            TOp::JmpZC | TOp::JmpNzC => {
                return Some(Plan::Truth {
                    value: self.truthy(st, cv(inst.a)?)?,
                    when: inst.op == TOp::JmpNzC,
                    consume: None,
                    taken: self.target(i)?,
                    fall: after(1)?,
                });
            }
            TOp::NumBinT | TOp::NumBinC | TOp::NumAssignOpC => {
                let (a, b) = (
                    self.operand(st, inst.k >> 4, inst.a)?,
                    self.operand(st, inst.k & 0xf, inst.b)?,
                );
                let (dst, next) = match inst.op {
                    TOp::NumBinT => (tmp(inst.c)?, after(1)?),
                    // Past the `AssignCv` the store into the variable does.
                    TOp::NumBinC => (cv(inst.d)?, after(2)?),
                    _ => (cv(inst.a)?, after(1)?),
                };
                self.ty(st, dst)?;
                if let Some(test) = compare_test(inst.e, a.float, b.float) {
                    // A comparison: a bool (not as `op=`).
                    return (inst.op != TOp::NumAssignOpC).then_some(Plan::Compare {
                        a,
                        b,
                        test,
                        dst,
                        next,
                    });
                }
                if a.float || b.float {
                    // Of these operators only `/` takes a float.
                    return (inst.e == E_DIV).then_some(Plan::Div { a, b, dst, next });
                }
                // An int quotient is an int or a float by its value: not
                // typed.
                let op = IntOp::of(inst.e)?;
                if let Num::Int(y) = b.num {
                    if !op.takes(y) {
                        return None;
                    }
                }
                return Some(Plan::Int2 {
                    op,
                    a,
                    b,
                    dst,
                    next,
                });
            }
            _ => {}
        }
        Some(match inst.op.shape() {
            Shape::Jmp => Plan::Jmp {
                to: self.target(i)?,
            },
            // `JmpZ` / `JmpNz` of a temporary holding a number: the
            // tape's instruction declines it (it takes bools), and the op
            // then decides by the number's truth and consumes it.
            Shape::JmpBoolT(when) => {
                let slot = tmp(inst.a)?;
                Plan::Truth {
                    value: self.truthy(st, slot)?,
                    when,
                    consume: Some(slot),
                    taken: self.target(i)?,
                    fall: after(1)?,
                }
            }
            Shape::Cmp { ka, kb, m } => {
                let (a, b) = (self.operand(st, ka, inst.a)?, self.operand(st, kb, inst.b)?);
                let test = if !a.float && !b.float {
                    Test::Int(int_cc(m))
                } else if a.float != b.float && inst.k & cmp_outcome::IDENTITY != 0 {
                    // An int and a float under `===` / `!==`.
                    Test::Always(inst.k & cmp_outcome::MISMATCH != 0)
                } else {
                    match float_test(inst.k) {
                        FloatTest::Always(taken) => Test::Always(taken),
                        test => Test::Float(test),
                    }
                };
                Plan::Cmp {
                    a,
                    b,
                    test,
                    taken: self.target(i)?,
                    fall: after(2)?,
                }
            }
            Shape::IncCmp { inc, kb, m } => {
                let slot = cv(inst.a)?;
                let bound = self.operand(st, kb, inst.b)?;
                if bound.float || self.number(st, slot)?.float {
                    return None;
                }
                Plan::IncCmp {
                    slot,
                    inc,
                    bound,
                    cc: int_cc(m),
                    taken: self.target(i)?,
                    fall: after(3)?,
                }
            }
            Shape::ArithTmp { op, ka, kb } => {
                let (a, b) = (self.operand(st, ka, inst.a)?, self.operand(st, kb, inst.b)?);
                let dst = tmp(inst.c)?;
                self.ty(st, dst)?;
                Plan::Arith {
                    op,
                    a,
                    b,
                    dst,
                    next: after(1)?,
                }
            }
            Shape::ArithCv { op, ka, kb } => {
                let (a, b) = (self.operand(st, ka, inst.a)?, self.operand(st, kb, inst.b)?);
                let dst = cv(inst.c)?;
                self.ty(st, dst)?;
                Plan::Arith {
                    op,
                    a,
                    b,
                    dst,
                    // Past the `AssignCv` the store into the variable does.
                    next: after(2)?,
                }
            }
            Shape::AssignOp { op, kb } => {
                let dst = cv(inst.a)?;
                let a = self.number(st, dst)?;
                let b = self.operand(st, kb, inst.b)?;
                Plan::Arith {
                    op,
                    a,
                    b,
                    dst,
                    next: after(1)?,
                }
            }
            Shape::IncDecC(inc) => {
                let slot = cv(inst.a)?;
                Plan::Step {
                    slot,
                    inc,
                    float: self.number(st, slot)?.float,
                    copy: None,
                    next: after(1)?,
                }
            }
            Shape::IncDecT { inc, post } => {
                let slot = cv(inst.a)?;
                let dst = tmp(inst.c)?;
                self.ty(st, dst)?;
                Plan::Step {
                    slot,
                    inc,
                    float: self.number(st, slot)?.float,
                    copy: Some((dst, post)),
                    next: after(1)?,
                }
            }
            Shape::AssignI => {
                let dst = cv(inst.a)?;
                self.ty(st, dst)?;
                #[allow(clippy::cast_possible_wrap)]
                let value = Opnd {
                    num: Num::Int(i64::from(inst.b as i32)),
                    float: false,
                };
                Plan::Const {
                    dst,
                    value,
                    next: after(1)?,
                }
            }
            Shape::AssignL => {
                let dst = cv(inst.a)?;
                self.ty(st, dst)?;
                Plan::Const {
                    dst,
                    value: literal(self.lits, inst.b)?,
                    next: after(1)?,
                }
            }
            Shape::AssignC | Shape::AssignT => {
                let consume = matches!(inst.op.shape(), Shape::AssignT);
                let dst = cv(inst.a)?;
                let src = if consume { tmp(inst.b)? } else { cv(inst.b)? };
                self.ty(st, dst)?;
                let next = after(1)?;
                match self.truthy(st, src)? {
                    // A bool: copied as its truth.
                    value @ Truthy::Bool(_) => Plan::SetBool {
                        value,
                        negate: false,
                        consume: consume.then_some(src),
                        dst,
                        next,
                    },
                    Truthy::Num(value) => Plan::Copy {
                        src,
                        dst,
                        float: value.float,
                        consume,
                        next,
                    },
                    Truthy::Known(_) => return None,
                }
            }
            _ => return None,
        })
    }

    /// Whether `slot` is one the loop does not track and never may: it
    /// holds something that owns (an array, a string, an object) now, so
    /// no numeric instruction of the loop reads or writes it.
    fn untracked(&self, st: &State, slot: SlotRef) -> bool {
        !st.contains_key(&slot)
            && !self.entry.contains_key(&slot)
            && (self.seen.snapshot)(slot).is_none()
    }

    /// The `dims` operand of kind `kind` and index `index` as a number the
    /// loop knows: a variable or a temporary tracked as one, or a number
    /// literal.
    fn dim_number(&mut self, st: &State, kind: u8, index: u32) -> Option<Opnd> {
        match kind {
            DIM_CV => self.number(st, cv(index.checked_mul(SLOT_U32)?)?),
            DIM_TMP | DIM_TMPKEEP => self.number(st, tmp(index.checked_mul(SLOT_U32)?)?),
            DIM_LIT => literal(self.lits, index.checked_mul(SLOT_U32)?),
            _ => None,
        }
    }

    /// Instruction `i` as a call of its family handler, for the
    /// instructions whose effects on the frame are spelled out here: the
    /// array reads and writes of `exec/tape/dims.rs` on a variable that
    /// holds an array, and `count()` / `strlen()`.
    #[allow(clippy::similar_names, clippy::too_many_lines)]
    fn call(&mut self, st: &State, i: usize) -> Option<Call> {
        let inst = *self.tape.get(i)?;
        let Shape::Family {
            handler,
            covers,
            ending,
        } = inst.op.shape()
        else {
            return None;
        };
        let after = (i + covers < self.tape.len()).then_some(i + covers)?;
        let mut call = Call {
            handler: handler as usize,
            polls: ending.polls,
            fall: Some(after),
            after,
            jump: None,
            result: None,
            consumed: [None; MAX_CONSUMED],
            fast: None,
        };
        // The array variable of a `dims` instruction: one the loop does
        // not track (it holds an array now, and no numeric instruction of
        // the loop writes it).
        let array = {
            let slot = cv(inst.a.checked_mul(SLOT_U32)?);
            move |an: &Self| {
                slot.filter(|slot| an.untracked(st, *slot))
                    .map(|slot| slot.at)
            }
        };
        let value_kind = (inst.k >> 3) & 7;
        match inst.op {
            // `$cv[dim]` read: into the variable of the `AssignCv` folded
            // into it (the handler then answers "jumped", past that op) or
            // into a temporary. The element's type is what the array's
            // first element has now.
            TOp::FetchDimC | TOp::FetchDimIC => {
                let array = array(self)?;
                let float = self.element_float(inst.a, 1)?;
                let index = self.dim(st, &mut call, inst.k & 7, inst.b)?;
                call.fast = index.map(|index| Fast::Get { array, index });
                let dst = u32::try_from(inst.p).ok()?;
                self.fetched(st, &mut call, i, inst.d, dst, float)?;
            }
            // `$cv['key']` read into the variable of the `AssignCv` folded
            // into it. The element's type is what the key holds now.
            TOp::FetchDimSC => {
                array(self)?;
                let float = match (self.seen.keyed)(inst.a, inst.b)? {
                    Ty::Int => false,
                    Ty::Float => true,
                    Ty::Bool | Ty::Plain => return None,
                };
                call.fast = Some(Fast::GetKey {
                    cv: inst.a,
                    lit: inst.b,
                });
                let dst = u32::try_from(inst.p).ok()?;
                self.fetched(st, &mut call, i, inst.d, dst, float)?;
            }
            // `$cv[d1][d2]...` read.
            TOp::FetchDimsC => {
                let array = array(self)?;
                let dims = self.place_dims(st, &mut call, inst.a, inst.d)?;
                let float = self.element_float(inst.a, dims.len())?;
                if let [Some(i), Some(j)] = dims[..] {
                    call.fast = Some(Fast::Get2 { array, i, j });
                }
                let dst = u32::try_from(inst.p).ok()?;
                self.fetched(st, &mut call, i, inst.b, dst, float)?;
            }
            // `$cv[dim] = val` and `$cv[] = val`.
            TOp::AssignDimC | TOp::AssignDimIC => {
                let array = array(self)?;
                let index = if inst.k & 7 == DIM_UNUSED {
                    None
                } else {
                    self.dim(st, &mut call, inst.k & 7, inst.b)?
                };
                let (value, consume) = self.value(st, &mut call, value_kind, inst.p)?;
                if let (Some(index), Some(value)) = (index, value) {
                    call.fast = Some(Fast::Set {
                        array,
                        index,
                        value,
                        consume,
                    });
                }
            }
            // `$cv[d1][d2]... = val`.
            TOp::AssignDimsC => {
                let array = array(self)?;
                let dims = self.place_dims(st, &mut call, inst.a, inst.d)?;
                let (value, consume) = self.value(st, &mut call, value_kind, inst.p)?;
                if let ([Some(i), Some(j)], Some(value)) = (&dims[..], value) {
                    call.fast = Some(Fast::Set2 {
                        array,
                        i: *i,
                        j: *j,
                        value,
                        consume,
                    });
                }
            }
            // `$cv[dim] op= val`.
            TOp::AssignOpDimC => {
                let array = array(self)?;
                let index = if inst.k & 7 == DIM_UNUSED {
                    None
                } else {
                    self.dim(st, &mut call, inst.k & 7, inst.b)?
                };
                let (value, consume) = self.value(st, &mut call, value_kind, inst.p)?;
                // The operator codes of `dims::op_code`: `+ - *`.
                if let (Some(index), Some(value), 0..=2) = (index, value, inst.e) {
                    call.fast = Some(Fast::Op {
                        array,
                        index,
                        op: usize::from(inst.e),
                        value,
                        consume,
                    });
                }
            }
            // `$cv[d1][d2]... op= val`.
            TOp::AssignOpDimsC => {
                array(self)?;
                self.place_dims(st, &mut call, inst.a, inst.d)?;
                self.value(st, &mut call, value_kind, inst.p)?;
            }
            // `$cv[dim]++` & co.; a used result is the element's number.
            TOp::IncDecDimC => {
                let array = array(self)?;
                let index = self.dim(st, &mut call, inst.k & 7, inst.b)?;
                // Without a used result, `++` and `--` are `+= 1` and
                // `-= 1` (the kind codes of `dims::kind_code`: odd ones
                // decrement).
                if let (Some(index), u32::MAX) = (index, inst.c) {
                    call.fast = Some(Fast::Op {
                        array,
                        index,
                        op: usize::from(inst.e & 1),
                        value: Opnd {
                            num: Num::Int(1),
                            float: false,
                        },
                        consume: None,
                    });
                }
                if inst.c != u32::MAX {
                    let float = self.element_float(inst.a, 1)?;
                    let dst = tmp(inst.c)?;
                    self.ty(st, dst)?;
                    call.result = Some((dst, num_ty(float)));
                }
            }
            // `$obj->prop` read into a temporary, or into the variable of
            // the `AssignCv` folded into it. The property's type is what
            // it holds now.
            TOp::FetchObjProp | TOp::FetchObjPropTo => {
                self.object_place(st, inst.a)?;
                let float = match (self.seen.property)(inst.a)? {
                    Ty::Int => false,
                    Ty::Float => true,
                    Ty::Bool | Ty::Plain => return None,
                };
                let to = if inst.op == TOp::FetchObjPropTo {
                    inst.d
                } else {
                    u32::MAX
                };
                self.fetched(st, &mut call, i, to, inst.b, float)?;
                if self.cached_place(inst.a) {
                    call.fast = Some(Fast::PropGet { place: inst.a });
                }
            }
            // `$obj->prop = val`, `op= val`, `++` & co., as statements (a
            // used result would be a value of a type not known here).
            TOp::AssignObjProp | TOp::AssignOpObjProp | TOp::IncDecObjProp => {
                let (place, value) = match *self.ops.get(inst.d as usize)? {
                    Op::AssignObjProp {
                        place,
                        val,
                        dst: None,
                    }
                    | Op::AssignOpObjProp {
                        place,
                        val,
                        dst: None,
                        ..
                    } => (place, Some(val)),
                    Op::IncDecObjProp {
                        place, dst: None, ..
                    } => (place, None),
                    _ => return None,
                };
                self.object_place(st, place)?;
                // The number the loop has for the value, for the short
                // cut, and the temporary it is consumed from.
                let (number, consume) = match value {
                    Some(Operand::Tmp(t)) => self.value(st, &mut call, DIM_TMP, t as usize)?,
                    Some(Operand::Cv(c)) => (self.dim_number(st, DIM_CV, c), None),
                    Some(Operand::Lit(l)) => (self.dim_number(st, DIM_LIT, l), None),
                    _ => (None, None),
                };
                if self.cached_place(place) {
                    call.fast = match (self.ops.get(inst.d as usize)?, number) {
                        (Op::AssignObjProp { .. }, Some(value)) => Some(Fast::PropSet {
                            place,
                            value,
                            consume,
                        }),
                        (Op::AssignOpObjProp { op, .. }, Some(value)) => {
                            let op = match *op {
                                php_runtime::BinaryOp::Add => Some(0),
                                php_runtime::BinaryOp::Sub => Some(1),
                                php_runtime::BinaryOp::Mul => Some(2),
                                _ => None,
                            };
                            op.map(|op| Fast::PropOp {
                                place,
                                op,
                                value,
                                consume,
                            })
                        }
                        (Op::IncDecObjProp { kind, .. }, _) => Some(Fast::PropOp {
                            place,
                            op: if kind.is_inc() { 3 } else { 4 },
                            value: Opnd {
                                num: Num::Int(1),
                                float: false,
                            },
                            consume: None,
                        }),
                        _ => None,
                    };
                }
            }
            // `isset($obj->prop)` / `empty($obj->prop)` into a temporary:
            // a bool.
            TOp::IssetObjProp => {
                let &Op::IssetObjProp { place, dst, .. } = self.ops.get(inst.d as usize)? else {
                    return None;
                };
                self.object_place(st, place)?;
                let dst = tmp(dst.checked_mul(SLOT_U32)?)?;
                self.ty(st, dst)?;
                call.result = Some((dst, Ty::Bool));
            }
            // `isset($cv[dim])` / `empty($cv[dim])` into a temporary: a
            // bool. (The variable may hold anything; it is only read.)
            TOp::IssetDimC => {
                self.dim(st, &mut call, inst.k & 7, inst.b)?;
                let dst = tmp(inst.c)?;
                self.ty(st, dst)?;
                call.result = Some((dst, Ty::Bool));
            }
            // `is_int($cv)` & co. and `isset($cv)` / `empty($cv)`: into a
            // temporary (a bool), or fused with the jump on it.
            TOp::TypeCheckC | TOp::IssetC => {
                if inst.c != u32::MAX {
                    let dst = tmp(inst.c)?;
                    self.ty(st, dst)?;
                    call.result = Some((dst, Ty::Bool));
                }
            }
            TOp::TypeCheckJmpC | TOp::IssetJmpC => {
                call.jump = Some(self.target(i)?);
            }
            // `$cv .= val` on a string the loop does not track, the result
            // unused.
            TOp::ConcatCv => {
                array(self)?;
                if inst.c != u32::MAX {
                    return None;
                }
                self.value(st, &mut call, value_kind, inst.p)?;
            }
            // `count($cv)` / `strlen($cv)` into a temporary: an int.
            TOp::CountC | TOp::StrlenC => {
                if !self.untracked(st, cv(inst.a)?) {
                    return None;
                }
                if inst.c != u32::MAX {
                    let dst = tmp(inst.c)?;
                    self.ty(st, dst)?;
                    call.result = Some((dst, Ty::Int));
                }
            }
            _ => return None,
        }
        // A read whose short cut does not fit what it reads (a hash table,
        // elements that are not numbers) calls its handler. (A write's
        // short cut falls back on the handler by itself.)
        if matches!(
            call.fast,
            Some(Fast::Get { .. } | Fast::Get2 { .. } | Fast::PropGet { .. } | Fast::GetKey { .. })
        ) && (!self.seen.fast || (self.seen.slow)(i))
        {
            call.fast = None;
        }
        Some(call)
    }

    /// The operand `o` of an op as a number the loop knows, and the
    /// temporary it is consumed from.
    fn op_number(&mut self, st: &State, o: Operand) -> Option<(Opnd, Option<SlotRef>)> {
        Some(match o {
            Operand::Cv(c) => (self.number(st, cv(c.checked_mul(SLOT_U32)?)?)?, None),
            Operand::Tmp(t) => {
                let slot = tmp(t.checked_mul(SLOT_U32)?)?;
                (self.number(st, slot)?, Some(slot))
            }
            Operand::TmpKeep(t) => (self.number(st, tmp(t.checked_mul(SLOT_U32)?)?)?, None),
            Operand::Lit(l) => (literal(self.lits, l.checked_mul(SLOT_U32)?)?, None),
            Operand::Unused => return None,
        })
    }

    /// Instruction `i` planned from its op, for the ops without a tape
    /// instruction that [`plannable_op`] lists.
    fn op_plan(&mut self, st: &State, i: usize) -> Option<Plan> {
        let next = (i + 1 < self.tape.len()).then_some(i + 1)?;
        let temp = |t: u32| tmp(t.checked_mul(SLOT_U32)?);
        match self.ops.get(i)? {
            // `hot::move_in`: a copy; a temporary source is taken.
            &Op::Move { src, dst } => {
                let dst = temp(dst)?;
                self.ty(st, dst)?;
                // A bool (a variable's, a temporary's or a literal's).
                let literal_bool = matches!(src, Operand::Lit(l)
                    if matches!(self.lits.get(l as usize), Some(Value::Bool(_))));
                if literal_bool || !matches!(src, Operand::Lit(_)) {
                    if let Some((value @ (Truthy::Bool(_) | Truthy::Known(_)), consume)) =
                        self.op_truthy(st, src)
                    {
                        return Some(Plan::SetBool {
                            value,
                            negate: false,
                            consume,
                            dst,
                            next,
                        });
                    }
                }
                let (value, consume) = self.op_number(st, src)?;
                Some(match value.num {
                    Num::Slot(src) => Plan::Copy {
                        src,
                        dst,
                        float: value.float,
                        consume: consume.is_some(),
                        next,
                    },
                    _ => Plan::Const { dst, value, next },
                })
            }
            &Op::Cast { kind, a, dst } => {
                let dst = temp(dst)?;
                self.ty(st, dst)?;
                let (a, consume) = self.op_number(st, a)?;
                Some(Plan::Conv {
                    to_float: match kind {
                        CastKind::Float => true,
                        CastKind::Int => false,
                        _ => return None,
                    },
                    a,
                    consume,
                    dst,
                    next,
                })
            }
            Op::FramelessICall(call) => {
                let name = (self.seen.builtin)(call.name)?;
                let dst = temp(call.dst?)?;
                let mut args = Vec::new();
                for operand in &call.vals[..usize::from(call.argc).min(MAX_ARGS + 1)] {
                    args.push(self.op_number(st, *operand)?.0);
                }
                self.math(st, &name, &args, Some(dst), next)
            }
            &Op::JmpZEx { a, dst, target } | &Op::JmpNzEx { a, dst, target } => {
                let dst = temp(dst)?;
                self.ty(st, dst)?;
                let (value, consume) = self.op_truthy(st, a)?;
                let target = target as usize;
                (target < self.tape.len()).then_some(Plan::JmpEx {
                    value,
                    when: matches!(self.ops.get(i), Some(Op::JmpNzEx { .. })),
                    consume,
                    dst,
                    target,
                    fall: next,
                })
            }
            &Op::Unary {
                op: op @ (UnKind::Not | UnKind::Bool),
                a,
                dst,
            } => {
                let dst = temp(dst)?;
                self.ty(st, dst)?;
                let (value, consume) = self.op_truthy(st, a)?;
                Some(Plan::SetBool {
                    value,
                    negate: op == UnKind::Not,
                    consume,
                    dst,
                    next,
                })
            }
            // A jump on a literal's truth.
            &Op::JmpZ(a @ Operand::Lit(_), target) | &Op::JmpNz(a @ Operand::Lit(_), target) => {
                let (Truthy::Known(truth), _) = self.op_truthy(st, a)? else {
                    return None;
                };
                let jumps = truth == matches!(self.ops.get(i), Some(Op::JmpNz(..)));
                let to = if jumps { target as usize } else { next };
                (to < self.tape.len()).then_some(Plan::Jmp { to })
            }
            &Op::AssignCv {
                cv: var,
                val,
                dst: Some(dst),
            } => {
                let (cv, tmp) = (cv(var.checked_mul(SLOT_U32)?)?, temp(dst)?);
                self.ty(st, cv)?;
                self.ty(st, tmp)?;
                let (value, consume) = self.op_number(st, val)?;
                Some(Plan::Assign2 {
                    value,
                    consume,
                    cv,
                    tmp,
                    next,
                })
            }
            // `a ?? b` with `a` a number: never null, so its value and the
            // jump past `b`.
            &Op::Coalesce { a, dst, target } => {
                let dst = temp(dst)?;
                self.ty(st, dst)?;
                let (value, consume) = self.op_number(st, a)?;
                let Num::Slot(src) = value.num else {
                    return None;
                };
                let target = target as usize;
                (target < self.tape.len()).then_some(Plan::Copy {
                    src,
                    dst,
                    float: value.float,
                    consume: consume.is_some(),
                    next: target,
                })
            }
            // A constant whose value the run-time cache holds: that value,
            // into the temporary, or straight into the variable of the
            // `AssignCv` folded into the read (which is then skipped).
            &Op::FetchConst {
                fallback: None,
                dst,
                ..
            }
            | &Op::FetchClassConst {
                name: Operand::Lit(_),
                dst,
                ..
            } => self.constant(st, i, temp(dst)?, next),
            &Op::FetchConstTo {
                fallback: None, to, ..
            }
            | &Op::FetchClassConstTo { to, .. } => {
                let next = (i + 2 < self.tape.len()).then_some(i + 2)?;
                self.constant(st, i, cv(to.checked_mul(SLOT_U32)?)?, next)
            }
            // `A::$p` read: a number now, into the variable of the
            // `AssignCv` folded into the read, or into its temporary.
            &Op::FetchStaticProp {
                place,
                dst,
                to,
                mode: crate::opcode::FetchMode::Read,
            } => {
                if (self.seen.slow)(i) {
                    return None;
                }
                let ty = match (self.seen.statik)(place)? {
                    ty @ (Ty::Int | Ty::Float) => ty,
                    Ty::Bool | Ty::Plain => return None,
                };
                let (slot, next) = match to {
                    Some(to) => (cv(to.checked_mul(SLOT_U32)?)?, i + 2),
                    None => (temp(dst)?, i + 1),
                };
                self.ty(st, slot)?;
                // (With `to`, the temporary is written when the variable
                // cannot take the value: it too must own nothing.)
                self.ty(st, temp(dst)?)?;
                (next < self.tape.len()).then_some(Plan::Ext {
                    result: Some((slot, ty)),
                    consume: None,
                    after: i + 1,
                    next,
                })
            }
            // `isset(A::$p)` / `empty(A::$p)` into a temporary: a bool.
            &Op::IssetStaticProp { dst, .. } => {
                if (self.seen.slow)(i) {
                    return None;
                }
                let dst = temp(dst)?;
                self.ty(st, dst)?;
                Some(Plan::Ext {
                    result: Some((dst, Ty::Bool)),
                    consume: None,
                    after: next,
                    next,
                })
            }
            // `A::$p = val` as a statement, over a number.
            &Op::AssignStaticProp {
                place,
                val,
                dst: None,
            } => {
                if (self.seen.slow)(i) {
                    return None;
                }
                (self.seen.statik)(place)?;
                let consume = match val {
                    Operand::Tmp(t) => {
                        let slot = temp(t)?;
                        self.ty(st, slot)?;
                        Some(slot)
                    }
                    Operand::Cv(c) => {
                        // Read where it is: any scalar the frame shows.
                        self.ty(st, cv(c.checked_mul(SLOT_U32)?)?)?;
                        None
                    }
                    Operand::Lit(_) => None,
                    _ => return None,
                };
                Some(Plan::Ext {
                    result: None,
                    consume,
                    after: next,
                    next,
                })
            }
            Op::Return { .. } => Some(Plan::Leave),
            // `abs()` as its own op: over the two ops that were its
            // `Send` and `DoCall`, and with `to` straight into the variable
            // of the `AssignCv` after them, which is then skipped too.
            &Op::Abs { src, dst, to } => {
                let (a, _) = self.op_number(st, src)?;
                let (dst, skip) = match (to, dst) {
                    (Some(to), _) => (Some(cv(to.checked_mul(SLOT_U32)?)?), 4),
                    (None, Some(dst)) => (Some(temp(dst)?), 3),
                    (None, None) => (None, 3),
                };
                let next = (i + skip < self.tape.len()).then_some(i + skip)?;
                self.math(st, b"abs", &[a], dst, next)
            }
            // A builtin call with computed arguments: nothing to do at
            // its `InitFCall`, each `Send` keeps its argument, the
            // `DoCall` computes.
            Op::InitFCall { .. } => {
                let site = self.sites.iter().find(|site| site.init == i)?;
                if self.inlined_site(i).is_some() {
                    return Some(Plan::Jmp { to: next });
                }
                let name = (self.seen.builtin)(site.name)?;
                computed(&name, site.sends.len()).then_some(Plan::Jmp { to: next })
            }
            Op::Send { src } => {
                let (site, k) = self.sites.iter().find_map(|site| {
                    Some((site, site.sends.iter().position(|send| *send == i)?))
                })?;
                if k >= MAX_ARGS {
                    return None;
                }
                if self.inlined_site(site.init).is_none() {
                    let name = (self.seen.builtin)(site.name)?;
                    if !computed(&name, site.sends.len()) {
                        return None;
                    }
                }
                let k = self.arg_base(site)? + k;
                let operand = match *src {
                    // (A call's result is a value to a parameter that is
                    // not by reference: none of these functions' is.)
                    ArgSrc::Val(operand) | ArgSrc::CallResult(operand) => operand,
                    // A plain variable.
                    ArgSrc::Place(id) => match self.places.get(id as usize)? {
                        Place {
                            base: PlaceBase::Cv(var),
                            segs,
                        } if segs.is_empty() => Operand::Cv(*var),
                        _ => return None,
                    },
                    ArgSrc::TempPlace(_) => return None,
                };
                let (value, consume) = self.op_number(st, operand)?;
                Some(Plan::Arg {
                    k,
                    value,
                    consume,
                    next,
                })
            }
            &Op::DoCall { dst } => {
                let site = self.sites.iter().find(|site| site.call == i)?.clone();
                let args = self.arg_base(&site)?;
                if let Some((k, callee)) = self.inlined_site(site.init) {
                    // Every argument a number, kept since its `Send`, that
                    // its parameter's type takes as it is.
                    for k in 0..site.sends.len() {
                        let arg = self.number(st, arg_slot(args + k))?;
                        let param = callee.func.params.get(k)?;
                        if !takes(param.ty.as_ref(), num_ty(arg.float)) {
                            return None;
                        }
                    }
                    *self.bound.get_mut(k)? = true;
                    let code = &callee.func.code;
                    return Some(Plan::Bind {
                        args,
                        argc: site.sends.len(),
                        cvs: code.cv_names.len(),
                        tmps: code.num_tmps as usize,
                        to: callee.base + callee.first,
                    });
                }
                let name = (self.seen.builtin)(site.name)?;
                let mut args = Vec::new();
                let first = self.arg_base(&site)?;
                for k in 0..site.sends.len().min(MAX_ARGS + 1) {
                    args.push(self.number(st, arg_slot(first + k))?);
                }
                let dst = match dst {
                    Some(dst) => Some(temp(dst)?),
                    None => None,
                };
                self.math(st, &name, &args, dst, next)
            }
            _ => None,
        }
    }

    /// The constant the op at `i` reads, as a store of its value into
    /// `dst`; the loop's entry then checks that the value is still that.
    fn constant(&mut self, st: &State, i: usize, dst: SlotRef, next: usize) -> Option<Plan> {
        if self.inline {
            // (The entry's check reads the loop's own op array.)
            return None;
        }
        self.ty(st, dst)?;
        let (ty, bits) = (self.seen.constant)(i)?;
        if !self.consts.iter().any(|(at, ..)| *at == i) {
            self.consts.push((i, ty, bits));
        }
        #[allow(clippy::cast_possible_wrap)]
        let value = match ty {
            Ty::Int => Opnd {
                num: Num::Int(bits as i64),
                float: false,
            },
            Ty::Float => Opnd {
                num: Num::Float(f64::from_bits(bits)),
                float: true,
            },
            Ty::Bool => {
                return Some(Plan::SetBool {
                    value: Truthy::Known(bits != 0),
                    negate: false,
                    consume: None,
                    dst,
                    next,
                });
            }
            Ty::Plain => return None,
        };
        Some(Plan::Const { dst, value, next })
    }

    /// The builtin `name` called with the numbers `args`, its result
    /// going to `dst` (unused without one), as an instruction of the loop;
    /// `None` when the second tier does not compute this call with these
    /// types.
    fn math(
        &mut self,
        st: &State,
        name: &[u8],
        args: &[Opnd],
        dst: Option<SlotRef>,
        next: usize,
    ) -> Option<Plan> {
        if !computed(name, args.len()) {
            return None;
        }
        // The functions have no effect: an unused result is no work.
        let Some(dst) = dst else {
            return Some(Plan::Jmp { to: next });
        };
        self.ty(st, dst)?;
        match (name, args) {
            (b"intdiv", &[a, b]) if !a.float && !b.float => {
                if matches!(b.num, Num::Int(y) if !IntOp::Div.takes(y)) {
                    return None;
                }
                Some(Plan::Int2 {
                    op: IntOp::Div,
                    a,
                    b,
                    dst,
                    next,
                })
            }
            (b"min" | b"max", &[a, b]) if !a.float && !b.float => Some(Plan::Pick {
                max: name == b"max",
                a,
                b,
                dst,
                next,
            }),
            (b"abs", &[a]) if !a.float => Some(Plan::AbsInt { a, dst, next }),
            (b"abs", &[a]) => Some(Plan::Math {
                f: Math::Abs,
                a,
                dst,
                next,
            }),
            (name, &[a]) => {
                let f = math_fn(name)?;
                // `floor()`, `ceil()` and `round()` of an int are the int
                // as a float.
                if !a.float && matches!(name, b"floor" | b"ceil" | b"round") {
                    return Some(Plan::Conv {
                        to_float: true,
                        a,
                        consume: None,
                        dst,
                        next,
                    });
                }
                Some(Plan::Math { f, a, dst, next })
            }
            _ => None,
        }
    }

    /// Instruction `i` as the step of a `foreach` over an array of
    /// numbers, when it is one: where the element and its key end up
    /// follows `hot::step_ext`'s arm, which also runs the plain
    /// assignments to the loop's variables that follow the fetch.
    fn each_step(&mut self, st: &State, i: usize) -> Option<Plan> {
        let &Op::FeFetch {
            iter,
            end,
            val,
            key,
        } = self.ops.get(i)?
        else {
            return None;
        };
        let (float, int_keys) = (self.seen.each)(iter)?;
        // The variable the `AssignCv` at `at` copies temporary `t` into.
        let assigned = |an: &Self, at: usize, t: u32| match an.ops.get(at) {
            Some(&Op::AssignCv {
                cv: to,
                val: Operand::Tmp(src),
                dst: None,
            }) if src == t => cv(to.checked_mul(SLOT_U32)?),
            _ => None,
        };
        let temp = |t: u32| tmp(t.checked_mul(SLOT_U32)?);
        let mut consumed = [None, None];
        let mut full = i + 1;
        let value = match assigned(self, full, val) {
            Some(to) => {
                consumed[0] = Some(temp(val)?);
                full += 1;
                to
            }
            None => temp(val)?,
        };
        let key = match key {
            None => None,
            Some(_) if !int_keys => return None,
            // Its assignment only runs with the value's.
            Some(k) => Some(
                match assigned(self, full, k).filter(|_| consumed[0].is_some()) {
                    Some(to) => {
                        consumed[1] = Some(temp(k)?);
                        full += 1;
                        to
                    }
                    None => temp(k)?,
                },
            ),
        };
        // All of them own nothing now, so the op's stores release nothing.
        for slot in [Some(value), key, consumed[0], consumed[1]]
            .into_iter()
            .flatten()
        {
            self.ty(st, slot)?;
        }
        let end = end as usize;
        (full < self.tape.len() && end < self.tape.len()).then_some(Plan::Each {
            slim: self.seen.fast && !(self.seen.slow)(i),
            iter,
            end,
            full,
            value: (value, float),
            key,
            consumed,
        })
    }

    /// Whether the place `id` is one the property cache covers (`$cv->p`,
    /// `$this->p`): the short cuts read the cache's entry for it.
    fn cached_place(&self, id: u32) -> bool {
        self.places
            .get(id as usize)
            .is_some_and(crate::place::propcache::cacheable)
    }

    /// Checks that the place `id` is a property path from an object the
    /// loop does not track (`$obj->a`, `$this->a->b`), with literal names:
    /// its handler then reads and writes no slot the loop tracks.
    fn object_place(&self, st: &State, id: u32) -> Option<()> {
        let place = self.places.get(id as usize)?;
        match place.base {
            PlaceBase::Cv(base) => {
                let slot = cv(base.checked_mul(SLOT_U32)?)?;
                if !self.untracked(st, slot) {
                    return None;
                }
            }
            PlaceBase::This => {}
            _ => return None,
        }
        (!place.segs.is_empty()
            && place
                .segs
                .iter()
                .all(|seg| matches!(seg, Seg::Prop(Operand::Lit(_)))))
        .then_some(())
    }

    /// Whether the numbers in the array variable `cv` holds, `depth`
    /// arrays down, are floats (by its first element now); `None` when
    /// they are not numbers.
    fn element_float(&self, cv: u32, depth: usize) -> Option<bool> {
        match (self.seen.element)(cv, depth)? {
            Ty::Int => Some(false),
            Ty::Float => Some(true),
            Ty::Bool | Ty::Plain => None,
        }
    }

    /// Notes that the handler of `call` may consume the temporary with
    /// index `index`, which must own nothing.
    fn consume(&mut self, st: &State, call: &mut Call, index: u32) -> Option<SlotRef> {
        let slot = tmp(index.checked_mul(SLOT_U32)?)?;
        self.ty(st, slot)?;
        *call.consumed.iter_mut().find(|c| c.is_none())? = Some(slot);
        Some(slot)
    }

    /// An offset operand of a `dims` instruction (kind and index): noted
    /// as consumed when it is a consumed temporary. `Some(Some(int))` when
    /// the loop has it as an int in a register, `Some(None)` for any other
    /// offset (the handler reads it from the frame).
    fn dim(&mut self, st: &State, call: &mut Call, kind: u8, index: u32) -> Option<Option<Opnd>> {
        if kind == DIM_TMP {
            self.consume(st, call, index)?;
        } else if kind == DIM_UNUSED {
            return None;
        }
        Some(self.dim_number(st, kind, index).filter(|o| !o.float))
    }

    /// The value operand of a `dims` write (kind and index), as
    /// [`Self::dim`]: the number the loop has for it, if any, and the
    /// temporary it is consumed from.
    fn value(
        &mut self,
        st: &State,
        call: &mut Call,
        kind: u8,
        index: usize,
    ) -> Option<(Option<Opnd>, Option<SlotRef>)> {
        let index = u32::try_from(index).ok()?;
        let consumed = if kind == DIM_TMP {
            Some(self.consume(st, call, index)?)
        } else {
            None
        };
        Some((self.dim_number(st, kind, index), consumed))
    }

    /// The offsets of the place `id`, which must be a path of array
    /// offsets from the variable `cv` (`$cv[d1][d2]...`): each as
    /// [`Self::dim`] gives it.
    fn place_dims(
        &mut self,
        st: &State,
        call: &mut Call,
        cv: u32,
        id: u32,
    ) -> Option<Vec<Option<Opnd>>> {
        let place = self.places.get(id as usize)?;
        if !matches!(place.base, PlaceBase::Cv(base) if base == cv) || place.segs.is_empty() {
            return None;
        }
        let mut dims = Vec::with_capacity(place.segs.len());
        for seg in &place.segs {
            let Seg::Dim(offset) = seg else {
                return None;
            };
            let (kind, index) = match *offset {
                Operand::Cv(index) => (DIM_CV, index),
                Operand::Tmp(index) => (DIM_TMP, index),
                Operand::TmpKeep(index) => (DIM_TMPKEEP, index),
                Operand::Lit(index) => (DIM_LIT, index),
                Operand::Unused => return None,
            };
            dims.push(self.dim(st, call, kind, index)?);
        }
        Some(dims)
    }

    /// Where a fetch leaves its number: in the variable `to` of the
    /// `AssignCv` folded into it (the handler then answers "jumped", past
    /// that op; when it stored into the temporary instead, the `AssignCv`
    /// is the first tier's), or, without one (`u32::MAX`), in the
    /// temporary with index `dst`.
    fn fetched(
        &mut self,
        st: &State,
        call: &mut Call,
        i: usize,
        to: u32,
        dst: u32,
        float: bool,
    ) -> Option<()> {
        if to == u32::MAX {
            let dst = tmp(dst.checked_mul(SLOT_U32)?)?;
            self.ty(st, dst)?;
            call.result = Some((dst, num_ty(float)));
        } else {
            let to = cv(to.checked_mul(SLOT_U32)?)?;
            self.ty(st, to)?;
            call.result = Some((to, num_ty(float)));
            call.jump = Some(self.target(i)?);
            call.fall = None;
        }
        Some(())
    }

    /// Merges the types `new` into `cur` (two ways into one instruction):
    /// a slot with two different types is only known to own nothing.
    /// Returns whether `cur` changed.
    ///
    /// A slot one of the two ways has not written still holds what it held
    /// on entry there: it keeps a type only if the other way's is the one
    /// the frame shows for it now, which then becomes its entry type (as
    /// for a read, [`Analysis::read`]). (Taken as "no type yet", it let
    /// `if ($a < 18) { $c = $a; } $d = $c % 8;` read the int `$c` came in
    /// with after the float was assigned.)
    fn merge(&mut self, cur: &mut State, new: &State) -> bool {
        let slots: Vec<SlotRef> = cur.keys().chain(new.keys()).copied().collect();
        let mut changed = false;
        for slot in slots {
            let merged = match (cur.get(&slot).copied(), new.get(&slot).copied()) {
                (Some(a), Some(b)) if a == b => continue,
                (Some(_), Some(_)) => Ty::Plain,
                (Some(ty), None) | (None, Some(ty)) => {
                    let held = if ty == Ty::Plain || slot.at < 0 {
                        None
                    } else {
                        (self.seen.snapshot)(slot)
                    };
                    if held == Some(ty) {
                        self.entry.insert(slot, ty);
                        ty
                    } else {
                        Ty::Plain
                    }
                }
                (None, None) => continue,
            };
            if cur.get(&slot) != Some(&merged) {
                cur.insert(slot, merged);
                changed = true;
            }
        }
        changed
    }
}

/// The size of a slot, for turning a slot index into an offset.
#[allow(clippy::cast_possible_truncation)]
const SLOT_U32: u32 = SLOT as u32;
/// The kinds of a `dims` instruction's operand (`exec/tape/dims.rs`,
/// `KIND_*`): a variable, a consumed temporary, a kept temporary, a
/// literal.
const DIM_CV: u8 = 0;
const DIM_TMP: u8 = 1;
const DIM_TMPKEEP: u8 = 2;
const DIM_LIT: u8 = 3;
/// The kind of a `dims` instruction's absent offset (`KIND_UNUSED`).
const DIM_UNUSED: u8 = 4;

/// The type of a number that is a float, or an int.
fn num_ty(float: bool) -> Ty {
    if float {
        Ty::Float
    } else {
        Ty::Int
    }
}

/// The compiled variable at operand offset `x`.
fn cv(x: u32) -> Option<SlotRef> {
    Some(SlotRef {
        tmp: false,
        at: off(x)?,
    })
}

/// The temporary at operand offset `x`.
fn tmp(x: u32) -> Option<SlotRef> {
    Some(SlotRef {
        tmp: true,
        at: off(x)?,
    })
}

/// The number literal at operand offset `x`.
fn literal(lits: &[Value], x: u32) -> Option<Opnd> {
    match lits.get(x as usize / SLOT)? {
        Value::Int(i) => Some(Opnd {
            num: Num::Int(*i),
            float: false,
        }),
        Value::Float(f) => Some(Opnd {
            num: Num::Float(*f),
            float: true,
        }),
        _ => None,
    }
}

/// Analyses the loop of `tape` that starts at `head`, with the types
/// `snapshot` reports for the slots it uses. `None` when the second tier
/// cannot compile it: some instruction reachable inside it is not a
/// numeric one, an operand is not known to be a number, or no round can
/// follow another with the same types.
pub(super) fn analyse(tape: &[Inst], head: usize, code: &OpArray, seen: &Seen<'_>) -> Option<Loop> {
    let sites = call_sites(&code.ops);
    // The calls of user functions from the head on that may run inline.
    let mut inl: Vec<Inlined> = Vec::new();
    for site in &sites {
        if site.init < head || site.sends.len() > MAX_ARGS {
            continue;
        }
        let Some(func) = (seen.user)(site.name, site.sends.len()) else {
            continue;
        };
        let callee = super::super::lower(&func.code);
        if callee.len() > INLINE_SPAN {
            continue;
        }
        inl.push(Inlined {
            init: site.init,
            call: site.call,
            name: site.name,
            base: tape.len() + INLINE_SPAN * inl.len(),
            argc: site.sends.len(),
            first: func.plain_first_op(site.sends.len()),
            func,
            tape: callee,
        });
    }
    let mut an = Analysis {
        tape,
        lits: &code.literals,
        seen,
        places: &code.places,
        ops: &code.ops,
        sites,
        entry: State::new(),
        consts: Vec::new(),
        inl: &inl,
        real: tape.len(),
        inline: false,
        bound: vec![false; inl.len()],
    };
    // The loop's instructions: from its head to its last jump back.
    let last = (head..tape.len()).rev().find(|i| {
        (an.target(*i) == Some(head) && jumps(&tape[*i]))
            || matches!(
                code.ops.get(*i),
                Some(&(Op::JmpNz(Operand::Lit(_), to) | Op::JmpZ(Operand::Lit(_), to)))
                    if to as usize == head
            )
    })?;
    // (An inlined function's instructions are after the tape's own.)
    let inside = |i: usize| (i > head && i <= last) || i >= tape.len();
    let mut ins: BTreeMap<usize, State> = BTreeMap::new();
    ins.insert(head, State::new());
    let mut work = vec![head];
    let mut visits = 0;
    while let Some(i) = work.pop() {
        visits += 1;
        if visits > MAX_VISITS || ins.len() > MAX_REGION {
            return None;
        }
        let before = ins.get(&i)?.clone();
        let plan = an.plan(&before, i)?;
        for to in plan.successors().into_iter().flatten() {
            if !inside(to) {
                // The jump back to the head, or a way out of the loop.
                continue;
            }
            let st = plan.out(&before, to);
            match ins.get_mut(&to) {
                None => {
                    ins.insert(to, st.clone());
                    work.push(to);
                }
                Some(cur) => {
                    if an.merge(cur, &st) {
                        work.push(to);
                    }
                }
            }
        }
    }
    let mut plans = BTreeMap::new();
    for (i, st) in ins {
        let plan = an.plan(&st, i)?;
        plans.insert(i, (plan, st));
    }
    // The calls the loop computes: their arguments must be redoable.
    let mut resume = BTreeMap::new();
    for site in &an.sites {
        if !matches!(plans.get(&site.init), Some((Plan::Jmp { .. }, _))) {
            continue;
        }
        for at in site.init + 1..=site.call {
            if let Some((plan, _)) = plans.get(&at) {
                if !plan.redoable() {
                    return None;
                }
            }
            // The outermost call's start, for a call in an argument.
            let start = resume.get(&site.init).copied().unwrap_or(site.init);
            let known = resume.entry(at).or_insert(start);
            *known = (*known).min(start);
        }
    }
    // An inlined function has no frame to go on in: whatever stops in it
    // has the whole call redone, like one that stops among the arguments.
    let mut virtuals = Vec::new();
    let mut inlined = Vec::new();
    for (callee, _) in inl.iter().zip(&an.bound).filter(|(_, bound)| **bound) {
        if !matches!(plans.get(&callee.init), Some((Plan::Jmp { .. }, _))) {
            return None;
        }
        let start = resume.get(&callee.init).copied().unwrap_or(callee.init);
        for at in callee.base..callee.base + callee.tape.len() {
            if plans.contains_key(&at) {
                resume.insert(at, start);
            }
        }
        let code = &callee.func.code;
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        for (tmp, count) in [(false, code.cv_names.len()), (true, code.num_tmps as usize)] {
            for k in 0..count {
                let slot = virtual_slot(SlotRef {
                    tmp,
                    at: (k * SLOT) as i32,
                });
                if !virtuals.contains(&slot) {
                    virtuals.push(slot);
                }
            }
        }
        // Kept alive for good, as the code is: its address then names it.
        let func = std::rc::Rc::clone(&callee.func);
        inlined.push((callee.name, std::rc::Rc::as_ptr(&func) as usize));
        std::mem::forget(func);
    }
    let lp = Loop {
        head,
        entry: an.entry,
        plans,
        resume,
        consts: an.consts,
        real: tape.len(),
        virtuals,
        inlined,
    };
    // Worth it only if some round can follow another.
    let repeats = lp.plans.values().any(|(plan, st)| {
        plan.successors().contains(&Some(head)) && lp.may_repeat(&plan.out(st, head))
    });
    repeats.then_some(lp)
}

/// Whether the loop of `tape` from `head` to its last jump back `last` is
/// worth showing to [`analyse`] when it is first reached: every one of its
/// instructions is of a kind the second tier has a form for. Decided when
/// the tape is compiled, so that the first-tier code of any other loop
/// (one with an array, a string, a `%`) carries no test for typed code at
/// its head: that test cost such loops 4 instructions a round.
pub(super) fn candidate(tape: &[Inst], code: &OpArray, head: usize, last: usize) -> bool {
    last - head < MAX_REGION
        && (head..=last).all(|i| {
            let Some(inst) = tape.get(i) else {
                return false;
            };
            // (`Ext` in a loop the first tier compiles is the step of a
            // `foreach`.)
            matches!(
                inst.op,
                TOp::Nop
                    | TOp::Ext
                    | TOp::FetchDimC
                    | TOp::FetchDimIC
                    | TOp::FetchDimSC
                    | TOp::FetchDimsC
                    | TOp::AssignDimC
                    | TOp::AssignDimIC
                    | TOp::AssignDimsC
                    | TOp::AssignOpDimC
                    | TOp::AssignOpDimsC
                    | TOp::IncDecDimC
                    | TOp::FetchObjProp
                    | TOp::FetchObjPropTo
                    | TOp::AssignObjProp
                    | TOp::AssignOpObjProp
                    | TOp::IncDecObjProp
                    | TOp::IssetObjProp
                    | TOp::IssetDimC
                    | TOp::TypeCheckC
                    | TOp::TypeCheckJmpC
                    | TOp::IssetC
                    | TOp::IssetJmpC
                    | TOp::ConcatCv
                    | TOp::CountC
                    | TOp::StrlenC
                    | TOp::JmpZC
                    | TOp::JmpNzC
                    | TOp::NumBinT
                    | TOp::NumBinC
                    | TOp::NumAssignOpC
            ) || matches!(
                inst.op.shape(),
                Shape::Jmp
                    // The jump op a smart branch covers: in the loop's
                    // range, never reached.
                    | Shape::JmpBoolT(_)
                    | Shape::Cmp { .. }
                    | Shape::IncCmp { .. }
                    | Shape::ArithTmp { .. }
                    | Shape::ArithCv { .. }
                    | Shape::AssignOp { .. }
                    | Shape::IncDecC(_)
                    | Shape::IncDecT { .. }
                    | Shape::AssignI
                    | Shape::AssignL
                    | Shape::AssignC
                    | Shape::AssignT
            ) || code
                .ops
                .get(i)
                .is_some_and(|op| plannable_op(op, &code.literals))
        })
}

/// Whether `inst` is an instruction that may jump.
fn jumps(inst: &Inst) -> bool {
    match inst.op.shape() {
        Shape::Jmp | Shape::JmpBoolT(_) | Shape::Cmp { .. } | Shape::IncCmp { .. } => true,
        Shape::Family { ending, .. } => ending.branches && inst.c != 0,
        _ => false,
    }
}

/// `dims::jit_get_key`.
type GetKey = unsafe extern "C" fn(*const crate::vm::Frame, usize, usize, *mut Value) -> usize;
/// `spec::jit_prop_get`.
type PropGet = unsafe extern "C" fn(*mut crate::vm::Frame, usize, *mut Value) -> usize;
/// `spec::jit_prop_set`.
type PropSet = unsafe extern "C" fn(*mut crate::vm::Frame, usize, usize, u64) -> usize;
/// `spec::jit_prop_op`.
type PropOp = unsafe extern "C" fn(*mut crate::vm::Frame, usize, usize, usize, u64) -> usize;
/// `dims::jit_get_num`.
type GetNum = unsafe extern "C" fn(*const Option<Value>, i64, *mut Value) -> usize;
/// `dims::jit_set_num`.
type SetNum = unsafe extern "C" fn(*mut Option<Value>, i64, usize, u64) -> usize;
/// `dims::jit_get_num2`.
type GetNum2 = unsafe extern "C" fn(*const Option<Value>, i64, i64, *mut Value) -> usize;
/// `dims::jit_set_num2`.
type SetNum2 = unsafe extern "C" fn(*mut Option<Value>, i64, i64, usize, u64) -> usize;
/// `dims::jit_op_num`.
type OpNum = unsafe extern "C" fn(*mut Option<Value>, i64, usize, usize, u64) -> usize;
/// `super::user_is`.
type UserIs = unsafe extern "C" fn(*mut crate::vm::Frame, usize, usize) -> usize;
/// `super::const_is`.
type ConstIs = unsafe extern "C" fn(*mut crate::vm::Frame, usize, usize, u64) -> usize;
/// `super::each_num`.
type EachNum = unsafe extern "C" fn(*mut crate::vm::Frame, usize, *mut Value, *mut Value) -> usize;

/// A block an instruction branches to, filled once the instruction's own
/// block is.
#[derive(Clone, Copy)]
enum Pending {
    /// Goes on at `to`, coming from `from`; `repeat` is whether the types
    /// allow another round, should `to` be the loop head.
    Edge {
        from: usize,
        to: usize,
        repeat: bool,
    },
    /// An int overflowed at the instruction: the first tier redoes it.
    Deopt(usize),
}

/// Builds the function of one typed loop.
struct Emit<'a, 'b> {
    b: FunctionBuilder<'b>,
    lp: &'a Loop,
    cvs: V,
    tmps: V,
    frame: V,
    record: V,
    /// The address of the safe-point flag, loaded once (the loop polls it
    /// on every round).
    flag: V,
    /// The signature of a family handler.
    handler_sig: SigRef,
    /// The signatures of the array helpers, by their number of arguments
    /// less three.
    helper_sigs: [SigRef; 3],
    /// The signature of a [`MathFn`].
    math_sig: SigRef,
    /// The int and the float variable of each slot.
    vars: BTreeMap<SlotRef, (Variable, Variable)>,
    blocks: BTreeMap<usize, Block>,
    pending: Vec<(Block, Pending)>,
}

/// Emits the function of the typed loop `lp` into `b`, with the signature
/// of the first tier's code (`super::Code`). `none_tag` is the tag of an
/// undefined variable.
#[allow(clippy::too_many_lines)]
pub(super) fn emit(mut b: FunctionBuilder<'_>, lp: &Loop, none_tag: u8) -> Option<()> {
    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    let params = b.block_params(entry).to_vec();
    let &[cvs, tmps, _lits, frame, _entry, record] = params.as_slice() else {
        return None;
    };
    let mut handler_sig = Signature::new(b.func.signature.call_conv);
    handler_sig
        .params
        .extend(std::iter::repeat_n(AbiParam::new(types::I64), 2));
    handler_sig.returns.push(AbiParam::new(types::I64));
    let helper_sig = |args: usize| {
        let mut sig = Signature::new(handler_sig.call_conv);
        sig.params
            .extend(std::iter::repeat_n(AbiParam::new(types::I64), args));
        sig.returns.push(AbiParam::new(types::I64));
        sig
    };
    let helper_sigs = [3, 4, 5].map(|args| b.import_signature(helper_sig(args)));
    let mut math_sig = Signature::new(handler_sig.call_conv);
    math_sig.params.push(AbiParam::new(types::F64));
    math_sig.returns.push(AbiParam::new(types::F64));
    let math_sig = b.import_signature(math_sig);
    let handler_sig = b.import_signature(handler_sig);
    let mut vars = BTreeMap::new();
    for (n, slot) in lp.entry.keys().enumerate() {
        let (int, float) = (
            Variable::from_u32(u32::try_from(2 * n).ok()?),
            Variable::from_u32(u32::try_from(2 * n + 1).ok()?),
        );
        b.declare_var(int, types::I64);
        b.declare_var(float, types::F64);
        vars.insert(*slot, (int, float));
    }
    for k in 0..ARG_VARS {
        let n = 2 * (lp.entry.len() + k);
        let (int, float) = (
            Variable::from_u32(u32::try_from(n).ok()?),
            Variable::from_u32(u32::try_from(n + 1).ok()?),
        );
        b.declare_var(int, types::I64);
        b.declare_var(float, types::F64);
        vars.insert(arg_slot(k), (int, float));
    }
    for (k, slot) in lp.virtuals.iter().enumerate() {
        let n = 2 * (lp.entry.len() + ARG_VARS + k);
        let (int, float) = (
            Variable::from_u32(u32::try_from(n).ok()?),
            Variable::from_u32(u32::try_from(n + 1).ok()?),
        );
        b.declare_var(int, types::I64);
        b.declare_var(float, types::F64);
        vars.insert(*slot, (int, float));
    }
    let flag = b
        .ins()
        .load(types::I64, mem().with_readonly(), record, rec_at(REC_FLAG)?);
    let blocks = lp.plans.keys().map(|i| (*i, b.create_block())).collect();
    let mut e = Emit {
        b,
        lp,
        cvs,
        tmps,
        frame,
        record,
        flag,
        handler_sig,
        helper_sigs,
        math_sig,
        vars,
        blocks,
        pending: Vec::new(),
    };
    // The guards: what the analysis assumed of the frame.
    let bail = e.b.create_block();
    e.b.set_cold_block(bail);
    for (slot, ty) in &lp.entry {
        let base = e.base(*slot);
        let tag = e.b.ins().load(types::I8, mem(), base, slot.at);
        let ok = match ty {
            Ty::Int => e.b.ins().icmp_imm(IntCC::Equal, tag, i64::from(TAG_INT)),
            Ty::Float => e.b.ins().icmp_imm(IntCC::Equal, tag, i64::from(TAG_FLOAT)),
            Ty::Bool => e.b.ins().icmp_imm(IntCC::Equal, tag, i64::from(TAG_BOOL)),
            Ty::Plain => {
                let scalar = e.b.ins().icmp_imm(
                    IntCC::UnsignedLessThanOrEqual,
                    tag,
                    i64::from(TAG_LAST_SCALAR),
                );
                if slot.tmp {
                    scalar
                } else {
                    let undef = e.b.ins().icmp_imm(IntCC::Equal, tag, i64::from(none_tag));
                    e.b.ins().bor(scalar, undef)
                }
            }
        };
        let next = e.b.create_block();
        e.b.ins().brif(ok, next, &[], bail, &[]);
        e.b.switch_to_block(next);
    }
    // The functions the loop runs inline: still what their names call.
    for (name, func) in &lp.inlined {
        #[allow(clippy::cast_possible_wrap)]
        let args = [
            e.frame,
            e.b.ins().iconst(types::I64, i64::from(*name)),
            e.b.ins().iconst(types::I64, *func as i64),
        ];
        let same = e.helper(super::user_is as UserIs as usize, &args)?;
        let next = e.b.create_block();
        e.b.ins().brif(same, next, &[], bail, &[]);
        e.b.switch_to_block(next);
    }
    // The constants the loop was compiled with: still those values.
    for (at, ty, bits) in &lp.consts {
        let tag = match ty {
            Ty::Int => TAG_INT,
            Ty::Float => TAG_FLOAT,
            Ty::Bool => TAG_BOOL,
            Ty::Plain => return None,
        };
        #[allow(clippy::cast_possible_wrap)]
        let args = [
            e.frame,
            e.b.ins().iconst(types::I64, i64::try_from(*at).ok()?),
            e.b.ins().iconst(types::I64, i64::from(tag)),
            e.b.ins().iconst(types::I64, *bits as i64),
        ];
        let same = e.helper(super::const_is as ConstIs as usize, &args)?;
        let next = e.b.create_block();
        e.b.ins().brif(same, next, &[], bail, &[]);
        e.b.switch_to_block(next);
    }
    // The numbers the loop starts with, into their variables.
    for (slot, ty) in &lp.entry {
        let base = e.base(*slot);
        let (int, float) = *e.vars.get(slot)?;
        match ty {
            Ty::Int => {
                let v =
                    e.b.ins()
                        .load(types::I64, mem(), base, slot.at + PAYLOAD_AT);
                e.b.def_var(int, v);
            }
            Ty::Float => {
                let v =
                    e.b.ins()
                        .load(types::F64, mem(), base, slot.at + PAYLOAD_AT);
                e.b.def_var(float, v);
            }
            // A bool lives in the slot's int variable, as 0 or 1.
            Ty::Bool => {
                let v = e.b.ins().uload8(types::I64, mem(), base, slot.at + BOOL_AT);
                e.b.def_var(int, v);
            }
            Ty::Plain => {}
        }
    }
    let head = *e.blocks.get(&lp.head)?;
    e.b.ins().jump(head, &[]);
    e.b.switch_to_block(bail);
    e.ret(lp.head, WHY_NOT_TYPED);

    for (i, (plan, st)) in &lp.plans {
        e.b.switch_to_block(*e.blocks.get(i)?);
        e.inst(*i, plan, st)?;
        while let Some((block, what)) = e.pending.pop() {
            e.b.switch_to_block(block);
            match what {
                Pending::Edge { from, to, repeat } => e.go(from, to, repeat)?,
                Pending::Deopt(at) => {
                    let at = lp.resume.get(&at).copied().unwrap_or(at);
                    e.ret(at, WHY_GENERIC);
                }
            }
        }
    }
    e.b.seal_all_blocks();
    e.b.finalize();
    Some(())
}

impl Emit<'_, '_> {
    /// The base address of the slot vector `slot` is in.
    fn base(&self, slot: SlotRef) -> V {
        if slot.tmp {
            self.tmps
        } else {
            self.cvs
        }
    }

    /// Returns: stopped for `why` at instruction `i`. (In an inlined
    /// function there is nowhere to stop: the call is redone from its
    /// start, [`Loop::resume`].)
    fn ret(&mut self, i: usize, why: u8) {
        let (i, why) = if i >= self.lp.real {
            (
                self.lp.resume.get(&i).copied().unwrap_or(self.lp.head),
                WHY_GENERIC,
            )
        } else {
            (i, why)
        };
        let packed = (i << WHY_BITS) | usize::from(why);
        #[allow(clippy::cast_possible_wrap)]
        let r = self.b.ins().iconst(types::I64, packed as i64);
        self.b.ins().return_(&[r]);
    }

    /// Ends the current block by going on at instruction `to`, coming from
    /// `from`: inside the loop when `to` is one of its instructions (a
    /// backward jump polls for a safe point first, as every taken jump of
    /// the tape does), else back in the first tier.
    fn go(&mut self, from: usize, to: usize, repeat: bool) -> Option<()> {
        if to == self.lp.head && !repeat {
            // The next round would start with other types.
            self.ret(to, WHY_NOT_TYPED);
            return Some(());
        }
        let Some(block) = self.blocks.get(&to).copied() else {
            self.ret(to, WHY_GENERIC);
            return Some(());
        };
        // (Out of an inlined function, back into the loop: no jump back.)
        if to > from || from >= self.lp.real {
            self.b.ins().jump(block, &[]);
            return Some(());
        }
        let flag = self.flag;
        // Set from other threads and signal handlers: a plain load.
        let due = self
            .b
            .ins()
            .load(types::I8, MemFlags::new().with_notrap(), flag, 0);
        let poll = self.b.create_block();
        self.b.set_cold_block(poll);
        self.b.ins().brif(due, poll, &[], block, &[]);
        self.b.switch_to_block(poll);
        let at = self.b.ins().iconst(types::I64, i64::try_from(from).ok()?);
        self.b.ins().store(mem(), at, self.record, rec_at(REC_AT)?);
        self.ret(to, super::WHY_POLL);
        Some(())
    }

    /// A new block that goes on at `to` ([`Self::go`]), for a branch.
    fn land(&mut self, from: usize, to: usize, out: &State) -> Block {
        let block = self.b.create_block();
        let repeat = self.lp.may_repeat(out);
        self.pending
            .push((block, Pending::Edge { from, to, repeat }));
        block
    }

    /// A cold block for an int overflow at instruction `i`.
    fn deopt(&mut self, i: usize) -> Block {
        let block = self.b.create_block();
        self.b.set_cold_block(block);
        self.pending.push((block, Pending::Deopt(i)));
        block
    }

    /// Returns: stopped for `why` at the instruction whose index is the
    /// value `index`.
    fn ret_at(&mut self, index: V, why: u8) {
        let shifted = self.b.ins().ishl_imm(index, i64::from(WHY_BITS));
        let packed = self.b.ins().bor_imm(shifted, i64::from(why));
        self.b.ins().return_(&[packed]);
    }

    /// Reads the number a call left in `slot` back into its variable; if
    /// it is not of the expected type, the first tier goes on at
    /// instruction `dest` (the instruction that stored it is done).
    fn read_back(&mut self, slot: SlotRef, ty: Ty, dest: usize) -> Option<()> {
        let base = self.base(slot);
        let tag = self.b.ins().load(types::I8, mem(), base, slot.at);
        let expected = match ty {
            Ty::Int => TAG_INT,
            Ty::Float => TAG_FLOAT,
            Ty::Bool => TAG_BOOL,
            Ty::Plain => return None,
        };
        let ok = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, tag, i64::from(expected));
        let (other, next) = (self.b.create_block(), self.b.create_block());
        self.b.set_cold_block(other);
        self.b.ins().brif(ok, next, &[], other, &[]);
        self.b.switch_to_block(other);
        self.ret(dest, WHY_GENERIC);
        self.b.switch_to_block(next);
        let (int, fl) = *self.vars.get(&slot)?;
        match ty {
            Ty::Float => {
                let v = self
                    .b
                    .ins()
                    .load(types::F64, mem(), base, slot.at + PAYLOAD_AT);
                self.b.def_var(fl, v);
            }
            // A bool lives in the int variable, as 0 or 1.
            Ty::Bool => {
                let v = self
                    .b
                    .ins()
                    .uload8(types::I64, mem(), base, slot.at + BOOL_AT);
                self.b.def_var(int, v);
            }
            _ => {
                let v = self
                    .b
                    .ins()
                    .load(types::I64, mem(), base, slot.at + PAYLOAD_AT);
                self.b.def_var(int, v);
            }
        }
        Some(())
    }

    /// Polls for a safe point asked by instruction `at`: when one is due,
    /// returns with `WHY_POLL`, to go on at instruction `to`; else goes on
    /// in a new block.
    fn poll(&mut self, at: usize, to: usize) -> Option<()> {
        let flag = self.flag;
        let due = self
            .b
            .ins()
            .load(types::I8, MemFlags::new().with_notrap(), flag, 0);
        let (poll, next) = (self.b.create_block(), self.b.create_block());
        self.b.set_cold_block(poll);
        self.b.ins().brif(due, poll, &[], next, &[]);
        self.b.switch_to_block(poll);
        let at = self.b.ins().iconst(types::I64, i64::try_from(at).ok()?);
        self.b.ins().store(mem(), at, self.record, rec_at(REC_AT)?);
        self.ret(to, super::WHY_POLL);
        self.b.switch_to_block(next);
        Some(())
    }

    /// Emits the call of a family handler for instruction `i` and what
    /// follows from its answer; `out` is the types after it.
    fn call(&mut self, i: usize, call: &Call, out: &State) -> Option<()> {
        if let Some(fast) = call.fast {
            // The short cut alone. A read it does not take is run outside
            // the loop (one call an access keeps the loop's code, and the
            // time to compile it, at half), and counted; a write falls
            // back on its handler in place (new elements and rows are
            // what writes are for).
            if self.fast(i, call, fast, out)? {
                self.ret(i, WHY_MISS);
            }
            return Some(());
        }
        #[allow(clippy::cast_possible_wrap)]
        let callee = self.b.ins().iconst(types::I64, call.handler as i64);
        let tape = self.b.ins().load(
            types::I64,
            mem().with_readonly(),
            self.record,
            rec_at(REC_TAPE)?,
        );
        let inst = self
            .b
            .ins()
            .iadd_imm(tape, i64::try_from(i * std::mem::size_of::<Inst>()).ok()?);
        let made = self
            .b
            .ins()
            .call_indirect(self.handler_sig, callee, &[self.frame, inst]);
        let r = *self.b.inst_results(made).first()?;
        // Declined, nothing changed: the first tier's (and from there the
        // generic op's).
        let deopt = self.deopt(i);
        let done = self.b.create_block();
        self.b.ins().brif(r, done, &[], deopt, &[]);
        self.b.switch_to_block(done);
        let repeat = self.lp.may_repeat(out);
        let Some(jump) = call.jump else {
            return self.after_call(i, call, call.fall, repeat);
        };
        let (jumped, fell) = (self.b.create_block(), self.b.create_block());
        let took = self.b.ins().icmp_imm(IntCC::Equal, r, 2);
        self.b.ins().brif(took, jumped, &[], fell, &[]);
        self.b.switch_to_block(fell);
        // A fall-through that is the first tier's: no result to read.
        if call.fall.is_some() {
            self.after_call(i, call, call.fall, repeat)?;
        } else {
            if call.polls {
                self.poll(i, call.after)?;
            }
            self.ret(call.after, WHY_GENERIC);
        }
        self.b.switch_to_block(jumped);
        self.after_call(i, call, Some(jump), repeat)
    }

    /// Emits the short cut `fast` of instruction `i`; leaves the builder
    /// in the block reached when the helper did not take the case, where
    /// the handler's call goes.
    #[allow(clippy::similar_names, clippy::too_many_lines)]
    fn fast(&mut self, i: usize, call: &Call, fast: Fast, out: &State) -> Option<bool> {
        use super::super::dims;
        let repeat = self.lp.may_repeat(out);
        let (done, slow) = (self.b.create_block(), self.b.create_block());
        let cvs = self.cvs;
        match fast {
            Fast::Get { .. } | Fast::Get2 { .. } | Fast::PropGet { .. } | Fast::GetKey { .. } => {
                let (slot, ty) = call.result?;
                let float = ty == Ty::Float;
                let dest = call.jump.unwrap_or(call.after);
                let base = self.base(slot);
                let dst = self.b.ins().iadd_imm(base, i64::from(slot.at));
                let (helper, args) = match fast {
                    Fast::Get { array, index } => {
                        let array = self.b.ins().iadd_imm(cvs, i64::from(array));
                        let helper = dims::jit_get_num as GetNum as usize;
                        (helper, vec![array, self.int(index)?, dst])
                    }
                    Fast::Get2 { array, i, j } => {
                        let array = self.b.ins().iadd_imm(cvs, i64::from(array));
                        let helper = dims::jit_get_num2 as GetNum2 as usize;
                        (helper, vec![array, self.int(i)?, self.int(j)?, dst])
                    }
                    Fast::GetKey { cv, lit } => {
                        let cv = self.b.ins().iconst(types::I64, i64::from(cv));
                        let lit = self.b.ins().iconst(types::I64, i64::from(lit));
                        let helper = dims::jit_get_key as GetKey as usize;
                        (helper, vec![self.frame, cv, lit, dst])
                    }
                    Fast::PropGet { place } => {
                        let place = self.b.ins().iconst(types::I64, i64::from(place));
                        let helper = crate::exec::spec::jit_prop_get as PropGet as usize;
                        (helper, vec![self.frame, place, dst])
                    }
                    _ => return None,
                };
                let tag = self.helper(helper, &args)?;
                let expected = if float { TAG_FLOAT } else { TAG_INT };
                let hit = self
                    .b
                    .ins()
                    .icmp_imm(IntCC::Equal, tag, i64::from(expected));
                let miss = self.b.create_block();
                self.b.set_cold_block(miss);
                self.b.ins().brif(hit, done, &[], miss, &[]);
                // A number of the other type was stored: the instruction
                // is done, and the first tier goes on after it.
                self.b.switch_to_block(miss);
                let other = self.b.create_block();
                self.b.set_cold_block(other);
                self.b.ins().brif(tag, other, &[], slow, &[]);
                self.b.switch_to_block(other);
                self.ret(dest, WHY_GENERIC);
                self.b.switch_to_block(done);
                let (int, fl) = *self.vars.get(&slot)?;
                let ty = if float { types::F64 } else { types::I64 };
                let v = self.b.ins().load(ty, mem(), base, slot.at + PAYLOAD_AT);
                self.b.def_var(if float { fl } else { int }, v);
                self.go(i, dest, repeat)?;
            }
            Fast::Set { value, consume, .. }
            | Fast::Set2 { value, consume, .. }
            | Fast::Op { value, consume, .. }
            | Fast::PropSet { value, consume, .. }
            | Fast::PropOp { value, consume, .. } => {
                let (tag, bits) = if value.float {
                    let v = self.float(value)?;
                    (
                        TAG_FLOAT,
                        self.b.ins().bitcast(types::I64, MemFlags::new(), v),
                    )
                } else {
                    (TAG_INT, self.int(value)?)
                };
                let tag = self.b.ins().iconst(types::I64, i64::from(tag));
                let (helper, args) = match fast {
                    Fast::Set { array, index, .. } => {
                        let array = self.b.ins().iadd_imm(cvs, i64::from(array));
                        let helper = dims::jit_set_num as SetNum as usize;
                        (helper, vec![array, self.int(index)?, tag, bits])
                    }
                    Fast::Set2 { array, i, j, .. } => {
                        let array = self.b.ins().iadd_imm(cvs, i64::from(array));
                        let helper = dims::jit_set_num2 as SetNum2 as usize;
                        (helper, vec![array, self.int(i)?, self.int(j)?, tag, bits])
                    }
                    Fast::Op {
                        array, index, op, ..
                    } => {
                        let array = self.b.ins().iadd_imm(cvs, i64::from(array));
                        let op = self.b.ins().iconst(types::I64, i64::try_from(op).ok()?);
                        let helper = dims::jit_op_num as OpNum as usize;
                        (helper, vec![array, self.int(index)?, op, tag, bits])
                    }
                    Fast::PropSet { place, .. } => {
                        let place = self.b.ins().iconst(types::I64, i64::from(place));
                        let helper = crate::exec::spec::jit_prop_set as PropSet as usize;
                        (helper, vec![self.frame, place, tag, bits])
                    }
                    Fast::PropOp { place, op, .. } => {
                        let place = self.b.ins().iconst(types::I64, i64::from(place));
                        let op = self.b.ins().iconst(types::I64, i64::try_from(op).ok()?);
                        let helper = crate::exec::spec::jit_prop_op as PropOp as usize;
                        (helper, vec![self.frame, place, op, tag, bits])
                    }
                    _ => return None,
                };
                let stored = self.helper(helper, &args)?;
                // (A new element is put within the table's capacity:
                // nothing was allocated, so no poll is due for it.)
                self.b.ins().brif(stored, done, &[], slow, &[]);
                // Not taken: the handler, which declines what is the
                // generic op's, and the poll after it.
                self.b.set_cold_block(slow);
                self.b.switch_to_block(slow);
                #[allow(clippy::cast_possible_wrap)]
                let callee = self.b.ins().iconst(types::I64, call.handler as i64);
                let tape = self.b.ins().load(
                    types::I64,
                    mem().with_readonly(),
                    self.record,
                    rec_at(REC_TAPE)?,
                );
                let inst = self
                    .b
                    .ins()
                    .iadd_imm(tape, i64::try_from(i * std::mem::size_of::<Inst>()).ok()?);
                let made =
                    self.b
                        .ins()
                        .call_indirect(self.handler_sig, callee, &[self.frame, inst]);
                let handled = *self.b.inst_results(made).first()?;
                let deopt = self.deopt(i);
                let ok = self.b.create_block();
                self.b.set_cold_block(ok);
                self.b.ins().brif(handled, ok, &[], deopt, &[]);
                self.b.switch_to_block(ok);
                if call.polls {
                    self.poll(i, call.after)?;
                }
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
                if let Some(tmp) = consume {
                    let null = self.b.ins().iconst(types::I8, i64::from(TAG_NULL));
                    self.b.ins().store(mem(), null, self.tmps, tmp.at);
                }
                self.go(i, call.after, repeat)?;
                return Some(false);
            }
        }
        self.b.set_cold_block(slow);
        self.b.switch_to_block(slow);
        Some(true)
    }

    /// Calls the array helper at `addr` with `args`; its result.
    fn helper(&mut self, addr: usize, args: &[V]) -> Option<V> {
        #[allow(clippy::cast_possible_wrap)]
        let callee = self.b.ins().iconst(types::I64, addr as i64);
        let sig = *self.helper_sigs.get(args.len().checked_sub(3)?)?;
        let made = self.b.ins().call_indirect(sig, callee, args);
        self.b.inst_results(made).first().copied()
    }

    /// After the handler of instruction `i` did its op, on the way to
    /// instruction `to` (`None`: the one after it, in the first tier): the
    /// poll, the number it left read back into its variable (the first
    /// tier goes on if it is not of the expected type), and the jump.
    fn after_call(&mut self, i: usize, call: &Call, to: Option<usize>, repeat: bool) -> Option<()> {
        let dest = to.unwrap_or(call.after);
        if call.polls {
            self.poll(i, dest)?;
        }
        if let Some((slot, ty)) = call.result {
            self.read_back(slot, ty, dest)?;
        }
        if let Some(to) = to {
            return self.go(i, to, repeat);
        }
        self.ret(dest, WHY_GENERIC);
        Some(())
    }

    /// The truth of `value` (not zero when true), for instruction `i`.
    /// The truth of a NaN is the generic op's (`spec::plain_truth`
    /// declines it).
    fn truth(&mut self, i: usize, value: Truthy) -> Option<V> {
        Some(match value {
            Truthy::Known(known) => self.b.ins().iconst(types::I8, i64::from(known)),
            Truthy::Bool(slot) => {
                let (var, _) = *self.vars.get(&slot)?;
                let v = self.b.use_var(var);
                self.b.ins().icmp_imm(IntCC::NotEqual, v, 0)
            }
            Truthy::Num(o) if o.float => {
                let x = self.float(o)?;
                let nan = self.b.ins().fcmp(FloatCC::Unordered, x, x);
                let deopt = self.deopt(i);
                let ok = self.b.create_block();
                self.b.ins().brif(nan, deopt, &[], ok, &[]);
                self.b.switch_to_block(ok);
                let zero = self.b.ins().f64const(0.0);
                self.b.ins().fcmp(FloatCC::NotEqual, x, zero)
            }
            Truthy::Num(o) => {
                let x = self.int(o)?;
                self.b.ins().icmp_imm(IntCC::NotEqual, x, 0)
            }
        })
    }

    /// Writes the bool `c` (0 or 1, as a comparison leaves it) to `dst`:
    /// its int variable, and its slot.
    fn write_bool(&mut self, st: &State, dst: SlotRef, c: V) -> Option<()> {
        let (var, _) = *self.vars.get(&dst)?;
        let wide = self.b.ins().uextend(types::I64, c);
        self.b.def_var(var, wide);
        if dst.at < 0 {
            return Some(());
        }
        let base = self.base(dst);
        if self.lp.ty(st, dst) != Some(Ty::Bool) {
            let tag = self.b.ins().iconst(types::I8, i64::from(TAG_BOOL));
            self.b.ins().store(mem(), tag, base, dst.at);
        }
        self.b.ins().store(mem(), c, base, dst.at + BOOL_AT);
        Some(())
    }

    /// An int operand.
    fn int(&mut self, o: Opnd) -> Option<V> {
        Some(match o.num {
            Num::Slot(slot) => {
                let (int, _) = *self.vars.get(&slot)?;
                self.b.use_var(int)
            }
            Num::Int(i) => self.b.ins().iconst(types::I64, i),
            Num::Float(_) => return None,
        })
    }

    /// An operand as a float: an int is widened (php-src's `(double)`).
    fn float(&mut self, o: Opnd) -> Option<V> {
        Some(match o.num {
            Num::Slot(slot) => {
                let (int, float) = *self.vars.get(&slot)?;
                if o.float {
                    self.b.use_var(float)
                } else {
                    let v = self.b.use_var(int);
                    self.b.ins().fcvt_from_sint(types::F64, v)
                }
            }
            #[allow(clippy::cast_precision_loss)]
            Num::Int(i) => self.b.ins().f64const(i as f64),
            Num::Float(f) => self.b.ins().f64const(f),
        })
    }

    /// Writes a number to `dst`: its variable, and its slot (the tag only
    /// when it is not known to be there already).
    fn write(&mut self, st: &State, dst: SlotRef, float: bool, v: V) -> Option<()> {
        let (int, fl) = *self.vars.get(&dst)?;
        let (var, ty, tag) = if float {
            (fl, Ty::Float, TAG_FLOAT)
        } else {
            (int, Ty::Int, TAG_INT)
        };
        self.b.def_var(var, v);
        if dst.at < 0 {
            // An argument variable: not a slot of the frame.
            return Some(());
        }
        let base = self.base(dst);
        if self.lp.ty(st, dst) != Some(ty) {
            let t = self.b.ins().iconst(types::I8, i64::from(tag));
            self.b.ins().store(mem(), t, base, dst.at);
        }
        self.b.ins().store(mem(), v, base, dst.at + PAYLOAD_AT);
        Some(())
    }

    /// `x op y` on ints; an overflow leaves for the first tier, having
    /// changed nothing.
    /// `x + delta` on ints for a `delta` known now, as [`Self::int_arith`]:
    /// the sum overflowed exactly when it went the wrong way, which is a
    /// compare and a branch (the overflow flag of an add costs two
    /// instructions more to branch on, on every round of a counting loop).
    fn int_step(&mut self, i: usize, x: V, delta: i64) -> V {
        if delta == 0 {
            return x;
        }
        let r = self.b.ins().iadd_imm(x, delta);
        let wrong = if delta > 0 {
            IntCC::SignedLessThan
        } else {
            IntCC::SignedGreaterThan
        };
        let overflow = self.b.ins().icmp(wrong, r, x);
        let deopt = self.deopt(i);
        let next = self.b.create_block();
        self.b.ins().brif(overflow, deopt, &[], next, &[]);
        self.b.switch_to_block(next);
        r
    }

    /// `a op b` on two int operands, by [`Self::int_step`] when one of
    /// them is known now and the operator is `+` or `-`.
    #[allow(clippy::many_single_char_names)] // the operands and their values
    fn int_binary(&mut self, i: usize, op: Arith, a: Opnd, b: Opnd) -> Option<V> {
        match (op, a.num, b.num) {
            (Arith::Add, _, Num::Int(c)) => {
                let x = self.int(a)?;
                Some(self.int_step(i, x, c))
            }
            (Arith::Add, Num::Int(c), _) => {
                let y = self.int(b)?;
                Some(self.int_step(i, y, c))
            }
            (Arith::Sub, _, Num::Int(c)) if c != i64::MIN => {
                let x = self.int(a)?;
                Some(self.int_step(i, x, -c))
            }
            _ => {
                let (x, y) = (self.int(a)?, self.int(b)?);
                Some(self.int_arith(i, op, x, y))
            }
        }
    }

    fn int_arith(&mut self, i: usize, op: Arith, x: V, y: V) -> V {
        let (r, overflow) = match op {
            Arith::Add => self.b.ins().sadd_overflow(x, y),
            Arith::Sub => self.b.ins().ssub_overflow(x, y),
            Arith::Mul => self.b.ins().smul_overflow(x, y),
        };
        let deopt = self.deopt(i);
        let next = self.b.create_block();
        self.b.ins().brif(overflow, deopt, &[], next, &[]);
        self.b.switch_to_block(next);
        r
    }

    /// Emits instruction `i` into the current block (its own), with the
    /// types `st` before it.
    #[allow(clippy::too_many_lines, clippy::many_single_char_names)]
    fn inst(&mut self, i: usize, plan: &Plan, st: &State) -> Option<()> {
        let mut out = st.clone();
        plan.apply(&mut out);
        let repeat = self.lp.may_repeat(&out);
        match *plan {
            Plan::Jmp { to } => self.go(i, to, repeat)?,
            Plan::Bind { args, argc, to, .. } => {
                for k in 0..argc {
                    let arg = arg_slot(args + k);
                    let float = match self.lp.ty(st, arg)? {
                        Ty::Int => false,
                        Ty::Float => true,
                        Ty::Bool | Ty::Plain => return None,
                    };
                    let (int, fl) = *self.vars.get(&arg)?;
                    let v = self.b.use_var(if float { fl } else { int });
                    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                    let param = virtual_slot(SlotRef {
                        tmp: false,
                        at: (k * SLOT) as i32,
                    });
                    self.write(st, param, float, v)?;
                }
                self.go(i, to, repeat)?;
            }
            Plan::Cmp {
                a,
                b,
                test,
                taken,
                fall,
            } => {
                let (taken, fall) = (self.land(i, taken, &out), self.land(i, fall, &out));
                let (c, negate) = match test {
                    Test::Always(always) => {
                        self.b.ins().jump(if always { taken } else { fall }, &[]);
                        return Some(());
                    }
                    Test::Int(cc) => {
                        let (x, y) = (self.int(a)?, self.int(b)?);
                        (self.b.ins().icmp(cc, x, y), false)
                    }
                    Test::Float(test) => {
                        let (x, y) = (self.float(a)?, self.float(b)?);
                        match test {
                            FloatTest::Always(_) => return None,
                            FloatTest::Cc { cc, negate } => (self.b.ins().fcmp(cc, x, y), negate),
                            FloatTest::Apart { negate } => {
                                let lt = self.b.ins().fcmp(FloatCC::LessThan, x, y);
                                let gt = self.b.ins().fcmp(FloatCC::GreaterThan, x, y);
                                (self.b.ins().bor(lt, gt), negate)
                            }
                        }
                    }
                };
                if negate {
                    self.b.ins().brif(c, fall, &[], taken, &[]);
                } else {
                    self.b.ins().brif(c, taken, &[], fall, &[]);
                }
            }
            Plan::Int2 {
                op,
                a,
                b,
                dst,
                next,
            } => {
                let (x, y) = (self.int(a)?, self.int(b)?);
                // A right operand known now was checked by the analysis;
                // any other is tested for what the fast path declines.
                let known = matches!(b.num, Num::Int(_));
                let refused = match op {
                    _ if known => None,
                    IntOp::Div | IntOp::Mod => {
                        let zero = self.b.ins().icmp_imm(IntCC::Equal, y, 0);
                        let minus = self.b.ins().icmp_imm(IntCC::Equal, y, -1);
                        Some(self.b.ins().bor(zero, minus))
                    }
                    IntOp::Shl | IntOp::Shr => Some(self.b.ins().icmp_imm(
                        IntCC::UnsignedGreaterThanOrEqual,
                        y,
                        64,
                    )),
                    IntOp::Or | IntOp::And | IntOp::Xor => None,
                };
                if let Some(refused) = refused {
                    let deopt = self.deopt(i);
                    let ok = self.b.create_block();
                    self.b.ins().brif(refused, deopt, &[], ok, &[]);
                    self.b.switch_to_block(ok);
                }
                let r = match op {
                    IntOp::Div => self.b.ins().sdiv(x, y),
                    IntOp::Mod => self.b.ins().srem(x, y),
                    IntOp::Shl => self.b.ins().ishl(x, y),
                    IntOp::Shr => self.b.ins().sshr(x, y),
                    IntOp::Or => self.b.ins().bor(x, y),
                    IntOp::And => self.b.ins().band(x, y),
                    IntOp::Xor => self.b.ins().bxor(x, y),
                };
                self.write(st, dst, false, r)?;
                self.go(i, next, repeat)?;
            }
            Plan::Div { a, b, dst, next } => {
                let (x, y) = (self.float(a)?, self.float(b)?);
                // A zero divisor (also -0.0) throws: the generic op's.
                let zero = self.b.ins().f64const(0.0);
                let by_zero = self.b.ins().fcmp(FloatCC::Equal, y, zero);
                let deopt = self.deopt(i);
                let ok = self.b.create_block();
                self.b.ins().brif(by_zero, deopt, &[], ok, &[]);
                self.b.switch_to_block(ok);
                let r = self.b.ins().fdiv(x, y);
                self.write(st, dst, true, r)?;
                self.go(i, next, repeat)?;
            }
            Plan::Call(call) => self.call(i, &call, &out)?,
            Plan::Leave => self.ret(i, WHY_GENERIC),
            Plan::Compare {
                a,
                b,
                test,
                dst,
                next,
            } => {
                let c = match test {
                    Test::Always(known) => self.b.ins().iconst(types::I8, i64::from(known)),
                    Test::Int(cc) => {
                        let (x, y) = (self.int(a)?, self.int(b)?);
                        self.b.ins().icmp(cc, x, y)
                    }
                    Test::Float(FloatTest::Cc { cc, .. }) => {
                        let (x, y) = (self.float(a)?, self.float(b)?);
                        self.b.ins().fcmp(cc, x, y)
                    }
                    Test::Float(_) => return None,
                };
                self.write_bool(st, dst, c)?;
                self.go(i, next, repeat)?;
            }
            Plan::SetBool {
                value,
                negate,
                consume,
                dst,
                next,
            } => {
                let truth = self.truth(i, value)?;
                let c = if negate {
                    self.b.ins().icmp_imm(IntCC::Equal, truth, 0)
                } else {
                    truth
                };
                if let Some(tmp) = consume {
                    let null = self.b.ins().iconst(types::I8, i64::from(TAG_NULL));
                    self.b.ins().store(mem(), null, self.tmps, tmp.at);
                }
                self.write_bool(st, dst, c)?;
                self.go(i, next, repeat)?;
            }
            Plan::JmpEx {
                value,
                when,
                consume,
                dst,
                target,
                fall,
            } => {
                let truth = self.truth(i, value)?;
                if let Some(tmp) = consume {
                    let null = self.b.ins().iconst(types::I8, i64::from(TAG_NULL));
                    self.b.ins().store(mem(), null, self.tmps, tmp.at);
                }
                let taken = self.b.create_block();
                let fall = self.land(i, fall, &plan.out(st, fall));
                if when {
                    self.b.ins().brif(truth, taken, &[], fall, &[]);
                } else {
                    self.b.ins().brif(truth, fall, &[], taken, &[]);
                }
                self.b.switch_to_block(taken);
                let known = self.b.ins().iconst(types::I8, i64::from(when));
                self.write_bool(st, dst, known)?;
                let after = plan.out(st, target);
                self.go(i, target, self.lp.may_repeat(&after))?;
            }
            Plan::Assign2 {
                value,
                consume,
                cv,
                tmp,
                next,
            } => {
                let v = if value.float {
                    self.float(value)?
                } else {
                    self.int(value)?
                };
                self.write(st, cv, value.float, v)?;
                if let Some(consumed) = consume {
                    let null = self.b.ins().iconst(types::I8, i64::from(TAG_NULL));
                    self.b.ins().store(mem(), null, self.tmps, consumed.at);
                }
                self.write(st, tmp, value.float, v)?;
                self.go(i, next, repeat)?;
            }
            Plan::Conv {
                to_float,
                a,
                consume,
                dst,
                next,
            } => {
                let v = if to_float {
                    self.float(a)?
                } else if a.float {
                    // `zend_dval_to_lval` of a float in the int range
                    // truncates; any other (a NaN too) is the generic
                    // op's.
                    let x = self.float(a)?;
                    let lo = self.b.ins().f64const(-9_223_372_036_854_775_808.0);
                    let hi = self.b.ins().f64const(9_223_372_036_854_775_808.0);
                    let above = self.b.ins().fcmp(FloatCC::GreaterThanOrEqual, x, lo);
                    let below = self.b.ins().fcmp(FloatCC::LessThan, x, hi);
                    let fits = self.b.ins().band(above, below);
                    let deopt = self.deopt(i);
                    let ok = self.b.create_block();
                    self.b.ins().brif(fits, ok, &[], deopt, &[]);
                    self.b.switch_to_block(ok);
                    self.b.ins().fcvt_to_sint_sat(types::I64, x)
                } else {
                    self.int(a)?
                };
                self.write(st, dst, to_float, v)?;
                if let Some(tmp) = consume {
                    let null = self.b.ins().iconst(types::I8, i64::from(TAG_NULL));
                    self.b.ins().store(mem(), null, self.tmps, tmp.at);
                }
                self.go(i, next, repeat)?;
            }
            Plan::Math { f, a, dst, next } => {
                let x = self.float(a)?;
                let r = match f {
                    Math::Abs => self.b.ins().fabs(x),
                    Math::Sqrt => self.b.ins().sqrt(x),
                    Math::Floor => self.b.ins().floor(x),
                    Math::Ceil => self.b.ins().ceil(x),
                    Math::Lib(addr) => {
                        #[allow(clippy::cast_possible_wrap)]
                        let callee = self.b.ins().iconst(types::I64, addr as i64);
                        let made = self.b.ins().call_indirect(self.math_sig, callee, &[x]);
                        *self.b.inst_results(made).first()?
                    }
                };
                self.write(st, dst, true, r)?;
                self.go(i, next, repeat)?;
            }
            Plan::AbsInt { a, dst, next } => {
                let x = self.int(a)?;
                // `-PHP_INT_MIN` is a float: the generic op's.
                let least = self.b.ins().icmp_imm(IntCC::Equal, x, i64::MIN);
                let deopt = self.deopt(i);
                let ok = self.b.create_block();
                self.b.ins().brif(least, deopt, &[], ok, &[]);
                self.b.switch_to_block(ok);
                let negated = self.b.ins().ineg(x);
                let negative = self.b.ins().icmp_imm(IntCC::SignedLessThan, x, 0);
                let r = self.b.ins().select(negative, negated, x);
                self.write(st, dst, false, r)?;
                self.go(i, next, repeat)?;
            }
            Plan::Arg {
                k,
                value,
                consume,
                next,
            } => {
                let v = if value.float {
                    self.float(value)?
                } else {
                    self.int(value)?
                };
                self.write(st, arg_slot(k), value.float, v)?;
                if let Some(tmp) = consume {
                    let null = self.b.ins().iconst(types::I8, i64::from(TAG_NULL));
                    self.b.ins().store(mem(), null, self.tmps, tmp.at);
                }
                self.go(i, next, repeat)?;
            }
            Plan::Pick {
                max,
                a,
                b,
                dst,
                next,
            } => {
                let (x, y) = (self.int(a)?, self.int(b)?);
                let cc = if max {
                    IntCC::SignedGreaterThan
                } else {
                    IntCC::SignedLessThan
                };
                let first = self.b.ins().icmp(cc, x, y);
                let r = self.b.ins().select(first, x, y);
                self.write(st, dst, false, r)?;
                self.go(i, next, repeat)?;
            }
            Plan::Each {
                slim,
                iter,
                end,
                full,
                value,
                key,
                ..
            } => {
                // The short cut: the next number straight into the loop's
                // slots (`super::each_num`).
                if slim {
                    let base = self.base(value.0);
                    let value_at = self.b.ins().iadd_imm(base, i64::from(value.0.at));
                    let key_at = match key {
                        Some(key) => {
                            let base = self.base(key);
                            self.b.ins().iadd_imm(base, i64::from(key.at))
                        }
                        None => self.b.ins().iconst(types::I64, 0),
                    };
                    let iter = self.b.ins().iconst(types::I64, i64::from(iter));
                    let r = self.helper(
                        super::each_num as EachNum as usize,
                        &[self.frame, iter, value_at, key_at],
                    )?;
                    let [fetched, miss, not_end, other, slow] =
                        std::array::from_fn(|_| self.b.create_block());
                    for cold in [other, slow] {
                        self.b.set_cold_block(cold);
                    }
                    let expected = if value.1 { TAG_FLOAT } else { TAG_INT };
                    let hit = self.b.ins().icmp_imm(IntCC::Equal, r, i64::from(expected));
                    self.b.ins().brif(hit, fetched, &[], miss, &[]);
                    self.b.switch_to_block(miss);
                    let over = self.land(i, end, st);
                    let ended = self.b.ins().icmp_imm(IntCC::Equal, r, 1);
                    self.b.ins().brif(ended, over, &[], not_end, &[]);
                    self.b.switch_to_block(not_end);
                    self.b.ins().brif(r, other, &[], slow, &[]);
                    // A number of the other type was stored: done, and the
                    // first tier's from the next instruction.
                    self.b.switch_to_block(other);
                    self.ret(full, WHY_GENERIC);
                    self.b.switch_to_block(fetched);
                    let (int, fl) = *self.vars.get(&value.0)?;
                    let ty = if value.1 { types::F64 } else { types::I64 };
                    let v = self.b.ins().load(ty, mem(), base, value.0.at + PAYLOAD_AT);
                    self.b.def_var(if value.1 { fl } else { int }, v);
                    if let Some(key) = key {
                        let base = self.base(key);
                        let (int, _) = *self.vars.get(&key)?;
                        let v = self
                            .b
                            .ins()
                            .load(types::I64, mem(), base, key.at + PAYLOAD_AT);
                        self.b.def_var(int, v);
                    }
                    self.go(i, full, repeat)?;
                    self.b.switch_to_block(slow);
                    self.ret(i, WHY_MISS);
                    return Some(());
                }
                #[allow(clippy::cast_possible_wrap)]
                let callee = self
                    .b
                    .ins()
                    .iconst(types::I64, super::fe_fetch as super::Step as usize as i64);
                let at = self.b.ins().iconst(types::I64, i64::try_from(i).ok()?);
                let made = self
                    .b
                    .ins()
                    .call_indirect(self.handler_sig, callee, &[self.frame, at]);
                let next = *self.b.inst_results(made).first()?;
                // Not an array iterated by value (`hot::DECLINED`, all
                // ones): nothing changed, the first tier's.
                let deopt = self.deopt(i);
                let [done, polled, go, fetched, rest, other] =
                    std::array::from_fn(|_| self.b.create_block());
                for cold in [polled, other] {
                    self.b.set_cold_block(cold);
                }
                let refused = self.b.ins().icmp_imm(IntCC::Equal, next, -1);
                self.b.ins().brif(refused, deopt, &[], done, &[]);
                // The poll `run_tape` makes after the op.
                self.b.switch_to_block(done);
                let flag = self.flag;
                let due = self
                    .b
                    .ins()
                    .load(types::I8, MemFlags::new().with_notrap(), flag, 0);
                self.b.ins().brif(due, polled, &[], go, &[]);
                self.b.switch_to_block(polled);
                self.b.ins().store(mem(), at, self.record, rec_at(REC_AT)?);
                self.ret_at(next, super::WHY_POLL);
                self.b.switch_to_block(go);
                let whole = self
                    .b
                    .ins()
                    .icmp_imm(IntCC::Equal, next, i64::try_from(full).ok()?);
                self.b.ins().brif(whole, fetched, &[], rest, &[]);
                // The end of the iteration, or an assignment that was not
                // made here (the first tier goes on at it).
                self.b.switch_to_block(rest);
                let over = self.land(i, end, st);
                let ended = self
                    .b
                    .ins()
                    .icmp_imm(IntCC::Equal, next, i64::try_from(end).ok()?);
                self.b.ins().brif(ended, over, &[], other, &[]);
                self.b.switch_to_block(other);
                self.ret_at(next, WHY_GENERIC);
                // The element, read back behind a test of its type.
                self.b.switch_to_block(fetched);
                self.read_back(value.0, num_ty(value.1), full)?;
                if let Some(key) = key {
                    self.read_back(key, Ty::Int, full)?;
                }
                self.go(i, full, repeat)?;
            }
            Plan::Ext {
                result,
                after,
                next,
                ..
            } => {
                #[allow(clippy::cast_possible_wrap)]
                let callee = self
                    .b
                    .ins()
                    .iconst(types::I64, super::fe_fetch as super::Step as usize as i64);
                let at = self.b.ins().iconst(types::I64, i64::try_from(i).ok()?);
                let made = self
                    .b
                    .ins()
                    .call_indirect(self.handler_sig, callee, &[self.frame, at]);
                let got = *self.b.inst_results(made).first()?;
                // Declined (`hot::DECLINED`, all ones): nothing changed,
                // the interpreter's op; counted, so that a loop whose
                // property stops being one for the fast path is given up
                // (`super::missed`).
                let [declined, done, as_planned, other] =
                    std::array::from_fn(|_| self.b.create_block());
                for cold in [declined, other] {
                    self.b.set_cold_block(cold);
                }
                let refused = self.b.ins().icmp_imm(IntCC::Equal, got, -1);
                self.b.ins().brif(refused, declined, &[], done, &[]);
                self.b.switch_to_block(declined);
                self.ret(i, WHY_MISS);
                self.b.switch_to_block(done);
                let whole = self
                    .b
                    .ins()
                    .icmp_imm(IntCC::Equal, got, i64::try_from(next).ok()?);
                self.b.ins().brif(whole, as_planned, &[], other, &[]);
                // Done, but not into the variable: the first tier's from
                // the `AssignCv`.
                self.b.switch_to_block(other);
                self.ret(after, WHY_GENERIC);
                self.b.switch_to_block(as_planned);
                // The poll `run_tape` makes after the op.
                self.poll(i, next)?;
                if let Some((slot, ty)) = result {
                    self.read_back(slot, ty, next)?;
                }
                self.go(i, next, repeat)?;
            }
            Plan::Truth {
                value,
                when,
                consume,
                taken,
                fall,
            } => {
                let (taken, fall) = (self.land(i, taken, &out), self.land(i, fall, &out));
                let truth = self.truth(i, value)?;
                if let Some(tmp) = consume {
                    let null = self.b.ins().iconst(types::I8, i64::from(TAG_NULL));
                    self.b.ins().store(mem(), null, self.tmps, tmp.at);
                }
                if when {
                    self.b.ins().brif(truth, taken, &[], fall, &[]);
                } else {
                    self.b.ins().brif(truth, fall, &[], taken, &[]);
                }
            }
            Plan::IncCmp {
                slot,
                inc,
                bound,
                cc,
                taken,
                fall,
            } => {
                // The bound first: it is read before the variable changes.
                let y = self.int(bound)?;
                let (var, _) = *self.vars.get(&slot)?;
                let x = self.b.use_var(var);
                let n = self.int_step(i, x, if inc { 1 } else { -1 });
                self.write(st, slot, false, n)?;
                let (taken, fall) = (self.land(i, taken, &out), self.land(i, fall, &out));
                let c = self.b.ins().icmp(cc, n, y);
                self.b.ins().brif(c, taken, &[], fall, &[]);
            }
            Plan::Arith {
                op,
                a,
                b,
                dst,
                next,
            } => {
                if a.float || b.float {
                    let (x, y) = (self.float(a)?, self.float(b)?);
                    let r = match op {
                        Arith::Add => self.b.ins().fadd(x, y),
                        Arith::Sub => self.b.ins().fsub(x, y),
                        Arith::Mul => self.b.ins().fmul(x, y),
                    };
                    self.write(st, dst, true, r)?;
                } else {
                    let r = self.int_binary(i, op, a, b)?;
                    self.write(st, dst, false, r)?;
                }
                self.go(i, next, repeat)?;
            }
            Plan::Step {
                slot,
                inc,
                float,
                copy,
                next,
            } => {
                let (int, fl) = *self.vars.get(&slot)?;
                let (old, new) = if float {
                    let old = self.b.use_var(fl);
                    let one = self.b.ins().f64const(1.0);
                    let new = if inc {
                        self.b.ins().fadd(old, one)
                    } else {
                        self.b.ins().fsub(old, one)
                    };
                    (old, new)
                } else {
                    let old = self.b.use_var(int);
                    (old, self.int_step(i, old, if inc { 1 } else { -1 }))
                };
                self.write(st, slot, float, new)?;
                if let Some((dst, post)) = copy {
                    self.write(st, dst, float, if post { old } else { new })?;
                }
                self.go(i, next, repeat)?;
            }
            Plan::Const { dst, value, next } => {
                let v = if value.float {
                    self.float(value)?
                } else {
                    self.int(value)?
                };
                self.write(st, dst, value.float, v)?;
                self.go(i, next, repeat)?;
            }
            Plan::Copy {
                src,
                dst,
                float,
                consume,
                next,
            } => {
                let (int, fl) = *self.vars.get(&src)?;
                let v = self.b.use_var(if float { fl } else { int });
                self.write(st, dst, float, v)?;
                if consume {
                    let null = self.b.ins().iconst(types::I8, i64::from(TAG_NULL));
                    let base = self.base(src);
                    self.b.ins().store(mem(), null, base, src.at);
                }
                self.go(i, next, repeat)?;
            }
        }
        Some(())
    }
}

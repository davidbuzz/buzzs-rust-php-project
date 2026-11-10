//! Zend/Optimizer passes (`crates/php-vm/src/compiler/optimizer/`).
//!
//! Implements observable, semantics-preserving optimization passes gated by
//! `opcache.optimization_level` bits matching php-src (`Zend/Optimizer/zend_optimizer.h`):
//! - Pass 1 (`1 << 0`): Local constant folding, constant branch folding, echo folding
//! - Pass 2 (`1 << 1`): Constant conversion & typecheck strength reduction
//! - Pass 3 (`1 << 2`): Jump optimization & jump-to-jump threading
//! - Pass 5 (`1 << 4`): CFG basic block pass, temp elision, smart branch negation, echo merging
//! - Pass 6 & 8 (`(1 << 5) | (1 << 7)`): DFA & SCCP constant propagation
//! - Pass 9 (`1 << 8`): Temporary variable compaction
//! - Pass 10 (`1 << 9`): NOP removal and label/try-table remapping
//! - Pass 11 (`1 << 10`): Literal table compaction & deduplication
//! - Pass 14 (`1 << 13`): Dead Code Elimination (DCE) & unreachable code removal

pub mod block_pass;
pub mod compact;
pub mod dce;
pub mod dump;
pub mod pass1;
pub mod pass3;
pub mod sccp;

pub use dump::dump_op_array;

use crate::compiler::specialise;
use crate::compiler::CompiledUnit;
use crate::opcode::{ArgSrc, ClassSpec, Label, Op, OpArray, Operand, Place, PlaceBase, Seg};
use php_runtime::Value;
use std::rc::Rc;

// -----------------------------------------------------------------------------
// Bitmask constants from Zend/Optimizer/zend_optimizer.h
// -----------------------------------------------------------------------------

/// Pass 1: Simple local optimizations (constant folding, constant branch folding).
pub const PASS_1: u32 = 1 << 0;
/// Pass 2: Constant conversion and strength reduction.
pub const PASS_2: u32 = 1 << 1;
/// Pass 3: Jump optimization.
pub const PASS_3: u32 = 1 << 2;
/// Pass 4: Function call optimization.
pub const PASS_4: u32 = 1 << 3;
/// Pass 5: CFG based basic block optimization.
pub const PASS_5: u32 = 1 << 4;
/// Pass 6: DFA based optimization.
pub const PASS_6: u32 = 1 << 5;
/// Pass 7: Call graph optimization.
pub const PASS_7: u32 = 1 << 6;
/// Pass 8: SCCP (Sparse Conditional Constant Propagation).
pub const PASS_8: u32 = 1 << 7;
/// Pass 9: Temporary variable compaction.
pub const PASS_9: u32 = 1 << 8;
/// Pass 10: NOP removal.
pub const PASS_10: u32 = 1 << 9;
/// Pass 11: Literal table compaction.
pub const PASS_11: u32 = 1 << 10;
/// Pass 12: Adjust used stack.
pub const PASS_12: u32 = 1 << 11;
/// Pass 13: Remove unused variables.
pub const PASS_13: u32 = 1 << 12;
/// Pass 14: Dead Code Elimination (DCE).
pub const PASS_14: u32 = 1 << 13;
/// Pass 15: Collect constants (unsafe, disabled by default).
pub const PASS_15: u32 = 1 << 14;
/// Pass 16: Inline functions.
pub const PASS_16: u32 = 1 << 15;

/// Default `opcache.optimization_level` (matches php-src's `DEFAULT_OPTIMIZATION_LEVEL` `0x7FFEBFFF`).
pub const DEFAULT_OPTIMIZATION_LEVEL: u32 = 0x7FFE_BFFF;

thread_local! {
    /// Lowercase names removed by `disable_functions`: they are no longer
    /// in the function table, so `zend_optimizer_eval_special_func_call`
    /// and friends (which look the function up first) never evaluate a
    /// call to one at compile time (GH-17733).
    static DISABLED_FUNCTIONS: std::cell::RefCell<std::collections::HashSet<Vec<u8>>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// Records the functions `disable_functions` removed (lowercase names),
/// replacing any earlier list.
pub fn set_disabled_functions(names: impl IntoIterator<Item = Vec<u8>>) {
    DISABLED_FUNCTIONS.with(|d| {
        let mut d = d.borrow_mut();
        d.clear();
        d.extend(names);
    });
}

/// Whether the internal function `lcname` was removed by `disable_functions`.
pub(crate) fn is_disabled_function(lcname: &[u8]) -> bool {
    DISABLED_FUNCTIONS.with(|d| {
        let d = d.borrow();
        !d.is_empty() && d.contains(lcname)
    })
}

/// What the passes must know about an op array beyond its instructions.
#[derive(Clone, Copy, Debug, Default)]
pub struct OptContext<'a> {
    /// Top-level code: its CVs are the global symbol table, which other
    /// functions reach through `global`/`$GLOBALS` (as references) and
    /// included files share, so php-src types them all `MAY_BE_REF` and
    /// SCCP never propagates them (`zend_ssa_inference` for a pseudo-main).
    pub is_main: bool,
    /// The CVs bound before the first op: parameters, then the variables a
    /// closure's `use` clause binds, with whether each is bound by
    /// reference. They keep their slots (the call and the closure binding
    /// address them by number, pass 12) and their assignments (pass 13),
    /// and a by-reference one may change whenever user code runs or coerce
    /// what is assigned to it (a typed reference).
    pub bound_cvs: &'a [(u32, bool)],
}

impl OptContext<'_> {
    /// One past the highest bound CV (0 when none): the prefix of CV slots
    /// that the call and the closure binding address by number.
    pub(crate) fn bound_cv_prefix(&self) -> u32 {
        self.bound_cvs
            .iter()
            .map(|&(cv, _)| cv.saturating_add(1))
            .max()
            .unwrap_or(0)
    }

    /// Whether any CV is bound by reference.
    pub(crate) fn has_ref_binding(&self) -> bool {
        self.bound_cvs.iter().any(|&(_, by_ref)| by_ref)
    }
}

/// Optimizes `op_array` in place according to `level` bitmask (a function
/// body with nothing bound by reference that is not top-level code).
pub fn optimize_op_array(op_array: &mut OpArray, level: u32) {
    optimize_op_array_with(op_array, level, &OptContext::default());
}

/// Optimizes `op_array` in place according to `level` bitmask and what
/// `ctx` says about the op array.
pub fn optimize_op_array_with(op_array: &mut OpArray, level: u32, ctx: &OptContext<'_>) {
    if level == 0 || op_array.ops.is_empty() {
        return;
    }

    // Iterative optimization loop: constant folding, jump threading, SCCP,
    // block-pass, and dead code elimination feed each other until a fixed
    // point or iteration limit is reached.
    for _ in 0..8 {
        let mut changed = false;

        if (level & PASS_1) != 0 {
            changed |= pass1::run_pass1(op_array, level);
        }
        if (level & PASS_2) != 0 {
            changed |= pass1::run_pass2(op_array);
        }
        if (level & PASS_4) != 0 {
            changed |= pass1::run_pass4(op_array);
        }
        if (level & PASS_3) != 0 {
            changed |= pass3::run_pass3(op_array);
        }
        if (level & (PASS_6 | PASS_8)) != 0 {
            changed |= sccp::run_sccp(op_array, ctx);
        }
        if (level & PASS_5) != 0 {
            changed |= block_pass::run_block_pass(op_array);
        }
        if (level & PASS_14) != 0 {
            changed |= dce::run_dce(op_array);
            if (level & PASS_6) != 0 && !ctx.is_main {
                changed |= block_pass::remove_unused_const_statics(op_array, ctx.bound_cv_prefix());
            }
        }
        if (level & PASS_13) != 0 {
            changed |= compact::remove_unused_variables(op_array, ctx);
        }

        if !changed {
            break;
        }
    }

    // Pass 13: Remove unused variables
    if (level & PASS_13) != 0 {
        compact::remove_unused_variables(op_array, ctx);
    }

    // Pass 12: Adjust used stack / compact CV slots
    if (level & PASS_12) != 0 {
        compact::compact_cvs(op_array, ctx);
    }

    // Pass 10: NOP removal (remaps all jump targets, try regions, and live ranges)
    if (level & PASS_10) != 0 {
        compact::remove_nops(op_array);
    }

    // Pass 9: Compact temporaries
    if (level & PASS_9) != 0 {
        compact::compact_tmps(op_array);
    }

    // Pass 11: Compact literals
    if (level & PASS_11) != 0 {
        compact::compact_literals(op_array);
    }

    // Re-run handler specialisation over the optimized instructions
    specialise::specialise(&mut op_array.ops, &op_array.places, &op_array.literals);
}

/// Optimizes a compiled unit in place (main op array).
pub fn optimize_unit(unit: &mut CompiledUnit, level: u32) {
    if level == 0 {
        return;
    }
    if let Some(user_fn) = Rc::get_mut(&mut unit.main) {
        if let Some(code) = Rc::get_mut(&mut user_fn.code) {
            let ctx = OptContext {
                is_main: true,
                bound_cvs: &[],
            };
            optimize_op_array_with(code, level & !PASS_13, &ctx);
        }
    }
}

// -----------------------------------------------------------------------------
// Common Traversal & Utility Helpers
// -----------------------------------------------------------------------------

pub(crate) fn for_each_operand_mut(op: &mut Op, mut f: impl FnMut(&mut Operand)) {
    match op {
        Op::Nop => {}
        Op::Move { src, .. } => f(src),
        Op::Bin { a, b, .. } => {
            f(a);
            f(b);
        }
        Op::Unary { a, .. } => f(a),
        Op::Cast { a, .. } => f(a),
        Op::Rope { parts, .. } => {
            for p in parts.iter_mut() {
                f(p);
            }
        }
        Op::InitArray { .. } => {}
        Op::ArrayAdd { key, val, .. } => {
            if let Some(k) = key {
                f(k);
            }
            f(val);
        }
        Op::ArrayAddRef { key, .. } => {
            if let Some(k) = key {
                f(k);
            }
        }
        Op::ArrayUnpack { src, .. } => f(src),
        Op::ConstArrayUnpack { src, .. } => f(src),
        Op::Jmp(_) => {}
        Op::JmpZ(cond, _) | Op::JmpNz(cond, _) => f(cond),
        Op::JmpZEx { a, .. }
        | Op::JmpNzEx { a, .. }
        | Op::JmpSet { a, .. }
        | Op::Coalesce { a, .. }
        | Op::JmpNull { a, .. } => f(a),
        Op::AssignCv { val, .. } => f(val),
        Op::Assign { val, .. } => f(val),
        Op::AssignOp { val, .. } => f(val),
        Op::IncDec { .. } => {}
        Op::AssignRef { src, .. } => f(src),
        Op::MakeRef { .. } => {}
        Op::ListFetchRef { .. } => {}
        Op::Fetch { .. } | Op::Isset { .. } | Op::Empty { .. } => {}
        Op::ArrayKeyExists { array, .. } => match array {
            ArgSrc::Val(op) | ArgSrc::CallResult(op) => f(op),
            ArgSrc::Place(_) | ArgSrc::TempPlace(_) => {}
        },
        Op::Unset { .. } => {}
        Op::Global { .. } => {}
        Op::BindStatic { .. } => {}
        Op::InitStatic { val, .. } => f(val),
        Op::FetchThis { .. } => {}
        Op::InitFCall { .. } => {}
        Op::InitDynCall { callee } => f(callee),
        Op::InitMethodCall { obj, name } => {
            f(obj);
            f(name);
        }
        Op::InitStaticCall { class, name } => {
            if let ClassSpec::Dyn(c) = class {
                f(c);
            }
            f(name);
        }
        Op::InitParentHook { .. } => {}
        Op::New { class, .. } => {
            if let ClassSpec::Dyn(c) = class {
                f(c);
            }
        }
        Op::Send { src } | Op::SendNamed { src, .. } => match src {
            ArgSrc::Val(op) | ArgSrc::CallResult(op) => f(op),
            ArgSrc::Place(_) | ArgSrc::TempPlace(_) => {}
        },
        Op::SendUnpack { src } => f(src),
        Op::DoCall { .. } => {}
        Op::CallableConvert { .. } => {}
        Op::SendPlaceholder { .. } => {}
        Op::CallablePartial { .. } => {}
        Op::Recv { .. } | Op::RecvInit { .. } => {}
        Op::RecvDefault { val, .. } => f(val),
        Op::RecvVariadic { .. } => {}
        Op::VerifyReturn { val } | Op::Return { val } | Op::ReturnByRefValue { val } => f(val),
        Op::ReturnRef { .. } => {}
        Op::GeneratorCreate => {}
        Op::Yield { key, val, .. } => {
            if let Some(k) = key {
                f(k);
            }
            if let Some(v) = val {
                f(v);
            }
        }
        Op::YieldRef { key, .. } => {
            if let Some(k) = key {
                f(k);
            }
        }
        Op::YieldByRefValue { key, val, .. } => {
            if let Some(k) = key {
                f(k);
            }
            f(val);
        }
        Op::YieldFrom { src, .. } => f(src),
        Op::DeclareFunction(_) | Op::DeclareClass(_) | Op::DeclareAnonClass { .. } => {}
        Op::MakeClosure { .. } => {}
        Op::DeclareConst { val, .. } => f(val),
        Op::FetchConst { .. } => {}
        Op::FetchClassConst { class, name, .. } => {
            if let ClassSpec::Dyn(c) = class {
                f(c);
            }
            f(name);
        }
        Op::FetchClassName { class, .. } => {
            if let ClassSpec::Dyn(c) = class {
                f(c);
            }
        }
        Op::InstanceOf { a, class, .. } => {
            f(a);
            if let ClassSpec::Dyn(c) = class {
                f(c);
            }
        }
        Op::Clone { a, .. } => f(a),
        Op::FuncNumArgs { .. } | Op::FuncGetArgs { .. } => {}
        Op::Strlen { a, .. }
        | Op::Count { a, .. }
        | Op::TypeCheck { a, .. }
        | Op::GetType { a, .. }
        | Op::GetClass { a, .. } => f(a),
        Op::GetCalledClass { .. } | Op::Defined { .. } => {}
        Op::InArray { a, .. } => f(a),
        Op::KeyExists { key, array, .. } => {
            f(key);
            f(array);
        }
        Op::Echo(op) | Op::Exit(op) | Op::Free(op) | Op::Throw(op) => f(op),
        Op::Catch { .. } | Op::Rethrow => {}
        Op::FastCall { .. } | Op::FastRet { .. } | Op::DiscardFinally { .. } => {}
        Op::BeginSilence { .. } => {}
        Op::EndSilence { saved } => f(saved),
        Op::Include { a, .. } => f(a),
        Op::FeReset { subject, .. } => f(subject),
        Op::FeFetch { .. } | Op::FeFree { .. } | Op::FeFreeOnReturn { .. } => {}
        Op::MatchError { subject } => f(subject),
        Op::TypeAssertArray { val, .. } => f(val),
        Op::Ticks(_) | Op::Diag { .. } | Op::UserOpcode { .. } | Op::AssertCheck { .. } => {}
        Op::ConstPropCheck { obj, name, .. } => {
            f(obj);
            f(name);
        }
        Op::ConstDimCheck { obj } => f(obj),
        // Specialised ops
        Op::Abs { src, .. } => f(src),
        Op::CmpJmp { a, b, .. }
        | Op::BinAssignCv { a, b, .. }
        | Op::BinAssignCvCmpJmp { a, b, .. }
        | Op::BinAssignCvJmp { a, b, .. } => {
            f(a);
            f(b);
        }
        Op::IncDecCv { .. } => {}
        Op::AssignOpCv { val, .. }
        | Op::AssignOpCvCmpJmp { val, .. }
        | Op::AssignOpCvJmp { val, .. } => f(val),
        Op::AssignOpDimCv { dim, val, .. } | Op::AssignDimCv { dim, val, .. } => {
            f(dim);
            f(val);
        }
        Op::IncDecDimCv { dim, .. } | Op::FetchDimCv { dim, .. } | Op::IssetDimCv { dim, .. } => {
            f(dim)
        }
        Op::FetchDimsCv { .. } => {}
        Op::FetchStaticProp { .. } | Op::FetchGlobal { .. } => {}
        Op::AssignStaticProp { val, .. } | Op::AssignOpStaticProp { val, .. } => f(val),
        Op::IssetStaticProp { .. } => {}
        Op::ConcatCv { val, .. } => f(val),
        Op::AssignDimsCv { val, .. } => f(val),
        Op::AssignOpDimsCv { val, .. } => f(val),
        Op::FetchConstTo { .. } | Op::FetchClassConstTo { .. } => {}
        Op::IncDecCvCmpJmp { .. } => {}
        Op::FetchObjProp { .. } | Op::IncDecObjProp { .. } | Op::IssetObjProp { .. } => {}
        Op::FetchObjPropDim { dim, .. }
        | Op::IncDecObjPropDim { dim, .. }
        | Op::IssetObjPropDim { dim, .. } => f(dim),
        Op::AssignObjProp { val, .. } | Op::AssignOpObjProp { val, .. } => f(val),
        Op::AssignObjPropDim { dim, val, .. } | Op::AssignOpObjPropDim { dim, val, .. } => {
            f(dim);
            f(val);
        }
    }
}

pub(crate) fn for_each_place_operand_mut(place: &mut Place, mut f: impl FnMut(&mut Operand)) {
    match &mut place.base {
        PlaceBase::Cv(_) | PlaceBase::This | PlaceBase::GlobalsArray => {}
        PlaceBase::VarVar(op) | PlaceBase::Value(op) | PlaceBase::Global(op) => f(op),
        PlaceBase::StaticProp { class, name } => {
            if let ClassSpec::Dyn(c) = class {
                f(c);
            }
            f(name);
        }
    }
    for seg in &mut place.segs {
        match seg {
            Seg::Dim(op) | Seg::Prop(op) => f(op),
            Seg::Append => {}
        }
    }
}

pub(crate) fn for_each_label_mut(op: &mut Op, mut f: impl FnMut(&mut Label)) {
    match op {
        Op::Jmp(t)
        | Op::JmpZ(_, t)
        | Op::JmpNz(_, t)
        | Op::JmpZEx { target: t, .. }
        | Op::JmpNzEx { target: t, .. }
        | Op::JmpSet { target: t, .. }
        | Op::Coalesce { target: t, .. }
        | Op::JmpNull { target: t, .. }
        | Op::BindStatic { skip: t, .. }
        | Op::RecvInit { skip: t, .. }
        | Op::Catch { next: t, .. }
        | Op::FastCall { target: t, .. }
        | Op::FeReset { end: t, .. }
        | Op::FeFetch { end: t, .. }
        | Op::FeFreeOnReturn { loop_at: t, .. }
        | Op::AssertCheck { skip: t, .. }
        | Op::CmpJmp { target: t, .. }
        | Op::BinAssignCvJmp { target: t, .. }
        | Op::AssignOpCvJmp { target: t, .. } => f(t),
        Op::New { skip: t, .. } => {
            if *t != 0 {
                f(t);
            }
        }
        _ => {}
    }
}

pub(crate) fn def_tmp(op: &Op) -> Option<u32> {
    match op {
        Op::Move { dst, .. }
        | Op::Bin { dst, .. }
        | Op::Unary { dst, .. }
        | Op::Cast { dst, .. }
        | Op::Rope { dst, .. }
        | Op::InitArray { dst }
        | Op::JmpZEx { dst, .. }
        | Op::JmpNzEx { dst, .. }
        | Op::JmpSet { dst, .. }
        | Op::Coalesce { dst, .. }
        | Op::JmpNull { dst, .. }
        | Op::MakeRef { dst, .. }
        | Op::Fetch { dst, .. }
        | Op::Isset { dst, .. }
        | Op::Empty { dst, .. }
        | Op::FetchThis { dst }
        | Op::New { dst, .. }
        | Op::CallableConvert { dst, .. }
        | Op::CallablePartial { dst, .. }
        | Op::DeclareAnonClass { dst, .. }
        | Op::MakeClosure { dst, .. }
        | Op::FetchConst { dst, .. }
        | Op::FetchClassConst { dst, .. }
        | Op::FetchClassName { dst, .. }
        | Op::InstanceOf { dst, .. }
        | Op::Clone { dst, .. }
        | Op::BeginSilence { dst }
        | Op::Include { dst, .. }
        | Op::AssertCheck { dst, .. }
        | Op::ConstPropCheck { dst, .. }
        | Op::CmpJmp { dst, .. }
        | Op::BinAssignCv { dst, .. }
        | Op::BinAssignCvCmpJmp { dst, .. }
        | Op::BinAssignCvJmp { dst, .. }
        | Op::FetchDimCv { dst, .. }
        | Op::FetchDimsCv { dst, .. }
        | Op::IssetDimCv { dst, .. }
        | Op::FetchStaticProp { dst, .. }
        | Op::FetchGlobal { dst, .. }
        | Op::IssetStaticProp { dst, .. }
        | Op::FetchConstTo { dst, .. }
        | Op::FetchClassConstTo { dst, .. } => Some(*dst),

        Op::AssignCv { dst, .. }
        | Op::Assign { dst, .. }
        | Op::AssignOp { dst, .. }
        | Op::IncDec { dst, .. }
        | Op::AssignRef { dst, .. }
        | Op::ListFetchRef { dst, .. }
        | Op::ArrayKeyExists { dst, .. }
        | Op::DoCall { dst }
        | Op::Yield { dst, .. }
        | Op::YieldRef { dst, .. }
        | Op::YieldByRefValue { dst, .. }
        | Op::YieldFrom { dst, .. }
        | Op::FuncNumArgs { dst }
        | Op::FuncGetArgs { dst }
        | Op::Strlen { dst, .. }
        | Op::Count { dst, .. }
        | Op::TypeCheck { dst, .. }
        | Op::GetType { dst, .. }
        | Op::GetClass { dst, .. }
        | Op::GetCalledClass { dst }
        | Op::Defined { dst, .. }
        | Op::InArray { dst, .. }
        | Op::KeyExists { dst, .. }
        | Op::IncDecCv { dst, .. }
        | Op::AssignOpCv { dst, .. }
        | Op::IncDecDimCv { dst, .. }
        | Op::AssignStaticProp { dst, .. }
        | Op::AssignOpStaticProp { dst, .. }
        | Op::ConcatCv { dst, .. }
        | Op::Abs { dst, .. } => *dst,

        _ => None,
    }
}

pub(crate) fn clear_def_tmp(op: &mut Op) -> bool {
    match op {
        Op::AssignCv { dst, .. }
        | Op::Assign { dst, .. }
        | Op::AssignOp { dst, .. }
        | Op::IncDec { dst, .. }
        | Op::AssignRef { dst, .. }
        | Op::ListFetchRef { dst, .. }
        | Op::ArrayKeyExists { dst, .. }
        | Op::Yield { dst, .. }
        | Op::YieldRef { dst, .. }
        | Op::YieldByRefValue { dst, .. }
        | Op::YieldFrom { dst, .. }
        | Op::FuncNumArgs { dst }
        | Op::FuncGetArgs { dst }
        | Op::Strlen { dst, .. }
        | Op::Count { dst, .. }
        | Op::TypeCheck { dst, .. }
        | Op::GetType { dst, .. }
        | Op::GetClass { dst, .. }
        | Op::GetCalledClass { dst }
        | Op::Defined { dst, .. }
        | Op::InArray { dst, .. }
        | Op::KeyExists { dst, .. }
        | Op::IncDecCv { dst, .. }
        | Op::AssignOpCv { dst, .. }
        | Op::IncDecDimCv { dst, .. }
        | Op::AssignStaticProp { dst, .. }
        | Op::AssignOpStaticProp { dst, .. }
        | Op::ConcatCv { dst, .. }
        | Op::Abs { dst, .. } => {
            if dst.is_some() {
                *dst = None;
                true
            } else {
                false
            }
        }
        // Not `DoCall`: a call keeps its result: `DoCall { dst: Some }` + `FREE` is how
        // `(void)f()` marks the value as used (`ZEND_FREE_VOID_CAST`, which
        // php-src's block_pass.c and dce.c keep), while `dst: None` makes a
        // `#[\NoDiscard]` function warn.
        _ => false,
    }
}

pub(crate) fn add_or_find_literal(literals: &mut Vec<Value>, val: Value) -> u32 {
    for (i, existing) in literals.iter().enumerate() {
        if literals_equal(existing, &val) {
            return i as u32;
        }
    }
    let idx = literals.len() as u32;
    literals.push(val);
    idx
}

pub(crate) fn literals_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
        (Value::Str(a), Value::Str(b)) => a.as_bytes() == b.as_bytes(),
        _ => false,
    }
}

/// The array temporary an array-literal build step adds to (`ZEND_ADD_ARRAY_ELEMENT`,
/// `ZEND_ADD_ARRAY_UNPACK`): a raw tmp index rather than an [`Operand`], so
/// [`for_each_operand_mut`] does not visit it. Every pass that counts, marks
/// or renumbers temporaries must include it.
pub(crate) fn array_build_tmp_mut(op: &mut Op) -> Option<&mut u32> {
    match op {
        Op::ArrayAdd { arr, .. }
        | Op::ArrayAddRef { arr, .. }
        | Op::ArrayUnpack { arr, .. }
        | Op::ConstArrayUnpack { arr, .. } => Some(arr),
        _ => None,
    }
}

pub(crate) fn count_tmp_uses(op_array: &OpArray, counts: &mut [u32]) {
    for op in &op_array.ops {
        if let Some(t) = array_build_tmp_mut(&mut op.clone()) {
            if let Some(c) = counts.get_mut(*t as usize) {
                *c += 1;
            }
        }
        let mut op_clone = op.clone();
        for_each_operand_mut(&mut op_clone, |operand| match operand {
            Operand::Tmp(t) | Operand::TmpKeep(t) => {
                if let Some(c) = counts.get_mut(*t as usize) {
                    *c += 1;
                }
            }
            _ => {}
        });
    }

    for place in &op_array.places {
        let mut place_clone = place.clone();
        for_each_place_operand_mut(&mut place_clone, |operand| match operand {
            Operand::Tmp(t) | Operand::TmpKeep(t) => {
                if let Some(c) = counts.get_mut(*t as usize) {
                    *c += 1;
                }
            }
            _ => {}
        });
    }

    for live in &op_array.live_tmps {
        if let Some(c) = counts.get_mut(live.tmp as usize) {
            *c += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::{compile, CompileOptions};

    fn compile_src(src: &str, level: u32) -> CompiledUnit {
        let program =
            php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
        let opts = CompileOptions {
            filename: "test.php".into(),
            optimization_level: level,
            ..CompileOptions::default()
        };
        compile(&program, Rc::from(src.as_bytes()), opts).expect("compile")
    }

    /// Pass 13 keeps an assignment whose variable is only read through an
    /// opcode it has no explicit rule for (here the argument of a call):
    /// `$c = $a = $b; var_dump($c);` once lost `$c` (opcache `jit/assign_020`).
    #[test]
    fn unused_variable_pass_keeps_variables_read_by_other_opcodes() {
        let unit = compile_src(
            "<?php function foo() { $c = array(); $a = 1; $b = 2; $c = $a = $b; var_dump($c); }",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let funcs = unit.main.code.decls.functions.borrow();
        let foo = funcs
            .iter()
            .find(|f| f.name.as_bytes() == b"foo")
            .expect("foo");
        let code = &foo.code;
        let c = code
            .cv_names
            .iter()
            .position(|n| n.as_bytes() == b"c")
            .and_then(|i| u32::try_from(i).ok())
            .expect("$c is a CV");
        let writes_c = code.ops.iter().any(|op| match op {
            Op::AssignCv { cv, .. } => *cv == c,
            Op::Assign { place, .. } => code
                .places
                .get(*place as usize)
                .is_some_and(|p| p.base == PlaceBase::Cv(c) && p.segs.is_empty()),
            _ => false,
        });
        assert!(writes_c, "the assignment to $c was removed: {:?}", code.ops);
    }

    /// The optimized code of the first function of `unit` matching `pick`
    /// (named functions and closures alike).
    fn find_code(
        unit: &CompiledUnit,
        pick: impl Fn(&crate::func::UserFunction) -> bool,
    ) -> Rc<OpArray> {
        unit.decls
            .functions
            .borrow()
            .iter()
            .find(|f| pick(f))
            .map(|f| f.code.clone())
            .expect("function")
    }

    fn named<'a>(name: &'a [u8]) -> impl Fn(&crate::func::UserFunction) -> bool + 'a {
        move |f| f.name.as_bytes() == name
    }

    fn cv_index(code: &OpArray, name: &[u8]) -> u32 {
        code.cv_names
            .iter()
            .position(|n| n.as_bytes() == name)
            .and_then(|i| u32::try_from(i).ok())
            .expect("CV")
    }

    /// The ops that assign `cv` as a whole (`$cv = v`).
    fn writes_of(code: &OpArray, cv: u32) -> usize {
        code.ops
            .iter()
            .filter(|op| match op {
                Op::AssignCv { cv: c, .. } => *c == cv,
                Op::Assign { place, .. } => code
                    .places
                    .get(*place as usize)
                    .is_some_and(|p| p.base == PlaceBase::Cv(cv) && p.segs.is_empty()),
                _ => false,
            })
            .count()
    }

    /// A `Catch` followed by its body's `Jmp` used to share one block, so
    /// the CFG lost the edge to the next clause and DCE dropped the
    /// `Rethrow`: an exception no clause matched vanished (jit/assign_055).
    #[test]
    fn unmatched_exception_is_still_rethrown() {
        let unit = compile_src(
            "<?php function f() { try { g(); } catch (Y $e) {} echo 1; }",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let code = find_code(&unit, named(b"f"));
        assert!(
            code.ops.iter().any(|op| matches!(op, Op::Rethrow)),
            "Rethrow removed: {:?}",
            code.ops
        );
    }

    /// The exit edge of `FE_FETCH` mid-block was lost the same way, so the
    /// code after a foreach saw only the value from before the loop
    /// (jit/qm_assign_004, jit/loop_vm_interrupt_001, jit/gh20838).
    #[test]
    fn code_after_foreach_sees_assignments_in_the_loop() {
        let unit = compile_src(
            "<?php function g($p) { $s = 0; foreach ($p as $x) { $s += 3; } return $s; }",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let code = find_code(&unit, named(b"g"));
        let folded = code.ops.iter().any(|op| match op {
            Op::Return {
                val: Operand::Lit(l),
            } => matches!(code.literals.get(*l as usize), Some(Value::Int(0))),
            _ => false,
        });
        assert!(
            !folded,
            "return folded to the pre-loop value: {:?}",
            code.ops
        );
    }

    /// Pass 9 renumbers the value and key temporaries of `FE_FETCH` with
    /// every other use of them (gh24577, jit/gh12527).
    #[test]
    fn temporary_compaction_renumbers_foreach_temporaries() {
        let unit = compile_src(
            "<?php function foo(array $a1) { $a2 = []; foreach ($a1 as $key => $val) { \
             if (!$val) { $a2['bad'][] = $key; } else { $a2[0][] = $key; } } \
             foreach ($a2 as $key => $val) { var_dump($key); } }",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let code = find_code(&unit, named(b"foo"));
        let mut fetches = 0;
        for op in &code.ops {
            if let Op::FeFetch { val, key, .. } = op {
                fetches += 1;
                assert!(*val < code.num_tmps, "value tmp {val} out of range");
                if let Some(k) = key {
                    assert!(*k < code.num_tmps, "key tmp {k} out of range");
                }
            }
        }
        assert_eq!(fetches, 2);
    }

    /// Pass 13 keeps a write to a by-reference `use` variable, which the
    /// enclosing scope sees (jit/assign_dim_op_006).
    #[test]
    fn by_reference_use_assignment_is_kept() {
        let unit = compile_src(
            "<?php $v = null; $f = function () use (&$v) { $v = 0; };",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let code = find_code(&unit, |f| f.uses.iter().any(|u| u.by_ref));
        let v = cv_index(&code, b"v");
        assert_eq!(writes_of(&code, v), 1, "write removed: {:?}", code.ops);
    }

    /// Pass 12 keeps the slots of `use` variables: the closure binds them
    /// by number, so dropping an unread `$a` bound `$b` into `$a`'s slot.
    #[test]
    fn use_variable_slots_are_not_compacted() {
        let unit = compile_src(
            "<?php $a = 1; $b = 2; $f = function () use ($a, $b) { return $b; };",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let funcs = unit.decls.functions.borrow();
        let closure = funcs.iter().find(|f| f.uses.len() == 2).expect("closure");
        for u in &closure.uses {
            let slot = closure.code.cv_names.get(u.cv as usize).expect("slot");
            assert_eq!(slot.as_bytes(), u.name.as_bytes());
        }
    }

    /// Top-level CVs are globals that an error handler can change (here
    /// the "Undefined variable" warning of `$undef`), so SCCP never
    /// replaces a read of one with a constant.
    #[test]
    fn top_level_variables_are_not_propagated() {
        let unit = compile_src(
            "<?php $x = 1; echo $undef; echo $x;",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let ops = &unit.main.code.ops;
        let cv_echoes = ops
            .iter()
            .filter(|op| matches!(op, Op::Echo(Operand::Cv(_))))
            .count();
        assert_eq!(cv_echoes, 2, "{ops:?}");
    }

    /// A by-reference parameter may be a typed reference (assigning `"5"`
    /// to one bound to an `int` property stores 5) or change when user code
    /// runs, so its reads are never replaced with a constant.
    #[test]
    fn by_reference_parameter_is_not_propagated() {
        let unit = compile_src(
            "<?php function f(&$x) { $x = '5'; echo $x; }",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let code = find_code(&unit, named(b"f"));
        assert!(
            code.ops
                .iter()
                .any(|op| matches!(op, Op::Echo(Operand::Cv(_)))),
            "{:?}",
            code.ops
        );
    }

    /// No CV propagation in a function with try/finally (php-src builds no
    /// SSA for it): the code after `finally` sees what the finally block
    /// assigned.
    #[test]
    fn variables_are_not_propagated_across_finally() {
        let unit = compile_src(
            "<?php function f() { $x = 1; try { $x = 1; } finally { $x = 2; } echo $x; }",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let code = find_code(&unit, named(b"f"));
        assert!(
            code.ops
                .iter()
                .any(|op| matches!(op, Op::Echo(Operand::Cv(_)))),
            "{:?}",
            code.ops
        );
    }

    /// `clone $a;` keeps the `FREE` that destroys the copy right away.
    #[test]
    fn discarded_object_is_still_freed() {
        let unit = compile_src(
            "<?php function f($a) { clone $a; echo 1; }",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let code = find_code(&unit, named(b"f"));
        assert!(
            code.ops
                .iter()
                .any(|op| matches!(op, Op::Free(Operand::Tmp(_)))),
            "{:?}",
            code.ops
        );
    }

    /// Dropping `if ($x) {}` keeps the read of `$x`, which warns when
    /// undefined (php-src's `CHECK_VAR`).
    #[test]
    fn empty_branch_still_reads_its_variable() {
        let unit = compile_src(
            "<?php function f() { if ($x) {} }",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let code = find_code(&unit, named(b"f"));
        assert!(
            code.ops
                .iter()
                .any(|op| matches!(op, Op::Free(Operand::Cv(_)) | Op::JmpZ(Operand::Cv(_), _))),
            "{:?}",
            code.ops
        );
    }

    /// Pass 13 keeps both writes of a variable that may hold an object: the
    /// overwrite is where the object is destroyed.
    #[test]
    fn unread_variable_holding_an_object_keeps_its_writes() {
        let unit = compile_src(
            "<?php function f() { $a = new stdClass; echo 1; $a = 1; echo 2; }",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let code = find_code(&unit, named(b"f"));
        let a = cv_index(&code, b"a");
        assert_eq!(writes_of(&code, a), 2, "{:?}", code.ops);
    }

    /// SCCP substitutes a known temporary constant into kept reads too:
    /// `match` reads its subject with `TmpKeep` in every arm, and the
    /// defining move is removed, so a missed kept read was undefined
    /// (`match($x)` with `$x = 3` fell through to `default`).
    #[test]
    fn sccp_substitutes_kept_temporary_reads() {
        let unit = compile_src(
            "<?php function t() { $x = 3; return match($x) { 1, 3 => 'a', default => 'b' }; }",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let funcs = unit.main.code.decls.functions.borrow();
        let t = funcs.iter().find(|f| f.name.as_bytes() == b"t").expect("t");
        let code = &t.code;
        let mut defined = vec![false; code.num_tmps as usize + 1];
        for op in &code.ops {
            if let Some(d) = def_tmp(op) {
                if let Some(slot) = defined.get_mut(d as usize) {
                    *slot = true;
                }
            }
        }
        for op in &code.ops {
            let mut probe = op.clone();
            for_each_operand_mut(&mut probe, |o| {
                if let Operand::TmpKeep(k) = o {
                    assert!(
                        defined.get(*k as usize).copied().unwrap_or(false),
                        "kept read of undefined T{k}: {:?}",
                        code.ops
                    );
                }
            });
        }
    }

    /// Literal compaction (pass 11) remaps the literal indices of every op,
    /// including a `name: ?` placeholder's name and `in_array`'s haystack:
    /// a duplicate `7` before them once left both pointing one literal too
    /// far ("Unknown named parameter $Int(8)", `partial_application/
    /// const_arg_opt`).
    #[test]
    fn literal_compaction_remaps_placeholder_names_and_in_array_sets() {
        let unit = compile_src(
            "<?php function h($a, $b, $c) {} function t($s) { $a = 7; $b = 7; \
             $f = h(1, c: ?, b: ?); return [$f, $a, $b, in_array($s, ['b', 'c'])]; }",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let funcs = unit.main.code.decls.functions.borrow();
        let t = funcs.iter().find(|f| f.name.as_bytes() == b"t").expect("t");
        let code = &t.code;
        let names: Vec<&[u8]> = code
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::SendPlaceholder { name: Some(n) } => match code.literals.get(*n as usize) {
                    Some(Value::Str(s)) => Some(s.as_bytes()),
                    other => panic!("placeholder name literal {other:?}: {:?}", code.ops),
                },
                _ => None,
            })
            .collect();
        assert_eq!(names, [&b"c"[..], &b"b"[..]], "{:?}", code.ops);
        for op in &code.ops {
            if let Op::InArray { set, .. } = op {
                assert!(
                    matches!(code.literals.get(*set as usize), Some(Value::Array(_))),
                    "in_array set literal {:?}",
                    code.literals.get(*set as usize)
                );
            }
        }
    }

    /// `(void)f()` compiles to a call with a result plus its `FREE`; the
    /// optimizer must keep both, since a call without a result makes a
    /// `#[\NoDiscard]` function warn (opcache `nodiscard_001`).
    #[test]
    fn void_cast_keeps_call_result() {
        let unit = compile_src(
            "<?php function t() { return 1; } function u() { (void)t(); }",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let funcs = unit.main.code.decls.functions.borrow();
        let u = funcs.iter().find(|f| f.name.as_bytes() == b"u").expect("u");
        let ops = &u.code.ops;
        let call = ops
            .iter()
            .find_map(|op| match op {
                Op::DoCall { dst } => Some(*dst),
                _ => None,
            })
            .expect("DoCall");
        let t = call.unwrap_or_else(|| panic!("call result dropped: {ops:?}"));
        assert!(
            ops.iter()
                .any(|op| matches!(op, Op::Free(Operand::Tmp(f)) if *f == t)),
            "FREE of the call result dropped: {ops:?}"
        );
    }

    #[test]
    fn test_constant_folding() {
        let unit = compile_src("<?php echo 1 + 2 * 3;", DEFAULT_OPTIMIZATION_LEVEL);
        let ops = &unit.main.code.ops;
        // There should be no Bin op remaining
        assert!(
            !ops.iter().any(|op| matches!(op, Op::Bin { .. })),
            "Expected Bin op to be folded, got: {ops:?}"
        );
        // There should be an Echo op
        assert!(
            ops.iter().any(|op| matches!(op, Op::Echo(_))),
            "Expected Echo op, got: {ops:?}"
        );
    }

    #[test]
    fn test_consecutive_echo_merging() {
        let unit = compile_src(
            "<?php echo 'hello '; echo 'world'; echo '!';",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let ops = &unit.main.code.ops;
        // All three string echoes should be merged into a single Echo op
        let echo_count = ops.iter().filter(|op| matches!(op, Op::Echo(_))).count();
        assert_eq!(
            echo_count, 1,
            "Expected 1 merged Echo op, got {echo_count}: {ops:?}"
        );
        if let Some(Op::Echo(Operand::Lit(l))) = ops.iter().find(|op| matches!(op, Op::Echo(_))) {
            let lit = &unit.main.code.literals[*l as usize];
            assert!(
                matches!(lit, Value::Str(s) if s.as_bytes() == b"hello world!"),
                "Expected 'hello world!', got: {lit:?}"
            );
        }
    }

    #[test]
    fn test_constant_branch_folding() {
        let unit = compile_src(
            "<?php if (true) { echo 'a'; } else { echo 'b'; }",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let ops = &unit.main.code.ops;
        // There should be no JmpZ or JmpNz
        assert!(
            !ops.iter()
                .any(|op| matches!(op, Op::JmpZ(..) | Op::JmpNz(..))),
            "Expected conditional jump to be folded, got: {ops:?}"
        );
    }

    #[test]
    fn test_nop_removal() {
        let unit = compile_src("<?php $a = 1; echo $a;", DEFAULT_OPTIMIZATION_LEVEL);
        let ops = &unit.main.code.ops;
        // NOPs should be removed
        assert!(
            !ops.iter().any(|op| matches!(op, Op::Nop)),
            "Expected no NOPs, got: {ops:?}"
        );
    }

    #[test]
    fn test_pass2_typecheck_folding() {
        let unit = compile_src(
            "<?php echo is_null(null) ? 'yes' : 'no';",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let ops = &unit.main.code.ops;
        assert!(
            !ops.iter().any(|op| matches!(op, Op::TypeCheck { .. })),
            "Expected TypeCheck to be folded, got: {ops:?}"
        );
        if let Some(Op::Echo(Operand::Lit(l))) = ops.iter().find(|op| matches!(op, Op::Echo(_))) {
            let lit = &unit.main.code.literals[*l as usize];
            assert!(
                matches!(lit, Value::Str(s) if s.as_bytes() == b"yes"),
                "Expected 'yes', got: {lit:?}"
            );
        }
    }

    #[test]
    fn test_pass4_builtin_folding() {
        let unit = compile_src("<?php echo strlen('hello');", DEFAULT_OPTIMIZATION_LEVEL);
        let ops = &unit.main.code.ops;
        assert!(
            !ops.iter()
                .any(|op| matches!(op, Op::Strlen { .. } | Op::InitFCall { .. })),
            "Expected strlen call to be folded, got: {ops:?}"
        );
        if let Some(Op::Echo(Operand::Lit(l))) = ops.iter().find(|op| matches!(op, Op::Echo(_))) {
            let lit = &unit.main.code.literals[*l as usize];
            assert!(
                matches!(lit, Value::Str(s) if s.as_bytes() == b"5"),
                "Expected '5', got: {lit:?}"
            );
        }
    }

    #[test]
    fn test_pass12_13_unused_variable_removal() {
        let unit = compile_src(
            "<?php function f() { $unused = 123; return 42; }",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let fn_code = unit.decls.functions.borrow()[0].code.clone();
        assert_eq!(
            fn_code.cv_names.len(),
            0,
            "Expected unused CV to be removed, got: {:?}",
            fn_code.cv_names
        );
    }

    /// The temporaries that `code` reads (from ops and places) but that no op
    /// writes.
    fn undefined_tmp_reads(code: &OpArray) -> Vec<u32> {
        let mut defined = std::collections::HashSet::new();
        for op in &code.ops {
            if let Some(t) = def_tmp(op) {
                defined.insert(t);
            }
            if let Op::FeFetch { val, key, .. } = op {
                defined.insert(*val);
                if let Some(k) = key {
                    defined.insert(*k);
                }
            }
        }
        let mut read = Vec::new();
        let mut note = |operand: &mut Operand| {
            if let Operand::Tmp(t) | Operand::TmpKeep(t) = operand {
                read.push(*t);
            }
        };
        for op in &code.ops {
            let mut op = op.clone();
            for_each_operand_mut(&mut op, &mut note);
        }
        for place in &code.places {
            let mut place = place.clone();
            for_each_place_operand_mut(&mut place, &mut note);
        }
        read.retain(|t| !defined.contains(t));
        read
    }

    /// SCCP replaces a constant temporary in the uses that keep it alive
    /// (`JMP_NULL`, `FETCH_LIST_R`) too before dropping its `QM_ASSIGN`: they
    /// once read a temporary nothing wrote, losing the "Cannot use int as
    /// array" and "Attempt to read property" warnings (opcache
    /// `opt/block_pass_003`, `opt/block_pass_004`).
    #[test]
    fn sccp_substitutes_kept_temporaries() {
        let unit = compile_src(
            "<?php function a() { (1?4:y)?->y; }\n\
             function b() { for ($i = 0; $i < 10; $i++) { list($a, $b) = 1 ? 1 : 2; } }",
            u32::MAX >> 1,
        );
        for name in [&b"a"[..], b"b"] {
            let code = find_code(&unit, named(name));
            let bad = undefined_tmp_reads(&code);
            assert!(
                bad.is_empty(),
                "undefined tmps {bad:?} read in {:?}",
                code.ops
            );
        }
    }

    /// The literal the first `Return` of `code` returns, if it returns one.
    fn returned_lit(code: &OpArray) -> Option<Value> {
        code.ops.iter().find_map(|op| match op {
            Op::Return {
                val: Operand::Lit(l),
            } => code.literals.get(*l as usize).cloned(),
            _ => None,
        })
    }

    /// `$a += $b` of constant CVs folds (opcache `opt/sccp_001`: the
    /// function becomes `RETURN int(4)`).
    #[test]
    fn sccp_folds_compound_assignment_of_constant_variables() {
        let unit = compile_src(
            "<?php function foo() { $a = 1; $b = $a + 2; $a += $b; return $a; }",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let code = find_code(&unit, named(b"foo"));
        assert!(
            matches!(returned_lit(&code), Some(Value::Int(4))),
            "{:?}",
            code.ops
        );
    }

    /// `++$a` folds; `$a++` keeps the old value as its result.
    #[test]
    fn sccp_folds_increments_with_their_own_results() {
        let unit = compile_src(
            "<?php function f() { $a = 1; $b = $a++; $c = ++$a; return $b * 10 + $c; }",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let code = find_code(&unit, named(b"f"));
        // 1 * 10 + 3, when folded at all.
        if let Some(v) = returned_lit(&code) {
            assert!(matches!(v, Value::Int(13)), "{v:?} in {:?}", code.ops);
        }
    }

    /// In-range offsets of literal strings fold; an out-of-range one warns
    /// and stays (opcache `opt/sccp_043`).
    #[test]
    fn sccp_folds_literal_string_offsets() {
        let unit = compile_src(
            "<?php function f() { return \"abc\"[-1]; }\n\
             function g() { return \"abc\"[-4]; }",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let f = find_code(&unit, named(b"f"));
        assert!(
            matches!(returned_lit(&f), Some(Value::Str(s)) if s.as_bytes() == b"c"),
            "{:?}",
            f.ops
        );
        let g = find_code(&unit, named(b"g"));
        assert!(returned_lit(&g).is_none(), "{:?}", g.ops);
    }

    /// An existing element of an array held by a constant CV, at a constant
    /// CV offset, folds; a missing one warns and stays.
    #[test]
    fn sccp_folds_reads_of_constant_arrays() {
        let unit = compile_src(
            "<?php function f() { $a = [1, 2]; $k = 1; return $a[$k]; }\n\
             function g() { $a = [1, 2]; $k = 5; return $a[$k]; }",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let f = find_code(&unit, named(b"f"));
        assert!(
            matches!(returned_lit(&f), Some(Value::Int(2))),
            "{:?}",
            f.ops
        );
        let g = find_code(&unit, named(b"g"));
        assert!(returned_lit(&g).is_none(), "{:?}", g.ops);
    }

    /// A by-reference parameter is never folded through `+=`.
    #[test]
    fn sccp_keeps_compound_assignment_of_references() {
        let unit = compile_src(
            "<?php function f(&$x) { $x = 1; $x += 2; return $x; }",
            DEFAULT_OPTIMIZATION_LEVEL,
        );
        let code = find_code(&unit, named(b"f"));
        assert!(returned_lit(&code).is_none(), "{:?}", code.ops);
    }
}

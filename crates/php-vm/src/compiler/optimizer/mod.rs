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

/// Optimizes `op_array` in place according to `level` bitmask.
pub fn optimize_op_array(op_array: &mut OpArray, level: u32) {
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
        if (level & PASS_3) != 0 {
            changed |= pass3::run_pass3(op_array);
        }
        if (level & (PASS_6 | PASS_8)) != 0 {
            changed |= sccp::run_sccp(op_array);
        }
        if (level & PASS_5) != 0 {
            changed |= block_pass::run_block_pass(op_array);
        }
        if (level & PASS_14) != 0 {
            changed |= dce::run_dce(op_array);
        }

        if !changed {
            break;
        }
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
            optimize_op_array(code, level);
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
        Op::CmpJmp { a, b, .. } | Op::BinAssignCv { a, b, .. } => {
            f(a);
            f(b);
        }
        Op::IncDecCv { .. } => {}
        Op::AssignOpCv { val, .. } => f(val),
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
        | Op::New { skip: t, .. }
        | Op::RecvInit { skip: t, .. }
        | Op::Catch { next: t, .. }
        | Op::FastCall { target: t, .. }
        | Op::FeReset { end: t, .. }
        | Op::FeFetch { end: t, .. }
        | Op::FeFreeOnReturn { loop_at: t, .. }
        | Op::AssertCheck { skip: t, .. }
        | Op::CmpJmp { target: t, .. } => f(t),
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
        | Op::Abs { dst, .. } => {
            if dst.is_some() {
                *dst = None;
                true
            } else {
                false
            }
        }
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

pub(crate) fn count_tmp_uses(op_array: &OpArray, counts: &mut [u32]) {
    for op in &op_array.ops {
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
}

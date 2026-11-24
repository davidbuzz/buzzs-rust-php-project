// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The compiler's last pass: selects specialised handlers for ops whose
//! operands make a typed fast path possible.
//!
//! php-src generates one handler per opcode and operand kind
//! (`zend_vm_gen.php` over the `SPEC(...)` annotations of
//! `Zend/zend_vm_def.h`) and marks a comparison whose result only feeds the
//! next `JMPZ`/`JMPNZ` as a smart branch (`zend_emit_cond_jump`,
//! `Zend/zend_compile.c`). Here the choice is made once per op array: a
//! generic op is replaced in place by its specialised form (the
//! `Op::CmpJmp` ... variants of [`crate::opcode::Op`]).
//!
//! Every rewrite keeps the op count and the op after the rewritten one, so
//! jump targets, try ranges, live ranges and lines stay valid. A specialised
//! op skips its successor only where the successor (the jump testing the
//! comparison, the `AssignCv` storing the result) would have done nothing
//! but consume the temporary: on its fast path, and for the constant
//! fetches (`FetchConstTo`, `FetchClassConstTo`) also after the generic
//! fetch, when the variable holds no refcounted value; on any other path it
//! writes the temporary and the successor runs as before. A jump landing on
//! the successor from elsewhere also runs it as before.
//!
//! Note: Single-argument builtin function calls matching deterministic type
//! predicates (`is_int`, `is_string`, `is_array`, etc.) and scalar functions
//! (`strlen`, `count`) are fast-pathed at call execution in [`crate::call`].

use crate::opcode::{
    cmp_outcome, ArgSrc, BinKind, ClassSpec, FetchMode, FramelessCall, IncDecKind, Literal, Op,
    OpArray, Operand, Place, PlaceBase, PlaceId, Seg, UnKind,
};
use php_runtime::{BinaryOp, Value};

/// How many tests of a short-circuit result [`result_leads_to`] follows. A
/// chain's tests have been sent on by the time an earlier jump reaches them
/// ([`thread_short_circuits`] goes from the last op to the first), so one
/// step is the rule; the bound keeps a long chain whose end cannot be
/// followed (`$x = $a && $b && ...`) linear to compile.
const SHORT_CIRCUIT_HOPS: usize = 8;

/// Where a jump onto `ops[at]` ends up when the temporary `tmp` holds
/// `value` and the ops there only test it: the `JmpZ` / `JmpNz` of an `if`
/// or a loop, the `JmpZEx` / `JmpNzEx` of the next `&&` / `||` of a
/// chain, which stores the same value into its own result and jumps on, or
/// falls through, or the `Bool` / `Not` that passes it (negated) to the
/// test after it (`a && (b || c)`: the inner result is the outer one's
/// right side; `!(a && b)`). `None` when the op there does anything else
/// with the temporary (stores it, returns it, passes it on).
fn result_leads_to(ops: &[Op], mut at: u32, mut tmp: u32, mut value: bool) -> Option<u32> {
    for _ in 0..SHORT_CIRCUIT_HOPS {
        match *ops.get(at as usize)? {
            Op::Unary {
                op: un @ (UnKind::Bool | UnKind::Not),
                a: Operand::Tmp(t),
                dst,
            } if t == tmp => {
                (at, tmp, value) = (at + 1, dst, value != (un == UnKind::Not));
            }
            Op::JmpZ(Operand::Tmp(t), to) if t == tmp => {
                return Some(if value { at + 1 } else { to });
            }
            Op::JmpNz(Operand::Tmp(t), to) if t == tmp => {
                return Some(if value { to } else { at + 1 });
            }
            Op::JmpZEx {
                a: Operand::Tmp(t),
                dst,
                target,
            } if t == tmp => {
                if value {
                    return Some(at + 1);
                }
                (at, tmp) = (target, dst);
            }
            Op::JmpNzEx {
                a: Operand::Tmp(t),
                dst,
                target,
            } if t == tmp => {
                if !value {
                    return Some(at + 1);
                }
                (at, tmp) = (target, dst);
            }
            _ => return None,
        }
    }
    None
}

/// Makes the `&&` / `||` of a condition jump to where its result leads, and
/// a `!` or `(bool)` that only feeds a jump that jump. Run before
/// [`specialise`], for a script compiled without the optimizer.
///
/// `zend_compile_short_circuiting` gives `a && b` a result of its own:
/// `JMPZ_EX a` to its end (the result is false), the right side, `BOOL`
/// into the result, and at the end whatever uses it. Where that is the jump
/// of an `if`, of a loop's test or of the next `&&` / `||` of a chain, a
/// decided left side is tested twice, and the right side's truth is stored
/// only to be tested again. opcache sends such jumps on
/// (`zend_optimize_jumps`, `Zend/Optimizer/pass3.c`: `T = JMPZ_EX(X, L1)`
/// with `L1: JMPZ(T, L2)`) and folds a `BOOL` / `BOOL_NOT` into the jump
/// after it (`Zend/Optimizer/pass1.c`); so does this:
///
/// - a `JmpZEx` / `JmpNzEx` whose target only tests its result
///   ([`result_leads_to`]) becomes the `JmpZ` / `JmpNz` of its operand to
///   where that test goes with the value it would have stored. The result
///   is then not stored: only that test read it;
/// - a `Bool` or `Not` into a temporary right before the `JmpZ` / `JmpNz`
///   of that temporary, when no jump lands on that test, becomes the jump
///   itself, on the operand (negated for a `Not`). The truth of a value is
///   the same at either op, and so is what reading it reports (an undefined
///   variable, a NaN), at the same op index and line. The old jump becomes
///   a `Nop`.
///
/// A comparison in front of such a jump is then a smart branch
/// ([`specialise`]'s `CmpJmp`). The op count, every other op, the lines
/// and the live ranges stay (a result that is no longer stored was not
/// stored on the fall-through path before either). Returns whether any
/// jump changed.
pub(crate) fn thread_short_circuits(ops: &mut [Op]) -> bool {
    let mut changed = false;
    // From the last op to the first: the test a jump is sent through has
    // then been sent on itself.
    for i in (0..ops.len()).rev() {
        let sent = match ops[i] {
            Op::JmpZEx { a, dst, target } => {
                result_leads_to(ops, target, dst, false).map(|to| Op::JmpZ(a, to))
            }
            Op::JmpNzEx { a, dst, target } => {
                result_leads_to(ops, target, dst, true).map(|to| Op::JmpNz(a, to))
            }
            _ => None,
        };
        if let Some(op) = sent {
            ops[i] = op;
            changed = true;
        }
    }
    if !ops.iter().any(|op| {
        matches!(
            op,
            Op::Unary {
                op: UnKind::Bool | UnKind::Not,
                ..
            }
        )
    }) {
        return changed;
    }
    // The ops a jump lands on now.
    let mut landed = vec![false; ops.len() + 1];
    for op in ops.iter_mut() {
        super::optimizer::for_each_label_mut(op, |to| {
            if let Some(l) = landed.get_mut(*to as usize) {
                *l = true;
            }
        });
    }
    // Backwards: the `!` in front of a `Bool` that became a jump is met
    // after it (`!$a && !$b` as a condition).
    for at in (1..ops.len()).rev() {
        if landed[at] {
            continue;
        }
        let Op::Unary {
            op: un @ (UnKind::Bool | UnKind::Not),
            a,
            dst,
        } = ops[at - 1]
        else {
            continue;
        };
        let (on_true, to) = match ops[at] {
            Op::JmpZ(Operand::Tmp(t), to) if t == dst => (false, to),
            Op::JmpNz(Operand::Tmp(t), to) if t == dst => (true, to),
            _ => continue,
        };
        ops[at - 1] = if on_true == (un == UnKind::Bool) {
            Op::JmpNz(a, to)
        } else {
            Op::JmpZ(a, to)
        };
        ops[at] = Op::Nop;
        changed = true;
    }
    changed
}

/// Rewrites `ops` (the finished body whose places are `places`) in place.
pub(crate) fn specialise(ops: &mut [Op], places: &[Place], literals: &[Literal]) {
    for i in 0..ops.len() {
        if i + 2 < ops.len() && is_abs_call(&ops[i], literals) {
            if let Op::Send { src } = &ops[i + 1] {
                if let Op::DoCall { dst } = ops[i + 2] {
                    if let Some(src_op) = arg_operand(places, src) {
                        let to = dst.and_then(|d| assigned_cv(ops.get(i + 3), d));
                        ops[i] = Op::Abs {
                            src: src_op,
                            dst,
                            to,
                        };
                        ops[i + 1] = Op::Nop;
                        ops[i + 2] = Op::Nop;
                        continue;
                    }
                }
            }
        }
        let (head, tail) = ops.split_at_mut(i + 1);
        let Some(cur) = head.last_mut() else {
            continue;
        };
        if let Some(op) = rewrite(cur, tail.first(), places) {
            *cur = op;
        }
    }
    // Second pass, over the specialised ops: a loop's step right before its
    // smart-branch condition.
    for i in 1..ops.len() {
        if !matches!(ops[i], Op::CmpJmp { .. }) {
            continue;
        }
        if let Op::IncDecCv {
            cv,
            kind,
            dst: None,
            place,
        } = ops[i - 1]
        {
            ops[i - 1] = Op::IncDecCvCmpJmp { cv, kind, place };
        } else if let Op::AssignOpCv {
            cv,
            op,
            val,
            dst: None,
            place,
        } = ops[i - 1]
        {
            ops[i - 1] = Op::AssignOpCvCmpJmp { cv, op, val, place };
        } else if i >= 2 {
            if let Op::AssignCv {
                cv,
                val: Operand::Tmp(t) | Operand::TmpKeep(t),
                dst: None,
            } = ops[i - 1]
            {
                if let Op::BinAssignCv {
                    op: bop,
                    a,
                    b,
                    dst: bdst,
                    cv: bcv,
                } = ops[i - 2]
                {
                    if bcv == cv && bdst == t {
                        ops[i - 2] = Op::BinAssignCvCmpJmp {
                            op: bop,
                            a,
                            b,
                            dst: bdst,
                            cv,
                        };
                    }
                }
            }
        }
    }
    // Third pass: compound assignment or binary assignment followed by a branch
    // testing the assigned variable ($x = $a + $b; if ($x) / $x += 1; if ($x)).
    for i in 0..ops.len() {
        if i + 2 < ops.len() {
            if let Op::BinAssignCv { op, a, b, dst, cv } = ops[i] {
                if let Op::AssignCv {
                    cv: acv,
                    val: Operand::Tmp(t) | Operand::TmpKeep(t),
                    dst: None,
                } = ops[i + 1]
                {
                    if acv == cv && t == dst {
                        match ops[i + 2] {
                            Op::JmpZ(Operand::Cv(jcv), target) if jcv == cv => {
                                ops[i] = Op::BinAssignCvJmp {
                                    op,
                                    a,
                                    b,
                                    dst,
                                    cv,
                                    target,
                                    jmp_if: false,
                                };
                            }
                            Op::JmpNz(Operand::Cv(jcv), target) if jcv == cv => {
                                ops[i] = Op::BinAssignCvJmp {
                                    op,
                                    a,
                                    b,
                                    dst,
                                    cv,
                                    target,
                                    jmp_if: true,
                                };
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        if i + 1 < ops.len() {
            if let Op::AssignOpCv {
                cv,
                op,
                val,
                dst: None,
                place,
            } = ops[i]
            {
                match ops[i + 1] {
                    Op::JmpZ(Operand::Cv(jcv), target) if jcv == cv => {
                        ops[i] = Op::AssignOpCvJmp {
                            cv,
                            op,
                            val,
                            place,
                            target,
                            jmp_if: false,
                        };
                    }
                    Op::JmpNz(Operand::Cv(jcv), target) if jcv == cv => {
                        ops[i] = Op::AssignOpCvJmp {
                            cv,
                            op,
                            val,
                            place,
                            target,
                            jmp_if: true,
                        };
                    }
                    _ => {}
                }
            }
        }
    }
}

/// The specialised form of `op` (followed by `next`), if any.
fn rewrite(op: &Op, next: Option<&Op>, places: &[Place]) -> Option<Op> {
    match op {
        Op::Bin { op, a, b, dst } => bin(*op, *a, *b, *dst, next),
        Op::IncDec { place, kind, dst } => plain_cv(places, *place)
            .map(|cv| Op::IncDecCv {
                cv,
                kind: *kind,
                dst: *dst,
                place: *place,
            })
            .or_else(|| {
                cv_dim(places, *place).map(|(cv, dim)| Op::IncDecDimCv {
                    cv,
                    dim,
                    kind: *kind,
                    dst: *dst,
                    place: *place,
                })
            })
            .or_else(|| incdec_prop(places, *place, *kind, *dst)),
        Op::AssignOp {
            place,
            op,
            val,
            dst,
        } => assign_op(places, *place, *op, *val, *dst),
        Op::Fetch { place, mode, dst } if *mode != FetchMode::List => cv_dim(places, *place)
            .map(|(cv, dim)| Op::FetchDimCv {
                cv,
                dim,
                place: *place,
                dst: *dst,
                to: assigned_cv(next, *dst),
                mode: *mode,
            })
            .or_else(|| {
                cv_dims(places, *place).map(|cv| Op::FetchDimsCv {
                    cv,
                    place: *place,
                    dst: *dst,
                    to: assigned_cv(next, *dst),
                    mode: *mode,
                })
            })
            .or_else(|| {
                static_prop(places, *place).then(|| Op::FetchStaticProp {
                    place: *place,
                    dst: *dst,
                    to: assigned_cv(next, *dst),
                    mode: *mode,
                })
            })
            .or_else(|| {
                global_place(places, *place).then(|| Op::FetchGlobal {
                    place: *place,
                    dst: *dst,
                    to: assigned_cv(next, *dst),
                    mode: *mode,
                })
            })
            .or_else(|| fetch_prop(places, *place, *mode, *dst, next)),
        Op::FetchConst {
            name,
            fallback,
            dst,
        } => assigned_cv(next, *dst).map(|to| Op::FetchConstTo {
            name: *name,
            fallback: *fallback,
            dst: *dst,
            to,
        }),
        Op::FetchClassConst {
            class,
            name: Operand::Lit(name),
            dst,
        } => assigned_cv(next, *dst).map(|to| Op::FetchClassConstTo {
            class: class.clone(),
            name: *name,
            dst: *dst,
            to,
        }),
        Op::Isset { place, dst } => isset(places, *place, *dst, false),
        Op::Empty { place, dst } => isset(places, *place, *dst, true),
        Op::Assign { place, val, dst } => assign(places, *place, *val, *dst),
        _ => None,
    }
}

/// The specialised `Assign` of `place = val` (result `dst`).
fn assign(places: &[Place], place: PlaceId, val: Operand, dst: Option<u32>) -> Option<Op> {
    if static_prop(places, place) {
        return Some(Op::AssignStaticProp { place, val, dst });
    }
    if let Some(shape) = prop_shape(places, place) {
        return Some(match shape {
            PropShape::Prop => Op::AssignObjProp { place, val, dst },
            PropShape::Dim(dim) => Op::AssignObjPropDim {
                place,
                dim,
                val,
                dst,
            },
            // `ZEND_ASSIGN_DIM` with an unused offset operand appends.
            PropShape::Append => Op::AssignObjPropDim {
                place,
                dim: Operand::Unused,
                val,
                dst,
            },
        });
    }
    if dst.is_some() {
        return None;
    }
    if let Some((cv, dim)) = cv_dim(places, place) {
        return Some(Op::AssignDimCv {
            cv,
            dim,
            val,
            place,
        });
    }
    // `$cv[] = val;`: `ZEND_ASSIGN_DIM` with an unused offset appends.
    if let Some(cv) = cv_append(places, place) {
        return Some(Op::AssignDimCv {
            cv,
            dim: Operand::Unused,
            val,
            place,
        });
    }
    // `$cv[d1][d2]... = val;` (`mkmatrix`'s `$mx[$i][$j] = $count++`).
    short_cv_dims(places, place).map(|cv| Op::AssignDimsCv { cv, val, place })
}

/// The specialised `AssignOp` of `place op= val` (result `dst`).
fn assign_op(
    places: &[Place],
    place: PlaceId,
    op: BinaryOp,
    val: Operand,
    dst: Option<u32>,
) -> Option<Op> {
    if static_prop(places, place) {
        return Some(Op::AssignOpStaticProp {
            place,
            op,
            val,
            dst,
        });
    }
    // `$base->name op= v` / `$base->name[$k] op= v`: the operators the
    // property cache performs in place (anything else would only add a
    // failed check before the generic op).
    if op == BinaryOp::Concat || is_numeric_assign_op(op) {
        match prop_shape(places, place) {
            Some(PropShape::Prop) => {
                return Some(Op::AssignOpObjProp {
                    place,
                    op,
                    val,
                    dst,
                })
            }
            Some(PropShape::Dim(dim)) => {
                return Some(Op::AssignOpObjPropDim {
                    place,
                    dim,
                    op,
                    val,
                    dst,
                })
            }
            Some(PropShape::Append) | None => {}
        }
    }
    if op == BinaryOp::Concat {
        if let Some(cv) = plain_cv(places, place) {
            return Some(Op::ConcatCv {
                cv,
                val,
                dst,
                place,
            });
        }
    }
    if is_numeric_assign_op(op) {
        if let Some(cv) = plain_cv(places, place) {
            return Some(Op::AssignOpCv {
                cv,
                op,
                val,
                dst,
                place,
            });
        }
    }
    // `$cv[k] op= v` as a statement: a number operated on in place, or `.=`
    // extending a string element in place (`ZEND_ASSIGN_DIM_OP` on the
    // fetched slot).
    if dst.is_some() || !(is_numeric_assign_op(op) || op == BinaryOp::Concat) {
        return None;
    }
    if let Some((cv, dim)) = cv_dim(places, place) {
        return Some(Op::AssignOpDimCv {
            cv,
            dim,
            op,
            val,
            place,
        });
    }
    // `$cv[d1][d2]... op= v` on numbers.
    if !is_numeric_assign_op(op) {
        return None;
    }
    short_cv_dims(places, place).map(|cv| Op::AssignOpDimsCv { cv, op, val, place })
}

/// The deepest dimension chain the nested write fast paths take
/// (`exec/spec.rs`, `MAX_WRITE_DIMS`).
const MAX_WRITE_DIMS: usize = 4;

/// [`cv_dims`] with at most [`MAX_WRITE_DIMS`] dimensions.
fn short_cv_dims(places: &[Place], pid: PlaceId) -> Option<u32> {
    let cv = cv_dims(places, pid)?;
    (places.get(pid as usize)?.segs.len() <= MAX_WRITE_DIMS).then_some(cv)
}

/// The specialised `isset()` (`empty` false) or `empty()` of `pid`
/// writing `dst`.
fn isset(places: &[Place], pid: PlaceId, dst: u32, empty: bool) -> Option<Op> {
    if let Some((cv, dim)) = cv_dim(places, pid) {
        return Some(Op::IssetDimCv {
            cv,
            dim,
            place: pid,
            dst,
            empty,
        });
    }
    if static_prop(places, pid) {
        return Some(Op::IssetStaticProp {
            place: pid,
            dst,
            empty,
        });
    }
    match prop_shape(places, pid)? {
        PropShape::Prop => Some(Op::IssetObjProp {
            place: pid,
            dst,
            empty,
        }),
        PropShape::Dim(dim) => Some(Op::IssetObjPropDim {
            place: pid,
            dim,
            dst,
            empty,
        }),
        PropShape::Append => None,
    }
}

/// What follows the literal property name of a place the property ops
/// cover ([`prop_shape`]).
#[derive(Clone, Copy)]
enum PropShape {
    /// `$base->name`
    Prop,
    /// `$base->name[dim]`, a defined offset operand.
    Dim(Operand),
    /// `$base->name[]`
    Append,
}

/// The shape of place `pid` when it is one the property cache serves
/// (`place::propcache`): a compiled variable or `$this`, a property named
/// by a literal, and at most one dimension (or `[]`) after it.
fn prop_shape(places: &[Place], pid: PlaceId) -> Option<PropShape> {
    let p = places.get(pid as usize)?;
    if !matches!(p.base, PlaceBase::Cv(_) | PlaceBase::This) {
        return None;
    }
    match p.segs.as_slice() {
        [Seg::Prop(Operand::Lit(_))] => Some(PropShape::Prop),
        [Seg::Prop(Operand::Lit(_)), Seg::Dim(dim)] if *dim != Operand::Unused => {
            Some(PropShape::Dim(*dim))
        }
        [Seg::Prop(Operand::Lit(_)), Seg::Append] => Some(PropShape::Append),
        _ => None,
    }
}

/// The property op of a `Fetch` (`Read` or `Is` mode) of `pid` writing
/// `dst`, followed by `next` (whose `AssignCv` of `dst` it may fold).
fn fetch_prop(
    places: &[Place],
    pid: PlaceId,
    mode: FetchMode,
    dst: u32,
    next: Option<&Op>,
) -> Option<Op> {
    let to = assigned_cv(next, dst);
    match prop_shape(places, pid)? {
        PropShape::Prop => Some(Op::FetchObjProp {
            place: pid,
            dst,
            to,
            mode,
        }),
        PropShape::Dim(dim) => Some(Op::FetchObjPropDim {
            place: pid,
            dim,
            dst,
            to,
            mode,
        }),
        // "Cannot use [] for reading".
        PropShape::Append => None,
    }
}

/// The property op of an `IncDec` of `pid`.
fn incdec_prop(places: &[Place], pid: PlaceId, kind: IncDecKind, dst: Option<u32>) -> Option<Op> {
    match prop_shape(places, pid)? {
        PropShape::Prop => Some(Op::IncDecObjProp {
            place: pid,
            kind,
            dst,
        }),
        PropShape::Dim(dim) => Some(Op::IncDecObjPropDim {
            place: pid,
            dim,
            kind,
            dst,
        }),
        PropShape::Append => None,
    }
}

/// Whether `pid` is a static property `A::$name` the run-time cache can
/// hold: a literal name, a class that is named or `self`/`parent`/
/// `static` (not an expression), no further segments.
fn static_prop(places: &[Place], pid: PlaceId) -> bool {
    places.get(pid as usize).is_some_and(|p| {
        p.segs.is_empty()
            && matches!(
                &p.base,
                PlaceBase::StaticProp {
                    class: ClassSpec::Named(_)
                        | ClassSpec::SelfClass
                        | ClassSpec::Parent
                        | ClassSpec::Static,
                    name: Operand::Lit(_),
                }
            )
    })
}

/// `zend_is_smart_branch`: the comparisons whose result can drive the
/// following jump directly.
fn is_smart_branch(op: BinKind) -> bool {
    matches!(
        op,
        BinKind::Equal
            | BinKind::NotEqual
            | BinKind::Identical
            | BinKind::NotIdentical
            | BinKind::Less
            | BinKind::LessEqual
            | BinKind::Spaceship
    )
}

/// The operators the sealed numeric fast path (`Vm::bin_num_fast`)
/// computes: rewriting any other `Bin` would only add a failed check.
fn has_numeric_fast_path(op: BinKind) -> bool {
    is_smart_branch(op)
        || matches!(
            op,
            BinKind::Spaceship
                | BinKind::Arith(
                    BinaryOp::Add
                        | BinaryOp::Sub
                        | BinaryOp::Mul
                        | BinaryOp::Div
                        | BinaryOp::Mod
                        | BinaryOp::ShiftLeft
                        | BinaryOp::ShiftRight
                        | BinaryOp::BitOr
                        | BinaryOp::BitAnd
                        | BinaryOp::BitXor
                )
        )
}

/// The compound assignments `Vm::bin_num_fast` computes on numbers.
fn is_numeric_assign_op(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Mod
            | BinaryOp::ShiftLeft
            | BinaryOp::ShiftRight
            | BinaryOp::BitOr
            | BinaryOp::BitAnd
            | BinaryOp::BitXor
    )
}

/// A `Bin` writing `dst`, followed by `next`.
fn bin(op: BinKind, a: Operand, b: Operand, dst: u32, next: Option<&Op>) -> Option<Op> {
    if !has_numeric_fast_path(op) {
        return None;
    }
    let branch = match next? {
        Op::JmpZ(Operand::Tmp(t) | Operand::TmpKeep(t), target) if *t == dst => {
            Some((*target, false))
        }
        Op::JmpNz(Operand::Tmp(t) | Operand::TmpKeep(t), target) if *t == dst => {
            Some((*target, true))
        }
        _ => None,
    };
    if let Some((target, jmp_if)) = branch {
        // `for_branch` is `None` for the operators that are not smart
        // branches (`zend_is_smart_branch`).
        if let Some(mask) = cmp_outcome::for_branch(op, jmp_if) {
            return Some(Op::CmpJmp {
                op,
                a,
                b,
                dst,
                target,
                jmp_if,
                mask,
            });
        }
    }
    assigned_cv(next, dst).map(|cv| Op::BinAssignCv { op, a, b, dst, cv })
}

/// The CV that `next` assigns temporary `dst` to, as a statement
/// (`$cv = <tmp>;`, result unused).
fn assigned_cv(next: Option<&Op>, dst: u32) -> Option<u32> {
    match next? {
        Op::AssignCv {
            cv,
            val: Operand::Tmp(t) | Operand::TmpKeep(t),
            dst: None,
        } if *t == dst => Some(*cv),
        _ => None,
    }
}

/// php-src's `ZEND_FRAMELESS_ICALL_n` (`zend_compile_frameless_icall`),
/// after [`specialise`]: the call of a builtin with a frameless class
/// ([`crate::special::frameless_flags`]) by a name without a namespace
/// fallback, whose 1..=3 positional arguments are literals or plain
/// compiled variables sent right after its `InitFCall`, all on one line,
/// becomes one [`Op::FramelessICall`]; its `Send`s and `DoCall` become
/// `Nop`s (the op count, jump targets and lines stay). A namespaced
/// unqualified name keeps its call ops (php-src's `ZEND_JMP_FRAMELESS`
/// branch). The op runs the call in place when it can, and the ops it
/// replaced otherwise, at the same line; the opcache dump expands it back
/// ([`unfuse_frameless_calls`]).
///
/// A call whose arguments are computed between its `InitFCall` and its
/// `DoCall` (`max($i & 3, 1)`, `ord(substr($s, $i, 1))`) is fused as well
/// ([`spread_frameless_call_at`]): the op then stands at the `DoCall`,
/// after the ops that compute the arguments, which is where php-src emits
/// its `ZEND_FRAMELESS_ICALL_n` (the arguments are compiled as expressions
/// first).
pub(crate) fn fuse_frameless_calls(oa: &mut OpArray) {
    let mut i = 0;
    while i < oa.ops.len() {
        if let Some((fc, end)) = frameless_call_at(oa, i) {
            oa.ops[i] = Op::FramelessICall(Box::new(fc));
            for op in &mut oa.ops[i + 1..=end] {
                *op = Op::Nop;
            }
            i = end + 1;
        } else {
            if let Some((fc, end)) = spread_frameless_call_at(oa, i) {
                let n = usize::from(fc.argc);
                for back in &fc.send_back[..n] {
                    oa.ops[end - *back as usize] = Op::Nop;
                }
                oa.ops[i] = Op::Nop;
                oa.ops[end] = Op::FramelessICall(Box::new(fc));
            }
            // The ops after the `InitFCall` are scanned too: a call among
            // the arguments is fused on its own.
            i += 1;
        }
    }
}

/// Whether `op` may stand between the `InitFCall` and the `DoCall` of a
/// call fused by [`spread_frameless_call_at`]: the ops of an argument
/// expression that only read (operators, casts, element, property and
/// constant reads, tests, the jumps of `?:`, `??`, `&&` and a ternary) and
/// the ops of another call (counted by the scan). Anything that writes a
/// variable, leaves the function or changes how errors are reported keeps
/// the call's own ops.
fn is_argument_op(op: &Op) -> bool {
    matches!(
        op,
        Op::Nop
            | Op::Move { .. }
            | Op::Bin { .. }
            | Op::Unary { .. }
            | Op::Cast { .. }
            | Op::Rope { .. }
            | Op::InitArray { .. }
            | Op::ArrayAdd { .. }
            | Op::Jmp(_)
            | Op::JmpZ(..)
            | Op::JmpNz(..)
            | Op::JmpZEx { .. }
            | Op::JmpNzEx { .. }
            | Op::JmpSet { .. }
            | Op::Coalesce { .. }
            | Op::CmpJmp { .. }
            | Op::Fetch {
                mode: FetchMode::Read | FetchMode::Is,
                ..
            }
            // The reads below: not the forms folded with the assignment
            // after them (`to`), which write a variable.
            | Op::FetchDimCv {
                to: None,
                mode: FetchMode::Read | FetchMode::Is,
                ..
            }
            | Op::FetchDimsCv {
                to: None,
                mode: FetchMode::Read | FetchMode::Is,
                ..
            }
            | Op::FetchObjProp {
                to: None,
                mode: FetchMode::Read | FetchMode::Is,
                ..
            }
            | Op::FetchStaticProp {
                to: None,
                mode: FetchMode::Read | FetchMode::Is,
                ..
            }
            | Op::FetchGlobal {
                to: None,
                mode: FetchMode::Read | FetchMode::Is,
                ..
            }
            | Op::FetchConst { .. }
            | Op::FetchClassConst { .. }
            | Op::Isset { .. }
            | Op::Empty { .. }
            | Op::IssetDimCv { .. }
            | Op::IssetObjProp { .. }
            | Op::Strlen { .. }
            | Op::Count { .. }
            | Op::TypeCheck { .. }
            | Op::Abs { to: None, .. }
            | Op::FramelessICall(_)
            | Op::InstanceOf { .. }
            | Op::FetchThis { .. }
    )
}

/// The jump target of `op`, if it jumps.
fn jump_target_of(op: &Op) -> Option<u32> {
    match op {
        Op::Jmp(t) | Op::JmpZ(_, t) | Op::JmpNz(_, t) => Some(*t),
        Op::JmpZEx { target, .. }
        | Op::JmpNzEx { target, .. }
        | Op::JmpSet { target, .. }
        | Op::Coalesce { target, .. }
        | Op::CmpJmp { target, .. } => Some(*target),
        _ => None,
    }
}

/// The [`Op::FramelessICall`] of the call starting at `ops[i]` when its
/// arguments are computed between its `InitFCall` and its `DoCall`, and the
/// index of that `DoCall`, where the op goes ([`fuse_frameless_calls`]).
///
/// The arguments are literals, plain compiled variables and temporaries
/// (an expression's or another call's result); every op in between is one
/// of an argument expression ([`is_argument_op`]) or belongs to a call
/// among the arguments, and no jump leaves the span. The `Send`s and the
/// `DoCall` are on one line (what they report carries it).
///
/// The fused op reads a compiled variable when it runs, after the other
/// arguments were computed. php-src does the same for its own frameless
/// functions (their arguments are compiled as expressions before the one
/// `ZEND_FRAMELESS_ICALL_n`), so for those any order of arguments is
/// fused ([`crate::special::defers_like_php_src`]). For the other
/// functions with a frameless class here (`abs()`, `ord()`, `sqrt()`, ...)
/// php-src sends each argument where it stands, so the call is fused only
/// when no op follows the `Send` of a variable: reading it later is then
/// reading it at the same point.
#[allow(clippy::too_many_lines)] // one scan of the call's ops, then its checks in order
fn spread_frameless_call_at(oa: &OpArray, i: usize) -> Option<(FramelessCall, usize)> {
    let Op::InitFCall {
        name,
        fallback: None,
        display,
    } = oa.ops[i]
    else {
        return None;
    };
    let Some(Value::Str(lc)) = oa.literals.get(name as usize) else {
        return None;
    };
    if crate::special::frameless_flags(lc.as_bytes()) == 0
        || super::optimizer::is_disabled_function(lc.as_bytes())
    {
        return None;
    }
    let unused = || ArgSrc::Val(Operand::Unused);
    let mut args = [unused(), unused(), unused()];
    let mut vals = [Operand::Unused; 3];
    let mut sent_at = [0usize; 3];
    let mut argc = 0usize;
    // Calls started and not finished among the arguments.
    let mut depth = 0usize;
    // A variable was sent, and an op came after that `Send`.
    let mut cv_sent = false;
    let mut read_late = false;
    let mut j = i + 1;
    let end = loop {
        let op = oa.ops.get(j)?;
        match op {
            Op::Send { src } if depth == 0 => {
                if argc == 3 {
                    return None;
                }
                read_late |= cv_sent;
                vals[argc] = match src {
                    ArgSrc::Val(Operand::Lit(l)) => Operand::Lit(*l),
                    ArgSrc::Val(Operand::Cv(cv)) => Operand::Cv(*cv),
                    ArgSrc::Place(pid) => Operand::Cv(plain_cv(&oa.places, *pid)?),
                    ArgSrc::Val(Operand::Tmp(t)) | ArgSrc::CallResult(Operand::Tmp(t)) => {
                        Operand::Tmp(*t)
                    }
                    _ => return None,
                };
                cv_sent |= matches!(vals[argc], Operand::Cv(_));
                args[argc] = src.clone();
                sent_at[argc] = j;
                argc += 1;
            }
            Op::DoCall { .. } if depth == 0 => break j,
            Op::Send { .. } => read_late |= cv_sent,
            Op::DoCall { .. } => {
                depth -= 1;
                read_late |= cv_sent;
            }
            Op::InitFCall { .. }
            | Op::InitMethodCall { .. }
            | Op::InitStaticCall { .. }
            | Op::New { .. } => {
                depth += 1;
                read_late |= cv_sent;
            }
            op if is_argument_op(op) => read_late |= cv_sent && !matches!(op, Op::Nop),
            _ => return None,
        }
        j += 1;
    };
    let Op::DoCall { dst } = oa.ops[end] else {
        return None;
    };
    if argc == 0 || (read_late && !crate::special::defers_like_php_src(lc.as_bytes(), argc)) {
        return None;
    }
    // What the `Send`s and the `DoCall` report is on one line.
    let line = oa.lines.get(end).copied()?;
    if sent_at[..argc]
        .iter()
        .any(|&s| oa.lines.get(s).copied() != Some(line))
    {
        return None;
    }
    // No jump leaves the span, and a `New` among the arguments skips its
    // constructor call inside it.
    let inside = |t: u32| (i + 1..=end).contains(&(t as usize));
    for op in &oa.ops[i + 1..end] {
        let target = match op {
            Op::New { skip, .. } if *skip != 0 => Some(*skip),
            op => jump_target_of(op),
        };
        if target.is_some_and(|t| !inside(t)) {
            return None;
        }
    }
    let back = |from: usize| u32::try_from(end - from).ok();
    let fc = FramelessCall {
        name,
        display,
        args,
        vals,
        argc: u8::try_from(argc).ok()?,
        dst,
        init_back: back(i)?,
        send_back: [
            back(sent_at[0].max(i))?,
            back(sent_at[1].max(i))?,
            back(sent_at[2].max(i))?,
        ],
    };
    Some((fc, end))
}

/// The [`Op::FramelessICall`] of the call starting at `ops[i]` and the
/// index of its `DoCall` ([`fuse_frameless_calls`]).
fn frameless_call_at(oa: &OpArray, i: usize) -> Option<(FramelessCall, usize)> {
    let Op::InitFCall {
        name,
        fallback: None,
        display,
    } = oa.ops[i]
    else {
        return None;
    };
    let Some(Value::Str(lc)) = oa.literals.get(name as usize) else {
        return None;
    };
    if crate::special::frameless_flags(lc.as_bytes()) == 0 {
        return None;
    }
    let line = oa.lines.get(i).copied()?;
    let unused = || ArgSrc::Val(Operand::Unused);
    let mut args = [unused(), unused(), unused()];
    let mut vals = [Operand::Unused; 3];
    let mut argc = 0usize;
    let mut j = i + 1;
    loop {
        if oa.lines.get(j).copied() != Some(line) {
            return None;
        }
        match oa.ops.get(j)? {
            Op::Send { src } if argc < 3 => {
                vals[argc] = match src {
                    ArgSrc::Val(Operand::Lit(l)) => Operand::Lit(*l),
                    ArgSrc::Val(Operand::Cv(cv)) => Operand::Cv(*cv),
                    ArgSrc::Place(pid) => Operand::Cv(plain_cv(&oa.places, *pid)?),
                    _ => return None,
                };
                args[argc] = src.clone();
                argc += 1;
            }
            Op::DoCall { dst } if argc > 0 => {
                let fc = FramelessCall {
                    name,
                    display,
                    args,
                    vals,
                    argc: u8::try_from(argc).ok()?,
                    dst: *dst,
                    init_back: 0,
                    send_back: [0; 3],
                };
                return Some((fc, j));
            }
            _ => return None,
        }
        j += 1;
    }
}

/// `ops` with each [`Op::FramelessICall`] expanded back into the
/// `InitFCall`, `Send`s and `DoCall` it replaced (in the `Nop`s after
/// it): the ops as compiled, for the opcache dump, which prints
/// php-src's `FRAMELESS_ICALL_n` (or the call ops) from them. Borrowed
/// when there is nothing to expand.
pub(crate) fn unfuse_frameless_calls(ops: &[Op]) -> std::borrow::Cow<'_, [Op]> {
    if !ops.iter().any(|op| matches!(op, Op::FramelessICall(_))) {
        return std::borrow::Cow::Borrowed(ops);
    }
    let mut out = ops.to_vec();
    for (i, op) in ops.iter().enumerate() {
        let Op::FramelessICall(fc) = op else {
            continue;
        };
        let n = usize::from(fc.argc);
        let init = Op::InitFCall {
            name: fc.name,
            fallback: None,
            display: fc.display,
        };
        if fc.init_back != 0 {
            // The op stands at the `DoCall`; the others where they stood.
            let Some(start) = i.checked_sub(fc.init_back as usize) else {
                continue;
            };
            out[start] = init;
            for (src, back) in fc.args[..n].iter().zip(&fc.send_back) {
                if let Some(at) = i.checked_sub(*back as usize) {
                    out[at] = Op::Send { src: src.clone() };
                }
            }
            out[i] = Op::DoCall { dst: fc.dst };
            continue;
        }
        if i + n + 1 >= out.len() {
            continue;
        }
        out[i] = init;
        for (k, src) in fc.args[..n].iter().enumerate() {
            out[i + 1 + k] = Op::Send { src: src.clone() };
        }
        out[i + 1 + n] = Op::DoCall { dst: fc.dst };
    }
    std::borrow::Cow::Owned(out)
}

/// The CV of a place that is a plain compiled variable (`$cv`).
fn plain_cv(places: &[Place], pid: PlaceId) -> Option<u32> {
    let p = places.get(pid as usize)?;
    match (&p.base, p.segs.as_slice()) {
        (PlaceBase::Cv(cv), []) => Some(*cv),
        _ => None,
    }
}

/// The CV and offset operand of a place `$cv[dim]` (one dimension, a
/// defined offset operand).
fn cv_dim(places: &[Place], pid: PlaceId) -> Option<(u32, Operand)> {
    let p = places.get(pid as usize)?;
    match (&p.base, p.segs.as_slice()) {
        (PlaceBase::Cv(cv), [Seg::Dim(dim)]) if !matches!(dim, Operand::Unused) => {
            Some((*cv, *dim))
        }
        _ => None,
    }
}

/// The CV of a place `$cv[]` (an append).
fn cv_append(places: &[Place], pid: PlaceId) -> Option<u32> {
    let p = places.get(pid as usize)?;
    match (&p.base, p.segs.as_slice()) {
        (PlaceBase::Cv(cv), [Seg::Append]) => Some(*cv),
        _ => None,
    }
}

/// The CV of a place `$cv[d1][d2]...` with two or more dimensions, all
/// with defined offset operands.
fn cv_dims(places: &[Place], pid: PlaceId) -> Option<u32> {
    let p = places.get(pid as usize)?;
    let PlaceBase::Cv(cv) = &p.base else {
        return None;
    };
    let all_dims = p
        .segs
        .iter()
        .all(|s| matches!(s, Seg::Dim(d) if *d != Operand::Unused));
    (p.segs.len() >= 2 && all_dims).then_some(*cv)
}

/// Whether `op` is a direct `InitFCall` to the global `abs()` function.
fn is_abs_call(op: &Op, literals: &[Literal]) -> bool {
    let Op::InitFCall { name, fallback, .. } = op else {
        return false;
    };
    if fallback.is_some() {
        return false;
    }
    match literals.get(*name as usize) {
        // `abs` is the internal function unless `disable_functions`
        // removed it, which lets user code declare its own `abs()`.
        Some(Value::Str(s)) => {
            s.as_bytes().eq_ignore_ascii_case(b"abs")
                && !super::optimizer::is_disabled_function(b"abs")
        }
        _ => false,
    }
}

/// The operand of an argument source if it is a plain compiled variable or value.
fn arg_operand(places: &[Place], src: &ArgSrc) -> Option<Operand> {
    match src {
        ArgSrc::Place(pid) | ArgSrc::TempPlace(pid) => plain_cv(places, *pid).map(Operand::Cv),
        ArgSrc::Val(op) | ArgSrc::CallResult(op) => Some(*op),
    }
}

/// Whether a place is a direct global variable access (`$GLOBALS['name']` or superglobal)
/// that can be specialised to [`Op::FetchGlobal`].
fn global_place(places: &[Place], pid: PlaceId) -> bool {
    let Some(p) = places.get(pid as usize) else {
        return false;
    };
    match (&p.base, p.segs.as_slice()) {
        (PlaceBase::Global(_), []) => true,
        (PlaceBase::GlobalsArray, [Seg::Dim(_)]) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{compile, CompileOptions};
    use crate::opcode::{cmp_outcome, BinKind, FetchMode, IncDecKind, Op, Operand, UnKind};
    use php_runtime::BinaryOp;
    use std::rc::Rc;

    fn main_ops(src: &str) -> Vec<Op> {
        let program =
            php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
        let opts = CompileOptions {
            filename: "t.php".into(),
            ..CompileOptions::default()
        };
        compile(&program, Rc::from(src.as_bytes()), opts)
            .expect("compile")
            .main
            .code
            .ops
            .clone()
    }

    fn hooked_ops(src: &str) -> Vec<Op> {
        let program =
            php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
        let opts = CompileOptions {
            filename: "t.php".into(),
            user_opcodes: vec!["ZEND_ADD"],
            ..CompileOptions::default()
        };
        compile(&program, Rc::from(src.as_bytes()), opts)
            .expect("compile")
            .main
            .code
            .ops
            .clone()
    }

    /// `ZEND_FRAMELESS_ICALL_n`: a call of a builtin with a frameless
    /// class by a global name, with literal and variable arguments on its
    /// line, is one op followed by the `Nop`s of its `Send`s and `DoCall`;
    /// other calls keep their ops, and the dump's expansion gives the
    /// call ops back.
    #[test]
    fn frameless_calls_are_fused_into_one_op() {
        let ops = main_ops("<?php $s = 'abc'; $p = strpos($s, 'b'); echo substr($s, 1, 1);");
        let fused: Vec<usize> = ops
            .iter()
            .enumerate()
            .filter(|(_, op)| matches!(op, Op::FramelessICall(_)))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(fused.len(), 2, "{ops:?}");
        for &i in &fused {
            let Op::FramelessICall(fc) = &ops[i] else {
                unreachable!()
            };
            let n = usize::from(fc.argc);
            assert!(ops[i + 1..=i + n + 1]
                .iter()
                .all(|op| matches!(op, Op::Nop)));
        }
        let Op::FramelessICall(fc) = &ops[fused[0]] else {
            unreachable!()
        };
        assert_eq!(fc.argc, 2);
        assert!(matches!(fc.vals[0], Operand::Cv(_)));
        assert!(matches!(fc.vals[1], Operand::Lit(_)));
        let Op::FramelessICall(fc) = &ops[fused[1]] else {
            unreachable!()
        };
        assert_eq!(fc.argc, 3);
        let back = super::unfuse_frameless_calls(&ops);
        assert!(!back.iter().any(|op| matches!(op, Op::FramelessICall(_))));
        let i = fused[0];
        assert!(matches!(back[i], Op::InitFCall { fallback: None, .. }));
        assert!(matches!(back[i + 1], Op::Send { .. }));
        assert!(matches!(back[i + 2], Op::Send { .. }));
        assert!(matches!(back[i + 3], Op::DoCall { .. }));
        assert_eq!(back.len(), ops.len());
        for src in [
            // A namespaced unqualified name (`ZEND_JMP_FRAMELESS` in php-src).
            "<?php namespace N; $s = 'a'; strpos($s, 'a');",
            // A variable sent before another argument is computed, to a
            // function php-src calls with `SEND`s (the variable is copied
            // where it stands).
            "<?php $x = 7; $i = 3; intdiv($x, $i - 2);",
            // An argument that assigns.
            "<?php $s = 'a'; strpos($s, $t = 'a');",
            // Arguments on other lines.
            "<?php $s = 'a'; strpos(\n$s,\n'a');",
            // Not a frameless function, a named argument, no argument.
            "<?php $s = 'a'; str_repeat($s, 2); strpos(haystack: $s, needle: 'a'); pi();",
            // A dimension.
            "<?php $a = ['x']; strpos($a[0], 'x');",
        ] {
            let ops = main_ops(src);
            assert!(
                !ops.iter().any(|op| matches!(op, Op::FramelessICall(_))),
                "{src}: {ops:?}"
            );
        }
        // Hooked opcodes keep the generic ops.
        let ops = hooked_ops("<?php $s = 'a'; strpos($s, 'a');");
        assert!(!ops.iter().any(|op| matches!(op, Op::FramelessICall(_))));
    }

    /// A call whose arguments are computed between its `InitFCall` and its
    /// `DoCall` is one op at the `DoCall`, the `InitFCall` and the `Send`s
    /// `Nop`s where they stood, also for a call among the arguments of
    /// another; the dump's expansion puts every op back.
    #[test]
    fn frameless_calls_with_computed_arguments_are_fused_at_their_call() {
        let ops = main_ops(
            "<?php $s = 'abc'; $i = 3; echo strpos($s . 'b', 'a'), max($i & 3, 1), ord(substr($s, $i % 3, 1));",
        );
        let mut fused = 0;
        let mut from_tmp = 0;
        for (i, op) in ops.iter().enumerate() {
            let Op::FramelessICall(fc) = op else {
                continue;
            };
            fused += 1;
            assert_ne!(fc.init_back, 0, "{ops:?}");
            assert_eq!(fc.tail(), 0);
            assert!(matches!(ops[i - fc.init_back as usize], Op::Nop));
            for back in &fc.send_back[..usize::from(fc.argc)] {
                assert!(*back != 0 && *back < fc.init_back);
                assert!(matches!(ops[i - *back as usize], Op::Nop));
            }
            from_tmp += fc
                .vals
                .iter()
                .filter(|v| matches!(v, Operand::Tmp(_)))
                .count();
        }
        // `strpos`, `max`, `ord` and the `substr` among `ord`'s arguments.
        assert_eq!(fused, 4, "{ops:?}");
        assert_eq!(from_tmp, 4, "{ops:?}");
        let back = super::unfuse_frameless_calls(&ops);
        assert_eq!(back.len(), ops.len());
        let count = |f: fn(&Op) -> bool| back.iter().filter(|op| f(op)).count();
        assert_eq!(count(|op| matches!(op, Op::FramelessICall(_))), 0);
        assert_eq!(count(|op| matches!(op, Op::InitFCall { .. })), 4);
        assert_eq!(count(|op| matches!(op, Op::DoCall { .. })), 4);
        assert_eq!(count(|op| matches!(op, Op::Send { .. })), 8);
    }

    #[test]
    fn for_loop_is_rotated_with_a_smart_branch() {
        // zend_compile_for: JMP cond; body; step; cond; JMPNZ body.
        let ops = main_ops("<?php for ($i = 0; $i < 10; $i++) { echo $i; }");
        let jmp = ops
            .iter()
            .position(|op| matches!(op, Op::Jmp(_)))
            .expect("entry jump");
        let Op::Jmp(cond) = ops[jmp] else {
            unreachable!()
        };
        let cond = cond as usize;
        assert!(cond > jmp, "{ops:?}");
        let Op::CmpJmp {
            op: BinKind::Less,
            a: Operand::Cv(_),
            b: Operand::Lit(_),
            target,
            jmp_if: true,
            dst,
            mask,
        } = ops[cond]
        else {
            panic!("no smart branch at {cond}: {ops:?}");
        };
        assert_eq!(target as usize, jmp + 1, "{ops:?}");
        assert_eq!(mask, cmp_outcome::LESS, "{ops:?}");
        // The jump the smart branch stands for is kept after it.
        assert!(
            matches!(ops[cond + 1], Op::JmpNz(Operand::Tmp(t), l) if t == dst && l == target),
            "{ops:?}"
        );
        // The step right before the condition carries it along.
        assert!(
            matches!(
                ops[cond - 1],
                Op::IncDecCvCmpJmp {
                    kind: IncDecKind::PostInc,
                    ..
                }
            ),
            "{ops:?}"
        );
    }

    #[test]
    fn while_loop_is_rotated() {
        let ops = main_ops("<?php $i = 0; while ($i < 3) { $i++; } echo $i;");
        let Some(Op::Jmp(cond)) = ops.iter().find(|op| matches!(op, Op::Jmp(_))) else {
            panic!("no entry jump: {ops:?}");
        };
        assert!(
            matches!(
                ops[*cond as usize],
                Op::CmpJmp {
                    op: BinKind::Less,
                    jmp_if: true,
                    ..
                }
            ),
            "{ops:?}"
        );
    }

    #[test]
    fn last_if_branch_has_no_exit_jump() {
        // zend_compile_if: "Don't generate a jump for the last branch".
        let ops = main_ops("<?php $i = 1; if ($i < 10) { $s = 1; }");
        assert!(!ops.iter().any(|op| matches!(op, Op::Jmp(_))), "{ops:?}");
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::CmpJmp {
                    op: BinKind::Less,
                    jmp_if: false,
                    ..
                }
            )),
            "{ops:?}"
        );
        // With an else branch the first branch still jumps over it.
        let ops = main_ops("<?php $i = 1; if ($i < 10) { $s = 1; } else { $s = 2; }");
        assert_eq!(
            ops.iter().filter(|op| matches!(op, Op::Jmp(_))).count(),
            1,
            "{ops:?}"
        );
    }

    #[test]
    fn arithmetic_assigned_to_a_cv_is_fused() {
        let ops = main_ops("<?php $x = 1; $i = 2; $x = $x + $i;");
        let at = ops
            .iter()
            .position(|op| matches!(op, Op::BinAssignCv { .. }))
            .expect("fused add");
        let Op::BinAssignCv { dst, cv, .. } = ops[at] else {
            unreachable!()
        };
        assert!(
            matches!(ops[at + 1], Op::AssignCv { cv: c, val: Operand::Tmp(t), dst: None } if c == cv && t == dst),
            "{ops:?}"
        );
        // A used result is not fused.
        let ops = main_ops("<?php $x = 1; echo $y = $x + 1;");
        assert!(
            !ops.iter().any(|op| matches!(op, Op::BinAssignCv { .. })),
            "{ops:?}"
        );
        // Concatenation has no numeric fast path.
        let ops = main_ops("<?php $x = 'a'; $x = $x . 'b';");
        assert!(
            !ops.iter().any(|op| matches!(op, Op::BinAssignCv { .. })),
            "{ops:?}"
        );
    }

    #[test]
    fn cv_dimensions_are_specialised() {
        let ops = main_ops("<?php $a = [1, 2]; $i = 1; $s = $a[$i & 1]; $a[$i] = 3;");
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::FetchDimCv {
                    to: Some(_),
                    mode: FetchMode::Read,
                    ..
                }
            )),
            "{ops:?}"
        );
        assert!(
            ops.iter().any(|op| matches!(op, Op::AssignDimCv { .. })),
            "{ops:?}"
        );
        let ops = main_ops(
            "<?php $a = []; $k = 'k'; $v = $a[$k] ?? 1; $i = isset($a[$k]); $e = empty($a[0]);",
        );
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::FetchDimCv {
                    to: None,
                    mode: FetchMode::Is,
                    ..
                }
            )),
            "{ops:?}"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, Op::IssetDimCv { empty: false, .. })),
            "{ops:?}"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, Op::IssetDimCv { empty: true, .. })),
            "{ops:?}"
        );
        // list() element fetches keep the generic op.
        let ops = main_ops("<?php $a = [1]; [$x] = $a;");
        assert!(
            !ops.iter().any(|op| matches!(op, Op::FetchDimCv { .. })),
            "{ops:?}"
        );
    }

    #[test]
    fn cv_dimension_updates_and_chains_are_specialised() {
        let ops = main_ops("<?php $s = 0; $s += 2; $s .= 'x'; $a = [1]; $a[0] += 1; $a[0]++; $a[0] .= 'x'; $y = $a[0] += 1;");
        assert_eq!(
            ops.iter()
                .filter(|op| matches!(op, Op::AssignOpCv { .. }))
                .count(),
            1,
            "{ops:?}"
        );
        // `$a[0] += 1` and `$a[0] .= 'x'` (the string element is extended
        // in place).
        assert_eq!(
            ops.iter()
                .filter(|op| matches!(op, Op::AssignOpDimCv { .. }))
                .count(),
            2,
            "{ops:?}"
        );
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::IncDecDimCv {
                    kind: IncDecKind::PostInc,
                    dst: None,
                    ..
                }
            )),
            "{ops:?}"
        );
        // `.=` on the CV is `ConcatCv`; a compound assignment of an element
        // with a used result keeps the generic op.
        assert!(
            ops.iter()
                .any(|op| matches!(op, Op::ConcatCv { dst: None, .. })),
            "{ops:?}"
        );
        assert_eq!(
            ops.iter()
                .filter(|op| matches!(op, Op::AssignOp { .. }))
                .count(),
            1,
            "{ops:?}"
        );
        // Nested dimension reads and writes get the chain forms; an append
        // `$a[] = v;` is `AssignDimCv` without an offset.
        let ops = main_ops(
            "<?php $a = [[1]]; echo $a[0][0]; $a[] = 1; $a[0][0] = 2; $x = $a[0][0]; $y = $a[0][0] ?? 1;",
        );
        assert!(
            !ops.iter().any(|op| matches!(op, Op::FetchDimCv { .. })
                || matches!(op, Op::AssignDimCv { dim, .. } if *dim != crate::opcode::Operand::Unused)),
            "{ops:?}"
        );
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::AssignDimCv {
                    dim: crate::opcode::Operand::Unused,
                    ..
                }
            )),
            "{ops:?}"
        );
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::FetchDimsCv {
                    to: None,
                    mode: FetchMode::Read,
                    ..
                }
            )),
            "{ops:?}"
        );
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::FetchDimsCv {
                    to: Some(_),
                    mode: FetchMode::Read,
                    ..
                }
            )),
            "{ops:?}"
        );
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::FetchDimsCv {
                    mode: FetchMode::Is,
                    ..
                }
            )),
            "{ops:?}"
        );
        // `$a[] = 3` is an `AssignDimCv` with no offset too, so no generic
        // `Assign` is left.
        assert_eq!(
            ops.iter()
                .filter(|op| matches!(op, Op::Assign { .. }))
                .count(),
            0,
            "{ops:?}"
        );
        assert!(
            ops.iter().any(|op| matches!(op, Op::AssignDimsCv { .. })),
            "{ops:?}"
        );
        // Nested compound assignments: numeric ones get the chain form; `.=`,
        // a used result and more than four dimensions keep the generic ops.
        let ops = main_ops(
            "<?php $m = [[1]]; $m[0][0] += 2; $m[0][0] .= 'x'; $y = ($m[0][0] -= 1); $m[0][0][0][0][0] = 1; $m[0][0][0][0][0] *= 2;",
        );
        assert_eq!(
            ops.iter()
                .filter(|op| matches!(
                    op,
                    Op::AssignOpDimsCv {
                        op: BinaryOp::Add,
                        ..
                    }
                ))
                .count(),
            1,
            "{ops:?}"
        );
        assert_eq!(
            ops.iter()
                .filter(|op| matches!(op, Op::AssignOp { .. }))
                .count(),
            3,
            "{ops:?}"
        );
        assert!(
            !ops.iter().any(|op| matches!(op, Op::AssignDimsCv { .. })),
            "{ops:?}"
        );
    }

    #[test]
    fn static_properties_are_specialised() {
        let ops = main_ops(
            "<?php class A { static $a = 0; } $x = A::$a; A::$a = 1; A::$a .= 'x'; $y = isset(A::$a); $z = empty(A::$a); $w = A::$a ?? 2; $d = 'a'; $v = A::$$d; A::$a[] = 1;",
        );
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::FetchStaticProp {
                    to: Some(_),
                    mode: FetchMode::Read,
                    ..
                }
            )),
            "{ops:?}"
        );
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::FetchStaticProp {
                    mode: FetchMode::Is,
                    ..
                }
            )),
            "{ops:?}"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, Op::AssignStaticProp { dst: None, .. })),
            "{ops:?}"
        );
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::AssignOpStaticProp {
                    op: BinaryOp::Concat,
                    ..
                }
            )),
            "{ops:?}"
        );
        for empty in [false, true] {
            assert!(
                ops.iter()
                    .any(|op| matches!(op, Op::IssetStaticProp { empty: e, .. } if *e == empty)),
                "{ops:?}"
            );
        }
        // A variable property name (`A::$$d`) and a dimension (`A::$a[]`)
        // keep the generic ops.
        assert!(
            ops.iter().any(|op| matches!(op, Op::Fetch { .. })),
            "{ops:?}"
        );
        assert!(
            ops.iter().any(|op| matches!(op, Op::Assign { .. })),
            "{ops:?}"
        );
    }

    #[test]
    fn constant_fetches_into_variables_are_specialised() {
        // Classes not declared in the file: nothing is folded at compile
        // time.
        let ops = main_ops("<?php $x = C; $y = B::X; echo C; echo B::X; $w = $o::X;");
        assert_eq!(
            ops.iter()
                .filter(|op| matches!(op, Op::FetchConstTo { .. }))
                .count(),
            1,
            "{ops:?}"
        );
        assert_eq!(
            ops.iter()
                .filter(|op| matches!(op, Op::FetchClassConstTo { .. }))
                .count(),
            2,
            "{ops:?}"
        );
        // A fetch whose value is used otherwise keeps the generic op.
        assert!(
            ops.iter().any(|op| matches!(op, Op::FetchConst { .. })),
            "{ops:?}"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, Op::FetchClassConst { .. })),
            "{ops:?}"
        );
    }

    #[test]
    fn object_properties_are_specialised() {
        let ops = main_ops(
            "<?php
            $x = $o->p;
            $y = $o->p['k'];
            $o->p = 1;
            $o->p['k'] = 2;
            $o->p[] = 3;
            $o->p += 4;
            $o->p /= 2;
            $o->p['k'] += 5;
            $o->p++;
            $o->p['k']--;
            $a = isset($o->p);
            $b = empty($o->p);
            $c = isset($o->p['k']);
            $d = empty($o->p['k']);
            ",
        );
        assert!(
            ops.iter().any(|op| matches!(op, Op::FetchObjProp { .. })),
            "{ops:?}"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, Op::FetchObjPropDim { .. })),
            "{ops:?}"
        );
        assert!(
            ops.iter().any(|op| matches!(op, Op::AssignObjProp { .. })),
            "{ops:?}"
        );
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::AssignObjPropDim {
                    dim: Operand::Unused,
                    ..
                }
            )),
            "{ops:?}"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, Op::AssignObjPropDim { .. })),
            "{ops:?}"
        );
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::AssignOpObjProp {
                    op: BinaryOp::Add,
                    ..
                }
            )),
            "{ops:?}"
        );
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::AssignOpObjProp {
                    op: BinaryOp::Div,
                    ..
                }
            )),
            "{ops:?}"
        );
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::AssignOpObjPropDim {
                    op: BinaryOp::Add,
                    ..
                }
            )),
            "{ops:?}"
        );
        assert!(
            ops.iter().any(|op| matches!(op, Op::IncDecObjProp { .. })),
            "{ops:?}"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, Op::IncDecObjPropDim { .. })),
            "{ops:?}"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, Op::IssetObjProp { empty: false, .. })),
            "{ops:?}"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, Op::IssetObjProp { empty: true, .. })),
            "{ops:?}"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, Op::IssetObjPropDim { empty: false, .. })),
            "{ops:?}"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, Op::IssetObjPropDim { empty: true, .. })),
            "{ops:?}"
        );
    }

    #[test]
    fn user_opcode_hooks_keep_the_generic_ops() {
        let ops = hooked_ops("<?php $x = 1; $x = $x + 1; if ($x < 3) { echo 1; }");
        assert!(
            !ops.iter()
                .any(|op| matches!(op, Op::BinAssignCv { .. } | Op::CmpJmp { .. })),
            "{ops:?}"
        );
    }

    #[test]
    fn abs_calls_are_specialised() {
        let ops = main_ops("<?php $x = -3; abs($x); $y = abs($x);");
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::Abs {
                    dst: None,
                    to: None,
                    ..
                }
            )),
            "{ops:?}"
        );
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::Abs {
                    dst: Some(_),
                    to: Some(_),
                    ..
                }
            )),
            "{ops:?}"
        );
    }

    /// With `abs` removed by `disable_functions`, user code may declare its
    /// own `abs()`: the call stays a call.
    #[test]
    fn abs_calls_of_a_disabled_abs_are_calls() {
        crate::compiler::optimizer::set_disabled_functions([b"abs".to_vec()]);
        let ops = main_ops("<?php function abs($x) { return 1; } $x = -3; $y = abs($x);");
        crate::compiler::optimizer::set_disabled_functions(std::iter::empty());
        assert!(
            !ops.iter().any(|op| matches!(op, Op::Abs { .. })),
            "{ops:?}"
        );
    }

    #[test]
    fn loop_steps_with_bin_assign_and_assign_op_are_fused() {
        let ops = main_ops("<?php for ($i = 0; $i < 10; $i = $i + 1) { echo $i; }");
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::BinAssignCvCmpJmp {
                    op: BinKind::Arith(BinaryOp::Add),
                    ..
                }
            )),
            "{ops:?}"
        );

        let ops = main_ops("<?php for ($i = 0; $i < 10; $i += 2) { echo $i; }");
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::AssignOpCvCmpJmp {
                    op: BinaryOp::Add,
                    ..
                }
            )),
            "{ops:?}"
        );
    }

    #[test]
    fn bin_assign_and_assign_op_followed_by_branch_are_fused() {
        let ops = main_ops("<?php $x = 1; $y = 2; $x = $x + $y; if ($x) { echo 1; }");
        assert!(
            ops.iter()
                .any(|op| matches!(op, Op::BinAssignCvJmp { jmp_if: false, .. })),
            "{ops:?}"
        );

        let ops = main_ops("<?php $x = 1; $x += 2; if ($x) { echo 1; }");
        assert!(
            ops.iter()
                .any(|op| matches!(op, Op::AssignOpCvJmp { jmp_if: false, .. })),
            "{ops:?}"
        );
    }

    #[test]
    fn short_circuit_conditions_jump_where_their_result_leads() {
        let exs = |ops: &[Op]| {
            ops.iter()
                .filter(|op| matches!(op, Op::JmpZEx { .. } | Op::JmpNzEx { .. }))
                .count()
        };
        let unaries = |ops: &[Op]| {
            ops.iter()
                .filter(|op| matches!(op, Op::Unary { .. }))
                .count()
        };
        let smart = |ops: &[Op]| {
            ops.iter()
                .filter(|op| matches!(op, Op::CmpJmp { .. }))
                .count()
        };
        // A loop's test: both comparisons are smart branches, nothing is
        // stored.
        let ops = main_ops("<?php $a = 1; $b = 9; while ($a < 5 && $b > 0) { $a++; }");
        assert_eq!(
            (exs(&ops), unaries(&ops), smart(&ops)),
            (0, 0, 2),
            "{ops:?}"
        );
        // The second comparison closes the loop itself (the loop's step,
        // right before the first comparison, is fused with that one).
        let head = ops
            .iter()
            .position(|op| matches!(op, Op::IncDecCv { .. } | Op::IncDecCvCmpJmp { .. }))
            .expect("the loop's body");
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::CmpJmp { target, jmp_if: true, .. } if *target as usize == head
            )),
            "{ops:?}"
        );
        // `||`, a chain, a mix of both, and `!` on either side.
        for src in [
            "<?php $a = 1; $b = 9; if ($a < 5 || $b > 0) { echo 1; }",
            "<?php $a = 1; $b = 9; $c = 3; if ($a < 5 && $b > 0 && $c == 3) { echo 1; }",
            "<?php $a = 1; $b = 9; $c = 3; if (($a < 5 && $b > 0) || $c == 3) { echo 1; }",
            "<?php $a = 1; $b = 9; $c = 3; if ($a < 5 && ($b > 0 || $c == 3)) { echo 1; }",
            "<?php $a = 1; $b = 9; do { $a++; } while ($a < 5 || $b < 0);",
            "<?php $a = 1; $b = 9; for (; $a < 5 && $b > 0; $a++) { echo 1; }",
            "<?php $a = 1; $b = 0; if (!$a && !$b) { echo 1; }",
            "<?php $a = 1; $b = 0; if (!($a && $b)) { echo 1; }",
            "<?php $a = 1; $b = 0; $c = 2; if (!($a || $b) && !$c) { echo 1; }",
            "<?php $a = 1; if (!$a) { echo 1; }",
            "<?php $a = [1]; if (isset($a[0], $a[1])) { echo 1; }",
        ] {
            let ops = main_ops(src);
            assert_eq!((exs(&ops), unaries(&ops)), (0, 0), "{src}: {ops:?}");
        }
        // A result that is used as a value keeps its ops.
        for src in [
            "<?php $a = 1; $b = 9; $x = $a < 5 && $b > 0;",
            "<?php $a = 1; $b = 9; var_dump($a < 5 || $b > 0);",
            "<?php $a = 1; $b = 9; return $a && $b;",
        ] {
            let ops = main_ops(src);
            assert_eq!(exs(&ops), 1, "{src}: {ops:?}");
        }
        // A chain with a value at its end is not followed past it.
        let ops = main_ops("<?php $a = 1; $b = 2; $c = 3; $x = $a && $b && $c;");
        assert_eq!(exs(&ops), 2, "{ops:?}");
    }

    #[test]
    fn a_jump_landing_on_a_test_keeps_the_bool_before_it() {
        // The `Bool` before the second test of the hand-made sequence stays:
        // the first jump lands on that test with its own value.
        let mut ops = vec![
            Op::JmpZ(Operand::Cv(0), 2),
            Op::Unary {
                op: UnKind::Bool,
                a: Operand::Cv(1),
                dst: 0,
            },
            Op::JmpZ(Operand::Tmp(0), 4),
            Op::Nop,
            Op::Nop,
        ];
        super::thread_short_circuits(&mut ops);
        assert!(matches!(ops[1], Op::Unary { .. }), "{ops:?}");
        // Without that jump it becomes the test.
        ops[0] = Op::Nop;
        super::thread_short_circuits(&mut ops);
        assert!(matches!(ops[1], Op::JmpZ(Operand::Cv(1), 4)), "{ops:?}");
        assert!(matches!(ops[2], Op::Nop), "{ops:?}");
        // `!`: the opposite jump.
        let mut ops = vec![
            Op::Unary {
                op: UnKind::Not,
                a: Operand::Cv(1),
                dst: 0,
            },
            Op::JmpZ(Operand::Tmp(0), 3),
            Op::Nop,
            Op::Nop,
        ];
        super::thread_short_circuits(&mut ops);
        assert!(matches!(ops[0], Op::JmpNz(Operand::Cv(1), 3)), "{ops:?}");
    }

    #[test]
    fn spaceship_is_fused_into_cmp_jmp() {
        let ops = main_ops("<?php $a = 1; $b = 2; if (($a <=> $b) == 0) { echo 1; }");
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::CmpJmp {
                    op: BinKind::Equal,
                    ..
                }
            )),
            "{ops:?}"
        );

        let ops = main_ops("<?php $a = 1; $b = 2; if ($a <=> $b) { echo 1; }");
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::CmpJmp {
                    op: BinKind::Spaceship,
                    jmp_if: false,
                    ..
                }
            )),
            "{ops:?}"
        );
    }
}

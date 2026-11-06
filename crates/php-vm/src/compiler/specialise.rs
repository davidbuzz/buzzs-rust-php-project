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
//! op skips its successor only on its fast path, where the successor (the
//! jump testing the comparison, the `AssignCv` storing the result) would
//! have done nothing but consume the temporary; on any other path it writes
//! the temporary and the successor runs as before. A jump landing on the
//! successor from elsewhere also runs it as before.

use crate::opcode::{
    cmp_outcome, BinKind, ClassSpec, FetchMode, Op, Operand, Place, PlaceBase, PlaceId, Seg,
};
use php_runtime::BinaryOp;

/// Rewrites `ops` (the finished body whose places are `places`) in place.
pub(crate) fn specialise(ops: &mut [Op], places: &[Place]) {
    for i in 0..ops.len() {
        let (head, tail) = ops.split_at_mut(i + 1);
        let Some(cur) = head.last_mut() else {
            continue;
        };
        if let Some(op) = rewrite(cur, tail.first(), places) {
            *cur = op;
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
            }),
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
    static_prop(places, pid).then_some(Op::IssetStaticProp {
        place: pid,
        dst,
        empty,
    })
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
        Op::JmpZ(Operand::Tmp(t), target) if *t == dst => Some((*target, false)),
        Op::JmpNz(Operand::Tmp(t), target) if *t == dst => Some((*target, true)),
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
            val: Operand::Tmp(t),
            dst: None,
        } if *t == dst => Some(*cv),
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use super::super::{compile, CompileOptions};
    use crate::opcode::{cmp_outcome, BinKind, FetchMode, IncDecKind, Op, Operand};
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
        assert!(
            ops.iter().any(|op| matches!(
                op,
                Op::IncDecCv {
                    kind: IncDecKind::PostInc,
                    dst: None,
                    ..
                }
            )),
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
        // Nested dimension reads and writes get the chain forms; appends
        // keep the generic place ops.
        let ops = main_ops(
            "<?php $a = [[1]]; echo $a[0][0]; $a[] = 1; $a[0][0] = 2; $x = $a[0][0]; $y = $a[0][0] ?? 1;",
        );
        assert!(
            !ops.iter()
                .any(|op| matches!(op, Op::FetchDimCv { .. } | Op::AssignDimCv { .. })),
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
        assert_eq!(
            ops.iter()
                .filter(|op| matches!(op, Op::Assign { .. }))
                .count(),
            1,
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
    fn user_opcode_hooks_keep_the_generic_ops() {
        let ops = hooked_ops("<?php $x = 1; $x = $x + 1; if ($x < 3) { echo 1; }");
        assert!(
            !ops.iter()
                .any(|op| matches!(op, Op::BinAssignCv { .. } | Op::CmpJmp { .. })),
            "{ops:?}"
        );
    }
}

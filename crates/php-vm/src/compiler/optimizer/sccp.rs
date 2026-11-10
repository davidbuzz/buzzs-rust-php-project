//! Pass 8: SCCP (Sparse Conditional Constant Propagation) & DFA (ZEND_OPTIMIZER_PASS_8)
//!
//! - Global SSA temporary constant propagation: for any temporary Tmp(t) defined
//!   by Move { src: Lit(l), dst: t }, AssignCv { val: Lit(l), dst: Some(t) },
//!   copy propagation from aliases, or folded constant operations
//!   (Bin, Unary, Cast, Strlen, TypeCheck), substitute Lit(l) at all read sites across ops and places.
//! - Conditional branch simplification: evaluate branches with constant conditions
//!   (JmpZ, JmpNz, CmpJmp) and simplify to Jmp or Nop.
//! - Inter-block forward DFA CV constant propagation: Kildall-style lattice dataflow
//!   analysis over basic blocks, propagating constants across block boundaries and joins.

use super::pass1::{fold_bin_op, fold_cast, fold_unary_op};
use super::{add_or_find_literal, for_each_operand_mut, for_each_place_operand_mut, OptContext};
use crate::compiler::expr::const_truthiness;
use crate::opcode::{
    type_mask, ArgSrc, BinKind, CastKind, FetchMode, IncDecKind, Op, OpArray, Operand, Place,
    PlaceBase, PlaceId, Seg,
};
use php_runtime::{ArrayKey, BinaryOp, Value, ZArray, ZStr};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CvLattice {
    Top,
    Const(u32),
    Bottom,
}

fn meet_cv(a: CvLattice, b: CvLattice, literals: &[Value]) -> CvLattice {
    match (a, b) {
        (CvLattice::Top, other) => other,
        (other, CvLattice::Top) => other,
        (CvLattice::Const(l1), CvLattice::Const(l2)) => {
            if l1 == l2 || super::literals_equal(&literals[l1 as usize], &literals[l2 as usize]) {
                CvLattice::Const(l1)
            } else {
                CvLattice::Bottom
            }
        }
        _ => CvLattice::Bottom,
    }
}

fn fold_abs(val: &Value) -> Option<Value> {
    match val {
        Value::Int(i) => Some(if *i == i64::MIN {
            #[allow(clippy::cast_precision_loss)]
            Value::Float(-(*i as f64))
        } else {
            Value::Int(i.abs())
        }),
        Value::Float(fl) => Some(Value::Float(fl.abs())),
        _ => None,
    }
}

fn fold_strlen(val: &Value) -> Option<i64> {
    match val {
        Value::Str(s) => Some(s.len() as i64),
        Value::Int(n) => Some(n.to_string().len() as i64),
        Value::Bool(b) => Some(if *b { 1 } else { 0 }),
        Value::Null => Some(0),
        _ => None,
    }
}

/// The CV that `place` names when it is a plain variable (`$cv`, no
/// dimension or property).
fn plain_cv(places: &[Place], place: PlaceId) -> Option<u32> {
    let p = places.get(place as usize)?;
    match p.base {
        PlaceBase::Cv(cv) if p.segs.is_empty() => Some(cv),
        _ => None,
    }
}

/// The literal of the only dimension of `place` (`base[lit]`).
fn single_dim_lit(place: &Place) -> Option<u32> {
    match place.segs.as_slice() {
        [Seg::Dim(Operand::Lit(l))] => Some(*l),
        _ => None,
    }
}

/// What reading `container[dim]` gives when that read can raise no
/// diagnostic: an existing element of a literal array, or an in-range
/// offset of a literal string (sccp.c `ct_eval_fetch_dim`). A `list()`
/// fetch from a string gives `null`, so only arrays fold there.
fn fold_fetch_dim(container: &Value, dim: &Value, mode: FetchMode) -> Option<Value> {
    let val = match (container, dim) {
        (Value::Array(arr), Value::Int(i)) => arr.get_int(*i).cloned()?,
        (Value::Array(arr), Value::Str(s)) => arr.get_sym(s.as_bytes()).cloned()?,
        (Value::Str(s), Value::Int(i)) if mode != FetchMode::List => {
            let len = i64::try_from(s.len()).ok()?;
            let idx = if *i < 0 { i.checked_add(len)? } else { *i };
            if !(0..len).contains(&idx) {
                return None;
            }
            let byte = *s.as_bytes().get(usize::try_from(idx).ok()?)?;
            Value::Str(ZStr::from(vec![byte]))
        }
        _ => return None,
    };
    // Only plain values: never hand out a reference, object or resource.
    matches!(
        val,
        Value::Null
            | Value::Bool(_)
            | Value::Int(_)
            | Value::Float(_)
            | Value::Str(_)
            | Value::Array(_)
    )
    .then_some(val)
}

/// What `isset(container[dim])` (or `empty(...)` when `empty`) gives for a
/// known container and offset (sccp.c `ct_eval_isset_dim`). Neither ever
/// warns: an array element is looked up (only integer and string keys
/// fold), a string offset is range-checked (only integer offsets fold), a
/// scalar has no elements. `None` when it does not fold.
fn fold_isset_dim(container: &Value, dim: &Value, empty: bool) -> Option<bool> {
    match container {
        Value::Array(arr) => {
            let elem = match dim {
                Value::Int(i) => arr.get_int(*i),
                Value::Str(s) => arr.get_sym(s.as_bytes()),
                _ => return None,
            };
            match (empty, elem) {
                (false, elem) => Some(elem.is_some_and(|v| !matches!(v, Value::Null))),
                (true, None) => Some(true),
                (true, Some(v)) => const_truthiness(v).map(|t| !t),
            }
        }
        Value::Str(s) => {
            let Value::Int(i) = dim else {
                return None;
            };
            let len = i64::try_from(s.len()).ok()?;
            let idx = if *i < 0 { i.checked_add(len)? } else { *i };
            if !(0..len).contains(&idx) {
                return Some(empty);
            }
            if !empty {
                return Some(true);
            }
            let byte = *s.as_bytes().get(usize::try_from(idx).ok()?)?;
            Some(byte == b'0')
        }
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) => Some(empty),
        _ => None,
    }
}

/// The container and offset literals of an `isset()`/`empty()` place
/// `container[dim]`: a literal base, or a CV holding a known constant.
fn isset_operands(place: &Place, cur_state: &[CvLattice]) -> Option<(u32, u32)> {
    let container = match &place.base {
        PlaceBase::Value(Operand::Lit(c)) => *c,
        PlaceBase::Cv(cv) => match cur_state.get(*cv as usize) {
            Some(CvLattice::Const(l)) => *l,
            _ => return None,
        },
        _ => return None,
    };
    Some((container, single_dim_lit(place)?))
}

/// Compile-time evaluation of a pure internal function on constant
/// arguments (sccp.c `ct_eval_func_call`: functions whose stub carries
/// `@compile-time-eval`). Only exact argument types fold, so no coercion
/// (and no `strict_types` difference) is involved; an argument the
/// function rejects (a `ValueError`) leaves the call alone.
fn ct_eval_func_call(name: &[u8], args: &[&Value]) -> Option<Value> {
    let string = |b: Vec<u8>| Some(Value::Str(ZStr::from(b)));
    let digest = |name: &[u8], s: &ZStr| -> Vec<u8> {
        if name == b"md5" {
            php_hash::md5(s.as_bytes()).to_vec()
        } else {
            php_hash::sha1(s.as_bytes()).to_vec()
        }
    };
    match (name, args) {
        (b"md5" | b"sha1", [Value::Str(s)]) => {
            string(php_hash::bin2hex(&digest(name, s)).into_bytes())
        }
        (b"md5" | b"sha1", [Value::Str(s), Value::Bool(raw)]) => {
            let d = digest(name, s);
            string(if *raw {
                d
            } else {
                php_hash::bin2hex(&d).into_bytes()
            })
        }
        (b"bin2hex", [Value::Str(s)]) => string(php_hash::bin2hex(s.as_bytes()).into_bytes()),
        (b"str_increment", [Value::Str(s)]) => {
            string(php_stdstr::transform::str_increment(s.as_bytes()).ok()?)
        }
        (b"str_decrement", [Value::Str(s)]) => {
            string(php_stdstr::transform::str_decrement(s.as_bytes()).ok()?)
        }
        _ => None,
    }
}

/// `INIT_FCALL f; SEND_VAL c...; T = DO_ICALL` of a function
/// [`ct_eval_func_call`] evaluates becomes `T = QM_ASSIGN result`. The call
/// must be straight-line (only `NOP`s and folded constant moves between its
/// steps) and name a global function resolved at compile time (no
/// namespace fallback) that is not disabled. Calls are visited innermost
/// first, so `bin2hex(md5($s, true))` folds in one go.
fn fold_const_func_calls(
    op_array: &mut OpArray,
    def_count: &[u32],
    lit_def: &mut [Option<u32>],
) -> bool {
    let mut changed = false;
    for i in (0..op_array.ops.len()).rev() {
        let Op::InitFCall {
            name,
            fallback: None,
            ..
        } = op_array.ops[i]
        else {
            continue;
        };
        let Some(Value::Str(fname)) = op_array.literals.get(name as usize) else {
            continue;
        };
        let fname = fname.as_bytes().to_ascii_lowercase();
        if super::is_disabled_function(&fname) {
            continue;
        }
        let mut args = Vec::new();
        let mut sends = Vec::new();
        let mut call = None;
        for (j, op) in op_array.ops.iter().enumerate().skip(i + 1) {
            match op {
                Op::Nop
                | Op::Move {
                    src: Operand::Lit(_),
                    ..
                } => {}
                Op::Send {
                    src: ArgSrc::Val(arg) | ArgSrc::CallResult(arg),
                } => {
                    let Some(l) = known_lit(arg, lit_def) else {
                        break;
                    };
                    args.push(l);
                    sends.push(j);
                }
                Op::DoCall { dst } => {
                    call = Some((j, *dst));
                    break;
                }
                _ => break,
            }
        }
        let Some((call_idx, dst)) = call else {
            continue;
        };
        let vals: Option<Vec<&Value>> = args
            .iter()
            .map(|&l| op_array.literals.get(l as usize))
            .collect();
        let Some(result) = vals.and_then(|v| ct_eval_func_call(&fname, &v)) else {
            continue;
        };
        let lit = add_or_find_literal(&mut op_array.literals, result);
        op_array.ops[i] = Op::Nop;
        for j in sends {
            op_array.ops[j] = Op::Nop;
        }
        op_array.ops[call_idx] = match dst {
            Some(d) => {
                if def_count.get(d as usize).copied() == Some(1) {
                    if let Some(slot) = lit_def.get_mut(d as usize) {
                        *slot = Some(lit);
                    }
                }
                Op::Move {
                    src: Operand::Lit(lit),
                    dst: d,
                }
            }
            None => Op::Nop,
        };
        changed = true;
    }
    changed
}

/// For an in-place update of a plain CV (`$cv op= val`, `++$cv`, ...):
/// the CV and its new constant (the literal index), or `None` for the
/// constant when the result is not known. `None` overall when `op` is no
/// such update.
fn plain_cv_update(
    op: &Op,
    places: &[Place],
    cur_state: &[CvLattice],
    literals: &mut Vec<Value>,
) -> Option<(u32, Option<u32>)> {
    let place = match op {
        Op::AssignOp { place, .. } | Op::IncDec { place, .. } => *place,
        _ => return None,
    };
    let cv = plain_cv(places, place)?;
    let CvLattice::Const(cur) = *cur_state.get(cv as usize)? else {
        return Some((cv, None));
    };
    let res = match op {
        Op::AssignOp { op: bop, val, .. } => {
            let rhs = match val {
                Operand::Lit(l) => Some(*l),
                Operand::Cv(c) => match cur_state.get(*c as usize) {
                    Some(CvLattice::Const(l)) => Some(*l),
                    _ => None,
                },
                _ => None,
            };
            rhs.and_then(|l| {
                let a = literals.get(cur as usize)?.clone();
                let b = literals.get(l as usize)?.clone();
                fold_bin_op(BinKind::Arith(*bop), &a, &b)
            })
        }
        Op::IncDec { kind, .. } => match literals.get(cur as usize) {
            Some(Value::Int(i)) => match kind {
                IncDecKind::PreInc | IncDecKind::PostInc => i.checked_add(1),
                IncDecKind::PreDec | IncDecKind::PostDec => i.checked_sub(1),
            }
            .map(Value::Int),
            _ => None,
        },
        _ => None,
    };
    Some((cv, res.map(|v| add_or_find_literal(literals, v))))
}

/// The literal of `operand` when it is a literal or a CV with a known
/// constant.
fn cv_or_lit(operand: &Operand, cur_state: &[CvLattice]) -> Option<u32> {
    match operand {
        Operand::Lit(l) => Some(*l),
        Operand::Cv(c) => match cur_state.get(*c as usize) {
            Some(CvLattice::Const(l)) => Some(*l),
            _ => None,
        },
        _ => None,
    }
}

/// The literal index of an array equal (`===`) to `arr`, adding it when
/// there is none, so the dataflow sees the same constant on every visit
/// (a fresh index for an identical array would meet as overdefined).
fn add_or_find_array_literal(literals: &mut Vec<Value>, arr: ZArray) -> u32 {
    let val = Value::Array(arr);
    if let Some(i) = literals.iter().position(|l| {
        matches!(l, Value::Array(_)) && php_runtime::ops::is_identical(l, &val).unwrap_or(false)
    }) {
        return u32::try_from(i).unwrap_or(u32::MAX);
    }
    add_or_find_literal(literals, val)
}

/// The array `container` becomes after `container[seg] = val`, when that
/// write is a known constant that cannot warn (see [`const_assign_dim`]).
fn assign_dim_result(
    container: &Value,
    seg: &Seg,
    val: &Operand,
    cur_state: &[CvLattice],
    literals: &[Value],
) -> Option<ZArray> {
    let Value::Array(arr) = container else {
        return None;
    };
    let v = literals.get(cv_or_lit(val, cur_state)? as usize)?.clone();
    let mut arr = arr.clone();
    let h = arr.make_mut();
    match seg {
        Seg::Append => {
            h.push(v).ok()?;
        }
        Seg::Dim(k) => match literals.get(cv_or_lit(k, cur_state)? as usize)? {
            Value::Int(n) => {
                h.insert(*n, v);
            }
            Value::Str(s) => {
                h.insert(ArrayKey::from_zstr(s), v);
            }
            _ => return None,
        },
        Seg::Prop(_) => return None,
    }
    Some(arr)
}

/// For `$cv[k] = v` / `$cv[] = v` of a CV holding a constant array: the CV
/// and its new constant, or `None` for the constant when the result is not
/// known (sccp.c `ct_eval_assign_dim`). Only integer and string keys fold:
/// a null, bool or float key may deprecate, and an append past
/// `PHP_INT_MAX` warns. Writing a scalar element of an array runs no user
/// code and raises nothing. `None` overall when `op` is no such write.
fn const_assign_dim(
    op: &Op,
    places: &[Place],
    cur_state: &[CvLattice],
    literals: &mut Vec<Value>,
) -> Option<(u32, Option<u32>)> {
    let Op::Assign { place, val, .. } = op else {
        return None;
    };
    let p = places.get(*place as usize)?;
    let PlaceBase::Cv(cv) = p.base else {
        return None;
    };
    let [seg] = p.segs.as_slice() else {
        return None;
    };
    let CvLattice::Const(cur) = *cur_state.get(cv as usize)? else {
        return Some((cv, None));
    };
    let new = assign_dim_result(literals.get(cur as usize)?, seg, val, cur_state, literals);
    Some((cv, new.map(|a| add_or_find_array_literal(literals, a))))
}

fn update_cv_state_for_op(
    op: &Op,
    places: &[crate::opcode::Place],
    num_cvs: usize,
    cur_state: &mut [CvLattice],
    literals: &mut Vec<Value>,
) {
    if let Some((cv, lit)) = plain_cv_update(op, places, cur_state, literals) {
        if let Some(slot) = cur_state.get_mut(cv as usize) {
            *slot = lit.map_or(CvLattice::Bottom, CvLattice::Const);
            return;
        }
    }
    if let Some((cv, lit)) = const_assign_dim(op, places, cur_state, literals) {
        if let Some(slot) = cur_state.get_mut(cv as usize) {
            *slot = lit.map_or(CvLattice::Bottom, CvLattice::Const);
            return;
        }
    }
    match op {
        Op::AssignCv { cv, val, .. } => {
            let cv_idx = *cv as usize;
            if cv_idx < num_cvs {
                match val {
                    Operand::Lit(l) => {
                        cur_state[cv_idx] = CvLattice::Const(*l);
                    }
                    Operand::Cv(other) => {
                        let o_idx = *other as usize;
                        if o_idx < num_cvs {
                            cur_state[cv_idx] = cur_state[o_idx];
                        } else {
                            cur_state[cv_idx] = CvLattice::Bottom;
                        }
                    }
                    _ => {
                        cur_state[cv_idx] = CvLattice::Bottom;
                    }
                }
            }
        }
        Op::Abs {
            to: Some(cv), src, ..
        } => {
            let cv_idx = *cv as usize;
            if cv_idx < num_cvs {
                let lit_src = match src {
                    Operand::Lit(l) => Some(*l),
                    Operand::Cv(c) => match cur_state.get(*c as usize) {
                        Some(CvLattice::Const(l)) => Some(*l),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(ls) = lit_src {
                    if let Some(val) = literals.get(ls as usize) {
                        if let Some(res) = fold_abs(val) {
                            let new_lit = add_or_find_literal(literals, res);
                            cur_state[cv_idx] = CvLattice::Const(new_lit);
                            return;
                        }
                    }
                }
                cur_state[cv_idx] = CvLattice::Bottom;
            }
        }
        Op::BinAssignCv {
            cv,
            op: bkind,
            a,
            b,
            ..
        } => {
            let cv_idx = *cv as usize;
            if cv_idx < num_cvs {
                let lit_a = match a {
                    Operand::Lit(l) => Some(*l),
                    Operand::Cv(c) => match cur_state.get(*c as usize) {
                        Some(CvLattice::Const(l)) => Some(*l),
                        _ => None,
                    },
                    _ => None,
                };
                let lit_b = match b {
                    Operand::Lit(l) => Some(*l),
                    Operand::Cv(c) => match cur_state.get(*c as usize) {
                        Some(CvLattice::Const(l)) => Some(*l),
                        _ => None,
                    },
                    _ => None,
                };
                if let (Some(la), Some(lb)) = (lit_a, lit_b) {
                    if let (Some(va), Some(vb)) = (
                        literals.get(la as usize).cloned(),
                        literals.get(lb as usize).cloned(),
                    ) {
                        if let Some(res) = fold_bin_op(*bkind, &va, &vb) {
                            let new_lit = add_or_find_literal(literals, res);
                            cur_state[cv_idx] = CvLattice::Const(new_lit);
                            return;
                        }
                    }
                }
                if *bkind == BinKind::Arith(BinaryOp::Concat) {
                    if let Some(la) = lit_a {
                        if matches!(literals.get(la as usize), Some(Value::Str(s)) if s.is_empty())
                        {
                            if let Operand::Cv(cb) = b {
                                if (*cb as usize) < num_cvs {
                                    cur_state[cv_idx] = cur_state[*cb as usize];
                                    return;
                                }
                            }
                        }
                    }
                    if let Some(lb) = lit_b {
                        if matches!(literals.get(lb as usize), Some(Value::Str(s)) if s.is_empty())
                        {
                            if let Operand::Cv(ca) = a {
                                if (*ca as usize) < num_cvs {
                                    cur_state[cv_idx] = cur_state[*ca as usize];
                                    return;
                                }
                            }
                        }
                    }
                }
                cur_state[cv_idx] = CvLattice::Bottom;
            }
        }
        Op::ConcatCv { cv, val, .. } => {
            let cv_idx = *cv as usize;
            if cv_idx < num_cvs {
                if let CvLattice::Const(l1) = cur_state[cv_idx] {
                    let lit_val = match val {
                        Operand::Lit(l) => Some(*l),
                        Operand::Cv(c) => match cur_state.get(*c as usize) {
                            Some(CvLattice::Const(l)) => Some(*l),
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some(l2) = lit_val {
                        let s1 = match literals.get(l1 as usize) {
                            Some(Value::Str(s)) => Some(s.as_bytes().to_vec()),
                            Some(Value::Int(n)) => Some(n.to_string().into_bytes()),
                            _ => None,
                        };
                        let s2 = match literals.get(l2 as usize) {
                            Some(Value::Str(s)) => Some(s.as_bytes().to_vec()),
                            Some(Value::Int(n)) => Some(n.to_string().into_bytes()),
                            _ => None,
                        };
                        if let (Some(mut b1), Some(b2)) = (s1, s2) {
                            b1.extend_from_slice(&b2);
                            let new_lit = add_or_find_literal(literals, Value::Str(ZStr::from(b1)));
                            cur_state[cv_idx] = CvLattice::Const(new_lit);
                            return;
                        }
                    }
                }
                cur_state[cv_idx] = CvLattice::Bottom;
            }
        }
        Op::IncDecCv { cv, kind, .. } => {
            let cv_idx = *cv as usize;
            if cv_idx < num_cvs {
                if let CvLattice::Const(l) = cur_state[cv_idx] {
                    if let Some(Value::Int(i)) = literals.get(l as usize) {
                        let new_val = match kind {
                            IncDecKind::PreInc | IncDecKind::PostInc => {
                                i.checked_add(1).map(Value::Int)
                            }
                            IncDecKind::PreDec | IncDecKind::PostDec => {
                                i.checked_sub(1).map(Value::Int)
                            }
                        };
                        if let Some(nv) = new_val {
                            let new_lit = add_or_find_literal(literals, nv);
                            cur_state[cv_idx] = CvLattice::Const(new_lit);
                            return;
                        }
                    }
                }
                cur_state[cv_idx] = CvLattice::Bottom;
            }
        }
        Op::AssignOpCv {
            cv, op: bop, val, ..
        } => {
            let cv_idx = *cv as usize;
            if cv_idx < num_cvs {
                if let CvLattice::Const(l1) = cur_state[cv_idx] {
                    let lit_val = match val {
                        Operand::Lit(l) => Some(*l),
                        Operand::Cv(c) => match cur_state.get(*c as usize) {
                            Some(CvLattice::Const(l)) => Some(*l),
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some(l2) = lit_val {
                        if let (Some(va), Some(vb)) = (
                            literals.get(l1 as usize).cloned(),
                            literals.get(l2 as usize).cloned(),
                        ) {
                            if let Some(res) = fold_bin_op(BinKind::Arith(*bop), &va, &vb) {
                                let new_lit = add_or_find_literal(literals, res);
                                cur_state[cv_idx] = CvLattice::Const(new_lit);
                                return;
                            }
                        }
                    }
                }
                cur_state[cv_idx] = CvLattice::Bottom;
            }
        }
        Op::IncDec { place, .. }
        | Op::Assign { place, .. }
        | Op::AssignOp { place, .. }
        | Op::AssignRef { place, .. }
        | Op::MakeRef { place, .. }
        | Op::Unset { place, .. } => {
            if let Some(p) = places.get(*place as usize) {
                match p.base {
                    PlaceBase::Cv(cv) => {
                        let cv_idx = cv as usize;
                        if cv_idx < num_cvs {
                            cur_state[cv_idx] = CvLattice::Bottom;
                        }
                    }
                    PlaceBase::VarVar(_) => {
                        cur_state.fill(CvLattice::Bottom);
                    }
                    _ => {}
                }
            }
        }
        Op::IncDecCvCmpJmp { cv, .. }
        | Op::BinAssignCvCmpJmp { cv, .. }
        | Op::BinAssignCvJmp { cv, .. }
        | Op::AssignOpCvCmpJmp { cv, .. }
        | Op::AssignOpCvJmp { cv, .. }
        | Op::FetchConstTo { to: cv, .. }
        | Op::FetchClassConstTo { to: cv, .. }
        | Op::Global { cv, .. }
        | Op::BindStatic { cv, .. }
        | Op::InitStatic { cv, .. }
        | Op::Recv { cv, .. }
        | Op::RecvInit { cv, .. }
        | Op::RecvVariadic { cv, .. }
        | Op::AssignDimCv { cv, .. }
        | Op::AssignDimsCv { cv, .. }
        | Op::AssignOpDimsCv { cv, .. }
        | Op::AssignOpDimCv { cv, .. }
        | Op::IncDecDimCv { cv, .. } => {
            let cv_idx = *cv as usize;
            if cv_idx < num_cvs {
                cur_state[cv_idx] = CvLattice::Bottom;
            }
        }
        _ => {}
    }
}

fn simplify_constant_branches(ops: &mut [Op], literals: &[Value]) -> bool {
    let mut changed = false;
    for op in ops {
        match op {
            Op::JmpZ(Operand::Lit(l), target) => {
                if let Some(va) = literals.get(*l as usize) {
                    if let Some(truthy) = const_truthiness(va) {
                        if !truthy {
                            *op = Op::Jmp(*target);
                        } else {
                            *op = Op::Nop;
                        }
                        changed = true;
                    }
                }
            }
            Op::JmpNz(Operand::Lit(l), target) => {
                if let Some(va) = literals.get(*l as usize) {
                    if let Some(truthy) = const_truthiness(va) {
                        if truthy {
                            *op = Op::Jmp(*target);
                        } else {
                            *op = Op::Nop;
                        }
                        changed = true;
                    }
                }
            }
            Op::CmpJmp {
                op: bkind,
                a: Operand::Lit(la),
                b: Operand::Lit(lb),
                target,
                jmp_if,
                ..
            } => {
                if let (Some(va), Some(vb)) = (
                    literals.get(*la as usize).cloned(),
                    literals.get(*lb as usize).cloned(),
                ) {
                    if let Some(Value::Bool(res)) = fold_bin_op(*bkind, &va, &vb) {
                        if res == *jmp_if {
                            *op = Op::Jmp(*target);
                        } else {
                            *op = Op::Nop;
                        }
                        changed = true;
                    }
                }
            }
            _ => {}
        }
    }
    changed
}

/// Whether a CV of `op_array` may be a reference (php-src infers
/// `MAY_BE_REF` for it): a reference can change whenever user code runs,
/// which any op that warns can do through an error handler, and assigning
/// to a typed reference coerces the value. Such CVs are never constant.
fn cvs_may_be_refs(op_array: &OpArray, ctx: &OptContext<'_>) -> bool {
    if ctx.is_main || ctx.has_ref_binding() {
        return true;
    }
    let decls = op_array.decls.functions.borrow();
    let ref_op = op_array.ops.iter().any(|op| match op {
        Op::MakeRef { .. }
        | Op::AssignRef { .. }
        | Op::ListFetchRef { .. }
        | Op::ArrayAddRef { .. }
        | Op::YieldRef { .. }
        | Op::ReturnRef { .. }
        | Op::Include { .. }
        | Op::Global { .. }
        | Op::BindStatic { .. }
        | Op::InitStatic { .. }
        | Op::FeReset { place: Some(_), .. }
        | Op::Send {
            src: ArgSrc::Place(_),
        }
        | Op::SendNamed {
            src: ArgSrc::Place(_),
            ..
        } => true,
        // `function () use (&$v)` turns `$v` into a reference here.
        Op::MakeClosure { idx, .. } => decls
            .get(*idx as usize)
            .is_none_or(|f| f.uses.iter().any(|u| u.by_ref)),
        _ => false,
    });
    ref_op
        || op_array.places.iter().any(|place| {
            matches!(
                place.base,
                PlaceBase::VarVar(_) | PlaceBase::Global(_) | PlaceBase::GlobalsArray
            )
        })
}

/// A build-step operand's literal: a literal, or a temporary known to
/// hold one.
fn known_lit(operand: &Operand, lit_def: &[Option<u32>]) -> Option<u32> {
    match operand {
        Operand::Lit(l) => Some(*l),
        Operand::Tmp(t) => lit_def.get(*t as usize).copied().flatten(),
        _ => None,
    }
}

/// `INIT_ARRAY` followed by `ADD_ARRAY_ELEMENT`s whose keys and values are
/// all known constants becomes one array literal (sccp.c
/// `ct_eval_add_array_elem` / `ct_eval_add_array_unpack` for the
/// `ZEND_INIT_ARRAY` / `ZEND_ADD_ARRAY_ELEMENT` chain). The build must be
/// straight-line: only folded constant moves and `Nop`s may sit between its
/// steps, and no jump may land inside it. Keys fold only when they cannot
/// warn or deprecate: integers and strings (`"1"` becomes `1`, as
/// `ZEND_HANDLE_NUMERIC_STR`); a null, bool, float or array key, a
/// reference element, an unpack, or an append past `PHP_INT_MAX` leaves the
/// build alone.
fn fold_const_array_builds(
    op_array: &mut OpArray,
    def_count: &[u32],
    lit_def: &mut [Option<u32>],
) -> bool {
    let mut jump_targets: Option<Vec<bool>> = None;
    let mut changed = false;
    let mut i = 0;
    while i < op_array.ops.len() {
        let Op::InitArray { dst: t } = op_array.ops[i] else {
            i += 1;
            continue;
        };
        if def_count.get(t as usize).copied() != Some(1) {
            i += 1;
            continue;
        }
        // The steps of the build, and where it ends (the last step).
        let mut steps = Vec::new();
        let mut end = None;
        for (j, op) in op_array.ops.iter().enumerate().skip(i + 1) {
            match op {
                Op::ArrayAdd { arr, key, val } if *arr == t => {
                    let Some(v) = known_lit(val, lit_def) else {
                        break;
                    };
                    let k = match key {
                        None => None,
                        Some(k) => match known_lit(k, lit_def) {
                            Some(k) => Some(k),
                            None => break,
                        },
                    };
                    steps.push((j, k, v));
                }
                Op::Nop => {}
                Op::Move {
                    src: Operand::Lit(_),
                    dst,
                } if *dst != t => {}
                _ => {
                    end = Some(j);
                    break;
                }
            }
        }
        // A build that stops at a step whose operands are not constant is
        // not foldable (nor one running off the end of the op array).
        let Some(end) = end else {
            i += 1;
            continue;
        };
        // The build must be complete: nothing after it adds to the array.
        if op_array.ops[end..].iter().any(|op| {
            matches!(op, Op::ArrayAdd { arr, .. } | Op::ArrayAddRef { arr, .. }
                | Op::ArrayUnpack { arr, .. } | Op::ConstArrayUnpack { arr, .. } if *arr == t)
        }) {
            i += 1;
            continue;
        }
        let targets = jump_targets.get_or_insert_with(|| {
            let mut v = vec![false; op_array.ops.len() + 1];
            for op in &op_array.ops {
                let mut op = op.clone();
                super::for_each_label_mut(&mut op, |l| {
                    if let Some(slot) = v.get_mut(*l as usize) {
                        *slot = true;
                    }
                });
            }
            v
        });
        if targets[i + 1..end].iter().any(|&b| b) {
            i += 1;
            continue;
        }
        let mut arr = ZArray::new();
        let mut ok = true;
        for &(_, k, v) in &steps {
            let (Some(val), key) = (
                op_array.literals.get(v as usize).cloned(),
                k.and_then(|k| op_array.literals.get(k as usize)),
            ) else {
                ok = false;
                break;
            };
            let h = arr.make_mut();
            match (k, key) {
                (None, _) => {
                    if h.push(val).is_err() {
                        ok = false;
                        break;
                    }
                }
                (Some(_), Some(Value::Int(n))) => {
                    h.insert(*n, val);
                }
                (Some(_), Some(Value::Str(s))) => {
                    h.insert(ArrayKey::from_zstr(s), val);
                }
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            i += 1;
            continue;
        }
        let lit = add_or_find_literal(&mut op_array.literals, Value::Array(arr));
        op_array.ops[i] = Op::Move {
            src: Operand::Lit(lit),
            dst: t,
        };
        for &(j, _, _) in &steps {
            op_array.ops[j] = Op::Nop;
        }
        if let Some(slot) = lit_def.get_mut(t as usize) {
            *slot = Some(lit);
        }
        changed = true;
        i = end;
    }
    changed
}

/// Runs SCCP / DFA constant propagation on `op_array`. Returns `true` if any substitution was made.
pub(crate) fn run_sccp(op_array: &mut OpArray, ctx: &OptContext<'_>) -> bool {
    let num_tmps = op_array.num_tmps as usize;
    if num_tmps == 0 && op_array.cv_names.is_empty() {
        return false;
    }

    let mut changed = false;
    let refs_possible = cvs_may_be_refs(op_array, ctx);

    // 1. Temporary constant discovery & SSA propagation
    if num_tmps > 0 {
        let mut def_count = vec![0u32; num_tmps + 1];
        let mut lit_def = vec![None; num_tmps + 1];
        let mut tmp_alias = vec![None; num_tmps + 1];

        for op in &op_array.ops {
            if let Some(t) = super::def_tmp(op) {
                let t_idx = t as usize;
                if t_idx <= num_tmps {
                    def_count[t_idx] += 1;
                    match op {
                        Op::Move { src, dst: _ } => match src {
                            Operand::Lit(l) => {
                                lit_def[t_idx] = Some(*l);
                            }
                            Operand::Tmp(s) => {
                                tmp_alias[t_idx] = Some(*s);
                            }
                            _ => {}
                        },
                        // The result of assigning to a reference is BOT:
                        // a typed reference coerces the value (sccp.c).
                        Op::AssignCv {
                            val, dst: Some(_), ..
                        } if !refs_possible => match val {
                            Operand::Lit(l) => {
                                lit_def[t_idx] = Some(*l);
                            }
                            Operand::Tmp(s) => {
                                tmp_alias[t_idx] = Some(*s);
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                }
            }
        }

        for t in 0..=num_tmps {
            if def_count[t] != 1 {
                lit_def[t] = None;
                tmp_alias[t] = None;
            }
        }

        // Resolve alias chains (e.g. t2 = t1 where t1 is Lit(l))
        for t in 0..=num_tmps {
            if lit_def[t].is_none() && tmp_alias[t].is_some() {
                let mut cur = tmp_alias[t];
                let mut hops = 0;
                while let Some(alias) = cur {
                    if hops >= 16 {
                        break;
                    }
                    hops += 1;
                    let a_idx = alias as usize;
                    if a_idx <= num_tmps {
                        if let Some(lit) = lit_def[a_idx] {
                            lit_def[t] = Some(lit);
                            break;
                        }
                        cur = tmp_alias[a_idx];
                    } else {
                        break;
                    }
                }
            }
        }

        // Fold pure expressions on known temporary constants
        for op in &mut op_array.ops {
            match op {
                Op::Bin {
                    op: bkind,
                    a,
                    b,
                    dst,
                } => {
                    let lit_a = match a {
                        Operand::Lit(l) => Some(*l),
                        Operand::Tmp(s) => lit_def.get(*s as usize).copied().flatten(),
                        _ => None,
                    };
                    let lit_b = match b {
                        Operand::Lit(l) => Some(*l),
                        Operand::Tmp(s) => lit_def.get(*s as usize).copied().flatten(),
                        _ => None,
                    };
                    if let (Some(la), Some(lb)) = (lit_a, lit_b) {
                        if let (Some(va), Some(vb)) = (
                            op_array.literals.get(la as usize).cloned(),
                            op_array.literals.get(lb as usize).cloned(),
                        ) {
                            if let Some(val) = fold_bin_op(*bkind, &va, &vb) {
                                let lit = add_or_find_literal(&mut op_array.literals, val);
                                if (*dst as usize) <= num_tmps {
                                    lit_def[*dst as usize] = Some(lit);
                                }
                                *op = Op::Move {
                                    src: Operand::Lit(lit),
                                    dst: *dst,
                                };
                                changed = true;
                            }
                        }
                    } else if *bkind == BinKind::Arith(BinaryOp::Concat) {
                        if let Some(la) = lit_a {
                            if matches!(op_array.literals.get(la as usize), Some(Value::Str(s)) if s.is_empty())
                            {
                                *op = Op::Cast {
                                    kind: CastKind::String,
                                    a: *b,
                                    dst: *dst,
                                };
                                changed = true;
                            }
                        } else if let Some(lb) = lit_b {
                            if matches!(op_array.literals.get(lb as usize), Some(Value::Str(s)) if s.is_empty())
                            {
                                *op = Op::Cast {
                                    kind: CastKind::String,
                                    a: *a,
                                    dst: *dst,
                                };
                                changed = true;
                            }
                        }
                    } else if *bkind == BinKind::Identical {
                        if let Some(lb) = lit_b {
                            if matches!(op_array.literals.get(lb as usize), Some(Value::Null)) {
                                *op = Op::TypeCheck {
                                    a: *a,
                                    mask: type_mask::NULL,
                                    dst: Some(*dst),
                                };
                                changed = true;
                            }
                        } else if let Some(la) = lit_a {
                            if matches!(op_array.literals.get(la as usize), Some(Value::Null)) {
                                *op = Op::TypeCheck {
                                    a: *b,
                                    mask: type_mask::NULL,
                                    dst: Some(*dst),
                                };
                                changed = true;
                            }
                        }
                    }
                }
                Op::BinAssignCv {
                    op: bkind,
                    a,
                    b,
                    dst,
                    cv,
                } => {
                    let lit_a = match a {
                        Operand::Lit(l) => Some(*l),
                        Operand::Tmp(s) => lit_def.get(*s as usize).copied().flatten(),
                        _ => None,
                    };
                    let lit_b = match b {
                        Operand::Lit(l) => Some(*l),
                        Operand::Tmp(s) => lit_def.get(*s as usize).copied().flatten(),
                        _ => None,
                    };
                    if let (Some(la), Some(lb)) = (lit_a, lit_b) {
                        if let (Some(va), Some(vb)) = (
                            op_array.literals.get(la as usize).cloned(),
                            op_array.literals.get(lb as usize).cloned(),
                        ) {
                            if let Some(val) = fold_bin_op(*bkind, &va, &vb) {
                                let lit = add_or_find_literal(&mut op_array.literals, val);
                                if (*dst as usize) <= num_tmps {
                                    lit_def[*dst as usize] = Some(lit);
                                }
                                *op = Op::AssignCv {
                                    cv: *cv,
                                    val: Operand::Lit(lit),
                                    dst: Some(*dst),
                                };
                                changed = true;
                            }
                        }
                    }
                }
                Op::Unary { op: ukind, a, dst } => {
                    let lit_a = match a {
                        Operand::Lit(l) => Some(*l),
                        Operand::Tmp(s) => lit_def.get(*s as usize).copied().flatten(),
                        _ => None,
                    };
                    if let Some(la) = lit_a {
                        if let Some(va) = op_array.literals.get(la as usize).cloned() {
                            if let Some(val) = fold_unary_op(*ukind, &va) {
                                let lit = add_or_find_literal(&mut op_array.literals, val);
                                if (*dst as usize) <= num_tmps {
                                    lit_def[*dst as usize] = Some(lit);
                                }
                                *op = Op::Move {
                                    src: Operand::Lit(lit),
                                    dst: *dst,
                                };
                                changed = true;
                            }
                        }
                    }
                }
                Op::Cast { kind, a, dst } => {
                    let lit_a = match a {
                        Operand::Lit(l) => Some(*l),
                        Operand::Tmp(s) => lit_def.get(*s as usize).copied().flatten(),
                        _ => None,
                    };
                    if let Some(la) = lit_a {
                        if let Some(va) = op_array.literals.get(la as usize).cloned() {
                            if let Some(val) = fold_cast(*kind, &va) {
                                let lit = add_or_find_literal(&mut op_array.literals, val);
                                if (*dst as usize) <= num_tmps {
                                    lit_def[*dst as usize] = Some(lit);
                                }
                                *op = Op::Move {
                                    src: Operand::Lit(lit),
                                    dst: *dst,
                                };
                                changed = true;
                            }
                        }
                    }
                }
                Op::Strlen { a, dst } => {
                    let lit_a = match a {
                        Operand::Lit(l) => Some(*l),
                        Operand::Tmp(s) => lit_def.get(*s as usize).copied().flatten(),
                        _ => None,
                    };
                    if let Some(la) = lit_a {
                        if let Some(va) = op_array.literals.get(la as usize) {
                            if let Some(len) = fold_strlen(va) {
                                let lit =
                                    add_or_find_literal(&mut op_array.literals, Value::Int(len));
                                if let Some(d) = dst {
                                    if (*d as usize) <= num_tmps {
                                        lit_def[*d as usize] = Some(lit);
                                    }
                                    *op = Op::Move {
                                        src: Operand::Lit(lit),
                                        dst: *d,
                                    };
                                } else {
                                    *op = Op::Nop;
                                }
                                changed = true;
                            }
                        }
                    }
                }
                Op::TypeCheck { a, mask, dst } => {
                    let lit_a = match a {
                        Operand::Lit(l) => Some(*l),
                        Operand::Tmp(s) => lit_def.get(*s as usize).copied().flatten(),
                        _ => None,
                    };
                    if let Some(la) = lit_a {
                        if let Some(va) = op_array.literals.get(la as usize) {
                            let res = type_mask::matches(*mask, va);
                            let lit = add_or_find_literal(&mut op_array.literals, Value::Bool(res));
                            if let Some(d) = dst {
                                if (*d as usize) <= num_tmps {
                                    lit_def[*d as usize] = Some(lit);
                                }
                                *op = Op::Move {
                                    src: Operand::Lit(lit),
                                    dst: *d,
                                };
                            } else {
                                *op = Op::Nop;
                            }
                            changed = true;
                        }
                    }
                }
                // `"abc"[-1]`, `[1, 2][0]`: a read of a literal container
                // that cannot warn.
                Op::Fetch { place, mode, dst }
                    if def_count.get(*dst as usize).copied() == Some(1) =>
                {
                    let folded = op_array.places.get(*place as usize).and_then(|p| {
                        let PlaceBase::Value(Operand::Lit(c)) = &p.base else {
                            return None;
                        };
                        let d = single_dim_lit(p)?;
                        fold_fetch_dim(
                            op_array.literals.get(*c as usize)?,
                            op_array.literals.get(d as usize)?,
                            *mode,
                        )
                    });
                    if let Some(val) = folded {
                        let lit = add_or_find_literal(&mut op_array.literals, val);
                        lit_def[*dst as usize] = Some(lit);
                        *op = Op::Move {
                            src: Operand::Lit(lit),
                            dst: *dst,
                        };
                        changed = true;
                    }
                }
                // `isset("abc"[1])`, `empty("abc"[3])`.
                Op::Isset { place, dst } | Op::Empty { place, dst }
                    if def_count.get(*dst as usize).copied() == Some(1) =>
                {
                    let (place, dst) = (*place, *dst);
                    let empty = matches!(op, Op::Empty { .. });
                    let folded = op_array.places.get(place as usize).and_then(|p| {
                        let (c, d) = isset_operands(p, &[])?;
                        fold_isset_dim(
                            op_array.literals.get(c as usize)?,
                            op_array.literals.get(d as usize)?,
                            empty,
                        )
                    });
                    if let Some(res) = folded {
                        let lit = add_or_find_literal(&mut op_array.literals, Value::Bool(res));
                        lit_def[dst as usize] = Some(lit);
                        *op = Op::Move {
                            src: Operand::Lit(lit),
                            dst,
                        };
                        changed = true;
                    }
                }
                Op::Abs { src, dst, to } => {
                    let lit_src = match src {
                        Operand::Lit(l) => Some(*l),
                        Operand::Tmp(s) => lit_def.get(*s as usize).copied().flatten(),
                        _ => None,
                    };
                    if let Some(ls) = lit_src {
                        if let Some(va) = op_array.literals.get(ls as usize) {
                            if let Some(val) = fold_abs(va) {
                                let lit = add_or_find_literal(&mut op_array.literals, val);
                                if let Some(d) = dst {
                                    if (*d as usize) <= num_tmps {
                                        lit_def[*d as usize] = Some(lit);
                                    }
                                }
                                match (*to, *dst) {
                                    (Some(cv), d) => {
                                        *op = Op::AssignCv {
                                            cv,
                                            val: Operand::Lit(lit),
                                            dst: d,
                                        };
                                    }
                                    (None, Some(d)) => {
                                        *op = Op::Move {
                                            src: Operand::Lit(lit),
                                            dst: d,
                                        };
                                    }
                                    (None, None) => {
                                        *op = Op::Nop;
                                    }
                                }
                                changed = true;
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        changed |= fold_const_func_calls(op_array, &def_count, &mut lit_def);
        changed |= fold_const_array_builds(op_array, &def_count, &mut lit_def);

        // Substitute known temporary constants: every use, including the
        // ones that keep the temporary alive (`JMP_NULL`, `FETCH_LIST_R`,
        // switch subjects), since the defining `QM_ASSIGN` goes below
        // (zend_optimizer_replace_by_const; opt/block_pass_003, 004).
        if lit_def.iter().any(|d| d.is_some()) {
            for op in &mut op_array.ops {
                for_each_operand_mut(op, |operand| {
                    // A kept read (`TmpKeep`, e.g. each arm of a `match`)
                    // too: the defining `Move` is removed below.
                    if let Operand::Tmp(t) | Operand::TmpKeep(t) = operand {
                        if let Some(Some(l)) = lit_def.get(*t as usize) {
                            *operand = Operand::Lit(*l);
                            changed = true;
                        }
                    }
                });
            }

            for place in &mut op_array.places {
                for_each_place_operand_mut(place, |operand| {
                    // A kept read (`TmpKeep`, e.g. each arm of a `match`)
                    // too: the defining `Move` is removed below.
                    if let Operand::Tmp(t) | Operand::TmpKeep(t) = operand {
                        if let Some(Some(l)) = lit_def.get(*t as usize) {
                            *operand = Operand::Lit(*l);
                            changed = true;
                        }
                    }
                });
            }

            for op in &mut op_array.ops {
                if let Op::Move {
                    src: Operand::Lit(_),
                    dst,
                } = op
                {
                    if let Some(Some(_)) = lit_def.get(*dst as usize) {
                        *op = Op::Nop;
                        changed = true;
                    }
                }
            }
        }
    }

    // Branch simplification for constant conditions
    changed |= simplify_constant_branches(&mut op_array.ops, &op_array.literals);

    // 2. Inter-block Forward DFA for CVs. Like php-src, which builds no SSA
    // for a function with try/catch/finally (`zend_dfa_analyze_op_array`),
    // skip it there: an exception leaves a block at any op, so the catch
    // sees a CV's value from the middle of the try block, and a `finally`
    // returns to the op after its `FastCall` with the values it assigned.
    let has_hazard = refs_possible || !op_array.try_table.is_empty();

    let num_cvs = op_array.cv_names.len();
    let len = op_array.ops.len();

    if !has_hazard && num_cvs > 0 && len > 0 {
        let leaders = super::block_pass::find_leaders(op_array);
        let mut block_starts = Vec::new();
        for i in 0..len {
            if leaders[i] {
                block_starts.push(i);
            }
        }
        block_starts.push(len);
        let num_blocks = block_starts.len() - 1;

        let mut inst_to_block = vec![0usize; len + 1];
        for b in 0..num_blocks {
            for i in block_starts[b]..block_starts[b + 1] {
                inst_to_block[i] = b;
            }
        }
        inst_to_block[len] = num_blocks;

        // Every CV is overdefined at entry: a parameter holds an unknown
        // argument, and any other CV is undefined, a real value whose read
        // warns "Undefined variable". Starting it at Top would let a loop
        // merge the entry path with a later assignment and fold the
        // first-iteration read into that constant (jit/assign_047).
        let entry_state: Vec<CvLattice> = vec![CvLattice::Bottom; num_cvs];

        let mut in_state = vec![vec![CvLattice::Top; num_cvs]; num_blocks];
        let mut out_state = vec![vec![CvLattice::Top; num_cvs]; num_blocks];
        let mut block_reachable = vec![false; num_blocks];

        in_state[0] = entry_state;
        block_reachable[0] = true;

        let mut worklist: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
        worklist.push_back(0);
        let mut in_worklist = vec![false; num_blocks];
        in_worklist[0] = true;

        while let Some(b) = worklist.pop_front() {
            in_worklist[b] = false;
            let b_start = block_starts[b];
            let b_end = block_starts[b + 1];
            let mut cur_state = in_state[b].clone();

            for i in b_start..b_end {
                let op = &op_array.ops[i];
                let is_call = matches!(
                    op,
                    Op::DoCall { .. }
                        | Op::InitFCall { .. }
                        | Op::InitDynCall { .. }
                        | Op::InitMethodCall { .. }
                        | Op::InitStaticCall { .. }
                );
                if is_call {
                    cur_state.fill(CvLattice::Bottom);
                    continue;
                }

                update_cv_state_for_op(
                    op,
                    &op_array.places,
                    num_cvs,
                    &mut cur_state,
                    &mut op_array.literals,
                );
            }

            out_state[b] = cur_state;

            // Determine executable successors of block b
            let last_idx = (b_start..b_end)
                .rev()
                .find(|&i| !matches!(op_array.ops[i], Op::Nop));
            let mut successors = Vec::new();

            if let Some(idx) = last_idx {
                match &op_array.ops[idx] {
                    Op::Jmp(t) => {
                        if (*t as usize) < len {
                            successors.push(inst_to_block[*t as usize]);
                        }
                    }
                    Op::Return { .. }
                    | Op::ReturnRef { .. }
                    | Op::ReturnByRefValue { .. }
                    | Op::Throw(_)
                    | Op::Rethrow
                    | Op::Exit(_)
                    | Op::FastRet { .. } => {
                        // No successors
                    }
                    Op::JmpZ(cond, t) => {
                        let cond_val = match cond {
                            Operand::Lit(l) => Some(CvLattice::Const(*l)),
                            Operand::Cv(cv) if (*cv as usize) < num_cvs => {
                                Some(out_state[b][*cv as usize])
                            }
                            _ => None,
                        };
                        let target_block = if (*t as usize) < len {
                            Some(inst_to_block[*t as usize])
                        } else {
                            None
                        };
                        let fallthrough = if b + 1 < num_blocks {
                            Some(b + 1)
                        } else {
                            None
                        };

                        match cond_val {
                            Some(CvLattice::Const(l)) => {
                                if let Some(va) = op_array.literals.get(l as usize) {
                                    let truthy = const_truthiness(va);
                                    if truthy == Some(false) {
                                        if let Some(tb) = target_block {
                                            successors.push(tb);
                                        }
                                    } else if truthy == Some(true) {
                                        if let Some(fb) = fallthrough {
                                            successors.push(fb);
                                        }
                                    } else {
                                        if let Some(tb) = target_block {
                                            successors.push(tb);
                                        }
                                        if let Some(fb) = fallthrough {
                                            successors.push(fb);
                                        }
                                    }
                                } else {
                                    if let Some(tb) = target_block {
                                        successors.push(tb);
                                    }
                                    if let Some(fb) = fallthrough {
                                        successors.push(fb);
                                    }
                                }
                            }
                            _ => {
                                if let Some(tb) = target_block {
                                    successors.push(tb);
                                }
                                if let Some(fb) = fallthrough {
                                    successors.push(fb);
                                }
                            }
                        }
                    }
                    Op::JmpNz(cond, t) => {
                        let cond_val = match cond {
                            Operand::Lit(l) => Some(CvLattice::Const(*l)),
                            Operand::Cv(cv) if (*cv as usize) < num_cvs => {
                                Some(out_state[b][*cv as usize])
                            }
                            _ => None,
                        };
                        let target_block = if (*t as usize) < len {
                            Some(inst_to_block[*t as usize])
                        } else {
                            None
                        };
                        let fallthrough = if b + 1 < num_blocks {
                            Some(b + 1)
                        } else {
                            None
                        };

                        match cond_val {
                            Some(CvLattice::Const(l)) => {
                                if let Some(va) = op_array.literals.get(l as usize) {
                                    let truthy = const_truthiness(va);
                                    if truthy == Some(true) {
                                        if let Some(tb) = target_block {
                                            successors.push(tb);
                                        }
                                    } else if truthy == Some(false) {
                                        if let Some(fb) = fallthrough {
                                            successors.push(fb);
                                        }
                                    } else {
                                        if let Some(tb) = target_block {
                                            successors.push(tb);
                                        }
                                        if let Some(fb) = fallthrough {
                                            successors.push(fb);
                                        }
                                    }
                                } else {
                                    if let Some(tb) = target_block {
                                        successors.push(tb);
                                    }
                                    if let Some(fb) = fallthrough {
                                        successors.push(fb);
                                    }
                                }
                            }
                            _ => {
                                if let Some(tb) = target_block {
                                    successors.push(tb);
                                }
                                if let Some(fb) = fallthrough {
                                    successors.push(fb);
                                }
                            }
                        }
                    }
                    Op::CmpJmp { target: t, .. }
                    | Op::JmpZEx { target: t, .. }
                    | Op::JmpNzEx { target: t, .. }
                    | Op::JmpSet { target: t, .. }
                    | Op::Coalesce { target: t, .. }
                    | Op::JmpNull { target: t, .. }
                    | Op::FastCall { target: t, .. }
                    | Op::Catch { next: t, .. }
                    | Op::FeReset { end: t, .. }
                    | Op::FeFetch { end: t, .. }
                    | Op::FeFreeOnReturn { loop_at: t, .. }
                    | Op::AssertCheck { skip: t, .. }
                    | Op::BindStatic { skip: t, .. }
                    | Op::RecvInit { skip: t, .. } => {
                        if (*t as usize) < len {
                            successors.push(inst_to_block[*t as usize]);
                        }
                        if b + 1 < num_blocks {
                            successors.push(b + 1);
                        }
                    }
                    Op::Match { table, .. } => {
                        // No fall-through: every table target and the default.
                        for t in table.labels() {
                            if (t as usize) < len {
                                successors.push(inst_to_block[t as usize]);
                            }
                        }
                    }
                    Op::New { skip: t, .. } => {
                        if *t != 0 && (*t as usize) < len {
                            successors.push(inst_to_block[*t as usize]);
                        }
                        if b + 1 < num_blocks {
                            successors.push(b + 1);
                        }
                    }
                    _ => {
                        if b + 1 < num_blocks {
                            successors.push(b + 1);
                        }
                    }
                }
            } else if b + 1 < num_blocks {
                successors.push(b + 1);
            }

            // Check try_table entries overlapping block b
            for entry in &op_array.try_table {
                let try_start = entry.try_start as usize;
                let try_end = entry.try_end as usize;
                if b_start < try_end && b_end > try_start {
                    if let Some(cs) = entry.catch_start {
                        if (cs as usize) < len {
                            successors.push(inst_to_block[cs as usize]);
                        }
                    }
                    if let Some((_, f_start, _)) = entry.finally {
                        if (f_start as usize) < len {
                            successors.push(inst_to_block[f_start as usize]);
                        }
                    }
                }
            }

            for succ in successors {
                if succ >= num_blocks {
                    continue;
                }
                let is_first = !block_reachable[succ];
                block_reachable[succ] = true;

                let mut succ_changed = is_first;
                for cv in 0..num_cvs {
                    let out_val = out_state[b][cv];
                    let in_val = in_state[succ][cv];
                    let merged = if is_first {
                        out_val
                    } else {
                        meet_cv(in_val, out_val, &op_array.literals)
                    };
                    if merged != in_val {
                        in_state[succ][cv] = merged;
                        succ_changed = true;
                    }
                }

                if succ_changed && !in_worklist[succ] {
                    worklist.push_back(succ);
                    in_worklist[succ] = true;
                }
            }
        }

        // Apply constant CVs across all reachable blocks
        for b in 0..num_blocks {
            if !block_reachable[b] {
                continue;
            }
            let b_start = block_starts[b];
            let b_end = block_starts[b + 1];
            let mut cur_state = in_state[b].clone();

            for i in b_start..b_end {
                let op = &mut op_array.ops[i];
                let is_call = matches!(
                    op,
                    Op::DoCall { .. }
                        | Op::InitFCall { .. }
                        | Op::InitDynCall { .. }
                        | Op::InitMethodCall { .. }
                        | Op::InitStaticCall { .. }
                );
                if is_call {
                    cur_state.fill(CvLattice::Bottom);
                    continue;
                }

                // Substitute Cv reads with known constants
                for_each_operand_mut(op, |operand| {
                    if let Operand::Cv(cv) = operand {
                        if let Some(CvLattice::Const(l)) = cur_state.get(*cv as usize) {
                            *operand = Operand::Lit(*l);
                            changed = true;
                        }
                    }
                });

                // Substitute Cv reads in the dimensions of the op's place:
                // they are read when the op runs. Integer constants only:
                // a literal string offset is expected in canonical form.
                let read_place = match op {
                    Op::Fetch { place, .. }
                    | Op::Isset { place, .. }
                    | Op::Empty { place, .. }
                    | Op::Assign { place, .. }
                    | Op::AssignOp { place, .. }
                    | Op::IncDec { place, .. }
                    | Op::Unset { place } => Some(*place),
                    _ => None,
                };
                let place_mut = match read_place {
                    Some(pid) => op_array.places.get_mut(pid as usize),
                    None => None,
                };
                if let Some(p) = place_mut {
                    for seg in &mut p.segs {
                        let Seg::Dim(operand) = seg else {
                            continue;
                        };
                        let Operand::Cv(cv) = *operand else {
                            continue;
                        };
                        if let Some(CvLattice::Const(l)) = cur_state.get(cv as usize) {
                            if matches!(op_array.literals.get(*l as usize), Some(Value::Int(_))) {
                                *operand = Operand::Lit(*l);
                                changed = true;
                            }
                        }
                    }
                }

                // `$cv op= c` / `++$cv` of a constant CV: a plain
                // assignment of the result (the CV is no reference here).
                if let Some((cv, Some(lit))) =
                    plain_cv_update(op, &op_array.places, &cur_state, &mut op_array.literals)
                {
                    let dst = match op {
                        Op::AssignOp { dst, .. }
                        | Op::IncDec {
                            kind: IncDecKind::PreInc | IncDecKind::PreDec,
                            dst,
                            ..
                        } => Some(*dst),
                        Op::IncDec { dst: None, .. } => Some(None),
                        _ => None,
                    };
                    if let Some(dst) = dst {
                        *op = Op::AssignCv {
                            cv,
                            val: Operand::Lit(lit),
                            dst,
                        };
                        changed = true;
                    }
                }

                // Fold newly constant operations
                match op {
                    // `$cv[c]` of a CV holding a known array or string.
                    Op::Fetch { place, mode, dst } => {
                        let folded = op_array.places.get(*place as usize).and_then(|p| {
                            let PlaceBase::Cv(c) = p.base else {
                                return None;
                            };
                            let CvLattice::Const(cl) = *cur_state.get(c as usize)? else {
                                return None;
                            };
                            let d = single_dim_lit(p)?;
                            fold_fetch_dim(
                                op_array.literals.get(cl as usize)?,
                                op_array.literals.get(d as usize)?,
                                *mode,
                            )
                        });
                        if let Some(val) = folded {
                            let lit = add_or_find_literal(&mut op_array.literals, val);
                            *op = Op::Move {
                                src: Operand::Lit(lit),
                                dst: *dst,
                            };
                            changed = true;
                        }
                    }
                    // `isset($cv[c])` / `empty("abc"[$k])` of known values.
                    Op::Isset { place, dst } | Op::Empty { place, dst } => {
                        let (place, dst) = (*place, *dst);
                        let empty = matches!(op, Op::Empty { .. });
                        let folded = op_array.places.get(place as usize).and_then(|p| {
                            let (c, d) = isset_operands(p, &cur_state)?;
                            fold_isset_dim(
                                op_array.literals.get(c as usize)?,
                                op_array.literals.get(d as usize)?,
                                empty,
                            )
                        });
                        if let Some(res) = folded {
                            let lit = add_or_find_literal(&mut op_array.literals, Value::Bool(res));
                            *op = Op::Move {
                                src: Operand::Lit(lit),
                                dst,
                            };
                            changed = true;
                        }
                    }
                    // `ZEND_MATCH` of a known value: `JMP` to its arm.
                    Op::Match {
                        subject: Operand::Lit(l),
                        table,
                    } => {
                        if let Some(va) = op_array.literals.get(*l as usize) {
                            let t = table.target(va);
                            *op = Op::Jmp(t);
                            changed = true;
                        }
                    }
                    Op::JmpZ(Operand::Lit(l), target) => {
                        if let Some(va) = op_array.literals.get(*l as usize) {
                            if let Some(truthy) = const_truthiness(va) {
                                *op = if !truthy { Op::Jmp(*target) } else { Op::Nop };
                                changed = true;
                            }
                        }
                    }
                    Op::JmpNz(Operand::Lit(l), target) => {
                        if let Some(va) = op_array.literals.get(*l as usize) {
                            if let Some(truthy) = const_truthiness(va) {
                                *op = if truthy { Op::Jmp(*target) } else { Op::Nop };
                                changed = true;
                            }
                        }
                    }
                    Op::CmpJmp {
                        op: bkind,
                        a: Operand::Lit(la),
                        b: Operand::Lit(lb),
                        target,
                        jmp_if,
                        ..
                    } => {
                        if let (Some(va), Some(vb)) = (
                            op_array.literals.get(*la as usize).cloned(),
                            op_array.literals.get(*lb as usize).cloned(),
                        ) {
                            if let Some(Value::Bool(res)) = fold_bin_op(*bkind, &va, &vb) {
                                *op = if res == *jmp_if {
                                    Op::Jmp(*target)
                                } else {
                                    Op::Nop
                                };
                                changed = true;
                            }
                        }
                    }
                    Op::Abs {
                        src: Operand::Lit(ls),
                        dst,
                        to,
                    } => {
                        if let Some(va) = op_array.literals.get(*ls as usize) {
                            if let Some(val) = fold_abs(va) {
                                let lit = add_or_find_literal(&mut op_array.literals, val);
                                *op = match (*to, *dst) {
                                    (Some(cv), d) => Op::AssignCv {
                                        cv,
                                        val: Operand::Lit(lit),
                                        dst: d,
                                    },
                                    (None, Some(d)) => Op::Move {
                                        src: Operand::Lit(lit),
                                        dst: d,
                                    },
                                    (None, None) => Op::Nop,
                                };
                                changed = true;
                            }
                        }
                    }
                    Op::Strlen {
                        a: Operand::Lit(la),
                        dst,
                    } => {
                        if let Some(va) = op_array.literals.get(*la as usize) {
                            if let Some(len) = fold_strlen(va) {
                                let lit =
                                    add_or_find_literal(&mut op_array.literals, Value::Int(len));
                                *op = match dst {
                                    Some(d) => Op::Move {
                                        src: Operand::Lit(lit),
                                        dst: *d,
                                    },
                                    None => Op::Nop,
                                };
                                changed = true;
                            }
                        }
                    }
                    Op::Unary {
                        op: ukind,
                        a: Operand::Lit(la),
                        dst,
                    } => {
                        if let Some(va) = op_array.literals.get(*la as usize).cloned() {
                            if let Some(val) = fold_unary_op(*ukind, &va) {
                                let lit = add_or_find_literal(&mut op_array.literals, val);
                                *op = Op::Move {
                                    src: Operand::Lit(lit),
                                    dst: *dst,
                                };
                                changed = true;
                            }
                        }
                    }
                    Op::Bin {
                        op: bkind,
                        a: Operand::Lit(la),
                        b: Operand::Lit(lb),
                        dst,
                    } => {
                        if let (Some(va), Some(vb)) = (
                            op_array.literals.get(*la as usize).cloned(),
                            op_array.literals.get(*lb as usize).cloned(),
                        ) {
                            if let Some(val) = fold_bin_op(*bkind, &va, &vb) {
                                let lit = add_or_find_literal(&mut op_array.literals, val);
                                *op = Op::Move {
                                    src: Operand::Lit(lit),
                                    dst: *dst,
                                };
                                changed = true;
                            }
                        }
                    }
                    Op::BinAssignCv {
                        op: bkind,
                        a: Operand::Lit(la),
                        b: Operand::Lit(lb),
                        dst,
                        cv,
                    } => {
                        if let (Some(va), Some(vb)) = (
                            op_array.literals.get(*la as usize).cloned(),
                            op_array.literals.get(*lb as usize).cloned(),
                        ) {
                            if let Some(val) = fold_bin_op(*bkind, &va, &vb) {
                                let lit = add_or_find_literal(&mut op_array.literals, val);
                                *op = Op::AssignCv {
                                    cv: *cv,
                                    val: Operand::Lit(lit),
                                    dst: Some(*dst),
                                };
                                changed = true;
                            }
                        }
                    }
                    _ => {}
                }

                // Update CV state
                update_cv_state_for_op(
                    op,
                    &op_array.places,
                    num_cvs,
                    &mut cur_state,
                    &mut op_array.literals,
                );
            }
        }

        changed |= simplify_constant_branches(&mut op_array.ops, &op_array.literals);
    }

    // 3. A literal store that propagation left unread (php-src's DCE
    // removes it with the dead SSA variable it defines).
    changed |= super::dce::remove_dead_entry_stores(op_array, ctx, refs_possible);

    changed
}

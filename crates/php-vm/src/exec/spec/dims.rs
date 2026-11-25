// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Frame-local fast paths of the specialised dimension ops and of `.=` on
//! a compiled variable ([`Op::FetchDimCv`], [`Op::FetchDimsCv`],
//! [`Op::IssetDimCv`], [`Op::AssignDimCv`], [`Op::AssignDimsCv`],
//! [`Op::AssignOpDimCv`], [`Op::AssignOpDimsCv`], [`Op::IncDecDimCv`],
//! [`Op::ConcatCv`]) and of `foreach` by value over an array
//! ([`Op::FeFetch`]): php-src's `ZEND_FETCH_DIM_R_SPEC_CV_*`,
//! `ZEND_ISSET_ISEMPTY_DIM_OBJ_SPEC_CV_*`,
//! `ZEND_ASSIGN_DIM_SPEC_CV_*_OP_DATA_*`, `ZEND_ASSIGN_DIM_OP_SPEC_CV_*`,
//! `FETCH_DIM_RW` + `PRE/POST_INC` and `ZEND_ASSIGN_OP_SPEC_CV_*` with
//! `concat_function` on an array (or string) container and an int or
//! string offset, and `ZEND_FE_FETCH_R` on an array.
//!
//! Each function works on the current frame alone (its variables,
//! temporaries and literals; a frame whose symbol table is attached gets
//! each written variable mirrored into it): it completes the op without
//! any diagnostic, user code or frame change, or changes nothing (beyond
//! a separation the generic write repeats as a no-op) and reports that the
//! generic op must run. That makes them hot ops of the frame-local loop
//! (`exec/hot.rs`), where a loop such as `for (...) { $s = $a[$i & 1023]; }`
//! stays with its frame and `ip` in registers, as php-src's handlers keep
//! `execute_data` and `opline` in machine registers.
//!
//! [`Op::FetchDimCv`]: crate::opcode::Op::FetchDimCv
//! [`Op::FetchDimsCv`]: crate::opcode::Op::FetchDimsCv
//! [`Op::IssetDimCv`]: crate::opcode::Op::IssetDimCv
//! [`Op::AssignDimCv`]: crate::opcode::Op::AssignDimCv
//! [`Op::AssignDimsCv`]: crate::opcode::Op::AssignDimsCv
//! [`Op::AssignOpDimCv`]: crate::opcode::Op::AssignOpDimCv
//! [`Op::AssignOpDimsCv`]: crate::opcode::Op::AssignOpDimsCv
//! [`Op::IncDecDimCv`]: crate::opcode::Op::IncDecDimCv
//! [`Op::ConcatCv`]: crate::opcode::Op::ConcatCv
//! [`Op::FeFetch`]: crate::opcode::Op::FeFetch

use super::super::{num_bin, Num};
use super::{
    assign_dims, assign_op_dims, copy_value, cv_dim_hit_in, dim_int_in, dim_key_in, dim_ref_in,
    dim_value_in, dims_value_in, dims_value_of, discard_plain, isset_dim_in, plain_value_in,
    put_tmp, release_place_str_tmps, release_str_tmp_in, store_fetched_in, string_offset_char,
    table_dim, table_dim_mut, write_keys_in, DimRef,
};
use crate::opcode::{BinKind, IncDecKind, Op, Operand, PlaceBase, PlaceId, Seg};
use crate::vm::{CvSlot, ForeachIter, Frame, FrameKind, Vm};
use php_runtime::{ArrayKey, BinaryOp, HashTable, Value, ZArray, ZStr};

/// [`Op::FetchDimCv`](crate::opcode::Op::FetchDimCv): `$cv[dim]` read when
/// `$cv` holds an array (directly or through a reference) that has the
/// key: `zend_fetch_dimension_address_inner` with `BP_VAR_R` on a hit; or
/// a string and the offset is an int inside it:
/// `zend_fetch_dimension_str`'s one-byte result (`ZSTR_CHAR`). The value
/// goes to the folded `AssignCv`'s variable `to` (`Some(true)`: the loop
/// skips that op) or to `dst` (`Some(false)`), and a consumed string
/// offset is released. `None` (a miss, an offset that needs converting or
/// lies outside the string, another container) leaves everything to the
/// generic fetch, nothing consumed.
#[inline(always)]
#[allow(clippy::inline_always)] // hot array-dimension read of the dispatch loops
pub(in crate::exec) fn fetch_dim_cv_in(
    f: &mut Frame,
    cv: u32,
    dim: Operand,
    dst: u32,
    to: Option<u32>,
) -> Option<bool> {
    if let Some(v) = cv_dim_hit_in(f, cv, dim) {
        release_str_tmp_in(f, dim);
        return Some(store_fetched_in(f, dst, to, v));
    }
    fetch_dim_cv_other(f, cv, dim, dst, to)
}

/// The rarer hits of [`fetch_dim_cv_in`], out of line: an offset inside a
/// string (`zend_fetch_dimension_str`: an int or canonical integer string
/// offset), and an int offset held by reference (which `dim_ref_in` does
/// not look through) on an array.
#[inline(never)]
fn fetch_dim_cv_other(
    f: &mut Frame,
    cv: u32,
    dim: Operand,
    dst: u32,
    to: Option<u32>,
) -> Option<bool> {
    if let Some(Value::Str(s)) = f.cvs.get(cv as usize).and_then(CvSlot::get) {
        let offset = match dim_ref_in(f, dim)? {
            DimRef::Int(i) => i,
            DimRef::Str(k) => match ArrayKey::from_zstr(k) {
                ArrayKey::Int(i) => i,
                ArrayKey::Str(_) => return None,
            },
        };
        let v = string_offset_char(s, offset)?;
        release_str_tmp_in(f, dim);
        return Some(store_fetched_in(f, dst, to, v));
    }
    let i = dim_int_in(f, dim)?;
    let v = f.peek_cv_dim_int(cv, i)?;
    Some(store_fetched_in(f, dst, to, v))
}

/// [`Op::FetchDimsCv`](crate::opcode::Op::FetchDimsCv): `$cv[d1][d2]...`
/// read when every container on the way is an array (the variable itself
/// possibly through a reference) holding the offset, and every offset
/// peeks as an int or string: the chain of `ZEND_FETCH_DIM_R` hits. Stored
/// and released as in [`fetch_dim_cv_in`]; `None` (nothing consumed)
/// leaves all of it to the generic fetch.
#[inline]
pub(in crate::exec) fn fetch_dims_cv_in(
    f: &mut Frame,
    cv: u32,
    place: PlaceId,
    dst: u32,
    to: Option<u32>,
) -> Option<bool> {
    let v = dims_value_in(f, cv, place)?;
    release_place_str_tmps(f, place);
    Some(store_fetched_in(f, dst, to, v))
}

/// [`Op::IssetDimCv`](crate::opcode::Op::IssetDimCv): `isset($cv[dim])` /
/// `empty($cv[dim])` when `$cv` holds an array (directly or through a
/// reference) and the offset is an int or string (the array fast path of
/// `ZEND_ISSET_ISEMPTY_DIM_OBJ`; `zend_isset_dim_slow` is never reached),
/// the result written to `dst` and a consumed string offset released.
/// `false`, nothing changed, leaves it to the generic `place_isset`.
#[inline(always)]
#[allow(clippy::inline_always)] // hot isset/empty array-dimension check of the dispatch loops
pub(in crate::exec) fn isset_dim_cv_in(
    f: &mut Frame,
    cv: u32,
    dim: Operand,
    dst: u32,
    empty: bool,
) -> bool {
    let Some(r) = isset_dim_in(f, cv, dim, empty) else {
        return false;
    };
    release_str_tmp_in(f, dim);
    let Some(slot) = f.tmps.get_mut(dst as usize) else {
        return false;
    };
    put_tmp(slot, Value::Bool(r));
    true
}

/// [`Op::AssignOpDimCv`](crate::opcode::Op::AssignOpDimCv): `$cv[dim] op=
/// val` when `$cv` holds a non-reference array whose element at the (int
/// or string) offset is a plain number and `val` peeks as a plain number:
/// `ZEND_ASSIGN_DIM_OP` with the `IS_LONG`/`IS_DOUBLE` fast path of
/// `zend_binary_op` on the fetched slot, after `SEPARATE_ARRAY`; `.=` is
/// [`concat_dim_cv_in`]. Returns `false`, having changed nothing,
/// otherwise (a missing key warns and inserts on the generic path).
#[inline(always)]
#[allow(clippy::inline_always)] // hot array-element compound assignment of the dispatch loops
pub(in crate::exec) fn assign_op_dim_cv_in(
    f: &mut Frame,
    cv: u32,
    dim: Operand,
    op: BinaryOp,
    val: Operand,
) -> bool {
    if op == BinaryOp::Concat {
        return concat_dim_cv_in(f, cv, dim, val);
    }
    let Some(key) = dim_ref_in(f, dim) else {
        return false;
    };
    let Some(y) = Vm::peek_num(f, val) else {
        return false;
    };
    let Some(slot) = f.cvs.get(cv as usize) else {
        return false;
    };
    // SAFETY: the CV slot is not otherwise borrowed: `key` is an int or a
    // string held by another operand (an array-holding `$cv` itself is no
    // int or string offset), and `y` is a copied number.
    let val_mut = unsafe { slot.get_mut_unchecked() };
    let Some(Value::Array(arr)) = val_mut else {
        return assign_op_dim_ref_in(f, cv, dim, op, y);
    };
    // The variable's own table (no other handle, no symbol-table mirror):
    // `SEPARATE_ARRAY` is a no-op, so the element is looked up once and
    // updated in place.
    if !f.symtab_attached {
        if let Some(tbl) = arr.get_mut_unshared() {
            let Some(el) = table_dim_mut(tbl, key) else {
                return false;
            };
            let x = match el {
                Value::Int(i) => Num::Int(*i),
                Value::Float(x) => Num::Float(*x),
                _ => return false,
            };
            let Some(r) = num_bin(BinKind::Arith(op), x, y) else {
                return false;
            };
            // A number replaces a number: no drop glue to run.
            std::mem::forget(std::mem::replace(el, r));
            release_str_tmp_in(f, dim);
            return true;
        }
    }
    let x = match table_dim(arr, key) {
        Some(Value::Int(i)) => Num::Int(*i),
        Some(Value::Float(x)) => Num::Float(*x),
        _ => return false,
    };
    let Some(r) = num_bin(BinKind::Arith(op), x, y) else {
        return false;
    };
    f.unsync_cv_from_symtab(cv);
    let Some(slot) = table_dim_mut(arr.make_mut(), key) else {
        if f.symtab_attached {
            f.sync_cv_to_symtab(cv, &Value::Array(arr.clone()));
        }
        return false;
    };
    // A number replaces a number: no drop glue to run.
    std::mem::forget(std::mem::replace(slot, r));
    if f.symtab_attached {
        f.sync_cv_to_symtab(cv, &Value::Array(arr.clone()));
    }
    release_str_tmp_in(f, dim);
    true
}

/// `$cv[dim] .= val` when `$cv` holds a non-reference array whose element
/// at the (int or string) offset is a string and `val` peeks as a string
/// or an int: `ZEND_ASSIGN_DIM_OP` runs `concat_function` on the fetched
/// slot (`result == op1`, after `SEPARATE_ARRAY`), so the string is
/// extended in place when nothing else holds it (`zend_string_extend`) and
/// copied once otherwise (a shared array's element after the separation,
/// `$a[$k] .= $a[$k]`); the digits of an int go straight into it. Returns
/// `false`, having changed nothing, otherwise: a missing key (warning,
/// then insert), an element of another type or held by reference, and a
/// right-hand side of another type (its conversion may warn or call
/// `__toString()`) take the generic path.
#[inline(never)]
fn concat_dim_cv_in(f: &mut Frame, cv: u32, dim: Operand, val: Operand) -> bool {
    let Some(key) = dim_key_in(f, dim) else {
        return false;
    };
    let add = match plain_value_in(f, val) {
        Some(v @ (Value::Str(_) | Value::Int(_))) => v,
        _ => return false,
    };
    let Some(slot) = f.cvs.get(cv as usize) else {
        return false;
    };
    // SAFETY: `key` and `add` are owned copies; nothing else borrows the
    // slot while the array in it is updated.
    let val_mut = unsafe { slot.get_mut_unchecked() };
    let Some(Value::Array(arr)) = val_mut else {
        return false;
    };
    if !matches!(arr.get(&key), Some(Value::Str(_))) {
        return false;
    }
    f.unsync_cv_from_symtab(cv);
    let Some(Value::Str(s)) = arr.make_mut().get_mut(&key) else {
        if f.symtab_attached {
            f.sync_cv_to_symtab(cv, &Value::Array(arr.clone()));
        }
        return false;
    };
    match &add {
        Value::Str(t) => s.append_str(t),
        Value::Int(i) => s.append_long(*i),
        // `add` is a string or an int (checked above).
        _ => {}
    }
    if f.symtab_attached {
        f.sync_cv_to_symtab(cv, &Value::Array(arr.clone()));
    }
    drop(add);
    // The offset and OP_DATA temporaries are consumed.
    release_str_tmp_in(f, dim);
    if let Operand::Tmp(t) = val {
        if let Some(slot) = f.tmps.get_mut(t as usize) {
            put_tmp(slot, Value::Null);
        }
    }
    true
}

/// [`Op::IncDecDimCv`](crate::opcode::Op::IncDecDimCv): `$cv[dim]++` & co.
/// when `$cv` holds a non-reference array whose element at the (int or
/// string) offset is an int that does not overflow, or a float
/// (`FETCH_DIM_RW` + the `IS_LONG`/`IS_DOUBLE` fast path of
/// `PRE/POST_INC/DEC`): the expression's value, or `None` (nothing
/// changed) for the generic path.
#[inline(always)]
#[allow(clippy::inline_always)] // hot array-element increment/decrement of the dispatch loops
pub(in crate::exec) fn incdec_dim_cv_in(
    f: &mut Frame,
    cv: u32,
    dim: Operand,
    kind: IncDecKind,
) -> Option<Value> {
    let key = dim_ref_in(f, dim)?;
    let slot = f.cvs.get(cv as usize)?;
    // SAFETY: as in `assign_op_dim_cv_in`, the offset `key` borrows
    // another operand, never this CV slot.
    let val_mut = unsafe { slot.get_mut_unchecked() };
    let Some(Value::Array(arr)) = val_mut else {
        return incdec_dim_ref_in(f, cv, dim, kind);
    };
    let step = |v: &Value| -> Option<(Value, Value)> {
        Some(match v {
            Value::Int(i) => {
                let i = *i;
                let n = if kind.is_inc() {
                    i.checked_add(1)?
                } else {
                    i.checked_sub(1)?
                };
                (Value::Int(i), Value::Int(n))
            }
            Value::Float(x) => {
                let x = *x;
                let n = if kind.is_inc() { x + 1.0 } else { x - 1.0 };
                (Value::Float(x), Value::Float(n))
            }
            _ => return None,
        })
    };
    // The variable's own table: one lookup, updated in place (see
    // `assign_op_dim_cv_in`).
    if !f.symtab_attached {
        if let Some(tbl) = arr.get_mut_unshared() {
            let el = table_dim_mut(tbl, key)?;
            let (old, new) = step(&*el)?;
            // A number replaces a number: no drop glue to run.
            std::mem::forget(std::mem::replace(el, new.clone()));
            release_str_tmp_in(f, dim);
            return Some(if kind.is_post() { old } else { new });
        }
    }
    let (old, new) = step(table_dim(arr, key)?)?;
    f.unsync_cv_from_symtab(cv);
    let Some(slot) = table_dim_mut(arr.make_mut(), key) else {
        if f.symtab_attached {
            f.sync_cv_to_symtab(cv, &Value::Array(arr.clone()));
        }
        return None;
    };
    // A number replaces a number: no drop glue to run.
    std::mem::forget(std::mem::replace(slot, new.clone()));
    if f.symtab_attached {
        f.sync_cv_to_symtab(cv, &Value::Array(arr.clone()));
    }
    release_str_tmp_in(f, dim);
    Some(if kind.is_post() { old } else { new })
}

/// [`assign_op_dim_cv_in`] (not `.=`) when `$cv` holds a reference to an
/// array whose element at the offset is a plain number: `ZEND_ASSIGN_DIM_OP`
/// on the dereferenced container (`ZVAL_DEREF`, then `SEPARATE_ARRAY` when
/// shared) and `zend_binary_op`'s numeric fast path on the element. Out of
/// line, as [`incdec_dim_ref_in`]; `false`, nothing changed, for the
/// generic path.
#[inline(never)]
fn assign_op_dim_ref_in(f: &mut Frame, cv: u32, dim: Operand, op: BinaryOp, y: Num) -> bool {
    let Some(key) = dim_ref_in(f, dim) else {
        return false;
    };
    let Some(Value::Reference(r)) = f.cvs.get(cv as usize).and_then(CvSlot::get) else {
        return false;
    };
    let Some(mut guard) = r.try_borrow_mut() else {
        return false;
    };
    let Value::Array(arr) = &mut *guard else {
        return false;
    };
    let x = match table_dim(arr, key) {
        Some(Value::Int(i)) => Num::Int(*i),
        Some(Value::Float(x)) => Num::Float(*x),
        _ => return false,
    };
    let Some(res) = num_bin(BinKind::Arith(op), x, y) else {
        return false;
    };
    let Some(el) = table_dim_mut(arr.make_mut(), key) else {
        return false;
    };
    // A number replaces a number: no drop glue to run.
    std::mem::forget(std::mem::replace(el, res));
    drop(guard);
    release_str_tmp_in(f, dim);
    true
}

/// [`incdec_dim_cv_in`] when `$cv` holds a reference (a variable bound by
/// `global` or `static`, or captured or passed by reference) to an array
/// whose element at the (int or string) offset is an int that does not
/// overflow, or a float: `FETCH_DIM_RW` looks through it (`ZVAL_DEREF`)
/// and separates the referenced array only when it is shared
/// (`SEPARATE_ARRAY`), then `PRE/POST_INC/DEC` update the element. Out of
/// line, so that the variable-holds-an-array path stays as it was. `None`,
/// nothing changed, for the generic path (a missing key warns there; a
/// reference borrowed elsewhere is being iterated or walked).
#[inline(never)]
fn incdec_dim_ref_in(f: &mut Frame, cv: u32, dim: Operand, kind: IncDecKind) -> Option<Value> {
    let key = dim_ref_in(f, dim)?;
    let Some(Value::Reference(r)) = f.cvs.get(cv as usize).and_then(CvSlot::get) else {
        return None;
    };
    let mut guard = r.try_borrow_mut()?;
    let Value::Array(arr) = &mut *guard else {
        return None;
    };
    let (old, new) = match table_dim(arr, key)? {
        Value::Int(i) => {
            let i = *i;
            let n = if kind.is_inc() {
                i.checked_add(1)?
            } else {
                i.checked_sub(1)?
            };
            (Value::Int(i), Value::Int(n))
        }
        Value::Float(x) => {
            let x = *x;
            let n = if kind.is_inc() { x + 1.0 } else { x - 1.0 };
            (Value::Float(x), Value::Float(n))
        }
        _ => return None,
    };
    let el = table_dim_mut(arr.make_mut(), key)?;
    // A number replaces a number: no drop glue to run.
    std::mem::forget(std::mem::replace(el, copy_value(&new)));
    drop(guard);
    release_str_tmp_in(f, dim);
    Some(if kind.is_post() { old } else { new })
}

/// `tbl[i] = v` for an int offset (`zend_fetch_dimension_address_inner_W`
/// and `zend_assign_to_variable`): an existing element of a packed table
/// is overwritten where it is (`zend_hash_index_find` on `HT_IS_PACKED`,
/// inline); otherwise the element is found or added
/// (`zend_hash_index_lookup`, as null, then overwritten). The replaced
/// element is released ([`release_elem`]). `false`, nothing changed, when
/// the existing element is a reference (the generic assignment writes
/// through it).
#[inline(always)]
#[allow(clippy::inline_always)] // the packed overwrite is the hot `$a[$i] = $v`
fn assign_int_dim(tbl: &mut HashTable, i: i64, v: Value) -> bool {
    if let Some(slot) = tbl.get_packed_mut(i) {
        if matches!(slot, Value::Reference(_)) {
            return false;
        }
        release_elem(std::mem::replace(slot, v));
        return true;
    }
    assign_int_dim_other(tbl, i, v)
}

/// Releases the element an element write replaced: a plain value
/// ([`discard_plain`]), or an array or object as `zend_assign_to_variable`
/// releases one (a possible collection root; a destructor its last handle
/// queues runs at the next safe point, `object::queue_destructor`).
#[inline(always)]
#[allow(clippy::inline_always)]
fn release_elem(old: Value) {
    if old.is_plain() {
        discard_plain(old);
    } else {
        super::super::release_rooted(Some(old));
    }
}

/// [`assign_int_dim`] past a packed hit: an append or a hash table.
#[inline(never)]
fn assign_int_dim_other(tbl: &mut HashTable, i: i64, v: Value) -> bool {
    let v = match tbl.assign_packed_int(i, v) {
        Ok(old) => {
            if let Some(old) = old {
                release_elem(old);
            }
            return true;
        }
        Err(v) => v,
    };
    let (slot, added) = tbl.lookup_ex(ArrayKey::Int(i));
    if !added && matches!(slot, Value::Reference(_)) {
        return false;
    }
    release_elem(std::mem::replace(slot, v));
    true
}

/// `tbl[s] = v` for a string offset with symbol-table semantics: an
/// existing element is found (`zend_symtable_find`) and overwritten
/// without building a key; a missing one is added under the key the
/// string converts to. The replaced element is released
/// ([`release_elem`]); `false`, nothing changed, when it is a reference.
#[inline(never)]
fn assign_str_dim(tbl: &mut HashTable, s: &ZStr, v: Value) -> bool {
    // A key that is surely no integer in digits (`ZEND_HANDLE_NUMERIC_STR`
    // leaves it alone) is found as it is with its stored hash and without
    // a call (`zend_hash_find_known_hash`); the others are looked at for
    // an integer first.
    let found = if ArrayKey::surely_str(s.as_bytes()) {
        tbl.lean_find_mut(s, s.hash_now())
    } else {
        tbl.get_sym_zstr_mut(s)
    };
    if let Some(slot) = found {
        if matches!(slot, Value::Reference(_)) {
            return false;
        }
        release_elem(std::mem::replace(slot, v));
        return true;
    }
    let (slot, added) = tbl.lookup_ex(ArrayKey::from_zstr(s));
    if !added && matches!(slot, Value::Reference(_)) {
        return false;
    }
    release_elem(std::mem::replace(slot, v));
    true
}

/// The offset of an [`Op::AssignDimCv`](crate::opcode::Op::AssignDimCv).
#[derive(Clone, Copy)]
pub(super) enum WriteDim<'a> {
    /// `$cv[] = val`.
    Append,
    Int(i64),
    Str(&'a ZStr),
}

/// Stores `v` into `tbl` at `dim`; `false` as [`assign_int_dim`] /
/// [`assign_str_dim`], or for an append to a full array (the next index
/// is taken: the generic op throws).
#[inline(always)]
#[allow(clippy::inline_always)]
pub(super) fn assign_write_dim(tbl: &mut HashTable, dim: WriteDim<'_>, v: Value) -> bool {
    match dim {
        WriteDim::Int(i) => assign_int_dim(tbl, i, v),
        WriteDim::Str(s) => assign_str_dim(tbl, s, v),
        WriteDim::Append => tbl.push(v).is_ok(),
    }
}

/// [`assign_write_dim`] into the table of `arr`, separated first when it
/// is shared (`SEPARATE_ARRAY`): a table this handle holds alone is
/// written in place without the call of `Rc::make_mut` (17 instructions
/// and a call around every write).
#[inline(always)]
#[allow(clippy::inline_always)] // two loads and a compare before the write
pub(super) fn assign_write_sep(arr: &mut ZArray, dim: WriteDim<'_>, v: Value) -> bool {
    if arr.get_mut_unshared().is_none() {
        return assign_write_dim(arr.make_mut(), dim, v);
    }
    match arr.get_mut_unshared() {
        Some(tbl) => assign_write_dim(tbl, dim, v),
        // (Not reached: the handle was just found alone.)
        None => false,
    }
}

/// [`Op::AssignDimCv`](crate::opcode::Op::AssignDimCv): `$cv[dim] = val;`
/// when `$cv` holds an array (directly or through a reference), the offset
/// is an int or string, the value is plain, an array or an object, and the
/// element it replaces (if any) is not a reference: the `zend_fetch_dimension_address_inner_W` and
/// `zend_assign_to_variable` path of `ZEND_ASSIGN_DIM` on an array,
/// separating a shared array first (`SEPARATE_ARRAY`): an existing element
/// is overwritten where it is, a missing key added (as null, then
/// overwritten, like the generic walk's `descend`). Returns `false`
/// otherwise, having changed nothing but possibly the separation the
/// generic write repeats as a no-op.
#[inline(always)]
#[allow(clippy::inline_always)] // hot array-dimension write of the dispatch loops
pub(in crate::exec) fn assign_dim_cv_in(
    f: &mut Frame,
    cv: u32,
    dim: Operand,
    val: Operand,
) -> bool {
    let Some(v) = dim_value_in(f, val) else {
        return false;
    };
    // A string offset stays borrowed: an existing element needs no key of
    // its own (only an insertion takes one).
    let wd = if dim == Operand::Unused {
        WriteDim::Append
    } else if let Some(i) = dim_int_in(f, dim) {
        WriteDim::Int(i)
    } else if let Some(DimRef::Str(s)) = dim_ref_in(f, dim) {
        WriteDim::Str(s)
    } else {
        return false;
    };
    let str_dim = matches!(wd, WriteDim::Str(_));
    let Some(slot) = f.cvs.get(cv as usize) else {
        return false;
    };
    // SAFETY: the offset borrows another operand (a string offset is never
    // the array-holding slot itself) and `v` is an owned copy; nothing
    // else reads or writes this slot while the array in it is updated.
    let val_mut = unsafe { slot.get_mut_unchecked() };
    let ok = match val_mut {
        Some(Value::Array(arr)) => {
            if f.symtab_attached {
                f.unsync_cv_from_symtab(cv);
                let ok = assign_write_dim(arr.make_mut(), wd, v);
                f.sync_cv_to_symtab(cv, &Value::Array(arr.clone()));
                ok
            } else {
                assign_write_dim(arr.make_mut(), wd, v)
            }
        }
        Some(Value::Reference(r)) => {
            let Some(mut guard) = r.try_borrow_mut() else {
                return false;
            };
            let Value::Array(arr) = &mut *guard else {
                return false;
            };
            assign_write_dim(arr.make_mut(), wd, v)
        }
        _ => return false,
    };
    if !ok {
        return false;
    }
    if str_dim {
        release_str_tmp_in(f, dim);
    }
    if let Operand::Tmp(t) = val {
        if let Some(slot) = f.tmps.get_mut(t as usize) {
            put_tmp(slot, Value::Null);
        }
    }
    true
}

/// [`Op::AssignDimsCv`](crate::opcode::Op::AssignDimsCv): `$cv[d1][d2]... =
/// val;` when `$cv` holds an array (not a reference), every offset is an
/// int or a string and the value is plain ([`assign_dims`]); the offset
/// and `OP_DATA` temporaries are consumed. Returns `false` otherwise,
/// having changed nothing but possibly separations the generic write
/// repeats.
///
/// What this plain path leaves goes on to [`assign_dims_wide_in`] (arrays
/// and objects written or written over, the array behind a reference),
/// which the generic place walk ran before: 31,000 writes a request of a
/// large framework application's page.
#[inline]
pub(in crate::exec) fn assign_dims_cv_in(
    f: &mut Frame,
    cv: u32,
    place: PlaceId,
    val: Operand,
) -> bool {
    assign_dims_plain_in(f, cv, place, val) || assign_dims_wide_in(f, cv, place, val)
}

/// The plain path of [`assign_dims_cv_in`].
#[inline(always)]
#[allow(clippy::inline_always)] // the body of `assign_dims_cv_in`
fn assign_dims_plain_in(f: &mut Frame, cv: u32, place: PlaceId, val: Operand) -> bool {
    let Some((keys, n)) = write_keys_in(f, place) else {
        return false;
    };
    let Some(v) = plain_value_in(f, val) else {
        return false;
    };
    let Some(slot) = f.cvs.get(cv as usize) else {
        return false;
    };
    // SAFETY: the keys and the value are owned copies; nothing else
    // borrows the slot while the array in it is updated.
    let val_mut = unsafe { slot.get_mut_unchecked() };
    let Some(Value::Array(arr)) = val_mut else {
        return false;
    };
    f.unsync_cv_from_symtab(cv);
    let res = assign_dims(arr.make_mut(), &keys[..n], v);
    if f.symtab_attached {
        f.sync_cv_to_symtab(cv, &Value::Array(arr.clone()));
    }
    let Ok(old) = res else {
        return false;
    };
    discard_plain(old);
    release_place_str_tmps(f, place);
    if let Operand::Tmp(t) = val {
        if let Some(slot) = f.tmps.get_mut(t as usize) {
            put_tmp(slot, Value::Null);
        }
    }
    true
}

/// [`assign_dims_cv_in`] for what it leaves to the generic place walk
/// and C runs on the same fast path (`ZEND_FETCH_DIM_W` along the chain,
/// then `ZEND_ASSIGN_DIM` + `OP_DATA` and `zend_assign_to_variable`): a
/// value that is an array or an object (a new handle of it), an element
/// written over that is an array or an object (released after the store,
/// a possible root; a destructor it owes runs at the row's safe point,
/// before the next op, as C runs it inside the assignment), and the array
/// held through a reference (a `global`, a by-reference parameter). Every
/// offset an int or a string, the containers on the way arrays (each
/// separated when shared) or missing or null (made empty arrays), the
/// element no reference. A frame with a symbol table, an element or a
/// container that is a reference, an object, a string or `false` on the
/// way: `false`, nothing changed but separations and created empty
/// arrays the generic write repeats as no-ops (it would create them too).
#[inline(never)]
fn assign_dims_wide_in(f: &mut Frame, cv: u32, place: PlaceId, val: Operand) -> bool {
    if f.symtab_attached {
        return false;
    }
    let Some((keys, n)) = write_keys_in(f, place) else {
        return false;
    };
    let Some(v) = dim_value_in(f, val) else {
        return false;
    };
    let Some(slot) = f.cvs.get_mut(cv as usize) else {
        return false;
    };
    let res = match slot.as_mut() {
        Some(Value::Array(arr)) => assign_dims_wide(arr.make_mut(), &keys[..n], v),
        Some(Value::Reference(r)) => match r.try_borrow_mut() {
            Some(mut held) => match &mut *held {
                Value::Array(arr) => assign_dims_wide(arr.make_mut(), &keys[..n], v),
                _ => Err(v),
            },
            None => Err(v),
        },
        _ => Err(v),
    };
    let Ok(old) = res else {
        return false;
    };
    if let Some(old) = old {
        release_elem(old);
    }
    release_place_str_tmps(f, place);
    if let Operand::Tmp(t) = val {
        if let Some(slot) = f.tmps.get_mut(t as usize) {
            put_tmp(slot, Value::Null);
        }
    }
    true
}

/// `tbl[k1][k2]... = v` along `keys` (all `Some`) for
/// [`assign_dims_wide_in`]: each container on the way an array
/// (separated) or missing or null (an empty array put there), then `v`
/// stored over any element that is no reference, or added. The old
/// element (`None` for a new key) on success; `v` back otherwise.
fn assign_dims_wide(
    tbl: &mut HashTable,
    keys: &[Option<ArrayKey>],
    v: Value,
) -> Result<Option<Value>, Value> {
    let Some((Some(key), rest)) = keys.split_first() else {
        return Err(v);
    };
    if rest.is_empty() {
        return match tbl.get_mut(key) {
            Some(Value::Reference(_)) => Err(v),
            Some(slot) => Ok(Some(std::mem::replace(slot, v))),
            None => {
                // A new element: the null the lookup adds owns nothing.
                std::mem::forget(std::mem::replace(tbl.lookup_ex(key.clone()).0, v));
                Ok(None)
            }
        };
    }
    let (elem, _) = tbl.lookup_ex(key.clone());
    if matches!(elem, Value::Null) {
        *elem = Value::Array(ZArray::new());
    }
    let Value::Array(inner) = elem else {
        return Err(v);
    };
    assign_dims_wide(inner.make_mut(), rest, v)
}

/// [`Op::AssignOpDimsCv`](crate::opcode::Op::AssignOpDimsCv):
/// `$cv[d1][d2]... op= val;` when `$cv` holds an array (not a reference),
/// every offset is an int or a string, `val` is a plain number and
/// [`assign_op_dims`] applies; the offset temporaries are consumed.
/// Returns `false` otherwise, having changed nothing but possibly
/// separations the generic op repeats.
#[inline]
pub(in crate::exec) fn assign_op_dims_cv_in(
    f: &mut Frame,
    cv: u32,
    op: BinaryOp,
    place: PlaceId,
    val: Operand,
) -> bool {
    let Some((keys, n)) = write_keys_in(f, place) else {
        return false;
    };
    let Some(y) = Vm::peek_num(f, val) else {
        return false;
    };
    let Some(slot) = f.cvs.get(cv as usize) else {
        return false;
    };
    // SAFETY: the keys are owned copies and `y` a copied number.
    let val_mut = unsafe { slot.get_mut_unchecked() };
    let Some(Value::Array(arr)) = val_mut else {
        return false;
    };
    f.unsync_cv_from_symtab(cv);
    let ok = assign_op_dims(arr.make_mut(), &keys[..n], op, y);
    if f.symtab_attached {
        f.sync_cv_to_symtab(cv, &Value::Array(arr.clone()));
    }
    if !ok {
        return false;
    }
    release_place_str_tmps(f, place);
    true
}

/// [`Op::ConcatCv`](crate::opcode::Op::ConcatCv): `$cv .= val` when the
/// variable holds a string directly (not a reference) and `val` is a
/// string or an int (a literal, a defined variable holding one directly,
/// a temporary): `concat_function` with `result == op1`, so the string is
/// extended in place when nothing else holds it (`zend_string_extend`) and
/// copied once otherwise; an int's digits go straight into it. A consumed
/// temporary operand is released and the result written to `dst` when
/// used. Returns `false`, having changed nothing, otherwise: an undefined
/// variable (warning), references and other types (conversions may warn
/// or call `__toString()`).
#[inline(always)]
#[allow(clippy::inline_always)] // hot string append of the dispatch loops
pub(in crate::exec) fn concat_cv_in(
    f: &mut Frame,
    cv: u32,
    val: Operand,
    dst: Option<u32>,
) -> bool {
    let idx = cv as usize;
    if !matches!(f.cvs.get(idx).and_then(CvSlot::get), Some(Value::Str(_))) {
        return false;
    }
    // The right-hand side, checked before anything changes (`None`: the
    // variable itself, `$s .= $s`). A consumed temporary is taken out of
    // its slot and released after the append (`FREE_OP2`).
    let taken;
    let rhs: Option<&Value> = match val {
        Operand::Cv(src) if src == cv => None,
        Operand::Cv(src) => match f.cvs.get(src as usize).and_then(CvSlot::get) {
            Some(v @ (Value::Str(_) | Value::Int(_))) => Some(v),
            _ => return false,
        },
        Operand::Lit(i) => match f.code.literals.get(i as usize) {
            Some(v @ (Value::Str(_) | Value::Int(_))) => Some(v),
            _ => return false,
        },
        Operand::Tmp(t) => match f.tmps.get_mut(t as usize) {
            Some(slot) if matches!(slot, Value::Str(_) | Value::Int(_)) => {
                taken = std::mem::replace(slot, Value::Null);
                Some(&taken)
            }
            _ => return false,
        },
        Operand::TmpKeep(t) => match f.tmps.get(t as usize) {
            Some(v @ (Value::Str(_) | Value::Int(_))) => Some(v),
            _ => return false,
        },
        Operand::Unused => return false,
    };
    f.unsync_cv_from_symtab(cv);
    let Some(slot) = f.cvs.get(idx) else {
        return false;
    };
    // SAFETY: `rhs` never points into this slot (the variable itself is
    // `None`): it is another variable's slot, a literal, a kept temporary
    // or the taken one, so the string is the only borrow of the slot.
    let Some(Value::Str(s)) = (unsafe { slot.get_mut_unchecked() }) else {
        return false;
    };
    match rhs {
        None => s.append_self(),
        Some(Value::Str(t)) => {
            if ZStr::ptr_eq(s, t) {
                s.append_self();
            } else {
                s.append_str(t);
            }
        }
        Some(Value::Int(n)) => s.append_long(*n),
        // Only strings and ints were let through above.
        Some(_) => {}
    }
    if f.symtab_attached || dst.is_some() {
        let r = Value::Str(s.clone());
        f.sync_cv_to_symtab(cv, &r);
        if let Some(d) = dst {
            if let Some(slot) = f.tmps.get_mut(d as usize) {
                put_tmp(slot, r);
            }
        }
    }
    true
}

/// [`Op::FeFetch`](crate::opcode::Op::FeFetch) of a by-value `foreach`
/// over an array (`ZEND_FE_FETCH_R` on `IS_ARRAY`: the next element from
/// the iterator's position, holes skipped, `ZVAL_COPY_DEREF` of its value
/// into `val` and its key into `key` when used): `Some(true)` with the
/// element fetched, `Some(false)` at the end (the loop jumps out). `None`,
/// nothing changed, for the other iterators (by reference, objects,
/// generators), which the generic op runs.
#[inline(always)]
#[allow(clippy::inline_always)] // the hot `foreach ($a as $k => $v)` step
pub(in crate::exec) fn fe_fetch_array_in(
    f: &mut Frame,
    iter: u32,
    val: u32,
    key: Option<u32>,
) -> Option<bool> {
    let Some(Some(ForeachIter::Array { arr, pos })) = f.iters.get_mut(iter as usize) else {
        return None;
    };
    // One pass over the slots for the value and, when the loop has a key
    // variable, its key.
    let (p, v, k) = if key.is_some() {
        let Some((p, v, k)) = arr.fe_fetch_with_key(*pos) else {
            return Some(false);
        };
        (p, v, Some(k))
    } else {
        let Some((p, v)) = arr.fe_fetch(*pos) else {
            return Some(false);
        };
        (p, v, None)
    };
    let v = match v {
        Value::Reference(r) => r.get(),
        v => copy_value(v),
    };
    *pos = p + 1;
    if let Some(slot) = f.tmps.get_mut(val as usize) {
        put_tmp(slot, v);
    }
    if let (Some(kt), Some(k)) = (key, k) {
        if let Some(slot) = f.tmps.get_mut(kt as usize) {
            put_tmp(slot, k);
        }
    }
    Some(true)
}

/// The assignment of a fetched `foreach` element to its loop variable,
/// run right after [`fe_fetch_array_in`] without a dispatch of its own:
/// when the op at `ip` is the `AssignCv` of temporary `t` (`$v` or `$k` of
/// `foreach ($a as $k => $v)`, `ZEND_ASSIGN_SPEC_CV_TMP` after
/// `ZEND_FE_FETCH_R`) and the variable is undefined or holds a scalar or
/// string, [`assign_cv_in`](super::super::hot::assign_cv_in) runs it here
/// and the `ip` after it is returned; otherwise `ip` itself, nothing
/// changed, and the op runs as usual. Releasing a scalar or a string runs
/// no destructor and buffers no GC root, so no safe point falls due
/// between the two ops (none is pending either: checked first), exactly
/// as when the dispatch loop polls between them.
#[inline(always)]
#[allow(clippy::inline_always)] // the hot `foreach ($a as $k => $v)` step
pub(in crate::exec) fn fe_assign_in(f: &mut Frame, ops: &[Op], ip: usize, t: u32) -> usize {
    let Some(&Op::AssignCv {
        cv,
        val: Operand::Tmp(src),
        dst: None,
    }) = ops.get(ip)
    else {
        return ip;
    };
    if src != t {
        return ip;
    }
    let plain = match f.cvs.get(cv as usize).map(CvSlot::get) {
        Some(None) => true,
        Some(Some(v)) => !matches!(v, Value::Array(_) | Value::Object(_) | Value::Reference(_)),
        None => false,
    };
    if plain
        && !crate::interrupt::safe_point_due()
        && super::super::hot::assign_cv_in(f, cv, Operand::Tmp(t), None)
    {
        ip + 1
    } else {
        ip
    }
}

/// `unset($a[k1][k2]...)` on the array `arr` along `keys` (all `Some`):
/// `ZEND_FETCH_DIM_UNSET` down the chain and `ZEND_UNSET_DIM` at its end,
/// as the generic walk's `descend` does them. Each container is separated
/// (`SEPARATE_ARRAY`) only when it has the key; a missing key, or a null
/// element on the way, unsets nothing (no diagnostic). `Some(removed)`
/// when the chain was done, the element taken out (released by the
/// caller once nothing is borrowed); `None`, having changed nothing but
/// separations the generic walk repeats as no-ops, when an element on the
/// way is anything but an array or null (a reference, an object, a
/// string, `false`, ...: the generic walk descends, throws or warns).
#[allow(clippy::option_option)] // "done, removed this" or "for the walk"
pub(super) fn unset_dims(arr: &mut ZArray, keys: &[Option<ArrayKey>]) -> Option<Option<Value>> {
    let (first, rest) = keys.split_first()?;
    let key = first.as_ref()?;
    if !arr.contains_key(key) {
        return Some(None);
    }
    let tbl = arr.make_mut();
    if rest.is_empty() {
        return Some(tbl.remove(key));
    }
    match tbl.get_mut(key)? {
        Value::Array(inner) => unset_dims(inner, rest),
        Value::Null => Some(None),
        _ => None,
    }
}

/// `unset($cv[d1]...[dn])` (one to four dimensions, none `[]`) of frame
/// `f` (place `place`) when `$cv` holds an array, directly or through a
/// reference not borrowed elsewhere, every offset peeks as an int or a
/// string ([`write_keys_in`]) and the frame's symbol table is not
/// attached: `ZEND_UNSET_DIM_SPEC_CV_*` (after the `FETCH_DIM_UNSET`s) on
/// arrays ([`unset_dims`]). A consumed string offset is released and the
/// removed element dropped. `false`, nothing changed but separations,
/// leaves the unset to the generic walk (an undefined variable warns
/// there, a string throws, `false` raises its deprecation).
#[inline(never)]
pub(in crate::exec) fn unset_dims_cv_in(f: &mut Frame, cv: u32, place: PlaceId) -> bool {
    if f.symtab_attached {
        return false;
    }
    let Some((keys, n)) = write_keys_in(f, place) else {
        return false;
    };
    let Some(keys) = keys.get(..n).filter(|k| !k.is_empty()) else {
        return false;
    };
    let Some(slot) = f.cvs.get_mut(cv as usize) else {
        return false;
    };
    let removed = match slot.as_mut() {
        Some(Value::Array(arr)) => unset_dims(arr, keys),
        Some(Value::Reference(r)) => {
            let Some(mut guard) = r.try_borrow_mut() else {
                return false;
            };
            match &mut *guard {
                Value::Array(arr) => unset_dims(arr, keys),
                _ => None,
            }
        }
        _ => None,
    };
    let Some(removed) = removed else {
        return false;
    };
    release_place_str_tmps(f, place);
    drop(removed);
    true
}

/// `$cv[d1]...[dn] = v` (one to four dimensions, the last possibly `[]`)
/// of place `pid` of frame `f`, when `$cv` holds an array directly or
/// through a reference not borrowed elsewhere and the frame's symbol table
/// is not attached: the `FETCH_DIM_W` chain and `ZEND_ASSIGN_DIM` of
/// [`super::props::store_dims`] for any value (an array or an object as
/// well, which [`Op::AssignDimsCv`](crate::opcode::Op::AssignDimsCv)
/// leaves to the generic walk). Consumed string offsets are released.
/// Returns the replaced element, `v` taken; `None`, `v` left, for the
/// generic assignment (an undefined or null variable, which the walk
/// promotes, a string, an object, a reference element, ...).
#[inline(never)]
pub(super) fn assign_cv_dims_in(
    f: &mut Frame,
    pid: PlaceId,
    v: &mut Option<Value>,
) -> Option<Value> {
    if f.symtab_attached {
        return None;
    }
    let p = f.code.places.get(pid as usize)?;
    let PlaceBase::Cv(cv) = p.base else {
        return None;
    };
    let (keys, n) = super::props::dim_keys_in(f, &p.segs)?;
    let keys = keys.get(..n)?;
    let old = match f.cvs.get_mut(cv as usize)?.as_mut() {
        Some(Value::Array(arr)) => super::props::store_dims(arr.make_mut(), keys, v),
        Some(Value::Reference(r)) => {
            let mut guard = r.try_borrow_mut()?;
            let old = match &mut *guard {
                Value::Array(arr) => super::props::store_dims(arr.make_mut(), keys, v),
                _ => None,
            };
            drop(guard);
            old
        }
        _ => None,
    }?;
    release_place_str_tmps(f, pid);
    Some(old)
}

impl Vm {
    /// `unset()` of an array element without the generic place walk:
    /// `unset($cv[...])` ([`unset_dims_cv_in`]) and `unset($this->name[...])`
    /// / `unset($cv->name[...])` on a property cache hit
    /// ([`super::props::unset_prop_dims_in`]). `false`, nothing changed but
    /// separations, for the generic `place_unset`.
    #[inline]
    pub(crate) fn unset_dims_fast(&mut self, pid: PlaceId) -> bool {
        let Some(f) = self.frames.last_mut() else {
            return false;
        };
        if f.kind == FrameKind::Builtin {
            return false;
        }
        let Some(p) = f.code.places.get(pid as usize) else {
            return false;
        };
        let head = match (&p.base, p.segs.as_slice()) {
            (PlaceBase::Cv(cv), [Seg::Dim(_), ..]) => Some(*cv),
            (PlaceBase::Cv(_) | PlaceBase::This, [Seg::Prop(Operand::Lit(_)), Seg::Dim(_), ..]) => {
                None
            }
            _ => return false,
        };
        match head {
            Some(cv) => unset_dims_cv_in(f, cv, pid),
            None => super::props::unset_prop_dims_in(f, pid) || self.unset_prop_dims_refill(pid),
        }
    }
}

#[cfg(test)]
mod tests {
    //! `foreach` by value over arrays with the fused value/key assignments
    //! ([`fe_assign_in`]) and their fallbacks. Expected output from
    //! `/usr/bin/php8.5 -n`.

    use crate::{Vm, VmOptions};
    use std::cell::RefCell;
    use std::io::Write;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct Sink(Rc<RefCell<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn run(src: &str) -> String {
        let sink = Sink::default();
        let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
        vm.run_script(src.as_bytes(), "t.php");
        drop(vm);
        let out = sink.0.borrow().clone();
        String::from_utf8_lossy(&out).into_owned()
    }

    /// Normalised keys, packed holes, a variable holding an object (its
    /// destructor runs at the assignment, value before key), one bound by
    /// reference, an attached symbol table.
    #[test]
    fn foreach_assignments_match_the_generic_ops() {
        let src = r#"<?php
class D { function __construct(public $n) {} function __destruct() { echo "~", $this->n, " "; } }
function f() {
    $h = ["a" => 1, "08" => 2, "8" => 3, "-0" => 4, -5 => 5];
    unset($h["a"]);
    $sum = [];
    foreach ($h as $k => $v) { $sum[$k] = ($sum[$k] ?? 0) + $v; var_dump($k); }
    foreach ($sum as $k => $s) { echo "$k=$s,"; }
    echo "\n";
    $v = new D(1); $k = new D(2);
    foreach ([10, 20] as $k => $v) { echo "[$k=$v] "; }
    echo "\n";
    $x = 0; $v = &$x;
    foreach ([7, 8] as $v) { echo $x, " "; }
    echo "\n";
    $p = [1, 2, 3]; unset($p[1]);
    foreach ($p as $i => $e) { echo "$i:$e "; }
    foreach ($p as $e) { echo "$e "; }
    echo "\n";
    foreach (["s" => "x", "t" => [1]] as $k => $v) { echo $k, $v === [1] ? "a" : $v, " "; }
    echo "\n";
    $name = "v";
    foreach ([1, 2] as $k => $v) { echo $$name, " "; }
    echo $k, $v, "\n";
}
f();
"#;
        let expected = r#"string(2) "08"
int(8)
string(2) "-0"
int(-5)
08=2,8=3,-0=4,-5=5,
~1 ~2 [0=10] [1=20] 
7 8 
0:1 2:3 1 3 
sx ta 
1 2 12
"#;
        assert_eq!(run(src), expected);
    }
}

/// The value an `Is`-mode read of `cur[d1]...[dn]` gives along the
/// dimension segments `segs` of frame `f` (`cur` `None`: an undefined
/// variable), when no step can warn, convert, throw or run user code:
/// php-src's `ZEND_FETCH_DIM_IS` chain (`zend_fetch_dimension_address_read`
/// with `BP_VAR_IS`), where a missing key, an undefined variable, `null`
/// and a `bool`/`int`/`float` container give `null` silently and further
/// offsets are not looked at. Every offset must peek as an int or a
/// string (checked before the walk, so a float or `null` key keeps its
/// deprecation on the generic path); a string or object container
/// (string offsets, `ArrayAccess`) gives `None`, as does a borrowed
/// reference.
pub(super) fn is_dims_value(f: &Frame, cur: Option<&Value>, segs: &[Seg]) -> Option<Value> {
    let Some((Seg::Dim(dim), rest)) = segs.split_first() else {
        return match (segs, cur) {
            ([], None) => Some(Value::Null),
            ([], Some(Value::Reference(r))) => Some(r.get()),
            ([], Some(v)) => Some(copy_value(v)),
            _ => None,
        };
    };
    match cur {
        None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)) => {
            Some(Value::Null)
        }
        Some(Value::Reference(r)) => {
            let inner = r.try_borrow()?;
            if matches!(&*inner, Value::Reference(_)) {
                return None;
            }
            is_dims_value(f, Some(&*inner), segs)
        }
        Some(Value::Array(a)) => is_dims_value(f, table_dim(a, dim_ref_in(f, *dim)?), rest),
        Some(_) => None,
    }
}

/// Whether every segment of `segs` is a dimension whose offset operand
/// peeks as an int or a string in frame `f` ([`dim_ref_in`]).
pub(super) fn plain_dims(f: &Frame, segs: &[Seg]) -> bool {
    segs.iter()
        .all(|s| matches!(s, Seg::Dim(d) if dim_ref_in(f, *d).is_some()))
}

/// The `Is`-mode `$cv[dim]` read ([`Op::FetchDimCv`](crate::opcode::Op::FetchDimCv)
/// in [`FetchMode::Is`](crate::opcode::FetchMode::Is): `$a['k'] ?? d`)
/// that [`fetch_dim_cv_in`] did not take: the key missing from the array,
/// the variable undefined or holding `null` or a `bool`/`int`/`float`
/// (`ZEND_FETCH_DIM_IS`: `null`, no diagnostic). Stored and released as
/// in [`fetch_dim_cv_in`]; `None`, nothing changed, for anything else
/// (a string or object container, an offset of another type).
#[inline(never)]
pub(in crate::exec) fn fetch_dim_is_miss_in(
    f: &mut Frame,
    cv: u32,
    dim: Operand,
    dst: u32,
    to: Option<u32>,
) -> Option<bool> {
    let segs = [Seg::Dim(dim)];
    if !plain_dims(f, &segs) {
        return None;
    }
    let cur = f.cvs.get(cv as usize)?.get();
    if cur.is_none() && f.symtab_attached {
        return None;
    }
    let v = is_dims_value(f, cur, &segs)?;
    release_str_tmp_in(f, dim);
    Some(store_fetched_in(f, dst, to, v))
}

/// The `Is`-mode `$cv[d1][d2]...` read ([`Op::FetchDimsCv`](crate::opcode::Op::FetchDimsCv)
/// in [`FetchMode::Is`](crate::opcode::FetchMode::Is): `$a['k']['j'] ?? d`,
/// `isset`'s neighbour `??`) that [`fetch_dims_cv_in`] did not take: as
/// [`fetch_dim_is_miss_in`], along every offset of `place`.
#[inline(never)]
pub(in crate::exec) fn fetch_dims_is_miss_in(
    f: &mut Frame,
    cv: u32,
    place: PlaceId,
    dst: u32,
    to: Option<u32>,
) -> Option<bool> {
    let segs = &f.code.places.get(place as usize)?.segs;
    if !plain_dims(f, segs) {
        return None;
    }
    let cur = f.cvs.get(cv as usize)?.get();
    if cur.is_none() && f.symtab_attached {
        return None;
    }
    let v = is_dims_value(f, cur, segs)?;
    release_place_str_tmps(f, place);
    Some(store_fetched_in(f, dst, to, v))
}

/// The base of a `tmp[d1]...[dn]` place (`PlaceBase::Value`: `f()['k']`,
/// `self::LIST[$k]`, the source of `[$a, $b] = ...`) of frame `f`, when
/// reading it and releasing it afterwards (`FREE_OP1` of a consumed
/// temporary) has no effect beyond the read: a kept temporary or a
/// literal (not released here), or a consumed temporary holding a plain
/// scalar or an array another handle shares (so that the release only
/// drops a count, as in `Frame::send_place_hit`). With the operand index
/// to release (`Some` for a consumed temporary).
fn tmp_base(f: &Frame, base: Operand) -> Option<(&Value, Option<u32>)> {
    match base {
        Operand::Tmp(t) => {
            let v = f.tmps.get(t as usize)?;
            match v {
                Value::Array(a) if a.refcount() >= 2 => Some((v, Some(t))),
                Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) => {
                    Some((v, Some(t)))
                }
                _ => None,
            }
        }
        Operand::TmpKeep(t) => Some((f.tmps.get(t as usize)?, None)),
        // `[$a, $b] = $cv` reads the variable in place (an undefined one
        // warns on the generic path).
        Operand::Cv(c) => Some((f.cvs.get(c as usize)?.get()?, None)),
        Operand::Lit(l) => Some((f.code.literals.get(l as usize)?, None)),
        _ => None,
    }
}

/// Releases what reading a `tmp[...]` place consumed: the offsets' string
/// temporaries and the consumed base temporary `base` ([`tmp_base`]).
fn release_tmp_place(f: &mut Frame, place: PlaceId, base: Option<u32>) {
    release_place_str_tmps(f, place);
    if let Some(slot) = base.and_then(|t| f.tmps.get_mut(t as usize)) {
        // A scalar, or an array whose last handle is elsewhere.
        drop(std::mem::take(slot));
    }
}

/// The value of an element of a temporary, `tmp[d1]...[dn]` (place
/// `place` of frame `f`, whose base is `PlaceBase::Value`), read in
/// `Read` or `List` mode (`is` false: `ZEND_FETCH_DIM_R` /
/// `ZEND_FETCH_LIST_R` on an array holding every offset; a missing key
/// warns on the generic path) or in `Is` mode (`is`: also the silent null
/// of a missing key or a scalar container, as [`is_dims_value`]). The
/// base and the consumed offsets are released ([`tmp_base`]). `None`,
/// nothing changed, for anything else.
#[inline(never)]
pub(in crate::exec) fn fetch_tmp_dims_in(f: &mut Frame, place: PlaceId, is: bool) -> Option<Value> {
    let (v, base) = {
        let p = f.code.places.get(place as usize)?;
        let PlaceBase::Value(b) = p.base else {
            return None;
        };
        let segs = p.segs.as_slice();
        if segs.is_empty() {
            return None;
        }
        let (cur, base) = tmp_base(f, b)?;
        let v = if is {
            if !plain_dims(f, segs) {
                return None;
            }
            is_dims_value(f, Some(cur), segs)?
        } else {
            dims_value_of(f, cur, segs)?
        };
        (v, base)
    };
    release_tmp_place(f, place, base);
    Some(v)
}

/// `isset()` (`empty` false) or `empty()` of an element of a temporary,
/// `tmp[d1]...[dn]` (`isset(self::MAP[$k])`, `empty(f()['k'])`):
/// `ZEND_ISSET_ISEMPTY_DIM_OBJ` on a TMP container that is an array (or a
/// scalar: not set), every offset an int or a string. The base and the
/// consumed offsets are released ([`tmp_base`]). `None`, nothing changed,
/// for anything else (a string offset, `ArrayAccess`, an object whose
/// truth its class decides, an offset that warns or converts).
#[inline(never)]
pub(in crate::exec) fn isset_tmp_dims_in(
    f: &mut Frame,
    place: PlaceId,
    empty: bool,
) -> Option<bool> {
    let (r, base) = {
        let p = f.code.places.get(place as usize)?;
        let PlaceBase::Value(b) = p.base else {
            return None;
        };
        let segs = p.segs.as_slice();
        if segs.is_empty() || !plain_dims(f, segs) {
            return None;
        }
        let (cur, base) = tmp_base(f, b)?;
        let v = is_dims_value(f, Some(cur), segs)?;
        let r = if empty {
            !super::plain_truth(&v)?
        } else {
            !v.is_null()
        };
        (r, base)
    };
    release_tmp_place(f, place, base);
    Some(r)
}

/// [`Op::KeyExists`](crate::opcode::Op::KeyExists): `array_key_exists($key,
/// $array)` when the key peeks as an int or a string and `$array` is an
/// array (a variable's, possibly through a reference, a literal, a kept
/// temporary, or a consumed temporary another handle shares): php-src's
/// `ZEND_ARRAY_KEY_EXISTS` with `zend_array_key_exists_fast` on an
/// `IS_STRING` (`ZEND_HANDLE_NUMERIC_STR`) or `IS_LONG` key. The result
/// goes to `dst` when there is one; a consumed string key and the
/// consumed array temporary are released. `false`, nothing changed, for
/// anything else (an undefined variable, a key of another type and its
/// diagnostics, a non-array and its `TypeError`).
#[inline(never)]
pub(in crate::exec) fn key_exists_in(
    f: &mut Frame,
    key: Operand,
    array: Operand,
    dst: Option<u32>,
) -> bool {
    let (found, release) = {
        let Some(k) = dim_ref_in(f, key) else {
            return false;
        };
        let (v, release) = match array {
            Operand::Cv(c) => match f.cvs.get(c as usize).and_then(CvSlot::get) {
                Some(v) => (v, None),
                None => return false,
            },
            Operand::Tmp(t) => match f.tmps.get(t as usize) {
                Some(v @ Value::Array(a)) if a.refcount() >= 2 => (v, Some(t)),
                _ => return false,
            },
            Operand::TmpKeep(t) => match f.tmps.get(t as usize) {
                Some(v) => (v, None),
                None => return false,
            },
            Operand::Lit(l) => match f.code.literals.get(l as usize) {
                Some(v) => (v, None),
                None => return false,
            },
            Operand::Unused => return false,
        };
        let found = match v {
            Value::Array(a) => table_dim(a, k).is_some(),
            Value::Reference(r) => match r.try_borrow().as_deref() {
                Some(Value::Array(a)) => table_dim(a, k).is_some(),
                _ => return false,
            },
            _ => return false,
        };
        (found, release)
    };
    if let Some(slot) = release.and_then(|t| f.tmps.get_mut(t as usize)) {
        // An array whose last handle is elsewhere: only a count drops.
        drop(std::mem::take(slot));
    }
    release_str_tmp_in(f, key);
    if let Some(slot) = dst.and_then(|d| f.tmps.get_mut(d as usize)) {
        put_tmp(slot, Value::Bool(found));
    }
    true
}

/// `isset()` (`empty` false) or `empty()` of `$cv[d1]...[dn]` (place
/// `place` of frame `f`, two or more offsets: one is
/// [`Op::IssetDimCv`](crate::opcode::Op::IssetDimCv)), every offset an int
/// or a string: the `ZEND_FETCH_DIM_IS` chain and
/// `ZEND_ISSET_ISEMPTY_DIM_OBJ` on the last container ([`is_dims_value`]:
/// a missing key, an undefined variable or a scalar container on the way
/// is not set). Consumed string offsets are released. `None`, nothing
/// changed, for a string offset, `ArrayAccess`, an object whose truth its
/// class decides, or a frame with an attached symbol table.
#[inline(never)]
pub(in crate::exec) fn isset_cv_dims_in(
    f: &mut Frame,
    cv: u32,
    place: PlaceId,
    empty: bool,
) -> Option<bool> {
    if f.symtab_attached {
        return None;
    }
    let r = {
        let segs = f.code.places.get(place as usize)?.segs.as_slice();
        if segs.is_empty() || !plain_dims(f, segs) {
            return None;
        }
        let v = is_dims_value(f, f.cvs.get(cv as usize)?.get(), segs)?;
        if empty {
            !super::plain_truth(&v)?
        } else {
            !v.is_null()
        }
    };
    release_place_str_tmps(f, place);
    Some(r)
}

/// [`Op::InArray`](crate::opcode::Op::InArray): `in_array($needle,
/// [literals...][, strict])` of frame `f` where the needle settles it by a
/// key lookup in the literal set `set` (`ZEND_IN_ARRAY`'s
/// `zend_hash_find_ex` / `zend_hash_index_find` paths): a string, in
/// strict mode an int (anything else is not in a strict set), in loose
/// mode `null` or `false` (equal only to `""`). The needle is read as
/// `Vm::read` reads it (a defined variable, through a reference; a
/// literal; a temporary, its string released when consumed); the result
/// goes to `dst` when there is one. `false`, nothing changed, for
/// anything else (an undefined variable, a loose comparison with each
/// element).
#[inline(never)]
pub(in crate::exec) fn in_array_in(
    f: &mut Frame,
    a: Operand,
    set: u32,
    strict: bool,
    dst: Option<u32>,
) -> bool {
    let r = {
        let Some(Value::Array(keys)) = f.code.literals.get(set as usize) else {
            return false;
        };
        let guard;
        let needle = match a {
            Operand::Cv(c) => match f.cvs.get(c as usize).and_then(CvSlot::get) {
                Some(Value::Reference(r)) => {
                    let Some(g) = r.try_borrow() else {
                        return false;
                    };
                    guard = g;
                    &*guard
                }
                Some(v) => v,
                None => return false,
            },
            Operand::Tmp(t) | Operand::TmpKeep(t) => match f.tmps.get(t as usize) {
                Some(Value::Reference(_)) | None => return false,
                Some(v) => v,
            },
            Operand::Lit(l) => match f.code.literals.get(l as usize) {
                Some(v) => v,
                None => return false,
            },
            Operand::Unused => return false,
        };
        match needle {
            Value::Str(s) => keys.get_str(s.as_bytes()).is_some(),
            Value::Int(i) if strict => keys.get_int(*i).is_some(),
            // A consumed array or object needle would be released here.
            Value::Array(_) | Value::Object(_) if matches!(a, Operand::Tmp(_)) => return false,
            _ if strict => false,
            Value::Null | Value::Bool(false) => keys.get_str(b"").is_some(),
            _ => return false,
        }
    };
    release_str_tmp_in(f, a);
    if let Some(slot) = dst.and_then(|d| f.tmps.get_mut(d as usize)) {
        put_tmp(slot, Value::Bool(r));
    }
    true
}

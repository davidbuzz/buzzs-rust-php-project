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
    dims_value_in, discard_plain, isset_dim_in, plain_value_in, put_tmp, release_place_str_tmps,
    release_str_tmp_in, store_fetched_in, string_offset_char, table_dim, table_dim_mut,
    write_keys_in, DimRef,
};
use crate::opcode::{BinKind, IncDecKind, Operand, PlaceId};
use crate::vm::{CvSlot, ForeachIter, Frame, Vm};
use php_runtime::{ArrayKey, BinaryOp, HashTable, Value, ZStr};

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
        return false;
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
        return None;
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

/// `tbl[i] = v` for an int offset (`zend_fetch_dimension_address_inner_W`
/// + `zend_assign_to_variable`): an existing plain element of a packed
/// table is overwritten where it is (`zend_hash_index_find` on
/// `HT_IS_PACKED`, inline); otherwise the element is found or added
/// (`zend_hash_index_lookup`, as null, then overwritten). `false`, nothing
/// changed, when the existing element is not plain (a reference, array,
/// object: the generic assignment handles it).
#[inline(always)]
#[allow(clippy::inline_always)] // the packed overwrite is the hot `$a[$i] = $v`
fn assign_int_dim(tbl: &mut HashTable, i: i64, v: Value) -> bool {
    if let Some(slot) = tbl.get_packed_mut(i) {
        if !slot.is_plain() {
            return false;
        }
        discard_plain(std::mem::replace(slot, v));
        return true;
    }
    assign_int_dim_other(tbl, i, v)
}

/// [`assign_int_dim`] past a packed hit: an append or a hash table.
#[inline(never)]
fn assign_int_dim_other(tbl: &mut HashTable, i: i64, v: Value) -> bool {
    let v = match tbl.assign_packed_int(i, v) {
        Ok(old) => {
            if let Some(old) = old {
                discard_plain(old);
            }
            return true;
        }
        Err(v) => v,
    };
    let (slot, added) = tbl.lookup_ex(ArrayKey::Int(i));
    if !added && !slot.is_plain() {
        return false;
    }
    discard_plain(std::mem::replace(slot, v));
    true
}

/// `tbl[s] = v` for a string offset with symbol-table semantics: an
/// existing element is found (`zend_symtable_find`) and overwritten
/// without building a key; a missing one is added under the key the
/// string converts to. `false`, nothing changed, when the existing
/// element is not plain.
#[inline(never)]
fn assign_str_dim(tbl: &mut HashTable, s: &ZStr, v: Value) -> bool {
    if let Some(slot) = tbl.get_sym_zstr_mut(s) {
        if !slot.is_plain() {
            return false;
        }
        discard_plain(std::mem::replace(slot, v));
        return true;
    }
    let (slot, added) = tbl.lookup_ex(ArrayKey::from_zstr(s));
    if !added && !slot.is_plain() {
        return false;
    }
    discard_plain(std::mem::replace(slot, v));
    true
}

/// The offset of an [`Op::AssignDimCv`](crate::opcode::Op::AssignDimCv).
#[derive(Clone, Copy)]
enum WriteDim<'a> {
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
fn assign_write_dim(tbl: &mut HashTable, dim: WriteDim<'_>, v: Value) -> bool {
    match dim {
        WriteDim::Int(i) => assign_int_dim(tbl, i, v),
        WriteDim::Str(s) => assign_str_dim(tbl, s, v),
        WriteDim::Append => tbl.push(v).is_ok(),
    }
}

/// [`Op::AssignDimCv`](crate::opcode::Op::AssignDimCv): `$cv[dim] = val;`
/// when `$cv` holds an array (directly or through a reference), the offset
/// is an int or string, the value is plain and the element it replaces
/// (if any) is plain: the `zend_fetch_dimension_address_inner_W` and
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
    let Some(v) = plain_value_in(f, val) else {
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
#[inline]
pub(in crate::exec) fn assign_dims_cv_in(
    f: &mut Frame,
    cv: u32,
    place: PlaceId,
    val: Operand,
) -> bool {
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
    let Some((p, v)) = arr.fe_fetch(*pos) else {
        return Some(false);
    };
    let v = match v {
        Value::Reference(r) => r.get(),
        v => copy_value(v),
    };
    let k = if key.is_some() {
        arr.key_value_at(p)
    } else {
        None
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

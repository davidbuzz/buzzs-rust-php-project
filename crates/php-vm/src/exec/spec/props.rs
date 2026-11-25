// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Handlers of the dedicated property ops ([`Op::FetchObjProp`],
//! [`Op::FetchObjPropDim`], [`Op::AssignObjProp`], [`Op::AssignObjPropDim`],
//! [`Op::AssignOpObjProp`], [`Op::AssignOpObjPropDim`], [`Op::IncDecObjProp`],
//! [`Op::IncDecObjPropDim`], [`Op::IssetObjProp`], [`Op::IssetObjPropDim`]):
//! `$this->p` and `$cv->p` with literal property names (and their one-dimension
//! or append forms) calling the property cache directly, skipping the place walk.
//!
//! On a cache hit the handler accesses the property's storage directly
//! (`place/propcache.rs`); anything the cache declines (magic methods, hooks,
//! readonly or visibility errors, typed-property coercion, references where
//! they matter, diagnostics) falls back to the generic place walk unchanged.
//!
//! [`Op::FetchObjProp`]: crate::opcode::Op::FetchObjProp
//! [`Op::FetchObjPropDim`]: crate::opcode::Op::FetchObjPropDim
//! [`Op::AssignObjProp`]: crate::opcode::Op::AssignObjProp
//! [`Op::AssignObjPropDim`]: crate::opcode::Op::AssignObjPropDim
//! [`Op::AssignOpObjProp`]: crate::opcode::Op::AssignOpObjProp
//! [`Op::AssignOpObjPropDim`]: crate::opcode::Op::AssignOpObjPropDim
//! [`Op::IncDecObjProp`]: crate::opcode::Op::IncDecObjProp
//! [`Op::IncDecObjPropDim`]: crate::opcode::Op::IncDecObjPropDim
//! [`Op::IssetObjProp`]: crate::opcode::Op::IssetObjProp
//! [`Op::IssetObjPropDim`]: crate::opcode::Op::IssetObjPropDim

// The hit paths are small and must disappear into the dispatch loop's
// pre-match: as calls, they return their results through memory.
#![allow(clippy::inline_always)]

use super::super::{discard_unrooted, Flow, Num};
use super::{plain_truth, put_tmp, store_fetched_in};
use crate::object::{self, ObjectData};
use crate::opcode::{
    ClassSpec, FetchMode, IncDecKind, Op, Operand, PlaceBase, PlaceId, PROP_CACHE_THIS,
};
use crate::place::propcache::{
    accepts, scope_addr, PC_DYN, PC_ISSET, PC_NO_SET, PC_READ, PC_WRITE,
};
use crate::vm::{CvSlot, Frame, FrameKind, Vm, VmResult};
use php_runtime::{ArrayKey, BinaryOp, Obj, Value, ZArray, ZStr};
use std::rc::Rc;

/// The plain data of a property cache entry that a hit needs, copied out
/// of the cache so that no borrow of it is held while the property is
/// accessed (the `CACHED_PTR` pair of a property op's runtime cache slot:
/// the class and the property's offset).
#[derive(Clone, Copy)]
struct Hit {
    /// [`crate::opcode::PropCacheEntry::class_addr`].
    class: usize,
    /// The declared property's slot (`prop_info->offset`).
    slot: usize,
    /// [`crate::opcode::PropCacheEntry::ok_class_addr`].
    ok_class: usize,
    /// The place's base: a compiled variable or [`PROP_CACHE_THIS`].
    base: u32,
    /// The literal index of the property name.
    lit: u32,
    /// [`crate::opcode::PropCacheEntry::accept`].
    accept: u32,
    /// The entry's `PC_*` bits.
    flags: u8,
}

/// The entry of place `pid` in frame `f`'s property cache when it is filled
/// in `f`'s class scope, allows every access in `want` and has none of the
/// `PC_*` bits in `refuse` (`PC_DYN`: a declared property only); the
/// object's class is checked by [`with_hit_obj`].
#[inline(always)]
fn hit_key(f: &Frame, pid: PlaceId, want: u8, refuse: u8) -> Option<Hit> {
    if f.kind == FrameKind::Builtin {
        return None;
    }
    let cache = f.code.prop_cache.try_borrow().ok()?;
    let e = cache.get(pid as usize)?;
    if e.class_addr == 0 || e.flags & (want | refuse) != want || e.scope_addr != scope_addr(f) {
        return None;
    }
    let hit = Hit {
        class: e.class_addr,
        slot: e.slot as usize,
        ok_class: e.ok_class_addr,
        base: e.base,
        lit: e.lit,
        accept: e.accept,
        flags: e.flags,
    };
    Some(hit)
}

/// Runs `access` on the data of the object the base of `hit` holds in
/// frame `f` (`$this`, or a compiled variable holding the object directly
/// or through a reference that is not being modified) when the object is of
/// the entry's class and not lazy (`EXPECTED(zobj->ce ==
/// CACHED_PTR(cache_slot))`); `None` otherwise. `access` is called from
/// this one place, so that it is inlined into each fast path (a closure
/// called from several places, as `with_base_obj`'s is, stays a call).
#[inline(always)]
fn with_hit_obj<R>(
    f: &Frame,
    hit: &Hit,
    access: impl FnOnce(&ObjectData) -> Option<R>,
) -> Option<R> {
    let guard;
    let obj: &Obj = if hit.base == PROP_CACHE_THIS {
        f.this.as_ref()?
    } else {
        match f.cvs.get(hit.base as usize)?.get()? {
            Value::Object(o) => o,
            // A reference being modified elsewhere (`descend`'s "Cannot
            // modify a value while it is being iterated") is left to the
            // walk.
            Value::Reference(r) => {
                guard = r.try_borrow()?;
                match &*guard {
                    Value::Object(o) => o,
                    _ => return None,
                }
            }
            _ => return None,
        }
    };
    let od = object::data(obj);
    if Rc::as_ptr(&od.class) as usize != hit.class
        || !od.lazy.try_borrow().is_ok_and(|l| l.is_none())
    {
        return None;
    }
    access(od)
}

/// `ZVAL_COPY` of a property or operand value: scalars are copied inline,
/// without the out-of-line `Clone` call.
#[inline(always)]
fn copy_value(v: &Value) -> Value {
    match v {
        Value::Null => Value::Null,
        Value::Bool(b) => Value::Bool(*b),
        Value::Int(i) => Value::Int(*i),
        Value::Float(x) => Value::Float(*x),
        other => other.clone(),
    }
}

/// Whether `v` may be stored as-is in the property `hit` describes: an
/// untyped property, a value of an accepted builtin type, or an object of
/// the class last accepted for a class-typed property.
#[inline(always)]
fn hit_accepts(hit: &Hit, v: &Value) -> bool {
    if hit.accept == u32::MAX || accepts(hit.accept, v) {
        return true;
    }
    match v {
        Value::Object(o) => {
            hit.ok_class != 0 && Rc::as_ptr(object::class_of(o)) as usize == hit.ok_class
        }
        _ => false,
    }
}

/// The name of the dynamic property `hit` describes (`None` for a declared
/// one).
#[inline(always)]
fn dyn_name<'a>(f: &'a Frame, hit: &Hit) -> Option<&'a ZStr> {
    if hit.flags & PC_DYN == 0 {
        return None;
    }
    match f.code.literals.get(hit.lit as usize) {
        Some(Value::Str(name)) => Some(name),
        _ => None,
    }
}

/// The data of the object the base of `hit` holds directly in frame `f`
/// (`$this`, or a compiled variable holding the object itself, not a
/// reference) when it is of the entry's class and not lazy: the hit's
/// `Z_OBJ_P(container)` and `EXPECTED(zobj->ce == CACHED_PTR(cache_slot))`
/// without a closure, so that the access after it is inlined into the
/// dispatch loop. `None` otherwise (a variable holding a reference is
/// then tried through [`with_hit_obj`]).
#[inline(always)]
fn hit_data<'a>(f: &'a Frame, hit: &Hit) -> Option<&'a ObjectData> {
    let obj: &Obj = if hit.base == PROP_CACHE_THIS {
        f.this.as_ref()?
    } else {
        match f.cvs.get(hit.base as usize)?.get()? {
            Value::Object(o) => o,
            _ => return None,
        }
    };
    let od = object::data(obj);
    if Rc::as_ptr(&od.class) as usize != hit.class
        || !od.lazy.try_borrow().is_ok_and(|l| l.is_none())
    {
        return None;
    }
    Some(od)
}

/// Whether the base of `hit` in frame `f` is a compiled variable holding
/// a reference (the only base [`with_hit_obj`] accepts that [`hit_data`]
/// does not).
#[inline(always)]
fn base_is_ref(f: &Frame, hit: &Hit) -> bool {
    hit.base != PROP_CACHE_THIS
        && matches!(
            f.cvs.get(hit.base as usize).and_then(CvSlot::get),
            Some(Value::Reference(_))
        )
}

/// The (dereferenced) value of the property `hit` describes in `od`,
/// copied: an initialized declared slot, or the existing dynamic property
/// `dyn_prop`.
#[inline(always)]
fn read_in(od: &ObjectData, slot: usize, dyn_prop: Option<&ZStr>) -> Option<Value> {
    if let Some(name) = dyn_prop {
        let table = od.dynamic.try_borrow().ok()?;
        let v = match table.as_deref()?.get_zstr(name)? {
            Value::Reference(r) => r.try_borrow().map(|inner| copy_value(&inner)),
            v => Some(copy_value(v)),
        };
        return v;
    }
    let slots = od.slots.try_borrow().ok()?;
    let v = match slots.get(slot)? {
        Some(Value::Reference(r)) => r.try_borrow().map(|inner| copy_value(&inner)),
        Some(v) => Some(copy_value(v)),
        None => None,
    };
    v
}

/// `ZEND_FETCH_OBJ_R` on a cache hit: the (dereferenced) value of an
/// initialized declared property or of an existing dynamic property
/// (`ZEND_DYNAMIC_PROPERTY_OFFSET`: `zend_hash_find` in
/// `zobj->properties`), copied.
#[inline(always)]
pub(in crate::exec) fn read_hit(f: &Frame, pid: PlaceId) -> Option<Value> {
    let hit = hit_key(f, pid, PC_READ, 0)?;
    let dyn_prop = dyn_name(f, &hit);
    if hit.flags & PC_DYN != 0 && dyn_prop.is_none() {
        return None;
    }
    let slot = hit.slot;
    if let Some(od) = hit_data(f, &hit) {
        return read_in(od, slot, dyn_prop);
    }
    if !base_is_ref(f, &hit) {
        return None;
    }
    with_hit_obj(f, &hit, |od| read_in(od, slot, dyn_prop))
}

/// `isset()` (or, with `empty`, `empty()`) of a plain value.
#[inline(always)]
fn isset_of(v: &Value, empty: bool) -> Option<bool> {
    if empty {
        plain_truth(v).map(|t| !t)
    } else {
        Some(!matches!(v, Value::Null))
    }
}

/// `isset()`/`empty()` of a property value, also through a reference.
#[inline(always)]
fn isset_deref(v: &Value, empty: bool) -> Option<bool> {
    match v {
        Value::Reference(rf) => {
            let inner = rf.try_borrow()?;
            isset_of(&inner, empty)
        }
        v => isset_of(v, empty),
    }
}

/// The array a declared property slot holds, directly or through a
/// reference (left by `&$this->a`, a by-reference argument or `foreach` by
/// reference), for reading.
#[inline(always)]
fn slot_array<R>(slot: Option<&Value>, read: impl FnOnce(&ZArray) -> Option<R>) -> Option<R> {
    match slot? {
        Value::Array(a) => read(a),
        Value::Reference(r) => match &*r.try_borrow()? {
            Value::Array(a) => read(a),
            _ => None,
        },
        _ => None,
    }
}

/// `ZEND_FETCH_OBJ_R` + `ZEND_FETCH_DIM_R` (or the `_IS` pair) of
/// `$this->name[dim]` / `$cv->name[dim]` on a property cache hit, read in
/// frame `f` alone: the base holds the object directly (a reference base
/// takes [`Vm::prop_cache_read_elem`]), the declared slot holds an array
/// with the int or string offset `dim` peeks as. The element,
/// dereferenced and copied; `None` otherwise.
#[inline(always)]
fn read_dim_hit(f: &Frame, pid: PlaceId, dim: Operand) -> Option<Value> {
    let hit = hit_key(f, pid, PC_READ, PC_DYN)?;
    let key = super::dim_ref_in(f, dim)?;
    let od = hit_data(f, &hit)?;
    let slots = od.slots.try_borrow().ok()?;
    let v = slot_array(slots.get(hit.slot)?.as_ref(), |a| {
        Some(match super::table_dim(a, key)? {
            Value::Reference(r) => copy_value(&*r.try_borrow()?),
            v => copy_value(v),
        })
    });
    drop(slots);
    v
}

/// `isset($this->name[dim])` / `empty(...)` (`ZEND_FETCH_OBJ_IS`, then
/// `ZEND_ISSET_ISEMPTY_DIM_OBJ`) on a property cache hit in frame `f`
/// alone, as [`read_dim_hit`]: a missing key or a null property answers
/// without conversions. `None` for the slower paths.
#[inline(always)]
fn isset_dim_hit(f: &Frame, pid: PlaceId, dim: Operand, empty: bool) -> Option<bool> {
    let hit = hit_key(f, pid, PC_ISSET, PC_DYN)?;
    let key = super::dim_ref_in(f, dim)?;
    let od = hit_data(f, &hit)?;
    let slots = od.slots.try_borrow().ok()?;
    let r = match slots.get(hit.slot)?.as_ref()? {
        Value::Null => Some(empty),
        v => slot_array(Some(v), |a| match super::table_dim(a, key) {
            Some(el) => isset_deref(el, empty),
            None => Some(empty),
        }),
    };
    drop(slots);
    r
}

/// `$this->name[dim] = val` / `$this->name[] = val` (`dim`
/// [`Operand::Unused`]) on a property cache hit for writing in frame `f`
/// alone (`ZEND_FETCH_OBJ_W` + `ZEND_ASSIGN_DIM` + `OP_DATA`): the base
/// holds the object directly, the declared slot holds an array (also
/// through a reference), the offset is an int or a string and the value
/// is plain, an array or an object; the store is
/// [`super::dims::assign_write_dim`]'s (separation, an existing element
/// overwritten unless it is a reference, a missing one added). The
/// offset's string temporary and the value's temporary are consumed and
/// the value written to `dst` when used. `false`, nothing changed but a
/// separation, otherwise.
#[inline(never)]
fn assign_dim_hit(
    f: &mut Frame,
    pid: PlaceId,
    dim: Operand,
    val: Operand,
    dst: Option<u32>,
) -> bool {
    use super::dims::{assign_write_dim, WriteDim};
    let Some(hit) = hit_key(f, pid, PC_READ | PC_WRITE, PC_DYN) else {
        return false;
    };
    let Some(v) = super::dim_value_in(f, val) else {
        return false;
    };
    let wd = if dim == Operand::Unused {
        WriteDim::Append
    } else {
        match super::dim_ref_in(f, dim) {
            Some(super::DimRef::Int(i)) => WriteDim::Int(i),
            Some(super::DimRef::Str(s)) => WriteDim::Str(s),
            None => return false,
        }
    };
    let Some(od) = hit_data(f, &hit) else {
        return false;
    };
    let ret = dst.map(|_| copy_value(&v));
    let Ok(mut slots) = od.slots.try_borrow_mut() else {
        return false;
    };
    let ok = match slots.get_mut(hit.slot) {
        Some(Some(Value::Array(arr))) => assign_write_dim(arr.make_mut(), wd, v),
        Some(Some(Value::Reference(r))) => match r.try_borrow_mut() {
            Some(mut inner) => match &mut *inner {
                Value::Array(arr) => assign_write_dim(arr.make_mut(), wd, v),
                _ => false,
            },
            None => false,
        },
        _ => false,
    };
    drop(slots);
    if !ok {
        return false;
    }
    if matches!(wd, WriteDim::Str(_)) {
        super::release_str_tmp_in(f, dim);
    }
    if let Operand::Tmp(t) = val {
        if let Some(slot) = f.tmps.get_mut(t as usize) {
            put_tmp(slot, Value::Null);
        }
    }
    if let (Some(d), Some(r)) = (dst, ret) {
        if let Some(slot) = f.tmps.get_mut(d as usize) {
            put_tmp(slot, r);
        }
    }
    true
}

/// `$this->name[dim]++` & co. (`ZEND_FETCH_OBJ_RW` + `ZEND_FETCH_DIM_RW`
/// + `PRE/POST_INC/DEC`) on a property cache hit for writing in frame `f`
/// alone, as [`assign_dim_hit`]: the declared slot holds an array (also
/// through a reference) whose element at the int or string offset is an
/// int that does not overflow, or a float. The array is separated when
/// shared, the element updated in place, and the expression's value
/// returned (a consumed string offset released). `None`, nothing changed,
/// otherwise (a missing key warns on the generic path).
#[inline(never)]
fn incdec_dim_hit(f: &mut Frame, pid: PlaceId, dim: Operand, kind: IncDecKind) -> Option<Value> {
    let hit = hit_key(f, pid, PC_READ | PC_WRITE, PC_DYN)?;
    let key = super::dim_ref_in(f, dim)?;
    let od = hit_data(f, &hit)?;
    let mut slots = od.slots.try_borrow_mut().ok()?;
    let step = |arr: &mut ZArray| -> Option<(Value, Value)> {
        let (old, new) = match super::table_dim(arr, key)? {
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
        let el = super::table_dim_mut(arr.make_mut(), key)?;
        // A number replaces a number: no drop glue to run.
        std::mem::forget(std::mem::replace(el, copy_value(&new)));
        Some((old, new))
    };
    let r = match slots.get_mut(hit.slot)? {
        Some(Value::Array(arr)) => step(arr),
        Some(Value::Reference(r)) => match &mut *r.try_borrow_mut()? {
            Value::Array(arr) => step(arr),
            _ => None,
        },
        _ => None,
    };
    drop(slots);
    let (old, new) = r?;
    super::release_str_tmp_in(f, dim);
    Some(if kind.is_post() { old } else { new })
}

/// `isset()`/`empty()` of the property `hit` describes in `od` when it is
/// an initialized declared slot or the existing dynamic property
/// `dyn_prop`.
#[inline(always)]
fn isset_in(od: &ObjectData, slot: usize, dyn_prop: Option<&ZStr>, empty: bool) -> Option<bool> {
    if let Some(name) = dyn_prop {
        let table = od.dynamic.try_borrow().ok()?;
        let r = isset_deref(table.as_deref()?.get_zstr(name)?, empty);
        return r;
    }
    let slots = od.slots.try_borrow().ok()?;
    let r = match slots.get(slot)? {
        Some(v) => isset_deref(v, empty),
        None => None,
    };
    r
}

/// `ZEND_ISSET_ISEMPTY_PROP_OBJ` on a cache hit: an initialized declared
/// property or an existing dynamic property (`zend_std_has_property`
/// returns without `__isset()`).
#[inline(always)]
fn isset_hit(f: &Frame, pid: PlaceId, empty: bool) -> Option<bool> {
    let hit = hit_key(f, pid, PC_ISSET, 0)?;
    let dyn_prop = dyn_name(f, &hit);
    if hit.flags & PC_DYN != 0 && dyn_prop.is_none() {
        return None;
    }
    let slot = hit.slot;
    if let Some(od) = hit_data(f, &hit) {
        return isset_in(od, slot, dyn_prop, empty);
    }
    if !base_is_ref(f, &hit) {
        return None;
    }
    with_hit_obj(f, &hit, |od| isset_in(od, slot, dyn_prop, empty))
}

/// Stores `new_val` into the property `hit` describes in `od` (the
/// dynamic property `dyn_key` when set): replaces the value (not a
/// reference) of an initialized declared slot or of an existing dynamic
/// property, or fills an undefined declared slot that `__set()` cannot
/// claim. Returns the replaced value (`Some(None)` for a filled slot), or
/// `None`, having changed nothing.
#[inline(always)]
fn store_in(
    od: &ObjectData,
    hit: &Hit,
    dyn_key: Option<ArrayKey>,
    new_val: Value,
) -> Option<Option<Value>> {
    if let Some(key) = dyn_key {
        let mut table = od.dynamic.try_borrow_mut().ok()?;
        let cur = table.as_deref_mut()?.get_mut(&key)?;
        if matches!(cur, Value::Reference(_)) {
            return None;
        }
        return Some(Some(std::mem::replace(cur, new_val)));
    }
    let slot = hit.slot;
    let mut slots = od.slots.try_borrow_mut().ok()?;
    let cell = slots.get_mut(slot)?;
    match cell {
        Some(Value::Reference(_)) => None,
        Some(cur) => Some(Some(std::mem::replace(cur, new_val))),
        None => {
            // `write_std_property` stores into an undefined declared
            // slot without `__set()` when the class has none, or when
            // the typed slot is uninitialized rather than unset.
            if hit.flags & PC_NO_SET != 0 || (hit.accept != u32::MAX && !od.slot_unset(slot)) {
                *cell = Some(new_val);
                od.set_slot_unset(slot, false);
                Some(None)
            } else {
                None
            }
        }
    }
}

/// `ZEND_ASSIGN_OBJ` with a plain `OP_DATA` on a cache hit, under one look
/// at frame `f`: the value (not a reference, of a type the property takes
/// as-is) replaces the slot's value (not a reference), or fills an
/// undefined slot that `__set()` cannot claim, or replaces the value (not
/// a reference) of an existing dynamic property (creating one may warn);
/// a consumed temporary operand is released and the result written to
/// `dst` when used. Returns `false`, having changed nothing, otherwise.
#[inline(always)]
fn assign_hit(f: &mut Frame, pid: PlaceId, val: Operand, dst: Option<u32>) -> bool {
    let Some(hit) = hit_key(f, pid, PC_WRITE, 0) else {
        return false;
    };
    let dyn_key = match dyn_name(f, &hit) {
        Some(name) => Some(ArrayKey::Str(name.clone())),
        None if hit.flags & PC_DYN != 0 => return false,
        None => None,
    };
    let src = match val {
        Operand::Tmp(t) | Operand::TmpKeep(t) => f.tmps.get(t as usize),
        Operand::Cv(cv) => f.cvs.get(cv as usize).and_then(CvSlot::get),
        Operand::Lit(lit) => f.code.literals.get(lit as usize),
        Operand::Unused => None,
    };
    let Some(src) = src else {
        return false;
    };
    if matches!(src, Value::Reference(_)) || !hit_accepts(&hit, src) {
        return false;
    }
    let new_val = copy_value(src);
    let result = dst.map(|_| copy_value(&new_val));
    let old = if let Some(od) = hit_data(f, &hit) {
        store_in(od, &hit, dyn_key, new_val)
    } else if base_is_ref(f, &hit) {
        with_hit_obj(f, &hit, |od| store_in(od, &hit, dyn_key, new_val))
    } else {
        None
    };
    let Some(old) = old else {
        return false;
    };
    // The old value is released once nothing is borrowed (a scalar is
    // forgotten).
    discard_unrooted(old);
    if let Operand::Tmp(t) = val {
        if let Some(slot) = f.tmps.get_mut(t as usize) {
            put_tmp(slot, Value::Null);
        }
    }
    if let (Some(d), Some(r)) = (dst, result) {
        if let Some(slot) = f.tmps.get_mut(d as usize) {
            put_tmp(slot, r);
        }
    }
    true
}

/// `ZEND_ASSIGN_OBJ_OP` of a number on the initialized declared slot
/// `slot` of `od` holding a number: `+`, `-`, `*` (an int result that
/// overflows becomes a float only where the property's type, `accept`,
/// allows one) and the bitwise ops of two ints. Returns the new value, or
/// `None`, having changed nothing.
#[inline(always)]
#[allow(clippy::too_many_lines, clippy::cast_precision_loss)] // PHP's int-to-float overflow
fn assign_op_in(
    od: &ObjectData,
    slot: usize,
    accept: u32,
    op: BinaryOp,
    rhs: Num,
) -> Option<Value> {
    if let Ok(mut slots) = od.slots.try_borrow_mut() {
        if let Some(Some(ref mut cell)) = slots.get_mut(slot) {
            match cell {
                Value::Int(ref mut n) => match rhs {
                    Num::Int(y) => match op {
                        BinaryOp::Add => {
                            if let Some(res) = n.checked_add(y) {
                                *n = res;
                                return Some(Value::Int(res));
                            } else {
                                let res = *n as f64 + y as f64;
                                if accept == u32::MAX || (accept & crate::types::mask::DOUBLE != 0)
                                {
                                    *cell = Value::Float(res);
                                    return Some(Value::Float(res));
                                }
                            }
                        }
                        BinaryOp::Sub => {
                            if let Some(res) = n.checked_sub(y) {
                                *n = res;
                                return Some(Value::Int(res));
                            } else {
                                let res = *n as f64 - y as f64;
                                if accept == u32::MAX || (accept & crate::types::mask::DOUBLE != 0)
                                {
                                    *cell = Value::Float(res);
                                    return Some(Value::Float(res));
                                }
                            }
                        }
                        BinaryOp::Mul => {
                            if let Some(res) = n.checked_mul(y) {
                                *n = res;
                                return Some(Value::Int(res));
                            } else {
                                let res = *n as f64 * y as f64;
                                if accept == u32::MAX || (accept & crate::types::mask::DOUBLE != 0)
                                {
                                    *cell = Value::Float(res);
                                    return Some(Value::Float(res));
                                }
                            }
                        }
                        BinaryOp::BitAnd => {
                            *n &= y;
                            return Some(Value::Int(*n));
                        }
                        BinaryOp::BitOr => {
                            *n |= y;
                            return Some(Value::Int(*n));
                        }
                        BinaryOp::BitXor => {
                            *n ^= y;
                            return Some(Value::Int(*n));
                        }
                        _ => {}
                    },
                    Num::Float(y) => {
                        let res = match op {
                            BinaryOp::Add => *n as f64 + y,
                            BinaryOp::Sub => *n as f64 - y,
                            BinaryOp::Mul => *n as f64 * y,
                            _ => return None,
                        };
                        if accept == u32::MAX || (accept & crate::types::mask::DOUBLE != 0) {
                            *cell = Value::Float(res);
                            return Some(Value::Float(res));
                        }
                    }
                },
                Value::Float(ref mut f) => {
                    let y = match rhs {
                        Num::Int(i) => i as f64,
                        Num::Float(fl) => fl,
                    };
                    match op {
                        BinaryOp::Add => {
                            *f += y;
                            return Some(Value::Float(*f));
                        }
                        BinaryOp::Sub => {
                            *f -= y;
                            return Some(Value::Float(*f));
                        }
                        BinaryOp::Mul => {
                            *f *= y;
                            return Some(Value::Float(*f));
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// [`assign_op_in`] on a cache hit of place `pid` in frame `f`: the new
/// value; a consumed temporary operand (a number) is left as it is.
#[inline(always)]
fn assign_op_hit(f: &Frame, pid: PlaceId, op: BinaryOp, val: Operand) -> Option<Value> {
    let hit = hit_key(f, pid, PC_READ | PC_WRITE, PC_DYN)?;
    let rhs = Vm::peek_num(f, val)?;
    if let Some(od) = hit_data(f, &hit) {
        return assign_op_in(od, hit.slot, hit.accept, op, rhs);
    }
    if !base_is_ref(f, &hit) {
        return None;
    }
    with_hit_obj(f, &hit, |od| {
        assign_op_in(od, hit.slot, hit.accept, op, rhs)
    })
}

/// `ZEND_PRE_INC_OBJ` & co. on a number in the initialized declared slot
/// `slot` of `od` (an int that would overflow takes the generic path): the
/// op's result.
#[inline(always)]
fn incdec_in(od: &ObjectData, slot: usize, kind: IncDecKind) -> Option<Value> {
    let mut slots = od.slots.try_borrow_mut().ok()?;
    let Some(Some(cell)) = slots.get_mut(slot) else {
        return None;
    };
    match cell {
        Value::Int(n) => {
            let old = *n;
            let new = if kind.is_inc() {
                old.checked_add(1)
            } else {
                old.checked_sub(1)
            };
            new.map(|new_n| {
                *n = new_n;
                Value::Int(if kind.is_post() { old } else { new_n })
            })
        }
        Value::Float(x) => {
            let old = *x;
            let new_x = if kind.is_inc() { old + 1.0 } else { old - 1.0 };
            *x = new_x;
            Some(Value::Float(if kind.is_post() { old } else { new_x }))
        }
        _ => None,
    }
}

/// [`incdec_in`] on a cache hit of place `pid` in frame `f`.
#[inline(always)]
fn incdec_hit(f: &Frame, pid: PlaceId, kind: IncDecKind) -> Option<Value> {
    let hit = hit_key(f, pid, PC_READ | PC_WRITE, PC_DYN)?;
    if let Some(od) = hit_data(f, &hit) {
        return incdec_in(od, hit.slot, kind);
    }
    if !base_is_ref(f, &hit) {
        return None;
    }
    with_hit_obj(f, &hit, |od| incdec_in(od, hit.slot, kind))
}

/// The frame-local fast paths of the property ops (`exec/hot.rs`, which
/// runs them in its own loop like the other run-time cache hits): `$o->p`
/// read, `=`, `op=` of a number, `++`/`--` and `isset()`/`empty()` on a
/// property cache hit, exactly as the [`Vm`] fast paths below do on a hit.
/// Returns `false`, having changed nothing, when the hit path does not
/// apply (the op then runs as its generic op, whose fast path also fills
/// the cache); `ip` is advanced past a folded `AssignCv`.
#[inline(always)]
pub(in crate::exec) fn prop_op_in(f: &mut Frame, op: &Op, ip: &mut usize) -> bool {
    match *op {
        Op::FetchObjProp {
            place,
            dst,
            to,
            mode,
        } => {
            if mode == FetchMode::List {
                return false;
            }
            let Some(v) = read_hit(f, place) else {
                return false;
            };
            if store_fetched_in(f, dst, to, v) {
                *ip += 1;
            }
            true
        }
        Op::AssignObjProp { place, val, dst } => assign_hit(f, place, val, dst),
        Op::AssignOpObjProp {
            place,
            op: kind,
            val,
            dst,
        } => {
            let Some(res) = assign_op_hit(f, place, kind, val) else {
                return false;
            };
            if let Operand::Tmp(t) = val {
                if let Some(slot) = f.tmps.get_mut(t as usize) {
                    put_tmp(slot, Value::Null);
                }
            }
            put_dst(f, dst, res);
            true
        }
        Op::IncDecObjProp { place, kind, dst } => {
            let Some(res) = incdec_hit(f, place, kind) else {
                return false;
            };
            put_dst(f, dst, res);
            true
        }
        Op::IssetObjProp { place, dst, empty } => {
            let Some(r) = isset_hit(f, place, empty) else {
                return false;
            };
            match f.tmps.get_mut(dst as usize) {
                Some(slot) => {
                    put_tmp(slot, Value::Bool(r));
                    true
                }
                None => false,
            }
        }
        Op::New {
            class: ClassSpec::Named(lit),
            dst,
            skip,
        } => {
            if dst as usize >= f.tmps.len() {
                return false;
            }
            let Some(cls) = cached_class(f, lit) else {
                return false;
            };
            let o = if cls.magic.construct.is_none() {
                // `new C` without arguments of a class without a
                // constructor (`skip` jumps over the `DoCall`). With
                // arguments, the generic op starts the placeholder call
                // that evaluates them.
                if skip == 0 {
                    return false;
                }
                let Some(o) = crate::props::new_plain(&cls) else {
                    return false;
                };
                *ip = skip as usize;
                o
            } else {
                // `new C(...)` of a class with a constructor: the object
                // and the constructor call on it (`DoCall` runs it).
                let Some(o) = crate::props::new_with_ctor(cls, &mut f.calls) else {
                    return false;
                };
                o
            };
            if let Some(slot) = f.tmps.get_mut(dst as usize) {
                put_tmp(slot, Value::Object(o));
            }
            true
        }
        _ => false,
    }
}

/// The class named by literal `lit` when it is in frame `f`'s run-time
/// cache (`CACHED_PTR`, filled by the generic op's
/// `Vm::resolve_class_spec`, which returns the cached class without
/// further effects).
#[inline(always)]
fn cached_class(f: &Frame, lit: u32) -> Option<Rc<crate::class::Class>> {
    f.code
        .class_cache
        .try_borrow()
        .ok()?
        .get(lit as usize)?
        .upgrade()
}

/// `Vm::set_dst` on frame `f`: the result of an op into its used
/// temporary; an unused scalar result is forgotten (no out-of-line
/// `drop_in_place`).
#[inline(always)]
fn put_dst(f: &mut Frame, dst: Option<u32>, v: Value) {
    if let Some(slot) = dst.and_then(|d| f.tmps.get_mut(d as usize)) {
        put_tmp(slot, v);
    } else {
        discard_unrooted(Some(v));
    }
}

impl Vm {
    /// `Fetch` of `$cv->name` / `$this->name` on a property cache hit:
    /// reads the slot directly, folds the following `AssignCv` when `to` is
    /// set, and returns whether `to` was written (`Some(true)`: the loop skips
    /// that op) or `dst` was (`Some(false)`). `None` on a cache miss (or
    /// `FetchMode::List`).
    #[inline(always)]
    pub(in crate::exec) fn fetch_obj_prop_fast(
        &mut self,
        pid: PlaceId,
        dst: u32,
        to: Option<u32>,
        mode: FetchMode,
    ) -> Option<bool> {
        if mode == FetchMode::List {
            return None;
        }
        // The hit: the read and the folded store under one look at the
        // frame.
        if let Some(f) = self.frames.last_mut() {
            if let Some(v) = read_hit(f, pid) {
                return Some(store_fetched_in(f, dst, to, v));
            }
        }
        let v = self.prop_cache_read(pid)?;
        Some(self.store_fetched(dst, to, v))
    }

    /// `Fetch` of `$cv->name[dim]` / `$this->name[dim]` on a property cache
    /// hit: reads the array element directly; `to` as in
    /// [`Vm::fetch_obj_prop_fast`].
    #[inline]
    pub(in crate::exec) fn fetch_obj_prop_dim_fast(
        &mut self,
        pid: PlaceId,
        dim: Operand,
        dst: u32,
        to: Option<u32>,
        mode: FetchMode,
    ) -> Option<bool> {
        if mode == FetchMode::List {
            return None;
        }
        if let Some(f) = self.frames.last_mut() {
            if let Some(v) = read_dim_hit(f, pid, dim) {
                super::release_str_tmp_in(f, dim);
                return Some(store_fetched_in(f, dst, to, v));
            }
        }
        let key = self.peek_dim_key(dim)?;
        let v = self.prop_cache_read_elem(pid, &key)?;
        self.release_tmp_str(dim);
        Some(self.store_fetched(dst, to, v))
    }

    /// `$cv->name = val` / `$this->name = val` on a property cache hit:
    /// writes a plain value directly into the property slot. Returns `true`
    /// on hit, `false` otherwise.
    #[inline(always)]
    pub(in crate::exec) fn assign_obj_prop_fast(
        &mut self,
        pid: PlaceId,
        val: Operand,
        dst: Option<u32>,
    ) -> bool {
        if let Some(f) = self.frames.last_mut() {
            if assign_hit(f, pid, val, dst) {
                return true;
            }
        }
        let Some(v) = self.peek_plain_value(val) else {
            return false;
        };
        match self.prop_cache_assign(pid, v) {
            Ok(r) => {
                if let Operand::Tmp(t) = val {
                    self.set_tmp(t, Value::Null);
                }
                if let Some(d) = dst {
                    self.set_tmp(d, r);
                }
                true
            }
            Err(_) => false,
        }
    }

    /// `$cv->name[dim] = val` / `$this->name[dim] = val` (or append `[]`
    /// when `dim` is [`Operand::Unused`]) on a property cache hit.
    #[inline]
    pub(in crate::exec) fn assign_obj_prop_dim_fast(
        &mut self,
        pid: PlaceId,
        dim: Operand,
        val: Operand,
        dst: Option<u32>,
    ) -> bool {
        if let Some(f) = self.frames.last_mut() {
            if assign_dim_hit(f, pid, dim, val, dst) {
                return true;
            }
        }
        let key = match dim {
            Operand::Unused => None,
            other => Some(match self.peek_dim_key(other) {
                Some(k) => k,
                None => return false,
            }),
        };
        let Some(v) = self.peek_dim_value(val) else {
            return false;
        };
        match self.prop_cache_assign_elem(pid, key.as_ref(), v) {
            Ok(r) => {
                self.release_tmp_str(dim);
                if let Operand::Tmp(t) = val {
                    self.set_tmp(t, Value::Null);
                }
                if let Some(d) = dst {
                    self.set_tmp(d, r);
                }
                true
            }
            Err(_) => false,
        }
    }

    /// `$cv->name op= val` / `$this->name op= val` on a property cache hit:
    /// `.=` on a string property, or a numeric op on a number property.
    #[inline(always)]
    pub(in crate::exec) fn assign_op_obj_prop_fast(
        &mut self,
        pid: PlaceId,
        op: BinaryOp,
        val: Operand,
        dst: Option<u32>,
    ) -> bool {
        if let Some(res) = self
            .frames
            .last()
            .and_then(|f| assign_op_hit(f, pid, op, val))
        {
            if let Operand::Tmp(t) = val {
                self.set_tmp(t, Value::Null);
            }
            if let Some(d) = dst {
                self.set_tmp(d, res);
            }
            return true;
        }
        if op == BinaryOp::Concat {
            let Some(rhs) = self.peek_plain_value(val) else {
                return false;
            };
            if let Some(r) = self.prop_cache_concat(pid, &rhs) {
                if let Operand::Tmp(t) = val {
                    self.set_tmp(t, Value::Null);
                }
                if let Some(d) = dst {
                    self.set_tmp(d, r);
                }
                return true;
            }
            return false;
        }
        let Some(rhs) = Self::peek_num(self.frame(), val) else {
            return false;
        };
        let rhs_val = match rhs {
            Num::Int(i) => Value::Int(i),
            Num::Float(f) => Value::Float(f),
        };
        if let Some(r) = self.prop_cache_assign_op_pure(pid, op, &rhs_val) {
            if let Operand::Tmp(t) = val {
                self.set_tmp(t, Value::Null);
            }
            if let Some(d) = dst {
                self.set_tmp(d, r);
            }
            return true;
        }
        false
    }

    /// `$cv->name[dim] op= val` / `$this->name[dim] op= val` on a property
    /// cache hit.
    #[inline]
    pub(in crate::exec) fn assign_op_obj_prop_dim_fast(
        &mut self,
        pid: PlaceId,
        dim: Operand,
        op: BinaryOp,
        val: Operand,
        dst: Option<u32>,
    ) -> bool {
        let Some(key) = self.peek_dim_key(dim) else {
            return false;
        };
        let Some(rhs) = self.peek_plain_value(val) else {
            return false;
        };
        if let Some(r) = self.prop_cache_assign_op_elem(pid, &key, op, &rhs) {
            self.release_tmp_str(dim);
            if let Operand::Tmp(t) = val {
                self.set_tmp(t, Value::Null);
            }
            if let Some(d) = dst {
                self.set_tmp(d, r);
            }
            return true;
        }
        false
    }

    /// `++`/`--` of `$cv->name` / `$this->name` on a property cache hit.
    #[inline(always)]
    pub(in crate::exec) fn incdec_obj_prop_fast(
        &self,
        pid: PlaceId,
        kind: IncDecKind,
    ) -> Option<Value> {
        let fast_val = self.frames.last().and_then(|f| incdec_hit(f, pid, kind));
        if let Some(v) = fast_val {
            return Some(v);
        }
        self.prop_cache_incdec(pid, kind)
    }

    /// `++`/`--` of `$cv->name[dim]` / `$this->name[dim]` on a property
    /// cache hit.
    #[inline]
    pub(in crate::exec) fn incdec_obj_prop_dim_fast(
        &mut self,
        pid: PlaceId,
        dim: Operand,
        kind: IncDecKind,
    ) -> Option<Value> {
        if let Some(f) = self.frames.last_mut() {
            if let Some(r) = incdec_dim_hit(f, pid, dim, kind) {
                return Some(r);
            }
        }
        let key = self.peek_dim_key(dim)?;
        let r = self.prop_cache_incdec_elem(pid, &key, kind)?;
        self.release_tmp_str(dim);
        Some(r)
    }

    /// `isset($cv->name)` / `empty($cv->name)` on a property cache hit.
    #[inline(always)]
    pub(in crate::exec) fn isset_obj_prop_fast(&self, pid: PlaceId, empty: bool) -> Option<bool> {
        if let Some(r) = self.frames.last().and_then(|f| isset_hit(f, pid, empty)) {
            return Some(r);
        }
        self.prop_cache_isset_pure(pid, empty)
    }

    /// `isset($cv->name[dim])` / `empty($cv->name[dim])` on a property
    /// cache hit.
    #[inline]
    pub(in crate::exec) fn isset_obj_prop_dim_fast(
        &mut self,
        pid: PlaceId,
        dim: Operand,
        empty: bool,
    ) -> Option<bool> {
        if let Some(f) = self.frames.last_mut() {
            if let Some(r) = isset_dim_hit(f, pid, dim, empty) {
                super::release_str_tmp_in(f, dim);
                return Some(r);
            }
        }
        let key = self.peek_dim_key(dim)?;
        let r = self.prop_cache_isset_elem_pure(pid, &key, empty)?;
        self.release_tmp_str(dim);
        Some(r)
    }

    /// Handler for [`Op::FetchObjProp`].
    pub(in crate::exec) fn op_fetch_obj_prop(
        &mut self,
        place: PlaceId,
        dst: u32,
        to: Option<u32>,
        mode: FetchMode,
        ip: &mut usize,
    ) -> VmResult<Flow> {
        if let Some(skipped) = self.fetch_obj_prop_fast(place, dst, to, mode) {
            if skipped {
                *ip += 1;
            }
            return Ok(Flow::Next);
        }
        let v = self.place_fetch(place, mode)?;
        self.set_tmp(dst, v);
        Ok(Flow::Next)
    }

    /// Handler for [`Op::FetchObjPropDim`].
    pub(in crate::exec) fn op_fetch_obj_prop_dim(
        &mut self,
        place: PlaceId,
        dim: Operand,
        dst: u32,
        to: Option<u32>,
        mode: FetchMode,
        ip: &mut usize,
    ) -> VmResult<Flow> {
        if let Some(skipped) = self.fetch_obj_prop_dim_fast(place, dim, dst, to, mode) {
            if skipped {
                *ip += 1;
            }
            return Ok(Flow::Next);
        }
        let v = self.place_fetch(place, mode)?;
        self.set_tmp(dst, v);
        Ok(Flow::Next)
    }

    /// Handler for [`Op::AssignObjProp`].
    pub(in crate::exec) fn op_assign_obj_prop(
        &mut self,
        place: PlaceId,
        val: Operand,
        dst: Option<u32>,
    ) -> VmResult<Flow> {
        if self.assign_obj_prop_fast(place, val, dst) {
            return Ok(Flow::Next);
        }
        let r = match val {
            Operand::Cv(cv) => match self.read_cv_op_data(cv) {
                super::super::CvOpData::Value(v) => match self.prop_cache_assign(place, v) {
                    Ok(r) => r,
                    Err(v) => self.place_assign(place, v)?,
                },
                super::super::CvOpData::Deferred => self.place_assign_cv_ref(place, cv)?,
                super::super::CvOpData::Undefined => self.place_assign_undef_cv(place, cv)?,
            },
            other => {
                let v = self.read(other)?;
                let v = self.copy_on_assign(v);
                match self.prop_cache_assign(place, v) {
                    Ok(r) => r,
                    Err(v) => self.place_assign(place, v)?,
                }
            }
        };
        self.set_dst(dst, r);
        Ok(Flow::Next)
    }

    /// Handler for [`Op::AssignObjPropDim`].
    pub(in crate::exec) fn op_assign_obj_prop_dim(
        &mut self,
        place: PlaceId,
        dim: Operand,
        val: Operand,
        dst: Option<u32>,
    ) -> VmResult<Flow> {
        if self.assign_obj_prop_dim_fast(place, dim, val, dst) {
            return Ok(Flow::Next);
        }
        let key = match dim {
            Operand::Unused => None,
            other => self.peek_dim_key(other),
        };
        let r = match val {
            Operand::Cv(cv) => match self.read_cv_op_data(cv) {
                super::super::CvOpData::Value(v) => {
                    if dim == Operand::Unused || key.is_some() {
                        match self.prop_cache_assign_elem(place, key.as_ref(), v) {
                            Ok(r) => {
                                self.release_tmp_str(dim);
                                r
                            }
                            Err(v) => self.place_assign(place, v)?,
                        }
                    } else {
                        self.place_assign(place, v)?
                    }
                }
                super::super::CvOpData::Deferred => self.place_assign_cv_ref(place, cv)?,
                super::super::CvOpData::Undefined => self.place_assign_undef_cv(place, cv)?,
            },
            other => {
                let v = self.read(other)?;
                let v = self.copy_on_assign(v);
                if dim == Operand::Unused || key.is_some() {
                    match self.prop_cache_assign_elem(place, key.as_ref(), v) {
                        Ok(r) => {
                            self.release_tmp_str(dim);
                            r
                        }
                        Err(v) => self.place_assign(place, v)?,
                    }
                } else {
                    self.place_assign(place, v)?
                }
            }
        };
        self.set_dst(dst, r);
        Ok(Flow::Next)
    }

    /// Handler for [`Op::AssignOpObjProp`].
    pub(in crate::exec) fn op_assign_op_obj_prop(
        &mut self,
        place: PlaceId,
        op: BinaryOp,
        val: Operand,
        dst: Option<u32>,
    ) -> VmResult<Flow> {
        if self.assign_op_obj_prop_fast(place, op, val, dst) {
            return Ok(Flow::Next);
        }
        let v = self.read(val)?;
        let r = if op == BinaryOp::Concat {
            match self.prop_cache_concat(place, &v) {
                Some(r) => r,
                None => self.place_assign_op(place, op, v)?,
            }
        } else {
            match self.prop_cache_assign_op(place, op, &v)? {
                Some(r) => r,
                None => self.place_assign_op(place, op, v)?,
            }
        };
        self.set_dst(dst, r);
        Ok(Flow::Next)
    }

    /// Handler for [`Op::AssignOpObjPropDim`].
    pub(in crate::exec) fn op_assign_op_obj_prop_dim(
        &mut self,
        place: PlaceId,
        dim: Operand,
        op: BinaryOp,
        val: Operand,
        dst: Option<u32>,
    ) -> VmResult<Flow> {
        if self.assign_op_obj_prop_dim_fast(place, dim, op, val, dst) {
            return Ok(Flow::Next);
        }
        let key = self.peek_dim_key(dim);
        let v = self.read(val)?;
        let r = if let Some(key) = key {
            match self.prop_cache_assign_op_elem(place, &key, op, &v) {
                Some(r) => {
                    self.release_tmp_str(dim);
                    r
                }
                None => self.place_assign_op(place, op, v)?,
            }
        } else {
            self.place_assign_op(place, op, v)?
        };
        self.set_dst(dst, r);
        Ok(Flow::Next)
    }

    /// Handler for [`Op::IncDecObjProp`].
    pub(in crate::exec) fn op_incdec_obj_prop(
        &mut self,
        place: PlaceId,
        kind: IncDecKind,
        dst: Option<u32>,
    ) -> VmResult<Flow> {
        let r = match self.incdec_obj_prop_fast(place, kind) {
            Some(r) => r,
            None => self.place_incdec(place, kind)?,
        };
        self.set_dst(dst, r);
        Ok(Flow::Next)
    }

    /// Handler for [`Op::IncDecObjPropDim`].
    pub(in crate::exec) fn op_incdec_obj_prop_dim(
        &mut self,
        place: PlaceId,
        dim: Operand,
        kind: IncDecKind,
        dst: Option<u32>,
    ) -> VmResult<Flow> {
        let r = match self.incdec_obj_prop_dim_fast(place, dim, kind) {
            Some(r) => r,
            None => self.place_incdec(place, kind)?,
        };
        self.set_dst(dst, r);
        Ok(Flow::Next)
    }

    /// Handler for [`Op::IssetObjProp`].
    pub(in crate::exec) fn op_isset_obj_prop(
        &mut self,
        place: PlaceId,
        dst: u32,
        empty: bool,
    ) -> VmResult<Flow> {
        let v = match self.isset_obj_prop_fast(place, empty) {
            Some(v) => v,
            None => match self.prop_cache_isset(place, empty)? {
                Some(v) => v,
                None => self.place_isset(place, empty)?,
            },
        };
        self.set_tmp(dst, Value::Bool(v));
        Ok(Flow::Next)
    }

    /// Handler for [`Op::IssetObjPropDim`].
    pub(in crate::exec) fn op_isset_obj_prop_dim(
        &mut self,
        place: PlaceId,
        dim: Operand,
        dst: u32,
        empty: bool,
    ) -> VmResult<Flow> {
        let v = match self.isset_obj_prop_dim_fast(place, dim, empty) {
            Some(v) => v,
            None => self.place_isset(place, empty)?,
        };
        self.set_tmp(dst, Value::Bool(v));
        Ok(Flow::Next)
    }
}

/// The keys of the dimensions after the head `->name` of place `pid` of
/// frame `f` (`$base->name[d1]...[dn]`, one to [`super::MAX_WRITE_DIMS`]
/// of them, the last possibly `[]`), as [`dim_keys_in`]. `None` for any
/// other shape.
fn prop_dim_keys_in(
    f: &Frame,
    pid: PlaceId,
) -> Option<([Option<ArrayKey>; super::MAX_WRITE_DIMS], usize)> {
    let segs = f.code.places.get(pid as usize)?.segs.get(1..)?;
    dim_keys_in(f, segs)
}

/// The keys of the dimension segments `segs` of frame `f` (at most
/// [`super::MAX_WRITE_DIMS`]), and how many: every offset peeks as an int
/// or a string ([`super::dim_key_in`]), the last segment may be `[]`
/// (`None`, an append). All are computed before a write walk changes
/// anything.
pub(super) fn dim_keys_in(
    f: &Frame,
    segs: &[crate::opcode::Seg],
) -> Option<([Option<ArrayKey>; super::MAX_WRITE_DIMS], usize)> {
    use crate::opcode::Seg;
    if segs.is_empty() || segs.len() > super::MAX_WRITE_DIMS {
        return None;
    }
    let mut keys: [Option<ArrayKey>; super::MAX_WRITE_DIMS] = Default::default();
    let last = segs.len() - 1;
    for (i, (key, seg)) in keys.iter_mut().zip(segs).enumerate() {
        *key = match seg {
            Seg::Dim(dim) => Some(super::dim_key_in(f, *dim)?),
            Seg::Append if i == last => None,
            _ => return None,
        };
    }
    Some((keys, segs.len()))
}

/// `unset($this->name[d1]...[dn])` / `unset($cv->name[...])` of place
/// `pid` of frame `f` on a property cache hit for writing (a declared,
/// initialized, accessible slot that is neither readonly nor
/// set-restricted, holding an array directly or through a reference not
/// borrowed elsewhere): `ZEND_FETCH_OBJ_UNSET`, which hands out the slot
/// (`get_property_ptr_ptr` with `BP_VAR_UNSET`: no magic method is
/// consulted for an initialized slot), then the dimension chain of
/// [`super::dims::unset_dims`]. A consumed string offset is released and
/// the removed element dropped. `false`, nothing changed but separations,
/// for the generic walk.
#[inline(never)]
pub(super) fn unset_prop_dims_in(f: &mut Frame, pid: PlaceId) -> bool {
    let Some(hit) = hit_key(f, pid, PC_WRITE, PC_DYN) else {
        return false;
    };
    let Some((keys, n)) = prop_dim_keys_in(f, pid) else {
        return false;
    };
    let Some(keys) = keys.get(..n) else {
        return false;
    };
    let slot = hit.slot;
    let removed = with_hit_obj(f, &hit, |od| {
        let mut slots = od.slots.try_borrow_mut().ok()?;
        let res = match slots.get_mut(slot)? {
            Some(Value::Array(arr)) => super::dims::unset_dims(arr, keys),
            Some(Value::Reference(r)) => {
                let mut guard = r.try_borrow_mut()?;
                let res = match &mut *guard {
                    Value::Array(arr) => super::dims::unset_dims(arr, keys),
                    _ => None,
                };
                drop(guard);
                res
            }
            _ => None,
        };
        drop(slots);
        res
    });
    let Some(removed) = removed else {
        return false;
    };
    super::release_place_str_tmps(f, pid);
    drop(removed);
    true
}

/// The dimensions after the head `->name` of place `pid` of frame `f`
/// when the place is `$base->name[d1]...[dn]` (one or more offsets, none
/// `[]`).
#[inline(always)]
fn prop_dim_segs(f: &Frame, pid: PlaceId) -> Option<&[crate::opcode::Seg]> {
    let segs = f.code.places.get(pid as usize)?.segs.get(1..)?;
    (!segs.is_empty()).then_some(segs)
}

/// The value of `cur[d1]...[dn]` along the dimension segments `segs` of
/// frame `f`, cloned and dereferenced, when every container is an array
/// (`cur` itself possibly through a reference) holding the int or string
/// offset its operand peeks as: the chain of `ZEND_FETCH_DIM_R`/`_IS`
/// hits. `None` (a miss, which warns in `Read` mode, a string offset, an
/// object, an element held by reference on the way) for the generic walk.
#[inline]
fn dims_read(f: &Frame, cur: &Value, segs: &[crate::opcode::Seg]) -> Option<Value> {
    let guard;
    let mut cur = match cur {
        Value::Reference(r) => {
            guard = r.try_borrow()?;
            &*guard
        }
        v => v,
    };
    for seg in segs {
        let crate::opcode::Seg::Dim(dim) = seg else {
            return None;
        };
        let key = super::dim_ref_in(f, *dim)?;
        let Value::Array(a) = cur else {
            return None;
        };
        cur = super::table_dim(a, key)?;
    }
    Some(match cur {
        Value::Reference(r) => copy_value(&*r.try_borrow()?),
        v => copy_value(v),
    })
}

/// `isset(cur[d1]...[dn])` (or, with `empty`, `empty(...)`) along `segs`
/// of frame `f`: a missing key, or a null container on the way, answers
/// at once (`ZEND_FETCH_DIM_IS` yields null, `ZEND_ISSET_ISEMPTY_DIM_OBJ`
/// on null is false); the last element is tested without conversions.
/// `None` for a string offset, an object (`offsetExists()`), an element
/// held by reference on the way, an offset that needs converting, or a
/// value whose truth needs a cast handler.
#[inline]
fn dims_isset(f: &Frame, cur: &Value, segs: &[crate::opcode::Seg], empty: bool) -> Option<bool> {
    let guard;
    let mut cur = match cur {
        Value::Reference(r) => {
            guard = r.try_borrow()?;
            &*guard
        }
        v => v,
    };
    for seg in segs {
        let crate::opcode::Seg::Dim(dim) = seg else {
            return None;
        };
        let key = super::dim_ref_in(f, *dim)?;
        cur = match cur {
            Value::Array(a) => match super::table_dim(a, key) {
                Some(v) => v,
                None => return Some(empty),
            },
            Value::Null => return Some(empty),
            _ => return None,
        };
    }
    isset_deref(cur, empty)
}

/// `cur[k1]...[kn] = v` on the table `tbl` of an array along `keys` (all
/// `Some` but possibly the last, `None` being `[]`): the
/// `ZEND_FETCH_DIM_W` chain, separating each nested array
/// (`SEPARATE_ARRAY`) and making a missing or null element on the way an
/// empty array (silently, as the generic walk's `descend` does), then
/// `ZEND_ASSIGN_DIM`'s store of `v` over the element, or added, or
/// appended. `Some(old)` on success (the replaced element, null for an
/// added one; `v` taken); `None`, `v` left and nothing created, when an
/// element on the way is of another kind (a reference, an object, a
/// string, `false`, ...), the last one is a reference (the generic
/// assignment writes through it) or an append finds the next index taken
/// (the generic assignment throws; an array created on the way is empty,
/// so this only happens to one that existed). Arrays on the way may have
/// been separated, which the generic walk repeats as no-ops.
pub(super) fn store_dims(
    tbl: &mut php_runtime::HashTable,
    keys: &[Option<ArrayKey>],
    v: &mut Option<Value>,
) -> Option<Value> {
    let (first, rest) = keys.split_first()?;
    if rest.is_empty() {
        let Some(key) = first else {
            let new = v.take()?;
            return match tbl.push(new) {
                Ok(_) => Some(Value::Null),
                Err(back) => {
                    *v = Some(back);
                    None
                }
            };
        };
        if matches!(tbl.get(key), Some(Value::Reference(_))) {
            return None;
        }
        let new = v.take()?;
        return Some(std::mem::replace(tbl.lookup_ex(key.clone()).0, new));
    }
    let key = first.as_ref()?;
    let (elem, _) = tbl.lookup_ex(key.clone());
    if matches!(elem, Value::Null) {
        *elem = Value::Array(php_runtime::ZArray::new());
    }
    let Value::Array(inner) = elem else {
        return None;
    };
    store_dims(inner.make_mut(), rest, v)
}

/// `$this->name[d1]...[dn]` / `$cv->name[...]` read (`Read` or `Is`
/// mode: no diagnostic on a hit) of place `pid` of frame `f` on a
/// property cache hit for reading: `ZEND_FETCH_OBJ_R`/`_IS` of an
/// initialized declared slot or an existing dynamic property, then
/// [`dims_read`]. A consumed string offset is released. `None`, nothing
/// consumed, for the generic fetch.
#[inline(never)]
pub(super) fn read_prop_dims_in(f: &mut Frame, pid: PlaceId) -> Option<Value> {
    let hit = hit_key(f, pid, PC_READ, 0)?;
    let segs = prop_dim_segs(f, pid)?;
    let dyn_prop = dyn_name(f, &hit);
    if hit.flags & PC_DYN != 0 && dyn_prop.is_none() {
        return None;
    }
    let slot = hit.slot;
    let v = with_hit_obj(f, &hit, |od| {
        if let Some(name) = dyn_prop {
            let table = od.dynamic.try_borrow().ok()?;
            let r = dims_read(f, table.as_deref()?.get_zstr(name)?, segs);
            drop(table);
            return r;
        }
        let slots = od.slots.try_borrow().ok()?;
        let r = dims_read(f, slots.get(slot)?.as_ref()?, segs);
        drop(slots);
        r
    })?;
    super::release_place_str_tmps(f, pid);
    Some(v)
}

/// `isset($this->name[d1]...[dn])` / `empty(...)` (and on `$cv->name`) of
/// place `pid` of frame `f` on a property cache hit for `isset()`:
/// `ZEND_FETCH_OBJ_IS` of an initialized declared slot or an existing
/// dynamic property, then [`dims_isset`]. A consumed string offset is
/// released. `None`, nothing consumed, for the generic `place_isset`.
#[inline(never)]
pub(super) fn isset_prop_dims_in(f: &mut Frame, pid: PlaceId, empty: bool) -> Option<bool> {
    let hit = hit_key(f, pid, PC_ISSET, 0)?;
    let segs = prop_dim_segs(f, pid)?;
    let dyn_prop = dyn_name(f, &hit);
    if hit.flags & PC_DYN != 0 && dyn_prop.is_none() {
        return None;
    }
    let slot = hit.slot;
    let r = with_hit_obj(f, &hit, |od| {
        if let Some(name) = dyn_prop {
            let table = od.dynamic.try_borrow().ok()?;
            let r = dims_isset(f, table.as_deref()?.get_zstr(name)?, segs, empty);
            drop(table);
            return r;
        }
        let slots = od.slots.try_borrow().ok()?;
        let r = dims_isset(f, slots.get(slot)?.as_ref()?, segs, empty);
        drop(slots);
        r
    })?;
    super::release_place_str_tmps(f, pid);
    Some(r)
}

/// `$this->name[d1]...[dn] = v` / `$cv->name[...] = v` (one to four
/// offsets, none `[]`) of place `pid` of frame `f` on a property cache hit
/// for writing, when the declared, initialized slot holds an array
/// directly: `ZEND_FETCH_OBJ_W` (which hands out the slot: no magic
/// method, no type check for a nested write into an array) and the chain
/// of [`store_dims`]. Returns the replaced element (to be released once
/// nothing is borrowed), `v` taken; `None`, `v` left, for the generic
/// assignment (a null or `false` property, whose promotion a type may
/// forbid, a reference, a dynamic property, ...).
#[inline(never)]
pub(super) fn assign_prop_dims_in(
    f: &mut Frame,
    pid: PlaceId,
    v: &mut Option<Value>,
) -> Option<Value> {
    let hit = hit_key(f, pid, PC_WRITE, PC_DYN)?;
    let (keys, n) = prop_dim_keys_in(f, pid)?;
    let keys = keys.get(..n)?;
    let slot = hit.slot;
    let old = with_hit_obj(f, &hit, |od| {
        let mut slots = od.slots.try_borrow_mut().ok()?;
        let r = match slots.get_mut(slot)? {
            Some(Value::Array(arr)) => store_dims(arr.make_mut(), keys, v),
            _ => None,
        };
        drop(slots);
        r
    })?;
    super::release_place_str_tmps(f, pid);
    Some(old)
}

/// The object at the end of the dimension segments `segs` of frame `f`
/// from `cur` (itself possibly through a reference): every container an
/// array holding the int or string offset its operand peeks as, the last
/// element an object (also through a reference), its handle copied. The
/// chain of `ZEND_FETCH_DIM_R`/`_IS` hits before a `ZEND_FETCH_OBJ_R`;
/// `None` for anything else.
#[inline]
fn dims_to_object(f: &Frame, cur: &Value, segs: &[crate::opcode::Seg]) -> Option<Obj> {
    let guard;
    let mut cur = match cur {
        Value::Reference(r) => {
            guard = r.try_borrow()?;
            &*guard
        }
        v => v,
    };
    for seg in segs {
        let crate::opcode::Seg::Dim(dim) = seg else {
            return None;
        };
        let key = super::dim_ref_in(f, *dim)?;
        let Value::Array(a) = cur else {
            return None;
        };
        cur = super::table_dim(a, key)?;
    }
    match cur {
        Value::Object(o) => Some(o.clone()),
        Value::Reference(r) => match &*r.try_borrow()? {
            Value::Object(o) => Some(o.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// `ZEND_FETCH_OBJ_R`/`_IS` of a place's last `->name` (literal `lit`) on
/// `obj`, with that segment's own cache entry `idx` (see
/// `place::propcache`'s tail entries), in frame `f` alone: the entry was
/// filled for the object's class and the frame's scope and allows plain
/// reads, the object is not lazy, and the property is an initialized
/// declared slot or an existing dynamic property. Its value, dereferenced
/// and copied; `None` otherwise.
#[inline]
fn read_tail_hit(f: &Frame, idx: usize, lit: u32, obj: &Obj) -> Option<Value> {
    let od = object::data(obj);
    let (slot, flags) = {
        let cache = f.code.prop_cache.try_borrow().ok()?;
        let e = cache.get(idx)?;
        if e.class_addr == 0
            || e.lit != lit
            || Rc::as_ptr(&od.class) as usize != e.class_addr
            || e.scope_addr != scope_addr(f)
            || e.flags & PC_READ == 0
        {
            return None;
        }
        (e.slot as usize, e.flags)
    };
    if !od.lazy.try_borrow().is_ok_and(|l| l.is_none()) {
        return None;
    }
    let dyn_prop = if flags & PC_DYN == 0 {
        None
    } else {
        match f.code.literals.get(lit as usize) {
            Some(Value::Str(name)) => Some(name),
            _ => return None,
        }
    };
    read_in(od, slot, dyn_prop)
}

/// The object a place ending in `->name` (literal `lit`) holds that last
/// property on, reached without the walk: `$this->a->b` / `$cv->a[$k]->b`
/// (the head from its cache entry, as [`read_hit`], then dimensions) or
/// `$cv[$k]...->b` (the variable, possibly a reference, then dimensions),
/// with the index of the last segment's own cache entry and `lit`. The
/// containers are only read: a write through the object needs no
/// separation of the arrays on the way (`FETCH_DIM_W` would separate
/// them, which no PHP code can observe: the copies share the object).
/// `None` for any other shape or container.
#[inline]
fn chain_object(f: &Frame, pid: PlaceId) -> Option<(Obj, usize, u32)> {
    use crate::opcode::Seg;
    let p = f.code.places.get(pid as usize)?;
    let (Seg::Prop(Operand::Lit(lit)), mid) = p.segs.split_last()? else {
        return None;
    };
    if crate::place::propcache::cacheable_head(p) {
        // `[->head, [dims...], ->name]`: one property before the last at
        // least, read through the head's entry.
        let (_, dims) = mid.split_first()?;
        let hit = hit_key(f, pid, PC_READ, PC_DYN)?;
        let od = hit_data(f, &hit)?;
        let slots = od.slots.try_borrow().ok()?;
        let o = dims_to_object(f, slots.get(hit.slot)?.as_ref()?, dims);
        drop(slots);
        return Some((o?, f.code.places.len() + pid as usize, *lit));
    }
    let PlaceBase::Cv(cv) = p.base else {
        return None;
    };
    if mid.is_empty() {
        return None;
    }
    let o = dims_to_object(f, f.cvs.get(cv as usize)?.get()?, mid)?;
    Some((o, pid as usize, *lit))
}

/// A read (`Read` or `Is` mode: no diagnostic on a hit) of a place ending
/// in `->name` whose object [`chain_object`] reaches, the last property
/// read with its own cache entry ([`read_tail_hit`]). A consumed string
/// offset is released. `None`, nothing consumed, for the generic fetch
/// (which also fills the entries).
#[inline(never)]
pub(super) fn read_chain_in(f: &mut Frame, pid: PlaceId) -> Option<Value> {
    let (obj, idx, lit) = chain_object(f, pid)?;
    let v = read_tail_hit(f, idx, lit, &obj)?;
    drop(obj);
    super::release_place_str_tmps(f, pid);
    Some(v)
}

/// The tail cache entry `idx` (literal `lit`) as a [`Hit`] for `obj` in
/// frame `f`, when it was filled for the object's class and the frame's
/// scope, allows every access in `want`, and the object is not lazy.
#[inline]
fn tail_hit(f: &Frame, idx: usize, lit: u32, obj: &Obj, want: u8) -> Option<Hit> {
    let od = object::data(obj);
    let hit = {
        let cache = f.code.prop_cache.try_borrow().ok()?;
        let e = cache.get(idx)?;
        if e.class_addr == 0
            || e.lit != lit
            || Rc::as_ptr(&od.class) as usize != e.class_addr
            || e.scope_addr != scope_addr(f)
            || e.flags & want != want
        {
            return None;
        }
        Hit {
            class: e.class_addr,
            slot: e.slot as usize,
            ok_class: e.ok_class_addr,
            base: e.base,
            lit: e.lit,
            accept: e.accept,
            flags: e.flags,
        }
    };
    if !od.lazy.try_borrow().is_ok_and(|l| l.is_none()) {
        return None;
    }
    Some(hit)
}

/// `ZEND_ASSIGN_OBJ` of `v` to the last `->name` of a place whose object
/// [`chain_object`] reaches (`$this->a->b = v`, `$list[$k]->name = v`),
/// on that segment's cache entry for writing: the value (not a reference,
/// of a type the property takes as-is) replaces an initialized slot's
/// value (not a reference), or fills an undefined slot `__set()` cannot
/// claim, or replaces the value of an existing dynamic property, as
/// [`assign_hit`] does ([`store_in`]). Consumed string offsets are
/// released. The replaced value on success (released by the caller);
/// `None` for the generic assignment, `v` dropped (the caller keeps a
/// copy).
#[inline(never)]
fn assign_chain_in(f: &mut Frame, pid: PlaceId, v: Value) -> Option<Option<Value>> {
    let (obj, idx, lit) = chain_object(f, pid)?;
    let hit = tail_hit(f, idx, lit, &obj, PC_WRITE)?;
    if matches!(v, Value::Reference(_)) || !hit_accepts(&hit, &v) {
        return None;
    }
    let dyn_key = if hit.flags & PC_DYN == 0 {
        None
    } else {
        match f.code.literals.get(lit as usize) {
            Some(Value::Str(name)) => Some(ArrayKey::Str(name.clone())),
            _ => return None,
        }
    };
    let old = store_in(object::data(&obj), &hit, dyn_key, v)?;
    drop(obj);
    super::release_place_str_tmps(f, pid);
    Some(old)
}

/// Whether place `pid` of the current frame is `$this->name[d1]...[dn]`
/// or `$cv->name[...]`: a literal property name and only dimensions
/// after it.
#[inline]
fn is_prop_dims_place(f: &Frame, pid: PlaceId) -> bool {
    f.code.places.get(pid as usize).is_some_and(|p| {
        matches!(p.base, PlaceBase::This | PlaceBase::Cv(_))
            && matches!(
                p.segs.as_slice(),
                [
                    crate::opcode::Seg::Prop(Operand::Lit(_)),
                    crate::opcode::Seg::Dim(_),
                    ..
                ]
            )
    })
}

impl Vm {
    /// Fills the property cache entry of place `pid`'s head after a miss of
    /// one of the `*_prop_dims_in` fast paths, when the entry is unfilled
    /// or was filled for another class or scope than the object's
    /// (php-src's `CACHE_POLYMORPHIC_PTR` is overwritten on such a miss
    /// too). Returns whether it was filled, so that the caller tries its
    /// fast path once more; an entry that matches but declined the access
    /// stays as it is (`false`).
    #[cold]
    #[inline(never)]
    fn prop_head_refill(&self, pid: PlaceId) -> bool {
        let stale = {
            let f = self.frame();
            let Ok(cache) = f.code.prop_cache.try_borrow() else {
                return false;
            };
            match cache.get(pid as usize) {
                None => true,
                Some(e) => {
                    e.class_addr == 0
                        || e.scope_addr != scope_addr(f)
                        || crate::place::propcache::with_base_obj(f, e.base, |o| {
                            Rc::as_ptr(object::class_of(o)) as usize != e.class_addr
                        })
                        .unwrap_or(false)
                }
            }
        };
        stale && self.prop_cache_fill(pid)
    }

    /// [`unset_prop_dims_in`] after a cache miss ([`Vm::prop_head_refill`]).
    #[cold]
    #[inline(never)]
    pub(in crate::exec) fn unset_prop_dims_refill(&mut self, pid: PlaceId) -> bool {
        if !self.prop_head_refill(pid) {
            return false;
        }
        match self.frames.last_mut() {
            Some(f) => unset_prop_dims_in(f, pid),
            None => false,
        }
    }

    /// The current frame, when it runs user code (a builtin's frame has no
    /// places of its own).
    #[inline(always)]
    fn user_frame_mut(&mut self) -> Option<&mut Frame> {
        self.frames
            .last_mut()
            .filter(|f| f.kind != FrameKind::Builtin)
    }

    /// `Fetch` (`Read` or `Is` mode) of `$this->name[d1]...[dn]` /
    /// `$cv->name[...]` without the place walk ([`read_prop_dims_in`]),
    /// filling the head's cache entry on a miss, or of a place ending in a
    /// property whose cache entries are filled ([`read_chain_in`]). `None`
    /// for the generic fetch.
    pub(crate) fn fetch_prop_dims_fast(&mut self, pid: PlaceId, mode: FetchMode) -> Option<Value> {
        if mode == FetchMode::List {
            return None;
        }
        let f = self.user_frame_mut()?;
        if !is_prop_dims_place(f, pid) {
            // `$this->a->b`, `$list[$k]->name` & co.
            return read_chain_in(f, pid);
        }
        if let Some(v) = read_prop_dims_in(f, pid) {
            return Some(v);
        }
        if !self.prop_head_refill(pid) {
            return None;
        }
        read_prop_dims_in(self.user_frame_mut()?, pid)
    }

    /// `isset()` / `empty()` of `$this->name[d1]...[dn]` / `$cv->name[...]`
    /// without the place walk ([`isset_prop_dims_in`]), filling the
    /// head's cache entry on a miss. `None` for the generic `place_isset`.
    pub(crate) fn isset_prop_dims_fast(&mut self, pid: PlaceId, empty: bool) -> Option<bool> {
        let f = self.user_frame_mut()?;
        if !is_prop_dims_place(f, pid) {
            return None;
        }
        if let Some(r) = isset_prop_dims_in(f, pid, empty) {
            return Some(r);
        }
        if !self.prop_head_refill(pid) {
            return None;
        }
        isset_prop_dims_in(self.user_frame_mut()?, pid, empty)
    }

    /// `$this->name[d1]...[dn] = v` / `$cv->name[...] = v` without the
    /// place walk ([`assign_prop_dims_in`]), filling the head's cache
    /// entry on a miss: the assignment's value on success, `v` back
    /// otherwise for the generic assignment.
    pub(crate) fn assign_prop_dims_fast(&mut self, pid: PlaceId, v: Value) -> Result<Value, Value> {
        let Some(f) = self.user_frame_mut() else {
            return Err(v);
        };
        if !is_prop_dims_place(f, pid) {
            // `$this->a->b = v`, `$list[$k]->name = v`.
            if matches!(
                f.code.places.get(pid as usize).and_then(|p| p.segs.last()),
                Some(crate::opcode::Seg::Prop(_))
            ) {
                let ret = v.clone();
                return match assign_chain_in(f, pid, v) {
                    Some(old) => {
                        super::super::release_rooted(old);
                        Ok(ret)
                    }
                    None => Err(ret),
                };
            }
            // `$a[$k][] = v`, `$a[$k][$j] = [...]` & co. on a variable.
            let ret = v.clone();
            let mut slot = Some(v);
            return match (super::dims::assign_cv_dims_in(f, pid, &mut slot), slot) {
                (Some(old), None) => {
                    super::super::release_rooted(Some(old));
                    Ok(ret)
                }
                (_, Some(v)) => Err(v),
                (None, None) => Ok(ret),
            };
        }
        let ret = v.clone();
        let mut slot = Some(v);
        let mut old = assign_prop_dims_in(f, pid, &mut slot);
        if old.is_none() && slot.is_some() && self.prop_head_refill(pid) {
            if let Some(f) = self.user_frame_mut() {
                old = assign_prop_dims_in(f, pid, &mut slot);
            }
        }
        match (old, slot) {
            (Some(old), None) => {
                super::super::release_rooted(Some(old));
                Ok(ret)
            }
            (_, Some(v)) => Err(v),
            (None, None) => Ok(ret),
        }
    }
}

#[cfg(test)]
mod tests {
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
        vm.run_script(src.as_bytes(), "test.php");
        let bytes = sink.0.borrow().clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn dedicated_property_ops_execute_correctly() {
        let out = run(r#"<?php
class Test {
    public $p = 10;
    public $arr = ['a' => 1, 'b' => 'hello'];
    public $str = 'foo';
}

$o = new Test();
// Read
echo $o->p, "\n";
echo $o->arr['a'], "\n";
echo $o->arr['b'], "\n";

// Write
$o->p = 20;
echo $o->p, "\n";
$o->arr['a'] = 42;
echo $o->arr['a'], "\n";
$o->arr[] = 100;
echo $o->arr[0], "\n";

// Assign-op
$o->p += 5;
echo $o->p, "\n";
$o->p /= 5;
echo $o->p, "\n";
$o->str .= 'bar';
echo $o->str, "\n";
$o->arr['a'] += 8;
echo $o->arr['a'], "\n";

// Inc/dec
$o->p++;
echo $o->p, "\n";
$o->arr['a']--;
echo $o->arr['a'], "\n";

// Isset/empty
echo isset($o->p) ? "1\n" : "0\n";
echo empty($o->p) ? "1\n" : "0\n";
echo isset($o->arr['a']) ? "1\n" : "0\n";
echo empty($o->arr['a']) ? "1\n" : "0\n";
echo isset($o->nonexistent) ? "1\n" : "0\n";
"#);

        assert_eq!(
            out,
            "10\n1\nhello\n20\n42\n100\n25\n5\nfoobar\n50\n6\n49\n1\n0\n1\n0\n0\n"
        );
    }

    /// Cached reads, writes and `isset`/`empty` of declared and dynamic
    /// properties in loops (the hits), with the cases they leave to the
    /// generic path: typed coercion, `__get`/`__isset` after `unset`,
    /// references in slots and dynamic properties, an object held by
    /// reference, a used assignment result, and released old values.
    #[test]
    fn cached_property_hits_match_php() {
        let out = run(r#"<?php
class T { public $p = 1; public int $n = 0; public ?T $t = null; public $u; }
class M { public $a = 1; function __get($k) { return "get:$k"; } function __isset($k) { return $k === 'a'; } }
$o = new stdClass; $o->v = 1;
$t = new T; $m = new M;
$r = [];
for ($i = 0; $i < 3; $i++) {
    $o->v = $o->v + $i;          // dynamic read + write
    $r[] = $o->v;
    $r[] = isset($o->v) ? 'set' : 'unset';
    $r[] = empty($o->w) ? 'empty' : 'full';
    $t->p = $i * 2;              // declared write
    $r[] = $t->p;
    $t->n = "5";                 // typed coercion (generic path)
    $r[] = var_export($t->n, true);
    $t->t = $t;                  // class-typed
    $r[] = $t->t === $t ? 'same' : 'diff';
    $r[] = isset($t->u) ? 'u' : 'no-u';
    $r[] = empty($t->p) ? 'e' : 'ne';
    unset($m->a);
    $r[] = $m->a;                 // __get after unset
    $r[] = isset($m->a) ? 'i' : 'ni';
    $m->a = $i;
    $r[] = $m->a;
}
// a reference in a declared slot and in a dynamic property
$x = 10; $t->p = &$x; $o->v = &$x;
for ($i = 0; $i < 2; $i++) { $t->p = $i; $o->v = $o->v + 1; $r[] = "$x/{$t->p}/{$o->v}"; }
unset($x);
// a reference holding the object
$ref = &$t; for ($i = 0; $i < 2; $i++) { $ref->p = $i + 100; $r[] = $ref->p; $r[] = isset($ref->p) ? 's' : 'n'; }
// result of an assignment used
for ($i = 0; $i < 2; $i++) { $y = $o->z = $i; $r[] = $y; $r[] = $o->z; }
// objects replaced in a slot are released
class D { function __construct(public $id) {} function __destruct() { echo "d{$this->id} "; } }
$h = new stdClass; $h->d = null;
for ($i = 0; $i < 3; $i++) { $h->d = new D($i); }
unset($h); echo "\n";
echo implode(',', $r), "\n";"#);
        assert_eq!(out, "d0 d1 d2 \n1,set,empty,0,5,same,no-u,e,get:a,i,0,2,set,empty,2,5,same,no-u,ne,get:a,i,1,4,set,empty,4,5,same,no-u,ne,get:a,i,2,1/1/1,2/2/2,100,s,101,s,0,0,1,1\n");
    }

    /// Cached `++`/`--` and compound assignments: int overflow into a
    /// float, typed-property overflow and coercion errors, strings, and a
    /// dynamic property.
    #[test]
    fn cached_property_incdec_and_assign_ops_match_php() {
        let out = run(r#"<?php
class N { public $a = PHP_INT_MAX - 1; public int $n = PHP_INT_MAX - 1; public float $f = 1.5; public $s = 'x'; }
$o = new N; $r = [];
for ($i = 0; $i < 2; $i++) { $o->a++; $r[] = var_export($o->a, true); }
for ($i = 0; $i < 2; $i++) { $o->f += 1; $r[] = var_export($o->f, true); $r[] = var_export($o->f--, true); }
for ($i = 0; $i < 2; $i++) { $o->s .= 'y'; $r[] = $o->s; }
try { for ($i = 0; $i < 2; $i++) { $o->n++; $r[] = $o->n; } } catch (Error $e) { $r[] = $e->getMessage(); }
try { for ($i = 0; $i < 2; $i++) { $o->n += 0; $o->n -= -1; $r[] = $o->n; } } catch (TypeError $e) { $r[] = get_class($e) . ': ' . $e->getMessage(); }
$d = new stdClass; $d->c = 0;
for ($i = 0; $i < 3; $i++) { $d->c += 2; $d->c++; $r[] = $d->c; }
echo implode(',', $r), "\n";"#);
        assert_eq!(out, "9223372036854775807,9.223372036854776E+18,2.5,2.5,2.5,2.5,xy,xyy,9223372036854775807,Cannot increment property N::$n of type int past its maximal value,TypeError: Cannot assign float to property N::$n of type int,3,6,9\n");
    }

    /// `$x = isset($o->p)` / `$x = empty($o->p)` with the assignment folded
    /// into the hit: variables holding scalars, a string, an array, and a
    /// reference (left to the assignment).
    #[test]
    fn folded_isset_assignment_matches_php() {
        let out = run(r#"<?php
class T { public $p = 1; public $q; function run() { $r = []; for ($i = 0; $i < 2; $i++) { $a = isset($this->p); $b = empty($this->q); $r[] = var_export($a, true) . '/' . var_export($b, true); } return $r; } }
$t = new T; $r = $t->run();
$s = 'str'; $z = [1];
for ($i = 0; $i < 2; $i++) { $s = isset($t->p); $z = empty($t->p); $r[] = var_export($s, true) . '/' . var_export($z, true); }
$v = 5; $w = &$v;
for ($i = 0; $i < 2; $i++) { $w = isset($t->q); $r[] = var_export($v, true); }
echo implode(',', $r), "\n";"#);
        assert_eq!(
            out,
            "true/true,true/true,true/false,true/false,false,false\n"
        );
    }

    /// The property ops in loops, where they run in the frame-local hot
    /// loop (`prop_op_in`): hits on `$this` and on a variable held by
    /// reference, and every fallback (typed overflow, readonly, coercion,
    /// uninitialized, magic methods, unset slots, dynamic properties,
    /// visibility). Expected output from `/usr/bin/php8.5 -n`.
    #[test]
    fn hot_loop_property_ops_match_php() {
        let out = run(r#"<?php
class A {
    public int $n = 0;
    public $u = 1;
    public ?A $next = null;
    public int $big = PHP_INT_MAX - 2;
    public float $fl = 0.5;
    public readonly int $ro;
    public int $un;
    function __construct() { $this->ro = 7; }
    function loop() {
        $s = 0;
        for ($i = 0; $i < 5; $i++) {
            $this->n += 2;
            $this->u = $this->n * 3;
            $this->u++;
            ++$this->n;
            $this->fl *= 2;
            $s += $this->u;
            $x = isset($this->next);
            $y = empty($this->u);
            try { $this->big++; } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
        }
        var_dump($s, $this->n, $this->u, $this->fl, $x, $y, $this->big);
    }
}
$a = new A; $a->loop();
$b = new A; $r = &$b;
for ($i = 0; $i < 3; $i++) { $r->u = $i; $r->n += $i; $v = $r->u; }
var_dump($v, $b->n);
try { for ($i = 0; $i < 2; $i++) { $b->ro = 1; } } catch (Error $e) { echo $e->getMessage(), "\n"; }
for ($i = 0; $i < 2; $i++) { echo isset($b->un) ? "set" : "unset", "\n"; }
try { for ($i = 0; $i < 2; $i++) { echo $b->un; } } catch (Error $e) { echo $e->getMessage(), "\n"; }
for ($i = 0; $i < 3; $i++) { try { $b->n = $i == 2 ? "x" : "5"; } catch (TypeError $e) { echo $e->getMessage(), "\n"; } var_dump($b->n); }
class M { private $data = []; public $real = 1;
  function __get($k) { echo "get $k\n"; return 3; }
  function __set($k, $v) { echo "set $k\n"; }
  function __isset($k) { echo "isset $k\n"; return true; } }
$m = new M;
for ($i = 0; $i < 2; $i++) { $z = $m->virt; $m->virt = 1; $q = isset($m->virt); $m->real += $z; }
var_dump($m->real);
unset($m->real);
for ($i = 0; $i < 2; $i++) { $z = $m->real; }
$o = new stdClass; $o->d = 1;
for ($i = 0; $i < 3; $i++) { $o->d += $i; $o->d++; $w = $o->d; $o->e = $w; }
var_dump($o);
#[AllowDynamicProperties]
class D { public $p = 1; }
$d = new D;
for ($i = 0; $i < 2; $i++) { $d->dyn = $i; }
var_dump($d);
class P { private $priv = 1; }
$p = new P;
try { for ($i = 0; $i < 2; $i++) { echo $p->priv; } } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#);
        assert_eq!(
            out,
            r#"TypeError: Cannot increment property A::$big of type int past its maximal value
TypeError: Cannot increment property A::$big of type int past its maximal value
TypeError: Cannot increment property A::$big of type int past its maximal value
int(125)
int(15)
int(43)
float(16)
bool(false)
bool(false)
int(9223372036854775807)
int(2)
int(3)
Cannot modify readonly property A::$ro
unset
unset
Typed property A::$un must not be accessed before initialization
int(5)
int(5)
Cannot assign string to property A::$n of type int
int(5)
get virt
set virt
isset virt
get virt
set virt
isset virt
int(7)
get real
get real
object(stdClass)#5 (2) {
  ["d"]=>
  int(7)
  ["e"]=>
  int(7)
}
object(D)#6 (2) {
  ["p"]=>
  int(1)
  ["dyn"]=>
  int(1)
}
Cannot access private property P::$priv
"#
        );
    }

    /// `new` in loops, where a class without a constructor is created in
    /// the frame-local hot loop (`new_in`): defaults evaluated on the first
    /// `new`, handle reuse order, constructors, `Throwable`s, abstract
    /// classes. Expected output from `/usr/bin/php8.5 -n`.
    #[test]
    fn hot_loop_new_matches_php() {
        let out = run(r#"<?php
class K { const C = 5; public $a = [1, 2]; public $b = self::C * 2; public int $t; }
class Ctor { public $v; function __construct() { $this->v = 1; echo "ctor\n"; } }
abstract class Ab {}
class E extends Exception {}
$ids = [];
for ($i = 0; $i < 4; $i++) {
    $k = new K;
    $k->a[] = $i;
    $ids[] = spl_object_id($k);
    $c = new Ctor;
    $e = new E;
}
var_dump($k, $ids, $e->getLine());
$keep = [];
for ($i = 0; $i < 3; $i++) { $keep[] = new K; }
echo count($keep), " ", spl_object_id($keep[2]), "\n";
for ($i = 0; $i < 2; $i++) { try { $x = new Ab; } catch (Error $err) { echo $err->getMessage(), "\n"; } }
for ($i = 0; $i < 2; $i++) { $o = new stdClass; $o->n = $i; }
var_dump($o);
"#);
        assert_eq!(
            out,
            r#"ctor
ctor
ctor
ctor
object(K)#2 (2) {
  ["a"]=>
  array(3) {
    [0]=>
    int(1)
    [1]=>
    int(2)
    [2]=>
    int(3)
  }
  ["b"]=>
  int(10)
  ["t"]=>
  uninitialized(int)
}
array(4) {
  [0]=>
  int(1)
  [1]=>
  int(4)
  [2]=>
  int(3)
  [3]=>
  int(2)
}
int(12)
3 6
Cannot instantiate abstract class Ab
Cannot instantiate abstract class Ab
object(stdClass)#9 (1) {
  ["n"]=>
  int(1)
}
"#
        );
    }

    /// `new C(...)` of a class with a constructor on the dispatch loop's
    /// hot path (`crate::props::new_with_ctor`): handle reuse, inherited
    /// constructor scope, promoted and readonly properties, destructor
    /// order, a private constructor, an argument that throws
    /// (`ZEND_LIVE_NEW`), `WeakMap` keys and GC roots. Expected
    /// output from `/usr/bin/php8.5 -n`, except the root count
    /// (`gc_status()["roots"]`): 8.5 reports 5, buffering the array
    /// `implode()` releases (`GC_TRY_DTOR_NO_REF`), and this collector
    /// buffers objects only (see TODO.md), so it reports 4.
    #[test]
    fn new_with_constructor_matches_php() {
        let out = run(r#"<?php
class P { public $x; public $y;
    function __construct($x, $y) { $this->x = $x; $this->y = $y; } }
class Base { private $secret; protected $n;
    function __construct($n) { $this->secret = "s$n"; $this->n = $n; }
    function secret() { return $this->secret; } }
class Child extends Base {}
class D { function __construct(public int $v, public readonly string $tag = "t") {}
    function __destruct() { echo "~D{$this->v} "; } }
class Priv { private function __construct() { echo "priv\n"; }
    static function make() { return new Priv; } }
class E { function __construct($a) { echo "ctor\n"; } function __destruct() { echo "dtor E\n"; } }
function boom() { throw new Exception("boom"); }
$ids = [];
for ($i = 0; $i < 4; $i++) { $p = new P($i, $i + 1); $ids[] = spl_object_id($p); }
echo implode(",", $ids), " ", $p->x + $p->y, "\n";
$c = new Child(7);
echo $c->secret(), "\n";
var_dump($c);
for ($i = 0; $i < 3; $i++) { $d = new D($i); }
echo "\n";
unset($d);
echo "\n";
$d = new D(9, "z");
var_dump($d);
$d = null;
echo "\n";
try { $x = new Priv; } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
$q = Priv::make();
try { $e1 = new E(boom()); } catch (Exception $e) { echo "caught ", $e->getMessage(), "\n"; }
$e2 = new E(1);
$e2 = null;
$wm = new WeakMap;
for ($i = 0; $i < 3; $i++) { $k = new P($i, 0); $wm[$k] = new D(100 + $i); echo spl_object_id($k), " "; }
echo count($wm), "\n";
unset($k);
echo count($wm), "\n";
$objs = [];
for ($i = 0; $i < 5; $i++) { $objs[] = new P($i, $i); }
$objs = null;
$s = gc_status();
echo $s['roots'], "\n";
$a = new P(1, 2); $b = new P(3, 4);
echo spl_object_id($a), spl_object_id($b), "\n";
$a = null;
$z = new P(5, 6);
echo spl_object_id($z), "\n";
for ($i = 0; $i < 100; $i++) { $t = new P($i, "s$i"); }
echo spl_object_id($t), " ", $t->y, "\n";
"#);
        assert_eq!(
            out,
            r#"1,2,1,2 7
s7
object(Child)#1 (2) {
  ["secret":"Base":private]=>
  string(2) "s7"
  ["n":protected]=>
  int(7)
}
~D0 ~D1 
~D2 
object(D)#3 (2) {
  ["v"]=>
  int(9)
  ["tag"]=>
  string(1) "z"
}
~D9 
Error: Call to private Priv::__construct() from global scope
priv
caught boom
ctor
dtor E
5 ~D100 8 ~D101 7 1
~D102 0
4
109
10
8 s99
"#
        );
    }
}

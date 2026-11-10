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
use crate::opcode::{FetchMode, IncDecKind, Op, Operand, PlaceId, PROP_CACHE_THIS};
use crate::place::propcache::{
    accepts, scope_addr, PC_DYN, PC_ISSET, PC_NO_SET, PC_READ, PC_WRITE,
};
use crate::vm::{CvSlot, Frame, FrameKind, Vm, VmResult};
use php_runtime::{ArrayKey, BinaryOp, Obj, Value, ZStr};
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
fn read_hit(f: &Frame, pid: PlaceId) -> Option<Value> {
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
        _ => false,
    }
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
        let key = match dim {
            Operand::Unused => None,
            other => Some(match self.peek_dim_key(other) {
                Some(k) => k,
                None => return false,
            }),
        };
        let Some(v) = self.peek_plain_value(val) else {
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
    /// visibility). Expected output from php 8.3.
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
}

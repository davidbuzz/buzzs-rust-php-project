// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Per-place runtime cache of property lookups: php-src's
//! `CACHE_ADDR(opline->extended_value)` slot of `ZEND_FETCH_OBJ_R`/`_IS`/
//! `_W`/`_RW`, `ZEND_ASSIGN_OBJ`, `ZEND_ASSIGN_OBJ_OP`,
//! `ZEND_PRE/POST_INC/DEC_OBJ`, `ZEND_ISSET_ISEMPTY_PROP_OBJ` and
//! `ZEND_UNSET_OBJ` (ref: `Zend/zend_vm_def.h`, `Zend/zend_execute.c`
//! `zend_fetch_property_address`, `Zend/zend_object_handlers.c`
//! `zend_get_property_offset`, `zend_std_write_property`,
//! `zend_std_unset_property`).
//!
//! A property place with a literal name on a CV or `$this` (`$o->p`,
//! `$this->p`) remembers what its last lookup found, keyed by the object's
//! class and the calling scope: a declared property's slot, or that the
//! name is a dynamic property (php-src's `ZEND_DYNAMIC_PROPERTY_OFFSET`).
//! Both key parts decide `zend_get_property_offset`'s answer; php-src's
//! runtime cache belongs to one op array in one scope (a closure rebound
//! to another scope gets its own), ours to the op array alone, so the
//! scope is part of the key. A hit reads or writes the value directly.
//! The same entry serves the element of an array property
//! (`$this->a[$k]` read, assigned, `op=`, `++`/`--`, `isset`) when the
//! offset is a literal or a compiled variable holding an int or a string.
//! A place that does not start with such a head keeps an entry for its
//! last `->name` segment instead (`$list[$i]->p`, `f()->p`); a place with
//! both (`$this->o->p`) has a second entry after the head entries
//! (see [`OpArray::prop_cache`]).
//!
//! Every case the cache does not cover takes the generic place walk
//! unchanged: lazy objects, undefined (uninitialized or unset) slots that
//! `__get`/`__set`/`__isset` may serve, missing dynamic properties,
//! values held by reference (for writes), hooked and virtual properties,
//! classes with native property handlers, readonly and
//! asymmetric-visibility properties for writes, typed properties when the
//! value would need a coercion, objects with a materialized property
//! table for `unset`, and elements that do not exist yet or whose
//! operation could raise a diagnostic. An initialized, accessible
//! declared slot or an existing dynamic property is exactly where
//! `zend_std_read_property` & co. return without consulting `__get`,
//! `__set` or `__isset`, so a hit needs no magic-method check.
//!
//! The hit paths are small and fully inlined into each access: the
//! entry's plain data is copied out of the cache ([`Key`]), classes are
//! compared by address, and the object's data is reached once.

// The probes take the access as a closure and must disappear into it.
#![allow(clippy::inline_always)]

use crate::class::Class;
use crate::func::flags;
use crate::object::{self, oflags, ObjectData};
use crate::opcode::{
    FetchMode, IncDecKind, OpArray, Operand, Place, PlaceBase, PlaceId, PropCacheEntry, Seg,
    PROP_CACHE_THIS,
};
use crate::props::PropLookup;
use crate::types::{mask, TypeDecl};
use crate::vm::{Frame, FrameKind, Vm, VmResult};
use php_runtime::{ops, ArrayKey, BinaryOp, Obj, PhpRef, Value, ZArray, ZStr};
use std::rc::{Rc, Weak};

/// Reads (`FETCH_OBJ_R`/`_IS`) may use the value: no native
/// `read_property` handler.
pub(crate) const PC_READ: u8 = 1 << 0;
/// `isset()`/`empty()` may use the value: no native `has_property`.
pub(crate) const PC_ISSET: u8 = 1 << 1;
/// Writes may store the value directly (no readonly, set visibility or
/// native write handler); typed properties only values in `accept`.
pub(crate) const PC_WRITE: u8 = 1 << 2;
/// The name is a dynamic property (`ZEND_DYNAMIC_PROPERTY_OFFSET`): no
/// declared or static property of the class or scope has it.
pub(crate) const PC_DYN: u8 = 1 << 3;
/// `unset()` may clear the property directly: what [`PC_WRITE`] needs,
/// and no native `unset_property` handler.
pub(crate) const PC_UNSET: u8 = 1 << 4;
/// The class has no `__set()`: a write to an undefined declared slot is
/// stored directly (`write_std_property` in `zend_std_write_property`).
pub(crate) const PC_NO_SET: u8 = 1 << 5;
/// An uninitialized declared slot (a typed property without a default, or
/// one `unset()` left) may be initialized directly with a value in
/// `accept`: the class has no `__set()` (so no magic applies to it), no
/// native write handler, no hooks or set visibility; for a readonly
/// property (whatever its set visibility), the lookup's scope is its
/// declaring class (where its one initialization is allowed). Set without [`PC_WRITE`] for a readonly
/// property, whose initialized slot no fast path may write.
pub(crate) const PC_INIT: u8 = 1 << 6;

/// The result of probing a place's cache entry.
enum Probe<T> {
    /// The fast path applied.
    Hit(T),
    /// The entry matches, but this access needs the generic walk.
    Slow,
    /// The entry is unfilled or for another class or scope.
    Miss,
}

/// What follows the cached head of a two-segment place.
enum Nested {
    /// `[$k]` (see [`simple_key`]).
    Dim(ArrayKey),
    /// `[]`.
    Append,
    /// `->name`: the literal index of the name.
    Prop(u32),
}

/// The plain-data part of a cache entry, copied out of the cache so that
/// no borrow of it is held while the property is accessed.
#[derive(Clone, Copy)]
struct Key {
    /// [`PropCacheEntry::class_addr`] (0: unfilled).
    class: usize,
    /// [`PropCacheEntry::scope_addr`].
    scope: usize,
    /// [`PropCacheEntry::ok_class_addr`].
    ok_class: usize,
    base: u32,
    slot: u32,
    lit: u32,
    accept: u32,
    flags: u8,
}

impl Key {
    #[inline(always)]
    fn from_entry(e: &PropCacheEntry) -> Key {
        Key {
            class: e.class_addr,
            scope: e.scope_addr,
            ok_class: e.ok_class_addr,
            base: e.base,
            slot: e.slot,
            lit: e.lit,
            accept: e.accept,
            flags: e.flags,
        }
    }

    /// The key of entry `idx` of `code`'s cache (`None`: the cache is not
    /// that large yet, or is being filled).
    #[inline(always)]
    fn of(code: &OpArray, idx: usize) -> Option<Key> {
        let cache = code.prop_cache.try_borrow().ok()?;
        let key = cache.get(idx).map(Key::from_entry);
        drop(cache);
        key
    }

    /// Whether the entry was filled for the class of the object `data`
    /// belongs to, in the scope whose address is `scope`.
    #[inline(always)]
    fn matches(&self, data: &ObjectData, scope: usize) -> bool {
        Rc::as_ptr(&data.class) as usize == self.class && self.scope == scope
    }
}

/// The address of `frame`'s class scope as entries store it (0: none).
#[inline(always)]
pub(crate) fn scope_addr(frame: &Frame) -> usize {
    frame.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize)
}

/// Whether `place` has the shape the cache covers: a CV or `$this` with
/// one property segment named by a literal.
#[inline]
pub(crate) fn cacheable(place: &Place) -> bool {
    matches!(
        (&place.base, place.segs.as_slice()),
        (
            PlaceBase::Cv(_) | PlaceBase::This,
            [Seg::Prop(Operand::Lit(_))]
        )
    )
}

/// Whether `place` starts with a cacheable property (`$this->a[$i]`,
/// `$cv->a->b`): reads may take its first segment from the cache. The
/// entry only describes that first property.
#[inline]
pub(crate) fn cacheable_head(place: &Place) -> bool {
    matches!(
        (&place.base, place.segs.as_slice()),
        (
            PlaceBase::Cv(_) | PlaceBase::This,
            [Seg::Prop(Operand::Lit(_)), ..]
        )
    )
}

/// The index of the entry of place `pid`'s last `->name` segment: `pid`
/// itself when the place has no cached head, else after the head entries.
#[inline(always)]
fn tail_index(code: &OpArray, pid: PlaceId) -> Option<usize> {
    let place = code.places.get(pid as usize)?;
    Some(if cacheable_head(place) {
        code.places.len() + pid as usize
    } else {
        pid as usize
    })
}

/// Stores `entry` at `idx` of `code`'s cache, sized to at least `len`
/// entries first. Returns whether it was stored.
fn store_entry(code: &OpArray, idx: usize, len: usize, entry: PropCacheEntry) -> bool {
    let Ok(mut cache) = code.prop_cache.try_borrow_mut() else {
        return false;
    };
    if cache.len() < len {
        crate::rtcache::grow_slots(&mut cache, len, PropCacheEntry::default);
    }
    let stored = match cache.get_mut(idx) {
        Some(slot) => {
            let mut entry = entry;
            let mut old = std::mem::replace(slot, PropCacheEntry::default());
            // The replaced entry, for another class, is kept as the
            // alternate (one level: its own alternate goes, its box is
            // reused for it, so a site cycling through three classes does
            // not allocate per refill).
            if old.class_addr != 0 && old.class_addr != entry.class_addr {
                let boxed = match old.alt.take() {
                    Some(mut b) => {
                        *b = old;
                        b
                    }
                    None => Box::new(old),
                };
                entry.alt = Some(boxed);
            }
            *slot = entry;
            true
        }
        None => false,
    };
    drop(cache);
    stored
}

/// Whether `value` may be stored as-is in a property accepting the
/// builtin type bits `accept` (`u32::MAX`: untyped): the side-effect-free
/// subset of `zend_verify_property_type` that needs no coercion, class or
/// callable check.
#[inline]
pub(crate) fn accepts(accept: u32, value: &Value) -> bool {
    if accept == u32::MAX {
        return true;
    }
    let bit = match value {
        Value::Null => mask::NULL,
        Value::Bool(true) => mask::TRUE,
        Value::Bool(false) => mask::FALSE,
        Value::Int(_) => mask::LONG,
        Value::Float(_) => mask::DOUBLE,
        Value::Str(_) => mask::STRING,
        Value::Array(_) => mask::ARRAY | mask::ITERABLE,
        Value::Object(_) => mask::OBJECT,
        Value::Resource(_) => mask::RESOURCE,
        Value::Reference(_) => 0,
    };
    accept & bit != 0
}

/// The lower-case class name of a property type naming exactly one class
/// (`Foo`, `?Foo`, `Foo|int`), other than `self`/`parent`/`static`.
fn single_class_name(ty: &TypeDecl) -> Option<ZStr> {
    if !ty.intersections.is_empty() || ty.classes.len() != 1 {
        return None;
    }
    let lc = ty.classes[0].to_ascii_lowercase();
    if matches!(lc.as_bytes(), b"self" | b"parent" | b"static") {
        return None;
    }
    Some(lc)
}

/// Whether object `value` may be stored as-is in the class-typed property
/// of entry `idx` (whose key is `key`): an instance of the class the type
/// names (`class_matches` in `check_type`). The class of an accepted
/// object is remembered, so the next object of that class passes with one
/// comparison (php-src resolves the type's class once instead of by name).
#[inline(always)]
fn class_accepts(code: &OpArray, idx: usize, key: &Key, value: &Value) -> bool {
    let Value::Object(obj) = value else {
        return false;
    };
    let cls = object::class_of(obj);
    let addr = Rc::as_ptr(cls) as usize;
    addr == key.ok_class || class_accepts_slow(code, idx, cls, addr)
}

/// [`class_accepts_slow`] of an object stored by the frame-local write
/// (`exec::spec::props::assign_hit`): whether the entry's class-typed
/// property takes it, remembering its class when it does, so that the
/// next write of an object of that class passes the entry's one test.
#[cold]
#[inline(never)]
pub(crate) fn learn_accepted_class(code: &OpArray, idx: usize, value: &Value) -> bool {
    let Value::Object(obj) = value else {
        return false;
    };
    let cls = object::class_of(obj);
    class_accepts_slow(code, idx, cls, Rc::as_ptr(cls) as usize)
}

/// [`class_accepts`] by name, remembering an accepted class.
#[cold]
#[inline(never)]
fn class_accepts_slow(code: &OpArray, idx: usize, cls: &Rc<Class>, addr: usize) -> bool {
    let named = code
        .prop_cache
        .try_borrow()
        .ok()
        .and_then(|cache| cache.get(idx).and_then(|e| e.class_lc.clone()));
    let Some(lc) = named else {
        return false;
    };
    if !cls.instance_of_name(lc.as_bytes()) {
        return false;
    }
    if let Ok(mut cache) = code.prop_cache.try_borrow_mut() {
        if let Some(e) = cache.get_mut(idx) {
            e.ok_class_addr = addr;
            e.ok_class = Rc::downgrade(cls);
        }
    }
    true
}

/// Runs `access` on the object the base of a cached place holds (a CV,
/// also through a reference, or `$this`), without cloning the handle;
/// `None` when it holds no object.
#[inline(always)]
pub(crate) fn with_base_obj<R>(
    frame: &Frame,
    base: u32,
    access: impl FnOnce(&Obj) -> R,
) -> Option<R> {
    if base == PROP_CACHE_THIS {
        return frame.this.as_ref().map(access);
    }
    let out = match frame
        .cvs
        .get(base as usize)
        .and_then(crate::vm::CvSlot::get)
    {
        Some(Value::Object(obj)) => Some(access(obj)),
        // A reference being modified elsewhere (`descend`'s "Cannot modify
        // a value while it is being iterated") is left to the walk.
        Some(Value::Reference(rf)) => match rf.try_borrow() {
            Some(inner) => match &*inner {
                Value::Object(obj) => Some(access(obj)),
                _ => None,
            },
            None => None,
        },
        _ => None,
    };
    out
}

/// Runs `access` on the current value of the property `key` describes:
/// an initialized declared slot or an existing dynamic property (the
/// name from `code`'s literals); `Slow` otherwise.
#[inline(always)]
fn with_value<R>(
    key: &Key,
    data: &ObjectData,
    code: &OpArray,
    access: impl FnOnce(&Value) -> Probe<R>,
) -> Probe<R> {
    if key.flags & PC_DYN == 0 {
        let slots = data.slots.borrow();
        let res = match slots.get(key.slot as usize) {
            Some(Some(value)) => access(value),
            _ => Probe::Slow,
        };
        drop(slots);
        return res;
    }
    let Some(Value::Str(name)) = code.literals.get(key.lit as usize) else {
        return Probe::Slow;
    };
    let table = data.dynamic.borrow();
    let res = match table.as_deref().and_then(|t| t.get_zstr(name)) {
        Some(value) => access(value),
        None => Probe::Slow,
    };
    drop(table);
    res
}

/// [`with_value`] with the value mutable.
#[inline(always)]
fn with_value_mut<R>(
    key: &Key,
    data: &ObjectData,
    code: &OpArray,
    access: impl FnOnce(&mut Value) -> Probe<R>,
) -> Probe<R> {
    if key.flags & PC_DYN == 0 {
        let mut slots = data.slots.borrow_mut();
        let res = match slots.get_mut(key.slot as usize) {
            Some(Some(value)) => access(value),
            _ => Probe::Slow,
        };
        drop(slots);
        return res;
    }
    let Some(Value::Str(name)) = code.literals.get(key.lit as usize) else {
        return Probe::Slow;
    };
    let mut table = data.dynamic.borrow_mut();
    let res = match table
        .as_deref_mut()
        .and_then(|t| t.get_mut(&ArrayKey::Str(name.clone())))
    {
        Some(value) => access(value),
        None => Probe::Slow,
    };
    drop(table);
    res
}

/// Runs `access` on the array a value holds, also through a reference.
#[inline(always)]
fn with_array<R>(value: &Value, access: impl FnOnce(&ZArray) -> Probe<R>) -> Probe<R> {
    match value {
        Value::Array(arr) => access(arr),
        Value::Reference(r) => match r.try_borrow() {
            Some(inner) => match &*inner {
                Value::Array(arr) => access(arr),
                _ => Probe::Slow,
            },
            None => Probe::Slow,
        },
        _ => Probe::Slow,
    }
}

/// Runs `access` on the array a declared slot holds: the container
/// `FETCH_OBJ_W` hands a `$this->a[$k]` write walk without promoting
/// anything (so without a typed-property check), and which the walk
/// separates (`SEPARATE_ARRAY`). Also through a reference (left by
/// `&$this->a`, a by-reference argument or `foreach` by reference): the
/// walk descends into it the same way, its type sources only constrain
/// a promotion of null or false (`walk_auto_init_error`).
#[inline(always)]
fn with_array_mut<R>(
    key: &Key,
    data: &ObjectData,
    access: impl FnOnce(&mut ZArray) -> Probe<R>,
) -> Probe<R> {
    if key.flags & PC_DYN != 0 {
        return Probe::Slow;
    }
    let mut slots = data.slots.borrow_mut();
    let res = match slots.get_mut(key.slot as usize) {
        Some(Some(Value::Array(arr))) => access(arr),
        Some(Some(Value::Reference(r))) => match r.try_borrow_mut() {
            Some(mut inner) => match &mut *inner {
                Value::Array(arr) => access(arr),
                _ => Probe::Slow,
            },
            None => Probe::Slow,
        },
        _ => Probe::Slow,
    };
    drop(slots);
    res
}

/// Runs `access` on a property value, or on the value a reference holds
/// when no typed property constrains it (`ZVAL_DEREF`, as
/// `zend_assign_to_variable` and the inc/dec and compound-assignment
/// handlers do); `Slow` for a reference with type sources
/// (`zend_assign_to_typed_ref` & co.) or one borrowed elsewhere (the
/// deferred self-assignment cases of the walk).
#[inline(always)]
fn deref_mut<R>(value: &mut Value, access: impl FnOnce(&mut Value) -> Probe<R>) -> Probe<R> {
    match value {
        Value::Reference(r) => {
            if crate::typed_refs::has_type_sources(r) {
                return Probe::Slow;
            }
            match r.try_borrow_mut() {
                Some(mut inner) => access(&mut inner),
                None => Probe::Slow,
            }
        }
        other => access(other),
    }
}

/// `ZVAL_DEREF` + `ZVAL_COPY`: the value a read hands out.
#[inline(always)]
fn deref_clone(value: &Value) -> Value {
    match value {
        Value::Reference(r) => r.get(),
        other => other.clone(),
    }
}

/// Replaces a current value, also through a reference without type
/// sources (see [`deref_mut`]).
#[inline(always)]
fn store_over(cur: &mut Value, value: &Value) -> Probe<Option<Value>> {
    deref_mut(cur, |target| {
        Probe::Hit(Some(std::mem::replace(target, value.clone())))
    })
}

/// `ZEND_ASSIGN_OBJ` into the property `key` describes: over its current
/// non-reference value, or into an undefined declared slot that php-src
/// writes without `__set()`: one never initialized nor unset (a typed
/// property's `IS_PROP_UNINIT`), or any when the class has no `__set()`
/// (`write_std_property`). The hit is the old value, to be dropped once
/// the borrows are released.
#[inline(always)]
fn store_value(
    key: &Key,
    data: &ObjectData,
    code: &OpArray,
    value: &Value,
) -> Probe<Option<Value>> {
    if key.flags & PC_DYN != 0 {
        return with_value_mut(key, data, code, |cur| store_over(cur, value));
    }
    let idx = key.slot as usize;
    let mut slots = data.slots.borrow_mut();
    let res = match slots.get_mut(idx) {
        Some(Some(cur)) => store_over(cur, value),
        Some(cell @ None) => {
            if key.flags & PC_NO_SET != 0 || (key.accept != u32::MAX && !data.slot_unset(idx)) {
                *cell = Some(value.clone());
                Probe::Hit(None)
            } else {
                Probe::Slow
            }
        }
        None => Probe::Slow,
    };
    drop(slots);
    if matches!(res, Probe::Hit(_)) {
        data.set_slot_unset(idx, false);
    }
    res
}

/// `ZEND_UNSET_OBJ` of the property `key` describes when
/// `zend_std_unset_property` neither calls `__unset()` nor has property
/// table bookkeeping to do: an initialized declared slot of an object
/// without a materialized property table or dynamic properties (the old
/// value is handed back), a typed slot never initialized nor unset
/// (unsetting clears `IS_PROP_UNINIT`), or an existing dynamic property.
#[inline(always)]
fn unset_value(key: &Key, data: &ObjectData, code: &OpArray) -> Probe<Option<Value>> {
    if key.flags & PC_DYN != 0 {
        let Some(Value::Str(name)) = code.literals.get(key.lit as usize) else {
            return Probe::Slow;
        };
        let akey = ArrayKey::Str(name.clone());
        let old = data
            .dynamic
            .borrow_mut()
            .as_deref_mut()
            .and_then(|t| t.remove(&akey));
        return match old {
            Some(old) => {
                object::forget_dynamic_prop_order(data, &akey);
                Probe::Hit(Some(old))
            }
            None => Probe::Slow,
        };
    }
    // `note_dynamic_property_table`, `begin_stale_count` and
    // `note_declared_unset` have nothing to do only without a property
    // table, present or about to be built.
    if data.flags.get() & (oflags::PROPS_TABLE | oflags::STALE_PROPS) != 0
        || data.dynamic.borrow().is_some()
    {
        return Probe::Slow;
    }
    let idx = key.slot as usize;
    let mut slots = data.slots.borrow_mut();
    let res = match slots.get_mut(idx) {
        Some(cell @ Some(_)) => Probe::Hit(cell.take()),
        Some(None) if key.accept != u32::MAX && !data.slot_unset(idx) => Probe::Hit(None),
        _ => Probe::Slow,
    };
    drop(slots);
    if matches!(res, Probe::Hit(_)) {
        data.set_slot_unset(idx, true);
    }
    res
}

/// `++`/`--` of an int (without overflow) or float in place
/// (`zend_pre_incdec_property_zval`'s `IS_LONG` fast path, and
/// `increment_function` on a double): the result, `Slow` otherwise. The
/// value keeps its type, so a typed property stays valid.
#[inline]
fn incdec_in_place(value: &mut Value, kind: IncDecKind) -> Probe<Value> {
    match value {
        Value::Int(cur) => {
            let old = *cur;
            let new = if kind.is_inc() {
                old.checked_add(1)
            } else {
                old.checked_sub(1)
            };
            match new {
                Some(new) => {
                    *cur = new;
                    Probe::Hit(Value::Int(if kind.is_post() { old } else { new }))
                }
                None => Probe::Slow,
            }
        }
        Value::Float(cur) => {
            let old = *cur;
            let new = if kind.is_inc() { old + 1.0 } else { old - 1.0 };
            *cur = new;
            Probe::Hit(Value::Float(if kind.is_post() { old } else { new }))
        }
        _ => Probe::Slow,
    }
}

/// The compound assignments that can neither raise a diagnostic nor
/// throw on numbers (see [`numeric_op`]).
#[inline(always)]
fn pure_op(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::BitAnd
            | BinaryOp::BitOr
            | BinaryOp::BitXor
    )
}

/// `add_function` & co. on numbers without any diagnostic: `+`, `-`, `*`
/// of ints and floats (an int overflow gives the float result) and the
/// bitwise operators on two ints; `None` for any other combination (a
/// float operand of a bitwise operator may deprecate).
#[inline(always)]
#[allow(clippy::cast_precision_loss)] // the int overflow result is the float operation
fn numeric_op(op: BinaryOp, a: &Value, b: &Value) -> Option<Value> {
    use Value::{Float, Int};
    Some(match (op, a, b) {
        (BinaryOp::Add, Int(x), Int(y)) => match x.checked_add(*y) {
            Some(r) => Int(r),
            None => Float(*x as f64 + *y as f64),
        },
        (BinaryOp::Sub, Int(x), Int(y)) => match x.checked_sub(*y) {
            Some(r) => Int(r),
            None => Float(*x as f64 - *y as f64),
        },
        (BinaryOp::Mul, Int(x), Int(y)) => match x.checked_mul(*y) {
            Some(r) => Int(r),
            None => Float(*x as f64 * *y as f64),
        },
        (BinaryOp::BitAnd, Int(x), Int(y)) => Int(x & y),
        (BinaryOp::BitOr, Int(x), Int(y)) => Int(x | y),
        (BinaryOp::BitXor, Int(x), Int(y)) => Int(x ^ y),
        (BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul, Int(_) | Float(_), Int(_) | Float(_)) => {
            let (x, y) = (as_f64(a), as_f64(b));
            Float(match op {
                BinaryOp::Add => x + y,
                BinaryOp::Sub => x - y,
                _ => x * y,
            })
        }
        _ => return None,
    })
}

#[inline(always)]
#[allow(clippy::cast_precision_loss)] // `zval_get_double` of an int
fn as_f64(v: &Value) -> f64 {
    match v {
        Value::Int(i) => *i as f64,
        Value::Float(f) => *f,
        _ => 0.0,
    }
}

/// `.=` of a string or an int onto a string held directly
/// (`concat_function` with `result == op1`: extended in place when
/// nothing else holds it, `zend_string_extend`).
#[inline(always)]
fn append_in_place(value: &mut Value, rhs: &Value) -> Probe<Value> {
    match (value, rhs) {
        (Value::Str(s), Value::Str(add)) => {
            s.append(add.as_bytes());
            Probe::Hit(Value::Str(s.clone()))
        }
        // The digits go straight into the string (`ZStr::append_long`).
        (Value::Str(s), Value::Int(i)) => {
            s.append_long(*i);
            Probe::Hit(Value::Str(s.clone()))
        }
        _ => Probe::Slow,
    }
}

/// Whether the compound assignment `op` with right-hand side `rhs` is one
/// the nested hits operate in place ([`operate_in_place`]): `.=` of a
/// string or an int, or a [`numeric_op`] operator with a number.
#[inline(always)]
fn in_place_op(op: BinaryOp, rhs: &Value) -> bool {
    if op == BinaryOp::Concat {
        matches!(rhs, Value::Str(_) | Value::Int(_))
    } else {
        pure_op(op) && matches!(rhs, Value::Int(_) | Value::Float(_))
    }
}

/// `zend_binary_op(var_ptr, var_ptr, value)` of an [`in_place_op`] on
/// `cur`, a value whose slot accepts the builtin type bits `accept`
/// (`u32::MAX`: untyped): `.=` appends to a string ([`append_in_place`];
/// a typed slot must admit strings), a numeric operator replaces a number
/// by a result the type admits. The result, or `Slow`, nothing changed.
#[inline(always)]
fn operate_in_place(cur: &mut Value, accept: u32, op: BinaryOp, rhs: &Value) -> Probe<Value> {
    if op == BinaryOp::Concat {
        if accept != u32::MAX && accept & mask::STRING == 0 {
            return Probe::Slow;
        }
        return append_in_place(cur, rhs);
    }
    match numeric_op(op, cur, rhs) {
        Some(new) if accepts(accept, &new) => {
            *cur = new.clone();
            Probe::Hit(new)
        }
        _ => Probe::Slow,
    }
}

/// `isset()` (or, with `empty`, `empty()`) of a property or element
/// value, dereferenced; `Err` with the value when only `to_bool` can tell
/// (`empty()` of an object, a resource or NAN: a cast handler or a
/// warning may be involved).
#[inline(always)]
fn isset_value(value: &Value, empty: bool) -> Result<bool, Value> {
    match value {
        Value::Reference(r) => isset_plain(&r.get(), empty),
        other => isset_plain(other, empty),
    }
}

/// [`isset_value`] of a value that is not a reference.
#[inline(always)]
#[allow(clippy::float_cmp)] // `zend_is_true`: exactly 0.0 (or -0.0) is false
fn isset_plain(value: &Value, empty: bool) -> Result<bool, Value> {
    if !empty {
        return Ok(!matches!(value, Value::Null));
    }
    Ok(match value {
        Value::Null => true,
        Value::Bool(b) => !*b,
        Value::Int(i) => *i == 0,
        Value::Float(f) if !f.is_nan() => *f == 0.0,
        Value::Str(s) => s.is_empty() || s.as_bytes() == b"0",
        Value::Array(a) => a.is_empty(),
        other => return Err(other.clone()),
    })
}

/// The array key of an int or string offset (numeric strings become int
/// keys, `ZEND_HANDLE_NUMERIC_STR`), also through a reference; `None` for
/// any other type, whose conversion may raise a diagnostic.
fn simple_key(v: &Value) -> Option<ArrayKey> {
    match v {
        Value::Int(i) => Some(ArrayKey::Int(*i)),
        Value::Str(s) => Some(ArrayKey::from_zstr(s)),
        Value::Reference(r) => r.try_borrow().and_then(|inner| match &*inner {
            Value::Int(i) => Some(ArrayKey::Int(*i)),
            Value::Str(s) => Some(ArrayKey::from_zstr(s)),
            _ => None,
        }),
        _ => None,
    }
}

impl Vm {
    /// Runs `access` on the matching cache entry of place `pid` (its
    /// [`Key`]), the object its base holds and that object's data. `need`
    /// are the `PC_*` bits the access requires; lazy objects always take
    /// the generic walk (`zend_lazy_object_init` runs there).
    #[inline(always)]
    fn probe<T>(
        &self,
        pid: PlaceId,
        need: u8,
        access: impl FnOnce(&Key, &Obj, &ObjectData, &OpArray) -> Probe<T>,
    ) -> Probe<T> {
        let frame = self.frame();
        if frame.kind == FrameKind::Builtin {
            return Probe::Slow;
        }
        let code: &OpArray = &frame.code;
        let Some(key) = Key::of(code, pid as usize) else {
            return Probe::Miss;
        };
        if key.class == 0 {
            return Probe::Miss;
        }
        let scope = scope_addr(frame);
        with_base_obj(frame, key.base, |obj| {
            let data = object::data(obj);
            if !key.matches(data, scope) {
                return Probe::Miss;
            }
            if key.flags & need != need || data.lazy.borrow().is_some() {
                return Probe::Slow;
            }
            access(&key, obj, data, code)
        })
        .unwrap_or(Probe::Slow)
    }

    /// [`Vm::probe`], filling the entry and probing again on a miss
    /// (`zend_get_property_offset` storing `CACHE_POLYMORPHIC_PTR`).
    #[inline(always)]
    fn probe_fill<T>(
        &self,
        pid: PlaceId,
        need: u8,
        access: impl Fn(&Key, &Obj, &ObjectData, &OpArray) -> Probe<T>,
    ) -> Option<T> {
        match self.probe(pid, need, &access) {
            Probe::Hit(res) => Some(res),
            Probe::Slow => None,
            Probe::Miss => self.probe_refill(pid, need, &access),
        }
    }

    /// The miss half of [`Vm::probe_fill`], out of line so that each
    /// access inlines one probe.
    #[cold]
    #[inline(never)]
    fn probe_refill<T>(
        &self,
        pid: PlaceId,
        need: u8,
        access: &dyn Fn(&Key, &Obj, &ObjectData, &OpArray) -> Probe<T>,
    ) -> Option<T> {
        if !self.prop_cache_fill(pid) {
            return None;
        }
        match self.probe(pid, need, access) {
            Probe::Hit(res) => Some(res),
            Probe::Slow | Probe::Miss => None,
        }
    }

    /// The cache entry of property `name` for objects of class `cls`
    /// accessed from `scope` (`zend_get_property_offset`): the declared
    /// slot or dynamic property, and which accesses may use it directly.
    fn cache_entry_for(
        &self,
        cls: &Rc<Class>,
        name: &ZStr,
        scope: Option<&Rc<Class>>,
        base: u32,
        lit: u32,
    ) -> PropCacheEntry {
        let mut entry = PropCacheEntry {
            class_addr: Rc::as_ptr(cls) as usize,
            scope_addr: scope.map_or(0, |s| Rc::as_ptr(s) as usize),
            class: Rc::downgrade(cls),
            scope: scope.map(Rc::downgrade),
            base,
            slot: 0,
            lit,
            accept: 0,
            class_lc: None,
            ok_class_addr: 0,
            ok_class: Weak::new(),
            flags: 0,
            alt: None,
        };
        let hooks = cls.hooks.as_deref();
        let read_ok = hooks.is_none_or(|hk| hk.read_property.is_none());
        let isset_ok = hooks.is_none_or(|hk| hk.has_property.is_none());
        let write_ok = hooks.is_none_or(|hk| hk.write_property.is_none());
        let unset_ok = write_ok && hooks.is_none_or(|hk| hk.unset_property.is_none());
        match self.lookup_prop(cls, name, scope) {
            PropLookup::Found(info) => {
                if info.hooks.is_none() && !info.is_virtual() {
                    entry.slot = info.slot;
                    if read_ok {
                        entry.flags |= PC_READ;
                    }
                    if isset_ok {
                        entry.flags |= PC_ISSET;
                    }
                    if write_ok && info.flags & (flags::READONLY | flags::SET_MASK) == 0 {
                        entry.flags |= PC_WRITE;
                        if unset_ok {
                            entry.flags |= PC_UNSET;
                        }
                        if cls.magic.set.is_none() {
                            entry.flags |= PC_NO_SET | PC_INIT;
                        }
                        entry.accept = info.ty.as_ref().map_or(u32::MAX, |ty| ty.mask);
                        entry.class_lc = info.ty.as_ref().and_then(single_class_name);
                    } else if write_ok
                        // (Any set visibility, also the `protected(set)` of
                        // a `public readonly` one, admits the declaring
                        // class, the only scope taken here.)
                        && info.flags & flags::READONLY != 0
                        && cls.magic.set.is_none()
                        && scope.is_some_and(|sc| {
                            sc.lc_name
                                .as_bytes()
                                .eq_ignore_ascii_case(info.class.as_bytes())
                        })
                    {
                        // A readonly property's initialization from its
                        // declaring class (a constructor's `$this->p = $p`).
                        entry.flags |= PC_INIT | PC_NO_SET;
                        entry.accept = info.ty.as_ref().map_or(u32::MAX, |ty| ty.mask);
                        entry.class_lc = info.ty.as_ref().and_then(single_class_name);
                    }
                }
            }
            PropLookup::NotFound => {
                // `zend_get_property_offset` without a "wrong offset": no
                // mangled name and no static property of that name, so an
                // existing dynamic property is read and written as such
                // (`undeclared_prop_check` would return `false` silently).
                let plain = name.as_bytes().first() != Some(&0)
                    && cls.find_static(name.as_bytes()).is_none()
                    && scope.is_none_or(|sc| sc.find_static(name.as_bytes()).is_none());
                if plain {
                    entry.flags |= PC_DYN;
                    entry.accept = u32::MAX;
                    if read_ok {
                        entry.flags |= PC_READ;
                    }
                    if isset_ok {
                        entry.flags |= PC_ISSET;
                    }
                    if write_ok {
                        entry.flags |= PC_WRITE;
                        if unset_ok {
                            entry.flags |= PC_UNSET;
                        }
                    }
                }
            }
            PropLookup::Inaccessible(_) => {}
        }
        entry
    }

    /// Fills the cache entry of property place `pid` for the object its
    /// base holds now (`zend_get_property_offset` storing
    /// `CACHE_POLYMORPHIC_PTR`). Returns whether an entry was stored.
    #[cold]
    #[inline(never)]
    pub(crate) fn prop_cache_fill(&self, pid: PlaceId) -> bool {
        let frame = self.frame();
        if frame.kind == FrameKind::Builtin {
            return false;
        }
        let code: &OpArray = &frame.code;
        let Some(place) = code.places.get(pid as usize) else {
            return false;
        };
        let (base, lit) = match (&place.base, place.segs.as_slice()) {
            (PlaceBase::Cv(cv), [Seg::Prop(Operand::Lit(lit)), ..]) => (*cv, *lit),
            (PlaceBase::This, [Seg::Prop(Operand::Lit(lit)), ..]) => (PROP_CACHE_THIS, *lit),
            _ => return false,
        };
        let Some(Value::Str(name)) = code.literals.get(lit as usize) else {
            return false;
        };
        let Some(cls) = with_base_obj(frame, base, |obj| object::class_of(obj).clone()) else {
            return false;
        };
        // The alternate entry is for this class and scope: the two swap
        // (the alternate's class is alive: its address is the object's).
        let scope = scope_addr(frame);
        if let Ok(mut cache) = code.prop_cache.try_borrow_mut() {
            if let Some(e) = cache.get_mut(pid as usize) {
                if let Some(alt) = e.alt.take() {
                    if alt.class_addr == Rc::as_ptr(&cls) as usize && alt.scope_addr == scope {
                        let mut prev = std::mem::replace(e, *alt);
                        prev.alt = None;
                        e.alt = Some(Box::new(prev));
                        return true;
                    }
                    e.alt = Some(alt);
                }
            }
        }
        let entry = self.cache_entry_for(&cls, name, frame.scope.as_ref(), base, lit);
        store_entry(code, pid as usize, code.places.len(), entry)
    }

    /// The literal property name of cacheable place `pid`.
    fn prop_cache_name(&self, pid: PlaceId) -> ZStr {
        let code = &self.frame().code;
        match code.places[pid as usize].segs.first() {
            Some(Seg::Prop(Operand::Lit(lit))) => match code.literals.get(*lit as usize) {
                Some(Value::Str(name)) => name.clone(),
                _ => ZStr::default(),
            },
            _ => ZStr::default(),
        }
    }

    /// The second segment of a two-segment place `pid` with a cached head,
    /// when the cache can perform it: an offset whose reading can neither
    /// warn nor convert noisily (a literal or a compiled variable holding an
    /// int or a string, also through a reference: see [`simple_key`]), `[]`,
    /// or a literal property name.
    #[inline]
    fn nested(&self, pid: PlaceId) -> Option<Nested> {
        let frame = self.frame();
        let code: &OpArray = &frame.code;
        match code.places.get(pid as usize)?.segs.as_slice() {
            [Seg::Prop(Operand::Lit(_)), Seg::Dim(op)] => match *op {
                Operand::Lit(lit) => code.literals.get(lit as usize).and_then(simple_key),
                Operand::Cv(cv) => frame.peek_cv(cv, |v| v.and_then(simple_key)),
                _ => None,
            }
            .map(Nested::Dim),
            [Seg::Prop(Operand::Lit(_)), Seg::Append] => Some(Nested::Append),
            [Seg::Prop(Operand::Lit(_)), Seg::Prop(Operand::Lit(lit))] => Some(Nested::Prop(*lit)),
            _ => None,
        }
    }

    /// The offset key of a `$base->name[$k]` place `pid` (see
    /// [`Vm::nested`]).
    #[inline]
    fn simple_dim_key(&self, pid: PlaceId) -> Option<ArrayKey> {
        match self.nested(pid)? {
            Nested::Dim(key) => Some(key),
            Nested::Append | Nested::Prop(_) => None,
        }
    }

    /// The object the cached head of place `pid` holds directly: what
    /// `ZEND_FETCH_OBJ_W`/`_RW`/`_IS` hand the next segment's opcode when
    /// no hook, native handler or lazy object is involved (a readonly or
    /// set-restricted property holding an object hands it out too:
    /// `zend_std_read_property` "might not actually modify object").
    #[inline]
    fn head_object(&self, pid: PlaceId) -> Option<Obj> {
        self.probe_fill(pid, PC_READ, |key, _, data, code| {
            with_value(key, data, code, |value| match value {
                Value::Object(obj) => Probe::Hit(obj.clone()),
                _ => Probe::Slow,
            })
        })
    }

    /// `ZEND_FETCH_OBJ_R`/`_IS` of the first segment of place `pid` (see
    /// [`cacheable_head`]): the (dereferenced) value of an initialized
    /// declared slot or an existing dynamic property, or `None` for the
    /// generic walk.
    #[inline(always)]
    pub(crate) fn prop_cache_read(&self, pid: PlaceId) -> Option<Value> {
        let frame = self.frame();
        if frame.kind != FrameKind::Builtin {
            if let Ok(cache) = frame.code.prop_cache.try_borrow() {
                if let Some(entry) = cache.get(pid as usize) {
                    if entry.class_addr != 0 && (entry.flags & (PC_READ | PC_DYN) == PC_READ) {
                        let scope = scope_addr(frame);
                        if entry.scope_addr == scope {
                            let fast_val = with_base_obj(frame, entry.base, |obj| {
                                let data = object::data(obj);
                                if Rc::as_ptr(&data.class) as usize == entry.class_addr
                                    && data.lazy.borrow().is_none()
                                {
                                    let slot = entry.slot as usize;
                                    if let Ok(slots) = data.slots.try_borrow() {
                                        if let Some(Some(value)) = slots.get(slot) {
                                            return match value {
                                                Value::Reference(r) => {
                                                    r.try_borrow().map(|inner| inner.clone())
                                                }
                                                other => Some(other.clone()),
                                            };
                                        }
                                    }
                                }
                                None
                            });
                            if let Some(Some(v)) = fast_val {
                                drop(cache);
                                return Some(v);
                            }
                        }
                    }
                }
            }
        }
        self.probe_fill(pid, PC_READ, |key, _, data, code| {
            with_value(key, data, code, |value| Probe::Hit(deref_clone(value)))
        })
    }

    /// `$base->name[$k]` read of place `pid` (`ZEND_FETCH_OBJ_R`/`_IS`,
    /// then `ZEND_FETCH_DIM_R`/`_IS`) when the cached property holds an
    /// array that has the element (no diagnostic in either mode): the
    /// element, dereferenced; `None` for the generic walk.
    #[inline]
    pub(crate) fn prop_cache_read_dim(&mut self, pid: PlaceId) -> Option<Value> {
        let dim = self.simple_dim_key(pid)?;
        self.prop_cache_read_elem(pid, &dim)
    }

    /// [`Vm::prop_cache_read_dim`] of the element at `dim` (the key of the
    /// place's offset, see [`simple_key`]).
    #[inline]
    pub(crate) fn prop_cache_read_elem(&self, pid: PlaceId, dim: &ArrayKey) -> Option<Value> {
        self.probe_fill(pid, PC_READ, |key, _, data, code| {
            with_value(key, data, code, |value| {
                with_array(value, |arr| match arr.get(dim) {
                    Some(elem) => Probe::Hit(deref_clone(elem)),
                    None => Probe::Slow,
                })
            })
        })
    }

    /// [`Vm::prop_cache_read_elem`] in `Is` mode (`$o->p[k] ?? $d`,
    /// `ZEND_FETCH_OBJ_IS` + `ZEND_FETCH_DIM_IS`): an array property that
    /// has no element at `dim` gives `null`, without a notice. Anything
    /// else the plain read would not take stays on the slow path.
    pub(crate) fn prop_cache_read_elem_is(&self, pid: PlaceId, dim: &ArrayKey) -> Option<Value> {
        self.probe_fill(pid, PC_READ, |key, _, data, code| {
            with_value(key, data, code, |value| {
                with_array(value, |arr| match arr.get(dim) {
                    Some(elem) => Probe::Hit(deref_clone(elem)),
                    None => Probe::Hit(Value::Null),
                })
            })
        })
    }

    /// Runs `access` on the entry of place `pid`'s last segment, named by
    /// literal `lit`, for `obj` (an object the place's walk reached): the
    /// cache slot php-src gives every `FETCH_OBJ_*`/`ASSIGN_OBJ` & co.
    /// opline, filled on a miss. `need` are the `PC_*` bits the access
    /// requires; `access` also gets the entry's index.
    #[inline(always)]
    fn tail_probe<T>(
        &self,
        pid: PlaceId,
        obj: &Obj,
        lit: u32,
        need: u8,
        access: impl FnOnce(&Key, &ObjectData, &OpArray, usize) -> Probe<T>,
    ) -> Option<T> {
        let frame = self.frame();
        if frame.kind == FrameKind::Builtin {
            return None;
        }
        let code: &OpArray = &frame.code;
        let idx = tail_index(code, pid)?;
        let data = object::data(obj);
        let key = match Key::of(code, idx) {
            Some(key) if key.lit == lit && key.matches(data, scope_addr(frame)) => key,
            _ => self.prop_cache_fill_tail(pid, idx, obj, lit)?,
        };
        if key.flags & need != need || data.lazy.borrow().is_some() {
            return None;
        }
        match access(&key, data, code, idx) {
            Probe::Hit(res) => Some(res),
            Probe::Slow | Probe::Miss => None,
        }
    }

    /// Fills entry `idx`, the one of the last segment of place `pid`, for
    /// `obj` when `lit` names that segment; its key, or `None`.
    #[cold]
    #[inline(never)]
    fn prop_cache_fill_tail(&self, pid: PlaceId, idx: usize, obj: &Obj, lit: u32) -> Option<Key> {
        let frame = self.frame();
        let code: &OpArray = &frame.code;
        let place = code.places.get(pid as usize)?;
        // A one-segment head place has its own entry: its generic read
        // only runs when that entry declined.
        if place.segs.len() == 1 && cacheable_head(place) {
            return None;
        }
        if !matches!(place.segs.last(), Some(Seg::Prop(Operand::Lit(l))) if *l == lit) {
            return None;
        }
        let Some(Value::Str(name)) = code.literals.get(lit as usize) else {
            return None;
        };
        let entry = self.cache_entry_for(object::class_of(obj), name, frame.scope.as_ref(), 0, lit);
        let key = Key::from_entry(&entry);
        let places = code.places.len();
        let len = if idx < places { places } else { 2 * places };
        store_entry(code, idx, len, entry).then_some(key)
    }

    /// `ZEND_FETCH_OBJ_R`/`_IS` of a later `->name` segment of place `pid`
    /// on `obj` (`$list[$i]->p`, `f()->p`, `$this->o->p`) with the last
    /// segment's entry (see [`OpArray::prop_cache`]); `name` is the
    /// segment's name operand. `None` leaves the read to `read_property`.
    #[inline]
    pub(crate) fn prop_cache_read_tail(
        &mut self,
        pid: PlaceId,
        obj: &Obj,
        name: Operand,
        mode: FetchMode,
    ) -> Option<Value> {
        let Operand::Lit(lit) = name else {
            return None;
        };
        if mode == FetchMode::List {
            return None;
        }
        self.tail_probe(pid, obj, lit, PC_READ, |key, data, code, _| {
            with_value(key, data, code, |value| Probe::Hit(deref_clone(value)))
        })
    }

    /// `FETCH_OBJ_W` of the first segment of place `pid` for a write walk
    /// that goes on inside the property (`$this->a[$k][$j] = v`): the
    /// object and the declared slot `property_container` would hand the
    /// walk (`PropContainer::Declared`), when the cache covers it: an
    /// initialized slot without reference, writable in place (no readonly
    /// or set visibility, no native handler), and for a typed property
    /// holding an array or object (nothing to auto-vivify, so no type
    /// check: `typed_prop_dim_write_check`).
    pub(crate) fn prop_cache_container(&mut self, pid: PlaceId) -> Option<(Obj, usize)> {
        self.probe_fill(pid, PC_READ | PC_WRITE, |key, obj, data, code| {
            if key.flags & PC_DYN != 0 {
                return Probe::Slow;
            }
            with_value(key, data, code, |value| {
                let ok = match value {
                    Value::Reference(_) => false,
                    Value::Array(_) | Value::Object(_) => true,
                    _ => key.accept == u32::MAX,
                };
                if ok {
                    Probe::Hit((obj.clone(), key.slot as usize))
                } else {
                    Probe::Slow
                }
            })
        })
    }

    /// `ZEND_FETCH_OBJ_W` + `ZEND_MAKE_REF` of cacheable place `pid`
    /// (`$r = &$this->p`, by-reference arguments, `foreach` by reference):
    /// the reference an initialized, in-place-writable declared slot now
    /// holds (`property_ref` through `PropContainer::Declared`), or `None`
    /// for the generic walk. Only a typed property becomes a type source.
    pub(crate) fn prop_cache_make_ref(&mut self, pid: PlaceId) -> Option<PhpRef> {
        let hit = self.probe_fill(pid, PC_READ | PC_WRITE, |key, obj, data, code| {
            if key.flags & PC_DYN != 0 {
                return Probe::Slow;
            }
            let typed = key.accept != u32::MAX;
            with_value_mut(key, data, code, |value| {
                Probe::Hit((
                    value.make_ref(),
                    typed.then(|| (obj.clone(), key.slot as usize)),
                ))
            })
        });
        let (rf, typed) = hit?;
        if let Some((obj, slot)) = typed {
            self.add_slot_type_source(&obj, slot, &rf);
        }
        Some(rf)
    }

    /// `ZEND_ASSIGN_OBJ` of cacheable place `pid`: stores `v` over an
    /// initialized declared slot or an existing dynamic property, or into
    /// an undefined declared slot php-src writes without `__set()` (see
    /// [`store_value`]), and returns the assigned value, or hands `v` back
    /// (`Err`) for the generic walk.
    #[inline(always)]
    pub(crate) fn prop_cache_assign(&self, pid: PlaceId, v: Value) -> Result<Value, Value> {
        if matches!(v, Value::Reference(_)) {
            return Err(v);
        }
        let frame = self.frame();
        if frame.kind != FrameKind::Builtin {
            if let Ok(cache) = frame.code.prop_cache.try_borrow() {
                if let Some(entry) = cache.get(pid as usize) {
                    if entry.class_addr != 0 && (entry.flags & (PC_WRITE | PC_DYN) == PC_WRITE) {
                        let scope = scope_addr(frame);
                        if entry.scope_addr == scope {
                            let type_ok = entry.accept == u32::MAX
                                || accepts(entry.accept, &v)
                                || (entry.ok_class_addr != 0
                                    && matches!(&v, Value::Object(o) if Rc::as_ptr(object::class_of(o)) as usize == entry.ok_class_addr));

                            if type_ok {
                                let slot = entry.slot as usize;
                                let no_set = entry.flags & PC_NO_SET != 0;
                                let accept = entry.accept;
                                let class_addr = entry.class_addr;
                                let base = entry.base;
                                drop(cache);

                                let stored = with_base_obj(frame, base, |obj| {
                                    let data = object::data(obj);
                                    if Rc::as_ptr(&data.class) as usize == class_addr
                                        && data.lazy.borrow().is_none()
                                    {
                                        if let Ok(mut slots) = data.slots.try_borrow_mut() {
                                            if let Some(cell) = slots.get_mut(slot) {
                                                let mut old = None;
                                                let ok = match cell {
                                                    Some(cur) => {
                                                        if !matches!(cur, Value::Reference(_)) {
                                                            old = Some(std::mem::replace(
                                                                cur,
                                                                v.clone(),
                                                            ));
                                                            true
                                                        } else {
                                                            false
                                                        }
                                                    }
                                                    None => {
                                                        if no_set
                                                            || (accept != u32::MAX
                                                                && !data.slot_unset(slot))
                                                        {
                                                            *cell = Some(v.clone());
                                                            data.set_slot_unset(slot, false);
                                                            true
                                                        } else {
                                                            false
                                                        }
                                                    }
                                                };
                                                drop(slots);
                                                drop(old);
                                                return ok;
                                            }
                                        }
                                    }
                                    false
                                });

                                if stored == Some(true) {
                                    return Ok(v);
                                }
                            }
                        }
                    }
                }
            }
        }
        let old = self.probe_fill(pid, PC_WRITE, |key, _, data, code| {
            if !accepts(key.accept, &v) && !class_accepts(code, pid as usize, key, &v) {
                return Probe::Slow;
            }
            store_value(key, data, code, &v)
        });
        match old {
            Some(old) => {
                drop(old);
                Ok(v)
            }
            None => Err(v),
        }
    }

    /// The assignment of a two-segment place `pid` with a cached head
    /// (`ZEND_FETCH_OBJ_W`, then `ZEND_ASSIGN_DIM` or `ZEND_ASSIGN_OBJ`):
    /// `$base->a[$k] = v` and `$base->a[] = v` on a property holding an
    /// array (the separated array's element is replaced or added,
    /// `zend_hash_next_index_insert` for `[]`), `$base->a->b = v` on the
    /// object the head holds (see [`Vm::prop_cache_assign`]). Returns `v`,
    /// or hands it back (`Err`) for the generic walk: an element holding a
    /// reference, a property not holding an array or object, an offset of
    /// another type, a full array's "Cannot add element".
    pub(crate) fn prop_cache_nested_assign(
        &mut self,
        pid: PlaceId,
        v: Value,
    ) -> Result<Value, Value> {
        if matches!(v, Value::Reference(_)) {
            return Err(v);
        }
        let old = match self.nested(pid) {
            Some(Nested::Prop(lit)) => {
                let Some(obj) = self.head_object(pid) else {
                    return Err(v);
                };
                self.tail_probe(pid, &obj, lit, PC_WRITE, |key, data, code, idx| {
                    if !accepts(key.accept, &v) && !class_accepts(code, idx, key, &v) {
                        return Probe::Slow;
                    }
                    store_value(key, data, code, &v)
                })
            }
            Some(Nested::Dim(dim)) => return self.prop_cache_assign_elem(pid, Some(&dim), v),
            Some(Nested::Append) => return self.prop_cache_assign_elem(pid, None, v),
            None => None,
        };
        match old {
            Some(old) => {
                drop(old);
                Ok(v)
            }
            None => Err(v),
        }
    }

    /// `$base->a[$k] = v` (`dim` the key of the place's offset, see
    /// [`simple_key`]) or `$base->a[] = v` (`dim` `None`) of a two-segment
    /// place `pid` with a cached head, on a property holding an array: see
    /// [`Vm::prop_cache_nested_assign`]. Returns `v`, or hands it back
    /// (`Err`) for the generic walk.
    pub(crate) fn prop_cache_assign_elem(
        &self,
        pid: PlaceId,
        dim: Option<&ArrayKey>,
        v: Value,
    ) -> Result<Value, Value> {
        if matches!(v, Value::Reference(_)) {
            return Err(v);
        }
        let old = self.probe_fill(pid, PC_READ | PC_WRITE, |key, _, data, _| {
            with_array_mut(key, data, |arr| {
                let table = arr.make_mut();
                let Some(dim) = dim else {
                    return match table.push(v.clone()) {
                        Ok(_) => Probe::Hit(None),
                        Err(_) => Probe::Slow,
                    };
                };
                match table.get_mut(dim) {
                    Some(Value::Reference(_)) => Probe::Slow,
                    Some(elem) => Probe::Hit(Some(std::mem::replace(elem, v.clone()))),
                    None => {
                        table.insert(dim.clone(), v.clone());
                        Probe::Hit(None)
                    }
                }
            })
        });
        match old {
            Some(old) => {
                drop(old);
                Ok(v)
            }
            None => Err(v),
        }
    }

    /// `ZEND_PRE/POST_INC/DEC_OBJ` of cacheable place `pid` on a property
    /// holding an int (without overflow) or a float, updated in place: the
    /// result, or `None` for the generic walk.
    #[inline]
    pub(crate) fn prop_cache_incdec(&self, pid: PlaceId, kind: IncDecKind) -> Option<Value> {
        self.probe_fill(pid, PC_READ | PC_WRITE, |key, _, data, code| {
            with_value_mut(key, data, code, |value| {
                deref_mut(value, |num| incdec_in_place(num, kind))
            })
        })
    }

    /// `++`/`--` of a two-segment place `pid` with a cached head:
    /// `$base->a[$k]++` (`ZEND_FETCH_DIM_RW` + `ZEND_PRE/POST_INC`) on an
    /// existing int (without overflow) or float element of an array
    /// property, `$base->a->b++` (`ZEND_PRE/POST_INC_OBJ`) on the object
    /// the head holds; updated in place. The result, or `None` for the
    /// generic walk.
    pub(crate) fn prop_cache_nested_incdec(
        &mut self,
        pid: PlaceId,
        kind: IncDecKind,
    ) -> Option<Value> {
        match self.nested(pid)? {
            Nested::Dim(dim) => self.prop_cache_incdec_elem(pid, &dim, kind),
            Nested::Prop(lit) => {
                let obj = self.head_object(pid)?;
                self.tail_probe(pid, &obj, lit, PC_READ | PC_WRITE, |key, data, code, _| {
                    with_value_mut(key, data, code, |value| {
                        deref_mut(value, |num| incdec_in_place(num, kind))
                    })
                })
            }
            Nested::Append => None,
        }
    }

    /// `$base->a[$k]++` & co. of a two-segment place `pid` with a cached
    /// head (`dim` the key of the place's offset, see [`simple_key`]): see
    /// [`Vm::prop_cache_nested_incdec`].
    pub(crate) fn prop_cache_incdec_elem(
        &self,
        pid: PlaceId,
        dim: &ArrayKey,
        kind: IncDecKind,
    ) -> Option<Value> {
        self.probe_fill(pid, PC_READ | PC_WRITE, |key, _, data, _| {
            with_array_mut(key, data, |arr| {
                if !matches!(arr.get(dim), Some(Value::Int(_) | Value::Float(_))) {
                    return Probe::Slow;
                }
                match arr.make_mut().get_mut(dim) {
                    Some(elem) => incdec_in_place(elem, kind),
                    None => Probe::Slow,
                }
            })
        })
    }

    /// `ZEND_ASSIGN_OBJ_OP` of cacheable place `pid` with a numeric
    /// right-hand side on a property holding a number. The operators of
    /// [`numeric_op`] operate on the slot in place; the others read the
    /// value, operate and write the result back like the generic walk
    /// (`fetch_property_rw`, `assign_op_value`, `write_property`) does.
    /// `Ok(None)` leaves the whole operation to the generic walk.
    ///
    /// # Errors
    /// The operation's error (`DivisionByZeroError` & co.) or the
    /// write-back's.
    pub(crate) fn prop_cache_assign_op(
        &mut self,
        pid: PlaceId,
        op: BinaryOp,
        rhs: &Value,
    ) -> VmResult<Option<Value>> {
        if !matches!(rhs, Value::Int(_) | Value::Float(_)) {
            return Ok(None);
        }
        if pure_op(op) {
            return Ok(self.prop_cache_assign_op_pure(pid, op, rhs));
        }
        let hit = self.probe_fill(pid, PC_READ | PC_WRITE, |key, obj, data, code| {
            with_value(key, data, code, |value| match value {
                Value::Int(_) | Value::Float(_) => Probe::Hit((obj.clone(), value.clone(), *key)),
                _ => Probe::Slow,
            })
        });
        let Some((obj, cur, key)) = hit else {
            return Ok(None);
        };
        let new = match ops::binary_op(op, &cur, rhs, self) {
            Ok(new) => new,
            Err(e) => {
                let t = self.rt_throw(e);
                if key.accept == u32::MAX {
                    return Err(t);
                }
                // A typed property keeps the checked UNDEF result
                // (`zend_binary_assign_op_typed_prop`).
                let name = self.prop_cache_name(pid);
                return Err(self.failed_assign_op_prop(&obj, &name, t));
            }
        };
        // A diagnostic of the operation (a float-to-int deprecation) may
        // have run an error handler that changed the object: the direct
        // store needs the property as it was, anything else goes through
        // the property handlers.
        if accepts(key.accept, &new) && object::data(&obj).lazy.borrow().is_none() {
            let code: &OpArray = &self.frame().code;
            let stored = with_value_mut(&key, object::data(&obj), code, |slot| {
                store_over(slot, &new)
            });
            if let Probe::Hit(old) = stored {
                drop(old);
                return Ok(Some(new));
            }
        }
        let name = self.prop_cache_name(pid);
        self.write_property(&obj, &name, new).map(Some)
    }

    /// The in-place half of [`Vm::prop_cache_assign_op`]: an operator of
    /// [`numeric_op`] with a numeric right-hand side on a property holding
    /// a number, operated on the slot (`zend_binary_op(zptr, zptr, value)`:
    /// nothing can intervene, no diagnostic is possible). The result, or
    /// `None`, nothing changed, for the generic walk (any other operator or
    /// right-hand side, a result the property's type does not accept).
    #[inline]
    pub(crate) fn prop_cache_assign_op_pure(
        &self,
        pid: PlaceId,
        op: BinaryOp,
        rhs: &Value,
    ) -> Option<Value> {
        if !pure_op(op) || !matches!(rhs, Value::Int(_) | Value::Float(_)) {
            return None;
        }
        self.probe_fill(pid, PC_READ | PC_WRITE, |key, _, data, code| {
            with_value_mut(key, data, code, |value| {
                deref_mut(value, |cur| match numeric_op(op, cur, rhs) {
                    Some(new) if accepts(key.accept, &new) => {
                        *cur = new.clone();
                        Probe::Hit(new)
                    }
                    _ => Probe::Slow,
                })
            })
        })
    }

    /// A compound assignment of a two-segment place `pid` with a cached
    /// head, operated in place (`zend_binary_op(var_ptr, var_ptr, value)`):
    /// a [`numeric_op`] operator with a numeric right-hand side on a number,
    /// or `.=` of a string or int onto a string. `$base->a[$k] op= v`
    /// (`ZEND_ASSIGN_DIM_OP`) on an existing element of an array property,
    /// `$base->a->b op= v` (`ZEND_ASSIGN_OBJ_OP`) on the object the head
    /// holds (see [`Vm::prop_cache_assign_op`], [`Vm::prop_cache_concat`]).
    /// The result, or `None` for the generic walk.
    pub(crate) fn prop_cache_nested_assign_op(
        &mut self,
        pid: PlaceId,
        op: BinaryOp,
        rhs: &Value,
    ) -> Option<Value> {
        if !in_place_op(op, rhs) {
            return None;
        }
        match self.nested(pid)? {
            Nested::Dim(dim) => self.prop_cache_assign_op_elem(pid, &dim, op, rhs),
            Nested::Prop(lit) => {
                let obj = self.head_object(pid)?;
                self.tail_probe(pid, &obj, lit, PC_READ | PC_WRITE, |key, data, code, _| {
                    with_value_mut(key, data, code, |value| {
                        deref_mut(value, |cur| operate_in_place(cur, key.accept, op, rhs))
                    })
                })
            }
            Nested::Append => None,
        }
    }

    /// `$base->a[$k] op= v` of a two-segment place `pid` with a cached
    /// head (`dim` the key of the place's offset, see [`simple_key`]) on an
    /// existing element of an array property, operated in place: see
    /// [`Vm::prop_cache_nested_assign_op`].
    pub(crate) fn prop_cache_assign_op_elem(
        &self,
        pid: PlaceId,
        dim: &ArrayKey,
        op: BinaryOp,
        rhs: &Value,
    ) -> Option<Value> {
        if !in_place_op(op, rhs) {
            return None;
        }
        let concat = op == BinaryOp::Concat;
        self.probe_fill(pid, PC_READ | PC_WRITE, |key, _, data, _| {
            with_array_mut(key, data, |arr| {
                let fits = match arr.get(dim) {
                    Some(Value::Str(_)) => concat,
                    Some(Value::Int(_) | Value::Float(_)) => !concat,
                    _ => false,
                };
                if !fits {
                    return Probe::Slow;
                }
                match arr.make_mut().get_mut(dim) {
                    Some(elem) => operate_in_place(elem, u32::MAX, op, rhs),
                    None => Probe::Slow,
                }
            })
        })
    }

    /// `ZEND_ASSIGN_OBJ_OP` with `.=` of cacheable place `pid` on a property
    /// holding a string: appends `rhs` (a string or an int) to that string
    /// in place. php-src operates on the fetched slot
    /// (`zend_binary_op(zptr, zptr, value)`, so `concat_function` with
    /// `result == op1` and `zend_string_extend` when nothing else holds the
    /// string; a shared or interned one is copied once and then grows in
    /// place), and `zend_binary_assign_op_typed_prop` does the same for a
    /// typed property whose value is a string ("make sure that in-place
    /// concatenation is used if the LHS is a string"): the result is a
    /// string, so the type that admitted the current value admits it.
    /// Returns the new value, or `None` for the generic walk: a right-hand
    /// side of another type (its conversion may warn or call
    /// `__toString()`), a property not holding a string directly
    /// (undefined, a reference, a number), and readonly, set-restricted,
    /// hooked or natively handled properties (no `PC_WRITE`).
    pub(crate) fn prop_cache_concat(&self, pid: PlaceId, rhs: &Value) -> Option<Value> {
        if !matches!(rhs, Value::Str(_) | Value::Int(_)) {
            return None;
        }
        self.probe_fill(pid, PC_READ | PC_WRITE, |key, _, data, code| {
            if key.accept != u32::MAX && key.accept & mask::STRING == 0 {
                return Probe::Slow;
            }
            with_value_mut(key, data, code, |value| {
                deref_mut(value, |s| append_in_place(s, rhs))
            })
        })
    }

    /// The result of a cached `isset()`/`empty()`: `found` converted with
    /// `to_bool` when [`isset_value`] could not decide.
    #[inline]
    fn finish_isset(&mut self, found: Option<Result<bool, Value>>) -> VmResult<Option<bool>> {
        match found {
            None => Ok(None),
            Some(Ok(res)) => Ok(Some(res)),
            Some(Err(value)) => Ok(Some(!self.to_bool(&value)?)),
        }
    }

    /// `ZEND_ISSET_ISEMPTY_PROP_OBJ` of cacheable place `pid` on an
    /// initialized declared slot or an existing dynamic property:
    /// `isset()` (or, with `empty`, `empty()`), or `Ok(None)` for the
    /// generic walk.
    ///
    /// # Errors
    /// The conversion of the value to bool for `empty()`.
    #[inline]
    pub(crate) fn prop_cache_isset(&mut self, pid: PlaceId, empty: bool) -> VmResult<Option<bool>> {
        let found = self.isset_probe(pid, empty);
        self.finish_isset(found)
    }

    /// [`Vm::prop_cache_isset`] when the answer needs no conversion: `None`
    /// also where only `to_bool` can tell (`empty()` of an object, a
    /// resource or NAN, see [`isset_value`]), which the generic walk then
    /// answers.
    #[inline]
    pub(crate) fn prop_cache_isset_pure(&self, pid: PlaceId, empty: bool) -> Option<bool> {
        self.isset_probe(pid, empty).and_then(Result::ok)
    }

    /// The probe of [`Vm::prop_cache_isset`].
    #[inline(always)]
    fn isset_probe(&self, pid: PlaceId, empty: bool) -> Option<Result<bool, Value>> {
        self.probe_fill(pid, PC_ISSET, |key, _, data, code| {
            with_value(key, data, code, |value| {
                Probe::Hit(isset_value(value, empty))
            })
        })
    }

    /// `isset()`/`empty()` of a two-segment place `pid` with a cached head
    /// (`ZEND_FETCH_OBJ_IS`, then `ZEND_ISSET_ISEMPTY_DIM_OBJ` or
    /// `_PROP_OBJ`): `$base->a[$k]` on a property holding an array,
    /// `$base->a->b` on the object the head holds; `Ok(None)` for the
    /// generic walk.
    ///
    /// # Errors
    /// The conversion of the value to bool for `empty()`.
    pub(crate) fn prop_cache_nested_isset(
        &mut self,
        pid: PlaceId,
        empty: bool,
    ) -> VmResult<Option<bool>> {
        let found = match self.nested(pid) {
            Some(Nested::Dim(dim)) => self.isset_elem_probe(pid, &dim, empty),
            Some(Nested::Prop(lit)) => self.head_object(pid).and_then(|obj| {
                self.tail_probe(pid, &obj, lit, PC_ISSET, |key, data, code, _| {
                    with_value(key, data, code, |value| {
                        Probe::Hit(isset_value(value, empty))
                    })
                })
            }),
            Some(Nested::Append) | None => None,
        };
        self.finish_isset(found)
    }

    /// `isset($base->a[$k])` / `empty($base->a[$k])` of a two-segment
    /// place `pid` with a cached head (`dim` the key of the place's offset,
    /// see [`simple_key`]) on a property holding an array, when the answer
    /// needs no conversion (see [`Vm::prop_cache_isset_pure`]); `None` for
    /// the generic walk.
    #[inline]
    pub(crate) fn prop_cache_isset_elem_pure(
        &self,
        pid: PlaceId,
        dim: &ArrayKey,
        empty: bool,
    ) -> Option<bool> {
        self.isset_elem_probe(pid, dim, empty).and_then(Result::ok)
    }

    /// The element probe of [`Vm::prop_cache_nested_isset`]: a missing
    /// element is not set (and empty).
    #[inline(always)]
    fn isset_elem_probe(
        &self,
        pid: PlaceId,
        dim: &ArrayKey,
        empty: bool,
    ) -> Option<Result<bool, Value>> {
        self.probe_fill(pid, PC_READ, |key, _, data, code| {
            with_value(key, data, code, |value| {
                with_array(value, |arr| {
                    Probe::Hit(match arr.get(dim) {
                        Some(elem) => isset_value(elem, empty),
                        None => Ok(empty),
                    })
                })
            })
        })
    }

    /// `ZEND_UNSET_OBJ` of cacheable place `pid` (`zend_std_unset_property`
    /// with a cache slot) where no handler, magic method or property-table
    /// bookkeeping is involved (see [`unset_value`]). Returns whether the
    /// unset was done; the old value is released afterwards, as the
    /// generic path does.
    pub(crate) fn prop_cache_unset(&mut self, pid: PlaceId) -> bool {
        let old = self.probe_fill(pid, PC_UNSET, |key, _, data, code| {
            unset_value(key, data, code)
        });
        match old {
            Some(old) => {
                drop(old);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
#[allow(clippy::needless_raw_string_hashes)]
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
        vm.run_script(src.as_bytes(), "t.php");
        drop(vm);
        let out = sink.0.borrow().clone();
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn accepts_matches_type_bits() {
        use super::accepts;
        use crate::types::mask;
        use php_runtime::Value;
        assert!(accepts(u32::MAX, &Value::Null));
        assert!(accepts(mask::LONG, &Value::Int(1)));
        assert!(!accepts(mask::DOUBLE, &Value::Int(1)));
        assert!(accepts(mask::LONG | mask::NULL, &Value::Null));
        assert!(!accepts(mask::BOOL, &Value::Null));
        assert!(accepts(mask::TRUE, &Value::Bool(true)));
        assert!(!accepts(mask::TRUE, &Value::Bool(false)));
        assert!(accepts(
            mask::ITERABLE,
            &Value::Array(php_runtime::ZArray::new())
        ));
        assert!(!accepts(
            mask::ANY,
            &Value::Reference(php_runtime::PhpRef::new(Value::Null))
        ));
    }

    /// Repeated reads, writes, compound assignments, `++`/`--` and
    /// `isset`/`empty` of one place, as a loop hits the cache.
    #[test]
    fn cached_accesses_in_a_loop() {
        let src = r#"<?php
class Foo {
    public $b = 0;
    public int $t = 5;
    public float $f = 1.5;
    function run() {
        $x = 0;
        for ($i = 0; $i < 3; $i++) {
            $x += $this->b;
            $this->b = $i;
            $this->b += 2;
            ++$this->b;
            $this->b--;
            $this->t++;
            $this->f += 1;
            $this->t *= 2;
            $s = isset($this->b) ? 'set' : 'unset';
            $e = empty($this->b) ? 'empty' : 'full';
            echo "$x {$this->b} {$this->t} {$this->f} $s $e\n";
        }
        $this->t = PHP_INT_MAX;
        try { $this->t++; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
        $this->b = PHP_INT_MAX;
        $this->b++;
        var_dump($this->b);
        $this->b = 1;
        try { $this->b %= 0; } catch (DivisionByZeroError $e) { echo $e->getMessage(), "\n"; }
        try { $this->t = "x"; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
        $this->t = "7";
        var_dump($this->t);
    }
}
(new Foo)->run();
$o = new Foo;
for ($i = 0; $i < 3; $i++) { $o->b = $i * 10; echo $o->b, " "; }
echo "\n";
"#;
        let expected = "0 2 12 2.5 set full\n2 3 26 3.5 set full\n5 4 54 4.5 set full\n\
Cannot increment property Foo::$t of type int past its maximal value\n\
float(9.223372036854776E+18)\n\
Modulo by zero\n\
Cannot assign string to property Foo::$t of type int\n\
int(7)\n\
0 10 20 \n";
        assert_eq!(run(src), expected);
    }

    /// One place seeing objects of different classes, a subclass shadowed
    /// by a private property of the scope, and a closure rebound to
    /// another scope: the entry is keyed by class and scope.
    #[test]
    fn class_and_scope_key_the_entry() {
        let src = r#"<?php
class A { private $p = 'A-private'; function get($o) { return $o->p; } }
class B extends A { public $p = 'B-public'; }
class C { public $p = 'C'; }
$a = new A; $b = new B; $c = new C;
foreach ([$a, $b, $c, $b, $a] as $o) { echo $a->get($o), "\n"; }
function rd($o) { return $o->p; }
foreach ([$b, $c, $b] as $o) { echo rd($o), "\n"; }
class P { private $q = 'P-private'; }
class Q { private $q = 'Q-private'; }
$f = function () { return $this->q; };
foreach ([new P, new Q, new P] as $o) {
    echo Closure::bind($f, $o, get_class($o))(), "\n";
}
try { echo Closure::bind($f, new P, Q::class)(), "\n"; } catch (Error $e) { echo $e->getMessage(), "\n"; }
"#;
        let expected = "A-private\nA-private\nC\nA-private\nA-private\n\
B-public\nC\nB-public\n\
P-private\nQ-private\nP-private\n\
Cannot access private property P::$q\n";
        assert_eq!(run(src), expected);
    }

    /// Unset slots go back to `__get`/`__set`/`__isset`, references stay
    /// shared, readonly properties still throw and hooks still run, after
    /// the place has cached the slot.
    #[test]
    fn uncovered_cases_take_the_generic_walk() {
        let src = r#"<?php
class M {
    public $p = 1;
    function __get($n) { echo "get $n\n"; return 42; }
    function __set($n, $v) { echo "set $n\n"; }
    function __isset($n) { echo "isset $n\n"; return true; }
}
function touch_m(M $m) {
    $x = $m->p;
    $m->p = 2;
    $s = isset($m->p);
    return "$x " . var_export($s, true);
}
$m = new M;
echo touch_m($m), "\n";
unset($m->p);
echo touch_m($m), "\n";
class R { public $v = 1; public readonly int $ro; function __construct() { $this->ro = 1; } }
$r = new R;
$x = 5;
$r->v = &$x;
for ($i = 0; $i < 2; $i++) { $r->v = $i + 10; }
var_dump($x);
for ($i = 0; $i < 2; $i++) {
    try { $r->ro = 2; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    try { $r->ro++; } catch (Error $e) { echo $e->getMessage(), "\n"; }
}
class H { public int $h = 0 { set { echo "hook\n"; $this->h = $value * 2; } } }
$h = new H;
for ($i = 1; $i <= 2; $i++) { $h->h = $i; echo $h->h, "\n"; }
"#;
        let expected = "1 true\n\
get p\nset p\nisset p\n42 true\n\
int(11)\n\
Cannot modify readonly property R::$ro\nCannot modify readonly property R::$ro\n\
Cannot modify readonly property R::$ro\nCannot modify readonly property R::$ro\n\
hook\n2\nhook\n4\n";
        assert_eq!(run(src), expected);
    }

    /// A chain starting with a cached property (`$this->a[$i]`,
    /// `$this->o->p`) goes on from the cached value, with the later
    /// segments' warnings and `??` semantics.
    #[test]
    fn cached_head_of_a_longer_chain() {
        let src = r#"<?php
class N {
    public $a = [1, 2, 3];
    public $o;
    public $n = null;
    function __construct() { $this->o = new stdClass; $this->o->p = 'deep'; }
    function run() {
        $s = 0;
        for ($i = 0; $i < 3; $i++) { $s += $this->a[$i]; }
        echo $s, "\n";
        for ($i = 0; $i < 2; $i++) { echo $this->o->p, "\n"; }
        echo $this->a[7] ?? 'dflt', "\n";
        echo $this->n['k'] ?? 'null-head', "\n";
        echo $this->n->q ?? 'null-obj', "\n";
        echo $this->a[5], "|\n";
        echo $this->n->q, "|\n";
    }
}
(new N)->run();
"#;
        let expected = "6\ndeep\ndeep\ndflt\nnull-head\nnull-obj\n\
\nWarning: Undefined array key 5 in t.php on line 15\n|\n\
\nWarning: Attempt to read property \"q\" on null in t.php on line 16\n|\n";
        assert_eq!(run(src), expected);
    }

    /// Dynamic properties: existing ones are read, written, incremented
    /// and tested directly; missing ones still reach `__get`, creation
    /// still deprecates, and a static name still notices.
    #[test]
    fn dynamic_properties() {
        let src = r#"<?php
$o = new stdClass;
$o->v = 1;
for ($i = 0; $i < 3; $i++) {
    $o->v = $o->v + $i;
    $o->v++;
    $o->v += 2;
    echo $o->v, isset($o->v) ? 'y' : 'n', empty($o->w) ? 'e' : 'f', "\n";
}
$o->w = 0;
echo empty($o->w) ? "e\n" : "f\n";
$r = 5;
$o->v = &$r;
for ($i = 0; $i < 2; $i++) { $o->v = $i; }
echo $r, "\n";
class D { public $decl = 1; function __get($n) { return "magic-$n"; } }
$d = new D;
for ($i = 0; $i < 2; $i++) { echo $d->dyn, "\n"; }
$d->dyn = 'real';
for ($i = 0; $i < 2; $i++) { echo $d->dyn, "\n"; }
unset($d->dyn);
echo $d->dyn, "\n";
class S { public static $st = 1; }
$s = new S;
for ($i = 0; $i < 2; $i++) { echo $s->st, "|\n"; }
"#;
        let expected = r#"4ye
8ye
13ye
e
1
magic-dyn
magic-dyn

Deprecated: Creation of dynamic property D::$dyn is deprecated in t.php on line 19
real
real
magic-dyn

Notice: Accessing static property S::$st as non static in t.php on line 25

Warning: Undefined property: S::$st in t.php on line 25
|

Notice: Accessing static property S::$st as non static in t.php on line 25

Warning: Undefined property: S::$st in t.php on line 25
|
"#;
        assert_eq!(run(src), expected);
    }

    /// Writes inside a cached property (`$this->a[] = v`, `$this->a[$k]
    /// op= v`) walk from the slot: copy-on-write, undefined-key warnings,
    /// string offsets and typed auto-vivification behave as before.
    #[test]
    fn writes_inside_a_cached_property() {
        let src = r#"<?php
class L {
    public $items = [];
    public array $typed = [];
    public ?array $maybe = null;
    public $cnt = ['a' => 0];
    public $str = 'abc';
    function run() {
        for ($i = 0; $i < 3; $i++) {
            $this->items[] = $i;
            $this->typed['k' . $i] = $i * 2;
            $this->cnt['a'] += $i;
        }
        $this->maybe[] = 1;
        $copy = $this->items;
        $this->items[] = 99;
        echo cnt($copy), ' ', cnt($this->items), "\n";
        echo enc([$this->items, $this->typed, $this->maybe, $this->cnt]), "\n";
        $this->cnt['zz'] .= 'q';
        echo $this->cnt['zz'], "\n";
        $this->str[1] = 'X';
        echo $this->str, "\n";
    }
}
(new L)->run();
class TI { public int $n = 0; }
$t = new TI;
try { $t->n[] = 1; } catch (Error $e) { echo $e->getMessage(), "\n"; }
$o = new L;
for ($i = 0; $i < 2; $i++) { $o->items['x'][] = $i; }
echo enc($o->items), "\n";
function cnt($a) { $n = 0; foreach ($a as $_) { $n++; } return $n; }
function enc($v) {
    if (!is_array($v)) { return is_string($v) ? "\"$v\"" : var_export($v, true); }
    $list = true; $i = 0; foreach ($v as $k => $_) { if ($k !== $i++) { $list = false; break; } }
    $parts = []; foreach ($v as $k => $x) { $parts[] = ($list ? '' : "\"$k\":") . enc($x); }
    return ($list ? '[' : '{') . implode(',', $parts) . ($list ? ']' : '}');
}
"#;
        let expected = r#"3 4
[[0,1,2,99],{"k0":0,"k1":2,"k2":4},[1],{"a":3}]

Warning: Undefined array key "zz" in t.php on line 19
q
aXc
Cannot use a scalar value as an array
{"x":[0,1]}
"#;
        assert_eq!(run(src), expected);
    }

    /// `isset()`/`empty()` of a chain take the container from the cache.
    #[test]
    fn isset_of_a_chain_with_a_cached_head() {
        let src = r#"<?php
class K {
    public $a = ['k' => 1, 'z' => 0, 'n' => null];
    public $o;
    public $n = null;
    function __construct() { $this->o = new stdClass; $this->o->p = 'x'; }
    function run() {
        for ($i = 0; $i < 2; $i++) {
            var_dump(isset($this->a['k']), isset($this->a['n']), isset($this->a['q']),
                empty($this->a['z']), empty($this->a['k']), isset($this->o->p), isset($this->o->q),
                isset($this->n['x']), empty($this->n->x), isset($this->a[$i]));
        }
    }
}
(new K)->run();
"#;
        let expected = r#"bool(true)
bool(false)
bool(false)
bool(true)
bool(false)
bool(true)
bool(false)
bool(false)
bool(true)
bool(false)
bool(true)
bool(false)
bool(false)
bool(true)
bool(false)
bool(true)
bool(false)
bool(false)
bool(true)
bool(false)
"#;
        assert_eq!(run(src), expected);
    }

    /// `++`/`--` inside a cached property (`$this->c['a']++`): both the
    /// read-modify-write walk and the write-back start from the slot.
    #[test]
    fn incdec_inside_a_cached_property() {
        let src = r#"<?php class AO implements ArrayAccess { public $a; function __construct($a) { $this->a = $a; } function offsetExists($k): bool { return isset($this->a[$k]); } function &offsetGet($k): mixed { return $this->a[$k]; } function offsetSet($k, $v): void { $this->a[$k] = $v; } function offsetUnset($k): void { unset($this->a[$k]); } }
class Cnt {
    public $c = ['a' => 0];
    public array $t = [1.5];
    public $o;
    function __construct() { $this->o = new AO(['n' => 1]); }
    function run() {
        for ($i = 0; $i < 3; $i++) {
            $this->c['a']++;
            ++$this->c['b'];
            $x = $this->t[0]--;
            $this->o['n']++;
        }
        echo enc($this->c), ' ', enc($this->t), ' ', $x, ' ', $this->o['n'], "\n";
        $y = $this->c['a']++;
        $z = --$this->c['a'];
        echo "$y $z\n";
    }
}
(new Cnt)->run();
function enc($v) {
    if (!is_array($v)) { return is_string($v) ? "\"$v\"" : var_export($v, true); }
    $list = true; $i = 0; foreach ($v as $k => $_) { if ($k !== $i++) { $list = false; break; } }
    $parts = []; foreach ($v as $k => $x) { $parts[] = ($list ? '' : "\"$k\":") . enc($x); }
    return ($list ? '[' : '{') . implode(',', $parts) . ($list ? ']' : '}');
}
"#;
        let expected = r#"
Warning: Undefined array key "b" in t.php on line 10
{"a":3,"b":3} [-1.5] -0.5 4
3 3
"#;
        assert_eq!(run(src), expected);
    }

    /// References to cached properties (`&$this->p`, by-reference
    /// arguments, `foreach` by reference, `&$this->a[$k]`): typed
    /// properties still become type sources of the reference.
    #[test]
    fn references_to_cached_properties() {
        let src = r#"<?php
class Rf {
    public $p = 1;
    public $list = [3, 1, 2];
    public int $t = 1;
    public $a = ['k' => 0];
    function run() {
        for ($i = 0; $i < 2; $i++) {
            $r = &$this->p;
            $r += 10;
            unset($r);
        }
        srt($this->list);
        foreach ($this->list as &$v) { $v *= 2; }
        unset($v);
        $q = &$this->t;
        try { $q = "x"; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
        $q = 5;
        $w = &$this->a['k'];
        $w = 9;
        echo $this->p, ' ', enc($this->list), ' ', $this->t, ' ', enc($this->a), "\n";
    }
}
(new Rf)->run();
$o = new Rf;
for ($i = 0; $i < 2; $i++) { push($o->list, $i); }
echo enc($o->list), "\n";
function srt(array &$a) {
    $v = []; foreach ($a as $x) { $v[] = $x; }
    $n = 0; foreach ($v as $_) { $n++; }
    for ($i = 1; $i < $n; $i++) { for ($j = $i; $j > 0 && $v[$j - 1] > $v[$j]; $j--) { $t = $v[$j]; $v[$j] = $v[$j - 1]; $v[$j - 1] = $t; } }
    $a = $v;
}
function push(array &$a, $v) { $a[] = $v; }
function enc($v) {
    if (!is_array($v)) { return is_string($v) ? "\"$v\"" : var_export($v, true); }
    $list = true; $i = 0; foreach ($v as $k => $_) { if ($k !== $i++) { $list = false; break; } }
    $parts = []; foreach ($v as $k => $x) { $parts[] = ($list ? '' : "\"$k\":") . enc($x); }
    return ($list ? '[' : '{') . implode(',', $parts) . ($list ? ']' : '}');
}
"#;
        let expected = r#"Cannot assign string to reference held by property Rf::$t of type int
21 [2,4,6] 5 {"k":9}
[3,1,2,0,1]
"#;
        assert_eq!(run(src), expected);
    }

    /// Constructors initializing typed properties: an uninitialized slot
    /// that was never unset takes an accepted value directly; coercions,
    /// type errors and `__set()` after `unset()` keep the generic path.
    #[test]
    fn typed_slots_initialized_through_the_cache() {
        let src = r#"<?php
class Dep {}
class Svc {
    public int $n;
    public ?Dep $d;
    public string $s;
    public float $f;
    function __construct(int $n, ?Dep $d) {
        $this->n = $n;
        $this->d = $d;
        $this->s = "s$n";
        $this->f = $n;
    }
}
$all = [];
for ($i = 0; $i < 3; $i++) { $all[] = new Svc($i, $i ? new Dep : null); }
var_dump($all[2]);
class U {
    public int $x;
    function __set($n, $v) { echo "__set $n\n"; }
    function reset() { unset($this->x); }
    function put($v) { $this->x = $v; }
}
$u = new U;
$u->put(1);
$u->reset();
$u->put(2);
var_dump($u);
$w = new U;
try { $w->put("abc"); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$w->put("42");
var_dump($w->x);
"#;
        let expected = r#"object(Svc)#4 (4) {
  ["n"]=>
  int(2)
  ["d"]=>
  object(Dep)#5 (0) {
  }
  ["s"]=>
  string(2) "s2"
  ["f"]=>
  float(2)
}
__set x
object(U)#6 (0) {
  ["x"]=>
  uninitialized(int)
}
Cannot assign string to property U::$x of type int
int(42)
"#;
        assert_eq!(run(src), expected);
    }

    /// Class-typed properties: instances of the one named class (or a
    /// subclass / implementation) are stored directly; others still get
    /// the `TypeError`, `self` types keep the generic check.
    #[test]
    fn class_typed_properties() {
        let src = r#"<?php
interface Shape {}
class Sq implements Shape {}
class BigSq extends Sq {}
class Other {}
class Holder {
    public ?Sq $sq = null;
    public Shape $shape;
    public self $me;
    function set($v) {
        try { $this->sq = $v; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
        try { $this->shape = $v; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
        $this->me = $this;
    }
}
$h = new Holder;
foreach ([new Sq, new BigSq, new Other, null, new Sq] as $v) {
    $h->set($v);
    echo get_class($h->sq ?? new stdClass), ' ', isset($h->shape) ? get_class($h->shape) : '-', "\n";
}
"#;
        let expected = r#"Sq Sq
BigSq BigSq
Cannot assign Other to property Holder::$sq of type ?Sq
Cannot assign Other to property Holder::$shape of type Shape
BigSq BigSq
Cannot assign null to property Holder::$shape of type Shape
stdClass BigSq
Sq Sq
"#;
        assert_eq!(run(src), expected);
    }

    /// `unset()` of cached properties (declared, typed, never initialized,
    /// dynamic) and writes into the unset slots: `__unset`/`__set`/`__get`
    /// still run where php-src calls them, a materialized property table
    /// keeps its counts, destructors run at the unset, readonly still throws.
    #[test]
    #[allow(clippy::too_many_lines)] // one oracle-checked script
    fn unset_and_rewrite_through_the_cache() {
        let src = r#"<?php
class A {
    public $u = 1;
    public int $t = 2;
    public int $never;
    function run() {
        for ($i = 0; $i < 3; $i++) {
            unset($this->u);
            var_dump(isset($this->u));
            $this->u = $i;
            unset($this->t);
            $this->t = $i * 2;
            echo $this->u, ' ', $this->t, "\n";
        }
        unset($this->u);
        echo $this->u, "|\n";
        unset($this->never);
        try { echo $this->never; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
        $this->never = 5;
        echo $this->never, "\n";
        var_dump($this);
    }
}
$a = new A; $a->run();
class M {
    public $p = 1;
    public int $q = 1;
    function __set($n, $v) { echo "__set($n)\n"; }
    function __get($n) { echo "__get($n)\n"; return 42; }
    function __unset($n) { echo "__unset($n)\n"; }
    function run() {
        for ($i = 0; $i < 2; $i++) {
            unset($this->p);
            unset($this->p);
            $this->p = 5;
            echo $this->p, "\n";
            unset($this->q);
            $this->q = 7;
        }
    }
}
$m = new M; $m->run();
class T { public $a = 1; public $b = 2; public $c = 3; }
function f(T $t) {
    foreach ($t as $k => $v) { echo "$k=$v "; }
    echo "\n";
    unset($t->b);
    var_dump($t);
    $t->b = 'B';
    var_dump($t);
    foreach ($t as $k => $v) { echo "$k=$v "; }
    echo "\n";
}
$t1 = new T; f($t1); $t2 = new T; f($t2);
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "destruct {$this->n}\n"; } }
class H {
    public $d;
    function run() {
        for ($i = 0; $i < 3; $i++) {
            $this->d = new D($i);
            echo "before unset $i\n";
            unset($this->d);
            echo "after unset $i\n";
        }
    }
}
$hh = new H; $hh->run();
$o = new stdClass;
for ($i = 0; $i < 3; $i++) {
    $o->x = $i;
    unset($o->x);
    var_dump(isset($o->x));
    $o->x = "again $i";
    echo $o->x, "\n";
}
var_dump($o);
class Dyn {}
$d = new Dyn;
for ($i = 0; $i < 2; $i++) { $d->z = $i; unset($d->z); }
var_dump($d);
class RO { public function __construct(public readonly int $r = 1) {} function un() { unset($this->r); } }
$ro = new RO;
for ($i = 0; $i < 2; $i++) { try { $ro->un(); } catch (Error $e) { echo $e->getMessage(), "\n"; } }
class W { public $a = 1; public $b = 2; }
$w = new W;
for ($i = 0; $i < 2; $i++) { $w->dyn = 1; unset($w->a); $w->a = $i; }
var_dump($w);
"#;
        let expected = r#"bool(false)
0 0
bool(false)
1 2
bool(false)
2 4

Warning: Undefined property: A::$u in t.php on line 16
|
Error: Typed property A::$never must not be accessed before initialization
5
object(A)#1 (2) {
  ["t"]=>
  int(4)
  ["never"]=>
  int(5)
}
__unset(p)
__set(p)
__get(p)
42
__set(q)
__unset(p)
__unset(p)
__set(p)
__get(p)
42
__unset(q)
__set(q)
a=1 b=2 c=3 
object(T)#3 (2) {
  ["a"]=>
  int(1)
  ["c"]=>
  int(3)
}
object(T)#3 (3) {
  ["a"]=>
  int(1)
  ["b"]=>
  string(1) "B"
  ["c"]=>
  int(3)
}
a=1 b=B c=3 
a=1 b=2 c=3 
object(T)#4 (2) {
  ["a"]=>
  int(1)
  ["c"]=>
  int(3)
}
object(T)#4 (3) {
  ["a"]=>
  int(1)
  ["b"]=>
  string(1) "B"
  ["c"]=>
  int(3)
}
a=1 b=B c=3 
before unset 0
destruct 0
after unset 0
before unset 1
destruct 1
after unset 1
before unset 2
destruct 2
after unset 2
bool(false)
again 0
bool(false)
again 1
bool(false)
again 2
object(stdClass)#6 (1) {
  ["x"]=>
  string(7) "again 2"
}

Deprecated: Creation of dynamic property Dyn::$z is deprecated in t.php on line 79

Deprecated: Creation of dynamic property Dyn::$z is deprecated in t.php on line 79
object(Dyn)#7 (0) {
}
Cannot unset readonly property RO::$r
Cannot unset readonly property RO::$r

Deprecated: Creation of dynamic property W::$dyn is deprecated in t.php on line 86
object(W)#9 (3) {
  ["a"]=>
  int(1)
  ["b"]=>
  int(2)
  ["dyn"]=>
  int(1)
}
"#;
        assert_eq!(run(src), expected);
    }

    /// `$this->a[$k]` read, assigned, `op=`, `++`/`--`, `.=` and tested from
    /// the cached slot: copy-on-write, references inside the array, numeric
    /// string keys, overflow, undefined keys, diagnostics of the generic walk,
    /// typed and readonly array properties, `ArrayAccess` objects.
    #[test]
    #[allow(clippy::too_many_lines)] // one oracle-checked script
    fn elements_of_cached_array_properties() {
        let src = r#"<?php
class L {
    public $a = ['x' => 1, 'y' => 2.5, 's' => 'str', 5 => 10];
    public array $ta = [1, 2, 3];
    public $n = null;
    public $str = 'abc';
    function run() {
        $k = 'x'; $five = 5; $num = '5'; $y = 'y'; $s = 's';
        for ($i = 0; $i < 3; $i++) {
            $this->a[$k]++;
            ++$this->a[$five];
            $this->a[$y] += 1;
            $this->a[$s] .= $i;
            $this->a[$num] *= 2;
            $this->a[$k] |= 16;
            $this->ta[$i] = $i * 10;
            $this->a['new' . $i] = $i;
            $x = $this->a[$k] + $this->a[$five];
            echo $x, ' ', $this->ta[$i], ' ', $this->a[$num], ' ', $this->a['y'], "\n";
        }
        echo dump($this->a), "\n", dump($this->ta), "\n";
        $copy = $this->a;
        $this->a['x'] = 'changed';
        echo $copy['x'], ' ', $this->a['x'], "\n";
        $r = &$this->a['x'];
        $this->a['x'] = 'via-ref';
        echo $r, "\n";
        $this->a['x'] .= '!';
        $this->a['x'] = 7;
        $this->a['x']++;
        echo $r, "\n";
        unset($r);
        $this->n[$k] = 1;
        echo dump($this->n), "\n";
        $this->str[1] = 'X';
        echo $this->str, "\n";
        echo $this->a['missing'] ?? 'dflt', "\n";
        echo isset($this->a[$k]) ? 'y' : 'n', isset($this->a['missing']) ? 'y' : 'n', empty($this->a[$k]) ? 'e' : 'f', empty($this->a['nope']) ? 'e' : 'f', "\n";
        $k = 'o';
        $this->a[$k] = PHP_INT_MAX;
        $this->a[$k]++;
        var_dump($this->a[$k]);
        $this->a[$k] = PHP_INT_MAX;
        $this->a[$k] += 1;
        var_dump($this->a[$k]);
        $this->a[$k] = PHP_INT_MIN;
        $x = $this->a[$k]--;
        var_dump($x, $this->a[$k]);
        $this->a[$k] = 2.5;
        $x = $this->a[$k]++;
        $this->a[$k] *= 3;
        $this->a[$k] -= 0.5;
        var_dump($x, $this->a[$k]);
        echo $this->a['nokey'], "|\n";
        $this->a['nokey2']++;
        var_dump($this->a['nokey2']);
        $this->a['nokey3'] .= 'z';
        var_dump($this->a['nokey3']);
        $this->a[$k] = 1.5;
        $this->a[$k] |= 1;
        var_dump($this->a[$k]);
        $this->a[$k] = '12';
        $this->a[$k] += 1;
        var_dump($this->a[$k]);
        $this->a[$k] = 10;
        try { $this->a[$k] %= 0; } catch (DivisionByZeroError $e) { echo $e->getMessage(), "\n"; }
        $this->a[$k] /= 4;
        var_dump($this->a[$k]);
        $this->a[-3] = 'neg';
        $this->a['007'] = 'str-key';
        $this->a['7'] = 'int-key';
        echo dump($this->a), "\n";
    }
}
function dump($v) {
    if (!is_array($v)) { return is_string($v) ? "\"$v\"" : var_export($v, true); }
    $parts = []; foreach ($v as $k => $x) { $parts[] = var_export($k, true) . '=>' . dump($x); }
    return '[' . implode(',', $parts) . ']';
}
(new L)->run();
class R2 {
    public function __construct(public readonly array $ro = [1]) {}
    function run() {
        $z = 0;
        try { $this->ro[$z] = 2; } catch (Error $e) { echo $e->getMessage(), "\n"; }
        try { $this->ro[$z]++; } catch (Error $e) { echo $e->getMessage(), "\n"; }
        try { $this->ro[$z] += 1; } catch (Error $e) { echo $e->getMessage(), "\n"; }
        echo $this->ro[$z], isset($this->ro[$z]) ? 'y' : 'n', "\n";
    }
}
(new R2)->run(); (new R2)->run();
class AA implements ArrayAccess {
    private $d = [];
    function offsetExists($o): bool { echo "exists $o\n"; return isset($this->d[$o]); }
    function offsetGet($o): mixed { echo "get $o\n"; return $this->d[$o] ?? null; }
    function offsetSet($o, $v): void { echo "set $o\n"; $this->d[$o] = $v; }
    function offsetUnset($o): void { unset($this->d[$o]); }
}
class HasAA {
    public $aa;
    function __construct() { $this->aa = new AA; }
    function run() { $k = 'q'; for ($i = 0; $i < 2; $i++) { $this->aa[$k] = $i; echo $this->aa[$k], "\n"; var_dump(isset($this->aa[$k])); } }
}
(new HasAA)->run();
class TA {
    public ?array $maybe = null;
    public int|array $ia = 5;
    public array $list = [];
    function run() {
        $k = 'k';
        $this->maybe[$k] = 1;
        try { $this->ia[$k] = 1; } catch (Error $e) { echo $e->getMessage(), "\n"; }
        for ($i = 0; $i < 3; $i++) { $this->list[$i] = $i; $this->list[$i] += 10; }
        echo dump($this->maybe), dump($this->list), "\n";
    }
}
(new TA)->run();
set_error_handler(function ($no, $msg) { echo "handler: $msg\n"; return true; });
class EH { public $a = ['k' => 1]; function run() { $u = 'undefined-key'; $this->a[$u]++; $this->a[$u] .= 'x'; echo $this->a[$u], "\n"; } }
(new EH)->run();
"#;
        let expected = r#"40 0 22 3.5
65 10 46 4.5
114 20 94 5.5
['x'=>20,'y'=>5.5,'s'=>"str012",5=>94,'new0'=>0,'new1'=>1,'new2'=>2]
[0=>0,1=>10,2=>20]
20 changed
via-ref
8
['x'=>1]
aXc
dflt
ynfe
float(9.223372036854776E+18)
float(9.223372036854776E+18)
int(-9223372036854775808)
float(-9.223372036854776E+18)
float(2.5)
float(10)

Warning: Undefined array key "nokey" in t.php on line 54
|

Warning: Undefined array key "nokey2" in t.php on line 55
int(1)

Warning: Undefined array key "nokey3" in t.php on line 57
string(1) "z"

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 60
int(1)
int(13)
Modulo by zero
float(2.5)
['x'=>8,'y'=>5.5,'s'=>"str012",5=>94,'new0'=>0,'new1'=>1,'new2'=>2,'o'=>2.5,'nokey2'=>1,'nokey3'=>"z",-3=>"neg",'007'=>"str-key",7=>"int-key"]
Cannot indirectly modify readonly property R2::$ro
Cannot indirectly modify readonly property R2::$ro
Cannot indirectly modify readonly property R2::$ro
1y
Cannot indirectly modify readonly property R2::$ro
Cannot indirectly modify readonly property R2::$ro
Cannot indirectly modify readonly property R2::$ro
1y
set q
get q
0
exists q
bool(true)
set q
get q
1
exists q
bool(true)
Cannot use a scalar value as an array
['k'=>1][0=>10,1=>11,2=>12]
handler: Undefined array key "undefined-key"
1x
"#;
        assert_eq!(run(src), expected);
    }

    /// The last `->name` of `$list[$i]->p`, `f()->p`, `$this->o->p` keeps its
    /// own entry: classes alternating at one site, scopes and visibility,
    /// `__get` for unset or missing properties, typed and protected
    /// properties, null in a chain, a property holding a reference.
    #[test]
    #[allow(clippy::too_many_lines)] // one oracle-checked script
    fn later_segments_have_their_own_entry() {
        let src = r#"<?php
class P { public $p = 'P'; protected $prot = 'prot'; private $priv = 'priv'; public int $t; }
class Q extends P { public $p = 'Q'; }
class Z { public $p = 'Z'; function __get($n) { return "magic $n"; } }
function first(array $list) { return $list[0]->p; }
function mk($o) { return $o; }
$std = new stdClass;
$std->p = 'std';
$objs = [new P, new Q, new Z, new P, $std, new Q];
foreach ($objs as $o) { echo first([$o]), ' ', mk($o)->p, "\n"; }
$z = new Z;
unset($z->p);
echo first([$z]), "\n";
echo mk($z)->nope ?? 'isnull', "\n";
$list = [new Q];
for ($i = 0; $i < 3; $i++) { echo $list[0]->p, $list[0]?->p, "\n"; }
for ($i = 0; $i < 2; $i++) { echo $list[0]->undef, "|\n"; }
for ($i = 0; $i < 2; $i++) { try { echo mk(new P)->t, "\n"; } catch (Error $e) { echo $e->getMessage(), "\n"; } }
for ($i = 0; $i < 2; $i++) { try { echo mk(new P)->prot, "\n"; } catch (Error $e) { echo $e->getMessage(), "\n"; } }
for ($i = 0; $i < 2; $i++) { echo mk(new P)->prot ?? 'quiet', "\n"; }
class S {
    private $s = 'S-private';
    static function rd($arr) { return $arr[0]->s; }
}
class SubS extends S { public $s = 'SubS-public'; }
for ($i = 0; $i < 2; $i++) { echo S::rd([new S]), ' ', S::rd([new SubS]), ' ', first2([new SubS]), "\n"; }
function first2($l) { return $l[0]->s; }
try { echo first2([new S]); } catch (Error $e) { echo $e->getMessage(), "\n"; }
class Node { public $next; public $v; function __construct($v, $n = null) { $this->v = $v; $this->next = $n; } }
class Walker {
    public $head;
    function __construct() { $this->head = new Node(1, new Node(2, new Node(3))); }
    function run() {
        for ($i = 0; $i < 2; $i++) {
            echo $this->head->v, $this->head->next->v, $this->head->next->next->v, "\n";
            $n = $this->head;
            echo $n->next->v, $n->next->next->v, "\n";
        }
        $this->head->next = null;
        echo $this->head->next->v ?? 'end', "\n";
        echo $this->head->next->v, "|\n";
    }
}
(new Walker)->run();
$rf = new P;
$x = 'refd';
$rf->p = &$x;
for ($i = 0; $i < 2; $i++) { echo first([$rf]), "\n"; $x .= '!'; }
$arr = [[new Q]];
for ($i = 0; $i < 2; $i++) { echo $arr[0][0]->p, "\n"; }
$f = function () { return $this->priv; };
for ($i = 0; $i < 2; $i++) { echo Closure::bind(function ($l) { return $l[0]->priv; }, null, P::class)([new P]), "\n"; }
"#;
        let expected = r#"P P
Q Q
Z Z
P P
std std
Q Q
magic p
magic nope
QQ
QQ
QQ

Warning: Undefined property: Q::$undef in t.php on line 17
|

Warning: Undefined property: Q::$undef in t.php on line 17
|
Typed property P::$t must not be accessed before initialization
Typed property P::$t must not be accessed before initialization
Cannot access protected property P::$prot
Cannot access protected property P::$prot
quiet
quiet
S-private S-private SubS-public
S-private S-private SubS-public
Cannot access private property S::$s
123
23
123
23
end

Warning: Attempt to read property "v" on null in t.php on line 41
|
refd
refd!
Q
Q
priv
priv
"#;
        assert_eq!(run(src), expected);
    }

    /// Class-typed properties remember the class of an accepted object (a
    /// subclass, an interface implementation), rejections still throw; and
    /// `isset()`/`empty()` of every kind of value, directly, through a
    /// reference and as array elements.
    #[test]
    #[allow(clippy::too_many_lines)] // one oracle-checked script
    fn class_typed_stores_and_isset_values() {
        let src = r#"<?php
interface Shape {}
class Sq implements Shape {}
class BigSq extends Sq {}
class Other {}
class Holder {
    public ?Sq $sq = null;
    public Shape $shape;
    public Sq|int $u = 0;
    public object $any;
    public Holder $self2;
    function set($v) {
        try { $this->sq = $v; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
        try { $this->shape = $v; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
        try { $this->u = $v; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
        try { $this->any = $v; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
        echo get_class($this->sq ?? new stdClass), ' ', isset($this->shape) ? get_class($this->shape) : '-', ' ', is_object($this->u) ? get_class($this->u) : $this->u, "\n";
    }
}
$h = new Holder;
foreach ([new Sq, new Sq, new BigSq, new Other, new Sq, new BigSq, null, new Other, 5, new Sq] as $v) { $h->set($v); }
class Box { public Holder $h; function put($x) { $this->h = $x; } }
$b = new Box;
for ($i = 0; $i < 3; $i++) { $b->put(new Holder); }
try { $b->put(new Box); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
$b->put(new Holder);
echo get_class($b->h), "\n";
class E {
    public $v;
    function t($v) {
        $this->v = $v;
        return (isset($this->v) ? 'S' : 's') . (empty($this->v) ? 'E' : 'e');
    }
}
$e = new E;
foreach ([null, false, true, 0, 1, -1, 0.0, -0.0, 1.5, "", "0", "00", " ", "a", [], [0], new stdClass] as $v) { echo $e->t($v), ' '; }
echo "\n";
$r = 0;
$e->v = &$r;
echo isset($e->v) ? 'S' : 's', empty($e->v) ? 'E' : 'e', "\n";
$r = "x";
echo isset($e->v) ? 'S' : 's', empty($e->v) ? 'E' : 'e', "\n";
class EA { public $a = ['z' => 0, 'n' => null, 'o' => '0', 'f' => 0.0, 'e' => [], 't' => 'yes'];
    function run() { foreach (['z', 'n', 'o', 'f', 'e', 't', 'missing'] as $k) { echo $k, ':', isset($this->a[$k]) ? 'S' : 's', empty($this->a[$k]) ? 'E' : 'e', ' '; } echo "\n"; } }
(new EA)->run(); (new EA)->run();
"#;
        let expected = r#"Sq Sq Sq
Sq Sq Sq
BigSq BigSq BigSq
Cannot assign Other to property Holder::$sq of type ?Sq
Cannot assign Other to property Holder::$shape of type Shape
Cannot assign Other to property Holder::$u of type Sq|int
BigSq BigSq BigSq
Sq Sq Sq
BigSq BigSq BigSq
Cannot assign null to property Holder::$shape of type Shape
Cannot assign null to property Holder::$u of type Sq|int
Cannot assign null to property Holder::$any of type object
stdClass BigSq BigSq
Cannot assign Other to property Holder::$sq of type ?Sq
Cannot assign Other to property Holder::$shape of type Shape
Cannot assign Other to property Holder::$u of type Sq|int
stdClass BigSq BigSq
Cannot assign int to property Holder::$sq of type ?Sq
Cannot assign int to property Holder::$shape of type Shape
Cannot assign int to property Holder::$any of type object
stdClass BigSq 5
Sq Sq Sq
Cannot assign Box to property Box::$h of type Holder
Holder
sE SE Se SE Se Se SE SE Se SE SE Se Se Se SE Se Se 
SE
Se
z:SE n:sE o:SE f:SE e:SE t:Se missing:sE 
z:SE n:sE o:SE f:SE e:SE t:Se missing:sE 
"#;
        assert_eq!(run(src), expected);
    }

    /// `$this->a[] = v` from the cached slot: the result value, copy-on-write,
    /// next keys after explicit ones, auto-vivification of null, strings and
    /// `ArrayAccess` objects, a full array, an array held by reference. (The
    /// readonly lines are php-src master's "Cannot indirectly modify".)
    #[test]
    #[allow(clippy::too_many_lines)] // one oracle-checked script
    fn appends_to_cached_array_properties() {
        let src = r#"<?php
class AA implements ArrayAccess {
    function offsetExists($o): bool { return false; }
    function offsetGet($o): mixed { return null; }
    function offsetSet($o, $v): void { echo "offsetSet(", var_export($o, true), ", $v)\n"; }
    function offsetUnset($o): void {}
}
class Bag {
    public $items = [];
    public array $typed = [];
    public $none;
    public $str = 'ab';
    public $obj;
    public $full = [PHP_INT_MAX => 'last'];
    function __construct() { $this->obj = new AA; }
    function run() {
        for ($i = 0; $i < 3; $i++) {
            $this->items[] = $i;
            $r = ($this->typed[] = $i * 2);
            echo $r, ' ';
        }
        echo "\n";
        $copy = $this->items;
        $this->items[] = 'more';
        echo implode(',', $copy), ' | ', implode(',', $this->items), "\n";
        $this->items[10] = 'ten';
        $this->items[] = 'eleven';
        $this->items['k'] = 'str';
        $this->items[] = 'twelve';
        foreach ($this->items as $k => $v) { echo "$k=$v "; }
        echo "\n";
        $this->none[] = 'vivified';
        echo implode(',', $this->none), "\n";
        try { $this->str[] = 'c'; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
        $this->obj[] = 'to-object';
        for ($i = 0; $i < 2; $i++) {
            try { $this->full[] = 'overflow'; } catch (Error $e) { echo $e->getMessage(), "\n"; }
        }
        $ref = &$this->items;
        $this->items[] = 'through-ref';
        echo count2($ref), "\n";
        unset($ref);
        $this->items[] = 'after';
        echo count2($this->items), "\n";
    }
}
function count2($a) { $n = 0; foreach ($a as $_) { $n++; } return $n; }
$b = new Bag;
$b->run();
class RoBag { public function __construct(public readonly array $list = []) {} function add($v) { $this->list[] = $v; } }
$rb = new RoBag;
for ($i = 0; $i < 2; $i++) { try { $rb->add($i); } catch (Error $e) { echo $e->getMessage(), "\n"; } }
$std = new stdClass;
$std->list = [];
for ($i = 0; $i < 3; $i++) { $std->list[] = $i; }
echo implode(',', $std->list), "\n";
"#;
        let expected = r#"0 2 4 
0,1,2 | 0,1,2,more
0=0 1=1 2=2 3=more 10=ten 11=eleven k=str 12=twelve 
vivified
Error: [] operator not supported for strings
offsetSet(NULL, to-object)
Cannot add element to the array as the next element is already occupied
Cannot add element to the array as the next element is already occupied
9
10
Cannot indirectly modify readonly property RoBag::$list
Cannot indirectly modify readonly property RoBag::$list
0,1,2
"#;
        assert_eq!(run(src), expected);
    }

    /// A property holding a reference (`$r = &$this->p`, `foreach` by
    /// reference, by-reference arguments): writes, `++`, `op=`, `.=` and
    /// element writes go through it; typed references keep their checks.
    #[test]
    #[allow(clippy::too_many_lines)] // one oracle-checked script
    fn writes_through_references_held_by_properties() {
        let src = r#"<?php
class RefHolder {
    public $b = 0;
    public $s = 'x';
    public $a = [1, 2, 3];
    public int $typed = 0;
    public array $ta = [5];
    function run() {
        $r = &$this->b;
        $rs = &$this->s;
        $ra = &$this->a;
        $rt = &$this->typed;
        $rta = &$this->ta;
        for ($i = 0; $i < 3; $i++) {
            $this->b = $i;
            $this->b++;
            $this->b += 10;
            $this->s .= $i;
            $this->a[$i] = $i * 100;
            $this->a[] = 'app';
            $this->a[$i]++;
            $this->a[$i] .= '!';
            $this->typed = $i;
            $this->typed++;
            $this->ta[] = $i;
            $this->ta[0] += 1;
        }
        echo $r, ' ', $rs, ' ', implode(',', $ra), ' ', $rt, ' ', implode(',', $rta), "\n";
        try { $this->typed = 'abc'; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
        $rt = PHP_INT_MAX;
        try { $this->typed++; } catch (Error $e) { echo $e->getMessage(), "\n"; }
        echo $rt, "\n";
        foreach ($this->a as &$v) { $v = "v$v"; }
        unset($v);
        $this->a[0] = 'after-foreach';
        echo implode(',', $this->a), "\n";
        sort2($this->a);
        $this->a[] = 'after-sort';
        echo implode(',', $this->a), "\n";
        unset($r);
        $this->b = 'no-longer-shared';
        echo $this->b, "\n";
    }
}
function sort2(array &$a) { $a[] = 'sorted'; }
$h = new RefHolder;
$h->run();
$o = new stdClass;
$x = 1;
$o->v = &$x;
for ($i = 0; $i < 3; $i++) { $o->v = $o->v + 1; $o->v++; $o->v *= 2; $o->v .= ''; }
var_dump($x);
class Self2 { public $p; function run() { $this->p = [1]; $this->p[0] = &$this->p; $this->p[1] = 'x'; echo count3($this->p), "\n"; } }
function count3($a) { $n = 0; foreach ($a as $_) { $n++; } return $n; }
$s2 = new Self2;
$s2->run();
"#;
        let expected = r#"13 x012 1!,101!,201!,app,app,app 3 8,0,1,2
Cannot assign string to property RefHolder::$typed of type int
Cannot increment a reference held by property RefHolder::$typed of type int past its maximal value
9223372036854775807
after-foreach,v101!,v201!,vapp,vapp,vapp
after-foreach,v101!,v201!,vapp,vapp,vapp,sorted,after-sort
no-longer-shared
string(2) "36"
2
"#;
        assert_eq!(run(src), expected);
    }

    /// `$this->o->p = v`, `++`, `op=`, `.=` and `isset()` on the object a
    /// cached head holds (also a readonly head), with the last segment's
    /// entry: typed, private, dynamic and referenced properties, a null or
    /// int head, a class change, a head holding `$this`, `__get` heads.
    #[test]
    #[allow(clippy::too_many_lines)] // one oracle-checked script
    fn writes_through_a_cached_head_object() {
        let src = r#"<?php
class Inner { public $v = 0; public int $n = 0; public $s = 'a'; public ?Inner $next = null; private $priv = 'p'; }
class Sub extends Inner { public $v = 100; }
class Outer {
    public $in;
    public readonly Inner $ro;
    public $none = null;
    public $num = 5;
    public $self;
    function __construct() { $this->in = new Inner; $this->ro = new Inner; $this->self = $this; }
    function run() {
        for ($i = 0; $i < 3; $i++) {
            $this->in->v = $i;
            $this->in->v++;
            $this->in->v += 10;
            $this->in->s .= $i;
            $this->in->n = $i * 2;
            $this->in->n++;
            $this->ro->v = $i;
            $this->ro->n += 3;
            $r = isset($this->in->v) ? 'S' : 's';
            $e = empty($this->in->s) ? 'E' : 'e';
            echo $this->in->v, ' ', $this->in->s, ' ', $this->in->n, ' ', $this->ro->v, ' ', $this->ro->n, " $r$e\n";
        }
        try { $this->in->n = 'x'; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
        $this->in->n = PHP_INT_MAX;
        try { $this->in->n++; } catch (Error $e) { echo $e->getMessage(), "\n"; }
        try { $this->none->v = 1; } catch (Error $e) { echo $e->getMessage(), "\n"; }
        try { $this->num->v = 1; } catch (Error $e) { echo $e->getMessage(), "\n"; }
        echo isset($this->none->v) ? 'S' : 's', isset($this->in->missing) ? 'S' : 's', "\n";
        try { $this->in->priv = 1; } catch (Error $e) { echo $e->getMessage(), "\n"; }
        $this->in->dyn = 'dynamic';
        echo $this->in->dyn, "\n";
        $this->in = new Sub;
        for ($i = 0; $i < 2; $i++) { $this->in->v++; echo $this->in->v, "\n"; }
        $this->self->num = 42;
        $this->self->num++;
        echo $this->num, "\n";
        $this->in->next = new Inner;
        $this->in->next->v = 'deep';
        echo $this->in->next->v, "\n";
        $x = 1;
        $this->in->v = &$x;
        $this->in->v = 7;
        $this->in->v++;
        echo $x, "\n";
        try { $this->ro = new Inner; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    }
}
$o = new Outer;
$o->run();
$m = new Outer;
for ($i = 0; $i < 2; $i++) { $m->in->v = "m$i"; $m->in->v .= '!'; echo $m->in->v, isset($m->in->v) ? '+' : '-', "\n"; }
class Magic { private $hidden; function __get($n) { echo "__get($n)\n"; return $this->inner ??= new Inner; } public $inner; }
$g = new Magic;
$g->hidden->v = 5;
"#;
        let expected = r#"11 a0 1 0 3 Se
12 a01 3 1 6 Se
13 a012 5 2 9 Se
Cannot assign string to property Inner::$n of type int
Cannot increment property Inner::$n of type int past its maximal value
Attempt to assign property "v" on null
Attempt to assign property "v" on int
ss
Cannot access private property Inner::$priv

Deprecated: Creation of dynamic property Inner::$dyn is deprecated in t.php on line 32
dynamic
101
102
43
deep
8
Cannot modify readonly property Outer::$ro
m0!+
m1!+
__get(hidden)
"#;
        assert_eq!(run(src), expected);
    }
}

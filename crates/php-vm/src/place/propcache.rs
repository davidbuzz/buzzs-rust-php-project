//! Per-place runtime cache of property lookups: php-src's
//! `CACHE_ADDR(opline->extended_value)` slot of `ZEND_FETCH_OBJ_R`/`_IS`,
//! `ZEND_ASSIGN_OBJ`, `ZEND_ASSIGN_OBJ_OP`, `ZEND_PRE/POST_INC/DEC_OBJ`
//! and `ZEND_ISSET_ISEMPTY_PROP_OBJ` (ref: `Zend/zend_vm_def.h`,
//! `Zend/zend_execute.c` `zend_fetch_property_address`,
//! `Zend/zend_object_handlers.c` `zend_get_property_offset`).
//!
//! A property place with a literal name on a CV or `$this` (`$o->p`,
//! `$this->p`) remembers what its last lookup found, keyed by the object's
//! class and the calling scope: a declared property's slot, or that the
//! name is a dynamic property (php-src's `ZEND_DYNAMIC_PROPERTY_OFFSET`).
//! Both key parts decide `zend_get_property_offset`'s answer; php-src's
//! runtime cache belongs to one op array in one scope (a closure rebound
//! to another scope gets its own), ours to the op array alone, so the
//! scope is part of the key. A hit reads or writes the value directly.
//!
//! Every case the cache does not cover takes the generic place walk
//! unchanged: lazy objects, undefined (uninitialized or unset) slots and
//! missing dynamic properties, values held by reference, hooked and
//! virtual properties, classes with native property handlers, readonly
//! and asymmetric-visibility properties for writes, and typed properties
//! when the value would need a coercion. An initialized, accessible
//! declared slot or an existing dynamic property is exactly where
//! `zend_std_read_property` & co. return without consulting `__get`,
//! `__set` or `__isset`, so a hit needs no magic-method check.

use crate::class::Class;
use crate::func::flags;
use crate::object;
use crate::opcode::{
    IncDecKind, OpArray, Operand, Place, PlaceBase, PlaceId, PropCacheEntry, Seg, PROP_CACHE_THIS,
};
use crate::props::PropLookup;
use crate::types::{mask, TypeDecl};
use crate::vm::{Frame, FrameKind, Vm, VmResult};
use php_runtime::{ops, ArrayKey, BinaryOp, Obj, PhpRef, Value, ZStr};
use std::rc::Rc;

/// The entry was filled (an unfilled entry never matches).
const PC_FILLED: u8 = 1 << 0;
/// Reads (`FETCH_OBJ_R`/`_IS`) may use the value.
const PC_READ: u8 = 1 << 1;
/// `isset()`/`empty()` may use the value.
const PC_ISSET: u8 = 1 << 2;
/// Writes may store the value directly (no readonly, set visibility or
/// native write handler); typed properties only values in `accept`.
const PC_WRITE: u8 = 1 << 3;
/// The name is a dynamic property (`ZEND_DYNAMIC_PROPERTY_OFFSET`): no
/// declared or static property of the class or scope has it.
const PC_DYN: u8 = 1 << 4;

/// The result of probing a place's cache entry.
enum Probe<T> {
    /// The fast path applied.
    Hit(T),
    /// The entry matches, but this access needs the generic walk.
    Slow,
    /// The entry is unfilled or for another class or scope.
    Miss,
}

/// Where a cached property's value lives.
enum Home {
    /// Declared slot.
    Slot(u32),
    /// Dynamic property of this name.
    Dyn(ZStr),
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

/// Whether `entry` was filled for class `cls` in scope `scope`.
#[inline]
fn key_matches(entry: &PropCacheEntry, cls: &Rc<Class>, scope: Option<&Rc<Class>>) -> bool {
    std::ptr::eq(entry.class.as_ptr(), Rc::as_ptr(cls))
        && match (&entry.scope, scope) {
            (None, None) => true,
            (Some(cached), Some(cur)) => std::ptr::eq(cached.as_ptr(), Rc::as_ptr(cur)),
            _ => false,
        }
}

/// Whether `value` may be stored as-is in a property accepting the
/// builtin type bits `accept` (`u32::MAX`: untyped): the side-effect-free
/// subset of `zend_verify_property_type` that needs no coercion, class or
/// callable check.
#[inline]
fn accepts(accept: u32, value: &Value) -> bool {
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

/// Whether `value` may be stored as-is in the property of `entry`: its
/// builtin type bits accept it ([`accepts`]), or it is an instance of the
/// one class the type names (`class_matches` in `check_type`).
#[inline]
fn entry_accepts(entry: &PropCacheEntry, value: &Value) -> bool {
    if accepts(entry.accept, value) {
        return true;
    }
    match (value, &entry.class_lc) {
        (Value::Object(obj), Some(lc)) => object::class_of(obj).instance_of_name(lc.as_bytes()),
        _ => false,
    }
}

/// Runs `access` on the object the base of a cached place holds (a CV,
/// also through a reference, or `$this`), without cloning the handle;
/// `None` when it holds no object.
#[inline]
fn with_base_obj<R>(frame: &Frame, base: u32, access: impl FnOnce(&Obj) -> R) -> Option<R> {
    if base == PROP_CACHE_THIS {
        return frame.this.as_ref().map(access);
    }
    let vars = frame.vars.borrow();
    let out = match vars.slots.get(frame.cv_idx(base)) {
        Some(Some(Value::Object(obj))) => Some(access(obj)),
        // A reference being modified elsewhere (`descend`'s "Cannot modify
        // a value while it is being iterated") is left to the walk.
        Some(Some(Value::Reference(rf))) => match rf.try_borrow() {
            Some(inner) => match &*inner {
                Value::Object(obj) => Some(access(obj)),
                _ => None,
            },
            None => None,
        },
        _ => None,
    };
    drop(vars);
    out
}

/// The home of `entry`'s property (the dynamic name from `code`).
#[inline]
fn home_of(entry: &PropCacheEntry, code: &OpArray) -> Option<Home> {
    if entry.flags & PC_DYN == 0 {
        return Some(Home::Slot(entry.slot));
    }
    match code.literals.get(entry.lit as usize) {
        Some(Value::Str(name)) => Some(Home::Dyn(name.clone())),
        _ => None,
    }
}

/// Runs `access` on the current value of a property: an initialized
/// declared slot or an existing dynamic property (`Slow` otherwise).
#[inline]
fn read_home<R>(obj: &Obj, home: &Home, access: impl FnOnce(&Value) -> Probe<R>) -> Probe<R> {
    let data = object::data(obj);
    match home {
        Home::Slot(slot) => {
            let slots = data.slots.borrow();
            let res = match slots.get(*slot as usize) {
                Some(Some(value)) => access(value),
                _ => Probe::Slow,
            };
            drop(slots);
            res
        }
        Home::Dyn(name) => {
            let table = data.dynamic.borrow();
            let res = match table
                .as_ref()
                .and_then(|t| t.get(&ArrayKey::Str(name.clone())))
            {
                Some(value) => access(value),
                None => Probe::Slow,
            };
            drop(table);
            res
        }
    }
}

/// [`read_home`] with the value mutable.
#[inline]
fn write_home<R>(obj: &Obj, home: &Home, access: impl FnOnce(&mut Value) -> Probe<R>) -> Probe<R> {
    let data = object::data(obj);
    match home {
        Home::Slot(slot) => {
            let mut slots = data.slots.borrow_mut();
            let res = match slots.get_mut(*slot as usize) {
                Some(Some(value)) => access(value),
                _ => Probe::Slow,
            };
            drop(slots);
            res
        }
        Home::Dyn(name) => {
            let mut table = data.dynamic.borrow_mut();
            let res = match table
                .as_mut()
                .and_then(|t| t.get_mut(&ArrayKey::Str(name.clone())))
            {
                Some(value) => access(value),
                None => Probe::Slow,
            };
            drop(table);
            res
        }
    }
}

/// Stores `value` over a property's current non-reference value; returns
/// the old value (to be dropped once all borrows are released), or `None`
/// when the property is undefined or holds a reference.
#[inline]
fn store_home(obj: &Obj, home: &Home, value: &Value) -> Option<Value> {
    let res = write_home(obj, home, |cur| {
        if matches!(cur, Value::Reference(_)) {
            Probe::Slow
        } else {
            Probe::Hit(std::mem::replace(cur, value.clone()))
        }
    });
    match res {
        Probe::Hit(old) => {
            if let Home::Slot(slot) = home {
                object::data(obj).set_slot_unset(*slot as usize, false);
            }
            Some(old)
        }
        Probe::Slow | Probe::Miss => None,
    }
}

/// The first write of a typed declared property that was never unset
/// (`zend_std_write_property` on an `IS_UNDEF` slot that is still
/// `IS_PROP_UNINIT`: no `__set()`, only the type check, which the caller's
/// `accepts` already passed): stores `value` into the empty slot. `false`
/// when the slot is not such a slot (untyped, defined, explicitly unset).
#[inline]
fn init_typed_slot(obj: &Obj, entry: &PropCacheEntry, home: &Home, value: &Value) -> bool {
    let Home::Slot(slot) = home else {
        return false;
    };
    let idx = *slot as usize;
    let data = object::data(obj);
    if entry.accept == u32::MAX || data.slot_unset(idx) {
        return false;
    }
    let mut slots = data.slots.borrow_mut();
    let stored = match slots.get_mut(idx) {
        Some(cell @ None) => {
            *cell = Some(value.clone());
            true
        }
        _ => false,
    };
    drop(slots);
    stored
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

impl Vm {
    /// Runs `access` on the matching cache entry of place `pid`, the
    /// object its base holds and the property's home. `need` are the
    /// `PC_*` bits the access requires; lazy objects always take the
    /// generic walk (`zend_lazy_object_init` runs there).
    #[inline]
    fn prop_probe<T>(
        &self,
        pid: PlaceId,
        need: u8,
        access: impl FnOnce(&PropCacheEntry, &Obj, Home) -> Probe<T>,
    ) -> Probe<T> {
        let frame = self.frame();
        if frame.kind == FrameKind::Builtin {
            return Probe::Slow;
        }
        let cache = frame.code.prop_cache.borrow();
        let Some(entry) = cache
            .get(pid as usize)
            .filter(|entry| entry.flags & PC_FILLED != 0)
        else {
            return Probe::Miss;
        };
        let scope = frame.scope.as_ref();
        let res = with_base_obj(frame, entry.base, |obj| {
            let data = object::data(obj);
            if !key_matches(entry, &data.class, scope) {
                return Probe::Miss;
            }
            if entry.flags & need != need || data.lazy.borrow().is_some() {
                return Probe::Slow;
            }
            match home_of(entry, &frame.code) {
                Some(home) => access(entry, obj, home),
                None => Probe::Slow,
            }
        });
        drop(cache);
        res.unwrap_or(Probe::Slow)
    }

    /// Fills the cache entry of property place `pid` for the object its
    /// base holds now (`zend_get_property_offset` storing
    /// `CACHE_POLYMORPHIC_PTR`). Returns whether an entry was stored.
    #[inline(never)]
    fn prop_cache_fill(&mut self, pid: PlaceId) -> bool {
        let frame = self.frame();
        if frame.kind == FrameKind::Builtin {
            return false;
        }
        let code = &frame.code;
        let place = &code.places[pid as usize];
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
        let scope = frame.scope.as_ref();
        let mut entry = PropCacheEntry {
            class: Rc::downgrade(&cls),
            scope: scope.map(Rc::downgrade),
            base,
            slot: 0,
            lit,
            accept: 0,
            class_lc: None,
            flags: PC_FILLED,
        };
        let hooks = cls.hooks.as_deref();
        let read_ok = hooks.is_none_or(|hk| hk.read_property.is_none());
        let isset_ok = hooks.is_none_or(|hk| hk.has_property.is_none());
        let write_ok = hooks.is_none_or(|hk| hk.write_property.is_none());
        match self.lookup_prop(&cls, name, scope) {
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
                    }
                }
            }
            PropLookup::Inaccessible(_) => {}
        }
        let len = code.places.len();
        let mut cache = code.prop_cache.borrow_mut();
        if cache.len() < len {
            cache.resize(len, PropCacheEntry::default());
        }
        cache[pid as usize] = entry;
        true
    }

    /// [`Vm::prop_probe`], filling the entry and probing again on a miss.
    #[inline]
    fn prop_probe_fill<T>(
        &mut self,
        pid: PlaceId,
        need: u8,
        access: impl Fn(&PropCacheEntry, &Obj, Home) -> Probe<T>,
    ) -> Option<T> {
        match self.prop_probe(pid, need, &access) {
            Probe::Hit(res) => Some(res),
            Probe::Slow => None,
            Probe::Miss => {
                if !self.prop_cache_fill(pid) {
                    return None;
                }
                match self.prop_probe(pid, need, &access) {
                    Probe::Hit(res) => Some(res),
                    Probe::Slow | Probe::Miss => None,
                }
            }
        }
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

    /// `ZEND_FETCH_OBJ_R`/`_IS` of the first segment of place `pid` (see
    /// [`cacheable_head`]): the (dereferenced) value of an initialized
    /// declared slot or an existing dynamic property, or `None` for the
    /// generic walk.
    #[inline]
    pub(crate) fn prop_cache_read(&mut self, pid: PlaceId) -> Option<Value> {
        self.prop_probe_fill(pid, PC_FILLED | PC_READ, |_, obj, home| {
            read_home(obj, &home, |value| Probe::Hit(value.deref()))
        })
    }

    /// `FETCH_OBJ_W` of the first segment of place `pid` for a write walk
    /// that goes on inside the property (`$this->a[$k] = v`): the object
    /// and the declared slot `property_container` would hand the walk
    /// (`PropContainer::Declared`), when the cache covers it: an
    /// initialized slot without reference, writable in place (no readonly
    /// or set visibility, no native handler), and for a typed property
    /// holding an array or object (nothing to auto-vivify, so no type
    /// check: `typed_prop_dim_write_check`).
    pub(crate) fn prop_cache_container(&mut self, pid: PlaceId) -> Option<(Obj, usize)> {
        self.prop_probe_fill(pid, PC_FILLED | PC_READ | PC_WRITE, |entry, obj, home| {
            let Home::Slot(slot) = &home else {
                return Probe::Slow;
            };
            let slot = *slot as usize;
            read_home(obj, &home, |value| {
                let ok = match value {
                    Value::Reference(_) => false,
                    Value::Array(_) | Value::Object(_) => true,
                    _ => entry.accept == u32::MAX,
                };
                if ok {
                    Probe::Hit((obj.clone(), slot))
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
    /// for the generic walk.
    pub(crate) fn prop_cache_make_ref(&mut self, pid: PlaceId) -> Option<PhpRef> {
        let hit = self.prop_probe_fill(pid, PC_FILLED | PC_READ | PC_WRITE, |_, obj, home| {
            let Home::Slot(slot) = &home else {
                return Probe::Slow;
            };
            let slot = *slot as usize;
            write_home(obj, &home, |value| {
                Probe::Hit((obj.clone(), slot, value.make_ref()))
            })
        });
        let (obj, slot, rf) = hit?;
        self.add_slot_type_source(&obj, slot, &rf);
        Some(rf)
    }

    /// `ZEND_ASSIGN_OBJ` of cacheable place `pid`: stores `v` over an
    /// initialized declared slot or an existing dynamic property, or into
    /// a typed slot not yet initialized (constructors), and returns the
    /// assigned value, or hands `v` back (`Err`) for the generic walk.
    #[inline]
    pub(crate) fn prop_cache_assign(&mut self, pid: PlaceId, v: Value) -> Result<Value, Value> {
        if matches!(v, Value::Reference(_)) {
            return Err(v);
        }
        let old = self.prop_probe_fill(pid, PC_FILLED | PC_WRITE, |entry, obj, home| {
            if !entry_accepts(entry, &v) {
                return Probe::Slow;
            }
            match store_home(obj, &home, &v) {
                Some(old) => Probe::Hit(Some(old)),
                None if init_typed_slot(obj, entry, &home, &v) => Probe::Hit(None),
                None => Probe::Slow,
            }
        });
        if let Some(old) = old {
            drop(old);
            Ok(v)
        } else {
            Err(v)
        }
    }

    /// `ZEND_PRE/POST_INC/DEC_OBJ` of cacheable place `pid` on a property
    /// holding an int (without overflow) or a float, updated in place: the
    /// result, or `None` for the generic walk.
    #[inline]
    pub(crate) fn prop_cache_incdec(&mut self, pid: PlaceId, kind: IncDecKind) -> Option<Value> {
        self.prop_probe_fill(pid, PC_FILLED | PC_READ | PC_WRITE, |_, obj, home| {
            write_home(obj, &home, |value| incdec_in_place(value, kind))
        })
    }

    /// `ZEND_ASSIGN_OBJ_OP` of cacheable place `pid` with a numeric
    /// right-hand side on a property holding a number: reads the value,
    /// operates and writes the result back like the generic walk
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
        let hit = self.prop_probe_fill(pid, PC_FILLED | PC_READ | PC_WRITE, |entry, obj, home| {
            let cur = read_home(obj, &home, |value| match value {
                Value::Int(_) | Value::Float(_) => Probe::Hit(value.clone()),
                _ => Probe::Slow,
            });
            match cur {
                Probe::Hit(cur) => Probe::Hit((obj.clone(), cur, home, entry.accept)),
                Probe::Slow | Probe::Miss => Probe::Slow,
            }
        });
        let Some((obj, cur, home, accept)) = hit else {
            return Ok(None);
        };
        let new = ops::binary_op(op, &cur, rhs, self).map_err(|e| self.rt_throw(e))?;
        // A diagnostic of the operation (a float-to-int deprecation) may
        // have run an error handler that changed the object: the direct
        // store needs the property as it was, anything else goes through
        // the property handlers.
        if accepts(accept, &new) && object::data(&obj).lazy.borrow().is_none() {
            if let Some(old) = store_home(&obj, &home, &new) {
                drop(old);
                return Ok(Some(new));
            }
        }
        let name = self.prop_cache_name(pid);
        self.write_property(&obj, &name, new).map(Some)
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
    pub(crate) fn prop_cache_concat(&mut self, pid: PlaceId, rhs: &Value) -> Option<Value> {
        if !matches!(rhs, Value::Str(_) | Value::Int(_)) {
            return None;
        }
        self.prop_probe_fill(pid, PC_FILLED | PC_READ | PC_WRITE, |e, o, h| {
            if e.accept != u32::MAX && e.accept & mask::STRING == 0 {
                return Probe::Slow;
            }
            write_home(o, &h, |v| match (v, rhs) {
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
            })
        })
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
        let value = self.prop_probe_fill(pid, PC_FILLED | PC_ISSET, |_, obj, home| {
            read_home(obj, &home, |value| Probe::Hit(value.deref()))
        });
        let Some(value) = value else {
            return Ok(None);
        };
        Ok(Some(if empty {
            !self.to_bool(&value)?
        } else {
            !value.is_null()
        }))
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
}

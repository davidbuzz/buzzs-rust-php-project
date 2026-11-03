//! `WeakReference`, `WeakMap` and `InternalIterator`
//! (`Zend/zend_weakrefs.c`, `Zend/zend_interfaces.c`).
//!
//! Objects are `Rc`-counted, so a weak reference is a pair of `Weak`
//! pointers: one to the object's [`ObjectData`] (alive until the object is
//! really freed, including while its destructor runs) and one to the
//! outer `Rc<dyn PhpObject>` handle (so that [`WeakReference::get()`]
//! returns the *same* handle and `===` holds).
//!
//! `WeakMap` entries must disappear the moment their key object is freed
//! (and the value must be released right then, which can run destructors).
//! For that the engine calls [`object_freed`] from `ObjectData::drop` — the
//! equivalent of `zend_weakrefs_notify()` in `zend_object_std_dtor()`. A
//! thread-local registry maps object handles to the maps holding them, so
//! a freed handle is purged from every map before the handle can be
//! reused by a new object (`spl_object_id` reuse never resurrects an
//! entry).

// Handlers have the signatures fixed by `ClassHooks` / `BuiltinFn`, so some
// return `Ok` unconditionally.
#![allow(clippy::unnecessary_wraps)]

use crate::builtins::classbuilder::ClassBuilder;
use crate::class::cflags;
use crate::ext::{ClassHooks, NativeIterator};
use crate::func::{flags, Args};
use crate::object::{self, ObjBox, ObjectData};
use crate::vm::{Vm, VmResult};
use php_runtime::{ArrayKey, HashTable, Obj, PhpObject, PhpRef, PropertyPurpose, Value, ZArray};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

const PUB: u32 = flags::PUBLIC;

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// A `WeakReference` singleton registered for a referent.
struct RefSlot {
    /// The referent (to detect a reused handle).
    referent: Weak<ObjectData>,
    /// The `WeakReference` object.
    weakref: Weak<dyn PhpObject>,
}

thread_local! {
    /// Set once any weak reference or map entry exists (fast path for
    /// [`object_freed`]).
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    /// Referent handle → its `WeakReference` singleton (`EG(weakrefs)`,
    /// `ZEND_WEAKREF_TAG_REF`).
    static REFS: RefCell<HashMap<u32, RefSlot>> = RefCell::new(HashMap::new());
    /// Key handle → maps that hold an entry for it
    /// (`ZEND_WEAKREF_TAG_MAP`).
    static MAPS: RefCell<HashMap<u32, Vec<Weak<MapCell>>>> = RefCell::new(HashMap::new());
}

/// Called by the engine when an object is freed (`zend_weakrefs_notify`):
/// drops the `WeakMap` entries keyed by it (releasing their values) and its
/// `WeakReference` registration. Must run before the handle is recycled.
pub(crate) fn object_freed(handle: u32) {
    if !ACTIVE.try_with(Cell::get).unwrap_or(false) {
        return;
    }
    let _ = REFS.try_with(|r| {
        if let Ok(mut r) = r.try_borrow_mut() {
            if r.get(&handle)
                .is_some_and(|s| s.referent.upgrade().is_none())
            {
                r.remove(&handle);
            }
        }
    });
    let maps = MAPS
        .try_with(|m| m.try_borrow_mut().ok().and_then(|mut m| m.remove(&handle)))
        .ok()
        .flatten()
        .unwrap_or_default();
    // Values are released only after every borrow is gone: dropping them
    // may free further objects and re-enter this function.
    let mut garbage: Vec<Entry> = Vec::new();
    let mut cells: Vec<Rc<MapCell>> = Vec::new();
    for w in maps {
        if let Some(cell) = w.upgrade() {
            if let Ok(mut st) = cell.try_borrow_mut() {
                if let Some(e) = st.remove(handle) {
                    garbage.push(e);
                }
            }
            cells.push(cell);
        }
    }
    drop(garbage);
    drop(cells);
}

fn activate() {
    ACTIVE.with(|a| a.set(true));
}

/// Records that `map` holds an entry keyed by `handle`.
fn register_map_key(handle: u32, map: &Rc<MapCell>) {
    activate();
    MAPS.with(|m| {
        let mut m = m.borrow_mut();
        let list = m.entry(handle).or_default();
        list.retain(|w| w.strong_count() > 0);
        if !list
            .iter()
            .any(|w| std::ptr::eq(w.as_ptr(), Rc::as_ptr(map)))
        {
            list.push(Rc::downgrade(map));
        }
    });
}

// ---------------------------------------------------------------------------
// Weak object handles
// ---------------------------------------------------------------------------

/// A weak handle to a PHP object.
#[derive(Clone)]
struct WeakObj {
    data: Weak<ObjectData>,
    outer: Weak<dyn PhpObject>,
}

impl WeakObj {
    fn new(o: &Obj) -> Self {
        let data = o
            .downcast_ref::<ObjBox>()
            .map_or_else(Weak::new, |b| Rc::downgrade(&b.0));
        WeakObj {
            data,
            outer: Rc::downgrade(o.rc()),
        }
    }

    /// The object, if it has not been freed. While its destructor runs the
    /// original handle is gone but the object is still alive (and already
    /// marked destructed), so a fresh handle is safe to hand out.
    fn get(&self) -> Option<Obj> {
        if let Some(rc) = self.outer.upgrade() {
            return Some(Obj::from_rc(rc));
        }
        self.data.upgrade().map(object::revive)
    }

    /// Whether this denotes `o`.
    fn is(&self, o: &Obj) -> bool {
        o.downcast_ref::<ObjBox>()
            .is_some_and(|b| std::ptr::eq(self.data.as_ptr(), Rc::as_ptr(&b.0)))
    }
}

// ---------------------------------------------------------------------------
// WeakReference
// ---------------------------------------------------------------------------

/// Internal state of a `WeakReference` object.
struct WeakRefData {
    referent: WeakObj,
}

fn weakref_construct(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.make_throw(
        "Error",
        "Direct instantiation of WeakReference is not allowed, use WeakReference::create instead",
    ))
}

/// `WeakReference::create()`: one `WeakReference` per live referent.
fn weakref_create(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Value::Object(target) = a.at(0).deref() else {
        return Ok(Value::Null);
    };
    let handle = target.handle();
    let existing = REFS.with(|r| {
        r.borrow().get(&handle).and_then(|s| {
            let same = std::ptr::eq(
                s.referent.as_ptr(),
                target
                    .downcast_ref::<ObjBox>()
                    .map_or(std::ptr::null(), |b| Rc::as_ptr(&b.0)),
            );
            if same {
                s.weakref.upgrade()
            } else {
                None
            }
        })
    });
    if let Some(wr) = existing {
        return Ok(Value::Object(Obj::from_rc(wr)));
    }
    let Some(cls) = vm.find_class(b"WeakReference") else {
        return Ok(Value::Null);
    };
    let wr = vm.new_object(&cls)?;
    let referent = WeakObj::new(&target);
    let slot = RefSlot {
        referent: referent.data.clone(),
        weakref: Rc::downgrade(wr.rc()),
    };
    object::set_internal(&wr, WeakRefData { referent });
    activate();
    // The replaced slot (a stale registration) is dropped outside the borrow.
    let old = REFS.with(|r| r.borrow_mut().insert(handle, slot));
    drop(old);
    Ok(Value::Object(wr))
}

fn weakref_target(o: &Obj) -> Option<Obj> {
    object::internal::<WeakRefData, _>(o, |d| d.referent.clone()).and_then(|w| w.get())
}

fn weakref_get(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(weakref_target(&a.this_obj()).map_or(Value::Null, Value::Object))
}

/// `get_debug_info`: `["object" => referent|null]`.
fn weakref_props(_vm: &mut Vm, o: &Obj, purpose: PropertyPurpose) -> VmResult<Option<ZArray>> {
    if purpose != PropertyPurpose::Debug {
        return Ok(None);
    }
    let mut t = HashTable::new();
    t.insert(
        ArrayKey::from("object"),
        weakref_target(o).map_or(Value::Null, Value::Object),
    );
    Ok(Some(ZArray::from_table(t)))
}

// ---------------------------------------------------------------------------
// WeakMap storage
// ---------------------------------------------------------------------------

/// One `WeakMap` entry.
struct Entry {
    handle: u32,
    key: WeakObj,
    /// The value; a `Value::Reference` when bound by reference (or
    /// temporarily, see [`MapState::transient`]).
    value: Value,
    /// `value` is a reference created only to let a dimension write
    /// (`$map[$k][] = 1`, `$map[$k]++`) reach the slot; it is unwrapped again
    /// unless something else kept hold of it.
    transient: bool,
}

/// Insertion-ordered entries keyed by object handle. Removed entries leave
/// holes so that iterator positions stay valid; holes are compacted when no
/// iterator is active.
#[derive(Default)]
struct MapState {
    entries: Vec<Option<Entry>>,
    index: HashMap<u32, usize>,
    /// Active iterators (positions must not move).
    iterators: usize,
    /// Position of an entry holding a transient reference.
    transient: Option<usize>,
}

type MapCell = RefCell<MapState>;

impl MapState {
    fn len(&self) -> usize {
        self.index.len()
    }

    fn find(&self, o: &Obj) -> Option<usize> {
        let i = *self.index.get(&o.handle())?;
        self.entries[i].as_ref().filter(|e| e.key.is(o)).map(|_| i)
    }

    /// Turns a no-longer-shared transient reference back into a plain
    /// value (a still-shared one has become a real PHP reference).
    fn settle(&mut self) {
        let Some(i) = self.transient.take() else {
            return;
        };
        let Some(Some(e)) = self.entries.get_mut(i) else {
            return;
        };
        if !e.transient {
            return;
        }
        e.transient = false;
        if let Value::Reference(r) = &e.value {
            if r.refcount() == 1 {
                let v = r.get();
                e.value = v;
            }
        }
    }

    fn remove(&mut self, handle: u32) -> Option<Entry> {
        let i = self.index.remove(&handle)?;
        if self.transient == Some(i) {
            self.transient = None;
        }
        let e = self.entries.get_mut(i).and_then(Option::take);
        self.maybe_compact();
        e
    }

    fn maybe_compact(&mut self) {
        if self.iterators > 0 || self.transient.is_some() {
            return;
        }
        if self.index.is_empty() {
            self.entries.clear();
            return;
        }
        if self.entries.len() < 16 || self.index.len() * 2 > self.entries.len() {
            return;
        }
        self.entries.retain(Option::is_some);
        self.index.clear();
        for (i, e) in self.entries.iter().enumerate() {
            if let Some(e) = e {
                self.index.insert(e.handle, i);
            }
        }
    }

    /// Inserts or replaces; returns the replaced value.
    fn set(&mut self, key: &Obj, value: Value) -> (Option<Value>, bool) {
        if let Some(i) = self.find(key) {
            if let Some(e) = self.entries[i].as_mut() {
                e.transient = false;
                if self.transient == Some(i) {
                    self.transient = None;
                }
                return (Some(std::mem::replace(&mut e.value, value)), false);
            }
        }
        let handle = key.handle();
        self.index.insert(handle, self.entries.len());
        self.entries.push(Some(Entry {
            handle,
            key: WeakObj::new(key),
            value,
            transient: false,
        }));
        (None, true)
    }

    /// The next live position at or after `pos`.
    fn live_from(&self, mut pos: usize) -> Option<usize> {
        while pos < self.entries.len() {
            if self.entries[pos].is_some() {
                return Some(pos);
            }
            pos += 1;
        }
        None
    }
}

/// `zend_weakmap_get_gc`: every value is kept alive only while both the
/// map and its key are (a conditional edge). Storage shared with an active
/// iterator is not reported (the iterator co-owns it).
impl crate::gc::GcTraverse for Rc<MapCell> {
    fn gc_traverse(&self, v: &mut crate::gc::GcVisitor<'_>) {
        if Rc::strong_count(self) != 1 {
            return;
        }
        let Ok(st) = self.try_borrow() else {
            v.opaque();
            return;
        };
        for e in st.entries.iter().flatten() {
            v.weak_entry(&e.key.data, &e.value);
        }
    }
}

fn map_of(o: &Obj) -> Option<Rc<MapCell>> {
    object::internal::<Rc<MapCell>, _>(o, |m| m.clone())
}

fn map_state(vm: &mut Vm, o: &Obj) -> VmResult<Rc<MapCell>> {
    map_of(o).ok_or_else(|| vm.make_throw("Error", "Object not initialized"))
}

/// The key operand as an object (`WeakMap key must be an object`).
fn key_object(vm: &mut Vm, key: &Value) -> VmResult<Obj> {
    match key.deref() {
        Value::Object(o) => Ok(o),
        _ => Err(vm.make_throw("TypeError", "WeakMap key must be an object")),
    }
}

fn not_contained(vm: &mut Vm, key: &Obj) -> crate::vm::Throw {
    let name = object::display_name(&object::class_of(key).name);
    vm.make_throw(
        "Error",
        &format!(
            "Object {}#{} not contained in WeakMap",
            name.to_string_lossy(),
            key.handle()
        ),
    )
}

// ---------------------------------------------------------------------------
// WeakMap handlers
// ---------------------------------------------------------------------------

fn map_create(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    object::set_internal(o, Rc::new(MapCell::default()));
    Ok(())
}

/// `read_dimension`. The engine uses this hook both for reads and as the
/// target of nested writes, so the slot is handed out as a reference.
fn map_read_dimension(vm: &mut Vm, o: &Obj, key: &Value) -> VmResult<Option<Value>> {
    let key = key_object(vm, key)?;
    let map = map_state(vm, o)?;
    let mut st = map.borrow_mut();
    st.settle();
    let Some(i) = st.find(&key) else {
        drop(st);
        return Err(not_contained(vm, &key));
    };
    let Some(e) = st.entries[i].as_mut() else {
        return Ok(Some(Value::Null));
    };
    if let Value::Reference(r) = &e.value {
        return Ok(Some(Value::Reference(r.clone())));
    }
    let r = PhpRef::new(std::mem::take(&mut e.value));
    e.value = Value::Reference(r.clone());
    e.transient = true;
    st.transient = Some(i);
    Ok(Some(Value::Reference(r)))
}

/// `write_dimension`; a `Value::Reference` binds the entry by reference
/// (`$map[$k] =& $v`).
fn map_write_dimension(vm: &mut Vm, o: &Obj, key: Option<&Value>, value: &Value) -> VmResult<bool> {
    let Some(key) = key else {
        return Err(vm.make_throw("Error", "Cannot append to WeakMap"));
    };
    let key = key_object(vm, key)?;
    let map = map_state(vm, o)?;
    let (old, inserted) = {
        let mut st = map.borrow_mut();
        st.settle();
        st.set(&key, value.clone())
    };
    if inserted {
        register_map_key(key.handle(), &map);
    }
    // Released after the borrow: the old value's destructor may touch the map.
    drop(old);
    Ok(true)
}

/// `has_dimension`: `isset()` (not null) or, with `check_empty`, truthiness.
fn map_has_dimension(
    vm: &mut Vm,
    o: &Obj,
    key: &Value,
    check_empty: bool,
) -> VmResult<Option<bool>> {
    let key = key_object(vm, key)?;
    let map = map_state(vm, o)?;
    let v = {
        let mut st = map.borrow_mut();
        st.settle();
        st.find(&key)
            .and_then(|i| st.entries[i].as_ref().map(|e| e.value.deref()))
    };
    let Some(v) = v else { return Ok(Some(false)) };
    if check_empty {
        return Ok(Some(vm.value_to_bool(&v)?));
    }
    Ok(Some(!v.is_null()))
}

fn map_unset_dimension(vm: &mut Vm, o: &Obj, key: &Value) -> VmResult<bool> {
    let key = key_object(vm, key)?;
    let map = map_state(vm, o)?;
    let old = {
        let mut st = map.borrow_mut();
        st.settle();
        if st.find(&key).is_some() {
            st.remove(key.handle())
        } else {
            None
        }
    };
    drop(old);
    Ok(true)
}

fn map_count(_vm: &mut Vm, o: &Obj) -> VmResult<Option<i64>> {
    Ok(Some(map_of(o).map_or(0, |m| {
        i64::try_from(m.borrow().len()).unwrap_or(i64::MAX)
    })))
}

/// `get_properties_for`: only `var_dump`/`print_r` see the entries, as
/// `[["key" => k, "value" => v], ...]`.
fn map_props(_vm: &mut Vm, o: &Obj, purpose: PropertyPurpose) -> VmResult<Option<ZArray>> {
    if purpose != PropertyPurpose::Debug {
        return Ok(None);
    }
    let Some(map) = map_of(o) else {
        return Ok(None);
    };
    let mut st = map.borrow_mut();
    st.settle();
    let mut out = HashTable::new();
    for e in st.entries.iter().flatten() {
        let Some(k) = e.key.get() else { continue };
        let mut pair = HashTable::new();
        pair.insert(ArrayKey::from("key"), Value::Object(k));
        pair.insert(ArrayKey::from("value"), e.value.clone());
        let _ = out.push(Value::Array(ZArray::from_table(pair)));
    }
    Ok(Some(ZArray::from_table(out)))
}

/// `clone_obj`: copies the entries (references stay shared).
fn map_clone(_vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    let fresh = Rc::new(MapCell::default());
    let mut handles = Vec::new();
    if let Some(src) = map_of(old) {
        let mut st = src.borrow_mut();
        st.settle();
        let mut dst = fresh.borrow_mut();
        for e in st.entries.iter().flatten() {
            let pos = dst.entries.len();
            dst.index.insert(e.handle, pos);
            dst.entries.push(Some(Entry {
                handle: e.handle,
                key: e.key.clone(),
                value: e.value.clone(),
                transient: false,
            }));
            handles.push(e.handle);
        }
    }
    for h in handles {
        register_map_key(h, &fresh);
    }
    object::set_internal(new, fresh);
    Ok(())
}

/// Iterator over a `WeakMap` (`zend_weakmap_iterator`): keys are the key
/// objects; entries removed during iteration are skipped.
struct MapIter {
    /// Keeps the map alive while iterating.
    _owner: Obj,
    map: Rc<MapCell>,
    pos: usize,
    by_ref: bool,
}

impl MapIter {
    fn new(owner: Obj, map: Rc<MapCell>, by_ref: bool) -> Self {
        map.borrow_mut().iterators += 1;
        MapIter {
            _owner: owner,
            map,
            pos: 0,
            by_ref,
        }
    }

    /// Moves `pos` to the next live entry; returns whether there is one.
    fn seek(&mut self) -> bool {
        let st = self.map.borrow();
        let next = st.live_from(self.pos);
        self.pos = next.unwrap_or(st.entries.len());
        next.is_some()
    }
}

impl Drop for MapIter {
    fn drop(&mut self) {
        if let Ok(mut st) = self.map.try_borrow_mut() {
            st.iterators = st.iterators.saturating_sub(1);
        }
    }
}

impl NativeIterator for MapIter {
    fn rewind(&mut self, _vm: &mut Vm) -> VmResult<()> {
        self.pos = 0;
        Ok(())
    }

    fn valid(&mut self, _vm: &mut Vm) -> VmResult<bool> {
        Ok(self.seek())
    }

    fn current(&mut self, _vm: &mut Vm) -> VmResult<Value> {
        if !self.seek() {
            return Ok(Value::Null);
        }
        let mut st = self.map.borrow_mut();
        st.settle();
        let Some(e) = st.entries[self.pos].as_mut() else {
            return Ok(Value::Null);
        };
        if !self.by_ref {
            return Ok(e.value.deref());
        }
        if !matches!(e.value, Value::Reference(_)) {
            e.value = Value::Reference(PhpRef::new(std::mem::take(&mut e.value)));
        }
        let Value::Reference(r) = &e.value else {
            return Ok(Value::Null);
        };
        let r = r.clone();
        e.transient = false;
        Ok(Value::Reference(r))
    }

    fn key(&mut self, _vm: &mut Vm) -> VmResult<Value> {
        if !self.seek() {
            return Ok(Value::Null);
        }
        let st = self.map.borrow();
        Ok(st.entries[self.pos]
            .as_ref()
            .and_then(|e| e.key.get())
            .map_or(Value::Null, Value::Object))
    }

    fn next(&mut self, _vm: &mut Vm) -> VmResult<()> {
        if self.seek() {
            self.pos += 1;
        }
        Ok(())
    }
}

fn map_get_iterator(vm: &mut Vm, o: &Obj, by_ref: bool) -> VmResult<Box<dyn NativeIterator>> {
    let map = map_state(vm, o)?;
    Ok(Box::new(MapIter::new(o.clone(), map, by_ref)))
}

// WeakMap methods (the explicit `ArrayAccess`/`Countable` API).

fn map_offset_get(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let key = key_object(vm, &a.at(0))?;
    let map = map_state(vm, &o)?;
    let v = {
        let mut st = map.borrow_mut();
        st.settle();
        st.find(&key)
            .and_then(|i| st.entries[i].as_ref().map(|e| e.value.deref()))
    };
    match v {
        Some(v) => Ok(v),
        None => Err(not_contained(vm, &key)),
    }
}

fn map_offset_set(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let key = a.at(0);
    let value = a.at(1).deref();
    map_write_dimension(vm, &o, Some(&key), &value)?;
    Ok(Value::Null)
}

fn map_offset_exists(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let r = map_has_dimension(vm, &o, &a.at(0), false)?;
    Ok(Value::Bool(r.unwrap_or(false)))
}

fn map_offset_unset(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    map_unset_dimension(vm, &o, &a.at(0))?;
    Ok(Value::Null)
}

fn map_count_method(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    Ok(Value::Int(map_count(vm, &o)?.unwrap_or(0)))
}

fn map_get_iterator_method(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let it = map_get_iterator(vm, &o, false)?;
    new_internal_iterator(vm, it).map(Value::Object)
}

// ---------------------------------------------------------------------------
// InternalIterator
// ---------------------------------------------------------------------------

/// State of an `InternalIterator` (`zend_internal_iterator`).
struct InternalIter {
    iter: Option<Box<dyn NativeIterator>>,
    rewound: bool,
}

/// Wraps a native iterator in an `InternalIterator` object
/// (`zend_create_internal_iterator_zval`), as returned by `getIterator()`
/// of internal `IteratorAggregate` classes.
pub(crate) fn new_internal_iterator(vm: &mut Vm, it: Box<dyn NativeIterator>) -> VmResult<Obj> {
    let Some(cls) = vm.find_class(b"InternalIterator") else {
        return Err(vm.make_throw("Error", "Class \"InternalIterator\" not found"));
    };
    let o = vm.new_object(&cls)?;
    object::set_internal(
        &o,
        InternalIter {
            iter: Some(it),
            rewound: false,
        },
    );
    Ok(o)
}

/// Runs `f` on the wrapped iterator (rewinding it first if needed).
fn with_internal_iter<R>(
    vm: &mut Vm,
    args: &Args,
    op: impl FnOnce(&mut Vm, &mut dyn NativeIterator) -> VmResult<R>,
) -> VmResult<R> {
    let this = args.this_obj();
    let taken = object::internal::<InternalIter, _>(&this, |s| (s.iter.take(), s.rewound))
        .unwrap_or((None, false));
    let (Some(mut it), rewound) = taken else {
        return Err(vm.make_throw(
            "Error",
            "The InternalIterator object has not been properly initialized",
        ));
    };
    let rewind = if rewound { Ok(()) } else { it.rewind(vm) };
    let out = rewind.and_then(|()| op(vm, it.as_mut()));
    object::internal::<InternalIter, _>(&this, |s| {
        s.iter = Some(it);
        s.rewound = true;
    });
    out
}

fn ii_construct(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.make_throw("Error", "Cannot manually construct InternalIterator"))
}

fn ii_current(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    with_internal_iter(vm, a, |vm, it| {
        if it.valid(vm)? {
            Ok(it.current(vm)?.deref())
        } else {
            Ok(Value::Null)
        }
    })
}

fn ii_key(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    // `InternalIterator::key()` asks the iterator without a validity
    // check (`get_current_key`): past the end it reports what the native
    // iterator reports.
    with_internal_iter(vm, a, |vm, it| it.key(vm))
}

fn ii_next(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    with_internal_iter(vm, a, |vm, it| it.next(vm)).map(|()| Value::Null)
}

fn ii_valid(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    with_internal_iter(vm, a, |vm, it| it.valid(vm)).map(Value::Bool)
}

fn ii_rewind(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    // An explicit rewind replaces the implicit first one.
    object::internal::<InternalIter, _>(&o, |s| s.rewound = true);
    with_internal_iter(vm, a, |vm, it| it.rewind(vm)).map(|()| Value::Null)
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Registers `WeakReference`, `WeakMap` and (unless already present)
/// `InternalIterator`.
pub(crate) fn register(vm: &mut Vm) {
    crate::gc::register_traverse::<Rc<MapCell>>();
    if vm.find_class(b"InternalIterator").is_none() {
        ClassBuilder::new("InternalIterator")
            .flags(cflags::FINAL | cflags::NO_CLONE)
            .implements(vm, &["Iterator"])
            .method("__construct()", flags::PRIVATE, ii_construct)
            .method("current(): mixed", PUB, ii_current)
            .method("key(): mixed", PUB, ii_key)
            .method("next(): void", PUB, ii_next)
            .method("valid(): bool", PUB, ii_valid)
            .method("rewind(): void", PUB, ii_rewind)
            .register(vm);
    }
    ClassBuilder::new("WeakReference")
        .flags(
            cflags::FINAL
                | cflags::NOT_SERIALIZABLE
                | cflags::NO_CLONE
                | cflags::NO_DYNAMIC_PROPERTIES,
        )
        .method("__construct()", PUB, weakref_construct)
        .method(
            "create(object $object): WeakReference",
            PUB | flags::STATIC,
            weakref_create,
        )
        .method("get(): ?object", PUB, weakref_get)
        .hooks(ClassHooks {
            properties_for: Some(weakref_props),
            ..ClassHooks::default()
        })
        .register(vm);
    ClassBuilder::new("WeakMap")
        .flags(cflags::FINAL | cflags::NOT_SERIALIZABLE | cflags::NO_DYNAMIC_PROPERTIES)
        .implements(vm, &["ArrayAccess", "Countable", "IteratorAggregate"])
        .method("offsetGet($object): mixed", PUB, map_offset_get)
        .method(
            "offsetSet($object, mixed $value): void",
            PUB,
            map_offset_set,
        )
        .method("offsetExists($object): bool", PUB, map_offset_exists)
        .method("offsetUnset($object): void", PUB, map_offset_unset)
        .method("count(): int", PUB, map_count_method)
        .method("getIterator(): Iterator", PUB, map_get_iterator_method)
        .hooks(ClassHooks {
            create: Some(map_create),
            clone: Some(map_clone),
            properties_for: Some(map_props),
            count: Some(map_count),
            read_dimension: Some(map_read_dimension),
            write_dimension: Some(map_write_dimension),
            has_dimension: Some(map_has_dimension),
            unset_dimension: Some(map_unset_dimension),
            get_iterator: Some(map_get_iterator),
            ..ClassHooks::default()
        })
        .register(vm);
}

#[cfg(test)]
mod tests {
    use crate::{Vm, VmOptions};
    use std::cell::RefCell;
    use std::io::Write;
    use std::rc::Rc;

    /// A `Write` sink shared with the test.
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
        drop(vm);
        let out = sink.0.borrow().clone();
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn weakreference_identity_and_release() {
        let out = run(
            "<?php $o = new stdClass; $a = WeakReference::create($o); $b = WeakReference::create($o);\n\
             var_dump($a === $b, $a->get() === $o); unset($o); var_dump($a->get());",
        );
        assert_eq!(out, "bool(true)\nbool(true)\nNULL\n");
    }

    #[test]
    fn weakreference_not_resurrected_by_handle_reuse() {
        let out = run(
            "<?php $o = new stdClass; $id = spl_object_id($o); $w = WeakReference::create($o); unset($o);\n\
             $p = new stdClass; var_dump(spl_object_id($p) === $id, $w->get(), WeakReference::create($p) === $w);",
        );
        assert_eq!(out, "bool(true)\nNULL\nbool(false)\n");
    }

    #[test]
    fn weakreference_rejects_new_and_clone() {
        let out = run(
            "<?php try { new WeakReference; } catch (Error $e) { echo $e->getMessage(), \"\\n\"; }\n\
             try { clone WeakReference::create(new stdClass); } catch (Error $e) { echo $e->getMessage(), \"\\n\"; }",
        );
        assert_eq!(
            out,
            "Direct instantiation of WeakReference is not allowed, use WeakReference::create instead\n\
             Trying to clone an uncloneable object of class WeakReference\n"
        );
    }

    #[test]
    fn weakmap_entries_follow_key_lifetime() {
        let out = run(
            "<?php class D { function __destruct() { echo \"dtor\\n\"; } }\n\
             $m = new WeakMap; $a = new stdClass; $b = new stdClass;\n\
             $m[$a] = 1; $m[$b] = new D; echo count($m), \"\\n\";\n\
             unset($b); echo count($m), \"\\n\";\n\
             $c = new stdClass; var_dump(isset($m[$c]));",
        );
        assert_eq!(out, "2\ndtor\n1\nbool(false)\n");
    }

    #[test]
    fn weakmap_iteration_and_nested_writes() {
        let out = run(
            "<?php $m = new WeakMap; $k = [new stdClass, new stdClass, new stdClass];\n\
             foreach ($k as $i => $o) { $m[$o] = [$i]; }\n\
             $m[$k[1]][] = 9; unset($k[0]);\n\
             foreach ($m as $o => $v) { echo spl_object_id($o), ':', implode(',', $v), ' '; }",
        );
        assert_eq!(out, "3:1,9 4:2 ");
    }

    #[test]
    fn weakmap_errors() {
        let out = run("<?php $m = new WeakMap;\n\
             try { $m[1] = 1; } catch (TypeError $e) { echo $e->getMessage(), \"\\n\"; }\n\
             try { $m[] = 1; } catch (Error $e) { echo $e->getMessage(), \"\\n\"; }\n\
             try { $m[new stdClass]; } catch (Error $e) { echo $e->getMessage(), \"\\n\"; }");
        assert_eq!(
            out,
            "WeakMap key must be an object\nCannot append to WeakMap\nObject stdClass#2 not contained in WeakMap\n"
        );
    }

    #[test]
    fn weakmap_clone_and_debug_dump() {
        let out = run(
            "<?php $m = new WeakMap; $o = new stdClass; $m[$o] = 1; $c = clone $m; unset($o);\n\
             var_dump(count($m), count($c)); $p = new stdClass; $c[$p] = true; var_dump($c);",
        );
        assert_eq!(
            out,
            "int(0)\nint(0)\nobject(WeakMap)#3 (1) {\n  [0]=>\n  array(2) {\n    [\"key\"]=>\n    \
             object(stdClass)#2 (0) {\n    }\n    [\"value\"]=>\n    bool(true)\n  }\n}\n"
        );
    }
}

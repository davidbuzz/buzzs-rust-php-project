//! Objects (`zend_object`): property slots, dynamic properties, handles and
//! destructor scheduling.
//!
//! An [`Obj`] from `php-runtime` is a thin handle to an `Rc<dyn PhpObject>`
//! allocation; ours holds an [`ObjBox`] wrapping an `Rc<ObjectData>`. When
//! the last `Obj` handle is dropped, `ObjBox::drop` runs: if the class has
//! a destructor that has not been called yet, a fresh `Obj` is created
//! around the same data and queued so that the VM calls `__destruct` at the
//! next safe point — this gives PHP's refcount-driven destructor timing
//! without access to the VM from `Drop`.
//!
//! The object store (`EG(objects_store)`, ref: Zend/zend_objects_API.c)
//! holds a weak handle to the current outer `Obj` of every object, so a
//! lookup by handle (shutdown destructors, `WeakReference::get()`) yields
//! the same `Obj` — the same identity for `===` and recursion guards, and
//! the same refcount — as every other holder, as php-src's one
//! `zend_object *` per handle does.

use crate::class::Class;
use php_runtime::{ArrayKey, HashTable, Obj, ObjWeak, PhpObject, Value, ZStr};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

/// Object flags.
pub mod oflags {
    /// `__destruct` has been called (or must not be called).
    pub const DESTRUCTED: u32 = 1 << 0;
    /// Readonly properties may be modified (during clone `__clone`).
    pub const CLONING: u32 = 1 << 1;
    /// Object is an enum case.
    pub const ENUM_CASE: u32 = 1 << 2;
    /// The object has its own property order (see [`super::PropOrder`]).
    pub const PROP_ORDER: u32 = 1 << 3;
    /// A handle of the object was buffered as a possible GC root (a hint
    /// like `GC_INFO`: freeing the handle removes its buffer entry).
    pub const GC_BUFFERED: u32 = 1 << 4;
    /// The object has a materialized property table (`zobj->properties`,
    /// built by `rebuild_object_properties` for `var_dump`, `foreach`,
    /// `get_object_vars` & co.).
    pub const PROPS_TABLE: u32 = 1 << 5;
    /// `HASH_FLAG_HAS_EMPTY_IND` on that table: a declared slot it points
    /// to was undefined, so its element count is recalculated.
    pub const EMPTY_IND: u32 = 1 << 6;
    /// The properties table holds frozen copies of the declared
    /// properties (`lazy::stale_slots`).
    pub const STALE_PROPS: u32 = 1 << 7;
}

/// The state of a PHP object.
pub struct ObjectData {
    /// Handle number (`spl_object_id`).
    pub handle: u32,
    /// The class.
    pub class: Rc<Class>,
    /// Declared property slots (`None` = undefined/uninitialized).
    pub slots: RefCell<Vec<Option<Value>>>,
    /// Dynamic properties (`zend_object.properties`). Boxed: most objects
    /// have none, and an inline table added ~90 bytes to every object.
    pub dynamic: RefCell<Option<Box<HashTable>>>,
    /// Internal state for builtin classes.
    pub internal: RefCell<Option<Box<dyn Any>>>,
    /// `oflags`.
    pub flags: Cell<u32>,
    /// Per declared slot: explicitly unset (`IS_PROP_UNINIT` cleared, so
    /// `__get()` & co. apply). Bit `i` for slot `i < 63`; bit 63 means the
    /// flags of slots 63 and up live in [`UNSET_OVERFLOW`]. Read and written
    /// through [`ObjectData::slot_unset`] / [`ObjectData::set_slot_unset`].
    /// php-src keeps this flag in the property zval's `u2`; a bit set here
    /// costs 8 bytes per object instead of a `RefCell<Vec<bool>>` (32 bytes
    /// plus an allocation).
    unset_bits: Cell<u64>,
    /// Lazy-object state (`ReflectionClass::newLazyGhost()` & co). Boxed:
    /// few objects are lazy, and inline it added ~70 bytes to every object.
    pub lazy: RefCell<Option<Box<crate::lazy::LazyInfo>>>,
}

/// Slots whose "explicitly unset" flag fits in [`ObjectData::unset_bits`].
const UNSET_INLINE_SLOTS: usize = 63;
/// [`ObjectData::unset_bits`] flag: the object has [`UNSET_OVERFLOW`] flags.
const UNSET_OVERFLOW_BIT: u64 = 1 << 63;

thread_local! {
    /// "Explicitly unset" flags of slots `>= UNSET_INLINE_SLOTS`, by object
    /// handle (index = slot - `UNSET_INLINE_SLOTS`). Only objects of classes
    /// with more than 63 declared properties ever get an entry.
    static UNSET_OVERFLOW: RefCell<HashMap<u32, Vec<bool>>> = RefCell::new(HashMap::new());
}

impl ObjectData {
    /// Whether declared slot `idx` was explicitly unset (`unset()` on a
    /// declared property, or an unserialized uninitialized one): the
    /// opposite of `IS_PROP_UNINIT` for an undefined slot.
    #[inline]
    #[must_use]
    pub fn slot_unset(&self, idx: usize) -> bool {
        let bits = self.unset_bits.get();
        if idx < UNSET_INLINE_SLOTS {
            return bits & (1 << idx) != 0;
        }
        bits & UNSET_OVERFLOW_BIT != 0 && self.overflow_unset(idx)
    }

    #[cold]
    #[inline(never)]
    fn overflow_unset(&self, idx: usize) -> bool {
        UNSET_OVERFLOW
            .try_with(|m| {
                m.borrow()
                    .get(&self.handle)
                    .and_then(|v| v.get(idx - UNSET_INLINE_SLOTS).copied())
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    /// Sets or clears the "explicitly unset" flag of declared slot `idx`.
    #[inline]
    pub fn set_slot_unset(&self, idx: usize, unset: bool) {
        if idx < UNSET_INLINE_SLOTS {
            let bit = 1u64 << idx;
            let bits = self.unset_bits.get();
            self.unset_bits
                .set(if unset { bits | bit } else { bits & !bit });
        } else if unset || self.unset_bits.get() & UNSET_OVERFLOW_BIT != 0 {
            self.set_overflow_unset(idx, unset);
        }
    }

    #[cold]
    #[inline(never)]
    fn set_overflow_unset(&self, idx: usize, unset: bool) {
        let i = idx - UNSET_INLINE_SLOTS;
        let _ = UNSET_OVERFLOW.try_with(|m| {
            let mut m = m.borrow_mut();
            let v = m.entry(self.handle).or_default();
            if v.len() <= i {
                v.resize(i + 1, false);
            }
            v[i] = unset;
        });
        self.unset_bits
            .set(self.unset_bits.get() | UNSET_OVERFLOW_BIT);
    }

    /// Copies all "explicitly unset" flags from `from` (object clone).
    pub fn copy_unset_flags(&self, from: &ObjectData) {
        let had_overflow = self.unset_bits.get() & UNSET_OVERFLOW_BIT != 0;
        let bits = from.unset_bits.get();
        self.unset_bits.set(bits);
        if had_overflow || bits & UNSET_OVERFLOW_BIT != 0 {
            let _ = UNSET_OVERFLOW.try_with(|m| {
                let mut m = m.borrow_mut();
                let src = m.get(&from.handle).cloned();
                match src {
                    Some(v) if bits & UNSET_OVERFLOW_BIT != 0 => m.insert(self.handle, v),
                    _ => m.remove(&self.handle),
                }
            });
        }
    }
}

/// Drops the [`UNSET_OVERFLOW`] flags of a freed object.
#[cold]
#[inline(never)]
fn forget_unset_overflow(handle: u32) {
    let _ = UNSET_OVERFLOW.try_with(|m| {
        if let Ok(mut m) = m.try_borrow_mut() {
            m.remove(&handle);
        }
    });
}

impl fmt::Debug for ObjectData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Object({}#{})", self.class.name, self.handle)
    }
}

thread_local! {
    static STORE: RefCell<ObjectStore> = RefCell::new(ObjectStore::default());
    static PENDING: RefCell<Vec<Obj>> = const { RefCell::new(Vec::new()) };
    static HAS_PENDING: Cell<bool> = const { Cell::new(false) };
    /// Bumped whenever an object is queued on [`PENDING`]: lets
    /// `ObjectData::drop` see whether releasing its contents queued
    /// destructors.
    static PENDING_SEQ: Cell<u32> = const { Cell::new(0) };
    static DESTRUCTORS_ENABLED: Cell<bool> = const { Cell::new(true) };
    /// Per-object property orders by handle; an entry only counts while its
    /// object carries [`oflags::PROP_ORDER`].
    static PROP_ORDERS: RefCell<HashMap<u32, Rc<PropOrder>>> = RefCell::new(HashMap::new());
    /// Size of [`PROP_ORDERS`] at which entries of freed objects are pruned.
    static PROP_ORDERS_PRUNE_AT: Cell<usize> = const { Cell::new(PROP_ORDERS_MIN_PRUNE) };
    /// Handles of objects whose declared property is being unset while
    /// releasing its old value runs destructors (see
    /// [`begin_stale_count`]); one entry per pending unset.
    static STALE_COUNTS: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

/// Smallest [`PROP_ORDERS`] size that triggers pruning.
const PROP_ORDERS_MIN_PRUNE: usize = 64;

/// `zend_objects_store_init(&EG(objects_store), 1024)`: the store's initial
/// size, allocated before any script runs.
const STORE_INITIAL: usize = 1024;

struct ObjectStore {
    /// Live objects by handle (index = handle - 1): the object's current
    /// outer `Obj` (a thin weak handle, so a slot stays one word).
    objects: Vec<Option<ObjWeak>>,
    /// Free handles (LIFO, like Zend's free list).
    free: Vec<u32>,
    /// `EG_FLAGS_OBJECT_STORE_NO_REUSE`: while shutdown destructors run,
    /// new objects take fresh handles past the end of the store.
    no_reuse: bool,
    /// Handles of freed objects whose release queued a destructor, keyed
    /// by the handle of the last object so queued: they join the free
    /// list once that object is freed (or survives its destructor). In
    /// php-src the nested object is destructed and freed inside
    /// `zend_object_std_dtor` of the outer one, before the outer handle
    /// goes on the free list, so the outer handle is reused first.
    followers: HashMap<u32, Vec<u32>>,
}

impl Default for ObjectStore {
    fn default() -> Self {
        Self {
            objects: Vec::with_capacity(STORE_INITIAL),
            free: Vec::with_capacity(STORE_INITIAL),
            no_reuse: false,
            followers: HashMap::new(),
        }
    }
}

impl ObjectStore {
    /// `zend_objects_store_put`: the next handle — the head of the free
    /// list unless reuse is suspended, else a new slot at the end.
    fn take_handle(&mut self) -> u32 {
        if !self.no_reuse {
            if let Some(h) = self.free.pop() {
                return h;
            }
        }
        if self.objects.len() == self.objects.capacity() {
            self.grow();
        }
        self.objects.push(None);
        u32::try_from(self.objects.len()).unwrap_or(u32::MAX)
    }

    /// The full store doubles (`zend_objects_store_put`'s `erealloc`). The
    /// free list, which Zend threads through the store's slots, grows with
    /// it, so freeing an object never allocates (no `memory_get_usage()`
    /// delta after a first object comes and goes, bug #79881).
    #[cold]
    #[inline(never)]
    fn grow(&mut self) {
        self.objects.reserve(self.objects.len().max(STORE_INITIAL));
        let cap = self.objects.capacity();
        self.free.reserve(cap.saturating_sub(self.free.len()));
    }

    /// `ZEND_OBJECTS_STORE_ADD_TO_FREE_LIST` for `handle` (then for the
    /// handles waiting on it), or, with `anchor`, defers them all until
    /// the object `anchor` is released.
    fn release_handle(&mut self, handle: u32, anchor: Option<u32>) {
        let waiting = if self.followers.is_empty() {
            None
        } else {
            self.followers.remove(&handle)
        };
        let dst = match anchor {
            Some(a) => self.followers.entry(a).or_default(),
            None => &mut self.free,
        };
        dst.push(handle);
        if let Some(w) = waiting {
            dst.extend(w);
        }
    }
}

/// Creates this thread's object store at its initial size (VM startup), so
/// the first object of a script does not allocate it.
pub(crate) fn prewarm_store() {
    STORE.with(|_| {});
}

impl Drop for ObjectData {
    fn drop(&mut self) {
        let handle = self.handle;
        if self.unset_bits.get() & UNSET_OVERFLOW_BIT != 0 {
            forget_unset_overflow(handle);
        }
        let seq = PENDING_SEQ.try_with(Cell::get).unwrap_or(0);
        // `zend_weakrefs_notify`: purge WeakMap entries before the handle
        // can be reused.
        crate::weakref::object_freed(handle);
        // `zend_object_std_dtor` releases the properties (and objects they
        // hold) before `zend_objects_store_del` puts this handle on the
        // free list, so this handle is the next one reused.
        let slots = std::mem::take(self.slots.get_mut());
        let dynamic = self.dynamic.get_mut().take();
        let internal = self.internal.get_mut().take();
        let stale = if self.flags.get() & oflags::STALE_PROPS == 0 {
            None
        } else {
            crate::lazy::take_stale_slots(handle)
        };
        drop((slots, dynamic, internal, stale));
        // A destructor queued meanwhile runs (and frees its object) before
        // this handle is reusable, as it would have inside php-src's
        // `zend_object_std_dtor`.
        let anchor = if PENDING_SEQ.try_with(Cell::get).unwrap_or(0) == seq {
            None
        } else {
            last_pending_handle()
        };
        // `try_with`: the store may already be gone during thread teardown.
        let _ = STORE.try_with(|s| {
            if let Ok(mut s) = s.try_borrow_mut() {
                if let Some(slot) = s.objects.get_mut(handle as usize - 1) {
                    *slot = None;
                }
                s.release_handle(handle, anchor);
            }
        });
    }
}

/// The `PhpObject` implementation: a handle to [`ObjectData`].
pub struct ObjBox(pub Rc<ObjectData>);

impl fmt::Debug for ObjBox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl PhpObject for ObjBox {
    fn handle(&self) -> u32 {
        self.0.handle
    }
    fn class_name(&self) -> ZStr {
        display_name(&self.0.class.name)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for ObjBox {
    fn drop(&mut self) {
        let data = &self.0;
        if data.flags.get() & oflags::GC_BUFFERED != 0 {
            // `GC_REMOVE_FROM_BUFFER` in `zend_objects_store_del`.
            crate::gc::forget_root(Obj::addr_of_payload::<ObjBox>(self));
        }
        if data.flags.get() & oflags::DESTRUCTED != 0 || !data.class.has_destructor() {
            return;
        }
        if !DESTRUCTORS_ENABLED.with(Cell::get) {
            return;
        }
        data.flags.set(data.flags.get() | oflags::DESTRUCTED);
        let revived = Obj::new(ObjBox(data.clone()));
        register_outer(&revived);
        let _ = PENDING.try_with(|p| {
            if let Ok(mut p) = p.try_borrow_mut() {
                p.push(revived);
                HAS_PENDING.with(|h| h.set(true));
                PENDING_SEQ.with(|c| c.set(c.get().wrapping_add(1)));
            }
        });
    }
}

impl std::ops::Deref for ObjBox {
    type Target = ObjectData;
    fn deref(&self) -> &ObjectData {
        &self.0
    }
}

/// Allocates a handle and creates an object of `class` with the given slot
/// values.
pub fn new_object(class: Rc<Class>, slots: Vec<Option<Value>>) -> Obj {
    STORE.with(|s| {
        let mut s = s.borrow_mut();
        let handle = s.take_handle();
        let data = Rc::new(ObjectData {
            handle,
            class,
            slots: RefCell::new(slots),
            dynamic: RefCell::new(None),
            internal: RefCell::new(None),
            flags: Cell::new(0),
            unset_bits: Cell::new(0),
            lazy: RefCell::new(None),
        });
        let o = Obj::new(ObjBox(data));
        s.objects[handle as usize - 1] = Some(o.downgrade());
        o
    })
}

/// Makes `o` the store's handle of its object: lookups by handle return
/// this `Obj` from now on (the previous outer `Obj` is gone or going).
fn register_outer(o: &Obj) {
    let handle = data(o).handle;
    // `try_with`/`try_borrow_mut`: called from `Drop`.
    let _ = STORE.try_with(|s| {
        if let Ok(mut s) = s.try_borrow_mut() {
            if let Some(slot) = (handle as usize)
                .checked_sub(1)
                .and_then(|i| s.objects.get_mut(i))
            {
                *slot = Some(o.downgrade());
            }
        }
    });
}

/// An object handle taken from the store with no object behind it.
///
/// Zend's `zend_object_iterator` is itself an object: `zend_iterator_init`
/// takes a handle from the object store for every `get_iterator` iterator
/// (`foreach` over an `Iterator`, an `IteratorAggregate`, a generator or an
/// internal class), and `FE_FREE` returns it after the iterated object has
/// been released. The guard reproduces that numbering: the handle goes
/// back on the free list when it drops.
pub struct HandleGuard(u32);

impl HandleGuard {
    /// Takes the next free handle (`zend_objects_store_put`).
    #[must_use]
    pub fn reserve() -> Self {
        STORE.with(|s| {
            let handle = s.borrow_mut().take_handle();
            HandleGuard(handle)
        })
    }

    /// The reserved handle number.
    #[must_use]
    pub fn handle(&self) -> u32 {
        self.0
    }
}

impl Drop for HandleGuard {
    fn drop(&mut self) {
        let handle = self.0;
        // `try_with`: the store may already be gone during thread teardown.
        let _ = STORE.try_with(|s| {
            if let Ok(mut s) = s.try_borrow_mut() {
                s.free.push(handle);
            }
        });
    }
}

impl fmt::Debug for HandleGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HandleGuard(#{})", self.0)
    }
}

/// Returns our object data for an `Obj`.
///
/// # Panics
/// Panics if the object was not created by this VM.
pub fn data(o: &Obj) -> &ObjectData {
    &o.downcast_ref::<ObjBox>().expect("phprust object").0
}

/// Returns the class of an object.
pub fn class_of(o: &Obj) -> &Rc<Class> {
    &data(o).class
}

/// Whether destructors are pending.
pub fn has_pending_destructors() -> bool {
    HAS_PENDING.with(Cell::get)
}

/// The handle of the object queued last for its destructor.
#[cold]
#[inline(never)]
fn last_pending_handle() -> Option<u32> {
    PENDING
        .try_with(|p| p.try_borrow().ok().and_then(|p| p.last().map(Obj::handle)))
        .ok()
        .flatten()
}

/// Puts the handles deferred behind `handle` (see `ObjectStore::followers`)
/// on the free list: called once the destructor of the object `handle`
/// ran and the object survived it.
pub fn release_deferred_handles(handle: u32) {
    let _ = STORE.try_with(|s| {
        if let Ok(mut s) = s.try_borrow_mut() {
            if let Some(w) = s.followers.remove(&handle) {
                s.free.extend(w);
            }
        }
    });
}

/// Takes the objects whose destructors must run now.
pub fn take_pending_destructors() -> Vec<Obj> {
    HAS_PENDING.with(|h| h.set(false));
    PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()))
}

/// Enables or disables destructor scheduling (disabled after fatal errors).
pub fn set_destructors_enabled(on: bool) {
    DESTRUCTORS_ENABLED.with(|c| c.set(on));
}

/// Whether destructors are enabled.
pub fn destructors_enabled() -> bool {
    DESTRUCTORS_ENABLED.with(Cell::get)
}

/// All live objects in handle order (for shutdown destructors).
pub fn live_objects() -> Vec<Rc<ObjectData>> {
    // The upgraded `Obj` is a second strong handle, so dropping it inside
    // the borrow never runs `ObjBox::drop`.
    STORE.with(|s| {
        s.borrow()
            .objects
            .iter()
            .filter_map(|w| {
                w.as_ref()
                    .and_then(ObjWeak::upgrade)
                    .and_then(|o| box_data(&o))
            })
            .collect()
    })
}

/// The number of handle slots in the store (`objects->top - 1`).
pub fn store_len() -> usize {
    STORE.with(|s| s.borrow().objects.len())
}

/// The live object in store slot `i` (handle `i + 1`), if any.
pub fn live_object_at(i: usize) -> Option<Rc<ObjectData>> {
    live_box_at(i).and_then(|o| box_data(&o))
}

/// The live object in store slot `i` (handle `i + 1`) as the `Obj` every
/// other holder has (same identity and refcount).
pub fn live_obj_at(i: usize) -> Option<Obj> {
    live_box_at(i)
}

fn live_box_at(i: usize) -> Option<Obj> {
    STORE.with(|s| {
        s.borrow()
            .objects
            .get(i)
            .and_then(|w| w.as_ref().and_then(ObjWeak::upgrade))
    })
}

/// The `Rc<ObjectData>` behind one of our objects.
fn box_data(o: &Obj) -> Option<Rc<ObjectData>> {
    o.downcast_ref::<ObjBox>().map(|b| b.0.clone())
}

/// Suspends (`true`) or restores handle reuse
/// (`EG_FLAGS_OBJECT_STORE_NO_REUSE`, set by
/// `zend_objects_store_call_destructors`).
pub fn set_handle_no_reuse(on: bool) {
    STORE.with(|s| s.borrow_mut().no_reuse = on);
}

/// The `Obj` of existing object data: the store's current handle when one
/// is alive (so identity and refcount match every other holder), else a
/// new outer `Obj`, which becomes the store's handle.
pub fn revive(data: Rc<ObjectData>) -> Obj {
    let live = (data.handle as usize).checked_sub(1).and_then(live_box_at);
    if let Some(o) = &live {
        if o.downcast_ref::<ObjBox>()
            .is_some_and(|b| Rc::ptr_eq(&b.0, &data))
        {
            return o.clone();
        }
    }
    let o = Obj::new(ObjBox(data));
    if live.is_none() {
        register_outer(&o);
    }
    drop(live);
    o
}

/// `rebuild_object_properties`: `d` now has a materialized property table
/// (`zobj->properties`). A declared slot that is undefined at that point
/// sets `HASH_FLAG_HAS_EMPTY_IND` on it.
#[inline]
pub fn note_property_table(d: &ObjectData) {
    if d.flags.get() & oflags::PROPS_TABLE == 0 {
        build_property_table(d);
    }
}

/// [`note_property_table`] for an object with dynamic properties, whose
/// creation materialized the table (`zend_std_write_property` calls
/// `rebuild_object_properties`). The slots' state at the creation is not
/// recorded; the current one stands in for it.
pub fn note_dynamic_property_table(d: &ObjectData) {
    if d.flags.get() & oflags::PROPS_TABLE == 0 && d.dynamic.try_borrow().is_ok_and(|t| t.is_some())
    {
        build_property_table(d);
    }
}

#[cold]
#[inline(never)]
fn build_property_table(d: &ObjectData) {
    let mut f = d.flags.get() | oflags::PROPS_TABLE;
    let holes = d.slots.try_borrow().is_ok_and(|slots| {
        d.class
            .props
            .iter()
            .filter(|p| !p.is_virtual())
            .any(|p| slots.get(p.slot as usize).is_none_or(Option::is_none))
    });
    if holes {
        f |= oflags::EMPTY_IND;
    }
    d.flags.set(f);
}

/// `zend_std_unset_property` on a defined declared slot of `d`: the slot is
/// `IS_UNDEF` already, but `HASH_FLAG_HAS_EMPTY_IND` is only set on the
/// property table after the old value was released. Destructors that
/// release runs synchronously (`zval_ptr_dtor`) therefore still see the
/// table's old element count (`zend_array_count`), e.g. in `var_dump`.
///
/// Returns whether that window applies (a property table exists and has
/// no empty indirect slot yet); the caller then runs the destructors and
/// calls [`end_stale_count`].
#[must_use]
pub fn begin_stale_count(d: &ObjectData) -> bool {
    let f = d.flags.get();
    if f & (oflags::PROPS_TABLE | oflags::EMPTY_IND) != oflags::PROPS_TABLE {
        return false;
    }
    STALE_COUNTS.with(|s| s.borrow_mut().push(d.handle));
    true
}

/// Ends the window opened by [`begin_stale_count`]: the table is flagged
/// `HASH_FLAG_HAS_EMPTY_IND`, so counts are exact from now on.
pub fn end_stale_count(d: &ObjectData) {
    let _ = STALE_COUNTS.try_with(|s| {
        if let Ok(mut s) = s.try_borrow_mut() {
            if let Some(p) = s.iter().rposition(|&h| h == d.handle) {
                s.remove(p);
            }
        }
    });
    d.flags.set(d.flags.get() | oflags::EMPTY_IND);
}

/// Unset declared properties `zend_array_count` still counts for the
/// object `handle` (see [`begin_stale_count`]).
#[must_use]
pub fn stale_count(handle: u32) -> usize {
    STALE_COUNTS.with(|s| {
        let s = s.borrow();
        if s.is_empty() {
            0
        } else {
            s.iter().filter(|&&h| h == handle).count()
        }
    })
}

/// `HASH_FLAG_HAS_EMPTY_IND` after a declared slot was unset with no
/// destructor in between (the table's count is recalculated from now on).
pub fn note_declared_unset(d: &ObjectData) {
    let f = d.flags.get();
    if f & oflags::PROPS_TABLE != 0 {
        d.flags.set(f | oflags::EMPTY_IND);
    }
}

/// The handle number the next object will get (`spl_object_id` tests).
pub fn next_handle() -> u32 {
    STORE.with(|s| {
        let s = s.borrow();
        s.free
            .last()
            .copied()
            .filter(|_| !s.no_reuse)
            .unwrap_or_else(|| u32::try_from(s.objects.len() + 1).unwrap_or(u32::MAX))
    })
}

/// Typed access to an object's internal state.
pub fn internal<T: 'static, R>(o: &Obj, f: impl FnOnce(&mut T) -> R) -> Option<R> {
    let d = data(o);
    let mut b = d.internal.borrow_mut();
    b.as_mut().and_then(|x| x.downcast_mut::<T>()).map(f)
}

/// Sets an object's internal state.
pub fn set_internal<T: 'static>(o: &Obj, v: T) {
    *data(o).internal.borrow_mut() = Some(Box::new(v));
}

/// Whether the object has internal state of type `T`.
pub fn has_internal<T: 'static>(o: &Obj) -> bool {
    data(o)
        .internal
        .borrow()
        .as_ref()
        .is_some_and(|x| x.is::<T>())
}

/// A class name as PHP prints it with `%s` (anonymous class names are cut
/// at the NUL byte).
pub fn display_name(name: &ZStr) -> ZStr {
    match name.as_bytes().iter().position(|&c| c == 0) {
        Some(p) => ZStr::from(&name.as_bytes()[..p]),
        None => name.clone(),
    }
}

/// A property of an object in a [`PropOrder`]: a declared property by slot
/// or a dynamic property by key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PropKey {
    /// A declared property (`PropInfo::slot`).
    Slot(u32),
    /// A dynamic property.
    Dynamic(ArrayKey),
}

/// An object's own property order.
///
/// Zend lists properties from `zobj->properties`, a table rebuilt in
/// declaration order (then dynamic properties) and normally kept in that
/// order; `ArrayObject::asort()` & co. over an object storage sort that
/// table in place, and the new order then shows everywhere (`var_dump`,
/// `foreach`, `get_object_vars`, `(array)`, `serialize`, `json_encode`).
/// Objects keep values in their slots and dynamic table here, so the
/// override is the rank of each property; listings stable-sort by it.
/// Properties without a rank (added after the reorder) keep their default
/// order after the ranked ones, as appends to `zobj->properties` do. Only
/// objects flagged [`oflags::PROP_ORDER`] have one, so ordinary listings
/// pay a flag test.
#[derive(Clone, Debug, Default)]
pub struct PropOrder {
    slots: HashMap<u32, usize>,
    dynamic: HashMap<ArrayKey, usize>,
}

impl PropOrder {
    /// The order of `keys` (the first occurrence of a key counts).
    #[must_use]
    pub fn from_keys(keys: impl IntoIterator<Item = PropKey>) -> Self {
        let mut order = Self::default();
        for (i, k) in keys.into_iter().enumerate() {
            match k {
                PropKey::Slot(s) => {
                    order.slots.entry(s).or_insert(i);
                }
                PropKey::Dynamic(k) => {
                    order.dynamic.entry(k).or_insert(i);
                }
            }
        }
        order
    }

    /// The rank of a declared property (`slot`) or of the dynamic
    /// property `key`; unranked properties sort last.
    #[must_use]
    pub fn rank(&self, slot: Option<u32>, key: &ArrayKey) -> usize {
        match slot {
            Some(s) => self.slots.get(&s),
            None => self.dynamic.get(key),
        }
        .copied()
        .unwrap_or(usize::MAX)
    }

    /// Stable-sorts property entries (in default order) into this order;
    /// `key_of` gives an entry's slot (declared) and key.
    pub fn sort<T>(&self, items: &mut [T], key_of: impl Fn(&T) -> (Option<u32>, &ArrayKey)) {
        items.sort_by_key(|item| {
            let (slot, key) = key_of(item);
            self.rank(slot, key)
        });
    }
}

/// The property order of an object, when it has its own.
#[must_use]
pub fn prop_order(d: &ObjectData) -> Option<Rc<PropOrder>> {
    if d.flags.get() & oflags::PROP_ORDER == 0 {
        return None;
    }
    PROP_ORDERS.with(|m| m.borrow().get(&d.handle).cloned())
}

/// Gives an object its own property order (`None` restores the default:
/// declaration order, then dynamic properties).
pub fn set_prop_order(d: &ObjectData, order: Option<PropOrder>) {
    match order {
        Some(order) => install_prop_order(d, Rc::new(order)),
        None => {
            if d.flags.get() & oflags::PROP_ORDER != 0 {
                d.flags.set(d.flags.get() & !oflags::PROP_ORDER);
                let _ = PROP_ORDERS.try_with(|m| m.borrow_mut().remove(&d.handle));
            }
        }
    }
}

/// `clone`: the copy lists its properties in the original's order.
pub fn copy_prop_order(from: &ObjectData, to: &ObjectData) {
    match prop_order(from) {
        Some(order) => install_prop_order(to, order),
        None => set_prop_order(to, None),
    }
}

/// A dynamic property was removed: re-adding it appends it (Zend deletes
/// the `zobj->properties` bucket).
pub fn forget_dynamic_prop_order(d: &ObjectData, key: &ArrayKey) {
    if d.flags.get() & oflags::PROP_ORDER == 0 {
        return;
    }
    PROP_ORDERS.with(|m| {
        if let Some(order) = m.borrow_mut().get_mut(&d.handle) {
            Rc::make_mut(order).dynamic.remove(key);
        }
    });
}

fn install_prop_order(d: &ObjectData, order: Rc<PropOrder>) {
    d.flags.set(d.flags.get() | oflags::PROP_ORDER);
    PROP_ORDERS.with(|m| {
        let mut m = m.borrow_mut();
        if !m.contains_key(&d.handle) && m.len() >= PROP_ORDERS_PRUNE_AT.with(Cell::get) {
            prune_prop_orders(&mut m);
        }
        m.insert(d.handle, order);
    });
}

/// Drops the orders of freed objects (their handles may since belong to
/// objects without the flag).
fn prune_prop_orders(m: &mut HashMap<u32, Rc<PropOrder>>) {
    STORE.with(|s| {
        let s = s.borrow();
        m.retain(|&h, _| {
            (h as usize)
                .checked_sub(1)
                .and_then(|i| s.objects.get(i))
                .and_then(Option::as_ref)
                .and_then(ObjWeak::upgrade)
                .is_some_and(|o| data(&o).flags.get() & oflags::PROP_ORDER != 0)
        });
    });
    let next = (m.len() * 2).max(PROP_ORDERS_MIN_PRUNE);
    PROP_ORDERS_PRUNE_AT.with(|c| c.set(next));
}

/// Builds a property table in an object's property order: entries are
/// pushed in default order (declared, then dynamic).
#[derive(Debug)]
pub struct PropTableBuilder {
    order: Option<Rc<PropOrder>>,
    table: HashTable,
    ranked: Vec<(usize, ArrayKey, Value)>,
}

impl PropTableBuilder {
    /// A builder for the properties of `d`.
    #[must_use]
    pub fn new(d: &ObjectData) -> Self {
        Self {
            order: prop_order(d),
            table: HashTable::new(),
            ranked: Vec::new(),
        }
    }

    /// Adds a declared (`slot`) or dynamic property.
    pub fn push(&mut self, slot: Option<u32>, key: ArrayKey, value: Value) {
        match &self.order {
            None => {
                self.table.insert(key, value);
            }
            Some(order) => {
                let rank = order.rank(slot, &key);
                self.ranked.push((rank, key, value));
            }
        }
    }

    /// Adds a property under `key`, ranked as the property `prop` (its
    /// key in the object's table) is.
    pub fn push_renamed(
        &mut self,
        slot: Option<u32>,
        prop: &ArrayKey,
        key: ArrayKey,
        value: Value,
    ) {
        match &self.order {
            None => {
                self.table.insert(key, value);
            }
            Some(order) => {
                let rank = order.rank(slot, prop);
                self.ranked.push((rank, key, value));
            }
        }
    }

    /// The finished table.
    #[must_use]
    pub fn finish(mut self) -> HashTable {
        self.ranked.sort_by_key(|e| e.0);
        for (_, key, value) in self.ranked {
            self.table.insert(key, value);
        }
        self.table
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> ArrayKey {
        ArrayKey::Str(ZStr::from(s))
    }

    #[test]
    fn store_free_list_grows_with_the_store() {
        let mut s = ObjectStore::default();
        assert!(s.objects.capacity() >= STORE_INITIAL);
        assert!(s.free.capacity() >= STORE_INITIAL);
        for _ in 0..=STORE_INITIAL {
            let _ = s.take_handle();
        }
        assert!(s.objects.capacity() >= 2 * STORE_INITIAL);
        assert!(s.free.capacity() >= s.objects.capacity());
        // Freeing every handle never reallocates the free list.
        let cap = s.free.capacity();
        for h in 1..=u32::try_from(s.objects.len()).unwrap() {
            s.free.push(h);
        }
        assert_eq!(s.free.capacity(), cap);
        assert_eq!(s.take_handle(), u32::try_from(STORE_INITIAL + 1).unwrap());
    }

    #[test]
    fn prop_order_ranks_slots_and_dynamic_keys() {
        let order = PropOrder::from_keys([
            PropKey::Dynamic(key("d")),
            PropKey::Slot(2),
            PropKey::Slot(0),
            PropKey::Slot(2),
        ]);
        assert_eq!(order.rank(None, &key("d")), 0);
        assert_eq!(order.rank(Some(2), &key("ignored")), 1);
        assert_eq!(order.rank(Some(0), &key("ignored")), 2);
        assert_eq!(order.rank(Some(1), &key("p1")), usize::MAX);
        assert_eq!(order.rank(None, &key("new")), usize::MAX);
    }

    #[test]
    fn prop_order_sort_is_stable_for_unranked_entries() {
        let order = PropOrder::from_keys([PropKey::Slot(1), PropKey::Dynamic(key("d"))]);
        let mut items = vec![
            (Some(0), key("a")),
            (Some(1), key("b")),
            (None, key("e")),
            (None, key("d")),
            (None, key("f")),
        ];
        order.sort(&mut items, |(slot, k)| (*slot, k));
        let names: Vec<_> = items.iter().map(|(_, k)| k.clone()).collect();
        assert_eq!(names, [key("b"), key("d"), key("a"), key("e"), key("f")]);
    }
}

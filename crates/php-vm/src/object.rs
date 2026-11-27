// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Objects (`zend_object`): property slots, dynamic properties, handles and
//! destructor scheduling.
//!
//! An object is one allocation, like php-src's `zend_object` with its
//! `properties_table` (ref: Zend/zend_objects.c `zend_objects_new`): the
//! [`Obj`] cell of `php-runtime` (counts, vtable), the fixed part
//! [`ObjectData`], then the declared property slots inline ([`Slots`]; the
//! payload type is an `ObjectBlock<N>` with room for `N` slots, `N` the
//! class's slot count rounded up to a size class). Every payload shares one
//! header type ([`Obj::new_in_family`]), so [`data`] is a type check plus a
//! constant offset; every `Obj` handle is one of php-src's `GC_REFCOUNT`
//! references, and an object keeps its address, handle and identity for its
//! whole life, destructor included.
//!
//! Destructor timing (`zend_objects_store_del`): when the last handle of an
//! object is dropped, `Obj::drop` calls [`PhpObject::release_last`] before
//! the count reaches zero. An object with an uncalled `__destruct` clones
//! that handle onto the pending queue and so stays alive at count 1
//! (php-src's `GC_SET_REFCOUNT(obj, 1)` around `dtor_obj`) until the
//! executor runs the queue at its next safe point and drops the handle
//! again, which frees it: the payload's `Drop` is `zend_object_std_dtor`
//! plus the tail of `zend_objects_store_del` (`GC_REMOVE_FROM_BUFFER`, the
//! store slot invalidated, the handle back on the free list).
//!
//! The object store (`EG(objects_store)`, ref: Zend/zend_objects_API.c)
//! holds a weak handle per live object, so a lookup by handle (shutdown
//! destructors, `WeakReference::get()`) yields the one object every other
//! holder has — the same identity for `===` and recursion guards, and the
//! same refcount — as php-src's one `zend_object *` per handle does.

use crate::class::Class;
use php_runtime::{ArrayKey, HashTable, Obj, ObjWeak, PhpObject, Value, ZStr};
use std::any::Any;
use std::cell::{Cell, RefCell, UnsafeCell};
use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::ops::{Deref, DerefMut};
use std::ptr::{self, NonNull};
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
    /// A `WeakReference` was made of the object or it became a `WeakMap`
    /// key (`IS_OBJ_WEAKLY_REFERENCED`): its free tells the weak
    /// references (`weakref::object_freed`). Never cleared.
    pub const WEAKLY_REF: u32 = 1 << 8;
}

/// The state of a PHP object: the fixed part of its allocation (the
/// declared property slots follow it, see [`Slots`]).
pub struct ObjectData {
    /// Handle number (`spl_object_id`). A cell: `new` over a variable's
    /// last object gives the block the handle a new object would get
    /// ([`reuse_for_new`]).
    pub handle: Cell<u32>,
    /// The class.
    pub class: Rc<Class>,
    /// Declared property slots (`None` = undefined/uninitialized), stored
    /// inline after this struct (`zend_object.properties_table`).
    pub slots: Slots,
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
    /// Whether the object is not lazy (`zend_object_is_lazy` false) and its
    /// lazy state is not being changed: the property cache's hits apply.
    #[inline(always)]
    #[allow(clippy::inline_always)]
    #[must_use]
    pub fn not_lazy(&self) -> bool {
        self.lazy.try_borrow().is_ok_and(|l| l.is_none())
    }

    /// Whether declared slot `idx` was explicitly unset (`unset()` on a
    /// declared property, or an unserialized uninitialized one): the
    /// opposite of `IS_PROP_UNINIT` for an undefined slot.
    #[inline]
    #[must_use]
    pub fn slot_unset(&self, idx: usize) -> bool {
        let bits = self.unset_bits.get();
        let unset = if idx < UNSET_INLINE_SLOTS {
            bits & (1 << idx) != 0
        } else {
            bits & UNSET_OVERFLOW_BIT != 0 && self.overflow_unset(idx)
        };
        unset || self.starts_unset(idx)
    }

    /// Whether declared slot `idx` starts undefined but not uninitialized
    /// ([`flags::DEFAULT_UNSET`]): it can only be defined or unset after.
    #[inline]
    fn starts_unset(&self, idx: usize) -> bool {
        self.class
            .props
            .get(idx)
            .is_some_and(|p| p.flags & crate::func::flags::DEFAULT_UNSET != 0)
    }

    #[cold]
    #[inline(never)]
    fn overflow_unset(&self, idx: usize) -> bool {
        UNSET_OVERFLOW
            .try_with(|m| {
                m.borrow()
                    .get(&self.handle.get())
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
            let v = m.entry(self.handle.get()).or_default();
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
                let src = m.get(&from.handle.get()).cloned();
                match src {
                    Some(v) if bits & UNSET_OVERFLOW_BIT != 0 => m.insert(self.handle.get(), v),
                    _ => m.remove(&self.handle.get()),
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
        write!(f, "Object({}#{})", self.class.name, self.handle.get())
    }
}

/// The declared property slots of an object (`zend_object.properties_table`):
/// a `RefCell`-like cell over a slice stored inline after the object's
/// fixed part, in the object's own allocation (a class with more than
/// `MAX_INLINE` (64) slots keeps them in a buffer of their own). The slot
/// count is fixed when the object is created.
///
/// [`Slots::borrow`] and [`Slots::borrow_mut`] (and their `try_` forms)
/// work as `RefCell`'s, with guards that dereference to `[Option<Value>]`.
pub struct Slots {
    /// `RefCell`'s borrow state: 0 unused, `n > 0` shared borrows, -1 the
    /// exclusive one.
    borrow: Cell<isize>,
    /// The first slot (inside the allocation, written while the block is
    /// built in place by [`Obj::new_in_family_with`]); the pointer carries
    /// the provenance of the whole allocation (or of the separate buffer),
    /// which a reference to the fixed part does not.
    first: Cell<NonNull<Option<Value>>>,
    /// The slot count.
    len: usize,
}

// Byte offsets of the `Slots` fields, mirroring what `RefCell::try_borrow`
// and slicing read: pinned by the `jit` layout tests so a renamed or
// reordered field breaks them, not a trace.
pub(crate) const SLOTS_BORROW_OFF: usize = std::mem::offset_of!(Slots, borrow);
pub(crate) const SLOTS_FIRST_OFF: usize = std::mem::offset_of!(Slots, first);
pub(crate) const SLOTS_LEN_OFF: usize = std::mem::offset_of!(Slots, len);

impl Slots {
    /// `len` slots at `first`.
    fn new(first: NonNull<Option<Value>>, len: usize) -> Self {
        Slots {
            borrow: Cell::new(0),
            first: Cell::new(first),
            len,
        }
    }

    /// The number of declared slots.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the object has no declared slots.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    fn slice(&self) -> NonNull<[Option<Value>]> {
        NonNull::slice_from_raw_parts(self.first.get(), self.len)
    }

    /// Borrows the slots (`RefCell::borrow`).
    ///
    /// # Panics
    /// Panics if they are mutably borrowed.
    #[inline]
    #[track_caller]
    pub fn borrow(&self) -> SlotsRef<'_> {
        self.try_borrow().expect("already mutably borrowed")
    }

    /// Borrows the slots unless they are mutably borrowed
    /// (`RefCell::try_borrow`).
    ///
    /// # Errors
    /// [`SlotsBorrowError`] while the slots are mutably borrowed.
    #[inline]
    pub fn try_borrow(&self) -> Result<SlotsRef<'_>, SlotsBorrowError> {
        let b = self.borrow.get();
        // Negative: mutably borrowed; `isize::MAX`: the count is full.
        if b < 0 || b == isize::MAX {
            return Err(SlotsBorrowError);
        }
        self.borrow.set(b + 1);
        Ok(SlotsRef {
            slots: self.slice(),
            borrow: &self.borrow,
        })
    }

    /// Mutably borrows the slots (`RefCell::borrow_mut`).
    ///
    /// # Panics
    /// Panics if they are borrowed.
    #[inline]
    #[track_caller]
    pub fn borrow_mut(&self) -> SlotsRefMut<'_> {
        self.try_borrow_mut().expect("already borrowed")
    }

    /// Mutably borrows the slots unless they are borrowed
    /// (`RefCell::try_borrow_mut`).
    ///
    /// # Errors
    /// [`SlotsBorrowError`] while the slots are borrowed.
    #[inline]
    pub fn try_borrow_mut(&self) -> Result<SlotsRefMut<'_>, SlotsBorrowError> {
        if self.borrow.get() != 0 {
            return Err(SlotsBorrowError);
        }
        self.borrow.set(-1);
        Ok(SlotsRefMut {
            slots: self.slice(),
            borrow: &self.borrow,
            _marker: PhantomData,
        })
    }
}

impl fmt::Debug for Slots {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Ok(s) = self.try_borrow() {
            fmt::Debug::fmt(&*s, f)
        } else {
            write!(f, "Slots(<borrowed>, {})", self.len)
        }
    }
}

/// The slots were borrowed in a conflicting way ([`Slots::try_borrow`],
/// [`Slots::try_borrow_mut`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotsBorrowError;

impl fmt::Display for SlotsBorrowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("object property slots already borrowed")
    }
}

impl std::error::Error for SlotsBorrowError {}

/// A shared borrow of an object's slots ([`Slots::borrow`]).
pub struct SlotsRef<'a> {
    slots: NonNull<[Option<Value>]>,
    borrow: &'a Cell<isize>,
}

impl SlotsRef<'_> {
    /// The slot values as a `Vec`, as `.clone()` of a borrow of the former
    /// `RefCell<Vec<_>>` slots gave it (a copy to work on once the borrow
    /// is released; the guard itself is not duplicated).
    // Named for the callers written against `Ref<Vec<_>>::clone`; a guard
    // implementing `Clone` would hold the borrow instead.
    #[allow(clippy::should_implement_trait)]
    #[must_use]
    pub fn clone(&self) -> Vec<Option<Value>> {
        self.to_vec()
    }
}

impl Deref for SlotsRef<'_> {
    type Target = [Option<Value>];

    #[inline]
    fn deref(&self) -> &[Option<Value>] {
        // SAFETY: `slots` is the object's initialized slot storage, which
        // lives as long as the object, kept alive by the `'a` borrow of its
        // data; the shared borrow counted in `borrow` keeps the only writer
        // (`SlotsRefMut`) away until this guard is dropped.
        unsafe { self.slots.as_ref() }
    }
}

impl Drop for SlotsRef<'_> {
    #[inline]
    fn drop(&mut self) {
        self.borrow.set(self.borrow.get() - 1);
    }
}

impl fmt::Debug for SlotsRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

/// An exclusive borrow of an object's slots ([`Slots::borrow_mut`]).
pub struct SlotsRefMut<'a> {
    slots: NonNull<[Option<Value>]>,
    borrow: &'a Cell<isize>,
    _marker: PhantomData<&'a mut [Option<Value>]>,
}

impl Deref for SlotsRefMut<'_> {
    type Target = [Option<Value>];

    #[inline]
    fn deref(&self) -> &[Option<Value>] {
        // SAFETY: as for `SlotsRef`; the exclusive borrow (`borrow` is -1)
        // keeps every other guard away until this one is dropped.
        unsafe { self.slots.as_ref() }
    }
}

impl DerefMut for SlotsRefMut<'_> {
    #[inline]
    fn deref_mut(&mut self) -> &mut [Option<Value>] {
        // SAFETY: as for `deref`; `&mut self` makes the reference unique.
        unsafe { self.slots.as_mut() }
    }
}

impl Drop for SlotsRefMut<'_> {
    #[inline]
    fn drop(&mut self) {
        self.borrow.set(self.borrow.get() + 1);
    }
}

impl fmt::Debug for SlotsRefMut<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

/// The largest inline slot storage: a class with more declared slots keeps
/// them in a buffer of their own.
const MAX_INLINE: usize = 64;

/// The payload of an object's allocation: the fixed part, then room for
/// `N` declared property slots, of which the first `data.slots.len()` are
/// initialized (`zend_object` and its `properties_table`). A block without
/// inline room (`N == 0`) keeps the slots of a class with more than
/// [`MAX_INLINE`] in a separate buffer.
#[repr(C)]
pub(crate) struct ObjectBlock<const N: usize> {
    data: ObjectData,
    /// Written through the [`Slots`] pointer while shared references to the
    /// payload exist, hence the `UnsafeCell`.
    inline: UnsafeCell<[MaybeUninit<Option<Value>>; N]>,
}

/// Every object payload: [`data`] finds its [`ObjectData`] through the
/// family header ([`Obj::new_in_family`]).
impl<const N: usize> PhpObject for ObjectBlock<N> {
    #[inline]
    fn handle(&self) -> u32 {
        self.data.handle.get()
    }
    fn class_name(&self) -> ZStr {
        display_name(&self.data.class.name)
    }
    #[inline]
    fn as_any(&self) -> &dyn Any {
        &self.data
    }
    #[inline]
    fn release_last(&self, this: &Obj) {
        self.data.release_last(this);
    }
}

/// The free (see [`free_object`]), once the last handle is gone for good.
impl<const N: usize> Drop for ObjectBlock<N> {
    fn drop(&mut self) {
        let cell = Obj::addr_of_payload::<Self>(self);
        let len = self.data.slots.len;
        if len > N {
            // SAFETY: slots that do not fit inline (a block of `N == 0` for
            // more than `MAX_INLINE`) are the `len` of the buffer
            // `new_block` made with `Box::into_raw` (a separate
            // allocation, not under the drop glue's exclusive borrow of the
            // cell); nothing refers to it once the object is being freed.
            let mut buf: Box<[Option<Value>]> = unsafe {
                Box::from_raw(ptr::slice_from_raw_parts_mut(
                    self.data.slots.first.get().as_ptr(),
                    len,
                ))
            };
            free_object(&mut self.data, &mut buf, cell);
        } else {
            let inline = &mut self.inline.get_mut()[..len];
            // SAFETY: `new_block` initialized the first `len` entries (they
            // are only ever replaced by other values since). They are
            // reached through this drop's own exclusive reference, not the
            // `Slots` pointer, which may not be used while the drop glue
            // holds the cell (`Obj::payload_ptr`).
            let slots: &mut [Option<Value>] = unsafe {
                std::slice::from_raw_parts_mut(inline.as_mut_ptr().cast::<Option<Value>>(), len)
            };
            free_object(&mut self.data, slots, cell);
        }
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
    /// Live objects by handle (index = handle - 1), as weak handles (the
    /// allocation's `Weak<dyn PhpObject>`, two words: the slot is cleared
    /// from inside the object's own `Drop`, when the cell may not be read).
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

/// `zend_objects_store_free_object_storage()` at the end of a request.
///
/// What is still alive once the request's variables, frames and tables are
/// released is what the request leaked to cycles. php-src frees it with
/// the request's memory; kept, it would grow a long-running worker by every
/// request, and the next request's collector would find (and count) it.
/// With destructors off (`zend_objects_store_mark_destructed`: the shutdown
/// destructors have run, or a fatal error skipped them), the properties and
/// native state of every live object are released in reverse handle order
/// (`free_obj`), which frees each cycle through an object; then the
/// references the collector buffered are emptied, which frees the cycles of
/// arrays and references alone.
pub(crate) fn free_object_storage() {
    let live = live_objects();
    let refs = php_runtime::ref_roots::take();
    if live.is_empty() && refs.is_empty() {
        return;
    }
    for o in &live {
        let d = data(o);
        d.flags.set(d.flags.get() | oflags::DESTRUCTED);
    }
    for o in live.iter().rev() {
        let d = data(o);
        // Everything is taken out before anything is released; the
        // objects themselves are freed once `live` goes.
        let slots: Vec<Option<Value>> = d
            .slots
            .try_borrow_mut()
            .map(|mut s| s.iter_mut().map(Option::take).collect())
            .unwrap_or_default();
        let dynamic = d.dynamic.try_borrow_mut().ok().and_then(|mut t| t.take());
        let internal = d.internal.try_borrow_mut().ok().and_then(|mut t| t.take());
        let lazy = d.lazy.try_borrow_mut().ok().and_then(|mut t| t.take());
        drop((slots, dynamic, internal, lazy));
    }
    for r in &refs {
        if let Value::Reference(r) = r {
            drop(r.set(Value::Null));
        }
    }
    drop(refs);
    drop(live);
    // The references the releases above buffered.
    drop(php_runtime::ref_roots::take());
}

/// Resets the thread-local object store and handle counters for a new request.
pub fn reset_store() {
    STORE.with(|s| *s.borrow_mut() = ObjectStore::default());
    PENDING.with(|p| p.borrow_mut().clear());
    HAS_PENDING.with(|p| p.set(false));
    PENDING_SEQ.with(|p| p.set(0));
    DESTRUCTORS_ENABLED.with(|d| d.set(true));
    PROP_ORDERS.with(|p| p.borrow_mut().clear());
    PROP_ORDERS_PRUNE_AT.with(|p| p.set(PROP_ORDERS_MIN_PRUNE));
    STALE_COUNTS.with(|s| s.borrow_mut().clear());
    UNSET_OVERFLOW.with(|m| m.borrow_mut().clear());
}

impl ObjectData {
    /// `zend_objects_store_del` on `GC_REFCOUNT == 0` (the payload's
    /// [`PhpObject::release_last`]): an object whose `__destruct` has not
    /// run keeps itself alive for it.
    #[inline]
    fn release_last(&self, this: &Obj) {
        if self.flags.get() & oflags::DESTRUCTED != 0 || !self.class.has_destructor() {
            return;
        }
        // A generator with nothing to unwind is closed here, without the
        // destructor call.
        if self.class.internal == crate::class::InternalKind::Generator
            && crate::generator::close_on_release(this)
        {
            return;
        }
        queue_destructor(self, this);
    }
}

/// Queues `this` (the last handle of `d`, about to be dropped) for its
/// destructor: the clone keeps the object alive at count 1, the executor
/// calls `__destruct` on it at the next safe point and drops it, which frees
/// the object unless the destructor stored it somewhere (`DESTRUCTED` then
/// keeps it from running twice).
#[cold]
#[inline(never)]
fn queue_destructor(d: &ObjectData, this: &Obj) {
    if !DESTRUCTORS_ENABLED.with(Cell::get) {
        return;
    }
    d.flags.set(d.flags.get() | oflags::DESTRUCTED);
    // `try_with`/`try_borrow_mut`: the last handle may go during thread
    // teardown or while the queue is being taken; the object is then freed
    // without its destructor, as before.
    let _ = PENDING.try_with(|p| {
        if let Ok(mut p) = p.try_borrow_mut() {
            p.push(this.clone());
            HAS_PENDING.with(|h| h.set(true));
            // The executor runs the queue at its next safe point.
            crate::interrupt::raise_safe_point();
            PENDING_SEQ.with(|c| c.set(c.get().wrapping_add(1)));
        }
    });
}

/// Whether a slot value owns nothing (undefined, or a scalar: php-src's
/// non-refcounted zvals), so copying it is a bitwise copy and releasing it
/// a no-op.
#[inline(always)]
#[allow(clippy::inline_always)] // a tag test in the per-slot loops
fn owns_nothing(v: &Option<Value>) -> bool {
    matches!(
        v,
        None | Some(Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_))
    )
}

/// Whether releasing a slot value runs nothing and frees nothing but the
/// value itself: [`owns_nothing`], or a string (no destructor, no
/// possible root, no handle of anything else).
#[inline(always)]
#[allow(clippy::inline_always)] // a tag test in the per-slot loop
fn releases_quietly(v: &Option<Value>) -> bool {
    owns_nothing(v) || matches!(v, Some(Value::Str(_)))
}

/// The free: `zend_object_std_dtor` and the tail of
/// `zend_objects_store_del`, once the last handle is gone for good (no
/// destructor left to run), from the payload's `Drop`. `slots` are the
/// object's initialized slots and `cell` its [`Obj::addr`]. Only `d`,
/// `slots` (reached through the payload's own exclusive reference) and weak
/// counts are touched: the cell is under the drop glue's exclusive borrow.
fn free_object(d: &mut ObjectData, slots: &mut [Option<Value>], cell: usize) {
    let handle = d.handle.get();
    let flags = d.flags.get();
    // The common free: no side tables, no dynamic properties or native
    // state, and only slots that own nothing. Nothing is released then
    // (the slots are left as they are: inline ones are never dropped
    // again, `MaybeUninit`, and a separate buffer's drop releases
    // nothing); anything else is [`free_contents`]'s.
    let plain = flags & oflags::STALE_PROPS == 0
        && d.unset_bits.get() & UNSET_OVERFLOW_BIT == 0
        && d.dynamic.get_mut().is_none()
        && d.internal.get_mut().is_none()
        && slots.iter().all(owns_nothing);
    if flags & oflags::GC_BUFFERED != 0 {
        // `GC_REMOVE_FROM_BUFFER`: an object that was `$this` of a
        // constructor or of a method called on a temporary has an entry.
        crate::gc::forget_root(cell, handle);
    }
    if !plain && d.unset_bits.get() & UNSET_OVERFLOW_BIT != 0 {
        forget_unset_overflow(handle);
    }
    let seq = PENDING_SEQ.try_with(Cell::get).unwrap_or(0);
    // `zend_weakrefs_notify`: purge WeakMap entries before the handle can
    // be reused (a value released there may queue a destructor).
    if flags & oflags::WEAKLY_REF != 0 {
        crate::weakref::object_freed(handle);
    }
    if !plain {
        free_contents(d, slots, flags);
    }
    // A destructor queued meanwhile runs (and frees its object) before
    // this handle is reusable, as it would have inside php-src's
    // `zend_object_std_dtor`; otherwise the handle goes straight back on
    // the free list (`zend_objects_store_del`).
    let anchor = if PENDING_SEQ.try_with(Cell::get).unwrap_or(0) == seq {
        None
    } else {
        last_pending_handle()
    };
    release_store_slot(handle, anchor);
}

/// `zend_object_std_dtor` of an object that holds something: the
/// properties (and objects they hold) are released before
/// `zend_objects_store_del` puts the handle on the free list, so that
/// handle is the next one reused: the declared slots in order, then the
/// dynamic table, the native state and the stale copies of a lazy
/// object's slots, everything taken out before anything is released.
#[inline(never)]
fn free_contents(d: &mut ObjectData, slots: &mut [Option<Value>], flags: u32) {
    let dynamic = d.dynamic.get_mut().take();
    let internal = d.internal.get_mut().take();
    let stale = if flags & oflags::STALE_PROPS == 0 {
        None
    } else {
        crate::lazy::take_stale_slots(d.handle.get())
    };
    for slot in slots {
        let v = slot.take();
        // A scalar owns nothing: forgetting it skips the out-of-line
        // `drop_in_place` call (`zval_ptr_dtor` of a non-refcounted zval).
        if owns_nothing(&v) {
            std::mem::forget(v);
        } else {
            // `i_zval_ptr_dtor` of the property: a value that stays
            // referenced (an object in a cycle of its own, an array held
            // elsewhere) becomes a possible root, as when a variable
            // holding it is released. Without it the children of a freed
            // owner that refer to each other were never offered to the
            // collector: `MediaWiki`'s test suite kept 3.8 GB where
            // php-src keeps 1.2 GB.
            if let Some(val) = &v {
                crate::gc::possible_root(val);
            }
            drop(v);
        }
    }
    // The dynamic properties likewise (`zend_array_destroy` of the
    // property table).
    if let Some(table) = &dynamic {
        for val in table.values() {
            crate::gc::possible_root(val);
        }
    }
    // The native state (`free_obj` of the class: a closure's bound
    // variables, an `ArrayObject`'s storage).
    if let Some(state) = &internal {
        crate::gc::root_native_state(&**state);
    }
    drop((dynamic, internal, stale));
}

/// The tail of `zend_objects_store_del`: the store slot of `handle` is
/// cleared and the handle goes on the free list (or waits behind
/// `anchor`, see [`ObjectStore::release_handle`]).
#[inline]
fn release_store_slot(handle: u32, anchor: Option<u32>) {
    // `try_with`: the store may already be gone during thread teardown.
    let _ = STORE.try_with(|s| {
        if let Ok(mut s) = s.try_borrow_mut() {
            let s = &mut *s;
            let Some(slot) = s.objects.get_mut((handle as usize).wrapping_sub(1)) else {
                // An object of a store emptied since (a request reset that
                // an aborted shutdown left objects behind for): its handle
                // is none of this store's.
                return;
            };
            *slot = None;
            if anchor.is_none() && s.followers.is_empty() {
                // `ZEND_OBJECTS_STORE_ADD_TO_FREE_LIST`: the head of the
                // free list. `ObjectStore::grow` keeps room for every
                // handle, so the push never allocates.
                s.free.push(handle);
            } else {
                s.release_handle(handle, anchor);
            }
        }
    });
}

#[cfg(debug_assertions)]
thread_local! {
    /// How many objects [`reuse_for_new`] made in a block at hand.
    static REUSES: Cell<u64> = const { Cell::new(0) };
}

/// How many objects `new` made in a block at hand on this thread
/// ([`reuse_for_new`]): nothing a script can tell, so the tests of it ask
/// here. Counted in debug builds only; 0 otherwise.
#[must_use]
pub fn reuse_count() -> u64 {
    #[cfg(debug_assertions)]
    {
        REUSES.with(Cell::get)
    }
    #[cfg(not(debug_assertions))]
    {
        0
    }
}

/// What [`reuse_for_new`] leaves to its caller: the handle the block
/// had, and whether that object was buffered as a possible root
/// (`gc::reused` then records what php-src's order of events leaves in
/// the root buffer).
pub(crate) struct Reused {
    pub(crate) old: u32,
    pub(crate) buffered: bool,
}

/// The bytes of an object's block before its slots: the two counts of its
/// `Rc`, the cell's header and the fixed part.
const BLOCK_BASE: usize = 2 * std::mem::size_of::<usize>()
    + php_runtime::value::obj_payload_offset::<ObjectData>()
    + std::mem::size_of::<ObjectData>();

/// The most slots of an object [`reuse_for_new`] takes: up to here a
/// block has room for exactly its slots (`create`'s exact size classes),
/// so its size follows from their number.
const REUSE_SLOTS: usize = 8;

/// The block of an object with `n` slots, as the allocator was asked for
/// it and will be told when the block is freed (the test
/// `block_layout_is_the_allocation` compares it with a live object's).
/// `None` past [`REUSE_SLOTS`].
#[inline]
fn block_layout(n: usize) -> Option<std::alloc::Layout> {
    if n > REUSE_SLOTS {
        return None;
    }
    std::alloc::Layout::from_size_align(
        BLOCK_BASE + n * std::mem::size_of::<Option<Value>>(),
        std::mem::align_of::<usize>(),
    )
    .ok()
}

/// `$v = new C(...)` where `o` (its data `d`), what `$v` holds, is the
/// last handle of an object of `C` itself with nothing beside its
/// declared slots: the new object is made in that block, which `$v` goes
/// on holding, instead of making an object and freeing this one. Nothing
/// a script can see differs:
///
/// - `o` is the last handle of an object of `C` (the caller's test: the
///   class is the object's own, `d.class`), with nothing
///   beside its declared slots (no dynamic properties, native state or
///   lazy state), no slot unset or holding anything to release but a
///   string ([`releases_quietly`]: it is released then), no mark of
///   a side table kept by its handle (`oflags`) other than a possible
///   root's, and no weak handle but the store's and that root's (a
///   `WeakReference` or a `WeakMap` key would see it die).
/// - Its class has no destructor.
/// - The store hands out and takes back handles the plain way (none
///   waiting behind a queued destructor, reuse not suspended). In
///   php-src's order, `zend_objects_store_put` of the new object then
///   `zend_objects_store_del` of the old one: the block takes the next
///   handle and the old one goes back on the free list after it.
/// - A block of this size taken and freed would be an ordinary small
///   allocation under the limit (`php_memlimit::transient`, which leaves
///   the peak where that allocation would have put it).
/// - When a root entry is involved (the new object becomes a possible
///   root at its constructor's return, or the old one is buffered), the
///   caller calls `gc::reused` with what is returned.
///
/// The slots are then the class's `defaults` and what `fill` stores (a
/// constructor's stores). `None`, nothing changed, otherwise.
#[inline(never)]
pub(crate) fn reuse_for_new(
    o: &Obj,
    d: &ObjectData,
    defaults: &[Option<Value>],
    fill: impl FnOnce(&mut [Option<Value>]),
) -> Option<Reused> {
    let cls = &d.class;
    let flags = d.flags.get();
    let buffered = flags & oflags::GC_BUFFERED != 0;
    let n = d.slots.len();
    if flags & !oflags::GC_BUFFERED != 0
        || d.unset_bits.get() != 0
        || n != defaults.len()
        || o.refcount() != 1
        || Rc::weak_count(o.rc()) != 1 + usize::from(buffered)
        || cls.has_destructor()
        || d.dynamic.try_borrow().map_or(true, |t| t.is_some())
        || d.internal.try_borrow().map_or(true, |t| t.is_some())
        || d.lazy.try_borrow().map_or(true, |t| t.is_some())
    {
        return None;
    }
    let mut slots = d.slots.try_borrow_mut().ok()?;
    if !slots.iter().all(releases_quietly) {
        return None;
    }
    let layout = block_layout(n)?;
    let old = d.handle.get();
    let new = STORE
        .try_with(|s| {
            let mut s = s.try_borrow_mut().ok()?;
            let s = &mut *s;
            let at = (old as usize).checked_sub(1)?;
            if s.no_reuse
                || !s.followers.is_empty()
                || !matches!(s.objects.get(at), Some(Some(_)))
                // (Asked last: it leaves the peak where the allocation
                // of a new block would have.)
                || !php_memlimit::transient(layout)
            {
                return None;
            }
            let new = s.take_handle();
            // The weak handle moves with the block: the slot of a handle
            // just taken holds nothing.
            s.objects.swap(at, (new as usize).checked_sub(1)?);
            // (`ObjectStore::grow` keeps room for every handle.)
            s.free.push(old);
            Some(new)
        })
        .ok()
        .flatten()?;
    d.handle.set(new);
    for (slot, default) in slots.iter_mut().zip(defaults) {
        if !owns_nothing(slot) {
            // A string (`releases_quietly`): released here rather than when
            // the old object would have been freed, after the new one was
            // made. Nothing runs in between, and the peak is where the
            // allocation of the new block would have put it (above).
            drop(std::mem::replace(slot, default.clone()));
        } else if owns_nothing(default) {
            // SAFETY: both are valid, aligned and distinct (the class's
            // table and the object's slot). A value that owns nothing is
            // its 16 bytes, so they are its copy; what the slot held owns
            // nothing either (checked above), so no drop is skipped. (The
            // safe form, `default.clone()`, was measured first: a jump on
            // the tag for every slot, 22 instructions for an int.)
            unsafe { ptr::copy_nonoverlapping(default, slot, 1) };
        } else {
            std::mem::forget(std::mem::replace(slot, default.clone()));
        }
    }
    fill(&mut slots);
    #[cfg(debug_assertions)]
    REUSES.with(|c| c.set(c.get() + 1));
    Some(Reused { old, buffered })
}

/// `object_properties_init`: the slots of a new object, copied from its
/// class's evaluated default properties (`ce->default_properties_table`).
#[inline]
#[must_use]
pub fn copy_slots(defaults: &[Option<Value>]) -> Vec<Option<Value>> {
    let mut slots = Vec::with_capacity(defaults.len());
    slots.extend_from_slice(defaults);
    slots
}

/// Allocates a handle and creates an object of `class` with the given slot
/// values (`zend_objects_new` + `zend_objects_store_put`): one allocation,
/// the slots moved in after the object's fixed part.
pub fn new_object(class: Rc<Class>, slots: Vec<Option<Value>>) -> Obj {
    create(class, Fill::Values(slots))
}

/// [`new_object`] with the slots copied straight from `defaults`
/// (`object_properties_init` from `ce->default_properties_table`), with no
/// vector in between: `new` of a class whose defaults are evaluated.
pub fn new_object_from_defaults(class: Rc<Class>, defaults: &[Option<Value>]) -> Obj {
    // The exact size classes of `create`, each with its own function.
    macro_rules! exact {
        ($($n:literal)*) => {
            match defaults.len() {
                $($n => {
                    if let Ok(d) = <&[Option<Value>; $n]>::try_from(defaults) {
                        return new_defaults::<$n>(class, d);
                    }
                })*
                _ => {}
            }
        };
    }
    exact!(1 2 3 4 5 6 7 8);
    create(class, Fill::Defaults(defaults))
}

/// [`new_block`] for the commonest `new`: an object whose `N` slots are
/// copies of its class's default properties (`zend_objects_new` +
/// `object_properties_init`), `N` an exact size class of [`create`].
/// Straight-line: the block is allocated, the defaults are copied in one
/// move of a size the compiler knows (a default that owns something is
/// then replaced by a real clone, as in [`Fill::write`]), the fixed part
/// is written and the handle's store slot filled. (`new_block` hands the
/// class, the fill and the buffer around as closure captures: 172
/// instructions for an object of one slot, a `memcpy` call for its 16
/// bytes among them.)
#[inline(never)]
fn new_defaults<const N: usize>(class: Rc<Class>, defaults: &[Option<Value>; N]) -> Obj {
    STORE.with(|s| {
        let mut s = s.borrow_mut();
        let handle = s.take_handle();
        // SAFETY: as in `new_block`: `ObjectBlock` is `repr(C)` with its
        // `ObjectData` first and aligned like it, and the closure
        // initializes the whole block (all `N` inline slots: bitwise
        // copies of the defaults, complete for those that own nothing;
        // the others are overwritten with clones before anything reads
        // them, the copies never dropped).
        let o = unsafe {
            Obj::new_in_family_with::<ObjectBlock<N>, ObjectData>(|p| {
                let block = p.as_ptr();
                let inline = ptr::addr_of_mut!((*block).inline).cast::<Option<Value>>();
                ptr::copy_nonoverlapping(defaults.as_ptr(), inline, N);
                for (k, v) in defaults.iter().enumerate() {
                    if !owns_nothing(v) {
                        inline.add(k).write(v.clone());
                    }
                }
                ptr::addr_of_mut!((*block).data).write(ObjectData {
                    handle: Cell::new(handle),
                    class,
                    slots: Slots::new(NonNull::new_unchecked(inline), N),
                    dynamic: RefCell::new(None),
                    internal: RefCell::new(None),
                    flags: Cell::new(0),
                    unset_bits: Cell::new(0),
                    lazy: RefCell::new(None),
                });
            })
        };
        s.objects[handle as usize - 1] = Some(o.downgrade());
        o
    })
}

/// Where the slot values of a new object come from.
enum Fill<'a> {
    /// Moved out of a vector.
    Values(Vec<Option<Value>>),
    /// Copies of the class's default properties.
    Defaults(&'a [Option<Value>]),
}

impl Fill<'_> {
    fn len(&self) -> usize {
        match self {
            Fill::Values(values) => values.len(),
            Fill::Defaults(values) => values.len(),
        }
    }

    /// Writes the values into `dst`, which has room for exactly
    /// [`Fill::len`] of them. Inlined into each [`new_block`], where the
    /// length is known to fit `N`: the out-of-line call cost more than
    /// copying the one or two slots of a small object.
    ///
    /// Both sources are copied bitwise in one block move (php-src's
    /// `object_properties_init` loop of `ZVAL_COPY_VALUE_PROP`); a copied
    /// default that owns something (a string, an array, ...) is then
    /// overwritten with a real clone (`ZVAL_COPY_OR_DUP_PROP`'s refcount
    /// increment). Should a clone panic, `dst` keeps bitwise copies, which
    /// are never dropped (`MaybeUninit`).
    #[inline(always)]
    #[allow(clippy::inline_always)] // once per `new`, see above
    fn write(self, dst: &mut [MaybeUninit<Option<Value>>]) {
        debug_assert_eq!(dst.len(), self.len());
        let len = dst.len().min(self.len());
        let out = dst.as_mut_ptr().cast::<Option<Value>>();
        match self {
            Fill::Values(mut values) => {
                // SAFETY: `values` holds `len` initialized values and
                // `dst` room for them (separate allocations); the values
                // move into `dst`: the vector forgets them (`set_len(0)`)
                // before it is dropped.
                unsafe {
                    ptr::copy_nonoverlapping(values.as_ptr(), out, len);
                    values.set_len(0);
                }
            }
            Fill::Defaults(values) => {
                // SAFETY: as above; the copies of the values that own
                // nothing are complete, the others are replaced below
                // before anything reads them.
                unsafe { ptr::copy_nonoverlapping(values.as_ptr(), out, len) };
                for (d, v) in dst.iter_mut().zip(values) {
                    if !owns_nothing(v) {
                        // No drop of the bitwise copy: `MaybeUninit`.
                        *d = MaybeUninit::new(v.clone());
                    }
                }
            }
        }
    }

    /// The values in a buffer of their own.
    fn into_boxed(self) -> Box<[Option<Value>]> {
        match self {
            Fill::Values(values) => values.into_boxed_slice(),
            Fill::Defaults(values) => values.into(),
        }
    }
}

/// Creates the object in the smallest block that holds its slots (the size
/// classes waste at most a fifth of the room above eight slots).
#[inline]
fn create(class: Rc<Class>, fill: Fill<'_>) -> Obj {
    match fill.len() {
        1 => new_block::<1>(class, fill),
        2 => new_block::<2>(class, fill),
        3 => new_block::<3>(class, fill),
        4 => new_block::<4>(class, fill),
        5 => new_block::<5>(class, fill),
        6 => new_block::<6>(class, fill),
        7 => new_block::<7>(class, fill),
        8 => new_block::<8>(class, fill),
        9..=10 => new_block::<10>(class, fill),
        11..=12 => new_block::<12>(class, fill),
        13..=16 => new_block::<16>(class, fill),
        17..=20 => new_block::<20>(class, fill),
        21..=24 => new_block::<24>(class, fill),
        25..=32 => new_block::<32>(class, fill),
        33..=40 => new_block::<40>(class, fill),
        41..=48 => new_block::<48>(class, fill),
        49..=MAX_INLINE => new_block::<MAX_INLINE>(class, fill),
        // No slots, or more than fit inline (a buffer of their own).
        _ => new_block::<0>(class, fill),
    }
}

/// [`create`] in a block with room for `N` slots (`zend_objects_new` plus
/// `object_properties_init`): the block is allocated first and built in
/// place, the values written straight into its inline slots, so nothing is
/// assembled on the stack and copied into the heap.
#[inline(never)]
fn new_block<const N: usize>(class: Rc<Class>, fill: Fill<'_>) -> Obj {
    let len = fill.len();
    let (buf, fill) = if len <= N {
        (None, Some(fill))
    } else {
        let raw = Box::into_raw(fill.into_boxed());
        // SAFETY: `Box::into_raw` never returns null.
        (
            Some(unsafe { NonNull::new_unchecked(raw.cast::<Option<Value>>()) }),
            None,
        )
    };
    STORE.with(|s| {
        let mut s = s.borrow_mut();
        let handle = s.take_handle();
        // SAFETY: `ObjectBlock` is `repr(C)` with its `ObjectData` first,
        // and aligned like it (the inline slots are 8-aligned values). The
        // closure initializes the whole block: the first `len` inline
        // slots (the rest stay `MaybeUninit`) and the fixed part.
        let o = unsafe {
            Obj::new_in_family_with::<ObjectBlock<N>, ObjectData>(|p| {
                let block = p.as_ptr();
                // `UnsafeCell` and arrays are laid out as their contents;
                // `p` reaches the whole allocation, so the slot pointer
                // keeps that provenance (see `Slots::first`).
                let inline =
                    ptr::addr_of_mut!((*block).inline).cast::<MaybeUninit<Option<Value>>>();
                let first = match (buf, fill) {
                    (Some(buf), _) => buf,
                    (None, fill) => {
                        if let Some(fill) = fill {
                            fill.write(std::slice::from_raw_parts_mut(inline, len));
                        }
                        NonNull::new_unchecked(inline.cast::<Option<Value>>())
                    }
                };
                ptr::addr_of_mut!((*block).data).write(ObjectData {
                    handle: Cell::new(handle),
                    class,
                    slots: Slots::new(first, len),
                    dynamic: RefCell::new(None),
                    internal: RefCell::new(None),
                    flags: Cell::new(0),
                    unset_bits: Cell::new(0),
                    lazy: RefCell::new(None),
                });
            })
        };
        s.objects[handle as usize - 1] = Some(o.downgrade());
        o
    })
}

/// Frees an object that `new` rejected after creating it (its
/// `get_constructor` threw: `new Generator`, `new Closure`). No destructor
/// runs for it (`zend_generator_dtor_storage` has no frame to close) and
/// its handle returns to the free list at once, as the `OBJ_RELEASE` of the
/// rejected object does in php-src.
pub(crate) fn discard_rejected(o: Obj) {
    let d = data(&o);
    d.flags.set(d.flags.get() | oflags::DESTRUCTED);
    drop(o);
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

/// Returns our object data for an `Obj` (`Z_OBJ_P`: a type check and an
/// offset into the one allocation).
///
/// Inlined everywhere: it is a type compare and an offset, and the
/// property handlers called it out of line (a call, and spills of the
/// operands around it, on every `$obj->p = v`).
///
/// # Panics
/// Panics if the object was not created by this VM.
#[inline(always)]
#[allow(clippy::inline_always)]
pub fn data(o: &Obj) -> &ObjectData {
    match o.downcast_ref::<ObjectData>() {
        Some(d) => d,
        None => not_a_vm_object(),
    }
}

/// [`data`] of an object this VM did not create.
#[cold]
#[inline(never)]
fn not_a_vm_object() -> ! {
    panic!("phprust object")
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
            // Nearly always empty: no lookup (a hash of the handle a
            // destructor) for nothing.
            if s.followers.is_empty() {
                return;
            }
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

/// Gives the emptied buffer of [`take_pending_destructors`] back to the
/// queue when nothing was queued meanwhile, so that the next destructor
/// queued does not allocate a new one (an allocation and its growth per
/// destructor when objects are freed one at a time).
pub fn return_pending_buffer(buf: Vec<Obj>) {
    if !buf.is_empty() || buf.capacity() == 0 || buf.capacity() > 256 {
        return;
    }
    let _ = PENDING.try_with(|p| {
        if let Ok(mut p) = p.try_borrow_mut() {
            if p.is_empty() && p.capacity() == 0 {
                *p = buf;
            }
        }
    });
}

/// Marks `o` weakly referenced ([`oflags::WEAKLY_REF`]).
pub(crate) fn mark_weakly_referenced(o: &Obj) {
    let d = data(o);
    d.flags.set(d.flags.get() | oflags::WEAKLY_REF);
}

/// Enables or disables destructor scheduling (disabled after fatal errors).
pub fn set_destructors_enabled(on: bool) {
    DESTRUCTORS_ENABLED.with(|c| c.set(on));
}

/// Whether destructors are enabled.
pub fn destructors_enabled() -> bool {
    DESTRUCTORS_ENABLED.with(Cell::get)
}

/// All live objects in handle order (`gc_collect_cycles()`'s scan of the
/// object store), as further handles of them.
pub fn live_objects() -> Vec<Obj> {
    STORE.with(|s| {
        s.borrow()
            .objects
            .iter()
            .filter_map(|w| w.as_ref().and_then(ObjWeak::upgrade))
            .collect()
    })
}

/// The number of handle slots in the store (`objects->top - 1`).
pub fn store_len() -> usize {
    STORE.with(|s| s.borrow().objects.len())
}

/// The live object in store slot `i` (handle `i + 1`), if any, as the
/// `Obj` every other holder has (same identity and refcount).
pub fn live_obj_at(i: usize) -> Option<Obj> {
    STORE.with(|s| {
        s.borrow()
            .objects
            .get(i)
            .and_then(|w| w.as_ref().and_then(ObjWeak::upgrade))
    })
}

/// Suspends (`true`) or restores handle reuse
/// (`EG_FLAGS_OBJECT_STORE_NO_REUSE`, set by
/// `zend_objects_store_call_destructors`).
pub fn set_handle_no_reuse(on: bool) {
    STORE.with(|s| s.borrow_mut().no_reuse = on);
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
    STALE_COUNTS.with(|s| s.borrow_mut().push(d.handle.get()));
    true
}

/// Ends the window opened by [`begin_stale_count`]: the table is flagged
/// `HASH_FLAG_HAS_EMPTY_IND`, so counts are exact from now on.
pub fn end_stale_count(d: &ObjectData) {
    let _ = STALE_COUNTS.try_with(|s| {
        if let Ok(mut s) = s.try_borrow_mut() {
            if let Some(p) = s.iter().rposition(|&h| h == d.handle.get()) {
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
    PROP_ORDERS.with(|m| m.borrow().get(&d.handle.get()).cloned())
}

/// Gives an object its own property order (`None` restores the default:
/// declaration order, then dynamic properties).
pub fn set_prop_order(d: &ObjectData, order: Option<PropOrder>) {
    match order {
        Some(order) => install_prop_order(d, Rc::new(order)),
        None => {
            if d.flags.get() & oflags::PROP_ORDER != 0 {
                d.flags.set(d.flags.get() & !oflags::PROP_ORDER);
                let _ = PROP_ORDERS.try_with(|m| m.borrow_mut().remove(&d.handle.get()));
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
        if let Some(order) = m.borrow_mut().get_mut(&d.handle.get()) {
            Rc::make_mut(order).dynamic.remove(key);
        }
    });
}

fn install_prop_order(d: &ObjectData, order: Rc<PropOrder>) {
    d.flags.set(d.flags.get() | oflags::PROP_ORDER);
    PROP_ORDERS.with(|m| {
        let mut m = m.borrow_mut();
        if !m.contains_key(&d.handle.get()) && m.len() >= PROP_ORDERS_PRUNE_AT.with(Cell::get) {
            prune_prop_orders(&mut m);
        }
        m.insert(d.handle.get(), order);
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

    /// [`block_layout`] is what the allocator was asked for an object of
    /// that many slots: the layout of its `Rc`'s block, counts and cell.
    #[test]
    fn block_layout_is_the_allocation() {
        let vm = crate::Vm::new(&crate::VmOptions::default(), Box::new(std::io::sink()));
        let cls = vm.find_class(b"stdClass").expect("stdClass");
        for n in 0..=REUSE_SLOTS {
            let defaults = vec![Some(Value::Int(1)); n];
            for o in [
                new_object(cls.clone(), defaults.clone()),
                new_object_from_defaults(cls.clone(), &defaults),
            ] {
                let cell = std::alloc::Layout::for_value::<dyn PhpObject>(&**o.rc());
                let (block, _) = std::alloc::Layout::new::<[usize; 2]>()
                    .extend(cell)
                    .expect("a layout");
                assert_eq!(block_layout(n), Some(block.pad_to_align()), "{n} slots");
            }
        }
        assert_eq!(block_layout(REUSE_SLOTS + 1), None);
    }

    fn key(s: &str) -> ArrayKey {
        ArrayKey::Str(ZStr::from(s))
    }

    /// One allocation per object: php-runtime's `Rc` counts (16 B), the
    /// `Obj` header (vtable `Rc` + `TypeId`, 32 B), this fixed part, then
    /// 16 B per declared slot (a zval's size).
    #[test]
    fn object_data_is_a_compact_cell_payload() {
        use std::mem::{align_of, offset_of, size_of};
        assert!(size_of::<ObjectData>() <= 104);
        assert_eq!(align_of::<ObjectData>(), 8);
        assert_eq!(size_of::<Option<Value>>(), 16);
        assert_eq!(size_of::<Slots>(), 24);
        assert_eq!(offset_of!(ObjectBlock<3>, data), 0);
        assert_eq!(offset_of!(ObjectBlock<3>, inline), size_of::<ObjectData>());
        assert_eq!(
            size_of::<ObjectBlock<3>>(),
            size_of::<ObjectData>() + 3 * 16
        );
        assert_eq!(size_of::<ObjectBlock<0>>(), size_of::<ObjectData>());
        assert_eq!(align_of::<ObjectBlock<64>>(), align_of::<ObjectData>());
    }

    /// `len` slots over `storage` (which must outlive them).
    fn slots_over(storage: &mut [Option<Value>]) -> Slots {
        let len = storage.len();
        Slots::new(NonNull::from(storage).cast(), len)
    }

    #[test]
    fn slots_borrow_like_a_refcell() {
        let mut storage = [Some(Value::Int(1)), None, Some(Value::Int(3))];
        let slots = slots_over(&mut storage);
        assert_eq!((slots.len(), slots.is_empty()), (3, false));
        {
            let a = slots.borrow();
            let b = slots.try_borrow().expect("shared borrows stack");
            assert!(slots.try_borrow_mut().is_err());
            assert_eq!(a.len(), 3);
            assert!(matches!(b[0], Some(Value::Int(1))));
            assert!(b.get(3).is_none());
            // `clone()` copies the values out, as `Ref<Vec<_>>` did.
            let copy: Vec<Option<Value>> = a.clone();
            assert_eq!(copy.len(), 3);
        }
        {
            let mut m = slots.borrow_mut();
            assert!(slots.try_borrow().is_err());
            assert!(slots.try_borrow_mut().is_err());
            m[1] = Some(Value::Int(2));
            if let Some(Some(Value::Int(n))) = m.get_mut(2) {
                *n = 30;
            }
            m.swap(0, 2);
        }
        let values: Vec<i64> = slots
            .borrow()
            .iter()
            .map(|s| match s {
                Some(Value::Int(n)) => *n,
                _ => -1,
            })
            .collect();
        assert_eq!(values, [30, 2, 1]);
        assert!(slots.try_borrow_mut().is_ok());
        drop(slots);
        assert!(matches!(storage[1], Some(Value::Int(2))));
    }

    #[test]
    #[should_panic(expected = "already borrowed")]
    fn slots_borrow_mut_while_borrowed_panics() {
        let mut storage = [None];
        let slots = slots_over(&mut storage);
        let _shared = slots.borrow();
        let _ = slots.borrow_mut();
    }

    #[test]
    #[should_panic(expected = "already mutably borrowed")]
    fn slots_borrow_while_mutably_borrowed_panics() {
        let mut storage = [None];
        let slots = slots_over(&mut storage);
        let _exclusive = slots.borrow_mut();
        let _ = slots.borrow();
    }

    /// Objects of every size class (inline up to `MAX_INLINE`, then a
    /// buffer of their own) keep their values, take writes, copy their
    /// class defaults and release everything when freed.
    #[test]
    fn slots_live_inline_or_in_their_own_buffer() {
        let vm = crate::Vm::new(&crate::VmOptions::default(), Box::new(std::io::sink()));
        let cls = vm.find_class(b"stdClass").expect("stdClass");
        // A table of its own (`ZArray::new()` shares the empty one).
        let shared = php_runtime::ZArray::from_values([Value::Int(1)]);
        for n in [0usize, 1, 2, 8, 9, 13, 48, 49, 64, 65, 300] {
            let values: Vec<Option<Value>> = (0..n)
                .map(|i| match i % 3 {
                    0 => None,
                    1 => Some(Value::Int(i64::try_from(i).unwrap_or(0))),
                    _ => Some(Value::Array(shared.clone())),
                })
                .collect();
            let held = n / 3;
            assert_eq!(shared.refcount(), 1 + held);
            let o = new_object(cls.clone(), values);
            let d = data(&o);
            assert_eq!(d.slots.len(), n);
            assert_eq!(shared.refcount(), 1 + held);
            let payload = o.payload_ptr::<ObjectData>().expect("object");
            let first = d.slots.borrow().as_ptr() as usize;
            let inline = payload.as_ptr() as usize + std::mem::size_of::<ObjectData>();
            if (1..=MAX_INLINE).contains(&n) {
                assert_eq!(first, inline, "{n} slots inline");
            } else if n > MAX_INLINE {
                assert_ne!(first, inline, "{n} slots in a buffer");
            }
            for (i, s) in d.slots.borrow().iter().enumerate() {
                match i % 3 {
                    0 => assert!(s.is_none()),
                    1 => assert!(
                        matches!(s, Some(Value::Int(v)) if *v == i64::try_from(i).unwrap_or(0))
                    ),
                    _ => assert!(matches!(s, Some(Value::Array(_)))),
                }
            }
            if n > 0 {
                d.slots.borrow_mut()[n - 1] = Some(Value::Int(-7));
                assert!(matches!(d.slots.borrow()[n - 1], Some(Value::Int(-7))));
            }
            let held = held - usize::from(n > 0 && (n - 1) % 3 == 2);
            assert_eq!(shared.refcount(), 1 + held);
            // A copy from the object's slots as class defaults.
            let defaults = d.slots.borrow().clone();
            let copy = new_object_from_defaults(cls.clone(), &defaults);
            drop(defaults);
            assert_eq!(shared.refcount(), 1 + 2 * held);
            assert_eq!(data(&copy).slots.len(), n);
            if n > 0 {
                data(&copy).slots.borrow_mut()[0] = Some(Value::Int(5));
                assert!(!matches!(d.slots.borrow()[0], Some(Value::Int(5))));
            }
            drop(o);
            assert_eq!(shared.refcount(), 1 + held);
            drop(copy);
            assert_eq!(shared.refcount(), 1);
        }
        drop(vm);
    }

    /// Objects whose slots own nothing take the short free; either way the
    /// freed handles are reused last-freed first (Zend's free list), and
    /// defaults that own something are real copies.
    #[test]
    fn freed_handles_are_reused_last_freed_first() {
        let vm = crate::Vm::new(&crate::VmOptions::default(), Box::new(std::io::sink()));
        let cls = vm.find_class(b"stdClass").expect("stdClass");
        let shared = php_runtime::ZArray::from_values([Value::Int(1)]);
        let plain = [None, Some(Value::Int(3)), Some(Value::Float(0.5))];
        let owning = [Some(Value::Bool(true)), Some(Value::Array(shared.clone()))];
        let a = new_object_from_defaults(cls.clone(), &plain);
        let b = new_object_from_defaults(cls.clone(), &owning);
        assert_eq!(shared.refcount(), 3);
        assert!(matches!(data(&a).slots.borrow()[1], Some(Value::Int(3))));
        assert!(matches!(
            data(&b).slots.borrow()[0],
            Some(Value::Bool(true))
        ));
        let (ha, hb) = (a.handle(), b.handle());
        assert_ne!(ha, hb);
        drop(a);
        drop(b);
        assert_eq!(shared.refcount(), 2);
        let c = new_object_from_defaults(cls.clone(), &plain);
        let d = new_object(cls.clone(), Vec::new());
        assert_eq!((c.handle(), d.handle()), (hb, ha));
        drop((c, d, owning));
        assert_eq!(shared.refcount(), 1);
        drop(vm);
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

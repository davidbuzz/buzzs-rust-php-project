//! PHP references (`zend_reference`): the shared mutable slot behind `&`.
//!
//! php-src's `zend_reference` is 32 bytes: the refcounted header (refcount
//! and type info, 8), the `zval` (16) and `sources` (8), a tagged pointer to
//! the typed properties the reference is bound to. [`PhpRef`] is one thin
//! pointer to a [`RefBox`] of 40 bytes: a `u32` strong count and a `u32`
//! weak count (8), the value in a `RefCell` (24: its borrow flag is the one
//! word php-src does not have, and what makes [`PhpRef::borrow_mut`] and
//! [`PhpRef::try_borrow`] checked) and the VM's metadata, the type sources,
//! as one thin pointer (8).
//!
//! The counts are kept by hand rather than by `Rc`, which spends two words
//! on them. The weak count exists for the cycle collector's root buffer
//! ([`crate::ref_roots`]), which holds possible roots without keeping them
//! alive, like php-src's root buffer holds them by address.

use crate::value::Value;
use std::any::Any;
use std::cell::{Cell, Ref, RefCell, RefMut};
use std::fmt;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;
use std::rc::Rc;

/// The payload of a reference: what the strong handles own.
struct RefInner {
    value: RefCell<Value>,
    /// Opaque slot for the VM (the typed-property type sources,
    /// `ZEND_REF_HAS_TYPE_SOURCES`). The fat `Rc<dyn Any>` is boxed so that
    /// the slot is one word, like php-src's `sources` pointer; the box is
    /// only paid for by typed references.
    meta: Cell<Option<Box<Rc<dyn Any>>>>,
}

/// The allocation behind a [`PhpRef`] (`zend_reference`).
///
/// `repr(C)` fixes the layout: the two counts (4 + 4), then the 32-byte
/// payload, 40 bytes in all.
#[repr(C)]
struct RefBox {
    /// Number of [`PhpRef`] handles (`GC_REFCOUNT`).
    strong: Cell<u32>,
    /// Number of [`WeakRef`] handles, plus one held by the strong handles
    /// collectively while any exists (as in `Rc`).
    weak: Cell<u32>,
    /// Dropped when the last strong handle goes; the allocation itself stays
    /// until the last weak handle goes.
    payload: ManuallyDrop<RefInner>,
}

/// A PHP reference (`zend_reference`): a shared mutable slot holding a value.
///
/// Cloning a `PhpRef` creates another handle to the *same* slot (it is how
/// `$a = &$b` is represented); [`PhpRef::refcount`] reports the number of
/// handles, which `var_dump` uses to decide whether to print `&`.
///
/// The handle is one thin pointer (8 bytes, with a niche for `Option`),
/// like the `zend_reference *` in a zval. Like `Rc`, it is neither `Send`
/// nor `Sync`.
pub struct PhpRef {
    ptr: NonNull<RefBox>,
    _owns: PhantomData<RefBox>,
}

impl PhpRef {
    /// Creates a new reference holding `value`. A reference never directly
    /// contains another reference: nested references are unwrapped.
    #[must_use]
    pub fn new(value: Value) -> Self {
        let value = match value {
            Value::Reference(r) => r.get(),
            v => v,
        };
        let b = Box::new(RefBox {
            strong: Cell::new(1),
            weak: Cell::new(1),
            payload: ManuallyDrop::new(RefInner {
                value: RefCell::new(value),
                meta: Cell::new(None),
            }),
        });
        PhpRef {
            ptr: NonNull::from(Box::leak(b)),
            _owns: PhantomData,
        }
    }

    #[inline]
    fn inner(&self) -> &RefBox {
        // SAFETY: this handle owns one strong count, so the allocation is
        // alive and its payload initialised. The payload is only dropped
        // once the strong count is zero, when no handle exists to get here,
        // and nothing ever holds a `&mut` to a live payload (its cells give
        // out guards, not references to the box).
        unsafe { self.ptr.as_ref() }
    }

    #[inline]
    fn payload(&self) -> &RefInner {
        &self.inner().payload
    }

    /// Returns a clone of the referenced value.
    #[inline]
    #[must_use]
    pub fn get(&self) -> Value {
        self.payload().value.borrow().clone()
    }

    /// Replaces the referenced value, returning the old one.
    #[inline]
    pub fn set(&self, value: Value) -> Value {
        let value = match value {
            Value::Reference(r) if !PhpRef::ptr_eq(&r, self) => r.get(),
            Value::Reference(_) => return self.get(),
            v => v,
        };
        std::mem::replace(&mut *self.payload().value.borrow_mut(), value)
    }

    /// Borrows the referenced value.
    ///
    /// # Panics
    /// Panics if the value is currently mutably borrowed.
    #[inline]
    #[must_use]
    pub fn borrow(&self) -> Ref<'_, Value> {
        self.payload().value.borrow()
    }

    /// Mutably borrows the referenced value.
    ///
    /// # Panics
    /// Panics if the value is currently borrowed.
    #[inline]
    #[must_use]
    pub fn borrow_mut(&self) -> RefMut<'_, Value> {
        self.payload().value.borrow_mut()
    }

    /// Borrows the referenced value unless it is currently mutably borrowed.
    #[inline]
    #[must_use]
    pub fn try_borrow(&self) -> Option<Ref<'_, Value>> {
        self.payload().value.try_borrow().ok()
    }

    /// Mutably borrows the referenced value unless it is currently borrowed.
    #[inline]
    #[must_use]
    pub fn try_borrow_mut(&self) -> Option<RefMut<'_, Value>> {
        self.payload().value.try_borrow_mut().ok()
    }

    /// Whether two handles denote the same reference.
    #[inline]
    #[must_use]
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        a.ptr == b.ptr
    }

    /// Number of handles to this reference (`GC_REFCOUNT`).
    #[inline]
    #[must_use]
    pub fn refcount(&self) -> usize {
        self.inner().strong.get() as usize
    }

    /// An address usable as an identity key (for recursion detection and
    /// serialization back-references).
    #[inline]
    #[must_use]
    pub fn addr(&self) -> usize {
        self.ptr.as_ptr().cast::<u8>() as usize
    }

    /// The VM's metadata attached to this reference, if any.
    #[inline]
    #[must_use]
    pub fn meta(&self) -> Option<Rc<dyn Any>> {
        let cell = &self.payload().meta;
        let m = cell.take();
        let out = m.as_deref().cloned();
        cell.set(m);
        out
    }

    /// Attaches (or clears) VM metadata.
    pub fn set_meta(&self, meta: Option<Rc<dyn Any>>) {
        self.payload().meta.set(meta.map(Box::new));
    }

    /// A weak handle (for the cycle collector's root buffer).
    pub(crate) fn downgrade(&self) -> WeakRef {
        let weak = &self.inner().weak;
        let n = weak.get();
        if n == u32::MAX {
            std::process::abort();
        }
        weak.set(n + 1);
        WeakRef { ptr: self.ptr }
    }
}

impl Clone for PhpRef {
    #[inline]
    fn clone(&self) -> Self {
        let strong = &self.inner().strong;
        let n = strong.get();
        // Like `Rc`: a count that wrapped would free the reference while
        // handles to it remain.
        if n == u32::MAX {
            std::process::abort();
        }
        strong.set(n + 1);
        PhpRef {
            ptr: self.ptr,
            _owns: PhantomData,
        }
    }
}

/// Releasing a handle while others remain (`GC_DELREF` to a non-zero
/// count) makes the reference a possible cycle root
/// (`gc_check_possible_root`). Releasing the last one frees the value.
impl Drop for PhpRef {
    #[inline]
    fn drop(&mut self) {
        if self.inner().strong.get() > 1 {
            // May run the collector, which may release other handles of
            // this reference: the count is read again below. This handle
            // still holds its count meanwhile, so the allocation stays.
            crate::ref_roots::released(self);
        }
        let strong = &self.inner().strong;
        let n = strong.get() - 1;
        strong.set(n);
        if n == 0 {
            // SAFETY: this was the last strong handle, and `self` is not
            // used afterwards.
            unsafe { release_last(self.ptr) }
        }
    }
}

impl fmt::Debug for PhpRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PhpRef({:?})", self.get())
    }
}

/// Frees the payload of the allocation at `ptr`, whose strong count has just
/// reached zero, and releases the strong handles' collective weak count.
///
/// # Safety
/// `ptr` must be a live reference allocation whose strong count is zero and
/// whose payload has not been dropped yet (this runs once per allocation).
#[inline(never)]
unsafe fn release_last(ptr: NonNull<RefBox>) {
    // SAFETY: no strong handle is left, so nothing else reaches the payload;
    // weak handles only touch the counts (`upgrade` sees the zero strong
    // count). The strong handles' weak count keeps the allocation alive
    // while the payload drops, so the PHP destructors that drop may run can
    // clone, upgrade and drop weak handles to it freely. The `&mut` covers
    // the payload field only, as `Rc`'s does.
    unsafe { ManuallyDrop::drop(&mut (*ptr.as_ptr()).payload) };
    // SAFETY: the strong handles' collective weak count, released exactly
    // once (a strong count never comes back from zero).
    unsafe { release_weak(ptr) };
}

/// Releases one weak count of the allocation at `ptr`, freeing the
/// allocation when it was the last.
///
/// # Safety
/// `ptr` must be a live reference allocation of which the caller owns one
/// weak count. When the count reaches zero the payload must already have
/// been dropped, which holds because the strong handles' collective weak
/// count is released only after it ([`release_last`]).
unsafe fn release_weak(ptr: NonNull<RefBox>) {
    // SAFETY: the allocation is alive (caller contract); a reference to the
    // count field only, so this is fine while the payload is being dropped.
    let weak = unsafe { &(*ptr.as_ptr()).weak };
    let n = weak.get() - 1;
    weak.set(n);
    if n == 0 {
        // SAFETY: no handle of any kind is left and the payload is dropped;
        // the box was allocated by `Box::new` in `PhpRef::new`, and
        // `ManuallyDrop` keeps `Box`'s drop glue off the payload.
        drop(unsafe { Box::from_raw(ptr.as_ptr()) });
    }
}

/// A weak handle to a reference (the cycle collector's root buffer): it
/// does not keep the reference alive, and [`WeakRef::upgrade`] returns a
/// handle to it while it is.
///
/// A weak handle only touches the two counts, never the payload, so it may
/// be used (cloned, dropped, upgraded) while the payload is being dropped,
/// which PHP destructors running then may do through the root buffer.
pub(crate) struct WeakRef {
    ptr: NonNull<RefBox>,
}

impl WeakRef {
    #[inline]
    fn strong(&self) -> &Cell<u32> {
        // SAFETY: this handle owns one weak count, so the allocation is
        // alive; a reference to the count field only (see the type's doc).
        unsafe { &(*self.ptr.as_ptr()).strong }
    }

    #[inline]
    fn weak(&self) -> &Cell<u32> {
        // SAFETY: as in `strong`.
        unsafe { &(*self.ptr.as_ptr()).weak }
    }

    /// The reference, if it is still alive.
    #[must_use]
    pub(crate) fn upgrade(&self) -> Option<PhpRef> {
        let strong = self.strong();
        let n = strong.get();
        if n == 0 {
            return None;
        }
        if n == u32::MAX {
            std::process::abort();
        }
        strong.set(n + 1);
        Some(PhpRef {
            ptr: self.ptr,
            _owns: PhantomData,
        })
    }

    /// Number of strong handles to the reference (`0` once it is freed).
    #[must_use]
    pub(crate) fn strong_count(&self) -> usize {
        self.strong().get() as usize
    }

    /// The [`PhpRef::addr`] of the reference (valid as a key even after the
    /// reference is freed, while this handle exists).
    #[must_use]
    pub(crate) fn addr(&self) -> usize {
        self.ptr.as_ptr().cast::<u8>() as usize
    }
}

impl Clone for WeakRef {
    fn clone(&self) -> Self {
        let weak = self.weak();
        let n = weak.get();
        if n == u32::MAX {
            std::process::abort();
        }
        weak.set(n + 1);
        WeakRef { ptr: self.ptr }
    }
}

impl Drop for WeakRef {
    fn drop(&mut self) {
        // SAFETY: this handle owns one weak count.
        unsafe { release_weak(self.ptr) }
    }
}

impl fmt::Debug for WeakRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WeakRef({:#x})", self.addr())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{Obj, PhpObject};
    use crate::ZStr;
    use std::mem::{align_of, size_of};

    /// php-src's `zend_reference` is 32 bytes (refcounted header 8, zval 16,
    /// sources 8). Ours is 40: the extra word is the `RefCell` borrow flag
    /// that makes borrowing through a reference checked.
    #[test]
    fn reference_allocation_is_40_bytes() {
        assert_eq!(size_of::<PhpRef>(), 8);
        assert_eq!(size_of::<Option<PhpRef>>(), 8);
        assert_eq!(size_of::<WeakRef>(), 8);
        assert_eq!(size_of::<Option<WeakRef>>(), 8);
        assert_eq!(size_of::<RefCell<Value>>(), 24);
        assert_eq!(size_of::<Cell<Option<Box<Rc<dyn Any>>>>>(), 8);
        assert_eq!(size_of::<RefInner>(), 32);
        assert_eq!(size_of::<RefBox>(), 40);
        assert_eq!(align_of::<RefBox>(), 8);
    }

    #[test]
    fn handles_share_one_slot_and_count() {
        let r = PhpRef::new(Value::Int(1));
        assert_eq!(r.refcount(), 1);
        let r2 = r.clone();
        assert_eq!(r.refcount(), 2);
        assert!(PhpRef::ptr_eq(&r, &r2));
        assert_eq!(r.addr(), r2.addr());
        r2.set(Value::Int(5));
        assert_eq!(r.get().as_int(), Some(5));
        drop(r2);
        assert_eq!(r.refcount(), 1);
        let other = PhpRef::new(Value::Int(0));
        assert!(!PhpRef::ptr_eq(&r, &other));
        assert_ne!(r.addr(), other.addr());
        // Storing a handle to itself keeps the value; another reference is
        // looked through, and a nested reference is flattened on creation.
        assert_eq!(r.set(Value::Reference(r.clone())).as_int(), Some(5));
        assert_eq!(r.refcount(), 1);
        assert_eq!(
            r.set(Value::Reference(PhpRef::new(Value::Int(9)))).as_int(),
            Some(5)
        );
        assert_eq!(r.get().as_int(), Some(9));
        let nested = PhpRef::new(Value::Reference(r.clone()));
        assert!(!PhpRef::ptr_eq(&nested, &r));
        assert_eq!(nested.get().as_int(), Some(9));
        assert_eq!(r.refcount(), 1);
    }

    #[test]
    fn borrows_are_checked() {
        let r = PhpRef::new(Value::Int(1));
        assert_eq!(format!("{r:?}"), "PhpRef(Int(1))");
        let v = Value::Reference(r.clone());
        assert_eq!(format!("{v:?}"), "Reference(Int(1))");
        {
            let guard = r.borrow();
            assert!(r.try_borrow().is_some());
            assert!(r.try_borrow_mut().is_none());
            assert_eq!(guard.as_int(), Some(1));
        }
        {
            let mut guard = r.borrow_mut();
            *guard = Value::Int(2);
            assert!(r.try_borrow().is_none());
            assert!(r.try_borrow_mut().is_none());
            assert_eq!(format!("{v:?}"), "Reference(<borrowed>)");
        }
        assert_eq!(r.get().as_int(), Some(2));
        assert!(r.try_borrow_mut().is_some());
    }

    #[test]
    fn meta_is_stored_thin_and_shared_by_handles() {
        let r = PhpRef::new(Value::Int(1));
        assert!(r.meta().is_none());
        let m: Rc<dyn Any> = Rc::new(7u8);
        r.set_meta(Some(m.clone()));
        assert_eq!(Rc::strong_count(&m), 2);
        let got = r.meta().expect("meta");
        assert!(Rc::ptr_eq(&got, &m));
        assert_eq!(got.downcast_ref::<u8>(), Some(&7));
        assert_eq!(Rc::strong_count(&m), 3);
        drop(got);
        // Reading leaves the slot in place.
        assert!(r.meta().is_some());
        assert_eq!(Rc::strong_count(&m), 2);
        // A clone of the handle sees the same slot; replacing drops the old.
        let r2 = r.clone();
        let m2: Rc<dyn Any> = Rc::new(8u8);
        r2.set_meta(Some(m2.clone()));
        assert_eq!(Rc::strong_count(&m), 1);
        assert_eq!(
            r.meta().and_then(|x| x.downcast_ref::<u8>().copied()),
            Some(8)
        );
        r2.set_meta(None);
        assert!(r.meta().is_none());
        assert_eq!(Rc::strong_count(&m2), 1);
        // Freeing the reference frees its metadata.
        r.set_meta(Some(m2.clone()));
        drop((r, r2));
        assert_eq!(Rc::strong_count(&m2), 1);
    }

    /// Counts drops of a value held by a reference.
    struct Counted(Rc<Cell<u32>>);
    impl PhpObject for Counted {
        fn handle(&self) -> u32 {
            1
        }
        fn class_name(&self) -> ZStr {
            ZStr::from("Counted")
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }
    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn last_handle_frees_the_value() {
        let drops = Rc::new(Cell::new(0));
        let r = PhpRef::new(Value::Object(Obj::new(Counted(drops.clone()))));
        let copies: Vec<PhpRef> = (0..5).map(|_| r.clone()).collect();
        assert_eq!(r.refcount(), 6);
        drop(copies);
        assert_eq!(r.refcount(), 1);
        assert_eq!(drops.get(), 0);
        drop(r);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn weak_handles_do_not_keep_the_value_alive() {
        let drops = Rc::new(Cell::new(0));
        let r = PhpRef::new(Value::Object(Obj::new(Counted(drops.clone()))));
        let w = r.downgrade();
        assert_eq!(w.strong_count(), 1);
        assert_eq!(w.addr(), r.addr());
        assert_eq!(format!("{w:?}"), format!("WeakRef({:#x})", r.addr()));
        let r2 = w.upgrade().expect("alive");
        assert!(PhpRef::ptr_eq(&r, &r2));
        assert_eq!(r.refcount(), 2);
        drop(r2);
        assert_eq!(w.strong_count(), 1);
        drop(r);
        // The value went with the last strong handle; the allocation stays
        // for the weak handles, which now see a dead reference.
        assert_eq!(drops.get(), 1);
        assert_eq!(w.strong_count(), 0);
        assert!(w.upgrade().is_none());
        let w2 = w.clone();
        drop(w);
        assert_eq!(w2.strong_count(), 0);
        assert!(w2.upgrade().is_none());
        drop(w2);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn weak_handles_outlived_by_the_reference() {
        let drops = Rc::new(Cell::new(0));
        let r = PhpRef::new(Value::Object(Obj::new(Counted(drops.clone()))));
        let w = r.downgrade();
        let w2 = w.clone();
        drop(w);
        drop(w2);
        assert_eq!(drops.get(), 0);
        assert_eq!(r.get().type_name(), "Counted");
        drop(r);
        assert_eq!(drops.get(), 1);
    }

    /// A value whose drop upgrades, clones and drops weak handles to the
    /// reference being freed (as PHP destructors may through the root
    /// buffer).
    struct TouchesWeak(Cell<Option<WeakRef>>, Rc<Cell<bool>>);
    impl PhpObject for TouchesWeak {
        fn handle(&self) -> u32 {
            2
        }
        fn class_name(&self) -> ZStr {
            ZStr::from("TouchesWeak")
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }
    impl Drop for TouchesWeak {
        fn drop(&mut self) {
            if let Some(w) = self.0.take() {
                assert_eq!(w.strong_count(), 0);
                assert!(w.upgrade().is_none());
                let w2 = w.clone();
                drop(w);
                assert!(w2.upgrade().is_none());
                drop(w2);
                self.1.set(true);
            }
        }
    }

    #[test]
    fn weak_handles_usable_while_the_value_drops() {
        let ran = Rc::new(Cell::new(false));
        let r = PhpRef::new(Value::Null);
        let w = r.downgrade();
        r.set(Value::Object(Obj::new(TouchesWeak(
            Cell::new(Some(w)),
            ran.clone(),
        ))));
        drop(r);
        assert!(ran.get());
    }
}

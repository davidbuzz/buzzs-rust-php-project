//! Objects (`zend_object`): property slots, dynamic properties, handles and
//! destructor scheduling.
//!
//! An [`Obj`] from `php-runtime` is an `Rc<dyn PhpObject>`; ours is an
//! [`ObjBox`] wrapping an `Rc<ObjectData>`. When the last `Obj` handle is
//! dropped, `ObjBox::drop` runs: if the class has a destructor that has not
//! been called yet, a fresh `Obj` is created around the same data and queued
//! so that the VM calls `__destruct` at the next safe point — this gives
//! PHP's refcount-driven destructor timing without access to the VM from
//! `Drop`.

use crate::class::Class;
use php_runtime::{HashTable, Obj, PhpObject, Value, ZStr};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::fmt;
use std::rc::{Rc, Weak};

/// Object flags.
pub mod oflags {
    /// `__destruct` has been called (or must not be called).
    pub const DESTRUCTED: u32 = 1 << 0;
    /// Readonly properties may be modified (during clone `__clone`).
    pub const CLONING: u32 = 1 << 1;
    /// Object is an enum case.
    pub const ENUM_CASE: u32 = 1 << 2;
}

/// The state of a PHP object.
pub struct ObjectData {
    /// Handle number (`spl_object_id`).
    pub handle: u32,
    /// The class.
    pub class: Rc<Class>,
    /// Declared property slots (`None` = undefined/uninitialized).
    pub slots: RefCell<Vec<Option<Value>>>,
    /// Dynamic properties.
    pub dynamic: RefCell<Option<HashTable>>,
    /// Internal state for builtin classes.
    pub internal: RefCell<Option<Box<dyn Any>>>,
    /// `oflags`.
    pub flags: Cell<u32>,
    /// Readonly slots that have been initialized (by slot index) — used to
    /// detect "Cannot modify readonly property".
    pub readonly_init: RefCell<Vec<bool>>,
    /// Lazy-object state (`ReflectionClass::newLazyGhost()` & co).
    pub lazy: RefCell<Option<crate::lazy::LazyInfo>>,
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
    static DESTRUCTORS_ENABLED: Cell<bool> = const { Cell::new(true) };
}

#[derive(Default)]
struct ObjectStore {
    /// Live objects by handle (index = handle - 1).
    objects: Vec<Option<Weak<ObjectData>>>,
    /// Free handles (LIFO, like Zend's free list).
    free: Vec<u32>,
}

impl Drop for ObjectData {
    fn drop(&mut self) {
        let handle = self.handle;
        // `zend_weakrefs_notify`: purge WeakMap entries before the handle
        // can be reused.
        crate::weakref::object_freed(handle);
        // `zend_object_std_dtor` releases the properties (and objects they
        // hold) before `zend_objects_store_del` puts this handle on the
        // free list, so this handle is the next one reused.
        let slots = std::mem::take(self.slots.get_mut());
        let dynamic = self.dynamic.get_mut().take();
        let internal = self.internal.get_mut().take();
        drop((slots, dynamic, internal));
        // `try_with`: the store may already be gone during thread teardown.
        let _ = STORE.try_with(|s| {
            if let Ok(mut s) = s.try_borrow_mut() {
                if let Some(slot) = s.objects.get_mut(handle as usize - 1) {
                    *slot = None;
                }
                s.free.push(handle);
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
        if data.flags.get() & oflags::DESTRUCTED != 0 || !data.class.has_destructor() {
            return;
        }
        if !DESTRUCTORS_ENABLED.with(Cell::get) {
            return;
        }
        data.flags.set(data.flags.get() | oflags::DESTRUCTED);
        let revived = Obj::new(ObjBox(data.clone()));
        let _ = PENDING.try_with(|p| {
            if let Ok(mut p) = p.try_borrow_mut() {
                p.push(revived);
                HAS_PENDING.with(|h| h.set(true));
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
    let n = slots.len();
    STORE.with(|s| {
        let mut s = s.borrow_mut();
        let handle = if let Some(h) = s.free.pop() {
            h
        } else {
            s.objects.push(None);
            u32::try_from(s.objects.len()).unwrap_or(u32::MAX)
        };
        let data = Rc::new(ObjectData {
            handle,
            class,
            slots: RefCell::new(slots),
            dynamic: RefCell::new(None),
            internal: RefCell::new(None),
            flags: Cell::new(0),
            readonly_init: RefCell::new(vec![false; n]),
            lazy: RefCell::new(None),
        });
        s.objects[handle as usize - 1] = Some(Rc::downgrade(&data));
        Obj::new(ObjBox(data))
    })
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
    STORE.with(|s| {
        s.borrow()
            .objects
            .iter()
            .filter_map(|w| w.as_ref().and_then(Weak::upgrade))
            .collect()
    })
}

/// Wraps existing object data in a new handle (used at shutdown for objects
/// only reachable through cycles).
pub fn revive(data: Rc<ObjectData>) -> Obj {
    Obj::new(ObjBox(data))
}

/// The handle number the next object will get (`spl_object_id` tests).
pub fn next_handle() -> u32 {
    STORE.with(|s| {
        let s = s.borrow();
        s.free
            .last()
            .copied()
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

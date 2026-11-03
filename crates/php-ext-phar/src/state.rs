//! Native state of `Phar`/`PharData` and `PharFileInfo` objects.
//!
//! The objects extend SPL classes whose state occupies the VM object's
//! internal slot, so the phar state lives in side tables keyed by object
//! address. Each slot keeps a weak handle to its object: a slot whose
//! object died (or whose address was reused) is released lazily
//! ([`sweep`]) — `__destruct()` releases eagerly.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use php_phar::object::{PharFileInfo, PharObject};
use php_runtime::Obj;
use php_vm::object::{ObjBox, ObjectData};

/// The object's data cell: unlike the `Obj` handle it survives the
/// revival of an object for its deferred `__destruct()` call.
fn data_of(o: &Obj) -> Option<&Rc<ObjectData>> {
    o.downcast_ref::<ObjBox>().map(|b| &b.0)
}

fn key_of(o: &Obj) -> usize {
    data_of(o).map_or_else(|| o.addr(), |d| Rc::as_ptr(d) as usize)
}

struct Slot<T> {
    weak: Weak<ObjectData>,
    state: Rc<RefCell<T>>,
}

impl<T> Slot<T> {
    fn alive_for(&self, o: &Obj) -> bool {
        match (self.weak.upgrade(), data_of(o)) {
            (Some(rc), Some(d)) => Rc::ptr_eq(&rc, d),
            _ => false,
        }
    }
}

thread_local! {
    static PHARS: RefCell<HashMap<usize, Slot<PharObject>>> = RefCell::new(HashMap::new());
    static INFOS: RefCell<HashMap<usize, Slot<PharFileInfo>>> = RefCell::new(HashMap::new());
}

/// Something with a `release()` (object destruction).
trait Release {
    fn release_state(&mut self);
}

impl Release for PharObject {
    fn release_state(&mut self) {
        self.release();
    }
}

impl Release for PharFileInfo {
    fn release_state(&mut self) {
        self.release();
    }
}

fn get_or_create<T: Release>(
    table: &'static std::thread::LocalKey<RefCell<HashMap<usize, Slot<T>>>>,
    o: &Obj,
    make: impl FnOnce() -> T,
) -> Rc<RefCell<T>> {
    let key = key_of(o);
    let stale = table.with(|t| {
        let mut t = t.borrow_mut();
        match t.get(&key) {
            Some(s) if s.alive_for(o) => return Ok(s.state.clone()),
            Some(_) => {}
            None => return Err(None),
        }
        Err(t.remove(&key).map(|s| s.state))
    });
    let stale = match stale {
        Ok(s) => return s,
        Err(s) => s,
    };
    if let Some(s) = stale {
        if let Ok(mut s) = s.try_borrow_mut() {
            s.release_state();
        }
    }
    let fresh = Rc::new(RefCell::new(make()));
    table.with(|t| {
        t.borrow_mut().insert(
            key,
            Slot {
                weak: data_of(o).map_or_else(Weak::new, Rc::downgrade),
                state: fresh.clone(),
            },
        );
    });
    fresh
}

fn remove<T: Release>(
    table: &'static std::thread::LocalKey<RefCell<HashMap<usize, Slot<T>>>>,
    o: &Obj,
) {
    let key = key_of(o);
    let s = table.with(|t| {
        let mut t = t.borrow_mut();
        if t.get(&key).is_some_and(|s| s.alive_for(o)) {
            t.remove(&key)
        } else {
            None
        }
    });
    if let Some(s) = s {
        if let Ok(mut st) = s.state.try_borrow_mut() {
            st.release_state();
        }
    }
}

fn sweep_table<T: Release>(
    table: &'static std::thread::LocalKey<RefCell<HashMap<usize, Slot<T>>>>,
    all: bool,
) {
    let dead: Vec<Slot<T>> = table.with(|t| {
        let mut t = t.borrow_mut();
        let keys: Vec<usize> = t
            .iter()
            .filter(|(_, s)| all || s.weak.strong_count() == 0)
            .map(|(k, _)| *k)
            .collect();
        keys.into_iter().filter_map(|k| t.remove(&k)).collect()
    });
    for s in dead {
        if let Ok(mut st) = s.state.try_borrow_mut() {
            st.release_state();
        }
    }
}

/// The `Phar`/`PharData` state of `o` (created uninitialised on first
/// use).
pub fn phar(o: &Obj, is_data: bool) -> Rc<RefCell<PharObject>> {
    get_or_create(&PHARS, o, || PharObject::new(is_data))
}

/// The `PharFileInfo` state of `o`.
pub fn info(o: &Obj) -> Rc<RefCell<PharFileInfo>> {
    get_or_create(&INFOS, o, PharFileInfo::new)
}

/// Releases the `Phar` state of a destroyed object.
pub fn release_phar(o: &Obj) {
    remove(&PHARS, o);
}

/// Releases the `PharFileInfo` state of a destroyed object.
pub fn release_info(o: &Obj) {
    remove(&INFOS, o);
}

/// Releases the state of objects that were freed without `__destruct()`.
pub fn sweep() {
    sweep_table(&INFOS, false);
    sweep_table(&PHARS, false);
}

/// Releases every state (request shutdown).
pub fn release_all() {
    sweep_table(&INFOS, true);
    sweep_table(&PHARS, true);
}

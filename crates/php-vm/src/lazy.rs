//! Lazy objects (`Zend/zend_lazy_objects.c`): ghosts and proxies created by
//! `ReflectionClass::newLazyGhost()` / `newLazyProxy()`.
//!
//! The property handlers in `props.rs` call [`Vm::lazy_object_for`] before
//! every property operation; it initializes an uninitialized lazy object
//! (or returns the real instance of an initialized proxy). The
//! initialization itself and the Reflection methods live in
//! `reflection/lazy.rs`; this module holds the state and the model of
//! `zobj->properties` for lazy objects (built, shared, frozen tables).

use crate::object::{self, oflags, ObjectData};
use crate::vm::{Vm, VmResult};
use php_runtime::{ArrayKey, Obj, Value, ZStr};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Lazy-object state stored in `ObjectData::lazy`.
// The flags mirror `zend_lazy_object_info.flags` and the `IS_OBJ_LAZY_*`
// object flags one to one.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct LazyInfo {
    /// Proxy (true) or ghost (false).
    pub proxy: bool,
    /// The initializer (ghost) or factory (proxy) callable.
    pub initializer: Value,
    /// `ReflectionClass::SKIP_*` options.
    pub options: i64,
    /// Whether initialization has completed.
    pub initialized: bool,
    /// The real instance of an initialized proxy.
    pub instance: Option<Obj>,
    /// Re-entrancy guard.
    pub initializing: bool,
    /// Declared property slots that are still lazy (`IS_PROP_LAZY`).
    pub lazy_slots: Vec<bool>,
    /// While a ghost's initializer runs: the object's properties table
    /// exists and is shared with the initialization snapshot
    /// (`GC_TRY_ADDREF(properties_snapshot)` in `zend_lazy_object_init`),
    /// so the next write to it separates it (see [`separate_props`]).
    pub props_shared: bool,
    /// The initializer separated the table: a failed initialization
    /// restores the snapshot, dropping the frozen copies.
    pub props_separated: bool,
}

thread_local! {
    /// Frozen declared-property values by object handle, for objects
    /// flagged [`oflags::STALE_PROPS`] (see [`separate_props`]).
    static STALE: RefCell<HashMap<u32, Rc<Vec<Option<Value>>>>> = RefCell::new(HashMap::new());
}

/// Resets lazy object stale property cache for a new request.
pub(crate) fn reset() {
    STALE.with(|s| s.borrow_mut().clear());
}

/// Whether php-src's `zobj->properties` table of `d` exists: it is built
/// by the first listing that goes through it (`rebuild_object_properties`)
/// and by the first dynamic property, and then kept.
pub(crate) fn props_built(d: &ObjectData) -> bool {
    d.flags.get() & oflags::PROPS_TABLE != 0 || d.dynamic.borrow().is_some()
}

/// Records that `zobj->properties` of `d` was built.
pub(crate) fn mark_props_built(d: &ObjectData) {
    crate::object::note_property_table(d);
}

/// `zend_object_is_lazy()`: an uninitialized ghost or proxy, or an
/// initialized proxy. Not an object whose initializer is running:
/// `zend_lazy_object_init` clears `IS_OBJ_LAZY_UNINITIALIZED` and
/// `IS_OBJ_LAZY_PROXY` for the duration (and an initialized ghost has no
/// lazy state left).
pub(crate) fn is_lazy(d: &ObjectData) -> bool {
    d.lazy.borrow().as_ref().is_some_and(|i| !i.initializing)
}

/// The entries of php-src's `zobj->properties` table of `o` in table
/// order, building the table if it does not exist yet
/// (`zend_std_get_properties_ex` → `rebuild_object_properties`): the
/// declared properties by slot (mangled keys; `None` for an undefined slot,
/// which the table points to with an `IS_INDIRECT` entry and which
/// `zend_hash_compare` orders below a defined one), then the dynamic
/// properties. A table of frozen copies ([`stale_slots`]) lists the copies
/// and has no entry for a slot that was undefined when it was copied.
///
/// With `make_refs`, every entry is turned into a reference first, as
/// `ZEND_FE_FETCH_RW` does to the table's entries (`ZVAL_MAKE_REF`): a
/// frozen copy stays a reference in the table, a dynamic property in the
/// object's table. Declared slots are never touched here: the caller only
/// asks for references over a frozen table.
pub(crate) fn property_table(o: &Obj, make_refs: bool) -> Vec<(ArrayKey, Option<Value>)> {
    let d = object::data(o);
    mark_props_built(d);
    let stale = stale_slots(d);
    let mut out: Vec<(ArrayKey, Option<Value>, Option<u32>)> = Vec::new();
    if let Some(frozen) = stale {
        let mut frozen = frozen.to_vec();
        for pi in &d.class.props {
            if pi.is_virtual() {
                continue;
            }
            let Some(Some(value)) = frozen.get_mut(pi.slot as usize) else {
                continue;
            };
            let value = if make_refs {
                Value::Reference(value.make_ref())
            } else {
                value.clone()
            };
            out.push((crate::props::mangle(pi), Some(value), Some(pi.slot)));
        }
        if make_refs {
            let prev = set_stale_slots(d, Some(frozen));
            drop(prev);
        }
    } else {
        let slots = d.slots.borrow();
        for pi in &d.class.props {
            if pi.is_virtual() {
                continue;
            }
            let value = slots.get(pi.slot as usize).cloned().flatten();
            out.push((crate::props::mangle(pi), value, Some(pi.slot)));
        }
    }
    if make_refs {
        if let Some(table) = d.dynamic.borrow_mut().as_mut() {
            for (key, value) in table.iter_mut() {
                let value = Value::Reference(value.make_ref());
                out.push((key.clone(), Some(value), None));
            }
        }
    } else if let Some(table) = d.dynamic.borrow().as_ref() {
        for (key, value) in table.iter() {
            out.push((key.clone(), Some(value.clone()), None));
        }
    }
    if let Some(order) = object::prop_order(d) {
        order.sort(&mut out, |(key, _, slot)| (*slot, key));
    }
    out.into_iter()
        .map(|(key, value, _)| (key, value))
        .collect()
}

/// The declared-property values listed for `d` when its properties table
/// holds frozen copies of them (indexed by slot; `None`: not in the table),
/// or `None` when listings read the slots.
///
/// In php-src the table built by `rebuild_object_properties` points into
/// the slots (`IS_INDIRECT`). `zend_array_dup` turns those entries into
/// plain copies (and drops the undefined ones); when it separates a
/// ghost's table during initialization, the object keeps the copy, so
/// `var_dump()`, `get_object_vars()`, `(array)` & co. show the values the
/// declared properties had at that moment while property reads and writes
/// use the slots.
#[must_use]
pub(crate) fn stale_slots(d: &ObjectData) -> Option<Rc<Vec<Option<Value>>>> {
    if d.flags.get() & oflags::STALE_PROPS == 0 {
        return None;
    }
    stale_slots_cold(d.handle)
}

#[cold]
#[inline(never)]
fn stale_slots_cold(handle: u32) -> Option<Rc<Vec<Option<Value>>>> {
    STALE
        .try_with(|m| m.borrow().get(&handle).cloned())
        .ok()
        .flatten()
}

/// Gives `d` frozen declared-property values (`None` removes them); returns
/// the previous ones for the caller to drop outside the table borrow.
pub(crate) fn set_stale_slots(
    d: &ObjectData,
    frozen: Option<Vec<Option<Value>>>,
) -> Option<Rc<Vec<Option<Value>>>> {
    let handle = d.handle;
    if let Some(values) = frozen {
        d.flags.set(d.flags.get() | oflags::STALE_PROPS);
        STALE
            .try_with(|m| m.borrow_mut().insert(handle, Rc::new(values)))
            .ok()
            .flatten()
    } else {
        if d.flags.get() & oflags::STALE_PROPS == 0 {
            return None;
        }
        d.flags.set(d.flags.get() & !oflags::STALE_PROPS);
        take_stale_slots(handle)
    }
}

/// Removes the frozen values of the object `handle` (freed, or its
/// separated table discarded); the caller drops them.
#[cold]
#[inline(never)]
pub(crate) fn take_stale_slots(handle: u32) -> Option<Rc<Vec<Option<Value>>>> {
    STALE
        .try_with(|m| m.try_borrow_mut().ok().and_then(|mut m| m.remove(&handle)))
        .ok()
        .flatten()
}

/// `clone`: `zend_objects_clone_members` copies the plain entries of the
/// original's properties table, so the copy lists the same frozen values.
pub(crate) fn copy_stale_slots(from: &ObjectData, to: &ObjectData) {
    if from.flags.get() & oflags::PROPS_TABLE != 0 {
        mark_props_built(to);
    }
    if let Some(s) = stale_slots(from) {
        let prev = set_stale_slots(to, Some(s.to_vec()));
        drop(prev);
    }
}

/// A write to the properties table of `o` (a dynamic property's
/// write, fetch for write or unset): while a ghost's initializer runs with
/// the table shared with the initialization snapshot, php-src separates it
/// first (`zend_array_dup` in `zend_std_write_property`,
/// `zend_std_get_property_ptr_ptr` and `zend_std_unset_property`), which
/// freezes the declared properties' current values in the object's table
/// (see [`stale_slots`]).
fn separate_props(o: &Obj, name: &ZStr) {
    let d = object::data(o);
    // A declared property's slot is written in place.
    let declared =
        d.class.prop_map.contains_key(name) || d.class.props.iter().any(|p| p.name == *name);
    if !declared {
        separate_shared_props(d);
    }
}

/// `ZEND_FE_RESET_R` / `ZEND_FE_RESET_RW` on `o` without `get_iterator`:
/// a properties table shared with the initialization snapshot
/// (`GC_REFCOUNT(properties) > 1`) is separated before the loop walks it,
/// freezing the declared properties' values like [`separate_props`].
pub(crate) fn separate_props_for_iteration(o: &Obj) {
    let d = object::data(o);
    if d.lazy.borrow().is_some() {
        separate_shared_props(d);
    }
}

/// `zend_array_dup` of a ghost's properties table shared with its
/// initialization snapshot: the object keeps the duplicate, whose declared
/// entries are plain copies of the slots' current values. Singly held
/// references are unwrapped, as `zend_array_dup_value` does.
#[cold]
#[inline(never)]
fn separate_shared_props(d: &ObjectData) {
    {
        let mut lazy = d.lazy.borrow_mut();
        let Some(i) = lazy.as_deref_mut() else {
            return;
        };
        if !i.initializing || !i.props_shared {
            return;
        }
        i.props_shared = false;
        if d.flags.get() & oflags::STALE_PROPS != 0 {
            // Already plain copies: the duplicate holds the same values.
            return;
        }
        i.props_separated = true;
    }
    let frozen: Vec<Option<Value>> = d
        .slots
        .borrow()
        .iter()
        .map(|s| {
            s.as_ref().map(|v| match v {
                Value::Reference(r) if r.refcount() == 1 => r.get(),
                v => v.clone(),
            })
        })
        .collect();
    let prev = set_stale_slots(d, Some(frozen));
    drop(prev);
}

impl Vm {
    /// The object a property operation on `o` must act on, initializing
    /// `o` first if it is an uninitialized lazy object. `name` is the
    /// property (or `None` for whole-object operations such as
    /// `get_object_vars`).
    pub(crate) fn lazy_object_for(&mut self, o: &Obj, name: Option<&ZStr>) -> VmResult<Obj> {
        if object::data(o).lazy.borrow().is_none() {
            return Ok(o.clone());
        }
        crate::reflection::lazy::object_for(self, o, name)
    }

    /// [`Vm::lazy_object_for`] for an operation that writes the property
    /// `name` (assignment, fetch for write, reference, unset): a dynamic
    /// property write during a ghost's initialization first separates the
    /// properties table shared with the initialization snapshot
    /// ([`separate_props`]).
    pub(crate) fn lazy_object_for_write(&mut self, o: &Obj, name: &ZStr) -> VmResult<Obj> {
        if object::data(o).lazy.borrow().is_none() {
            return Ok(o.clone());
        }
        separate_props(o, name);
        crate::reflection::lazy::object_for(self, o, Some(name))
    }

    /// `zend_lazy_object_init` for extensions that read an object's
    /// property table directly: initializes an uninitialized lazy object
    /// and returns the object holding the properties (the real instance
    /// of a proxy; `o` itself otherwise).
    ///
    /// # Errors
    /// The initializer's exception (the object stays lazy).
    pub fn lazy_object_init(&mut self, o: &Obj) -> VmResult<Obj> {
        self.lazy_object_for(o, None)
    }

    /// Whether `o` is an uninitialized lazy object (`var_dump` prefix).
    #[allow(clippy::unused_self)] // a dump hook of the VM, like `object_dump`
    pub(crate) fn lazy_prefix(&self, o: &Obj) -> &'static str {
        match object::data(o).lazy.borrow().as_ref() {
            Some(i) if i.initializing => "",
            Some(i) if !i.initialized => {
                if i.proxy {
                    "lazy proxy "
                } else {
                    "lazy ghost "
                }
            }
            Some(i) if i.proxy => "lazy proxy ",
            _ => "",
        }
    }
}

//! The cycle garbage collector (`Zend/zend_gc.c`).
//!
//! Values are reference counted with `Rc` ([`php_runtime::Obj`],
//! [`php_runtime::ZArray`], [`php_runtime::PhpRef`]), so reference cycles
//! (`$o->self = $o`, `$a[] = &$a`, a closure capturing the object it is
//! stored in, ...) never reach a zero count on their own. This module finds
//! and breaks them with the synchronous algorithm of Bacon & Rajan that
//! php-src implements:
//!
//! 1. **Candidates.** The subgraph reachable from the *possible roots*:
//!    nodes whose count was decremented to a non-zero value (recorded by
//!    [`possible_root`] and by registered [`RootProvider`]s). An explicit
//!    `gc_collect_cycles()` additionally scans every live object (the object
//!    store is enumerable, [`crate::object::live_objects`]), which makes
//!    collection robust against release sites that were never instrumented.
//!    Object roots are kept in php-src's root buffer layout ([`RootBuffer`]:
//!    freed slots are reused last-in first-out, `gc_compact` fills holes
//!    with the last entries), because the buffer order is the order in
//!    which garbage is destructed.
//! 2. **Trial deletion.** Rust cannot decrement an `Rc` without freeing, so
//!    instead of php-src's "mark grey" pass the collector counts, for every
//!    candidate node, the references that come from *other candidate nodes*
//!    (`internal`). A node whose `strong_count` exceeds its internal count
//!    (plus the collector's own handle) is referenced from outside the
//!    subgraph: it is live, and so is everything reachable from it ("scan
//!    black"). Every other node is garbage ("white"). In particular a shared
//!    array or reference is only ever freed when *all* its handles are
//!    owned by garbage.
//! 3. **Destructors.** Garbage objects whose `__destruct` has not run yet are
//!    handled with php-src's two-pass protocol: their destructors run first
//!    (through [`Vm::call_destructor`], setting `oflags::DESTRUCTED`), the
//!    data reachable from them is excluded from this pass (it may be
//!    resurrected), and the collection is re-run once afterwards
//!    (`GC_DTOR_GARBAGE`, `should_rerun_gc`).
//! 4. **Freeing.** Cycles are broken by emptying the garbage objects'
//!    property tables and native state and the garbage references' values;
//!    `Rc` then frees everything, running the usual `ObjectData` teardown
//!    (handle recycling, `weakref::object_freed`).
//!
//! Native state (`ObjectData::internal`) is enumerated through
//! [`GcTraverse`] implementations registered per state type with
//! [`register_traverse`] — the equivalent of the `get_gc` object handler.
//!
//! Collections are triggered explicitly (`gc_collect_cycles()`) or when the
//! root buffer reaches the threshold (10001 roots, adapted like
//! `gc_adjust_threshold`); the latter is only noted when the root arrives
//! (the collector cannot run inside `Drop`) and performed by the VM at the
//! next safe point through [`Vm::gc_safe_point`].

mod builtins;
mod collect;
mod dtor_fiber;
mod graph;
mod traverse;

#[cfg(test)]
mod tests;

pub use traverse::{register_traverse, register_traverse_with, GcTraverse, GcVisitor};

use crate::object::ObjBox;
use crate::vm::{Vm, VmResult};
use php_runtime::{Obj, PhpObject, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

/// `GC_DEFAULT_BUF_SIZE`.
const DEFAULT_BUF_SIZE: usize = 16 * 1024;
/// `GC_BUF_GROW_STEP`.
const BUF_GROW_STEP: usize = 128 * 1024;
/// `GC_MAX_BUF_SIZE`.
const MAX_BUF_SIZE: usize = 0x4000_0000;
/// `GC_THRESHOLD_DEFAULT` (10000 roots + `GC_FIRST_ROOT`).
const THRESHOLD_DEFAULT: usize = 10_001;
/// `GC_THRESHOLD_STEP`.
const THRESHOLD_STEP: usize = 10_000;
/// `GC_THRESHOLD_MAX`.
const THRESHOLD_MAX: usize = 1_000_000_000;
/// `GC_THRESHOLD_TRIGGER`: a run collecting fewer nodes raises the threshold.
const THRESHOLD_TRIGGER: i64 = 100;

/// An external source of possible roots (the reference counts it watches
/// live outside this crate, e.g. `PhpRef` handles in `php-runtime`).
#[derive(Clone, Copy)]
pub struct RootProvider {
    /// Takes the buffered possible roots (as strong values) and empties the
    /// buffer. Dead entries are skipped.
    pub take: fn() -> Vec<Value>,
    /// Number of buffered possible roots (`num_roots` contribution).
    pub len: fn() -> usize,
    /// Enables or disables recording; returns the previous setting. The
    /// collector disables recording while it clones and drops handles of
    /// its own, so that its bookkeeping never shows up as new roots.
    pub set_recording: fn(bool) -> bool,
}

impl std::fmt::Debug for RootProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RootProvider")
    }
}

/// No slot (`GC_INVALID`).
const NO_SLOT: usize = usize::MAX;

/// The object root buffer (`GC_G(buf)`), laid out like php-src's: slot `i`
/// is php-src's `GC_FIRST_ROOT + i`.
///
/// * A slot holds a weak handle of a buffered object (`GC_ROOT`), a dead
///   weak handle standing for garbage of the running collection
///   (`GC_GARBAGE`, see [`RootBuffer::push_garbage`]), or nothing
///   (`GC_UNUSED`).
/// * Removing a root (`gc_remove_from_roots`) puts its slot on the unused
///   list, and the next root takes the most recently freed slot
///   (`GC_FETCH_UNUSED`) before the buffer grows (`GC_FETCH_NEXT_UNUSED`).
/// * [`RootBuffer::compact`] (`gc_compact`) moves the last entries into the
///   holes.
///
/// A collection visits the roots in slot order, so this layout decides the
/// order in which garbage is destructed (`Zend/tests/fibers/destructors_005`).
/// Each object is buffered at most once (`GC_INFO != 0`): `index` maps a
/// buffered handle's address to its slot (a weak handle keeps the address
/// from being reused while it is buffered).
struct RootBuffer {
    slots: Vec<Option<Weak<dyn PhpObject>>>,
    /// The unused list (`GC_G(unused)`): freed slots, the last freed on top.
    unused: Vec<usize>,
    /// Used slots (`GC_G(num_roots)`).
    num_roots: usize,
    index: AddrMap,
}

/// The root index: buffered handle address → slot.
type AddrMap = HashMap<usize, usize, BuildHasherDefault<AddrHasher>>;

/// Hashes the root index's keys, object addresses, with one folded
/// multiply instead of `SipHash`: buffering a root and freeing a buffered
/// object each look the index up (php-src keeps the slot in the object's
/// `GC_INFO` and needs no lookup at all). The keys are heap addresses, not
/// script-controlled data, so there is no hash flooding to defend against.
#[derive(Clone, Copy, Default)]
struct AddrHasher(u64);

/// Multiplier of [`AddrHasher`] (2^64 / golden ratio).
const ADDR_HASH_K: u64 = 0x9e37_79b9_7f4a_7c15;

impl AddrHasher {
    #[inline]
    #[allow(clippy::cast_possible_truncation)] // folds the 128-bit product
    fn mix(&mut self, x: u64) {
        let m = u128::from(self.0 ^ x) * u128::from(ADDR_HASH_K);
        self.0 = (m as u64) ^ ((m >> 64) as u64);
    }
}

impl Hasher for AddrHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut b = [0u8; 8];
            b[..chunk.len()].copy_from_slice(chunk);
            self.mix(u64::from_le_bytes(b));
        }
    }

    #[inline]
    fn write_usize(&mut self, n: usize) {
        self.mix(n as u64);
    }
}

impl Default for RootBuffer {
    fn default() -> Self {
        RootBuffer {
            slots: Vec::new(),
            unused: Vec::new(),
            num_roots: 0,
            index: AddrMap::default(),
        }
    }
}

/// The address a buffered weak handle is indexed by ([`Obj::addr`]).
fn weak_addr(w: &Weak<dyn PhpObject>) -> usize {
    w.as_ptr().cast::<u8>() as usize
}

impl RootBuffer {
    /// `GC_HAS_UNUSED()`.
    fn has_unused(&self) -> bool {
        !self.unused.is_empty()
    }

    /// `GC_G(first_unused) - GC_FIRST_ROOT`: the slots handed out so far.
    fn first_unused(&self) -> usize {
        self.slots.len()
    }

    /// Whether `o` is buffered.
    fn contains(&self, o: &Obj) -> bool {
        self.index.contains_key(&o.addr())
    }

    /// `GC_FETCH_UNUSED()`, else `GC_FETCH_NEXT_UNUSED()`.
    fn fetch(&mut self) -> usize {
        while let Some(i) = self.unused.pop() {
            if matches!(self.slots.get(i), Some(None)) {
                return i;
            }
        }
        self.slots.push(None);
        self.slots.len() - 1
    }

    /// Buffers `o` (`gc_possible_root`); its slot, or `None` if it already
    /// is buffered.
    #[cfg(test)]
    fn push(&mut self, o: &Obj) -> Option<usize> {
        if self.contains(o) {
            return None;
        }
        Some(self.insert_new(o))
    }

    /// Buffers `o`, which is not buffered; returns its slot.
    fn insert_new(&mut self, o: &Obj) -> usize {
        let i = self.fetch();
        self.slots[i] = Some(Rc::downgrade(o.rc()));
        self.index.insert(o.addr(), i);
        self.num_roots += 1;
        i
    }

    /// Takes a slot for garbage that is no possible root (`gc_add_garbage`
    /// of an array, or of an object without destructor): it only holds its
    /// place in the buffer until the garbage is freed.
    fn push_garbage(&mut self) -> usize {
        let i = self.fetch();
        let empty: Weak<dyn PhpObject> = Weak::<ObjBox>::new();
        self.slots[i] = Some(empty);
        self.num_roots += 1;
        i
    }

    /// `gc_remove_from_roots`: slot `i` goes to the unused list.
    fn release(&mut self, i: usize) {
        match self.slots.get(i) {
            Some(Some(w)) => {
                let addr = weak_addr(w);
                if self.index.get(&addr) == Some(&i) {
                    self.index.remove(&addr);
                }
            }
            _ => return,
        }
        self.slots[i] = None;
        self.unused.push(i);
        self.num_roots = self.num_roots.saturating_sub(1);
    }

    /// Releases slot `i` if it holds no live object: a garbage placeholder
    /// (or a root freed without notice), never a root buffered since.
    fn release_dead(&mut self, i: usize) {
        if matches!(self.slots.get(i), Some(Some(w)) if w.strong_count() == 0) {
            self.release(i);
        }
    }

    /// `GC_REMOVE_FROM_BUFFER` of the handle at `addr`; `false` if it was
    /// not buffered.
    fn remove(&mut self, addr: usize) -> bool {
        // One index lookup: the entry goes first, then its slot (which
        // holds this handle, as the index follows the slots).
        let Some(i) = self.index.remove(&addr) else {
            return false;
        };
        if matches!(self.slots.get(i), Some(Some(w)) if weak_addr(w) == addr) {
            self.slots[i] = None;
            self.unused.push(i);
            self.num_roots = self.num_roots.saturating_sub(1);
        } else {
            // Not reached while the index follows the slots; what
            // `release` does for an entry it finds in place.
            self.index.insert(addr, i);
            self.release(i);
        }
        true
    }

    /// Releases the slots of roots that were freed without notice.
    fn drop_dead(&mut self) {
        let dead: Vec<usize> = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.as_ref().is_some_and(|w| w.strong_count() == 0))
            .map(|(i, _)| i)
            .collect();
        for i in dead {
            self.release(i);
        }
    }

    /// `gc_compact`: moves the last entries into the holes (two fingers,
    /// the first hole gets the last entry) until the used slots are
    /// `0..num_roots`; empties the unused list.
    fn compact(&mut self) {
        let len = self.slots.len();
        if self.num_roots == len {
            return;
        }
        if self.num_roots > 0 {
            // `end = GC_IDX2PTR(GC_G(num_roots))`: the last slot kept.
            let end = self.num_roots - 1;
            let mut free = 0;
            let mut scan = len - 1;
            while free < scan {
                while free < len && self.slots[free].is_some() {
                    free += 1;
                }
                while scan > 0 && self.slots[scan].is_none() {
                    scan -= 1;
                }
                if scan > free && free < len {
                    let moved = self.slots[scan].take();
                    if let Some(w) = &moved {
                        let addr = weak_addr(w);
                        if self.index.get(&addr) == Some(&scan) {
                            self.index.insert(addr, free);
                        }
                    }
                    self.slots[free] = moved;
                    free += 1;
                    scan -= 1;
                    if scan <= end {
                        break;
                    }
                }
            }
        }
        self.slots.truncate(self.num_roots);
        self.unused.clear();
    }

    /// The buffered objects in slot order, with their slots.
    fn roots(&self) -> Vec<(usize, Obj)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                s.as_ref()
                    .and_then(Weak::upgrade)
                    .map(|rc| (i, Obj::from_rc(rc)))
            })
            .collect()
    }

    /// Buffers `later`'s roots after these (roots buffered while a due run
    /// waited for its safe point: php-src buffers them after the run).
    fn append(&mut self, later: RootBuffer) {
        for w in later.slots.into_iter().flatten() {
            if w.strong_count() == 0 {
                continue;
            }
            let addr = weak_addr(&w);
            if self.index.contains_key(&addr) {
                continue;
            }
            let i = self.fetch();
            self.slots[i] = Some(w);
            self.index.insert(addr, i);
            self.num_roots += 1;
        }
    }
}

/// The roots buffered when the threshold was reached
/// (`gc_possible_root_when_full`): php-src collects exactly these, before
/// buffering the root that crossed the threshold. The run itself waits
/// for the next safe point, so the buffers are set aside (weakly) until
/// then and the roots released meanwhile are buffered separately.
#[derive(Default)]
struct Frozen {
    objs: RootBuffer,
    refs: php_runtime::ref_roots::Stash,
}

impl Frozen {
    fn len(&self) -> usize {
        self.objs.num_roots + self.refs.len()
    }
}

/// Collector state (`zend_gc_globals`).
#[allow(clippy::struct_excessive_bools)] // mirrors php-src's flags
struct GcState {
    /// `gc_enabled` (`zend.enable_gc`).
    enabled: bool,
    /// `gc_protected`: no roots are recorded (until the GC is first enabled,
    /// or after a fatal error).
    protected: bool,
    /// `gc_active`: a collection is running.
    active: bool,
    /// `gc_full`: the buffer overflowed.
    full: bool,
    /// `gc_runs`.
    runs: u64,
    /// `collected`.
    collected: u64,
    /// `gc_threshold`.
    threshold: usize,
    /// `buf_size`.
    buf_size: usize,
    /// Object roots.
    roots: RootBuffer,
    /// Possible roots buffered by providers (references) since their
    /// buffers were last emptied: with the object slots handed out, they
    /// drive the threshold.
    ref_pending: usize,
    /// Arrays that were made to contain themselves (see
    /// [`self_array_created`]), weakly; dead entries are pruned as it grows.
    self_arrays: Vec<php_runtime::WeakZArray>,
    /// Prune [`GcState::self_arrays`] when it grows past this.
    self_arrays_prune_at: usize,
    /// The buffer reached the threshold: an automatic run is due at the
    /// next safe point (`gc_possible_root_when_full`).
    due: bool,
    /// The roots of the due run, set aside when it became due.
    frozen: Option<Frozen>,
    /// External root providers.
    providers: Vec<RootProvider>,
    /// `activated_at`.
    activated_at: Instant,
    /// `collector_time`.
    collector_time: Duration,
    /// `dtor_time`.
    dtor_time: Duration,
    /// `free_time`.
    free_time: Duration,
}

impl Default for GcState {
    fn default() -> Self {
        GcState {
            enabled: false,
            protected: true,
            active: false,
            full: false,
            runs: 0,
            collected: 0,
            threshold: THRESHOLD_DEFAULT,
            buf_size: DEFAULT_BUF_SIZE,
            roots: RootBuffer::default(),
            ref_pending: 0,
            self_arrays: Vec::new(),
            self_arrays_prune_at: 16,
            due: false,
            frozen: None,
            providers: Vec::new(),
            activated_at: Instant::now(),
            collector_time: Duration::ZERO,
            dtor_time: Duration::ZERO,
            free_time: Duration::ZERO,
        }
    }
}

thread_local! {
    static STATE: RefCell<GcState> = RefCell::new(GcState::default());
    /// Set when an automatic run is due; setting it also raises the
    /// executor's safe-point flag (`crate::interrupt::raise_safe_point`).
    static WANT_RUN: Cell<bool> = const { Cell::new(false) };
}

fn with_state<R>(f: impl FnOnce(&mut GcState) -> R) -> Option<R> {
    STATE
        .try_with(|s| s.try_borrow_mut().ok().map(|mut s| f(&mut s)))
        .ok()
        .flatten()
}

/// Root providers record while the buffer is not protected.
fn sync_recording() {
    let Some((providers, on)) = with_state(|s| (s.providers.clone(), !s.protected && !s.active))
    else {
        return;
    };
    for p in providers {
        (p.set_recording)(on);
    }
}

/// `gc_enable()`: turns the collector on or off; returns the old setting.
/// The first enabling unprotects the root buffer (`gc_reset`).
pub fn set_enabled(on: bool) -> bool {
    let old = with_state(|s| {
        let old = s.enabled;
        s.enabled = on;
        if on && s.protected && !s.active {
            s.protected = false;
            s.full = false;
        }
        old
    });
    sync_recording();
    old.unwrap_or(false)
}

/// `gc_enabled()`.
#[must_use]
pub fn enabled() -> bool {
    with_state(|s| s.enabled).unwrap_or(false)
}

/// `gc_protect()`: stops (or resumes) recording roots, e.g. after a fatal
/// error. Returns the old setting.
pub fn set_protected(on: bool) -> bool {
    let old = with_state(|s| std::mem::replace(&mut s.protected, on)).unwrap_or(true);
    sync_recording();
    old
}

/// Whether a collection is running (`gc_status()["running"]`).
#[must_use]
pub fn running() -> bool {
    with_state(|s| s.active).unwrap_or(false)
}

/// Registers an external source of possible roots (idempotent).
pub fn add_root_provider(p: RootProvider) {
    with_state(|s| {
        if !s
            .providers
            .iter()
            .any(|q| std::ptr::fn_addr_eq(q.take, p.take))
        {
            s.providers.push(p);
        }
    });
    sync_recording();
}

/// `gc_check_possible_root()`: records `v` — whose count is about to be
/// decremented to a non-zero value — as a possible cycle root.
///
/// Call it with the old value at the engine's release sites (assignment
/// overwrite, `unset`, scope exit, element/property overwrite or removal)
/// *before* dropping it. Only objects are buffered here; references are
/// recorded by a `php-runtime` [`RootProvider`], and arrays never close a
/// cycle on their own (in this value model every cycle passes through an
/// object or a reference). Releasing the last handle of an array releases
/// its elements (`zend_array_destroy`), which buffers the objects among
/// them that are still referenced elsewhere. Cheap when the value is not
/// an object or an unshared non-empty array.
pub fn possible_root(v: &Value) {
    match v {
        Value::Object(o) => root_obj(o),
        // Releasing the last handle of a reference releases its value.
        Value::Reference(r) if r.refcount() == 1 => {
            if let Some(inner) = r.try_borrow() {
                if let Value::Object(o) = &*inner {
                    root_obj(o);
                }
            }
        }
        Value::Array(a) if a.refcount() == 1 && !a.is_empty() => released_array(a),
        _ => {}
    }
}

/// `zend_array_destroy` → `i_zval_ptr_dtor` on every element of an array
/// whose last handle is being released: objects that stay referenced become
/// possible roots; unshared nested arrays are released in turn.
#[cold]
#[inline(never)]
fn released_array(a: &php_runtime::ZArray) {
    if !with_state(|s| !s.protected).unwrap_or(false) {
        return;
    }
    let mut pending: Vec<&php_runtime::ZArray> = vec![a];
    while let Some(arr) = pending.pop() {
        for v in arr.values() {
            match v {
                Value::Array(inner) if inner.refcount() == 1 && !inner.is_empty() => {
                    pending.push(inner);
                }
                Value::Object(_) | Value::Reference(_) => possible_root(v),
                _ => {}
            }
        }
    }
}

/// Nesting limit of [`release_value`]'s walk through unshared objects.
const RELEASE_DEPTH: u32 = 64;

/// `zval_ptr_dtor` of `v`, a handle about to be dropped: like
/// [`possible_root`], and when it is the last handle of an object, the
/// object is freed (`zend_object_std_dtor`), releasing its properties and
/// a closure's bound variables and `$this` the same way. Call it before
/// dropping `v`.
pub(crate) fn release_value(v: &Value) {
    release_value_at(v, 0);
}

fn release_value_at(v: &Value, depth: u32) {
    match v {
        Value::Object(o) if o.refcount() == 1 && depth < RELEASE_DEPTH => {
            released_object(o, depth + 1);
        }
        _ => possible_root(v),
    }
}

#[cold]
#[inline(never)]
fn released_object(o: &Obj, depth: u32) {
    if o.downcast_ref::<ObjBox>().is_none() {
        return;
    }
    let d = crate::object::data(o);
    if let Ok(slots) = d.slots.try_borrow() {
        for v in slots.iter().flatten() {
            release_value_at(v, depth);
        }
    }
    if let Ok(dynamic) = d.dynamic.try_borrow() {
        for v in dynamic.iter().flat_map(|t| t.values()) {
            release_value_at(v, depth);
        }
    }
    if let Ok(state) = d.internal.try_borrow() {
        if let Some(c) = state
            .as_ref()
            .and_then(|s| s.downcast_ref::<crate::closure::ClosureData>())
        {
            for (_, v) in &c.vars {
                release_value_at(v, depth);
            }
            if let Some(this) = &c.this {
                root_obj(this);
            }
        }
    }
}

/// Drops `v`, an extra handle of a reference that stays bound elsewhere
/// (e.g. the unused result of `$a = &$b`), without buffering the
/// reference as a possible root: php-src never counted that handle, so
/// releasing it is no `GC_DELREF`.
#[inline]
pub(crate) fn release_duplicate(v: Value) {
    let quiet = match &v {
        Value::Reference(r) => {
            r.refcount() > 1
                && r.try_borrow()
                    .is_some_and(|x| matches!(*x, Value::Array(_) | Value::Object(_)))
        }
        _ => false,
    };
    if quiet {
        let was = php_runtime::ref_roots::set_recording(false);
        drop(v);
        php_runtime::ref_roots::set_recording(was);
    }
}

/// `zend_gc_check_root_tmpvars` for one value: after a collection, the
/// collectable values held by the live temporaries and loop variables of
/// the running frames become possible roots again
/// (`gc_check_possible_root`, through a reference to its value). TMPVARs
/// are freed without a GC check (`zval_ptr_dtor_nogc`), so a root the run
/// found alive only through such a holder would otherwise leak once the
/// holder is released (`Zend/tests/gc/gc_047`).
pub(crate) fn root_live_value(v: &Value) {
    match v {
        Value::Object(o) => root_live_obj(o),
        Value::Reference(r) => match r.try_borrow().as_deref() {
            Some(Value::Object(o)) => root_live_obj(o),
            // A reference to an array is the root in this model.
            Some(Value::Array(_)) => php_runtime::ref_roots::check_possible_root(r),
            _ => {}
        },
        _ => {}
    }
}

/// [`root_live_value`] for an object held by a live temporary.
pub(crate) fn root_live_obj(o: &Obj) {
    if o.downcast_ref::<ObjBox>().is_some() {
        with_state(|s| buffer_obj(s, o));
    }
}

/// Buffers `o`, one handle of which is about to be released.
fn root_obj(o: &Obj) {
    // A count of 1 drops to zero: the object is freed, not a root.
    if o.refcount() < 2 || o.downcast_ref::<ObjBox>().is_none() {
        return;
    }
    with_state(|s| buffer_obj(s, o));
}

/// `gc_possible_root` of an object: buffers `o` unless it already is
/// (`GC_MAY_LEAK`); returns its slot. Roots released by destructors during
/// a run are kept for the next one (they never trigger a nested run).
fn buffer_obj(s: &mut GcState, o: &Obj) -> Option<usize> {
    if s.protected {
        return None;
    }
    // php-src reads `GC_INFO` from the object itself. Here an object none
    // of whose handles was ever buffered (`oflags::GC_BUFFERED` clear, the
    // case of every fresh object) cannot be in the index, so the lookup
    // is skipped.
    let data = o.downcast_ref::<ObjBox>().map(|b| &*b.0);
    let maybe_buffered =
        data.is_none_or(|d| d.flags.get() & crate::object::oflags::GC_BUFFERED != 0);
    if maybe_buffered && s.roots.contains(o) {
        return None;
    }
    if !s.roots.has_unused() {
        // May set the buffered roots aside (the new root then starts an
        // empty buffer).
        note_new_slot(s);
    }
    let i = s.roots.insert_new(o);
    let d = data.unwrap_or_else(|| crate::object::data(o));
    d.flags
        .set(d.flags.get() | crate::object::oflags::GC_BUFFERED);
    Some(i)
}

/// `a` was made to contain itself by value (`$t = &$a; $a[] = $t;`,
/// `ZArray::store_self_at`): a cycle of arrays alone. php-src finds it
/// through the array's root buffer entry (arrays are possible roots there);
/// here every run looks at the arrays registered by this call, and frees
/// those found unreachable by emptying them (`zend_hash_destroy` of a
/// garbage array whatever its count). Cold: call it where such an array
/// is made.
#[cold]
#[inline(never)]
pub fn self_array_created(a: &php_runtime::ZArray) {
    with_state(|s| {
        if s.self_arrays.len() >= s.self_arrays_prune_at {
            s.self_arrays.retain(php_runtime::WeakZArray::is_alive);
            s.self_arrays_prune_at = (s.self_arrays.len() * 2).max(16);
        }
        s.self_arrays.push(a.downgrade());
    });
}

/// The registered self-containing arrays still alive (pruning the others).
fn take_self_arrays(s: &mut GcState) -> Vec<php_runtime::ZArray> {
    s.self_arrays.retain(php_runtime::WeakZArray::is_alive);
    s.self_arrays_prune_at = (s.self_arrays.len() * 2).max(16);
    s.self_arrays
        .iter()
        .filter_map(php_runtime::WeakZArray::upgrade)
        .collect()
}

/// `OBJ_RELEASE(Z_OBJ(call->This))` after a native constructor called by
/// `new` (`ZEND_CALL_RELEASE_THIS`): the new object, still held by the
/// result of `new`, becomes a possible root. Call it before dropping the
/// call's handle.
pub(crate) fn released_ctor_this(o: &Obj) {
    root_obj(o);
}

/// A buffered object handle (`oflags::GC_BUFFERED`) at `addr` is being
/// freed: its root buffer entry goes (`GC_REMOVE_FROM_BUFFER`), and its
/// slot is the next one reused.
pub(crate) fn forget_root(addr: usize) {
    with_state(|s| {
        s.roots.remove(addr);
        if let Some(f) = &mut s.frozen {
            f.objs.remove(addr);
        }
    });
}

/// Records the objects held by the variables of a frame being torn down
/// (`i_free_compiled_variables` → `gc_check_possible_root`), then the
/// handle the call held (`zend_leave_helper`): a closure's object
/// (`ZEND_CALL_CLOSURE`), or `$this` of `new` and of a method called on a
/// variable or a temporary (`ZEND_CALL_RELEASE_THIS`). `$this->m()`,
/// `parent::m()`/`self::m()` (same `$this` as `caller_this`, the calling
/// frame's) and calls made by internal code (`zend_call_function`) pass
/// `$this` without a reference of their own, so releasing it is no
/// possible root. Call it just before dropping a returning frame.
pub fn frame_released(f: &crate::vm::Frame, caller_this: Option<&Obj>) {
    if Rc::strong_count(&f.vars) != 1 {
        return;
    }
    if let Ok(vars) = f.vars.try_borrow() {
        for v in vars.slots.iter().flatten() {
            possible_root(v);
        }
    }
    if let Some(c) = &f.closure {
        root_obj(c);
    } else if let Some(this) = &f.this {
        if !f.called_from_internal && !caller_this.is_some_and(|t| Obj::ptr_eq(t, this)) {
            root_obj(this);
        }
    }
}

/// Called by root providers each time they are about to buffer a new
/// possible root (*before* adding it, so that a threshold run sees the
/// previous roots only, like `gc_possible_root_when_full`).
pub fn note_possible_root() {
    with_state(|s| {
        if s.protected {
            return;
        }
        if !s.roots.has_unused() {
            note_new_slot(s);
        }
        s.ref_pending += 1;
    });
}

/// A new root is about to take a slot past those handed out (no freed slot
/// to reuse, `GC_HAS_UNUSED()` is false): the buffer grows when full, and
/// with `threshold - 1` slots handed out the new root triggers a run
/// (`GC_HAS_NEXT_UNUSED_UNDER_THRESHOLD`). The run cannot happen inside the
/// release (it may be in `Drop`): it is due at the next safe point, over
/// the roots buffered until now (frozen), not the new one.
fn note_new_slot(s: &mut GcState) {
    let used = s.roots.first_unused() + s.ref_pending + 1;
    if used > s.buf_size {
        grow_buffer(s);
    }
    if s.enabled && !s.active && !s.due && used >= s.threshold {
        s.due = true;
        freeze(s);
        WANT_RUN.with(|w| w.set(true));
        crate::interrupt::raise_safe_point();
    }
}

/// Sets the buffered roots aside for the due run (see [`Frozen`]); the
/// root about to be buffered goes to the emptied buffers.
#[cold]
#[inline(never)]
fn freeze(s: &mut GcState) {
    // Roots still frozen for a run that did not happen stay the oldest.
    thaw(s);
    let objs = std::mem::take(&mut s.roots);
    let refs = php_runtime::ref_roots::stash();
    s.frozen = Some(Frozen { objs, refs });
    s.ref_pending = 0;
}

/// Puts frozen roots back into the buffers (ahead of the newer ones).
fn thaw(s: &mut GcState) {
    if let Some(f) = s.frozen.take() {
        let later = std::mem::replace(&mut s.roots, f.objs);
        s.roots.append(later);
        php_runtime::ref_roots::unstash(f.refs);
        s.ref_pending = php_runtime::ref_roots::len();
    }
}

/// `gc_grow_root_buffer`.
fn grow_buffer(s: &mut GcState) {
    if s.buf_size >= MAX_BUF_SIZE {
        return;
    }
    let new = if s.buf_size < BUF_GROW_STEP {
        s.buf_size * 2
    } else {
        s.buf_size + BUF_GROW_STEP
    };
    s.buf_size = new.min(MAX_BUF_SIZE);
}

/// `gc_adjust_threshold`: raise the threshold after unproductive automatic
/// runs, lower it back after productive ones.
fn adjust_threshold(s: &mut GcState, count: i64) {
    if count < THRESHOLD_TRIGGER || s.roots.num_roots + s.ref_pending >= s.threshold {
        if s.threshold < THRESHOLD_MAX {
            let new = (s.threshold + THRESHOLD_STEP).min(THRESHOLD_MAX);
            if new > s.buf_size {
                grow_buffer(s);
            }
            if new <= s.buf_size {
                s.threshold = new;
            }
        }
    } else if s.threshold > THRESHOLD_DEFAULT {
        s.threshold = s
            .threshold
            .saturating_sub(THRESHOLD_STEP)
            .max(THRESHOLD_DEFAULT);
    }
}

/// Number of buffered possible roots (`num_roots`).
#[must_use]
pub fn num_roots() -> usize {
    let (mine, providers) = with_state(|s| {
        (
            s.roots.num_roots + s.frozen.as_ref().map_or(0, Frozen::len),
            s.providers.clone(),
        )
    })
    .unwrap_or_default();
    mine + providers.iter().map(|p| (p.len)()).sum::<usize>()
}

/// Whether an automatic collection is due. The VM checks this at its
/// safe point after an instruction and then calls [`Vm::gc_safe_point`].
#[inline]
#[must_use]
pub fn wants_run() -> bool {
    WANT_RUN.try_with(Cell::get).unwrap_or(false)
}

/// Resets the collector for a new request (`gc_reset`), with
/// `zend.enable_gc` as given.
pub(crate) fn request_startup(enable: bool) {
    with_state(|s| {
        let providers = std::mem::take(&mut s.providers);
        *s = GcState {
            providers,
            ..GcState::default()
        };
    });
    WANT_RUN.with(|w| w.set(false));
    set_enabled(enable);
}

/// Statistics reported by `gc_status()` (`zend_gc_status`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GcStatus {
    /// `running`.
    pub running: bool,
    /// `protected`.
    pub protected: bool,
    /// `full`.
    pub full: bool,
    /// `runs`.
    pub runs: u64,
    /// `collected`.
    pub collected: u64,
    /// `threshold`.
    pub threshold: usize,
    /// `buffer_size`.
    pub buffer_size: usize,
    /// `roots`.
    pub roots: usize,
    /// `application_time` (seconds).
    pub application_time: f64,
    /// `collector_time` (seconds).
    pub collector_time: f64,
    /// `destructor_time` (seconds).
    pub destructor_time: f64,
    /// `free_time` (seconds).
    pub free_time: f64,
}

/// `zend_gc_get_status`.
#[must_use]
pub fn status() -> GcStatus {
    let roots = num_roots();
    with_state(|s| GcStatus {
        running: s.active,
        protected: s.protected,
        full: s.full,
        runs: s.runs,
        collected: s.collected,
        threshold: s.threshold,
        buffer_size: s.buf_size,
        roots,
        application_time: s.activated_at.elapsed().as_secs_f64(),
        collector_time: s.collector_time.as_secs_f64(),
        destructor_time: s.dtor_time.as_secs_f64(),
        free_time: s.free_time.as_secs_f64(),
    })
    .unwrap_or(GcStatus {
        running: false,
        protected: true,
        full: false,
        runs: 0,
        collected: 0,
        threshold: THRESHOLD_DEFAULT,
        buffer_size: DEFAULT_BUF_SIZE,
        roots,
        application_time: 0.0,
        collector_time: 0.0,
        destructor_time: 0.0,
        free_time: 0.0,
    })
}

/// Registers the `gc_*` builtins, the `zend.enable_gc` modify handler and
/// the traversal of the engine's native object states.
pub(crate) fn register(vm: &mut Vm) {
    builtins::register(vm);
    traverse::register_engine_traversals();
    let on = vm.ini.get_bool("zend.enable_gc");
    request_startup(on);
}

impl Vm {
    /// Runs a due automatic collection. The VM calls this at a safe point
    /// (between instructions) when [`wants_run`] is set.
    ///
    /// # Errors
    /// Propagates exceptions thrown by destructors of garbage objects.
    pub fn gc_safe_point(&mut self) -> VmResult<()> {
        WANT_RUN.with(|w| w.set(false));
        // The run looks at the frozen roots only: those buffered since
        // (`late`) are set aside until it is over.
        let due = with_state(|s| {
            let due = std::mem::take(&mut s.due) && !s.active && s.enabled;
            if !due {
                thaw(s);
                return None;
            }
            let Some(f) = s.frozen.take() else {
                return Some(Frozen::default());
            };
            let late = Frozen {
                objs: std::mem::replace(&mut s.roots, f.objs),
                refs: php_runtime::ref_roots::stash(),
            };
            php_runtime::ref_roots::unstash(f.refs);
            Some(late)
        });
        let Some(Some(late)) = due else {
            return Ok(());
        };
        let r = self.gc_run(collect::Mode::Auto);
        // php-src buffers the late roots after the run.
        with_state(|s| {
            s.roots.append(late.objs);
            php_runtime::ref_roots::unstash(late.refs);
            s.ref_pending = php_runtime::ref_roots::len();
        });
        let count = r?;
        with_state(|s| adjust_threshold(s, count));
        Ok(())
    }

    /// `gc_collect_cycles()`: an explicit collection. Returns the number of
    /// freed objects and arrays.
    ///
    /// # Errors
    /// Propagates exceptions thrown by destructors of garbage objects.
    pub fn gc_collect_cycles(&mut self) -> VmResult<i64> {
        WANT_RUN.with(|w| w.set(false));
        with_state(|s| {
            s.due = false;
            thaw(s);
        });
        self.gc_run(collect::Mode::Full)
    }
}

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

/// The root buffer for objects (`GC_G(buf)`): weak handles, deduplicated by
/// address so that a node is buffered at most once (`GC_INFO != 0`).
#[derive(Default)]
struct RootBuffer {
    entries: Vec<Weak<dyn PhpObject>>,
    index: HashMap<usize, usize>,
    /// Compact (drop dead entries) when `entries` grows past this, so that
    /// the buffer stays bounded while no collection runs (GC disabled).
    compact_at: usize,
}

impl RootBuffer {
    /// Buffers `o`; `false` if it already is.
    fn push(&mut self, o: &Obj) -> bool {
        if self.entries.len() >= self.compact_at.max(4096) {
            self.compact();
        }
        let addr = o.addr();
        if let Some(&i) = self.index.get(&addr) {
            if let Some(e) = self.entries.get(i) {
                if e.strong_count() > 0 && std::ptr::addr_eq(e.as_ptr(), Rc::as_ptr(o.rc())) {
                    return false;
                }
            }
            // A dead entry whose address was reused: replace it.
            if let Some(slot) = self.entries.get_mut(i) {
                *slot = Rc::downgrade(o.rc());
                return true;
            }
        }
        self.index.insert(addr, self.entries.len());
        self.entries.push(Rc::downgrade(o.rc()));
        true
    }

    /// Drops the entry of the freed handle at `addr` (`gc_remove_from_buffer`);
    /// `false` if it was not buffered.
    /// The order of the other entries is kept (it is the collection order);
    /// trailing dead entries are popped so the buffer does not grow while
    /// short-lived objects come and go.
    fn remove(&mut self, addr: usize) -> bool {
        let Some(i) = self.index.remove(&addr) else {
            return false;
        };
        if let Some(slot) = self.entries.get_mut(i) {
            let empty: Weak<dyn PhpObject> = Weak::<ObjBox>::new();
            *slot = empty;
        }
        while let Some(last) = self.entries.last() {
            if last.strong_count() > 0 {
                break;
            }
            let a = last.as_ptr().cast::<u8>() as usize;
            let j = self.entries.len() - 1;
            if self.index.get(&a) == Some(&j) {
                self.index.remove(&a);
            }
            self.entries.pop();
        }
        true
    }

    fn compact(&mut self) {
        self.entries.retain(|e| e.strong_count() > 0);
        self.index.clear();
        for (i, e) in self.entries.iter().enumerate() {
            self.index.insert(e.as_ptr().cast::<u8>() as usize, i);
        }
        self.compact_at = self.entries.len() * 2;
    }

    fn live_len(&self) -> usize {
        self.entries.iter().filter(|e| e.strong_count() > 0).count()
    }

    fn take(&mut self) -> Vec<Value> {
        self.index.clear();
        std::mem::take(&mut self.entries)
            .into_iter()
            .filter_map(|e| e.upgrade())
            .map(|rc| Value::Object(Obj::from_rc(rc)))
            .collect()
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
    /// Possible roots recorded since the buffer was last emptied (also
    /// those buffered by providers): drives the threshold.
    pending: usize,
    /// The buffer reached the threshold: an automatic run is due at the
    /// next safe point (`gc_possible_root_when_full`).
    due: bool,
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
            pending: 0,
            due: false,
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
    /// Set when an automatic run is due (the per-instruction check is a
    /// single thread-local load).
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

/// Buffers `o`, one handle of which is about to be released.
fn root_obj(o: &Obj) {
    // A count of 1 drops to zero: the object is freed, not a root.
    if o.refcount() < 2 || o.downcast_ref::<ObjBox>().is_none() {
        return;
    }
    with_state(|s| {
        // Roots released by destructors during a run are kept for the
        // next one (they never trigger a nested run).
        if !s.protected && s.roots.push(o) {
            let d = crate::object::data(o);
            d.flags
                .set(d.flags.get() | crate::object::oflags::GC_BUFFERED);
            if !s.active {
                note_new_root(s);
            }
        }
    });
}

/// A buffered object handle (`oflags::GC_BUFFERED`) at `addr` is being
/// freed: its root buffer entry goes (`GC_REMOVE_FROM_BUFFER`).
pub(crate) fn forget_root(addr: usize) {
    with_state(|s| {
        // The freed slot no longer counts towards the threshold
        // (`gc_remove_from_buffer` decrements `num_roots`).
        if s.roots.remove(addr) {
            s.pending = s.pending.saturating_sub(1);
        }
    });
}

/// Records the objects held by the variables of a frame being torn down
/// (`i_free_compiled_variables` → `gc_check_possible_root`). Call it just
/// before dropping a returning frame.
pub fn frame_released(f: &crate::vm::Frame) {
    if Rc::strong_count(&f.vars) != 1 {
        return;
    }
    if let Ok(vars) = f.vars.try_borrow() {
        for v in vars.slots.iter().flatten() {
            possible_root(v);
        }
    }
    if let Some(this) = &f.this {
        root_obj(this);
    }
}

/// Called by root providers each time they are about to buffer a new
/// possible root (*before* adding it, so that a threshold run sees the
/// previous roots only, like `gc_possible_root_when_full`).
pub fn note_possible_root() {
    with_state(|s| {
        if !s.protected && !s.active {
            note_new_root(s);
        }
    });
}

fn note_new_root(s: &mut GcState) {
    s.pending += 1;
    if s.pending > s.buf_size {
        grow_buffer(s);
    }
    // The buffer holds `threshold - 1` roots when the next one triggers a
    // run (`GC_HAS_NEXT_UNUSED_UNDER_THRESHOLD`). The run cannot happen
    // inside the release (it may be in `Drop`): it is due at the next safe
    // point, over the roots buffered by then.
    if s.enabled && !s.due && s.pending >= s.threshold {
        s.due = true;
        WANT_RUN.with(|w| w.set(true));
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
    if count < THRESHOLD_TRIGGER || s.pending >= s.threshold {
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
    let (mine, providers) =
        with_state(|s| (s.roots.live_len(), s.providers.clone())).unwrap_or_default();
    mine + providers.iter().map(|p| (p.len)()).sum::<usize>()
}

/// Whether an automatic collection is due. The VM checks this after each
/// instruction and then calls [`Vm::gc_safe_point`].
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
        let due = with_state(|s| std::mem::take(&mut s.due) && !s.active && s.enabled);
        if due != Some(true) {
            return Ok(());
        }
        let count = self.gc_run(collect::Mode::Auto)?;
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
        with_state(|s| s.due = false);
        self.gc_run(collect::Mode::Full)
    }
}

// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Enumeration of the references held by native object state: the
//! `get_gc` handler, as a trait.
//!
//! # The `get_gc` contract
//!
//! An object's native state (`ObjectData::internal`, a `Box<dyn Any>`)
//! is invisible to the collector unless its type implements [`GcTraverse`]
//! and is registered with [`register_traverse`] (usually from the class's
//! registration code). An implementation must:
//!
//! * report every `Value`/`Obj`/`ZArray`/`PhpRef` handle the state *owns*,
//!   exactly once per handle it holds (a value stored twice is reported
//!   twice);
//! * never report a handle owned by something else — in particular not the
//!   contents of an `Rc`-shared structure unless the state holds the only
//!   strong count of it (check `Rc::strong_count(..) == 1`);
//! * report `WeakMap`-style entries (kept alive only while both the
//!   container and a weakly-held key live) with [`GcVisitor::weak_entry`];
//! * not run PHP code, not allocate PHP objects and not mutate anything.
//!
//! Omitting a handle is always safe (its target is then considered
//! referenced from outside and kept alive); over-reporting is not.
//!
//! When an object with traversed state is found to be garbage, the
//! collector drops the state (`free_obj`) to break the cycles that run
//! through it.

use super::graph::Graph;
use crate::closure::ClosureData;
use crate::fiber::FiberData;
use crate::generator::{Delegate, GenData};
use crate::vm::{FinState, ForeachIter, Frame, PendingCall, VarStore};
use php_runtime::{HashTable, Obj, ObjWeak, PhpRef, Value, ZArray};
use rustc_hash::FxHashMap;
use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::rc::Rc;

/// Receives the references held by an object's native state.
pub struct GcVisitor<'a> {
    /// The candidate graph a collection builds; `None` when the state is
    /// being freed and what it owns is offered as possible roots instead
    /// ([`root_native_state`]).
    graph: Option<&'a mut Graph>,
    from: u32,
}

impl std::fmt::Debug for GcVisitor<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GcVisitor(node {})", self.from)
    }
}

impl<'a> GcVisitor<'a> {
    pub(super) fn new(graph: &'a mut Graph, from: u32) -> Self {
        GcVisitor {
            graph: Some(graph),
            from,
        }
    }

    /// A value owned by the state (scalars and strings are ignored).
    pub fn value(&mut self, v: &Value) {
        match &mut self.graph {
            Some(g) => g.edge(v),
            None => super::possible_root(v),
        }
    }

    /// Several owned values.
    pub fn values<'v>(&mut self, vs: impl IntoIterator<Item = &'v Value>) {
        for v in vs {
            self.value(v);
        }
    }

    /// An owned object handle.
    pub fn obj(&mut self, o: &Obj) {
        match &mut self.graph {
            Some(g) => {
                if let Some(t) = g.intern_obj(o) {
                    g.link(t);
                }
            }
            None => super::possible_root_obj(o),
        }
    }

    /// An owned array handle.
    pub fn array(&mut self, a: &ZArray) {
        match &mut self.graph {
            Some(g) => {
                let t = g.intern_arr(a);
                g.link(t);
            }
            None => super::possible_root_array(a),
        }
    }

    /// An owned reference handle.
    pub fn reference(&mut self, r: &PhpRef) {
        match &mut self.graph {
            Some(g) => {
                let t = g.intern_ref(r);
                g.link(t);
            }
            None => super::possible_root_ref(r),
        }
    }

    /// The values of a hash table stored inline in the state.
    pub fn table(&mut self, t: &HashTable) {
        self.values(t.values());
    }

    /// An entry whose value is kept alive only while both this object and
    /// the (weakly held) `key` object are alive — a `WeakMap` entry.
    pub fn weak_entry(&mut self, key: &ObjWeak, v: &Value) {
        match &mut self.graph {
            Some(g) => g.cond_edge(self.from, key, v),
            None => super::possible_root(v),
        }
    }

    /// The values held by a suspended frame (generator or fiber) the
    /// state owns.
    pub fn frame(&mut self, f: &Frame) {
        frame(self, f);
    }

    /// The values held by a prepared call the state owns.
    pub fn pending_call(&mut self, c: &PendingCall) {
        pending_call(self, c);
    }

    /// Declares that the state holds references that cannot be reported
    /// (e.g. a native stack): the object is kept alive.
    pub fn opaque(&mut self) {
        if let Some(g) = &mut self.graph {
            g.pin(self.from);
        }
    }
}

/// `zval_ptr_dtor` of everything the native state `state` of an object
/// being freed owns (a closure's bound variables and `$this`, a suspended
/// generator's frame, the storage of an `ArrayObject` or
/// `SplObjectStorage`): each value that stays referenced becomes a
/// possible root, as php-src's `free_obj` handlers make it
/// (`zend_closure_free_storage`, `zend_generator_free_storage`,
/// `spl_array_object_free_storage`, ...). Called before the state is
/// dropped, while it still owns its handles. State of a type without a
/// traversal is left alone.
///
/// Without it a cycle whose last outside holder was such an object was
/// never offered to the collector once a run had seen it alive: a callback
/// (`function () use ($parser)`) kept in a registry and dropped later.
pub(crate) fn root_native_state(state: &dyn Any) {
    if let Some(f) = lookup(state.type_id()) {
        let mut v = GcVisitor {
            graph: None,
            from: super::graph::NONE,
        };
        f(state, &mut v);
    }
}

/// Native object state whose references the collector can enumerate (the
/// `get_gc` object handler). See the module documentation for the
/// contract.
pub trait GcTraverse: Any {
    /// Reports every handle the state owns.
    fn gc_traverse(&self, v: &mut GcVisitor<'_>);
}

type TraverseFn = Rc<dyn Fn(&dyn Any, &mut GcVisitor<'_>)>;

thread_local! {
    // Looked up once per object with native state in every run: a `TypeId`
    // is already a hash, `SipHash` over it was 6% of a run.
    static REGISTRY: RefCell<FxHashMap<TypeId, TraverseFn>> = RefCell::new(FxHashMap::default());
}

/// Registers `T` (stored with `object::set_internal`) as traversable.
pub fn register_traverse<T: GcTraverse>() {
    register_traverse_with::<T>(T::gc_traverse);
}

/// Registers a traversal function for native state of type `T` — for
/// state types defined in another crate, which cannot implement
/// [`GcTraverse`] there (orphan rule). Same contract as
/// [`GcTraverse::gc_traverse`].
pub fn register_traverse_with<T: Any>(f: fn(&T, &mut GcVisitor<'_>)) {
    insert::<T>(f, true);
}

fn insert<T: Any>(f: fn(&T, &mut GcVisitor<'_>), replace: bool) {
    let erased: TraverseFn = Rc::new(move |state: &dyn Any, v: &mut GcVisitor<'_>| {
        if let Some(s) = state.downcast_ref::<T>() {
            f(s, v);
        }
    });
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        if replace {
            r.insert(TypeId::of::<T>(), erased);
        } else {
            r.entry(TypeId::of::<T>()).or_insert(erased);
        }
    });
}

pub(super) fn lookup(t: TypeId) -> Option<TraverseFn> {
    REGISTRY
        .try_with(|r| r.try_borrow().ok().and_then(|r| r.get(&t).cloned()))
        .ok()
        .flatten()
}

/// Whether native state of this type is traversed (and hence dropped when
/// its object is garbage).
pub(super) fn is_traversed(state: &dyn Any) -> bool {
    lookup(state.type_id()).is_some()
}

/// The engine's own state types. A module may register a more complete
/// traversal for its type (before or after): it is kept.
pub(super) fn register_engine_traversals() {
    insert::<ClosureData>(ClosureData::gc_traverse, false);
    insert::<GenData>(GenData::gc_traverse, false);
    insert::<FiberData>(FiberData::gc_traverse, false);
}

/// Statics storage shared through an `Rc`: owned only when unshared.
fn statics(v: &mut GcVisitor<'_>, s: &Rc<RefCell<Vec<Option<PhpRef>>>>) {
    if Rc::strong_count(s) != 1 {
        return;
    }
    if let Ok(s) = s.try_borrow() {
        for r in s.iter().flatten() {
            v.reference(r);
        }
    }
}

fn var_store(v: &mut GcVisitor<'_>, s: &Rc<RefCell<VarStore>>) {
    if Rc::strong_count(s) != 1 {
        return;
    }
    match s.try_borrow() {
        Ok(s) => v.values(s.slots.iter().flatten()),
        Err(_) => v.opaque(),
    }
}

fn pending_call(v: &mut GcVisitor<'_>, c: &PendingCall) {
    if let Some(o) = &c.this {
        v.obj(o);
    }
    v.values(c.inline_args[..c.inline_count as usize].iter().flatten());
    v.values(c.args.iter().flatten());
    v.values(c.named.iter().map(|(_, x)| x));
    if let Some(o) = &c.closure {
        v.obj(o);
    }
}

/// A suspended frame (generator or fiber).
fn frame(v: &mut GcVisitor<'_>, f: &Frame) {
    for slot in &f.cvs {
        if let Some(val) = slot.get() {
            v.value(val);
        }
    }
    if f.symtab_attached {
        var_store(v, &f.vars);
    }
    v.values(&f.tmps);
    for it in f.iters.iter().flatten() {
        match it {
            ForeachIter::Array { arr, .. } => v.array(arr),
            ForeachIter::ArrayRef { r, .. } => v.reference(r),
            ForeachIter::Props { subject, .. } => v.value(subject),
            ForeachIter::Iterator { obj, .. } | ForeachIter::Generator { obj, .. } => v.obj(obj),
            // Native iterators keep their own handles.
            ForeachIter::Native(..) | ForeachIter::Empty => {}
        }
    }
    for fin in &f.fins {
        if let FinState::Exception(e) = fin {
            v.obj(e);
        }
    }
    for c in &f.calls {
        pending_call(v, c);
    }
    for o in [&f.this, &f.closure, &f.generator, &f.pending_exc]
        .into_iter()
        .flatten()
    {
        v.obj(o);
    }
    v.values(f.args.iter().flatten());
    v.values(f.named.iter().map(|(_, x)| x));
    if let Some(s) = &f.statics {
        statics(v, s);
    }
}

/// `zend_closure_get_gc`: bound `$this`, captured variables and the
/// closure's own static variables.
impl GcTraverse for ClosureData {
    fn gc_traverse(&self, v: &mut GcVisitor<'_>) {
        if let Some(o) = &self.this {
            v.obj(o);
        }
        v.values(self.vars.iter().map(|(_, x)| x));
        statics(v, &self.statics);
    }
}

/// `zend_generator_get_gc`: the suspended frame, current key/value, return
/// value, `yield from` delegate, parent generator and closure.
impl GcTraverse for GenData {
    fn gc_traverse(&self, v: &mut GcVisitor<'_>) {
        if let Some(f) = &self.frame {
            frame(v, f);
        }
        v.values([&self.key, &self.value, &self.ret].into_iter().flatten());
        match &self.values {
            Some(Delegate::Array(a, _)) => v.array(a),
            Some(Delegate::Iter(o, _)) => v.obj(o),
            None => {}
        }
        if let Some(p) = &self.parent {
            v.obj(p);
        }
        if let Some(c) = &self.closure {
            v.obj(c);
        }
    }
}

/// `zend_fiber_object_gc`, partially: the callable, return value and the
/// frames of a suspended fiber (the resolved call is private to `fiber.rs`,
/// which can register a complete traversal; Rust temporaries on the fiber's
/// native stack stay invisible, i.e. are treated as external references).
impl GcTraverse for FiberData {
    fn gc_traverse(&self, v: &mut GcVisitor<'_>) {
        v.value(&self.callable);
        v.value(&self.ret);
        for f in &self.frames {
            frame(v, f);
        }
    }
}

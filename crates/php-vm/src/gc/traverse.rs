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
use crate::object::ObjectData;
use crate::vm::{FinState, ForeachIter, Frame, PendingCall, VarStore};
use php_runtime::{HashTable, Obj, PhpRef, Value, ZArray};
use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

/// Receives the references held by an object's native state.
pub struct GcVisitor<'a> {
    graph: &'a mut Graph,
    from: u32,
}

impl std::fmt::Debug for GcVisitor<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GcVisitor(node {})", self.from)
    }
}

impl<'a> GcVisitor<'a> {
    pub(super) fn new(graph: &'a mut Graph, from: u32) -> Self {
        GcVisitor { graph, from }
    }

    /// A value owned by the state (scalars and strings are ignored).
    pub fn value(&mut self, v: &Value) {
        self.graph.edge(v);
    }

    /// Several owned values.
    pub fn values<'v>(&mut self, vs: impl IntoIterator<Item = &'v Value>) {
        for v in vs {
            self.graph.edge(v);
        }
    }

    /// An owned object handle.
    pub fn obj(&mut self, o: &Obj) {
        if let Some(t) = self.graph.intern_obj(o) {
            self.graph.link(t);
        }
    }

    /// An owned array handle.
    pub fn array(&mut self, a: &ZArray) {
        let t = self.graph.intern_arr(a);
        self.graph.link(t);
    }

    /// An owned reference handle.
    pub fn reference(&mut self, r: &PhpRef) {
        let t = self.graph.intern_ref(r);
        self.graph.link(t);
    }

    /// The values of a hash table stored inline in the state.
    pub fn table(&mut self, t: &HashTable) {
        self.values(t.values());
    }

    /// An entry whose value is kept alive only while both this object and
    /// the (weakly held) `key` object are alive — a `WeakMap` entry.
    pub fn weak_entry(&mut self, key: &Weak<ObjectData>, v: &Value) {
        self.graph.cond_edge(self.from, key, v);
    }

    /// Declares that the state holds references that cannot be reported
    /// (e.g. a native stack): the object is kept alive.
    pub fn opaque(&mut self) {
        self.graph.pin(self.from);
    }
}

/// Native object state whose references the collector can enumerate (the
/// `get_gc` object handler). See the module documentation for the
/// contract.
pub trait GcTraverse: Any {
    /// Reports every handle the state owns.
    fn gc_traverse(&self, v: &mut GcVisitor<'_>);
}

type TraverseFn = fn(&dyn Any, &mut GcVisitor<'_>);

thread_local! {
    static REGISTRY: RefCell<HashMap<TypeId, TraverseFn>> = RefCell::new(HashMap::new());
}

fn traverse_as<T: GcTraverse>(state: &dyn Any, v: &mut GcVisitor<'_>) {
    if let Some(s) = state.downcast_ref::<T>() {
        s.gc_traverse(v);
    }
}

/// Registers `T` (stored with `object::set_internal`) as traversable.
pub fn register_traverse<T: GcTraverse>() {
    REGISTRY.with(|r| {
        r.borrow_mut()
            .insert(TypeId::of::<T>(), traverse_as::<T> as TraverseFn);
    });
}

pub(super) fn lookup(t: TypeId) -> Option<TraverseFn> {
    REGISTRY
        .try_with(|r| r.try_borrow().ok().and_then(|r| r.get(&t).copied()))
        .ok()
        .flatten()
}

/// Whether native state of this type is traversed (and hence dropped when
/// its object is garbage).
pub(super) fn is_traversed(state: &dyn Any) -> bool {
    lookup(state.type_id()).is_some()
}

pub(super) fn register_engine_traversals() {
    register_traverse::<ClosureData>();
    register_traverse::<GenData>();
    register_traverse::<FiberData>();
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
    v.values(c.args.iter().flatten());
    v.values(c.named.iter().map(|(_, x)| x));
    if let Some(o) = &c.closure {
        v.obj(o);
    }
}

/// A suspended frame (generator or fiber).
fn frame(v: &mut GcVisitor<'_>, f: &Frame) {
    var_store(v, &f.vars);
    v.values(&f.tmps);
    for it in f.iters.iter().flatten() {
        match it {
            ForeachIter::Array { arr, .. } => v.array(arr),
            ForeachIter::ArrayRef { r, .. } => v.reference(r),
            ForeachIter::Props { obj, .. }
            | ForeachIter::Iterator { obj, .. }
            | ForeachIter::Generator { obj, .. } => v.obj(obj),
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
/// value, `yield from` delegate and parent generator.
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
    }
}

/// `zend_fiber_object_gc`: the callable, return value and the frames of a
/// suspended fiber. (Rust temporaries on the fiber's native stack stay
/// invisible, i.e. are treated as external references.)
impl GcTraverse for FiberData {
    fn gc_traverse(&self, v: &mut GcVisitor<'_>) {
        v.value(&self.callable);
        v.value(&self.ret);
        for f in &self.frames {
            frame(v, f);
        }
    }
}

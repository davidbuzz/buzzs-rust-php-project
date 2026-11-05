//! A collection run (`zend_gc_collect_cycles`): analysis, the destructor
//! protocol, freeing, and the single re-run after destructors.

use super::graph::{Graph, Handle, NONE};
use super::traverse::is_traversed;
use super::{with_state, RootProvider};
use crate::object::{self, oflags, ObjectData};
use crate::vm::{Throw, Vm, VmResult};
use php_runtime::{Obj, PhpObject, Value};
use std::any::Any;
use std::rc::{Rc, Weak};
use std::time::Instant;

/// Values emptied out of garbage nodes are released this many at a time.
const TRASH_BATCH: usize = 4096;

/// What a run considers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// `gc_collect_cycles()`: the buffered roots plus every live object.
    Full,
    /// A threshold-triggered run over the buffered roots.
    Auto,
}

/// Disables root recording in every provider while alive.
struct Quiet {
    providers: Vec<(RootProvider, bool)>,
}

impl Quiet {
    fn new(providers: &[RootProvider]) -> Self {
        Quiet {
            providers: providers
                .iter()
                .map(|p| (*p, (p.set_recording)(false)))
                .collect(),
        }
    }
}

impl Drop for Quiet {
    fn drop(&mut self) {
        for (p, old) in self.providers.drain(..).rev() {
            (p.set_recording)(old);
        }
    }
}

/// Clears `active` even when a destructor bails out.
struct ActiveGuard;

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        with_state(|s| s.active = false);
    }
}

/// What a single pass leaves for the next one.
#[derive(Default)]
struct PassResult {
    /// Freed objects and arrays.
    count: i64,
    /// Destructors were called (a re-run is due).
    had_dtors: bool,
    /// Objects whose destructor ran, still possibly garbage (roots of the
    /// re-run): weakly held unless only the collector holds them.
    survivors: Vec<Weak<dyn PhpObject>>,
    /// Objects only the collector still holds after their destructor
    /// (`GC_DELREF` to zero without freeing): freed by the re-run.
    zombies: Vec<Obj>,
    /// Whether the pass had anything to look at (`num_roots != 0`).
    ran: bool,
}

fn has_pending_dtor(d: &ObjectData) -> bool {
    d.flags.get() & oflags::DESTRUCTED == 0 && d.class.magic.destruct.is_some()
}

impl Vm {
    /// Runs a collection; returns the number of freed objects and arrays.
    pub(super) fn gc_run(&mut self, mode: Mode) -> VmResult<i64> {
        let start = Instant::now();
        let Some(providers) = with_state(|s| {
            if s.active || s.protected {
                return None;
            }
            s.active = true;
            Some(s.providers.clone())
        })
        .flatten() else {
            return Ok(0);
        };
        let guard = ActiveGuard;
        let full = mode == Mode::Full;
        let mut extra: Vec<Value> = Vec::new();
        let mut total = 0;
        let mut err: Option<Throw> = None;
        let mut rerun = false;
        loop {
            let pass = self.gc_pass(&providers, full, std::mem::take(&mut extra), &mut err);
            let pass = match pass {
                Ok(p) => p,
                Err(e) => {
                    drop(guard);
                    with_state(|s| s.collector_time += start.elapsed());
                    return Err(e);
                }
            };
            total += pass.count;
            with_state(|s| {
                // The re-run after destructors always finds the destructor
                // objects buffered as roots in php-src.
                if pass.ran || rerun {
                    s.runs += 1;
                }
                s.collected += u64::try_from(pass.count).unwrap_or(0);
            });
            if !pass.had_dtors || rerun {
                // No third pass: zombies are released (their destructor
                // has run, so this simply frees them).
                drop(pass.zombies);
                break;
            }
            // Objects with destructors were removed from this pass: run
            // again right away to free them (once).
            rerun = true;
            extra.extend(pass.zombies.into_iter().map(Value::Object));
            extra.extend(
                pass.survivors
                    .iter()
                    .filter_map(Weak::upgrade)
                    .map(|rc| Value::Object(Obj::from_rc(rc))),
            );
        }
        drop(guard);
        with_state(|s| s.collector_time += start.elapsed());
        // Values released by freeing (e.g. `WeakMap` entries) may have
        // queued destructors of ordinary objects.
        let r = self.run_pending_destructors();
        match err {
            Some(e) => Err(e),
            None => r.map(|()| total),
        }
    }

    /// One mark/scan/collect pass. Destructor exceptions are accumulated in
    /// `err` (chained like `zend_objects_destroy_object`); other unwinds
    /// abort the pass.
    fn gc_pass(
        &mut self,
        providers: &[RootProvider],
        full: bool,
        extra: Vec<Value>,
        err: &mut Option<Throw>,
    ) -> VmResult<PassResult> {
        let quiet = Quiet::new(providers);
        let mut roots = with_state(|s| {
            s.pending = 0;
            s.roots.take()
        })
        .unwrap_or_default();
        for p in providers {
            roots.extend((p.take)());
        }
        roots.extend(extra);
        let had_roots = !roots.is_empty();
        let mut g = Graph::default();
        for r in &roots {
            g.add_root(r);
        }
        drop(roots);
        if full {
            for d in object::live_objects() {
                g.add_object(d);
            }
        }
        g.build();
        let garbage = g.analyze();
        let mut res = PassResult {
            ran: had_roots || garbage > 0,
            ..PassResult::default()
        };
        if garbage == 0 {
            return Ok(res);
        }
        let n = g.nodes.len();
        let mut count: i64 = g
            .nodes
            .iter()
            .filter(|n| !n.live && n.counted())
            .count()
            .try_into()
            .unwrap_or(i64::MAX);
        // Garbage objects whose destructor has to run first.
        let in_d: Vec<bool> = g
            .nodes
            .iter()
            .map(|n| !n.live && n.data().is_some_and(|d| has_pending_dtor(d)) && n.obj().is_some())
            .collect();
        let mut removed = vec![false; n];
        let mut dtors: Vec<Weak<dyn PhpObject>> = Vec::new();
        if in_d.iter().any(|&b| b) {
            res.had_dtors = true;
            // `gc_remove_nested_data_from_buffer`: data reachable from an
            // object with a destructor may be resurrected by it; it is left
            // for the re-run.
            for start in 0..n {
                if !in_d[start] {
                    continue;
                }
                let start = u32::try_from(start).unwrap_or(NONE);
                count -= remove_nested(&g, start, &in_d, &mut removed);
                if let Some(o) = g.nodes[start as usize].obj() {
                    dtors.push(Rc::downgrade(o.rc()));
                }
            }
        }
        // Keep the handles of the nodes to free; release everything else
        // (no destructor can run before the destructor phase below, and
        // nothing here can reach a zero count: every node is still held by
        // its cycle). The edges and indexes are done with, and the node
        // list itself keeps the handles (no second list: peak memory on
        // large heaps).
        g.drop_edges();
        for (i, node) in g.nodes.iter_mut().enumerate() {
            if node.live || removed[i] {
                node.handle = Handle::Taken;
                node.outer = None;
            }
        }
        let to_free = g.nodes;
        drop(quiet);

        // Destructor phase.
        if !dtors.is_empty() {
            let t = Instant::now();
            let r = self.gc_call_destructors(&dtors, err, &mut res.zombies);
            with_state(|s| s.dtor_time += t.elapsed());
            r?;
            res.survivors = dtors;
        }

        // Free phase: break the cycles, then let `Rc` free the nodes.
        let t = Instant::now();
        let quiet = Quiet::new(providers);
        let mut trash: Vec<Value> = Vec::new();
        let mut states: Vec<Box<dyn Any>> = Vec::new();
        for node in to_free.iter() {
            match &node.handle {
                Handle::Obj(inner) => clear_object(inner, &mut trash, &mut states),
                Handle::Ref(r) => {
                    if let Some(mut v) = r.try_borrow_mut() {
                        trash.push(std::mem::take(&mut *v));
                    }
                }
                Handle::Arr(_) | Handle::Taken => {}
            }
            // Released in batches (bounded memory on large heaps):
            // `to_free` still holds every node being freed, so this only
            // releases values outside it; graphs below the batch size keep
            // the single release after the loop (and its free order).
            if trash.len() >= TRASH_BATCH {
                drop(std::mem::take(&mut trash));
            }
        }
        drop(to_free);
        drop(states);
        drop(trash);
        drop(quiet);
        with_state(|s| s.free_time += t.elapsed());
        res.count = count.max(0);
        Ok(res)
    }

    /// Calls the destructors of garbage objects in buffer order
    /// (`gc_call_destructors`), skipping objects freed or destructed in the
    /// meantime. Inside a fiber they run in a dedicated fiber
    /// (`gc_call_destructors_in_fiber`).
    fn gc_call_destructors(
        &mut self,
        dtors: &[Weak<dyn PhpObject>],
        err: &mut Option<Throw>,
        zombies: &mut Vec<Obj>,
    ) -> VmResult<()> {
        if !self.fibers.is_empty() {
            return self.gc_call_destructors_in_fiber(dtors, err, zombies);
        }
        for w in dtors {
            if let Some(z) = self.gc_destruct_one(w, err)? {
                zombies.push(z);
            }
        }
        Ok(())
    }

    /// Runs one garbage object's destructor. Exceptions are chained into
    /// `err` (`zend_objects_destroy_object`); returns the object if only the
    /// collector holds it afterwards (`GC_DELREF` to zero without freeing:
    /// the re-run frees it).
    pub(super) fn gc_destruct_one(
        &mut self,
        w: &Weak<dyn PhpObject>,
        err: &mut Option<Throw>,
    ) -> VmResult<Option<Obj>> {
        let Some(rc) = w.upgrade() else {
            return Ok(None);
        };
        let o = Obj::from_rc(rc);
        let d = object::data(&o);
        if d.flags.get() & oflags::DESTRUCTED != 0 {
            return Ok(None);
        }
        d.flags.set(d.flags.get() | oflags::DESTRUCTED);
        let r = self
            .call_destructor(&o)
            .and_then(|()| self.run_pending_destructors());
        if let Err(e) = r {
            match e {
                Throw::Exception(new) => {
                    if let Some(Throw::Exception(old)) = err.take() {
                        crate::exceptions::chain_previous(&new, &old);
                    }
                    *err = Some(Throw::Exception(new));
                }
                other => return Err(other),
            }
        }
        let sole = o
            .downcast_ref::<crate::object::ObjBox>()
            .is_some_and(|b| Rc::strong_count(&b.0) == 1);
        Ok((o.refcount() == 1 && sole).then_some(o))
    }
}

/// `free_obj` for a garbage object: empties its properties and traversed
/// native state into `trash`, and drops the `WeakMap` entries keyed by it
/// (which may hold the object itself).
fn clear_object(d: &Rc<ObjectData>, trash: &mut Vec<Value>, states: &mut Vec<Box<dyn Any>>) {
    if let Ok(mut slots) = d.slots.try_borrow_mut() {
        trash.extend(slots.iter_mut().filter_map(Option::take));
    }
    if let Ok(mut dynamic) = d.dynamic.try_borrow_mut() {
        if let Some(t) = dynamic.take() {
            trash.push(Value::Array(php_runtime::ZArray::from_table(t)));
        }
    }
    if let Ok(mut state) = d.internal.try_borrow_mut() {
        if state.as_ref().is_some_and(|s| is_traversed(&**s)) {
            states.extend(state.take());
        }
    }
    crate::weakref::object_freed(d.handle);
}

/// Removes the garbage reachable from destructor object `start` from this
/// pass (stopping at other destructor objects); returns how many counted
/// nodes were removed (`start` included).
fn remove_nested(g: &Graph, start: u32, in_d: &[bool], removed: &mut [bool]) -> i64 {
    let mut count = 0;
    let mut stack = vec![start];
    while let Some(x) = stack.pop() {
        let xi = x as usize;
        if removed[xi] || g.nodes[xi].live || (x != start && in_d[xi]) {
            continue;
        }
        removed[xi] = true;
        if g.nodes[xi].counted() {
            count += 1;
        }
        stack.extend(g.children(x));
    }
    count
}

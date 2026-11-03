//! Generators (`Zend/zend_generators.c`).
//!
//! A generator owns its suspended frame; resuming pushes the frame and runs
//! a nested interpreter loop until the next `yield`, `yield from`, `return`
//! or uncaught exception.
//!
//! `yield from <generator>` follows php-src's delegation tree: the
//! delegating generator (a *child*) points at the generator it delegates to
//! (its *parent*). The innermost generator without a parent is the *root*;
//! it is the one actually executing, and its current key/value are the
//! current key/value of every leaf above it. Several generators may delegate
//! to the same generator, which is then shared. When a root finishes, the
//! child on the path to the generator being resumed receives the return
//! value and becomes the new root (`zend_generator_update_current`).
//! `yield from` over arrays and other `Traversable`s iterates them inside
//! the delegating generator ("delegated values").

use crate::builtins::classbuilder::ClassBuilder;
use crate::class::{cflags, InternalKind};
use crate::exec::Flow;
use crate::func::{flags, Args};
use crate::object;
use crate::vm::{Frame, FrameKind, RetTarget, Throw, Vm, VmResult};
use php_runtime::{ArrayKey, HashTable, Obj, PhpObject, Value, ZArray, ZStr};
use std::cell::Cell;
use std::rc::{Rc, Weak};

/// Generator state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenState {
    /// Created, not yet started.
    Created,
    /// Suspended at a `yield` or `yield from`.
    Suspended,
    /// Currently running (`ZEND_GENERATOR_CURRENTLY_RUNNING`).
    Running,
    /// Finished (returned, threw or was destroyed): `execute_data == NULL`.
    Finished,
}

/// Values delegated to by `yield from` over a non-generator.
#[derive(Debug)]
pub enum Delegate {
    /// An array and the next position.
    Array(ZArray, u32),
    /// An `Iterator` and the number of elements fetched so far.
    Iter(Obj, u64),
}

/// Internal state of a `Generator` object.
#[derive(Debug)]
pub struct GenData {
    /// The suspended frame (`None` while running or once finished).
    pub frame: Option<Box<Frame>>,
    /// State.
    pub state: GenState,
    /// Current key (`None` = UNDEF).
    pub key: Option<Value>,
    /// Current value (`None` = UNDEF: not yet primed).
    pub value: Option<Value>,
    /// `largest_used_integer_key`.
    pub largest_key: i64,
    /// Temporary receiving the sent value.
    pub send_target: Option<u32>,
    /// Return value (`None` = UNDEF).
    pub ret: Option<Value>,
    /// Delegated values of `yield from` over an array/`Traversable`.
    pub values: Option<Delegate>,
    /// Whether the frame is suspended at a `yield from`, and the temporary
    /// receiving its result.
    pub yield_from: Option<Option<u32>>,
    /// The generator delegated to (`node.parent`).
    pub parent: Option<Obj>,
    /// Generators delegating to this one (`node.children`).
    children: Vec<Weak<dyn PhpObject>>,
    /// Cached root and the tree epoch it was computed in.
    root: Option<(Weak<dyn PhpObject>, u64)>,
    /// `ZEND_GENERATOR_DO_INIT`.
    do_init: bool,
    /// `ZEND_GENERATOR_AT_FIRST_YIELD`.
    at_first_yield: bool,
    /// `ZEND_GENERATOR_FORCED_CLOSE`.
    forced_close: bool,
    /// Exception to throw at the suspension point on the next resume.
    pending_exc: Option<Obj>,
    /// The frame is on the VM stack below a running delegate (for
    /// backtraces), not in `frame`.
    lent: bool,
    /// Function name for `__debugInfo()`.
    func_name: ZStr,
}

thread_local! {
    /// Bumped whenever a delegation link is removed, invalidating cached
    /// roots.
    static EPOCH: Cell<u64> = const { Cell::new(0) };
}

fn epoch() -> u64 {
    EPOCH.with(Cell::get)
}

fn bump_epoch() {
    EPOCH.with(|e| e.set(e.get() + 1));
}

fn with<R>(g: &Obj, f: impl FnOnce(&mut GenData) -> R) -> Option<R> {
    object::internal::<GenData, R>(g, f)
}

fn state(g: &Obj) -> GenState {
    with(g, |d| d.state).unwrap_or(GenState::Finished)
}

fn finished(g: &Obj) -> bool {
    state(g) == GenState::Finished
}

fn parent_of(g: &Obj) -> Option<Obj> {
    with(g, |d| d.parent.clone()).flatten()
}

fn children_of(g: &Obj) -> Vec<Obj> {
    with(g, |d| d.children.iter().filter_map(Weak::upgrade).map(Obj::from_rc).collect()).unwrap_or_default()
}

fn remove_child(parent: &Obj, child: &Obj) {
    let target = Rc::downgrade(child.rc());
    with(parent, |d| d.children.retain(|w| !Weak::ptr_eq(w, &target) && w.strong_count() > 0));
}

/// Removes `g` from its parent's children and clears its parent link.
fn detach(g: &Obj) {
    if let Some(p) = with(g, |d| d.parent.take()).flatten() {
        remove_child(&p, g);
        bump_epoch();
    }
}

/// Closes a generator whose frame is gone (`zend_generator_close`).
fn close(g: &Obj) {
    with(g, |d| {
        d.state = GenState::Finished;
        d.frame = None;
        d.values = None;
        d.yield_from = None;
        d.send_target = None;
    });
    detach(g);
}

/// Marks a generator as finished (its frame was unwound by an exception).
pub(crate) fn mark_finished(g: &Obj) {
    close(g);
}

/// Finishes a generator with a return value.
pub(crate) fn finish_with(g: &Obj, v: Value) {
    with(g, |d| d.ret = Some(v));
    close(g);
}

/// The internal "graceful exit" object used to unwind a force-closed
/// generator through its `finally` blocks.
fn is_graceful(e: &Obj) -> bool {
    crate::fiber::is_graceful_exit(e)
}

/// Removes graceful-exit objects linked as `previous` of `e` (an exception
/// thrown by a `finally` block while force-closing).
pub(crate) fn strip_graceful_previous(e: &Obj) {
    let mut cur = e.clone();
    for _ in 0..10_000 {
        match crate::exceptions::get_base_prop(&cur, "previous") {
            Some(Value::Object(p)) if is_graceful(&p) => {
                crate::exceptions::set_base_prop(&cur, "previous", Value::Null);
                return;
            }
            Some(Value::Object(p)) => cur = p,
            _ => return,
        }
    }
}

impl Vm {
    /// `ZEND_GENERATOR_CREATE`.
    pub(crate) fn op_generator_create(&mut self, base: usize) -> VmResult<Flow> {
        let fidx = self.frames.len() - 1;
        let mut frame = self.frames.pop().expect("generator frame");
        let ret = frame.ret;
        frame.ret = RetTarget::Discard;
        let func_name = match (&frame.func, &frame.scope) {
            (Some(f), Some(s)) => {
                let mut n = s.name.as_bytes().to_vec();
                n.extend_from_slice(b"::");
                n.extend_from_slice(f.name().as_bytes());
                ZStr::from(n)
            }
            (Some(f), None) => f.name().clone(),
            _ => ZStr::from(""),
        };
        let cls = self.find_class(b"Generator").expect("Generator class");
        let g = object::new_object(cls, Vec::new());
        object::set_internal(
            &g,
            GenData {
                frame: Some(Box::new(frame)),
                state: GenState::Created,
                key: None,
                value: None,
                largest_key: -1,
                send_target: None,
                ret: None,
                values: None,
                yield_from: None,
                parent: None,
                children: Vec::new(),
                root: None,
                do_init: false,
                at_first_yield: false,
                forced_close: false,
                pending_exc: None,
                lent: false,
                func_name,
            },
        );
        let v = Value::Object(g);
        if fidx <= base {
            return Ok(Flow::Done(v));
        }
        if let RetTarget::Tmp(t) = ret {
            self.set_tmp(t, v);
        }
        Ok(Flow::Reload)
    }

    /// Pops the running generator frame and stores it in the generator.
    fn gen_suspend_frame(&mut self, base: usize, update: impl FnOnce(&mut GenData)) -> Flow {
        let fidx = self.frames.len() - 1;
        let mut frame = self.frames.pop().expect("generator frame");
        if let Some(g) = frame.generator.take() {
            with(&g, move |d| {
                update(d);
                d.frame = Some(Box::new(frame));
                d.state = GenState::Suspended;
            });
        }
        if fidx <= base {
            Flow::Done(Value::Null)
        } else {
            Flow::Reload
        }
    }

    fn running_generator(&self) -> Option<Obj> {
        self.frame().generator.clone()
    }

    /// `ZEND_YIELD`.
    pub(crate) fn op_yield(&mut self, key: Option<Value>, val: Option<Value>, dst: Option<u32>, base: usize) -> VmResult<Flow> {
        let Some(g) = self.running_generator() else {
            return Ok(Flow::Next);
        };
        if with(&g, |d| d.forced_close).unwrap_or(false) {
            return Err(self.make_throw("Error", "Cannot yield from finally in a force-closed generator"));
        }
        let by_ref = self
            .frame()
            .ufunc
            .as_ref()
            .is_some_and(|u| u.flags & flags::RETURN_REF != 0);
        let v = val.unwrap_or_default();
        let v = if by_ref { v } else { v.deref() };
        let key = key.map(|k| k.deref());
        if let Some(d) = dst {
            self.set_tmp(d, Value::Null);
        }
        Ok(self.gen_suspend_frame(base, move |d| {
            match key {
                Some(k) => {
                    if let Value::Int(i) = k {
                        if i > d.largest_key {
                            d.largest_key = i;
                        }
                    }
                    d.key = Some(k);
                }
                None => {
                    d.largest_key += 1;
                    d.key = Some(Value::Int(d.largest_key));
                }
            }
            d.value = Some(v);
            d.send_target = dst;
            d.yield_from = None;
        }))
    }

    /// `ZEND_YIELD_FROM`.
    pub(crate) fn op_yield_from(&mut self, src: Value, dst: Option<u32>, base: usize) -> VmResult<Flow> {
        let Some(g) = self.running_generator() else {
            return Ok(Flow::Next);
        };
        if with(&g, |d| d.forced_close).unwrap_or(false) {
            return Err(self.make_throw("Error", "Cannot use \"yield from\" in a force-closed generator"));
        }
        match src.deref() {
            Value::Array(a) => {
                with(&g, |d| d.values = Some(Delegate::Array(a, 0)));
            }
            Value::Object(o) if object::has_internal::<GenData>(&o) => {
                if finished(&o) {
                    let ret = with(&o, |d| d.ret.clone()).flatten();
                    let Some(r) = ret else {
                        return Err(self.make_throw(
                            "Error",
                            "Generator passed to yield from was aborted without proper return and is unable to continue",
                        ));
                    };
                    self.set_dst(dst, r);
                    return Ok(Flow::Next);
                }
                let cur = self.gen_get_current(&o)?;
                if Obj::ptr_eq(&cur, &g) {
                    return Err(self.make_throw("Error", "Impossible to yield from the Generator being currently run"));
                }
                // zend_generator_yield_from
                let weak = Rc::downgrade(g.rc());
                with(&o, |d| d.children.push(weak));
                with(&g, |d| {
                    d.parent = Some(o.clone());
                    d.do_init = true;
                });
            }
            Value::Object(o) if self.instance_of(&o, b"traversable") => {
                let it = self.get_iterator(&o)?;
                self.call_method_name(&it, "rewind", Vec::new())?;
                with(&g, |d| d.values = Some(Delegate::Iter(it, 0)));
            }
            _ => {
                return Err(self.make_throw("Error", "Can use \"yield from\" only with arrays and Traversables"));
            }
        }
        self.set_dst(dst, Value::Null);
        Ok(self.gen_suspend_frame(base, move |d| {
            d.send_target = None;
            d.yield_from = Some(dst);
        }))
    }

    /// `zend_generator_get_current`: the generator whose key/value `g`
    /// currently exposes (the root of its delegation tree).
    pub(crate) fn gen_get_current(&mut self, g: &Obj) -> VmResult<Obj> {
        let Some(parent) = parent_of(g) else {
            return Ok(g.clone());
        };
        let now = epoch();
        let cached = with(g, |d| match &d.root {
            Some((w, e)) if *e == now => w.upgrade().map(Obj::from_rc),
            _ => None,
        })
        .flatten();
        let mut root = cached.unwrap_or(parent);
        while let Some(p) = parent_of(&root) {
            root = p;
        }
        let w = Rc::downgrade(root.rc());
        with(g, |d| d.root = Some((w, now)));
        if !finished(&root) {
            return Ok(root);
        }
        self.gen_update_current(g, &root, false)
    }

    /// `zend_generator_update_current`: the root finished; its child on
    /// the path to `g` takes over and receives the return value.
    fn gen_update_current(&mut self, g: &Obj, old_root: &Obj, exception_pending: bool) -> VmResult<Obj> {
        // Walk down single-child chains from the old root, else up from g.
        let mut cur = old_root.clone();
        let mut found = None;
        loop {
            if !finished(&cur) {
                found = Some(cur);
                break;
            }
            let ch = children_of(&cur);
            if ch.len() != 1 {
                break;
            }
            cur = ch[0].clone();
        }
        let new_root = match found {
            Some(n) => n,
            None => {
                let mut n = g.clone();
                while let Some(p) = parent_of(&n) {
                    if finished(&p) {
                        break;
                    }
                    n = p;
                }
                n
            }
        };
        let Some(new_parent) = parent_of(&new_root) else {
            return Ok(new_root);
        };
        let at_yield_from = with(&new_root, |d| d.yield_from).flatten();
        let mut closed_exc = None;
        if !exception_pending {
            if let Some(dst) = at_yield_from {
                let ret = with(&new_parent, |d| d.ret.clone()).flatten();
                match ret {
                    None => {
                        closed_exc = Some(self.gen_exception_in(
                            &new_root,
                            "ClosedGeneratorException",
                            "Generator yielded from aborted, no return value available",
                        )?);
                    }
                    Some(r) => {
                        let pv = with(&new_parent, |d| d.value.clone()).flatten();
                        with(&new_root, |d| {
                            if pv.is_some() {
                                d.value = pv;
                            }
                            if let (Some(t), Some(f)) = (dst, d.frame.as_mut()) {
                                f.tmps[t as usize] = r;
                            }
                        });
                    }
                }
            }
        }
        detach(&new_root);
        let w = Rc::downgrade(new_root.rc());
        let now = epoch();
        with(g, |d| d.root = Some((w, now)));
        if let Some(e) = closed_exc {
            with(&new_root, |d| d.pending_exc = Some(e));
            if state(old_root) != GenState::Running {
                self.gen_resume(g)?;
                return self.gen_get_current(g);
            }
        }
        Ok(new_root)
    }

    /// Creates an exception as if thrown inside `g`'s frame.
    fn gen_exception_in(&mut self, g: &Obj, class: &str, msg: &str) -> VmResult<Obj> {
        let frame = with(g, |d| d.frame.take()).flatten();
        let pushed = frame.is_some();
        if let Some(f) = frame {
            self.frames.push(*f);
        }
        let r = self.create_exception(class, msg, 0);
        if pushed {
            if let Some(f) = self.frames.pop() {
                with(g, |d| d.frame = Some(Box::new(f)));
            }
        }
        r
    }

    /// `zend_generator_get_next_delegated_value`. Returns `true` when a
    /// value was produced.
    fn gen_next_delegated(&mut self, g: &Obj) -> VmResult<bool> {
        let Some(del) = with(g, |d| d.values.take()).flatten() else {
            return Ok(false);
        };
        match del {
            Delegate::Array(a, pos) => {
                let p = a.valid_pos(pos);
                if let Some((k, v)) = a.at(p) {
                    let (k, v) = (k.to_value(), v.deref());
                    with(g, |d| {
                        d.value = Some(v);
                        d.key = Some(k);
                        d.values = Some(Delegate::Array(a.clone(), p + 1));
                    });
                    return Ok(true);
                }
                Ok(false)
            }
            Delegate::Iter(it, index) => {
                if index > 0 {
                    self.call_method_name(&it, "next", Vec::new())?;
                }
                let valid = self.call_method_name(&it, "valid", Vec::new())?;
                if !self.to_bool(&valid)? {
                    return Ok(false);
                }
                let v = self.call_method_name(&it, "current", Vec::new())?;
                let k = self.call_method_name(&it, "key", Vec::new())?;
                with(g, |d| {
                    d.value = Some(v);
                    d.key = Some(k);
                    d.values = Some(Delegate::Iter(it, index + 1));
                });
                Ok(true)
            }
        }
    }

    /// Runs `g`'s frame until it suspends or finishes, throwing its
    /// pending exception (if any) at the suspension point first.
    fn gen_run(&mut self, g: &Obj) -> VmResult<()> {
        let Some((frame, exc)) = with(g, |d| {
            d.state = GenState::Running;
            d.yield_from = None;
            (d.frame.take(), d.pending_exc.take())
        }) else {
            return Ok(());
        };
        let Some(mut frame) = frame else {
            close(g);
            return exc.map_or(Ok(()), |e| Err(Throw::Exception(e)));
        };
        frame.generator = Some(g.clone());
        self.frames.push(*frame);
        let base = self.frames.len() - 1;
        let r = match exc {
            Some(e) => match self.handle_exception(e, base) {
                Ok(()) => self.execute(base).map(|_| ()),
                Err(t) => Err(t),
            },
            None => self.execute(base).map(|_| ()),
        };
        if r.is_err() && state(g) != GenState::Finished {
            // The frame was unwound (exit, fatal error).
            close(g);
        }
        r
    }

    /// `zend_generator_resume`.
    fn gen_resume(&mut self, orig: &Obj) -> VmResult<()> {
        let mut gen = self.gen_get_current(orig)?;
        if finished(&gen) {
            return Ok(());
        }
        let mut delegator = orig.clone();
        loop {
            if state(&gen) == GenState::Running {
                return Err(self.make_throw("Error", "Cannot resume an already running generator"));
            }
            let do_init = with(&delegator, |d| std::mem::take(&mut d.do_init)).unwrap_or(false);
            if do_init {
                let primed = with(&gen, |d| d.value.is_some() && d.pending_exc.is_none()).unwrap_or(false);
                if primed {
                    return Ok(());
                }
            }
            with(orig, |d| d.at_first_yield = false);
            let has_values = with(&gen, |d| d.values.is_some() && d.pending_exc.is_none()).unwrap_or(false);
            if has_values {
                with(&gen, |d| d.state = GenState::Running);
                let r = self.gen_next_delegated(&gen);
                with(&gen, |d| d.state = GenState::Suspended);
                match r {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(Throw::Exception(e)) => {
                        with(&gen, |d| {
                            d.values = None;
                            d.pending_exc = Some(e);
                        });
                    }
                    Err(t) => return Err(t),
                }
            }
            let chain = self.gen_lend_chain(orig, &gen);
            let r = self.gen_run(&gen);
            self.gen_return_chain(chain);
            match r {
                Ok(()) => {}
                Err(Throw::Exception(e)) => {
                    if Obj::ptr_eq(&gen, orig) {
                        return Err(Throw::Exception(e));
                    }
                    // Rethrow into the generator that delegated to it.
                    let next = match parent_of(orig) {
                        Some(_) => {
                            let root = self.gen_root_of(orig);
                            if finished(&root) {
                                self.gen_update_current(orig, &root, true)?
                            } else {
                                root
                            }
                        }
                        None => orig.clone(),
                    };
                    if finished(&next) {
                        return Err(Throw::Exception(e));
                    }
                    with(&next, |d| {
                        d.values = None;
                        d.pending_exc = Some(e);
                    });
                    gen = next;
                    delegator = orig.clone();
                    continue;
                }
                Err(t) => return Err(t),
            }
            if !finished(&gen) && with(&gen, |d| d.yield_from.is_some()).unwrap_or(false) {
                delegator = gen.clone();
                gen = self.gen_get_current(orig)?;
                continue;
            }
            if !Obj::ptr_eq(&gen, orig) && finished(&gen) && with(&gen, |d| d.ret.is_some()).unwrap_or(false) {
                delegator = orig.clone();
                gen = self.gen_get_current(orig)?;
                if finished(&gen) {
                    return Ok(());
                }
                continue;
            }
            return Ok(());
        }
    }

    /// Pushes the suspended frames of the generators delegating from
    /// `leaf` down to `root` (exclusive), so that backtraces show the
    /// `yield from` chain as php-src's placeholder frames do.
    fn gen_lend_chain(&mut self, leaf: &Obj, root: &Obj) -> Vec<Obj> {
        // Bounded: deep delegation chains (recursive `yield from`) would
        // make every resumption linear in the depth.
        const MAX_LENT: usize = 64;
        let mut chain = Vec::new();
        let mut cur = leaf.clone();
        while !Obj::ptr_eq(&cur, root) {
            if chain.len() == MAX_LENT {
                return Vec::new();
            }
            chain.push(cur.clone());
            match parent_of(&cur) {
                Some(p) => cur = p,
                None => return Vec::new(),
            }
        }
        let mut lent = Vec::new();
        for g in chain {
            let frame = with(&g, |d| {
                let f = d.frame.take();
                d.lent = f.is_some();
                f
            })
            .flatten();
            if let Some(f) = frame {
                self.frames.push(*f);
                lent.push(g);
            }
        }
        lent
    }

    /// Takes back the frames lent by [`Vm::gen_lend_chain`].
    fn gen_return_chain(&mut self, lent: Vec<Obj>) {
        for g in lent.into_iter().rev() {
            if let Some(f) = self.frames.pop() {
                with(&g, |d| {
                    d.lent = false;
                    if d.state != GenState::Finished {
                        d.frame = Some(Box::new(f));
                    }
                });
            }
        }
    }

    /// The root of `g`'s delegation tree without updating anything.
    fn gen_root_of(&self, g: &Obj) -> Obj {
        let mut root = g.clone();
        while let Some(p) = parent_of(&root) {
            root = p;
        }
        root
    }

    /// `zend_generator_ensure_initialized`.
    fn gen_ensure_initialized(&mut self, g: &Obj) -> VmResult<()> {
        let need = with(g, |d| d.value.is_none() && d.state != GenState::Finished && d.parent.is_none()).unwrap_or(false);
        if need {
            self.gen_resume(g)?;
            with(g, |d| d.at_first_yield = true);
        }
        Ok(())
    }

    /// `Generator::next()` (also used by `foreach`).
    pub(crate) fn gen_next(&mut self, g: &Obj) -> VmResult<()> {
        self.gen_ensure_initialized(g)?;
        self.gen_resume(g)
    }

    /// `Generator::send()`.
    pub(crate) fn gen_send(&mut self, g: &Obj, v: Value) -> VmResult<Value> {
        self.gen_ensure_initialized(g)?;
        if finished(g) {
            return Ok(Value::Null);
        }
        let root = self.gen_get_current(g)?;
        with(&root, |d| {
            if d.state != GenState::Running {
                if let (Some(t), Some(f)) = (d.send_target, d.frame.as_mut()) {
                    f.tmps[t as usize] = v;
                }
            }
        });
        self.gen_resume(g)?;
        let root = self.gen_get_current(g)?;
        if finished(g) {
            return Ok(Value::Null);
        }
        Ok(with(&root, |d| d.value.clone()).flatten().unwrap_or_default().deref())
    }

    /// Whether a generator has a current element (starting it if needed).
    pub(crate) fn gen_valid(&mut self, g: &Obj) -> VmResult<bool> {
        self.gen_ensure_initialized(g)?;
        self.gen_get_current(g)?;
        Ok(!finished(g))
    }

    /// Current key and value (the value keeps a yielded reference).
    pub(crate) fn gen_current(&mut self, g: &Obj) -> VmResult<(Value, Value)> {
        self.gen_ensure_initialized(g)?;
        let root = self.gen_get_current(g)?;
        if finished(g) {
            return Ok((Value::Null, Value::Null));
        }
        Ok(with(&root, |d| {
            (
                d.key.clone().unwrap_or_default().deref(),
                d.value.clone().unwrap_or_default(),
            )
        })
        .unwrap_or_default())
    }

    /// `Generator::rewind()` / `foreach` rewind.
    pub(crate) fn gen_rewind(&mut self, g: &Obj) -> VmResult<()> {
        self.gen_ensure_initialized(g)?;
        if !with(g, |d| d.at_first_yield).unwrap_or(false) {
            return Err(self.make_throw("Exception", "Cannot rewind a generator that was already run"));
        }
        Ok(())
    }

    /// Whether a generator is finished.
    pub(crate) fn gen_finished(&self, g: &Obj) -> bool {
        finished(g)
    }

    /// `Generator::throw()`.
    fn gen_throw(&mut self, g: &Obj, e: Obj) -> VmResult<Value> {
        self.gen_ensure_initialized(g)?;
        if finished(g) {
            return Err(Throw::Exception(e));
        }
        let root = self.gen_get_current(g)?;
        if state(&root) == GenState::Running {
            return Err(self.make_throw("Error", "Cannot resume an already running generator"));
        }
        with(&root, |d| {
            d.values = None;
            d.pending_exc = Some(e);
        });
        self.gen_resume(g)?;
        let root = self.gen_get_current(g)?;
        if finished(g) {
            return Ok(Value::Null);
        }
        Ok(with(&root, |d| d.value.clone()).flatten().unwrap_or_default().deref())
    }

    /// `zend_generator_dtor_storage`: runs pending `finally` blocks of a
    /// suspended generator that is being destroyed.
    fn gen_destroy(&mut self, g: &Obj) -> VmResult<()> {
        // Executing (possibly suspended inside a fiber): the frames are
        // unwound with their owner; only forbid yielding from `finally`.
        if with(g, |d| d.state == GenState::Running || d.lent).unwrap_or(false) {
            with(g, |d| d.forced_close = true);
            return Ok(());
        }
        with(g, |d| d.values = None);
        // A generator delegated to keeps running for its other children.
        for c in children_of(g) {
            detach(&c);
        }
        // Releasing the delegate destroys it first when this was its last
        // reference (`OBJ_RELEASE(&parent->std)`).
        if let Some(p) = with(g, |d| d.parent.take()).flatten() {
            remove_child(&p, g);
            bump_epoch();
            if Rc::strong_count(p.rc()) == 1 {
                self.gen_destroy(&p)?;
            }
        }
        let in_finally_scope = with(g, |d| {
            d.frame.as_ref().is_some_and(|f| {
                let ip = f.ip.saturating_sub(1) as u32;
                f.code.try_table.iter().any(|e| {
                    e.finally.is_some_and(|(_, fs, _)| ip >= e.try_start && ip < fs)
                })
            })
        })
        .unwrap_or(false);
        if state(g) != GenState::Suspended || !in_finally_scope {
            close(g);
            return Ok(());
        }
        let Some(ge) = crate::fiber::create_graceful_exit() else {
            close(g);
            return Ok(());
        };
        // zend_generator_cleanup_unfinished_execution: release pending
        // calls and loop variables before entering `finally`.
        let released = with(g, |d| {
            d.forced_close = true;
            d.pending_exc = Some(ge);
            d.frame.as_mut().map(|f| (std::mem::take(&mut f.calls), f.iters.iter_mut().map(Option::take).collect::<Vec<_>>()))
        })
        .flatten();
        drop(released);
        self.run_pending_destructors()?;
        let r = self.gen_run(g);
        close(g);
        match r {
            Err(Throw::Exception(e)) if is_graceful(&e) => Ok(()),
            Err(Throw::Exception(e)) => {
                strip_graceful_previous(&e);
                Err(Throw::Exception(e))
            }
            other => other,
        }
    }
}

fn gen_obj(a: &Args) -> Obj {
    a.this_obj()
}

fn g_current(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let g = gen_obj(a);
    Ok(vm.gen_current(&g)?.1.deref())
}

fn g_key(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let g = gen_obj(a);
    Ok(vm.gen_current(&g)?.0)
}

fn g_next(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let g = gen_obj(a);
    vm.gen_next(&g)?;
    Ok(Value::Null)
}

fn g_valid(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let g = gen_obj(a);
    Ok(Value::Bool(vm.gen_valid(&g)?))
}

fn g_rewind(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let g = gen_obj(a);
    vm.gen_rewind(&g)?;
    Ok(Value::Null)
}

fn g_send(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let g = gen_obj(a);
    let v = a.at(0);
    vm.gen_send(&g, v)
}

fn g_throw(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let g = gen_obj(a);
    let Value::Object(e) = a.at(0) else {
        return Ok(Value::Null);
    };
    vm.gen_throw(&g, e)
}

fn g_get_return(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let g = gen_obj(a);
    vm.gen_ensure_initialized(&g)?;
    match with(&g, |d| d.ret.clone()).flatten() {
        Some(v) => Ok(v),
        None => Err(vm.make_throw(
            "Exception",
            "Cannot get return value of a generator that hasn't returned",
        )),
    }
}

fn g_debug_info(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let g = gen_obj(a);
    let name = with(&g, |d| d.func_name.clone()).unwrap_or_else(|| ZStr::from(""));
    let mut t = HashTable::new();
    t.insert(ArrayKey::Str(ZStr::from("function")), Value::Str(name));
    Ok(Value::Array(ZArray::from_table(t)))
}

/// The `dtor_obj` handler.
fn g_destruct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let g = gen_obj(a);
    // php-src destroys without a call frame: the generator appears called
    // from the code that released it (or from internal code at shutdown).
    let own = vm.frames.pop();
    let hidden = vm.frames.is_empty();
    if hidden {
        vm.frames.push(Frame::with_vars(vm.empty_code.clone(), FrameKind::Builtin, vm.empty_vars.clone(), None));
    }
    let r = vm.gen_destroy(&g);
    if hidden {
        vm.frames.pop();
    }
    if let Some(f) = own {
        vm.frames.push(f);
    }
    r.map(|()| Value::Null)
}

/// Registers the `Generator` and `ClosedGeneratorException` classes.
pub(crate) fn register(vm: &mut Vm) {
    ClassBuilder::new("Generator")
        .flags(cflags::FINAL | cflags::NO_NEW | cflags::NOT_SERIALIZABLE | cflags::NO_CLONE | cflags::NO_DYNAMIC_PROPERTIES)
        .internal(InternalKind::Generator)
        .implements(vm, &["Iterator"])
        .method("rewind(): void", flags::PUBLIC, g_rewind)
        .method("valid(): bool", flags::PUBLIC, g_valid)
        .method("current(): mixed", flags::PUBLIC, g_current)
        .method("key(): mixed", flags::PUBLIC, g_key)
        .method("next(): void", flags::PUBLIC, g_next)
        .method("send(mixed $value): mixed", flags::PUBLIC, g_send)
        .method("throw(Throwable $exception): mixed", flags::PUBLIC, g_throw)
        .method("getReturn(): mixed", flags::PUBLIC, g_get_return)
        .method("__debugInfo(): array", flags::PUBLIC, g_debug_info)
        // The `dtor_obj` handler (php-src has no such method).
        .method("__destruct(): void", flags::PUBLIC, g_destruct)
        .register(vm);
    if vm.find_class(b"ClosedGeneratorException").is_none() {
        ClassBuilder::new("ClosedGeneratorException").extends(vm, "Exception").register(vm);
    }
}

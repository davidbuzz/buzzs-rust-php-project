//! Generators (`Zend/zend_generators.c`): suspended frames resumed by a
//! nested interpreter loop.

use crate::builtins::classbuilder::ClassBuilder;
use crate::class::{cflags, InternalKind};
use crate::exec::Flow;
use crate::func::{flags, Args};
use crate::object;
use crate::vm::{Frame, RetTarget, Throw, Vm, VmResult};
use php_runtime::{Obj, Value, ZArray};

/// Generator state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenState {
    /// Created, not yet started.
    Created,
    /// Suspended at a `yield`.
    Suspended,
    /// Currently running.
    Running,
    /// Finished (returned or threw).
    Finished,
}

/// `yield from` delegation state.
#[derive(Debug)]
pub enum Delegate {
    /// Iterating an array.
    Array(ZArray, u32),
    /// Delegating to another generator.
    Gen(Obj),
    /// Delegating to an `Iterator`.
    Iter(Obj, bool),
}

/// Internal state of a `Generator` object.
#[derive(Debug)]
pub struct GenData {
    /// The suspended frame.
    pub frame: Option<Box<Frame>>,
    /// State.
    pub state: GenState,
    /// Current key.
    pub key: Value,
    /// Current value.
    pub value: Value,
    /// Next auto key.
    pub auto_key: i64,
    /// Temporary receiving the sent value.
    pub send_target: Option<u32>,
    /// Return value.
    pub ret: Option<Value>,
    /// Active delegation.
    pub delegate: Option<Delegate>,
    /// Whether the generator has advanced past its first yield.
    pub advanced: bool,
}

fn with<R>(g: &Obj, f: impl FnOnce(&mut GenData) -> R) -> Option<R> {
    object::internal::<GenData, R>(g, f)
}

/// Marks a generator as finished (its frame was unwound by an exception).
pub(crate) fn mark_finished(g: &Obj) {
    with(g, |d| {
        d.state = GenState::Finished;
        d.value = Value::Null;
        d.key = Value::Null;
        d.delegate = None;
    });
}

/// Finishes a generator with a return value.
pub(crate) fn finish_with(g: &Obj, v: Value) {
    with(g, |d| {
        d.state = GenState::Finished;
        d.ret = Some(v);
        d.value = Value::Null;
        d.key = Value::Null;
    });
}

impl Vm {
    /// `ZEND_GENERATOR_CREATE`.
    pub(crate) fn op_generator_create(&mut self, base: usize) -> VmResult<Flow> {
        let fidx = self.frames.len() - 1;
        let mut frame = self.frames.pop().expect("generator frame");
        let ret = frame.ret;
        frame.ret = RetTarget::Discard;
        let cls = self.find_class(b"Generator").expect("Generator class");
        let g = object::new_object(cls, Vec::new());
        object::set_internal(
            &g,
            GenData {
                frame: Some(Box::new(frame)),
                state: GenState::Created,
                key: Value::Null,
                value: Value::Null,
                auto_key: 0,
                send_target: None,
                ret: None,
                delegate: None,
                advanced: false,
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

    fn suspend(&mut self, key: Option<Value>, val: Value, dst: Option<u32>, base: usize, rewind_ip: bool) -> VmResult<Flow> {
        let fidx = self.frames.len() - 1;
        let mut frame = self.frames.pop().expect("generator frame");
        if rewind_ip {
            frame.ip -= 1;
        }
        let g = frame.generator.take().expect("generator frame has generator");
        with(&g, move |d| {
            match key {
                Some(k) => {
                    if let Value::Int(i) = k {
                        if i >= d.auto_key {
                            d.auto_key = i + 1;
                        }
                    }
                    d.key = k;
                }
                None => {
                    d.key = Value::Int(d.auto_key);
                    d.auto_key += 1;
                }
            }
            d.value = val;
            d.send_target = dst;
            d.frame = Some(Box::new(frame));
            d.state = GenState::Suspended;
        });
        if fidx <= base {
            Ok(Flow::Done(Value::Null))
        } else {
            Ok(Flow::Reload)
        }
    }

    /// `yield`.
    pub(crate) fn op_yield(&mut self, key: Option<Value>, val: Option<Value>, dst: Option<u32>, base: usize) -> VmResult<Flow> {
        let by_ref = self
            .frame()
            .ufunc
            .as_ref()
            .is_some_and(|u| u.flags & flags::RETURN_REF != 0);
        let v = val.unwrap_or_default();
        let v = if by_ref { v } else { v.deref() };
        if let Some(d) = dst {
            self.set_tmp(d, Value::Null);
        }
        self.suspend(key, v, dst, base, false)
    }

    /// `yield from`.
    pub(crate) fn op_yield_from(&mut self, src: Value, dst: Option<u32>, base: usize) -> VmResult<Flow> {
        let g = self.frame().generator.clone().expect("generator");
        let has_delegate = with(&g, |d| d.delegate.is_some()).unwrap_or(false);
        if !has_delegate {
            let src = src.deref();
            let del = match src {
                Value::Array(a) => Delegate::Array(a, 0),
                Value::Object(o) if object::has_internal::<GenData>(&o) => {
                    if Obj::ptr_eq(&o, &g) {
                        return Err(self.make_throw("Error", "Impossible to yield from the Generator being currently run"));
                    }
                    let st = with(&o, |d| d.state).unwrap_or(GenState::Finished);
                    if st == GenState::Finished {
                        let has_ret = with(&o, |d| d.ret.is_some()).unwrap_or(false);
                        if !has_ret {
                            return Err(self.make_throw("Error", "Generator passed to yield from was aborted without proper return and is unable to continue"));
                        }
                    }
                    self.gen_ensure_started(&o)?;
                    Delegate::Gen(o)
                }
                Value::Object(o) if self.instance_of(&o, b"traversable") => {
                    let it = self.get_iterator(&o)?;
                    self.call_method_name(&it, "rewind", Vec::new())?;
                    Delegate::Iter(it, false)
                }
                _ => {
                    return Err(self.make_throw(
                        "Error",
                        "Can use \"yield from\" only with arrays and Traversables",
                    ))
                }
            };
            with(&g, |d| d.delegate = Some(del));
        } else {
            // Resumed: advance the delegate.
            let sent = dst.map(|t| std::mem::take(&mut self.frame_mut().tmps[t as usize]));
            let del = with(&g, |d| d.delegate.take()).flatten();
            let del = match del {
                Some(Delegate::Array(a, pos)) => Some(Delegate::Array(a, pos + 1)),
                Some(Delegate::Gen(o)) => {
                    match sent {
                        Some(v) if !v.is_null() => {
                            self.gen_send(&o, v)?;
                        }
                        _ => self.gen_next(&o)?,
                    }
                    Some(Delegate::Gen(o))
                }
                Some(Delegate::Iter(o, _)) => {
                    self.call_method_name(&o, "next", Vec::new())?;
                    Some(Delegate::Iter(o, true))
                }
                None => None,
            };
            with(&g, |d| d.delegate = del);
        }
        // Produce the current item of the delegate, or finish.
        let del = with(&g, |d| d.delegate.take()).flatten();
        match del {
            Some(Delegate::Array(a, pos)) => {
                let p = a.valid_pos(pos);
                if let Some((k, v)) = a.at(p) {
                    let (k, v) = (k.to_value(), v.deref());
                    with(&g, |d| d.delegate = Some(Delegate::Array(a.clone(), p)));
                    return self.suspend_delegate(k, v, dst, base);
                }
                if let Some(t) = dst {
                    self.set_tmp(t, Value::Null);
                }
                Ok(Flow::Next)
            }
            Some(Delegate::Gen(o)) => {
                let st = with(&o, |d| d.state).unwrap_or(GenState::Finished);
                if st != GenState::Finished {
                    let (k, v) = with(&o, |d| (d.key.clone(), d.value.clone())).unwrap_or_default();
                    with(&g, |d| d.delegate = Some(Delegate::Gen(o.clone())));
                    return self.suspend_delegate(k, v, dst, base);
                }
                let r = with(&o, |d| d.ret.clone()).flatten().unwrap_or_default();
                if let Some(t) = dst {
                    self.set_tmp(t, r);
                }
                Ok(Flow::Next)
            }
            Some(Delegate::Iter(o, started)) => {
                let valid = self.call_method_name(&o, "valid", Vec::new())?;
                if self.to_bool(&valid)? {
                    let v = self.call_method_name(&o, "current", Vec::new())?;
                    let k = self.call_method_name(&o, "key", Vec::new())?;
                    with(&g, |d| d.delegate = Some(Delegate::Iter(o.clone(), started)));
                    return self.suspend_delegate(k, v, dst, base);
                }
                if let Some(t) = dst {
                    self.set_tmp(t, Value::Null);
                }
                Ok(Flow::Next)
            }
            None => Ok(Flow::Next),
        }
    }

    fn suspend_delegate(&mut self, k: Value, v: Value, dst: Option<u32>, base: usize) -> VmResult<Flow> {
        // Keys from delegates do not change the auto key.
        let g = self.frame().generator.clone().expect("generator");
        let saved = with(&g, |d| d.auto_key).unwrap_or(0);
        let r = self.suspend(Some(k), v, dst, base, true);
        with(&g, |d| d.auto_key = saved);
        r
    }

    /// Resumes a generator (optionally throwing an exception at the yield).
    fn gen_resume(&mut self, g: &Obj, throw: Option<Obj>) -> VmResult<()> {
        let st = with(g, |d| d.state).unwrap_or(GenState::Finished);
        match st {
            GenState::Finished => {
                if let Some(e) = throw {
                    return Err(Throw::Exception(e));
                }
                return Ok(());
            }
            GenState::Running => {
                return Err(self.make_throw("Error", "Cannot resume an already running generator"));
            }
            _ => {}
        }
        let frame = with(g, |d| {
            d.state = GenState::Running;
            d.frame.take()
        })
        .flatten();
        let Some(mut frame) = frame else {
            return Ok(());
        };
        frame.generator = Some(g.clone());
        self.frames.push(*frame);
        let base = self.frames.len() - 1;
        let r = match throw {
            Some(e) => match self.handle_exception(e, base) {
                Ok(()) => self.execute(base).map(|_| ()),
                Err(t) => Err(t),
            },
            None => self.execute(base).map(|_| ()),
        };
        if r.is_err() {
            // The frame was unwound.
            with(g, |d| {
                if d.state == GenState::Running {
                    d.state = GenState::Finished;
                    d.value = Value::Null;
                    d.key = Value::Null;
                }
            });
        }
        r
    }

    /// Runs a fresh generator to its first yield.
    pub(crate) fn gen_ensure_started(&mut self, g: &Obj) -> VmResult<()> {
        let st = with(g, |d| d.state).unwrap_or(GenState::Finished);
        if st == GenState::Created {
            self.gen_resume(g, None)?;
        }
        Ok(())
    }

    /// `Generator::next()`.
    pub(crate) fn gen_next(&mut self, g: &Obj) -> VmResult<()> {
        let st = with(g, |d| d.state).unwrap_or(GenState::Finished);
        if st == GenState::Created {
            self.gen_resume(g, None)?;
        }
        with(g, |d| d.advanced = true);
        // Set the sent value to null.
        self.gen_resume(g, None)
    }

    /// `Generator::send()`.
    pub(crate) fn gen_send(&mut self, g: &Obj, v: Value) -> VmResult<Value> {
        let st = with(g, |d| d.state).unwrap_or(GenState::Finished);
        if st == GenState::Created {
            self.gen_resume(g, None)?;
        }
        with(g, |d| d.advanced = true);
        let target = with(g, |d| d.send_target).flatten();
        if let Some(t) = target {
            with(g, |d| {
                if let Some(f) = &mut d.frame {
                    f.tmps[t as usize] = v.clone();
                }
            });
        }
        self.gen_resume(g, None)?;
        Ok(with(g, |d| d.value.clone()).unwrap_or_default())
    }

    /// Whether a generator has a current element (starting it if needed).
    pub(crate) fn gen_valid(&mut self, g: &Obj) -> VmResult<bool> {
        self.gen_ensure_started(g)?;
        Ok(with(g, |d| d.state != GenState::Finished).unwrap_or(false))
    }

    /// Current key and value.
    pub(crate) fn gen_current(&mut self, g: &Obj) -> VmResult<(Value, Value)> {
        self.gen_ensure_started(g)?;
        Ok(with(g, |d| (d.key.clone(), d.value.clone())).unwrap_or_default())
    }

    /// Prepares a generator for `foreach`.
    pub(crate) fn gen_rewind(&mut self, g: &Obj) -> VmResult<()> {
        self.gen_ensure_started(g)?;
        let advanced = with(g, |d| d.advanced).unwrap_or(false);
        if advanced {
            return Err(self.make_throw("Exception", "Cannot rewind a generator that was already run"));
        }
        Ok(())
    }

    /// Whether a generator is finished.
    pub(crate) fn gen_finished(&self, g: &Obj) -> bool {
        with(g, |d| d.state == GenState::Finished).unwrap_or(true)
    }
}

fn gen_obj(a: &Args) -> Obj {
    a.this_obj()
}

fn g_current(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let g = gen_obj(a);
    Ok(vm.gen_current(&g)?.1)
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
    let st = with(&g, |d| d.state).unwrap_or(GenState::Finished);
    if st == GenState::Created {
        // Throwing into a fresh generator: it is first started.
        vm.gen_resume(&g, None)?;
    }
    // Delegate throws go to the inner generator.
    let inner = with(&g, |d| match &d.delegate {
        Some(Delegate::Gen(o)) => Some(o.clone()),
        _ => None,
    })
    .flatten();
    if let Some(i) = inner {
        let r = vm.gen_resume(&i, Some(e.clone()));
        if let Err(Throw::Exception(ex)) = r {
            with(&g, |d| d.delegate = None);
            vm.gen_resume(&g, Some(ex))?;
        } else {
            r?;
            vm.gen_resume(&g, None)?;
        }
    } else {
        vm.gen_resume(&g, Some(e))?;
    }
    Ok(with(&g, |d| d.value.clone()).unwrap_or_default())
}

fn g_get_return(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let g = gen_obj(a);
    let r = with(&g, |d| (d.state, d.ret.clone()));
    match r {
        Some((GenState::Finished, Some(v))) => Ok(v),
        Some((GenState::Finished, None)) => Err(vm.make_throw(
            "Exception",
            "Cannot get return value of a generator that hasn't returned",
        )),
        _ => Err(vm.make_throw(
            "Exception",
            "Cannot get return value of a generator that hasn't returned",
        )),
    }
}

/// Registers the `Generator` class.
pub(crate) fn register(vm: &mut Vm) {
    ClassBuilder::new("Generator")
        .flags(cflags::FINAL | cflags::NO_NEW | cflags::NOT_SERIALIZABLE)
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
        .register(vm);
}

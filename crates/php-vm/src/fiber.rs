//! Fibers (`Zend/zend_fibers.c`).
//!
//! A fiber runs its callable in a nested interpreter loop whose base frame
//! is the fiber function. `Fiber::suspend()` moves the fiber's frames off
//! the VM stack into the fiber object and unwinds to `start()`/`resume()`
//! with [`Throw::FiberSuspend`]; resuming pushes them back. Suspending from
//! inside an internal callback (e.g. `array_map`) is not supported.

use crate::builtins::classbuilder::ClassBuilder;
use crate::class::cflags;
use crate::func::{flags, Args};
use crate::object;
use crate::vm::{Frame, FrameKind, Throw, Vm, VmResult};
use php_runtime::{Obj, Value};

/// Fiber state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FiberState {
    /// Constructed, not started.
    Init,
    /// Running.
    Running,
    /// Suspended.
    Suspended,
    /// Returned.
    Returned,
    /// Threw.
    Threw,
}

/// Internal state of a `Fiber`.
#[derive(Debug)]
pub struct FiberData {
    /// The callable.
    pub callable: Value,
    /// State.
    pub state: FiberState,
    /// Saved frames while suspended.
    pub frames: Vec<Frame>,
    /// Temporary of the top frame receiving the resume value.
    pub resume_dst: Option<u32>,
    /// Index of the fiber's base frame while running.
    pub base: usize,
    /// Value passed by `suspend()`.
    pub transfer: Value,
    /// Return value.
    pub ret: Value,
}

fn with<R>(o: &Obj, f: impl FnOnce(&mut FiberData) -> R) -> Option<R> {
    object::internal::<FiberData, R>(o, f)
}

impl Vm {
    fn fiber_error(&mut self, msg: &str) -> Throw {
        self.make_throw("FiberError", msg)
    }

    /// Runs the fiber until it suspends, returns or throws.
    fn fiber_run(&mut self, fiber: &Obj, r: VmResult<Value>) -> VmResult<Value> {
        self.fibers.pop();
        match r {
            Ok(v) => {
                with(fiber, |d| {
                    d.state = FiberState::Returned;
                    d.ret = v;
                });
                Ok(Value::Null)
            }
            Err(Throw::FiberSuspend) => {
                let v = with(fiber, |d| std::mem::take(&mut d.transfer)).unwrap_or_default();
                Ok(v)
            }
            Err(e) => {
                with(fiber, |d| d.state = FiberState::Threw);
                Err(e)
            }
        }
    }

    /// `Fiber::suspend()` implementation: called from the builtin frame.
    fn fiber_suspend(&mut self, v: Value) -> VmResult<Value> {
        let Some(fiber) = self.fibers.last().cloned() else {
            return Err(self.fiber_error("Cannot suspend outside of fiber"));
        };
        let base = with(&fiber, |d| d.base).unwrap_or(0);
        // Internal frames between the fiber base and the suspend call mean
        // a nested native call we cannot unwind.
        let top = self.frames.len() - 1;
        if self.frames[base..top].iter().any(|f| f.kind == FrameKind::Builtin) {
            return Err(self.fiber_error("Cannot switch fibers in current execution state"));
        }
        let mut frames: Vec<Frame> = self.frames.drain(base..).collect();
        frames.pop(); // the builtin frame of Fiber::suspend()
        let dst = self.pending_call_dst.take();
        with(&fiber, move |d| {
            d.frames = frames;
            d.resume_dst = dst;
            d.state = FiberState::Suspended;
            d.transfer = v;
        });
        Err(Throw::FiberSuspend)
    }

    fn fiber_continue(&mut self, fiber: &Obj, v: Value, exc: Option<Obj>) -> VmResult<Value> {
        let (frames, dst) = with(fiber, |d| {
            d.state = FiberState::Running;
            (std::mem::take(&mut d.frames), d.resume_dst.take())
        })
        .unwrap_or_default();
        let base = self.frames.len();
        with(fiber, |d| d.base = base);
        self.frames.extend(frames);
        if let Some(t) = dst {
            self.set_tmp(t, v);
        }
        self.fibers.push(fiber.clone());
        let r = match exc {
            Some(e) => match self.handle_exception(e, base) {
                Ok(()) => self.execute(base),
                Err(t) => Err(t),
            },
            None => self.execute(base),
        };
        self.fiber_run(fiber, r)
    }
}

fn fiber_construct(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    object::set_internal(
        &o,
        FiberData {
            callable: a.at(0),
            state: FiberState::Init,
            frames: Vec::new(),
            resume_dst: None,
            base: 0,
            transfer: Value::Null,
            ret: Value::Null,
        },
    );
    Ok(Value::Null)
}

fn state_of(o: &Obj) -> FiberState {
    with(o, |d| d.state).unwrap_or(FiberState::Init)
}

fn fiber_start(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    if state_of(&o) != FiberState::Init {
        return Err(vm.fiber_error("Cannot start a fiber that has already been started"));
    }
    let cb = with(&o, |d| d.callable.clone()).unwrap_or_default();
    let args: Vec<Value> = a.values.clone();
    let named = std::mem::take(&mut a.named);
    let base = vm.frames.len();
    with(&o, |d| {
        d.state = FiberState::Running;
        d.base = base;
    });
    vm.fibers.push(o.clone());
    let r = vm.call_value_named(&cb, args, named);
    vm.fiber_run(&o, r)
}

fn fiber_resume(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    if state_of(&o) != FiberState::Suspended {
        return Err(vm.fiber_error("Cannot resume a fiber that is not suspended"));
    }
    let v = a.get(0).unwrap_or(Value::Null);
    vm.fiber_continue(&o, v, None)
}

fn fiber_throw(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    if state_of(&o) != FiberState::Suspended {
        return Err(vm.fiber_error("Cannot resume a fiber that is not suspended"));
    }
    let Value::Object(e) = a.at(0) else {
        return Ok(Value::Null);
    };
    vm.fiber_continue(&o, Value::Null, Some(e))
}

fn fiber_get_return(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    match state_of(&o) {
        FiberState::Returned => Ok(with(&o, |d| d.ret.clone()).unwrap_or_default()),
        FiberState::Init => Err(vm.fiber_error("Cannot get fiber return value: The fiber has not been started")),
        FiberState::Threw => Err(vm.fiber_error("Cannot get fiber return value: The fiber threw an exception")),
        _ => Err(vm.fiber_error("Cannot get fiber return value: The fiber has not returned")),
    }
}

fn fiber_is_started(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(state_of(&a.this_obj()) != FiberState::Init))
}
fn fiber_is_suspended(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(state_of(&a.this_obj()) == FiberState::Suspended))
}
fn fiber_is_running(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(state_of(&a.this_obj()) == FiberState::Running))
}
fn fiber_is_terminated(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(matches!(state_of(&a.this_obj()), FiberState::Returned | FiberState::Threw)))
}

fn fiber_suspend(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let v = a.get(0).unwrap_or(Value::Null);
    vm.fiber_suspend(v)
}

fn fiber_get_current(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(vm.fibers.last().cloned().map_or(Value::Null, Value::Object))
}

fn fiber_error_construct(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.make_throw("Error", "The \"FiberError\" class is reserved for internal use and cannot be manually instantiated"))
}

/// Registers `Fiber` and `FiberError`.
pub(crate) fn register(vm: &mut Vm) {
    ClassBuilder::new("Fiber")
        .flags(cflags::FINAL | cflags::NOT_SERIALIZABLE | cflags::NO_CLONE)
        .method("__construct(callable $callback)", flags::PUBLIC, fiber_construct)
        .method("start(mixed ...$args): mixed", flags::PUBLIC, fiber_start)
        .method("resume(mixed $value = null): mixed", flags::PUBLIC, fiber_resume)
        .method("throw(Throwable $exception): mixed", flags::PUBLIC, fiber_throw)
        .method("getReturn(): mixed", flags::PUBLIC, fiber_get_return)
        .method("isStarted(): bool", flags::PUBLIC, fiber_is_started)
        .method("isSuspended(): bool", flags::PUBLIC, fiber_is_suspended)
        .method("isRunning(): bool", flags::PUBLIC, fiber_is_running)
        .method("isTerminated(): bool", flags::PUBLIC, fiber_is_terminated)
        .method("suspend(mixed $value = null): mixed", flags::PUBLIC | flags::STATIC, fiber_suspend)
        .method("getCurrent(): ?Fiber", flags::PUBLIC | flags::STATIC, fiber_get_current)
        .register(vm);
    ClassBuilder::new("FiberError")
        .extends(vm, "Error")
        .flags(cflags::FINAL)
        .method("__construct()", flags::PUBLIC, fiber_error_construct)
        .register(vm);
}

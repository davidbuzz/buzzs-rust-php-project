//! Fibers (`Zend/zend_fibers.c`).
//!
//! Each fiber runs on its own native stack ([`stack::Coroutine`]), so a
//! fiber can suspend from anywhere: nested user calls, internal callbacks
//! (`array_map`), generators, destructors, `__get`, ...
//!
//! The VM frame stack (`vm.frames`) is shared. While a fiber runs, its
//! frames sit on top of the resumer's frames starting at a fixed index
//! `base` (indices held by the interpreter loops on the fiber's native stack
//! must stay valid across suspensions). When the resumer's stack is
//! shallower than `base`, invisible filler frames pad it; when it is deeper,
//! the frames just above `{main}` are set aside for the duration of the run
//! (they only matter for backtraces). On suspension the fiber's frames are
//! moved into the fiber object and the resumer's frames are restored.
//!
//! Destroying a suspended fiber (`zend_fiber_object_destroy`) resumes it
//! with an uncatchable "graceful exit" exception that runs `finally` blocks
//! and destructors while unwinding.

mod stack;

use crate::builtins::classbuilder::ClassBuilder;
use crate::class::{cflags, Class};
use crate::func::{flags, Args};
use crate::object;
use crate::vm::{Frame, FrameKind, Throw, Vm, VmResult};
use php_runtime::{Obj, Value, ZStr};
use stack::{Coroutine, StackError, Suspender};
use std::cell::RefCell;
use std::rc::Rc;

/// Fiber status (`ZEND_FIBER_STATUS_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FiberState {
    /// Constructed, not started.
    Init,
    /// Running, or resuming another fiber (`caller != NULL`).
    Running,
    /// Suspended by `Fiber::suspend()`.
    Suspended,
    /// Returned, threw or bailed out.
    Dead,
}

/// Internal state of a `Fiber`.
#[derive(Debug)]
pub struct FiberData {
    /// The callable.
    pub callable: Value,
    /// Status.
    pub state: FiberState,
    /// `ZEND_FIBER_FLAG_THREW`.
    pub threw: bool,
    /// `ZEND_FIBER_FLAG_BAILOUT`.
    pub bailout: bool,
    /// `ZEND_FIBER_FLAG_DESTROYED`.
    pub destroyed: bool,
    /// The native stack (from `start()` until the fiber terminates).
    co: Option<Rc<Coroutine>>,
    /// Switches back to the resumer (valid while `co` lives).
    suspender: Option<Suspender>,
    /// The fiber's frames while it is suspended.
    pub frames: Vec<Frame>,
    /// Index of the fiber's first frame in `vm.frames`.
    pub base: usize,
    /// `vm.depth` of the fiber's native stack while suspended.
    depth: u32,
    /// `EG(error_reporting)` of the fiber while suspended (part of the VM
    /// state swapped by `zend_fiber_switch_context`).
    error_reporting: i64,
    /// Value (or error) passed by the last switch in either direction.
    transfer: Option<VmResult<Value>>,
    /// Return value.
    pub ret: Value,
}

impl FiberData {
    fn new(callable: Value) -> Self {
        FiberData {
            callable,
            state: FiberState::Init,
            threw: false,
            bailout: false,
            destroyed: false,
            co: None,
            suspender: None,
            frames: Vec::new(),
            base: 0,
            depth: 0,
            error_reporting: 0,
            transfer: None,
            ret: Value::Null,
        }
    }
}

fn with<R>(o: &Obj, f: impl FnOnce(&mut FiberData) -> R) -> Option<R> {
    object::internal::<FiberData, R>(o, f)
}

/// Extra room left below a fiber's frames so that it can later be resumed
/// from a deeper call stack without hiding frames from backtraces.
const SLACK: usize = 32;

/// `ZEND_FIBER_DEFAULT_C_STACK_SIZE`.
const DEFAULT_STACK: usize = 4096 * 512;

/// Our interpreter needs more native stack per PHP-level nesting than
/// php-src does; scale `fiber.stack_size` by this factor.
const STACK_SCALE: usize = 4;

/// Conservative native stack use of one nested interpreter loop, used to
/// turn the stack size into a nesting budget (`vm.depth`) so that deep
/// recursion fails with an error instead of hitting the guard page.
const BYTES_PER_LEVEL: usize = 64 * 1024;

/// Default `error_reporting` (`E_ALL`).
const E_ALL: i64 = 30719;

/// Nesting limit enforced by `Vm::execute`.
const MAX_DEPTH: u32 = 20000;

thread_local! {
    static GRACEFUL_EXIT: RefCell<Option<Rc<Class>>> = const { RefCell::new(None) };
}

/// Whether `o` is the internal "graceful exit" unwinding object
/// (`zend_is_graceful_exit`), which no `catch` block matches.
pub(crate) fn is_graceful_exit(o: &Obj) -> bool {
    GRACEFUL_EXIT.with(|g| g.borrow().as_ref().is_some_and(|c| Rc::ptr_eq(c, object::class_of(o))))
}

fn create_graceful_exit() -> Option<Obj> {
    let cls = GRACEFUL_EXIT.with(|g| g.borrow().clone())?;
    Some(object::new_object(cls, Vec::new()))
}

/// How the resumer's frames were adjusted to put the fiber's frames at
/// their base index.
enum Prefix {
    /// Nothing to do.
    Exact,
    /// `n` filler frames were inserted at `pos`.
    Fillers { pos: usize, n: usize },
    /// These frames were removed from index 1.
    Stash(Vec<Frame>),
}

impl Vm {
    fn fiber_error(&mut self, msg: &str) -> Throw {
        self.make_throw("FiberError", msg)
    }

    /// A frame that backtraces skip but whose location is that of `model`.
    fn filler_frame(&self, model: &Frame) -> Frame {
        let kind = if model.kind == FrameKind::Builtin {
            FrameKind::Builtin
        } else {
            FrameKind::ConstExpr
        };
        let mut f = Frame::with_vars(self.empty_code.clone(), kind, self.empty_vars.clone(), None);
        f.code = model.code.clone();
        f.ip = model.ip;
        f
    }

    /// Makes `self.frames.len() == base`, keeping the topmost frame (the
    /// `Fiber->start()`/`resume()` call) on top.
    fn fiber_install_prefix(&mut self, base: usize) -> Prefix {
        let d = self.frames.len();
        if d == 0 || base < 2 {
            // Nothing to keep in place: pad (or leave as is).
            if d < base {
                let model = self.frames.last().map(|m| self.filler_frame(m));
                let n = base - d;
                for _ in 0..n {
                    let f = match &model {
                        Some(m) => self.filler_frame(m),
                        None => Frame::with_vars(self.empty_code.clone(), FrameKind::ConstExpr, self.empty_vars.clone(), None),
                    };
                    self.frames.insert(d.saturating_sub(1), f);
                }
                return Prefix::Fillers { pos: d.saturating_sub(1), n };
            }
            return Prefix::Exact;
        }
        match d.cmp(&base) {
            std::cmp::Ordering::Equal => Prefix::Exact,
            std::cmp::Ordering::Less => {
                let n = base - d;
                let pos = d - 1;
                let model_idx = d.saturating_sub(2);
                let fillers: Vec<Frame> = (0..n).map(|_| self.filler_frame(&self.frames[model_idx])).collect();
                self.frames.splice(pos..pos, fillers);
                Prefix::Fillers { pos, n }
            }
            std::cmp::Ordering::Greater => {
                let n = d - base;
                Prefix::Stash(self.frames.drain(1..=n).collect())
            }
        }
    }

    fn fiber_restore_prefix(&mut self, p: Prefix) {
        match p {
            Prefix::Exact => {}
            Prefix::Fillers { pos, n } => {
                let end = (pos + n).min(self.frames.len());
                self.frames.drain(pos.min(end)..end);
            }
            Prefix::Stash(v) => {
                let at = 1.min(self.frames.len());
                self.frames.splice(at..at, v);
            }
        }
    }

    /// `zend_fiber_resume_internal`: switches into `fiber`, passing `input`
    /// (the value `Fiber::suspend()` returns, or the error it throws), and
    /// returns what the fiber passes back when it suspends or terminates.
    fn fiber_switch_in(&mut self, fiber: &Obj, input: VmResult<Value>) -> VmResult<Value> {
        let Some((co, base, frames, depth, er)) = with(fiber, |d| {
            d.state = FiberState::Running;
            d.transfer = Some(input);
            (d.co.clone(), d.base, std::mem::take(&mut d.frames), d.depth, d.error_reporting)
        }) else {
            return Ok(Value::Null);
        };
        let Some(co) = co else {
            return Ok(Value::Null);
        };
        let prefix = self.fiber_install_prefix(base);
        self.frames.extend(frames);
        let saved_depth = std::mem::replace(&mut self.depth, depth);
        let saved_er = std::mem::replace(&mut self.error_reporting, er);
        self.fibers.push(fiber.clone());
        let finished = co.resume();
        self.fibers.pop();
        let fiber_depth = std::mem::replace(&mut self.depth, saved_depth);
        let fiber_er = std::mem::replace(&mut self.error_reporting, saved_er);
        let frames = if self.frames.len() > base {
            self.frames.split_off(base)
        } else {
            Vec::new()
        };
        self.fiber_restore_prefix(prefix);
        let out = with(fiber, |d| {
            if finished {
                d.state = FiberState::Dead;
                d.co = None;
                d.suspender = None;
            } else {
                d.frames = frames;
                d.depth = fiber_depth;
                d.error_reporting = fiber_er;
            }
            d.transfer.take()
        })
        .flatten();
        drop(co);
        out.unwrap_or(Ok(Value::Null))
    }

    /// Creates the native stack of a fiber about to start.
    fn fiber_create_context(&mut self, fiber: &Obj, args: Vec<Value>, named: Vec<(ZStr, Value)>) -> VmResult<()> {
        let requested = match self.ini.typed("fiber.stack_size") {
            php_support::ini::IniTyped::Long(n) => usize::try_from(n).unwrap_or(0),
            _ => DEFAULT_STACK,
        };
        if requested < stack::MIN_STACK {
            return Err(self.make_throw(
                "Exception",
                &format!("Fiber stack size is too small, it needs to be at least {} bytes", stack::MIN_STACK),
            ));
        }
        let size = requested.saturating_mul(STACK_SCALE).max(1 << 20);
        let cb = with(fiber, |d| d.callable.clone()).unwrap_or_default();
        let vm_ptr: *mut Vm = self;
        let body = Box::new(move |s: &Suspender| {
            // SAFETY: the body only runs inside `Coroutine::resume`, called
            // by `fiber_switch_in` while it holds `&mut Vm` and does not use
            // it until `resume` returns: control is handed back and forth,
            // never shared. The VM does not move while scripts run.
            let vm = unsafe { &mut *vm_ptr };
            fiber_body(vm, *s, &cb, args, named);
        });
        match Coroutine::new(size, body) {
            Ok(co) => {
                let budget = u32::try_from(size / BYTES_PER_LEVEL).unwrap_or(MAX_DEPTH).min(MAX_DEPTH);
                let base = self.frames.len() + SLACK;
                // Fibers start with the INI error_reporting, not a value
                // lowered by a `@` around `start()`.
                let er = match self.ini.get("error_reporting") {
                    Some(v) if !v.is_empty() => std::str::from_utf8(v)
                        .ok()
                        .and_then(|s| s.trim().parse::<i64>().ok())
                        .unwrap_or(0),
                    _ => E_ALL,
                };
                with(fiber, |d| {
                    d.co = Some(Rc::new(co));
                    d.base = base;
                    d.depth = MAX_DEPTH - budget;
                    d.error_reporting = er;
                });
                Ok(())
            }
            Err(StackError::TooSmall) => Err(self.make_throw(
                "Exception",
                &format!("Fiber stack size is too small, it needs to be at least {} bytes", stack::MIN_STACK),
            )),
            Err(StackError::Mmap(m)) => {
                Err(self.make_throw("Exception", &format!("Fiber stack allocate failed: mmap failed: {m}")))
            }
            Err(StackError::Unsupported) => {
                Err(self.fiber_error("Fibers are not supported on this platform"))
            }
        }
    }
}

/// `zend_fiber_execute`: runs on the fiber's own stack.
fn fiber_body(vm: &mut Vm, s: Suspender, cb: &Value, args: Vec<Value>, named: Vec<(ZStr, Value)>) {
    // No strong reference to the fiber may live on its own stack, or an
    // unreferenced suspended fiber would never be destroyed.
    let Some(()) = vm.fibers.last().and_then(|f| with(f, |d| d.suspender = Some(s))) else {
        return;
    };
    let r = vm.call_value_named(cb, args, named);
    let Some(fiber) = vm.fibers.last().cloned() else {
        return;
    };
    let destroyed = with(&fiber, |d| d.destroyed).unwrap_or(false);
    let out = match r {
        Ok(v) => {
            with(&fiber, |d| d.ret = v.deref());
            Ok(Value::Null)
        }
        Err(Throw::Exception(e)) if destroyed && is_graceful_exit(&e) => Ok(Value::Null),
        Err(Throw::Exception(e)) => {
            with(&fiber, |d| d.threw = true);
            Err(Throw::Exception(e))
        }
        Err(Throw::Fatal) => {
            with(&fiber, |d| d.bailout = true);
            Err(Throw::Fatal)
        }
        Err(e) => Err(e),
    };
    with(&fiber, |d| {
        d.transfer = Some(out);
        d.state = FiberState::Dead;
    });
}

fn state_of(o: &Obj) -> FiberState {
    with(o, |d| d.state).unwrap_or(FiberState::Init)
}

fn fiber_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    if object::has_internal::<FiberData>(&o) {
        return Err(vm.fiber_error("Cannot call constructor twice"));
    }
    object::set_internal(&o, FiberData::new(a.at(0)));
    Ok(Value::Null)
}

fn fiber_start(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    if state_of(&o) != FiberState::Init {
        return Err(vm.fiber_error("Cannot start a fiber that has already been started"));
    }
    let args: Vec<Value> = a.values.clone();
    let named = std::mem::take(&mut a.named);
    vm.fiber_create_context(&o, args, named)?;
    vm.fiber_switch_in(&o, Ok(Value::Null))
}

fn fiber_resume(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    if state_of(&o) != FiberState::Suspended {
        return Err(vm.fiber_error("Cannot resume a fiber that is not suspended"));
    }
    let v = a.get(0).unwrap_or(Value::Null);
    vm.fiber_switch_in(&o, Ok(v))
}

fn fiber_throw(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let Value::Object(e) = a.at(0) else {
        return Ok(Value::Null);
    };
    if state_of(&o) != FiberState::Suspended {
        return Err(vm.fiber_error("Cannot resume a fiber that is not suspended"));
    }
    vm.fiber_switch_in(&o, Err(Throw::Exception(e)))
}

fn fiber_get_return(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let Some((state, threw, bailout, ret)) = with(&o, |d| (d.state, d.threw, d.bailout, d.ret.clone())) else {
        return Err(vm.fiber_error("Cannot get fiber return value: The fiber has not been started"));
    };
    let msg = match state {
        FiberState::Dead if threw => "The fiber threw an exception",
        FiberState::Dead if bailout => "The fiber exited with a fatal error",
        FiberState::Dead => return Ok(ret),
        FiberState::Init => "The fiber has not been started",
        _ => "The fiber has not returned",
    };
    Err(vm.fiber_error(&format!("Cannot get fiber return value: {msg}")))
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
    Ok(Value::Bool(state_of(&a.this_obj()) == FiberState::Dead))
}

fn fiber_suspend(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let v = a.get(0).unwrap_or(Value::Null);
    let Some(fiber) = vm.fibers.last().cloned() else {
        return Err(vm.fiber_error("Cannot suspend outside of a fiber"));
    };
    if with(&fiber, |d| d.destroyed).unwrap_or(false) {
        return Err(vm.fiber_error("Cannot suspend in a force-closed fiber"));
    }
    let Some(s) = with(&fiber, |d| {
        d.transfer = Some(Ok(v));
        d.state = FiberState::Suspended;
        d.suspender
    })
    .flatten() else {
        return Err(vm.fiber_error("Cannot switch fibers in current execution context"));
    };
    // Do not keep the fiber alive from its own stack while suspended.
    drop(fiber);
    s.suspend();
    vm.fibers
        .last()
        .and_then(|f| with(f, |d| d.transfer.take()).flatten())
        .unwrap_or(Ok(Value::Null))
}

fn fiber_get_current(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(vm.fibers.last().cloned().map_or(Value::Null, Value::Object))
}

/// `zend_fiber_object_destroy` (the `dtor_obj` handler): unwinds a
/// suspended fiber.
fn fiber_destroy(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    if state_of(&o) != FiberState::Suspended {
        return Ok(Value::Null);
    }
    let Some(g) = create_graceful_exit() else {
        return Ok(Value::Null);
    };
    with(&o, |d| d.destroyed = true);
    vm.fiber_switch_in(&o, Err(Throw::Exception(g))).map(|_| Value::Null)
}

fn fiber_error_construct(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.make_throw(
        "Error",
        "The \"FiberError\" class is reserved for internal use and cannot be manually instantiated",
    ))
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
        // The `dtor_obj` handler. php-src has no such method; ClassBuilder
        // only wires destructors through `__destruct`.
        .method("__destruct(): void", flags::PUBLIC, fiber_destroy)
        .register(vm);
    ClassBuilder::new("FiberError")
        .extends(vm, "Error")
        .flags(cflags::FINAL)
        .method("__construct()", flags::PUBLIC, fiber_error_construct)
        .register(vm);
    // The graceful-exit unwinder: a hidden class outside the Throwable
    // hierarchy, so no `catch` matches it.
    let cls = ClassBuilder::new("FiberGracefulExit").flags(cflags::FINAL).register(vm);
    vm.classes.remove(&cls.lc_name);
    vm.class_order.retain(|n| *n != cls.lc_name);
    GRACEFUL_EXIT.with(|g| *g.borrow_mut() = Some(cls));
}

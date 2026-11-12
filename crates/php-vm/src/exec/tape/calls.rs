//! Tape instructions for calls of user functions: php-src's call protocol.
//!
//! php-src prepares a call's frame when the call starts: `ZEND_INIT_FCALL`
//! reserves the callee's `zend_execute_data` on the VM stack, each
//! `ZEND_SEND_VAL` / `ZEND_SEND_VAR` writes its argument straight into the
//! callee's compiled-variable slot, and `ZEND_DO_UCALL` only links the frame
//! in and switches to it (ref: `Zend/zend_vm_def.h` `ZEND_INIT_FCALL`,
//! `ZEND_SEND_VAL`, `ZEND_DO_UCALL`; `Zend/zend_execute.h`
//! `zend_vm_stack_push_call_frame`).
//!
//! The generic protocol here builds a [`PendingCall`] record at
//! `InitFCall`, stages the arguments in it at `Send`, and at `DoCall`
//! re-initialises a pooled frame and moves the arguments across
//! (`Vm::push_simple_ucall`). The tape does what php-src does for the
//! calls that path accepts: a statically named user function, found in
//! the call site's run-time cache, whose declaration and arguments make a
//! plain call (`UserFunction::call_shape`). [`prepare`] readies the pooled
//! frame above the current one for the function at `InitFCall`, [`send`]
//! writes each argument into that frame's compiled variable, and [`enter`]
//! makes the frame current at `DoCall`.
//!
//! At most one call is prepared at a time, and only by the tape loop
//! (`run_tape`), which holds it in a local ([`Prepared`]). Whenever
//! anything else could see or change the caller's calls (another call
//! starting, an op the tape hands back to the generic code, the tape loop
//! returning to `Vm::run`, a safe point), the loop first turns the
//! prepared call into the [`PendingCall`] the generic protocol would have
//! built ([`materialize`]) and returns the frame to the pool, so the rest
//! of the VM never sees a half-prepared frame.

use super::{Inst, TOp};
use crate::call::{accepts_without_coercion, clone_value};
use crate::func::{CallShape, Func, UserFunction};
use crate::opcode::{ArgSrc, ClassSpec, Op, OpArray, Operand, PlaceBase};
use crate::vm::{FrameKind, PendingCall, RetTarget, Vm};
use php_runtime::Value;
use std::rc::Rc;

/// The most arguments a prepared call takes: the pending call's inline
/// buffer (what [`materialize`] moves them into) and the precomputed
/// first ops of [`UserFunction::plain_first_op`].
const MAX_ARGS: usize = 6;

/// The specialised instruction of the op at `i` when this family has one.
pub(super) fn lower(code: &OpArray, i: usize) -> Option<Inst> {
    let plain = Inst::plain;
    match code.ops.get(i)? {
        op if preparable(op) => Some(plain(TOp::CallInit)),
        // Only an argument of a call one of those started can go to a
        // prepared call; the others keep the generic `Send`, without the
        // detour.
        Op::Send { src }
            if direct_src(code, src) && call_start(code, i).is_some_and(preparable) =>
        {
            Some(plain(TOp::CallSend))
        }
        _ => None,
    }
}

/// Whether a call started by `op` may be prepared ([`prepare`],
/// [`prepare_method`]).
fn preparable(op: &Op) -> bool {
    matches!(
        op,
        Op::InitFCall { fallback: None, .. }
            | Op::InitMethodCall {
                name: Operand::Lit(_),
                ..
            }
            | Op::InitStaticCall {
                class: ClassSpec::Named(_),
                name: Operand::Lit(_),
            }
            | Op::InitDynCall {
                callee: Operand::Cv(_),
            }
            | Op::New {
                class: ClassSpec::Named(_),
                ..
            }
    )
}

/// The op that started the call the `Send` at `i` passes an argument to:
/// the nearest call-starting op before it whose call is not finished
/// before `i` (calls nest: an argument's own calls start and end in
/// between). `None` when there is none (not expected).
fn call_start(code: &OpArray, i: usize) -> Option<&Op> {
    let mut finished = 0usize;
    for op in code.ops.get(..i)?.iter().rev() {
        match op {
            Op::DoCall { .. } | Op::CallableConvert { .. } | Op::CallablePartial { .. } => {
                finished += 1;
            }
            Op::InitFCall { .. }
            | Op::InitDynCall { .. }
            | Op::InitMethodCall { .. }
            | Op::InitStaticCall { .. }
            | Op::InitParentHook { .. }
            | Op::New { .. } => {
                if finished == 0 {
                    return Some(op);
                }
                finished -= 1;
            }
            _ => {}
        }
    }
    None
}

/// Whether [`send`] can pass `src` (by value: a literal, a temporary, or a
/// compiled variable).
fn direct_src(code: &OpArray, src: &ArgSrc) -> bool {
    match src {
        ArgSrc::Val(Operand::Lit(_) | Operand::Tmp(_) | Operand::Cv(_))
        | ArgSrc::CallResult(Operand::Tmp(_)) => true,
        ArgSrc::Place(pid) => place_cv(code, *pid).is_some(),
        _ => false,
    }
}

/// The compiled variable a place stands for when it is one without
/// segments (`f($x)` compiles the argument as a place: by reference when
/// the parameter is).
fn place_cv(code: &OpArray, pid: u32) -> Option<u32> {
    let p = code.places.get(pid as usize)?;
    match (&p.base, p.segs.is_empty()) {
        (PlaceBase::Cv(cv), true) => Some(*cv),
        _ => None,
    }
}

/// A call prepared by [`prepare`]: the function, how many arguments
/// [`send`] wrote into the frame, and how many pending calls the caller
/// had when it started (the place of this call among them).
pub(super) struct Prepared {
    uf: Rc<UserFunction>,
    n: usize,
    calls_len: usize,
    /// Started by `InitFCall` (the pending call's `direct`); otherwise a
    /// method, static method or closure call, whose `$this`, scopes and
    /// closure are in the prepared frame.
    fcall: bool,
    /// A closure call (`$f()`, the pending call's `dynamic`): its bound
    /// variables are copied into the frame at `DoCall`.
    closure: bool,
    /// The constructor call of `new` (the pending call's `is_ctor`).
    ctor: bool,
}

/// `ZEND_INIT_FCALL` of `Op::InitFCall { name, fallback: None, .. }` in the
/// current frame: when the call site's cache holds a user function whose
/// declaration makes a plain call (as `Vm::push_simple_ucall` requires),
/// the pooled frame above the current one is readied for it and the call
/// is prepared. `None`, having changed nothing, otherwise: the generic
/// `InitFCall` runs.
pub(super) fn prepare(vm: &mut Vm, name: u32) -> Option<Prepared> {
    if vm.observer.enabled {
        return None;
    }
    let live = vm.frames.live;
    // `push_user_call`'s stack-depth backstop is the generic path's.
    if live == 0 || live > 50_000 {
        return None;
    }
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        // No pooled frame at this depth yet: the generic call pushes one.
        return None;
    };
    let func = caller
        .code
        .fcall_cache
        .borrow()
        .get(name as usize)
        .and_then(|c| c.as_ref().and_then(crate::opcode::CachedFunc::get))?;
    let Func::User(uf) = func else {
        return None;
    };
    if uf.call_shape() == CallShape::NOT_PLAIN {
        return None;
    }
    ready_frame(frame, &uf);
    Some(Prepared {
        uf,
        n: 0,
        calls_len: caller.calls.len(),
        fcall: true,
        closure: false,
        ctor: false,
    })
}

/// Readies the pooled frame `frame` for a call of `uf`, as
/// `Vm::push_simple_ucall` readies it at `DoCall` (`FrameStack::push_in_place`).
fn ready_frame(frame: &mut crate::vm::Frame, uf: &Rc<UserFunction>) {
    debug_assert!(
        frame.this.is_none()
            && frame.scope.is_none()
            && frame.called.is_none()
            && frame.closure.is_none()
            && frame.calls.is_empty()
    );
    let code = &uf.code;
    if frame.cvs.len() == code.cv_names.len()
        && frame.tmps.len() == code.num_tmps as usize
        && frame.iters.is_empty()
        && frame.fins.is_empty()
    {
        frame.reinit_same_call(code, FrameKind::Function);
    } else {
        frame.reinit_for_call(Rc::clone(code), FrameKind::Function);
    }
    if !frame.ufunc.as_ref().is_some_and(|f| Rc::ptr_eq(f, uf)) {
        // The frame keeps the function across calls in this slot
        // (`Frame::clear_for_reuse`): only another function replaces it.
        if let Some(old) = frame.ufunc.replace(Rc::clone(uf)) {
            drop(old);
        }
        if let Some(old) = frame.func.replace(Func::User(Rc::clone(uf))) {
            drop(old);
        }
    }
}

/// `ZEND_INIT_METHOD_CALL` of `Op::InitMethodCall { obj, name: Lit(lit) }`
/// at `slot` in the current frame: on the method cache hit
/// `Vm::init_method_call_fast` takes (the object's class and the calling
/// scope as cached), when the method is a user function whose declaration
/// makes a plain call, the pooled frame above the current one is readied
/// for it with its `$this` (moved out of a temporary, else a copy; none
/// for a static method) and scopes, and the call is prepared. `None`,
/// having changed nothing, otherwise.
pub(super) fn prepare_method(vm: &mut Vm, obj: Operand, lit: u32, slot: usize) -> Option<Prepared> {
    if vm.observer.enabled {
        return None;
    }
    let live = vm.frames.live;
    if live == 0 || live > 50_000 {
        return None;
    }
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return None;
    };
    let o = match obj {
        Operand::Cv(cv) if !caller.symtab_attached => {
            match caller.cvs.get(cv as usize).and_then(crate::vm::CvSlot::get) {
                Some(Value::Object(o)) => o,
                _ => return None,
            }
        }
        Operand::Tmp(t) => match caller.tmps.get(t as usize) {
            Some(Value::Object(o)) => o,
            _ => return None,
        },
        Operand::Unused => caller.this.as_ref()?,
        _ => return None,
    };
    let cls = crate::object::class_of(o);
    if cls.hooks.as_ref().and_then(|h| h.get_method).is_some() {
        return None;
    }
    let cls_addr = Rc::as_ptr(cls) as usize;
    let scope_addr = caller.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
    let (uf, scope, is_static) = {
        let cache = caller.code.method_cache.borrow();
        let cached = cache.get(slot)?.as_ref()?;
        if cached.class_addr != cls_addr
            || cached.scope_addr != scope_addr
            || cached.name_lit != Some(lit)
        {
            return None;
        }
        let Some(Func::User(uf)) = cached.func.get() else {
            return None;
        };
        (
            uf,
            cached
                .method_scope
                .as_ref()
                .and_then(std::rc::Weak::upgrade),
            cached.is_static,
        )
    };
    if uf.call_shape() == CallShape::NOT_PLAIN {
        return None;
    }
    let called = Rc::clone(cls);
    // The call's `$this`, as `Vm::init_method_call_fast` takes it: the
    // temporary's handle moved out, or a copy of the variable's object;
    // a static method takes none (the temporary's handle is released
    // after the frame is readied).
    let (this, released) = match obj {
        Operand::Tmp(t) => match caller.tmps.get_mut(t as usize).map(std::mem::take) {
            Some(Value::Object(held)) if !is_static => (Some(held), None),
            other => (None, other),
        },
        _ if is_static => (None, None),
        _ => (Some(o.clone()), None),
    };
    ready_frame(frame, &uf);
    frame.this = this;
    frame.scope = scope;
    frame.called = Some(called);
    let calls_len = caller.calls.len();
    drop(released);
    Some(Prepared {
        uf,
        n: 0,
        calls_len,
        fcall: false,
        closure: false,
        ctor: false,
    })
}

/// `ZEND_INIT_STATIC_METHOD_CALL` of `Op::InitStaticCall { class:
/// Named(_), name: Lit(_) }` at `at` in the current frame: on the call
/// site's cache hit (`Vm::op_init_static_call`'s first case: the site's
/// slot past the literals, indexed by the op after it), when the method
/// is a user function whose declaration makes a plain call, the pooled
/// frame above the current one is readied for it with its scopes (no
/// `$this`) and the call is prepared. `None`, having changed nothing,
/// otherwise.
pub(super) fn prepare_static(vm: &mut Vm, at: usize) -> Option<Prepared> {
    if vm.observer.enabled {
        return None;
    }
    let live = vm.frames.live;
    if live == 0 || live > 50_000 {
        return None;
    }
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return None;
    };
    let slot = caller.code.literals.len() + at + 1;
    let (func, scope, called) = caller
        .code
        .fcall_cache
        .borrow()
        .get(slot)
        .and_then(|c| c.as_ref().and_then(crate::opcode::CachedFunc::get_call))?;
    let Func::User(uf) = func else {
        return None;
    };
    if uf.call_shape() == CallShape::NOT_PLAIN {
        return None;
    }
    ready_frame(frame, &uf);
    frame.scope = scope;
    frame.called = called;
    Some(Prepared {
        uf,
        n: 0,
        calls_len: caller.calls.len(),
        fcall: false,
        closure: false,
        ctor: false,
    })
}

/// `ZEND_INIT_DYNAMIC_CALL` of `Op::InitDynCall { callee: Cv(cv) }` in the
/// current frame: when the variable holds a closure of a user function
/// whose declaration makes a plain call (`Vm::init_closure_call_fast`,
/// without a `__call` trampoline), the pooled frame above the current one
/// is readied for it with the closure's `$this`, scopes and the closure
/// itself, and the call is prepared. `None`, having changed nothing,
/// otherwise.
pub(super) fn prepare_closure(vm: &mut Vm, cv: u32) -> Option<Prepared> {
    if vm.observer.enabled {
        return None;
    }
    let live = vm.frames.live;
    if live == 0 || live > 50_000 {
        return None;
    }
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return None;
    };
    if caller.symtab_attached {
        return None;
    }
    let Some(Value::Object(o)) = caller.cvs.get(cv as usize).and_then(crate::vm::CvSlot::get)
    else {
        return None;
    };
    let data = crate::object::data(o);
    if data.class.lc_name.as_bytes() != b"closure" {
        return None;
    }
    let internal = data.internal.try_borrow().ok()?;
    let d = internal
        .as_ref()
        .and_then(|x| x.downcast_ref::<crate::closure::ClosureData>())?;
    let Func::User(uf) = &d.func else {
        return None;
    };
    if d.magic_name.is_some() || uf.call_shape() == CallShape::NOT_PLAIN {
        return None;
    }
    let uf = Rc::clone(uf);
    ready_frame(frame, &uf);
    frame.this = d.this.clone();
    frame.scope = d.scope.clone();
    frame.called = d.called.clone();
    frame.closure = if d.fake { None } else { Some(o.clone()) };
    drop(internal);
    Some(Prepared {
        uf,
        n: 0,
        calls_len: caller.calls.len(),
        fcall: false,
        closure: true,
        ctor: false,
    })
}

/// What [`new_object`] did.
pub(super) enum NewDone {
    /// The object of a class without a constructor is in the result:
    /// continue at the op after the constructor call (`skip`).
    Skip(usize),
    /// The object is in the result and its constructor call is prepared.
    Prepared(Prepared),
    /// The object is in the result and its constructor call is the
    /// generic pending call (`props::new_with_ctor`).
    Pending,
}

/// `ZEND_NEW` of `Op::New { class: Named(lit), dst, skip }` in the current
/// frame, on the class cache hit the props fast path takes: a class
/// without a constructor gets its object (`props::new_plain`, then the
/// jump over the constructor call); a class whose constructor is a user
/// function that makes a plain call gets its object and the constructor's
/// frame prepared on it (`ZEND_NEW` pushing the call frame with the object
/// as `$this`); any other constructor, the generic pending call. `None`,
/// having changed nothing, when none of that applies (the generic `New`).
pub(super) fn new_object(vm: &mut Vm, lit: u32, dst: u32, skip: u32) -> Option<NewDone> {
    let live = vm.frames.live;
    if live == 0 {
        return None;
    }
    let observed = vm.observer.enabled;
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let caller = callers.last_mut()?;
    if dst as usize >= caller.tmps.len() {
        return None;
    }
    let cls = caller
        .code
        .class_cache
        .try_borrow()
        .ok()?
        .get(lit as usize)
        .and_then(std::rc::Weak::upgrade)?;
    if cls.magic.construct.is_none() {
        // Without a `skip` target (arguments being evaluated), the generic
        // op starts the placeholder call that evaluates them.
        if skip == 0 {
            return None;
        }
        let o = crate::props::new_plain(&cls)?;
        crate::exec::spec::put_tmp(&mut caller.tmps[dst as usize], Value::Object(o));
        return Some(NewDone::Skip(skip as usize));
    }
    let direct = match (crate::props::ctor_call(&cls), pool.first_mut()) {
        (Some((Func::User(uf), scope)), Some(frame))
            if !observed && live <= 50_000 && uf.call_shape() != CallShape::NOT_PLAIN =>
        {
            Some((uf, scope, frame))
        }
        _ => None,
    };
    let Some((uf, scope, frame)) = direct else {
        let o = crate::props::new_with_ctor(cls, &mut caller.calls)?;
        crate::exec::spec::put_tmp(&mut caller.tmps[dst as usize], Value::Object(o));
        return Some(NewDone::Pending);
    };
    let o = crate::props::new_for_ctor(&cls)?;
    ready_frame(frame, &uf);
    frame.this = Some(o.clone());
    frame.scope = Some(scope);
    frame.called = Some(cls);
    crate::exec::spec::put_tmp(&mut caller.tmps[dst as usize], Value::Object(o));
    Some(NewDone::Prepared(Prepared {
        uf,
        n: 0,
        calls_len: caller.calls.len(),
        fcall: false,
        closure: false,
        ctor: true,
    }))
}

/// `ZEND_SEND_VAL` / `ZEND_SEND_VAR` of `src` for the prepared call: the
/// argument is copied (a literal, a variable: dereferenced) or moved (a
/// temporary) straight into the callee's compiled variable. `false`,
/// having changed nothing, when the send is not the prepared call's (a
/// later pending call, started by `new`, is on top), the argument has no
/// declared parameter or is more than [`MAX_ARGS`], or the variable is
/// undefined (the generic send warns): the caller then materializes the
/// call and the generic `Send` runs.
pub(super) fn send(vm: &mut Vm, prep: &mut Prepared, src: &ArgSrc) -> bool {
    let n = prep.n;
    if n >= MAX_ARGS || n >= prep.uf.params.len() {
        return false;
    }
    let live = vm.frames.live;
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return false;
    };
    if caller.calls.len() != prep.calls_len {
        return false;
    }
    let v = match src {
        ArgSrc::Val(Operand::Lit(i)) => match caller.code.literals.get(*i as usize) {
            Some(v) => clone_value(v),
            None => return false,
        },
        ArgSrc::Val(Operand::Tmp(t)) | ArgSrc::CallResult(Operand::Tmp(t)) => {
            match caller.tmps.get_mut(*t as usize) {
                Some(slot) => match std::mem::take(slot) {
                    Value::Reference(r) => r.get(),
                    v => v,
                },
                None => return false,
            }
        }
        ArgSrc::Val(Operand::Cv(cv)) => match cv_value(caller, *cv) {
            Some(v) => v,
            None => return false,
        },
        ArgSrc::Place(pid) => {
            match place_cv(&caller.code, *pid).and_then(|cv| cv_value(caller, cv)) {
                Some(v) => v,
                None => return false,
            }
        }
        _ => return false,
    };
    let Some(slot) = frame.cvs.get_mut(n) else {
        return false;
    };
    // A prepared frame's compiled variables are empty.
    if let Some(old) = slot.as_mut().replace(v) {
        drop(old);
    }
    prep.n = n + 1;
    true
}

/// The value of compiled variable `cv` of `frame` as an argument: a copy,
/// dereferenced; `None` when it is undefined.
fn cv_value(frame: &crate::vm::Frame, cv: u32) -> Option<Value> {
    match frame
        .cvs
        .get(cv as usize)
        .and_then(crate::vm::CvSlot::get)?
    {
        Value::Reference(r) => Some(r.get()),
        v => Some(clone_value(v)),
    }
}

/// `ZEND_DO_UCALL` of the prepared call (result to `dst`; the caller
/// resumes at `caller_ip`): the frame becomes the current one, as
/// `Vm::push_simple_ucall` leaves it. `Err` gives the call back, having
/// changed nothing, when the arguments do not make a plain call (too few,
/// or one that a typed parameter does not accept as it is) or the call is
/// not the caller's newest: the caller materializes it and the generic
/// `DoCall` runs.
pub(super) fn enter(
    vm: &mut Vm,
    prep: Prepared,
    dst: Option<u32>,
    caller_ip: usize,
) -> Result<(), Prepared> {
    let live = vm.frames.live;
    let n = prep.n;
    let uf = &prep.uf;
    if n < uf.required as usize || n > uf.params.len() {
        return Err(prep);
    }
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return Err(prep);
    };
    if caller.calls.len() != prep.calls_len {
        return Err(prep);
    }
    if uf.call_shape() == CallShape::TYPED {
        for (p, slot) in uf.params.iter().zip(&frame.cvs[..n]) {
            if let Some(ty) = &p.ty {
                if !ty.is_mixed()
                    && !slot
                        .get()
                        .is_some_and(|v| accepts_without_coercion(ty.mask, v))
                {
                    return Err(prep);
                }
            }
        }
    }
    frame.ret = dst.map_or(RetTarget::Discard, RetTarget::Tmp);
    frame.caller_strict = caller.code.strict_types;
    frame.called_from_internal = false;
    frame.is_ctor = prep.ctor;
    // The passed-argument count (`ZEND_CALL_NUM_ARGS`): one empty entry
    // per argument, each already in its compiled variable.
    if !frame.args.is_empty() {
        frame.args.clear();
    }
    for _ in 0..n {
        frame.args.push(None);
    }
    // The `RECV`s of the passed arguments have nothing left to do.
    frame.ip = uf.plain_first_op(n);
    if prep.closure {
        bind_closure_vars(frame);
    }
    // Released by `Frame::release_direct` at its return.
    frame.direct = true;
    caller.ip = caller_ip;
    vm.frames.live = live + 1;
    Ok(())
}

/// The bound variables of the frame's closure (`use`), copied into their
/// compiled variables after the arguments, as `Vm::bind_closure_vars` does
/// once `Vm::push_simple_ucall` pushed the frame (the closure's `$this` and
/// scopes are the frame's already, and a plain call's closure has no
/// static variables).
fn bind_closure_vars(frame: &mut crate::vm::Frame) {
    let Some(c) = frame.closure.as_ref() else {
        return;
    };
    let internal = crate::object::data(c).internal.borrow();
    let Some(d) = internal
        .as_ref()
        .and_then(|x| x.downcast_ref::<crate::closure::ClosureData>())
    else {
        return;
    };
    for (cv, v) in &d.vars {
        if let Some(slot) = frame.cvs.get_mut(*cv as usize) {
            if let Some(old) = slot.as_mut().replace(v.clone()) {
                drop(old);
            }
        }
    }
}

/// Turns the prepared call into the [`PendingCall`] the generic
/// `InitFCall` and `Send`s would have built (the arguments sent so far in
/// its inline buffer), in its place among the caller's pending calls, and
/// returns the frame to the pool.
pub(super) fn materialize(vm: &mut Vm, prep: Prepared) {
    let live = vm.frames.live;
    let (callers, pool) = vm.frames.vec.split_at_mut(live);
    let (Some(caller), Some(frame)) = (callers.last_mut(), pool.first_mut()) else {
        return;
    };
    let mut call = PendingCall {
        direct: prep.fcall,
        dynamic: prep.closure,
        is_ctor: prep.ctor,
        this: frame.this.take(),
        scope: frame.scope.take(),
        called: frame.called.take(),
        closure: frame.closure.take(),
        ..PendingCall::new(Func::User(prep.uf))
    };
    for (arg, slot) in call.inline_args.iter_mut().zip(&mut frame.cvs[..prep.n]) {
        *arg = slot.as_mut().take();
    }
    #[allow(clippy::cast_possible_truncation)] // at most `MAX_ARGS`
    {
        call.inline_count = prep.n as u8;
    }
    let at = prep.calls_len.min(caller.calls.len());
    caller.calls.insert(at, call);
    frame.clear_for_reuse(&vm.empty_code);
}

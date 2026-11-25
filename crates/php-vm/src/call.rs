// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Function calls: `INIT_*`, `SEND_*`, `DO_FCALL`, `RECV*` and returns.

use crate::class::Class;
use crate::exec::scope_display;
use crate::func::{flags, Args, Builtin, Func, Method, UserFunction};
use crate::object;
use crate::opcode::{ArgSrc, ClassSpec, FetchMode, MethodCallCacheEntry, Op, Operand};
use crate::typecheck::{given_type, TypeCtx};
use crate::vm::{Frame, FrameKind, PendingCall, RetTarget, Throw, Vm, VmResult};
use php_runtime::{ArrayKey, HashTable, Obj, PhpRef, Value, ZArray, ZStr};
use std::rc::Rc;

thread_local! {
    /// The stand-in constructor of `new` on a constructor-less class with
    /// arguments, built once: it is never called, it only lets the SEND ops
    /// evaluate the arguments (see `Vm::op_new`).
    static NO_CTOR: Rc<Builtin> = Rc::new(Builtin::new(
        ZStr::from("__construct"),
        crate::func::Signature::default(),
        flags::NO_BODY,
        |_vm, _args| Ok(Value::Null),
    ));
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) enum CallTarget<'a> {
    Func(&'a str),
    Method(&'a Obj, &'a str),
    Static(&'a Rc<Class>, &'a str),
}

impl Vm {
    /// Returns true if execution observers are active.
    #[inline(always)]
    pub(crate) fn has_observers(&self) -> bool {
        self.observer.enabled
    }

    /// Dynamic or named function/callable initialization (`op_init_call`).
    #[inline(always)]
    #[allow(dead_code)]
    pub(crate) fn op_init_call(&mut self, callee: Value) -> VmResult<()> {
        self.op_init_dyn_call(callee)
    }

    /// Calls a target directly by enum, avoiding lowercase allocation if already lowercase ASCII.
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn op_init_call_target(&mut self, target: CallTarget<'_>) -> VmResult<()> {
        match target {
            CallTarget::Func(name) => {
                let bytes = name.as_bytes();
                let f = if !bytes.iter().any(u8::is_ascii_uppercase) {
                    self.functions.get(bytes).cloned()
                } else {
                    let lc = name.to_ascii_lowercase();
                    self.functions.get(lc.as_bytes()).cloned()
                };
                let Some(f) = f else {
                    return Err(
                        self.make_throw("Error", &format!("Call to undefined function {name}()"))
                    );
                };
                self.push_pending(PendingCall::new(f));
                Ok(())
            }
            CallTarget::Method(o, name) => {
                let call = self.method_call(o, &ZStr::from(name))?;
                self.push_pending(call);
                Ok(())
            }
            CallTarget::Static(cls, name) => {
                let call = self.static_method_call(cls, &ZStr::from(name), false)?;
                self.push_pending(call);
                Ok(())
            }
        }
    }
    /// Display name of a function for messages (`A::m`, `foo`):
    /// `get_function_or_method_name`. A closure is named after its bound
    /// scope like a method (`K::{closure:K::mk():3}`, `Closure::{closure:…}`
    /// for an object bound without a scope); only an unscoped one prints
    /// its bare name.
    #[allow(clippy::unused_self)] // method-style API used across the VM
    /// `Class::method` (or `function`) as error messages name it: the full
    /// class name, so an anonymous class keeps its NUL byte and the file and
    /// line after it (GH-25352).
    pub(crate) fn func_display(&self, func: &Func, scope: Option<&Rc<Class>>) -> String {
        let name = func.name().to_string_lossy();
        match scope {
            Some(c) => format!("{}::{}", c.name.to_string_lossy(), name),
            None => name,
        }
    }

    #[allow(dead_code)]
    fn frame_func_display(&self, f: &Frame) -> String {
        match &f.func {
            Some(func) => {
                let mut n = self.func_display(func, f.scope.as_ref());
                if let Some(m) = &f.magic_name {
                    if func.is_closure() && m.eq_ignore_ascii_case(b"__invoke") {
                        n = "Closure::__invoke".to_string();
                    } else {
                        n = format!(
                            "{}::{}",
                            f.scope
                                .as_ref()
                                .map_or_else(String::new, |c| c.name.to_string_lossy()),
                            m
                        );
                    }
                }
                n
            }
            None => String::new(),
        }
    }

    // ------------------------------------------------------------------
    // INIT
    // ------------------------------------------------------------------

    /// Pushes a pending call. Always inlined: as a call, the ~140-byte
    /// `PendingCall` is copied once more on the way into `calls`.
    #[inline(always)]
    #[allow(clippy::inline_always)] // see above: one copy of the call less per INIT
    fn push_pending(&mut self, call: PendingCall) {
        self.frame_mut().calls.push(call);
    }

    /// Pushes the pending call `make` builds, built in place in the
    /// `calls` vector (php-src's `zend_vm_stack_push_call_frame` writes the
    /// call into the VM stack slot directly). `Vec::push` takes a finished
    /// `PendingCall` (216 bytes), built on the native stack and then copied
    /// with an out-of-line `memcpy`, because the vector may grow between
    /// the two; here the room is reserved first and the call is written
    /// straight into it.
    #[inline(always)]
    #[allow(clippy::inline_always)] // the point is building in place
    fn push_pending_with(&mut self, make: impl FnOnce() -> PendingCall) {
        push_call_in_place(&mut self.frame_mut().calls, make);
    }

    /// Installs a pooled argument buffer into the pending call before its
    /// first argument, so the SEND ops do not allocate (php-src's VM-stack
    /// call frames). Zero-argument calls never touch the pool, and later
    /// arguments of a call that already has a buffer leave it alone.
    #[inline]
    pub(crate) fn pooled_args(&mut self) {
        let Some(frame) = self.frames.last_mut() else {
            return;
        };
        let Some(call) = frame.calls.last_mut() else {
            return;
        };
        if call.inline_count > 0 {
            let n = call.inline_count as usize;
            if let Some(buf) = self.call_bufs.args.pop() {
                install_buffer(&mut call.args, buf);
            }
            call.args.reserve(n);
            for i in 0..n {
                call.args.push(call.inline_args[i].take());
            }
            call.inline_count = 0;
        }
        if call.args.capacity() != 0 {
            return;
        }
        if let Some(v) = self.call_bufs.args.pop() {
            install_buffer(&mut call.args, v);
        }
    }

    /// Returns an argument buffer to the pool, keeping larger capacities when
    /// small Rust-origin callback buffers compete with prewarmed buffers.
    #[inline]
    pub(crate) fn put_arg_vec(&mut self, mut v: Vec<Option<Value>>) {
        if v.capacity() > 0 && v.capacity() <= 256 {
            v.clear();
            if self.call_bufs.args.len() < 4 {
                self.call_bufs.args.push(v);
            } else if let Some(smallest) = self
                .call_bufs
                .args
                .iter_mut()
                .min_by_key(|buffer| buffer.capacity())
            {
                if smallest.capacity() < v.capacity() {
                    *smallest = v;
                }
            }
        }
    }

    /// Returns a ZPP value buffer to the pool.
    #[inline]
    pub(crate) fn put_val_vec(&mut self, mut v: Vec<Value>) {
        if v.capacity() > 0 && v.capacity() <= 256 && self.call_bufs.values.len() < 4 {
            v.clear();
            self.call_bufs.values.push(v);
        }
    }

    /// Returns a ZPP passed-flag buffer to the pool.
    #[inline]
    pub(crate) fn put_bool_vec(&mut self, mut v: Vec<bool>) {
        if v.capacity() > 0 && v.capacity() <= 256 && self.call_bufs.passed.len() < 4 {
            v.clear();
            self.call_bufs.passed.push(v);
        }
    }

    /// `ZEND_INIT_FCALL` of a call site whose function is in the run-time
    /// cache, from the dispatch loop's hot-instruction block (no
    /// `SAVE_OPLINE`): [`Vm::op_init_fcall`]'s cache hit for a name without
    /// a global fallback (the fallback's per-site slot is found by the
    /// saved instruction pointer). Returns `false`, having changed
    /// nothing, otherwise.
    #[inline]
    pub(crate) fn init_fcall_fast(&mut self, name: u32, fallback: Option<u32>) -> bool {
        if fallback.is_some() {
            return false;
        }
        let Some(frame) = self.frames.last_mut() else {
            return false;
        };
        init_fcall_in(frame, name)
    }

    pub(crate) fn op_init_fcall(
        &mut self,
        name: u32,
        fallback: Option<u32>,
        display: u32,
    ) -> VmResult<()> {
        // A site that resolved to the global fallback keeps it, even after
        // another site cached a later-declared `ns\foo` under the literal.
        if fallback.is_some() {
            if let Some(func) = self.fallback_site_cached() {
                self.push_pending_with(move || PendingCall::new(func));
                return Ok(());
            }
        }
        let frame = self.frames.last_mut().expect("active frame");
        let cached = frame
            .code
            .fcall_cache
            .borrow()
            .get(name as usize)
            .and_then(|c| c.as_ref().and_then(crate::opcode::CachedFunc::get));
        if let Some(func) = cached {
            let direct = fallback.is_none();
            push_call_in_place(&mut frame.calls, move || PendingCall {
                direct,
                ..PendingCall::new(func)
            });
            return Ok(());
        }
        let lc = self.lit_str(name);
        let mut f = self.functions.get(&lc).cloned();
        // Primary-name hits are cached by literal; global-fallback hits are
        // cached per call site (literals are shared between call sites, and
        // a later `ns\foo` declaration must win over the global fallback at
        // sites that have not run yet).
        let cacheable = f.is_some();
        if f.is_none() {
            if let Some(fb) = fallback {
                let fbn = self.lit_str(fb);
                f = self.functions.get(&fbn).cloned();
                if let Some(func) = &f {
                    self.cache_fallback_site(func);
                }
            }
        }
        let Some(func) = f else {
            let d = self.lit_str(display);
            return Err(self.make_throw(
                "Error",
                &format!("Call to undefined function {}()", d.to_string_lossy()),
            ));
        };
        if cacheable {
            let slot = name as usize;
            // Sized for every literal at the first miss, like the run-time
            // cache `init_func_run_time_cache()` allocates up front: later
            // call sites never grow it while the code runs (which
            // `memory_get_usage()` deltas would show, bug #81142).
            let size = self.frame().code.literals.len().max(slot + 1);
            let mut cache = self.frame().code.fcall_cache.borrow_mut();
            if cache.len() <= slot {
                crate::rtcache::grow_slots(&mut cache, size, || None);
            }
            cache[slot] = Some(crate::opcode::CachedFunc::new(&func));
        }
        let mut call = PendingCall::new(func);
        call.direct = fallback.is_none();
        self.push_pending(call);
        Ok(())
    }

    /// The `fcall_cache` slot of the running `InitFCall` when it resolved
    /// to the global fallback: `ZEND_INIT_NS_FCALL_BY_NAME` caches the
    /// function in the call site's run-time cache slot, so the site keeps
    /// calling it after a same-named namespaced function is declared.
    /// Slots past the literals are indexed by op.
    fn fallback_site_slot(&self) -> usize {
        let f = self.frame();
        f.code.literals.len() + f.ip
    }

    #[inline]
    fn fallback_site_cached(&self) -> Option<Func> {
        let slot = self.fallback_site_slot();
        self.frame()
            .code
            .fcall_cache
            .borrow()
            .get(slot)
            .and_then(|c| c.as_ref().and_then(crate::opcode::CachedFunc::get))
    }

    #[cold]
    fn cache_fallback_site(&self, func: &Func) {
        let slot = self.fallback_site_slot();
        let code = &self.frame().code;
        let size = (code.literals.len() + code.ops.len()).max(slot + 1);
        let mut cache = code.fcall_cache.borrow_mut();
        if cache.len() <= slot {
            crate::rtcache::grow_slots(&mut cache, size, || None);
        }
        cache[slot] = Some(crate::opcode::CachedFunc::new(func));
    }

    #[inline]
    fn static_site_cached(&self) -> Option<(Func, Option<Rc<Class>>, Option<Rc<Class>>)> {
        let slot = self.fallback_site_slot();
        self.frame()
            .code
            .fcall_cache
            .borrow()
            .get(slot)
            .and_then(|c| c.as_ref().and_then(crate::opcode::CachedFunc::get_call))
    }

    #[cold]
    fn cache_static_site(
        &self,
        func: &Func,
        scope: Option<&Rc<Class>>,
        called: Option<&Rc<Class>>,
    ) {
        let slot = self.fallback_site_slot();
        let code = &self.frame().code;
        let size = (code.literals.len() + code.ops.len()).max(slot + 1);
        let mut cache = code.fcall_cache.borrow_mut();
        if cache.len() <= slot {
            crate::rtcache::grow_slots(&mut cache, size, || None);
        }
        cache[slot] = Some(crate::opcode::CachedFunc::new_static(func, scope, called));
    }

    pub(crate) fn op_init_dyn_call(&mut self, callee: Value) -> VmResult<()> {
        let callee = callee.deref();
        // `$closure()` (`zend_init_dynamic_call_object` of a closure): the
        // call is built straight from the closure's data.
        if let Value::Object(o) = &callee {
            if self.push_closure_user_call(o) {
                return Ok(());
            }
            if let Some(mut call) = self.closure_call(o) {
                call.dynamic = true;
                self.push_pending(call);
                return Ok(());
            }
        }
        let mut call = self.resolve_dyn_callable(&callee)?;
        call.dynamic = true;
        self.push_pending(call);
        Ok(())
    }

    /// `ZEND_INIT_DYNAMIC_CALL` of `$cv()` for a variable holding a closure
    /// of a user function, from the dispatch loop's hot-instruction block
    /// (no `SAVE_OPLINE`): [`Vm::op_init_dyn_call`]'s first case without
    /// the generic operand read. Returns `false`, having changed nothing,
    /// for an undefined variable, a reference or anything but such a
    /// closure, and in a frame with an attached symbol table.
    #[inline]
    pub(crate) fn init_closure_call_fast(&mut self, cv: u32) -> bool {
        let Some(frame) = self.frames.last_mut() else {
            return false;
        };
        if frame.symtab_attached {
            return false;
        }
        // The variable's object is borrowed, not copied: the call takes
        // its own handle (`closure`) from it.
        match frame.cvs.get(cv as usize).and_then(crate::vm::CvSlot::get) {
            Some(Value::Object(o)) => push_closure_user_call_in(&mut frame.calls, o),
            _ => false,
        }
    }

    /// [`Vm::closure_user_call`] as a dynamic call (`$closure()`), the call
    /// built in place in `calls` ([`Vm::push_pending_with`]): only the
    /// handles the call takes from the closure are carried out of its
    /// data, not a whole `PendingCall` returned and pushed (two copies of
    /// 216 bytes). Returns `false`, having pushed nothing, when the object
    /// is not a closure of a user function.
    #[inline]
    fn push_closure_user_call(&mut self, o: &Obj) -> bool {
        push_closure_user_call_in(&mut self.frame_mut().calls, o)
    }

    /// `zend_init_dynamic_call_string` / `_array`: like
    /// [`Vm::resolve_callable_for_call`], but a `"C::m"` / `["C", "m"]`
    /// callee never borrows the caller's `$this` (a non-static method, or
    /// the `__call` trampoline, "cannot be called statically"), and the
    /// method name of `[$obj, "P::m"]` is looked up whole by the object's
    /// `get_method` handler (an undefined method, or `__call("P::m")`):
    /// only `zend_is_callable_ex` splits such names, for `call_user_func()`
    /// and `Z_PARAM_FUNC`.
    fn resolve_dyn_callable(&mut self, callee: &Value) -> VmResult<PendingCall> {
        let class_method = match callee {
            Value::Str(s) => find_double_colon(s.as_bytes()).map(|p| {
                (
                    ZStr::from(&s.as_bytes()[..p]),
                    ZStr::from(&s.as_bytes()[p + 2..]),
                )
            }),
            Value::Array(a) if a.len() == 2 => {
                let target = a.get(&ArrayKey::Int(0)).map(Value::deref);
                let method = a.get(&ArrayKey::Int(1)).map(Value::deref);
                match (target, method) {
                    (Some(Value::Str(c)), Some(Value::Str(m))) => Some((c, m)),
                    (Some(Value::Object(o)), Some(Value::Str(m)))
                        if find_double_colon(m.as_bytes()).is_some() =>
                    {
                        return self.method_call(&o, &m);
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        let Some((cls_name, m)) = class_method else {
            if let Value::Object(o) = callee.deref() {
                if let Some(c) = self.closure_user_call(&o) {
                    return Ok(c);
                }
            }
            return self.resolve_callable_for_call(callee);
        };
        let cls = self.lookup_class_or_error(&cls_name)?;
        self.static_method_call_ex(&cls, &m, false, false)
    }

    /// Resolves a callable value for a dynamic call (`$f()`).
    pub(crate) fn resolve_callable_for_call(&mut self, callee: &Value) -> VmResult<PendingCall> {
        match callee {
            Value::Str(s) => {
                if let Some(p) = find_double_colon(s.as_bytes()) {
                    let cls_name = ZStr::from(&s.as_bytes()[..p]);
                    let m = ZStr::from(&s.as_bytes()[p + 2..]);
                    let cls = self.lookup_class_or_error(&cls_name)?;
                    return self.static_method_call(&cls, &m, false);
                }
                let name = s.as_bytes().strip_prefix(b"\\").unwrap_or(s.as_bytes());
                let f = if !name.iter().any(u8::is_ascii_uppercase) {
                    self.functions.get(name).cloned()
                } else {
                    let lc = name.to_ascii_lowercase();
                    self.functions.get(lc.as_slice()).cloned()
                };
                match f {
                    Some(f) => Ok(PendingCall::new(f)),
                    None => Err(self.make_throw(
                        "Error",
                        &format!("Call to undefined function {}()", s.to_string_lossy()),
                    )),
                }
            }
            Value::Array(a) => {
                if a.len() != 2 {
                    return Err(
                        self.make_throw("Error", "Array callback must have exactly two elements")
                    );
                }
                let (Some(target), Some(method)) =
                    (a.get(&ArrayKey::Int(0)), a.get(&ArrayKey::Int(1)))
                else {
                    return Err(
                        self.make_throw("Error", "Array callback has to contain indices 0 and 1")
                    );
                };
                let target = target.deref();
                let method = method.deref();
                // `zend_init_dynamic_call_array`: the target is checked
                // before the method name.
                if !matches!(target, Value::Object(_) | Value::Str(_)) {
                    return Err(self.make_throw(
                        "Error",
                        "First array member is not a valid class name or object",
                    ));
                }
                let Value::Str(m) = method else {
                    return Err(
                        self.make_throw("Error", "Second array member is not a valid method")
                    );
                };
                match target {
                    Value::Object(o) => {
                        if let Some(p) = find_double_colon(m.as_bytes()) {
                            // `[$obj, 'parent::m']` split like
                            // `zend_is_callable_ex` does: reached by the
                            // callers that do not resolve through it yet
                            // (`Fiber`) and by [`Vm::call_value`]'s error
                            // path (no parent class).
                            let cn = ZStr::from(&m.as_bytes()[..p]);
                            let mn = ZStr::from(&m.as_bytes()[p + 2..]);
                            let cls = if cn.eq_ignore_ascii_case(b"parent") {
                                object::class_of(&o).parent.clone().ok_or_else(|| {
                                    self.make_throw("Error", "Cannot use \"parent\" when current class scope has no parent")
                                })?
                            } else {
                                self.lookup_class_or_error(&cn)?
                            };
                            let mut c = self.method_call_on(&o, &cls, &mn)?;
                            c.called = Some(object::class_of(&o).clone());
                            return Ok(c);
                        }
                        self.method_call(&o, &m)
                    }
                    Value::Str(cn) => {
                        let cls = self.lookup_class_or_error(&cn)?;
                        self.static_method_call(&cls, &m, false)
                    }
                    _ => Err(self.make_throw(
                        "Error",
                        "First array member is not a valid class name or object",
                    )),
                }
            }
            Value::Object(o) => {
                if let Some(c) = self.closure_user_call(o) {
                    return Ok(c);
                }
                if let Some(c) = self.closure_call(o) {
                    return Ok(c);
                }
                let cls = object::class_of(o).clone();
                match cls.magic.invoke.clone() {
                    Some(m) => Ok(PendingCall {
                        this: Some(o.clone()),
                        scope: Some(m.scope()),
                        called: Some(cls),
                        ..PendingCall::new(m.func.clone())
                    }),
                    None => Err(self.make_throw(
                        "Error",
                        &format!(
                            "Object of type {} is not callable",
                            object::display_name(&cls.name)
                        ),
                    )),
                }
            }
            other => Err(self.make_throw(
                "Error",
                &format!("Value of type {} is not callable", other.type_name()),
            )),
        }
    }

    /// The object operand of `ZEND_INIT_METHOD_CALL`: for op1 `IS_UNUSED`
    /// (`$this->name()` where the compiler found `$this` guaranteed) the
    /// frame's `$this`, otherwise the operand's value. A frame without one
    /// (a closure nested in a closure that was unbound from its object)
    /// throws as `ZEND_FETCH_THIS` would (as PHP 8.3 did; the handler of 8.5
    /// and master assumes an object, and 8.5 crashes on such a script).
    #[inline]
    pub(crate) fn method_object(&mut self, obj: Operand) -> VmResult<Value> {
        if !matches!(obj, Operand::Unused) {
            return self.read(obj);
        }
        match self.frame().this.clone() {
            Some(o) => Ok(Value::Object(o)),
            None => Err(self.make_throw("Error", "Using $this when not in object context")),
        }
    }

    /// `ZEND_INIT_METHOD_CALL` with the method name as an operand. A
    /// literal name (`$o->m()`) probes the run-time cache in place
    /// ([`Vm::init_cached_method_call`]); other names, and misses, are read
    /// and take [`Vm::op_init_method_call`].
    pub(crate) fn op_init_method_call_op(&mut self, obj: Value, name: Operand) -> VmResult<()> {
        let obj = match name {
            Operand::Lit(lit) => match self.init_cached_method_call(obj, lit) {
                Ok(()) => return Ok(()),
                Err(obj) => obj,
            },
            _ => obj,
        };
        let n = self.read(name)?;
        self.op_init_method_call(obj, n)
    }

    /// `ZEND_INIT_METHOD_CALL` of `$cv->name()`, `$tmp->name()` (`$this`
    /// fetched by `FetchThis`, a call result, ...) or `$this->name()` (op1
    /// [`Operand::Unused`]) on a run-time cache hit, from the dispatch
    /// loop's hot-instruction block (no `SAVE_OPLINE`): the hit of
    /// [`Vm::init_cached_method_call`] without the generic operand read.
    /// The call's `$this` is a handle of its own, as there: a copy of the
    /// variable's object or of the frame's `$this`, or the temporary's,
    /// moved out of it as the read moves it. `slot` is the instruction's
    /// index (its cache slot). Returns `false`, having changed nothing, for
    /// an undefined variable, a reference or a value that is not an object,
    /// a variable of a frame with an attached symbol table, a frame without
    /// `$this`, a class with a native `get_method` handler or a cache miss.
    #[inline]
    pub(crate) fn init_method_call_fast(&mut self, obj: Operand, lit: u32, slot: usize) -> bool {
        let Some(frame) = self.frames.last_mut() else {
            return false;
        };
        init_method_call_in(frame, obj, lit, slot)
    }

    /// The run-time cache hit of `ZEND_INIT_METHOD_CALL` for the method
    /// named by literal `lit` (`CACHED_POLYMORPHIC_PTR`): the call is pushed
    /// straight from the cached method, `$this` moved in, with the name read
    /// in place from the op array and the calling scope from the current
    /// frame (it executes this op, so it is not a builtin frame and is the
    /// frame [`Vm::scope`] finds). Gives the operand back on a miss, for an
    /// object whose class has a native `get_method` handler, and for a
    /// non-object.
    #[inline]
    fn init_cached_method_call(&mut self, obj: Value, lit: u32) -> Result<(), Value> {
        let Value::Object(o) = obj else {
            return Err(obj);
        };
        let cls = object::class_of(&o);
        if cls.hooks.as_ref().and_then(|h| h.get_method).is_some() {
            return Err(Value::Object(o));
        }
        let f = self.frame();
        let slot = f.ip.saturating_sub(1);
        let cls_addr = Rc::as_ptr(cls) as usize;
        let scope_addr = f.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);

        let hit = method_entry_hit(&f.code, slot, cls_addr, scope_addr, lit);

        if let Some((func, scope, is_static)) = hit {
            let called = cls.clone();
            let this = if is_static { None } else { Some(o) };
            self.push_pending_with(move || PendingCall {
                this,
                scope,
                called: Some(called),
                ..PendingCall::new(func)
            });
            return Ok(());
        }

        let Some(Value::Str(name)) = f.code.literals.get(lit as usize) else {
            return Err(Value::Object(o));
        };
        if name.eq_ignore_ascii_case(b"__invoke") {
            if let Some(mut call) = self.closure_user_call(&o) {
                call.magic_name = Some(ZStr::from("__invoke"));
                self.push_pending(call);
                return Ok(());
            }
        }
        let Some(m) = self.call_bufs.methods.get(cls, f.scope.as_ref(), name) else {
            return Err(Value::Object(o));
        };
        let called = cls.clone();
        let is_static = m.is_static() && m.flags & flags::STATIC_KEEPS_THIS == 0;
        let entry = MethodCallCacheEntry {
            class_addr: cls_addr,
            scope_addr,
            class: Rc::downgrade(cls),
            method_scope: Some(Rc::downgrade(&m.scope())),
            func: crate::opcode::CachedFunc::new(&m.func),
            is_static,
            name_lit: Some(lit),
            name: name.clone(),
        };
        let mut cache = f.code.method_cache.borrow_mut();
        if cache.len() <= slot {
            let size = f.code.ops.len().max(slot + 1);
            crate::rtcache::grow_slots(&mut cache, size, || None);
        }
        // The entry of another class this one replaces becomes the site's
        // second entry ([`method_entry_hit`]).
        if let Some(old) = cache[slot].replace(entry) {
            if old.class_addr != cls_addr {
                let second = second_method_slot(&f.code, slot);
                if cache.len() <= second {
                    crate::rtcache::grow_slots(&mut cache, second + 1, || None);
                }
                cache[second] = Some(old);
            }
        }
        drop(cache);

        self.push_pending(PendingCall {
            this: if is_static { None } else { Some(o) },
            scope: Some(m.scope()),
            called: Some(called),
            ..PendingCall::new(m.func.clone())
        });
        Ok(())
    }

    pub(crate) fn op_init_method_call(&mut self, obj: Value, name: Value) -> VmResult<()> {
        let Value::Str(name) = name else {
            return Err(self.make_throw("Error", "Method name must be a string"));
        };
        let Value::Object(o) = obj else {
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Call to a member function {}() on {}",
                    name.to_string_lossy(),
                    obj.value_name()
                ),
            ));
        };
        if name.eq_ignore_ascii_case(b"__invoke") {
            if let Some(mut call) = self.closure_user_call(&o) {
                call.magic_name = Some(ZStr::from("__invoke"));
                self.push_pending(call);
                return Ok(());
            }
        }
        let cls = object::class_of(&o);
        let has_get_method = cls.hooks.as_ref().and_then(|h| h.get_method).is_some();
        let slot = self.frame().ip.saturating_sub(1);
        let cls_addr = Rc::as_ptr(cls) as usize;
        let scope = self.frame().scope.clone();
        let scope_addr = scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);

        let hit = if !has_get_method {
            if let Some(Some(cached)) = self.frame().code.method_cache.borrow().get(slot) {
                if cached.class_addr == cls_addr
                    && cached.scope_addr == scope_addr
                    && cached.name.as_bytes().eq_ignore_ascii_case(name.as_bytes())
                {
                    cached.func.get().map(|func| {
                        (
                            func,
                            cached
                                .method_scope
                                .as_ref()
                                .and_then(std::rc::Weak::upgrade),
                            cached.is_static,
                        )
                    })
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        if let Some((func, method_scope, is_static)) = hit {
            let called = cls.clone();
            let this = if is_static { None } else { Some(o) };
            self.push_pending(PendingCall {
                this,
                scope: method_scope,
                called: Some(called),
                ..PendingCall::new(func)
            });
            return Ok(());
        }
        // A hit in the run-time cache of `ZEND_INIT_METHOD_CALL`: the call
        // is pushed straight from the cached method, `$this` moved in.
        let cls = cls.clone();
        if !has_get_method {
            if let Some(m) = self.call_bufs.methods.get(&cls, scope.as_ref(), &name) {
                let is_static = m.is_static() && m.flags & flags::STATIC_KEEPS_THIS == 0;
                let entry = MethodCallCacheEntry {
                    class_addr: cls_addr,
                    scope_addr,
                    class: Rc::downgrade(&cls),
                    method_scope: Some(Rc::downgrade(&m.scope())),
                    func: crate::opcode::CachedFunc::new(&m.func),
                    is_static,
                    name_lit: match self.frame().code.ops.get(slot) {
                        Some(Op::InitMethodCall {
                            name: Operand::Lit(lit),
                            ..
                        }) => Some(*lit),
                        _ => None,
                    },
                    name: name.clone(),
                };
                let mut cache = self.frame().code.method_cache.borrow_mut();
                if cache.len() <= slot {
                    let size = self.frame().code.ops.len().max(slot + 1);
                    crate::rtcache::grow_slots(&mut cache, size, || None);
                }
                cache[slot] = Some(entry);
                drop(cache);

                self.push_pending(PendingCall {
                    this: if is_static { None } else { Some(o) },
                    scope: Some(m.scope()),
                    called: Some(cls),
                    ..PendingCall::new(m.func.clone())
                });
                return Ok(());
            }
        }
        let call = self.method_call(&o, &name)?;
        if !has_get_method && call.magic_name.is_none() {
            let is_static = call.this.is_none();
            let entry = MethodCallCacheEntry {
                class_addr: cls_addr,
                scope_addr,
                class: Rc::downgrade(&cls),
                method_scope: call.scope.as_ref().map(Rc::downgrade),
                func: crate::opcode::CachedFunc::new(&call.func),
                is_static,
                name_lit: match self.frame().code.ops.get(slot) {
                    Some(Op::InitMethodCall {
                        name: Operand::Lit(lit),
                        ..
                    }) => Some(*lit),
                    _ => None,
                },
                name,
            };
            let mut cache = self.frame().code.method_cache.borrow_mut();
            if cache.len() <= slot {
                let size = self.frame().code.ops.len().max(slot + 1);
                crate::rtcache::grow_slots(&mut cache, size, || None);
            }
            cache[slot] = Some(entry);
        }
        self.push_pending(call);
        Ok(())
    }

    /// Whether `scope` may call method `m`.
    pub(crate) fn can_access_method(&self, m: &Method, scope: Option<&Rc<Class>>) -> bool {
        match m.visibility() {
            flags::PUBLIC => true,
            flags::PRIVATE => scope.is_some_and(|s| Rc::ptr_eq(s, &m.scope())),
            _ => {
                let Some(s) = scope else {
                    return false;
                };
                let root = m.proto_scope.upgrade().unwrap_or_else(|| m.scope());
                s.is_subclass_of(&root) || root.is_subclass_of(s) || {
                    let ms = m.scope();
                    s.is_subclass_of(&ms) || ms.is_subclass_of(s)
                }
            }
        }
    }

    /// Prepares `$obj->name()`.
    pub(crate) fn method_call(&mut self, o: &Obj, name: &ZStr) -> VmResult<PendingCall> {
        let cls = object::class_of(o).clone();
        // Native `get_method` handler (e.g. SPL's "wasn't initialized
        // properly" check before any method runs).
        if let Some(f) = cls.hooks.as_ref().and_then(|h| h.get_method) {
            f(self, o, name)?;
        }
        if name.eq_ignore_ascii_case(b"__invoke") {
            if let Some(mut call) = self.closure_user_call(o) {
                call.magic_name = Some(ZStr::from("__invoke"));
                return Ok(call);
            }
        }
        // The run-time cache of `ZEND_INIT_METHOD_CALL`.
        let scope = self.scope();
        if let Some(m) = self.call_bufs.methods.get(&cls, scope.as_ref(), name) {
            return Ok(self.pending_for_method(&m, Some(o.clone()), cls));
        }
        self.method_call_on(o, &cls, name)
    }

    fn method_call_on(&mut self, o: &Obj, cls: &Rc<Class>, name: &ZStr) -> VmResult<PendingCall> {
        let scope = self.scope();
        let lc = if name.as_bytes().iter().any(u8::is_ascii_uppercase) {
            ZStr::from(name.to_ascii_lowercase())
        } else {
            name.clone()
        };
        // Only a lookup for the object's own class is cached: the private
        // method rule below depends on it.
        let cacheable = Rc::ptr_eq(cls, object::class_of(o));
        // A private method of the calling scope takes precedence.
        if let Some(s) = &scope {
            if let Some(m) = s.methods.get(&lc) {
                if m.visibility() == flags::PRIVATE
                    && Rc::ptr_eq(&m.scope(), s)
                    && object::class_of(o).is_subclass_of(s)
                {
                    if cacheable {
                        self.call_bufs.methods.insert(cls, Some(s), name, m);
                    }
                    return Ok(self.pending_for_method(
                        m,
                        Some(o.clone()),
                        object::class_of(o).clone(),
                    ));
                }
            }
        }
        if let Some(m) = cls.methods.get(&lc).cloned() {
            if self.can_access_method(&m, scope.as_ref()) {
                if cacheable {
                    self.call_bufs.methods.insert(cls, scope.as_ref(), name, &m);
                }
                return Ok(self.pending_for_method(
                    &m,
                    Some(o.clone()),
                    object::class_of(o).clone(),
                ));
            }
            if let Some(callm) = cls.magic.call.clone() {
                return Ok(self.magic_call(&callm, Some(o.clone()), cls.clone(), name));
            }
            let vis = if m.visibility() == flags::PRIVATE {
                "private"
            } else {
                "protected"
            };
            // `zend_bad_method_call`: the method's declaring scope and the
            // name as spelled by the caller.
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Call to {vis} method {}::{}() from {}",
                    m.scope().name.to_string_lossy(),
                    name.to_string_lossy(),
                    scope_display(scope.as_ref())
                ),
            ));
        }
        if let Some(mut c) = self.closure_method_call(o, &lc) {
            c.magic_name = Some(ZStr::from("__invoke"));
            return Ok(c);
        }
        if let Some(callm) = cls.magic.call.clone() {
            return Ok(self.magic_call(&callm, Some(o.clone()), cls.clone(), name));
        }
        if let Some(m) = self.extra_method(o, cls, name) {
            return Ok(self.pending_for_method(&m, Some(o.clone()), object::class_of(o).clone()));
        }
        Err(self.make_throw(
            "Error",
            &format!(
                "Call to undefined method {}::{}()",
                object::display_name(&cls.name),
                name.to_string_lossy()
            ),
        ))
    }

    /// The rest of an object's `get_method` handler once
    /// `zend_std_get_method` found nothing ([`ClassHooks::extra_method`],
    /// e.g. PDO's driver methods): asked only for the object's own class
    /// and when that class has no `__call` (whose trampoline
    /// `zend_std_get_method` would have returned).
    ///
    /// [`ClassHooks::extra_method`]: crate::ext::ClassHooks::extra_method
    #[cold]
    #[inline(never)]
    pub(crate) fn extra_method(
        &mut self,
        o: &Obj,
        cls: &Rc<Class>,
        name: &ZStr,
    ) -> Option<Rc<Method>> {
        if !Rc::ptr_eq(cls, object::class_of(o)) || cls.magic.call.is_some() {
            return None;
        }
        let f = cls.hooks.as_ref().and_then(|h| h.extra_method)?;
        f(self, o, name)
    }

    fn pending_for_method(
        &self,
        m: &Rc<Method>,
        this: Option<Obj>,
        called: Rc<Class>,
    ) -> PendingCall {
        let is_static = m.is_static() && m.flags & flags::STATIC_KEEPS_THIS == 0;
        PendingCall {
            this: if is_static { None } else { this },
            scope: Some(m.scope()),
            called: Some(called),
            ..PendingCall::new(m.func.clone())
        }
    }

    /// `X::__construct()` on an inaccessible private constructor
    /// (`ZEND_INIT_STATIC_METHOD_CALL` with an unused op2): "Cannot call
    /// private `X::`__`construct()`" unless `$this` is exactly the constructor's
    /// class.
    #[cold]
    fn private_ctor_call_error(
        &mut self,
        cls: &Rc<Class>,
        m: &Method,
        this: Option<&Obj>,
        lc: &ZStr,
    ) -> Option<Throw> {
        if lc.as_bytes() != b"__construct" || m.visibility() != flags::PRIVATE {
            return None;
        }
        if this.is_some_and(|t| Rc::ptr_eq(object::class_of(t), &m.scope())) {
            return None;
        }
        Some(self.make_throw(
            "Error",
            &format!(
                "Cannot call private {}::__construct()",
                object::display_name(&cls.name)
            ),
        ))
    }

    fn magic_call(
        &self,
        m: &Rc<Method>,
        this: Option<Obj>,
        called: Rc<Class>,
        name: &ZStr,
    ) -> PendingCall {
        PendingCall {
            this,
            scope: Some(m.scope()),
            called: Some(called),
            magic_name: Some(trampoline_name(name)),
            ..PendingCall::new(m.func.clone())
        }
    }

    /// Prepares `Class::name()`.
    pub(crate) fn static_method_call(
        &mut self,
        cls: &Rc<Class>,
        name: &ZStr,
        forward: bool,
    ) -> VmResult<PendingCall> {
        self.static_method_call_ex(cls, name, forward, true)
    }

    /// [`Vm::static_method_call`]; `use_this: false` is a dynamic call's
    /// `zend_std_get_static_method` without object forwarding.
    #[allow(clippy::too_many_lines)] // zend_std_get_static_method, one lookup order
    fn static_method_call_ex(
        &mut self,
        cls: &Rc<Class>,
        name: &ZStr,
        forward: bool,
        use_this: bool,
    ) -> VmResult<PendingCall> {
        let scope = self.scope();
        let this = self
            .frames
            .iter()
            .rev()
            .find(|f| f.kind != FrameKind::Builtin)
            .and_then(|f| f.this.clone());
        let cur_called = self
            .frames
            .iter()
            .rev()
            .find(|f| f.kind != FrameKind::Builtin)
            .and_then(|f| f.called.clone());
        let lc = if name.as_bytes().iter().any(u8::is_ascii_uppercase) {
            ZStr::from(name.to_ascii_lowercase())
        } else {
            name.clone()
        };
        let found = cls.methods.get(&lc).cloned();
        let m = match found {
            Some(m) if self.can_access_method(&m, scope.as_ref()) => m,
            other => {
                if let Some(m) = &other {
                    if let Some(e) = self.private_ctor_call_error(cls, m, this.as_ref(), &lc) {
                        return Err(e);
                    }
                }
                // __call in object context, else __callStatic
                // (`get_static_method_fallback`): the top-level `__call()`
                // of `$this`'s class is favoured over the named class's.
                if let Some(t) = &this {
                    if cls.magic.call.is_some() && object::class_of(t).is_subclass_of(cls) {
                        if let Some(callm) = object::class_of(t).magic.call.clone() {
                            if !use_this {
                                // The (non-static) trampoline needs `$this`.
                                return Err(self.make_throw(
                                    "Error",
                                    &format!(
                                        "Non-static method {}::{}() cannot be called statically",
                                        object::display_name(&callm.scope().name),
                                        name.to_string_lossy()
                                    ),
                                ));
                            }
                            return Ok(self.magic_call(
                                &callm,
                                Some(t.clone()),
                                object::class_of(t).clone(),
                                name,
                            ));
                        }
                    }
                }
                if let Some(cs) = cls.magic.call_static.clone() {
                    // `zend_get_call_trampoline_func` copies
                    // `ZEND_ACC_ABSTRACT`: an interface's or abstract
                    // class's `__callStatic` cannot be called (GH-17718).
                    if cs.is_abstract() {
                        return Err(self.abstract_method_error(&cs.scope(), name));
                    }
                    // The trampoline forwards the called scope like a
                    // resolved static method would.
                    let called = forwarded_called(cur_called, cls, forward);
                    return Ok(self.magic_call(&cs, None, called, name));
                }
                return Err(match other {
                    Some(m) => {
                        let vis = if m.visibility() == flags::PRIVATE {
                            "private"
                        } else {
                            "protected"
                        };
                        self.make_throw(
                            "Error",
                            &format!(
                                "Call to {vis} method {}::{}() from {}",
                                m.scope().name.to_string_lossy(),
                                name.to_string_lossy(),
                                scope_display(scope.as_ref())
                            ),
                        )
                    }
                    None => self.make_throw(
                        "Error",
                        &format!(
                            "Call to undefined method {}::{}()",
                            object::display_name(&cls.name),
                            name.to_string_lossy()
                        ),
                    ),
                });
            }
        };
        if m.is_abstract() {
            return Err(self.abstract_method_error(&m.scope(), &m.name));
        }
        if m.is_static() {
            let called = forwarded_called(cur_called, cls, forward);
            return Ok(PendingCall {
                scope: Some(m.scope()),
                called: Some(called),
                ..PendingCall::new(m.func.clone())
            });
        }
        // Non-static method: needs a compatible $this.
        if let Some(t) = this.filter(|_| use_this) {
            if object::class_of(&t).is_subclass_of(cls) {
                let called = object::class_of(&t).clone();
                return Ok(PendingCall {
                    this: Some(t),
                    scope: Some(m.scope()),
                    called: Some(called),
                    ..PendingCall::new(m.func.clone())
                });
            }
        }
        Err(self.make_throw(
            "Error",
            &format!(
                "Non-static method {}::{}() cannot be called statically",
                object::display_name(&m.scope().name),
                m.name
            ),
        ))
    }

    /// `zend_abstract_method_call`.
    #[cold]
    fn abstract_method_error(&mut self, scope: &Class, name: &ZStr) -> Throw {
        self.make_throw(
            "Error",
            &format!(
                "Cannot call abstract method {}::{}()",
                object::display_name(&scope.name),
                name.to_string_lossy()
            ),
        )
    }

    /// `ZEND_INIT_STATIC_METHOD_CALL` of `self::name()`, `parent::name()`
    /// or `static::name()` on a hit in the op's own monomorphic slot
    /// (php-src's `CACHED_PTR(opline->result.num) == ce`, then `fbc` from
    /// the next slot): the op's [`OpArray::method_cache`] entry remembers
    /// the class the spec resolved to, the calling scope and the method
    /// [`Vm::static_call_hit`] found for that pair
    /// ([`Vm::cache_relative_static_site`]), so a hit is two address
    /// compares where the member cache searched its entries, checked the
    /// access and upgraded the method. The call is then set up as that hit
    /// sets it up: a static method gets the frame's called scope when that
    /// is a subclass of the resolved class (`self::`, `parent::` and
    /// `static::` forward it), else the class; an instance method the
    /// frame's `$this` when its class is one. Returns `false`, having
    /// pushed nothing, on a miss, a released method, and an instance
    /// method without such a `$this` (the generic path raises the error).
    ///
    /// [`OpArray::method_cache`]: crate::opcode::OpArray::method_cache
    #[inline]
    fn init_relative_static_call_cached(&mut self, spec: &ClassSpec, lit: u32) -> bool {
        let Some(f) = self.frames.last_mut() else {
            return false;
        };
        if f.kind == FrameKind::Builtin {
            return false;
        }
        let slot = f.ip.wrapping_sub(1);
        init_relative_static_in(f, spec, lit, slot)
    }

    /// Fills the monomorphic slot of the running `self::name()` /
    /// `parent::name()` / `static::name()` op
    /// ([`Vm::init_relative_static_call_cached`]) from `hit`, the member
    /// cache's hit for the class the spec resolves to and the calling
    /// scope: only a pair the member cache already resolved (an accessible,
    /// non-abstract method found by name in a class that is not a trait,
    /// `Vm::static_call_fill`), which the same pair resolves to for good.
    #[cold]
    #[inline(never)]
    fn cache_relative_static_site(
        &self,
        spec: &ClassSpec,
        lit: u32,
        hit: &crate::rtcache::StaticCallHit,
    ) {
        let f = self.frame();
        let (Some(cls), Some(Value::Str(name))) = (
            relative_spec_class(f, spec),
            f.code.literals.get(lit as usize),
        ) else {
            return;
        };
        let slot = f.ip.wrapping_sub(1);
        if slot >= f.code.ops.len() {
            return;
        }
        let entry = MethodCallCacheEntry {
            class_addr: Rc::as_ptr(cls) as usize,
            scope_addr: f.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize),
            class: Rc::downgrade(cls),
            method_scope: Some(Rc::downgrade(&hit.scope)),
            func: crate::opcode::CachedFunc::new(&hit.func),
            is_static: hit.this.is_none(),
            name_lit: Some(lit),
            name: name.clone(),
        };
        let Ok(mut cache) = f.code.method_cache.try_borrow_mut() else {
            return;
        };
        if cache.len() <= slot {
            let size = f.code.ops.len();
            crate::rtcache::grow_slots(&mut cache, size, || None);
        }
        cache[slot] = Some(entry);
    }

    pub(crate) fn op_init_static_call(&mut self, class: &ClassSpec, name: Operand) -> VmResult<()> {
        let is_literal = matches!(class, ClassSpec::Named(_)) && matches!(name, Operand::Lit(_));
        if is_literal {
            if let Some((func, scope, called)) = self.static_site_cached() {
                self.push_pending_with(move || PendingCall {
                    scope,
                    called,
                    ..PendingCall::new(func)
                });
                return Ok(());
            }
        }
        let relative = match (class, name) {
            (ClassSpec::SelfClass | ClassSpec::Parent | ClassSpec::Static, Operand::Lit(lit)) => {
                if self.init_relative_static_call_cached(class, lit) {
                    return Ok(());
                }
                Some(lit)
            }
            _ => None,
        };
        // The op's run-time cache slot (see `rtcache`).
        if let Some(hit) = self.static_call_hit(class, name) {
            if let Some(lit) = relative {
                self.cache_relative_static_site(class, lit, &hit);
            }
            let crate::rtcache::StaticCallHit {
                func,
                this,
                scope,
                called,
            } = hit;
            self.push_pending_with(move || PendingCall {
                this,
                scope: Some(scope),
                called: Some(called),
                ..PendingCall::new(func)
            });
            return Ok(());
        }
        let name_op = name;
        let forward = matches!(
            class,
            ClassSpec::SelfClass | ClassSpec::Parent | ClassSpec::Static
        );
        // The class is fetched before the method name operand is read
        // (FETCH_CLASS / INIT_STATIC_METHOD_CALL).
        let cls = self.resolve_class_spec(class)?;
        let Value::Str(name) = self.read(name)? else {
            return Err(self.make_throw("Error", "Method name must be a string"));
        };
        // `X::__construct()` compiles to `ZEND_INIT_STATIC_METHOD_CALL` with
        // an unused method operand: it calls `ce->constructor`, never a
        // `__callStatic` fallback.
        if cls.magic.construct.is_none() && name.as_bytes().eq_ignore_ascii_case(b"__construct") {
            return Err(self.make_throw("Error", "Cannot call constructor"));
        }
        let call = self.static_method_call(&cls, &name, forward)?;
        self.static_call_fill(class, name_op, &cls, &call);
        if is_literal
            && call.this.is_none()
            && call.magic_name.is_none()
            && !cls.is_trait()
            && call.func.flags() & flags::VISIBILITY == flags::PUBLIC
        {
            self.cache_static_site(&call.func, call.scope.as_ref(), call.called.as_ref());
        }
        if cls.is_trait() {
            // `zend_std_get_static_method()`: the resolved method (or the
            // `__callStatic` trampoline) is scoped to the trait itself.
            let fname = match &call.magic_name {
                Some(n) => n.clone(),
                None => cls
                    .find_method(name.as_bytes())
                    .map_or_else(|| name.clone(), |m| m.name.clone()),
            };
            self.error(
                crate::errors::level::DEPRECATED,
                &format!(
                    "Calling static trait method {}::{fname} is deprecated, it should only be called on a class using the trait",
                    object::display_name(&cls.name)
                ),
            )?;
        }
        self.push_pending(call);
        Ok(())
    }

    /// `new`: creates the object, stores it and starts the constructor call.
    #[inline]
    pub(crate) fn op_new(&mut self, class: &ClassSpec, dst: u32) -> VmResult<bool> {
        let cls = self.resolve_class_spec(class)?;
        if cls.magic.construct.is_none() {
            let skip = match self.frame().code.ops.get(self.frame().ip.saturating_sub(1)) {
                Some(Op::New { skip, .. }) => *skip,
                _ => match self.frame().code.ops.get(self.frame().ip) {
                    Some(Op::New { skip, .. }) => *skip,
                    _ => 0,
                },
            };
            if skip != 0 {
                let o = self.instantiate(&cls)?;
                if let Some(hooks) = &cls.hooks {
                    if let Some(f) = hooks.get_constructor {
                        f(self, &o)?;
                    }
                }
                self.set_tmp(dst, Value::Object(o));
                return Ok(false);
            }
        }
        let o = self.instantiate(&cls)?;
        if cls.magic.construct.is_none() {
            if let Some(hooks) = &cls.hooks {
                if let Some(f) = hooks.get_constructor {
                    f(self, &o)?;
                }
            }
            // A constructor-less class still evaluates positional arguments and
            // rejects named arguments, without declaring a reflected constructor.
            let func = Func::Builtin(NO_CTOR.with(Rc::clone));
            let mut call = PendingCall::new(func);
            call.is_ctor = true;
            // Never called; marks the object constructor-failed when an
            // argument throws (`ZEND_LIVE_NEW`).
            call.this = Some(o.clone());
            self.set_tmp(dst, Value::Object(o));
            self.push_pending(call);
            return Ok(true);
        }
        self.set_tmp(dst, Value::Object(o.clone()));
        if let Some(hooks) = &cls.hooks {
            if let Some(f) = hooks.get_constructor {
                f(self, &o)?;
            }
        }
        let ctor = cls.magic.construct.as_ref().unwrap();
        // `zend_std_get_constructor` checks the scope only for a
        // non-public constructor.
        if ctor.visibility() != flags::PUBLIC {
            self.check_ctor_access(ctor, &o)?;
        }
        let is_static = ctor.is_static() && ctor.flags & flags::STATIC_KEEPS_THIS == 0;
        let scope = if std::rc::Weak::as_ptr(&ctor.scope) == Rc::as_ptr(&cls) {
            Some(cls.clone())
        } else {
            Some(ctor.scope())
        };
        let ctor_func = ctor.func.clone();
        // `ZEND_NEW` writes the constructor's call frame straight into the
        // VM stack: built in place, the call is not copied on the way.
        self.push_pending_with(move || PendingCall {
            this: if is_static { None } else { Some(o) },
            scope,
            called: Some(cls),
            is_ctor: true,
            ..PendingCall::new(ctor_func)
        });
        Ok(true)
    }

    /// The scope check of a non-public constructor called by `new`.
    #[cold]
    #[inline(never)]
    fn check_ctor_access(&mut self, ctor: &Method, o: &Obj) -> VmResult<()> {
        let scope = self.scope();
        if !self.can_access_method(ctor, scope.as_ref()) {
            // zend_std_get_constructor: zend_object_store_ctor_failed()
            // (GH-14456), so the destructor never runs.
            let d = object::data(o);
            d.flags.set(d.flags.get() | object::oflags::DESTRUCTED);
            let vis = if ctor.visibility() == flags::PRIVATE {
                "private"
            } else {
                "protected"
            };
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Call to {vis} {}::{}() from {}",
                    ctor.scope().name.to_string_lossy(),
                    ctor.name,
                    scope_display(scope.as_ref())
                ),
            ));
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // SEND
    // ------------------------------------------------------------------

    pub(crate) fn arg_position(&mut self, name: &ZStr) -> VmResult<Option<usize>> {
        let call = self.frame().calls.last().expect("pending call");
        if call.is_magic_trampoline() {
            return Ok(None);
        }
        let params = call.func.params();
        if let Some(p) = params.iter().position(|p| p.name == *name && !p.variadic) {
            return Ok(Some(p));
        }
        if call.func.is_variadic() {
            return Ok(None);
        }
        Err(self.make_throw(
            "Error",
            &format!("Unknown named parameter ${}", name.to_string_lossy()),
        ))
    }

    #[inline(always)]
    #[allow(dead_code)]
    pub(crate) fn op_send_val(&mut self, v: Value) -> VmResult<()> {
        let frame = self.frames.last_mut().expect("active frame");
        let call = frame.calls.last_mut().expect("pending call");
        let arg_idx = call.arg_count();
        let by_val =
            !call.is_magic_trampoline() && !call.has_named && !call.func.arg_by_ref(arg_idx);
        if by_val {
            if call.args.is_empty() && (call.inline_count as usize) < call.inline_args.len() {
                let idx = call.inline_count as usize;
                call.inline_args[idx] = Some(v);
                call.inline_count += 1;
                return Ok(());
            }
            if call.inline_count > 0 {
                let n = call.inline_count as usize;
                if let Some(buf) = self.call_bufs.args.pop() {
                    install_buffer(&mut call.args, buf);
                }
                call.args.reserve(n + 1);
                for i in 0..n {
                    call.args.push(call.inline_args[i].take());
                }
                call.inline_count = 0;
            } else if call.args.capacity() == 0 {
                if let Some(buf) = self.call_bufs.args.pop() {
                    install_buffer(&mut call.args, buf);
                }
            }
            call.args.push(Some(v));
            return Ok(());
        }
        let pos = Some(arg_idx);
        let by_ref = {
            let magic = call.is_magic_trampoline();
            !magic && call.func.arg_by_ref(arg_idx)
        };
        if by_ref {
            return Err(self.not_passable_by_reference(pos));
        }
        self.push_arg(pos, None, v)
    }

    /// `ZEND_SEND_VAL` / `ZEND_SEND_VAR` of a positional by-value argument
    /// into the pending call's inline buffer, from the dispatch loop's
    /// hot-instruction block (no `SAVE_OPLINE`): a literal, a temporary
    /// (a call result included) or a defined compiled variable, possibly
    /// named by a plain `$cv` place, of a frame without an attached symbol
    /// table, for a call that is not a `__call` trampoline, has had no
    /// named argument and has inline room. That is [`Vm::op_send`]'s own
    /// fast path for these operands, which cannot warn, throw or run code;
    /// returns `false`, having changed nothing, for everything else.
    #[inline]
    pub(crate) fn send_fast(&mut self, src: &ArgSrc) -> bool {
        let Some(frame) = self.frames.last_mut() else {
            return false;
        };
        send_in(frame, src)
    }

    pub(crate) fn op_send(&mut self, src: &ArgSrc, name: Option<ZStr>) -> VmResult<()> {
        // Fast path: a positional by-value argument of an ordinary call
        // (the general path below computes the same thing in more steps).
        if name.is_none() {
            let by_val = {
                let frame = self.frames.last_mut().expect("active frame");
                let call = frame.calls.last_mut().expect("pending call");
                let arg_idx = call.arg_count();
                let by_val = !call.is_magic_trampoline()
                    && !call.has_named
                    && !call.func.arg_by_ref(arg_idx);
                if by_val {
                    if call.args.is_empty() && (call.inline_count as usize) < call.inline_args.len()
                    {
                        let idx = call.inline_count as usize;
                        match src {
                            ArgSrc::Val(Operand::Lit(i)) => {
                                let v = frame.code.literals[*i as usize].clone();
                                call.inline_args[idx] = Some(v);
                                call.inline_count += 1;
                                return Ok(());
                            }
                            ArgSrc::Val(Operand::Tmp(t)) => {
                                let v = match std::mem::take(&mut frame.tmps[*t as usize]) {
                                    Value::Reference(r) => r.get(),
                                    v => v,
                                };
                                call.inline_args[idx] = Some(v);
                                call.inline_count += 1;
                                return Ok(());
                            }
                            ArgSrc::Val(Operand::Cv(cv)) => {
                                if !frame.symtab_attached {
                                    if let Some(val) =
                                        frame.cvs.get(*cv as usize).and_then(crate::vm::CvSlot::get)
                                    {
                                        let v = match val {
                                            Value::Reference(r) => r.get(),
                                            v => v.clone(),
                                        };
                                        call.inline_args[idx] = Some(v);
                                        call.inline_count += 1;
                                        return Ok(());
                                    }
                                }
                            }
                            ArgSrc::Place(pid) => {
                                if let Some(cv) =
                                    frame.code.places.get(*pid as usize).and_then(|p| {
                                        match (&p.base, p.segs.is_empty()) {
                                            (crate::opcode::PlaceBase::Cv(cv), true) => Some(*cv),
                                            _ => None,
                                        }
                                    })
                                {
                                    if !frame.symtab_attached {
                                        if let Some(val) = frame
                                            .cvs
                                            .get(cv as usize)
                                            .and_then(crate::vm::CvSlot::get)
                                        {
                                            let v = match val {
                                                Value::Reference(r) => r.get(),
                                                v => v.clone(),
                                            };
                                            call.inline_args[idx] = Some(v);
                                            call.inline_count += 1;
                                            return Ok(());
                                        }
                                    }
                                }
                            }
                            ArgSrc::CallResult(Operand::Tmp(t)) => {
                                let v = match std::mem::take(&mut frame.tmps[*t as usize]) {
                                    Value::Reference(r) => r.get(),
                                    v => v,
                                };
                                call.inline_args[idx] = Some(v);
                                call.inline_count += 1;
                                return Ok(());
                            }
                            _ => {}
                        }
                    } else {
                        if call.inline_count > 0 {
                            let n = call.inline_count as usize;
                            if let Some(v) = self.call_bufs.args.pop() {
                                install_buffer(&mut call.args, v);
                            }
                            call.args.reserve(n + 1);
                            for i in 0..n {
                                call.args.push(call.inline_args[i].take());
                            }
                            call.inline_count = 0;
                        } else if call.args.capacity() == 0 {
                            if let Some(v) = self.call_bufs.args.pop() {
                                install_buffer(&mut call.args, v);
                            }
                        }
                        match src {
                            ArgSrc::Val(Operand::Lit(i)) => {
                                let v = frame.code.literals[*i as usize].clone();
                                call.args.push(Some(v));
                                return Ok(());
                            }
                            ArgSrc::Val(Operand::Tmp(t)) => {
                                let v = match std::mem::take(&mut frame.tmps[*t as usize]) {
                                    Value::Reference(r) => r.get(),
                                    v => v,
                                };
                                call.args.push(Some(v));
                                return Ok(());
                            }
                            ArgSrc::Val(Operand::Cv(cv)) => {
                                if !frame.symtab_attached {
                                    if let Some(val) =
                                        frame.cvs.get(*cv as usize).and_then(crate::vm::CvSlot::get)
                                    {
                                        let v = match val {
                                            Value::Reference(r) => r.get(),
                                            v => v.clone(),
                                        };
                                        call.args.push(Some(v));
                                        return Ok(());
                                    }
                                }
                            }
                            ArgSrc::Place(pid) => {
                                if let Some(cv) =
                                    frame.code.places.get(*pid as usize).and_then(|p| {
                                        match (&p.base, p.segs.is_empty()) {
                                            (crate::opcode::PlaceBase::Cv(cv), true) => Some(*cv),
                                            _ => None,
                                        }
                                    })
                                {
                                    if !frame.symtab_attached {
                                        if let Some(val) = frame
                                            .cvs
                                            .get(cv as usize)
                                            .and_then(crate::vm::CvSlot::get)
                                        {
                                            let v = match val {
                                                Value::Reference(r) => r.get(),
                                                v => v.clone(),
                                            };
                                            call.args.push(Some(v));
                                            return Ok(());
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                by_val
            };
            if by_val {
                let v = match src {
                    ArgSrc::Val(op) => self.read(*op)?,
                    ArgSrc::CallResult(op) => self.read_raw(*op)?.deref(),
                    // `ZEND_SEND_VAR` of a plain compiled variable (`f($x)`):
                    // read here, without entering the place walk.
                    ArgSrc::Place(pid) => match self.plain_cv_place(*pid) {
                        Some(cv) => self.read_cv(cv)?,
                        None => self.send_place_read(*pid)?,
                    },
                    ArgSrc::TempPlace(pid) => self.send_place_read(*pid)?,
                };
                let frame = self.frames.last_mut().expect("active frame");
                let call = frame.calls.last_mut().expect("pending call");
                if call.args.is_empty() && (call.inline_count as usize) < call.inline_args.len() {
                    let idx = call.inline_count as usize;
                    call.inline_args[idx] = Some(v);
                    call.inline_count += 1;
                } else {
                    if call.inline_count > 0 {
                        let n = call.inline_count as usize;
                        if let Some(buf) = self.call_bufs.args.pop() {
                            install_buffer(&mut call.args, buf);
                        }
                        call.args.reserve(n + 1);
                        for i in 0..n {
                            call.args.push(call.inline_args[i].take());
                        }
                        call.inline_count = 0;
                    }
                    call.args.push(Some(v));
                }
                return Ok(());
            }
        }
        self.pooled_args();
        let pos = match &name {
            Some(n) => self.arg_position(n)?,
            None => Some(self.frame().calls.last().expect("pending call").arg_count()),
        };
        let by_ref = {
            let call = self.frame().calls.last().expect("pending call");
            let magic = call.is_magic_trampoline();
            !magic && pos.is_some_and(|p| call.func.arg_by_ref(p))
                || (!magic && pos.is_none() && call.func.arg_by_ref(call.func.params().len()))
        };
        // ZEND_ARG_SEND_PREFER_REF: by reference when possible, else by
        // value without complaint.
        let prefer_ref = by_ref && {
            let call = self.frame().calls.last().expect("pending call");
            let ps = call.func.params();
            pos.and_then(|p| ps.get(p).or_else(|| ps.last().filter(|l| l.variadic)))
                .is_some_and(|x| x.promote & crate::opcode::PREFER_REF != 0)
        };
        let value = self.send_value(src, pos, by_ref, prefer_ref)?;
        self.push_arg(pos, name, value)
    }

    /// A by-value argument place of the current frame (`ZEND_FETCH_*_R`
    /// or `_FUNC_ARG` for a by-value parameter, then `ZEND_SEND_VAR`): the
    /// specialised fetch ops' hit ([`Frame::send_place_hit`]), else the
    /// place walk with its diagnostics.
    #[inline]
    fn send_place_read(&mut self, pid: crate::opcode::PlaceId) -> VmResult<Value> {
        if let Some(v) = self.frames.last_mut().and_then(|f| f.send_place_hit(pid)) {
            return Ok(v);
        }
        self.place_fetch(pid, FetchMode::Read)
    }

    /// The compiled variable that place `pid` of the current frame names,
    /// when the place is a plain one (`$x`, no dimension or property): what
    /// [`Vm::place_fetch`] reads with [`Vm::read_cv`] in `Read` mode.
    #[inline]
    fn plain_cv_place(&self, pid: crate::opcode::PlaceId) -> Option<u32> {
        let p = self.frame().code.places.get(pid as usize)?;
        match (&p.base, p.segs.is_empty()) {
            (crate::opcode::PlaceBase::Cv(cv), true) => Some(*cv),
            _ => None,
        }
    }
}

#[inline(always)]
#[allow(dead_code)]
fn deref_cv(s: Option<&Value>) -> Option<Value> {
    match s {
        Some(Value::Reference(r)) => Some(r.get()),
        Some(v) => Some(v.clone()),
        None => None,
    }
}

impl Vm {
    /// The value the general path of [`Vm::op_send`] sends for the argument
    /// at `pos`: read by value, or as a reference for a by-reference
    /// parameter (`ZEND_SEND_REF`, `ZEND_SEND_VAR_NO_REF`,
    /// `ZEND_SEND_FUNC_ARG`), with the errors of a value that cannot become
    /// one.
    fn send_value(
        &mut self,
        src: &ArgSrc,
        pos: Option<usize>,
        by_ref: bool,
        prefer_ref: bool,
    ) -> VmResult<Value> {
        Ok(match src {
            ArgSrc::Val(op) => {
                let v = self.read(*op)?;
                if by_ref && !prefer_ref {
                    return Err(self.not_passable_by_reference(pos));
                }
                v
            }
            ArgSrc::CallResult(op) => {
                let v = self.read_raw(*op)?;
                if by_ref {
                    match v {
                        Value::Reference(r) => Value::Reference(r),
                        other if prefer_ref => other,
                        other => {
                            self.notice("Only variables should be passed by reference")?;
                            Value::Reference(PhpRef::new(other))
                        }
                    }
                } else {
                    v.deref()
                }
            }
            ArgSrc::Place(pid) => {
                let globals = {
                    let frame = self.frames.last().ok_or(Throw::Fatal)?;
                    let p = frame.code.places.get(*pid as usize).ok_or(Throw::Fatal)?;
                    matches!(p.base, crate::opcode::PlaceBase::GlobalsArray) && p.segs.is_empty()
                };
                if by_ref && globals && !prefer_ref {
                    return Err(self.not_passable_by_reference(pos));
                }
                if by_ref && !globals {
                    Value::Reference(self.place_make_ref(*pid)?)
                } else {
                    self.place_fetch(*pid, FetchMode::Read)?
                }
            }
            ArgSrc::TempPlace(pid) => {
                if by_ref {
                    // zend_use_tmp_in_write_context_helper
                    return Err(self
                        .make_throw("Error", "Cannot use temporary expression in write context"));
                }
                self.place_fetch(*pid, FetchMode::Read)?
            }
        })
    }

    /// `zend_cannot_pass_by_reference`: the Error for a value sent to the
    /// by-reference parameter at `pos` (`None`: the variadic one) of the
    /// pending call.
    #[cold]
    fn not_passable_by_reference(&mut self, pos: Option<usize>) -> Throw {
        let Some(call) = self.frames.last().and_then(|f| f.calls.last()).cloned() else {
            return Throw::Fatal;
        };
        let p = pos.unwrap_or_else(|| call.func.params().len().saturating_sub(1));
        let pname = call
            .func
            .params()
            .get(p)
            .filter(|p| !p.variadic)
            .map_or_else(String::new, |x| format!(" (${})", x.name));
        let fname = self.func_display(&call.func, call.scope.as_ref());
        self.make_throw(
            "Error",
            &format!(
                "{fname}(): Argument #{}{pname} could not be passed by reference",
                p + 1
            ),
        )
    }

    fn push_arg(&mut self, pos: Option<usize>, name: Option<ZStr>, value: Value) -> VmResult<()> {
        let frame = self.frames.last_mut().expect("active frame");
        let call = frame.calls.last_mut().expect("pending call");
        if call.inline_count > 0 {
            let n = call.inline_count as usize;
            if let Some(buf) = self.call_bufs.args.pop() {
                install_buffer(&mut call.args, buf);
            }
            call.args.reserve(n);
            for i in 0..n {
                call.args.push(call.inline_args[i].take());
            }
            call.inline_count = 0;
        }
        match (pos, name) {
            (Some(p), Some(n)) => {
                call.has_named = true;
                if call.args.len() > p && call.args[p].is_some() {
                    let what = if call
                        .pfa
                        .as_ref()
                        .is_some_and(|x| x.placeholders.get(p).copied().unwrap_or(false))
                    {
                        "placeholder"
                    } else {
                        "argument"
                    };
                    return Err(self.make_throw(
                        "Error",
                        &format!(
                            "Named parameter ${} overwrites previous {what}",
                            n.to_string_lossy()
                        ),
                    ));
                }
                if call.args.len() <= p {
                    call.args.resize(p + 1, None);
                }
                call.args[p] = Some(value);
            }
            (Some(_), None) => {
                if call.has_named {
                    return Err(self.make_throw(
                        "Error",
                        "Cannot use positional argument after named argument",
                    ));
                }
                call.args.push(Some(value));
            }
            (None, Some(n)) => {
                call.has_named = true;
                if call.named.iter().any(|(k, _)| *k == n) {
                    return Err(self.make_throw(
                        "Error",
                        &format!(
                            "Named parameter ${} overwrites previous argument",
                            n.to_string_lossy()
                        ),
                    ));
                }
                call.named.push((n, value));
            }
            (None, None) => call.args.push(Some(value)),
        }
        Ok(())
    }

    /// `ZEND_SEND_UNPACK`: the elements of an array or `Traversable` as
    /// arguments of the pending call.
    pub(crate) fn op_send_unpack(&mut self, src: Operand) -> VmResult<()> {
        let raw = match src {
            Operand::Cv(cv) => match self.cv_get(cv) {
                Some(v) => v,
                None => self.read(src)?,
            },
            _ => self.read_raw(src)?,
        };
        let mut v = raw.deref();
        let items: Vec<(Value, Value)> = match &v {
            Value::Array(a) => a.iter().map(|(k, v)| (k.to_value(), v.clone())).collect(),
            Value::Object(o) if self.instance_of(o, b"traversable") => {
                // Each element is sent as the iterator yields it: its
                // by-reference warning or argument error comes before the
                // iterator moves on, and when an exception ends the walk the
                // iterator, then this operand (`FREE_OP1`, the locals here),
                // then the arguments sent so far (`cleanup_unfinished_calls`)
                // are released.
                let o = o.clone();
                return self.iterate_each(&o, &mut |vm: &mut Vm, key: Value, val: Value| {
                    vm.send_unpacked(key, val, None).map(|_| ())
                });
            }
            _ => {
                // ZEND_SEND_UNPACK raises zend_type_error.
                return Err(self.make_throw(
                    "TypeError",
                    &format!(
                        "Only arrays and Traversables can be unpacked, {} given",
                        given_type(&v)
                    ),
                ));
            }
        };
        let mut changed = false;
        for (k, val) in items {
            changed |= self.send_unpacked(k, val, Some(&mut v))?;
        }
        if changed {
            match raw {
                Value::Reference(r) => {
                    self.assign_ref(&r, v)?;
                }
                _ => {
                    if let Operand::Cv(cv) = src {
                        if let Some((r, value)) = self.cv_assign(cv, v) {
                            self.assign_ref(&r, value)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Sends the element `val` with key `k` of an unpacked array (`array`:
    /// that array, whose element becomes a reference when the parameter
    /// takes one; returns whether it did) or `Traversable` (`array: None`),
    /// as one step of `ZEND_SEND_UNPACK`.
    fn send_unpacked(&mut self, k: Value, val: Value, array: Option<&mut Value>) -> VmResult<bool> {
        let traversable = array.is_none();
        // ZEND_SEND_UNPACK: a Traversable's numeric string keys
        // (ZEND_HANDLE_NUMERIC) are positional (GH-18581).
        let k = match k {
            Value::Str(s) if traversable => match ArrayKey::from_zstr(&s) {
                ArrayKey::Int(i) => Value::Int(i),
                ArrayKey::Str(s) => Value::Str(s),
            },
            k => k,
        };
        let (pos, name) = match &k {
            Value::Str(s) => (self.arg_position(s)?, Some(s.clone())),
            Value::Int(_) => {
                let call = self.frame().calls.last().expect("pending call");
                if call.has_named {
                    return Err(self.make_throw(
                        "Error",
                        "Cannot use positional argument after named argument during unpacking",
                    ));
                }
                (Some(call.arg_count()), None)
            }
            _ => {
                return Err(self.make_throw(
                    "Error",
                    "Keys must be of type int|string during argument unpacking",
                ))
            }
        };
        let (by_ref, n, prefer_ref) = {
            let call = self.frame().calls.last().expect("pending call");
            let params = call.func.params();
            let n = pos.unwrap_or_else(|| params.len().saturating_sub(1));
            let param = params.get(n).or_else(|| params.last());
            (
                !call.is_magic_trampoline() && call.func.arg_by_ref(n),
                n,
                param.is_some_and(|p| p.promote & crate::opcode::PREFER_REF != 0),
            )
        };
        let mut changed = false;
        let val = if by_ref && !traversable {
            let value = match val {
                Value::Reference(r) => Value::Reference(r),
                other => Value::Reference(PhpRef::new(other)),
            };
            if let Some(Value::Array(a)) = array {
                let key = match &k {
                    Value::Str(s) => ArrayKey::Str(s.clone()),
                    Value::Int(i) => ArrayKey::Int(*i),
                    _ => unreachable!("validated unpack key"),
                };
                a.make_mut().insert(key, value.clone());
                changed = true;
            }
            value
        } else if by_ref && !prefer_ref {
            let call = self.frame().calls.last().expect("pending call");
            let fname = self.func_display(&call.func, call.scope.as_ref());
            self.warning(&format!(
                "Cannot pass by-reference argument {} of {fname}() by unpacking a Traversable, passing by-value instead", n + 1
            ))?;
            Value::Reference(PhpRef::new(val.deref()))
        } else {
            val.deref()
        };
        self.push_arg(pos, name, val)?;
        Ok(changed)
    }

    // ------------------------------------------------------------------
    // DO_FCALL
    // ------------------------------------------------------------------

    /// Performs the pending call. Returns `true` when a user frame was
    /// pushed.
    ///
    /// The dispatch loop tries [`Vm::push_simple_ucall`] before it gets
    /// here.
    pub(crate) fn op_do_call(&mut self, dst: Option<u32>) -> VmResult<bool> {
        // `ZEND_FRAMELESS_ICALL_n`: a frameless builtin, called in place.
        if let Some(r) = self.frameless_icall_in_place(dst) {
            let v = r?;
            if let Some(d) = dst {
                self.set_tmp(d, v);
            }
            return Ok(false);
        }
        let frame = self.frames.last_mut().expect("active frame");
        let strict = frame.code.strict_types;
        let mut call = frame.calls.pop().expect("pending call");
        // `ZEND_DO_UCALL`: a user function that is neither deprecated, nor
        // `#[\NoDiscard]`, nor a body-less constructor. The call's parts
        // move into the frame (no copy of the call, no husk to drop).
        if let Func::User(uf) = &call.func {
            if uf.flags & (flags::DEPRECATED | flags::NODISCARD | flags::NO_BODY) == 0 {
                let ret = dst.map_or(RetTarget::Discard, RetTarget::Tmp);
                self.push_user_call(call, ret, strict, false)?;
                return Ok(true);
            }
        }
        // Builtin fast path: plain calls without named arguments that are not
        // `#[\NoDiscard]` and not constructors skip `op_do_call_checked`.
        if let Func::Builtin(b) = &call.func {
            let b = b.clone();
            if !self.observer.enabled {
                if let Some(r) = self.fast_builtin_call(&b, &mut call) {
                    let v = r?;
                    if let Some(d) = dst {
                        self.set_tmp(d, v);
                    }
                    return Ok(false);
                }
                if let Some(r) = self.call_builtin_pooled(&b, &mut call, dst) {
                    let v = r?;
                    release_spent_call(call);
                    if let Some(d) = dst {
                        self.set_tmp(d, v);
                    }
                    return Ok(false);
                }
            }
            if b.flags & flags::NODISCARD == 0 && !call.is_ctor {
                self.pending_call_dst = dst;
                // `fast_builtin_call` was tried just above.
                let v = self.call_builtin_framed(&b, call, strict)?;
                if let Some(d) = dst {
                    self.set_tmp(d, v);
                }
                return Ok(false);
            }
        }
        if dst.is_none() && call.func.flags() & flags::NODISCARD != 0 {
            return self.op_do_call_nodiscard(call, strict);
        }
        self.op_do_call_checked(call, dst, strict)
    }

    /// `ZEND_DO_UCALL` of a plain user function, closure or method
    /// (`i_init_func_execute_data`'s common case), done where everything
    /// lives: the pending call is read in place in the caller's `calls`,
    /// the callee frame is the pooled frame just above the live ones
    /// (php-src's VM stack slot), and the arguments move from the call
    /// straight into the callee's compiled variables. Neither the
    /// `PendingCall` nor the `Frame` is moved (each move is an out-of-line
    /// `memcpy` of 216 or 328 bytes), and the emptied call is not passed
    /// to its drop glue (five out-of-line `drop_in_place` calls, ~140
    /// instructions per call, for fields that hold nothing by then).
    ///
    /// The call qualifies when the general path would take the fast branch
    /// of [`Vm::push_user_call`] anyway: a user function that is neither
    /// deprecated, `#[\NoDiscard]`, body-less nor a generator, without
    /// static variables, foreach or finally slots, called with positional
    /// arguments only (all in the inline buffer, at least the required
    /// ones, none spread or by reference, each accepted by its declared
    /// type as it is), not a partial application nor a `__call`
    /// trampoline (a constructor called by `new` qualifies: the frame
    /// keeps `is_ctor` for unwinding, as the general path's does), with
    /// no observer and a pooled frame ready. It then does exactly what that branch does, in the same order; a
    /// closure's bound variables are bound afterwards as the general path
    /// binds them. `caller_ip` is stored as the caller's resume point
    /// (`SAVE_OPLINE`) when the call is made. Returns `false`, having
    /// changed nothing, otherwise.
    ///
    /// A pooled frame that last ran the same function still holds its
    /// handles ([`Frame::clear_for_reuse`] keeps them): they are not
    /// replaced.
    ///
    /// Out of line on purpose: inlined into a dispatch loop (`Vm::run`'s
    /// fast `DoCall` arm, or `exec::tape::run_tape`, which then goes on in
    /// the callee's frame itself), whose registers are taken by the loop's
    /// own state, every counter and pointer here lived in a stack slot (the
    /// per-argument loops cost ~13 and ~17 instructions an argument); a
    /// function of its own keeps them in registers for the price of one
    /// call.
    #[inline(never)]
    #[allow(clippy::same_item_push)] // `resize` is an out-of-line call (see the argument buffer)
    pub(crate) fn push_simple_ucall(&mut self, dst: Option<u32>, caller_ip: usize) -> bool {
        if self.observer.enabled {
            return false;
        }
        let live = self.frames.live;
        // `push_user_call`'s stack-depth backstop is the general path's.
        if live == 0 || live > 50_000 {
            return false;
        }
        let (spare_args, closure) = {
            let (live_frames, pool) = self.frames.vec.split_at_mut(live);
            let (Some(caller), Some(frame)) = (live_frames.last_mut(), pool.first_mut()) else {
                return false;
            };
            let caller_strict = caller.code.strict_types;
            let pending = caller.calls.len();
            let Some(call) = caller.calls.last_mut() else {
                return false;
            };
            let Func::User(uf) = &call.func else {
                return false;
            };
            let n = call.inline_count as usize;
            if !simple_ucall(uf, call, n) {
                return false;
            }
            debug_assert!(call.inline_args[n..].iter().all(Option::is_none));
            let code = &uf.code;
            let same_func = frame.ufunc.as_ref().is_some_and(|f| Rc::ptr_eq(f, uf));
            // `FrameStack::push_in_place`.
            if frame.cvs.len() == code.cv_names.len()
                && frame.tmps.len() == code.num_tmps as usize
                && frame.iters.len() == code.num_iters as usize
                && frame.fins.len() == code.num_finally as usize
            {
                frame.reinit_same_call(code, FrameKind::Function);
            } else {
                frame.reinit_for_call(Rc::clone(code), FrameKind::Function);
            }
            set_opt(&mut frame.this, call.this.take());
            set_opt(&mut frame.scope, call.scope.take());
            set_opt(&mut frame.called, call.called.take());
            frame.ret = dst.map_or(RetTarget::Discard, RetTarget::Tmp);
            frame.caller_strict = caller_strict;
            frame.called_from_internal = false;
            // `new`'s constructor call (`ZEND_NEW` pushes it like any
            // other): the frame only remembers it for unwinding
            // (`zend_object_store_ctor_failed`).
            frame.is_ctor = call.is_ctor;
            // The passed-argument count (`ZEND_CALL_NUM_ARGS`): one empty
            // entry per argument, each moved into its compiled variable.
            // The recycled buffer is empty (`Frame::clear_for_reuse`).
            if !frame.args.is_empty() {
                frame.args.clear();
            }
            // A `push` per argument (an inline capacity test and two
            // stores; the buffer only grows on a first call this deep):
            // `extend` of a mapped range compiled to an out-of-line
            // `Vec::extend_trusted` call of ~30 instructions per call.
            //
            // A variadic function's arguments past its declared parameters
            // stay in the buffer for its `RECV_VARIADIC` (`push_user_call`'s
            // layout); every other argument has its parameter (`simple_ucall`).
            let bound = if uf.flags & flags::VARIADIC == 0 {
                n
            } else {
                n.min(uf.params.len().saturating_sub(1))
            };
            for _ in 0..bound {
                frame.args.push(None);
            }
            for (slot, arg) in frame.cvs.iter_mut().zip(&mut call.inline_args[..bound]) {
                // The slot is empty: nothing to release.
                if let Some(old) = slot.set(arg.take()) {
                    drop(old);
                }
            }
            if bound < n {
                variadic_extras(&mut frame.args, &mut call.inline_args[bound..n]);
            }
            // `RECV` of an argument that was passed and needs no coercion
            // has nothing left to do: start after it (after a `RECV_INIT`'s
            // default-value code). Worked out once per argument count with
            // the call shape (`simple_ucall` read it), not walked per call.
            frame.ip = uf.plain_first_op(n);
            call.inline_count = 0;
            let spare = if call.args.capacity() > 0 {
                Some(std::mem::take(&mut call.args))
            } else {
                None
            };
            // Empty (`simple_ucall`), but it may own a buffer.
            if call.named.capacity() > 0 {
                drop(std::mem::take(&mut call.named));
            }
            let closure = call.closure.take();
            caller.ip = caller_ip;
            // What is left of the call owns nothing but its function
            // handle: `this`, `scope`, `called`, `closure`, the argument
            // buffer and the named arguments were taken above, the inline
            // arguments `..n` moved into the frame and those after `n` are
            // `None` (the inline buffer is filled in order: the
            // `debug_assert` above, and what `gc::traverse` relies on),
            // `magic_name` and `pfa` are `None` (`simple_ucall`), and the
            // other fields are plain data.
            //
            // SAFETY: `pending - 1` is the index of `call` (the last
            // element, `pending >= 1` as `last_mut` found it). Its `func` is
            // read out once and the vector's length is then cut below the
            // element, so the element is never dropped or read again and
            // the handle is owned by `func` alone; nothing between the read
            // and the length store can panic. Forgetting the rest of the
            // element leaks nothing (see above).
            let func = unsafe { std::ptr::read(&call.func) };
            // SAFETY: as above: `call` was the last element and its handle
            // was read out, so the shorter vector never drops it again.
            unsafe { caller.calls.set_len(pending - 1) };
            if same_func {
                // The frame holds this function already: the call's handle
                // is released by an inline reference-count decrement (the
                // frame's `ufunc` holds another, so it is never the last),
                // where dropping the `Func` was an out-of-line
                // `drop_in_place::<Func>` call per call.
                if let Func::User(uf) = func {
                    drop(uf);
                }
            } else {
                let ufunc = match &func {
                    Func::User(uf) => Some(Rc::clone(uf)),
                    Func::Builtin(_) => None,
                };
                set_opt(&mut frame.ufunc, ufunc);
                set_opt(&mut frame.func, Some(func));
            }
            (spare, closure)
        };
        self.frames.live = live + 1;
        if let Some(args) = spare_args {
            self.put_arg_vec(args);
        }
        if let Some(c) = closure {
            self.bind_closure_vars(live, &c);
            self.frames.vec[live].closure = Some(c);
        }
        true
    }

    /// `ZEND_RETURN` from a user function frame on the common path
    /// (`zend_leave_helper` for a plain `ZEND_CALL_TOP_FUNCTION`-less call):
    /// the value is read and checked against the return type in place, the
    /// frame is released as [`Vm::op_return_raw`] releases it and the value
    /// is stored into the caller's temporary, with one lookup of the frame
    /// instead of one per step and without the out-of-line read and
    /// return-type calls.
    ///
    /// It applies to a function frame that is not the base frame of this
    /// `run`, not the main script's, not a generator's, has no symbol table
    /// attached and does not return by reference, with no observer, when
    /// the operand is a defined compiled variable, a temporary not holding
    /// a reference, a literal, or none (`return;`, the implicit return of
    /// a body, without a return type or with `void`), and the value is accepted by the return
    /// type as it is (`verify_return`'s fast path). The result is then the
    /// one `op_return` gives: the same checks, the same release order
    /// ([`Frame::release_returning`]: the possible roots, then the release) and a
    /// dereferenced value (`copy_on_assign` has nothing left to do).
    /// `ip` is the instruction after the `RETURN`, stored as the frame's
    /// resume point before the release (`SAVE_OPLINE`). Returns `None`,
    /// having changed nothing, otherwise.
    ///
    /// Out of line for the reason [`Vm::push_simple_ucall`] is; called by
    /// `Vm::run`'s fast `Return` arm and by `exec::tape::run_tape`.
    ///
    /// Only the shape the call workloads return through — an untyped
    /// user function returning a scalar, null, a string or such a literal
    /// into a caller temporary holding a scalar — runs here, small and without spilled registers;
    /// anything else goes to [`Vm::return_generic`], which repeats the
    /// tests for it (the operand shape is dispatched before this
    /// function's prologue, so the common fallbacks do not pay it twice).
    #[inline(never)]
    pub(crate) fn return_simple(
        &mut self,
        val: Operand,
        base: usize,
        ip: usize,
    ) -> Option<crate::exec::Flow> {
        if !matches!(
            val,
            Operand::Cv(_) | Operand::Tmp(_) | Operand::Lit(_) | Operand::Unused
        ) {
            return self.return_generic::<false>(val, base, ip);
        }
        if self.observer.enabled {
            return None;
        }
        let fidx = self.frames.live.checked_sub(1)?;
        if fidx <= base {
            return None;
        }
        {
            let (callers, rest) = self.frames.vec.split_at_mut(fidx);
            let caller = callers.last_mut()?;
            let frame = rest.first_mut()?;
            if frame.kind != FrameKind::Function
                || frame.symtab_attached
                || frame.generator.is_some()
                || Rc::ptr_eq(&frame.vars, &self.globals)
            {
                return None;
            }
            let uf = frame.ufunc.as_ref()?;
            if uf.flags & (flags::RETURN_REF | flags::GENERATOR) != 0 {
                return None;
            }
            let rt = uf.return_type.as_ref();
            // A value its declared return type does not take as it is
            // (`return_accepts`) is left to `return_generic`: coercion
            // rules may rewrite it (int 3 returned from a `: string`
            // function gives "3"), or it fails. So are references, arrays and objects (copied, not moved,
            // for `gc::frame_released`), and a caller's result slot that
            // holds anything but a scalar (released after the frame).
            // Every test comes before the value moves out of its slot:
            // `return_generic` reads it there again.
            let dst = match frame.ret {
                RetTarget::Tmp(t) => match caller.tmps.get_mut(t as usize) {
                    Some(slot) if is_scalar(slot) => Some(slot),
                    _ => return self.return_generic::<false>(val, base, ip),
                },
                RetTarget::Discard => None,
            };
            // A scalar, null or a string: none is a possible cycle root,
            // so it moves out (a literal is copied) without changing what
            // `gc::frame_released` records. With a declared return type,
            // only a value the type takes as it is (`return_accepts`).
            // The returned value: in place in the frame's variable or
            // temporary (`Ok`, moved), or a literal's copy (`Err`).
            let src: Result<&mut Value, Value> = match val {
                Operand::Cv(cv) => match frame.cvs.get_mut(cv as usize) {
                    Some(slot)
                        if slot.get().is_some_and(|v| {
                            returns_directly(v) && (rt.is_none() || return_accepts(rt, v))
                        }) =>
                    {
                        Ok(slot.as_mut().as_mut()?)
                    }
                    _ => return self.return_generic::<false>(val, base, ip),
                },
                Operand::Tmp(t) => match frame.tmps.get_mut(t as usize) {
                    Some(slot)
                        if returns_directly(slot) && (rt.is_none() || return_accepts(rt, slot)) =>
                    {
                        Ok(slot)
                    }
                    _ => return self.return_generic::<false>(val, base, ip),
                },
                Operand::Lit(l) => match frame.code.literals.get(l as usize) {
                    Some(lit)
                        if returns_directly(lit) && (rt.is_none() || return_accepts(rt, lit)) =>
                    {
                        Err(clone_value(lit))
                    }
                    _ => return self.return_generic::<false>(val, base, ip),
                },
                // `return;` and the implicit return at the end of a body (a
                // constructor's, a procedure's): null, for no return type
                // or `void` (`verify_return(None)`; any other type goes on
                // to its diagnostics in `return_generic`).
                Operand::Unused if rt.is_none_or(crate::types::TypeDecl::is_void) => {
                    Err(Value::Null)
                }
                _ => return self.return_generic::<false>(val, base, ip),
            };
            // `SAVE_OPLINE()`: the release below may report this line.
            frame.ip = ip;
            // The store runs before the release: the value's register is
            // then dead across it (the possible-root walk only reads the
            // frame and the caller's `$this`). The caller's slot held a
            // scalar, which owns nothing: a moved value trades places with
            // it (one 16-byte swap, where the move out and the store in
            // went through a stack temporary byte group by byte group),
            // and the scalar left in the frame's slot is released with the
            // frame (`release_cv` / `release_slot`: forgotten). A
            // discarded string is released.
            match (dst, src) {
                (Some(slot), Ok(v)) => std::mem::swap(slot, v),
                (Some(slot), Err(v)) => std::mem::forget(std::mem::replace(slot, v)),
                (None, src) => {
                    let v = match src {
                        Ok(v) => std::mem::take(v),
                        Err(v) => v,
                    };
                    if matches!(v, Value::Str(_)) {
                        drop(v);
                    } else {
                        std::mem::forget(v);
                    }
                }
            }
            // `zend_leave_helper`: the possible roots of the frame's
            // variables and call handle, then `FrameStack::release_top` of
            // the frame at hand, in one step and in place (no out-of-line
            // call, no second lookup of the top frame); the stack is
            // popped just below.
            frame.release_returning(caller.this.as_ref(), &self.empty_code);
        };
        self.frames.pop_cleared_top();
        Some(crate::exec::Flow::Reload)
    }

    /// `ZEND_RETURN` from the frame a nested execution started with
    /// (`base`: a callback called by a builtin, `zend_call_function`):
    /// [`Vm::return_generic`]'s direct return, whose result is the
    /// execution's (`Flow::Done`) instead of going to a caller's slot.
    /// `None`, having changed nothing, for any frame or value it leaves to
    /// the generic `Return`.
    pub(crate) fn return_at_base(
        &mut self,
        val: Operand,
        base: usize,
        ip: usize,
    ) -> Option<crate::exec::Flow> {
        self.return_generic::<true>(val, base, ip)
    }

    /// The generic `ZEND_RETURN` of [`Vm::return_simple`]: any frame or
    /// result shape the direct path leaves, with the value's slot order
    /// (`set_tmp` releasing the result slot after the frame) it keeps.
    /// With `AT_BASE`, of the frame `base` itself ([`Vm::return_at_base`]);
    /// otherwise of a frame above it.
    #[inline(never)]
    #[allow(clippy::too_many_lines)] // the direct path's shape, in full
    fn return_generic<const AT_BASE: bool>(
        &mut self,
        val: Operand,
        base: usize,
        ip: usize,
    ) -> Option<crate::exec::Flow> {
        // A parameter of the type, so the ordinary return compiles to the
        // tests it had before (it is every call's return).
        let at_base = AT_BASE;
        if self.observer.enabled {
            return None;
        }
        let fidx = self.frames.live.checked_sub(1)?;
        if fidx < base || (fidx == base) != at_base || fidx == 0 {
            return None;
        }
        let (v, ret) = {
            let (callers, rest) = self.frames.vec.split_at_mut(fidx);
            let caller = callers.last_mut()?;
            let frame = rest.first_mut()?;
            if frame.kind != FrameKind::Function
                || frame.symtab_attached
                || frame.generator.is_some()
                || Rc::ptr_eq(&frame.vars, &self.globals)
            {
                return None;
            }
            let uf = frame.ufunc.as_ref()?;
            if uf.flags & (flags::RETURN_REF | flags::GENERATOR) != 0 {
                return None;
            }
            let rt = uf.return_type.as_ref();
            // `return_accepts` is inline: the closure it replaces was an
            // out-of-line call per return.
            let v = match val {
                Operand::Cv(cv) => {
                    let slot = frame.cvs.get_mut(cv as usize)?.as_mut();
                    match slot.as_ref()? {
                        Value::Reference(r) => {
                            let v = r.get();
                            if !return_accepts(rt, &v) {
                                return None;
                            }
                            v
                        }
                        v => {
                            if !return_accepts(rt, v) {
                                return None;
                            }
                            // The frame is released below and nothing else
                            // reads its variables (no symbol table is
                            // attached): a value that cannot be a garbage
                            // collection root moves out of its slot instead
                            // of being copied and then released with the
                            // frame (a reference count up and down for a
                            // string). Arrays and objects are copied, so
                            // that `gc::frame_released` still buffers them
                            // as possible roots, as `zend_leave_helper`'s
                            // release of the variables does.
                            if matches!(v, Value::Array(_) | Value::Object(_)) {
                                clone_value(v)
                            } else {
                                slot.take()?
                            }
                        }
                    }
                }
                Operand::Tmp(t) => {
                    let slot = frame.tmps.get_mut(t as usize)?;
                    if matches!(slot, Value::Reference(_)) || !return_accepts(rt, slot) {
                        return None;
                    }
                    // An unused result whose release may run a destructor
                    // is left to `Vm::op_return_raw`, which runs it in this
                    // frame and rethrows what it throws in the caller
                    // (`ZEND_RETURN`'s release of the temporary).
                    if !at_base && frame.ret == RetTarget::Discard && may_destruct(slot) {
                        return None;
                    }
                    std::mem::take(slot)
                }
                // A literal is shared with the op array: its release here
                // frees nothing.
                Operand::Lit(i) => {
                    let lit = frame.code.literals.get(i as usize)?;
                    if matches!(lit, Value::Reference(_)) || !return_accepts(rt, lit) {
                        return None;
                    }
                    clone_value(lit)
                }
                // `return;` and the implicit return at the end of a body:
                // `verify_return(None)` gives null for no return type and
                // for `void`, and goes on to its diagnostics otherwise.
                Operand::Unused => {
                    if !rt.is_none_or(crate::types::TypeDecl::is_void) {
                        return None;
                    }
                    Value::Null
                }
                Operand::TmpKeep(_) => return None,
            };
            // `SAVE_OPLINE()`: the release below may report this line.
            frame.ip = ip;
            let ret = frame.ret;
            // `ZEND_RETURN` releases an unused result before the frame is
            // left (`EX(return_value)` is NULL). What arrives here cannot
            // run a destructor (`may_destruct`; a compiled variable's value
            // is a copy, released with the frame's variables); a scalar
            // owns nothing, so the out-of-line drop glue is skipped.
            let v = match ret {
                RetTarget::Discard if !at_base => {
                    if is_scalar(&v) {
                        std::mem::forget(v);
                    } else {
                        drop(v);
                    }
                    None
                }
                _ => Some(v),
            };
            // `ZEND_RETURN` copies the value into the caller's result slot
            // (`EX(return_value)`) before `zend_leave_helper` releases the
            // frame: done here, through the caller already at hand, when
            // the slot holds a scalar (its release has no effect). Another
            // value in the slot is released after the frame, by `set_tmp`
            // below, as before. The possible-root walk of the release
            // reads only the frame and the caller's `$this`, so it may
            // follow the store.
            let v = match (ret, v) {
                (_, None) => None,
                // The execution's result: no caller slot.
                (_, v) if at_base => v,
                (RetTarget::Tmp(t), Some(v)) => match caller.tmps.get_mut(t as usize) {
                    Some(slot) if is_scalar(slot) => {
                        std::mem::forget(std::mem::replace(slot, v));
                        None
                    }
                    _ => Some(v),
                },
                (RetTarget::Discard, v) => v,
            };
            // `zend_leave_helper`: the possible roots, then
            // `FrameStack::release_top` of the frame at hand, in one step
            // and in place (no out-of-line call, no second lookup of the
            // top frame); the stack is popped just below. Same release,
            // same order (the frame's values, then the result's slot).
            frame.release_returning(caller.this.as_ref(), &self.empty_code);
            (v, ret)
        };
        self.frames.pop_cleared_top();
        let Some(v) = v else {
            return Some(crate::exec::Flow::Reload);
        };
        if at_base {
            return Some(crate::exec::Flow::Done(v));
        }
        // An unused result was released above, in the frame.
        if let RetTarget::Tmp(t) = ret {
            self.set_tmp(t, v);
        }
        Some(crate::exec::Flow::Reload)
    }

    /// `ZEND_DO_FCALL` of a `#[\NoDiscard]` function whose result is
    /// unused: `zend_deprecated_function` first, then
    /// `zend_nodiscard_function`, both before the call.
    #[cold]
    #[inline(never)]
    fn op_do_call_nodiscard(&mut self, call: PendingCall, strict: bool) -> VmResult<bool> {
        let deprecated = call.func.flags() & flags::DEPRECATED != 0;
        if deprecated {
            self.check_deprecated_call(&call)?;
        }
        self.check_nodiscard_call(&call)?;
        if deprecated && matches!(call.func, Func::Builtin(_)) {
            // Already reported: `call_builtin_impl` must not repeat it.
            self.builtin_deprecation_reported = true;
        }
        let r = self.op_do_call_checked(call, None, strict);
        self.builtin_deprecation_reported = false;
        r
    }

    /// Avoids native argument snapshots only for the float-key lifetime case.
    pub(crate) fn op_array_key_exists(&mut self, src: &ArgSrc, dst: Option<u32>) -> VmResult<bool> {
        let key = {
            let call = self.frame().calls.last().expect("pending call");
            let first_arg = if call.inline_count == 1 && call.args.is_empty() {
                call.inline_args[0].as_ref()
            } else if call.inline_count == 0 && call.args.len() == 1 {
                call.args[0].as_ref()
            } else {
                None
            };
            match (&call.func, first_arg) {
                (Func::Builtin(b), Some(Value::Float(f)))
                    if b.name.as_bytes() == b"array_key_exists"
                        && b.flags & (flags::DEPRECATED | flags::NODISCARD) == 0
                        && (!f.is_finite() || !php_runtime::convert::double_fits_long(*f)) =>
                {
                    Some(*f)
                }
                _ => None,
            }
        };
        let array = key.and_then(|_| {
            let ArgSrc::Place(pid) = src else {
                return None;
            };
            let p = &self.frame().code.places[*pid as usize];
            let crate::opcode::PlaceBase::Cv(cv) = &p.base else {
                return None;
            };
            if !p.segs.is_empty() {
                return None;
            }
            self.cv_get(*cv).map(|v| v.deref())
        });
        if let (Some(f), Some(Value::Array(a))) = (key, array) {
            let mut call = self.frame_mut().calls.pop().expect("pending call");
            for slot in &mut call.inline_args[..call.inline_count as usize] {
                *slot = None;
            }
            call.inline_count = 0;
            self.put_arg_vec(std::mem::take(&mut call.args));
            // This snapshot is Zend's temporary addref, not a surviving owner.
            let had_owner = a.refcount() > 1;
            let found =
                php_stdarr::basic::array_key_exists(self, "array_key_exists", &Value::Float(f), &a)
                    .map_err(|e| self.rt_throw(e))?;
            let lost_owner = had_owner && a.refcount() == 1;
            if let Some(d) = dst {
                self.set_tmp(d, Value::Bool(!lost_owner && found));
            }
            return Ok(false);
        }
        self.op_send(src, None)?;
        self.op_do_call(dst)
    }

    fn op_do_call_checked(
        &mut self,
        call: PendingCall,
        dst: Option<u32>,
        strict: bool,
    ) -> VmResult<bool> {
        if call.is_ctor && call.func.flags() & flags::NO_BODY != 0 {
            return Ok(false);
        }
        let skip_deprecation = dst.is_none() && call.func.flags() & flags::NODISCARD != 0;
        match &call.func {
            Func::User(uf) => {
                if !skip_deprecation && uf.flags & flags::DEPRECATED != 0 {
                    self.check_deprecated_call(&call)?;
                }
                let ret = dst.map_or(RetTarget::Discard, RetTarget::Tmp);
                self.push_user_call(call, ret, strict, false)?;
                Ok(true)
            }
            Func::Builtin(b) => {
                let b = b.clone();
                self.pending_call_dst = dst;
                let v = self.call_builtin(&b, call, strict)?;
                if let Some(d) = dst {
                    self.set_tmp(d, v);
                }
                Ok(false)
            }
        }
    }

    /// `zend_nodiscard_function`.
    pub(crate) fn check_nodiscard_call(&mut self, call: &PendingCall) -> VmResult<()> {
        let (attrs, code) = match &call.func {
            Func::User(uf) => (uf.attributes.clone(), crate::errors::level::USER_WARNING),
            Func::Builtin(b) => {
                // Stub attributes (`zend_add_function_attribute`), keyed
                // like `builtin_key`: lowercase `name` or `class::method`.
                let name = b.name.as_bytes().to_ascii_lowercase();
                let key = match &call.scope {
                    Some(sc) => {
                        let mut k = sc.lc_name.as_bytes().to_vec();
                        k.extend_from_slice(b"::");
                        k.extend_from_slice(&name);
                        ZStr::from(k)
                    }
                    None => ZStr::from(name),
                };
                let attrs = self
                    .builtin_attributes
                    .get(&key)
                    .map(|v| (**v).clone())
                    .unwrap_or_default();
                (attrs, crate::errors::level::WARNING)
            }
        };
        let mut suffix = Vec::new();
        if let Some(info) = attrs
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(b"NoDiscard"))
        {
            if !info.args.is_empty() {
                if let Some(cls) = self.find_class(b"NoDiscard") {
                    let info = info.clone();
                    let o = self.attribute_object(&info, &cls, call.scope.as_ref())?;
                    if let Ok(Value::Str(m)) = self.get_property(&o, "message") {
                        if !m.is_empty() {
                            suffix.extend_from_slice(b", ");
                            suffix.extend_from_slice(m.as_bytes());
                        }
                    }
                }
            }
        }
        let name = match (&call.magic_name, &call.scope) {
            // The __call/__callStatic trampoline carries the called name.
            (Some(m), Some(sc)) => {
                format!(
                    "{}::{}",
                    crate::object::display_name(&sc.name),
                    m.to_string_lossy()
                )
            }
            _ => self.func_display(&call.func, call.scope.as_ref()),
        };
        let kind = if call.scope.is_some() {
            "method"
        } else {
            "function"
        };
        let mut msg = format!(
            "The return value of {kind} {name}() should either be used or intentionally ignored by casting it as (void)"
        )
        .into_bytes();
        msg.extend_from_slice(&suffix);
        self.error_bytes(code, &msg)
    }

    /// `get_deprecation_suffix_from_attribute()` instantiates the
    /// `#[\Deprecated]` attribute of an internal function too; the suffix
    /// of a builtin is precomputed, so its constructor call is only made
    /// where it is observable (observers).
    #[cold]
    #[inline(never)]
    fn observe_deprecated_attribute(&mut self) -> VmResult<()> {
        if let Some(cls) = self.find_class(b"Deprecated") {
            self.construct_with(&cls, Vec::new(), Vec::new())?;
        }
        Ok(())
    }

    fn check_deprecated_call(&mut self, call: &PendingCall) -> VmResult<()> {
        if call.func.flags() & flags::DEPRECATED != 0 {
            // `zend_deprecated_function`.
            let (suffix, scope, code) = match &call.func {
                Func::User(uf) => {
                    let scope = call.scope.clone();
                    let suffix = self.deprecation_suffix(&uf.attributes, scope.as_ref())?;
                    (suffix, scope, crate::errors::level::USER_DEPRECATED)
                }
                Func::Builtin(b) => {
                    let suffix = self
                        .builtin_deprecations
                        .get(&ZStr::from(b.name.to_ascii_lowercase()))
                        .cloned()
                        .unwrap_or_default();
                    if !suffix.is_empty() && self.observer.enabled {
                        self.observe_deprecated_attribute()?;
                    }
                    (suffix, call.scope.clone(), crate::errors::level::DEPRECATED)
                }
            };
            let name = match (&call.magic_name, &scope) {
                // The __call/__callStatic trampoline carries the called name.
                (Some(m), Some(sc)) => {
                    format!(
                        "{}::{}",
                        crate::object::display_name(&sc.name),
                        m.to_string_lossy()
                    )
                }
                _ => self.func_display(&call.func, scope.as_ref()),
            };
            let kind = if call.scope.is_some() {
                "Method"
            } else {
                "Function"
            };
            let mut msg = format!("{kind} {name}() is deprecated").into_bytes();
            msg.extend_from_slice(&suffix);
            self.error_bytes(code, &msg)?;
        }
        Ok(())
    }

    /// Pushes the frame of a user function call (`ZEND_DO_UCALL`:
    /// `zend_vm_stack_push_call_frame` + `i_init_func_execute_data`).
    ///
    /// The pending call's parts move into the frame, which is set up where
    /// it lives on the frame stack: a `Frame` is a few hundred bytes and
    /// every move of one is a `memcpy`, so a pooled frame is copied once
    /// (pool to stack, [`move_last_frame`]) and initialised in place, and
    /// the arguments move straight into their compiled variables
    /// ([`init_cvs`]), as php-src's SEND ops store them in the callee frame.
    /// `uf` is the user function `call.func` holds.
    pub(crate) fn push_user_call(
        &mut self,
        mut call: PendingCall,
        ret: RetTarget,
        caller_strict: bool,
        from_internal: bool,
    ) -> VmResult<()> {
        if self.frames.len() > 50_000 {
            // A backstop like `execute`'s nesting limit: php-src would have
            // hit its native stack limit by now, so the error is
            // `zend_call_stack_size_error()`'s. The error object is created
            // before the arguments are released (object handle reuse is
            // observable); the rest of the call goes with it, in order.
            let e = crate::builtins::dump::stack_size_error(self);
            self.put_arg_vec(std::mem::take(&mut call.args));
            return Err(e);
        }
        let PendingCall {
            func,
            this,
            scope,
            called,
            mut args,
            named,
            closure,
            magic_name,
            is_ctor,
            mut inline_args,
            inline_count,
            ..
        } = call;
        let Func::User(uf) = func else { unreachable!() };

        let passed_argc = if args.is_empty() {
            inline_count as usize
        } else {
            0
        };

        // `uf.statics` is not looked at (see `simple_ucall`): without
        // static variables the call never reads it, and a cached `static`
        // closure object sits in it.
        if !uf.is_generator()
            && uf.code.num_statics == 0
            && uf.code.num_finally == 0
            && named.is_empty()
            && !self.has_observers()
            && magic_name.is_none()
            && args.is_empty()
            && passed_argc >= uf.required as usize
            && passed_argc <= uf.params.len()
            && uf.params.iter().all(|p| !p.variadic && !p.by_ref)
            && uf.params[..passed_argc]
                .iter()
                .enumerate()
                .all(|(i, p)| match &p.ty {
                    None => true,
                    Some(ty) => inline_args[i]
                        .as_ref()
                        .is_some_and(|v| param_accepts(ty, v)),
                })
        {
            if args.capacity() > 0 {
                self.put_arg_vec(args);
            }
            let idx = self.frames.push_in_place(&uf.code, FrameKind::Function);
            let f = &mut self.frames[idx];
            f.this = this;
            f.scope = scope;
            f.called = called;
            f.ret = ret;
            f.caller_strict = caller_strict;
            f.called_from_internal = from_internal;
            f.is_ctor = is_ctor;

            f.args.clear();
            f.args.resize(passed_argc, None);

            for i in 0..passed_argc {
                if let Some(slot) = f.cvs.get_mut(i) {
                    slot.set(inline_args[i].take());
                }
            }

            let mut first_op = 0;
            for i in 0..passed_argc {
                match uf.code.ops.get(first_op) {
                    Some(Op::Recv { n, .. }) if *n as usize == i => first_op += 1,
                    Some(Op::RecvInit { n, skip, .. }) if *n as usize == i => {
                        first_op = *skip as usize;
                    }
                    _ => break,
                }
            }
            f.ip = first_op;
            f.func = Some(Func::User(uf.clone()));
            f.ufunc = Some(uf);

            // A closure (e.g. an arrow function passed to `array_map`)
            // binds its captured variables after the arguments, as below.
            if let Some(c) = closure {
                self.bind_closure_vars(idx, &c);
                self.frames[idx].closure = Some(c);
            }
            return Ok(());
        }

        let is_generator = uf.is_generator();
        let idx = self.frames.push_in_place(&uf.code, FrameKind::Function);
        let spare_args = {
            let f = &mut self.frames[idx];
            f.this = this;
            f.scope = scope;
            f.called = called;
            f.ret = ret;
            f.caller_strict = caller_strict;
            f.called_from_internal = from_internal;
            f.is_ctor = is_ctor;
            f.magic_name = magic_name.clone();
            let spare = if inline_count > 0 {
                let n = inline_count as usize;
                f.args.clear();
                f.args.resize(n, None);
                let ncv = f.code.cv_names.len();
                let ops = &f.code.ops;
                let mut skipping = true;
                let mut first_op = 0;
                let mut bound_count = 0;
                for (i, p) in uf.params.iter().enumerate().take(n) {
                    if p.variadic || i == ncv {
                        break;
                    }
                    if let Some(v) = inline_args[i].take() {
                        let ok = match &p.ty {
                            None => true,
                            Some(ty) => ty.is_mixed() || accepts_without_coercion(ty.mask, &v),
                        };
                        if let Some(slot) = f.cvs.get_mut(i) {
                            slot.set(Some(v));
                        }
                        bound_count = i + 1;
                        if skipping && ok {
                            match ops.get(first_op) {
                                Some(Op::Recv { n, .. }) if *n as usize == i => first_op += 1,
                                Some(Op::RecvInit { n, skip, .. }) if *n as usize == i => {
                                    first_op = *skip as usize;
                                }
                                _ => skipping = false,
                            }
                        } else {
                            skipping = false;
                        }
                    } else {
                        skipping = false;
                    }
                }
                for j in bound_count..n {
                    f.args[j] = inline_args[j].take();
                }
                f.ip = first_op;
                None
            } else {
                let spare = match magic_name {
                    Some(name) if !uf.is_closure() => {
                        // __call($name, $args). `ZEND_CALL_TRAMPOLINE` packs the
                        // arguments as sent: `zend_call_function` does not
                        // unwrap references for a trampoline (bug #50394), the
                        // VM's sends already passed values.
                        let mut arr = HashTable::new();
                        for a in args.drain(..).flatten() {
                            let _ = arr.push(a);
                        }
                        for (k, v) in named {
                            arr.insert(ArrayKey::Str(k), v);
                        }
                        f.args.clear();
                        f.args.push(Some(Value::Str(name)));
                        f.args.push(Some(Value::Array(ZArray::from_table(arr))));
                        args
                    }
                    _ => {
                        if !named.is_empty() {
                            f.named = named;
                        }
                        // The frame takes the call's argument buffer (the
                        // extra arguments stay in it); its own goes back to
                        // the pool.
                        std::mem::replace(&mut f.args, args)
                    }
                };
                f.ip = init_cvs(&uf, f);
                Some(spare)
            };
            f.func = Some(Func::User(uf.clone()));
            f.ufunc = Some(uf);
            spare
        };
        if let Some(spare) = spare_args {
            self.put_arg_vec(spare);
        }
        // A pending `memory_limit` violation is raised as a `__call` /
        // `__callStatic` trampoline is entered (`ZEND_VM_ENTER`): the
        // backtrace shows the trampoline (GHSA-8mjj-2r7p-4hg2).
        if php_memlimit::is_pending() && self.frames[idx].magic_name.is_some() && closure.is_none()
        {
            self.trampoline_entry = Some(idx);
        }
        if let Some(c) = closure {
            self.bind_closure_vars(idx, &c);
            self.frames[idx].closure = Some(c);
        }
        if self.observer.enabled && !is_generator {
            self.observer_begin_top();
        }
        Ok(())
    }

    /// Calls a builtin.
    pub(crate) fn call_builtin(
        &mut self,
        b: &Rc<Builtin>,
        mut call: PendingCall,
        strict: bool,
    ) -> VmResult<Value> {
        if !self.observer.enabled {
            if let Some(r) = self.fast_builtin_call(b, &mut call) {
                return r;
            }
        }
        self.call_builtin_framed(b, call, strict)
    }

    /// `ZEND_DO_ICALL` of a pending call of a builtin by name whose
    /// arguments are all by value and of the declared types as they are:
    /// the call the tape's prepared native call makes
    /// (`exec::tape::calls::call_native`), from the pending call's inline
    /// arguments. A call whose arguments include another call's result, an
    /// array or closure literal, `$this` or a static property is not
    /// prepared by the tape (`stays_prepared`) and comes here; the general
    /// [`Vm::call_builtin_impl`] it took instead moves the call twice,
    /// copies its function, scope and name handles into the frame, tests
    /// each argument's parameter field by field and releases the frame
    /// through `pop_frames_to` (about 900 instructions more than the
    /// prepared call: `implode(',', array_keys($a))` 5,667 against 4,773
    /// as two statements).
    ///
    /// The builtin frame is the pooled one above the caller
    /// (`FrameStack::push_builtin`), its argument buffer takes the
    /// arguments (`ZEND_CALL_NUM_ARGS`, what backtraces and
    /// `func_get_args()` of callers read), the handler gets copies and the
    /// declared defaults of the omitted parameters, and the frame is
    /// released in place (`Frame::release_native`). It applies to a call
    /// with no named arguments, `$this`, scope, closure, `__call` name or
    /// partial application, not dynamic and not a constructor, with no
    /// observer, of a function `Builtin::plain_call_upto` answers
    /// [`PlainCall::Named`](crate::func::PlainCall) for up to the last
    /// argument, given at least its required arguments, each accepted as it
    /// is ([`Builtin::accepts`]); a function with a frameless class whose
    /// arguments allow its frameless call keeps that one. Returns `None`,
    /// having changed nothing, otherwise.
    ///
    /// Out of line, as [`Vm::push_simple_ucall`] is.
    #[inline(never)]
    fn call_builtin_pooled(
        &mut self,
        b: &Rc<Builtin>,
        call: &mut PendingCall,
        dst: Option<u32>,
    ) -> Option<VmResult<Value>> {
        if !call.args.is_empty()
            || call.has_named
            || call.dynamic
            || call.is_ctor
            || !call.named.is_empty()
            || call.magic_name.is_some()
            || call.this.is_some()
            || call.scope.is_some()
            || call.called.is_some()
            || call.closure.is_some()
            || call.pfa.is_some()
        {
            return None;
        }
        let n = usize::from(call.inline_count);
        let (kind, upto) = b.plain_call_upto();
        if kind != crate::func::PlainCall::Named || n > upto {
            return None;
        }
        let sig = b.signature();
        let nparams = sig.params.len();
        let variadic = b.flags & flags::VARIADIC != 0;
        if n < sig.required as usize || (!variadic && n > nparams) {
            return None;
        }
        let sent = call.inline_args.get(..n)?;
        let class = (b.flags & flags::FRAMELESS) >> flags::FRAMELESS_SHIFT;
        if class != 0 && crate::special::frameless_args(class, sent) {
            return None;
        }
        let table = b.accepts();
        let last = table.last().copied().unwrap_or(u32::MAX);
        let taken = sent.iter().enumerate().all(|(i, arg)| {
            arg.as_ref().is_some_and(|v| {
                crate::func::accept_bit(v) & table.get(i).copied().unwrap_or(last) != 0
            })
        });
        if !taken {
            return None;
        }
        // A resolution left by an earlier builtin's callable argument is
        // not this call's (`call_builtin_impl`).
        if self.checked_callable.is_some() {
            self.checked_callable = None;
        }
        let mut values = self.call_bufs.values.pop().unwrap_or_default();
        let mut passed = self.call_bufs.passed.pop().unwrap_or_default();
        let live = self.frames.live;
        {
            let frame = self.frames.push_builtin(&self.empty_code, &self.empty_vars);
            if !frame.args.is_empty() {
                frame.args.clear();
            }
            for arg in call.inline_args.iter_mut().take(n) {
                frame.args.push(arg.take());
            }
            // The handler's copies (php-src's handler reads them in the
            // frame).
            values.extend(frame.args.iter().map(|arg| match arg {
                Some(v) => clone_value(v),
                None => Value::Null,
            }));
            if !frame.named.is_empty() {
                frame.named.clear();
            }
            set_opt(&mut frame.this, None);
            set_opt(&mut frame.closure, None);
            set_opt(&mut frame.scope, None);
            set_opt(&mut frame.called, None);
            set_opt(&mut frame.magic_name, None);
            set_opt(&mut frame.func, Some(Func::Builtin(Rc::clone(b))));
            set_opt(&mut frame.builtin_name, Some(b.name.clone()));
            frame.observer_hidden = false;
        }
        call.inline_count = 0;
        passed.resize(values.len(), true);
        // The declared defaults of the omitted trailing parameters, not
        // marked as passed.
        let upto = if variadic {
            nparams.saturating_sub(1)
        } else {
            nparams
        };
        while values.len() < upto {
            let i = values.len();
            match self.builtin_default(b, i) {
                Some(d) if i >= b.signature().required as usize => {
                    values.push(d);
                    passed.push(false);
                }
                _ => break,
            }
        }
        self.pending_call_dst = dst;
        self.magic_call_name = None;
        let mut args = Args {
            values,
            passed,
            named: Vec::new(),
            this: None,
            called: None,
            func: Some(Rc::clone(b)),
        };
        let mut r = (b.handler)(self, &mut args);
        if r.is_ok() {
            if let Err(e) = self.check_interrupt() {
                r = Err(e);
            }
        }
        {
            let Args { values, passed, .. } = args;
            self.put_val_vec(values);
            self.put_bool_vec(passed);
        }
        // Released by the handler, destructed while its frame is current.
        if object::has_pending_destructors() && self.frames.live == live + 1 {
            if let Err(e) = self.run_pending_destructors() {
                r = match (r, e) {
                    (Ok(_), e) => Err(e),
                    (Err(Throw::Exception(old)), Throw::Exception(new)) => {
                        crate::exceptions::chain_previous(&new, &old);
                        Err(Throw::Exception(new))
                    }
                    (Err(old), _) => Err(old),
                };
            }
        }
        if self.frames.live == live + 1 && self.frames.vec[live].release_native(&self.empty_vars) {
            self.frames.pop_cleared_top();
        } else {
            self.pop_frames_to(live + 1);
        }
        Some(r)
    }

    /// [`Vm::call_builtin`] after its [`Vm::fast_builtin_call`] attempt
    /// (which `op_do_call` has just made itself).
    #[inline]
    fn call_builtin_framed(
        &mut self,
        b: &Rc<Builtin>,
        mut call: PendingCall,
        strict: bool,
    ) -> VmResult<Value> {
        if !self.observer.enabled {
            if call.inline_count > 0 {
                let n = call.inline_count as usize;
                if let Some(buf) = self.call_bufs.args.pop() {
                    install_buffer(&mut call.args, buf);
                }
                call.args.reserve(n);
                for i in 0..n {
                    call.args.push(call.inline_args[i].take());
                }
                call.inline_count = 0;
            }
            // php-src's frameless internal calls, where no frame can be seen.
            if crate::special::frameless_call(b, &call) {
                return self.call_builtin_frameless(b, call);
            }
            return self.call_builtin_impl(b, call, strict, false);
        }
        if call.inline_count > 0 {
            let n = call.inline_count as usize;
            if let Some(buf) = self.call_bufs.args.pop() {
                install_buffer(&mut call.args, buf);
            }
            call.args.reserve(n);
            for i in 0..n {
                call.args.push(call.inline_args[i].take());
            }
            call.inline_count = 0;
        }
        let depth = self.frames.len();
        let keep = std::mem::replace(&mut self.observer.keep_builtin_frame, true);
        let quiet = std::mem::take(&mut self.observer.quiet_builtin);
        let result = self.call_builtin_impl(b, call, strict, quiet);
        self.observer.keep_builtin_frame = keep;
        self.observer_end_frame(depth, result.as_ref().ok(), result.as_ref().err());
        self.frames.truncate(depth);
        result
    }

    /// Fast path for single-argument builtin calls matching deterministic type
    /// predicates (`is_int`, `is_string`, `is_array`, etc.) or scalar functions
    /// (`strlen`, `count`). Avoids full call-frame setup and parameter reflection.
    #[inline]
    fn fast_builtin_call(
        &mut self,
        b: &Builtin,
        call: &mut PendingCall,
    ) -> Option<VmResult<Value>> {
        if b.flags & (flags::DEPRECATED | flags::NODISCARD) != 0
            || call.has_named
            || !call.named.is_empty()
            || call.is_ctor
            || call.magic_name.is_some()
            || call.dynamic
            // The names are global functions': a method of the same name
            // (`BcMath\Number::sqrt`) is not one of them.
            || call.scope.is_some()
            || call.this.is_some()
        {
            return None;
        }
        let raw = if call.inline_count == 1 && call.args.is_empty() {
            call.inline_args[0].as_ref()?
        } else if call.inline_count == 0 && call.args.len() == 1 {
            call.args[0].as_ref()?
        } else {
            return None;
        };
        let res = match raw {
            Value::Reference(r) => {
                let borrowed = r.try_borrow()?;
                fast_builtin_eval(b.name.as_bytes(), &borrowed)?
            }
            val => fast_builtin_eval(b.name.as_bytes(), val)?,
        };
        if let Err(e) = self.check_interrupt() {
            return Some(Err(e));
        }
        call.inline_args[0] = None;
        call.inline_count = 0;
        if !call.args.is_empty() || call.args.capacity() > 0 {
            let args = std::mem::take(&mut call.args);
            self.put_arg_vec(args);
        }
        let named = std::mem::take(&mut call.named);
        forget_unallocated(named);
        Some(Ok(res))
    }
}

#[inline]
fn fast_builtin_eval(name: &[u8], val: &Value) -> Option<Value> {
    match name {
        b"is_int" | b"is_integer" | b"is_long" => Some(Value::Bool(matches!(val, Value::Int(_)))),
        b"is_string" => Some(Value::Bool(matches!(val, Value::Str(_)))),
        b"is_array" => Some(Value::Bool(matches!(val, Value::Array(_)))),
        b"is_bool" => Some(Value::Bool(matches!(val, Value::Bool(_)))),
        b"is_null" => Some(Value::Bool(matches!(val, Value::Null))),
        b"is_float" | b"is_double" | b"is_real" => {
            Some(Value::Bool(matches!(val, Value::Float(_))))
        }
        b"is_object" => Some(Value::Bool(matches!(val, Value::Object(_)))),
        b"is_resource" => Some(Value::Bool(
            matches!(val, Value::Resource(r) if !r.is_closed()),
        )),
        b"is_scalar" => Some(Value::Bool(matches!(
            val,
            Value::Int(_) | Value::Float(_) | Value::Str(_) | Value::Bool(_)
        ))),
        b"is_numeric" => Some(Value::Bool(match val {
            Value::Int(_) | Value::Float(_) => true,
            Value::Str(s) => {
                php_runtime::numeric::is_numeric_string_ex(s.as_bytes(), false).is_some()
            }
            _ => false,
        })),
        b"strlen" => match val {
            Value::Str(s) => Some(Value::Int(i64::try_from(s.len()).unwrap_or(i64::MAX))),
            _ => None,
        },
        b"count" | b"sizeof" => match val {
            Value::Array(a) => Some(Value::Int(i64::try_from(a.len()).unwrap_or(i64::MAX))),
            _ => None,
        },
        b"abs" => match val {
            Value::Int(i) if *i != i64::MIN => Some(Value::Int(i.abs())),
            Value::Float(f) => Some(Value::Float(f.abs())),
            _ => None,
        },
        b"ord" => match val {
            Value::Str(s) if s.len() == 1 => Some(Value::Int(i64::from(s.as_bytes()[0]))),
            _ => None,
        },
        b"chr" => match val {
            // The shared one-character string (`ZSTR_CHAR`), as the
            // handler returns it.
            Value::Int(c) => u8::try_from(*c)
                .ok()
                .map(|c| Value::Str(ZStr::from_byte(c))),
            _ => None,
        },
        _ => float_builtin_eval(name, val),
    }
}

/// The one-argument math functions of `ext/standard/math.c` evaluated
/// without their handlers, for an int or a float argument (`None` for
/// any other name or argument): what the handler returns, computed by the
/// same functions, for arguments with which the handler reports nothing.
/// `Z_PARAM_DOUBLE` (`sqrt()`, `sin()`, ..., `log()` without a base,
/// `is_nan()`) widens an int silently in either typing mode;
/// `Z_PARAM_NUMBER` (`floor()`, `ceil()`, `round()` without places or
/// mode) takes an int as it is. These are the functions of frameless
/// classes `FLOAT`, `LOG` and `ROUND` (`special::frameless_flags`) called
/// with one argument, so the frameless call needs no handler, argument
/// buffers or signature for them.
///
/// The names are matched most frequent first ([`float_fn_id`]); a name
/// that is not one of them (an alias, say) falls back to the handler.
#[inline]
pub(crate) fn float_builtin_eval(name: &[u8], val: &Value) -> Option<Value> {
    float_fn_eval(float_fn_id(name)?, val)
}

/// The functions [`float_builtin_eval`] evaluates, by the id
/// [`float_fn_id`] gives their names: what a caller that resolved the name
/// once (a tape instruction) carries instead of it.
pub(crate) mod ff {
    pub(crate) const SQRT: u8 = 0;
    pub(crate) const SIN: u8 = 1;
    pub(crate) const COS: u8 = 2;
    pub(crate) const FLOOR: u8 = 3;
    pub(crate) const CEIL: u8 = 4;
    pub(crate) const ROUND: u8 = 5;
    pub(crate) const EXP: u8 = 6;
    pub(crate) const TAN: u8 = 7;
    pub(crate) const ATAN: u8 = 8;
    pub(crate) const LOG: u8 = 9;
    pub(crate) const LOG10: u8 = 10;
    pub(crate) const ASIN: u8 = 11;
    pub(crate) const ACOS: u8 = 12;
    pub(crate) const SINH: u8 = 13;
    pub(crate) const COSH: u8 = 14;
    pub(crate) const TANH: u8 = 15;
    pub(crate) const ASINH: u8 = 16;
    pub(crate) const ACOSH: u8 = 17;
    pub(crate) const ATANH: u8 = 18;
    pub(crate) const EXPM1: u8 = 19;
    pub(crate) const LOG1P: u8 = 20;
    pub(crate) const DEG2RAD: u8 = 21;
    pub(crate) const RAD2DEG: u8 = 22;
    pub(crate) const IS_NAN: u8 = 23;
    pub(crate) const IS_FINITE: u8 = 24;
    pub(crate) const IS_INFINITE: u8 = 25;
    /// One more than the largest id.
    pub(crate) const COUNT: u8 = 26;
}

/// The id ([`ff`]) of the function of [`float_builtin_eval`] named `name`
/// (lower case); `None` for any other name.
#[inline]
pub(crate) fn float_fn_id(name: &[u8]) -> Option<u8> {
    Some(match name {
        b"sqrt" => ff::SQRT,
        b"sin" => ff::SIN,
        b"cos" => ff::COS,
        b"floor" => ff::FLOOR,
        b"ceil" => ff::CEIL,
        b"round" => ff::ROUND,
        b"exp" => ff::EXP,
        b"tan" => ff::TAN,
        b"atan" => ff::ATAN,
        // `log()` without a base: `log(num)`.
        b"log" => ff::LOG,
        b"log10" => ff::LOG10,
        b"asin" => ff::ASIN,
        b"acos" => ff::ACOS,
        b"sinh" => ff::SINH,
        b"cosh" => ff::COSH,
        b"tanh" => ff::TANH,
        b"asinh" => ff::ASINH,
        b"acosh" => ff::ACOSH,
        b"atanh" => ff::ATANH,
        b"expm1" => ff::EXPM1,
        b"log1p" => ff::LOG1P,
        b"deg2rad" => ff::DEG2RAD,
        b"rad2deg" => ff::RAD2DEG,
        b"is_nan" => ff::IS_NAN,
        b"is_finite" => ff::IS_FINITE,
        b"is_infinite" => ff::IS_INFINITE,
        _ => return None,
    })
}

/// [`float_builtin_eval`] of the function with id `id` ([`ff`]); `None`
/// for an id that names none and for an argument that is neither an int
/// nor a float.
#[inline]
pub(crate) fn float_fn_eval(id: u8, val: &Value) -> Option<Value> {
    use php_stdstr::math as m;
    let x = match val {
        Value::Float(f) => *f,
        Value::Int(i) => {
            // `Z_PARAM_NUMBER`: `floor()`/`ceil()`/`round()` of an int are
            // `(double) $num` (`round()` with no places:
            // `math::round_long(num, 0)`).
            #[allow(clippy::cast_precision_loss)]
            let f = *i as f64;
            if matches!(id, ff::FLOOR | ff::CEIL | ff::ROUND) {
                return Some(Value::Float(f));
            }
            f
        }
        _ => return None,
    };
    let r = match id {
        ff::SQRT => x.sqrt(),
        ff::SIN => x.sin(),
        ff::COS => x.cos(),
        ff::FLOOR => x.floor(),
        ff::CEIL => x.ceil(),
        ff::ROUND => php_support::fmt::php_round(x, 0, php_support::fmt::RoundingMode::HalfUp),
        ff::EXP => x.exp(),
        ff::TAN => x.tan(),
        ff::ATAN => x.atan(),
        ff::LOG => x.ln(),
        ff::LOG10 => x.log10(),
        ff::ASIN => x.asin(),
        ff::ACOS => x.acos(),
        ff::SINH => x.sinh(),
        ff::COSH => x.cosh(),
        ff::TANH => x.tanh(),
        ff::ASINH => m::asinh(x),
        ff::ACOSH => m::acosh(x),
        ff::ATANH => m::atanh(x),
        ff::EXPM1 => x.exp_m1(),
        ff::LOG1P => x.ln_1p(),
        ff::DEG2RAD => m::deg2rad(x),
        ff::RAD2DEG => m::rad2deg(x),
        ff::IS_NAN => return Some(Value::Bool(x.is_nan())),
        ff::IS_FINITE => return Some(Value::Bool(x.is_finite())),
        ff::IS_INFINITE => return Some(Value::Bool(x.is_infinite())),
        _ => return None,
    };
    Some(Value::Float(r))
}

impl Vm {
    /// `zend_try_compile_special_func()`: statically named calls without
    /// named arguments that php-src compiles to dedicated opcodes, so no
    /// internal call exists for observers to see. Inside a function body,
    /// `func_num_args()` / `func_get_args()` become `ZEND_FUNC_NUM_ARGS` /
    /// `ZEND_FUNC_GET_ARGS`; `strlen($x)` becomes `ZEND_STRLEN`. (A
    /// `call_user_func()` / `call_user_func_array()` php-src compiles to
    /// `ZEND_INIT_USER_CALL` compiles to `Op::InitUserCall` here too, so
    /// no builtin call is made for it.) (Only consulted while observers
    /// are enabled.)
    fn compiles_to_opcode(&self, b: &Builtin, call: &PendingCall) -> bool {
        if !call.direct || call.has_named {
            return false;
        }
        match b.name.as_bytes() {
            b"func_num_args" | b"func_get_args" => {
                call.args.is_empty()
                    && self
                        .frames
                        .last()
                        .is_some_and(|caller| caller.kind == FrameKind::Function)
            }
            b"strlen" => call.args.len() == 1,
            _ => false,
        }
    }

    #[allow(clippy::too_many_lines)] // the builtin call protocol (zend_execute_internal)
    fn call_builtin_impl(
        &mut self,
        b: &Rc<Builtin>,
        mut call: PendingCall,
        strict: bool,
        quiet: bool,
    ) -> VmResult<Value> {
        // A native `__call`/`__callStatic` gets the called name through
        // `vm.magic_call_name()`.
        self.magic_call_name = call.magic_name.take();
        // A resolution left by an earlier builtin's callable argument is
        // not this call's.
        if self.checked_callable.is_some() {
            self.checked_callable = None;
        }
        let ctor_this = if call.is_ctor {
            call.this.clone()
        } else {
            None
        };
        let nparams = b.params.len();
        let variadic = b.flags & flags::VARIADIC != 0;
        let npassed = call.args.len();
        // Zero-argument calls (func_num_args() and friends) never touch the
        // buffer pools: empty Vecs are free.
        let (mut values, mut passed): (Vec<Value>, Vec<bool>) = if npassed == 0 && nparams == 0 {
            (Vec::new(), Vec::new())
        } else {
            let cap = nparams.max(npassed);
            let mut values = self.call_bufs.values.pop().unwrap_or_default();
            values.reserve(cap);
            let mut passed = self.call_bufs.passed.pop().unwrap_or_default();
            passed.reserve(cap);
            (values, passed)
        };
        if b.flags & flags::DEPRECATED != 0
            && !std::mem::take(&mut self.builtin_deprecation_reported)
        {
            // Before zpp, like ZEND_DO_ICALL.
            self.check_deprecated_call(&call)?;
        }
        // Builtin frame (for backtraces and func_get_args of callers),
        // pushed before zpp so arity and argument errors show it. It is
        // pushed first and filled in place: a `Frame` is a few hundred
        // bytes, and every move of one is a `memcpy`.
        // (`call_user_func[_array]()` written by name never gets here: it
        // compiles to `Op::InitUserCall`, as php-src's does.)
        let builtin_name = Some(b.name.clone());
        // Computed before the push: `compiles_to_opcode` inspects the caller.
        let observer_hidden = self.observer.enabled
            && (quiet
                || (b.name.as_bytes() == b"__destruct"
                    && call.scope.as_ref().is_some_and(|c| {
                        matches!(
                            c.name.as_bytes(),
                            b"Generator" | b"Fiber" | b"_ZendTestFiber"
                        )
                    }))
                || self.compiles_to_opcode(b, &call));
        // The frame takes the pending call's argument buffer as is
        // (php-src's arguments stay in the call frame on the VM stack); the
        // handler gets copies.
        let pushed_args = std::mem::take(&mut call.args);
        // The builtin frame, readied in place in the frame pool (a pooled
        // frame keeps its buffers for the next user call at this depth)
        // and without allocating: no compiled variables, temporaries or
        // pending calls (a handler that starts a call grows `calls` then),
        // and the argument buffer is the call's own. Only the named
        // arguments, `$this` and scopes the call has are copied; every
        // other field is in its released state.
        let magic_name = self.magic_call_name.clone();
        let frame = self.frames.push_builtin(&self.empty_code, &self.empty_vars);
        if !call.named.is_empty() {
            frame.named = call.named.clone();
        }
        let spare_args = std::mem::replace(&mut frame.args, pushed_args);
        set_opt(&mut frame.this, call.this.clone());
        set_opt(&mut frame.closure, call.closure.clone());
        set_opt(&mut frame.func, Some(call.func.clone()));
        set_opt(&mut frame.scope, call.scope.clone());
        set_opt(&mut frame.called, call.called.clone());
        set_opt(&mut frame.builtin_name, builtin_name);
        set_opt(&mut frame.magic_name, magic_name);
        frame.observer_hidden = observer_hidden;
        if spare_args.capacity() > 0 {
            self.put_arg_vec(spare_args);
        }
        let depth = self.frames.len();
        self.observer_begin_top();
        // `zend_execute_internal` wraps the handler, which parses its own
        // arguments: internal enter precedes arity and type checks.
        self.observer_internal(depth - 1, None);
        // The display name is only needed for error messages: build it lazily.
        let fname = |vm: &Self| {
            vm.func_display(
                &Func::Builtin(b.clone()),
                vm.frames[depth - 1].scope.as_ref(),
            )
        };
        // Arity.
        if npassed < b.required as usize && (!call.has_named || !call.named.is_empty()) {
            let name = fname(self);
            let e = self.arg_count_error_builtin(&name, b, npassed);
            return self.builtin_arg_error(depth, e);
        }
        if !variadic && npassed > nparams {
            let name = fname(self);
            let e = self.arg_count_error_builtin(&name, b, npassed);
            return self.builtin_arg_error(depth, e);
        }
        let nframe_args = npassed;
        if !call.has_named {
            // Positional fast path: all passed arguments are in place without
            // gaps. Arity, type acceptance check and copying into `values` are
            // performed in a single pass.
            for i in 0..npassed {
                let p = if i < nparams {
                    &b.params[i]
                } else {
                    &b.params[nparams - 1]
                };
                let mut v = self.frames[depth - 1].args[i].as_ref().unwrap().clone();
                let accepted = match &p.ty {
                    None => true,
                    Some(ty) => {
                        p.by_ref
                            || p.promote & crate::opcode::UNCHECKED_TYPE != 0
                            || ty.is_mixed()
                            || accepts_without_coercion(ty.mask, &v)
                    }
                };
                if !accepted {
                    if let Err(e) =
                        self.coerce_builtin_arg(b, &call, strict, i, &mut v, depth, nframe_args)
                    {
                        return self.builtin_arg_error(depth, e);
                    }
                }
                values.push(v);
                passed.push(true);
            }
            if !variadic || values.len() + 1 < nparams {
                let upto = if variadic {
                    nparams.saturating_sub(1)
                } else {
                    nparams
                };
                while values.len() < upto {
                    let i = values.len();
                    match self.builtin_default(b, i) {
                        Some(d) if i >= b.required as usize => {
                            values.push(d);
                            passed.push(false);
                        }
                        _ => break,
                    }
                }
            }
        } else {
            for i in 0..npassed {
                match self.frames[depth - 1].args[i].clone() {
                    Some(v) => {
                        values.push(v);
                        passed.push(true);
                    }
                    None => {
                        if i < b.required as usize {
                            let pname = b
                                .params
                                .get(i)
                                .map_or_else(String::new, |p| p.name.to_string_lossy());
                            let fname = fname(self);
                            let e = self.make_throw(
                                "ArgumentCountError",
                                &format!("{fname}(): Argument #{} (${pname}) not passed", i + 1),
                            );
                            return self.builtin_arg_error(depth, e);
                        }
                        values.push(self.builtin_default(b, i).unwrap_or_default());
                        passed.push(false);
                    }
                }
            }
            // Omitted trailing optional parameters get their declared defaults
            // (not marked as passed); backtraces only show the passed ones.
            if !variadic || values.len() + 1 < nparams {
                let upto = if variadic { nparams - 1 } else { nparams };
                while values.len() < upto {
                    let i = values.len();
                    match self.builtin_default(b, i) {
                        Some(d) if i >= b.required as usize => {
                            values.push(d);
                            passed.push(false);
                        }
                        _ => break,
                    }
                }
            }
            if values.len() < b.required as usize {
                let i = values.len();
                let pname = b
                    .params
                    .get(i)
                    .map_or_else(String::new, |p| p.name.to_string_lossy());
                let fname = fname(self);
                let e = self.make_throw(
                    "ArgumentCountError",
                    &format!("{fname}(): Argument #{} (${pname}) not passed", i + 1),
                );
                return self.builtin_arg_error(depth, e);
            }
            // Only named-argument gaps (`None`s, now filled with defaults) can
            // make `values` differ from the copy taken when the frame was pushed.
            if let Some(f) = self.frames.get_mut(depth - 1) {
                frame_args(&values[..nframe_args], &mut f.args);
            }
            // Array variadics use Z_PARAM_VARIADIC, not Z_PARAM_VARIADIC_WITH_NAMED.
            if !call.named.is_empty() && b.name.as_bytes().starts_with(b"array_") {
                let e = self.make_throw(
                    "ArgumentCountError",
                    &format!(
                        "Internal function {}() does not accept named variadic arguments",
                        fname(self)
                    ),
                );
                return self.builtin_arg_error(depth, e);
            }
            // Type coercion.
            for (i, v) in values.iter_mut().enumerate() {
                if !passed[i] {
                    continue;
                }
                let p = if i < nparams {
                    &b.params[i]
                } else if variadic {
                    &b.params[nparams - 1]
                } else {
                    continue;
                };
                let Some(ty) = &p.ty else { continue };
                if p.by_ref || p.promote & crate::opcode::UNCHECKED_TYPE != 0 {
                    continue;
                }
                // Fast path: the value already has an accepted type, so
                // `verify_type` would accept it unchanged and without side effects.
                if ty.is_mixed() || accepts_without_coercion(ty.mask, v) {
                    continue;
                }
                if let Err(e) = self.coerce_builtin_arg(b, &call, strict, i, v, depth, nframe_args)
                {
                    return self.builtin_arg_error(depth, e);
                }
            }
        }
        if call.dynamic && self.frames[depth - 1].scope.is_none() {
            if let Some(e) = self.forbidden_dynamic_call(b) {
                return self.builtin_arg_error(depth, e);
            }
        }
        let mut args = Args {
            values,
            passed,
            named: call.named,
            this: call.this,
            called: call.called,
            func: Some(b.clone()),
        };
        let mut r = (b.handler)(self, &mut args);
        r = self.flush_typed_arg_writes(r);
        self.observer_internal(depth - 1, Some(&r));
        // Blocking builtins (sleep, stream reads) return to a pending
        // interrupt.
        if r.is_ok() {
            if let Err(e) = self.check_interrupt() {
                r = Err(e);
            }
        }
        // Objects released by the builtin (e.g. by-ref outputs overwriting
        // the last reference) are destructed while its frame is still
        // active, so traces show `[internal function]` + the builtin.
        {
            let Args { values, passed, .. } = args;
            self.put_val_vec(values);
            self.put_bool_vec(passed);
        }
        if object::has_pending_destructors() && self.frames.len() == depth {
            if let Err(e) = self.run_pending_destructors() {
                r = match (r, e) {
                    (Ok(_), e) => Err(e),
                    (Err(Throw::Exception(old)), Throw::Exception(new)) => {
                        crate::exceptions::chain_previous(&new, &old);
                        Err(Throw::Exception(new))
                    }
                    (Err(old), _) => Err(old),
                };
            }
        }
        if let Some(f) = self.frames.get(depth - 1) {
            root_released_args(&f.args);
        }
        self.pop_frames_to(depth);
        if let Some(t) = &ctor_this {
            // `zend_object_store_ctor_failed` for native constructors.
            if r.is_err() {
                let d = object::data(t);
                d.flags.set(d.flags.get() | object::oflags::DESTRUCTED);
            }
            // `OBJ_RELEASE(This)` of `new` (`ZEND_CALL_RELEASE_THIS`).
            crate::gc::released_ctor_this(t);
        }
        r
    }

    /// The zpp of builtin argument `i` (`v`) whose value does not already
    /// have an accepted type: callable checks, weak-mode coercion and the
    /// `TypeError`. Out of line: the common call has accepted types only.
    #[cold]
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn coerce_builtin_arg(
        &mut self,
        b: &Rc<Builtin>,
        call: &PendingCall,
        strict: bool,
        i: usize,
        v: &mut Value,
        depth: usize,
        nframe_args: usize,
    ) -> Result<(), Throw> {
        let nparams = b.params.len();
        let p = if i < nparams {
            &b.params[i]
        } else {
            &b.params[nparams - 1]
        };
        let Some(ty) = &p.ty else { return Ok(()) };
        let ctx = TypeCtx {
            scope: call.scope.as_ref(),
            called: call.called.as_ref(),
            strict,
            internal_arg: Some(crate::typecheck::InternalArg {
                func: b,
                argn: i + 1,
                param: &p.name,
            }),
        };
        // The function's and the parameter's display names, for the
        // messages only (most calls need neither).
        let names = |vm: &Self| {
            (
                vm.func_display(&Func::Builtin(b.clone()), call.scope.as_ref()),
                p.name.to_string_lossy(),
            )
        };
        if ty.mask & crate::types::mask::CALLABLE != 0
            && !ty.has_classes()
            && ty.mask & !(crate::types::mask::CALLABLE | crate::types::mask::NULL) == 0
            && !(ty.allows_null() && v.is_null())
        {
            let resolved = self.zend_is_callable(v, true)?;
            if let Ok(call) = resolved {
                // `Z_PARAM_FUNC` hands the resolution to the function
                // (`zend_fcall_info_cache`): `call_user_func[_array]()`
                // takes it instead of resolving the callable again, when
                // resolving it cannot raise a callable deprecation.
                if plain_callable(v) {
                    self.checked_callable = Some((v.clone(), call));
                }
                return Ok(());
            }
            if let Err(why) = resolved {
                // `Z_PARAM_FUNC` checks without suppressing the callable
                // deprecations; a valid callable emits them when the
                // function resolves it, an invalid one here (a class that
                // was not found has none, and is not autoloaded twice).
                if !why.contains("not found") {
                    let _ = self.zend_is_callable(v, false)?;
                }
                let (fname, pname) = names(self);
                let nul = if ty.allows_null() { " or null" } else { "" };
                return Err(self.make_throw(
                    "TypeError",
                    &format!(
                        "{fname}(): Argument #{} (${pname}) must be a valid callback{nul}, {why}",
                        i + 1
                    ),
                ));
            }
            return Ok(());
        }
        if !self.verify_type(ty, v, &ctx)? {
            let (fname, pname) = names(self);
            let msg = if ty.mask & crate::types::mask::CALLABLE != 0
                && !ty.has_classes()
                && ty.mask & !(crate::types::mask::CALLABLE | crate::types::mask::NULL) == 0
            {
                let why = self.callable_error(v);
                let nul = if ty.allows_null() { " or null" } else { "" };
                format!(
                    "{fname}(): Argument #{} (${pname}) must be a valid callback{nul}, {why}",
                    i + 1
                )
            } else {
                format!(
                    "{fname}(): Argument #{} (${pname}) must be of type {}, {} given",
                    i + 1,
                    ty.to_php_string(),
                    given_type(v)
                )
            };
            return Err(self.make_throw("TypeError", &msg));
        }
        // Weak-mode zpp converts to a string in place
        // (`zend_parse_arg_str_weak`), which backtraces show; other scalar
        // conversions only fill C locals, leaving the argument as passed
        // (`NULL` for a deprecated null-to-int).
        if matches!(v, Value::Str(_)) && i < nframe_args {
            if let Some(slot) = self
                .frames
                .get_mut(depth - 1)
                .and_then(|f| f.args.get_mut(i))
            {
                *slot = Some(v.clone());
            }
        }
        Ok(())
    }

    /// The default of native parameter `i`: a parsed literal, or a
    /// constant (`SOME_CONST`, `Class::CONST`) resolved at call time.
    ///
    /// The literal (what nearly every omitted parameter has) is read in
    /// line; only a default given by name goes out of line.
    #[inline]
    pub(crate) fn builtin_default(&mut self, b: &Builtin, i: usize) -> Option<Value> {
        if let Some(Some(v)) = b.defaults.get(i) {
            return Some(clone_value(v));
        }
        self.builtin_default_named(b, i)
    }

    /// [`Vm::builtin_default`] of a parameter whose default is not a
    /// literal: a constant's value, or `None`.
    ///
    /// `UNKNOWN` is no constant: it is `gen_stub`'s marker of a parameter
    /// whose default the function works out itself (`array_keys()`'s
    /// `$filter_value`, `mt_rand()`'s bounds), which php-src never looks
    /// up. It is answered before anything else: looking it up as a
    /// constant on every call of such a function cost ~250 instructions
    /// a call, and a script's own `UNKNOWN` constant is not the default.
    #[inline(never)]
    fn builtin_default_named(&mut self, b: &Builtin, i: usize) -> Option<Value> {
        let text = b.params.get(i)?.default_text.as_deref()?.trim();
        if text == "UNKNOWN" {
            return None;
        }
        let text = text.strip_prefix('\\').unwrap_or(text);
        if text.is_empty()
            || !text
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b':' || c == b'\\')
        {
            return None;
        }
        if let Some(p) = find_double_colon(text.as_bytes()) {
            let cls = self.find_class(&text.as_bytes()[..p])?;
            return self
                .class_constant(&cls, &ZStr::from(&text.as_bytes()[p + 2..]))
                .ok();
        }
        self.constants.get(text.as_bytes()).cloned()
    }

    /// Pops frames above `depth - 1` (the builtin frame pushed at `depth`).
    pub(crate) fn pop_frames_to(&mut self, depth: usize) {
        if self.observer.keep_builtin_frame {
            return;
        }
        if self.frames.len() >= depth {
            let args = std::mem::take(&mut self.frames[depth - 1].args);
            // Frames left above the builtin's (none, normally) go; the
            // builtin frame itself is released into the pool, as php-src
            // pops its call frame off the VM stack without touching the
            // stack above.
            if self.frames.len() > depth {
                self.frames.truncate(depth);
            }
            self.frames
                .release_builtin_top(&self.empty_code, &self.empty_vars);
            self.put_arg_vec(args);
        }
    }

    /// `zend_forbid_dynamic_call()` of the scope-introspection functions,
    /// after their parameter parsing.
    #[cold]
    fn forbidden_dynamic_call(&mut self, b: &Builtin) -> Option<Throw> {
        matches!(
            b.name.as_bytes(),
            b"extract"
                | b"compact"
                | b"get_defined_vars"
                | b"func_get_args"
                | b"func_get_arg"
                | b"func_num_args"
        )
        .then(|| {
            self.make_throw(
                "Error",
                &format!("Cannot call {}() dynamically", b.name.to_string_lossy()),
            )
        })
    }

    /// An argument error raised before a builtin's handler ran: the
    /// internal-leave notification, then the frame pop.
    fn builtin_arg_error(&mut self, depth: usize, e: Throw) -> VmResult<Value> {
        if let (Throw::Exception(o), Some(Func::Builtin(b))) =
            (&e, self.frames.get(depth - 1).and_then(|f| f.func.as_ref()))
        {
            if b.flags & flags::SCRUB_ARG_ERROR_TRACE != 0 {
                scrub_trace_args(o);
            }
        }
        let r = Err(e);
        self.observer_internal(depth - 1, Some(&r));
        self.pop_frames_to(depth);
        r
    }

    fn arg_count_error_builtin(&mut self, fname: &str, b: &Builtin, given: usize) -> Throw {
        let variadic = b.flags & flags::VARIADIC != 0;
        let nparams = b.params.len();
        let required = b.required as usize;
        let (kind, n) = if given < required {
            if required == nparams && !variadic {
                ("exactly", required)
            } else {
                ("at least", required)
            }
        } else if required == nparams {
            ("exactly", nparams)
        } else {
            ("at most", nparams)
        };
        self.make_throw(
            "ArgumentCountError",
            &format!(
                "{fname}() expects {kind} {n} argument{}, {given} given",
                if n == 1 { "" } else { "s" }
            ),
        )
    }

    // ------------------------------------------------------------------
    // RECV
    // ------------------------------------------------------------------

    fn recv_type_error(&mut self, n: u32, v: &Value) -> Throw {
        let f = self.frame();
        let func = f.func.clone();
        let uf = f.ufunc.clone();
        let scope = f.scope.clone();
        let called = f.called.clone();
        let internal = f.called_from_internal;
        let (Some(func), Some(uf)) = (func, uf) else {
            return Throw::Fatal;
        };
        let p = uf
            .params
            .get(n as usize)
            .or_else(|| uf.params.last())
            .cloned();
        let Some(p) = p else { return Throw::Fatal };
        let tyname = p.ty.as_ref().map_or_else(String::new, |t| {
            t.display(
                scope.as_ref().map(|s| &s.name),
                scope.as_ref().and_then(|s| s.parent_name()),
                called.as_ref().map(|c| &c.name),
            )
        });
        // Argument errors use `%pS` for the complete member name, including
        // the NUL-separated suffix of an anonymous class.
        let fname = match scope.as_ref() {
            Some(c) => format!("{}::{}", c.name, func.name()),
            None => func.name().to_string_lossy(),
        };
        let pname = if p.variadic {
            String::new()
        } else {
            format!(" (${})", p.name)
        };
        let mut msg = format!(
            "{fname}(): Argument #{}{pname} must be of type {tyname}, {} given",
            n + 1,
            given_type(v)
        );
        let through_call_user_func = self.frames.iter().rev().nth(1).is_some_and(|caller| {
            caller.builtin_name.is_none()
                && caller.func.as_ref().is_some_and(|func| {
                    let name = func.name();
                    matches!(name.as_bytes(), b"call_user_func" | b"call_user_func_array")
                })
        });
        let is_closure = f.closure.is_some() || uf.is_closure();
        let is_closure_invoke = is_closure
            && (f
                .magic_name
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case(b"__invoke"))
                || f.this
                    .as_ref()
                    .is_some_and(|t| object::class_of(t).name.eq_ignore_ascii_case(b"closure")));
        if let Some((file, line)) = self.attribute_caller_site() {
            msg.push_str(&format!(", called in {file} on line {line}"));
        } else if (!internal || through_call_user_func)
            && !is_closure_invoke
            && self.frames.len() >= 2
        {
            let caller = self
                .frames
                .iter()
                .rev()
                .skip(1)
                .find(|f| f.kind != FrameKind::Builtin);
            if let Some(caller) = caller {
                msg.push_str(&format!(
                    ", called in {} on line {}",
                    caller.code.filename,
                    caller.line()
                ));
            }
        }
        self.make_throw("TypeError", &msg)
    }

    fn recv_check(&mut self, n: u32, mut v: Value) -> VmResult<Value> {
        // Fast path: untyped parameter, or a value accepted as-is.
        {
            let f = self.frame();
            let uf = f.ufunc.as_ref().expect("user frame");
            let p = uf.params.get(n as usize).or_else(|| uf.params.last());
            match p.and_then(|p| p.ty.as_ref()) {
                None => return Ok(v),
                Some(ty) if ty.is_mixed() || accepts_without_coercion(ty.mask, &v) => {
                    return Ok(v);
                }
                Some(_) => {}
            }
        }
        if !self.recv_verify(n, &mut v)? {
            // php-src already holds the rejected value in the CV, so
            // the TypeError's trace shows it: keep it visible.
            if let Some(slot) = self.frame_mut().args.get_mut(n as usize) {
                if slot.is_none() {
                    *slot = Some(v.clone());
                }
            }
            return Err(self.recv_type_error(n, &v));
        }
        Ok(v)
    }

    /// The typed part of [`Vm::recv_check`] (`zend_verify_recv_arg_type`):
    /// verifies and, where allowed, coerces `v` in place. `false` rejects
    /// it (the caller raises the `TypeError`).
    fn recv_verify(&mut self, n: u32, v: &mut Value) -> VmResult<bool> {
        // The parameter type is read in place from the function
        // (`zend_verify_recv_arg_type` reads `arg_info->type`): the
        // function's `Rc` keeps it alive while the check runs user code (a
        // `__toString()` of a coerced argument), so only that handle is
        // cloned, not the declaration with its class names.
        let uf = self.frame().ufunc.clone().expect("user frame");
        let p = uf.params.get(n as usize).or_else(|| uf.params.last());
        let Some(ty) = p.and_then(|p| p.ty.as_ref()) else {
            return Ok(true);
        };
        let f = self.frame();
        let (strict, scope, called) = (f.caller_strict, f.scope.clone(), f.called.clone());
        let ctx = TypeCtx {
            scope: scope.as_ref(),
            called: called.as_ref(),
            strict,
            internal_arg: None,
        };
        self.verify_type(ty, v, &ctx)
    }

    /// `ZEND_RECV` / `ZEND_RECV_INIT` of an argument that
    /// [`init_cvs`] already stored in its CV, as php-src's SEND ops store
    /// it in the callee frame: only the type is verified, in place. `false`
    /// when the CV is undefined (a gap left by named arguments).
    fn recv_bound(&mut self, n: u32, cv: u32) -> VmResult<bool> {
        let idx = {
            let f = self.frame();
            let idx = cv as usize;
            let Some(v) = f.cvs.get(idx).and_then(crate::vm::CvSlot::get) else {
                return Ok(false);
            };
            let uf = f.ufunc.as_ref().expect("user frame");
            let p = uf.params.get(n as usize).or_else(|| uf.params.last());
            match p.and_then(|p| p.ty.as_ref()) {
                None => return Ok(true),
                Some(ty) if ty.is_mixed() || accepts_without_coercion(ty.mask, v) => {
                    return Ok(true);
                }
                Some(_) => idx,
            }
        };
        self.recv_bound_slow(n, idx)
    }

    /// [`Vm::recv_bound`] of a value that needs coercion or is rejected.
    /// Like the RECV of a value not yet in its CV, the value is out of the
    /// CV while it is coerced (a `__toString()` call may run).
    #[cold]
    #[inline(never)]
    fn recv_bound_slow(&mut self, n: u32, idx: usize) -> VmResult<bool> {
        let taken = self.frame_mut().cvs.get_mut(idx).and_then(|c| c.set(None));
        let Some(mut v) = taken else {
            return Ok(false);
        };
        let accepted = self.recv_verify(n, &mut v)?;
        if accepted {
            self.frame_mut().cvs[idx].set(Some(v));
            return Ok(true);
        }
        // php-src's CV holds the rejected value: the TypeError's trace
        // shows it.
        self.frame_mut().cvs[idx].set(Some(v.clone()));
        Err(self.recv_type_error(n, &v))
    }

    #[inline(always)]
    pub(crate) fn op_recv(&mut self, n: u32, cv: u32) -> VmResult<()> {
        let f = self.frame_mut();
        let n_idx = n as usize;
        if let Some(slot) = f.args.get_mut(n_idx) {
            if slot.is_none() {
                // Bound by init_cvs
                let c_idx = cv as usize;
                if let Some(v) = f.cvs.get(c_idx).and_then(crate::vm::CvSlot::get) {
                    if let Some(uf) = &f.ufunc {
                        let p = uf.params.get(n_idx).or_else(|| uf.params.last());
                        match p.and_then(|p| p.ty.as_ref()) {
                            None => return Ok(()),
                            Some(ty) if ty.is_mixed() || accepts_without_coercion(ty.mask, v) => {
                                return Ok(());
                            }
                            _ => {}
                        }
                    }
                }
                if self.recv_bound(n, cv)? {
                    return Ok(());
                }
                return Err(self.missing_arg_error(n));
            } else if let Some(v) = slot.take() {
                let v = self.recv_check(n, v)?;
                self.cv_bind(cv, v);
                return Ok(());
            }
        }
        Err(self.missing_arg_error(n))
    }

    /// The attribute a constructor frame was called for: the dummy frame
    /// of `zend_get_attribute_object()` is user code at the attribute's
    /// file and line, so argument errors report it as the caller.
    #[cold]
    fn attribute_caller_site(&self) -> Option<(ZStr, u32)> {
        let caller = self.frames.len().checked_sub(2)?;
        self.attribute_call_sites
            .iter()
            .rev()
            .find(|s| s.0 == caller)
            .map(|s| (s.1.clone(), s.2))
    }

    fn missing_arg_error(&mut self, n: u32) -> Throw {
        let f = self.frame();
        let uf = f.ufunc.clone().expect("user frame");
        let func = f.func.clone().expect("func");
        let scope = f.scope.clone();
        let passed = f.num_args();
        let internal = f.called_from_internal;
        let fname = self.func_display(&func, scope.as_ref());
        if passed > n as usize {
            // Skipped by named args.
            let p = &uf.params[n as usize];
            return self.make_throw(
                "ArgumentCountError",
                &format!("{fname}(): Argument #{} (${}) not passed", n + 1, p.name),
            );
        }
        // `required_num_args == num_args`, where `num_args` leaves the
        // variadic parameter out (`f($a, $b, ...$c)` needs "exactly 2").
        let declared = uf.params.len() - usize::from(uf.flags & flags::VARIADIC != 0);
        let exact = uf.required as usize == declared;
        let mut msg = format!("Too few arguments to function {fname}(), {passed} passed");
        let caller = self
            .frames
            .iter()
            .rev()
            .skip(1)
            .find(|caller| caller.kind != FrameKind::Builtin);
        let through_call_user_func = self.frames.iter().rev().nth(1).is_some_and(|caller| {
            caller.builtin_name.is_none()
                && caller.func.as_ref().is_some_and(|func| {
                    let name = func.name();
                    matches!(name.as_bytes(), b"call_user_func" | b"call_user_func_array")
                })
        });
        if let Some((file, line)) = self.attribute_caller_site() {
            msg.push_str(&format!(" in {file} on line {line}"));
        } else if !internal || through_call_user_func {
            if let Some(caller) = caller {
                msg.push_str(&format!(
                    " in {} on line {}",
                    caller.code.filename,
                    caller.line()
                ));
            }
        }
        msg.push_str(&format!(
            " and {} {} expected",
            if exact { "exactly" } else { "at least" },
            uf.required
        ));
        self.make_throw("ArgumentCountError", &msg)
    }

    #[inline(always)]
    pub(crate) fn op_recv_init(&mut self, n: u32, cv: u32) -> VmResult<bool> {
        let f = self.frame_mut();
        let n_idx = n as usize;
        if let Some(slot) = f.args.get_mut(n_idx) {
            if slot.is_none() {
                // Bound by init_cvs, or a named-argument gap (default).
                let c_idx = cv as usize;
                if let Some(v) = f.cvs.get(c_idx).and_then(crate::vm::CvSlot::get) {
                    if let Some(uf) = &f.ufunc {
                        let p = uf.params.get(n_idx).or_else(|| uf.params.last());
                        match p.and_then(|p| p.ty.as_ref()) {
                            None => return Ok(true),
                            Some(ty) if ty.is_mixed() || accepts_without_coercion(ty.mask, v) => {
                                return Ok(true);
                            }
                            _ => {}
                        }
                    }
                }
                return self.recv_bound(n, cv);
            } else if let Some(v) = slot.take() {
                let v = self.recv_check(n, v)?;
                self.cv_bind(cv, v);
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn op_recv_default(&mut self, n: u32, cv: u32, v: Value) -> VmResult<()> {
        // RECV_INIT verifies the (possibly constant-expression) default and
        // coerces it (`float $x = 0` gives float(0)).
        let v = self.recv_check(n, v)?;
        self.cv_bind(cv, v);
        Ok(())
    }

    pub(crate) fn op_recv_variadic(&mut self, n: u32, cv: u32) -> VmResult<()> {
        let passed = self.frame().args.len();
        let mut arr = HashTable::new();
        for i in n as usize..passed {
            let Some(v) = self.frame().args[i].clone() else {
                continue;
            };
            let v = self.recv_check(u32::try_from(i).unwrap_or(n), v)?;
            self.frame_mut().args[i] = Some(v.clone());
            let _ = arr.push(v);
        }
        for i in 0..self.frame().named.len() {
            let (k, v) = self.frame().named[i].clone();
            let v = self.recv_check(n, v)?;
            self.frame_mut().named[i].1 = v.clone();
            arr.insert(ArrayKey::Str(k), v);
        }
        self.cv_bind(cv, Value::Array(ZArray::from_table(arr)));
        Ok(())
    }

    /// [`Vm::op_recv_variadic`] of an untyped (or `mixed`) variadic
    /// parameter `n` without named arguments (`ZEND_RECV_VARIADIC`, its
    /// loop over the extra arguments without the type check): the
    /// arguments past the declared parameters, as they are, into a list in
    /// variable `cv` (an empty array when there are none). The tape runs
    /// it in place (`exec/tape.rs`, `run_vm_op`). `false`, having changed
    /// nothing, for any other parameter: the generic op runs.
    #[inline(never)]
    pub(crate) fn recv_variadic_plain(&mut self, n: u32, cv: u32) -> bool {
        let Some(f) = self.frames.last() else {
            return false;
        };
        let Some(uf) = f.ufunc.as_ref() else {
            return false;
        };
        if !f.named.is_empty()
            || uf
                .params
                .get(n as usize)
                .is_none_or(|p| !p.variadic || p.ty.as_ref().is_some_and(|t| !t.is_mixed()))
        {
            return false;
        }
        let mut arr = HashTable::new();
        for v in f.args.iter().skip(n as usize).flatten() {
            let _ = arr.push(v.clone());
        }
        self.cv_bind(cv, Value::Array(ZArray::from_table(arr)));
        true
    }

    /// Verifies the return value of the current frame.
    pub(crate) fn verify_return(&mut self, v: Option<Value>) -> VmResult<Value> {
        // Fast path: no return type, or a returned value accepted as-is
        // (skips building the function/type display strings).
        {
            let f = self.frame();
            let Some(uf) = &f.ufunc else {
                return Ok(v.unwrap_or_default());
            };
            match (&uf.return_type, &v) {
                (None, _) => return Ok(v.unwrap_or_default()),
                (Some(rt), Some(val))
                    if !uf.is_generator()
                        && !rt.is_void()
                        && !rt.is_never()
                        && (rt.is_mixed() || accepts_without_coercion(rt.mask, val)) =>
                {
                    return Ok(v.unwrap_or_default());
                }
                _ => {}
            }
        }
        // The return type is read in place from the function
        // (`zend_verify_return_type` reads `func->common.arg_info[-1]`):
        // the function's `Rc` keeps it alive while the check runs user code
        // (a `__toString()` of a coerced value), so only that handle is
        // cloned, not the declaration with its class names.
        let Some(uf) = self.frame().ufunc.clone() else {
            return Ok(v.unwrap_or_default());
        };
        let Some(rt) = uf.return_type.as_ref() else {
            return Ok(v.unwrap_or_default());
        };
        if uf.is_generator() {
            return Ok(v.unwrap_or_default());
        }
        // `void` and `never` exclude each other: the common `void` return
        // is answered before anything else is fetched from the frame.
        if rt.is_void() {
            return Ok(Value::Null);
        }
        let (strict, func, scope, called) = {
            let f = self.frame();
            (
                f.code.strict_types,
                f.func.clone(),
                f.scope.clone(),
                f.called.clone(),
            )
        };
        if rt.is_never() {
            let fname = func.as_ref().map_or_else(String::new, |f| match &scope {
                Some(c) => format!("{}::{}", c.name, f.name()),
                None => f.name().to_string_lossy(),
            });
            let what = if scope.is_some() {
                "method"
            } else {
                "function"
            };
            return Err(self.make_throw(
                "TypeError",
                &format!("{fname}(): never-returning {what} must not implicitly return"),
            ));
        }
        // Return type errors print the scope and function name separately
        // with `%s`. The strings are only built for an error
        // (`zend_verify_return_error`), not for a value that coerces.
        let error_names = || {
            // `get_function_or_method_name()`: both names whole (a closure
            // of an anonymous class carries that class's full name too).
            let fname = func.as_ref().map_or_else(String::new, |f| {
                let name = f.name().to_string_lossy();
                match scope.as_ref() {
                    Some(c) => format!("{}::{name}", c.name.to_string_lossy()),
                    None => name,
                }
            });
            let tyname = rt.display(
                scope.as_ref().map(|s| &s.name),
                scope.as_ref().and_then(|s| s.parent_name()),
                called.as_ref().map(|c| &c.name),
            );
            (fname, tyname)
        };
        let Some(mut v) = v else {
            let (fname, tyname) = error_names();
            return Err(self.make_throw(
                "TypeError",
                &format!("{fname}(): Return value must be of type {tyname}, none returned"),
            ));
        };
        let ctx = TypeCtx {
            scope: scope.as_ref(),
            called: called.as_ref(),
            strict,
            internal_arg: None,
        };
        if !self.verify_type(rt, &mut v, &ctx)? {
            let (fname, tyname) = error_names();
            return Err(self.make_throw(
                "TypeError",
                &format!(
                    "{fname}(): Return value must be of type {tyname}, {} returned",
                    given_type(&v)
                ),
            ));
        }
        Ok(v)
    }

    // ------------------------------------------------------------------
    // Calls from Rust
    // ------------------------------------------------------------------

    /// Runs a prepared call to completion.
    pub(crate) fn call_pending(&mut self, call: PendingCall) -> VmResult<Value> {
        self.call_pending_ex(call, true)
    }

    /// [`Vm::call_pending`]; `from_internal` is false for a call made
    /// straight from the user code running (`zend_error` calling the user
    /// error handler with `EG(current_execute_data)` a user frame), whose
    /// argument errors say "called in ... on line N".
    pub(crate) fn call_pending_ex(
        &mut self,
        mut call: PendingCall,
        from_internal: bool,
    ) -> VmResult<Value> {
        // `zend_call_function` services pending interrupts (async signals)
        // before entering user code from internal code.
        self.check_interrupt()?;
        if call.inline_count > 0 {
            if call.args.is_empty() && takes_args_inline(&call) {
                // Every parameter by value (`fill_call_args`): the arguments
                // are dereferenced in the inline buffer, and the frame is
                // readied from it by `push_user_call`'s plain entry.
                let n = call.inline_count as usize;
                for slot in &mut call.inline_args[..n] {
                    let v = match slot {
                        Some(Value::Reference(r)) => r.get(),
                        _ => continue,
                    };
                    *slot = Some(v);
                }
                return self.call_user_inline(call, from_internal);
            }
        }
        if !call.is_magic_trampoline() {
            for (i, arg) in call.args.iter_mut().enumerate() {
                let Some(value) = arg else { continue };
                let param = call
                    .func
                    .params()
                    .get(i)
                    .or_else(|| call.func.params().last().filter(|p| p.variadic));
                if let Some(param) = param.filter(|p| p.by_ref) {
                    if !value.is_reference() && param.promote & crate::opcode::PREFER_REF == 0 {
                        let pname = if param.variadic {
                            String::new()
                        } else {
                            format!(" (${})", param.name)
                        };
                        let fname = if call
                            .magic_name
                            .as_deref()
                            .is_some_and(|m| m.eq_ignore_ascii_case(b"__invoke"))
                            && call.func.is_closure()
                        {
                            "Closure::__invoke".to_string()
                        } else {
                            self.func_display(&call.func, call.scope.as_ref())
                        };
                        self.warning(&format!(
                            "{fname}(): Argument #{}{pname} must be passed by reference, value given", i + 1
                        ))?;
                        *value = Value::Reference(PhpRef::new(std::mem::take(value)));
                    }
                } else {
                    *value = value.deref();
                }
            }
            for (_, value) in &mut call.named {
                let param = call.func.params().last().filter(|p| p.variadic);
                if let Some(param) = param.filter(|p| p.by_ref) {
                    if !value.is_reference() && param.promote & crate::opcode::PREFER_REF == 0 {
                        let fname = if call
                            .magic_name
                            .as_deref()
                            .is_some_and(|m| m.eq_ignore_ascii_case(b"__invoke"))
                            && call.func.is_closure()
                        {
                            "Closure::__invoke".to_string()
                        } else {
                            self.func_display(&call.func, call.scope.as_ref())
                        };
                        self.warning(&format!(
                            "{fname}(): Argument #{} must be passed by reference, value given",
                            call.func.params().len()
                        ))?;
                        *value = Value::Reference(PhpRef::new(std::mem::take(value)));
                    }
                } else {
                    *value = value.deref();
                }
            }
        }
        let r = match &call.func {
            Func::User(_) => {
                self.check_deprecated_call(&call)?;
                let strict = call.strict;
                self.push_user_call(call, RetTarget::Discard, strict, from_internal)?;
                let base = self.frames.len() - 1;
                self.execute(base)
            }
            Func::Builtin(b) => {
                let b = b.clone();
                let strict = call.strict;
                self.call_builtin(&b, call, strict)
            }
        };
        match r {
            Err(Throw::Exception(old)) => self.interrupt_after_throw(old),
            r => r,
        }
    }

    /// [`Vm::call_pending_ex`] of a user function whose arguments are in
    /// the inline buffer, dereferenced: `zend_call_function` /
    /// `zend_call_known_function` pushing one frame, binding the arguments
    /// and calling `execute_ex`. No argument vector is taken from the pool
    /// or given back, the by-reference loop is skipped (no parameter is by
    /// reference), and `push_user_call` takes its plain entry (the
    /// arguments straight into the compiled variables, the `RECV` ops
    /// skipped) instead of `init_cvs`.
    fn call_user_inline(&mut self, call: PendingCall, from_internal: bool) -> VmResult<Value> {
        self.check_deprecated_call(&call)?;
        let strict = call.strict;
        self.push_user_call(call, RetTarget::Discard, strict, from_internal)?;
        let base = self.frames.len() - 1;
        match self.execute(base) {
            Err(Throw::Exception(old)) => self.interrupt_after_throw(old),
            r => r,
        }
    }

    /// Puts `args` into `call` for [`Vm::call_pending`]: in the call's
    /// inline buffer when the callee is a user function that takes every
    /// argument by value and there are at most six (no vector taken from
    /// the pool, and the frame is set up by `push_user_call`'s plain
    /// entry), otherwise in a pooled argument vector as before.
    #[inline]
    pub(crate) fn fill_call_args(&mut self, call: &mut PendingCall, args: Vec<Value>) {
        let n = args.len();
        if n == 0 {
            return;
        }
        if n <= call.inline_args.len()
            && call.inline_count == 0
            && call.args.is_empty()
            && takes_args_inline(call)
        {
            for (slot, a) in call.inline_args.iter_mut().zip(args) {
                *slot = Some(a);
            }
            #[allow(clippy::cast_possible_truncation)] // n <= 6
            let count = n as u8;
            call.inline_count = count;
            return;
        }
        let mut call_args = self.call_bufs.args.pop().unwrap_or_default();
        call_args.clear();
        call_args.reserve(n);
        call_args.extend(args.into_iter().map(Some));
        call.args = call_args;
    }

    /// [`Vm::fill_call_args`] from a slice, whose values are moved out
    /// (left `null`): the magic methods and interface methods the engine
    /// calls with a fixed argument list (`__get($name)`, `__set($name,
    /// $value)`, `offsetGet($offset)`, ...) build no vector at all for a
    /// plain user method.
    #[inline]
    pub(crate) fn fill_call_args_slice(&mut self, call: &mut PendingCall, args: &mut [Value]) {
        let n = args.len();
        if n == 0 {
            return;
        }
        if n <= call.inline_args.len()
            && call.inline_count == 0
            && call.args.is_empty()
            && takes_args_inline(call)
        {
            for (slot, a) in call.inline_args.iter_mut().zip(args.iter_mut()) {
                *slot = Some(std::mem::take(a));
            }
            #[allow(clippy::cast_possible_truncation)] // n <= 6
            let count = n as u8;
            call.inline_count = count;
            return;
        }
        let mut call_args = self.call_bufs.args.pop().unwrap_or_default();
        call_args.clear();
        call_args.reserve(n);
        call_args.extend(args.iter_mut().map(|a| Some(std::mem::take(a))));
        call.args = call_args;
    }

    /// `zend_call_known_instance_method()`: calls method `m` (known per
    /// class: a magic method from `Class::magic`, an interface method
    /// looked up once) on `o` with a fixed argument list, moved out of
    /// `args`. The same call as [`Vm::call_method`], without its argument
    /// vector.
    pub(crate) fn call_known_method(
        &mut self,
        o: &Obj,
        m: &Rc<Method>,
        args: &mut [Value],
    ) -> VmResult<Value> {
        if let Some(r) = self.enter_method_direct(o, m, args) {
            return r;
        }
        let mut call = self.pending_for_method(m, Some(o.clone()), object::class_of(o).clone());
        self.fill_call_args_slice(&mut call, args);
        self.call_pending(call)
    }

    /// [`Vm::call_known_method`] of a plain user method, with the frame
    /// readied in place: what `call_pending` and `push_user_call`'s plain
    /// entry do for that call, without building, moving and taking apart a
    /// `PendingCall` (`zend_call_known_function`: `zend_vm_stack_push_call_frame`,
    /// the arguments copied into the frame's slots, `execute_ex`).
    ///
    /// A plain user method here: an instance method that is no generator,
    /// has no static variables or `finally`, is not deprecated, takes every
    /// parameter by value and none variadic, gets at least its required
    /// argument count and at most its parameter count, each typed
    /// parameter a value its type takes as it is (no coercion to report),
    /// and no observer is active. `None` for any other call, which takes
    /// the `PendingCall` path; the arguments may then have been
    /// dereferenced already, as that path dereferences them for by-value
    /// parameters (every parameter is one when the type test declines).
    fn enter_method_direct(
        &mut self,
        o: &Obj,
        m: &Rc<Method>,
        args: &mut [Value],
    ) -> Option<VmResult<Value>> {
        let Func::User(uf) = &m.func else {
            return None;
        };
        let n = args.len();
        if m.is_static()
            || uf.is_generator()
            || uf.code.num_statics != 0
            || uf.code.num_finally != 0
            || uf.flags & flags::DEPRECATED != 0
            || self.has_observers()
            || n < uf.required as usize
            || n > uf.params.len()
            || self.frames.len() > 50_000
            || uf.params.iter().any(|p| p.variadic || p.by_ref)
        {
            return None;
        }
        // By-value arguments are passed dereferenced (`call_pending`).
        for a in args.iter_mut() {
            if let Value::Reference(r) = a {
                let v = r.get();
                *a = v;
            }
        }
        if !uf.params[..n]
            .iter()
            .zip(args.iter())
            .all(|(p, v)| p.ty.as_ref().is_none_or(|ty| param_accepts(ty, v)))
        {
            return None;
        }
        // `zend_call_function` services pending interrupts before entering
        // user code from internal code.
        if let Err(t) = self.check_interrupt() {
            return Some(Err(t));
        }
        let uf = uf.clone();
        let idx = self.frames.push_in_place(&uf.code, FrameKind::Function);
        let f = &mut self.frames[idx];
        f.this = Some(o.clone());
        f.scope = Some(m.scope());
        f.called = Some(object::class_of(o).clone());
        f.ret = RetTarget::Discard;
        f.caller_strict = false;
        f.called_from_internal = true;
        f.is_ctor = false;
        f.args.clear();
        f.args.resize(n, None);
        for (i, a) in args.iter_mut().enumerate() {
            let v = std::mem::take(a);
            if let Some(slot) = f.cvs.get_mut(i) {
                slot.set(Some(v));
            }
        }
        // The RECV ops of the passed arguments are skipped, as
        // `push_user_call`'s plain entry skips them.
        let mut first_op = 0;
        for i in 0..n {
            match uf.code.ops.get(first_op) {
                Some(Op::Recv { n, .. }) if *n as usize == i => first_op += 1,
                Some(Op::RecvInit { n, skip, .. }) if *n as usize == i => {
                    first_op = *skip as usize;
                }
                _ => break,
            }
        }
        f.ip = first_op;
        f.func = Some(m.func.clone());
        f.ufunc = Some(uf);
        let base = self.frames.len() - 1;
        Some(match self.execute(base) {
            Err(Throw::Exception(old)) => self.interrupt_after_throw(old),
            r => r,
        })
    }

    /// `zend_call_function` checks `EG(vm_interrupt)` once the callee has
    /// returned, in the caller's frame, also when the callee threw `old`
    /// (e.g. a tick function raising a signal and throwing): the interrupt
    /// handlers run with the exception set aside (pcntl's dispatcher, master)
    /// and one they throw gets `old` as its previous exception.
    #[cold]
    #[inline(never)]
    fn interrupt_after_throw(&mut self, old: Obj) -> VmResult<Value> {
        match self.check_interrupt() {
            Ok(()) => Err(Throw::Exception(old)),
            Err(Throw::Exception(new)) => {
                crate::exceptions::chain_previous(&new, &old);
                Err(Throw::Exception(new))
            }
            Err(e) => Err(e),
        }
    }

    /// Calls a callable value with arguments: `zend_call_function()` for a
    /// callable an internal function received through `Z_PARAM_FUNC`
    /// (validated with [`Vm::resolve_callable`] or [`Vm::is_callable`],
    /// which is where the `"self"`/`"parent"`/`"static"` and
    /// `["C", "P::m"]` deprecations belong, so none is raised here).
    ///
    /// The callable is resolved by the `zend_is_callable_ex` port in the
    /// calling user frame's scope, like the `zend_fcall_info_cache` php-src
    /// fills at parameter parsing: `"parent::m"` names the frame's parent
    /// class, `"C::m"` and `[$obj, "P::m"]` bind that frame's `$this`, and
    /// method visibility is judged from that frame. A value the port
    /// rejects keeps the dynamic-call resolution for its error ("Call to
    /// undefined function f()").
    ///
    /// [`Vm::resolve_callable`]: crate::Vm::resolve_callable
    /// [`Vm::is_callable`]: crate::Vm::is_callable
    pub fn call_value(&mut self, callable: &Value, args: Vec<Value>) -> VmResult<Value> {
        if let Value::Object(o) = callable.deref() {
            if let Some(mut call) = self.closure_user_call(&o) {
                self.fill_call_args(&mut call, args);
                return self.call_pending(call);
            }
        }
        let mut call = match self.zend_is_callable(callable, true)? {
            Ok(call) => call,
            Err(_) => self.resolve_callable_for_call(callable)?,
        };
        self.fill_call_args(&mut call, args);
        self.call_pending(call)
    }

    /// Resolves `callable` once for repeated calls from an internal
    /// function, like the `zend_fcall_info_cache` that `Z_PARAM_FUNC` fills:
    /// the call template [`Vm::call_prepared`] reuses. `Ok(None)` when the
    /// value does not resolve that way; [`Vm::call_value`] then reports the
    /// error on each call, as before.
    ///
    /// The resolution is [`Vm::call_value`]'s: a closure's bound function,
    /// `$this` and scope (fixed for the closure's lifetime), or the
    /// `zend_is_callable_ex` port in the calling frame's scope.
    pub fn prepare_call_value(&mut self, callable: &Value) -> VmResult<Option<PendingCall>> {
        if let Value::Object(o) = callable.deref() {
            if let Some(call) = self.closure_user_call(&o) {
                return Ok(Some(call));
            }
        }
        Ok(self.zend_is_callable(callable, true)?.ok())
    }

    /// Calls a template from [`Vm::prepare_call_value`] (or
    /// [`Vm::resolve_callable`]) with `args`, which are moved out of the
    /// slice (left `null`) into the callee frame: `zend_call_function` with
    /// a prepared `zend_fcall_info_cache`.
    ///
    /// A user function whose parameters are all by value and that takes at
    /// most six arguments gets them in the pending call's inline buffer, so
    /// no argument vector is allocated and the frame is set up like a
    /// `DO_UCALL` from user code; every other callee takes the generic
    /// [`Vm::call_resolved`] path. Both are observably identical: by-value
    /// arguments are dereferenced either way (`call_pending`).
    ///
    /// [`Vm::resolve_callable`]: crate::Vm::resolve_callable
    pub fn call_prepared(&mut self, template: &PendingCall, args: &mut [Value]) -> VmResult<Value> {
        let n = args.len();
        let inline = n <= template.inline_args.len()
            && template.args.is_empty()
            && template.inline_count == 0
            && template.named.is_empty()
            && !template.has_named
            && template.pfa.is_none()
            && template.magic_name.is_none()
            && match &template.func {
                Func::User(uf) => !uf.params.iter().any(|p| p.by_ref),
                Func::Builtin(_) => false,
            };
        if !inline {
            let mut call_args = self.call_bufs.args.pop().unwrap_or_default();
            call_args.clear();
            call_args.reserve(n);
            call_args.extend(args.iter_mut().map(|a| Some(std::mem::take(a))));
            let mut call = template.clone();
            call.args = call_args;
            return self.call_pending(call);
        }
        // A pure int function compiled whole, every parameter passed an
        // int (`array_map(fn($x) => $x & 1, $ints)`): the call is made in
        // machine code, as the tape makes it (`exec::tape::calls::compiled`).
        if !template.is_ctor {
            if let Func::User(uf) = &template.func {
                if let Some(r) = self.native_callback(uf, args) {
                    return r;
                }
            }
        }
        // `call_pending` for a by-value user callee: the interrupt check,
        // dereferenced arguments, the deprecation, then the frame.
        self.check_interrupt()?;
        // The template's handles only (`zend_fcall_info_cache`): its
        // argument buffers, named arguments, `__call` name and placeholders
        // are empty here (tested above), so a derived `clone` would copy
        // six empty inline slots and two empty vectors per call.
        debug_assert!(template.inline_args.iter().all(Option::is_none));
        let mut call = PendingCall {
            this: template.this.clone(),
            scope: template.scope.clone(),
            called: template.called.clone(),
            closure: template.closure.clone(),
            direct: template.direct,
            is_ctor: template.is_ctor,
            strict: template.strict,
            dynamic: template.dynamic,
            ..PendingCall::new(template.func.clone())
        };
        for (slot, a) in call.inline_args.iter_mut().zip(args.iter_mut()) {
            *slot = Some(match std::mem::take(a) {
                Value::Reference(r) => r.get(),
                v => v,
            });
        }
        #[allow(clippy::cast_possible_truncation)] // n <= 6
        let count = n as u8;
        call.inline_count = count;
        self.check_deprecated_call(&call)?;
        let strict = call.strict;
        self.push_user_call(call, RetTarget::Discard, strict, true)?;
        let base = self.frames.len() - 1;
        match self.execute(base) {
            Err(Throw::Exception(old)) => self.interrupt_after_throw(old),
            r => r,
        }
    }

    /// [`Vm::call_prepared`] of a user function whose whole body runs as
    /// machine code (`exec::tape::jit::native_call`: a pure int function,
    /// counted and compiled there once it is called often enough): a plain
    /// call that passes every declared parameter an int. Such a body reads
    /// no `$this`, scope or bound variable, so a closure's, a method's and
    /// a function's call are the same call. `None`, nothing changed, when
    /// it does not apply or the native code declined (an overflow, a
    /// pending safe point): the call is then made with its frame.
    #[inline]
    fn native_callback(
        &mut self,
        uf: &Rc<UserFunction>,
        args: &mut [Value],
    ) -> Option<VmResult<Value>> {
        use crate::exec::tape::jit;
        let n = args.len();
        if uf.call_shape.native.get() == jit::NATIVE_REFUSED
            || n != uf.params.len()
            || n > jit::NATIVE_MAX_ARGS
            || self.observer.enabled
            || uf.flags & (flags::DEPRECATED | flags::NODISCARD) != 0
            || uf.call_shape() == crate::func::CallShape::NOT_PLAIN
        {
            return None;
        }
        let mut ints = [0i64; jit::NATIVE_MAX_ARGS];
        for (slot, a) in ints.iter_mut().zip(args.iter()) {
            let Value::Int(i) = a else {
                return None;
            };
            *slot = *i;
        }
        // The interrupt check of `call_pending`, before the call.
        if let Err(e) = self.check_interrupt() {
            return Some(Err(e));
        }
        let r = jit::native_call(uf, &ints[..n])?;
        // The arguments are moved out, as the framed call leaves them.
        for a in args.iter_mut() {
            *a = Value::Null;
        }
        Some(Ok(Value::Int(r)))
    }

    /// Calls a callable with named arguments too.
    pub(crate) fn call_value_named(
        &mut self,
        callable: &Value,
        args: Vec<Value>,
        named: Vec<(ZStr, Value)>,
    ) -> VmResult<Value> {
        if let Value::Object(o) = callable.deref() {
            if let Some(call) = self.closure_user_call(&o) {
                return self.call_resolved_named(call, args, named);
            }
        }
        let call = match self.zend_is_callable(callable, false)? {
            Ok(call) => call,
            Err(reason) => return Err(self.make_throw("TypeError", &reason)),
        };
        self.call_resolved_named(call, args, named)
    }

    /// [`Vm::call_value_named`] for an already resolved callable.
    pub(crate) fn call_resolved_named(
        &mut self,
        mut call: PendingCall,
        args: Vec<Value>,
        named: Vec<(ZStr, Value)>,
    ) -> VmResult<Value> {
        if named.is_empty() {
            // `Vm::call_resolved`: positional arguments only.
            self.fill_call_args(&mut call, args);
            return self.call_pending(call);
        }
        let mut call_args = self.call_bufs.args.pop().unwrap_or_default();
        call_args.clear();
        call_args.reserve(args.len());
        call_args.extend(args.into_iter().map(Some));
        call.args = call_args;
        for (k, v) in named {
            let pos = if call.is_magic_trampoline() {
                None
            } else {
                call.func
                    .params()
                    .iter()
                    .position(|p| p.name == k && !p.variadic)
            };
            match pos {
                Some(p) => {
                    if call.args.len() <= p {
                        call.args.resize(p + 1, None);
                    } else if call.args[p].is_some() {
                        return Err(self.make_throw(
                            "Error",
                            &format!(
                                "Named parameter ${} overwrites previous argument",
                                k.to_string_lossy()
                            ),
                        ));
                    }
                    call.args[p] = Some(v);
                }
                None => {
                    if !call.func.is_variadic() && !call.is_magic_trampoline() {
                        return Err(self.make_throw(
                            "Error",
                            &format!("Unknown named parameter ${}", k.to_string_lossy()),
                        ));
                    }
                    call.named.push((k, v));
                }
            }
            call.has_named = true;
        }
        self.call_pending(call)
    }

    /// Calls a method on an object.
    pub(crate) fn call_method(
        &mut self,
        o: &Obj,
        m: &Rc<Method>,
        mut args: Vec<Value>,
    ) -> VmResult<Value> {
        if let Some(r) = self.enter_method_direct(o, m, &mut args) {
            return r;
        }
        let mut call = self.pending_for_method(m, Some(o.clone()), object::class_of(o).clone());
        self.fill_call_args(&mut call, args);
        self.call_pending(call)
    }

    /// Calls a method by name (with `__call` fallback), dereferencing the
    /// result.
    pub(crate) fn call_method_name(
        &mut self,
        o: &Obj,
        name: &str,
        args: Vec<Value>,
    ) -> VmResult<Value> {
        Ok(self.call_method_name_raw(o, name, args)?.deref())
    }

    /// Calls a method by name, keeping a returned reference.
    pub(crate) fn call_method_name_raw(
        &mut self,
        o: &Obj,
        name: &str,
        args: Vec<Value>,
    ) -> VmResult<Value> {
        let m = class_method_lc(object::class_of(o), name);
        let mut call = match m {
            // `offsetGet()`, the Iterator methods, ...: the method found
            // takes `call_method`'s entry (its frame readied in place).
            Some(m) => return self.call_method(o, &m, args),
            None => self.method_call(o, &ZStr::from(name))?,
        };
        self.fill_call_args(&mut call, args);
        self.call_pending(call)
    }

    /// Calls a method by name with possibly undefined arguments (`None` is
    /// `ZVAL_UNDEF`, e.g. the offset `spl_array_read_dimension_ex` hands
    /// `offsetGet()` for `$o[]`): a user method reports "Argument #n (...)
    /// not passed".
    pub fn call_method_with_undef(
        &mut self,
        o: &Obj,
        name: &str,
        args: Vec<Option<Value>>,
    ) -> VmResult<Value> {
        let cls = object::class_of(o).clone();
        let m = class_method_lc(&cls, name);
        let mut call = match m {
            Some(m) => self.pending_for_method(&m, Some(o.clone()), cls),
            None => self.method_call(o, &ZStr::from(name))?,
        };
        call.args = args;
        self.call_pending(call)
    }

    /// Calls a static method by name.
    #[allow(dead_code)]
    pub(crate) fn call_static_method(
        &mut self,
        cls: &Rc<Class>,
        name: &str,
        args: Vec<Value>,
    ) -> VmResult<Value> {
        let mut call = self.static_method_call(cls, &ZStr::from(name), false)?;
        self.fill_call_args(&mut call, args);
        self.call_pending(call)
    }

    /// Calls a function by name.
    pub fn call_function(&mut self, name: &str, args: Vec<Value>) -> VmResult<Value> {
        let f = if !name.as_bytes().iter().any(u8::is_ascii_uppercase) {
            self.functions.get(name.as_bytes()).cloned()
        } else {
            let lc = name.to_ascii_lowercase();
            self.functions.get(lc.as_bytes()).cloned()
        };
        let Some(f) = f else {
            return Err(self.make_throw("Error", &format!("Call to undefined function {name}()")));
        };
        let quiet = matches!(&f, Func::Builtin(_)) && self.observer.enabled;
        let mut call = PendingCall::new(f);
        self.fill_call_args(&mut call, args);
        if !self.observer.enabled {
            return self.call_pending(call);
        }
        let saved = self.observer.quiet_builtin;
        self.observer.quiet_builtin = quiet;
        let result = self.call_pending(call);
        self.observer.quiet_builtin = saved;
        result
    }
}

/// Whether a call's arguments may go in its inline buffer
/// ([`Vm::fill_call_args`]): a user function (not a `__call` trampoline,
/// whose arguments are packed from `args`) without named arguments or
/// placeholders that takes no parameter by reference, so the arguments
/// only need dereferencing (`call_pending`'s loop does nothing else for
/// such a callee).
#[inline]
fn takes_args_inline(call: &PendingCall) -> bool {
    call.magic_name.is_none()
        && call.named.is_empty()
        && !call.has_named
        && call.pfa.is_none()
        && match &call.func {
            Func::User(uf) => !uf.params.iter().any(|p| p.by_ref),
            Func::Builtin(_) => false,
        }
}

/// The method `name` of `cls`, looked up by its lower-case name: a name of
/// up to 64 bytes is lowered on the stack (`offsetGet`, `getIterator`:
/// the engine's own calls), a longer one in a heap buffer.
fn class_method_lc(cls: &Class, name: &str) -> Option<Rc<Method>> {
    let b = name.as_bytes();
    if !b.iter().any(u8::is_ascii_uppercase) {
        return cls.methods.get(b).cloned();
    }
    let mut buf = [0u8; 64];
    if let Some(dst) = buf.get_mut(..b.len()) {
        for (d, s) in dst.iter_mut().zip(b) {
            *d = s.to_ascii_lowercase();
        }
        return cls.methods.get(&*dst).cloned();
    }
    cls.methods
        .get(name.to_ascii_lowercase().as_bytes())
        .cloned()
}

/// The method name a `__call`/`__callStatic` trampoline carries:
/// `zend_get_call_trampoline_func` cuts it at the first NUL byte (kept
/// for compatibility, bug #46238).
pub(crate) fn trampoline_name(name: &ZStr) -> ZStr {
    match name.as_bytes().iter().position(|&b| b == 0) {
        Some(n) => ZStr::from(&name.as_bytes()[..n]),
        None => name.clone(),
    }
}

/// Whether `call` of `uf` with `n` inline arguments takes
/// [`Vm::push_simple_ucall`]: the conditions under which `op_do_call`
/// hands a user function to [`Vm::push_user_call`] and that takes its fast
/// branch (the observer and stack-depth checks are the caller's).
///
/// What depends on the declaration alone (not deprecated, `#[\NoDiscard]`,
/// body-less nor a generator, no static variables, foreach or finally
/// slots, no by-reference or variadic parameter, no more parameters than
/// compiled variables) is the function's cached [`CallShape`]: one load and
/// compare per call. The arguments' types are checked only when a
/// parameter is typed.
///
/// The function's static-variable store (`UserFunction::statics`) is not
/// looked at: without static variables (`num_statics == 0`, the shape) no
/// op of the body reads it (`Vm::static_slot`, `Vm::static_init`), and
/// what a call does never depends on it. It is not empty for a `static`
/// closure declared in a function or method, whose first object is cached
/// in slot 0 (`ZEND_DECLARE_LAMBDA_FUNCTION`'s cache slot,
/// `Vm::make_closure`): testing it sent every call of such a closure down
/// the general path (~425 instructions more per call).
///
/// [`CallShape`]: crate::func::CallShape
#[inline]
fn simple_ucall(uf: &UserFunction, call: &PendingCall, n: usize) -> bool {
    use crate::func::CallShape;
    let shape = uf.call_shape();
    // `n <= params.len() <= cv_names.len()` (the shape),
    // so every argument has its compiled variable, and a trailing variadic
    // parameter gets none (its `RECV_VARIADIC` collects nothing).
    if !call.args.is_empty()
        || !call.named.is_empty()
        || call.magic_name.is_some()
        || call.pfa.is_some()
        || n < uf.required as usize
        || n > call.inline_args.len()
    {
        return false;
    }
    if shape == CallShape::NOT_PLAIN {
        // Static variables, a trailing variadic parameter, a by-reference
        // parameter past the arguments passed (`near_plain`).
        return near_plain(uf, n);
    }
    if n > uf.params.len() {
        return false;
    }
    if shape == CallShape::TYPED {
        // A plain loop: the `Iterator::all` chain it replaces was an
        // out-of-line `try_fold` call per check.
        for (p, arg) in uf.params.iter().zip(&call.inline_args[..n]) {
            if let Some(ty) = &p.ty {
                if !arg.as_ref().is_some_and(|v| param_accepts(ty, v)) {
                    return false;
                }
            }
        }
    }
    true
}

/// A function or method that would make a plain call but for its `static`
/// variables (`function f() { static $n = 0; return ++$n; }`), a trailing
/// variadic parameter (`function f($name, ...$args)`) or a by-reference
/// parameter the call does not reach (`function get($k, &$found = null)`
/// called as `get($k)`), called with `n` arguments: such calls took the
/// generic `InitFCall`/`Send`/`DoCall` (`Vm::push_user_call`), about a
/// thousand instructions more per call. The tape's prepared call
/// (`exec/tape/calls.rs`) and [`Vm::push_simple_ucall`] take them
/// ([`UserFunction::near_plain_limit`] says which, and how many arguments).
///
/// Static variables: a frame of such a function never holds them
/// (`Frame::statics` is only set for a closure, whose variables are its
/// object's, and closures with statics are excluded); `Vm::static_slot`
/// and `Vm::static_init` read the function's own store
/// (`UserFunction::statics`) through `Frame::ufunc`, which the prepared
/// frame holds as the generic call's does. At the return,
/// `Frame::keeps_function` declines a function with statics, so the frame
/// drops its handles as after a generic call.
///
/// A variadic parameter: the arguments past the declared ones go to the
/// frame's argument buffer after a `None` for each declared parameter, as
/// `push_user_call` leaves them, where `RecvVariadic`, `func_get_args()`
/// and backtraces read them.
#[cold]
#[inline(never)]
pub(crate) fn near_plain(uf: &UserFunction, n: usize) -> bool {
    uf.near_plain_limit().is_some_and(|most| n <= most)
}

/// The arguments of a plain call of a variadic function past its declared
/// parameters (`extra`, moved out), appended to the frame's argument
/// buffer where its `RECV_VARIADIC` collects them (`Vm::op_recv_variadic`).
#[cold]
#[inline(never)]
fn variadic_extras(args: &mut Vec<Option<Value>>, extra: &mut [Option<Value>]) {
    for a in extra {
        args.push(a.take());
    }
}

/// Whether a value is returned as it is from a function with return type
/// `rt` (`verify_return`'s fast path, [`Vm::return_simple`]).
#[inline(always)]
#[allow(clippy::inline_always)] // once per return, in three places
fn return_accepts(rt: Option<&crate::types::TypeDecl>, v: &Value) -> bool {
    match rt {
        None => true,
        // An object of a class the type names (`: Column`) is accepted by
        // name as a parameter is ([`param_accepts`]); `self`, `static` and
        // intersections are left to the generic check.
        Some(rt) => {
            !rt.is_void()
                && !rt.is_never()
                && (rt.is_mixed()
                    || accepts_without_coercion(rt.mask, v)
                    || (matches!(v, Value::Object(_)) && param_accepts(rt, v)))
        }
    }
}

/// A callable whose resolution raises no deprecation, with or without
/// them suppressed: a function name, a closure or invokable object, or an
/// object with a plain method name (`self::`/`parent::`/`static::` forms
/// and `Class::method` strings are left out).
fn plain_callable(v: &Value) -> bool {
    match v {
        Value::Str(s) => !s.as_bytes().contains(&b':'),
        Value::Object(_) => true,
        Value::Array(a) => {
            a.len() == 2
                && matches!(
                    a.get(&php_runtime::ArrayKey::Int(0)),
                    Some(Value::Object(_))
                )
                && matches!(a.get(&php_runtime::ArrayKey::Int(1)),
                    Some(Value::Str(m)) if !m.as_bytes().contains(&b':'))
        }
        _ => false,
    }
}

/// Whether `a` and `b` are the same callable value (the same handle).
pub(crate) fn same_callable(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => ZStr::ptr_eq(x, y),
        (Value::Object(x), Value::Object(y)) => php_runtime::Obj::ptr_eq(x, y),
        (Value::Array(x), Value::Array(y)) => php_runtime::ZArray::ptr_eq(x, y),
        _ => false,
    }
}

/// `v.clone()` with the scalar cases inline: the derived `Clone` of
/// [`Value`] is an out-of-line call even for an integer.
#[inline(always)]
#[allow(clippy::inline_always)] // an argument or result per call
pub(crate) fn clone_value(v: &Value) -> Value {
    match v {
        Value::Null => Value::Null,
        Value::Bool(b) => Value::Bool(*b),
        Value::Int(i) => Value::Int(*i),
        Value::Float(f) => Value::Float(*f),
        _ => v.clone(),
    }
}

/// The arguments of a returning native call, about to be dropped
/// (`zend_vm_stack_free_args`, `zval_ptr_dtor_nogc`): an array or a
/// reference whose last handle they hold releases its contents
/// (`zend_array_destroy`), buffering the objects still referenced
/// elsewhere as possible roots; the arguments themselves are not buffered.
#[inline]
fn root_released_args(args: &[Option<Value>]) {
    for v in args.iter().flatten() {
        let last = match v {
            Value::Array(a) => a.refcount() == 1,
            Value::Reference(r) => r.refcount() == 1,
            _ => false,
        };
        if last {
            crate::gc::possible_root(v);
        }
    }
}

/// A value [`Vm::return_simple`] returns as it is: a scalar, null or a
/// string (no possible cycle root, nothing for `gc::frame_released`).
#[inline(always)]
#[allow(clippy::inline_always)] // one test per return
fn returns_directly(v: &Value) -> bool {
    matches!(
        v,
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_)
    )
}

/// Whether releasing `v`, one handle of a value, may run a destructor: an
/// object or a non-empty array held nowhere else (an array's elements may
/// be the last handles of objects). Anything else only loses a count.
#[inline(always)]
#[allow(clippy::inline_always)] // one tag test per discarded return
fn may_destruct(v: &Value) -> bool {
    match v {
        Value::Object(o) => o.refcount() == 1,
        Value::Array(a) => a.refcount() == 1 && !a.is_empty(),
        _ => false,
    }
}

/// Whether `v` is a scalar, whose release does nothing.
#[inline(always)]
#[allow(clippy::inline_always)] // one tag test per return
fn is_scalar(v: &Value) -> bool {
    matches!(
        v,
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
    )
}

/// Releases a pending call [`Vm::call_builtin_pooled`] has made: it owns
/// nothing then but its function handle and possibly its two argument
/// buffers (its inline arguments were moved out, and it has no `$this`,
/// scope, closure, `__call` name or partial application, as that function
/// checked), so the rest is forgotten rather than passed through the
/// call's drop glue.
#[inline]
fn release_spent_call(call: PendingCall) {
    let PendingCall {
        func,
        this,
        scope,
        called,
        args,
        inline_args,
        named,
        closure,
        magic_name,
        pfa,
        ..
    } = call;
    drop(func);
    debug_assert!(inline_args.iter().all(Option::is_none));
    debug_assert!(this.is_none() && scope.is_none() && called.is_none());
    debug_assert!(closure.is_none() && magic_name.is_none() && pfa.is_none());
    std::mem::forget((this, scope, called, inline_args, closure, magic_name, pfa));
    if args.capacity() == 0 {
        std::mem::forget(args);
    } else {
        drop(args);
    }
    if named.capacity() == 0 {
        std::mem::forget(named);
    } else {
        drop(named);
    }
}

/// Stores `v` into an `Option` field of a frame being set up, releasing
/// what it held: the drop glue of the payload runs only when there was
/// one (a recycled frame's fields are empty), where `*slot = v` calls the
/// out-of-line `drop_in_place` of the whole `Option` either way.
#[inline(always)]
#[allow(clippy::inline_always)] // five fields per call
fn set_opt<T>(slot: &mut Option<T>, v: Option<T>) {
    if let Some(old) = std::mem::replace(slot, v) {
        drop(old);
    }
}

/// [`Vm::push_closure_user_call`] into the pending calls `calls` of the
/// running frame: the call is built in place straight from a shared borrow
/// of the closure's data (one look at the object's header and one type
/// check of its internal state, where `object::internal` looked the object
/// up again and borrowed the state mutably). Returns `false`, having pushed
/// nothing, when `o` is not a closure of a user function.
#[inline]
fn push_closure_user_call_in(calls: &mut Vec<PendingCall>, o: &Obj) -> bool {
    let data = object::data(o);
    if data.class.lc_name.as_bytes() != b"closure" {
        return false;
    }
    let Ok(internal) = data.internal.try_borrow() else {
        return false;
    };
    let Some(d) = internal
        .as_ref()
        .and_then(|x| x.downcast_ref::<crate::closure::ClosureData>())
    else {
        return false;
    };
    if !matches!(&d.func, Func::User(_)) {
        return false;
    }
    push_call_in_place(calls, || PendingCall {
        this: d.this.clone(),
        scope: d.scope.clone(),
        called: d.called.clone(),
        closure: if d.fake { None } else { Some(o.clone()) },
        magic_name: d.magic_name.clone(),
        dynamic: true,
        ..PendingCall::new(d.func.clone())
    });
    true
}

/// The class a `self`/`parent`/`static` spec names in the running user
/// frame `f` (`static` is its called scope, as `rtcache`'s `spec_class_in`
/// resolves it); `None` for any other spec, or when the frame has no such
/// class.
#[inline]
pub(crate) fn relative_spec_class<'a>(f: &'a Frame, spec: &ClassSpec) -> Option<&'a Rc<Class>> {
    match spec {
        ClassSpec::SelfClass => f.scope.as_ref(),
        ClassSpec::Parent => f.scope.as_ref()?.parent.as_ref(),
        ClassSpec::Static => f.called.as_ref(),
        _ => None,
    }
}

/// [`Vm::init_relative_static_call_cached`] on the running frame `f`, the
/// op at `slot` (its index): `self::name()` / `parent::name()` /
/// `static::name()` on the site's monomorphic cache hit, the pending call
/// pushed in place. Returns `false`, having changed nothing, on a miss.
/// Frame-local, so the dispatch loop's hot block (`exec/hot.rs`) runs it
/// as well.
pub(crate) fn init_relative_static_in(
    f: &mut Frame,
    spec: &ClassSpec,
    lit: u32,
    slot: usize,
) -> bool {
    let Some(cls) = relative_spec_class(f, spec) else {
        return false;
    };
    let scope_addr = f.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
    let hit = match f.code.method_cache.borrow().get(slot) {
        Some(Some(e))
            if e.class_addr == Rc::as_ptr(cls) as usize
                && e.scope_addr == scope_addr
                && e.name_lit == Some(lit) =>
        {
            match (
                e.func.get(),
                e.method_scope.as_ref().and_then(std::rc::Weak::upgrade),
            ) {
                (Some(func), Some(scope)) => Some((func, scope, e.is_static)),
                _ => None,
            }
        }
        _ => None,
    };
    let Some((func, scope, is_static)) = hit else {
        return false;
    };
    let (this, called) = if is_static {
        let called = match f.called.as_ref() {
            Some(c) if c.is_subclass_of(cls) => Rc::clone(c),
            _ => Rc::clone(cls),
        };
        (None, called)
    } else {
        let Some(this) = f.this.as_ref() else {
            return false;
        };
        let called = object::class_of(this);
        if !called.is_subclass_of(cls) {
            return false;
        }
        (Some(this.clone()), Rc::clone(called))
    };
    push_call_in_place(&mut f.calls, move || PendingCall {
        this,
        scope: Some(scope),
        called: Some(called),
        ..PendingCall::new(func)
    });
    true
}

/// [`Vm::init_method_call_fast`] on the running frame `frame`, the op at
/// `slot` (its index): `$obj->name()` on the site's method cache hit, the
/// pending call pushed in place. Returns `false`, having changed nothing,
/// on a miss. Frame-local, so the dispatch loop's hot block
/// (`exec/hot.rs`) runs it as well.
#[inline]
pub(crate) fn init_method_call_in(frame: &mut Frame, obj: Operand, lit: u32, slot: usize) -> bool {
    let o = match obj {
        Operand::Cv(cv) if !frame.symtab_attached => {
            match frame.cvs.get(cv as usize).and_then(crate::vm::CvSlot::get) {
                Some(Value::Object(o)) => o,
                _ => return false,
            }
        }
        Operand::Tmp(t) => match frame.tmps.get(t as usize) {
            Some(Value::Object(o)) => o,
            _ => return false,
        },
        // `$this->name()` (op1 `IS_UNUSED`): the frame's `$this`.
        Operand::Unused => match frame.this.as_ref() {
            Some(o) => o,
            None => return false,
        },
        _ => return false,
    };
    let cls = object::class_of(o);
    if cls.hooks.as_ref().and_then(|h| h.get_method).is_some() {
        return false;
    }
    let cls_addr = Rc::as_ptr(cls) as usize;
    let scope_addr = frame.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
    let Some((func, scope, is_static)) =
        method_entry_hit(&frame.code, slot, cls_addr, scope_addr, lit)
    else {
        return false;
    };
    let called = Rc::clone(cls);
    // The call's `$this`: a copy of the variable's object, or the
    // temporary's handle itself, moved out as the read's
    // `std::mem::take` moves it (`ZEND_CALL_RELEASE_THIS` without an
    // `addref`/`release` pair: the call owns the operand's reference).
    // A static method's call takes none: the temporary's handle is
    // released after the push (the last handle of the object goes as
    // it goes there, its destructor left to the safe point after the
    // instruction).
    let (this, released) = match obj {
        Operand::Tmp(t) => match frame.tmps.get_mut(t as usize).map(std::mem::take) {
            Some(Value::Object(held)) if !is_static => (Some(held), None),
            other => (None, other),
        },
        _ if is_static => (None, None),
        _ => (Some(o.clone()), None),
    };
    push_call_in_place(&mut frame.calls, move || PendingCall {
        this,
        scope,
        called: Some(called),
        ..PendingCall::new(func)
    });
    drop(released);
    true
}

/// A method call site's cached resolution for an object of the class at
/// `cls_addr` called from the scope at `scope_addr` (the method, its
/// declaring scope, whether it is static), when the site's
/// [`OpArray::method_cache`] entry was filled for them. A site keeps two
/// entries (php-src's is monomorphic; a call site that sees two classes in
/// turn, `$item->render()` over a list of two kinds, missed every time):
/// the first is tested inline; on a class mismatch the second is, and a
/// hit there swaps the two ([`promote_method_entry`]).
///
/// [`OpArray::method_cache`]: crate::opcode::OpArray::method_cache
#[inline(always)]
#[allow(clippy::inline_always)] // the method call start's cache probe
pub(crate) fn method_entry_hit(
    code: &crate::opcode::OpArray,
    slot: usize,
    cls_addr: usize,
    scope_addr: usize,
    lit: u32,
) -> Option<(Func, Option<Rc<Class>>, bool)> {
    let other_class = {
        let cache = code.method_cache.borrow();
        match cache.get(slot) {
            Some(Some(cached)) if cached.class_addr == cls_addr => {
                return cached_method(cached, scope_addr, lit);
            }
            Some(Some(_)) => true,
            _ => false,
        }
    };
    if other_class && promote_method_entry(code, slot, cls_addr) {
        let cache = code.method_cache.borrow();
        return cached_method(cache.get(slot)?.as_ref()?, scope_addr, lit);
    }
    None
}

/// [`method_entry_hit`]'s test of an entry filled for the right class.
#[inline(always)]
#[allow(clippy::inline_always)]
fn cached_method(
    cached: &crate::opcode::MethodCallCacheEntry,
    scope_addr: usize,
    lit: u32,
) -> Option<(Func, Option<Rc<Class>>, bool)> {
    if cached.scope_addr != scope_addr || cached.name_lit != Some(lit) {
        return None;
    }
    cached.func.get().map(|func| {
        (
            func,
            cached
                .method_scope
                .as_ref()
                .and_then(std::rc::Weak::upgrade),
            cached.is_static,
        )
    })
}

/// Where call site `slot` keeps its second method cache entry: past the
/// first entries of every op.
fn second_method_slot(code: &crate::opcode::OpArray, slot: usize) -> usize {
    code.ops.len() + slot
}

/// Swaps call site `slot`'s two method cache entries when the second one
/// was filled for the class at `cls_addr` (see [`method_entry_hit`]).
#[inline(never)]
fn promote_method_entry(code: &crate::opcode::OpArray, slot: usize, cls_addr: usize) -> bool {
    let Ok(mut cache) = code.method_cache.try_borrow_mut() else {
        return false;
    };
    let second = second_method_slot(code, slot);
    match cache.get(second) {
        Some(Some(e)) if e.class_addr == cls_addr => {
            cache.swap(slot, second);
            true
        }
        _ => false,
    }
}

/// Appends the pending call `make` builds to `calls`, written straight
/// into the vector's spare slot: `extend` of a one-element trusted-length
/// iterator reserves the room before `make` runs, so the call is not built
/// on the native stack and copied in afterwards (as `Vec::push` of a built
/// value is, with an out-of-line `memcpy` of the whole `PendingCall`).
#[inline(always)]
#[allow(clippy::inline_always)] // the point is building in place
fn push_call_in_place(calls: &mut Vec<PendingCall>, make: impl FnOnce() -> PendingCall) {
    calls.extend(std::iter::once_with(make));
}

/// `ZEND_INIT_NS_FCALL_BY_NAME` on the running frame `frame` (an
/// unqualified function called from namespaced code) when the run-time
/// cache resolves it, looked up as [`Vm::op_init_fcall`] looks it up: the
/// call site's own slot `site` (its global fallback, cached there), else
/// the namespaced name's. The pending call is not a direct one. Returns
/// `false`, having changed nothing, on a miss (the generic op resolves
/// the name and fills the cache). Frame-local, like [`init_fcall_in`], so
/// the tape starts such calls without leaving.
pub(crate) fn init_ns_fcall_in(frame: &mut Frame, name: u32, site: usize) -> bool {
    let cached = {
        let cache = frame.code.fcall_cache.borrow();
        match cache.get(site).and_then(Option::as_ref) {
            Some(entry) => entry.get(),
            None => cache
                .get(name as usize)
                .and_then(|c| c.as_ref().and_then(crate::opcode::CachedFunc::get)),
        }
    };
    let Some(func) = cached else {
        return false;
    };
    push_call_in_place(&mut frame.calls, move || PendingCall::new(func));
    true
}

/// [`Vm::init_fcall_fast`] on the running frame `frame`: `ZEND_INIT_FCALL`
/// of a call site without a global fallback whose function is in the
/// run-time cache, the pending call built in place in `frame.calls`.
/// Returns `false`, having changed nothing, on a cache miss. Frame-local,
/// so the dispatch loop's hot block (`exec/hot.rs`) runs it as well.
#[inline(always)]
#[allow(clippy::inline_always)] // one cache probe and an in-place push per call site
pub(crate) fn init_fcall_in(frame: &mut Frame, name: u32) -> bool {
    let cached = frame
        .code
        .fcall_cache
        .borrow()
        .get(name as usize)
        .and_then(|c| c.as_ref().and_then(crate::opcode::CachedFunc::get));
    let Some(func) = cached else {
        return false;
    };
    push_call_in_place(&mut frame.calls, move || PendingCall {
        direct: true,
        ..PendingCall::new(func)
    });
    true
}

/// [`Vm::send_fast`] on the running frame `frame`: a positional by-value
/// argument (a literal, a temporary or a defined compiled variable of a
/// frame without an attached symbol table) into the pending call's inline
/// buffer. Returns `false`, having changed nothing, otherwise.
/// Frame-local, so the dispatch loop's hot block (`exec/hot.rs`) runs it
/// as well.
#[inline(always)]
#[allow(clippy::inline_always)] // one SEND per argument of every call
pub(crate) fn send_in(frame: &mut Frame, src: &ArgSrc) -> bool {
    let Some(call) = frame.calls.last_mut() else {
        return false;
    };
    let idx = call.inline_count as usize;
    if idx >= call.inline_args.len()
        || !call.args.is_empty()
        || call.has_named
        || call.magic_name.is_some()
        || sends_by_ref(&call.func, idx)
    {
        return false;
    }
    let cv = match src {
        ArgSrc::Val(Operand::Lit(i)) => {
            let Some(v) = frame.code.literals.get(*i as usize) else {
                return false;
            };
            call.inline_args[idx] = Some(clone_value(v));
            call.inline_count += 1;
            return true;
        }
        ArgSrc::Val(Operand::Tmp(t)) | ArgSrc::CallResult(Operand::Tmp(t)) => {
            let Some(slot) = frame.tmps.get_mut(*t as usize) else {
                return false;
            };
            let v = match std::mem::take(slot) {
                Value::Reference(r) => r.get(),
                v => v,
            };
            call.inline_args[idx] = Some(v);
            call.inline_count += 1;
            return true;
        }
        ArgSrc::Val(Operand::Cv(cv)) => *cv,
        ArgSrc::Place(pid) => {
            let plain = frame.code.places.get(*pid as usize).and_then(|p| {
                match (&p.base, p.segs.is_empty()) {
                    (crate::opcode::PlaceBase::Cv(cv), true) => Some(*cv),
                    _ => None,
                }
            });
            match plain {
                Some(cv) => cv,
                // (A place without segments that is not a variable is no
                // shape `send_place_hit` reads.)
                None => return send_place_in(frame, *pid, idx),
            }
        }
        ArgSrc::TempPlace(pid) => return send_place_in(frame, *pid, idx),
        _ => return false,
    };
    if frame.symtab_attached {
        return false;
    }
    let v = match frame.cvs.get(cv as usize).and_then(crate::vm::CvSlot::get) {
        Some(Value::Reference(r)) => r.get(),
        Some(v) => clone_value(v),
        None => return false,
    };
    call.inline_args[idx] = Some(v);
    call.inline_count += 1;
    true
}

/// The by-value send of place `pid` of `frame` (a property, an array
/// element, a chain of them, an element of a temporary) as inline
/// argument `idx` of the newest pending call, which [`send_in`] found
/// takes it by value: the specialised fetch ops' hit
/// ([`Frame::send_place_hit`]) instead of the place walk. `false`, having
/// changed nothing, otherwise.
#[inline(never)]
fn send_place_in(frame: &mut Frame, pid: crate::opcode::PlaceId, idx: usize) -> bool {
    let Some(v) = frame.send_place_hit(pid) else {
        return false;
    };
    // `send_in` checked the call and its slot; reading the place changes
    // no pending call.
    let Some(call) = frame.calls.last_mut() else {
        return false;
    };
    call.inline_args[idx] = Some(v);
    call.inline_count += 1;
    true
}

/// [`Func::arg_by_ref`] made inline for [`send_in`] (`ZEND_SEND_VAL`'s
/// `ARG_MUST_BE_SENT_BY_REF` test, once per argument of every call): the
/// out-of-line call cost ~26 instructions an argument. Same answer: the
/// parameter's by-reference flag, and past the declared parameters that
/// of a by-reference variadic.
#[inline(always)]
#[allow(clippy::inline_always)] // one test per SEND
fn sends_by_ref(func: &Func, n: usize) -> bool {
    let params: &[crate::opcode::ParamInfo] = match func {
        Func::User(u) => &u.params,
        Func::Builtin(b) => &b.params,
    };
    match params.get(n) {
        Some(p) => p.by_ref,
        None => params.last().is_some_and(|l| l.variadic && l.by_ref),
    }
}

fn install_buffer<T>(slot: &mut Vec<T>, buf: Vec<T>) {
    let old = std::mem::replace(slot, buf);
    if old.capacity() == 0 {
        std::mem::forget(old);
    }
}

/// Entries of a [`MethodCache`] (a power of two).
const METHOD_CACHE_SIZE: usize = 64;

/// Longest method name a [`MethodCache`] entry holds.
const METHOD_CACHE_NAME: usize = 31;

/// The run-time cache of `ZEND_INIT_METHOD_CALL` (`CACHED_POLYMORPHIC_PTR`
/// in `zend_vm_def.h`: the method `zend_std_get_method` found for the
/// object's class), shared by all call sites as a small direct-mapped
/// table keyed by (class of the object, calling scope, method name).
///
/// Classes are held weakly, for their identity only (a `Weak` keeps the
/// address from being reused), and the name as an inline copy, so that
/// the cache owns no PHP-visible string. The table is allocated with the
/// VM and never grows while code runs (`memory_get_usage()` deltas).
#[derive(Debug)]
pub(crate) struct MethodCache {
    entries: Vec<Option<MethodCacheEntry>>,
}

#[derive(Debug)]
struct MethodCacheEntry {
    class: std::rc::Weak<Class>,
    scope: Option<std::rc::Weak<Class>>,
    name_len: u8,
    name: [u8; METHOD_CACHE_NAME],
    method: Rc<Method>,
}

impl Default for MethodCache {
    fn default() -> Self {
        MethodCache {
            entries: (0..METHOD_CACHE_SIZE).map(|_| None).collect(),
        }
    }
}

impl MethodCache {
    #[inline]
    #[allow(clippy::cast_possible_truncation)] // a hash, truncated on purpose
    fn slot(cls: &Rc<Class>, name: &ZStr) -> usize {
        ((Rc::as_ptr(cls) as usize >> 4) ^ name.hash_value() as usize) & (METHOD_CACHE_SIZE - 1)
    }

    /// The method cached for `name` on an object of class `cls` called
    /// from `scope`.
    #[inline]
    pub(crate) fn get(
        &self,
        cls: &Rc<Class>,
        scope: Option<&Rc<Class>>,
        name: &ZStr,
    ) -> Option<Rc<Method>> {
        let e = self.entries.get(Self::slot(cls, name))?.as_ref()?;
        let same_scope = match (scope, &e.scope) {
            (None, None) => true,
            (Some(s), Some(w)) => std::ptr::eq(Rc::as_ptr(s), w.as_ptr()),
            _ => false,
        };
        let hit = std::ptr::eq(Rc::as_ptr(cls), e.class.as_ptr())
            && same_scope
            && e.name
                .get(..usize::from(e.name_len))
                .is_some_and(|n| n.eq_ignore_ascii_case(name.as_bytes()));
        hit.then(|| e.method.clone())
    }

    /// Remembers that `name` on an object of class `cls` called from
    /// `scope` is `method`. Longer names than an entry holds are not
    /// cached.
    pub(crate) fn insert(
        &mut self,
        cls: &Rc<Class>,
        scope: Option<&Rc<Class>>,
        name: &ZStr,
        method: &Rc<Method>,
    ) {
        let bytes = name.as_bytes();
        let Ok(name_len) = u8::try_from(bytes.len()) else {
            return;
        };
        if bytes.len() > METHOD_CACHE_NAME {
            return;
        }
        let mut copy = [0u8; METHOD_CACHE_NAME];
        copy[..bytes.len()].copy_from_slice(bytes);
        let slot = Self::slot(cls, name);
        if let Some(e) = self.entries.get_mut(slot) {
            *e = Some(MethodCacheEntry {
                class: Rc::downgrade(cls),
                scope: scope.map(Rc::downgrade),
                name_len,
                name: copy,
                method: method.clone(),
            });
        }
    }
}

/// The compiled-variable set-up of `i_init_func_execute_data`
/// (`Zend/zend_execute.c`) for a new user frame `f` whose `args` hold the
/// arguments as sent: fills the frame's CV slots in one pass, each passed
/// positional argument of a declared, non-variadic parameter moving
/// straight into its CV (where php-src's SEND ops put it, so
/// `RECV`/`RECV_INIT` only verify it, [`Vm::recv_bound`]) and every other
/// CV undefined (`zend_init_cvs`). A bound argument's `args` entry is left
/// `None` (`func_num_args()` still counts it, `func_get_args()` and
/// backtraces read the CV), as a `RECV` leaves it. Extra arguments and
/// those of a variadic parameter stay in `args`.
///
/// For a function without parameter types or when passed arguments satisfy
/// parameter types without coercion, the `RECV`/`RECV_INIT` ops of
/// the leading passed arguments are skipped ("Skip useless `ZEND_RECV` and
/// `ZEND_RECV_INIT` opcodes"): the returned first op is where executing
/// them would have led. A named-argument gap or type mismatch stops the
/// skipping (its op reports the missing argument, evaluates the default,
/// or executes the coercion/type check).
///
/// The frame's slots may be empty ([`Frame::reinit_for_call`]) or filled
/// with undefined values (a fresh [`Frame::new`]); parameter `i` is CV `i`
/// (the compiler's layout). A frame on a shared, mapped store (never a
/// function frame) is left alone.
fn init_cvs(uf: &UserFunction, f: &mut Frame) -> usize {
    let ncv = uf.code.cv_names.len();
    let ops = &uf.code.ops;
    let mut skipping = true;
    let mut first_op = 0;
    for (i, (arg, p)) in f.args.iter_mut().zip(&uf.params).enumerate() {
        if p.variadic || i == ncv {
            break;
        }
        match arg.take() {
            Some(v) => {
                let ok = match &p.ty {
                    None => true,
                    Some(ty) => ty.is_mixed() || accepts_without_coercion(ty.mask, &v),
                };
                if let Some(slot) = f.cvs.get_mut(i) {
                    slot.set(Some(v));
                }
                if skipping && ok {
                    match ops.get(first_op) {
                        Some(Op::Recv { n, .. }) if *n as usize == i => first_op += 1,
                        Some(Op::RecvInit { n, skip, .. }) if *n as usize == i => {
                            first_op = *skip as usize;
                        }
                        _ => skipping = false,
                    }
                } else {
                    skipping = false;
                }
            }
            None => {
                skipping = false;
            }
        }
    }
    first_op
}

/// Drops `v`, skipping the out-of-line `drop_in_place` call when it owns
/// no allocation (and so, for a sized element type, no elements).
#[inline]
pub(crate) fn forget_unallocated<T>(v: Vec<T>) {
    if v.capacity() == 0 && std::mem::size_of::<T>() != 0 {
        std::mem::forget(v);
    }
}

fn forwarded_called(cur: Option<Rc<Class>>, cls: &Rc<Class>, forward: bool) -> Rc<Class> {
    if forward {
        cur.filter(|c| c.is_subclass_of(cls))
            .unwrap_or_else(|| cls.clone())
    } else {
        cls.clone()
    }
}

/// Finds `::` in a callable string.
pub(crate) fn find_double_colon(s: &[u8]) -> Option<usize> {
    s.windows(2).position(|w| w == b"::")
}

/// Whether a user function's parameter of type `ty` takes `v` as it is
/// (`zend_check_type` without coercion): [`accepts_without_coercion`], or
/// an object of a class the type names (the class, a parent or an
/// interface, by name: nothing is loaded), for a union of class names
/// without `self`/`parent` and without intersections. The plain call paths
/// take only such arguments; a class-typed parameter (`Request $r`) sent
/// every call of its function the generic way.
#[inline]
pub(crate) fn param_accepts(ty: &crate::types::TypeDecl, v: &Value) -> bool {
    if ty.is_mixed() || accepts_without_coercion(ty.mask, v) {
        return true;
    }
    match v {
        Value::Object(o) if ty.intersections.is_empty() => {
            let class = object::class_of(o);
            // The names lower-cased as the class table keys them
            // (`TypeDecl::classes` keeps them as written).
            ty.class_refs().classes.iter().any(|c| match c {
                crate::types::ClassRef::Named(lc) => class.instance_of_name(lc.as_bytes()),
                crate::types::ClassRef::Scope | crate::types::ClassRef::Parent => false,
            })
        }
        _ => false,
    }
}

/// Whether `verify_type` would accept `v` for a type with mask `m` as-is:
/// the side-effect-free subset of `type_accepts` (no callable/class checks).
#[inline(always)]
pub(crate) fn accepts_without_coercion(m: u32, v: &Value) -> bool {
    use crate::types::mask;
    match v {
        Value::Int(_) => m & mask::LONG != 0,
        Value::Float(_) => m & mask::DOUBLE != 0,
        Value::Str(_) => m & mask::STRING != 0,
        Value::Bool(b) => {
            let bit = if *b { mask::TRUE } else { mask::FALSE };
            m & bit != 0
        }
        Value::Null => m & mask::NULL != 0,
        Value::Array(_) => m & (mask::ARRAY | mask::ITERABLE) != 0,
        Value::Object(_) => m & mask::OBJECT != 0,
        Value::Resource(_) => m & mask::RESOURCE != 0,
        Value::Reference(_) => false,
    }
}

/// The backtrace copy of a builtin's arguments (a plain loop: the iterator
/// chain this replaces is a dozen non-inlined calls per element in debug).
fn frame_args(values: &[Value], out: &mut Vec<Option<Value>>) {
    out.clear();
    for v in values {
        out.push(Some(v.clone()));
    }
}

/// The static method a `"Class::name"` string or a `['Class', 'name']`
/// array (`class`, `method`) names at the `ZEND_INIT_USER_CALL` whose
/// method cache slot is `slot` in the running frame `f`, as
/// [`Vm::remember_user_call`] stored it (or as an `[$object, "name"]`
/// array of an object of that class stored it for a static method, which
/// resolves alike): the function, its declaring scope and the class, its
/// called scope. `None` on a miss: another class name (an alias too),
/// method or calling scope, and a calling frame whose `$this` makes the
/// call late-bound (`zend_is_callable_check_class`: a `$this` of the
/// calling scope's class, which is the named class or a subclass of it,
/// is the call's object and called scope).
pub(crate) fn user_call_static_hit(
    f: &Frame,
    slot: usize,
    class: &[u8],
    method: &[u8],
) -> Option<(Func, Option<Rc<Class>>, Rc<Class>)> {
    let class = class.strip_prefix(b"\\").unwrap_or(class);
    let cache = f.code.method_cache.try_borrow().ok()?;
    let e = cache.get(slot)?.as_ref()?;
    let scope_addr = f.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
    if !e.is_static || e.scope_addr != scope_addr || !e.name.as_bytes().eq_ignore_ascii_case(method)
    {
        return None;
    }
    let cls = e.class.upgrade()?;
    if Rc::as_ptr(&cls) as usize != e.class_addr || !cls.name.as_bytes().eq_ignore_ascii_case(class)
    {
        return None;
    }
    if let (Some(fs), Some(t)) = (&f.scope, &f.this) {
        if object::class_of(t).is_subclass_of(fs) && fs.is_subclass_of(&cls) {
            return None;
        }
    }
    let func = e.func.get()?;
    let scope = match &e.method_scope {
        Some(w) => Some(w.upgrade()?),
        None => None,
    };
    Some((func, scope, cls))
}

/// [`user_call_static_hit`] at the running `ZEND_INIT_USER_CALL` (its
/// method cache slot the op's index: the frame's `ip` is past it) as the
/// pending call [`Vm::user_call_cached`] returns.
fn user_call_static_call(f: &Frame, class: &[u8], method: &[u8]) -> Option<PendingCall> {
    let (func, scope, called) = user_call_static_hit(f, f.ip.checked_sub(1)?, class, method)?;
    Some(PendingCall {
        scope,
        called: Some(called),
        dynamic: true,
        ..PendingCall::new(func)
    })
}

/// `sodium_remove_param_values_from_backtrace()`: replaces the `args` of
/// every frame in the exception's `trace` with an empty array.
#[cold]
#[inline(never)]
fn scrub_trace_args(o: &Obj) {
    let class = object::class_of(o);
    let Some(slot) = class
        .props
        .iter()
        .find(|p| p.name.as_bytes() == b"trace")
        .map(|p| p.slot as usize)
    else {
        return;
    };
    let data = object::data(o);
    let mut slots = data.slots.borrow_mut();
    let Some(Some(Value::Array(trace))) = slots.get_mut(slot) else {
        return;
    };
    let key = ArrayKey::from("args");
    for (_, frame) in trace.make_mut().iter_mut() {
        if let Value::Array(frame) = frame {
            if frame.contains_key(&key) {
                frame.make_mut().insert("args", Value::Array(ZArray::new()));
            }
        }
    }
}

// ----------------------------------------------------------------------
// `call_user_func()` / `call_user_func_array()` written by name
// ----------------------------------------------------------------------

impl Vm {
    /// `ZEND_INIT_USER_CALL` (`Zend/zend_vm_def.h`): resolves the callable
    /// `callee` like the builtin's `Z_PARAM_FUNC` (`zend_is_callable`, in
    /// the calling frame's scope, with the callable deprecations) and
    /// starts a dynamic call of it (`ZEND_CALL_DYNAMIC`). A value that is
    /// not callable is the builtin's `TypeError` for argument 1, named
    /// after `call_user_func` or (`array`) `call_user_func_array`.
    ///
    /// The call site remembers a function named by a string and a method
    /// of an `[$object, "name"]` array (by the object's class and the
    /// calling scope), so a site called again with the same kind of
    /// callable skips the resolution.
    pub(crate) fn op_init_user_call(&mut self, callee: Operand, array: bool) -> VmResult<()> {
        let cb = self.read(callee)?;
        if let Some(call) = self.user_call_cached(&cb) {
            self.push_pending(call);
            return Ok(());
        }
        let mut call = self.user_call_resolve(&cb, array)?;
        call.dynamic = true;
        self.push_pending(call);
        Ok(())
    }

    /// [`Vm::op_init_user_call`] run by the tape (`exec/tape.rs`), the op
    /// at `at` in the current frame: the call it would push, returned
    /// instead, for the tape to prepare (or push). The frame's resume point
    /// is stored first (`SAVE_OPLINE()`: the "Use of "self" in callables"
    /// deprecation, an undefined variable's warning and the `TypeError`
    /// report this op's line, and an exception unwinds from it).
    pub(crate) fn user_call_pending(
        &mut self,
        callee: Operand,
        array: bool,
        at: usize,
    ) -> VmResult<PendingCall> {
        self.frame_mut().ip = at + 1;
        let cb = self.read(callee)?;
        if let Some(call) = self.user_call_cached(&cb) {
            return Ok(call);
        }
        let mut call = self.user_call_resolve(&cb, array)?;
        call.dynamic = true;
        Ok(call)
    }

    /// The call site's remembered resolution of `cb` ([`Vm::op_init_user_call`]),
    /// as the pending call `zend_is_callable` would make of it: a plain
    /// function named by a string (any case, one leading `\`), or the
    /// method an `[$object, "name"]` array named for the same class and
    /// calling scope. `None` on a miss.
    #[inline]
    fn user_call_cached(&self, cb: &Value) -> Option<PendingCall> {
        let f = self.frame();
        match cb {
            Value::Str(s) => {
                let name = s.as_bytes();
                if let Some(p) = find_double_colon(name) {
                    return user_call_static_call(f, &name[..p], &name[p + 2..]);
                }
                let slot = f.code.literals.len() + f.ip;
                let cache = f.code.fcall_cache.borrow();
                let func = cache.get(slot)?.as_ref()?.get()?;
                let name = name.strip_prefix(b"\\").unwrap_or(name);
                if !func.name().as_bytes().eq_ignore_ascii_case(name) {
                    return None;
                }
                let mut call = PendingCall::new(func);
                call.dynamic = true;
                Some(call)
            }
            Value::Array(a) if a.len() == 2 => {
                let (Some(Value::Object(o)), Some(Value::Str(m))) = (
                    a.get(&ArrayKey::Int(0)).map(Value::deref),
                    a.get(&ArrayKey::Int(1)).map(Value::deref),
                ) else {
                    return match (a.get(&ArrayKey::Int(0)), a.get(&ArrayKey::Int(1))) {
                        (Some(Value::Str(c)), Some(Value::Str(m))) => {
                            user_call_static_call(f, c.as_bytes(), m.as_bytes())
                        }
                        _ => None,
                    };
                };
                let cls = object::class_of(&o);
                let slot = f.ip.checked_sub(1)?;
                let cache = f.code.method_cache.borrow();
                let e = cache.get(slot)?.as_ref()?;
                let scope_addr = f.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
                if e.class_addr != Rc::as_ptr(cls) as usize
                    || e.scope_addr != scope_addr
                    || !e.name.eq_ignore_ascii_case(m.as_bytes())
                {
                    return None;
                }
                let func = e.func.get()?;
                let scope = match &e.method_scope {
                    Some(w) => Some(w.upgrade()?),
                    None => None,
                };
                let mut call = PendingCall {
                    this: (!e.is_static).then(|| o.clone()),
                    scope,
                    called: Some(cls.clone()),
                    ..PendingCall::new(func)
                };
                call.dynamic = true;
                Some(call)
            }
            _ => None,
        }
    }

    /// [`Vm::op_init_user_call`] on a cache miss: `zend_is_callable`, its
    /// failure as the builtin's `TypeError`, and a resolution worth
    /// remembering stored in the call site's cache.
    #[inline(never)]
    fn user_call_resolve(&mut self, cb: &Value, array: bool) -> VmResult<PendingCall> {
        let fname = if array {
            "call_user_func_array"
        } else {
            "call_user_func"
        };
        let call = match self.zend_is_callable(cb, false) {
            Ok(Ok(call)) => call,
            Ok(Err(reason)) => {
                return Err(self.make_throw(
                    "TypeError",
                    &format!(
                        "{fname}(): Argument #1 ($callback) must be a valid callback, {reason}"
                    ),
                ))
            }
            // An error handler threw on the "Use of "self" in callables"
            // deprecation: `zend_is_callable_check_class` fails, and the
            // opcode's `TypeError` gets the exception as its previous one.
            Err(Throw::Exception(prev)) if names_callable_keyword(cb) => {
                return Err(cuf_keyword_error(self, fname, &prev));
            }
            Err(t) => return Err(t),
        };
        self.remember_user_call(cb, &call);
        Ok(call)
    }

    /// Stores the resolution `call` of `cb` in the running
    /// `ZEND_INIT_USER_CALL`'s cache slots when [`Vm::user_call_cached`]
    /// can rebuild it: a function found by its name (no class, no `$this`,
    /// no trampoline), or a method an `[$object, "name"]` array found
    /// directly (bound to that object, or static, its called class the
    /// object's).
    fn remember_user_call(&self, cb: &Value, call: &PendingCall) {
        if call.magic_name.is_some() || call.closure.is_some() || call.pfa.is_some() {
            return;
        }
        let f = self.frame();
        match cb {
            Value::Str(s) => {
                let name = s.as_bytes();
                if let Some(p) = find_double_colon(name) {
                    self.remember_user_call_static(&name[..p], &name[p + 2..], call);
                    return;
                }
                let name = name.strip_prefix(b"\\").unwrap_or(name);
                if call.this.is_some()
                    || call.scope.is_some()
                    || call.called.is_some()
                    || !call.func.name().as_bytes().eq_ignore_ascii_case(name)
                {
                    return;
                }
                let code = &f.code;
                let slot = code.literals.len() + f.ip;
                let mut cache = code.fcall_cache.borrow_mut();
                if cache.len() <= slot {
                    let size = (code.literals.len() + code.ops.len()).max(slot + 1);
                    crate::rtcache::grow_slots(&mut cache, size, || None);
                }
                cache[slot] = Some(crate::opcode::CachedFunc::new(&call.func));
            }
            Value::Array(a) if a.len() == 2 => {
                let (Some(Value::Object(o)), Some(Value::Str(m))) = (
                    a.get(&ArrayKey::Int(0)).map(Value::deref),
                    a.get(&ArrayKey::Int(1)).map(Value::deref),
                ) else {
                    if let (Some(Value::Str(c)), Some(Value::Str(m))) =
                        (a.get(&ArrayKey::Int(0)), a.get(&ArrayKey::Int(1)))
                    {
                        self.remember_user_call_static(c.as_bytes(), m.as_bytes(), call);
                    }
                    return;
                };
                let cls = object::class_of(&o);
                if find_double_colon(m.as_bytes()).is_some()
                    || cls.hooks.as_ref().and_then(|h| h.get_method).is_some()
                    || !call
                        .func
                        .name()
                        .as_bytes()
                        .eq_ignore_ascii_case(m.as_bytes())
                    || !call.called.as_ref().is_some_and(|c| Rc::ptr_eq(c, cls))
                    || call.this.as_ref().is_some_and(|t| !Obj::ptr_eq(t, &o))
                {
                    return;
                }
                // Only a method of the class itself: one an object's
                // `get_method` adds (a PDO driver's) depends on the object.
                let declared = cls
                    .methods
                    .get(&m.to_ascii_lowercase())
                    .is_some_and(|d| same_func(&d.func, &call.func));
                let Some(slot) = f.ip.checked_sub(1).filter(|_| declared) else {
                    return;
                };
                let entry = MethodCallCacheEntry {
                    class_addr: Rc::as_ptr(cls) as usize,
                    scope_addr: f.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize),
                    class: Rc::downgrade(cls),
                    method_scope: call.scope.as_ref().map(Rc::downgrade),
                    func: crate::opcode::CachedFunc::new(&call.func),
                    is_static: call.this.is_none(),
                    name_lit: None,
                    name: m.clone(),
                };
                let Ok(mut cache) = f.code.method_cache.try_borrow_mut() else {
                    return;
                };
                if cache.len() <= slot {
                    let size = f.code.ops.len().max(slot + 1);
                    crate::rtcache::grow_slots(&mut cache, size, || None);
                }
                cache[slot] = Some(entry);
            }
            _ => {}
        }
    }

    /// [`Vm::remember_user_call`] of a static method named by a
    /// `"Class::name"` string or a `['Class', 'name']` array (`class` and
    /// `method` its two parts): remembered in the op's slot of the method
    /// cache, as a static method of the class itself (its name, not an
    /// alias, and a method it declares), when the call has no `$this` and
    /// the class is its called scope. [`user_call_static_hit`] judges a
    /// later call again for the late static binding the calling frame's
    /// `$this` may bring in.
    fn remember_user_call_static(&self, class: &[u8], method: &[u8], call: &PendingCall) {
        let class = class.strip_prefix(b"\\").unwrap_or(class);
        let Some(cls) = call.called.as_ref() else {
            return;
        };
        if call.this.is_some()
            || find_double_colon(method).is_some()
            || !cls.name.as_bytes().eq_ignore_ascii_case(class)
            || !call.func.name().as_bytes().eq_ignore_ascii_case(method)
        {
            return;
        }
        let method = ZStr::from(method);
        let declared = cls
            .methods
            .get(&method.to_ascii_lowercase())
            .is_some_and(|d| d.flags & flags::STATIC != 0 && same_func(&d.func, &call.func));
        let f = self.frame();
        let Some(slot) = f.ip.checked_sub(1).filter(|_| declared) else {
            return;
        };
        let entry = MethodCallCacheEntry {
            class_addr: Rc::as_ptr(cls) as usize,
            scope_addr: f.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize),
            class: Rc::downgrade(cls),
            method_scope: call.scope.as_ref().map(Rc::downgrade),
            func: crate::opcode::CachedFunc::new(&call.func),
            is_static: true,
            name_lit: None,
            name: method,
        };
        let Ok(mut cache) = f.code.method_cache.try_borrow_mut() else {
            return;
        };
        if cache.len() <= slot {
            let size = f.code.ops.len().max(slot + 1);
            crate::rtcache::grow_slots(&mut cache, size, || None);
        }
        cache[slot] = Some(entry);
    }

    /// `ZEND_SEND_USER`: argument `n` (1-based) of the pending
    /// `call_user_func()` call, by value (`GET_OP1_ZVAL_PTR_DEREF`). A
    /// parameter that must be passed by reference gets a reference to a
    /// copy, after the "must be passed by reference, value given" warning
    /// (`zend_param_must_be_ref`); one that prefers a reference gets the
    /// value.
    pub(crate) fn op_send_user(&mut self, src: Operand, n: u32) -> VmResult<()> {
        let v = self.read(src)?;
        let i = (n as usize).saturating_sub(1);
        let (by_ref, must) = {
            let call = self.frame().calls.last().expect("pending call");
            let by_ref = !call.is_magic_trampoline() && call.func.arg_by_ref(i);
            (by_ref, by_ref && !prefers_ref(&call.func, i))
        };
        if !by_ref {
            return self.op_send_val(v);
        }
        let v = if must {
            self.param_must_be_ref(i)?;
            Value::Reference(PhpRef::new(v))
        } else {
            v
        };
        self.push_arg(Some(i), None, v)
    }

    /// `ZEND_SEND_ARRAY`: the elements of the array `src` as the arguments
    /// of the pending `call_user_func_array()` call: string keys are
    /// named arguments (`zend_handle_named_arg`), a reference in the array
    /// is passed as it is to a by-reference parameter and dereferenced for
    /// a by-value one (kept for a `__call` trampoline), and a value for a
    /// parameter that must be passed by reference is passed in a new
    /// reference after the "must be passed by reference, value given"
    /// warning. With `len` not [`Operand::Unused`], the elements are
    /// those `array_slice($args, skip, len)` would give, all positional.
    pub(crate) fn op_send_array(&mut self, src: Operand, len: Operand, skip: u32) -> VmResult<()> {
        let ht = match self.read(src)? {
            Value::Array(ht) => ht,
            other => {
                // `FREE_OP2`: a temporary length is released unread.
                if let Operand::Tmp(t) = len {
                    drop(std::mem::take(&mut self.frame_mut().tmps[t as usize]));
                }
                return Err(self.make_throw(
                    "TypeError",
                    &format!(
                        "call_user_func_array(): Argument #2 ($args) must be of type array, {} given",
                        given_type(&other)
                    ),
                ));
            }
        };
        if len != Operand::Unused {
            return self.send_array_slice(&ht, len, skip);
        }
        let mut have_named = false;
        for (k, arg) in ht.iter() {
            let (pos, name) = match k {
                ArrayKey::Str(name) => {
                    have_named = true;
                    (self.arg_position(name)?, Some(name.clone()))
                }
                ArrayKey::Int(_) => {
                    if have_named {
                        return Err(self.make_throw(
                            "Error",
                            "Cannot use positional argument after named argument",
                        ));
                    }
                    let call = self.frame().calls.last().expect("pending call");
                    (Some(call.arg_count()), None)
                }
            };
            let i = pos.unwrap_or_else(|| {
                let call = self.frame().calls.last().expect("pending call");
                call.func.params().len().saturating_sub(1)
            });
            let (v, by_ref) = self.send_array_value(arg, i)?;
            if name.is_none() && !by_ref {
                self.op_send_val(v)?;
            } else {
                self.push_arg(pos, name, v)?;
            }
        }
        Ok(())
    }

    /// The `array_slice($args, skip, len)` form of [`Vm::op_send_array`]
    /// (`zend_compile_func_cufa` with a literal offset): `len` an int, or
    /// `null` for the rest; anything else is converted as `array_slice()`'s
    /// `?int $length` would be, its `TypeError` named after it.
    fn send_array_slice(&mut self, ht: &ZArray, len: Operand, skip: u32) -> VmResult<()> {
        let lv = self.read(len)?;
        let count = ht.len();
        let skip = skip as usize;
        let mut len = match lv {
            Value::Int(l) => l,
            Value::Null => i64::try_from(count.wrapping_sub(skip)).unwrap_or(i64::MAX),
            other => {
                let parsed = if self.frame().code.strict_types {
                    None
                } else {
                    self.parse_arg_long_weak(&other)?
                };
                match parsed {
                    Some(l) => l,
                    None => {
                        return Err(self.make_throw(
                            "TypeError",
                            &format!(
                            "array_slice(): Argument #3 ($length) must be of type ?int, {} given",
                            given_type(&other)
                        ),
                        ))
                    }
                }
            }
        };
        if skip >= count {
            return Ok(());
        }
        let rest = i64::try_from(count - skip).unwrap_or(i64::MAX);
        if len < 0 {
            len += rest;
        }
        if len <= 0 {
            return Ok(());
        }
        let len = usize::try_from(len.min(rest)).unwrap_or(usize::MAX);
        for (i, (_, arg)) in ht.iter().skip(skip).take(len).enumerate() {
            let (v, by_ref) = self.send_array_value(arg, i)?;
            if by_ref {
                self.push_arg(Some(i), None, v)?;
            } else {
                self.op_send_val(v)?;
            }
        }
        Ok(())
    }

    /// The value `ZEND_SEND_ARRAY` passes for array element `arg` as
    /// argument `i` (0-based; the variadic parameter past the declared
    /// ones) of the pending call, and whether the parameter is
    /// by-reference (`ARG_SHOULD_BE_SENT_BY_REF`; never for a `__call`
    /// trampoline).
    fn send_array_value(&mut self, arg: &Value, i: usize) -> VmResult<(Value, bool)> {
        let (trampoline, by_ref, prefer) = {
            let call = self.frame().calls.last().expect("pending call");
            (
                call.is_magic_trampoline(),
                call.func.arg_by_ref(i),
                prefers_ref(&call.func, i),
            )
        };
        if trampoline {
            // "don't separate references for __call"
            return Ok((arg.clone(), false));
        }
        if !by_ref {
            return Ok((arg.deref(), false));
        }
        if arg.is_reference() || prefer {
            return Ok((arg.clone(), true));
        }
        // By-value send is not allowed: a warning, but the call is made.
        self.param_must_be_ref(i)?;
        Ok((Value::Reference(PhpRef::new(arg.clone())), true))
    }

    /// `zend_param_must_be_ref` for argument `i` (0-based) of the pending
    /// call: "f(): Argument #1 ($x) must be passed by reference, value
    /// given".
    #[cold]
    fn param_must_be_ref(&mut self, i: usize) -> VmResult<()> {
        let msg = {
            let call = self.frame().calls.last().expect("pending call");
            self.must_be_ref_message(call, i)
        };
        self.warning(&msg)
    }

    /// The text of `zend_param_must_be_ref` for argument `i` (0-based) of
    /// `call`: the function as `get_function_or_method_name` names it
    /// (`Closure::__invoke` for a closure called through `__invoke`), and
    /// the parameter's name unless it is the variadic one.
    fn must_be_ref_message(&self, call: &PendingCall, i: usize) -> String {
        let params = call.func.params();
        let pname = match params.get(i) {
            Some(p) if !p.variadic => format!(" (${})", p.name),
            _ => String::new(),
        };
        let fname = if call
            .magic_name
            .as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case(b"__invoke"))
            && call.func.is_closure()
        {
            "Closure::__invoke".to_string()
        } else {
            self.func_display(&call.func, call.scope.as_ref())
        };
        format!(
            "{fname}(): Argument #{}{pname} must be passed by reference, value given",
            i + 1
        )
    }
}

/// Whether the callable's class part (of `"C::m"` or `["C", "m"]`) is `self`, `parent` or `static`.
fn names_callable_keyword(cb: &Value) -> bool {
    let class = match cb.deref() {
        Value::Str(s) => {
            let b = s.as_bytes();
            match b.windows(2).rposition(|w| w == b"::") {
                Some(p) => b[..p].to_ascii_lowercase(),
                None => return false,
            }
        }
        Value::Array(a) => match a.get(&ArrayKey::Int(0)).map(Value::deref) {
            Some(Value::Str(c)) => c.as_bytes().to_ascii_lowercase(),
            _ => return false,
        },
        _ => return false,
    };
    matches!(class.as_slice(), b"self" | b"parent" | b"static")
}

/// The `TypeError` of `ZEND_INIT_USER_CALL` when the "Use of \"self\" in
/// callables" deprecation threw `prev`.
#[cold]
fn cuf_keyword_error(vm: &mut Vm, fname: &str, prev: &Obj) -> Throw {
    let e = vm.make_throw(
        "TypeError",
        &format!(
            "{fname}(): Argument #1 ($callback) must be a valid callback, \
             cannot use \"self\"/\"parent\"/\"static\" in callables"
        ),
    );
    if let Throw::Exception(o) = &e {
        crate::exceptions::chain_previous(o, prev);
    }
    e
}

/// Whether `a` and `b` are the same function.
fn same_func(a: &Func, b: &Func) -> bool {
    match (a, b) {
        (Func::User(x), Func::User(y)) => Rc::ptr_eq(x, y),
        (Func::Builtin(x), Func::Builtin(y)) => Rc::ptr_eq(x, y),
        _ => false,
    }
}

/// Whether argument `i` (0-based) of `func` is sent by reference only when
/// it can be (`ZEND_SEND_PREFER_REF`, `ARG_MAY_BE_SENT_BY_REF`).
fn prefers_ref(func: &Func, i: usize) -> bool {
    let params = func.params();
    params
        .get(i)
        .or_else(|| params.last().filter(|p| p.variadic))
        .is_some_and(|p| p.promote & crate::opcode::PREFER_REF != 0)
}

#[cfg(test)]
mod buffer_tests {
    use super::*;
    use crate::VmOptions;

    /// A literal default is the stub's value; `UNKNOWN` (`gen_stub`'s "the
    /// function works it out") is no default, also when a script defined
    /// a constant of that name (php-src never looks it up).
    #[test]
    fn builtin_defaults_literal_and_unknown() {
        let mut vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
        assert_eq!(
            vm.run_script(b"<?php define('UNKNOWN', 7);", "unknown.php"),
            0
        );
        let builtin = |vm: &Vm, name: &str| match vm.functions.get(&ZStr::from(name)) {
            Some(Func::Builtin(b)) => b.clone(),
            _ => panic!("{name} is a core builtin"),
        };
        let mt_rand = builtin(&vm, "mt_rand");
        assert!(vm.builtin_default(&mt_rand, 0).is_none());
        assert!(vm.builtin_default(&mt_rand, 1).is_none());
        let round = builtin(&vm, "round");
        assert!(matches!(vm.builtin_default(&round, 1), Some(Value::Int(0))));
        assert!(vm.builtin_default(&round, 0).is_none(), "required");
        assert!(vm.builtin_default(&round, 9).is_none(), "no such parameter");
    }

    fn capacities(vm: &Vm) -> Vec<usize> {
        let mut capacities: Vec<_> = vm.call_bufs.args.iter().map(Vec::capacity).collect();
        capacities.sort_unstable();
        capacities
    }

    #[test]
    fn builtin_backtrace_buffers_survive_coercion_named_args_and_callbacks() {
        let mut vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
        let before = capacities(&vm);
        let status = vm.run_script(
            br#"<?php
strlen("abc");
strlen(123);
strlen(string: "abc");
set_error_handler(function() { debug_backtrace(); });
trigger_error("callback");
restore_error_handler();
"#,
            "buffers.php",
        );
        assert_eq!(status, 0);
        assert_eq!(capacities(&vm), before);
        assert!(vm.call_bufs.args.iter().all(Vec::is_empty));
    }

    #[test]
    fn builtin_error_cleanup_recycles_backtrace_buffers_and_drops_values() {
        let mut vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
        let before = capacities(&vm);
        let held = ZStr::from("backtrace argument");
        let mut frame = Frame::with_vars(
            vm.empty_code.clone(),
            FrameKind::Builtin,
            vm.empty_vars.clone(),
            None,
        );
        frame.args = vm.call_bufs.args.pop().expect("prewarmed buffer");
        frame.args.push(Some(Value::Str(held.clone())));
        vm.frames.push(frame);
        let depth = vm.frames.len();
        vm.pop_frames_to(depth);
        assert_eq!(capacities(&vm), before);
        assert_eq!(held.refcount(), 1);
        assert!(vm.frames.is_empty());
    }

    #[test]
    fn smaller_callback_buffers_do_not_displace_prewarmed_capacity() {
        let mut vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
        let before = capacities(&vm);
        let mut borrowed = vm.call_bufs.args.pop().expect("prewarmed buffer");
        let held = ZStr::from("temporary argument");
        borrowed.push(Some(Value::Str(held.clone())));
        vm.put_arg_vec(vec![Some(Value::Str(held.clone()))]);
        vm.put_arg_vec(borrowed);
        assert_eq!(capacities(&vm), before);
        assert_eq!(held.refcount(), 1);
        assert!(vm.call_bufs.args.iter().all(Vec::is_empty));
    }

    #[test]
    fn popping_a_builtin_frame_releases_frame_values_before_arguments() {
        let mut vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
        let before = capacities(&vm);
        let arg = ZStr::from("argument");
        let tmp = ZStr::from("temporary");
        let mut frame = Frame::with_vars(
            vm.empty_code.clone(),
            FrameKind::Builtin,
            vm.empty_vars.clone(),
            None,
        );
        frame.args = vm.call_bufs.args.pop().expect("prewarmed buffer");
        frame.args.push(Some(Value::Str(arg.clone())));
        frame.tmps.push(Value::Str(tmp.clone()));
        vm.frames.push(frame);
        let depth = vm.frames.len();
        vm.pop_frames_to(depth);
        assert!(vm.frames.is_empty());
        assert_eq!(capacities(&vm), before);
        assert_eq!(arg.refcount(), 1);
        assert_eq!(tmp.refcount(), 1);
    }

    /// `send_in`'s inline by-reference test answers as
    /// [`Func::arg_by_ref`] for every registered function and position,
    /// past the declared parameters too (variadics).
    #[test]
    fn sends_by_ref_matches_arg_by_ref() {
        let vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
        let mut by_ref = 0;
        for f in vm.functions.values() {
            for n in 0..f.params().len() + 2 {
                assert_eq!(sends_by_ref(f, n), f.arg_by_ref(n), "{} #{n}", f.name());
                by_ref += usize::from(sends_by_ref(f, n));
            }
        }
        assert!(by_ref > 0);
        let Some(f) = vm.functions.get(&ZStr::from("str_replace")) else {
            panic!("str_replace is a core builtin");
        };
        assert!(!sends_by_ref(f, 2));
        assert!(sends_by_ref(f, 3));
    }

    #[test]
    fn forget_unallocated_drops_allocated_vectors() {
        let held = ZStr::from("held");
        forget_unallocated(vec![Value::Str(held.clone())]);
        assert_eq!(held.refcount(), 1);
        forget_unallocated(Vec::<Value>::new());
        forget_unallocated(vec![(); 3]);
    }

    #[test]
    fn install_buffer_replaces_and_releases_the_old_buffer() {
        let held = ZStr::from("old buffer value");
        let mut slot: Vec<Option<Value>> = Vec::new();
        install_buffer(&mut slot, Vec::with_capacity(8));
        assert!(slot.is_empty());
        assert_eq!(slot.capacity(), 8);
        slot.push(Some(Value::Str(held.clone())));
        assert_eq!(held.refcount(), 2);
        install_buffer(&mut slot, vec![Some(Value::Int(1))]);
        assert_eq!(held.refcount(), 1);
        assert!(matches!(slot.as_slice(), [Some(Value::Int(1))]));
    }

    #[test]
    fn backtrace_refresh_preserves_capacity_and_releases_old_values() {
        let held = ZStr::from("old argument");
        let mut args = Vec::with_capacity(8);
        args.push(Some(Value::Str(held.clone())));
        let capacity = args.capacity();
        frame_args(&[Value::Int(123)], &mut args);
        assert_eq!(args.capacity(), capacity);
        assert_eq!(held.refcount(), 1);
        assert!(matches!(args.as_slice(), [Some(Value::Int(123))]));
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod fast_builtin_tests {
    use super::fast_builtin_eval;
    use php_runtime::Value;

    fn float(name: &str, v: Value) -> f64 {
        match fast_builtin_eval(name.as_bytes(), &v) {
            Some(Value::Float(f)) => f,
            other => panic!("{name}: {other:?}"),
        }
    }

    /// Expectations from `/usr/bin/php8.5 -n` (`var_dump` of each call).
    #[test]
    fn float_math_matches_the_handlers() {
        assert_eq!(float("sqrt", Value::Int(2)), 1.414_213_562_373_095_1);
        assert!(float("sqrt", Value::Int(-1)).is_nan());
        let z = float("sqrt", Value::Float(-0.0));
        assert!(z == 0.0 && z.is_sign_negative());
        assert_eq!(float("sin", Value::Int(1)), 0.841_470_984_807_896_5);
        assert_eq!(float("cos", Value::Int(3)), -0.989_992_496_600_445_4);
        assert_eq!(float("tan", Value::Float(0.5)), 0.546_302_489_843_790_5);
        assert_eq!(float("exp", Value::Int(1)), std::f64::consts::E);
        assert_eq!(float("atan", Value::Int(1)), std::f64::consts::FRAC_PI_4);
        assert_eq!(float("floor", Value::Float(-2.5)), -3.0);
        assert_eq!(float("ceil", Value::Float(-2.5)), -2.0);
        assert_eq!(float("floor", Value::Int(7)), 7.0);
        let c = float("ceil", Value::Float(-0.5));
        assert!(c == 0.0 && c.is_sign_negative());
        assert_eq!(
            float("asinh", Value::Float(0.044_318_560_479_840_056)),
            0.044_304_065_350_020_95
        );
        assert_eq!(
            float("deg2rad", Value::Int(90)),
            std::f64::consts::FRAC_PI_2
        );
        assert_eq!(float("rad2deg", Value::Int(1)), 57.295_779_513_082_32);
        assert_eq!(float("expm1", Value::Float(1e-10)), 1.000_000_000_05e-10);
        assert_eq!(
            float("log1p", Value::Float(1e-10)),
            9.999_999_999_500_001e-11
        );
        assert_eq!(float("log10", Value::Int(1000)), 3.0);
        assert_eq!(float("tanh", Value::Float(0.5)), 0.462_117_157_260_009_74);
        assert_eq!(float("acos", Value::Float(0.5)), 1.047_197_551_196_597_9);
        // php-src master's `round()` (no pre-rounding; `/usr/bin/php8.5`).
        assert_eq!(float("round", Value::Float(0.499_999_999_999_999_94)), 0.0);
        let r = float("round", Value::Float(-0.4));
        assert!(r == 0.0 && r.is_sign_negative());
        assert_eq!(
            float("round", Value::Int(i64::MAX)),
            9.223_372_036_854_776e18
        );
        assert!(matches!(
            fast_builtin_eval(b"is_finite", &Value::Float(f64::INFINITY)),
            Some(Value::Bool(false))
        ));
        assert!(matches!(
            fast_builtin_eval(b"is_infinite", &Value::Float(f64::NEG_INFINITY)),
            Some(Value::Bool(true))
        ));
    }

    /// Anything needing coercion or a type error takes the full call.
    #[test]
    fn non_numbers_decline() {
        for name in ["sqrt", "sin", "floor", "ceil", "round", "log", "is_nan"] {
            assert!(fast_builtin_eval(name.as_bytes(), &Value::Bool(true)).is_none());
            assert!(fast_builtin_eval(name.as_bytes(), &Value::Null).is_none());
            assert!(fast_builtin_eval(name.as_bytes(), &Value::Str("4".into())).is_none());
        }
    }
}

/// Call-site caches of `INIT_METHOD_CALL`, `INIT_STATIC_METHOD_CALL` with
/// `self`/`parent`/`static`, and closure calls: scripts whose sites run
/// several times (fill, then hit). Expected output from `/usr/bin/php8.5 -n`.
#[cfg(test)]
#[allow(clippy::needless_raw_string_hashes)]
mod method_site_cache_tests {
    use crate::{Vm, VmOptions};
    use std::cell::RefCell;
    use std::io::Write;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct Sink(Rc<RefCell<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn run(src: &str) -> String {
        let sink = Sink::default();
        let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
        vm.run_script(src.as_bytes(), "t.php");
        drop(vm);
        let out = sink.0.borrow().clone();
        String::from_utf8_lossy(&out).into_owned()
    }

    /// Each `self::`/`parent::`/`static::` site runs several times, so later
    /// runs take the monomorphic slot: late static binding (the forwarded
    /// called scope), private access, `parent::` with `$this`, a trait
    /// method run for two using classes and a closure rebound to several
    /// scopes (the slot's class and scope keys), and the error of an instance
    /// method without `$this`.
    #[test]
    fn relative_static_calls_keep_late_static_binding_and_scope() {
        let src = r###"<?php
class A {
    public static function who() { return static::class; }
    private static function priv() { return "A::priv"; }
    public static function viaSelf() { return self::who() . "/" . self::priv(); }
    public static function viaStatic() { return static::who(); }
    public function inst() { return "inst:" . static::class . ":" . get_class($this); }
    public static function callInst() { return self::inst(); }
}
class B extends A {
    public static function who() { return "B!" . static::class; }
    public static function viaParent() { return parent::who(); }
    public function pinst() { return parent::inst(); }
}
class C extends B {}
trait T {
    public static function t() { return static::class; }
    public static function call() { return self::t() . "," . static::t(); }
}
class U1 { use T; }
class U2 { use T; }
for ($i = 0; $i < 3; $i++) {
    echo A::viaSelf(), " ", B::viaSelf(), " ", C::viaSelf(), "\n";
    echo A::viaStatic(), " ", B::viaStatic(), " ", C::viaStatic(), "\n";
    echo B::viaParent(), " ", C::viaParent(), "\n";
    echo (new B)->pinst(), " ", (new C)->pinst(), "\n";
    echo U1::call(), " ", U2::call(), "\n";
}
$f = function () { return self::who(); };
foreach ([A::class, B::class, C::class, A::class] as $cls) {
    echo Closure::bind($f, null, $cls)(), "\n";
}
for ($i = 0; $i < 3; $i++) {
    try {
        echo A::callInst(), "\n";
    } catch (Error $e) {
        echo get_class($e), ": ", $e->getMessage(), "\n";
    }
}
"###;
        let expected = r###"A/A::priv B/A::priv C/A::priv
A B!B B!C
B C
inst:B:B inst:C:C
U1,U1 U2,U2
A/A::priv B/A::priv C/A::priv
A B!B B!C
B C
inst:B:B inst:C:C
U1,U1 U2,U2
A/A::priv B/A::priv C/A::priv
A B!B B!C
B C
inst:B:B inst:C:C
U1,U1 U2,U2
A
B!B
B!C
A
Error: Non-static method A::inst() cannot be called statically
Error: Non-static method A::inst() cannot be called statically
Error: Non-static method A::inst() cannot be called statically
"###;
        assert_eq!(run(src), expected);
    }

    /// A cached `self::inst()` site whose `$this` changes class, then is
    /// missing or unrelated (the generic path raises the error); `$tmp->m()`
    /// and `$tmp->s()` on the last handle of an object (its destructor after
    /// the call, or before a static method's call), chains, and `$this->m()`
    /// in a loop.
    #[test]
    fn relative_instance_calls_and_temporary_receivers() {
        let src = r###"<?php
class D {
    public $n = "d";
    public function inst() { return "D::inst " . get_class($this) . " " . static::class; }
}
class E extends D {}
class X {}
$g = function () { return self::inst(); };
foreach ([new D, new E, new E, new D] as $o) {
    echo Closure::bind($g, $o, D::class)(), "\n";
}
foreach ([null, new X] as $o) {
    try {
        echo Closure::bind($g, $o, D::class)(), "\n";
    } catch (Error $e) {
        echo get_class($e), ": ", $e->getMessage(), "\n";
    }
}
class K {
    public function __construct(public $id) {}
    public function __destruct() { echo "dtor {$this->id}\n"; }
    public function m() { echo "m {$this->id}\n"; return $this; }
    public static function s() { echo "s\n"; }
    public function loop() {
        for ($i = 0; $i < 3; $i++) { $this->m(); $this->s(); }
    }
}
function mk($id) { return new K($id); }
for ($i = 0; $i < 3; $i++) {
    mk($i)->m();
    echo "after m\n";
    mk("s$i")->s();
    echo "after s\n";
    mk("c$i")->m()->m();
    echo "after chain\n";
}
(new K("loop"))->loop();
echo "end\n";
"###;
        let expected = r###"D::inst D D
D::inst E E
D::inst E E
D::inst D D
Error: Non-static method D::inst() cannot be called statically
Error: Non-static method D::inst() cannot be called statically
m 0
dtor 0
after m
dtor s0
s
after s
m c0
m c0
dtor c0
after chain
m 1
dtor 1
after m
dtor s1
s
after s
m c1
m c1
dtor c1
after chain
m 2
dtor 2
after m
dtor s2
s
after s
m c2
m c2
dtor c2
after chain
m loop
s
m loop
s
m loop
s
dtor loop
end
"###;
        assert_eq!(run(src), expected);
    }

    /// `$closure()` through the fast path: bound `$this` and scope, a
    /// by-reference `use`, static variables per closure object, recursion
    /// through a captured reference, static arrow functions, first-class
    /// callables of a builtin and of a method, and `func_get_args()`.
    #[test]
    fn closure_calls_bind_this_scope_uses_and_statics() {
        let src = r###"<?php
class P {
    private $secret = "p-secret";
    public function make() {
        $k = 0;
        return function ($x) use (&$k) {
            static $calls = 0;
            $calls++;
            $k += $x;
            return static::class . " " . $this->secret . " k=$k calls=$calls";
        };
    }
}
class Q extends P {}
$c = (new Q)->make();
for ($i = 1; $i <= 3; $i++) { echo $c($i), "\n"; }
$d = (new P)->make();
echo $d(10), "\n";
$fact = function ($n) use (&$fact) { return $n <= 1 ? 1 : $n * $fact($n - 1); };
echo $fact(6), "\n";
$s = static fn($v) => $v * 2;
for ($i = 0; $i < 3; $i++) { echo $s($i); }
echo "\n";
$fc = strlen(...);
echo $fc("abcd"), "\n";
$m = (new Q)->make(...);
echo get_class($m), " ", $m()(5), "\n";
$h = function () { return func_num_args() . ":" . implode(",", func_get_args()); };
for ($i = 0; $i < 2; $i++) { echo $h(1, 2, $i), "\n"; }
"###;
        let expected = r###"Q p-secret k=1 calls=1
Q p-secret k=3 calls=2
Q p-secret k=6 calls=3
P p-secret k=10 calls=1
720
024
4
Closure Q p-secret k=5 calls=1
3:1,2,0
3:1,2,1
"###;
        assert_eq!(run(src), expected);
    }

    /// `$this->m()` with the implicit `$this` operand (a method, closures
    /// nested in one) and with `FETCH_THIS` (a closure in a static method,
    /// a static closure, a static method, a top-level closure bound and
    /// not): cache hits with arguments, by-reference arguments, a private
    /// method, a static method, `__call`, `$this?->m()` and `$this?->m(...)`
    /// (no `JMP_NULL`: allowed with a callable conversion), first-class
    /// callables, generators, `Closure::bind()` refusing to unbind a
    /// closure whose only `$this` is a method call, the error of a nested
    /// closure that lost `$this` (PHP 8.3's error; the handler of 8.5 and
    /// master assumes an object, and `/usr/bin/php8.5` crashes on this
    /// script), and the destructor of an object whose last outside
    /// reference goes while a `$this->m()` call holds it. Expected output
    /// from `/usr/bin/php8.3 -n` with master's warning text.
    #[test]
    fn implicit_this_receivers_match_fetch_this() {
        let src = r###"<?php
class A {
    public $log = "";
    public function __construct(public $id) {}
    public function __destruct() { echo "dtor {$this->id}\n"; }
    public function g($x = 0) { return "g{$this->id}:$x"; }
    private function p() { return "p{$this->id}"; }
    public static function s() { return "s:" . static::class; }
    public function inc(&$r) { $r++; }
    public function __call($n, $a) { return "__call $n(" . implode(",", $a) . ")"; }
    public function run() {
        $out = [];
        $r = 0;
        for ($i = 0; $i < 3; $i++) {
            $out[] = $this->g($i) . " " . $this->p() . " " . $this->s() . " " . $this->nope($i);
            $this->inc($r);
        }
        $out[] = "r=$r";
        $out[] = $this?->g(9);
        $f = $this?->g(...);
        $out[] = $f(8);
        $h = $this->p(...);
        $out[] = $h();
        return implode("\n", $out);
    }
    public function nested() {
        return function () { return fn($y) => $this->g($y); };
    }
    public function direct() {
        return function () { return $this->g(1); };
    }
    public static function fromStatic() {
        return function () { return $this->g(2); };
    }
    public function staticClosure() {
        return static function () { return $this->g(3); };
    }
    public static function sm() { return $this->g(4); }
    public function gen() { for ($i = 0; $i < 2; $i++) { yield $this->g($i); } }
}
class B extends A {
    public function callPrivate() { return $this->p(); }
}
$a = new A("a");
echo $a->run(), "\n";
echo $a->nested()()(5), "\n";
$d = $a->direct();
echo $d(), "\n";
var_dump(Closure::bind($d, null, A::class));
$n = $a->nested();
$u = Closure::bind($n, null, A::class);
var_dump($u === null);
if ($u !== null) {
    try { echo $u()(6), "\n"; } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), " @", $e->getLine(), "\n"; }
}
foreach ([A::fromStatic(), $a->staticClosure()] as $c) {
    try { echo $c(), "\n"; } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), " @", $e->getLine(), "\n"; }
}
try { A::sm(); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), " @", $e->getLine(), "\n"; }
$top = function () { return $this->g(7); };
echo Closure::bind($top, new A("t"), A::class)(), "\n";
try { $top(); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), " @", $e->getLine(), "\n"; }
foreach ($a->gen() as $v) { echo $v, "\n"; }
try { (new B("b"))->callPrivate(); } catch (Error $e) { echo get_class($e), ": ", $e->getMessage(), " @", $e->getLine(), "\n"; }
class D {
    public function __destruct() { echo "dtor D\n"; }
    public function m() { global $o; $o = null; echo "in m\n"; $this->n(); echo "after n\n"; }
    public function n() { echo "n\n"; }
}
$o = new D;
$o->m();
echo "end\n";
"###;
        let expected = r###"ga:0 pa s:A __call nope(0)
ga:1 pa s:A __call nope(1)
ga:2 pa s:A __call nope(2)
r=3
ga:9
ga:8
pa
ga:5
ga:1

Warning: Cannot unbind $this of closure using $this, this will be an error in PHP 9 in t.php on line 49
NULL
bool(false)
Error: Using $this when not in object context @27
Error: Using $this when not in object context @33
Error: Using $this when not in object context @36
Error: Using $this when not in object context @38
dtor t
gt:7
Error: Using $this when not in object context @60
ga:0
ga:1
dtor b
in m
n
after n
dtor D
end
dtor a
"###;
        assert_eq!(run(src), expected);
    }
}

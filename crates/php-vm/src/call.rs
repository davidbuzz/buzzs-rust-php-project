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
    static NO_CTOR: Rc<Builtin> = Rc::new(Builtin {
        name: ZStr::from("__construct"),
        params: Vec::new(),
        defaults: Vec::new(),
        required: 0,
        return_type: None,
        flags: flags::NO_BODY,
        handler: |_vm, _args| Ok(Value::Null),
    });
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
    /// Display name of a function for messages (`A::m`, `foo`).
    pub(crate) fn func_display(&self, func: &Func, scope: Option<&Rc<Class>>) -> String {
        let name = func.name().to_string_lossy();
        if let Func::User(u) = func {
            if u.is_closure() && !name.starts_with("{closure:pfa:") {
                return name;
            }
        }
        match scope {
            Some(c) => format!("{}::{}", object::display_name(&c.name), name),
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
                cache.resize(size, None);
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
            cache.resize(size, None);
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
            cache.resize(size, None);
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
        let o = {
            let Some(frame) = self.frames.last() else {
                return false;
            };
            if frame.symtab_attached {
                return false;
            }
            match frame.cvs.get(cv as usize).and_then(crate::vm::CvSlot::get) {
                Some(Value::Object(o)) => o.clone(),
                _ => return false,
            }
        };
        self.push_closure_user_call(&o)
    }

    /// [`Vm::closure_user_call`] as a dynamic call (`$closure()`), the call
    /// built in place in `calls` ([`Vm::push_pending_with`]): only the
    /// handles the call takes from the closure are carried out of its
    /// data, not a whole `PendingCall` returned and pushed (two copies of
    /// 216 bytes). Returns `false`, having pushed nothing, when the object
    /// is not a closure of a user function.
    #[inline]
    fn push_closure_user_call(&mut self, o: &Obj) -> bool {
        if object::class_of(o).lc_name.as_bytes() != b"closure" {
            return false;
        }
        let parts = object::internal::<crate::closure::ClosureData, _>(o, |d| {
            if !matches!(&d.func, Func::User(_)) {
                return None;
            }
            Some((
                d.this.clone(),
                d.scope.clone(),
                d.called.clone(),
                if d.fake { None } else { Some(o.clone()) },
                d.magic_name.clone(),
                d.func.clone(),
            ))
        })
        .flatten();
        let Some((this, scope, called, closure, magic_name, func)) = parts else {
            return false;
        };
        self.push_pending_with(move || PendingCall {
            this,
            scope,
            called,
            closure,
            magic_name,
            dynamic: true,
            ..PendingCall::new(func)
        });
        true
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
                let Value::Str(m) = method else {
                    return Err(self.make_throw("Error", "Method name must be a string"));
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

    /// `ZEND_INIT_METHOD_CALL` of `$cv->name()` or `$tmp->name()` (`$this`
    /// fetched by `FetchThis`, a call result, ...) on a run-time cache hit,
    /// from the dispatch loop's hot-instruction block (no `SAVE_OPLINE`):
    /// the hit of [`Vm::init_cached_method_call`] without the generic
    /// operand read. The call's `$this` is a handle of its own, as there:
    /// a copy of the variable's object, or the temporary's, which is
    /// emptied as the read empties it. `slot` is the instruction's index
    /// (its cache slot). Returns `false`, having changed nothing, for an
    /// undefined variable, a reference or a value that is not an object, a
    /// variable of a frame with an attached symbol table, a class with a
    /// native `get_method` handler or a cache miss.
    #[inline]
    pub(crate) fn init_method_call_fast(&mut self, obj: Operand, lit: u32, slot: usize) -> bool {
        let Some(frame) = self.frames.last_mut() else {
            return false;
        };
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
            _ => return false,
        };
        let cls = object::class_of(o);
        if cls.hooks.as_ref().and_then(|h| h.get_method).is_some() {
            return false;
        }
        let cls_addr = Rc::as_ptr(cls) as usize;
        let scope_addr = frame.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
        let hit = match frame.code.method_cache.borrow().get(slot) {
            Some(Some(cached))
                if cached.class_addr == cls_addr
                    && cached.scope_addr == scope_addr
                    && cached.name_lit == Some(lit) =>
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
            }
            _ => None,
        };
        let Some((func, scope, is_static)) = hit else {
            return false;
        };
        let called = Rc::clone(cls);
        let this = if is_static { None } else { Some(o.clone()) };
        push_call_in_place(&mut frame.calls, move || PendingCall {
            this,
            scope,
            called: Some(called),
            ..PendingCall::new(func)
        });
        // The temporary's handle is the read's (`std::mem::take`): released
        // here, after the call took its own (the last handle of a static
        // method's object goes as it goes there, its destructor left to
        // the safe point after the instruction).
        if let Operand::Tmp(t) = obj {
            if let Some(slot) = frame.tmps.get_mut(t as usize) {
                drop(std::mem::take(slot));
            }
        }
        true
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

        let hit = if let Some(Some(cached)) = f.code.method_cache.borrow().get(slot) {
            if cached.class_addr == cls_addr
                && cached.scope_addr == scope_addr
                && cached.name_lit == Some(lit)
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
        };

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
            cache.resize(size, None);
        }
        cache[slot] = Some(entry);
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
                    cache.resize(size, None);
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
                cache.resize(size, None);
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
                    object::display_name(&m.scope().name),
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
                                object::display_name(&m.scope().name),
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
        // The op's run-time cache slot (see `rtcache`).
        if let Some(hit) = self.static_call_hit(class, name) {
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
        let call = PendingCall {
            this: if is_static { None } else { Some(o) },
            scope,
            called: Some(cls),
            is_ctor: true,
            ..PendingCall::new(ctor_func)
        };
        self.push_pending(call);
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
                    object::display_name(&ctor.scope().name),
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
        let Some(call) = frame.calls.last_mut() else {
            return false;
        };
        let idx = call.inline_count as usize;
        if idx >= call.inline_args.len()
            || !call.args.is_empty()
            || call.has_named
            || call.magic_name.is_some()
            || call.func.arg_by_ref(idx)
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
            ArgSrc::Place(pid) => match frame.code.places.get(*pid as usize) {
                Some(p) if p.segs.is_empty() => match &p.base {
                    crate::opcode::PlaceBase::Cv(cv) => *cv,
                    _ => return false,
                },
                _ => return false,
            },
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
                        None => self.place_fetch(*pid, FetchMode::Read)?,
                    },
                    ArgSrc::TempPlace(pid) => self.place_fetch(*pid, FetchMode::Read)?,
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
            }
            if b.flags & flags::NODISCARD == 0 && !call.is_ctor {
                self.pending_call_dst = dst;
                let v = self.call_builtin(&b, call, strict)?;
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
    /// type as it is), not a constructor, a partial application nor a
    /// `__call` trampoline, with no observer and a pooled frame ready. It
    /// then does exactly what that branch does, in the same order; a
    /// closure's bound variables are bound afterwards as the general path
    /// binds them. `caller_ip` is stored as the caller's resume point
    /// (`SAVE_OPLINE`) when the call is made. Returns `false`, having
    /// changed nothing, otherwise.
    ///
    /// A pooled frame that last ran the same function still holds its
    /// handles ([`Frame::clear_for_reuse`] keeps them): they are not
    /// replaced.
    #[inline]
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
                && frame.iters.is_empty()
                && frame.fins.is_empty()
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
            frame.is_ctor = false;
            // The passed-argument count (`ZEND_CALL_NUM_ARGS`): one empty
            // entry per argument, each moved into its compiled variable.
            // The recycled buffer is empty (`Frame::clear_for_reuse`).
            if !frame.args.is_empty() {
                frame.args.clear();
            }
            #[allow(clippy::same_item_push)] // inline, unlike `resize`
            for _ in 0..n {
                frame.args.push(None);
            }
            for (slot, arg) in frame.cvs.iter_mut().zip(&mut call.inline_args[..n]) {
                // The slot is empty: nothing to release.
                if let Some(old) = slot.set(arg.take()) {
                    drop(old);
                }
            }
            // `RECV` of an argument that was passed and needs no coercion
            // has nothing left to do: start after it (after a `RECV_INIT`'s
            // default-value code).
            let mut first_op = 0;
            for i in 0..n {
                match code.ops.get(first_op) {
                    Some(Op::Recv { n, .. }) if *n as usize == i => first_op += 1,
                    Some(Op::RecvInit { n, skip, .. }) if *n as usize == i => {
                        first_op = *skip as usize;
                    }
                    _ => break,
                }
            }
            frame.ip = first_op;
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
            unsafe { caller.calls.set_len(pending - 1) };
            if same_func {
                // The frame holds this function already.
                drop(func);
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
    /// a reference, or a literal, and the value is accepted by the return
    /// type as it is (`verify_return`'s fast path). The result is then the
    /// one `op_return` gives: the same checks, the same release order
    /// (`gc::frame_released`, then [`FrameStack::release_top`]) and a
    /// dereferenced value (`copy_on_assign` has nothing left to do).
    /// `ip` is the instruction after the `RETURN`, stored as the frame's
    /// resume point before the release (`SAVE_OPLINE`). Returns `None`,
    /// having changed nothing, otherwise.
    ///
    /// [`FrameStack::release_top`]: crate::vm::FrameStack::release_top
    #[inline]
    pub(crate) fn return_simple(
        &mut self,
        val: Operand,
        base: usize,
        ip: usize,
    ) -> Option<crate::exec::Flow> {
        if self.observer.enabled {
            return None;
        }
        let fidx = self.frames.live.checked_sub(1)?;
        if fidx <= base {
            return None;
        }
        let (v, ret) = {
            let (callers, rest) = self.frames.vec.split_at_mut(fidx);
            let caller = callers.last()?;
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
            let accepts = |v: &Value| match rt {
                None => true,
                Some(rt) => {
                    !rt.is_void()
                        && !rt.is_never()
                        && (rt.is_mixed() || accepts_without_coercion(rt.mask, v))
                }
            };
            let v = match val {
                Operand::Cv(cv) => {
                    let v = match frame
                        .cvs
                        .get(cv as usize)
                        .and_then(crate::vm::CvSlot::get)?
                    {
                        Value::Reference(r) => r.get(),
                        v => clone_value(v),
                    };
                    if !accepts(&v) {
                        return None;
                    }
                    v
                }
                Operand::Tmp(t) => {
                    let slot = frame.tmps.get_mut(t as usize)?;
                    if matches!(slot, Value::Reference(_)) || !accepts(&*slot) {
                        return None;
                    }
                    std::mem::take(slot)
                }
                Operand::Lit(i) => {
                    let lit = frame.code.literals.get(i as usize)?;
                    if matches!(lit, Value::Reference(_)) || !accepts(lit) {
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
            // `gc::frame_released` has nothing to record for a frame
            // without `$this`, a closure or a compiled variable holding an
            // array, object or reference (no symbol table is attached).
            if frame.this.is_some() || frame.closure.is_some() || cvs_may_hold_roots(&frame.cvs) {
                crate::gc::frame_released(frame, caller.this.as_ref());
            }
            (v, frame.ret)
        };
        self.frames.release_top(&self.empty_code);
        match ret {
            RetTarget::Tmp(t) => self.set_tmp(t, v),
            // An unused result (`g();`): a scalar owns nothing, so the
            // out-of-line drop glue is skipped.
            RetTarget::Discard => {
                if matches!(
                    v,
                    Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
                ) {
                    std::mem::forget(v);
                } else {
                    drop(v);
                }
            }
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

        if !uf.is_generator()
            && uf.code.num_statics == 0
            && uf.statics.borrow().is_empty()
            && uf.code.num_iters == 0
            && uf.code.num_finally == 0
            && named.is_empty()
            && !self.has_observers()
            && closure.is_none()
            && magic_name.is_none()
            && !is_ctor
            && args.is_empty()
            && passed_argc >= uf.required as usize
            && passed_argc <= uf.params.len()
            && uf.params.iter().all(|p| !p.variadic && !p.by_ref)
            && uf.params[..passed_argc]
                .iter()
                .enumerate()
                .all(|(i, p)| match &p.ty {
                    None => true,
                    Some(ty) => {
                        ty.is_mixed()
                            || inline_args[i]
                                .as_ref()
                                .map_or(false, |v| accepts_without_coercion(ty.mask, v))
                    }
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
            f.is_ctor = false;

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
            Value::Int(c) if (0..=255).contains(c) => Some(Value::Str(ZStr::from(&[*c as u8][..]))),
            _ => None,
        },
        _ => None,
    }
}

impl Vm {
    /// `zend_try_compile_special_func()`: statically named calls without
    /// named arguments that php-src compiles to dedicated opcodes, so no
    /// internal call exists for observers to see. Inside a function body,
    /// `func_num_args()` / `func_get_args()` become `ZEND_FUNC_NUM_ARGS` /
    /// `ZEND_FUNC_GET_ARGS`; `strlen($x)` becomes `ZEND_STRLEN`; and
    /// `call_user_func()` / `call_user_func_array()` become
    /// `ZEND_INIT_USER_CALL` + `ZEND_SEND_USER`/`ZEND_SEND_ARRAY`
    /// (`zend_compile_func_cuf()` / `zend_compile_func_cufa()`), calling
    /// the callback directly. (Only consulted while observers are enabled.)
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
            b"call_user_func" => !call.args.is_empty(),
            b"call_user_func_array" => call.args.len() == 2,
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
        // Direct call_user_func calls compile to frameless call forwarding in Zend.
        let builtin_name = (!call.direct
            || !matches!(
                b.name.as_bytes(),
                b"call_user_func" | b"call_user_func_array"
            ))
        .then(|| b.name.clone());
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
        self.frames.push(Frame::with_vars(
            self.empty_code.clone(),
            FrameKind::Builtin,
            self.empty_vars.clone(),
            None,
        ));
        let depth = self.frames.len();
        {
            // A new frame has no named arguments, `$this` or scopes: only
            // the ones the call has are copied.
            let f = &mut self.frames[depth - 1];
            f.func = Some(call.func.clone());
            f.builtin_name = builtin_name;
            if !call.named.is_empty() {
                f.named = call.named.clone();
            }
            install_buffer(&mut f.args, pushed_args);
            f.this = call.this.clone();
            f.scope = call.scope.clone();
            f.called = call.called.clone();
            f.closure = call.closure.clone();
            f.magic_name = self.magic_call_name.clone();
            f.observer_hidden = observer_hidden;
        }
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
        let fname = self.func_display(&Func::Builtin(b.clone()), call.scope.as_ref());
        let pname = p.name.to_string_lossy();
        let ctx = TypeCtx {
            scope: call.scope.as_ref(),
            called: call.called.as_ref(),
            strict,
            internal_arg: Some((&fname, i + 1, &pname)),
        };
        if ty.mask & crate::types::mask::CALLABLE != 0
            && !ty.has_classes()
            && ty.mask & !(crate::types::mask::CALLABLE | crate::types::mask::NULL) == 0
            && !(ty.allows_null() && v.is_null())
        {
            if let Err(why) = self.zend_is_callable(v, true)? {
                // `Z_PARAM_FUNC` checks without suppressing the callable
                // deprecations; a valid callable emits them when the
                // function resolves it, an invalid one here (a class that
                // was not found has none, and is not autoloaded twice).
                if !why.contains("not found") {
                    let _ = self.zend_is_callable(v, false)?;
                }
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
    pub(crate) fn builtin_default(&mut self, b: &Builtin, i: usize) -> Option<Value> {
        if let Some(Some(v)) = b.defaults.get(i) {
            return Some(v.clone());
        }
        let text = b.params.get(i)?.default_text.as_deref()?.trim();
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
    fn pop_frames_to(&mut self, depth: usize) {
        if self.observer.keep_builtin_frame {
            return;
        }
        if self.frames.len() >= depth {
            let args = std::mem::take(&mut self.frames[depth - 1].args);
            self.frames.truncate(depth - 1);
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
        let passed = f.args.len();
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
        let exact = uf.required as usize == uf.params.len() && uf.flags & flags::VARIADIC == 0;
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
            let fname = func.as_ref().map_or_else(String::new, |f| {
                let name = object::display_name(f.name());
                match scope.as_ref() {
                    Some(c) => format!("{}::{name}", object::display_name(&c.name)),
                    None => name.to_string_lossy(),
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
    pub(crate) fn call_pending(&mut self, mut call: PendingCall) -> VmResult<Value> {
        // `zend_call_function` services pending interrupts (async signals)
        // before entering user code from internal code.
        self.check_interrupt()?;
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
                self.push_user_call(call, RetTarget::Discard, strict, true)?;
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
                call.args = args.into_iter().map(Some).collect();
                return self.call_pending(call);
            }
        }
        let mut call = match self.zend_is_callable(callable, true)? {
            Ok(call) => call,
            Err(_) => self.resolve_callable_for_call(callable)?,
        };
        let mut call_args = self.call_bufs.args.pop().unwrap_or_default();
        call_args.clear();
        call_args.reserve(args.len());
        call_args.extend(args.into_iter().map(Some));
        call.args = call_args;
        self.call_pending(call)
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
        args: Vec<Value>,
    ) -> VmResult<Value> {
        let mut call = self.pending_for_method(m, Some(o.clone()), object::class_of(o).clone());
        let mut call_args = self.call_bufs.args.pop().unwrap_or_default();
        call_args.clear();
        call_args.reserve(args.len());
        call_args.extend(args.into_iter().map(Some));
        call.args = call_args;
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
        let cls = object::class_of(o).clone();
        let m = if !name.as_bytes().iter().any(u8::is_ascii_uppercase) {
            cls.methods.get(name.as_bytes()).cloned()
        } else {
            let lc = name.to_ascii_lowercase();
            cls.methods.get(lc.as_bytes()).cloned()
        };
        let call = match m {
            Some(m) => self.pending_for_method(&m, Some(o.clone()), cls),
            None => self.method_call(o, &ZStr::from(name))?,
        };
        let mut call = call;
        let mut call_args = self.call_bufs.args.pop().unwrap_or_default();
        call_args.clear();
        call_args.reserve(args.len());
        call_args.extend(args.into_iter().map(Some));
        call.args = call_args;
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
        let m = if !name.as_bytes().iter().any(u8::is_ascii_uppercase) {
            cls.methods.get(name.as_bytes()).cloned()
        } else {
            let lc = name.to_ascii_lowercase();
            cls.methods.get(lc.as_bytes()).cloned()
        };
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
        let mut call_args = self.call_bufs.args.pop().unwrap_or_default();
        call_args.clear();
        call_args.reserve(args.len());
        call_args.extend(args.into_iter().map(Some));
        call.args = call_args;
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
        let mut call_args = self.call_bufs.args.pop().unwrap_or_default();
        call_args.clear();
        call_args.reserve(args.len());
        call_args.extend(args.into_iter().map(Some));
        call.args = call_args;
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
#[inline]
fn simple_ucall(uf: &UserFunction, call: &PendingCall, n: usize) -> bool {
    if uf.flags & (flags::DEPRECATED | flags::NODISCARD | flags::NO_BODY | flags::GENERATOR) != 0
        || uf.code.num_statics != 0
        || uf.code.num_iters != 0
        || uf.code.num_finally != 0
        || !call.args.is_empty()
        || !call.named.is_empty()
        || call.magic_name.is_some()
        || call.pfa.is_some()
        || call.is_ctor
        || n < uf.required as usize
        || n > uf.params.len()
        || n > call.inline_args.len()
        || n > uf.code.cv_names.len()
        || !uf.statics.try_borrow().is_ok_and(|s| s.is_empty())
    {
        return false;
    }
    // Plain loops: the `Iterator::all` chains these replace were an
    // out-of-line `try_fold` call per check.
    for (i, p) in uf.params.iter().enumerate() {
        if p.variadic || p.by_ref {
            return false;
        }
        if i < n {
            if let Some(ty) = &p.ty {
                if !ty.is_mixed()
                    && !call.inline_args[i]
                        .as_ref()
                        .is_some_and(|v| accepts_without_coercion(ty.mask, v))
                {
                    return false;
                }
            }
        }
    }
    true
}

/// `v.clone()` with the scalar cases inline: the derived `Clone` of
/// [`Value`] is an out-of-line call even for an integer.
#[inline(always)]
#[allow(clippy::inline_always)] // an argument or result per call
fn clone_value(v: &Value) -> Value {
    match v {
        Value::Null => Value::Null,
        Value::Bool(b) => Value::Bool(*b),
        Value::Int(i) => Value::Int(*i),
        Value::Float(f) => Value::Float(*f),
        _ => v.clone(),
    }
}

/// Whether a compiled variable of a returning frame holds a value
/// [`crate::gc::possible_root`] may record: an array, an object or a
/// reference (it ignores everything else).
#[inline]
fn cvs_may_hold_roots(cvs: &[crate::vm::CvSlot]) -> bool {
    for slot in cvs {
        if matches!(
            slot.get(),
            Some(Value::Array(_) | Value::Object(_) | Value::Reference(_))
        ) {
            return true;
        }
    }
    false
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

#[cfg(test)]
mod buffer_tests {
    use super::*;
    use crate::VmOptions;

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

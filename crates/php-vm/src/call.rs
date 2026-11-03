//! Function calls: `INIT_*`, `SEND_*`, `DO_FCALL`, `RECV*` and returns.

use crate::class::Class;
use crate::exec::scope_display;
use crate::func::{flags, Args, Builtin, Func, Method, UserFunction};
use crate::object;
use crate::opcode::{ArgSrc, ClassSpec, FetchMode};
use crate::typecheck::{given_type, TypeCtx};
use crate::vm::{Frame, FrameKind, PendingCall, RetTarget, Throw, Vm, VmResult};
use php_runtime::{ArrayKey, HashTable, Obj, PhpRef, Value, ZArray, ZStr};
use std::rc::Rc;

impl Vm {
    /// Display name of a function for messages (`A::m`, `foo`).
    pub(crate) fn func_display(&self, func: &Func, scope: Option<&Rc<Class>>) -> String {
        let name = func.name().to_string_lossy();
        if let Func::User(u) = func {
            if u.is_closure() {
                return name;
            }
        }
        match scope {
            Some(c) => format!("{}::{}", object::display_name(&c.name), name),
            None => name,
        }
    }

    fn frame_func_display(&self, f: &Frame) -> String {
        match &f.func {
            Some(func) => {
                let mut n = self.func_display(func, f.scope.as_ref());
                if let Some(m) = &f.magic_name {
                    n = format!(
                        "{}::{}",
                        f.scope
                            .as_ref()
                            .map_or_else(String::new, |c| c.name.to_string_lossy()),
                        m
                    );
                }
                n
            }
            None => String::new(),
        }
    }

    // ------------------------------------------------------------------
    // INIT
    // ------------------------------------------------------------------

    pub(crate) fn op_init_fcall(
        &mut self,
        name: u32,
        fallback: Option<u32>,
        display: u32,
    ) -> VmResult<()> {
        let lc = self.lit_str(name);
        let mut f = self.functions.get(&lc).cloned();
        if f.is_none() {
            if let Some(fb) = fallback {
                let fbn = self.lit_str(fb);
                f = self.functions.get(&fbn).cloned();
            }
        }
        let Some(func) = f else {
            let d = self.lit_str(display);
            return Err(self.make_throw(
                "Error",
                &format!("Call to undefined function {}()", d.to_string_lossy()),
            ));
        };
        self.frame_mut().calls.push(PendingCall::new(func));
        Ok(())
    }

    pub(crate) fn op_init_dyn_call(&mut self, callee: Value) -> VmResult<()> {
        let call = self.resolve_callable_for_call(&callee)?;
        self.frame_mut().calls.push(call);
        Ok(())
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
                let lc = ZStr::from(name.to_ascii_lowercase());
                match self.functions.get(&lc).cloned() {
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
                        self.make_throw("Error", "Array callback must have exactly two elements")
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
                            // [$obj, 'parent::m']
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
            _ => Err(self.make_throw("Error", "Value not callable")),
        }
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
        let call = self.method_call(&o, &name)?;
        self.frame_mut().calls.push(call);
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
        self.method_call_on(o, &cls, name)
    }

    fn method_call_on(&mut self, o: &Obj, cls: &Rc<Class>, name: &ZStr) -> VmResult<PendingCall> {
        let scope = self.scope();
        let lc = ZStr::from(name.to_ascii_lowercase());
        // A private method of the calling scope takes precedence.
        if let Some(s) = &scope {
            if let Some(m) = s.methods.get(&lc) {
                if m.visibility() == flags::PRIVATE
                    && Rc::ptr_eq(&m.scope(), s)
                    && object::class_of(o).is_subclass_of(s)
                {
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
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Call to {vis} method {}::{}() from {}",
                    object::display_name(&cls.name),
                    m.name,
                    scope_display(scope.as_ref())
                ),
            ));
        }
        if let Some(c) = self.closure_method_call(o, &lc) {
            return Ok(c);
        }
        if let Some(callm) = cls.magic.call.clone() {
            return Ok(self.magic_call(&callm, Some(o.clone()), cls.clone(), name));
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
            magic_name: Some(name.clone()),
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
        let lc = ZStr::from(name.to_ascii_lowercase());
        let found = cls.methods.get(&lc).cloned();
        let m = match found {
            Some(m) if self.can_access_method(&m, scope.as_ref()) => m,
            other => {
                // __call in object context, else __callStatic.
                if let Some(t) = &this {
                    if object::class_of(t).is_subclass_of(cls) {
                        if let Some(callm) = cls.magic.call.clone() {
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
                    return Ok(self.magic_call(&cs, None, cls.clone(), name));
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
                                object::display_name(&cls.name),
                                m.name,
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
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Cannot call abstract method {}::{}()",
                    object::display_name(&m.scope().name),
                    m.name
                ),
            ));
        }
        if m.is_static() {
            let called = if forward {
                cur_called
                    .filter(|c| c.is_subclass_of(cls))
                    .unwrap_or_else(|| cls.clone())
            } else {
                cls.clone()
            };
            return Ok(PendingCall {
                scope: Some(m.scope()),
                called: Some(called),
                ..PendingCall::new(m.func.clone())
            });
        }
        // Non-static method: needs a compatible $this.
        if let Some(t) = this {
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

    pub(crate) fn op_init_static_call(&mut self, class: &ClassSpec, name: Value) -> VmResult<()> {
        let Value::Str(name) = name else {
            return Err(self.make_throw("Error", "Method name must be a string"));
        };
        let forward = matches!(
            class,
            ClassSpec::SelfClass | ClassSpec::Parent | ClassSpec::Static
        );
        let cls = self.resolve_class_spec(class)?;
        let call = self.static_method_call(&cls, &name, forward)?;
        self.frame_mut().calls.push(call);
        Ok(())
    }

    /// `new`: creates the object, stores it and starts the constructor call.
    pub(crate) fn op_new(&mut self, class: &ClassSpec, dst: u32) -> VmResult<bool> {
        let cls = self.resolve_class_spec(class)?;
        let o = self.instantiate(&cls)?;
        self.set_tmp(dst, Value::Object(o.clone()));
        if let Some(f) = cls.hooks.as_ref().and_then(|h| h.get_constructor) {
            f(self, &o)?;
        }
        let Some(ctor) = cls.magic.construct.clone() else {
            return Ok(false);
        };
        let scope = self.scope();
        if !self.can_access_method(&ctor, scope.as_ref()) {
            let vis = if ctor.visibility() == flags::PRIVATE {
                "private"
            } else {
                "protected"
            };
            return Err(self.make_throw(
                "Error",
                &format!(
                    "Call to {vis} {}::__construct() from {}",
                    object::display_name(&cls.name),
                    scope_display(scope.as_ref())
                ),
            ));
        }
        let mut call = self.pending_for_method(&ctor, Some(o), cls);
        call.is_ctor = true;
        self.frame_mut().calls.push(call);
        Ok(true)
    }

    // ------------------------------------------------------------------
    // SEND
    // ------------------------------------------------------------------

    pub(crate) fn arg_position(&mut self, name: &ZStr) -> VmResult<Option<usize>> {
        let call = self.frame().calls.last().expect("pending call");
        if call.magic_name.is_some() {
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

    pub(crate) fn op_send(&mut self, src: &ArgSrc, name: Option<ZStr>) -> VmResult<()> {
        let pos = match &name {
            Some(n) => self.arg_position(n)?,
            None => Some(self.frame().calls.last().expect("pending call").args.len()),
        };
        let (by_ref, is_magic) = {
            let call = self.frame().calls.last().expect("pending call");
            let magic = call.magic_name.is_some();
            (
                !magic && pos.is_some_and(|p| call.func.arg_by_ref(p))
                    || (!magic && pos.is_none() && call.func.arg_by_ref(call.func.params().len())),
                magic,
            )
        };
        let _ = is_magic;
        // ZEND_ARG_SEND_PREFER_REF: by reference when possible, else by
        // value without complaint.
        let prefer_ref = by_ref && {
            let call = self.frame().calls.last().expect("pending call");
            let ps = call.func.params();
            pos.and_then(|p| ps.get(p).or_else(|| ps.last().filter(|l| l.variadic)))
                .is_some_and(|x| x.promote & crate::opcode::PREFER_REF != 0)
        };
        let value = match src {
            ArgSrc::Val(op) => {
                let v = self.read(*op)?;
                if by_ref && !prefer_ref {
                    let p = pos.unwrap_or(0);
                    let call = self.frame().calls.last().expect("pending call").clone();
                    let pname = call
                        .func
                        .params()
                        .get(p)
                        .map_or_else(String::new, |x| x.name.to_string_lossy());
                    let fname = self.func_display(&call.func, call.scope.as_ref());
                    return Err(self.make_throw(
                        "Error",
                        &format!(
                            "{fname}(): Argument #{} (${pname}) could not be passed by reference",
                            p + 1
                        ),
                    ));
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
                if by_ref {
                    Value::Reference(self.place_make_ref(*pid)?)
                } else {
                    self.place_fetch(*pid, FetchMode::Read)?
                }
            }
        };
        self.push_arg(pos, name, value)
    }

    fn push_arg(&mut self, pos: Option<usize>, name: Option<ZStr>, value: Value) -> VmResult<()> {
        let call = self.frame_mut().calls.last_mut().expect("pending call");
        match (pos, name) {
            (Some(p), Some(n)) => {
                call.has_named = true;
                if call.args.len() > p && call.args[p].is_some() {
                    let what = if call.pfa.as_ref().is_some_and(|x| x.placeholders.get(p).copied().unwrap_or(false)) {
                        "placeholder"
                    } else {
                        "argument"
                    };
                    return Err(self.make_throw(
                        "Error",
                        &format!("Named parameter ${} overwrites previous {what}", n.to_string_lossy()),
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

    pub(crate) fn op_send_unpack(&mut self, v: Value) -> VmResult<()> {
        let items: Vec<(Value, Value)> = match &v {
            Value::Array(a) => a.iter().map(|(k, v)| (k.to_value(), v.clone())).collect(),
            Value::Object(o) if self.instance_of(o, b"traversable") => self.iterate_to_vec(&v)?,
            _ => {
                return Err(self.make_throw(
                    "Error",
                    &format!(
                        "Only arrays and Traversables can be unpacked, {} given",
                        given_type(&v)
                    ),
                ))
            }
        };
        for (k, val) in items {
            match k {
                Value::Str(s) => {
                    let pos = self.arg_position(&s)?;
                    let by_ref = {
                        let call = self.frame().calls.last().expect("pending call");
                        pos.is_some_and(|p| call.func.arg_by_ref(p))
                    };
                    let val = if by_ref { val } else { val.deref() };
                    self.push_arg(pos, Some(s), val)?;
                }
                _ => {
                    let has_named = self.frame().calls.last().expect("pending call").has_named;
                    if has_named {
                        return Err(self.make_throw(
                            "Error",
                            "Cannot use positional argument after named argument during unpacking",
                        ));
                    }
                    let (by_ref, n) = {
                        let call = self.frame().calls.last().expect("pending call");
                        let n = call.args.len();
                        (call.func.arg_by_ref(n), n)
                    };
                    let _ = n;
                    let val = if by_ref {
                        match val {
                            Value::Reference(r) => Value::Reference(r),
                            other => Value::Reference(PhpRef::new(other)),
                        }
                    } else {
                        val.deref()
                    };
                    self.push_arg(None, None, val)?;
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // DO_FCALL
    // ------------------------------------------------------------------

    /// Performs the pending call. Returns `true` when a user frame was
    /// pushed.
    pub(crate) fn op_do_call(&mut self, dst: Option<u32>) -> VmResult<bool> {
        let call = self.frame_mut().calls.pop().expect("pending call");
        let strict = self.frame().code.strict_types;
        if dst.is_none() && call.func.flags() & flags::NODISCARD != 0 {
            if call.func.flags() & flags::DEPRECATED != 0 && matches!(call.func, Func::User(_)) {
                self.check_deprecated_call(&call)?;
            }
            self.check_nodiscard_call(&call)?;
            return self.op_do_call_checked(call, dst, strict);
        }
        self.op_do_call_checked(call, dst, strict)
    }

    fn op_do_call_checked(
        &mut self,
        call: PendingCall,
        dst: Option<u32>,
        strict: bool,
    ) -> VmResult<bool> {
        let skip_deprecation = dst.is_none() && call.func.flags() & flags::NODISCARD != 0;
        match call.func.clone() {
            Func::User(uf) => {
                if !skip_deprecation {
                    self.check_deprecated_call(&call)?;
                }
                let ret = dst.map_or(RetTarget::Discard, RetTarget::Tmp);
                self.push_user_frame(&uf, call, ret, strict, false)?;
                Ok(true)
            }
            Func::Builtin(b) => {
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
    fn check_nodiscard_call(&mut self, call: &PendingCall) -> VmResult<()> {
        let (attrs, code) = match &call.func {
            Func::User(uf) => (uf.attributes.clone(), crate::errors::level::USER_WARNING),
            Func::Builtin(_) => (Vec::new(), crate::errors::level::WARNING),
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
        let name = self.func_display(&call.func, call.scope.as_ref());
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

    /// Pushes a frame for a user function call.
    pub(crate) fn push_user_frame(
        &mut self,
        uf: &Rc<UserFunction>,
        call: PendingCall,
        ret: RetTarget,
        caller_strict: bool,
        from_internal: bool,
    ) -> VmResult<()> {
        let mut f = Frame::new(uf.code.clone(), FrameKind::Function);
        f.func = Some(call.func);
        f.ufunc = Some(uf.clone());
        f.this = call.this;
        f.scope = call.scope;
        f.called = call.called;
        f.ret = ret;
        f.caller_strict = caller_strict;
        f.called_from_internal = from_internal;
        f.is_ctor = call.is_ctor;
        if let Some(name) = call.magic_name {
            // __call($name, $args)
            let mut arr = HashTable::new();
            for a in call.args.into_iter().flatten() {
                let _ = arr.push(a.deref());
            }
            for (k, v) in call.named {
                arr.insert(ArrayKey::Str(k), v.deref());
            }
            f.args = vec![
                Some(Value::Str(name.clone())),
                Some(Value::Array(ZArray::from_table(arr))),
            ];
            f.magic_name = Some(name);
        } else {
            f.args = call.args;
            f.named = call.named;
        }
        if let Some(c) = &call.closure {
            self.bind_closure_vars(&mut f, c);
            f.closure = Some(c.clone());
        }
        if self.frames.len() > 50_000 {
            return Err(self.make_throw(
                "Error",
                "Maximum call stack size reached. Infinite recursion?",
            ));
        }
        self.frames.push(f);
        Ok(())
    }

    /// Calls a builtin.
    pub(crate) fn call_builtin(
        &mut self,
        b: &Rc<Builtin>,
        mut call: PendingCall,
        strict: bool,
    ) -> VmResult<Value> {
        // A native `__call`/`__callStatic` gets the called name through
        // `vm.magic_call_name()`.
        self.magic_call_name = call.magic_name.take();
        let fname = self.func_display(&Func::Builtin(b.clone()), call.scope.as_ref());
        let nparams = b.params.len();
        let variadic = b.flags & flags::VARIADIC != 0;
        let mut values: Vec<Value> = Vec::with_capacity(call.args.len());
        let mut passed: Vec<bool> = Vec::with_capacity(call.args.len());
        let npassed = call.args.len();
        if b.flags & flags::DEPRECATED != 0 {
            // Before zpp, like ZEND_DO_ICALL.
            self.check_deprecated_call(&call)?;
        }
        // Builtin frame (for backtraces and func_get_args of callers),
        // pushed before zpp so arity and argument errors show it.
        let mut f = Frame::with_vars(
            self.empty_code.clone(),
            FrameKind::Builtin,
            self.empty_vars.clone(),
            None,
        );
        f.func = Some(Func::Builtin(b.clone()));
        f.builtin_name = Some(b.name.clone());
        f.args.clone_from(&call.args);
        f.this.clone_from(&call.this);
        f.scope = call.scope.clone();
        f.called.clone_from(&call.called);
        self.frames.push(f);
        let depth = self.frames.len();
        // Arity.
        let positional_count = call.args.iter().filter(|a| a.is_some()).count();
        if !call.has_named && npassed < b.required as usize {
            let e = self.arg_count_error_builtin(&fname, b, npassed);
            self.pop_frames_to(depth);
            return Err(e);
        }
        if !variadic && npassed > nparams {
            let e = self.arg_count_error_builtin(&fname, b, npassed);
            self.pop_frames_to(depth);
            return Err(e);
        }
        let _ = positional_count;
        for (i, a) in call.args.into_iter().enumerate() {
            match a {
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
                        let e = self.make_throw(
                            "ArgumentCountError",
                            &format!("{fname}(): Argument #{} (${pname}) not passed", i + 1),
                        );
                        self.pop_frames_to(depth);
                        return Err(e);
                    }
                    values.push(self.builtin_default(b, i).unwrap_or_default());
                    passed.push(false);
                }
            }
        }
        // Omitted trailing optional parameters get their declared defaults
        // (not marked as passed); backtraces only show the passed ones.
        let nframe_args = values.len();
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
        if call.has_named && values.len() < b.required as usize {
            let i = values.len();
            let pname = b
                .params
                .get(i)
                .map_or_else(String::new, |p| p.name.to_string_lossy());
            let e = self.make_throw(
                "ArgumentCountError",
                &format!("{fname}(): Argument #{} (${pname}) not passed", i + 1),
            );
            self.pop_frames_to(depth);
            return Err(e);
        }
        if let Some(f) = self.frames.get_mut(depth - 1) {
            f.args = values.iter().take(nframe_args).cloned().map(Some).collect();
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
            if p.by_ref {
                continue;
            }
            let pname = p.name.to_string_lossy();
            let ctx = TypeCtx {
                scope: call.scope.as_ref(),
                called: call.called.as_ref(),
                strict,
                internal_arg: Some((&fname, i + 1, &pname)),
            };
            let ty = ty.clone();
            let ok = match self.verify_type(&ty, v, &ctx) {
                Ok(ok) => ok,
                Err(e) => {
                    self.pop_frames_to(depth);
                    return Err(e);
                }
            };
            if !ok {
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
                let e = self.make_throw("TypeError", &msg);
                self.pop_frames_to(depth);
                return Err(e);
            }
        }
        if let Some(f) = self.frames.get_mut(depth - 1) {
            f.args = values.iter().take(nframe_args).cloned().map(Some).collect();
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
        drop(args);
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
        if self.frames.len() >= depth {
            self.frames.truncate(depth - 1);
        }
        r
    }

    /// The default of native parameter `i`: a parsed literal, or a
    /// constant (`SOME_CONST`, `Class::CONST`) resolved at call time.
    fn builtin_default(&mut self, b: &Builtin, i: usize) -> Option<Value> {
        if let Some(v) = b.defaults.get(i).cloned().flatten() {
            return Some(v);
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
        if self.frames.len() >= depth {
            self.frames.truncate(depth - 1);
        }
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
        let fname = self.func_display(&func, scope.as_ref());
        let mut msg = format!(
            "{fname}(): Argument #{} (${}) must be of type {tyname}, {} given",
            n + 1,
            p.name,
            given_type(v)
        );
        if !internal && self.frames.len() >= 2 {
            let caller = &self.frames[self.frames.len() - 2];
            if caller.kind != FrameKind::Builtin {
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
        let (ty, strict) = {
            let f = self.frame();
            let uf = f.ufunc.as_ref().expect("user frame");
            let p = uf.params.get(n as usize).or_else(|| uf.params.last());
            (p.and_then(|p| p.ty.clone()), f.caller_strict)
        };
        if let Some(ty) = ty {
            let scope = self.frame().scope.clone();
            let called = self.frame().called.clone();
            let ctx = TypeCtx {
                scope: scope.as_ref(),
                called: called.as_ref(),
                strict,
                internal_arg: None,
            };
            if !self.verify_type(&ty, &mut v, &ctx)? {
                // php-src already holds the rejected value in the CV, so
                // the TypeError's trace shows it: keep it visible.
                if let Some(slot) = self.frame_mut().args.get_mut(n as usize) {
                    if slot.is_none() {
                        *slot = Some(v.clone());
                    }
                }
                return Err(self.recv_type_error(n, &v));
            }
        }
        Ok(v)
    }

    pub(crate) fn op_recv(&mut self, n: u32, cv: u32) -> VmResult<()> {
        let arg = self
            .frame_mut()
            .args
            .get_mut(n as usize)
            .and_then(Option::take);
        match arg {
            Some(v) => {
                let v = self.recv_check(n, v)?;
                self.cv_bind(cv, v);
                Ok(())
            }
            None => Err(self.missing_arg_error(n)),
        }
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
        if !internal && self.frames.len() >= 2 {
            let caller = &self.frames[self.frames.len() - 2];
            if caller.kind != FrameKind::Builtin {
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

    pub(crate) fn op_recv_init(&mut self, n: u32, cv: u32) -> VmResult<bool> {
        let arg = self
            .frame_mut()
            .args
            .get_mut(n as usize)
            .and_then(Option::take);
        match arg {
            Some(v) => {
                let v = self.recv_check(n, v)?;
                self.cv_bind(cv, v);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub(crate) fn op_recv_default(&mut self, n: u32, cv: u32, v: Value) -> VmResult<()> {
        // RECV_INIT verifies the (possibly constant-expression) default and
        // coerces it (`float $x = 0` gives float(0)).
        let v = self.recv_check(n, v)?;
        self.cv_bind(cv, v);
        Ok(())
    }

    pub(crate) fn op_recv_variadic(&mut self, n: u32, cv: u32) -> VmResult<()> {
        let rest: Vec<Option<Value>> = {
            let f = self.frame_mut();
            if f.args.len() > n as usize {
                f.args.drain(n as usize..).collect()
            } else {
                Vec::new()
            }
        };
        let named = std::mem::take(&mut self.frame_mut().named);
        let mut arr = HashTable::new();
        for (i, a) in rest.into_iter().enumerate() {
            let Some(v) = a else { continue };
            let v = self.recv_check(n + u32::try_from(i).unwrap_or(0), v)?;
            let _ = arr.push(v);
        }
        for (k, v) in named {
            let v = self.recv_check(n, v)?;
            arr.insert(ArrayKey::Str(k), v);
        }
        self.cv_bind(cv, Value::Array(ZArray::from_table(arr)));
        Ok(())
    }

    /// Verifies the return value of the current frame.
    pub(crate) fn verify_return(&mut self, v: Option<Value>) -> VmResult<Value> {
        let (rt, strict, is_gen, func, scope, called) = {
            let f = self.frame();
            let Some(uf) = &f.ufunc else {
                return Ok(v.unwrap_or_default());
            };
            (
                uf.return_type.clone(),
                f.code.strict_types,
                uf.is_generator(),
                f.func.clone(),
                f.scope.clone(),
                f.called.clone(),
            )
        };
        let Some(rt) = rt else {
            return Ok(v.unwrap_or_default());
        };
        if is_gen {
            return Ok(v.unwrap_or_default());
        }
        let fname = func
            .as_ref()
            .map_or_else(String::new, |f| self.func_display(f, scope.as_ref()));
        if rt.is_void() {
            return Ok(Value::Null);
        }
        if rt.is_never() {
            return Err(self.make_throw(
                "TypeError",
                &format!("{fname}(): never-returning function must not implicitly return"),
            ));
        }
        let tyname = rt.display(
            scope.as_ref().map(|s| &s.name),
            scope.as_ref().and_then(|s| s.parent_name()),
            called.as_ref().map(|c| &c.name),
        );
        let Some(mut v) = v else {
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
        if !self.verify_type(&rt, &mut v, &ctx)? {
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
        match call.func.clone() {
            Func::User(uf) => {
                // Arguments passed by value to by-ref parameters.
                for (i, a) in call.args.iter_mut().enumerate() {
                    if let Some(v) = a {
                        if uf.params.get(i).is_some_and(|p| p.by_ref)
                            || (i >= uf.params.len()
                                && uf.params.last().is_some_and(|p| p.variadic && p.by_ref))
                        {
                            if !v.is_reference() {
                                let pname = uf
                                    .params
                                    .get(i)
                                    .or_else(|| uf.params.last())
                                    .map_or_else(String::new, |p| p.name.to_string_lossy());
                                let fname = self.func_display(&call.func, call.scope.as_ref());
                                self.warning(&format!(
                                    "{fname}(): Argument #{} (${pname}) must be passed by reference, value given",
                                    i + 1
                                ))?;
                                *v = Value::Reference(PhpRef::new(std::mem::take(v)));
                            }
                        } else if let Value::Reference(_) = v {
                            *v = v.deref();
                        }
                    }
                }
                self.check_deprecated_call(&call)?;
                self.push_user_frame(&uf, call, RetTarget::Discard, false, true)?;
                let base = self.frames.len() - 1;
                self.execute(base)
            }
            Func::Builtin(b) => self.call_builtin(&b, call, false),
        }
    }

    /// Calls a callable value with arguments (`call_user_func`).
    pub fn call_value(&mut self, callable: &Value, args: Vec<Value>) -> VmResult<Value> {
        let mut call = self.resolve_callable_for_call(callable)?;
        call.args = args.into_iter().map(Some).collect();
        self.call_pending(call)
    }

    /// Calls a callable with named arguments too.
    pub(crate) fn call_value_named(
        &mut self,
        callable: &Value,
        args: Vec<Value>,
        named: Vec<(ZStr, Value)>,
    ) -> VmResult<Value> {
        let mut call = self.resolve_callable_for_call(callable)?;
        call.args = args.into_iter().map(Some).collect();
        for (k, v) in named {
            let pos = if call.magic_name.is_some() {
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
                    if !call.func.is_variadic() && call.magic_name.is_none() {
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
        call.args = args.into_iter().map(Some).collect();
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
        let lc = ZStr::from(name.to_ascii_lowercase());
        let call = match cls.methods.get(&lc).cloned() {
            Some(m) => self.pending_for_method(&m, Some(o.clone()), cls),
            None => self.method_call(o, &ZStr::from(name))?,
        };
        let mut call = call;
        call.args = args.into_iter().map(Some).collect();
        self.call_pending(call)
    }

    /// Calls a static method by name.
    pub(crate) fn call_static_method(
        &mut self,
        cls: &Rc<Class>,
        name: &str,
        args: Vec<Value>,
    ) -> VmResult<Value> {
        let mut call = self.static_method_call(cls, &ZStr::from(name), false)?;
        call.args = args.into_iter().map(Some).collect();
        self.call_pending(call)
    }

    /// Calls a function by name.
    pub fn call_function(&mut self, name: &str, args: Vec<Value>) -> VmResult<Value> {
        let f = self
            .functions
            .get(&ZStr::from(name.to_ascii_lowercase()))
            .cloned();
        let Some(f) = f else {
            return Err(self.make_throw("Error", &format!("Call to undefined function {name}()")));
        };
        let mut call = PendingCall::new(f);
        call.args = args.into_iter().map(Some).collect();
        self.call_pending(call)
    }
}

/// Finds `::` in a callable string.
pub(crate) fn find_double_colon(s: &[u8]) -> Option<usize> {
    s.windows(2).position(|w| w == b"::")
}

//! `zend_is_callable_ex` (`Zend/zend_API.c`): resolution of callable values
//! with the scope rules, deprecations and error messages used by
//! `is_callable()`, `callable` type checks and `Closure::fromCallable()`.

use crate::class::Class;
use crate::func::Method;
use crate::object;
use crate::vm::{FrameKind, PendingCall, Vm, VmResult};
use php_runtime::{ArrayKey, Obj, Value, ZStr};
use std::rc::Rc;

/// `zend_fcall_info_cache` while resolving.
#[derive(Default)]
struct Fcc {
    calling: Option<Rc<Class>>,
    called: Option<Rc<Class>>,
    object: Option<Obj>,
}

/// The calling user frame (`EG(current_execute_data)` skipping internal
/// frames): its scope, `$this` and called scope.
struct FrameCtx {
    scope: Option<Rc<Class>>,
    this: Option<Obj>,
    called: Option<Rc<Class>>,
}

/// The resolved function of a callable.
enum Handler {
    /// A method or function.
    Method(Rc<Method>),
    /// A `__call`/`__callStatic` trampoline for the named method.
    Magic(Rc<Method>, ZStr),
    /// The `Closure::__invoke` trampoline.
    Invoke(PendingCall),
}

/// Result of resolving a callable: the call or the reason it is not
/// callable.
pub(crate) type Resolved = Result<PendingCall, String>;

fn instance_of(a: &Rc<Class>, b: &Rc<Class>) -> bool {
    a.is_subclass_of(b)
}

impl Vm {
    fn callable_frame_ctx(&self) -> FrameCtx {
        let f = self
            .frames
            .iter()
            .rev()
            .find(|f| f.kind != FrameKind::Builtin);
        FrameCtx {
            scope: f.and_then(|f| f.scope.clone()),
            this: f.and_then(|f| f.this.clone()),
            called: f.and_then(|f| match &f.this {
                Some(t) => Some(object::class_of(t).clone()),
                None => f.called.clone(),
            }),
        }
    }

    /// `Z_PARAM_FUNC` for extension functions that keep the callable
    /// (`zend_fcall_info_cache`): resolves `v` in the calling user
    /// frame's scope, emitting the callable deprecations. `Ok(Err(reason))`
    /// when `v` is not callable there.
    pub fn resolve_callable(&mut self, v: &Value) -> VmResult<Result<PendingCall, String>> {
        self.zend_is_callable(v, false)
    }

    /// The class a `self`/`parent`/`static` callable target stands for in
    /// the calling user frame (`fcc->calling_scope`); `None` for other
    /// names or when no such class is in scope.
    #[must_use]
    pub fn callable_keyword_class(&self, keyword: &[u8]) -> Option<Rc<Class>> {
        let ctx = self.callable_frame_ctx();
        match keyword.to_ascii_lowercase().as_slice() {
            b"self" => ctx.scope,
            b"parent" => ctx.scope.and_then(|s| s.parent.clone()),
            b"static" => ctx.called,
            _ => None,
        }
    }

    /// `zend_is_callable_ex`: resolves `v` in the current scope.
    /// Deprecations ("Use of \"self\" in callables …") are emitted unless
    /// `suppress_deprecations`.
    pub(crate) fn zend_is_callable(
        &mut self,
        v: &Value,
        suppress_deprecations: bool,
    ) -> VmResult<Resolved> {
        match v {
            Value::Reference(r) => {
                let inner = r.get();
                self.zend_is_callable(&inner, suppress_deprecations)
            }
            Value::Str(s) => {
                let name = s.as_bytes().strip_prefix(b"\\").unwrap_or(s.as_bytes());
                if let Some(f) = self
                    .functions
                    .get(name.to_ascii_lowercase().as_slice())
                    .cloned()
                {
                    return Ok(Ok(PendingCall::new(f)));
                }
                let mut fcc = Fcc::default();
                self.callable_check_func(s, &mut fcc, false, suppress_deprecations)
            }
            Value::Array(a) => {
                if a.len() != 2 {
                    return Ok(Err("array callback must have exactly two members".into()));
                }
                let (Some(target), Some(method)) =
                    (a.get(&ArrayKey::Int(0)), a.get(&ArrayKey::Int(1)))
                else {
                    return Ok(Err("array callback has to contain indices 0 and 1".into()));
                };
                let target = target.deref();
                let Value::Str(method) = method.deref() else {
                    if !matches!(target, Value::Str(_) | Value::Object(_)) {
                        return Ok(Err(
                            "first array member is not a valid class name or object".into(),
                        ));
                    }
                    return Ok(Err("second array member is not a valid method".into()));
                };
                let mut fcc = Fcc::default();
                let mut strict = false;
                match target {
                    Value::Str(cn) => {
                        let ctx = self.callable_frame_ctx();
                        if let Err(e) = self.callable_check_class(
                            &cn,
                            &ctx,
                            &mut fcc,
                            &mut strict,
                            suppress_deprecations,
                        )? {
                            return Ok(Err(e));
                        }
                    }
                    Value::Object(o) => {
                        fcc.calling = Some(object::class_of(&o).clone());
                        fcc.object = Some(o);
                    }
                    _ => {
                        return Ok(Err(
                            "first array member is not a valid class name or object".into(),
                        ))
                    }
                }
                self.callable_check_func(&method, &mut fcc, strict, suppress_deprecations)
            }
            Value::Object(o) => {
                if let Some(c) = self.closure_call(o) {
                    return Ok(Ok(c));
                }
                let cls = object::class_of(o).clone();
                match cls.magic.invoke.clone() {
                    Some(m) => Ok(Ok(PendingCall {
                        this: Some(o.clone()),
                        scope: Some(m.scope()),
                        called: Some(cls),
                        ..PendingCall::new(m.func.clone())
                    })),
                    None => Ok(Err("no array or string given".into())),
                }
            }
            _ => Ok(Err("no array or string given".into())),
        }
    }

    /// `zend_is_callable_check_class`.
    fn callable_check_class(
        &mut self,
        name: &ZStr,
        ctx: &FrameCtx,
        fcc: &mut Fcc,
        strict: &mut bool,
        suppress: bool,
    ) -> VmResult<Result<(), String>> {
        *strict = false;
        let lc = name.as_bytes().to_ascii_lowercase();
        match lc.as_slice() {
            b"self" => {
                let Some(scope) = ctx.scope.clone() else {
                    return Ok(Err(
                        "cannot access \"self\" when no class scope is active".into()
                    ));
                };
                if !suppress {
                    self.deprecated("Use of \"self\" in callables is deprecated")?;
                }
                fcc.called = ctx
                    .called
                    .clone()
                    .filter(|c| instance_of(c, &scope))
                    .or_else(|| Some(scope.clone()));
                fcc.calling = Some(scope);
                if fcc.object.is_none() {
                    fcc.object.clone_from(&ctx.this);
                }
            }
            b"parent" => {
                let Some(scope) = ctx.scope.clone() else {
                    return Ok(Err(
                        "cannot access \"parent\" when no class scope is active".into(),
                    ));
                };
                let Some(parent) = scope.parent.clone() else {
                    return Ok(Err(
                        "cannot access \"parent\" when current class scope has no parent".into(),
                    ));
                };
                if !suppress {
                    self.deprecated("Use of \"parent\" in callables is deprecated")?;
                }
                fcc.called = ctx
                    .called
                    .clone()
                    .filter(|c| instance_of(c, &parent))
                    .or_else(|| Some(parent.clone()));
                fcc.calling = Some(parent);
                if fcc.object.is_none() {
                    fcc.object.clone_from(&ctx.this);
                }
                *strict = true;
            }
            b"static" => {
                let Some(called) = ctx.called.clone() else {
                    return Ok(Err(
                        "cannot access \"static\" when no class scope is active".into(),
                    ));
                };
                if !suppress {
                    self.deprecated("Use of \"static\" in callables is deprecated")?;
                }
                fcc.called = Some(called.clone());
                fcc.calling = Some(called);
                if fcc.object.is_none() {
                    fcc.object.clone_from(&ctx.this);
                }
                *strict = true;
            }
            _ => {
                let Some(ce) = self.lookup_class(name)? else {
                    return Ok(Err(format!(
                        "class \"{}\" not found",
                        name.to_string_lossy()
                    )));
                };
                fcc.calling = Some(ce.clone());
                match (&ctx.scope, &fcc.object) {
                    (Some(fs), None) => match &ctx.this {
                        Some(t) if instance_of(object::class_of(t), fs) && instance_of(fs, &ce) => {
                            fcc.called = Some(object::class_of(t).clone());
                            fcc.object = Some(t.clone());
                        }
                        _ => fcc.called = Some(ce),
                    },
                    (_, Some(o)) => fcc.called = Some(object::class_of(o).clone()),
                    (None, None) => fcc.called = Some(ce),
                }
                *strict = true;
            }
        }
        Ok(Ok(()))
    }

    /// `zend_is_callable_check_func` (`zend_is_method_callable`).
    #[allow(clippy::too_many_lines)]
    fn callable_check_func(
        &mut self,
        callable: &ZStr,
        fcc: &mut Fcc,
        strict_in: bool,
        suppress: bool,
    ) -> VmResult<Resolved> {
        let ce_org = fcc.calling.take();
        let ctx = self.callable_frame_ctx();
        let mut strict = strict_in;
        let bytes = callable.as_bytes();
        let mname: ZStr = if let Some(p) = rfind_double_colon(bytes) {
            if p == 0 {
                return Ok(Err("invalid function name".into()));
            }
            let class_name = ZStr::from(&bytes[..p]);
            let mut cctx = FrameCtx {
                scope: ce_org.clone().or_else(|| ctx.scope.clone()),
                this: ctx.this.clone(),
                called: ctx.called.clone(),
            };
            if ce_org.is_some() {
                cctx.scope.clone_from(&ce_org);
            }
            if let Err(e) = self.callable_check_class(
                &class_name,
                &cctx,
                fcc,
                &mut strict,
                suppress || ce_org.is_some(),
            )? {
                return Ok(Err(e));
            }
            let calling = fcc.calling.clone().expect("calling scope");
            if let Some(org) = &ce_org {
                if !instance_of(org, &calling) {
                    return Ok(Err(format!(
                        "class {} is not a subclass of {}",
                        org.name, calling.name
                    )));
                }
                if !suppress {
                    self.deprecated(&format!(
                        "Callables of the form [\"{}\", \"{}\"] are deprecated",
                        org.name,
                        callable.to_string_lossy()
                    ))?;
                }
            }
            ZStr::from(&bytes[p + 2..])
        } else if let Some(org) = &ce_org {
            fcc.calling = Some(org.clone());
            callable.clone()
        } else {
            // `"%s"` of `ZSTR_VAL(callable)`: the name stops at a NUL byte.
            let shown = bytes.split(|&c| c == 0).next().unwrap_or_default();
            return Ok(Err(format!(
                "function \"{}\" not found or invalid function name",
                String::from_utf8_lossy(shown)
            )));
        };
        let lm = mname.to_ascii_lowercase();
        let mut handler: Option<Handler> = None;
        let mut via_handler = false;
        let calling = fcc.calling.clone();
        let found = calling.as_ref().and_then(|c| c.methods.get(&lm).cloned());
        let mut need_handler = false;
        if strict && calling.is_some() && lm.as_bytes() == b"__construct" {
            handler = calling
                .as_ref()
                .and_then(|c| c.magic.construct.clone())
                .map(Handler::Method);
        } else if let Some(mut m) = found {
            if !strict {
                if let Some(s) = &ctx.scope {
                    if instance_of(&m.scope(), s) {
                        if let Some(pm) = s.methods.get(&lm) {
                            if pm.visibility() == crate::func::flags::PRIVATE
                                && Rc::ptr_eq(&pm.scope(), s)
                            {
                                m = pm.clone();
                            }
                        }
                    }
                }
            }
            let cs = calling.as_ref().expect("calling scope");
            let has_magic = if fcc.object.is_some() {
                cs.magic.call.is_some()
            } else {
                cs.magic.call_static.is_some()
            };
            if m.visibility() != crate::func::flags::PUBLIC
                && has_magic
                && !self.can_access_method(&m, ctx.scope.as_ref())
            {
                need_handler = true;
            } else {
                handler = Some(Handler::Method(m));
            }
        } else {
            need_handler = true;
        }
        if need_handler {
            let same_org = match (&calling, &ce_org) {
                (Some(a), Some(b)) => Rc::ptr_eq(a, b),
                _ => false,
            };
            if let (Some(obj), true) = (fcc.object.clone(), same_org) {
                let org = ce_org.clone().expect("ce_org");
                if strict && org.magic.call.is_some() {
                    handler = org
                        .magic
                        .call
                        .clone()
                        .map(|c| Handler::Magic(c, mname.clone()));
                    via_handler = true;
                } else if lm.as_bytes() == b"__invoke"
                    && object::has_internal::<crate::closure::ClosureData>(&obj)
                {
                    if let Some(c) = self.closure_method_call(&obj, &lm) {
                        handler = Some(Handler::Invoke(c));
                        via_handler = true;
                    }
                } else if let Some(c) = org.magic.call.clone() {
                    handler = Some(Handler::Magic(c, mname.clone()));
                    via_handler = true;
                }
            } else if let Some(cs) = &calling {
                // zend_std_get_static_method.
                let m = cs.methods.get(&lm).cloned();
                let accessible = m
                    .as_ref()
                    .is_some_and(|m| self.can_access_method(m, ctx.scope.as_ref()));
                if let (Some(m), true) = (m.clone(), accessible) {
                    handler = Some(Handler::Method(m));
                } else if let Some(t) = ctx
                    .this
                    .as_ref()
                    .filter(|t| cs.magic.call.is_some() && instance_of(object::class_of(t), cs))
                {
                    handler = object::class_of(t)
                        .magic
                        .call
                        .clone()
                        .map(|c| Handler::Magic(c, mname.clone()));
                    via_handler = true;
                    fcc.object = Some(t.clone());
                } else if m.is_some() && cs.magic.call_static.is_none() {
                    handler = None;
                } else if let Some(c) = cs.magic.call_static.clone() {
                    handler = Some(Handler::Magic(c, mname.clone()));
                    via_handler = true;
                    if fcc.object.is_none() {
                        if let Some(t) = &ctx.this {
                            if instance_of(object::class_of(t), cs) {
                                fcc.object = Some(t.clone());
                            }
                        }
                    }
                }
            }
        }
        let Some(handler) = handler else {
            return Ok(Err(match &fcc.calling {
                Some(c) => format!(
                    "class {} does not have a method \"{}\"",
                    c.name,
                    mname.to_string_lossy()
                ),
                None => format!("function {}() does not exist", mname.to_string_lossy()),
            }));
        };
        if let (Some(cs), false, Handler::Method(m)) = (&fcc.calling, via_handler, &handler) {
            if m.is_abstract() {
                return Ok(Err(format!(
                    "cannot call abstract method {}::{}()",
                    cs.name, m.name
                )));
            }
            if fcc.object.is_none() && !m.is_static() {
                return Ok(Err(format!(
                    "non-static method {}::{}() cannot be called statically",
                    cs.name, m.name
                )));
            }
            if m.visibility() != crate::func::flags::PUBLIC
                && !self.can_access_method(m, ctx.scope.as_ref())
            {
                let vis = if m.visibility() == crate::func::flags::PRIVATE {
                    "private"
                } else {
                    "protected"
                };
                return Ok(Err(format!(
                    "cannot access {vis} method {}::{}()",
                    cs.name, m.name
                )));
            }
        }
        if let Some(o) = &fcc.object {
            fcc.called = Some(object::class_of(o).clone());
        }
        Ok(Ok(match handler {
            Handler::Invoke(c) => c,
            Handler::Method(m) => PendingCall {
                this: if m.is_static() {
                    None
                } else {
                    fcc.object.clone()
                },
                scope: Some(m.scope()),
                called: fcc.called.clone(),
                ..PendingCall::new(m.func.clone())
            },
            Handler::Magic(m, name) => {
                let is_static = m.name.eq_ignore_ascii_case(b"__callStatic");
                PendingCall {
                    this: if is_static { None } else { fcc.object.clone() },
                    scope: Some(m.scope()),
                    called: fcc.called.clone(),
                    magic_name: Some(name),
                    ..PendingCall::new(m.func.clone())
                }
            }
        }))
    }
}

/// Position of the last `::` in a callable name.
fn rfind_double_colon(s: &[u8]) -> Option<usize> {
    s.windows(2).rposition(|w| w == b"::")
}

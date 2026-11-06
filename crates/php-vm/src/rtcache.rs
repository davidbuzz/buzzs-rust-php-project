//! Run-time caches of named lookups: global constants, class constants,
//! static properties, static method calls and class names
//! (php-src's op-array run-time cache, `CACHED_PTR` / `CACHE_PTR` /
//! `CACHE_POLYMORPHIC_PTR` in `Zend/zend_execute.h`, as filled by
//! `ZEND_FETCH_CONSTANT`, `ZEND_FETCH_CLASS_CONSTANT`,
//! `ZEND_FETCH_STATIC_PROP_*`, `ZEND_INIT_STATIC_METHOD_CALL` and
//! `ZEND_INSTANCEOF` in `Zend/zend_vm_def.h`).
//!
//! Once an op resolved a name it keeps the result, so the next execution
//! skips the name hashing, visibility checks and deprecation probes. Slots
//! are indexed by the op's name literal, like [`OpArray::class_cache`] and
//! [`OpArray::fcall_cache`]; because literals are shared by every op of an
//! op array naming the same string, a slot remembers the class it was
//! filled for (php-src's polymorphic cache pair `ce, ptr`) and, for a
//! non-public member, the calling scope whose access check it passed.
//! A slot that does not match is a miss: the op takes its uncached path,
//! which raises every error, deprecation and autoload exactly as before,
//! and refills the slot.
//!
//! Only results that cannot change while the op array lives are cached:
//! classes, their constants, static property slots and methods are never
//! replaced once a request declared them (classes are held weakly, so the
//! cache keeps neither them nor its own op array alive); a cached global
//! constant value is one whose refcount is not observable (scalars and
//! interned strings), and a value found through the namespace fallback is
//! only reused while no constant was added (php-src instead caches the
//! global constant for good).
//!
//! [`OpArray::class_cache`]: crate::opcode::OpArray::class_cache
//! [`OpArray::fcall_cache`]: crate::opcode::OpArray::fcall_cache

use crate::class::Class;
use crate::func::{flags, Method};
use crate::object;
use crate::opcode::{ClassSpec, Operand};
use crate::vm::{FrameKind, PendingCall, Vm, VmResult};
use php_runtime::{Value, ZStr};
use std::cell::RefCell;
use std::rc::{Rc, Weak};

#[cfg(test)]
mod tests;

/// [`MemberSlot::class_lit`] of a slot not filled by a `ClassSpec::Named`
/// op.
const NO_CLASS_LIT: u32 = u32::MAX;

/// The run-time cache of one op array (see the module docs).
#[derive(Debug, Default)]
pub struct RuntimeCache {
    /// `FetchConst` results, by name literal.
    consts: RefCell<Vec<ConstSlot>>,
    /// `FetchClassConst` results, by constant-name literal.
    class_consts: RefCell<Vec<Option<MemberSlot>>>,
    /// Static property slots of `A::$name` places, by name literal.
    static_props: RefCell<Vec<Option<MemberSlot>>>,
    /// `InitStaticCall` method resolutions, by method-name literal.
    static_calls: RefCell<Vec<Option<CallSlot>>>,
}

/// A cached global constant.
#[derive(Debug, Default)]
enum ConstSlot {
    /// Not resolved yet.
    #[default]
    Empty,
    /// Resolved to something that must not be cached (a deprecated
    /// constant, a refcounted value, `__COMPILER_HALT_OFFSET__`, ...): the
    /// op always takes the uncached path.
    Never,
    /// The constant named by the literal itself.
    Direct(Value),
    /// The global fallback of an unqualified name in a namespace, valid
    /// while the constant table has `epoch` entries (a newly defined
    /// namespaced constant would take precedence).
    Fallback { value: Value, epoch: usize },
}

/// A cached class member (class constant or static property).
#[derive(Debug)]
struct MemberSlot {
    /// The class literal of the `ClassSpec::Named` op that filled the slot
    /// ([`NO_CLASS_LIT`] otherwise).
    class_lit: u32,
    /// The class the member was found in.
    class: Weak<Class>,
    /// Who may use the slot.
    access: Access,
    /// Index into the class's `consts` / `static_props`.
    idx: usize,
}

/// A cached static method call target.
#[derive(Debug)]
struct CallSlot {
    /// As [`MemberSlot::class_lit`].
    class_lit: u32,
    /// The class named by the call.
    class: Weak<Class>,
    /// As [`MemberSlot::access`].
    access: Access,
    /// The resolved method (static, or an instance method called with a
    /// compatible `$this`).
    method: Weak<Method>,
}

/// The access check a cached member passed.
#[derive(Debug)]
enum Access {
    /// A public member: valid from any scope.
    Public,
    /// A non-public member, found accessible from this calling scope.
    From(Weak<Class>),
}

/// Stores `x` at `i`, growing the slot vector as needed.
fn put<T: Default>(slots: &RefCell<Vec<T>>, i: usize, x: T) {
    let mut v = slots.borrow_mut();
    if v.len() <= i {
        v.resize_with(i + 1, T::default);
    }
    v[i] = x;
}

/// Whether a constant value may be shared by the cache: values whose
/// refcount `debug_zval_dump()` cannot observe.
fn immutable_value(v: &Value) -> bool {
    match v {
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) => true,
        Value::Str(s) => s.is_interned(),
        _ => false,
    }
}

impl Vm {
    // ------------------------------------------------------------------
    // Class resolution without side effects
    // ------------------------------------------------------------------

    /// The `ClassSpec::Named` class an op array already resolved
    /// ([`OpArray::class_cache`](crate::opcode::OpArray::class_cache)).
    #[inline]
    pub(crate) fn peek_named_class(&self, lit: u32) -> Option<Rc<Class>> {
        self.frame()
            .code
            .class_cache
            .borrow()
            .get(lit as usize)
            .and_then(Weak::upgrade)
    }

    /// Records `cls` as the class literal `lit` resolves to.
    pub(crate) fn remember_named_class(&self, lit: u32, cls: &Rc<Class>) {
        let code = &self.frame().code;
        let mut cache = code.class_cache.borrow_mut();
        let slot = lit as usize;
        if cache.len() <= slot {
            cache.resize(slot + 1, Weak::new());
        }
        cache[slot] = Rc::downgrade(cls);
    }

    /// The called scope (`static`), as `resolve_class_spec` finds it.
    #[inline]
    fn rt_called(&self) -> Option<Rc<Class>> {
        self.frames
            .iter()
            .rev()
            .find(|f| f.kind != FrameKind::Builtin)
            .and_then(|f| f.called.clone())
    }

    /// The class `spec` resolves to when `resolve_class_spec` would succeed
    /// without side effects (no autoloading, no operand reads); `None`
    /// otherwise.
    fn rt_peek_class(&self, spec: &ClassSpec) -> Option<Rc<Class>> {
        match spec {
            ClassSpec::Named(l) => self.peek_named_class(*l),
            ClassSpec::SelfClass => self.scope(),
            ClassSpec::Parent => self.scope()?.parent.clone(),
            ClassSpec::Static => self.rt_called(),
            ClassSpec::Dyn(_) => None,
        }
    }

    /// The class `spec` resolves to, if it is the one a slot was filled
    /// for (`lit`, `class`).
    #[inline]
    fn rt_class(&self, spec: &ClassSpec, lit: u32, class: &Weak<Class>) -> Option<Rc<Class>> {
        let c = match spec {
            ClassSpec::Named(l) if *l == lit => return class.upgrade(),
            ClassSpec::Named(l) => self.peek_named_class(*l)?,
            ClassSpec::SelfClass => self.scope()?,
            ClassSpec::Parent => self.scope()?.parent.clone()?,
            ClassSpec::Static => self.rt_called()?,
            ClassSpec::Dyn(_) => return None,
        };
        (Rc::as_ptr(&c) == class.as_ptr()).then_some(c)
    }

    /// Whether a slot's access check holds for the calling scope.
    #[inline]
    fn rt_access_ok(&self, access: &Access) -> bool {
        match access {
            Access::Public => true,
            Access::From(w) => self.scope().is_some_and(|s| Rc::as_ptr(&s) == w.as_ptr()),
        }
    }

    /// The [`Access`] of a member that just passed its access check
    /// (`None`: a non-public member without a calling scope, not cached).
    fn rt_access(&self, public: bool) -> Option<Access> {
        if public {
            Some(Access::Public)
        } else {
            self.scope().map(|s| Access::From(Rc::downgrade(&s)))
        }
    }

    /// The name literal `lit` of the running op array, if a string.
    fn rt_lit_str(&self, lit: u32) -> Option<ZStr> {
        match self.frame().code.literals.get(lit as usize) {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        }
    }

    // ------------------------------------------------------------------
    // Global constants (ZEND_FETCH_CONSTANT)
    // ------------------------------------------------------------------

    /// `Op::FetchConst` through the cache.
    #[inline]
    pub(crate) fn fetch_constant_cached(
        &mut self,
        name: u32,
        fallback: Option<u32>,
    ) -> VmResult<Value> {
        let fill = match self.const_probe(name, fallback.is_some()) {
            Ok(v) => return Ok(v),
            Err(fill) => fill,
        };
        let v = self.fetch_constant(name, fallback)?;
        if fill {
            self.const_fill(name, fallback);
        }
        Ok(v)
    }

    /// The cached value, or whether the slot should be filled after the
    /// uncached fetch.
    #[inline]
    fn const_probe(&self, name: u32, has_fallback: bool) -> Result<Value, bool> {
        let cache = self.frame().code.rt_cache.consts.borrow();
        match cache.get(name as usize) {
            Some(ConstSlot::Direct(v)) => Ok(v.clone()),
            Some(ConstSlot::Fallback { value, epoch })
                if has_fallback && *epoch == self.constants.len() =>
            {
                Ok(value.clone())
            }
            Some(ConstSlot::Never) => Err(false),
            _ => Err(true),
        }
    }

    /// Fills the slot of a `FetchConst` that just succeeded, repeating the
    /// lookup order of [`Vm::fetch_constant`].
    #[cold]
    #[inline(never)]
    fn const_fill(&self, name: u32, fallback: Option<u32>) {
        let slot = self.const_slot(name, fallback);
        put(&self.frame().code.rt_cache.consts, name as usize, slot);
    }

    fn const_slot(&self, name: u32, fallback: Option<u32>) -> ConstSlot {
        let cacheable = |k: &ZStr, v: &Value| {
            immutable_value(v)
                // `SID` is the one constant whose value changes at runtime
                // (`session_start()`; php-src updates its zval in place).
                && k.as_bytes() != b"SID"
                && !self.deprecated_constants.contains_key(k)
                && !self.const_attributes.contains_key(k)
        };
        let Some(n) = self.rt_lit_str(name) else {
            return ConstSlot::Never;
        };
        if let Some(v) = self.constants.get(&n) {
            return if cacheable(&n, v) {
                ConstSlot::Direct(v.clone())
            } else {
                ConstSlot::Never
            };
        }
        let Some(f) = fallback.and_then(|fb| self.rt_lit_str(fb)) else {
            return ConstSlot::Never;
        };
        match self.constants.get(&f) {
            Some(v) if cacheable(&f, v) => ConstSlot::Fallback {
                value: v.clone(),
                epoch: self.constants.len(),
            },
            // Not found (a special or case-insensitively namespaced
            // constant) or not cacheable: leave the slot empty so that a
            // later definition is still picked up.
            _ => ConstSlot::Empty,
        }
    }

    // ------------------------------------------------------------------
    // Class constants (ZEND_FETCH_CLASS_CONSTANT)
    // ------------------------------------------------------------------

    /// `Op::FetchClassConst` through the cache.
    #[inline]
    pub(crate) fn fetch_class_const_cached(
        &mut self,
        spec: &ClassSpec,
        name: Operand,
    ) -> VmResult<Value> {
        if let Operand::Lit(l) = name {
            if let Some(v) = self.class_const_hit(spec, l) {
                return Ok(v);
            }
        }
        let n = self.read(name)?;
        let v = self.op_fetch_class_const(spec, n)?;
        if let Operand::Lit(l) = name {
            self.class_const_fill(spec, l);
        }
        Ok(v)
    }

    #[inline]
    fn class_const_hit(&self, spec: &ClassSpec, name: u32) -> Option<Value> {
        let cache = self.frame().code.rt_cache.class_consts.borrow();
        let e = cache.get(name as usize)?.as_ref()?;
        let cls = self.rt_class(spec, e.class_lit, &e.class)?;
        if !self.rt_access_ok(&e.access) {
            return None;
        }
        let v = cls.consts.get(e.idx)?.value.borrow().clone();
        v
    }

    /// Fills the slot of a `FetchClassConst` that just succeeded: an
    /// evaluated constant of a class (not a trait) without
    /// `#[\Deprecated]`.
    #[cold]
    #[inline(never)]
    fn class_const_fill(&self, spec: &ClassSpec, name: u32) {
        let Some(cls) = self.rt_peek_class(spec) else {
            return;
        };
        if cls.is_trait() {
            return;
        }
        let Some(n) = self.rt_lit_str(name) else {
            return;
        };
        let Some(&idx) = cls.const_map.get(n.as_bytes()) else {
            return;
        };
        let c = &cls.consts[idx];
        if c.value.borrow().is_none()
            || c.attributes
                .iter()
                .any(|a| a.name.eq_ignore_ascii_case(b"Deprecated"))
        {
            return;
        }
        let Some(access) = self.rt_access(c.flags & flags::VISIBILITY == flags::PUBLIC) else {
            return;
        };
        let class_lit = match spec {
            ClassSpec::Named(l) => *l,
            _ => NO_CLASS_LIT,
        };
        let slot = MemberSlot {
            class_lit,
            class: Rc::downgrade(&cls),
            access,
            idx,
        };
        put(
            &self.frame().code.rt_cache.class_consts,
            name as usize,
            Some(slot),
        );
    }

    // ------------------------------------------------------------------
    // Static properties (ZEND_FETCH_STATIC_PROP_*)
    // ------------------------------------------------------------------

    /// The cached storage slot of the `A::$name` place base, if any: a
    /// declared, initialised static property of a class (not a trait)
    /// without asymmetric visibility, accessible from the calling scope.
    #[inline]
    pub(crate) fn static_prop_hit(
        &self,
        spec: &ClassSpec,
        name: Operand,
    ) -> Option<Rc<RefCell<Value>>> {
        let Operand::Lit(l) = name else {
            return None;
        };
        let cache = self.frame().code.rt_cache.static_props.borrow();
        let e = cache.get(l as usize)?.as_ref()?;
        let cls = self.rt_class(spec, e.class_lit, &e.class)?;
        if !self.rt_access_ok(&e.access) {
            return None;
        }
        let sp = cls.static_props.get(e.idx)?;
        // What `static_prop_slot` does on every fetch of an initialised
        // property of a class.
        crate::typed_refs::note_static_prop(sp);
        Some(sp.cell.clone())
    }

    /// Fills the slot of an `A::$name` place base that just resolved to a
    /// storage cell.
    #[cold]
    #[inline(never)]
    pub(crate) fn static_prop_fill(&self, spec: &ClassSpec, name: Operand) {
        let Operand::Lit(l) = name else {
            return;
        };
        let Some(cls) = self.rt_peek_class(spec) else {
            return;
        };
        if cls.is_trait() {
            return;
        }
        let Some(n) = self.rt_lit_str(l) else {
            return;
        };
        let Some(&idx) = cls.static_map.get(n.as_bytes()) else {
            return;
        };
        let sp = &cls.static_props[idx];
        if !sp.initialized.get() || sp.flags & flags::SET_MASK != 0 {
            return;
        }
        let Some(access) = self.rt_access(sp.flags & flags::VISIBILITY == flags::PUBLIC) else {
            return;
        };
        let class_lit = match spec {
            ClassSpec::Named(c) => *c,
            _ => NO_CLASS_LIT,
        };
        let slot = MemberSlot {
            class_lit,
            class: Rc::downgrade(&cls),
            access,
            idx,
        };
        put(
            &self.frame().code.rt_cache.static_props,
            l as usize,
            Some(slot),
        );
    }

    // ------------------------------------------------------------------
    // Static method calls (ZEND_INIT_STATIC_METHOD_CALL)
    // ------------------------------------------------------------------

    /// The pending call of `A::name()` from the cache, if the op resolved
    /// the same class before and the method is still callable the same
    /// way: a static method, or an instance method with a compatible
    /// `$this` (`parent::__construct()`).
    #[inline]
    pub(crate) fn static_call_hit(&self, spec: &ClassSpec, name: Operand) -> Option<PendingCall> {
        let Operand::Lit(l) = name else {
            return None;
        };
        let cache = self.frame().code.rt_cache.static_calls.borrow();
        let e = cache.get(l as usize)?.as_ref()?;
        let cls = self.rt_class(spec, e.class_lit, &e.class)?;
        if !self.rt_access_ok(&e.access) {
            return None;
        }
        let m = e.method.upgrade()?;
        let caller = self
            .frames
            .iter()
            .rev()
            .find(|f| f.kind != FrameKind::Builtin);
        if m.is_static() {
            // `forwarded_called` (call.rs): `self::`/`parent::`/`static::`
            // forward the called scope when it is a subclass.
            let forward = matches!(
                spec,
                ClassSpec::SelfClass | ClassSpec::Parent | ClassSpec::Static
            );
            let called = if forward {
                caller
                    .and_then(|f| f.called.clone())
                    .filter(|c| c.is_subclass_of(&cls))
                    .unwrap_or(cls)
            } else {
                cls
            };
            return Some(PendingCall {
                scope: Some(m.scope()),
                called: Some(called),
                ..PendingCall::new(m.func.clone())
            });
        }
        let this = caller.and_then(|f| f.this.clone())?;
        let called = object::class_of(&this).clone();
        if !called.is_subclass_of(&cls) {
            return None;
        }
        Some(PendingCall {
            this: Some(this),
            scope: Some(m.scope()),
            called: Some(called),
            ..PendingCall::new(m.func.clone())
        })
    }

    /// Fills the slot of an `A::name()` whose call `call` was just set up
    /// for the (non-trait) class `cls`: only a method found by name and
    /// accessible (no `__call`/`__callStatic` trampoline).
    #[cold]
    #[inline(never)]
    pub(crate) fn static_call_fill(
        &self,
        spec: &ClassSpec,
        name: Operand,
        cls: &Rc<Class>,
        call: &PendingCall,
    ) {
        let Operand::Lit(l) = name else {
            return;
        };
        if call.magic_name.is_some() || cls.is_trait() || matches!(spec, ClassSpec::Dyn(_)) {
            return;
        }
        let Some(n) = self.rt_lit_str(l) else {
            return;
        };
        let lc = ZStr::from(n.to_ascii_lowercase());
        let Some(m) = cls.methods.get(&lc) else {
            return;
        };
        if m.is_abstract() || !Self::same_func(&m.func, &call.func) {
            return;
        }
        let Some(access) = self.rt_access(m.visibility() == flags::PUBLIC) else {
            return;
        };
        let class_lit = match spec {
            ClassSpec::Named(c) => *c,
            _ => NO_CLASS_LIT,
        };
        let slot = CallSlot {
            class_lit,
            class: Rc::downgrade(cls),
            access,
            method: Rc::downgrade(m),
        };
        put(
            &self.frame().code.rt_cache.static_calls,
            l as usize,
            Some(slot),
        );
    }

    /// Whether two function handles are the same function.
    fn same_func(a: &crate::func::Func, b: &crate::func::Func) -> bool {
        use crate::func::Func;
        match (a, b) {
            (Func::User(x), Func::User(y)) => Rc::ptr_eq(x, y),
            (Func::Builtin(x), Func::Builtin(y)) => Rc::ptr_eq(x, y),
            _ => false,
        }
    }
}

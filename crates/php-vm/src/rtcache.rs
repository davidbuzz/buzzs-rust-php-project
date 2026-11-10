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
//! op array naming the same string (`A::X` and `B::X`), a member slot
//! holds up to [`POLY_ENTRIES`] entries, each remembering the class it was
//! filled for (php-src's polymorphic cache pair `ce, ptr`) and, for a
//! non-public member, the calling scope whose access check it passed.
//! No matching entry is a miss: the op takes its uncached path, which
//! raises every error, deprecation and autoload exactly as before, and
//! adds an entry.
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
use crate::opcode::{ClassSpec, Operand, PlaceBase, PlaceId};
use crate::vm::{FrameKind, PendingCall, Vm, VmResult};
use php_runtime::{Value, ZStr};
use std::cell::RefCell;
use std::rc::{Rc, Weak};

/// A cached static property entry for fast-path accesses.
#[derive(Clone, Debug)]
pub struct StaticPropCacheEntry {
    /// Address of calling scope (`Class`), 0 if no scope.
    pub scope_ptr: usize,
    /// Address of called class (`Class`), 0 if no called class.
    pub called_ptr: usize,
    /// Address of the resolved class holding the static property.
    pub class_ptr: usize,
    /// Direct storage cell of the static property.
    pub cell: Rc<RefCell<Value>>,
    /// Whether the property is untyped/plain (can mutate directly in place without re-checking type hints or visibility).
    pub fast_plain: bool,
}

impl Default for StaticPropCacheEntry {
    fn default() -> Self {
        Self {
            scope_ptr: 0,
            called_ptr: 0,
            class_ptr: 0,
            cell: Rc::new(RefCell::new(Value::Null)),
            fast_plain: false,
        }
    }
}

#[cfg(test)]
mod tests;

/// [`MemberSlot::class_lit`] of an entry not filled by a
/// `ClassSpec::Named` op.
const NO_CLASS_LIT: u32 = u32::MAX;

/// Entries per member slot: the classes one name literal of an op array
/// is cached for at a time (`static::X` in a base class method sees one
/// class per subclass; past this many the oldest entry is replaced).
const POLY_ENTRIES: usize = 8;

/// The run-time cache of one op array (see the module docs).
#[derive(Debug, Default)]
pub struct RuntimeCache {
    /// `FetchConst` results, by name literal.
    consts: RefCell<Vec<ConstSlot>>,
    /// `FetchClassConst` results, by constant-name literal.
    class_consts: RefCell<Vec<Vec<MemberSlot>>>,
    /// Static property slots of `A::$name` places, by name literal.
    static_props: RefCell<Vec<Vec<MemberSlot>>>,
    /// Fast static property entries, by place id.
    pub static_prop_entries: RefCell<Vec<StaticPropCacheEntry>>,
    /// `InitStaticCall` method resolutions, by method-name literal.
    static_calls: RefCell<Vec<Vec<MemberSlot>>>,
    /// Global variable resolutions, by place id.
    pub(crate) globals: RefCell<Vec<GlobalSlot>>,
}

impl RuntimeCache {
    /// Clears all cached lookups so an op array carries no request state.
    pub fn clear(&self) {
        self.consts.borrow_mut().clear();
        self.class_consts.borrow_mut().clear();
        self.static_props.borrow_mut().clear();
        self.static_prop_entries.borrow_mut().clear();
        self.static_calls.borrow_mut().clear();
        self.globals.borrow_mut().clear();
    }
}

/// A cached global variable lookup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GlobalSlot {
    /// Not resolved yet.
    #[default]
    Empty,
    /// Resolved to CV `cv` in the main frame (`self.frames[0]`, the VM's global scope).
    GlobalCv(u32),
    /// Resolved to slot `slot` in the global symbol table (`self.globals`).
    GlobalStore(u32),
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

/// One entry of a member slot: a class member (class constant, static
/// property or method) as looked up through one class.
#[derive(Debug)]
struct MemberSlot {
    /// The class literal of the `ClassSpec::Named` op that filled the
    /// entry ([`NO_CLASS_LIT`] otherwise).
    class_lit: u32,
    /// The class the member was looked up in.
    class: Weak<Class>,
    /// Who may use the entry.
    access: Access,
    /// The member.
    target: Target,
}

/// What a [`MemberSlot`] caches.
#[derive(Debug)]
enum Target {
    /// The value of a class constant whose refcount is not observable
    /// (see [`immutable_value`]): read without touching the class.
    Value(Value),
    /// Index into the class's `consts` / `static_props`.
    Index(usize),
    /// Direct storage cell and typed flag of a static property.
    StaticProp {
        cell: Rc<RefCell<Value>>,
        typed: bool,
        idx: usize,
    },
    /// The resolved method of a static call (a static method, or an
    /// instance method called with a compatible `$this`).
    Method(Weak<Method>),
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

/// Adds `x` to the member slot `i`: it replaces the entry for the same
/// class (filled under another scope or by another spelling of the class),
/// and the oldest entry once the slot is full.
fn put_member(slots: &RefCell<Vec<Vec<MemberSlot>>>, i: usize, x: MemberSlot) {
    let mut v = slots.borrow_mut();
    if v.len() <= i {
        v.resize_with(i + 1, Vec::new);
    }
    let entries = &mut v[i];
    entries.retain(|e| e.class.as_ptr() != x.class.as_ptr());
    if entries.len() >= POLY_ENTRIES {
        entries.remove(0);
    }
    entries.push(x);
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

/// The value a class-constant entry caches for the class `cls` it was
/// filled for.
fn class_const_value(cls: &Class, e: &MemberSlot) -> Option<Value> {
    match &e.target {
        Target::Value(v) => Some(v.clone()),
        Target::Index(idx) => cls.consts.get(*idx)?.value.borrow().clone(),
        Target::StaticProp { .. } | Target::Method(_) => None,
    }
}

/// A cached `A::name()` call ([`Vm::static_call_hit`]): the parts of its
/// `PendingCall`.
pub(crate) struct StaticCallHit {
    /// The method's function.
    pub(crate) func: crate::func::Func,
    /// `$this` (an instance method called with a compatible `$this`).
    pub(crate) this: Option<php_runtime::Obj>,
    /// The method's declaring class.
    pub(crate) scope: Rc<Class>,
    /// The called class (`static::`).
    pub(crate) called: Rc<Class>,
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

    /// `ZEND_CATCH`: whether the exception class `cls` is an instance of
    /// the class named by literal `lit`, looked up without autoloading and
    /// cached once found (a class not declared yet catches nothing).
    pub(crate) fn catch_class_matches(&self, cls: &Rc<Class>, lit: u32) -> bool {
        let catch_ce = if let Some(c) = self.peek_named_class(lit) {
            c
        } else {
            let Some(c) = self
                .rt_lit_str(lit)
                .and_then(|n| self.find_class(n.as_bytes()))
            else {
                return false;
            };
            self.remember_named_class(lit, &c);
            c
        };
        cls.is_subclass_of(&catch_ce)
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

    /// The entry of a member slot filled for the class `spec` resolves to
    /// and valid in the calling scope, with that class.
    #[inline]
    fn rt_find<'a>(
        &self,
        spec: &ClassSpec,
        entries: &'a [MemberSlot],
    ) -> Option<(Rc<Class>, &'a MemberSlot)> {
        let c = match spec {
            ClassSpec::Named(l) => {
                if let Some(e) = entries.iter().find(|e| e.class_lit == *l) {
                    let c = e.class.upgrade()?;
                    return self.rt_access_ok(&e.access).then_some((c, e));
                }
                self.peek_named_class(*l)?
            }
            ClassSpec::SelfClass => self.frame().scope.clone()?,
            ClassSpec::Parent => self.frame().scope.as_ref()?.parent.clone()?,
            ClassSpec::Static => self.rt_called()?,
            ClassSpec::Dyn(_) => return None,
        };
        let e = entries
            .iter()
            .find(|e| Rc::as_ptr(&c) == e.class.as_ptr() && self.rt_access_ok(&e.access))?;
        Some((c, e))
    }

    /// Whether a slot's access check holds for the calling scope.
    #[inline(always)]
    fn rt_access_ok(&self, access: &Access) -> bool {
        match access {
            Access::Public => true,
            Access::From(w) => self
                .frame()
                .scope
                .as_ref()
                .is_some_and(|s| Rc::as_ptr(s) == w.as_ptr()),
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
        match self.const_probe(name, fallback.is_some()) {
            Ok(v) => Ok(v),
            Err(fill) => {
                let v = self.fetch_constant(name, fallback)?;
                if fill {
                    self.const_fill(name, fallback);
                }
                Ok(v)
            }
        }
    }

    /// The cached value of `Op::FetchConst` (name literal `name`,
    /// namespace fallback `fallback`), if the slot holds one: no lookup,
    /// no diagnostic (the hot pre-match of `Vm::run`).
    #[inline]
    pub(crate) fn const_hit(&self, name: u32, fallback: Option<u32>) -> Option<Value> {
        self.const_probe(name, fallback.is_some()).ok()
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
            // Not cacheable, or found otherwise (the case-insensitive
            // namespace lookup): every fetch stays uncached, which also
            // sees a namespaced constant defined later.
            _ => ConstSlot::Never,
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
        let Operand::Lit(l) = name else {
            let n = self.read(name)?;
            return self.op_fetch_class_const(spec, n);
        };
        // `$obj::X` (`ZEND_FETCH_CLASS_CONSTANT` with a VAR class operand
        // caches the constant for the class, `CACHED_PTR(...) == ce`): the
        // class of the object the operand holds, peeked before the generic
        // path consumes the operand.
        let dyn_class = match spec {
            ClassSpec::Dyn(op) => Some(self.rt_dyn_class(*op)),
            _ => None,
        };
        if let Some(v) = self.class_const_hit(spec, l, dyn_class.as_ref()) {
            if let ClassSpec::Dyn(op) = spec {
                self.rt_release_dyn_class(*op);
            }
            return Ok(v);
        }
        let n = self.read(name)?;
        let v = self.op_fetch_class_const(spec, n)?;
        self.class_const_fill(spec, l, dyn_class.flatten());
        Ok(v)
    }

    /// The cached value of `A::NAME` (`name` the constant-name literal) for
    /// a class named or `self`/`parent`/`static`, if the op's slot holds
    /// one valid for the calling scope: no lookup, no diagnostic, nothing
    /// consumed (the hot pre-match of `Vm::run`). A dynamic class (its
    /// operand is consumed on a hit) gives `None`.
    #[inline]
    pub(crate) fn class_const_cached(&self, spec: &ClassSpec, name: u32) -> Option<Value> {
        if matches!(spec, ClassSpec::Dyn(_)) {
            return None;
        }
        self.class_const_hit(spec, name, None)
    }

    /// The class of the object a dynamic class operand (`$obj::X`) holds,
    /// looked at without reading the operand: a temporary or a compiled
    /// variable holding the object directly. A class name string, a
    /// reference or anything else gives `None` (not cached).
    #[inline]
    fn rt_dyn_class(&self, op: Operand) -> Option<Rc<Class>> {
        let f = self.frame();
        match op {
            Operand::Tmp(t) | Operand::TmpKeep(t) => match f.tmps.get(t as usize) {
                Some(Value::Object(o)) => Some(object::class_of(o).clone()),
                _ => None,
            },
            Operand::Cv(cv) => f.peek_cv(cv, |slot| match slot {
                Some(Value::Object(o)) => Some(object::class_of(o).clone()),
                _ => None,
            }),
            Operand::Lit(_) | Operand::Unused => None,
        }
    }

    /// Releases a consumed temporary class operand after a cache hit,
    /// as the read of the generic path would have (`FREE_OP1`).
    #[inline]
    fn rt_release_dyn_class(&mut self, op: Operand) {
        if let Operand::Tmp(t) = op {
            let v = std::mem::take(&mut self.frame_mut().tmps[t as usize]);
            drop(v);
        }
    }

    /// The cached value of the class constant named by literal `name`
    /// for the class `spec` resolves to. `dyn_class` is `Some` for a
    /// `ClassSpec::Dyn` spec: the class [`Vm::rt_dyn_class`] found, if any.
    #[inline(always)]
    fn class_const_hit(
        &self,
        spec: &ClassSpec,
        name: u32,
        dyn_class: Option<&Option<Rc<Class>>>,
    ) -> Option<Value> {
        let cache = self.frame().code.rt_cache.class_consts.borrow();
        let entries = cache.get(name as usize)?;
        if let Some(cls) = dyn_class {
            let cls = cls.as_ref()?;
            let e = entries
                .iter()
                .find(|e| Rc::as_ptr(cls) == e.class.as_ptr() && self.rt_access_ok(&e.access))?;
            return class_const_value(cls, e);
        }
        // `A::X` of a public scalar constant: php-src's `CACHED_PTR` fast
        // path, the class itself is not needed.
        if let ClassSpec::Named(l) = spec {
            if let Some(MemberSlot {
                class,
                access: Access::Public,
                target: Target::Value(v),
                ..
            }) = entries.iter().find(|e| e.class_lit == *l)
            {
                if class.strong_count() > 0 {
                    return Some(v.clone());
                }
            }
        } else if let ClassSpec::SelfClass = spec {
            if let Some(scope) = &self.frame().scope {
                if let Some(e) = entries
                    .iter()
                    .find(|e| Rc::as_ptr(scope) == e.class.as_ptr() && self.rt_access_ok(&e.access))
                {
                    if let Target::Value(v) = &e.target {
                        return Some(v.clone());
                    }
                    return class_const_value(scope, e);
                }
            }
        }
        let (cls, e) = self.rt_find(spec, entries)?;
        class_const_value(&cls, e)
    }

    /// Fills the slot of a `FetchClassConst` that just succeeded: an
    /// evaluated constant of a class (not a trait) without
    /// `#[\Deprecated]`. `dyn_class` is the class a `ClassSpec::Dyn`
    /// operand held before the fetch consumed it.
    #[cold]
    #[inline(never)]
    fn class_const_fill(&self, spec: &ClassSpec, name: u32, dyn_class: Option<Rc<Class>>) {
        let Some(cls) = dyn_class.or_else(|| self.rt_peek_class(spec)) else {
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
        if c.attributes
            .iter()
            .any(|a| a.name.eq_ignore_ascii_case(b"Deprecated"))
        {
            return;
        }
        let target = match c.value.borrow().as_ref() {
            None => return,
            Some(v) if immutable_value(v) => Target::Value(v.clone()),
            Some(_) => Target::Index(idx),
        };
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
            target,
        };
        put_member(
            &self.frame().code.rt_cache.class_consts,
            name as usize,
            slot,
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
        self.static_prop_cached(spec, l).map(|(cell, _)| cell)
    }

    /// [`Vm::static_prop_hit`] for the literal name `name`, also telling
    /// whether the property is typed (a type other than `mixed`: its
    /// writes are checked and its null may mean "uninitialized", see
    /// `typed_refs::note_static_prop`). The static-property ops'
    /// fast paths (`exec/spec/statics.rs`) take only untyped properties
    /// where a write would need the type check.
    #[inline]
    pub(crate) fn static_prop_cached(
        &self,
        spec: &ClassSpec,
        name: u32,
    ) -> Option<(Rc<RefCell<Value>>, bool)> {
        let cache = self.frame().code.rt_cache.static_props.borrow();
        let entries = cache.get(name as usize)?;
        if let ClassSpec::Named(l) = spec {
            if let Some(e) = entries
                .iter()
                .find(|e| e.class_lit == *l && matches!(e.access, Access::Public))
            {
                if let Target::StaticProp { cell, typed, idx } = &e.target {
                    if *typed {
                        if let Some(cls) = e.class.upgrade() {
                            if let Some(sp) = cls.static_props.get(*idx) {
                                crate::typed_refs::note_static_prop(sp);
                            }
                        }
                    }
                    return Some((cell.clone(), *typed));
                }
            }
        }
        let (cls, e) = self.rt_find(spec, entries)?;
        match &e.target {
            Target::StaticProp { cell, typed, idx } => {
                if *typed {
                    if let Some(sp) = cls.static_props.get(*idx) {
                        crate::typed_refs::note_static_prop(sp);
                    }
                }
                Some((cell.clone(), *typed))
            }
            Target::Index(idx) => {
                let sp = cls.static_props.get(*idx)?;
                let typed = !sp.ty.as_ref().is_none_or(crate::types::TypeDecl::is_mixed);
                if typed {
                    crate::typed_refs::note_static_prop(sp);
                }
                Some((sp.cell.clone(), typed))
            }
            _ => None,
        }
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
        let typed = !sp.ty.as_ref().is_none_or(crate::types::TypeDecl::is_mixed);
        let slot = MemberSlot {
            class_lit,
            class: Rc::downgrade(&cls),
            access,
            target: Target::StaticProp {
                cell: sp.cell.clone(),
                typed,
                idx,
            },
        };
        put_member(&self.frame().code.rt_cache.static_props, l as usize, slot);
    }

    /// Monomorphic probe of the static property cache for place `pid`.
    #[inline(always)]
    pub(crate) fn static_prop_cache_probe(&self, pid: PlaceId) -> Option<StaticPropCacheEntry> {
        let frame = self.frame();
        if frame.kind == FrameKind::Builtin {
            return None;
        }
        let cache = frame.code.rt_cache.static_prop_entries.borrow();
        let entry = cache.get(pid as usize)?;
        if entry.class_ptr == 0 {
            return None;
        }
        let scope_ptr = frame.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
        let called_ptr = frame.called.as_ref().map_or(0, |c| Rc::as_ptr(c) as usize);

        let place = frame.code.places.get(pid as usize)?;
        let PlaceBase::StaticProp { class, .. } = &place.base else {
            return None;
        };
        let ok = match class {
            ClassSpec::SelfClass | ClassSpec::Parent => {
                entry.scope_ptr == scope_ptr && entry.scope_ptr != 0
            }
            ClassSpec::Static => entry.called_ptr == called_ptr && entry.called_ptr != 0,
            ClassSpec::Named(l) => {
                (entry.scope_ptr == scope_ptr || entry.scope_ptr == 0)
                    && (frame
                        .code
                        .class_cache
                        .borrow()
                        .get(*l as usize)
                        .map_or(0, |w| w.as_ptr() as usize)
                        == entry.class_ptr
                        || entry.class_ptr != 0)
            }
            ClassSpec::Dyn(_) => false,
        };
        if ok {
            Some(entry.clone())
        } else {
            None
        }
    }

    /// Fills the fast static property cache for place `pid`.
    #[cold]
    #[inline(never)]
    pub(crate) fn static_prop_cache_fill(
        &self,
        pid: PlaceId,
        spec: &ClassSpec,
        _name: u32,
        cell: &Rc<RefCell<Value>>,
        fast_plain: bool,
    ) {
        let frame = self.frame();
        if frame.kind == FrameKind::Builtin {
            return;
        }
        let scope_ptr = frame.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
        let called_ptr = frame.called.as_ref().map_or(0, |c| Rc::as_ptr(c) as usize);
        let class_ptr = match spec {
            ClassSpec::SelfClass => scope_ptr,
            ClassSpec::Static => called_ptr,
            ClassSpec::Parent => frame
                .scope
                .as_ref()
                .and_then(|s| s.parent.as_ref())
                .map_or(0, |p| Rc::as_ptr(p) as usize),
            ClassSpec::Named(l) => frame
                .code
                .class_cache
                .borrow()
                .get(*l as usize)
                .map_or(0, |w| w.as_ptr() as usize),
            ClassSpec::Dyn(_) => 0,
        };
        if class_ptr == 0 {
            return;
        }
        let entry = StaticPropCacheEntry {
            scope_ptr,
            called_ptr,
            class_ptr,
            cell: cell.clone(),
            fast_plain,
        };
        let mut entries = frame.code.rt_cache.static_prop_entries.borrow_mut();
        let idx = pid as usize;
        if entries.len() <= idx {
            entries.resize_with(idx + 1, StaticPropCacheEntry::default);
        }
        entries[idx] = entry;
    }

    // ------------------------------------------------------------------
    // Static method calls (ZEND_INIT_STATIC_METHOD_CALL)
    // ------------------------------------------------------------------

    /// The pending call of `A::name()` from the cache, if the op resolved
    /// the same class before and the method is still callable the same
    /// way: a static method, or an instance method with a compatible
    /// `$this` (`parent::__construct()`). The parts are returned, not a
    /// built `PendingCall`, so that the caller builds the call in place.
    #[inline]
    pub(crate) fn static_call_hit(&self, spec: &ClassSpec, name: Operand) -> Option<StaticCallHit> {
        let Operand::Lit(l) = name else {
            return None;
        };
        let cache = self.frame().code.rt_cache.static_calls.borrow();
        let (cls, e) = self.rt_find(spec, cache.get(l as usize)?)?;
        let Target::Method(m) = &e.target else {
            return None;
        };
        let m = m.upgrade()?;
        let caller_frame = self
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
                caller_frame
                    .and_then(|f| f.called.clone())
                    .filter(|c| c.is_subclass_of(&cls))
                    .unwrap_or(cls)
            } else {
                cls
            };
            return Some(StaticCallHit {
                func: m.func.clone(),
                this: None,
                scope: m.scope(),
                called,
            });
        }
        let this = caller_frame.and_then(|f| f.this.clone())?;
        let called = object::class_of(&this).clone();
        if !called.is_subclass_of(&cls) {
            return None;
        }
        Some(StaticCallHit {
            func: m.func.clone(),
            this: Some(this),
            scope: m.scope(),
            called,
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
        let lc = n.to_ascii_lowercase();
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
        let slot = MemberSlot {
            class_lit,
            class: Rc::downgrade(cls),
            access,
            target: Target::Method(Rc::downgrade(m)),
        };
        put_member(&self.frame().code.rt_cache.static_calls, l as usize, slot);
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

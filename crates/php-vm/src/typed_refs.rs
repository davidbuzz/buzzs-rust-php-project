//! Typed references: references bound to typed properties carry the
//! properties as "type sources" (`ZEND_REF_HAS_TYPE_SOURCES`), and every
//! assignment through such a reference must satisfy all of them
//! (`zend_assign_to_typed_ref`, `zend_verify_ref_assignable_zval`,
//! `zend_incdec_typed_ref`, `zend_verify_ref_array_assignable`).
//!
//! The sources live in the reference's VM metadata slot
//! ([`PhpRef::meta`]). Instead of php-src's eager bookkeeping on every
//! unset / rebind / object destruction (`zend_ref_del_type_source`), a
//! source records *where* the property lives (an object slot or a static
//! property cell, both held weakly) and counts only while that storage
//! still holds this very reference; stale sources are pruned whenever the
//! list is consulted. Adding a source is the eager part
//! (`zend_ref_add_type_source`): whenever a typed property slot becomes, or
//! is bound to, a reference.

use crate::class::{Class, PropInfo, StaticProp};
use crate::object::{self, ObjBox, ObjectData};
use crate::typecheck::TypeCtx;
use crate::types::{mask, TypeDecl};
use crate::vm::{Throw, Vm, VmResult};
use php_runtime::{ArrayKey, Obj, PhpRef, Value, ZStr};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// Where a source property stores its value.
enum Holder {
    /// Declared property slot `.1` of an object.
    Slot(Weak<ObjectData>, usize),
    /// A static property cell.
    Static(Weak<RefCell<Value>>),
}

impl Holder {
    fn same(&self, other: &Holder) -> bool {
        match (self, other) {
            (Holder::Slot(a, i), Holder::Slot(b, j)) => i == j && Weak::ptr_eq(a, b),
            (Holder::Static(a), Holder::Static(b)) => Weak::ptr_eq(a, b),
            _ => false,
        }
    }
}

/// One type source (a `zend_property_info` of a
/// `zend_property_info_source_list`).
struct Source {
    holder: Holder,
    prop: Rc<PropInfo>,
    /// The declaring class (resolves `self`/`parent` in the type).
    decl: Option<Rc<Class>>,
}

impl Source {
    /// Whether the property storage still holds `r`.
    fn holds(&self, r: &PhpRef) -> bool {
        let is_r = |v: &Value| matches!(v, Value::Reference(x) if PhpRef::ptr_eq(x, r));
        match &self.holder {
            Holder::Slot(w, idx) => w.upgrade().is_some_and(|d| {
                // A slot borrowed by an ongoing write walk still holds it.
                d.slots.try_borrow().map_or(true, |s| {
                    s.get(*idx).and_then(Option::as_ref).is_some_and(is_r)
                })
            }),
            Holder::Static(w) => w
                .upgrade()
                .is_some_and(|c| c.try_borrow().map_or(true, |v| is_r(&v))),
        }
    }
}

/// The source list stored in [`PhpRef::meta`].
#[derive(Default)]
pub(crate) struct TypeSources {
    list: RefCell<Vec<Source>>,
}

/// A live type source, as seen by the checks.
#[derive(Clone)]
pub(crate) struct SourceProp {
    pub prop: Rc<PropInfo>,
    pub decl: Option<Rc<Class>>,
}

impl SourceProp {
    fn mask(&self) -> u32 {
        self.prop.ty.as_ref().map_or(mask::ANY, |t| t.mask)
    }

    /// `zend_type_to_string` of the property type.
    pub fn type_string(&self) -> String {
        self.prop.ty.as_ref().map_or_else(
            || "mixed".to_string(),
            |t| {
                t.display(
                    self.decl.as_ref().map(|c| &c.name),
                    self.decl.as_ref().and_then(|c| c.parent_name()),
                    None,
                )
            },
        )
    }

    /// `%s::$%s` of the property (the class name is printed in full,
    /// like `%pS` of `ce->name`).
    pub fn qualified_name(&self) -> String {
        format!(
            "{}::${}",
            self.prop.class.to_string_lossy(),
            self.prop.name.to_string_lossy()
        )
    }
}

fn sources_meta(r: &PhpRef) -> Option<Rc<dyn Any>> {
    r.meta().filter(|m| m.is::<TypeSources>())
}

/// The live type sources of `r` (pruning stale ones). Empty when the
/// reference is untyped.
pub(crate) fn type_sources(r: &PhpRef) -> Vec<SourceProp> {
    let Some(meta) = sources_meta(r) else {
        return Vec::new();
    };
    let Some(ts) = meta.downcast_ref::<TypeSources>() else {
        return Vec::new();
    };
    let live: Vec<SourceProp> = {
        let Ok(mut list) = ts.list.try_borrow_mut() else {
            return Vec::new();
        };
        list.retain(|s| s.holds(r));
        list.iter()
            .map(|s| SourceProp {
                prop: s.prop.clone(),
                decl: s.decl.clone(),
            })
            .collect()
    };
    if live.is_empty() {
        r.set_meta(None);
    }
    live
}

/// Whether assignments through `r` are type-checked
/// (`ZEND_REF_HAS_TYPE_SOURCES`).
pub(crate) fn has_type_sources(r: &PhpRef) -> bool {
    if r.meta().is_none() {
        return false;
    }
    !type_sources(r).is_empty()
}

/// Whether `v` is a reference with type sources.
pub(crate) fn is_typed_ref(v: &Value) -> bool {
    matches!(v, Value::Reference(r) if has_type_sources(r))
}

fn add_source(r: &PhpRef, holder: Holder, prop: Rc<PropInfo>, decl: Option<Rc<Class>>) {
    let meta = sources_meta(r).unwrap_or_else(|| {
        let m: Rc<dyn Any> = Rc::new(TypeSources::default());
        r.set_meta(Some(m.clone()));
        m
    });
    let Some(ts) = meta.downcast_ref::<TypeSources>() else {
        return;
    };
    let Ok(mut list) = ts.list.try_borrow_mut() else {
        return;
    };
    // A holder stores one reference: rebinding it replaces its source.
    list.retain(|s| !s.holder.same(&holder));
    list.push(Source { holder, prop, decl });
}

/// Whether a property type makes a reference typed (`mixed` accepts all).
fn is_checked_type(pi: &PropInfo) -> bool {
    pi.ty.as_ref().is_some_and(|t| !t.is_mixed())
}

/// The `Rc<ObjectData>` behind a VM object.
fn object_rc(o: &Obj) -> Option<&Rc<ObjectData>> {
    o.downcast_ref::<ObjBox>().map(|b| &b.0)
}

/// The reference held by declared slot `idx` of `o`, if any.
fn slot_ref(o: &Obj, idx: usize) -> Option<PhpRef> {
    let d = object_rc(o)?;
    let slots = d.slots.try_borrow().ok()?;
    match slots.get(idx)? {
        Some(Value::Reference(r)) => Some(r.clone()),
        _ => None,
    }
}

/// The typed reference held by declared slot `idx` of `o`, if any.
pub(crate) fn slot_typed_ref(o: &Obj, idx: usize) -> Option<PhpRef> {
    slot_ref(o, idx).filter(has_type_sources)
}

/// The typed reference held by dynamic property `key` of `o`, if any.
pub(crate) fn dynamic_typed_ref(o: &Obj, key: &ArrayKey) -> Option<PhpRef> {
    let d = object_rc(o)?;
    let dynp = d.dynamic.try_borrow().ok()?;
    match dynp.as_ref()?.get(key)? {
        Value::Reference(r) if has_type_sources(r) => Some(r.clone()),
        _ => None,
    }
}

/// The typed reference held by a static property cell, if any.
pub(crate) fn cell_typed_ref(cell: &RefCell<Value>) -> Option<PhpRef> {
    match &*cell.try_borrow().ok()? {
        Value::Reference(r) if has_type_sources(r) => Some(r.clone()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Inc/dec context (`zend_incdec_typed_ref` / `zend_incdec_typed_prop`)
// ---------------------------------------------------------------------------

thread_local! {
    /// Set while the result of an integer `++`/`--` that overflowed to
    /// float is written back: `Some(true)` for increments.
    static INCDEC_OVERFLOW: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Marks the write-back of an overflowing `++`/`--` (restored on drop).
pub(crate) struct IncDecGuard(Option<bool>);

impl IncDecGuard {
    /// Starts the context when `old` → `new` is an int overflowing to a
    /// float.
    pub(crate) fn new(is_inc: bool, old: &Value, new: &Value) -> Self {
        let old_is_int = match old {
            Value::Reference(r) => matches!(r.try_borrow().as_deref(), Some(Value::Int(_))),
            v => matches!(v, Value::Int(_)),
        };
        let prev = INCDEC_OVERFLOW.with(Cell::get);
        INCDEC_OVERFLOW.with(|c| {
            c.set((old_is_int && matches!(new, Value::Float(_))).then_some(is_inc));
        });
        IncDecGuard(prev)
    }
}

impl Drop for IncDecGuard {
    fn drop(&mut self) {
        let prev = self.0;
        INCDEC_OVERFLOW.with(|c| c.set(prev));
    }
}

// ---------------------------------------------------------------------------
// By-reference argument writes of internal functions
// ---------------------------------------------------------------------------

thread_local! {
    /// Writes of internal functions to typed by-reference arguments
    /// ([`crate::func::Args::set_ref`]), checked once the function returns.
    static DEFERRED_ARG_WRITES: RefCell<Vec<(PhpRef, Value)>> = const { RefCell::new(Vec::new()) };
}

/// Queues a write to a typed by-reference argument (the VM is not at hand
/// in [`crate::func::Args::set_ref`]); see [`Vm::flush_typed_arg_writes`].
pub(crate) fn defer_typed_arg_write(r: PhpRef, v: Value) {
    DEFERRED_ARG_WRITES.with(|d| d.borrow_mut().push((r, v)));
}

fn incdec_overflow() -> Option<bool> {
    INCDEC_OVERFLOW.with(Cell::get)
}

/// Consumes the inc/dec context (it applies to one write only).
fn take_incdec_overflow() -> Option<bool> {
    INCDEC_OVERFLOW.with(Cell::take)
}

// ---------------------------------------------------------------------------
// Pure checks and messages
// ---------------------------------------------------------------------------

/// `zend_zval_type_name`: `bool` for booleans, the class name for objects.
fn zval_type_name(v: &Value) -> String {
    match v {
        Value::Reference(r) => zval_type_name(&r.get()),
        Value::Object(o) => o.class_name().to_string_lossy(),
        other => other.simple_type_name().to_string(),
    }
}

/// `zend_zval_value_name`.
fn zval_value_name(v: &Value) -> String {
    match v {
        Value::Reference(r) => zval_value_name(&r.get()),
        Value::Object(o) => o.class_name().to_string_lossy(),
        other => other.value_name(),
    }
}

/// `zend_verify_ref_array_assignable`: the message of the error raised
/// when a `null`/`false` held by `r` would be auto-vivified into an array,
/// `None` when that is allowed (or `r` is untyped).
pub(crate) fn auto_init_error(r: &PhpRef) -> Option<String> {
    if r.meta().is_none() {
        return None;
    }
    type_sources(r)
        .into_iter()
        .find(|s| s.mask() & mask::ARRAY == 0)
        .map(|s| {
            format!(
                "Cannot auto-initialize an array inside a reference held by property {} of type {}",
                s.qualified_name(),
                s.type_string()
            )
        })
}

/// For a write walk about to auto-vivify the value held by reference `r`
/// into an array: the error message when a type source forbids it.
pub(crate) fn walk_auto_init_error(r: &PhpRef) -> Option<String> {
    if r.meta().is_none() {
        return None;
    }
    match r.try_borrow().as_deref() {
        Some(Value::Null | Value::Bool(false)) => {}
        _ => return None,
    }
    auto_init_error(r)
}

/// `zend_throw_incdec_ref_error` / `zend_throw_incdec_prop_error`.
fn incdec_error_message(src: &SourceProp, is_inc: bool, via_ref: bool) -> String {
    let what = if via_ref {
        "a reference held by property"
    } else {
        "property"
    };
    let (verb, bound) = if is_inc {
        ("increment", "maximal")
    } else {
        ("decrement", "minimal")
    };
    format!(
        "Cannot {verb} {what} {} of type {} past its {bound} value",
        src.qualified_name(),
        src.type_string()
    )
}

/// Writes `v` into `r` unless `r` is borrowed (a self-referencing write
/// still walking it); returns the stored value.
fn set_ref_value(r: &PhpRef, v: Value) -> Value {
    let v = match v {
        Value::Reference(x) if PhpRef::ptr_eq(&x, r) => return r.get(),
        Value::Reference(x) => x.get(),
        v => v,
    };
    if let Some(mut b) = r.try_borrow_mut() {
        let old = std::mem::replace(&mut *b, v.clone());
        drop(b);
        drop(old);
    }
    v
}

/// A `PropInfo` view of a static property (for messages and checks).
fn static_prop_info(sp: &StaticProp, ty: TypeDecl) -> PropInfo {
    PropInfo {
        name: sp.name.clone(),
        slot: 0,
        flags: sp.flags,
        ty: Some(ty),
        class: sp.class.clone(),
        default: None,
        doc_comment: None,
        attributes: Vec::new(),
        hooks: None,
    }
}

/// Outcome of `i_zend_verify_type_assignable_zval`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Assignable {
    Yes,
    No,
    /// May be valid after a coercion.
    Coerce,
}

/// The coercion-free part of `i_zend_verify_type_assignable_zval` for a
/// value the type does not accept as is.
fn coercion_possible(m: u32, v: &Value, strict: bool) -> Assignable {
    if strict {
        return if m & mask::DOUBLE != 0 && matches!(v, Value::Int(_)) {
            Assignable::Coerce
        } else {
            Assignable::No
        };
    }
    if matches!(v, Value::Null) {
        return Assignable::No;
    }
    if m & (mask::LONG | mask::DOUBLE | mask::STRING) == 0 && m & mask::BOOL != mask::BOOL {
        return Assignable::No;
    }
    Assignable::Coerce
}

impl Vm {
    /// Strictness of the code performing the current assignment
    /// (`EX_USES_STRICT_TYPES` of the innermost user frame).
    pub(crate) fn uses_strict_types(&self) -> bool {
        self.frames
            .iter()
            .rev()
            .find(|f| f.kind != crate::vm::FrameKind::Builtin)
            .is_some_and(|f| f.code.strict_types)
    }

    /// `i_zend_verify_type_assignable_zval`.
    fn type_assignable(&mut self, src: &SourceProp, v: &Value, strict: bool) -> Assignable {
        let Some(ty) = src.prop.ty.as_ref() else {
            return Assignable::Yes;
        };
        let ctx = TypeCtx {
            scope: src.decl.as_ref(),
            called: None,
            strict,
            internal_arg: None,
        };
        if ty.is_mixed() || self.type_accepts(ty, v, &ctx) {
            return Assignable::Yes;
        }
        coercion_possible(ty.mask, v, strict)
    }

    /// `zend_verify_weak_scalar_type_hint` on a copy of `v`.
    fn weak_coerced(&mut self, src: &SourceProp, v: &Value) -> VmResult<Option<Value>> {
        let mut tmp = v.clone();
        Ok(self.weak_scalar_coerce(src.mask(), &mut tmp)?.then_some(tmp))
    }

    fn ref_type_error(&mut self, src: &SourceProp, v: &Value) -> Throw {
        let msg = format!(
            "Cannot assign {} to reference held by property {} of type {}",
            zval_value_name(v),
            src.qualified_name(),
            src.type_string()
        );
        self.make_throw("TypeError", &msg)
    }

    fn conflicting_coercion_error(&mut self, a: &SourceProp, b: &SourceProp, v: &Value) -> Throw {
        let msg = format!(
            "Cannot assign {} to reference held by property {} of type {} and property {} of type {}, as this would result in an inconsistent type conversion",
            zval_value_name(v),
            a.qualified_name(),
            a.type_string(),
            b.qualified_name(),
            b.type_string()
        );
        self.make_throw("TypeError", &msg)
    }

    /// `zend_verify_ref_assignable_zval`: checks `v` against every source
    /// of a typed reference, coercing it (consistently for all sources) in
    /// weak mode. Throws the `TypeError` on failure.
    pub(crate) fn verify_ref_assignable(
        &mut self,
        sources: &[SourceProp],
        v: &mut Value,
        strict: bool,
    ) -> VmResult<()> {
        if let Value::Reference(r) = v {
            *v = r.get();
        }
        let mut first: Option<&SourceProp> = None;
        let mut coerced: Option<Value> = None;
        for src in sources {
            match self.type_assignable(src, v, strict) {
                Assignable::No => return Err(self.ref_type_error(src, v)),
                Assignable::Coerce => match (first, &coerced) {
                    (None, _) => {
                        first = Some(src);
                        match self.weak_coerced(src, v)? {
                            Some(c) => coerced = Some(c),
                            None => return Err(self.ref_type_error(src, v)),
                        }
                    }
                    (Some(f), None) => return Err(self.conflicting_coercion_error(f, src, v)),
                    (Some(f), Some(c)) => {
                        let c = c.clone();
                        let Some(tmp) = self.weak_coerced(src, v)? else {
                            return Err(self.ref_type_error(src, v));
                        };
                        if !php_runtime::ops::is_identical(&c, &tmp).unwrap_or(false) {
                            return Err(self.conflicting_coercion_error(f, src, v));
                        }
                    }
                },
                Assignable::Yes => match (first, &coerced) {
                    (None, _) => first = Some(src),
                    (Some(f), Some(_)) => return Err(self.conflicting_coercion_error(f, src, v)),
                    (Some(_), None) => {}
                },
            }
        }
        if let Some(c) = coerced {
            *v = c;
        }
        Ok(())
    }

    /// Assigns `v` through reference `r` (`zend_assign_to_variable` on a
    /// reference): typed references check and coerce the value against all
    /// of their sources first (`zend_assign_to_typed_ref`). Returns the
    /// value stored.
    pub(crate) fn assign_ref(&mut self, r: &PhpRef, v: Value) -> VmResult<Value> {
        self.assign_ref_ex(r, v, None)
    }

    /// [`Vm::assign_ref`] with an explicit strictness (`None`: that of
    /// the calling code).
    fn assign_ref_ex(&mut self, r: &PhpRef, v: Value, strict: Option<bool>) -> VmResult<Value> {
        let incdec = take_incdec_overflow();
        if r.meta().is_none() {
            return Ok(set_ref_value(r, v));
        }
        let sources = type_sources(r);
        if sources.is_empty() {
            return Ok(set_ref_value(r, v));
        }
        let mut v = match v {
            Value::Reference(x) => x.get(),
            v => v,
        };
        if let (Some(is_inc), Value::Float(_)) = (incdec, &v) {
            if let Some(src) = sources.iter().find(|s| s.mask() & mask::DOUBLE == 0) {
                let msg = incdec_error_message(src, is_inc, true);
                return Err(self.make_throw("TypeError", &msg));
            }
        }
        let strict = strict.unwrap_or_else(|| self.uses_strict_types());
        self.verify_ref_assignable(&sources, &mut v, strict)?;
        Ok(set_ref_value(r, v))
    }

    /// `zend_assign_to_variable(..., strict = 1)`: like [`Vm::assign_ref`]
    /// with strict typing whatever the calling code declares (`catch`
    /// variables).
    pub(crate) fn assign_ref_strict(&mut self, r: &PhpRef, v: Value) -> VmResult<Value> {
        self.assign_ref_ex(r, v, Some(true))
    }

    /// Performs the writes internal functions made to typed by-reference
    /// arguments (`zend_try_assign_typed_ref`) once the function returned
    /// `result`; the first failing write's `TypeError` replaces a
    /// successful result.
    pub(crate) fn flush_typed_arg_writes(&mut self, result: VmResult<Value>) -> VmResult<Value> {
        let pending = DEFERRED_ARG_WRITES.with(|d| {
            if d.borrow().is_empty() {
                Vec::new()
            } else {
                std::mem::take(&mut *d.borrow_mut())
            }
        });
        let mut result = result;
        for (r, v) in pending {
            if let Err(e) = self.assign_ref(&r, v) {
                if result.is_ok() {
                    result = Err(e);
                }
            }
        }
        result
    }

    /// Whether a by-reference parameter check may coerce the value of `r`
    /// (`zend_check_type_slow`: "We cannot have conversions for typed
    /// refs").
    pub(crate) fn ref_allows_param_coercion(r: &PhpRef) -> bool {
        !has_type_sources(r)
    }

    /// `ZEND_REF_ADD_TYPE_SOURCE` for declared slot `idx` of `o` once it
    /// holds reference `r` (taking `&$o->p`, cloning, foreach by
    /// reference, ...).
    pub(crate) fn add_slot_type_source(&mut self, o: &Obj, idx: usize, r: &PhpRef) {
        let Some(pi) = object::class_of(o).props.get(idx).cloned() else {
            return;
        };
        if !is_checked_type(&pi) {
            return;
        }
        let Some(d) = object_rc(o) else {
            return;
        };
        let holder = Holder::Slot(Rc::downgrade(d), idx);
        let decl = self.find_class(pi.class.as_bytes());
        add_source(r, holder, pi, decl);
    }

    /// Adds the sources of every declared slot of `o` holding a reference
    /// (after `clone`, like `zend_objects_clone_members`).
    pub(crate) fn add_object_type_sources(&mut self, o: &Obj) {
        let n = object_rc(o).map_or(0, |d| d.slots.try_borrow().map_or(0, |s| s.len()));
        for idx in 0..n {
            if let Some(r) = slot_ref(o, idx) {
                self.add_slot_type_source(o, idx, &r);
            }
        }
    }

    /// `ZEND_REF_ADD_TYPE_SOURCE` for a static property cell holding `r`.
    pub(crate) fn add_static_type_source(&mut self, sp: &StaticProp, r: &PhpRef) {
        let Some(ty) = sp.ty.as_ref().filter(|t| !t.is_mixed()) else {
            return;
        };
        let pi = Rc::new(static_prop_info(sp, ty.clone()));
        let decl = self.find_class(sp.class.as_bytes());
        add_source(r, Holder::Static(Rc::downgrade(&sp.cell)), pi, decl);
    }

    /// `zend_verify_prop_assignable_by_ref` for `$o->p = &$x` (or a static
    /// property): checks the referenced value against property `pi`
    /// (coercing it in place when the reference is not typed yet). Throws
    /// the `TypeError` on failure.
    pub(crate) fn verify_prop_assignable_by_ref(
        &mut self,
        pi: &Rc<PropInfo>,
        r: &PhpRef,
    ) -> VmResult<()> {
        let mut v = Value::Reference(r.clone());
        self.verify_prop_assignable_by_ref_ex(pi, &mut v, false)
    }

    /// The value returned by `__get()` for an unset typed property
    /// (`zend_std_read_property`, `ZEND_VERIFY_PROP_ASSIGNABLE_BY_REF_CONTEXT_MAGIC_GET`):
    /// must be compatible with the property type (and is coerced to it).
    pub(crate) fn verify_magic_get_result(
        &mut self,
        pi: &Rc<PropInfo>,
        v: &mut Value,
    ) -> VmResult<()> {
        self.verify_prop_assignable_by_ref_ex(pi, v, true)
    }

    /// `zend_verify_prop_assignable_by_ref_ex`: `v` is a reference or a
    /// plain value (coerced in place, through the reference when not
    /// typed).
    fn verify_prop_assignable_by_ref_ex(
        &mut self,
        pi: &Rc<PropInfo>,
        v: &mut Value,
        magic_get: bool,
    ) -> VmResult<()> {
        if !is_checked_type(pi) {
            return Ok(());
        }
        let decl = self.find_class(pi.class.as_bytes());
        let target = SourceProp {
            prop: pi.clone(),
            decl,
        };
        let strict = self.uses_strict_types();
        let r = match v {
            Value::Reference(r) => Some(r.clone()),
            _ => None,
        };
        let val = match &r {
            Some(r) => r.get(),
            None => v.clone(),
        };
        let sources = r.as_ref().map(type_sources).unwrap_or_default();
        if let Some(first) = sources.first() {
            match self.type_assignable(&target, &val, strict) {
                Assignable::Yes => return Ok(()),
                Assignable::Coerce => {
                    if self.weak_coerced(&target, &val)?.is_some() {
                        let msg = format!(
                            "Reference with value of type {} held by property {} of type {} is not compatible with property {} of type {}",
                            zval_type_name(&val),
                            first.qualified_name(),
                            first.type_string(),
                            target.qualified_name(),
                            target.type_string()
                        );
                        return Err(self.make_throw("TypeError", &msg));
                    }
                }
                Assignable::No => {}
            }
        } else if let Some(ty) = target.prop.ty.clone() {
            let mut c = val.clone();
            let ctx = TypeCtx {
                scope: target.decl.as_ref(),
                called: None,
                strict,
                internal_arg: None,
            };
            if self.verify_type(&ty, &mut c, &ctx)? {
                if !php_runtime::ops::is_identical(&c, &val).unwrap_or(false) {
                    match &r {
                        Some(r) => {
                            set_ref_value(r, c);
                        }
                        None => *v = c,
                    }
                }
                return Ok(());
            }
        }
        let msg = if magic_get {
            format!(
                "Value of type {} returned from {}::__get() must be compatible with unset property {} of type {}",
                zval_type_name(&val),
                pi.class.to_string_lossy(),
                target.qualified_name(),
                target.type_string()
            )
        } else {
            format!(
                "Cannot assign {} to property {} of type {}",
                zval_value_name(&val),
                target.qualified_name(),
                target.type_string()
            )
        };
        Err(self.make_throw("TypeError", &msg))
    }

    /// `$o->p = &$x` for declared slot `idx` of `o`: verifies the value and
    /// records the source. Call before storing `r` in the slot.
    pub(crate) fn bind_slot_ref(&mut self, o: &Obj, idx: usize, r: &Value) -> VmResult<()> {
        let Value::Reference(r) = r else {
            return Ok(());
        };
        let Some(pi) = object::class_of(o).props.get(idx).cloned() else {
            return Ok(());
        };
        if !is_checked_type(&pi) {
            return Ok(());
        }
        self.verify_prop_assignable_by_ref(&pi, r)?;
        self.add_slot_type_source(o, idx, r);
        Ok(())
    }

    /// `Cls::$p = &$x` for a typed static property: verifies the value and
    /// records the source. Call before storing `r` in the cell.
    pub(crate) fn bind_static_ref(&mut self, sp: &StaticProp, r: &Value) -> VmResult<()> {
        let Value::Reference(r) = r else {
            return Ok(());
        };
        let Some(ty) = sp.ty.as_ref().filter(|t| !t.is_mixed()) else {
            return Ok(());
        };
        let pi = Rc::new(static_prop_info(sp, ty.clone()));
        self.verify_prop_assignable_by_ref(&pi, r)?;
        self.add_static_type_source(sp, r);
        Ok(())
    }

    /// The slot of an uninitialized typed property `name` of `o` that a
    /// reference operation handles itself (`zend_std_get_property_ptr_ptr`
    /// returns the `IS_UNDEF` slot for writes); `None` leaves the case to
    /// the regular property path (untyped, hooked, readonly or
    /// asymmetric-visibility properties, inaccessible ones, properties
    /// `unset()` since, lazy objects).
    fn uninit_typed_slot(&mut self, o: &Obj, name: &ZStr) -> VmResult<Option<Rc<PropInfo>>> {
        let cls = object::class_of(o).clone();
        let scope = self.scope();
        let pi = match self.lookup_prop(&cls, name, scope.as_ref()) {
            crate::props::PropLookup::Found(pi) => pi,
            _ => return Ok(None),
        };
        if pi.hooks.is_some()
            || pi.flags & (crate::func::flags::READONLY | crate::func::flags::SET_MASK) != 0
            || !is_checked_type(&pi)
        {
            return Ok(None);
        }
        let idx = pi.slot as usize;
        let Some(d) = object_rc(o) else {
            return Ok(None);
        };
        let uninit = d
            .slots
            .try_borrow()
            .is_ok_and(|s| matches!(s.get(idx), Some(None)));
        // After unset(), `__get()` takes over.
        let reinit = d.readonly_init.borrow().get(idx).copied().unwrap_or(false)
            && cls.magic.get.is_some();
        if !uninit || reinit || self.lazy_object_for(o, Some(name))?.ne(o) {
            return Ok(None);
        }
        Ok(Some(pi))
    }

    /// `$r = &$o->p` on an uninitialized typed property
    /// (`zend_handle_fetch_obj_flags`, `ZEND_FETCH_REF`): nullable types
    /// start out as `null`, others throw. `None` when the regular path
    /// applies.
    pub(crate) fn uninit_typed_prop_ref(
        &mut self,
        o: &Obj,
        name: &ZStr,
    ) -> VmResult<Option<PhpRef>> {
        let Some(pi) = self.uninit_typed_slot(o, name)? else {
            return Ok(None);
        };
        if !pi.ty.as_ref().is_some_and(TypeDecl::allows_null) {
            let msg = format!(
                "Cannot access uninitialized non-nullable property {}::${} by reference",
                pi.class.to_string_lossy(),
                pi.name.to_string_lossy()
            );
            return Err(self.make_throw("Error", &msg));
        }
        let idx = pi.slot as usize;
        let r = {
            let d = object::data(o);
            let Ok(mut slots) = d.slots.try_borrow_mut() else {
                return Ok(None);
            };
            let Some(slot) = slots.get_mut(idx) else {
                return Ok(None);
            };
            slot.get_or_insert(Value::Null).make_ref()
        };
        self.add_slot_type_source(o, idx, &r);
        Ok(Some(r))
    }

    /// `$o->p = &$x` on an uninitialized typed property
    /// (`zend_assign_to_property_reference`): verifies, binds and records
    /// the source. `None` when the regular path applies.
    pub(crate) fn assign_uninit_typed_prop_ref(
        &mut self,
        o: &Obj,
        name: &ZStr,
        r: &Value,
    ) -> VmResult<Option<Value>> {
        if !matches!(r, Value::Reference(_)) {
            return Ok(None);
        }
        let Some(pi) = self.uninit_typed_slot(o, name)? else {
            return Ok(None);
        };
        let idx = pi.slot as usize;
        self.bind_slot_ref(o, idx, r)?;
        let old = {
            let d = object::data(o);
            let Ok(mut slots) = d.slots.try_borrow_mut() else {
                return Ok(None);
            };
            let Some(slot) = slots.get_mut(idx) else {
                return Ok(None);
            };
            slot.replace(r.clone())
        };
        drop(old);
        Ok(Some(r.clone()))
    }

    /// The property-type part of `$o->p = v` for a declared property
    /// (`zend_std_write_property`): checks and coerces `v` against the
    /// property type and returns the typed reference the slot holds, if
    /// any (the caller then assigns through it with [`Vm::assign_ref`]).
    /// The write-back of an overflowing `++`/`--` through a typed
    /// reference skips the property check: the reference reports it.
    pub(crate) fn typed_prop_write(
        &mut self,
        o: &Obj,
        pi: &PropInfo,
        v: Value,
    ) -> VmResult<(Option<PhpRef>, Value)> {
        let typed_ref = slot_typed_ref(o, pi.slot as usize);
        if typed_ref.is_some() && incdec_overflow().is_some() {
            return Ok((typed_ref, v));
        }
        self.incdec_prop_overflow(pi, &v)?;
        let v = self.coerce_prop(Some(o), pi, v)?;
        Ok((typed_ref, v))
    }

    /// Typed-property `++`/`--` overflow (`zend_incdec_typed_prop`): when
    /// an integer overflowing to a float is written back to a property
    /// whose type does not accept floats, throws the inc/dec error.
    pub(crate) fn incdec_prop_overflow(&mut self, pi: &PropInfo, v: &Value) -> VmResult<()> {
        let (Some(is_inc), Value::Float(_), Some(ty)) = (incdec_overflow(), v, pi.ty.as_ref())
        else {
            return Ok(());
        };
        if ty.mask & mask::DOUBLE != 0 || ty.is_mixed() {
            return Ok(());
        }
        take_incdec_overflow();
        let decl = self.find_class(pi.class.as_bytes());
        let src = SourceProp {
            prop: Rc::new(pi.clone()),
            decl,
        };
        let msg = incdec_error_message(&src, is_inc, false);
        Err(self.make_throw("TypeError", &msg))
    }
}

#[cfg(test)]
mod tests;

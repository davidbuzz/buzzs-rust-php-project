// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
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
use crate::object::{self, ObjectData};
use crate::typecheck::TypeCtx;
use crate::types::{mask, TypeDecl};
use crate::vm::{Throw, Vm, VmResult};
use php_runtime::{ArrayKey, Obj, ObjWeak, PhpRef, Value, ZStr};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// Where a source property stores its value.
enum Holder {
    /// Declared property slot `.1` of an object.
    Slot(ObjWeak, usize),
    /// A static property cell.
    Static(Weak<RefCell<Value>>),
}

impl Holder {
    fn same(&self, other: &Holder) -> bool {
        match (self, other) {
            (Holder::Slot(a, i), Holder::Slot(b, j)) => i == j && a.addr() == b.addr(),
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
            Holder::Slot(w, idx) => w.upgrade().is_some_and(|o| {
                // A slot borrowed by an ongoing write walk still holds it.
                object::data(&o).slots.try_borrow().map_or(true, |s| {
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

    /// `zend_type_to_string` of the property type (`self`/`parent` are
    /// not resolved in these messages).
    pub fn type_string(&self) -> String {
        self.prop
            .ty
            .as_ref()
            .map_or_else(|| "mixed".to_string(), |t| t.display(None, None, None))
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
    // A holder stores one reference: it is a source once (in the order the
    // sources were added, which error messages reflect).
    list.retain(|s| s.holds(r));
    if list.iter().any(|s| s.holder.same(&holder)) {
        return;
    }
    list.push(Source { holder, prop, decl });
}

/// Whether a property type makes a reference typed (`mixed` accepts all).
fn is_checked_type(pi: &PropInfo) -> bool {
    pi.ty.as_ref().is_some_and(|t| !t.is_mixed())
}

/// The data of a VM object (`None` for other `PhpObject` implementations).
fn object_rc(o: &Obj) -> Option<&ObjectData> {
    o.downcast_ref::<ObjectData>()
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
    r.meta()?;
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
    r.meta()?;
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

/// The value the UNDEF result of a failed binary operation takes when it
/// is checked in weak mode against a property type of mask `m`
/// (`zend_verify_weak_scalar_type_hint()`, Zend/zend_execute.c): int 0
/// when the type takes an int (`zend_parse_arg_long_weak()` reads a type
/// below `IS_TRUE` other than null as 0), else float 0.0 when it takes a
/// float (`zend_parse_arg_double_weak()`); a conversion to string or bool
/// fails on the exception the operation left pending, and a type without
/// a scalar has none: `None`, the property keeps its value.
pub(crate) fn failed_op_coercion(m: u32) -> Option<Value> {
    if m & mask::LONG != 0 {
        Some(Value::Int(0))
    } else if m & mask::DOUBLE != 0 {
        Some(Value::Float(0.0))
    } else {
        None
    }
}

/// `zend_exception_set_previous()` of a new exception `t` onto `prev`.
fn chain_onto(t: Throw, prev: &Obj) -> Throw {
    if let Throw::Exception(o) = &t {
        crate::exceptions::chain_previous(o, prev);
    }
    t
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
        Ok(self
            .weak_scalar_coerce(src.mask(), &mut tmp)?
            .then_some(tmp))
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

    /// Assigns through a reference with explicit strictness (`None`: that
    /// of the calling code), using the typed-reference assignment checks.
    pub fn assign_ref_ex(&mut self, r: &PhpRef, v: Value, strict: Option<bool>) -> VmResult<Value> {
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

    /// `zend_binary_assign_op_typed_ref()` (Zend/zend_execute.c) when the
    /// operation of `$ref op= v` threw `err`: the operation's temporary
    /// result is UNDEF, and `zend_verify_ref_assignable_zval()` checks that
    /// against every property the reference belongs to (see
    /// [`failed_op_coercion`]). When each converts it alike, the reference
    /// takes the converted value; otherwise the reference's `TypeError`
    /// ("Cannot assign null to reference held by ...", UNDEF named as
    /// null, or the inconsistent-conversion error) is thrown with `err`
    /// as its previous exception (`zend_throw_ref_type_error_zval()` does
    /// not check for a pending exception). An untyped reference stays as
    /// it was (`zend_binary_op(var_ptr, var_ptr, value)` leaves `op1`).
    #[cold]
    #[inline(never)]
    pub(crate) fn failed_assign_op_ref(&mut self, r: &PhpRef, err: Throw) -> Throw {
        let Throw::Exception(prev) = &err else {
            return err;
        };
        let sources = type_sources(r);
        if sources.is_empty() {
            return err;
        }
        let strict = self.uses_strict_types();
        let mut first: Option<(&SourceProp, Value)> = None;
        for src in &sources {
            let m = src.mask();
            // `i_zend_verify_type_assignable_zval()`: strict mode takes no
            // UNDEF; weak mode tries a conversion when the type has a
            // scalar to convert to.
            let coercible = !strict
                && (m & (mask::LONG | mask::DOUBLE | mask::STRING) != 0
                    || m & mask::BOOL == mask::BOOL);
            let Some(c) = failed_op_coercion(m).filter(|_| coercible) else {
                let t = self.ref_type_error(src, &Value::Null);
                return chain_onto(t, prev);
            };
            if let Some((f, fc)) = &first {
                if !php_runtime::ops::is_identical(fc, &c).unwrap_or(false) {
                    let f: &SourceProp = f;
                    let t = self.conflicting_coercion_error(f, src, &Value::Null);
                    return chain_onto(t, prev);
                }
                continue;
            }
            first = Some((src, c));
        }
        if let Some((_, c)) = first {
            drop(set_ref_value(r, c));
        }
        err
    }

    /// `zend_binary_assign_op_typed_prop()` (Zend/zend_execute.c) when the
    /// operation of `$obj->name op= v` threw `err`: a typed declared
    /// property takes the weak-mode conversion of the UNDEF result (see
    /// [`failed_op_coercion`]); `zend_verify_property_type_error()` adds
    /// nothing while `err` is pending. A property holding a typed reference
    /// follows [`Vm::failed_assign_op_ref`]; an untyped, dynamic or hooked
    /// property (`zend_binary_op(zptr, zptr, value)`, or the overloaded
    /// path, which writes only on success) stays as it was. Returns the
    /// exception to throw.
    #[cold]
    #[inline(never)]
    pub(crate) fn failed_assign_op_prop(&mut self, obj: &Obj, name: &ZStr, err: Throw) -> Throw {
        if !matches!(err, Throw::Exception(_)) {
            return err;
        }
        let cls = object::class_of(obj).clone();
        let scope = self.scope();
        let pi = match self.lookup_prop(&cls, name, scope.as_ref()) {
            crate::props::PropLookup::Found(pi) => pi,
            // A dynamic property may hold a typed reference.
            crate::props::PropLookup::NotFound => {
                let held = object::data(obj)
                    .dynamic
                    .borrow()
                    .as_ref()
                    .and_then(|t| t.get(&ArrayKey::Str(name.clone())).cloned());
                return match held {
                    Some(Value::Reference(r)) => self.failed_assign_op_ref(&r, err),
                    _ => err,
                };
            }
            crate::props::PropLookup::Inaccessible(_) => return err,
        };
        if pi.hooks.is_some() || pi.is_virtual() {
            return err;
        }
        let slot = pi.slot as usize;
        let held = object::data(obj)
            .slots
            .borrow()
            .get(slot)
            .cloned()
            .flatten();
        match held {
            Some(Value::Reference(r)) => self.failed_assign_op_ref(&r, err),
            Some(_) if !self.uses_strict_types() => {
                if let Some(v) = pi.ty.as_ref().and_then(|t| failed_op_coercion(t.mask)) {
                    let old = object::data(obj)
                        .slots
                        .borrow_mut()
                        .get_mut(slot)
                        .and_then(|s| s.replace(v));
                    drop(old);
                }
                err
            }
            _ => err,
        }
    }

    /// `zend_binary_assign_op_typed_prop()` for a static property
    /// (`ZEND_ASSIGN_STATIC_PROP_OP`) whose operation threw `err`: as
    /// [`Vm::failed_assign_op_prop`], for the property's cell.
    #[cold]
    #[inline(never)]
    pub(crate) fn failed_assign_op_static(
        &mut self,
        cell: &Rc<RefCell<Value>>,
        err: Throw,
    ) -> Throw {
        if !matches!(err, Throw::Exception(_)) {
            return err;
        }
        if let Some(r) = cell_typed_ref(cell) {
            return self.failed_assign_op_ref(&r, err);
        }
        let Some(info) = static_info(cell) else {
            return err;
        };
        if self.uses_strict_types() {
            return err;
        }
        if let Some(v) = info
            .prop
            .ty
            .as_ref()
            .and_then(|t| failed_op_coercion(t.mask))
        {
            if let Ok(mut c) = cell.try_borrow_mut() {
                if !matches!(&*c, Value::Reference(_)) {
                    let old = std::mem::replace(&mut *c, v);
                    drop(c);
                    drop(old);
                }
            }
        }
        err
    }

    /// Performs the writes internal functions made to typed by-reference
    /// arguments (`zend_try_assign_typed_ref`) once the function returned
    /// `result`; the first failing write's `TypeError` replaces a
    /// successful result.
    pub(crate) fn flush_typed_arg_writes(&mut self, result: VmResult<Value>) -> VmResult<Value> {
        // Every builtin call passes through here: the common empty case
        // returns without building and dropping an empty iterator.
        let pending = DEFERRED_ARG_WRITES.with(|d| {
            if d.borrow().is_empty() {
                None
            } else {
                Some(std::mem::take(&mut *d.borrow_mut()))
            }
        });
        let Some(pending) = pending else {
            return result;
        };
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
        if object_rc(o).is_none() {
            return;
        }
        let holder = Holder::Slot(o.downgrade(), idx);
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
        self.verify_prop_assignable_by_ref_ex(pi, &mut v, false, None)
    }

    /// `zend_verify_prop_assignable_by_ref(info, data, /* strict */ 1)` of
    /// `unserialize()`: `v` (a reference or a plain value) checked against
    /// property `pi` in strict mode; a reference already typed by another
    /// property must satisfy it without coercion. Throws the `TypeError`.
    pub(crate) fn verify_prop_assignable_by_ref_strict(
        &mut self,
        pi: &Rc<PropInfo>,
        v: &mut Value,
    ) -> VmResult<()> {
        self.verify_prop_assignable_by_ref_ex(pi, v, false, Some(true))
    }

    /// The value returned by `__get()` for an unset typed property
    /// (`zend_std_read_property`, `ZEND_VERIFY_PROP_ASSIGNABLE_BY_REF_CONTEXT_MAGIC_GET`):
    /// must be compatible with the property type (and is coerced to it).
    pub(crate) fn verify_magic_get_result(
        &mut self,
        pi: &Rc<PropInfo>,
        v: &mut Value,
    ) -> VmResult<()> {
        self.verify_prop_assignable_by_ref_ex(pi, v, true, None)
    }

    /// `zend_verify_prop_assignable_by_ref_ex`: `v` is a reference or a
    /// plain value (coerced in place, through the reference when not
    /// typed). `strict` defaults to the calling code's `strict_types`.
    fn verify_prop_assignable_by_ref_ex(
        &mut self,
        pi: &Rc<PropInfo>,
        v: &mut Value,
        magic_get: bool,
        strict: Option<bool>,
    ) -> VmResult<()> {
        if !is_checked_type(pi) {
            return Ok(());
        }
        let decl = self.find_class(pi.class.as_bytes());
        let target = SourceProp {
            prop: pi.clone(),
            decl,
        };
        let strict = strict.unwrap_or_else(|| self.uses_strict_types());
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

    /// The slot of an uninitialized typed property `name` of `o` that a
    /// reference operation handles itself (`zend_std_get_property_ptr_ptr`
    /// returns the `IS_UNDEF` slot for writes); `None` leaves the case to
    /// the regular property path (untyped, hooked, readonly or
    /// asymmetric-visibility properties, inaccessible ones, properties
    /// `unset()` since, lazy objects).
    pub(crate) fn uninit_typed_slot(&mut self, o: &Obj, name: &ZStr) -> Option<Rc<PropInfo>> {
        let cls = object::class_of(o).clone();
        let scope = self.scope();
        let crate::props::PropLookup::Found(pi) = self.lookup_prop(&cls, name, scope.as_ref())
        else {
            return None;
        };
        if pi.hooks.is_some()
            || pi.flags & (crate::func::flags::READONLY | crate::func::flags::SET_MASK) != 0
            || !is_checked_type(&pi)
        {
            return None;
        }
        let idx = pi.slot as usize;
        let d = object_rc(o)?;
        let uninit = d
            .slots
            .try_borrow()
            .is_ok_and(|s| matches!(s.get(idx), Some(None)));
        // After unset(), `__get()` takes over.
        let reinit = d.slot_unset(idx) && cls.magic.get.is_some();
        if !uninit || reinit || d.lazy.borrow().is_some() {
            return None;
        }
        Some(pi)
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
        let Some(pi) = self.uninit_typed_slot(o, name) else {
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
        let Some(pi) = self.uninit_typed_slot(o, name) else {
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

// ---------------------------------------------------------------------------
// Typed static properties
// ---------------------------------------------------------------------------

/// A typed static property as seen through its storage cell.
struct StaticInfo {
    cell: Weak<RefCell<Value>>,
    prop: Rc<PropInfo>,
    /// Whether a value was ever stored (typed static properties without a
    /// default start out uninitialized).
    init: Cell<bool>,
}

thread_local! {
    /// Typed static properties by cell address (the place walk only sees
    /// the cell).
    ///
    /// Keyed by an address: hashed with `FxHash` (the default SipHash
    /// cost every fetch of a typed static property ~60 instructions).
    static STATIC_INFO: RefCell<rustc_hash::FxHashMap<usize, Rc<StaticInfo>>> =
        RefCell::new(rustc_hash::FxHashMap::default());
    /// The size of `STATIC_INFO` past which the entries of freed cells
    /// are dropped at the next insertion (twice what the last sweep
    /// left): a test runner that resets static properties between tests
    /// makes new cells all the time, and the map only grew.
    static STATIC_INFO_SWEEP: Cell<usize> = const { Cell::new(256) };
}

/// Resets typed reference state and static property caches for a new request.
pub(crate) fn reset() {
    INCDEC_OVERFLOW.with(|c| c.set(None));
    DEFERRED_ARG_WRITES.with(|d| d.borrow_mut().clear());
    STATIC_INFO.with(|m| m.borrow_mut().clear());
    STATIC_INFO_SWEEP.with(|c| c.set(256));
}

fn cell_key(cell: &Rc<RefCell<Value>>) -> usize {
    Rc::as_ptr(cell) as usize
}

/// Records a typed static property when its cell is fetched (so writes
/// through the cell can be checked).
pub(crate) fn note_static_prop(sp: &StaticProp) {
    if sp.ty.as_ref().is_none_or(TypeDecl::is_mixed) {
        return;
    }
    let key = cell_key(&sp.cell);
    STATIC_INFO.with(|m| {
        let mut m = m.borrow_mut();
        let fresh = m
            .get(&key)
            .is_none_or(|i| !i.cell.upgrade().is_some_and(|c| Rc::ptr_eq(&c, &sp.cell)));
        if fresh {
            if m.len() >= STATIC_INFO_SWEEP.with(Cell::get) {
                m.retain(|_, i| i.cell.strong_count() != 0);
                STATIC_INFO_SWEEP.with(|c| c.set((m.len() * 2).max(256)));
            }
            let ty = sp.ty.clone().unwrap_or_default();
            m.insert(
                key,
                Rc::new(StaticInfo {
                    cell: Rc::downgrade(&sp.cell),
                    prop: Rc::new(static_prop_info(sp, ty)),
                    init: Cell::new(sp.default.is_some()),
                }),
            );
        }
    });
}

fn static_info(cell: &Rc<RefCell<Value>>) -> Option<Rc<StaticInfo>> {
    let key = cell_key(cell);
    STATIC_INFO.with(|m| {
        m.borrow()
            .get(&key)
            .filter(|i| i.cell.upgrade().is_some_and(|c| Rc::ptr_eq(&c, cell)))
            .cloned()
    })
}

/// The builtin type bits a value may have to be stored as it is in the
/// typed static property behind `cell` (`place::propcache::accepts`), and
/// the lower-case name of the one class its type names, an instance of
/// which is stored as it is too: its declared type's, when a plain store
/// is all `zend_verify_property_type` would do for such a value (no
/// coercion). `(0, None)` for a property with a set visibility of its own
/// or `readonly`, and for a cell that is no noted typed property. A null
/// is taken only once the property is initialized (it is what an
/// uninitialized one holds, and the full write marks it initialized).
pub(crate) fn static_accept(cell: &Rc<RefCell<Value>>) -> (u32, Option<ZStr>) {
    let Some(info) = static_info(cell) else {
        return (0, None);
    };
    if info.prop.flags & (crate::func::flags::READONLY | crate::func::flags::SET_MASK) != 0 {
        return (0, None);
    }
    let Some(ty) = info.prop.ty.as_ref() else {
        return (0, None);
    };
    let bits = if info.init.get() {
        ty.mask
    } else {
        ty.mask & !mask::NULL
    };
    (bits, crate::place::propcache::single_class_name(ty))
}

impl StaticInfo {
    /// Uninitialized: never assigned, no default, and still `null`.
    fn is_uninit(&self, cell: &RefCell<Value>) -> bool {
        !self.init.get() && cell.try_borrow().is_ok_and(|v| matches!(*v, Value::Null))
    }
}

/// Whether `cell` is an uninitialized typed static property (`isset()` is
/// false).
pub(crate) fn static_cell_uninit(cell: &Rc<RefCell<Value>>) -> bool {
    static_info(cell).is_some_and(|i| i.is_uninit(cell))
}

/// Whether arrays may be auto-vivified in a property of this type
/// (`check_type_array_assignable`; `iterable` includes `array`).
fn type_allows_array(ty: Option<&TypeDecl>) -> bool {
    ty.is_none_or(|t| t.mask & (mask::ARRAY | mask::ITERABLE) != 0)
}

/// `promotes_to_array`: `null`/`false` (or a reference to one).
fn promotes_to_array(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null | Value::Bool(false)) => true,
        Some(Value::Reference(r)) => {
            matches!(
                r.try_borrow().as_deref(),
                Some(Value::Null | Value::Bool(false))
            )
        }
        Some(_) => false,
    }
}

impl Vm {
    fn source_of(&mut self, prop: &Rc<PropInfo>) -> SourceProp {
        let decl = self.find_class(prop.class.as_bytes());
        SourceProp {
            prop: prop.clone(),
            decl,
        }
    }

    /// `zend_throw_auto_init_in_prop_error`.
    fn auto_init_in_prop_error(&mut self, prop: &Rc<PropInfo>) -> Throw {
        let src = self.source_of(prop);
        let msg = format!(
            "Cannot auto-initialize an array inside property {} of type {}",
            src.qualified_name(),
            src.type_string()
        );
        self.make_throw("TypeError", &msg)
    }

    fn uninit_static_error(&mut self, prop: &PropInfo) -> Throw {
        let msg = format!(
            "Typed static property {}::${} must not be accessed before initialization",
            prop.class.to_string_lossy(),
            prop.name.to_string_lossy()
        );
        self.make_throw("Error", &msg)
    }

    /// Reading a static property (`zend_fetch_static_property_address`,
    /// `BP_VAR_R`): uninitialized typed ones throw (`isset` mode does not).
    pub(crate) fn typed_static_read_check(
        &mut self,
        cell: &Rc<RefCell<Value>>,
        mode: crate::opcode::FetchMode,
    ) -> VmResult<()> {
        if mode == crate::opcode::FetchMode::Is {
            return Ok(());
        }
        match static_info(cell) {
            Some(i) if i.is_uninit(cell) => Err(self.uninit_static_error(&i.prop)),
            _ => Ok(()),
        }
    }

    /// The type-checked part of a write walk starting at a static property
    /// cell: assignments (`zend_verify_property_type`, then the typed
    /// reference the cell may hold), `&Cls::$p` (type source),
    /// `Cls::$p = &$x` (`zend_verify_prop_assignable_by_ref`), reads for
    /// read-modify-write and array auto-vivification. Returns the result
    /// when the operation was performed here, `None` to continue with the
    /// regular walk.
    pub(crate) fn typed_static_write(
        &mut self,
        cell: &Rc<RefCell<Value>>,
        segs: &[crate::place::RSeg],
        wop: &mut crate::place::WOp,
    ) -> VmResult<Option<Value>> {
        use crate::place::{RSeg, WOp};
        let Some(info) = static_info(cell) else {
            return Ok(None);
        };
        let uninit = info.is_uninit(cell);
        if let Some(seg) = segs.first() {
            if matches!(wop, WOp::Unset) {
                return Ok(None);
            }
            if matches!(seg, RSeg::Dim(_) | RSeg::Append) {
                let promotes = uninit || promotes_to_array(cell.try_borrow().ok().as_deref());
                if promotes && !type_allows_array(info.prop.ty.as_ref()) {
                    return Err(self.auto_init_in_prop_error(&info.prop));
                }
            } else if uninit {
                return Err(self.uninit_static_error(&info.prop));
            }
            info.init.set(true);
            return Ok(None);
        }
        match wop {
            WOp::Assign(v) => {
                let v = std::mem::take(v);
                let typed_ref = cell_typed_ref(cell);
                let v = if typed_ref.is_some() && incdec_overflow().is_some() {
                    v
                } else {
                    self.incdec_prop_overflow(&info.prop, &v)?;
                    let v = match v {
                        Value::Reference(r) => r.get(),
                        v => v,
                    };
                    self.coerce_prop(None, &info.prop, v)?
                };
                info.init.set(true);
                if let Some(r) = typed_ref {
                    return self.assign_ref(&r, v).map(Some);
                }
                let old = match cell.try_borrow_mut() {
                    Ok(mut c) => match &mut *c {
                        Value::Reference(r) => r.set(v.clone()),
                        slot => std::mem::replace(slot, v.clone()),
                    },
                    Err(_) => return Ok(None),
                };
                drop(old);
                Ok(Some(v))
            }
            WOp::FetchRw | WOp::AssignOp(..) => {
                if uninit {
                    return Err(self.uninit_static_error(&info.prop));
                }
                Ok(None)
            }
            WOp::MakeRef => {
                if uninit && !info.prop.ty.as_ref().is_some_and(TypeDecl::allows_null) {
                    let msg = format!(
                        "Cannot access uninitialized non-nullable property {}::${} by reference",
                        info.prop.class.to_string_lossy(),
                        info.prop.name.to_string_lossy()
                    );
                    return Err(self.make_throw("Error", &msg));
                }
                let Ok(mut c) = cell.try_borrow_mut() else {
                    return Ok(None);
                };
                let r = c.make_ref();
                drop(c);
                info.init.set(true);
                let decl = self.find_class(info.prop.class.as_bytes());
                add_source(
                    &r,
                    Holder::Static(info.cell.clone()),
                    info.prop.clone(),
                    decl,
                );
                Ok(Some(Value::Reference(r)))
            }
            WOp::AssignRef(r) => {
                let r = std::mem::take(r);
                if let Value::Reference(x) = &r {
                    self.verify_prop_assignable_by_ref(&info.prop, x)?;
                    let decl = self.find_class(info.prop.class.as_bytes());
                    add_source(
                        x,
                        Holder::Static(info.cell.clone()),
                        info.prop.clone(),
                        decl,
                    );
                }
                info.init.set(true);
                let old = match cell.try_borrow_mut() {
                    Ok(mut c) => std::mem::replace(&mut *c, r.clone()),
                    Err(_) => return Ok(None),
                };
                drop(old);
                Ok(Some(r))
            }
            WOp::Unset => Ok(None),
            WOp::AssignDeref(_) => {
                let w = std::mem::replace(wop, WOp::Unset);
                *wop = w.resolve_deferred();
                self.typed_static_write(cell, segs, wop)
            }
        }
    }

    /// `$o->p[...] = v` on a declared typed property
    /// (`zend_handle_fetch_obj_flags`, `ZEND_FETCH_DIM_WRITE`): auto-
    /// vivifying `null`/`false`/uninitialized into an array needs a type
    /// accepting arrays; an uninitialized property whose type does is
    /// initialized to `null` first (the walk then creates the array).
    /// `next` is the segment after the property.
    pub(crate) fn typed_prop_dim_write_check(
        &mut self,
        o: &Obj,
        name: &ZStr,
        next: &crate::place::RSeg,
        wop: &crate::place::WOp,
    ) -> VmResult<()> {
        use crate::place::{RSeg, WOp};
        if matches!(wop, WOp::Unset) || matches!(next, RSeg::Prop(_)) {
            return Ok(());
        }
        let cls = object::class_of(o).clone();
        let Some(pi) = cls.prop_map.get(name).cloned() else {
            return Ok(());
        };
        if pi.hooks.is_some() || pi.flags & crate::func::flags::STATIC != 0 || pi.ty.is_none() {
            return Ok(());
        }
        let Some(d) = object_rc(o) else {
            return Ok(());
        };
        let idx = pi.slot as usize;
        let (promotes, uninit) = match d.slots.try_borrow() {
            Ok(s) => match s.get(idx) {
                Some(v) => (promotes_to_array(v.as_ref()), v.is_none()),
                None => return Ok(()),
            },
            Err(_) => return Ok(()),
        };
        if !promotes {
            return Ok(());
        }
        // Access checks, `__get()` and lazy objects: the regular path.
        let scope = self.scope();
        let accessible = matches!(
            self.lookup_prop(&cls, name, scope.as_ref()),
            crate::props::PropLookup::Found(p) if Rc::ptr_eq(&p, &pi)
        );
        let unset_since = d.slot_unset(idx);
        if !accessible
            || (uninit && (unset_since && cls.magic.get.is_some()))
            || pi.flags & (crate::func::flags::READONLY | crate::func::flags::SET_MASK) != 0
            || d.lazy.borrow().is_some()
        {
            return Ok(());
        }
        if !type_allows_array(pi.ty.as_ref()) {
            return Err(self.auto_init_in_prop_error(&pi));
        }
        if uninit {
            if let Ok(mut s) = d.slots.try_borrow_mut() {
                if let Some(slot @ None) = s.get_mut(idx) {
                    *slot = Some(Value::Null);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

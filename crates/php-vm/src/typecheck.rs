//! Type declarations at runtime: argument, return value and property type
//! verification with PHP's coercion rules (`zend_check_type`,
//! `zend_verify_scalar_type_hint`).

use crate::class::Class;
use crate::object;
use crate::types::{mask, ClassRef, ClassRefs, TypeDecl};
use crate::vm::{Vm, VmResult};
use php_runtime::numeric::{is_numeric_string_ex, Number};
use php_runtime::{convert, Value, ZStr};
use std::rc::Rc;

/// Context of a type check (for error messages and `self`/`static`).
#[derive(Clone, Copy)]
pub(crate) struct TypeCtx<'a> {
    pub scope: Option<&'a Rc<Class>>,
    pub called: Option<&'a Rc<Class>>,
    pub strict: bool,
    /// Internal function argument: null coerces with a deprecation.
    pub internal_arg: Option<(&'a str, usize, &'a str)>,
}

/// `instanceof_function` against the class a type member names, resolved
/// like `zend_fetch_class(name, ZEND_FETCH_CLASS_AUTO)`: `self`/`parent`
/// through the declaring scope, any other name by its lower-case form
/// against the names of the object's class, its parents and interfaces.
/// Called for every class-typed argument, return value and property
/// assignment: a matching check touches neither the class table nor the
/// allocator. Aliases are left to [`Vm::class_alias_matches`].
fn class_matches(cls: &Rc<Class>, member: &ClassRef, ctx: &TypeCtx<'_>) -> bool {
    match member {
        ClassRef::Named(lc) => cls.instance_of_name(lc.as_bytes()),
        ClassRef::Scope => ctx
            .scope
            .is_some_and(|s| cls.instance_of_name(s.lc_name.as_bytes())),
        ClassRef::Parent => ctx
            .scope
            .and_then(|s| s.parent.as_ref())
            .is_some_and(|p| cls.instance_of_name(p.lc_name.as_bytes())),
    }
}

impl Vm {
    /// `zend_fetch_ce_from_type` for the names the object's hierarchy does
    /// not carry, run only once every name comparison has failed: php-src
    /// resolves a type's class through the class table without autoloading
    /// (`ZEND_FETCH_CLASS_NO_AUTOLOAD`), and `class_alias()` registers a
    /// class under a second key there, so an object satisfies a type naming
    /// an alias of one of its classes.
    #[inline(never)]
    #[cold]
    fn class_alias_matches(&self, cls: &Rc<Class>, refs: &ClassRefs, ctx: &TypeCtx<'_>) -> bool {
        refs.classes
            .iter()
            .any(|c| self.aliased_class_matches(cls, c))
            || refs.intersections.iter().any(|group| {
                group
                    .iter()
                    .all(|c| class_matches(cls, c, ctx) || self.aliased_class_matches(cls, c))
            })
    }

    /// Whether `member` names, through a `class_alias()`, a class `cls` is
    /// an instance of (`instanceof_function` on the class table's entry).
    fn aliased_class_matches(&self, cls: &Rc<Class>, member: &ClassRef) -> bool {
        match member {
            ClassRef::Named(lc) => self
                .classes
                .get(lc)
                .is_some_and(|ce| ce.lc_name.as_bytes() != lc.as_bytes() && cls.is_subclass_of(ce)),
            // The declaring scope sits in the table under its own name.
            ClassRef::Scope | ClassRef::Parent => false,
        }
    }

    /// Whether `v` satisfies `ty` without coercion, resolving callables
    /// with the callable deprecations suppressed.
    pub(crate) fn type_accepts(&mut self, ty: &TypeDecl, v: &Value, ctx: &TypeCtx<'_>) -> bool {
        match v {
            Value::Int(_) => return ty.mask & mask::LONG != 0,
            Value::Float(_) => return ty.mask & mask::DOUBLE != 0,
            Value::Bool(b) => {
                let bit = if *b { mask::TRUE } else { mask::FALSE };
                return ty.mask & bit != 0;
            }
            _ => {}
        }
        // With the deprecations suppressed nothing in the check can fail.
        matches!(self.check_type(ty, v, ctx, false), Ok(true))
    }

    /// `zend_check_type_slow` without coercion: whether `v` satisfies `ty`.
    /// `emit_callable_deprecations` is php-src's `!is_internal`: a user
    /// function's parameter or return type resolves a string/array
    /// callable with `zend_is_callable_ex(arg, NULL, 0, ...)`, so the
    /// "Use of \"self\" in callables" and `Callables of the form [...]`
    /// deprecations are raised (in the current frame, i.e. at the callee's
    /// `RECV` or the `VERIFY_RETURN_TYPE` line), once per check.
    fn check_type(
        &mut self,
        ty: &TypeDecl,
        v: &Value,
        ctx: &TypeCtx<'_>,
        emit_callable_deprecations: bool,
    ) -> VmResult<bool> {
        let m = ty.mask;
        Ok(match v {
            Value::Int(_) => m & mask::LONG != 0,
            Value::Float(_) => m & mask::DOUBLE != 0,
            Value::Str(_) => {
                m & mask::STRING != 0
                    || (m & mask::CALLABLE != 0
                        && self.callable_accepts(v, emit_callable_deprecations)?)
            }
            Value::Bool(true) => m & mask::TRUE != 0,
            Value::Bool(false) => m & mask::FALSE != 0,
            Value::Null => m & mask::NULL != 0,
            Value::Array(_) => {
                m & (mask::ARRAY | mask::ITERABLE) != 0
                    || (m & mask::CALLABLE != 0
                        && self.callable_accepts(v, emit_callable_deprecations)?)
            }
            Value::Resource(_) => m & mask::RESOURCE != 0,
            Value::Reference(r) => {
                let inner = r.get();
                self.check_type(ty, &inner, ctx, emit_callable_deprecations)?
            }
            Value::Object(o) => {
                if m & mask::OBJECT != 0 {
                    return Ok(true);
                }
                let cls = object::class_of(o);
                if m & mask::ITERABLE != 0 && cls.instance_of_name(b"traversable") {
                    return Ok(true);
                }
                if m & mask::STATIC != 0 {
                    if let Some(c) = ctx.called {
                        if cls.is_subclass_of(c) {
                            return Ok(true);
                        }
                    }
                }
                // Objects (closures, `__invoke`) have no callable
                // deprecations: always resolve them suppressed.
                if m & mask::CALLABLE != 0 && self.is_callable_value(v) {
                    return Ok(true);
                }
                let refs = ty.class_refs();
                if refs.classes.iter().any(|c| class_matches(cls, c, ctx))
                    || refs
                        .intersections
                        .iter()
                        .any(|group| group.iter().all(|c| class_matches(cls, c, ctx)))
                {
                    return Ok(true);
                }
                ty.has_classes() && self.class_alias_matches(cls, refs, ctx)
            }
        })
    }

    /// The `MAY_BE_CALLABLE` step of `zend_check_type_slow` for a string or
    /// array: `zend_is_callable_ex` in the current frame, with
    /// `IS_CALLABLE_SUPPRESS_DEPRECATIONS` unless `emit_deprecations`.
    /// Emitting, an exception (a throwing error handler or autoloader)
    /// propagates.
    #[inline(never)]
    #[cold]
    fn callable_accepts(&mut self, v: &Value, emit_deprecations: bool) -> VmResult<bool> {
        if emit_deprecations {
            Ok(self.zend_is_callable(v, false)?.is_ok())
        } else {
            Ok(self.is_callable_value(v))
        }
    }

    /// Verifies and coerces `v` against `ty`. Returns `Ok(false)` when the
    /// value is not acceptable (the caller raises the `TypeError`).
    pub(crate) fn verify_type(
        &mut self,
        ty: &TypeDecl,
        v: &mut Value,
        ctx: &TypeCtx<'_>,
    ) -> VmResult<bool> {
        if ty.is_mixed() {
            return Ok(true);
        }
        // Fast-path: primitive values matching the declared mask without coercion
        match v {
            Value::Int(_) if ty.mask & mask::LONG != 0 => return Ok(true),
            Value::Float(_) if ty.mask & mask::DOUBLE != 0 => return Ok(true),
            Value::Str(_) if ty.mask & mask::STRING != 0 && ty.mask & mask::CALLABLE == 0 => {
                return Ok(true);
            }
            Value::Bool(b) => {
                let bit = if *b { mask::TRUE } else { mask::FALSE };
                if ty.mask & bit != 0 {
                    return Ok(true);
                }
            }
            Value::Null if ty.mask & mask::NULL != 0 => return Ok(true),
            _ => {}
        }
        if let Value::Reference(r) = v {
            let mut inner = r.get();
            // "We cannot have conversions for typed refs."
            if !Self::ref_allows_param_coercion(r) {
                return self.check_type(ty, &inner, ctx, ctx.internal_arg.is_none());
            }
            let ok = self.verify_type(ty, &mut inner, ctx)?;
            if ok {
                r.set(inner);
            }
            return Ok(ok);
        }
        // php-src's `is_internal`: only internal function arguments resolve
        // callables with the deprecations suppressed.
        if self.check_type(ty, v, ctx, ctx.internal_arg.is_none())? {
            return Ok(true);
        }
        let m = ty.mask;
        // Internal functions: null to scalar coerces with a deprecation.
        if let (Value::Null, Some((fname, argn, pname))) = (&*v, ctx.internal_arg) {
            if !ctx.strict && m & (mask::LONG | mask::DOUBLE | mask::STRING | mask::BOOL) != 0 {
                let tyname = ty.to_php_string();
                self.deprecated(&format!(
                    "{fname}(): Passing null to parameter #{argn} (${pname}) of type {tyname} is deprecated"
                ))?;
                *v = if m & mask::LONG != 0 {
                    Value::Int(0)
                } else if m & mask::DOUBLE != 0 {
                    Value::Float(0.0)
                } else if m & mask::STRING != 0 {
                    Value::Str(ZStr::empty())
                } else {
                    Value::Bool(false)
                };
                return Ok(true);
            }
        }
        // Strict mode: only int → float widening.
        if ctx.strict {
            if let Value::Int(i) = v {
                if m & mask::DOUBLE != 0 {
                    #[allow(clippy::cast_precision_loss)]
                    let f = *i as f64;
                    *v = Value::Float(f);
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        self.weak_scalar_coerce(m, v)
    }

    /// `zend_verify_weak_scalar_type_hint`: preference order
    /// int → float → string → bool.
    pub(crate) fn weak_scalar_coerce(&mut self, m: u32, v: &mut Value) -> VmResult<bool> {
        if m & mask::LONG != 0 {
            if let (true, Value::Str(s)) = (m & mask::DOUBLE != 0, &*v) {
                // int|float with a string: is_numeric_string() decides.
                if let Some(n) = is_numeric_string_ex(s.as_bytes(), false) {
                    *v = match n.value {
                        Number::Int(i) => Value::Int(i),
                        Number::Float(f) => Value::Float(f),
                    };
                    return Ok(true);
                }
            } else if let Some(l) = self.parse_arg_long_weak(v)? {
                *v = Value::Int(l);
                return Ok(true);
            }
        }
        if m & mask::DOUBLE != 0 {
            if let Some(d) = parse_arg_double_weak(v) {
                *v = Value::Float(d);
                return Ok(true);
            }
        }
        if m & mask::STRING != 0 {
            match v {
                Value::Int(i) => {
                    *v = Value::Str(convert::long_to_zstr(*i));
                    return Ok(true);
                }
                Value::Float(f) => {
                    let f = *f;
                    let s = convert::double_to_zstr(f, self).map_err(|e| self.rt_throw(e))?;
                    *v = Value::Str(s);
                    return Ok(true);
                }
                Value::Bool(b) => {
                    *v = Value::Str(ZStr::from(if *b { "1" } else { "" }));
                    return Ok(true);
                }
                Value::Object(o) => {
                    let o = o.clone();
                    if let Some(s) = self.object_to_string_opt(&o)? {
                        *v = Value::Str(s);
                        return Ok(true);
                    }
                }
                _ => {}
            }
        }
        if m & mask::BOOL == mask::BOOL {
            let b = match v {
                Value::Int(i) => Some(*i != 0),
                Value::Float(f) => {
                    if f.is_nan() {
                        self.warning("unexpected NAN value was coerced to bool")?;
                    }
                    Some(*f != 0.0)
                }
                Value::Str(s) => Some(!(s.is_empty() || s.as_bytes() == b"0")),
                Value::Bool(b) => Some(*b),
                _ => None,
            };
            if let Some(b) = b {
                *v = Value::Bool(b);
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `zend_parse_arg_long_weak` (with the precision-loss deprecations).
    #[allow(clippy::many_single_char_names)]
    pub(crate) fn parse_arg_long_weak(&mut self, v: &Value) -> VmResult<Option<i64>> {
        Ok(match v {
            Value::Int(i) => Some(*i),
            Value::Float(d) => {
                let d = *d;
                if d.is_nan() || !fits_long(d) {
                    return Ok(None);
                }
                #[allow(clippy::cast_possible_truncation)]
                let l = d as i64;
                #[allow(clippy::cast_precision_loss, clippy::float_cmp)]
                if l as f64 != d {
                    let s = php_runtime::float::format_double(d, -1, false);
                    self.deprecated(&format!(
                        "Implicit conversion from float {s} to int loses precision"
                    ))?;
                }
                Some(l)
            }
            Value::Str(s) => match is_numeric_string_ex(s.as_bytes(), false) {
                Some(n) => match n.value {
                    Number::Int(i) => Some(i),
                    Number::Float(d) => {
                        if d.is_nan() || !fits_long(d) {
                            return Ok(None);
                        }
                        #[allow(clippy::cast_possible_truncation)]
                        let l = d as i64;
                        #[allow(clippy::cast_precision_loss, clippy::float_cmp)]
                        if l as f64 != d {
                            self.deprecated(&format!(
                                "Implicit conversion from float-string \"{}\" to int loses precision",
                                s.to_string_lossy()
                            ))?;
                        }
                        Some(l)
                    }
                },
                None => None,
            },
            Value::Bool(b) => Some(i64::from(*b)),
            _ => None,
        })
    }
}

/// `zend_parse_arg_double_weak` for non-null values (`None` = NAN).
fn parse_arg_double_weak(v: &Value) -> Option<f64> {
    match v {
        #[allow(clippy::cast_precision_loss)]
        Value::Int(i) => Some(*i as f64),
        Value::Str(s) => is_numeric_string_ex(s.as_bytes(), false).map(|n| n.value.to_f64()),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// `ZEND_DOUBLE_FITS_LONG`.
pub(crate) fn fits_long(d: f64) -> bool {
    #[allow(clippy::cast_precision_loss)]
    let max = i64::MAX as f64;
    #[allow(clippy::cast_precision_loss)]
    let min = i64::MIN as f64;
    d >= min && d < max
}

/// The type name used in "X given" (`zend_zval_value_name`).
pub(crate) fn given_type(v: &Value) -> String {
    match v {
        Value::Reference(r) => given_type(&r.get()),
        Value::Object(o) => object::display_name(&object::class_of(o).name).to_string_lossy(),
        other => other.value_name(),
    }
}

#[cfg(test)]
mod tests;

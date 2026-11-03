//! `ReflectionType`, `ReflectionNamedType`, `ReflectionUnionType`,
//! `ReflectionIntersectionType` and the php-src view of type declarations
//! (`zend_type`).

use super::{new_refl, this_refl, Refl, PRIV, PUB};
use crate::builtins::classbuilder::ClassBuilder;
use crate::class::{cflags, Class};
use crate::func::Args;
use crate::types::{mask, TypeDecl};
use crate::vm::{Vm, VmResult};
use php_runtime::{Value, ZArray, ZStr};
use std::rc::Rc;

/// An element of a type list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TypeElem {
    /// A class name.
    Class(ZStr),
    /// An intersection group.
    Inter(Vec<ZStr>),
}

/// A type in php-src form: a builtin type mask plus a list of class names /
/// intersection groups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ZType {
    /// `MAY_BE_*` bits (`crate::types::mask`, without `ITERABLE`).
    pub mask: u32,
    /// Class names and intersection groups.
    pub names: Vec<TypeElem>,
    /// A pure intersection type (`A&B`): `names` are its members.
    pub intersection: bool,
    /// `iterable` (BC: displayed as `iterable`).
    pub iterable: bool,
}

impl ZType {
    /// Converts the VM's type declaration.
    pub fn from_decl(t: &TypeDecl) -> Self {
        let mut z = ZType { mask: t.mask & !mask::ITERABLE, names: Vec::new(), intersection: false, iterable: false };
        if t.pure_intersection {
            if let Some(g) = t.intersections.first() {
                z.names = g.iter().map(|n| TypeElem::Class(n.clone())).collect();
            }
            z.intersection = true;
            return z;
        }
        for g in &t.intersections {
            z.names.push(TypeElem::Inter(g.clone()));
        }
        for c in &t.classes {
            z.names.push(TypeElem::Class(c.clone()));
        }
        if t.mask & mask::ITERABLE != 0 {
            if z.names.is_empty() && t.mask & !(mask::ITERABLE | mask::NULL) == 0 {
                z.iterable = true;
            }
            if !z.names.iter().any(|e| matches!(e, TypeElem::Class(c) if c.eq_ignore_ascii_case(b"Traversable"))) {
                z.names.push(TypeElem::Class(ZStr::from("Traversable")));
            }
            z.mask |= mask::ARRAY;
        }
        z
    }

    /// A builtin-only type.
    pub fn simple(m: u32) -> Self {
        ZType { mask: m, names: Vec::new(), intersection: false, iterable: false }
    }

    /// A single class type.
    pub fn class(n: ZStr) -> Self {
        ZType { mask: 0, names: vec![TypeElem::Class(n)], intersection: false, iterable: false }
    }

    /// `ZEND_TYPE_ALLOW_NULL`.
    pub fn allows_null(&self) -> bool {
        self.mask & mask::NULL != 0
    }

    /// Whether the type is `mixed`.
    pub fn is_mixed(&self) -> bool {
        self.mask & mask::ANY == mask::ANY
    }

    /// `zend_type_to_string`.
    pub fn to_php(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.intersection {
            let s: Vec<String> = self
                .names
                .iter()
                .map(|e| match e {
                    TypeElem::Class(c) => c.to_string_lossy(),
                    TypeElem::Inter(g) => g.iter().map(ZStr::to_string_lossy).collect::<Vec<_>>().join("&"),
                })
                .collect();
            parts.push(s.join("&"));
        } else {
            for e in &self.names {
                match e {
                    TypeElem::Class(c) => parts.push(c.to_string_lossy()),
                    TypeElem::Inter(g) => {
                        parts.push(format!("({})", g.iter().map(ZStr::to_string_lossy).collect::<Vec<_>>().join("&")));
                    }
                }
            }
        }
        let m = self.mask;
        if m & mask::ANY == mask::ANY {
            parts.push("mixed".into());
            return parts.join("|");
        }
        let simple = [
            (mask::STATIC, "static"),
            (mask::CALLABLE, "callable"),
            (mask::OBJECT, "object"),
            (mask::ARRAY, "array"),
            (mask::STRING, "string"),
            (mask::LONG, "int"),
            (mask::DOUBLE, "float"),
        ];
        for (b, n) in simple {
            if m & b != 0 {
                parts.push(n.into());
            }
        }
        if m & mask::BOOL == mask::BOOL {
            parts.push("bool".into());
        } else if m & mask::FALSE != 0 {
            parts.push("false".into());
        } else if m & mask::TRUE != 0 {
            parts.push("true".into());
        }
        if m & mask::VOID != 0 {
            parts.push("void".into());
        }
        if m & mask::NEVER != 0 {
            parts.push("never".into());
        }
        let mut s = parts.join("|");
        if m & mask::NULL != 0 {
            if !s.is_empty() && !s.contains('|') && !s.contains('&') {
                return format!("?{s}");
            }
            if !s.is_empty() {
                s.push('|');
            }
            s.push_str("null");
        }
        s
    }

    /// `zend_named_reflection_type_to_string` (BC for `iterable`).
    pub fn to_named_string(&self) -> String {
        if self.iterable {
            return if self.allows_null() { "?iterable".into() } else { "iterable".into() };
        }
        self.to_php()
    }

    /// `get_type_kind`: 0 named, 1 union, 2 intersection.
    pub fn kind(&self) -> u8 {
        if self.intersection {
            return 2;
        }
        let without_null = self.mask & !mask::NULL;
        if self.names.len() > 1 || self.names.iter().any(|e| matches!(e, TypeElem::Inter(_))) {
            return 1;
        }
        if !self.names.is_empty() {
            if self.iterable {
                return 0;
            }
            return u8::from(without_null != 0);
        }
        if without_null == mask::BOOL || self.mask & mask::ANY == mask::ANY {
            return 0;
        }
        u8::from(without_null & without_null.wrapping_sub(1) != 0)
    }
}

/// The current `ZType` of a reflection type object.
fn this_type(vm: &mut Vm, a: &Args) -> VmResult<(ZType, bool)> {
    match this_refl(vm, a)? {
        Refl::Type { ty, legacy, .. } => Ok((ZType::from_decl(&ty), legacy)),
        _ => Err(vm.make_throw("Error", "Internal error: Failed to retrieve the reflection object")),
    }
}

/// Converts a `ZType` back to the VM representation (for nested types).
fn to_decl(z: &ZType) -> TypeDecl {
    let mut t = TypeDecl { mask: z.mask, ..Default::default() };
    if z.iterable {
        t.mask = (z.mask & !mask::ARRAY) | mask::ITERABLE;
        return t;
    }
    if z.intersection {
        t.pure_intersection = true;
        t.intersections.push(
            z.names
                .iter()
                .filter_map(|e| match e {
                    TypeElem::Class(c) => Some(c.clone()),
                    TypeElem::Inter(_) => None,
                })
                .collect(),
        );
        return t;
    }
    for e in &z.names {
        match e {
            TypeElem::Class(c) => t.classes.push(c.clone()),
            TypeElem::Inter(g) => t.intersections.push(g.clone()),
        }
    }
    t
}

/// `reflection_type_factory`.
pub(crate) fn type_value(vm: &mut Vm, ty: &TypeDecl, scope: Option<&Rc<Class>>, legacy: bool) -> VmResult<Value> {
    let z = ZType::from_decl(ty);
    zt_value(vm, &z, scope, legacy)
}

fn zt_value(vm: &mut Vm, z: &ZType, scope: Option<&Rc<Class>>, legacy: bool) -> VmResult<Value> {
    let kind = z.kind();
    let cls = match kind {
        2 => "ReflectionIntersectionType",
        1 => "ReflectionUnionType",
        _ => "ReflectionNamedType",
    };
    let is_only_null = z.mask == mask::NULL && z.names.is_empty();
    let legacy = legacy && kind == 0 && !z.is_mixed() && !is_only_null;
    new_refl(vm, cls, Refl::Type { ty: to_decl(z), scope: scope.cloned(), legacy }, None, None)
}

fn rt_allows_null(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(this_type(vm, a)?.0.allows_null()))
}

fn rt_to_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Str(ZStr::from(this_type(vm, a)?.0.to_named_string())))
}

fn rt_get_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (mut z, legacy) = this_type(vm, a)?;
    if legacy {
        z.mask &= !mask::NULL;
    }
    Ok(Value::Str(ZStr::from(z.to_named_string())))
}

fn rt_is_builtin(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (z, _) = this_type(vm, a)?;
    if z.iterable {
        return Ok(Value::Bool(true));
    }
    Ok(Value::Bool(z.names.is_empty() && z.mask & mask::STATIC == 0))
}

fn push_type(vm: &mut Vm, out: &mut Vec<Value>, mut z: ZType, scope: Option<&Rc<Class>>) -> VmResult<()> {
    z.iterable = false;
    out.push(zt_value(vm, &z, scope, false)?);
    Ok(())
}

fn rt_get_types(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (z, _) = this_type(vm, a)?;
    let scope = match this_refl(vm, a)? {
        Refl::Type { scope, .. } => scope,
        _ => None,
    };
    let mut out = Vec::new();
    for e in &z.names {
        let t = match e {
            TypeElem::Class(c) => ZType::class(c.clone()),
            TypeElem::Inter(g) => ZType {
                mask: 0,
                names: g.iter().map(|n| TypeElem::Class(n.clone())).collect(),
                intersection: true,
                iterable: false,
            },
        };
        push_type(vm, &mut out, t, scope.as_ref())?;
    }
    if z.intersection {
        return Ok(Value::Array(ZArray::from_values(out)));
    }
    let m = z.mask;
    for b in [mask::STATIC, mask::CALLABLE, mask::OBJECT, mask::ARRAY, mask::STRING, mask::LONG, mask::DOUBLE] {
        if m & b != 0 {
            push_type(vm, &mut out, ZType::simple(b), scope.as_ref())?;
        }
    }
    if m & mask::BOOL == mask::BOOL {
        push_type(vm, &mut out, ZType::simple(mask::BOOL), scope.as_ref())?;
    } else if m & mask::TRUE != 0 {
        push_type(vm, &mut out, ZType::simple(mask::TRUE), scope.as_ref())?;
    } else if m & mask::FALSE != 0 {
        push_type(vm, &mut out, ZType::simple(mask::FALSE), scope.as_ref())?;
    }
    if m & mask::NULL != 0 {
        push_type(vm, &mut out, ZType::simple(mask::NULL), scope.as_ref())?;
    }
    Ok(Value::Array(ZArray::from_values(out)))
}

fn clone_error(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(super::rexc(vm, "Cannot clone object using __clone()"))
}

/// Registers the type classes.
pub(crate) fn register(vm: &mut Vm) {
    ClassBuilder::new("ReflectionType")
        .flags(cflags::ABSTRACT | cflags::EXPLICIT_ABSTRACT | cflags::NOT_SERIALIZABLE)
        .implements(vm, &["Stringable"])
        .method("__clone(): void", PRIV, clone_error)
        .method("allowsNull(): bool", PUB, rt_allows_null)
        .method("__toString(): string", PUB, rt_to_string)
        .register(vm);
    ClassBuilder::new("ReflectionNamedType")
        .extends(vm, "ReflectionType")
        .method("getName(): string", PUB, rt_get_name)
        .method("isBuiltin(): bool", PUB, rt_is_builtin)
        .register(vm);
    ClassBuilder::new("ReflectionUnionType")
        .extends(vm, "ReflectionType")
        .method("getTypes(): array", PUB, rt_get_types)
        .register(vm);
    ClassBuilder::new("ReflectionIntersectionType")
        .extends(vm, "ReflectionType")
        .method("getTypes(): array", PUB, rt_get_types)
        .register(vm);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(m: u32, classes: &[&str]) -> TypeDecl {
        TypeDecl { mask: m, classes: classes.iter().map(|c| ZStr::from(*c)).collect(), ..Default::default() }
    }

    #[test]
    fn nullable_and_union_rendering() {
        assert_eq!(ZType::from_decl(&decl(mask::LONG | mask::NULL, &[])).to_php(), "?int");
        assert_eq!(ZType::from_decl(&decl(mask::LONG | mask::STRING | mask::NULL, &[])).to_php(), "string|int|null");
        assert_eq!(ZType::from_decl(&decl(mask::NULL, &["Foo"])).to_php(), "?Foo");
        assert_eq!(ZType::from_decl(&decl(mask::ANY, &[])).to_php(), "mixed");
        assert_eq!(ZType::from_decl(&decl(mask::FALSE | mask::LONG, &[])).to_php(), "int|false");
    }

    #[test]
    fn iterable_bc() {
        let z = ZType::from_decl(&decl(mask::ITERABLE, &[]));
        assert_eq!(z.to_named_string(), "iterable");
        assert_eq!(z.to_php(), "Traversable|array");
        assert_eq!(z.kind(), 0);
        let z = ZType::from_decl(&decl(mask::ITERABLE | mask::NULL, &[]));
        assert_eq!(z.to_named_string(), "?iterable");
    }

    #[test]
    fn kinds() {
        assert_eq!(ZType::from_decl(&decl(mask::BOOL | mask::NULL, &[])).kind(), 0);
        assert_eq!(ZType::from_decl(&decl(mask::LONG | mask::STRING, &[])).kind(), 1);
        assert_eq!(ZType::from_decl(&decl(mask::LONG, &["A"])).kind(), 1);
        assert_eq!(ZType::from_decl(&decl(mask::NULL, &["A"])).kind(), 0);
        assert_eq!(ZType::from_decl(&decl(0, &["A", "B"])).kind(), 1);
    }
}

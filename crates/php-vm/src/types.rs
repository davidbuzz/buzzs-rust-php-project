// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Type declarations (`zend_type`).

use php_runtime::ZStr;
use std::cell::OnceCell;
use std::fmt;
use std::fmt::Write as _;

/// Bits of [`TypeDecl::mask`] (`MAY_BE_*`).
pub mod mask {
    /// `null`
    pub const NULL: u32 = 1 << 0;
    /// `false`
    pub const FALSE: u32 = 1 << 1;
    /// `true`
    pub const TRUE: u32 = 1 << 2;
    /// `bool`
    pub const BOOL: u32 = FALSE | TRUE;
    /// `int`
    pub const LONG: u32 = 1 << 3;
    /// `float`
    pub const DOUBLE: u32 = 1 << 4;
    /// `string`
    pub const STRING: u32 = 1 << 5;
    /// `array`
    pub const ARRAY: u32 = 1 << 6;
    /// `object`
    pub const OBJECT: u32 = 1 << 7;
    /// `resource` (never declarable; used by `mixed`)
    pub const RESOURCE: u32 = 1 << 8;
    /// `callable`
    pub const CALLABLE: u32 = 1 << 9;
    /// `void`
    pub const VOID: u32 = 1 << 10;
    /// `never`
    pub const NEVER: u32 = 1 << 11;
    /// `static`
    pub const STATIC: u32 = 1 << 12;
    /// `iterable` (kept for display: `Traversable|array`)
    pub const ITERABLE: u32 = 1 << 13;
    /// `mixed`
    pub const ANY: u32 = NULL | BOOL | LONG | DOUBLE | STRING | ARRAY | OBJECT | RESOURCE;
}

/// A class-like member in declaration order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeMember {
    /// Index into `TypeDecl::classes`.
    Class(usize),
    /// Index into `TypeDecl::intersections`.
    Intersection(usize),
    /// The `Traversable` member of the iterable alias.
    Iterable,
}

/// A class-like member of a type as the runtime check resolves it
/// (`zend_fetch_class` with `ZEND_FETCH_CLASS_AUTO` on the declared name).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClassRef {
    /// `self`: the declaring class.
    Scope,
    /// `parent`: the declaring class's parent.
    Parent,
    /// A class name lower-cased and without a leading `\`, the form the
    /// class table keys it by (`Class::lc_name`).
    Named(ZStr),
}

impl ClassRef {
    /// Classifies a declared class name.
    fn from_name(name: &ZStr) -> Self {
        let bytes = name.as_bytes();
        let stripped = bytes.strip_prefix(b"\\").unwrap_or(bytes);
        if stripped.eq_ignore_ascii_case(b"self") {
            ClassRef::Scope
        } else if stripped.eq_ignore_ascii_case(b"parent") {
            ClassRef::Parent
        } else if stripped.len() == bytes.len() {
            ClassRef::Named(name.to_ascii_lowercase())
        } else {
            ClassRef::Named(ZStr::from(stripped.to_ascii_lowercase()))
        }
    }
}

/// The class-like members of a [`TypeDecl`] as the runtime check compares
/// them ([`TypeDecl::class_refs`]), parallel to `classes` and
/// `intersections`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassRefs {
    /// One per `TypeDecl::classes` entry.
    pub classes: Vec<ClassRef>,
    /// One group per `TypeDecl::intersections` entry.
    pub intersections: Vec<Vec<ClassRef>>,
}

/// A parsed type declaration.
#[derive(Clone, Default)]
pub struct TypeDecl {
    /// Builtin type bits.
    pub mask: u32,
    /// Class names of a union (resolved; `self`/`parent` kept as written,
    /// lower-case).
    pub classes: Vec<ZStr>,
    /// Intersection groups (`A&B`, or DNF members `(A&B)|C`).
    pub intersections: Vec<Vec<ZStr>>,
    /// Whether the type is a pure intersection type (`A&B`).
    pub pure_intersection: bool,
    /// Class-like members in source order; empty for legacy builtin declarations.
    pub members: Vec<TypeMember>,
    /// [`Self::class_refs`], computed by the first runtime check of the
    /// finished declaration; cloned along, ignored by equality.
    pub lc_cache: OnceCell<ClassRefs>,
    /// The class of the last object the full check accepted for this type
    /// through a `class_alias()` name (`TypeDecl::learn_alias_class`):
    /// the by-name tests of the plain call paths cannot see an alias, so
    /// every call passing such an object took the generic path. Held
    /// weakly, which pins the address compared. Cloned along, ignored by
    /// equality.
    pub alias_ok: std::cell::RefCell<Option<std::rc::Weak<crate::class::Class>>>,
}

impl fmt::Debug for TypeDecl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypeDecl")
            .field("mask", &self.mask)
            .field("classes", &self.classes)
            .field("intersections", &self.intersections)
            .field("pure_intersection", &self.pure_intersection)
            .field("members", &self.members)
            .finish_non_exhaustive()
    }
}

impl PartialEq for TypeDecl {
    fn eq(&self, other: &Self) -> bool {
        self.mask == other.mask
            && self.classes == other.classes
            && self.intersections == other.intersections
            && self.pure_intersection == other.pure_intersection
            && self.members == other.members
    }
}

impl Eq for TypeDecl {}

impl TypeDecl {
    /// Records that objects of `cls` satisfy this type through an alias.
    #[cold]
    #[inline(never)]
    pub(crate) fn learn_alias_class(&self, cls: &std::rc::Rc<crate::class::Class>) {
        if let Ok(mut slot) = self.alias_ok.try_borrow_mut() {
            *slot = Some(std::rc::Rc::downgrade(cls));
        }
    }

    /// Whether `cls` is the class [`TypeDecl::learn_alias_class`] recorded.
    #[inline]
    pub(crate) fn alias_class_is(&self, cls: &std::rc::Rc<crate::class::Class>) -> bool {
        self.alias_ok.try_borrow().is_ok_and(|slot| {
            slot.as_ref()
                .is_some_and(|w| std::ptr::eq(w.as_ptr(), std::rc::Rc::as_ptr(cls)))
        })
    }

    /// The class-like members lower-cased for the runtime check, computed
    /// once per declaration (php-src's `zend_type` names resolve through
    /// the class table's lower-case keys; `self`/`parent` through the
    /// declaring scope). The declaration must not change afterwards.
    #[inline]
    pub fn class_refs(&self) -> &ClassRefs {
        match self.lc_cache.get() {
            Some(refs) => {
                debug_assert!(
                    refs.classes.len() == self.classes.len()
                        && refs.intersections.len() == self.intersections.len(),
                    "type declaration changed after its first check"
                );
                refs
            }
            None => self.init_class_refs(),
        }
    }

    #[cold]
    #[inline(never)]
    fn init_class_refs(&self) -> &ClassRefs {
        self.lc_cache.get_or_init(|| ClassRefs {
            classes: self.classes.iter().map(ClassRef::from_name).collect(),
            intersections: self
                .intersections
                .iter()
                .map(|group| group.iter().map(ClassRef::from_name).collect())
                .collect(),
        })
    }

    /// `mixed`.
    pub fn mixed() -> Self {
        TypeDecl {
            mask: mask::ANY,
            ..Default::default()
        }
    }

    /// A builtin-only type.
    pub fn simple(m: u32) -> Self {
        TypeDecl {
            mask: m,
            ..Default::default()
        }
    }

    /// Whether `null` is accepted.
    pub fn allows_null(&self) -> bool {
        self.mask & mask::NULL != 0
    }

    /// Whether this is exactly `mixed`.
    pub fn is_mixed(&self) -> bool {
        self.mask & mask::ANY == mask::ANY
    }

    /// Whether any class names are involved.
    pub fn has_classes(&self) -> bool {
        !self.classes.is_empty() || !self.intersections.is_empty()
    }

    /// Whether the type is exactly `void`.
    pub fn is_void(&self) -> bool {
        self.mask == mask::VOID && !self.has_classes()
    }

    /// Whether the type is exactly `never`.
    pub fn is_never(&self) -> bool {
        self.mask == mask::NEVER && !self.has_classes()
    }

    /// Checks a folded property or class-constant initializer without weak coercion.
    pub(crate) fn accepts_default(&self, value: &php_runtime::Value) -> bool {
        use php_runtime::Value;
        let bit = match value {
            Value::Null => mask::NULL,
            Value::Bool(false) => mask::FALSE,
            Value::Bool(true) => mask::TRUE,
            Value::Int(_) => mask::LONG | mask::DOUBLE,
            Value::Float(_) => mask::DOUBLE,
            Value::Str(_) => mask::STRING,
            Value::Array(_) => mask::ARRAY | mask::ITERABLE,
            _ => return false,
        };
        self.mask & bit != 0
    }

    /// Renders the type like `zend_type_to_string_resolved`; `self_name` and
    /// `parent_name` resolve `self`/`parent`, `static_name` resolves
    /// `static`.
    pub fn display(
        &self,
        self_name: Option<&ZStr>,
        parent_name: Option<&ZStr>,
        static_name: Option<&ZStr>,
    ) -> String {
        let resolve = |n: &ZStr| -> String {
            if n.eq_ignore_ascii_case(b"self") {
                if let Some(s) = self_name {
                    return crate::object::display_name(s).to_string_lossy();
                }
            } else if n.eq_ignore_ascii_case(b"parent") {
                if let Some(p) = parent_name {
                    return crate::object::display_name(p).to_string_lossy();
                }
            }
            crate::object::display_name(n).to_string_lossy()
        };
        let mut parts: Vec<String> = Vec::new();
        if self.pure_intersection {
            if let Some(g) = self.intersections.first() {
                parts.push(g.iter().map(&resolve).collect::<Vec<_>>().join("&"));
            }
        } else if self.members.is_empty() {
            for g in &self.intersections {
                parts.push(format!(
                    "({})",
                    g.iter().map(&resolve).collect::<Vec<_>>().join("&")
                ));
            }
            for c in &self.classes {
                parts.push(resolve(c));
            }
        } else {
            for member in &self.members {
                match *member {
                    TypeMember::Class(i) => parts.push(resolve(&self.classes[i])),
                    TypeMember::Intersection(i) => parts.push(format!(
                        "({})",
                        self.intersections[i]
                            .iter()
                            .map(&resolve)
                            .collect::<Vec<_>>()
                            .join("&")
                    )),
                    TypeMember::Iterable => parts.push("Traversable".into()),
                }
            }
        }
        let m = self.mask;
        if m & mask::ANY == mask::ANY {
            parts.push("mixed".into());
            return parts.join("|");
        }
        if m & mask::ITERABLE != 0 {
            // `iterable` is displayed as `Traversable|array`.
            if !parts.iter().any(|p| p.eq_ignore_ascii_case("Traversable")) {
                parts.push("Traversable".into());
            }
        }
        if m & mask::STATIC != 0 {
            parts.push(static_name.map_or_else(|| "static".to_string(), ZStr::to_string_lossy));
        }
        if m & mask::CALLABLE != 0 {
            parts.push("callable".into());
        }
        if m & mask::OBJECT != 0 {
            parts.push("object".into());
        }
        if m & (mask::ARRAY | mask::ITERABLE) != 0 {
            parts.push("array".into());
        }
        if m & mask::STRING != 0 {
            parts.push("string".into());
        }
        if m & mask::LONG != 0 {
            parts.push("int".into());
        }
        if m & mask::DOUBLE != 0 {
            parts.push("float".into());
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

    /// Display without scope resolution.
    pub fn to_php_string(&self) -> String {
        self.display(None, None, None)
    }
}

/// Builds the "must be of type X, Y given" fragment.
pub fn type_error_fragment(expected: &str, given: &str) -> String {
    let mut s = String::new();
    let _ = write!(s, "must be of type {expected}, {given} given");
    s
}

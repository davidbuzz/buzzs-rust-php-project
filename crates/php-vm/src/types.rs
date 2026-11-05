//! Type declarations (`zend_type`).

use php_runtime::ZStr;
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

/// A parsed type declaration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
}

impl TypeDecl {
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
                    return s.to_string_lossy();
                }
            } else if n.eq_ignore_ascii_case(b"parent") {
                if let Some(p) = parent_name {
                    return p.to_string_lossy();
                }
            }
            n.to_string_lossy()
        };
        let mut parts: Vec<String> = Vec::new();
        if self.pure_intersection {
            if let Some(g) = self.intersections.first() {
                parts.push(g.iter().map(&resolve).collect::<Vec<_>>().join("&"));
            }
        } else {
            for g in &self.intersections {
                parts.push(format!(
                    "({})",
                    g.iter().map(&resolve).collect::<Vec<_>>().join("&")
                ));
            }
            for c in &self.classes {
                parts.push(resolve(c));
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

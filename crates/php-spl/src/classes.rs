//! The SPL class table: every class and interface with its parent,
//! interfaces, constants and method signatures (generated from the
//! `*.stub.php` files by `tools/gen_classes.py`), so the VM can register
//! them generically.

/// Whether a declaration is a class, an abstract class or an interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassKind {
    /// A concrete class.
    Class,
    /// An abstract class.
    Abstract,
    /// An interface.
    Interface,
}

/// Method modifier bits for [`MethodDef::flags`].
#[derive(Debug, Clone, Copy)]
pub struct MethodFlags;

impl MethodFlags {
    /// `static`.
    pub const STATIC: u32 = 1;
    /// `abstract`.
    pub const ABSTRACT: u32 = 2;
    /// `final`.
    pub const FINAL: u32 = 4;
    /// `protected`.
    pub const PROTECTED: u32 = 8;
    /// `private`.
    pub const PRIVATE: u32 = 16;
}

/// A parameter as declared in the stub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamDef {
    /// Name without `$`.
    pub name: &'static str,
    /// Declared type, as written (`?string`, `Traversable|array`, ...).
    pub ty: Option<&'static str>,
    /// Default value as PHP source (`null`, `0`, `SORT_REGULAR`, ...).
    pub default: Option<&'static str>,
    /// Passed by reference.
    pub by_ref: bool,
    /// Variadic (`...$args`).
    pub variadic: bool,
}

/// A `#[\Deprecated]` attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deprecation {
    /// `since:` value.
    pub since: Option<&'static str>,
    /// `message:` value.
    pub message: Option<&'static str>,
}

/// A method or function signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodDef {
    /// Name as declared.
    pub name: &'static str,
    /// [`MethodFlags`] bits (public when no visibility bit is set).
    pub flags: u32,
    /// Parameters.
    pub params: &'static [ParamDef],
    /// Declared return type.
    pub return_type: Option<&'static str>,
    /// Whether the return type is tentative.
    pub tentative: bool,
    /// Deprecation attribute.
    pub deprecated: Option<Deprecation>,
    /// `@implementation-alias` target (`Class::method`), informational.
    pub alias_of: Option<&'static str>,
}

impl MethodDef {
    /// Number of required parameters.
    #[must_use]
    pub fn required(&self) -> usize {
        self.params
            .iter()
            .take_while(|p| p.default.is_none() && !p.variadic)
            .count()
    }
}

/// A class or interface declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassDef {
    /// Class name.
    pub name: &'static str,
    /// Class, abstract class or interface.
    pub kind: ClassKind,
    /// `final`.
    pub is_final: bool,
    /// Parent class (`None` for roots and interfaces).
    pub parent: Option<&'static str>,
    /// Directly implemented interfaces (for interfaces: extended ones).
    pub interfaces: &'static [&'static str],
    /// Class constants (all `int`).
    pub constants: &'static [(&'static str, i64)],
    /// Methods declared by this class.
    pub methods: &'static [MethodDef],
}

mod generated {
    #![allow(clippy::too_many_lines)]
    include!("classes_gen.rs");
}

pub use generated::{CLASSES, FUNCTIONS};

/// All SPL classes and interfaces in registration order.
#[must_use]
pub fn class_table() -> &'static [ClassDef] {
    CLASSES
}

/// Looks up a class by name (any case).
#[must_use]
pub fn find_class(name: &str) -> Option<&'static ClassDef> {
    CLASSES.iter().find(|c| c.name.eq_ignore_ascii_case(name))
}

/// The canonical spelling of an SPL class name.
#[must_use]
pub fn canonical_name(name: &str) -> Option<&'static str> {
    find_class(name).map(|c| c.name)
}

/// Finds the declaration of `method` for `class`, walking up the SPL
/// parents; returns the declaring class and the signature.
#[must_use]
pub fn find_method(class: &str, method: &str) -> Option<(&'static str, &'static MethodDef)> {
    let mut cur = find_class(class);
    while let Some(c) = cur {
        if let Some(m) = c
            .methods
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(method))
        {
            return Some((c.name, m));
        }
        cur = c.parent.and_then(find_class);
    }
    None
}

/// A class constant value.
#[must_use]
pub fn constant(class: &str, name: &str) -> Option<i64> {
    let mut cur = find_class(class);
    while let Some(c) = cur {
        if let Some((_, v)) = c.constants.iter().find(|(n, _)| *n == name) {
            return Some(*v);
        }
        cur = c.parent.and_then(find_class);
    }
    None
}

/// Whether `class` is `ancestor` or one of its SPL subclasses, or
/// implements it (SPL interfaces and the engine interfaces named in the
/// table).
#[must_use]
pub fn is_subclass_of(class: &str, ancestor: &str) -> bool {
    if class.eq_ignore_ascii_case(ancestor) {
        return true;
    }
    let Some(c) = find_class(class) else {
        if let Some((_, parents)) = ENGINE_INTERFACES
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(class))
        {
            return parents.iter().any(|p| is_subclass_of(p, ancestor));
        }
        return crate::exceptions::exception_is_a(class, ancestor);
    };
    if c.interfaces.iter().any(|i| is_subclass_of(i, ancestor)) {
        return true;
    }
    c.parent.is_some_and(|p| is_subclass_of(p, ancestor))
}

/// Engine interfaces SPL classes implement, with the interfaces they
/// extend.
const ENGINE_INTERFACES: &[(&str, &[&str])] = &[
    ("Traversable", &[]),
    ("Iterator", &["Traversable"]),
    ("IteratorAggregate", &["Traversable"]),
    ("ArrayAccess", &[]),
    ("Countable", &[]),
    ("Serializable", &[]),
    ("Stringable", &[]),
    ("JsonSerializable", &[]),
    ("Throwable", &["Stringable"]),
    ("Exception", &["Throwable"]),
];

/// The SPL functions.
#[must_use]
pub fn function_table() -> &'static [MethodDef] {
    FUNCTIONS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_basics() {
        assert_eq!(CLASSES.len(), 55);
        let c = find_class("splstack").unwrap();
        assert_eq!(c.parent, Some("SplDoublyLinkedList"));
        assert_eq!(constant("SplStack", "IT_MODE_LIFO"), Some(2));
        let (decl, m) = find_method("SplMinHeap", "insert").unwrap();
        assert_eq!(decl, "SplHeap");
        assert_eq!(m.required(), 1);
        assert!(is_subclass_of("RecursiveArrayIterator", "Traversable"));
        assert!(is_subclass_of("SplTempFileObject", "SplFileInfo"));
        assert!(is_subclass_of("BadMethodCallException", "LogicException"));
        assert_eq!(constant("RecursiveTreeIterator", "LEAVES_ONLY"), Some(0));
        let (_, fa) = find_method("SplFixedArray", "fromArray").unwrap();
        assert_ne!(fa.flags & MethodFlags::STATIC, 0);
    }
}

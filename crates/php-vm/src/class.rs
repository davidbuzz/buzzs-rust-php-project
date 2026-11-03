//! Classes: compiled declarations and linked runtime class entries.

use crate::func::{Func, Method, UserFunction};
use crate::opcode::OpArray;
use crate::types::TypeDecl;
use php_runtime::{Value, ZStr};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

/// Class flags (`ZEND_ACC_*` subset) stored in [`Class::flags`].
pub mod cflags {
    /// `abstract class` (explicit) or class with abstract methods.
    pub const ABSTRACT: u32 = 1 << 0;
    /// `final class`
    pub const FINAL: u32 = 1 << 1;
    /// `interface`
    pub const INTERFACE: u32 = 1 << 2;
    /// `trait`
    pub const TRAIT: u32 = 1 << 3;
    /// `enum`
    pub const ENUM: u32 = 1 << 4;
    /// `readonly class`
    pub const READONLY: u32 = 1 << 5;
    /// Anonymous class.
    pub const ANON: u32 = 1 << 6;
    /// `#[AllowDynamicProperties]` (or inherited / stdClass).
    pub const ALLOW_DYNAMIC: u32 = 1 << 7;
    /// Internal (builtin) class.
    pub const INTERNAL: u32 = 1 << 8;
    /// Instances cannot be created with `new` (Closure, Generator...).
    pub const NO_NEW: u32 = 1 << 9;
    /// Cannot be serialized.
    pub const NOT_SERIALIZABLE: u32 = 1 << 10;
    /// Explicitly declared `abstract`.
    pub const EXPLICIT_ABSTRACT: u32 = 1 << 11;
    /// Has `#[\Deprecated]`.
    pub const DEPRECATED: u32 = 1 << 12;
    /// Cannot be cloned.
    pub const NO_CLONE: u32 = 1 << 13;
}

/// An attribute applied to a declaration (for Reflection).
#[derive(Clone, Debug)]
pub struct AttributeInfo {
    /// Resolved attribute class name.
    pub name: ZStr,
    /// Arguments: (name for named args, value expression).
    pub args: Vec<(Option<ZStr>, ConstExpr)>,
    /// Source line.
    pub line: u32,
}

/// A constant expression: a literal value or code that computes it (run in
/// the scope of the declaring class).
#[derive(Clone)]
pub enum ConstExpr {
    /// Already a value.
    Value(Value),
    /// Code computing the value (ends with `Return`).
    Code(Rc<OpArray>),
}

impl fmt::Debug for ConstExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConstExpr::Value(v) => write!(f, "Value({v:?})"),
            ConstExpr::Code(_) => f.write_str("Code"),
        }
    }
}

/// A compiled class constant declaration.
#[derive(Clone, Debug)]
pub struct ConstDecl {
    /// Name.
    pub name: ZStr,
    /// Value.
    pub value: ConstExpr,
    /// Visibility and `final` flags.
    pub flags: u32,
    /// Declared type.
    pub ty: Option<TypeDecl>,
    /// Doc comment.
    pub doc_comment: Option<ZStr>,
    /// Attributes.
    pub attributes: Vec<AttributeInfo>,
    /// Line.
    pub line: u32,
}

/// Property hooks (`get`/`set`).
#[derive(Clone, Debug, Default)]
pub struct PropHooks {
    /// The `get` hook.
    pub get: Option<Rc<UserFunction>>,
    /// The `set` hook.
    pub set: Option<Rc<UserFunction>>,
    /// `final` flags of the hooks (get, set).
    pub final_get: bool,
    /// `final` on the set hook.
    pub final_set: bool,
    /// Whether the hooks are abstract (interface/abstract property).
    pub abstract_get: bool,
    /// Abstract set hook.
    pub abstract_set: bool,
    /// Class that declares the hooks.
    pub class: ZStr,
}

/// A compiled property declaration.
#[derive(Clone, Debug)]
pub struct PropDecl {
    /// Name.
    pub name: ZStr,
    /// Default value (`None`: no default — uninitialized when typed, `null`
    /// otherwise).
    pub default: Option<ConstExpr>,
    /// Flags (visibility, static, readonly).
    pub flags: u32,
    /// Declared type.
    pub ty: Option<TypeDecl>,
    /// Doc comment.
    pub doc_comment: Option<ZStr>,
    /// Attributes.
    pub attributes: Vec<AttributeInfo>,
    /// Line.
    pub line: u32,
    /// Property hooks.
    pub hooks: Option<Rc<PropHooks>>,
}

/// A compiled method declaration.
#[derive(Clone, Debug)]
pub struct MethodDecl {
    /// Name as written.
    pub name: ZStr,
    /// The compiled function.
    pub func: Rc<UserFunction>,
    /// Flags.
    pub flags: u32,
}

/// A trait adaptation rule.
#[derive(Clone, Debug)]
pub enum TraitRule {
    /// `T::m insteadof A, B`
    Precedence {
        /// Trait name.
        trait_name: ZStr,
        /// Method.
        method: ZStr,
        /// Excluded traits.
        insteadof: Vec<ZStr>,
    },
    /// `[T::]m as [visibility] [alias]`
    Alias {
        /// Trait name, if given.
        trait_name: Option<ZStr>,
        /// Method.
        method: ZStr,
        /// New flags (visibility/final), 0 for none.
        flags: u32,
        /// New name.
        alias: Option<ZStr>,
    },
}

/// The kind of class-like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClassKind {
    /// `class`
    Class,
    /// `interface`
    Interface,
    /// `trait`
    Trait,
    /// `enum`
    Enum,
}

/// A compiled (unlinked) class declaration.
#[derive(Debug)]
pub struct ClassDecl {
    /// Fully qualified name (generated for anonymous classes).
    pub name: ZStr,
    /// Kind.
    pub kind: ClassKind,
    /// Class flags.
    pub flags: u32,
    /// Parent class name.
    pub parent: Option<ZStr>,
    /// Implemented (or, for interfaces, extended) interfaces.
    pub interfaces: Vec<ZStr>,
    /// Used traits.
    pub traits: Vec<ZStr>,
    /// Trait rules.
    pub trait_rules: Vec<TraitRule>,
    /// Constants.
    pub consts: Vec<ConstDecl>,
    /// Properties.
    pub props: Vec<PropDecl>,
    /// Methods.
    pub methods: Vec<MethodDecl>,
    /// Enum cases (name, value expression), in order.
    pub cases: Vec<(ZStr, Option<ConstExpr>, u32)>,
    /// Enum backing type (`mask::LONG` or `mask::STRING`).
    pub backing: Option<u32>,
    /// Attributes.
    pub attributes: Vec<AttributeInfo>,
    /// Doc comment.
    pub doc_comment: Option<ZStr>,
    /// File.
    pub filename: ZStr,
    /// First line.
    pub line_start: u32,
    /// Last line.
    pub line_end: u32,
}

/// A declared instance property.
#[derive(Clone, Debug)]
pub struct PropInfo {
    /// Name.
    pub name: ZStr,
    /// Slot index in objects.
    pub slot: u32,
    /// Flags (visibility, readonly, static).
    pub flags: u32,
    /// Declared type.
    pub ty: Option<TypeDecl>,
    /// Declaring class name (case preserved).
    pub class: ZStr,
    /// Default value expression.
    pub default: Option<ConstExpr>,
    /// Doc comment.
    pub doc_comment: Option<ZStr>,
    /// Attributes.
    pub attributes: Vec<AttributeInfo>,
    /// Property hooks.
    pub hooks: Option<Rc<PropHooks>>,
}

impl PropInfo {
    /// Whether the property is virtual (hooked, no backing store).
    pub fn is_virtual(&self) -> bool {
        self.flags & crate::func::flags::VIRTUAL != 0
    }
    /// Visibility bits.
    pub fn visibility(&self) -> u32 {
        self.flags & crate::func::flags::VISIBILITY
    }
    /// Whether the property is readonly.
    pub fn is_readonly(&self) -> bool {
        self.flags & crate::func::flags::READONLY != 0
    }
}

/// A static property (storage shared with the parent unless redeclared).
#[derive(Clone, Debug)]
pub struct StaticProp {
    /// Name.
    pub name: ZStr,
    /// Flags.
    pub flags: u32,
    /// Type.
    pub ty: Option<TypeDecl>,
    /// Declaring class name.
    pub class: ZStr,
    /// The storage slot (shared with subclasses that do not redeclare
    /// the property); may hold a `Value::Reference`.
    pub cell: Rc<RefCell<Value>>,
    /// Default expression (evaluated on first use).
    pub default: Option<ConstExpr>,
    /// Whether the default has been evaluated.
    pub initialized: Rc<Cell<bool>>,
    /// Doc comment.
    pub doc_comment: Option<ZStr>,
}

/// A class constant (lazily evaluated).
#[derive(Clone, Debug)]
pub struct ClassConst {
    /// Name.
    pub name: ZStr,
    /// The value once evaluated.
    pub value: RefCell<Option<Value>>,
    /// The expression.
    pub expr: ConstExpr,
    /// Flags (visibility, final).
    pub flags: u32,
    /// Declaring class name.
    pub class: ZStr,
    /// Type.
    pub ty: Option<TypeDecl>,
    /// Whether this is an enum case.
    pub is_case: bool,
    /// Whether evaluation is in progress (recursion detection).
    pub evaluating: Cell<bool>,
    /// Doc comment.
    pub doc_comment: Option<ZStr>,
    /// Attributes.
    pub attributes: Vec<AttributeInfo>,
}

/// Cached magic methods.
#[derive(Clone, Debug, Default)]
pub struct Magic {
    /// `__construct`
    pub construct: Option<Rc<Method>>,
    /// `__destruct`
    pub destruct: Option<Rc<Method>>,
    /// `__get`
    pub get: Option<Rc<Method>>,
    /// `__set`
    pub set: Option<Rc<Method>>,
    /// `__isset`
    pub isset: Option<Rc<Method>>,
    /// `__unset`
    pub unset: Option<Rc<Method>>,
    /// `__call`
    pub call: Option<Rc<Method>>,
    /// `__callStatic`
    pub call_static: Option<Rc<Method>>,
    /// `__toString`
    pub to_string: Option<Rc<Method>>,
    /// `__invoke`
    pub invoke: Option<Rc<Method>>,
    /// `__clone`
    pub clone: Option<Rc<Method>>,
    /// `__debugInfo`
    pub debug_info: Option<Rc<Method>>,
    /// `__serialize`
    pub serialize: Option<Rc<Method>>,
    /// `__unserialize`
    pub unserialize: Option<Rc<Method>>,
    /// `__sleep`
    pub sleep: Option<Rc<Method>>,
    /// `__wakeup`
    pub wakeup: Option<Rc<Method>>,
}

/// Hooks of internal classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InternalKind {
    /// Plain class.
    None,
    /// `Throwable` implementations (set file/line/trace on creation).
    Throwable,
    /// `Closure`.
    Closure,
    /// `Generator`.
    Generator,
}

/// A linked class.
pub struct Class {
    /// Name (case preserved).
    pub name: ZStr,
    /// Lower-case name.
    pub lc_name: ZStr,
    /// Flags (`cflags`).
    pub flags: u32,
    /// Parent class.
    pub parent: Option<Rc<Class>>,
    /// All interfaces implemented (direct and inherited), each once.
    pub interfaces: Vec<Rc<Class>>,
    /// Names of directly used traits.
    pub traits: Vec<Rc<Class>>,
    /// Constants in declaration order (inherited ones included).
    pub consts: Vec<Rc<ClassConst>>,
    /// Constant lookup.
    pub const_map: HashMap<ZStr, usize>,
    /// Instance property infos in slot order (inherited private ones
    /// included).
    pub props: Vec<Rc<PropInfo>>,
    /// Properties accessible by name from this class's scope.
    pub prop_map: HashMap<ZStr, Rc<PropInfo>>,
    /// Static properties.
    pub static_props: Vec<StaticProp>,
    /// Static property lookup.
    pub static_map: HashMap<ZStr, usize>,
    /// Methods by lower-case name.
    pub methods: HashMap<ZStr, Rc<Method>>,
    /// Method order (lower-case names) for `get_class_methods`.
    pub method_order: Vec<ZStr>,
    /// Magic methods.
    pub magic: Magic,
    /// Default slot values, resolved on first instantiation.
    pub defaults: RefCell<Option<Vec<Option<Value>>>>,
    /// Enum backing type mask.
    pub backing: Option<u32>,
    /// Enum case objects by name.
    pub enum_cases: RefCell<HashMap<ZStr, Value>>,
    /// The compiled declaration (user classes).
    pub decl: Option<Rc<ClassDecl>>,
    /// Internal class kind.
    pub internal: InternalKind,
    /// Attributes.
    pub attributes: Vec<AttributeInfo>,
    /// File (user classes).
    pub filename: Option<ZStr>,
    /// Lines.
    pub line_start: u32,
    /// Last line.
    pub line_end: u32,
    /// Doc comment.
    pub doc_comment: Option<ZStr>,
    /// Whether the class has been fully linked (constants checked).
    pub linked: Cell<bool>,
    /// Native object handlers (inherited).
    pub hooks: Option<Rc<crate::ext::ClassHooks>>,
}

impl fmt::Debug for Class {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Class({})", self.name)
    }
}

impl Class {
    /// Whether this is an interface.
    pub fn is_interface(&self) -> bool {
        self.flags & cflags::INTERFACE != 0
    }
    /// Whether this is a trait.
    pub fn is_trait(&self) -> bool {
        self.flags & cflags::TRAIT != 0
    }
    /// Whether this is an enum.
    pub fn is_enum(&self) -> bool {
        self.flags & cflags::ENUM != 0
    }
    /// Whether the class is abstract.
    pub fn is_abstract(&self) -> bool {
        self.flags & cflags::ABSTRACT != 0
    }

    /// `instanceof_function`: whether `self` is `other` or a subclass /
    /// implementation of it.
    pub fn is_subclass_of(self: &Rc<Class>, other: &Class) -> bool {
        if std::ptr::eq(Rc::as_ptr(self), other) {
            return true;
        }
        if other.is_interface() {
            return self.interfaces.iter().any(|i| std::ptr::eq(Rc::as_ptr(i), other));
        }
        let mut c = self.parent.clone();
        while let Some(p) = c {
            if std::ptr::eq(Rc::as_ptr(&p), other) {
                return true;
            }
            c = p.parent.clone();
        }
        false
    }

    /// Whether `self` is `other` or derives from / implements it (by name).
    pub fn instance_of_name(self: &Rc<Class>, lc: &[u8]) -> bool {
        if self.lc_name.as_bytes() == lc {
            return true;
        }
        if self.interfaces.iter().any(|i| i.lc_name.as_bytes() == lc) {
            return true;
        }
        let mut c = self.parent.clone();
        while let Some(p) = c {
            if p.lc_name.as_bytes() == lc {
                return true;
            }
            c = p.parent.clone();
        }
        false
    }

    /// Finds a method by (any case) name.
    pub fn find_method(&self, name: &[u8]) -> Option<&Rc<Method>> {
        if let Some(m) = self.methods.get(name) {
            return Some(m);
        }
        let lc = name.to_ascii_lowercase();
        self.methods.get(lc.as_slice())
    }

    /// Finds a constant.
    pub fn find_const(&self, name: &[u8]) -> Option<&Rc<ClassConst>> {
        self.const_map.get(name).map(|&i| &self.consts[i])
    }

    /// Finds a static property.
    pub fn find_static(&self, name: &[u8]) -> Option<&StaticProp> {
        self.static_map.get(name).map(|&i| &self.static_props[i])
    }

    /// The method as a callable function.
    pub fn method_func(&self, name: &[u8]) -> Option<Func> {
        self.find_method(name).map(|m| m.func.clone())
    }

    /// The parent's name, if any.
    pub fn parent_name(&self) -> Option<&ZStr> {
        self.parent.as_ref().map(|p| &p.name)
    }

    /// Number of instance property slots.
    pub fn num_slots(&self) -> usize {
        self.props.len()
    }
}

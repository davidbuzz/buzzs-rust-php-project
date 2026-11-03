//! Functions: user functions (compiled op arrays) and builtins.

use crate::class::Class;
use crate::opcode::{OpArray, ParamInfo};
use crate::types::TypeDecl;
use crate::vm::{Vm, VmResult};
use php_runtime::{Obj, PhpRef, Value, ZStr};
use std::cell::RefCell;
use std::fmt;
use std::rc::{Rc, Weak};

/// Function flags (`ZEND_ACC_*` subset).
pub mod flags {
    /// `public`
    pub const PUBLIC: u32 = 1 << 0;
    /// `protected`
    pub const PROTECTED: u32 = 1 << 1;
    /// `private`
    pub const PRIVATE: u32 = 1 << 2;
    /// Visibility mask.
    pub const VISIBILITY: u32 = PUBLIC | PROTECTED | PRIVATE;
    /// `static`
    pub const STATIC: u32 = 1 << 4;
    /// `final`
    pub const FINAL: u32 = 1 << 5;
    /// `abstract`
    pub const ABSTRACT: u32 = 1 << 6;
    /// `readonly` (properties/classes)
    pub const READONLY: u32 = 1 << 7;
    /// Closure.
    pub const CLOSURE: u32 = 1 << 8;
    /// Generator.
    pub const GENERATOR: u32 = 1 << 9;
    /// Variadic.
    pub const VARIADIC: u32 = 1 << 10;
    /// Returns by reference.
    pub const RETURN_REF: u32 = 1 << 11;
    /// `#[\Deprecated]`
    pub const DEPRECATED: u32 = 1 << 12;
    /// Arrow function.
    pub const ARROW: u32 = 1 << 13;
    /// Defined in an interface / abstract declaration without body.
    pub const NO_BODY: u32 = 1 << 14;
    /// Uses `func_get_args()` etc. (not tracked).
    pub const CTOR: u32 = 1 << 15;
    /// Trait method alias / copied from trait.
    pub const FROM_TRAIT: u32 = 1 << 16;
    /// Fake closure (`Closure::fromCallable`, first-class callable).
    pub const FAKE_CLOSURE: u32 = 1 << 17;
    /// `#[\NoDiscard]`
    pub const NODISCARD: u32 = 1 << 18;
}

/// A closure `use` binding.
#[derive(Clone, Debug)]
pub struct ClosureUse {
    /// Variable name.
    pub name: ZStr,
    /// By reference.
    pub by_ref: bool,
    /// The CV in the enclosing function (read when the closure is created).
    pub parent_cv: u32,
    /// The CV in the closure body.
    pub cv: u32,
    /// Implicit capture of an arrow function (no warning when undefined).
    pub implicit: bool,
}

/// A compiled user function, method or closure body.
pub struct UserFunction {
    /// Name as declared (namespaced for functions; `{closure}` for closures).
    pub name: ZStr,
    /// Parameters.
    pub params: Vec<ParamInfo>,
    /// Number of required parameters.
    pub required: u32,
    /// Return type.
    pub return_type: Option<TypeDecl>,
    /// `ZEND_ACC_*` flags (visibility etc. for methods).
    pub flags: u32,
    /// The code.
    pub code: Rc<OpArray>,
    /// First line.
    pub line_start: u32,
    /// Last line.
    pub line_end: u32,
    /// Doc comment.
    pub doc_comment: Option<ZStr>,
    /// Static variables (`static $x`), shared by all calls.
    pub statics: RefCell<Vec<Option<PhpRef>>>,
    /// Closure `use` variables.
    pub uses: Vec<ClosureUse>,
    /// Name of the class the function was declared in (methods, closures in
    /// methods), as written.
    pub class_name: Option<ZStr>,
    /// Attributes (name, args) for reflection.
    pub attributes: Vec<crate::class::AttributeInfo>,
}

impl fmt::Debug for UserFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "UserFunction({})", self.name)
    }
}

impl UserFunction {
    /// Whether the function is a generator.
    pub fn is_generator(&self) -> bool {
        self.flags & flags::GENERATOR != 0
    }
    /// Whether the function is a closure.
    pub fn is_closure(&self) -> bool {
        self.flags & flags::CLOSURE != 0
    }
}

/// The handler of a builtin function.
pub type BuiltinFn = fn(&mut Vm, &mut Args) -> VmResult<Value>;

/// A builtin (internal) function or method.
pub struct Builtin {
    /// Name as declared.
    pub name: ZStr,
    /// Parameters (from the stub signature).
    pub params: Vec<ParamInfo>,
    /// Default values of optional parameters (`None` when not a simple
    /// literal; the handler then supplies it).
    pub defaults: Vec<Option<Value>>,
    /// Number of required parameters.
    pub required: u32,
    /// Return type.
    pub return_type: Option<TypeDecl>,
    /// Flags (visibility/static for methods, `VARIADIC`, `RETURN_REF`).
    pub flags: u32,
    /// The handler.
    pub handler: BuiltinFn,
}

impl fmt::Debug for Builtin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Builtin({})", self.name)
    }
}

/// A callable function body.
#[derive(Clone, Debug)]
pub enum Func {
    /// User code.
    User(Rc<UserFunction>),
    /// Rust code.
    Builtin(Rc<Builtin>),
}

impl Func {
    /// The declared name.
    pub fn name(&self) -> &ZStr {
        match self {
            Func::User(u) => &u.name,
            Func::Builtin(b) => &b.name,
        }
    }

    /// The parameters.
    pub fn params(&self) -> &[ParamInfo] {
        match self {
            Func::User(u) => &u.params,
            Func::Builtin(b) => &b.params,
        }
    }

    /// Flags.
    pub fn flags(&self) -> u32 {
        match self {
            Func::User(u) => u.flags,
            Func::Builtin(b) => b.flags,
        }
    }

    /// Number of required parameters.
    pub fn required(&self) -> u32 {
        match self {
            Func::User(u) => u.required,
            Func::Builtin(b) => b.required,
        }
    }

    /// Return type.
    pub fn return_type(&self) -> Option<&TypeDecl> {
        match self {
            Func::User(u) => u.return_type.as_ref(),
            Func::Builtin(b) => b.return_type.as_ref(),
        }
    }

    /// Whether argument `n` (0-based) is passed by reference.
    pub fn arg_by_ref(&self, n: usize) -> bool {
        let p = self.params();
        if let Some(pi) = p.get(n) {
            return pi.by_ref;
        }
        p.last().is_some_and(|l| l.variadic && l.by_ref)
    }

    /// Whether the function is variadic.
    pub fn is_variadic(&self) -> bool {
        self.params().last().is_some_and(|p| p.variadic)
    }

    /// Whether this is user code.
    pub fn is_user(&self) -> bool {
        matches!(self, Func::User(_))
    }

    /// Whether this is a static method.
    pub fn is_static(&self) -> bool {
        self.flags() & flags::STATIC != 0
    }

    /// Identity comparison.
    pub fn ptr_eq(a: &Func, b: &Func) -> bool {
        match (a, b) {
            (Func::User(x), Func::User(y)) => Rc::ptr_eq(x, y),
            (Func::Builtin(x), Func::Builtin(y)) => Rc::ptr_eq(x, y),
            _ => false,
        }
    }
}

/// A method of a class.
#[derive(Debug)]
pub struct Method {
    /// Name as declared (case preserved).
    pub name: ZStr,
    /// The body.
    pub func: Func,
    /// `ZEND_ACC_*` flags.
    pub flags: u32,
    /// The class that declares the method (`scope`).
    pub scope: Weak<Class>,
    /// The class at the root of the prototype chain (for protected access
    /// checks).
    pub proto_scope: Weak<Class>,
}

impl Method {
    /// The declaring class.
    ///
    /// # Panics
    /// Panics if the class was dropped (classes live for the whole request).
    pub fn scope(&self) -> Rc<Class> {
        self.scope.upgrade().expect("method scope class alive")
    }

    /// Visibility flags.
    pub fn visibility(&self) -> u32 {
        self.flags & flags::VISIBILITY
    }

    /// Whether the method is static.
    pub fn is_static(&self) -> bool {
        self.flags & flags::STATIC != 0
    }

    /// Whether the method is abstract.
    pub fn is_abstract(&self) -> bool {
        self.flags & flags::ABSTRACT != 0
    }
}

/// Arguments of a builtin call.
#[derive(Debug, Default)]
pub struct Args {
    /// Positional arguments (by-ref parameters receive `Value::Reference`).
    /// Missing optional arguments skipped by named arguments are filled with
    /// the stub default or `Value::Null` (see [`Args::passed`]).
    pub values: Vec<Value>,
    /// Which positions were actually passed.
    pub passed: Vec<bool>,
    /// Extra named arguments collected by a variadic parameter.
    pub named: Vec<(ZStr, Value)>,
    /// `$this` for methods.
    pub this: Option<Obj>,
    /// The called scope (`static::`).
    pub called: Option<Rc<Class>>,
    /// The function being called.
    pub func: Option<Rc<Builtin>>,
}

impl Args {
    /// Number of arguments.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether no argument was passed.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Argument `n` (dereferenced) if passed.
    pub fn get(&self, n: usize) -> Option<Value> {
        if self.passed.get(n).copied().unwrap_or(false) {
            self.values.get(n).map(Value::deref)
        } else {
            None
        }
    }

    /// Argument `n` (dereferenced), `Null` if missing.
    pub fn at(&self, n: usize) -> Value {
        self.values.get(n).map(Value::deref).unwrap_or_default()
    }

    /// Whether argument `n` was passed.
    pub fn has(&self, n: usize) -> bool {
        self.passed.get(n).copied().unwrap_or(false)
    }

    /// The reference passed for by-ref argument `n`.
    pub fn reference(&self, n: usize) -> Option<PhpRef> {
        match self.values.get(n) {
            Some(Value::Reference(r)) => Some(r.clone()),
            _ => None,
        }
    }

    /// Writes to by-ref argument `n`.
    pub fn set_ref(&self, n: usize, v: Value) {
        if let Some(r) = self.reference(n) {
            r.set(v);
        }
    }

    /// `$this` (panics if missing — only for instance methods).
    pub fn this_obj(&self) -> Obj {
        self.this.clone().expect("instance method called without $this")
    }

    /// The function name (for messages).
    pub fn func_name(&self) -> String {
        self.func
            .as_ref()
            .map_or_else(String::new, |f| f.name.to_string_lossy())
    }
}

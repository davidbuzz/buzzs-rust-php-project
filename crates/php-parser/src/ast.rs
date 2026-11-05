//! The abstract syntax tree.
//!
//! The tree mirrors the node kinds of the Zend engine (`Zend/zend_ast.h`)
//! but uses Rust enums and structs instead of a single untyped node type.
//! Every node carries a [`Span`]: byte offsets into the source and the line
//! number PHP associates with the node (`zend_ast_get_lineno`), which is the
//! line used in compile-time and runtime diagnostics.
//!
//! PHP source text is a byte string, so identifiers and string literals are
//! stored as `Vec<u8>`.

pub use php_lexer::Span;

/// A parsed file (`ZEND_AST_STMT_LIST` at the top level).
#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    /// Source was scanned with Zend multibyte filtering. Consumers must use
    /// semantic AST bytes rather than slicing the original input with spans.
    pub multibyte: bool,
    /// Top-level statements.
    pub statements: Vec<Stmt>,
    /// Byte offset just past `__halt_compiler();` if the file contains it
    /// (the value of `__COMPILER_HALT_OFFSET__`).
    pub halt_compiler_offset: Option<usize>,
    /// Non-fatal diagnostics raised while scanning and parsing, in source
    /// order.
    pub warnings: Vec<Diagnostic>,
    /// Byte offsets of the tokens the grammar accepted as identifiers
    /// although the scanner returned a keyword (`zend_lex_tstring()`:
    /// `Foo::class`, `$a->list()`, `const FOR = 1`, `f(array: 1)`,
    /// `readonly(...)`, ...), in ascending order. `ext/tokenizer` reports
    /// these as `T_STRING` in `TOKEN_PARSE` mode. Only filled when
    /// [`crate::ParseOptions::record_identifier_tokens`] is set.
    pub identifier_tokens: Vec<usize>,
    /// Byte offsets (statement span starts) of `foreach` statements whose
    /// key was written by reference (`foreach ($a as &$k => $v)`), which the
    /// compiler rejects ("Key element cannot be a reference").
    pub foreach_ref_keys: Vec<usize>,
}

/// A non-fatal diagnostic (compile warning or deprecation).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// Severity.
    pub level: DiagnosticLevel,
    /// Message text, without the `Warning: ` prefix.
    pub message: String,
    /// Line PHP reports.
    pub line: u32,
}

/// Severity of a [`Diagnostic`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticLevel {
    /// `E_COMPILE_WARNING`.
    CompileWarning,
    /// `E_DEPRECATED`.
    Deprecated,
    /// `E_WARNING` raised by the compiler.
    Warning,
}

/// An identifier (function, class, constant, member, label or variable
/// name without the `$`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ident {
    /// The identifier bytes, as written.
    pub name: Vec<u8>,
    /// Location.
    pub span: Span,
}

impl Ident {
    /// Creates an identifier.
    pub fn new(name: impl Into<Vec<u8>>, span: Span) -> Self {
        Self {
            name: name.into(),
            span,
        }
    }

    /// The name as (lossy) UTF-8 text.
    pub fn as_str_lossy(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.name)
    }

    /// Case-insensitive ASCII comparison.
    pub fn eq_ignore_case(&self, other: &str) -> bool {
        self.name.eq_ignore_ascii_case(other.as_bytes())
    }
}

/// How a [`Name`] was written (`ZEND_NAME_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NameKind {
    /// `Foo` (`T_STRING`).
    Unqualified,
    /// `Foo\Bar` (`T_NAME_QUALIFIED`).
    Qualified,
    /// `\Foo\Bar` (`T_NAME_FULLY_QUALIFIED`); the leading `\` is not
    /// stored.
    FullyQualified,
    /// `namespace\Foo` (`T_NAME_RELATIVE`); the `namespace\` prefix is not
    /// stored.
    Relative,
}

/// A possibly namespaced name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    /// How the name was written.
    pub kind: NameKind,
    /// The name, without a leading `\` or `namespace\` prefix.
    pub name: Vec<u8>,
    /// Location.
    pub span: Span,
}

impl Name {
    /// Whether the name is fully qualified.
    pub fn is_fully_qualified(&self) -> bool {
        self.kind == NameKind::FullyQualified
    }

    /// Whether the name is a single segment without any prefix.
    pub fn is_unqualified(&self) -> bool {
        self.kind == NameKind::Unqualified
    }

    /// Case-insensitive comparison with an unqualified name.
    pub fn is_special(&self, lower: &str) -> bool {
        self.kind == NameKind::Unqualified && self.name.eq_ignore_ascii_case(lower.as_bytes())
    }

    /// The name as written in the source (with `\` or `namespace\`
    /// prefixes).
    pub fn to_source(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.name.len() + 10);
        match self.kind {
            NameKind::FullyQualified => out.push(b'\\'),
            NameKind::Relative => out.extend_from_slice(b"namespace\\"),
            NameKind::Unqualified | NameKind::Qualified => {}
        }
        out.extend_from_slice(&self.name);
        out
    }
}

/// A group of attributes: `#[A, B(1)]`.
#[derive(Clone, Debug, PartialEq)]
pub struct AttributeGroup {
    /// The attributes of the group.
    pub attributes: Vec<Attribute>,
    /// Location.
    pub span: Span,
}

/// One attribute inside an [`AttributeGroup`].
#[derive(Clone, Debug, PartialEq)]
pub struct Attribute {
    /// Attribute class name.
    pub name: Name,
    /// Arguments, if parentheses were written.
    pub args: Option<ArgList>,
    /// Location.
    pub span: Span,
}

/// Attribute groups attached to a declaration.
pub type Attributes = Vec<AttributeGroup>;

/// Declaration modifier flags (`ZEND_ACC_*` / modifier tokens).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers(pub u32);

impl Modifiers {
    /// No modifiers.
    pub const NONE: Self = Self(0);
    /// `public`
    pub const PUBLIC: Self = Self(1 << 0);
    /// `protected`
    pub const PROTECTED: Self = Self(1 << 1);
    /// `private`
    pub const PRIVATE: Self = Self(1 << 2);
    /// `static`
    pub const STATIC: Self = Self(1 << 3);
    /// `abstract`
    pub const ABSTRACT: Self = Self(1 << 4);
    /// `final`
    pub const FINAL: Self = Self(1 << 5);
    /// `readonly`
    pub const READONLY: Self = Self(1 << 6);
    /// `public(set)`
    pub const PUBLIC_SET: Self = Self(1 << 7);
    /// `protected(set)`
    pub const PROTECTED_SET: Self = Self(1 << 8);
    /// `private(set)`
    pub const PRIVATE_SET: Self = Self(1 << 9);
    /// `var` (property declared with the legacy keyword; implies public).
    pub const VAR: Self = Self(1 << 10);

    /// Visibility flags.
    pub const VISIBILITY: Self = Self(0b111);
    /// Set-visibility flags.
    pub const SET_VISIBILITY: Self = Self(0b11_1000_0000);

    /// Whether all flags of `other` are set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether any flag of `other` is set.
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Whether no flag is set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Union of both flag sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl std::ops::BitOr for Modifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl std::ops::BitOrAssign for Modifiers {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

// ---------------------------------------------------------------------------
// Statements
// ---------------------------------------------------------------------------

/// A statement.
#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    /// What kind of statement this is.
    pub kind: StmtKind,
    /// Location; `span.line` is the line PHP associates with the statement.
    pub span: Span,
}

/// The kinds of statement.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)] // boxing would complicate pattern matching
pub enum StmtKind {
    /// `{ ... }`, also the body of alternative-syntax blocks.
    Block(Vec<Stmt>),
    /// `if`/`elseif`/`else`, in brace or alternative syntax.
    If {
        /// The `if` and `elseif` branches, in order.
        branches: Vec<IfBranch>,
        /// The `else` branch.
        else_branch: Option<Box<Stmt>>,
    },
    /// `while (cond) body`.
    While {
        /// Loop condition.
        cond: Expr,
        /// Loop body.
        body: Box<Stmt>,
    },
    /// `do body while (cond);`.
    DoWhile {
        /// Loop body.
        body: Box<Stmt>,
        /// Loop condition.
        cond: Expr,
    },
    /// `for (init; cond; step) body`.
    For {
        /// Initialisation expressions.
        init: Vec<Expr>,
        /// Condition expressions (the last one decides).
        cond: Vec<Expr>,
        /// Step expressions.
        step: Vec<Expr>,
        /// Loop body.
        body: Box<Stmt>,
    },
    /// `foreach (subject as key => value) body`.
    Foreach {
        /// The iterated expression.
        subject: Expr,
        /// The key target.
        key: Option<Expr>,
        /// The value target (a variable, or a list/array for destructuring).
        value: Expr,
        /// Whether the value is bound by reference (`&$v`).
        by_ref: bool,
        /// Loop body.
        body: Box<Stmt>,
    },
    /// `switch (subject) { cases }`.
    Switch {
        /// The switched-on expression.
        subject: Expr,
        /// The cases, in order.
        cases: Vec<SwitchCase>,
    },
    /// `break n;`
    Break(Option<Expr>),
    /// `continue n;`
    Continue(Option<Expr>),
    /// `return expr;`
    Return(Option<Expr>),
    /// `global $a, $b;` — each element is a [`ExprKind::Var`].
    Global(Vec<Expr>),
    /// `static $a = 1, $b;`
    Static(Vec<StaticVar>),
    /// `echo a, b;` (also `<?=`).
    Echo(Vec<Expr>),
    /// Inline HTML outside of PHP tags.
    InlineHtml(Vec<u8>),
    /// An expression statement.
    Expr(Expr),
    /// `unset($a, $b);`
    Unset(Vec<Expr>),
    /// `declare(ticks=1) body`; `body` is `None` for `declare(...);`.
    Declare {
        /// The directives.
        directives: Vec<ConstElem>,
        /// The body, if any.
        body: Option<Box<Stmt>>,
    },
    /// The empty statement `;`.
    Nop,
    /// `try { } catch (...) { } finally { }`.
    Try {
        /// The protected block.
        body: Vec<Stmt>,
        /// Catch clauses.
        catches: Vec<Catch>,
        /// The finally block.
        finally: Option<Vec<Stmt>>,
    },
    /// `goto label;`
    Goto(Ident),
    /// `label:`
    Label(Ident),
    /// `function name() {}`.
    Function(Box<FunctionDecl>),
    /// A class, interface, trait or enum declaration.
    Class(Box<ClassDecl>),
    /// `const A = 1, B = 2;` at the top level or in a namespace.
    Const {
        /// The constants.
        consts: Vec<ConstElem>,
        /// Attributes (PHP 8.5).
        attributes: Attributes,
    },
    /// `__halt_compiler();`
    HaltCompiler {
        /// Byte offset just after the statement (`__COMPILER_HALT_OFFSET__`).
        offset: usize,
    },
    /// `namespace A;` or `namespace A { }` or `namespace { }`.
    Namespace {
        /// The namespace name (`None` for the global namespace block).
        name: Option<Name>,
        /// The braced body, `None` for the statement form.
        body: Option<Vec<Stmt>>,
    },
    /// `use A\B as C, D;` (also `use function`/`use const`).
    Use {
        /// The kind of symbol imported.
        kind: UseKind,
        /// The imported names.
        uses: Vec<UseItem>,
    },
    /// `use A\{B, C as D};`
    GroupUse {
        /// The kind given after `use` (`use function A\{...}`); for a
        /// mixed group use (`use A\{function f, C}`) this is `None` and
        /// each item carries its own kind.
        kind: Option<UseKind>,
        /// The common prefix.
        prefix: Name,
        /// The imported names, relative to the prefix.
        uses: Vec<UseItem>,
    },
}

/// One `if`/`elseif` branch.
#[derive(Clone, Debug, PartialEq)]
pub struct IfBranch {
    /// The condition.
    pub cond: Expr,
    /// The body.
    pub body: Box<Stmt>,
    /// Location (from the condition).
    pub span: Span,
}

/// A `case`/`default` of a `switch`.
#[derive(Clone, Debug, PartialEq)]
pub struct SwitchCase {
    /// The case value, `None` for `default`.
    pub cond: Option<Expr>,
    /// The statements of the case.
    pub body: Vec<Stmt>,
    /// Whether the case label was terminated by `;` instead of `:`
    /// (deprecated syntax).
    pub semicolon: bool,
    /// Location.
    pub span: Span,
}

/// One `catch` clause.
#[derive(Clone, Debug, PartialEq)]
pub struct Catch {
    /// The caught class names (`A|B`).
    pub types: Vec<Name>,
    /// The variable, without `$` (optional since PHP 8.0).
    pub var: Option<Ident>,
    /// The handler body.
    pub body: Vec<Stmt>,
    /// Location.
    pub span: Span,
}

/// A `static $x = expr` element.
#[derive(Clone, Debug, PartialEq)]
pub struct StaticVar {
    /// The variable name, without `$`.
    pub name: Ident,
    /// The initialiser.
    pub default: Option<Expr>,
    /// Location.
    pub span: Span,
}

/// A `NAME = expr` element of `const` and `declare` (`ZEND_AST_CONST_ELEM`).
#[derive(Clone, Debug, PartialEq)]
pub struct ConstElem {
    /// The constant (or directive) name.
    pub name: Ident,
    /// The value.
    pub value: Expr,
    /// Doc comment written before the element.
    pub doc_comment: Option<Vec<u8>>,
    /// Location.
    pub span: Span,
}

/// Which symbol table a `use` imports into (`ZEND_SYMBOL_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UseKind {
    /// A class, interface, trait, enum or namespace.
    Class,
    /// `use function`.
    Function,
    /// `use const`.
    Const,
}

/// One imported name of a `use` statement.
#[derive(Clone, Debug, PartialEq)]
pub struct UseItem {
    /// The kind; differs from the statement's kind only inside a mixed
    /// group use (`use A\{function f, const C}`).
    pub kind: UseKind,
    /// The imported name.
    pub name: Name,
    /// `as` alias.
    pub alias: Option<Ident>,
    /// Location.
    pub span: Span,
}

// ---------------------------------------------------------------------------
// Functions and classes
// ---------------------------------------------------------------------------

/// A type declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct Type {
    /// The kind of type.
    pub kind: TypeKind,
    /// Location.
    pub span: Span,
}

/// The kinds of type declaration.
#[derive(Clone, Debug, PartialEq)]
pub enum TypeKind {
    /// A named type: a class name or a builtin type name such as `int`,
    /// `string`, `mixed`, `void`, `never`, `self` (resolved by the compiler).
    Named(Name),
    /// `array`
    Array,
    /// `callable`
    Callable,
    /// `static`
    Static,
    /// `?T`
    Nullable(Box<Type>),
    /// `A|B`
    Union(Vec<Type>),
    /// `A&B`
    Intersection(Vec<Type>),
}

/// A function or method parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    /// Attributes.
    pub attributes: Attributes,
    /// Constructor promotion modifiers (empty for ordinary parameters).
    pub modifiers: Modifiers,
    /// Declared type.
    pub ty: Option<Type>,
    /// `&$x`
    pub by_ref: bool,
    /// `...$x`
    pub variadic: bool,
    /// The name, without `$`.
    pub name: Ident,
    /// Default value.
    pub default: Option<Expr>,
    /// Doc comment.
    pub doc_comment: Option<Vec<u8>>,
    /// Property hooks of a promoted property.
    pub hooks: Option<Vec<PropertyHook>>,
    /// Location.
    pub span: Span,
}

/// A named function declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct FunctionDecl {
    /// Attributes.
    pub attributes: Attributes,
    /// Function name.
    pub name: Ident,
    /// Returns by reference (`function &f()`).
    pub by_ref: bool,
    /// Parameters.
    pub params: Vec<Param>,
    /// Return type.
    pub return_type: Option<Type>,
    /// Body.
    pub body: Vec<Stmt>,
    /// Doc comment.
    pub doc_comment: Option<Vec<u8>>,
    /// Whether the body contains `yield` (making it a generator).
    pub is_generator: bool,
    /// Location; `span.line` is the start line.
    pub span: Span,
    /// Line of the closing brace.
    pub end_line: u32,
}

/// A closure (`function () use (...) {}`).
#[derive(Clone, Debug, PartialEq)]
pub struct Closure {
    /// Attributes.
    pub attributes: Attributes,
    /// `static function`.
    pub is_static: bool,
    /// Returns by reference.
    pub by_ref: bool,
    /// Parameters.
    pub params: Vec<Param>,
    /// `use (...)` variables.
    pub uses: Vec<ClosureUse>,
    /// Return type.
    pub return_type: Option<Type>,
    /// Body.
    pub body: Vec<Stmt>,
    /// Doc comment.
    pub doc_comment: Option<Vec<u8>>,
    /// Whether the body contains `yield`.
    pub is_generator: bool,
    /// Location; `span.line` is the start line.
    pub span: Span,
    /// Line of the closing brace.
    pub end_line: u32,
}

/// A variable imported into a closure with `use`.
#[derive(Clone, Debug, PartialEq)]
pub struct ClosureUse {
    /// Variable name, without `$`.
    pub name: Ident,
    /// Imported by reference.
    pub by_ref: bool,
    /// Location.
    pub span: Span,
}

/// An arrow function (`fn ($x) => expr`).
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // independent syntactic flags
pub struct ArrowFn {
    /// Attributes.
    pub attributes: Attributes,
    /// `static fn`.
    pub is_static: bool,
    /// Returns by reference.
    pub by_ref: bool,
    /// Parameters.
    pub params: Vec<Param>,
    /// Return type.
    pub return_type: Option<Type>,
    /// The returned expression.
    pub body: Box<Expr>,
    /// Doc comment.
    pub doc_comment: Option<Vec<u8>>,
    /// Whether the body contains `yield`.
    pub is_generator: bool,
    /// Whether the arrow function was wrapped in parentheses
    /// (`ZEND_PARENTHESIZED_ARROW_FUNC`).
    pub parenthesized: bool,
    /// Location; `span.line` is the start line.
    pub span: Span,
    /// Line on which the body ends.
    pub end_line: u32,
}

/// The kind of class-like declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

/// A class, interface, trait or enum declaration, or an anonymous class.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassDecl {
    /// Attributes.
    pub attributes: Attributes,
    /// Class, interface, trait or enum.
    pub kind: ClassKind,
    /// `abstract`, `final` and `readonly` modifiers.
    pub modifiers: Modifiers,
    /// The name; `None` for anonymous classes.
    pub name: Option<Ident>,
    /// The parent class (`extends`) of a class.
    pub extends: Option<Name>,
    /// Implemented interfaces; for an interface, the interfaces it extends
    /// (stored in the same slot as `zend_ast_decl` does).
    pub implements: Vec<Name>,
    /// Backing type of an enum.
    pub backing_type: Option<Type>,
    /// Members.
    pub members: Vec<ClassMember>,
    /// Doc comment.
    pub doc_comment: Option<Vec<u8>>,
    /// Location; `span.line` is the line of the `class` keyword.
    pub span: Span,
    /// Line of the closing brace.
    pub end_line: u32,
}

/// A member of a class body.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassMember {
    /// What kind of member.
    pub kind: ClassMemberKind,
    /// Location.
    pub span: Span,
}

/// The kinds of class members.
#[derive(Clone, Debug, PartialEq)]
pub enum ClassMemberKind {
    /// A property declaration (`ZEND_AST_PROP_GROUP`).
    Property(PropertyGroup),
    /// A class constant declaration (`ZEND_AST_CLASS_CONST_GROUP`).
    Const(ClassConstGroup),
    /// A method.
    Method(MethodDecl),
    /// `use TraitA, TraitB { ... }`.
    TraitUse(TraitUse),
    /// `case Foo = 1;` in an enum.
    EnumCase(EnumCase),
}

/// A property declaration, possibly declaring several properties.
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyGroup {
    /// Attributes.
    pub attributes: Attributes,
    /// Modifiers (`var` gives [`Modifiers::VAR`] | [`Modifiers::PUBLIC`]).
    pub modifiers: Modifiers,
    /// Declared type.
    pub ty: Option<Type>,
    /// The properties.
    pub props: Vec<PropertyItem>,
}

/// One property of a [`PropertyGroup`].
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyItem {
    /// Name without `$`.
    pub name: Ident,
    /// Default value.
    pub default: Option<Expr>,
    /// Doc comment.
    pub doc_comment: Option<Vec<u8>>,
    /// Property hooks (`{ get; set; }`), `None` if no hook block.
    pub hooks: Option<Vec<PropertyHook>>,
    /// Location.
    pub span: Span,
}

/// A property hook (`get { }` / `set($value) => expr;`).
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyHook {
    /// Attributes.
    pub attributes: Attributes,
    /// Modifiers (only `final` is valid).
    pub modifiers: Modifiers,
    /// `&get`
    pub by_ref: bool,
    /// Hook name (`get` or `set`, validated by the compiler).
    pub name: Ident,
    /// Explicit parameter list.
    pub params: Option<Vec<Param>>,
    /// The body.
    pub body: HookBody,
    /// Doc comment.
    pub doc_comment: Option<Vec<u8>>,
    /// Whether the body contains `yield`.
    pub is_generator: bool,
    /// Location.
    pub span: Span,
    /// Line on which the hook ends.
    pub end_line: u32,
}

/// The body of a [`PropertyHook`].
#[derive(Clone, Debug, PartialEq)]
pub enum HookBody {
    /// `get;` (abstract or interface hook).
    None,
    /// `get { ... }`
    Block(Vec<Stmt>),
    /// `get => expr;`
    Expr(Box<Expr>),
}

/// A class constant declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassConstGroup {
    /// Attributes.
    pub attributes: Attributes,
    /// Modifiers.
    pub modifiers: Modifiers,
    /// Declared type (PHP 8.3).
    pub ty: Option<Type>,
    /// The constants.
    pub consts: Vec<ConstElem>,
}

/// A method declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct MethodDecl {
    /// Attributes.
    pub attributes: Attributes,
    /// Modifiers as written.
    pub modifiers: Modifiers,
    /// Returns by reference.
    pub by_ref: bool,
    /// Method name.
    pub name: Ident,
    /// Parameters.
    pub params: Vec<Param>,
    /// Return type.
    pub return_type: Option<Type>,
    /// Body; `None` for abstract/interface methods.
    pub body: Option<Vec<Stmt>>,
    /// Doc comment.
    pub doc_comment: Option<Vec<u8>>,
    /// Whether the body contains `yield`.
    pub is_generator: bool,
    /// Location; `span.line` is the start line.
    pub span: Span,
    /// Line of the end of the method.
    pub end_line: u32,
}

/// `use A, B { ... }` inside a class.
#[derive(Clone, Debug, PartialEq)]
pub struct TraitUse {
    /// The used traits.
    pub traits: Vec<Name>,
    /// Conflict resolution rules.
    pub adaptations: Vec<TraitAdaptation>,
}

/// A reference to a trait method: `method` or `Trait::method`.
#[derive(Clone, Debug, PartialEq)]
pub struct TraitMethodRef {
    /// The trait name, if given.
    pub trait_name: Option<Name>,
    /// The method name.
    pub method: Ident,
    /// Location.
    pub span: Span,
}

/// One trait adaptation rule.
#[derive(Clone, Debug, PartialEq)]
pub enum TraitAdaptation {
    /// `A::foo insteadof B, C;`
    Precedence {
        /// The method that wins.
        method: TraitMethodRef,
        /// The traits whose method is excluded.
        insteadof: Vec<Name>,
        /// Location.
        span: Span,
    },
    /// `foo as protected bar;`
    Alias {
        /// The aliased method.
        method: TraitMethodRef,
        /// New visibility/flags (only visibility and `final` are allowed).
        modifiers: Modifiers,
        /// The new name.
        alias: Option<Ident>,
        /// Location.
        span: Span,
    },
}

/// `case Name = value;` in an enum.
#[derive(Clone, Debug, PartialEq)]
pub struct EnumCase {
    /// Attributes.
    pub attributes: Attributes,
    /// Case name.
    pub name: Ident,
    /// Backing value.
    pub value: Option<Expr>,
    /// Doc comment.
    pub doc_comment: Option<Vec<u8>>,
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

/// An expression.
#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    /// What kind of expression.
    pub kind: ExprKind,
    /// Location; `span.line` is the line PHP associates with the node.
    pub span: Span,
}

impl Expr {
    /// Creates an expression node.
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Self { kind, span }
    }

    /// Whether the expression is a variable in the grammar's sense
    /// (writable: `$a`, `$a[1]`, `$a->b`, `A::$b`, and calls).
    pub fn is_variable(&self) -> bool {
        matches!(
            self.kind,
            ExprKind::Var(_)
                | ExprKind::Dim { .. }
                | ExprKind::Prop { .. }
                | ExprKind::StaticProp { .. }
                | ExprKind::Call { .. }
                | ExprKind::MethodCall { .. }
                | ExprKind::StaticCall { .. }
        )
    }
}

/// The kinds of expression.
#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    /// Integer literal.
    Int(i64),
    /// Float literal.
    Float(f64),
    /// String literal (escapes already processed).
    String(Vec<u8>),
    /// A string with interpolation: `"a $b c"` or heredoc
    /// (`ZEND_AST_ENCAPS_LIST`).
    Interpolated(Vec<InterpPart>),
    /// A backtick shell command.
    ShellExec(Vec<InterpPart>),
    /// Array literal, or a `list()`/`[]` destructuring target.
    Array {
        /// The items; `None` is an empty slot (only valid in destructuring).
        items: Vec<Option<ArrayItem>>,
        /// How the array was written.
        syntax: ArraySyntax,
    },
    /// A constant (`FOO`, `\A\B`, `true`, `null`, ...).
    Const(Name),
    /// A magic constant such as `__LINE__`.
    MagicConst(MagicConst),
    /// `A::CONST` or `A::{expr}`.
    ClassConst {
        /// The class.
        class: Box<ClassRef>,
        /// The constant name.
        name: MemberName,
    },
    /// `A::class`.
    ClassName(Box<ClassRef>),
    /// A variable: `$a`, `$$a`, `${expr}`.
    Var(VarName),
    /// `$a[index]`; `index` is `None` for `$a[]`.
    Dim {
        /// The base expression.
        base: Box<Expr>,
        /// The offset.
        index: Option<Box<Expr>>,
    },
    /// `$a->b` or `$a?->b`.
    Prop {
        /// The object.
        object: Box<Expr>,
        /// The property name.
        name: MemberName,
        /// `?->`
        nullsafe: bool,
    },
    /// `A::$b`.
    StaticProp {
        /// The class.
        class: Box<ClassRef>,
        /// The property name.
        name: VarName,
        /// Parenthesized: `(A::$b)` (`ZEND_PARENTHESIZED_STATIC_PROP`).
        parenthesized: bool,
    },
    /// A function call.
    Call {
        /// The callee.
        callee: Box<Callee>,
        /// The arguments.
        args: ArgList,
    },
    /// `$a->b()` or `$a?->b()`.
    MethodCall {
        /// The object.
        object: Box<Expr>,
        /// The method name.
        name: MemberName,
        /// The arguments.
        args: ArgList,
        /// `?->`
        nullsafe: bool,
    },
    /// `A::b()`.
    StaticCall {
        /// The class.
        class: Box<ClassRef>,
        /// The method name.
        name: MemberName,
        /// The arguments.
        args: ArgList,
    },
    /// `new A(args)` or `new class {}`.
    New {
        /// The class.
        class: Box<NewClass>,
        /// Constructor arguments (empty when no parentheses were written).
        args: ArgList,
    },
    /// `a = b`.
    Assign {
        /// The target (a variable or a `list()`/`[]` pattern).
        target: Box<Expr>,
        /// The value.
        value: Box<Expr>,
    },
    /// `a =& b`.
    AssignRef {
        /// The target.
        target: Box<Expr>,
        /// The referenced variable.
        value: Box<Expr>,
    },
    /// `a op= b` (including `??=`).
    AssignOp {
        /// The operator.
        op: BinaryOp,
        /// The target.
        target: Box<Expr>,
        /// The value.
        value: Box<Expr>,
    },
    /// A binary operation.
    Binary {
        /// The operator.
        op: BinaryOp,
        /// Left operand.
        left: Box<Expr>,
        /// Right operand.
        right: Box<Expr>,
    },
    /// A prefix unary operation.
    Unary {
        /// The operator.
        op: UnaryOp,
        /// Operand.
        expr: Box<Expr>,
    },
    /// `++$a`, `$a--` etc.
    IncDec {
        /// Which increment/decrement.
        op: IncDecOp,
        /// The variable.
        var: Box<Expr>,
    },
    /// `(int) $a` etc.
    Cast {
        /// Target type.
        kind: CastKind,
        /// Operand.
        expr: Box<Expr>,
    },
    /// `(void) expr` (PHP 8.5, statement level only).
    VoidCast(Box<Expr>),
    /// `isset(a, b)`.
    Isset(Vec<Expr>),
    /// `empty(a)`.
    Empty(Box<Expr>),
    /// `@expr`.
    Silence(Box<Expr>),
    /// `print expr`.
    Print(Box<Expr>),
    /// `include`, `require`, ... and `eval`.
    Include {
        /// Which construct.
        kind: IncludeKind,
        /// The operand.
        expr: Box<Expr>,
    },
    /// `a instanceof B`.
    Instanceof {
        /// The tested expression.
        expr: Box<Expr>,
        /// The class.
        class: Box<ClassRef>,
    },
    /// `a ? b : c` or `a ?: c`.
    Conditional {
        /// Condition.
        cond: Box<Expr>,
        /// Value if true; `None` for the short ternary.
        then: Option<Box<Expr>>,
        /// Value if false.
        else_: Box<Expr>,
        /// Whether the whole expression was in parentheses
        /// (`ZEND_PARENTHESIZED_CONDITIONAL`).
        parenthesized: bool,
    },
    /// `yield`, `yield v`, `yield k => v`.
    Yield {
        /// The key.
        key: Option<Box<Expr>>,
        /// The value.
        value: Option<Box<Expr>>,
    },
    /// `yield from expr`.
    YieldFrom(Box<Expr>),
    /// `throw expr`.
    Throw(Box<Expr>),
    /// A closure.
    Closure(Box<Closure>),
    /// An arrow function.
    ArrowFn(Box<ArrowFn>),
    /// `match (subject) { arms }`.
    Match {
        /// The subject.
        subject: Box<Expr>,
        /// The arms.
        arms: Vec<MatchArm>,
    },
    /// `a |> b` (PHP 8.5).
    Pipe {
        /// The piped value.
        left: Box<Expr>,
        /// The callable.
        right: Box<Expr>,
    },
}

/// A part of an interpolated string.
#[derive(Clone, Debug, PartialEq)]
pub enum InterpPart {
    /// Literal text (escapes already processed).
    Literal(Vec<u8>, Span),
    /// An embedded expression.
    Expr {
        /// The expression.
        expr: Expr,
        /// How it was embedded.
        syntax: InterpSyntax,
    },
}

/// How an expression was embedded in a string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InterpSyntax {
    /// `$a`, `$a[0]`, `$a->b`, `$a?->b`.
    Simple,
    /// `{$expr}`.
    Braced,
    /// `${name}` or `${name[expr]}` (`ZEND_ENCAPS_VAR_DOLLAR_CURLY`,
    /// deprecated).
    DollarCurly,
    /// `${expr}` (`ZEND_ENCAPS_VAR_DOLLAR_CURLY_VAR_VAR`, deprecated).
    DollarCurlyExpr,
}

/// How an array literal was written (`ZEND_ARRAY_SYNTAX_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArraySyntax {
    /// `list(...)`
    List,
    /// `array(...)`
    Long,
    /// `[...]`
    Short,
}

/// An element of an array literal or destructuring pattern.
#[derive(Clone, Debug, PartialEq)]
pub struct ArrayItem {
    /// The key.
    pub key: Option<Expr>,
    /// The value.
    pub value: Expr,
    /// `&$v`
    pub by_ref: bool,
    /// `...$v`
    pub unpack: bool,
    /// Location.
    pub span: Span,
}

/// A magic constant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MagicConst {
    /// `__LINE__`
    Line,
    /// `__FILE__`
    File,
    /// `__DIR__`
    Dir,
    /// `__TRAIT__`
    Trait,
    /// `__METHOD__`
    Method,
    /// `__FUNCTION__`
    Function,
    /// `__PROPERTY__`
    Property,
    /// `__NAMESPACE__`
    Namespace,
    /// `__CLASS__`
    Class,
}

impl MagicConst {
    /// The constant as written.
    pub const fn as_str(self) -> &'static str {
        match self {
            MagicConst::Line => "__LINE__",
            MagicConst::File => "__FILE__",
            MagicConst::Dir => "__DIR__",
            MagicConst::Trait => "__TRAIT__",
            MagicConst::Method => "__METHOD__",
            MagicConst::Function => "__FUNCTION__",
            MagicConst::Property => "__PROPERTY__",
            MagicConst::Namespace => "__NAMESPACE__",
            MagicConst::Class => "__CLASS__",
        }
    }
}

/// The name part of a variable.
#[derive(Clone, Debug, PartialEq)]
pub enum VarName {
    /// `$name`
    Name(Ident),
    /// `$$a` or `${expr}`: the name is computed at runtime.
    Expr(Box<Expr>),
}

/// The name of a property, method or class constant.
#[derive(Clone, Debug, PartialEq)]
pub enum MemberName {
    /// A literal identifier.
    Ident(Ident),
    /// `{expr}` or `$var`: computed at runtime.
    Expr(Box<Expr>),
}

/// The class part of `A::x`, `instanceof A` etc.
#[derive(Clone, Debug, PartialEq)]
pub enum ClassRef {
    /// A class name, including `self`, `parent` and `static`.
    Name(Name),
    /// A dynamic class (`$obj::x`, `(expr)::x`).
    Expr(Expr),
}

impl ClassRef {
    /// Whether `class::name(...)` is a call of a parent property hook,
    /// `parent::$prop::get()` / `parent::$prop::set()`
    /// (`zend_ast_is_parent_hook_call`).
    pub fn is_parent_hook_call(&self, name: &MemberName) -> bool {
        let ClassRef::Expr(e) = self else {
            return false;
        };
        let ExprKind::StaticProp {
            class,
            name: VarName::Name(_),
            parenthesized: false,
        } = &e.kind
        else {
            return false;
        };
        let ClassRef::Name(parent) = class.as_ref() else {
            return false;
        };
        parent.is_special("parent")
            && matches!(name, MemberName::Ident(m) if m.eq_ignore_case("get") || m.eq_ignore_case("set"))
    }
}

/// The callee of a function call.
#[derive(Clone, Debug, PartialEq)]
pub enum Callee {
    /// A function name.
    Name(Name),
    /// A dynamic callee (`$f()`, `(expr)()`).
    Expr(Expr),
}

/// The class of a `new` expression.
#[derive(Clone, Debug, PartialEq)]
pub enum NewClass {
    /// `new A`
    Name(Name),
    /// `new $a`, `new (expr)`
    Expr(Expr),
    /// `new class {}`
    Anonymous(ClassDecl),
}

/// An argument list.
#[derive(Clone, Debug, PartialEq)]
pub struct ArgList {
    /// The arguments.
    pub args: Vec<Arg>,
    /// Location (parentheses included).
    pub span: Span,
}

impl ArgList {
    /// Whether the call is a first-class callable creation or partial
    /// application (`f(...)`, `f(?, 1)`), i.e. contains a placeholder.
    pub fn is_partial(&self) -> bool {
        self.args.iter().any(|a| {
            matches!(
                a.kind,
                ArgKind::Placeholder | ArgKind::VariadicPlaceholder | ArgKind::NamedPlaceholder(_)
            )
        })
    }

    /// Whether this is exactly `(...)` (first-class callable syntax).
    pub fn is_first_class_callable(&self) -> bool {
        self.args.len() == 1 && self.args[0].kind == ArgKind::VariadicPlaceholder
    }
}

/// A call argument.
#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    /// What kind of argument.
    pub kind: ArgKind,
    /// Location.
    pub span: Span,
}

/// The kinds of argument.
#[derive(Clone, Debug, PartialEq)]
pub enum ArgKind {
    /// `expr`
    Positional(Expr),
    /// `...expr`
    Unpack(Expr),
    /// `name: expr`
    Named {
        /// Parameter name.
        name: Ident,
        /// Value.
        value: Expr,
    },
    /// `?` (partial function application).
    Placeholder,
    /// `...` (first-class callable syntax).
    VariadicPlaceholder,
    /// `name: ?`
    NamedPlaceholder(Ident),
}

/// An arm of a `match` expression.
#[derive(Clone, Debug, PartialEq)]
pub struct MatchArm {
    /// The conditions; `None` for `default`.
    pub conds: Option<Vec<Expr>>,
    /// The result expression.
    pub body: Expr,
    /// Location.
    pub span: Span,
}

/// Binary operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Mod,
    /// `**`
    Pow,
    /// `.`
    Concat,
    /// `<<`
    ShiftLeft,
    /// `>>`
    ShiftRight,
    /// `|`
    BitOr,
    /// `&`
    BitAnd,
    /// `^`
    BitXor,
    /// `&&` and `and`
    BoolAnd,
    /// `||` and `or`
    BoolOr,
    /// `xor`
    BoolXor,
    /// `===`
    Identical,
    /// `!==`
    NotIdentical,
    /// `==`
    Equal,
    /// `!=` and `<>`
    NotEqual,
    /// `<`
    Less,
    /// `<=`
    LessEqual,
    /// `>`
    Greater,
    /// `>=`
    GreaterEqual,
    /// `<=>`
    Spaceship,
    /// `??`
    Coalesce,
}

impl BinaryOp {
    /// The operator as written (canonical form).
    pub const fn as_str(self) -> &'static str {
        match self {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Mod => "%",
            BinaryOp::Pow => "**",
            BinaryOp::Concat => ".",
            BinaryOp::ShiftLeft => "<<",
            BinaryOp::ShiftRight => ">>",
            BinaryOp::BitOr => "|",
            BinaryOp::BitAnd => "&",
            BinaryOp::BitXor => "^",
            BinaryOp::BoolAnd => "&&",
            BinaryOp::BoolOr => "||",
            BinaryOp::BoolXor => "xor",
            BinaryOp::Identical => "===",
            BinaryOp::NotIdentical => "!==",
            BinaryOp::Equal => "==",
            BinaryOp::NotEqual => "!=",
            BinaryOp::Less => "<",
            BinaryOp::LessEqual => "<=",
            BinaryOp::Greater => ">",
            BinaryOp::GreaterEqual => ">=",
            BinaryOp::Spaceship => "<=>",
            BinaryOp::Coalesce => "??",
        }
    }
}

/// Prefix unary operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// `+a`
    Plus,
    /// `-a`
    Minus,
    /// `!a`
    Not,
    /// `~a`
    BitNot,
}

/// Increment and decrement operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IncDecOp {
    /// `++$a`
    PreInc,
    /// `--$a`
    PreDec,
    /// `$a++`
    PostInc,
    /// `$a--`
    PostDec,
}

/// Cast target types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CastKind {
    /// `(int)`
    Int,
    /// `(float)`
    Float,
    /// `(string)`
    String,
    /// `(array)`
    Array,
    /// `(object)`
    Object,
    /// `(bool)`
    Bool,
    /// `(unset)` (removed in PHP 8, still parsed)
    Unset,
}

/// `include`-like constructs (`ZEND_AST_INCLUDE_OR_EVAL`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IncludeKind {
    /// `include`
    Include,
    /// `include_once`
    IncludeOnce,
    /// `require`
    Require,
    /// `require_once`
    RequireOnce,
    /// `eval`
    Eval,
}

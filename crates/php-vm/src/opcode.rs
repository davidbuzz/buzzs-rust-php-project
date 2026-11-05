//! The compiled form of PHP code: opcode arrays.
//!
//! Like Zend's `zend_op_array`, every function, method, closure and file is
//! compiled to a flat array of [`Op`]s that address three kinds of operands:
//! compiled variables (`CV`, the function's named locals), temporaries
//! (`TMP`, single-assignment registers that are *moved out* when read so
//! that values are released as early as PHP releases them) and literals.
//!
//! Writable expressions (`$a[1]->b[]`) are compiled to a [`Place`]: a base
//! plus a path of dimension/property segments whose offsets are evaluated
//! into temporaries beforehand. The executor walks a place in one step,
//! which mirrors Zend's `FETCH_*_W` + `ASSIGN_DIM`/`ASSIGN_OBJ` sequences
//! without holding pointers across opcodes.

use crate::types::TypeDecl;
use php_runtime::{BinaryOp, Value, ZStr};
use std::cell::RefCell;
use std::rc::Rc;

/// An index into [`OpArray::ops`].
pub type Label = u32;

/// An instruction operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    /// A compiled variable (read: warns "Undefined variable" when unset).
    Cv(u32),
    /// A temporary, consumed by the read.
    Tmp(u32),
    /// A temporary that is read without being consumed (switch subjects).
    TmpKeep(u32),
    /// A literal from [`OpArray::literals`].
    Lit(u32),
    /// No operand.
    Unused,
}

/// How a class is named by an instruction.
#[derive(Clone, Debug, PartialEq)]
pub enum ClassSpec {
    /// A class name (resolved, case preserved), index into literals.
    Named(u32),
    /// `self`
    SelfClass,
    /// `parent`
    Parent,
    /// `static`
    Static,
    /// A dynamic class: an object or a class-name string.
    Dyn(Operand),
}

/// The base of a [`Place`].
#[derive(Clone, Debug, PartialEq)]
pub enum PlaceBase {
    /// A compiled variable.
    Cv(u32),
    /// A variable variable (`$$a`, `${expr}`); the operand evaluates to the
    /// name.
    VarVar(Operand),
    /// `$this`.
    This,
    /// `A::$b`; the operand evaluates to the property name.
    StaticProp {
        /// The class.
        class: ClassSpec,
        /// The property name.
        name: Operand,
    },
    /// A temporary value (`f()[0]`, `(new A)->x`).
    Value(Operand),
    /// A global variable (`$GLOBALS['name']`, superglobals); the operand is
    /// the name.
    Global(Operand),
    /// `$GLOBALS` itself (read-only copy).
    GlobalsArray,
}

/// One step of a [`Place`] path.
#[derive(Clone, Debug, PartialEq)]
pub enum Seg {
    /// `[offset]`
    Dim(Operand),
    /// `[]`
    Append,
    /// `->name`
    Prop(Operand),
}

/// A writable location.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    /// Where the path starts.
    pub base: PlaceBase,
    /// The access path.
    pub segs: Vec<Seg>,
}

/// Index into [`OpArray::places`].
pub type PlaceId = u32;

/// Binary operations of [`Op::Bin`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinKind {
    /// An arithmetic/bitwise/string operator implemented by `php_runtime::ops`.
    Arith(BinaryOp),
    /// `==`
    Equal,
    /// `!=`
    NotEqual,
    /// `===`
    Identical,
    /// `!==`
    NotIdentical,
    /// `<`
    Less,
    /// `<=`
    LessEqual,
    /// `<=>`
    Spaceship,
}

/// Unary operations of [`Op::Unary`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnKind {
    /// `-a`
    Neg,
    /// `+a`
    Plus,
    /// `!a`
    Not,
    /// `~a`
    BitNot,
    /// `(bool) a`
    Bool,
}

/// Cast kinds of [`Op::Cast`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    /// `(unset)` — evaluates to null.
    Null,
}

/// Increment/decrement kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncDecKind {
    /// `++$a`
    PreInc,
    /// `--$a`
    PreDec,
    /// `$a++`
    PostInc,
    /// `$a--`
    PostDec,
}

impl IncDecKind {
    /// Whether this is an increment.
    pub fn is_inc(self) -> bool {
        matches!(self, IncDecKind::PreInc | IncDecKind::PostInc)
    }
    /// Whether the result is the old value.
    pub fn is_post(self) -> bool {
        matches!(self, IncDecKind::PostInc | IncDecKind::PostDec)
    }
}

/// Fetch modes for reading a place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchMode {
    /// Normal read, emitting "Undefined ..." warnings.
    Read,
    /// Silent read (`??`, `isset`).
    Is,
    /// `list()` element fetch (non-arrays give `null` silently).
    List,
}

/// `include`-like constructs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// Where a call argument comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum ArgSrc {
    /// A value (by-ref parameters raise an error).
    Val(Operand),
    /// The result of a function call (by-ref parameters get a notice).
    CallResult(Operand),
    /// A variable: passed by reference if the parameter is by-ref.
    Place(PlaceId),
    /// A dimension or property of a temporary expression (`[0, 1][0]`):
    /// by value only; a by-ref parameter throws "Cannot use temporary
    /// expression in write context" (`FETCH_DIM/OBJ_FUNC_ARG` on a TMP).
    TempPlace(PlaceId),
}

/// The kind of a `foreach` loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForeachKind {
    /// By value.
    Value,
    /// By reference.
    Ref,
}

/// An instruction.
#[derive(Clone, Debug)]
pub enum Op {
    /// Does nothing.
    Nop,
    /// Copies (dereferenced) `src` into `dst`.
    Move { src: Operand, dst: u32 },
    /// Binary operation.
    Bin {
        op: BinKind,
        a: Operand,
        b: Operand,
        dst: u32,
    },
    /// Unary operation.
    Unary { op: UnKind, a: Operand, dst: u32 },
    /// Cast.
    Cast {
        kind: CastKind,
        a: Operand,
        dst: u32,
    },
    /// String interpolation: concatenates the parts.
    Rope { parts: Box<[Operand]>, dst: u32 },
    /// Starts an array literal in `dst`.
    InitArray { dst: u32 },
    /// Adds an element to the array in `arr`.
    ArrayAdd {
        arr: u32,
        key: Option<Operand>,
        val: Operand,
    },
    /// Adds a reference element to the array in `arr`.
    ArrayAddRef {
        arr: u32,
        key: Option<Operand>,
        place: PlaceId,
    },
    /// Spreads `src` into the array in `arr` (`...$x`).
    ArrayUnpack { arr: u32, src: Operand },
    /// `...$x` inside a constant expression: only arrays may be spread
    /// (`zend_ast_add_unpacked_element`, `Zend/zend_ast.c`).
    ConstArrayUnpack { arr: u32, src: Operand },

    /// Unconditional jump.
    Jmp(Label),
    /// Jump if falsy.
    JmpZ(Operand, Label),
    /// Jump if truthy.
    JmpNz(Operand, Label),
    /// `&&`/`and`: if falsy, `dst = false` and jump.
    JmpZEx { a: Operand, dst: u32, target: Label },
    /// `||`/`or`: if truthy, `dst = true` and jump.
    JmpNzEx { a: Operand, dst: u32, target: Label },
    /// `?:`: if truthy, `dst = a` and jump.
    JmpSet { a: Operand, dst: u32, target: Label },
    /// `??`: if not null, `dst = a` and jump.
    Coalesce { a: Operand, dst: u32, target: Label },
    /// Nullsafe: if null, `dst = null` and jump.
    JmpNull { a: Operand, dst: u32, target: Label },

    /// `$cv = val`.
    AssignCv {
        cv: u32,
        val: Operand,
        dst: Option<u32>,
    },
    /// `place = val`.
    Assign {
        place: PlaceId,
        val: Operand,
        dst: Option<u32>,
    },
    /// `place op= val`.
    AssignOp {
        place: PlaceId,
        op: BinaryOp,
        val: Operand,
        dst: Option<u32>,
    },
    /// `++place` etc.
    IncDec {
        place: PlaceId,
        kind: IncDecKind,
        dst: Option<u32>,
    },
    /// `place =& src` where `src` holds a reference.
    AssignRef {
        place: PlaceId,
        src: Operand,
        dst: Option<u32>,
    },
    /// Makes `place` a reference and stores it in `dst`.
    MakeRef { place: PlaceId, dst: u32 },
    /// `ZEND_FETCH_LIST_W` (+ `ZEND_MAKE_REF` into `dst`) of a destructuring
    /// element whose source is a call result: unless the result is a
    /// reference, notices and reads the element by value instead.
    ListFetchRef { place: PlaceId, dst: Option<u32> },
    /// Reads a place.
    Fetch {
        place: PlaceId,
        mode: FetchMode,
        dst: u32,
    },
    /// `isset(place)`.
    Isset { place: PlaceId, dst: u32 },
    /// `empty(place)`.
    Empty { place: PlaceId, dst: u32 },
    /// Finishes a pending call with a literal nonrepresentable float key.
    /// Only a native call with a defined CV array avoids copying that array.
    ArrayKeyExists { array: ArgSrc, dst: Option<u32> },
    /// `unset(place)`.
    Unset { place: PlaceId },
    /// `global $name` bound to `cv`.
    Global { cv: u32, name: u32 },
    /// `static $x`: binds `cv` to static slot `idx`; jumps to `skip` when
    /// already initialised.
    BindStatic { cv: u32, idx: u32, skip: Label },
    /// Initialises static slot `idx` with `val` and binds `cv`.
    InitStatic { cv: u32, idx: u32, val: Operand },
    /// Reads `$this` (error if not in object context).
    FetchThis { dst: u32 },

    /// Starts a call to a named function (`name` and `fallback` are
    /// lower-cased literal names).
    InitFCall {
        name: u32,
        fallback: Option<u32>,
        display: u32,
    },
    /// Starts a call to a callable value.
    InitDynCall { callee: Operand },
    /// Starts a method call.
    InitMethodCall { obj: Operand, name: Operand },
    /// Starts a static method call.
    InitStaticCall { class: ClassSpec, name: Operand },
    /// `parent::$prop::get()` / `parent::$prop::set()`.
    InitParentHook { prop: u32, set: bool },
    /// Creates an object and starts its constructor call; jumps to `skip`
    /// if the class has no constructor.
    New {
        class: ClassSpec,
        dst: u32,
        skip: Label,
    },
    /// Passes a positional argument.
    Send { src: ArgSrc },
    /// Passes `...$args`.
    SendUnpack { src: Operand },
    /// Passes a named argument.
    SendNamed { name: u32, src: ArgSrc },
    /// Performs the pending call.
    DoCall { dst: Option<u32> },
    /// Turns the pending call into a closure (`f(...)`).
    CallableConvert {
        dst: u32,
        /// Compiled in a constant expression, where a `__callStatic`
        /// trampoline is an `Error` (`zend_ast_evaluate_inner()`).
        const_expr: bool,
    },
    /// `ZEND_SEND_PLACEHOLDER` (`?` or `name: ?`) of a partial application.
    SendPlaceholder { name: Option<u32> },
    /// `ZEND_CALLABLE_CONVERT_PARTIAL`: turns the pending call into a
    /// Closure. `const_args`: bitmask of literal positional arguments;
    /// `name`: literal index of the `{closure:pfa:...}` name.
    CallablePartial {
        dst: u32,
        variadic: bool,
        const_args: u32,
        name: u32,
    },

    /// Receives required argument `n` into `cv`.
    Recv { n: u32, cv: u32 },
    /// Receives optional argument `n`; jumps to `skip` when it was passed,
    /// otherwise falls through to code computing the default.
    RecvInit { n: u32, cv: u32, skip: Label },
    /// Stores a computed default value for parameter `n`.
    RecvDefault { n: u32, cv: u32, val: Operand },
    /// Receives the variadic rest.
    RecvVariadic { n: u32, cv: u32 },
    /// Checks/coerces the return value in `val` (a temporary) against the
    /// function's return type.
    VerifyReturn { val: Operand },
    /// Returns from the function.
    Return { val: Operand },
    /// Returns a reference to `place` (by-ref functions).
    ReturnRef { place: PlaceId },
    /// `return expr;` in a by-reference function where `expr` is not a
    /// variable: references pass through, values get the
    /// "Only variable references should be returned by reference" notice.
    ReturnByRefValue { val: Operand },
    /// Suspends the call frame into a new `Generator`.
    GeneratorCreate,
    /// `yield`.
    Yield {
        key: Option<Operand>,
        val: Option<Operand>,
        dst: Option<u32>,
    },
    /// `yield` of a reference (by-ref generators).
    YieldRef {
        key: Option<Operand>,
        place: PlaceId,
        dst: Option<u32>,
    },
    /// `yield expr` in a by-reference generator where `expr` is not a
    /// variable (`ZEND_YIELD` with a CONST/TMP operand, or a call result
    /// flagged `ZEND_RETURNS_FUNCTION` when `call`): raises the
    /// "Only variable references should be yielded by reference" notice
    /// unless `call` and the call returned a reference.
    YieldByRefValue {
        key: Option<Operand>,
        val: Operand,
        call: bool,
        dst: Option<u32>,
    },
    /// `yield from`.
    YieldFrom { src: Operand, dst: Option<u32> },

    /// Declares a (conditionally defined) function.
    DeclareFunction(u32),
    /// Declares a class.
    DeclareClass(u32),
    /// Declares an anonymous class and stores its name... (the class) in `dst`.
    DeclareAnonClass { idx: u32, dst: u32 },
    /// Creates a closure object.
    MakeClosure { idx: u32, dst: u32 },
    /// `const NAME = val;`
    /// `attrs`: index into [`DeclTable::const_attributes`] (`u32::MAX`
    /// without attributes).
    DeclareConst { name: u32, val: Operand, attrs: u32 },

    /// Reads a constant; `fallback` is the global name for unqualified
    /// namespaced constants.
    FetchConst {
        name: u32,
        fallback: Option<u32>,
        dst: u32,
    },
    /// `A::NAME`
    FetchClassConst {
        class: ClassSpec,
        name: Operand,
        dst: u32,
    },
    /// `A::class`
    FetchClassName {
        class: ClassSpec,
        dst: u32,
        /// Compiled in an evaluated constant expression (a global `const`
        /// value or a parameter default): with no class scope the error is
        /// `ZEND_AST_CLASS_NAME`'s "Cannot use \"self\" when no class scope
        /// is active" (`zend_ast_evaluate_inner()`), not
        /// `ZEND_FETCH_CLASS_NAME`'s "... in the global scope".
        const_expr: bool,
    },
    /// `a instanceof B`
    InstanceOf {
        a: Operand,
        class: ClassSpec,
        dst: u32,
    },
    /// `clone a`
    Clone { a: Operand, dst: u32 },
    /// `func_num_args()` called by name, without arguments, in a function
    /// body (`ZEND_FUNC_NUM_ARGS`): no call frame, no observer frame.
    FuncNumArgs { dst: Option<u32> },
    /// `func_get_args()` likewise (`ZEND_FUNC_GET_ARGS`).
    FuncGetArgs { dst: Option<u32> },

    /// `echo`
    Echo(Operand),
    /// `exit`/`die`
    Exit(Operand),
    /// Discards a temporary.
    Free(Operand),
    /// `throw`
    Throw(Operand),
    /// Starts a catch clause: if the pending exception is an instance of
    /// one of the classes (literal names), binds it to `cv` and continues;
    /// otherwise jumps to `next`.
    Catch {
        classes: Box<[u32]>,
        cv: Option<u32>,
        next: Label,
    },
    /// Re-raises the pending exception (no catch clause matched).
    Rethrow,
    /// Runs finally block `fin` at `target`, then returns here.
    FastCall { fin: u32, target: Label },
    /// End of finally block `fin`.
    FastRet { fin: u32 },
    /// Discards the pending state of finally block `fin` (jumping out of a
    /// finally block).
    DiscardFinally { fin: u32 },
    /// Starts `@`: saves `error_reporting` into `dst`.
    BeginSilence { dst: u32 },
    /// Ends `@`.
    EndSilence { saved: Operand },
    /// `include`/`require`/`eval`.
    Include {
        kind: IncludeKind,
        a: Operand,
        dst: u32,
    },
    /// Starts a foreach loop over `subject` (or the place, by reference);
    /// jumps to `end` when empty.
    FeReset {
        subject: Operand,
        place: Option<PlaceId>,
        kind: ForeachKind,
        iter: u32,
        end: Label,
    },
    /// Fetches the next element; jumps to `end` when done.
    FeFetch {
        iter: u32,
        end: Label,
        val: u32,
        key: Option<u32>,
    },
    /// Releases a foreach iterator.
    FeFree { iter: u32 },
    /// `FE_FREE` emitted by `return` (`ZEND_FREE_ON_RETURN`): an exception
    /// from the freed iterator is dispatched from the loop (`loop_at`),
    /// not from inside the try blocks the `return` is in.
    FeFreeOnReturn { iter: u32, loop_at: u32 },
    /// Throws `UnhandledMatchError` for the subject.
    MatchError { subject: Operand },
    /// `ZEND_TYPE_ASSERT` of an inlined internal call: throws a `TypeError`
    /// "`msg`, X given" unless `val` is an array.
    TypeAssertArray { val: Operand, msg: u32 },
    /// `declare(ticks=N)` tick.
    /// `ZEND_TICKS` with N of `declare(ticks=N)`.
    Ticks(u32),
    /// Emits a compile-time-style diagnostic at runtime (deferred warnings).
    Diag { level: u8, msg: u32 },
    /// `assert()`: when assertions are disabled at runtime, stores `true`
    /// in `dst` and jumps to `skip`.
    AssertCheck { dst: u32, skip: Label },
    /// The checks of `ZEND_AST_PROP` in `zend_ast_evaluate_ex`: converts
    /// `name` to a string (into `dst`), then throws when the (kept) `obj`
    /// of a constant-expression property fetch is a non-enum object.
    ConstPropCheck {
        obj: Operand,
        name: Operand,
        dst: u32,
    },
}

/// A temporary that an exception thrown by an op in `start..end` releases
/// before the handler runs (`zend_live_range` / `cleanup_live_vars`,
/// `Zend/zend_execute.c`). Only recorded where the release is observable:
/// the values of a list assignment from an array literal.
#[derive(Clone, Copy, Debug)]
pub struct LiveTmp {
    /// First op at which the temporary holds its value.
    pub start: Label,
    /// One past the op that consumes it.
    pub end: Label,
    /// The temporary.
    pub tmp: u32,
}

/// A try/catch/finally region.
#[derive(Clone, Debug, Default)]
pub struct TryEntry {
    /// First op of the try body.
    pub try_start: Label,
    /// One past the last op of the try body.
    pub try_end: Label,
    /// First op of the catch clauses (if any).
    pub catch_start: Option<Label>,
    /// One past the catch clauses.
    pub catch_end: Label,
    /// The finally block, if any.
    pub finally: Option<(u32, Label, Label)>,
    /// The temporaries the try body allocates (`start..end`): dead once a
    /// catch clause of this entry takes over (`cleanup_live_vars`).
    pub try_tmps: (u32, u32),
}

/// A literal table entry.
pub type Literal = Value;

/// Bit in [`ParamInfo::promote`] of native parameters declared
/// `@prefer-ref` (`ZEND_ARG_SEND_PREFER_REF`): sent by reference when the
/// argument is a variable, by value otherwise.
pub const PREFER_REF: u32 = 1 << 31;

/// Bit in [`ParamInfo::promote`] of native parameters declared
/// `@unchecked`: the type is arginfo only (Reflection, inheritance checks);
/// the handler parses the argument itself, like `zend_parse_parameters`
/// does in php-src, so the VM neither checks nor coerces it.
pub const UNCHECKED_TYPE: u32 = 1 << 30;

/// The [`ParamInfo::promote`] bits that mark native parameters rather than
/// constructor promotion.
pub const NATIVE_PARAM_BITS: u32 = PREFER_REF | UNCHECKED_TYPE;

/// Information about one declared parameter.
#[derive(Clone, Debug)]
pub struct ParamInfo {
    /// Name without `$`.
    pub name: ZStr,
    /// By-reference parameter.
    pub by_ref: bool,
    /// Variadic parameter.
    pub variadic: bool,
    /// Declared type.
    pub ty: Option<TypeDecl>,
    /// Whether a default exists (optional parameter).
    pub has_default: bool,
    /// Default value as source text for messages/reflection.
    pub default_text: Option<String>,
    /// Constructor promotion flags (0 if not promoted).
    pub promote: u32,
    /// `#[SensitiveParameter]`
    pub sensitive: bool,
    /// Default value expression (for Reflection).
    pub default_expr: Option<crate::class::ConstExpr>,
    /// Attributes.
    pub attributes: Vec<crate::class::AttributeInfo>,
}

/// A compiled function body (also used for files and `eval`).
#[derive(Debug, Default)]
pub struct OpArray {
    /// The instructions.
    pub ops: Vec<Op>,
    /// The source line of each instruction.
    pub lines: Vec<u32>,
    /// Literal values.
    pub literals: Vec<Literal>,
    /// Writable locations used by instructions.
    pub places: Vec<Place>,
    /// Names of compiled variables.
    pub cv_names: Vec<ZStr>,
    /// Number of temporaries.
    pub num_tmps: u32,
    /// Number of foreach iterator slots.
    pub num_iters: u32,
    /// Number of finally blocks.
    pub num_finally: u32,
    /// Number of `static` variables.
    pub num_statics: u32,
    /// Try regions, innermost first.
    pub try_table: Vec<TryEntry>,
    /// Live ranges of temporaries released on exceptions.
    pub live_tmps: Vec<LiveTmp>,
    /// The file the code comes from.
    pub filename: ZStr,
    /// `declare(strict_types=1)`.
    pub strict_types: bool,
    /// Nested functions/classes declared by this code.
    pub decls: Rc<DeclTable>,
    /// Per-call-site cache of `InitFCall` resolutions, indexed by the
    /// name literal (like php-src's runtime cache slot: once a call site
    /// resolved a function it keeps calling it).
    pub fcall_cache: RefCell<Vec<Option<CachedFunc>>>,
    /// Per-literal cache of resolved `ClassSpec::Named` classes (classes
    /// are never unregistered once declared). Weak: a method's op array
    /// naming its own class would otherwise form an `Rc` cycle.
    pub class_cache: RefCell<Vec<std::rc::Weak<crate::class::Class>>>,
}

/// An [`OpArray::fcall_cache`] entry. User functions are held weakly: the
/// function's own op array may hold the entry (recursion), and a strong
/// `Rc` would make an unfreeable cycle.
#[derive(Debug, Clone)]
pub enum CachedFunc {
    /// A builtin.
    Builtin(Rc<crate::func::Builtin>),
    /// A user function.
    User(std::rc::Weak<crate::func::UserFunction>),
}

impl CachedFunc {
    /// The cache entry for `f`.
    pub fn new(f: &crate::func::Func) -> Self {
        match f {
            crate::func::Func::Builtin(b) => CachedFunc::Builtin(b.clone()),
            crate::func::Func::User(u) => CachedFunc::User(Rc::downgrade(u)),
        }
    }

    /// The cached function, if it is still alive.
    pub fn get(&self) -> Option<crate::func::Func> {
        match self {
            CachedFunc::Builtin(b) => Some(crate::func::Func::Builtin(b.clone())),
            CachedFunc::User(w) => w.upgrade().map(crate::func::Func::User),
        }
    }
}

/// Functions, classes and closures declared inside a compiled unit.
#[derive(Debug, Default)]
pub struct DeclTable {
    /// User functions (named and closures, by index).
    pub functions: RefCell<Vec<Rc<crate::func::UserFunction>>>,
    /// Class declarations.
    pub classes: RefCell<Vec<Rc<crate::class::ClassDecl>>>,
    /// Attributes of `const` statements (PHP 8.5).
    pub const_attributes: RefCell<Vec<Rc<Vec<crate::class::AttributeInfo>>>>,
}

impl OpArray {
    /// The line of op `ip`.
    pub fn line(&self, ip: usize) -> u32 {
        self.lines.get(ip).copied().unwrap_or(0)
    }
}

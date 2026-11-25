// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
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

/// What a nullsafe chain short-circuited by [`Op::JmpNull`] evaluates
/// to (`ZEND_SHORT_CIRCUITING_CHAIN_*` in the op's `extended_value`, with
/// `ZEND_JMP_NULL_BP_VAR_IS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NullChain {
    /// `null`; an undefined variable warns.
    Expr,
    /// `null`, read quietly (the operand of `??`).
    Quiet,
    /// `false` (`isset()`).
    Isset,
    /// `true` (`empty()`).
    Empty,
}

impl NullChain {
    /// The chain's value when it short-circuits.
    #[must_use]
    pub fn value(self) -> Value {
        match self {
            NullChain::Expr | NullChain::Quiet => Value::Null,
            NullChain::Isset => Value::Bool(false),
            NullChain::Empty => Value::Bool(true),
        }
    }

    /// Whether an undefined variable operand warns.
    #[must_use]
    pub fn warns(self) -> bool {
        self == NullChain::Expr
    }
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

/// The operands of [`Op::FramelessICall`]: the call's `InitFCall`
/// (`name`, `display`; no namespace fallback), its arguments and its
/// `DoCall` result.
///
/// The op stands in one of two places. When the `Send`s and the `DoCall`
/// follow the `InitFCall` directly, it replaces the `InitFCall` and the ops
/// after it are `Nop`s (`init_back == 0`). When arguments are computed
/// between them, it replaces the `DoCall`, and the `InitFCall` and the
/// `Send`s before it are `Nop`s where they stood (`init_back` and
/// `send_back`: how many ops back).
#[derive(Clone, Debug, PartialEq)]
pub struct FramelessCall {
    /// The lower-case function name literal (the run-time cache key): the
    /// global builtin's.
    pub name: u32,
    /// A namespaced call's own name literal (`app\in_array`, the
    /// `InitFCall`'s `name`, `name` above being its global fallback):
    /// php-src's `ZEND_JMP_FRAMELESS`, which runs the call in place only
    /// while no such function was found when the call site first ran (the
    /// site's run-time cache slot keeps the outcome). `None` for a call by
    /// a global name.
    pub ns: Option<u32>,
    /// The display name literal.
    pub display: u32,
    /// The arguments' `Send` sources (`args[..argc]`).
    pub args: [ArgSrc; 3],
    /// The arguments as read by the in-place call: `Lit`, `Cv`, or (only
    /// where arguments are computed first) `Tmp`.
    pub vals: [Operand; 3],
    /// The number of arguments (1..=3).
    pub argc: u8,
    /// The call's result temporary.
    pub dst: Option<u32>,
    /// 0: the op stands at the call's `InitFCall`. Else it stands at the
    /// `DoCall`, this many ops after the `InitFCall`.
    pub init_back: u32,
    /// With `init_back != 0`: how many ops before the op each `Send` stood.
    pub send_back: [u32; 3],
}

impl FramelessCall {
    /// The ops right after this one that belong to the call (the `Nop`s of
    /// its `Send`s and `DoCall`): none when the op stands at the `DoCall`.
    #[inline]
    #[must_use]
    pub fn tail(&self) -> usize {
        if self.init_back == 0 {
            usize::from(self.argc) + 1
        } else {
            0
        }
    }
}

/// The kind of a `foreach` loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForeachKind {
    /// By value.
    Value,
    /// By reference.
    Ref,
}

/// A key of a [`MatchTable`] (`zend_compile_match` builds the jump table
/// from integer and string literals only).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MatchKey {
    /// An integer arm condition (an index key of the table).
    Int(i64),
    /// A string arm condition, never converted to an integer key
    /// (`zend_hash_add`, not the symtable variant).
    Str(Box<[u8]>),
}

/// The jump table of [`Op::Match`].
#[derive(Clone, Debug)]
pub struct MatchTable {
    /// Keys and targets in insertion order (the dump prints them in this
    /// order); a repeated key keeps its first target (`zend_hash_add`).
    pub entries: Vec<(MatchKey, Label)>,
    /// Where an unlisted value or a subject that is neither an int nor a
    /// string jumps: the default arm or the `MatchError`.
    pub default: Label,
    /// Integer key -> index into `entries`.
    ints: std::collections::HashMap<i64, usize>,
    /// String key -> index into `entries`.
    strs: std::collections::HashMap<Box<[u8]>, usize>,
}

impl MatchTable {
    /// An empty table jumping to `default`.
    #[must_use]
    pub fn new(default: Label) -> Self {
        MatchTable {
            entries: Vec::new(),
            default,
            ints: std::collections::HashMap::new(),
            strs: std::collections::HashMap::new(),
        }
    }

    /// Adds `key -> target` unless `key` is already present (the first arm
    /// listing a value wins, as with a chain of `===` tests). Returns
    /// whether it was added.
    pub fn insert(&mut self, key: MatchKey, target: Label) -> bool {
        let at = self.entries.len();
        let added = match &key {
            MatchKey::Int(i) => match self.ints.entry(*i) {
                std::collections::hash_map::Entry::Occupied(_) => false,
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(at);
                    true
                }
            },
            MatchKey::Str(s) => {
                if self.strs.contains_key(s) {
                    false
                } else {
                    self.strs.insert(s.clone(), at);
                    true
                }
            }
        };
        if added {
            self.entries.push((key, target));
        }
        added
    }

    /// The jump target for a (dereferenced) subject value.
    #[must_use]
    pub fn target(&self, v: &Value) -> Label {
        let at = match v {
            Value::Int(i) => self.ints.get(i),
            Value::Str(s) => self.strs.get(s.as_bytes()),
            _ => None,
        };
        at.and_then(|&k| self.entries.get(k))
            .map_or(self.default, |&(_, l)| l)
    }

    /// Every jump target: the entries' in order, then the default.
    #[must_use]
    pub fn labels(&self) -> impl Iterator<Item = Label> + '_ {
        self.entries
            .iter()
            .map(|&(_, l)| l)
            .chain(std::iter::once(self.default))
    }

    /// Every jump target (the entries' and the default), mutably.
    pub fn labels_mut(&mut self) -> impl Iterator<Item = &mut Label> {
        self.entries
            .iter_mut()
            .map(|(_, l)| l)
            .chain(std::iter::once(&mut self.default))
    }
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
    /// Nullsafe: if null, `dst` = what the short-circuited chain gives
    /// and jump.
    JmpNull {
        a: Operand,
        dst: u32,
        target: Label,
        chain: NullChain,
    },

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
    /// Starts a method call. `obj` [`Operand::Unused`] is the frame's
    /// `$this` (`ZEND_INIT_METHOD_CALL` with op1 `IS_UNUSED`, compiled where
    /// `$this` is guaranteed; elsewhere `$this` is a [`Op::FetchThis`]
    /// temporary).
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
    /// `func_get_args()` likewise (`ZEND_FUNC_GET_ARGS`); with `skip`,
    /// `array_slice(func_get_args(), skip)` (`zend_compile_func_array_slice`:
    /// the arguments after the first `skip`).
    FuncGetArgs { dst: Option<u32>, skip: Option<u32> },
    /// `strlen($x)` (`ZEND_STRLEN`, `zend_compile_func_strlen`): no call
    /// frame; weak-mode coercion and the `TypeError` happen in place.
    Strlen { a: Operand, dst: Option<u32> },
    /// `count($x)` / `sizeof($x)` with one argument (`ZEND_COUNT`,
    /// `zend_compile_func_count`); `sizeof` names the function in the
    /// `TypeError`.
    Count {
        a: Operand,
        dst: Option<u32>,
        sizeof: bool,
    },
    /// `is_null()` ... `is_scalar()` (`ZEND_TYPE_CHECK`,
    /// `zend_compile_func_typecheck`): whether the type of `a` is one of
    /// the [`type_mask`] bits in `mask`.
    TypeCheck {
        a: Operand,
        mask: u8,
        dst: Option<u32>,
    },
    /// `gettype($x)` (`ZEND_GET_TYPE`, `zend_compile_func_gettype`).
    GetType { a: Operand, dst: Option<u32> },
    /// `get_class($x)`, or `get_class()` with `a` [`Operand::Unused`]
    /// (`ZEND_GET_CLASS`, `zend_compile_func_get_class`).
    GetClass { a: Operand, dst: Option<u32> },
    /// `get_called_class()` (`ZEND_GET_CALLED_CLASS`).
    GetCalledClass { dst: Option<u32> },
    /// `defined('NAME')` with a literal name without `\` or `:`
    /// (`ZEND_DEFINED`, `zend_compile_func_defined`): whether the constant
    /// named by literal `name` exists. Only the exact name is looked up
    /// (`zend_quick_check_constant`): `true`/`false`/`null` were resolved
    /// at compile time, and `__COMPILER_HALT_OFFSET__` is not found. A
    /// non-string literal names the constant `zval_get_string` makes of it.
    Defined { name: u32, dst: Option<u32> },
    /// `in_array($needle, [literals...][, strict])` (`ZEND_IN_ARRAY`,
    /// `zend_compile_func_in_array`): whether `a` is one of the keys of the
    /// literal array `set`, which holds the haystack's values as keys
    /// (strings as string keys even when numeric, integers as integer
    /// keys).
    InArray {
        a: Operand,
        set: u32,
        strict: bool,
        dst: Option<u32>,
    },
    /// `array_key_exists($key, $array)` (`ZEND_ARRAY_KEY_EXISTS`,
    /// `zend_compile_func_array_key_exists`): no call frame; the
    /// undefined-variable warnings, the key diagnostics and the
    /// `TypeError` for a non-array happen in place, in php-src's order.
    KeyExists {
        key: Operand,
        array: Operand,
        dst: Option<u32>,
    },

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
    /// `ZEND_MATCH`: a match whose conditions are all integer and string
    /// literals jumps through a table keyed by the subject (identity: an
    /// int never finds a string key nor the reverse); any other subject,
    /// or one not in the table, jumps to the table's default. Never falls
    /// through. The subject is not consumed (a temporary is freed after
    /// the match, as `ZEND_FREE_SWITCH`).
    Match {
        subject: Operand,
        table: Box<MatchTable>,
    },
    /// `ZEND_TYPE_ASSERT` of an inlined internal call: throws a `TypeError`
    /// "`msg`, X given" unless `val` is an array.
    TypeAssertArray { val: Operand, msg: u32 },
    /// `declare(ticks=N)` tick.
    /// `ZEND_TICKS` with N of `declare(ticks=N)`.
    Ticks(u32),
    /// Emits a compile-time-style diagnostic at runtime (deferred warnings).
    Diag { level: u8, msg: u32 },
    /// Runs the `zend_set_user_opcode_handler()` hook of the Zend opcode
    /// named by literal `opcode` (the next instruction implements it).
    UserOpcode { opcode: u32 },
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
    /// The object check of `ZEND_AST_DIM` in `zend_ast_evaluate_ex`:
    /// throws when the (kept) container `obj` of a constant-expression
    /// dimension fetch is an object, before the offset is evaluated.
    ConstDimCheck { obj: Operand },

    // ------------------------------------------------------------------
    // Specialised forms (zend_vm_def.h `SPEC(...)` handlers). The compiler's
    // last pass (`compiler/specialise.rs`) rewrites a generic op in place
    // into one of these; the op after it is left untouched. Each handler
    // runs a typed fast path and, when an operand is not of the expected
    // type, does exactly what the generic op it replaced does, so
    // behaviour never depends on the rewrite.
    // ------------------------------------------------------------------
    /// `Bin` (a comparison) whose result is only tested by the `JmpZ` /
    /// `JmpNz` right after it: php-src's smart branch
    /// (`zend_emit_cond_jump` marks `IS_SMART_BRANCH_JMPZ/JMPNZ`;
    /// `ZEND_IS_SMALLER_LONG_SPEC` + `ZEND_VM_SMART_BRANCH`). When both
    /// operands are plain numbers the comparison jumps to `target` when
    /// its result equals `jmp_if` and otherwise skips the jump op; any other
    /// operands compute the comparison into `dst` and fall through to the
    /// jump op. `mask` is [`cmp_outcome::for_branch`] of `op` and `jmp_if`:
    /// the numeric outcomes that take the jump.
    CmpJmp {
        op: BinKind,
        a: Operand,
        b: Operand,
        dst: u32,
        target: Label,
        jmp_if: bool,
        mask: u8,
    },
    /// `Bin` whose result is only assigned by the `AssignCv { cv, val:
    /// Tmp(dst), dst: None }` right after it (opcache's DFA pass writes
    /// such a result straight into the CV, `zend_dfa_optimize_op_array`).
    /// A numeric result stored into a CV holding no refcounted value skips
    /// the `AssignCv`; anything else writes `dst` and falls through to it.
    BinAssignCv {
        op: BinKind,
        a: Operand,
        b: Operand,
        dst: u32,
        cv: u32,
    },
    /// `IncDec` of a plain compiled variable (`ZEND_PRE_INC_SPEC_CV`,
    /// `ZEND_POST_INC_SPEC_CV`, ...: the `IS_LONG`/`IS_DOUBLE` fast paths);
    /// other values go through `place`.
    IncDecCv {
        cv: u32,
        kind: IncDecKind,
        dst: Option<u32>,
        place: PlaceId,
    },
    /// `AssignOp` (`+=`, `-=`, `*=`, `|=`, `&=`, `^=`) of a plain compiled
    /// variable (`ZEND_ASSIGN_OP_SPEC_CV_*` with the `IS_LONG`/`IS_DOUBLE`
    /// fast paths of `zend_binary_op`): a CV and value holding plain
    /// numbers are combined in place; anything else goes through `place`.
    AssignOpCv {
        cv: u32,
        op: BinaryOp,
        val: Operand,
        dst: Option<u32>,
        place: PlaceId,
    },
    /// `AssignOp` (`+=`, `-=`, `*=`, `|=`, `&=`, `^=`) of `$cv[dim]` without
    /// a used result (`ZEND_ASSIGN_DIM_OP_SPEC_CV_*` on an array whose
    /// element at the int or string offset is a plain number, with a plain
    /// number as `OP_DATA`); anything else goes through `place`.
    AssignOpDimCv {
        cv: u32,
        dim: Operand,
        op: BinaryOp,
        val: Operand,
        place: PlaceId,
    },
    /// `IncDec` of `$cv[dim]`: php-src compiles `$a[k]++` to a `FETCH_DIM_RW`
    /// followed by `PRE_INC`/`POST_INC` on the fetched slot; this is the
    /// `IS_LONG`/`IS_DOUBLE` fast path of that pair on an array element.
    /// Other elements and containers go through `place`.
    IncDecDimCv {
        cv: u32,
        dim: Operand,
        kind: IncDecKind,
        dst: Option<u32>,
        place: PlaceId,
    },
    /// `Fetch` (`Read` or `Is` mode) of `$cv[dim]`
    /// (`ZEND_FETCH_DIM_R_SPEC_CV_*` / `ZEND_FETCH_DIM_IS_SPEC_CV_*` with
    /// `zend_fetch_dimension_address_inner` on an array and an int or
    /// string offset). With `to`, the `AssignCv { cv: to, val: Tmp(dst),
    /// dst: None }` right after it is done here too when the CV holds no
    /// refcounted value. A missing key or a non-array falls back to
    /// `place` in `mode`.
    FetchDimCv {
        cv: u32,
        dim: Operand,
        place: PlaceId,
        dst: u32,
        to: Option<u32>,
        mode: FetchMode,
    },
    /// `Fetch` (`Read` or `Is` mode) of `$cv[d1][d2]...`: a chain of two or
    /// more dimensions (`ZEND_FETCH_DIM_R` hits all the way down when
    /// every container is an array holding the int or string offset);
    /// `to` as in [`Op::FetchDimCv`]. Any miss falls back to `place` in
    /// `mode`.
    FetchDimsCv {
        cv: u32,
        place: PlaceId,
        dst: u32,
        to: Option<u32>,
        mode: FetchMode,
    },
    /// `Isset` / `Empty` (`empty`) of `$cv[dim]`
    /// (`ZEND_ISSET_ISEMPTY_DIM_OBJ_SPEC_CV_*` on an array with an int or
    /// string offset); anything else goes through `place`.
    IssetDimCv {
        cv: u32,
        dim: Operand,
        place: PlaceId,
        dst: u32,
        empty: bool,
    },
    /// `Assign` of `$cv[dim] = val` without a used result
    /// (`ZEND_ASSIGN_DIM_SPEC_CV_*_OP_DATA_*` on an array with an int or
    /// string offset and a scalar value; `dim` [`Operand::Unused`] is
    /// `$cv[] = val`, `ZEND_ASSIGN_DIM_SPEC_CV_UNUSED_*`, appending);
    /// anything else goes through `place`.
    AssignDimCv {
        cv: u32,
        dim: Operand,
        val: Operand,
        place: PlaceId,
    },
    /// `Fetch` (`Read` or `Is` mode) of a static property `A::$name`
    /// (`place` has a `PlaceBase::StaticProp` base with a literal name and
    /// a non-dynamic class, and no segments): `ZEND_FETCH_STATIC_PROP_R` /
    /// `_IS` on a run-time cache hit (`zend_fetch_static_property_address`
    /// with `CACHED_PTR`). `to` as in [`Op::FetchDimCv`]. A miss, or a
    /// typed property holding null in `Read` mode (possibly uninitialized),
    /// falls back to `place` in `mode`.
    FetchStaticProp {
        place: PlaceId,
        dst: u32,
        to: Option<u32>,
        mode: FetchMode,
    },
    /// `Fetch` (`Read` or `Is` mode) of a global variable (`$GLOBALS['name']`,
    /// superglobals). With `to`, the `AssignCv { cv: to, val: Tmp(dst), dst: None }`
    /// following this op is folded.
    FetchGlobal {
        place: PlaceId,
        dst: u32,
        to: Option<u32>,
        mode: FetchMode,
    },
    /// `Assign` of a static property place (as in
    /// [`Op::FetchStaticProp`]): `ZEND_ASSIGN_STATIC_PROP` on a cache hit
    /// of an untyped property holding a scalar or string, with a scalar or
    /// string value; anything else is the generic `Assign`.
    AssignStaticProp {
        place: PlaceId,
        val: Operand,
        dst: Option<u32>,
    },
    /// `AssignOp` of a static property place (as in
    /// [`Op::FetchStaticProp`]): `ZEND_ASSIGN_STATIC_PROP_OP` on a cache
    /// hit, `.=` extending a string the property holds in place
    /// (`zend_binary_assign_op_typed_prop`'s in-place concatenation), or a
    /// numeric operator on an untyped property holding a number; anything
    /// else is the generic `AssignOp`.
    AssignOpStaticProp {
        place: PlaceId,
        op: BinaryOp,
        val: Operand,
        dst: Option<u32>,
    },
    /// `Isset` / `Empty` (`empty`) of a static property place (as in
    /// [`Op::FetchStaticProp`]): `ZEND_ISSET_ISEMPTY_STATIC_PROP` on a
    /// cache hit; anything else goes through `place`.
    IssetStaticProp {
        place: PlaceId,
        dst: u32,
        empty: bool,
    },
    /// `AssignOp` `.=` of a plain compiled variable (`$cv .= val`,
    /// `ZEND_ASSIGN_OP_SPEC_CV_*` with `ZEND_CONCAT`, whose
    /// `concat_function` has `result == op1`): a variable holding a string
    /// is extended in place by a string or the digits of an int; anything
    /// else is the generic `.=` of `place`.
    ConcatCv {
        cv: u32,
        val: Operand,
        dst: Option<u32>,
        place: PlaceId,
    },
    /// `Assign` of `$cv[d1][d2]... = val` (two to four dimensions, none
    /// `[]`) without a used result: the chain of `FETCH_DIM_W`s and the
    /// `ASSIGN_DIM` on arrays, each separated (`SEPARATE_ARRAY`), a missing
    /// or null container element becoming an empty array, with a scalar or
    /// string value replacing a scalar or string element (or none);
    /// anything else is the generic `Assign` of `place`.
    AssignDimsCv {
        cv: u32,
        val: Operand,
        place: PlaceId,
    },
    /// `AssignOp` (`+=`, `-=`, `*=`, `|=`, `&=`, `^=`) of `$cv[d1][d2]...`
    /// (as in [`Op::AssignDimsCv`]) without a used result, when every
    /// container exists and is an array and the element is a plain number
    /// (the `FETCH_DIM_RW` chain and `ZEND_ASSIGN_DIM_OP`'s numeric fast
    /// path); anything else is the generic `AssignOp` of `place`.
    AssignOpDimsCv {
        cv: u32,
        op: BinaryOp,
        val: Operand,
        place: PlaceId,
    },
    /// `FetchConst` whose result is only assigned by the `AssignCv { cv:
    /// to, val: Tmp(dst), dst: None }` right after it: the value is stored
    /// into `to` directly when that variable holds no refcounted value,
    /// skipping the `AssignCv` (else written to `dst` for it). A cached
    /// value (`ZEND_FETCH_CONSTANT`'s `CACHED_PTR` hit) needs no lookup; a
    /// miss runs the generic fetch first.
    FetchConstTo {
        name: u32,
        fallback: Option<u32>,
        dst: u32,
        to: u32,
    },
    /// `FetchClassConst` of `class::NAME` (literal `name`) followed by such
    /// an `AssignCv` (as in [`Op::FetchConstTo`]), with
    /// `ZEND_FETCH_CLASS_CONSTANT`'s cache.
    FetchClassConstTo {
        class: ClassSpec,
        name: u32,
        dst: u32,
        to: u32,
    },
    /// An [`Op::IncDecCv`] without a used result right before an
    /// [`Op::CmpJmp`]: a rotated loop's step and condition (`for (...; $i <
    /// $n; $i++)`, `while (...) { ...; $i++; }`). After the in-place `++` /
    /// `--`, the comparison of that `CmpJmp` is decided here too when its
    /// operands are plain numbers: the op jumps to the `CmpJmp`'s target or
    /// skips the `CmpJmp` and its jump op; otherwise the `CmpJmp` runs as
    /// its own op. Entering at the `CmpJmp` (the loop's entry jump) runs it
    /// alone; anything but a plain number in the variable is the generic
    /// `IncDec` of `place`.
    IncDecCvCmpJmp {
        cv: u32,
        kind: IncDecKind,
        place: PlaceId,
    },
    /// An [`Op::BinAssignCv`] right before an [`Op::CmpJmp`] (with the kept
    /// [`Op::AssignCv`] in between): a rotated loop's step and condition
    /// (`for (...; $i < $n; $i = $i + 1)`). After computing the binary
    /// operation and assigning to `cv`, the comparison of that `CmpJmp` is
    /// decided here too when operands are plain numbers: the op jumps to the
    /// `CmpJmp`'s target or skips the `AssignCv`, the `CmpJmp` and its kept
    /// jump op; otherwise the `CmpJmp` runs as its own op.
    BinAssignCvCmpJmp {
        op: BinKind,
        a: Operand,
        b: Operand,
        dst: u32,
        cv: u32,
    },
    /// An [`Op::AssignOpCv`] without a used result right before an
    /// [`Op::CmpJmp`]: a rotated loop's step and condition (`for (...; $i <
    /// $n; $i += 1)`). After the in-place compound assignment, the comparison
    /// of that `CmpJmp` is decided here too when operands are plain numbers:
    /// the op jumps to the `CmpJmp`'s target or skips the `CmpJmp` and its
    /// kept jump op; otherwise the `CmpJmp` runs as its own op.
    AssignOpCvCmpJmp {
        cv: u32,
        op: BinaryOp,
        val: Operand,
        place: PlaceId,
    },
    /// An [`Op::BinAssignCv`] followed by a branch testing the assigned variable
    /// (`$x = $a + $b; if ($x)`, `$cond = $a < $b; if ($cond)`).
    /// After computing the binary operation and assigning to `cv`, branches
    /// directly to `target` if the truthiness of `cv` equals `jmp_if`, skipping
    /// the `AssignCv` and the branch op.
    BinAssignCvJmp {
        op: BinKind,
        a: Operand,
        b: Operand,
        dst: u32,
        cv: u32,
        target: Label,
        jmp_if: bool,
    },
    /// An [`Op::AssignOpCv`] without a used result followed by a branch
    /// testing the variable (`$i += 1; if ($i)`). After the in-place
    /// compound assignment, branches directly to `target` if the truthiness
    /// of `cv` equals `jmp_if`, skipping the branch op.
    AssignOpCvJmp {
        cv: u32,
        op: BinaryOp,
        val: Operand,
        place: PlaceId,
        target: Label,
        jmp_if: bool,
    },
    /// `InitFCall("abs")` + `Send` + `DoCall` specialised into an inline operation.
    /// When `src` holds an int or float, computes its absolute value inline
    /// without call frame setup.
    Abs {
        src: Operand,
        dst: Option<u32>,
        to: Option<u32>,
    },
    /// `InitFCall` of a builtin with a frameless class, its 1..=3
    /// positional `Send`s of literals and variables and its `DoCall`, as
    /// one op (php-src's `ZEND_FRAMELESS_ICALL_n`,
    /// `zend_compile_frameless_icall`); the ops it replaced follow as
    /// `Nop`s. The call runs in place when the arguments have the shapes
    /// the handler takes without diagnostics; otherwise it runs as the ops
    /// it replaced, one after the other.
    FramelessICall(Box<FramelessCall>),

    // Property ops (`ZEND_FETCH_OBJ_R` & co. with a `CACHE_SLOT`): `place`
    // is `$cv->name` or `$this->name` with a literal name (the `...Dim`
    // forms: followed by one `[dim]`, or `[]` where `dim` is
    // `Operand::Unused`). On a hit of the place's property cache entry
    // (`place/propcache.rs`) the handler (`exec/spec/props.rs`) accesses
    // the property's slot directly, without the place walk; anything the
    // cache declines (magic methods, hooks, readonly or visibility errors,
    // typed-property coercion, references where they matter, diagnostics)
    // is the generic op of `place`.
    /// `Fetch` (`Read` or `Is` mode) of `$cv->name` / `$this->name`
    /// (`ZEND_FETCH_OBJ_R` / `ZEND_FETCH_OBJ_IS`). With `to`, the `AssignCv
    /// { cv: to, val: Tmp(dst), dst: None }` right after it is done here
    /// too when the variable holds no refcounted value (as in
    /// [`Op::FetchDimCv`]).
    FetchObjProp {
        place: PlaceId,
        dst: u32,
        to: Option<u32>,
        mode: FetchMode,
    },
    /// `Fetch` (`Read` or `Is` mode) of `$cv->name[dim]` /
    /// `$this->name[dim]` (`ZEND_FETCH_OBJ_R` + `ZEND_FETCH_DIM_R`, or the
    /// `_IS` pair) when the property holds an array with the element; `to`
    /// as in [`Op::FetchObjProp`].
    FetchObjPropDim {
        place: PlaceId,
        dim: Operand,
        dst: u32,
        to: Option<u32>,
        mode: FetchMode,
    },
    /// `Assign` of `$cv->name = val` / `$this->name = val`
    /// (`ZEND_ASSIGN_OBJ` + `OP_DATA`).
    AssignObjProp {
        place: PlaceId,
        val: Operand,
        dst: Option<u32>,
    },
    /// `Assign` of `$cv->name[dim] = val`, or `$cv->name[] = val` when
    /// `dim` is [`Operand::Unused`] (`ZEND_FETCH_OBJ_W` +
    /// `ZEND_ASSIGN_DIM` + `OP_DATA`), on a property holding an array.
    AssignObjPropDim {
        place: PlaceId,
        dim: Operand,
        val: Operand,
        dst: Option<u32>,
    },
    /// `AssignOp` of `$cv->name op= val` (`ZEND_ASSIGN_OBJ_OP`): `.=` of a
    /// string or int onto a string the property holds, extended in place;
    /// `+=`, `-=`, `*=`, `|=`, `&=`, `^=` of a number on a number.
    AssignOpObjProp {
        place: PlaceId,
        op: BinaryOp,
        val: Operand,
        dst: Option<u32>,
    },
    /// `AssignOp` of `$cv->name[dim] op= val` (`ZEND_FETCH_OBJ_RW` +
    /// `ZEND_ASSIGN_DIM_OP`) on an existing element of an array property,
    /// with the operators of [`Op::AssignOpObjProp`].
    AssignOpObjPropDim {
        place: PlaceId,
        dim: Operand,
        op: BinaryOp,
        val: Operand,
        dst: Option<u32>,
    },
    /// `IncDec` of `$cv->name` (`ZEND_PRE_INC_OBJ`, `ZEND_POST_DEC_OBJ`,
    /// ...) on a property holding an int (without overflow) or a float.
    IncDecObjProp {
        place: PlaceId,
        kind: IncDecKind,
        dst: Option<u32>,
    },
    /// `IncDec` of `$cv->name[dim]` (`ZEND_FETCH_OBJ_RW` +
    /// `ZEND_FETCH_DIM_RW` + `ZEND_PRE_INC` & co.) on an existing int
    /// (without overflow) or float element of an array property.
    IncDecObjPropDim {
        place: PlaceId,
        dim: Operand,
        kind: IncDecKind,
        dst: Option<u32>,
    },
    /// `Isset` / `Empty` (`empty`) of `$cv->name`
    /// (`ZEND_ISSET_ISEMPTY_PROP_OBJ`) when the answer needs no conversion.
    IssetObjProp {
        place: PlaceId,
        dst: u32,
        empty: bool,
    },
    /// `Isset` / `Empty` (`empty`) of `$cv->name[dim]` (`ZEND_FETCH_OBJ_IS`,
    /// then `ZEND_ISSET_ISEMPTY_DIM_OBJ`) on a property holding an array,
    /// when the answer needs no conversion.
    IssetObjPropDim {
        place: PlaceId,
        dim: Operand,
        dst: u32,
        empty: bool,
    },
}

/// The bits of [`Op::CmpJmp`]'s `mask`: the outcomes of comparing two
/// plain numbers for which the smart branch jumps, plus the
/// [`IDENTITY`](cmp_outcome::IDENTITY) flag. Decided once by the compiler
/// from the comparison and the jump's sense, so the handler tests one bit
/// instead of decoding the operator (php-src generates one handler per
/// comparison and jump sense, `ZEND_IS_SMALLER_SPEC_..._JMPZ` & co.).
pub mod cmp_outcome {
    use super::BinKind;

    /// `a < b`.
    pub const LESS: u8 = 1 << 0;
    /// `a == b`.
    pub const EQUAL: u8 = 1 << 1;
    /// `a > b`.
    pub const GREATER: u8 = 1 << 2;
    /// A NaN operand: no order (every comparison but `!=`/`!==` is false).
    pub const UNORDERED: u8 = 1 << 3;
    /// An int and a float under `===` / `!==` (different types).
    pub const MISMATCH: u8 = 1 << 4;
    /// Flag, not an outcome: the comparison is `===` / `!==`, so an int
    /// and a float are a [`MISMATCH`] instead of being compared as floats.
    pub const IDENTITY: u8 = 1 << 5;
    /// Every outcome.
    pub const ALL: u8 = LESS | EQUAL | GREATER | UNORDERED | MISMATCH;

    /// The mask of a smart branch on comparison `op` that jumps when the
    /// comparison's result is `jmp_if`; `None` when `op` is not one of the
    /// smart-branch comparisons (`zend_is_smart_branch`).
    #[must_use]
    pub fn for_branch(op: BinKind, jmp_if: bool) -> Option<u8> {
        let (taken, identity) = match op {
            BinKind::Less => (LESS, false),
            BinKind::LessEqual => (LESS | EQUAL, false),
            BinKind::Equal => (EQUAL, false),
            BinKind::NotEqual => (LESS | GREATER | UNORDERED, false),
            BinKind::Identical => (EQUAL, true),
            BinKind::NotIdentical => (LESS | GREATER | UNORDERED | MISMATCH, true),
            BinKind::Spaceship => (LESS | GREATER | UNORDERED, false),
            BinKind::Arith(_) => return None,
        };
        let taken = if jmp_if { taken } else { ALL & !taken };
        Some(if identity { taken | IDENTITY } else { taken })
    }
}

/// The type bits of [`Op::TypeCheck`] (`zend_compile_func_typecheck`'s
/// `1 << IS_*` masks, one bit per PHP type).
pub mod type_mask {
    /// `null`
    pub const NULL: u8 = 1 << 0;
    /// `false` and `true`
    pub const BOOL: u8 = 1 << 1;
    /// `int`
    pub const INT: u8 = 1 << 2;
    /// `float`
    pub const FLOAT: u8 = 1 << 3;
    /// `string`
    pub const STRING: u8 = 1 << 4;
    /// `array`
    pub const ARRAY: u8 = 1 << 5;
    /// `object`
    pub const OBJECT: u8 = 1 << 6;
    /// `resource` (alone, a closed resource does not match: `is_resource`)
    pub const RESOURCE: u8 = 1 << 7;
    /// `is_scalar()`
    pub const SCALAR: u8 = BOOL | INT | FLOAT | STRING;

    /// Whether `v` (dereferenced) has one of the types in `mask`
    /// (`ZEND_TYPE_CHECK`).
    pub fn matches(mask: u8, v: &php_runtime::Value) -> bool {
        use php_runtime::Value;
        match v {
            Value::Null => mask & NULL != 0,
            Value::Bool(_) => mask & BOOL != 0,
            Value::Int(_) => mask & INT != 0,
            Value::Float(_) => mask & FLOAT != 0,
            Value::Str(_) => mask & STRING != 0,
            Value::Array(_) => mask & ARRAY != 0,
            Value::Object(_) => mask & OBJECT != 0,
            // `opline->extended_value != MAY_BE_RESOURCE ||
            // zend_rsrc_list_get_rsrc_type(...)`.
            Value::Resource(r) => mask & RESOURCE != 0 && (mask != RESOURCE || !r.is_closed()),
            Value::Reference(r) => matches(mask, &r.get()),
        }
    }
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
#[allow(clippy::struct_excessive_bools)] // mirrors `zend_arg_info`'s independent flags
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

/// One `static $name = init;` declaration ([`OpArray::static_vars`]).
#[derive(Clone, Debug, Default)]
pub struct StaticVarDecl {
    /// Variable name without `$`.
    pub name: ZStr,
    /// The initial value when the initializer is a constant (a literal
    /// operand; `null` without initializer), else `None` (computed when
    /// the declaration first runs).
    pub init: Option<Value>,
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
    /// The `static` variable declarations, indexed by slot (php-src's
    /// `op_array->static_variables`): reflection and closures read this
    /// table, so the optimizer may delete the `BindStatic`/`InitStatic` of
    /// an unused static without losing the variable.
    pub static_vars: Vec<StaticVarDecl>,
    /// Try regions, innermost first.
    pub try_table: Vec<TryEntry>,
    /// Live ranges of temporaries released on exceptions.
    pub live_tmps: Vec<LiveTmp>,
    /// The file the code comes from.
    pub filename: ZStr,
    /// `declare(strict_types=1)`.
    pub strict_types: bool,
    /// The functions, closures, classes and constant attributes declared by
    /// this code, which its declaration ops index (php-src's
    /// `dynamic_func_defs`). A nested op array's table holds only its own
    /// declarations, so no declared function's code refers back to a table
    /// that holds the function; a unit's main op array has the unit's table,
    /// which also lists every declaration nested deeper
    /// ([`crate::compiler::CompiledUnit::decls`]).
    pub decls: Rc<DeclTable>,
    /// Per-call-site cache of `InitFCall` resolutions, indexed by the
    /// name literal (like php-src's runtime cache slot: once a call site
    /// resolved a function it keeps calling it).
    pub fcall_cache: RefCell<Vec<Option<CachedFunc>>>,
    /// Per-literal cache of resolved `ClassSpec::Named` classes (classes
    /// are never unregistered once declared). Weak: a method's op array
    /// naming its own class would otherwise form an `Rc` cycle.
    pub class_cache: RefCell<Vec<std::rc::Weak<crate::class::Class>>>,
    /// Run-time cache of constant, class constant, static property and
    /// static method lookups (php-src's `run_time_cache`; see
    /// [`crate::rtcache`]).
    pub rt_cache: crate::rtcache::RuntimeCache,
    /// Per-place cache of property lookups (php-src's `CACHE_ADDR` slot of
    /// `FETCH_OBJ_*`/`ASSIGN_OBJ` & co., filled by
    /// `zend_fetch_property_address`), sized on first use: entry `pid`
    /// describes the first segment of place `pid` when that is a
    /// `$cv->name` / `$this->name` head, otherwise its last `->name`
    /// segment; entry `places.len() + pid` the last segment of a place
    /// that has both (`$this->a->b`). See `place::propcache`.
    pub prop_cache: RefCell<Vec<PropCacheEntry>>,
    /// Monomorphic call-site cache of method calls (`InitMethodCall`),
    /// indexed by instruction pointer (op index).
    pub method_cache: RefCell<Vec<Option<MethodCallCacheEntry>>>,
    /// The instructions pre-decoded for the frame-local loop, built when
    /// the op array first runs there (`exec/tape.rs`). `ops` must not
    /// change once it exists.
    pub(crate) tape: crate::exec::tape::Tape,
    /// The instructions an optimizer pass dropped while their result
    /// temporary keeps its number in php-src (a `BOOL_NOT` the block pass
    /// folds into a jump): without pass 9 the dump counts them where they
    /// stood.
    pub dropped_tmps: Vec<u32>,
}

/// [`PropCacheEntry::base`] of a `$this->name` place.
pub const PROP_CACHE_THIS: u32 = u32::MAX;

/// An [`OpArray::prop_cache`] entry: the property a literal-named
/// segment of a property place resolved to for one class and calling
/// scope, like php-src's cached `(ce, offset, prop_info)` triple: a
/// declared slot or a dynamic property. Classes are compared by address;
/// the entry holds them weakly, which pins the address without keeping
/// the class alive (a method's op array caching its own class would
/// otherwise be an `Rc` cycle).
#[derive(Debug, Default, Clone)]
pub struct PropCacheEntry {
    /// Address of the object's class (`Rc::as_ptr`; pinned by `class`),
    /// 0 in an unfilled entry, which therefore never matches.
    pub class_addr: usize,
    /// Address of the class scope the lookup ran in (pinned by `scope`),
    /// 0 for no class scope.
    pub scope_addr: usize,
    /// The object's class (`Weak::new()` in an unfilled entry).
    pub class: std::rc::Weak<crate::class::Class>,
    /// The class scope the lookup ran in (`None`: no class scope).
    pub scope: Option<std::rc::Weak<crate::class::Class>>,
    /// The place's base: a CV, or [`PROP_CACHE_THIS`] (unused by an entry
    /// for a later segment).
    pub base: u32,
    /// The declared property's slot.
    pub slot: u32,
    /// The literal index of the property name.
    pub lit: u32,
    /// Builtin type bits (`types::mask`) a written value may have to be
    /// stored without coercion; `u32::MAX` for an untyped property.
    pub accept: u32,
    /// The lower-case class a typed property names (exactly one, not
    /// `self`/`parent`/`static`): instances of it are stored as-is too.
    pub class_lc: Option<ZStr>,
    /// Address of the class of the last object stored because it is an
    /// instance of `class_lc` (pinned by `ok_class`; 0: none yet): later
    /// instances of that class pass with one comparison.
    pub ok_class_addr: usize,
    /// Pins `ok_class_addr`.
    pub ok_class: std::rc::Weak<crate::class::Class>,
    /// `PC_*` bits of `place::propcache`: which fast paths apply.
    pub flags: u8,
}

/// An [`OpArray::fcall_cache`] entry. User functions and classes are held
/// weakly: the function's own op array may hold the entry (recursion), and a
/// strong `Rc` would make an unfreeable cycle.
#[derive(Debug, Clone)]
pub struct CachedFunc {
    /// The function handle.
    pub func: CachedFuncKind,
    /// Declaring class (for methods).
    pub scope: Option<std::rc::Weak<crate::class::Class>>,
    /// Called class (`static::class`).
    pub called: Option<std::rc::Weak<crate::class::Class>>,
}

/// The cached function body.
#[derive(Debug, Clone)]
pub enum CachedFuncKind {
    /// A builtin.
    Builtin(Rc<crate::func::Builtin>),
    /// A user function.
    User(std::rc::Weak<crate::func::UserFunction>),
}

impl CachedFunc {
    /// The cache entry for `f`.
    pub fn new(f: &crate::func::Func) -> Self {
        let func = match f {
            crate::func::Func::Builtin(b) => CachedFuncKind::Builtin(b.clone()),
            crate::func::Func::User(u) => CachedFuncKind::User(Rc::downgrade(u)),
        };
        CachedFunc {
            func,
            scope: None,
            called: None,
        }
    }

    /// The cache entry for a static method `f` with declaring scope and called class.
    pub fn new_static(
        f: &crate::func::Func,
        scope: Option<&Rc<crate::class::Class>>,
        called: Option<&Rc<crate::class::Class>>,
    ) -> Self {
        let func = match f {
            crate::func::Func::Builtin(b) => CachedFuncKind::Builtin(b.clone()),
            crate::func::Func::User(u) => CachedFuncKind::User(Rc::downgrade(u)),
        };
        CachedFunc {
            func,
            scope: scope.map(Rc::downgrade),
            called: called.map(Rc::downgrade),
        }
    }

    /// The cached function, if it is still alive.
    #[inline]
    pub fn get(&self) -> Option<crate::func::Func> {
        match &self.func {
            CachedFuncKind::Builtin(b) => Some(crate::func::Func::Builtin(b.clone())),
            CachedFuncKind::User(w) => w.upgrade().map(crate::func::Func::User),
        }
    }

    /// The cached call target with declaring scope and called class.
    #[inline]
    pub fn get_call(
        &self,
    ) -> Option<(
        crate::func::Func,
        Option<Rc<crate::class::Class>>,
        Option<Rc<crate::class::Class>>,
    )> {
        let f = self.get()?;
        let scope = match &self.scope {
            Some(w) => Some(w.upgrade()?),
            None => None,
        };
        let called = match &self.called {
            Some(w) => Some(w.upgrade()?),
            None => None,
        };
        Some((f, scope, called))
    }
}

/// An [`OpArray::method_cache`] entry: monomorphic method resolution cached
/// at an `InitMethodCall` call site. Classes are compared by address;
/// the entry holds them weakly, which pins the address without keeping
/// the class alive.
#[derive(Debug, Clone)]
pub struct MethodCallCacheEntry {
    /// Address of the object's class (`Rc::as_ptr`), 0 in an unfilled entry.
    pub class_addr: usize,
    /// Address of the calling class scope, 0 for no class scope.
    pub scope_addr: usize,
    /// Weak reference to the object's class to pin the address without an `Rc` cycle.
    pub class: std::rc::Weak<crate::class::Class>,
    /// Declaring class scope of the resolved method.
    pub method_scope: Option<std::rc::Weak<crate::class::Class>>,
    /// The resolved method function.
    pub func: CachedFunc,
    /// Whether the method is static and drops `$this`.
    pub is_static: bool,
    /// Literal index of the method name, if named by a literal operand.
    pub name_lit: Option<u32>,
    /// Method name (for matching when called dynamically).
    pub name: ZStr,
}

/// Functions, classes and closures declared inside an op array
/// ([`OpArray::decls`]).
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

    /// Clears run-time caches so this op array carries no request state.
    pub fn clear_runtime_caches(&self) {
        // A JIT trace that inlined a call resolved it through
        // `fcall_cache`: clearing it means a later call at the same site
        // can resolve to a different function (see `jit.rs`).
        crate::func::bump_func_table_version();
        self.fcall_cache.borrow_mut().clear();
        self.class_cache.borrow_mut().clear();
        self.rt_cache.clear();
        self.prop_cache.borrow_mut().clear();
        self.method_cache.borrow_mut().clear();
    }
}

#[cfg(test)]
mod match_table_tests {
    use super::{MatchKey, MatchTable};
    use php_runtime::{Value, ZStr};

    #[test]
    fn lookup_is_by_identity_and_the_first_key_wins() {
        let mut t = MatchTable::new(9);
        assert!(t.insert(MatchKey::Int(1), 3));
        assert!(t.insert(MatchKey::Str(b"1".as_slice().into()), 4));
        assert!(!t.insert(MatchKey::Int(1), 5));
        assert!(t.insert(MatchKey::Str(b"a".as_slice().into()), 6));
        assert_eq!(t.entries.len(), 3);
        assert_eq!(t.target(&Value::Int(1)), 3);
        assert_eq!(t.target(&Value::Str(ZStr::from("1"))), 4);
        assert_eq!(t.target(&Value::Str(ZStr::from("a"))), 6);
        // Floats, bools, null and unlisted values take the default.
        assert_eq!(t.target(&Value::Float(1.0)), 9);
        assert_eq!(t.target(&Value::Bool(true)), 9);
        assert_eq!(t.target(&Value::Null), 9);
        assert_eq!(t.target(&Value::Int(2)), 9);
        assert_eq!(t.target(&Value::Str(ZStr::from("01"))), 9);
        assert_eq!(t.labels().collect::<Vec<_>>(), vec![3, 4, 6, 9]);
        for l in t.labels_mut() {
            *l += 1;
        }
        assert_eq!(t.target(&Value::Int(1)), 4);
        assert_eq!(t.default, 10);
    }
}

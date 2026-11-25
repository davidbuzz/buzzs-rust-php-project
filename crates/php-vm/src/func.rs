// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Functions: user functions (compiled op arrays) and builtins.

use crate::class::Class;
use crate::opcode::{OpArray, ParamInfo};
use crate::types::TypeDecl;
use crate::vm::{Vm, VmResult};
use php_runtime::{Obj, PhpRef, Value, ZStr};
use std::cell::{Cell, OnceCell, RefCell};
use std::fmt;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};

/// Bumped on every change to the VM's function table (a declaration, an
/// include's hoisted declaration, a reset, a discard, a disable, a
/// re-registration). A JIT trace that inlines a user-function call
/// resolves the callee at plan time without a per-entry name lookup, so
/// its entry code compares this counter with the planned value and
/// disarms itself when they differ (see `jit.rs`).
pub(crate) static FUNC_TABLE_VERSION: AtomicU64 = AtomicU64::new(1);

/// Records a change to the function table.
#[inline]
pub(crate) fn bump_func_table_version() {
    FUNC_TABLE_VERSION.fetch_add(1, Ordering::Relaxed);
}

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
    /// `public(set)`
    pub const SET_PUBLIC: u32 = 1 << 19;
    /// `protected(set)`
    pub const SET_PROTECTED: u32 = 1 << 20;
    /// `private(set)`
    pub const SET_PRIVATE: u32 = 1 << 21;
    /// Virtual (hooked, unbacked) property.
    pub const VIRTUAL: u32 = 1 << 22;
    /// A static native method that still receives `$this` when called on
    /// an instance (`XMLReader::open`'s `get_method` override).
    pub const STATIC_KEEPS_THIS: u32 = 1 << 23;
    /// A temporary native method made by a `get_method` handler
    /// (`internal_function.module` is NULL): it belongs to no extension.
    pub const NO_MODULE: u32 = 1 << 24;
    /// A native function whose argument-parsing errors (arity, type) have
    /// the argument values removed from every frame of the exception's
    /// backtrace (ext/sodium's `sodium_remove_param_values_from_backtrace`
    /// after a failed `zend_parse_parameters`).
    pub const SCRUB_ARG_ERROR_TRACE: u32 = 1 << 25;
    /// A property whose slot starts undefined but not `IS_PROP_UNINIT`
    /// (`__get()` & co. apply as after `unset()`): a virtual hooked
    /// redeclaration of a backed hooked parent property that had a default
    /// (`do_inherit_property`: "Default value was removed in child").
    pub const DEFAULT_UNSET: u32 = 1 << 26;
    /// Set-visibility mask.
    pub const SET_MASK: u32 = SET_PUBLIC | SET_PROTECTED | SET_PRIVATE;
    /// The lowest of the five bits (27..=31) of a builtin *function*'s
    /// flags that hold its frameless-call class (php-src's
    /// `ZEND_FRAMELESS_FUNCTION` infos), zero for none. Set at
    /// registration from the function's name (`crate::special`); methods,
    /// closures and trampolines never carry one.
    pub const FRAMELESS_SHIFT: u32 = 27;
    /// Mask of the frameless-call class bits (see [`FRAMELESS_SHIFT`]).
    pub const FRAMELESS: u32 = 0x1f << FRAMELESS_SHIFT;
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
    /// For property hooks: the property name.
    pub hook_prop: Option<ZStr>,
    /// What the plain-call test of `Vm::push_simple_ucall` needs to know
    /// about the declaration, worked out on the first call
    /// ([`UserFunction::call_shape`]).
    pub call_shape: CallShape,
}

/// [`UserFunction::call_shape`]'s answer, cached in the function: whether
/// the declaration rules out the plain call path of `Vm::push_simple_ucall`
/// whatever the arguments, and whether the arguments' types must be checked
/// one by one. Starts unknown; every `UserFunction` literal sets
/// `CallShape::default()`.
///
/// With it, for a plain declaration, the instruction a plain call with `n`
/// passed arguments starts at (`UserFunction::plain_first_op`): the
/// `RECV`-skip walk of the body's first ops, done once per count instead
/// of once per call.
#[derive(Debug, Default)]
pub struct CallShape {
    kind: Cell<u8>,
    /// [`first_op_after_recv`] of the code for `n` arguments, at index `n`
    /// (`0..=6`, the pending call's inline buffer); filled with `kind`.
    first_ops: [Cell<u32>; FIRST_OPS],
    /// The machine code of a function compiled whole (`exec/tape/jit.rs`,
    /// "whole functions": a pure int function, its recursion included): 0
    /// while its plain calls are being counted, 1 once it was refused,
    /// else the address of its entry.
    pub(crate) native: Cell<usize>,
    /// With `native`: the address of the body, which the native code of
    /// other functions calls.
    pub(crate) native_body: Cell<usize>,
    /// The plain calls counted toward compiling it; afterwards, how many
    /// times in a row its native code declined a call.
    pub(crate) native_count: Cell<u32>,
    /// The body as a list of `$this->property = $parameter` steps when it
    /// is nothing else ([`UserFunction::ctor_fold`]), worked out on first
    /// use.
    fold: std::cell::OnceCell<Option<Box<[FoldStep]>>>,
    /// [`UserFunction::near_plain_limit`]'s answer for a declaration of
    /// shape [`CallShape::NOT_PLAIN`]: 0 while unknown, [`NEAR_NO`] when
    /// its calls stay generic, else one more than the most arguments a
    /// plain call of it passes.
    near: Cell<u8>,
}

/// [`CallShape::near`] of a declaration whose calls all stay generic.
const NEAR_NO: u8 = u8::MAX;

impl Drop for CallShape {
    fn drop(&mut self) {
        // Freed as it was allocated: outside the accounting.
        if let Some(Some(steps)) = self.fold.take() {
            php_memlimit::persistent(|| drop(steps));
        }
    }
}

/// One step of a constructor that only stores its parameters
/// ([`UserFunction::ctor_fold`]): `$this-><property> = $<parameter>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FoldStep {
    /// The property's place in the function's op array: its entry of the
    /// property cache says which slot of which class it is.
    pub(crate) place: u32,
    /// The parameter stored (its index, which is its compiled variable).
    pub(crate) param: u32,
}

/// The most steps [`UserFunction::ctor_fold`] gives.
pub(crate) const FOLD_STEPS: usize = 8;

/// Passed-argument counts with a precomputed first op ([`CallShape`]):
/// `0..=6`, as many as the pending call's inline argument buffer holds
/// (the plain call takes no more).
const FIRST_OPS: usize = 7;

impl CallShape {
    /// Not worked out yet.
    const UNKNOWN: u8 = 0;
    /// Deprecated, `#[\NoDiscard]`, body-less or a generator, with static
    /// variables, foreach or finally slots, a by-reference or variadic
    /// parameter, or more parameters than compiled variables.
    pub const NOT_PLAIN: u8 = 1;
    /// Plain, and no parameter has a type other than `mixed`.
    pub const UNTYPED: u8 = 2;
    /// Plain, with a parameter whose type each argument must pass.
    pub const TYPED: u8 = 3;
}

impl fmt::Debug for UserFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "UserFunction({})", self.name)
    }
}

impl UserFunction {
    /// The plain-call shape of the declaration ([`CallShape`]): computed
    /// from the flags, the op array's slot counts and the parameters on the
    /// first call and cached, as none of them changes once the function is
    /// shared. One load and compare per call where the checks it stands
    /// for were a dozen loads and a loop over the parameters.
    #[inline]
    pub fn call_shape(&self) -> u8 {
        let s = self.call_shape.kind.get();
        if s != CallShape::UNKNOWN {
            return s;
        }
        let s = self.compute_call_shape();
        self.call_shape.kind.set(s);
        s
    }

    /// Where a plain call (`Vm::push_simple_ucall`) of this function with
    /// `n` passed arguments starts: `first_op_after_recv` of its code,
    /// read from the table [`UserFunction::call_shape`] filled for a plain
    /// declaration (call that first), worked out for a count beyond it.
    #[inline]
    pub(crate) fn plain_first_op(&self, n: usize) -> usize {
        match self.call_shape.first_ops.get(n) {
            Some(c) if self.call_shape.kind.get() > CallShape::NOT_PLAIN => c.get() as usize,
            _ => first_op_after_recv(&self.code, n) as usize,
        }
    }

    /// For a declaration of shape [`CallShape::NOT_PLAIN`] that differs
    /// from a plain one only by its static variables, a trailing variadic
    /// parameter or by-reference parameters, the most arguments a call of
    /// it may pass and still take the plain call paths (the tape's
    /// prepared call, `Vm::push_simple_ucall`); `None` for any other.
    ///
    /// php-src makes no difference between such calls and others at the
    /// call: the static variables live with the function and
    /// `ZEND_BIND_STATIC` binds them as the body runs; the arguments past
    /// the declared ones wait in the call frame for `ZEND_RECV_VARIADIC`
    /// (here: in the frame's argument buffer, after a `None` for each
    /// declared parameter bound into its compiled variable, as the generic
    /// call leaves them); a by-reference parameter that the call does not
    /// reach (fewer arguments than its position, the limit) is bound from
    /// its default by its `RECV_INIT` like any other. Every parameter must
    /// be untyped (or `mixed`): the plain paths check argument types only
    /// for [`CallShape::TYPED`]. A closure with static variables is left
    /// out (its variables are its object's, `Frame::statics`).
    ///
    /// The JIT and the tape's whole-function paths read only
    /// [`UserFunction::call_shape`] and keep away from these.
    #[inline]
    pub(crate) fn near_plain_limit(&self) -> Option<usize> {
        let mut s = self.call_shape.near.get();
        if s == 0 {
            s = self.compute_near();
            self.call_shape.near.set(s);
        }
        if s == NEAR_NO {
            None
        } else {
            Some(usize::from(s - 1))
        }
    }

    #[cold]
    #[inline(never)]
    fn compute_near(&self) -> u8 {
        let nparams = self.params.len();
        let plain_rest = self.flags
            & (flags::DEPRECATED | flags::NODISCARD | flags::NO_BODY | flags::GENERATOR)
            == 0
            && (self.flags & flags::CLOSURE == 0 || self.code.num_statics == 0)
            && self.code.num_finally == 0
            && nparams <= self.code.cv_names.len()
            && self.params.iter().enumerate().all(|(i, p)| {
                (!p.variadic || i + 1 == nparams) && p.ty.as_ref().is_none_or(TypeDecl::is_mixed)
            });
        if !plain_rest {
            return NEAR_NO;
        }
        let variadic = self.params.last().is_some_and(|p| p.variadic);
        let most = if variadic { usize::MAX } else { nparams };
        let first_ref = self
            .params
            .iter()
            .position(|p| p.by_ref)
            .unwrap_or(usize::MAX);
        // (At most 253: the count is kept in a byte, next to `NEAR_NO`.)
        let limit = most.min(first_ref).min(usize::from(NEAR_NO) - 2);
        u8::try_from(limit + 1).unwrap_or(NEAR_NO)
    }

    /// The steps of this function's body when the whole of it, called
    /// with every declared parameter passed, stores parameters in
    /// properties of `$this` and returns: `$this->a = $a; ...` and nothing
    /// else (a promoted constructor parameter compiles to the same op).
    /// Such a call needs no frame (`exec/tape/calls.rs`, `new_plain`): the
    /// values go from the caller's operands into the object's slots.
    /// `None` for any other body, for a declaration that makes no plain
    /// call, and for more than [`FOLD_STEPS`] steps.
    #[inline]
    pub(crate) fn ctor_fold(&self) -> Option<&[FoldStep]> {
        self.call_shape
            .fold
            // A run-time cache, outside the request's memory accounting
            // (`memory_get_usage()` must not move on a first `new`).
            .get_or_init(|| php_memlimit::persistent(|| self.compute_fold()))
            .as_deref()
    }

    #[cold]
    #[inline(never)]
    fn compute_fold(&self) -> Option<Box<[FoldStep]>> {
        use crate::opcode::{Op, Operand, PlaceBase, Seg};
        if self.call_shape() == CallShape::NOT_PLAIN
            || !self.uses.is_empty()
            || self.return_type.is_some()
        {
            return None;
        }
        let n = self.params.len();
        let mut steps = Vec::new();
        for op in self.code.ops.get(self.plain_first_op(n)..)? {
            match op {
                Op::Nop => {}
                Op::AssignObjProp {
                    place,
                    val: Operand::Cv(cv),
                    dst: None,
                } if (*cv as usize) < n && steps.len() < FOLD_STEPS => {
                    let p = self.code.places.get(*place as usize)?;
                    if !matches!(
                        (&p.base, p.segs.as_slice()),
                        (PlaceBase::This, [Seg::Prop(Operand::Lit(_))])
                    ) {
                        return None;
                    }
                    steps.push(FoldStep {
                        place: *place,
                        param: *cv,
                    });
                }
                Op::Return {
                    val: Operand::Unused,
                } => return Some(steps.into_boxed_slice()),
                _ => return None,
            }
        }
        None
    }

    #[cold]
    #[inline(never)]
    fn compute_call_shape(&self) -> u8 {
        if self.flags & (flags::DEPRECATED | flags::NODISCARD | flags::NO_BODY | flags::GENERATOR)
            != 0
            || self.code.num_statics != 0
            // (A `foreach` is no reason: the frame's iterators are sized
            // and released by the plain call's paths as by the generic
            // one, `FrameStack::push_in_place`, `Frame::clear_for_reuse`.)
            || self.code.num_finally != 0
            || self.params.len() > self.code.cv_names.len()
            // A variadic parameter too: the plain paths of the JIT and the
            // tape's whole-function calls bind every parameter from an
            // argument (a plain call passes at most one per parameter).
            || self.params.iter().any(|p| p.variadic || p.by_ref)
        {
            return CallShape::NOT_PLAIN;
        }
        if u32::try_from(self.params.len()).is_err() {
            return CallShape::NOT_PLAIN;
        }
        for (n, slot) in self.call_shape.first_ops.iter().enumerate() {
            slot.set(first_op_after_recv(&self.code, n));
        }
        if self
            .params
            .iter()
            .any(|p| p.ty.as_ref().is_some_and(|t| !t.is_mixed()))
        {
            CallShape::TYPED
        } else {
            CallShape::UNTYPED
        }
    }

    /// Whether the function is a generator.
    pub fn is_generator(&self) -> bool {
        self.flags & flags::GENERATOR != 0
    }
    /// Whether the function is a closure.
    pub fn is_closure(&self) -> bool {
        self.flags & flags::CLOSURE != 0
    }
}

/// The instruction a call of `code` with `n` passed arguments, each bound
/// into its compiled variable and needing no coercion, starts at: the
/// `RECV` of an argument that was passed has nothing left to do, so the
/// call starts after it (after a `RECV_INIT`'s default-value code, at its
/// `skip` label), in parameter order, up to the first op that is not the
/// next parameter's receive (`i_init_func_execute_data` skipping the
/// `RECV`s of passed arguments).
pub(crate) fn first_op_after_recv(code: &OpArray, n: usize) -> u32 {
    use crate::opcode::Op;
    let mut first_op: u32 = 0;
    for i in 0..n {
        match code.ops.get(first_op as usize) {
            Some(Op::Recv { n, .. }) if *n as usize == i => first_op += 1,
            Some(Op::RecvInit { n, skip, .. }) if *n as usize == i => first_op = *skip,
            _ => break,
        }
    }
    first_op
}

/// The handler of a builtin function.
pub type BuiltinFn = fn(&mut Vm, &mut Args) -> VmResult<Value>;

/// The parsed part of a builtin's stub signature (`zend_internal_arg_info`).
#[derive(Clone, Debug, Default)]
pub struct Signature {
    /// Parameters (from the stub signature).
    pub params: Vec<ParamInfo>,
    /// Default values of optional parameters (`None` when not a simple
    /// literal; the handler then supplies it).
    pub defaults: Vec<Option<Value>>,
    /// Number of required parameters.
    pub required: u32,
    /// Return type.
    pub return_type: Option<TypeDecl>,
}

impl<N> From<crate::builtins::sig::Sig<N>> for Signature {
    fn from(s: crate::builtins::sig::Sig<N>) -> Self {
        Signature {
            params: s.params,
            defaults: s.defaults,
            required: s.required,
            return_type: s.ret,
        }
    }
}

/// The stub text a builtin's [`Signature`] is parsed from on first use.
#[derive(Clone)]
enum StubText {
    /// Built already (no text to parse).
    None,
    /// A signature from the binary's read-only data.
    Static(&'static str),
    /// A signature built at run time.
    Owned(Box<str>),
}

/// A builtin (internal) function or method.
///
/// Its [`Signature`] (parameters, defaults, required count, return type) is
/// parsed from the stub text on first use: php-src has these tables
/// compiled in, and parsing all ~10,000 stubs at start-up cost about 14 M
/// instructions a process, while a script uses a handful. The signature's
/// fields read through `Deref` (`b.params`, `b.required`, ...) as when
/// they were plain fields.
#[derive(Clone)]
pub struct Builtin {
    /// Name as declared.
    pub name: ZStr,
    /// Flags (visibility/static for methods, `VARIADIC`, `RETURN_REF`).
    pub flags: u32,
    /// The handler.
    pub handler: BuiltinFn,
    /// The parsed signature, once parsed.
    sig: OnceCell<Signature>,
    /// The stub text `sig` is parsed from.
    text: StubText,
    /// [`Builtin::plain_call`], once worked out: [`PLAIN_UNKNOWN`],
    /// [`PLAIN_NO`], [`PLAIN_NAMED`], [`PLAIN_UNNAMED`] or
    /// [`PLAIN_NAMED_REF`].
    plain: Cell<u8>,
    /// The position of the first by-reference parameter
    /// ([`Builtin::plain_call_upto`]), [`NO_REF_PARAM`] when there is none;
    /// worked out with `plain`.
    first_ref: Cell<u8>,
    /// [`Builtin::accepts`], once worked out.
    accepts: OnceCell<Box<[u32]>>,
}

/// [`Builtin::plain_call`] not worked out yet.
const PLAIN_UNKNOWN: u8 = 0;
/// Not a plain call.
const PLAIN_NO: u8 = 1;
/// A plain call whose frame carries the function's name.
const PLAIN_NAMED: u8 = 2;
/// A plain call whose frame has no name: `call_user_func()` and
/// `call_user_func_array()` called by name, which php-src compiles to the
/// callback's own call (`zend_compile_func_cuf`), so no frame of theirs
/// shows in a backtrace.
const PLAIN_UNNAMED: u8 = 3;
/// [`PLAIN_NAMED`] for a function with a by-reference parameter: a plain
/// call only while the call's arguments stop before it
/// ([`Builtin::plain_call_upto`]); [`Builtin::plain_call`] answers
/// [`PlainCall::No`] for it.
const PLAIN_NAMED_REF: u8 = 4;
/// [`Builtin::first_ref`] of a function without by-reference parameters.
const NO_REF_PARAM: u8 = u8::MAX;

/// The bit of [`Builtin::accepts`] a value has: its `types::mask` type
/// bit (`null`, `false`, `true`, `int`, `float`, `string`, `array`,
/// `object`, `resource` are bits 0 to 8 there), [`ACCEPT_REF`] for a
/// reference. A parameter's entry has the bit when zpp takes the value as
/// it is, the test `call::accepts_without_coercion` makes.
#[inline]
pub(crate) fn accept_bit(v: &Value) -> u32 {
    use crate::types::mask;
    match v {
        Value::Null => mask::NULL,
        Value::Bool(false) => mask::FALSE,
        Value::Bool(true) => mask::TRUE,
        Value::Int(_) => mask::LONG,
        Value::Float(_) => mask::DOUBLE,
        Value::Str(_) => mask::STRING,
        Value::Array(_) => mask::ARRAY,
        Value::Object(_) => mask::OBJECT,
        Value::Resource(_) => mask::RESOURCE,
        Value::Reference(_) => ACCEPT_REF,
    }
}

/// [`accept_bit`] of a reference: only a parameter that takes anything
/// (untyped, `mixed`, by reference, unchecked) has it.
const ACCEPT_REF: u32 = 1 << 31;

/// The [`Builtin::accepts`] entry of parameter `p`: every bit when its
/// type is not checked against the argument before the handler runs (no
/// type, `mixed`, a by-reference parameter, whose type the handler checks
/// as `Z_PARAM_ARRAY_EX(.., 0, 1)` of `array_pop()` does, or one marked
/// unchecked), else the value tags its type takes without coercion
/// (`iterable` takes an array; an object only for `object`, a class type
/// is checked by the generic call).
fn param_accepts(p: &ParamInfo) -> u32 {
    use crate::types::mask;
    match &p.ty {
        Some(ty)
            if !p.by_ref && p.promote & crate::opcode::UNCHECKED_TYPE == 0 && !ty.is_mixed() =>
        {
            let scalar = mask::NULL
                | mask::BOOL
                | mask::LONG
                | mask::DOUBLE
                | mask::STRING
                | mask::ARRAY
                | mask::OBJECT
                | mask::RESOURCE;
            let iterable = if ty.mask & mask::ITERABLE != 0 {
                mask::ARRAY
            } else {
                0
            };
            (ty.mask & scalar) | iterable
        }
        _ => u32::MAX,
    }
}

/// How a call by name of a builtin can run on the tape ([`Builtin::plain_call`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlainCall {
    /// Through the generic call.
    No,
    /// Directly; the frame carries the function's name.
    Named,
    /// Directly; the frame has no name.
    Unnamed,
}

impl Builtin {
    /// A builtin with an already built signature.
    pub fn new(name: ZStr, sig: Signature, flags: u32, handler: BuiltinFn) -> Self {
        Builtin {
            name,
            flags,
            handler,
            sig: OnceCell::from(sig),
            text: StubText::None,
            plain: Cell::new(PLAIN_UNKNOWN),
            first_ref: Cell::new(NO_REF_PARAM),
            accepts: OnceCell::new(),
        }
    }

    /// A builtin whose signature is parsed from the static stub `text` on
    /// first use (`name` and `flags` come from the caller's scan of it).
    pub(crate) fn from_static_stub(
        name: ZStr,
        text: &'static str,
        flags: u32,
        handler: BuiltinFn,
    ) -> Self {
        Builtin {
            name,
            flags,
            handler,
            sig: OnceCell::new(),
            text: StubText::Static(text),
            plain: Cell::new(PLAIN_UNKNOWN),
            first_ref: Cell::new(NO_REF_PARAM),
            accepts: OnceCell::new(),
        }
    }

    /// [`Self::from_static_stub`] for a stub built at run time.
    pub(crate) fn from_stub(name: ZStr, text: &str, flags: u32, handler: BuiltinFn) -> Self {
        Builtin {
            name,
            flags,
            handler,
            sig: OnceCell::new(),
            text: StubText::Owned(text.into()),
            plain: Cell::new(PLAIN_UNKNOWN),
            first_ref: Cell::new(NO_REF_PARAM),
            accepts: OnceCell::new(),
        }
    }

    /// This builtin under another name (`alias`), sharing the stub.
    pub(crate) fn renamed(&self, name: ZStr) -> Self {
        Builtin {
            name,
            // Depends on the name.
            plain: Cell::new(PLAIN_UNKNOWN),
            ..self.clone()
        }
    }

    /// The signature, parsed from the stub on the first call.
    #[inline]
    pub fn signature(&self) -> &Signature {
        self.sig.get_or_init(|| self.parse_stub())
    }

    /// Whether a call by name of this function, all of whose arguments are
    /// passed by value and have the declared types, can run as the tape's
    /// direct internal call (`exec/tape/calls.rs`): no parameter is passed
    /// by reference and the function is neither deprecated nor
    /// `#[\NoDiscard]` (diagnostics before the call). A function with a
    /// frameless class is called without its frame there when its
    /// arguments allow it (`Vm::frameless_icall_prepared`). Worked out
    /// once.
    #[inline]
    pub(crate) fn plain_call(&self) -> PlainCall {
        match self.plain_code() {
            PLAIN_NAMED => PlainCall::Named,
            PLAIN_UNNAMED => PlainCall::Unnamed,
            _ => PlainCall::No,
        }
    }

    /// The worked-out [`Builtin::plain`] code. Not recursive (the slow path
    /// stores it, then it is read again), so that the two answers above
    /// inline into their callers: as a recursive function each was an
    /// out-of-line call of ~36 instructions in the tape's native call.
    #[inline(always)]
    #[allow(clippy::inline_always)] // a load and a test
    fn plain_code(&self) -> u8 {
        let code = self.plain.get();
        if code != PLAIN_UNKNOWN {
            return code;
        }
        self.plain_call_slow();
        self.plain.get()
    }

    /// [`Builtin::plain_call`] decided per call rather than per function:
    /// the answer for a call whose arguments stop before the returned
    /// position, the first by-reference parameter's (`usize::MAX` when
    /// there is none). php-src's `ZEND_SEND_VAL`/`ZEND_SEND_VAR` of the
    /// arguments before it are the same whatever comes after them, so
    /// `preg_match($re, $s)` or `str_replace($a, $b, $c)` is as plain as
    /// `strlen($s)`; an argument at or past the position is sent by
    /// reference (`ZEND_SEND_REF`, `calls::send_ref`) or the call is not
    /// plain. Worked out once.
    #[inline]
    pub(crate) fn plain_call_upto(&self) -> (PlainCall, usize) {
        match self.plain_code() {
            PLAIN_NAMED => (PlainCall::Named, usize::MAX),
            PLAIN_UNNAMED => (PlainCall::Unnamed, usize::MAX),
            PLAIN_NAMED_REF => (PlainCall::Named, usize::from(self.first_ref.get())),
            _ => (PlainCall::No, usize::MAX),
        }
    }

    /// [`Builtin::plain_call_upto`] inline, for a caller that has just
    /// called it (`calls::prepare`): the answer is worked out then, and
    /// a second out-of-line call is not needed to read it again.
    #[inline]
    pub(crate) fn plain_call_known(&self) -> (PlainCall, usize) {
        match self.plain.get() {
            PLAIN_NAMED => (PlainCall::Named, usize::MAX),
            PLAIN_UNNAMED => (PlainCall::Unnamed, usize::MAX),
            PLAIN_NAMED_REF => (PlainCall::Named, usize::from(self.first_ref.get())),
            PLAIN_UNKNOWN => self.plain_call_upto(),
            _ => (PlainCall::No, usize::MAX),
        }
    }

    /// Whether a plain call with `n` arguments sent one by reference
    /// ([`Builtin::plain_call_upto`]'s position is below `n`), from the
    /// two cells directly once the answer is worked out: the call into
    /// `plain_call_upto` cost a tape builtin call ~33 instructions.
    #[inline]
    pub(crate) fn sends_ref_within(&self, n: usize) -> bool {
        match self.plain.get() {
            PLAIN_NAMED_REF => usize::from(self.first_ref.get()) < n,
            PLAIN_UNKNOWN => self.plain_call_upto().1 < n,
            _ => false,
        }
    }

    /// Whether the parameter at `pos` is by reference (a position past the
    /// declared ones: the variadic one's), and whether it is
    /// `ZEND_ARG_SEND_PREFER_REF` (by reference when the argument is a
    /// variable, by value otherwise).
    #[inline]
    pub(crate) fn param_ref_at(&self, pos: usize) -> (bool, bool) {
        let ps = &self.signature().params;
        match ps.get(pos).or_else(|| ps.last().filter(|l| l.variadic)) {
            Some(p) if p.by_ref => (true, p.promote & crate::opcode::PREFER_REF != 0),
            _ => (false, false),
        }
    }

    /// Per parameter, the [`accept_bit`]s of the values zpp takes as they
    /// are (`call_builtin_impl`'s positional fast path; anything else is
    /// coerced or rejected there): argument `i` of a direct call is taken
    /// when `accept_bit(arg) & accepts[min(i, len - 1)] != 0`, a position
    /// past the declared ones being the variadic parameter's. Empty when
    /// every parameter takes anything, so a call of such a function tests
    /// nothing. Worked out once.
    #[inline]
    pub(crate) fn accepts(&self) -> &[u32] {
        self.accepts.get_or_init(|| self.accepts_slow())
    }

    #[cold]
    #[inline(never)]
    fn accepts_slow(&self) -> Box<[u32]> {
        let table: Vec<u32> = self.signature().params.iter().map(param_accepts).collect();
        if table.iter().all(|&m| m == u32::MAX) {
            Box::default()
        } else {
            table.into_boxed_slice()
        }
    }

    #[cold]
    #[inline(never)]
    fn plain_call_slow(&self) {
        let first_ref = self.signature().params.iter().position(|p| p.by_ref);
        let code = if self.flags & (flags::DEPRECATED | flags::NODISCARD) != 0 {
            PLAIN_NO
        } else if let Some(pos) = first_ref {
            // A position past `u8::MAX - 1` (none is) is never reached.
            self.first_ref
                .set(u8::try_from(pos).unwrap_or(NO_REF_PARAM - 1));
            PLAIN_NAMED_REF
        } else if matches!(
            self.name.as_bytes(),
            b"call_user_func" | b"call_user_func_array"
        ) {
            PLAIN_UNNAMED
        } else {
            PLAIN_NAMED
        };
        self.plain.set(code);
    }

    /// Parses the stub text. The tables are persistent in php-src
    /// (`zend_internal_arg_info`), so parsing them mid-request does not show
    /// in `memory_get_usage()`.
    #[cold]
    #[inline(never)]
    fn parse_stub(&self) -> Signature {
        let text: &str = match &self.text {
            StubText::None => return Signature::default(),
            StubText::Static(t) => t,
            StubText::Owned(t) => t,
        };
        php_memlimit::persistent(|| {
            let s = crate::builtins::sig::parse_sig_borrowed(text);
            debug_assert_eq!(
                s.variadic,
                crate::builtins::sig::sig_head(text).variadic,
                "variadic scan of {text}"
            );
            Signature::from(s)
        })
    }
}

impl std::ops::Deref for Builtin {
    type Target = Signature;

    #[inline]
    fn deref(&self) -> &Signature {
        self.signature()
    }
}

impl std::ops::DerefMut for Builtin {
    /// Parses the stub first if needed (`zend_test` edits the arginfo of
    /// some of its functions after registration).
    fn deref_mut(&mut self) -> &mut Signature {
        self.signature();
        // Worked out again from the edited signature.
        self.plain.set(PLAIN_UNKNOWN);
        self.accepts = OnceCell::new();
        self.sig.get_mut().expect("signature initialised above")
    }
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
    #[inline]
    pub fn params(&self) -> &[ParamInfo] {
        match self {
            Func::User(u) => &u.params,
            Func::Builtin(b) => &b.params,
        }
    }

    /// Flags.
    #[inline]
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
    ///
    /// Inline: `ZEND_SEND_*` asks it once per argument of every call, and
    /// out of line it was a call of ~30 instructions per argument (the
    /// builtin arm's lazy parameter list stays an out-of-line call on its
    /// first use only).
    #[inline]
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

    /// Whether this function is a closure.
    pub fn is_closure(&self) -> bool {
        self.flags() & flags::CLOSURE != 0
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

/// The handler's copy of a sent argument (a builtin call's [`Args`],
/// php-src's handler reading it in the frame): a handle's count
/// incremented inline, where `Value::clone` is an out-of-line call
/// dispatching on the tag (~23 instructions an argument).
#[inline(always)]
#[allow(clippy::inline_always)] // once per argument of a builtin call
pub(crate) fn copy_arg(v: &Value) -> Value {
    match v {
        Value::Str(s) => Value::Str(s.clone()),
        Value::Array(a) => Value::Array(a.clone()),
        Value::Object(o) => Value::Object(o.clone()),
        Value::Reference(r) => Value::Reference(r.clone()),
        _ => crate::call::clone_value(v),
    }
}

/// Empties a handler's argument copies front to back, as `Vec::clear`
/// releases them, without a `Drain` (whose out-of-line drop cost ~22
/// instructions a call) and without the out-of-line drop of a scalar.
#[inline(always)]
#[allow(clippy::inline_always)] // once per builtin call
fn release_arg_copies(values: &mut Vec<Value>) {
    for slot in values.iter_mut() {
        if matches!(
            slot,
            Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_)
        ) {
            continue;
        }
        // A copy whose original the frame still holds is not the last
        // handle: its count is decremented inline (`GC_DELREF`), without
        // the out-of-line drop glue of a `Value` and its dispatch on the
        // tag again (~30 instructions an argument). A last handle (the
        // handler replaced the copy) is left to the glue.
        match std::mem::take(slot) {
            Value::Str(s) => drop(s),
            Value::Array(a) if a.refcount() > 1 => drop(a),
            Value::Object(o) if o.refcount() > 1 => drop(o),
            Value::Reference(r) if r.refcount() > 1 => drop(r),
            other => drop(other),
        }
    }
    // Every element is a scalar now, which `clear`'s drop loop passes
    // over with a tag test each.
    values.clear();
}

impl Args {
    /// Releases a builtin call's arguments once its handler returned: the
    /// copies of the arguments front to back ([`release_arg_copies`]), the
    /// two vectors back home or to their pools
    /// ([`crate::vm::CallBufs::put_values`]), and the other fields emptied
    /// only when set. Everything is then empty, so the caller forgets the
    /// husk rather than run it through the drop glue: destructuring it by
    /// value moved its fifteen words to another stack slot first, and
    /// `put_val_vec`'s call with a drop call per value and the drop of the
    /// empty named-argument vector cost ~130 instructions of a two-argument
    /// call.
    #[inline(always)]
    #[allow(clippy::inline_always)] // once per builtin call
    pub(crate) fn release_into(&mut self, bufs: &mut crate::vm::CallBufs) {
        release_arg_copies(&mut self.values);
        bufs.put_values(std::mem::take(&mut self.values));
        bufs.put_passed(std::mem::take(&mut self.passed));
        // An empty vector that never allocated owns nothing.
        if self.named.capacity() != 0 {
            drop(std::mem::take(&mut self.named));
        }
        if self.this.is_some() {
            self.this = None;
        }
        if self.called.is_some() {
            self.called = None;
        }
        if self.func.is_some() {
            self.func = None;
        }
    }
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
            match self.values.get(n) {
                Some(Value::Reference(r)) => Some(r.get()),
                Some(v) => Some(v.clone()),
                None => None,
            }
        } else {
            None
        }
    }

    /// Argument `n` (dereferenced), `Null` if missing.
    pub fn at(&self, n: usize) -> Value {
        match self.values.get(n) {
            Some(Value::Reference(r)) => r.get(),
            Some(v) => v.clone(),
            None => Value::Null,
        }
    }

    /// Argument `n` as a borrowed value if passed and not a PHP reference.
    pub fn get_ref(&self, n: usize) -> Option<&Value> {
        if self.passed.get(n).copied().unwrap_or(false) {
            match self.values.get(n) {
                Some(Value::Reference(_)) => None,
                other => other,
            }
        } else {
            None
        }
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
            if crate::typed_refs::has_type_sources(&r) {
                crate::typed_refs::defer_typed_arg_write(r, v);
                return;
            }
            r.set(v);
        }
    }

    /// `$this` (panics if missing — only for instance methods).
    pub fn this_obj(&self) -> Obj {
        self.this
            .clone()
            .expect("instance method called without $this")
    }

    /// The function name (for messages).
    pub fn func_name(&self) -> String {
        self.func
            .as_ref()
            .map_or_else(String::new, |f| f.name.to_string_lossy())
    }

    /// [`Args::func_name`] borrowed instead of copied into a new `String`
    /// (a builtin's name is ASCII; one that is not UTF-8 gives `""`): for
    /// the builtins that pass their name along on every call (`count()`,
    /// `array_key_exists()`, ...), where the copy was an allocation and a
    /// UTF-8 scan per call.
    #[must_use]
    pub fn func_name_str(&self) -> &str {
        self.func
            .as_ref()
            .and_then(|f| std::str::from_utf8(f.name.as_bytes()).ok())
            .unwrap_or("")
    }
}

#[cfg(test)]
mod plain_call_tests {
    use super::{flags, Args, Builtin, PlainCall};
    use crate::{Vm, VmResult};
    use php_runtime::{Value, ZStr};

    fn handler(_: &mut Vm, _: &mut Args) -> VmResult<Value> {
        Ok(Value::Null)
    }

    fn builtin(stub: &str, fl: u32) -> Builtin {
        let name = stub.split('(').next().unwrap_or_default();
        Builtin::from_stub(ZStr::from(name), stub, fl, handler)
    }

    #[test]
    fn by_value_functions_are_plain_whole() {
        let b = builtin("strlen(string $string): int", 0);
        assert_eq!(b.plain_call(), PlainCall::Named);
        assert_eq!(b.plain_call_upto(), (PlainCall::Named, usize::MAX));
        let b = builtin(
            "call_user_func(callable $callback, mixed ...$args): mixed",
            flags::VARIADIC,
        );
        assert_eq!(b.plain_call_upto(), (PlainCall::Unnamed, usize::MAX));
    }

    #[test]
    fn by_reference_functions_are_plain_up_to_the_first_reference() {
        let b = builtin(
            "preg_match(string $pattern, string $subject, &$matches = null, int $flags = 0, int $offset = 0): int|false",
            0,
        );
        // Asked first through the per-call answer, then the per-function one.
        assert_eq!(b.plain_call_upto(), (PlainCall::Named, 2));
        assert_eq!(b.plain_call(), PlainCall::No);
        assert_eq!(b.param_ref_at(1), (false, false));
        assert_eq!(b.param_ref_at(2), (true, false));
        assert_eq!(b.param_ref_at(3), (false, false));
        let b = builtin("array_pop(array &$array): mixed", 0);
        assert_eq!(b.plain_call(), PlainCall::No);
        assert_eq!(b.plain_call_upto(), (PlainCall::Named, 0));
        let b = builtin("f(array|object $array): mixed", 0);
        assert_eq!(b.param_ref_at(0), (false, false));
        let b = builtin("g(@prefer-ref array|object &$array): mixed", 0);
        assert_eq!(b.param_ref_at(0), (true, true));
    }

    #[test]
    fn a_variadic_by_reference_parameter_covers_every_later_position() {
        let b = builtin(
            "sscanf(string $string, string $format, mixed &...$vars): array|int|null",
            flags::VARIADIC,
        );
        assert_eq!(b.plain_call_upto(), (PlainCall::Named, 2));
        assert_eq!(b.param_ref_at(2), (true, false));
        assert_eq!(b.param_ref_at(5), (true, false));
    }

    #[test]
    fn accepts_is_empty_when_every_parameter_takes_anything() {
        let b = builtin(
            "var_dump(mixed $value, mixed ...$values): void",
            flags::VARIADIC,
        );
        assert!(b.accepts().is_empty());
        let b = builtin("array_pop(array &$array): mixed", 0);
        assert!(b.accepts().is_empty());
        let b = builtin("f($x): mixed", 0);
        assert!(b.accepts().is_empty());
    }

    #[test]
    fn accepts_has_the_tags_each_type_takes_as_it_is() {
        use super::accept_bit;
        use php_runtime::ZArray;
        let b = builtin(
            "implode(array|string $separator, ?array $array = null): string",
            0,
        );
        let t = b.accepts();
        assert_eq!(t.len(), 2);
        let s = Value::Str(ZStr::from("x"));
        let a = Value::Array(ZArray::new());
        assert_ne!(accept_bit(&s) & t[0], 0);
        assert_ne!(accept_bit(&a) & t[0], 0);
        assert_eq!(accept_bit(&Value::Int(1)) & t[0], 0);
        assert_eq!(accept_bit(&Value::Null) & t[0], 0);
        assert_ne!(accept_bit(&Value::Null) & t[1], 0);
        assert_eq!(accept_bit(&Value::Bool(true)) & t[1], 0);
        let b = builtin("g(int|false $a, iterable $i, mixed $m): bool", 0);
        let t = b.accepts();
        assert_ne!(accept_bit(&Value::Bool(false)) & t[0], 0);
        assert_eq!(accept_bit(&Value::Bool(true)) & t[0], 0);
        assert_eq!(accept_bit(&Value::Float(1.0)) & t[0], 0);
        assert_ne!(accept_bit(&a) & t[1], 0);
        assert_eq!(t[2], u32::MAX);
        let r = Value::Reference(php_runtime::PhpRef::new(Value::Int(1)));
        assert_eq!(accept_bit(&r) & t[0], 0);
        assert_ne!(accept_bit(&r) & t[2], 0);
    }

    #[test]
    fn deprecated_functions_are_never_plain() {
        let b = builtin("f(array &$a): void", flags::DEPRECATED);
        assert_eq!(b.plain_call_upto(), (PlainCall::No, usize::MAX));
        assert_eq!(b.plain_call(), PlainCall::No);
    }
}

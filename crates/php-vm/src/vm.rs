//! The virtual machine: global state, frames and request lifecycle.

use crate::class::{Class, NameMap};
use crate::compiler::CompiledUnit;
use crate::func::{Func, UserFunction};
use crate::opcode::OpArray;
use php_runtime::{ArrayKey, HashTable, Obj, PhpRef, Value, ZStr};
use php_support::ini::IniRegistry;
use php_support::output::OutputStack;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::rc::Rc;

/// Non-local exits of the interpreter.
#[derive(Debug, Clone)]
pub enum Throw {
    /// A PHP exception is being thrown.
    Exception(Obj),
    /// `exit()`: unwind everything, run shutdown functions.
    Exit,
    /// A fatal error was reported: bail out.
    Fatal,
    /// The running fiber suspended (unwinds to `Fiber::start`/`resume`).
    FiberSuspend,
}

/// Result type of VM operations.
pub type VmResult<T> = Result<T, Throw>;

/// The names of a variable table's slots (`op_array->vars`).
///
/// Every frame of a function shares the compiled-variable names of its op
/// array; a table copies them only when a variable is created at run time
/// (`$$name`, `extract()`, the variables of an included file), as php-src
/// builds a symbol table (`zend_rebuild_symbol_table`) only then. The
/// names read as one slice either way (`Deref<Target = [ZStr]>`).
#[derive(Default)]
pub struct VarNames {
    /// The op array whose compiled-variable names are the slots' names,
    /// while none was added at run time.
    shared: Option<Rc<OpArray>>,
    /// The table's own names, once a variable was added at run time
    /// (empty while `shared` is set).
    owned: Vec<ZStr>,
}

impl VarNames {
    /// The compiled-variable names of `code`, shared with it.
    #[must_use]
    pub fn shared(code: &Rc<OpArray>) -> Self {
        VarNames {
            shared: Some(code.clone()),
            owned: Vec::new(),
        }
    }

    /// Makes the names those of `code`'s compiled variables again, keeping
    /// a small owned buffer for the next run-time copy.
    fn share(&mut self, code: &Rc<OpArray>) {
        self.shared = Some(code.clone());
        self.owned.clear();
    }

    /// Drops all names; a small owned buffer keeps its capacity for reuse.
    fn clear(&mut self) {
        self.shared = None;
        clear_frame_buffer(&mut self.owned);
    }

    /// The names as a list this table owns, copying shared ones first
    /// (once per table, like php-src's symbol-table rebuild).
    pub fn to_mut(&mut self) -> &mut Vec<ZStr> {
        if let Some(code) = self.shared.take() {
            self.owned.clear();
            self.owned.extend_from_slice(&code.cv_names);
        }
        &mut self.owned
    }

    /// The capacity of the owned list (0 while the names are shared and
    /// no buffer is kept).
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.owned.capacity()
    }
}

impl std::ops::Deref for VarNames {
    type Target = [ZStr];

    #[inline]
    fn deref(&self) -> &[ZStr] {
        match &self.shared {
            Some(code) => &code.cv_names,
            None => &self.owned,
        }
    }
}

impl std::fmt::Debug for VarNames {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// A variable table (symbol table): compiled variables plus dynamically
/// created ones.
#[derive(Debug, Default)]
pub struct VarStore {
    /// Values (`None` = undefined).
    pub slots: Vec<Option<Value>>,
    /// Names, parallel to `slots`.
    pub names: VarNames,
    index: Option<HashMap<ZStr, u32>>,
}

impl VarStore {
    /// A store for variables with the given names (its own copy).
    pub fn with_names(names: &[ZStr]) -> Self {
        VarStore {
            slots: vec![None; names.len()],
            names: VarNames {
                shared: None,
                owned: names.to_vec(),
            },
            index: None,
        }
    }

    /// A store for the compiled variables of `code`, sharing their names
    /// with the op array (a php-src frame reads `op_array->vars`).
    #[must_use]
    pub fn for_code(code: &Rc<OpArray>) -> Self {
        VarStore {
            slots: vec![None; code.cv_names.len()],
            names: VarNames::shared(code),
            index: None,
        }
    }

    fn build_index(&mut self) {
        if self.index.is_none() {
            let mut m = HashMap::with_capacity(self.names.len());
            for (i, n) in self.names.iter().enumerate() {
                m.insert(n.clone(), u32::try_from(i).unwrap_or(0));
            }
            self.index = Some(m);
        }
    }

    /// Finds a variable slot by name.
    pub fn find(&mut self, name: &[u8]) -> Option<u32> {
        if self.names.len() < 8 {
            return self
                .names
                .iter()
                .position(|n| n.as_bytes() == name)
                .map(|p| u32::try_from(p).unwrap_or(0));
        }
        self.build_index();
        self.index.as_ref().and_then(|m| m.get(name).copied())
    }

    /// Finds or creates a variable slot by name.
    pub fn find_or_add(&mut self, name: &[u8]) -> u32 {
        if let Some(i) = self.find(name) {
            return i;
        }
        let i = u32::try_from(self.slots.len()).unwrap_or(0);
        self.slots.push(None);
        let z = ZStr::from(name);
        self.names.to_mut().push(z.clone());
        if let Some(m) = &mut self.index {
            m.insert(z, i);
        }
        i
    }

    /// The variables as the array a `$GLOBALS` read yields
    /// (`ZEND_FETCH_GLOBALS` -> `zend_proptable_to_symtable(.., 1)`):
    /// undefined slots (php-src's UNDEF `INDIRECT` entries) are skipped, a
    /// reference only the table holds (refcount 1) is copied as its value
    /// (`zend_array_dup_value`), and canonical numeric names such as
    /// `${'1'}` become integer keys (`ZEND_HANDLE_NUMERIC`).
    pub fn to_array(&self) -> HashTable {
        let mut t = HashTable::new();
        for (n, v) in self.names.iter().zip(&self.slots) {
            let Some(v) = v else { continue };
            let v = match v {
                Value::Reference(r) if r.refcount() == 1 => r.get(),
                other => other.clone(),
            };
            t.insert(php_runtime::ArrayKey::from_zstr(n), v);
        }
        t
    }
}

/// State of a finally block in a frame.
#[derive(Debug, Clone, Default)]
pub enum FinState {
    /// Not active.
    #[default]
    None,
    /// Entered by `FastCall`: return to this op.
    Return(usize),
    /// Entered while unwinding an exception.
    Exception(Obj),
}

/// Where a frame's return value goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetTarget {
    /// Discarded.
    Discard,
    /// Stored in the caller's temporary.
    Tmp(u32),
}

/// The kind of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// A user function, method or closure.
    Function,
    /// The main script.
    Main,
    /// An included file.
    Include,
    /// `eval()`'d code.
    Eval,
    /// A builtin function call (for backtraces).
    Builtin,
    /// Constant expression evaluation.
    ConstExpr,
}

/// A registered class loader: the callable as `spl_autoload_functions()`
/// reports it and the call it was resolved to when it was registered
/// (`zend_fcall_info_cache`). The resolution happens in the registering
/// scope, so a loader that names a method inaccessible from the code
/// triggering the autoload (or a `__call` trampoline) is still called
/// as registered.
#[derive(Debug, Clone)]
pub struct ClassLoader {
    /// The callable value.
    pub callable: Value,
    /// The resolved call; `None` resolves `callable` at call time.
    pub fcc: Option<PendingCall>,
}

impl ClassLoader {
    /// A loader resolved at call time.
    #[must_use]
    pub fn unresolved(callable: Value) -> Self {
        Self {
            callable,
            fcc: None,
        }
    }

    /// Whether both entries stand for the same registered callable
    /// (identical values).
    #[must_use]
    pub fn is_same(&self, other: &Self) -> bool {
        php_runtime::ops::is_identical(&self.callable, &other.callable).unwrap_or(false)
    }
}

/// A call being prepared (`INIT_FCALL` .. `DO_FCALL`).
#[derive(Debug, Clone)]
pub struct PendingCall {
    /// The function.
    pub func: Func,
    /// `$this`.
    pub this: Option<Obj>,
    /// The scope (declaring class of a method).
    pub scope: Option<Rc<Class>>,
    /// The called scope (`static::`).
    pub called: Option<Rc<Class>>,
    /// Arguments by position (`None` = not passed).
    pub args: Vec<Option<Value>>,
    /// Extra named arguments (collected by a variadic or `__call`).
    pub named: Vec<(ZStr, Value)>,
    /// The closure object being called.
    pub closure: Option<Obj>,
    /// The original method name when calling `__call`/`__callStatic`.
    pub magic_name: Option<ZStr>,
    /// Whether named arguments were used.
    pub has_named: bool,
    /// Whether `INIT_FCALL` resolved a statically named call.
    pub direct: bool,
    /// Whether this is a constructor call from `new`.
    pub is_ctor: bool,
    /// Placeholders of a partial application.
    pub pfa: Option<Box<crate::pfa::PfaArgs>>,
    /// Coerce arguments in strict mode (`ZEND_CALL_STRICT_TYPES` for calls
    /// made from Rust, e.g. attribute instantiation).
    pub strict: bool,
    /// Made by `ZEND_INIT_DYNAMIC_CALL` (`$f()`): `ZEND_CALL_DYNAMIC`.
    pub dynamic: bool,
}

/// Reusable buffers of the call protocol (see [`Vm::call_bufs`]).
#[derive(Debug, Default)]
pub(crate) struct CallBufs {
    /// Pending-call argument vectors.
    pub(crate) args: Vec<Vec<Option<Value>>>,
    /// ZPP value vectors.
    pub(crate) values: Vec<Vec<Value>>,
    /// ZPP passed-flag vectors.
    pub(crate) passed: Vec<Vec<bool>>,
    /// Prewarmed and pooled user function frames.
    pub(crate) user_frames: Vec<Frame>,
    /// The run-time cache of `ZEND_INIT_METHOD_CALL`.
    pub(crate) methods: crate::call::MethodCache,
}

impl CallBufs {
    /// Pools pre-filled at startup so that reaching steady state does not
    /// allocate while user code runs (`memory_get_usage()` deltas, e.g.
    /// Zend/tests/bug79514.phpt, must not see the pools grow).
    pub(crate) fn prewarmed(empty_code: &Rc<OpArray>) -> Self {
        let mut b = CallBufs {
            args: Vec::with_capacity(16),
            values: Vec::with_capacity(16),
            passed: Vec::with_capacity(16),
            user_frames: Vec::with_capacity(8),
            methods: crate::call::MethodCache::default(),
        };
        for _ in 0..4 {
            b.args.push(Vec::with_capacity(8));
            b.values.push(Vec::with_capacity(8));
            b.passed.push(Vec::with_capacity(8));
            b.user_frames.push(Frame::new_pooled(empty_code.clone()));
        }
        b
    }
}

impl PendingCall {
    /// A call without object context.
    pub fn new(func: Func) -> Self {
        PendingCall {
            func,
            this: None,
            scope: None,
            called: None,
            args: Vec::new(),
            named: Vec::new(),
            closure: None,
            magic_name: None,
            has_named: false,
            direct: false,
            is_ctor: false,
            pfa: None,
            strict: false,
            dynamic: false,
        }
    }
}

/// An active foreach loop.
#[derive(Debug)]
pub enum ForeachIter {
    /// By-value over an array snapshot.
    Array {
        /// The array.
        arr: php_runtime::ZArray,
        /// Next position.
        pos: u32,
    },
    /// By-reference over an array in a variable.
    ArrayRef {
        /// The reference holding the array.
        r: PhpRef,
        /// The registered iterator.
        id: php_runtime::IterId,
    },
    /// Over an object's properties.
    Props {
        /// The object, or the reference holding it for by-reference iteration.
        subject: Value,
        /// Identity of the last property target, without keeping it alive.
        target: std::rc::Weak<dyn php_runtime::PhpObject>,
        /// Snapshot of (key, name) pairs.
        keys: Vec<(Value, ZStr, Option<ZStr>)>,
        /// Next index.
        pos: usize,
        /// By reference.
        by_ref: bool,
    },
    /// An `Iterator` object.
    Iterator {
        /// The iterator.
        obj: Obj,
        /// Whether `rewind` has been done / first fetch.
        started: bool,
        /// The `zend_object_iterator`'s own handle, released after `obj`.
        handle: crate::object::HandleGuard,
    },
    /// A generator.
    Generator {
        /// The generator.
        obj: Obj,
        /// First fetch done.
        started: bool,
        /// The `zend_object_iterator`'s own handle, released after `obj`.
        handle: crate::object::HandleGuard,
    },
    /// A native iterator (extension `get_iterator` hook), whether the first
    /// fetch is done, and the iterator's own handle (released last).
    Native(NativeIter, bool, crate::object::HandleGuard),
    /// An empty iteration (non-iterable subject).
    Empty,
}

/// Wrapper for native iterators in `foreach`.
pub struct NativeIter(pub Box<dyn crate::ext::NativeIterator>);

impl std::fmt::Debug for NativeIter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeIter")
    }
}

/// A call frame.
///
/// The fields are declared in the order php-src's `zend_leave_helper`
/// releases what a frame holds, so that dropping a `Frame` (unwinding,
/// `frames.truncate`, a generator destroyed with its suspended frame) and
/// recycling one ([`Frame::clear_for_pool`]) queue destructors in the same
/// order: temporaries, iterators, finally states and unfinished calls (all
/// empty after a normal return: an exception releases them before leaving
/// the frame), then the compiled variables in order
/// (`i_free_compiled_variables`), the extra named arguments
/// (`zend_free_extra_named_params`), the extra positional arguments
/// (`zend_vm_stack_free_extra_args_ex`), `$this` and the closure object
/// (`OBJ_RELEASE`), and last the state no PHP code can observe.
#[derive(Debug)]
pub struct Frame {
    /// Temporaries.
    pub tmps: Vec<Value>,
    /// Foreach iterators.
    pub iters: Vec<Option<ForeachIter>>,
    /// Finally states.
    pub fins: Vec<FinState>,
    /// Pending calls.
    pub calls: Vec<PendingCall>,
    /// Variables.
    pub vars: Rc<RefCell<VarStore>>,
    /// Extra named args.
    pub named: Vec<(ZStr, Value)>,
    /// Arguments as passed (positional; `None` = skipped, or already moved
    /// into its compiled variable when the frame was pushed).
    pub args: Vec<Option<Value>>,
    /// `$this`.
    pub this: Option<Obj>,
    /// The closure being executed.
    pub closure: Option<Obj>,
    /// The generator this frame belongs to.
    pub generator: Option<Obj>,
    /// The exception being matched by `Catch` ops.
    pub pending_exc: Option<Obj>,
    /// Static variable storage for this call (closures have their own).
    pub statics: Option<Rc<RefCell<Vec<Option<PhpRef>>>>>,
    /// The function (None for top-level code).
    pub func: Option<Func>,
    /// The user function (for user frames).
    pub ufunc: Option<Rc<UserFunction>>,
    /// The code.
    pub code: Rc<OpArray>,
    /// Next instruction.
    pub ip: usize,
    /// CV index → slot index in `vars` (identity when `None`).
    pub cv_map: Option<Rc<[u32]>>,
    /// Class scope (`self`).
    pub scope: Option<Rc<Class>>,
    /// Called scope (`static`).
    pub called: Option<Rc<Class>>,
    /// Where the return value goes.
    pub ret: RetTarget,
    /// Frame kind.
    pub kind: FrameKind,
    /// Whether this is a constructor call (`new` result is the object).
    pub is_ctor: bool,
    /// The `error_reporting` masks saved by the `@` operators active in
    /// this frame (`ZEND_BEGIN_SILENCE`), restored when an exception
    /// leaves them.
    pub silence_saved: Vec<i64>,
    /// Builtin being executed (for backtraces).
    pub builtin_name: Option<ZStr>,
    /// Original method name for `__call` frames.
    pub magic_name: Option<ZStr>,
    /// Whether arguments were passed from strict-types code.
    pub caller_strict: bool,
    /// Whether the call came from internal code (no "called in" suffix).
    pub called_from_internal: bool,
    /// Whether this invocation has an active begin/end observation.
    pub(crate) observed: bool,
    /// VM implementation frames must not be exposed as PHP calls.
    pub(crate) observer_hidden: bool,
}

/// Keep small call buffers allocation-free without retaining exceptional frame sizes.
#[inline]
fn clear_frame_buffer<T>(buffer: &mut Vec<T>) {
    if buffer.capacity() > 256 {
        *buffer = Vec::new();
    } else {
        buffer.clear();
    }
}

/// Empties `v` and fills it with `n` values made by `make`: the
/// `Vec::resize` of a pooled frame buffer, without the generic growth
/// machinery (`extend_with`/`extend_trusted` cost ~45 instructions per
/// call), for buffers that usually have the capacity already and a small
/// `n` (the temporaries, iterators, finally slots and compiled variables
/// of one function).
#[inline]
fn refill_with<T>(v: &mut Vec<T>, n: usize, make: impl Fn() -> T) {
    v.clear();
    if n != 0 {
        v.reserve(n);
        for _ in 0..n {
            v.push(make());
        }
    }
}

/// Moves the last frame of `from` onto the end of `to` with one copy.
///
/// php-src reuses the VM stack memory in place
/// (`zend_vm_stack_push_call_frame` / `zend_vm_stack_free_call_frame`); the
/// frame pool stands in for that, and a `Frame` is a few hundred bytes, so
/// how it travels matters: `Vec::pop` + `Vec::push` and `extend(drain(..))`
/// both copy it twice, through a temporary (measured at 227 instructions
/// per transfer). Returns `false`, moving nothing, when `from` is empty.
#[inline]
pub(crate) fn move_last_frame(from: &mut Vec<Frame>, to: &mut Vec<Frame>) -> bool {
    let Some(last) = from.len().checked_sub(1) else {
        return false;
    };
    to.reserve(1);
    let len = to.len();
    // SAFETY: `from[last]` is an initialised element, and `to` has room
    // for one more element after `reserve`. The frame's bytes are copied
    // once into that spare slot; `from` then stops owning the frame (its
    // length is cut below `last`) and `to` starts owning the copy (its
    // length is raised over it). Nothing between the copy and the two
    // length stores can panic, so the frame is owned by exactly one of the
    // two vectors at every point: this is `Vec::pop` followed by
    // `Vec::push` without the intermediate value.
    unsafe {
        std::ptr::copy_nonoverlapping(from.as_ptr().add(last), to.as_mut_ptr().add(len), 1);
        from.set_len(last);
        to.set_len(len + 1);
    }
    true
}

impl Frame {
    /// Creates a pre-allocated empty frame for the pool.
    pub fn new_pooled(empty_code: Rc<OpArray>) -> Self {
        Frame {
            tmps: Vec::with_capacity(16),
            iters: Vec::with_capacity(4),
            fins: Vec::with_capacity(2),
            calls: Vec::with_capacity(4),
            vars: Rc::new(RefCell::new(VarStore {
                slots: Vec::with_capacity(8),
                names: VarNames::default(),
                index: None,
            })),
            named: Vec::with_capacity(4),
            args: Vec::with_capacity(8),
            this: None,
            closure: None,
            generator: None,
            pending_exc: None,
            statics: None,
            func: None,
            ufunc: None,
            code: empty_code,
            ip: 0,
            cv_map: None,
            scope: None,
            called: None,
            ret: RetTarget::Discard,
            kind: FrameKind::Function,
            is_ctor: false,
            silence_saved: Vec::with_capacity(2),
            builtin_name: None,
            magic_name: None,
            caller_strict: false,
            called_from_internal: false,
            observed: false,
            observer_hidden: false,
        }
    }

    /// Releases everything the frame holds, in the order a dropped `Frame`
    /// releases it (see the type's documentation: php-src's
    /// `zend_leave_helper` order), and leaves it ready for the pool:
    /// ordinary buffers keep their capacity, oversized ones are released,
    /// and a variable store shared with another frame is left to that
    /// frame and replaced.
    pub fn clear_for_pool(&mut self, empty_code: &Rc<OpArray>) {
        clear_frame_buffer(&mut self.tmps);
        clear_frame_buffer(&mut self.iters);
        clear_frame_buffer(&mut self.fins);
        clear_frame_buffer(&mut self.calls);
        if let Some(rc_mut) = Rc::get_mut(&mut self.vars) {
            let vs = rc_mut.get_mut();
            clear_frame_buffer(&mut vs.slots);
            vs.names.clear();
            vs.index = None;
        } else {
            self.vars = Rc::new(RefCell::new(VarStore::default()));
        }
        clear_frame_buffer(&mut self.named);
        clear_frame_buffer(&mut self.args);
        self.this = None;
        self.closure = None;
        self.generator = None;
        self.pending_exc = None;
        self.statics = None;
        self.func = None;
        self.ufunc = None;
        self.code = empty_code.clone();
        self.ip = 0;
        self.cv_map = None;
        self.scope = None;
        self.called = None;
        self.ret = RetTarget::Discard;
        self.is_ctor = false;
        clear_frame_buffer(&mut self.silence_saved);
        self.builtin_name = None;
        self.magic_name = None;
        self.caller_strict = false;
        self.called_from_internal = false;
        self.observed = false;
        self.observer_hidden = false;
    }

    /// Re-initializes a pooled frame for a new user function call, leaving
    /// the compiled variables empty for the caller to fill (the arguments
    /// move into them, [`crate::call`]'s `init_cvs`).
    pub(crate) fn reinit_for_call(&mut self, code: Rc<OpArray>, kind: FrameKind) {
        self.kind = kind;
        self.ip = 0;
        refill_with(&mut self.tmps, code.num_tmps as usize, || Value::Null);
        refill_with(&mut self.iters, code.num_iters as usize, || None);
        refill_with(&mut self.fins, code.num_finally as usize, || FinState::None);
        self.calls.clear();
        self.silence_saved.clear();
        if let Some(rc_mut) = Rc::get_mut(&mut self.vars) {
            let vs = rc_mut.get_mut();
            vs.slots.clear();
            // The CV names stay the op array's (`op_array->vars`): no
            // per-call copy of them.
            vs.names.share(&code);
            vs.index = None;
        } else {
            let mut vs = VarStore::for_code(&code);
            vs.slots.clear();
            self.vars = Rc::new(RefCell::new(vs));
        }
        self.code = code;
    }

    /// Re-initializes a pooled frame for a new user function call, with
    /// every compiled variable undefined.
    pub fn reinit_for_user(&mut self, code: Rc<OpArray>, kind: FrameKind) {
        self.reinit_for_call(code, kind);
        let ncv = self.code.cv_names.len();
        if let Some(rc_mut) = Rc::get_mut(&mut self.vars) {
            refill_with(&mut rc_mut.get_mut().slots, ncv, || None);
        }
    }

    /// Creates a frame for `code` with its own variable store, whose
    /// compiled-variable names are shared with the op array.
    pub fn new(code: Rc<OpArray>, kind: FrameKind) -> Self {
        let vars = Rc::new(RefCell::new(VarStore::for_code(&code)));
        Frame::with_vars(code, kind, vars, None)
    }

    /// Creates a frame sharing an existing variable store.
    pub fn with_vars(
        code: Rc<OpArray>,
        kind: FrameKind,
        vars: Rc<RefCell<VarStore>>,
        cv_map: Option<Rc<[u32]>>,
    ) -> Self {
        let ntmps = code.num_tmps as usize;
        let niters = code.num_iters as usize;
        let nfin = code.num_finally as usize;
        Frame {
            // Empty vectors are built without the generic `from_elem` /
            // `collect` machinery (builtin frames and most functions need
            // no iterators or finally slots; this matters in debug builds).
            tmps: if ntmps == 0 {
                Vec::new()
            } else {
                vec![Value::Null; ntmps]
            },
            iters: if niters == 0 {
                Vec::new()
            } else {
                (0..niters).map(|_| None).collect()
            },
            fins: if nfin == 0 {
                Vec::new()
            } else {
                vec![FinState::None; nfin]
            },
            calls: Vec::new(),
            vars,
            named: Vec::new(),
            args: Vec::new(),
            this: None,
            closure: None,
            generator: None,
            pending_exc: None,
            statics: None,
            func: None,
            ufunc: None,
            code,
            ip: 0,
            cv_map,
            scope: None,
            called: None,
            ret: RetTarget::Discard,
            kind,
            is_ctor: false,
            silence_saved: Vec::new(),
            builtin_name: None,
            magic_name: None,
            caller_strict: false,
            called_from_internal: false,
            observed: false,
            observer_hidden: false,
        }
    }

    /// The slot index of compiled variable `cv`.
    #[inline]
    pub fn cv_idx(&self, cv: u32) -> usize {
        match &self.cv_map {
            None => cv as usize,
            Some(m) => m[cv as usize] as usize,
        }
    }

    /// Looks at compiled variable `cv` in place (php-src's `EX_VAR(cv)`):
    /// `read` gets the slot's value (`None` = undefined), and what it
    /// returns cannot borrow from it.
    ///
    /// Unlike `self.vars.borrow()`, no borrow guard is taken: the borrow
    /// flag is only checked, never written, so the hot reads (operand
    /// peeks, `read_cv`) do no read-modify-write on the store and the
    /// compiler can keep the frame in registers across them. `read` is a
    /// plain function (a closure without captures), so it can reach
    /// neither the VM nor this store: it can only inspect or clone the
    /// value, which runs no PHP code. (It must not dig the store out of
    /// global state either; crate-private for that reason.)
    ///
    /// # Panics
    /// Like `RefCell::borrow`, if the store is mutably borrowed.
    #[inline(always)]
    #[allow(clippy::inline_always)] // `read` must become a direct (inlined) call
    pub(crate) fn peek_cv<R>(&self, cv: u32, read: fn(Option<&Value>) -> R) -> R {
        let idx = self.cv_idx(cv);
        // SAFETY: `try_borrow_unguarded` fails while a mutable borrow of the
        // store is active, so no `&mut VarStore` exists now. The shared
        // reference is only handed to `read`, a non-capturing function that
        // cannot name this store (it receives one slot and nothing else),
        // and `R` cannot borrow from it (the reference's lifetime is
        // higher-ranked), so no mutable borrow can begin while it is alive.
        let Ok(vs) = (unsafe { self.vars.try_borrow_unguarded() }) else {
            store_mutably_borrowed()
        };
        read(vs.slots.get(idx).and_then(Option::as_ref))
    }

    /// `$cv[key]` looked up in place: the element of the array compiled
    /// variable `cv` holds (directly or through a reference), dereferenced
    /// and cloned; `None` when the variable is undefined, holds no array or
    /// the array has no such key. The keyed form of [`Frame::peek_cv`]
    /// (whose `read` cannot capture the key); the lookup and the clone run
    /// no PHP code.
    ///
    /// # Panics
    /// Like `RefCell::borrow`, if the store is mutably borrowed.
    #[inline]
    pub(crate) fn peek_cv_dim(&self, cv: u32, key: &ArrayKey) -> Option<Value> {
        let idx = self.cv_idx(cv);
        // SAFETY: as in `peek_cv`: `try_borrow_unguarded` fails while a
        // mutable borrow of the store is active, so no `&mut VarStore`
        // exists now, and the shared reference is used only here, to look
        // up and clone one element (a refcount bump, or the clone of a
        // reference's inner value: no PHP code, no access to the store),
        // and is gone before this function returns.
        let Ok(vs) = (unsafe { self.vars.try_borrow_unguarded() }) else {
            store_mutably_borrowed()
        };
        match vs.slots.get(idx)? {
            Some(Value::Array(a)) => a.get(key).map(Value::deref),
            Some(Value::Reference(r)) => match &*r.try_borrow()? {
                Value::Array(a) => a.get(key).map(Value::deref),
                _ => None,
            },
            _ => None,
        }
    }

    /// The current line.
    pub fn line(&self) -> u32 {
        self.code.line(self.ip.saturating_sub(1))
    }
}

/// [`Frame::peek_cv`] on a mutably borrowed store: the panic
/// `RefCell::borrow` raises.
#[cold]
#[inline(never)]
#[track_caller]
fn store_mutably_borrowed() -> ! {
    panic!("already mutably borrowed: BorrowError")
}

/// An entry of the error handler stack.
#[derive(Debug, Clone)]
pub struct ErrorHandler {
    /// The callable (`Null` = none).
    pub callback: Value,
    /// The error types it handles.
    pub mask: i64,
}

/// Information about the last error (`error_get_last`).
#[derive(Debug, Clone)]
pub struct LastError {
    /// Type bits.
    pub level: i64,
    /// Message.
    pub message: Vec<u8>,
    /// File.
    pub file: ZStr,
    /// Line.
    pub line: u32,
}

/// The PHP virtual machine (one request).
pub struct Vm {
    /// Call frames.
    pub frames: Vec<Frame>,
    /// Functions by lower-case name (`EG(function_table)`). A [`NameMap`],
    /// like php-src's tables hashed with the unseeded
    /// `zend_inline_hash_func`; nothing depends on its iteration order
    /// (declaration order is kept where it is observable).
    pub functions: NameMap<Func>,
    /// Classes by lower-case name (`EG(class_table)`, a [`NameMap`]).
    pub classes: NameMap<Rc<Class>>,
    /// Class declaration order (lower-case names) for `get_declared_classes`.
    pub class_order: Vec<ZStr>,
    /// Constants by name (`EG(zend_constants)`, a [`NameMap`]).
    pub constants: NameMap<Value>,
    /// User-defined constant names in definition order.
    pub user_constants: Vec<ZStr>,
    /// The global variable table.
    pub globals: Rc<RefCell<VarStore>>,
    /// INI settings.
    pub ini: IniRegistry,
    /// Complete startup configuration supplied by the SAPI, if available.
    pub(crate) configuration: Option<php_support::ini::IniMap>,
    /// Output buffering stack.
    pub output: OutputStack<Value>,
    /// Standard output.
    pub stdout: Box<dyn Write>,
    /// `error_reporting`.
    pub error_reporting: i64,
    /// User error handlers.
    pub error_handlers: Vec<ErrorHandler>,
    /// User exception handlers.
    pub exception_handlers: Vec<Value>,
    /// Included files (resolved paths), in order.
    pub included: Vec<ZStr>,
    /// Set of included files.
    pub included_set: HashSet<ZStr>,
    /// Registered class loaders, in call order
    /// (`zend_class_autoload_functions`).
    pub autoloaders: Vec<ClassLoader>,
    /// Classes currently being autoloaded.
    pub autoloading: HashSet<ZStr>,
    /// The dummy call frames of `ReflectionAttribute::newInstance()`
    /// (`zend_get_attribute_object`): the index of the builtin frame and
    /// the attribute's file and line, which calls made from it report as
    /// their call site.
    pub attribute_call_sites: Vec<(usize, ZStr, u32)>,
    /// Exit status.
    pub exit_status: i32,
    /// Shutdown functions.
    pub shutdown_functions: Vec<(Value, Vec<Value>)>,
    /// The last error.
    pub last_error: Option<LastError>,
    /// `EG(last_fatal_error_backtrace)`: the stack recorded for the last
    /// fatal error under `fatal_error_backtraces` (cleared by every error).
    pub last_fatal_error_backtrace: Option<php_runtime::ZArray>,
    /// Exceptions/bailouts raised inside runtime callbacks.
    pub pending_throw: Option<Throw>,
    /// The last user output handler call threw (`retval` UNDEF), even if
    /// the user exception handler took the exception at shutdown.
    pub(crate) output_handler_threw: bool,
    /// The main script path.
    pub script_path: ZStr,
    /// Command line arguments.
    pub argv: Vec<ZStr>,
    /// Class declarations already bound (by address).
    pub bound_decls: HashMap<usize, Rc<Class>>,
    /// Compiled units that declare functions, closures, classes or
    /// attributed constants (Reflection finds closures here; preloading
    /// images them). Units declaring nothing are freed once run.
    pub units: Vec<Rc<CompiledUnit>>,
    /// `precision`.
    pub precision: i64,
    /// `serialize_precision`.
    pub serialize_precision: i64,
    /// Whether we are in shutdown.
    pub in_shutdown: bool,
    /// Nesting depth of user error handler calls.
    pub in_error_handler: u32,
    /// Set just before reporting a message that `php_verror` already
    /// HTML-escaped (and may have given a docref link) under `html_errors`;
    /// consumed by the next report so the display does not escape it again.
    pub(crate) error_msg_html_ready: bool,
    /// A delayed `memory_limit` fatal is being reported
    /// (`zend_handle_delayed_oom`, heap `overflow` set): user output
    /// handlers do not run while its buffers are discarded.
    pub(crate) reporting_oom: bool,
    /// Builtin-specific state.
    pub ext: crate::builtins::ExtState,
    /// Anonymous class counter.
    pub anon_counter: u32,
    /// Depth of nested `execute` calls (stack protection).
    pub depth: u32,
    /// Strings already reported by `E_DEPRECATED` once-only notices.
    pub once_flags: HashSet<String>,
    /// Ticks functions.
    pub tick_functions: Vec<Value>,
    /// Extension interrupt handlers (run when the interrupt flag is set).
    pub interrupt_handlers: Vec<crate::interrupt::InterruptFn>,
    /// Extension tick handlers (`php_add_tick_function`).
    pub tick_hooks: Vec<crate::interrupt::TickFn>,
    /// `register_tick_function()` entries.
    pub(crate) user_ticks: Vec<crate::interrupt::UserTick>,
    /// `EG(ticks_count)`.
    pub(crate) ticks_count: u32,
    /// `zend_fiber_switch_block()` depth.
    pub fiber_switch_block: u32,
    /// `EG(timeout_seconds)`.
    pub(crate) timeout_seconds: i64,
    /// Whether output has been flushed to the SAPI at least once.
    pub headers_sent: bool,
    /// First accepted response MIME type (`SG(sapi_headers).mimetype`).
    pub(crate) response_mimetype: Option<Vec<u8>>,
    /// Whether the SAPI still needs to supply its default content type.
    pub(crate) send_default_content_type: bool,
    /// Active non-CLI SAPI's header operation (`sapi_header_op`).
    pub response_header_hook: Option<crate::ext::ResponseHeaderFn>,
    /// Active non-CLI SAPI's `sapi_send_headers()` run with the VM at hand
    /// (`php_header()`): called when output first reaches the SAPI
    /// (`php_output_header`) and at request shutdown, before the CLI's own
    /// header state is marked sent; it runs the SAPI's
    /// `header_register_callback()` callable and sends its header list.
    /// Returning `false` disables output (a HEAD request).
    pub send_headers_hook: Option<crate::ext::SendHeadersFn>,
    /// Active non-CLI SAPI's `sapi_flush()`, run by the output layer after
    /// every write under `implicit_flush` / `ob_implicit_flush()`; the CLI
    /// flushes its stdout instead.
    pub sapi_flush_hook: Option<crate::ext::SapiFlushFn>,
    /// A SAPI running the execution timer itself (`zend_set_timeout` on a
    /// CPU-time clock, as `ITIMER_PROF` does): [`Vm::set_time_limit`] tells
    /// it the limit instead of arming the VM's wall-clock timer thread, and
    /// it reports expiry with [`crate::interrupt::sapi_timer_expired`].
    pub timer_hook: Option<crate::ext::TimerFn>,
    /// Observer of the SAPI's multipart parser (`php_rfc1867_callback`).
    pub rfc1867_callback: Option<crate::ext::Rfc1867Fn>,
    /// `SG(rfc1867_uploaded_files)`: the temporary files of this request's
    /// uploads, the only paths ext/standard's `is_uploaded_file()` and
    /// `move_uploaded_file()` accept. The multipart parser's caller (the
    /// SAPI, or `zend_test_handle_post_data()`) fills it and deletes the
    /// files still listed at request shutdown.
    pub rfc1867_uploaded_files: Vec<std::path::PathBuf>,
    /// Active web SAPI's request body parse for `request_parse_body()`
    /// ([`crate::ext::ParseBodyFn`]); `None` (the CLI) means the request
    /// has no content type.
    pub parse_body_hook: Option<crate::ext::ParseBodyFn>,
    /// Running fibers (innermost last).
    pub fibers: Vec<Obj>,
    /// Destination temporary of the builtin call in progress (fibers).
    pub pending_call_dst: Option<u32>,
    /// Set by `op_do_call_nodiscard` when it already reported a native
    /// function's deprecation (`call_builtin_impl` consumes it).
    pub(crate) builtin_deprecation_reported: bool,
    /// Names of registered extensions.
    pub loaded_extensions: Vec<String>,
    /// Request shutdown hooks of extensions.
    pub shutdown_hooks: Vec<fn(&mut Vm)>,
    /// Request startup hooks of extensions (`RINIT`).
    pub startup_hooks: Vec<fn(&mut Vm)>,
    /// Module startup hooks of extensions, run after registration.
    pub module_startup_hooks: Vec<fn(&mut Vm)>,
    /// Extension `on_modify` handlers by INI name (`ZEND_INI_MH`).
    pub ini_modify_hooks: HashMap<String, crate::ext::IniModifyFn>,
    /// `PHP_MINFO` callbacks by extension name.
    pub module_info: Vec<(String, crate::ext::ModuleInfoFn)>,
    /// Context of the running `read_dimension` hook.
    pub(crate) dim_ctx: crate::ext::DimensionCtx,
    /// Context of the running `read_property` hook.
    pub(crate) prop_ctx: crate::ext::PropCtx,
    /// Stage of the running INI `on_modify` handler.
    pub(crate) ini_stage: crate::ext::IniStage,
    /// Method name of the native `__call`/`__callStatic` being run.
    pub(crate) magic_call_name: Option<ZStr>,
    /// INI warnings raised while building the registry at startup (the
    /// SAPI reports them as "PHP Startup: ...").
    pub startup_diagnostics: Vec<php_support::Diagnostic>,
    /// SAPI log sink for `log_errors` (`sapi_module.log_message`);
    /// `None` writes to stderr / the `error_log` file.
    pub log_hook: Option<Box<dyn FnMut(&[u8])>>,
    /// The value the main script `return`ed (a router's `false`).
    pub main_return: Value,
    /// Native stack address at the first execution (stack-limit checks).
    pub(crate) native_stack_base: usize,
    /// Size of the native stack the VM runs on.
    pub(crate) native_stack_size: usize,
    /// Cached `(zend.max_allowed_stack_size, zend.reserved_stack_size)`;
    /// both are `INI_SYSTEM`, so they never change after startup.
    pub(crate) stack_limit_ini: Option<(i64, usize)>,
    /// Reusable buffers of the call protocol (php-src keeps call frames and
    /// arguments on the VM stack; these pools avoid per-call mallocs).
    pub(crate) call_bufs: CallBufs,
    /// Named native output handlers (`ob_start('ob_gzhandler')`).
    pub output_handler_aliases: HashMap<ZStr, crate::ext::OutputHandlerAlias>,
    /// Module conflict checks, keyed by exact handler name.
    pub output_handler_conflicts: HashMap<ZStr, crate::ext::OutputHandlerConflictFn>,
    /// Request-setting refresh hooks for internal handlers.
    pub output_handler_hooks: HashMap<ZStr, crate::ext::OutputHandlerHookFn>,
    /// `include` opener hook.
    pub include_opener: Option<crate::ext::IncludeOpenerFn>,
    /// Source filter hook.
    pub source_filter: Option<crate::ext::SourceFilterFn>,
    /// Current mbstring encoding/substitution settings.
    pub(crate) script_encoding_provider: Option<crate::ext::ScriptEncodingFn>,
    /// Source ASTs retained with their original compile-time encoding semantics.
    pub(crate) parsed_sources: std::cell::RefCell<HashMap<ZStr, Rc<php_parser::Program>>>,
    /// `#[\Deprecated]` suffixes (" since X, msg") of native functions,
    /// by lowercase name.
    pub builtin_deprecations: HashMap<ZStr, Vec<u8>>,
    /// Attributes of native functions and methods declared by stubs
    /// (`zend_add_function_attribute`), by lowercase `name` or
    /// `class::method`.
    pub builtin_attributes: HashMap<ZStr, Rc<Vec<crate::class::AttributeInfo>>>,
    /// Doc comments of native functions and methods exposed by stubs
    /// (`@genstubs-expose-comment-block`), keyed like
    /// [`Vm::builtin_attributes`].
    pub builtin_doc_comments: HashMap<ZStr, ZStr>,
    /// Class constants whose `#[\Deprecated]` notice is being built.
    pub(crate) const_deprecation_guard: HashSet<usize>,
    /// Attributes of user constants declared with `const` (by name).
    pub const_attributes: HashMap<ZStr, Rc<Vec<crate::class::AttributeInfo>>>,
    /// File that defined each user constant (`ReflectionConstant::getFileName`).
    pub const_files: HashMap<ZStr, ZStr>,
    /// `zend_constant.name` of user constants whose name differs from
    /// their `EG(zend_constants)` key (`define()` lowercases the key's
    /// namespace part only; GH-11423).
    pub const_names: HashMap<ZStr, ZStr>,
    /// Deprecated native constants: name → " since X, msg" suffix.
    pub deprecated_constants: HashMap<ZStr, Vec<u8>>,
    /// `(entries, sorted hashes)` of the names in `deprecated_constants`
    /// and `const_attributes`, rebuilt when their size changes (entries
    /// are only ever added).
    pub(crate) const_deprecation_filter: std::cell::RefCell<(usize, Vec<u64>)>,
    /// The constants the compiler substitutes (`zend_try_ct_eval_const`),
    /// keyed by the number of internal and deprecated constants it was
    /// built from (see [`Vm::ct_constants`]).
    pub(crate) ct_constants: Option<((usize, usize), crate::compiler::CtConstants)>,
    /// `ZEND_COMPILE_NO_BUILTINS`: compile every builtin call as a call
    /// instead of the dedicated opcodes and constants of
    /// `zend_try_compile_special_func()` (see [`Vm::special_builtins`]).
    /// Off by default, as in php-src.
    pub compile_no_builtins: bool,
    /// Shared empty code for builtin frames.
    pub empty_code: Rc<OpArray>,
    /// Shared empty variable store for builtin frames.
    pub empty_vars: Rc<RefCell<VarStore>>,
    /// Active `__get`/`__set`/`__isset`/`__unset` guards: (object handle,
    /// property name, kind).
    pub prop_guards: HashSet<(u32, ZStr, u8)>,
    /// Request-local observer initialization and callback state.
    pub(crate) observer: crate::observer::ObserverState,
    /// `zend_set_user_opcode_handler()` hooks: Zend opcode name and the
    /// handler run before each such opcode compiled after registration.
    pub(crate) user_opcode_handlers: Vec<(&'static str, crate::observer::UserOpcodeHandler)>,
}

impl std::fmt::Debug for Vm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vm")
            .field("frames", &self.frames.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::{Frame, FrameKind, VarStore};
    use crate::opcode::OpArray;
    use php_runtime::{Value, ZStr};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn frame_with(names: &[&str]) -> Frame {
        let code = OpArray {
            cv_names: names.iter().map(|n| ZStr::from(*n)).collect(),
            ..OpArray::default()
        };
        Frame::new(Rc::new(code), FrameKind::Function)
    }

    fn as_int(s: Option<&Value>) -> Option<i64> {
        match s {
            Some(Value::Int(i)) => Some(*i),
            _ => None,
        }
    }

    #[test]
    fn peek_cv_reads_slots_in_place_without_holding_a_borrow() {
        let f = frame_with(&["a", "b"]);
        f.vars.borrow_mut().slots[0] = Some(Value::Int(7));
        assert_eq!(f.peek_cv(0, as_int), Some(7));
        // Undefined.
        assert!(f.peek_cv(1, |s| s.is_none()));
        // No guard outlives the peek: the store can be borrowed mutably
        // right after it.
        assert!(f.vars.try_borrow_mut().is_ok());
        // Shared borrows elsewhere do not stop a peek.
        let shared = f.vars.borrow();
        assert_eq!(f.peek_cv(0, as_int), Some(7));
        drop(shared);
    }

    #[test]
    fn peek_cv_follows_the_cv_map_of_a_shared_store() {
        let code = Rc::new(OpArray {
            cv_names: vec![ZStr::from("x")],
            ..OpArray::default()
        });
        let vars = Rc::new(RefCell::new(VarStore::with_names(&[
            ZStr::from("argv"),
            ZStr::from("x"),
        ])));
        vars.borrow_mut().slots[1] = Some(Value::Int(3));
        let f = Frame::with_vars(code, FrameKind::Main, vars, Some(Rc::from(vec![1u32])));
        assert_eq!(f.peek_cv(0, as_int), Some(3));
    }

    #[test]
    fn peek_cv_clones_share_the_string() {
        let f = frame_with(&["s"]);
        f.vars.borrow_mut().slots[0] = Some(Value::Str(ZStr::from("abc")));
        let v = f.peek_cv(0, |slot: Option<&Value>| slot.cloned());
        assert!(matches!(&v, Some(Value::Str(s)) if s.as_bytes() == b"abc"));
        // The slot still holds its value.
        assert!(f.peek_cv(0, |s| matches!(s, Some(Value::Str(_)))));
    }

    #[test]
    #[should_panic(expected = "already mutably borrowed")]
    fn peek_cv_panics_like_borrow_while_mutably_borrowed() {
        let f = frame_with(&["a"]);
        let _guard = f.vars.borrow_mut();
        let _ = f.peek_cv(0, |s| s.is_some());
    }

    #[test]
    fn move_last_frame_transfers_ownership_once() {
        let code = Rc::new(OpArray::default());
        let mut pool = vec![
            Frame::new_pooled(code.clone()),
            Frame::new_pooled(code.clone()),
        ];
        let mut stack: Vec<Frame> = Vec::new();
        let held = ZStr::from("held by the moved frame");
        pool[1].tmps.push(Value::Str(held.clone()));
        pool[1].ip = 7;
        // The op array: this handle plus one per frame.
        assert_eq!(Rc::strong_count(&code), 3);
        assert!(super::move_last_frame(&mut pool, &mut stack));
        assert_eq!((pool.len(), stack.len()), (1, 1));
        assert_eq!(stack[0].ip, 7);
        assert!(
            matches!(stack[0].tmps.as_slice(), [Value::Str(s)] if s.as_bytes() == held.as_bytes())
        );
        // Moved, not copied: one frame holds the string and the code.
        assert_eq!(held.refcount(), 2);
        assert_eq!(Rc::strong_count(&code), 3);
        // And back.
        assert!(super::move_last_frame(&mut stack, &mut pool));
        assert_eq!((pool.len(), stack.len()), (2, 0));
        assert_eq!(pool[1].ip, 7);
        assert_eq!(held.refcount(), 2);
        // An empty source moves nothing.
        assert!(!super::move_last_frame(&mut stack, &mut pool));
        assert_eq!((pool.len(), stack.len()), (2, 0));
        drop(pool);
        assert_eq!(held.refcount(), 1);
        assert_eq!(Rc::strong_count(&code), 1);
    }

    #[test]
    fn refill_with_reuses_the_buffer_and_fills_exactly_n() {
        let mut v: Vec<Value> = Vec::with_capacity(8);
        v.push(Value::Int(1));
        let p = v.as_ptr();
        super::refill_with(&mut v, 3, || Value::Null);
        assert_eq!(v.len(), 3);
        assert!(v.iter().all(Value::is_null));
        assert_eq!(v.as_ptr(), p);
        super::refill_with(&mut v, 0, || Value::Null);
        assert!(v.is_empty());
        assert_eq!(v.capacity(), 8);
        super::refill_with(&mut v, 20, || Value::Int(2));
        assert_eq!(v.len(), 20);
        assert!(v.iter().all(|x| matches!(x, Value::Int(2))));
    }

    #[test]
    fn reinit_for_call_leaves_the_compiled_variables_to_the_caller() {
        let empty = Rc::new(OpArray::default());
        let code = Rc::new(OpArray {
            cv_names: vec![ZStr::from("a"), ZStr::from("b")],
            num_tmps: 3,
            num_iters: 1,
            num_finally: 1,
            ..OpArray::default()
        });
        let mut f = Frame::new_pooled(empty.clone());
        f.reinit_for_call(code.clone(), FrameKind::Function);
        assert_eq!(f.tmps.len(), 3);
        assert_eq!(f.iters.len(), 1);
        assert_eq!(f.fins.len(), 1);
        assert_eq!(f.kind, FrameKind::Function);
        assert!(Rc::ptr_eq(&f.code, &code));
        {
            let vars = f.vars.borrow();
            assert!(vars.slots.is_empty());
            assert_eq!(vars.names.len(), 2);
        }
        // `reinit_for_user` fills them as undefined.
        f.clear_for_pool(&empty);
        f.reinit_for_user(code.clone(), FrameKind::Function);
        let vars = f.vars.borrow();
        assert_eq!(vars.slots.len(), 2);
        assert!(vars.slots.iter().all(Option::is_none));
    }

    #[test]
    fn clear_for_pool_releases_every_handle() {
        let empty = Rc::new(OpArray::default());
        let code = Rc::new(OpArray {
            cv_names: vec![ZStr::from("a")],
            num_tmps: 1,
            ..OpArray::default()
        });
        let mut f = Frame::new_pooled(empty.clone());
        f.reinit_for_user(code.clone(), FrameKind::Function);
        let s = ZStr::from("value");
        f.vars.borrow_mut().slots[0] = Some(Value::Str(s.clone()));
        f.tmps[0] = Value::Str(s.clone());
        f.args.push(Some(Value::Str(s.clone())));
        f.named.push((ZStr::from("n"), Value::Str(s.clone())));
        f.ip = 5;
        f.ret = super::RetTarget::Tmp(2);
        f.caller_strict = true;
        assert_eq!(s.refcount(), 5);
        f.clear_for_pool(&empty);
        assert_eq!(s.refcount(), 1);
        assert_eq!(Rc::strong_count(&code), 1);
        assert!(Rc::ptr_eq(&f.code, &empty));
        assert_eq!(f.ip, 0);
        assert_eq!(f.ret, super::RetTarget::Discard);
        assert!(!f.caller_strict);
        assert!(f.tmps.is_empty() && f.args.is_empty() && f.named.is_empty());
        assert!(f.vars.borrow().slots.is_empty());
    }
}
